//! EXPLAIN planning, projection, and optional single execution.

use super::*;
use crate::{
	gql::optimizer::{
		Cardinality,
		GraphResultOperator,
		MatchOperator,
		PhysicalGraphResultPlan,
		PhysicalPlan,
		PhysicalSelectPlan,
		PhysicalTraversalPlan,
		PlanCost,
		PlannerDemand,
		PlannerSnapshot,
		SourceOperator,
		TraversalOperator,
		TraversalSourceOperator,
	},
	syn::{ExplainFormat, ExplainMode},
};

#[derive(Clone, Copy)]
struct AnalyzeObservation {
	actual_rows: u64,
	pulls: u64,
	affected_rows: u64,
	elapsed_ns: u64,
	peak_memory: u64,
	spill_bytes: u64,
	spill_runs: u64,
	path_expansions: u64,
	match_metrics: crate::gql::options::MatchMetricsSnapshot,
	graph_assembly: crate::gql::options::GraphAssemblyMetricsSnapshot,
	/// Actual candidates, refinements, fallbacks, tree pages and delta records.
	spatial: [u64; 5],
	/// Sketch hits, exact fallbacks, charged work and consulted artifact bytes.
	neighbourhood: [u64; 4],
	/// Logical history reader counters, with admitted scratch peak last.
	changefeed: [u64; 9],
	publication: &'static str,
}

#[allow(clippy::too_many_arguments, reason = "EXPLAIN reuses the statement interpreter context")]
pub(super) fn execute_explain<Read: QueryRead>(
	read: &mut Read,
	explain: &crate::gql::ir::LogicalExplain,
	slots: &mut [Value],
	bindings: &mut Object,
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	depth: u32,
) -> FerrosGrynnResult<ControlFlow> {
	let snapshot = read.snapshot()?;
	let planning_started = Instant::now();
	let match_binding = match explain.statement.as_ref() {
		LogicalStatement::Match(plan) => Some((
			resolve_match_graph(read, snapshot, plan, slots, source, control)?,
			resolve_match_max_depth(read, snapshot, plan, slots, source, control)?,
		)),
		_ => None,
	};
	let demand = if let (LogicalStatement::Match(plan), Some((graph, _))) =
		(explain.statement.as_ref(), &match_binding)
	{
		PlannerDemand::for_match_bound(plan, graph.id())
	} else {
		PlannerDemand::for_statement(&explain.statement)
	};
	let planner_snapshot = read.planner_snapshot(snapshot, &demand)?;
	let deterministic_snapshot;
	let planning_snapshot = if control.deterministic() {
		deterministic_snapshot = planner_snapshot.deterministic_projection();
		&deterministic_snapshot
	} else {
		&planner_snapshot
	};
	let history_plan = match explain.statement.as_ref() {
		LogicalStatement::Schema(statement) => changefeed::plan(read, statement, slots, control)?,
		_ => None,
	};
	let live_plan = live_planning::plan(read, &explain.statement, slots, control)?;
	let mut live_account = None;
	let plan = if let Some((plan, account)) = live_plan {
		live_account = Some(account);
		PhysicalPlan::Live(plan)
	} else if let Some(plan) = history_plan {
		plan
	} else if let (LogicalStatement::Match(statement), Some((graph, max_depth))) =
		(explain.statement.as_ref(), &match_binding)
	{
		crate::gql::optimizer::finish_match_graph_result(
			statement,
			crate::gql::optimizer::plan_match_bound(
				statement,
				planning_snapshot,
				control.deterministic(),
				Some(graph),
				*max_depth,
			)?,
			control.max_graph_nodes(),
			control.max_graph_edges(),
			control.barrier_memory_limit(),
		)
	} else {
		physical_plan(
			&explain.statement,
			planning_snapshot,
			control.barrier_memory_limit(),
			control.deterministic(),
			control.fast(),
			control.max_graph_nodes(),
			control.max_graph_edges(),
		)?
	};
	let planning_ns = nanos(planning_started.elapsed());
	let observation = if explain.mode == ExplainMode::Analyze {
		let mutation_before = read.mutation_count();
		let pulls_before = control.row_pulls();
		let path_expansions_before = control.path_expansions();
		let match_metrics_before = control.match_metrics();
		let graph_assembly_before = control.graph_assembly_metrics();
		let spatial_before = control.spatial_metrics();
		let neighbourhood_before = control.neighbourhood_metrics();
		let (spill_bytes_before, spill_runs_before) = control.spill_metrics();
		let analyze_control = control.clone().with_instrumentation();
		let started = Instant::now();
		let value = match execute_statement(
			read,
			&explain.statement,
			slots,
			bindings,
			source,
			&analyze_control,
			runtime,
			depth + 1,
			false,
		)? {
			ControlFlow::Value(value) | ControlFlow::Return(value) => value,
			ControlFlow::Break | ControlFlow::Continue => {
				return Err(FerrosGrynnError::invalid_argument(
					"EXPLAIN ANALYZE statement returned loop control.",
				));
			}
		};
		analyze_control.discard_continuation();
		let mutations = read.mutation_count().saturating_sub(mutation_before);
		let (spill_bytes, spill_runs) = analyze_control.spill_metrics();
		Some(AnalyzeObservation {
			actual_rows: result_rows(&value),
			pulls: analyze_control.row_pulls().saturating_sub(pulls_before),
			affected_rows: u64::try_from(mutations).unwrap_or(u64::MAX),
			elapsed_ns: nanos(started.elapsed()),
			peak_memory: u64::try_from(analyze_control.barrier_memory_peak()).unwrap_or(u64::MAX),
			spill_bytes: spill_bytes.saturating_sub(spill_bytes_before),
			spill_runs: spill_runs.saturating_sub(spill_runs_before),
			path_expansions: analyze_control
				.path_expansions()
				.saturating_sub(path_expansions_before),
			match_metrics: analyze_control.match_metrics().saturating_sub(match_metrics_before),
			graph_assembly: analyze_control
				.graph_assembly_metrics()
				.saturating_sub(graph_assembly_before),
			spatial: std::array::from_fn(|index| {
				analyze_control.spatial_metrics()[index].saturating_sub(spatial_before[index])
			}),
			neighbourhood: std::array::from_fn(|index| {
				analyze_control.neighbourhood_metrics()[index]
					.saturating_sub(neighbourhood_before[index])
			}),
			changefeed: analyze_control.changefeed_metrics(),
			publication: if logical_mutates(&explain.statement) {
				if control.publication_deferred() {
					"deferred"
				} else {
					"pending"
				}
			} else {
				"not_applicable"
			},
		})
	} else {
		None
	};
	let query_fingerprint = match explain.statement.as_ref() {
		LogicalStatement::Select(select) => {
			crate::gql::optimizer::fingerprint::fingerprints(select).0
		}
		_ => plan.hash(),
	};
	let seed = control.statement_seed(query_fingerprint, snapshot.0);
	let value = match explain.format {
		ExplainFormat::Json => explain_json(
			explain.mode,
			&explain.statement,
			&plan,
			&planner_snapshot,
			snapshot,
			seed,
			planning_ns,
			observation.as_ref(),
		),
		ExplainFormat::Text => explain_text(
			explain.mode,
			&explain.statement,
			&plan,
			&planner_snapshot,
			snapshot,
			seed,
			planning_ns,
			observation.as_ref(),
		),
	};
	drop(live_account);
	Ok(ControlFlow::Value(value))
}

fn physical_plan(
	statement: &LogicalStatement,
	snapshot: &PlannerSnapshot,
	barrier_bytes: usize,
	deterministic: bool,
	fast: bool,
	max_graph_nodes: u64,
	max_graph_edges: u64,
) -> FerrosGrynnResult<PhysicalPlan> {
	match statement {
		LogicalStatement::Select(select) => {
			crate::gql::optimizer::prepare_select(select, snapshot, barrier_bytes)
				.map(|(_logical, plan)| PhysicalPlan::Select(plan))
		}
		LogicalStatement::Match(statement) => crate::gql::optimizer::plan_match_graph_result(
			statement,
			snapshot,
			deterministic,
			max_graph_nodes,
			max_graph_edges,
			barrier_bytes,
		),
		LogicalStatement::Traverse(statement) => crate::gql::optimizer::plan_traverse_graph_result(
			statement,
			snapshot,
			barrier_bytes,
			deterministic,
			fast,
			max_graph_nodes,
			max_graph_edges,
		),
		LogicalStatement::ReturnGraph(result) => Ok(crate::gql::optimizer::plan_return_graph(
			result,
			max_graph_nodes,
			max_graph_edges,
			barrier_bytes,
		)),
		LogicalStatement::BuildGraph(build) => crate::gql::optimizer::plan_build_graph(
			build,
			snapshot,
			barrier_bytes,
			deterministic,
			fast,
			max_graph_nodes,
			max_graph_edges,
		),
		LogicalStatement::Schema(_) | LogicalStatement::Dml(_) | LogicalStatement::GraphDml(_) => {
			Ok(PhysicalPlan::Write(crate::gql::optimizer::plan_write(statement, snapshot)))
		}
		_ => Err(FerrosGrynnError::query_feature_not_supported("EXPLAIN for this statement")),
	}
}

#[allow(clippy::too_many_arguments, reason = "the explain envelope has fixed contract fields")]
fn explain_json(
	mode: ExplainMode,
	statement: &LogicalStatement,
	plan: &PhysicalPlan,
	snapshot: &PlannerSnapshot,
	query_snapshot: Snapshot,
	seed: u64,
	planning_ns: u64,
	observation: Option<&AnalyzeObservation>,
) -> Value {
	let mut root = Object::new();
	root.insert("plan_version", u64::from(crate::gql::optimizer::PLAN_ENCODING_VERSION));
	root.insert("plan_hash", plan.hash());
	root.insert("statement_kind", statement_name(statement));
	root.insert("mode", mode_name(mode));
	root.insert("snapshot", query_snapshot.0);
	root.insert("seed", seed);
	root.insert("statistics_revision", snapshot.revision);
	root.insert("statistics", Value::Object(statistics_object(snapshot)));
	root.insert("planning_elapsed_ns", planning_ns);
	let (required_ordering, provided_ordering) = ordering_projection(statement, plan);
	root.insert("required_ordering", Value::Array(required_ordering.into()));
	root.insert("provided_ordering", provided_ordering);
	root.insert(
		"nodes",
		Value::Array(if mode == ExplainMode::Logical {
			logical_nodes(statement).into()
		} else {
			physical_nodes(plan, snapshot).into()
		}),
	);
	root.insert("warnings", Value::Array(plan_warnings(plan).into()));
	root.insert("rules", Value::Array(plan_rules(plan).into()));
	if let Some(observation) = observation {
		root.insert("analyze", Value::Object(observation_object(observation)));
	}
	Value::Object(root)
}

#[allow(clippy::too_many_arguments, reason = "the explain envelope has fixed contract fields")]
fn explain_text(
	mode: ExplainMode,
	statement: &LogicalStatement,
	plan: &PhysicalPlan,
	snapshot: &PlannerSnapshot,
	query_snapshot: Snapshot,
	seed: u64,
	planning_ns: u64,
	observation: Option<&AnalyzeObservation>,
) -> Value {
	let mut lines = vec![format!(
		"EXPLAIN {} {} plan=v{} hash={:016x} snapshot={} seed={} stats_revision={} planning_ns={}",
		mode_name(mode),
		statement_name(statement),
		crate::gql::optimizer::PLAN_ENCODING_VERSION,
		plan.hash(),
		query_snapshot.0,
		seed,
		snapshot.revision,
		planning_ns,
	)];
	let nodes = if mode == ExplainMode::Logical {
		logical_nodes(statement)
	} else {
		physical_nodes(plan, snapshot)
	};
	for node in nodes {
		lines.push(format!("  {}", compact_node(&node)));
	}
	for warning in plan_warnings(plan) {
		lines.push(format!("  warning: {}", value_text(&warning)));
	}
	for rule in plan_rules(plan) {
		lines.push(format!("  rule: {}", value_text(&rule)));
	}
	if let Some(observation) = observation {
		lines.push(format!(
			"  actual rows={} pulls={} affected={} elapsed_ns={} spill_bytes={} spill_runs={} \
			 peak_memory={} path_expansions={} frontier_peak={} trace_nodes={} graph_opens={} \
			 containment_links={} hyper_assignments={} cancellation_checks={} physical_windows={} \
			 physical_resident_pages={} physical_wal_pages={} physical_sync_pages={} \
			 physical_ioring_pages={} graph_source_rows={} graph_selected_nodes={} \
			 graph_selected_edges={} graph_closure_nodes={} graph_closure_edges={} \
			 graph_deduplicated_values={} graph_final_nodes={} graph_final_edges={} \
			 graph_child_graphs={} graph_scope_slots={} graph_validation_ns={} publication={}",
			observation.actual_rows,
			observation.pulls,
			observation.affected_rows,
			observation.elapsed_ns,
			observation.spill_bytes,
			observation.spill_runs,
			observation.peak_memory,
			observation.path_expansions,
			observation.match_metrics.frontier_peak,
			observation.match_metrics.trace_nodes,
			observation.match_metrics.graph_opens,
			observation.match_metrics.containment_links,
			observation.match_metrics.hyper_assignments,
			observation.match_metrics.cancellation_checks,
			observation.match_metrics.physical_windows,
			observation.match_metrics.physical_resident_pages,
			observation.match_metrics.physical_wal_pages,
			observation.match_metrics.physical_sync_pages,
			observation.match_metrics.physical_ioring_pages,
			observation.graph_assembly.source_rows,
			observation.graph_assembly.selected_nodes,
			observation.graph_assembly.selected_edges,
			observation.graph_assembly.closure_nodes,
			observation.graph_assembly.closure_edges,
			observation.graph_assembly.deduplicated_values,
			observation.graph_assembly.final_nodes,
			observation.graph_assembly.final_edges,
			observation.graph_assembly.child_graphs,
			observation.graph_assembly.scope_slots,
			observation.graph_assembly.validation_ns,
			observation.publication,
		));
		lines.push(format!(
			"  landmark_lookups={} landmark_fallbacks={} landmark_artifact_bytes={}",
			observation.match_metrics.landmark_lookups,
			observation.match_metrics.landmark_fallbacks,
			observation.match_metrics.landmark_artifact_bytes,
		));
		lines.push(format!(
			"  algorithm_work={} algorithm_paths={} tensor_cache_hits={} tensor_fallbacks={} \
			 tensor_cells={} tensor_nonzeros={} tensor_iterations={} tensor_artifact_bytes={}",
			observation.match_metrics.algorithm_work,
			observation.match_metrics.algorithm_paths,
			observation.match_metrics.tensor_cache_hits,
			observation.match_metrics.tensor_fallbacks,
			observation.match_metrics.tensor_cells,
			observation.match_metrics.tensor_nonzeros,
			observation.match_metrics.tensor_iterations,
			observation.match_metrics.tensor_artifact_bytes
		));
		lines.push(format!(
			"  spatial_candidates={} spatial_refinements={} spatial_fallbacks={}",
			observation.spatial[0], observation.spatial[1], observation.spatial[2]
		));
		lines.push(format!(
			"  spatial_tree_pages={} spatial_delta_records={}",
			observation.spatial[3], observation.spatial[4]
		));
		lines.push(format!(
			"  neighbourhood_sketch_reads={} neighbourhood_fallbacks={} \
			 neighbourhood_exact_work={} graph_artifact_bytes={}",
			observation.neighbourhood[0],
			observation.neighbourhood[1],
			observation.neighbourhood[2],
			observation.neighbourhood[3]
		));
		for (name, counter) in [
			"changefeed_batches",
			"changefeed_entries",
			"changefeed_gaps",
			"changefeed_groups_inspected",
			"changefeed_page_reads",
			"changefeed_bytes_read",
			"changefeed_segment_visits",
			"changefeed_work",
			"changefeed_peak_scratch_bytes",
		]
		.into_iter()
		.zip(observation.changefeed)
		{
			lines.push(format!("  {name}={counter}"));
		}
	}
	Value::from(lines.join("\n"))
}

fn logical_nodes(statement: &LogicalStatement) -> Vec<Value> {
	let mut nodes = Vec::new();
	if let Some((_, operation)) = live_planning::command(statement) {
		push_node(&mut nodes, 0, operation.name(), None, None);
		return nodes;
	}
	match statement {
		LogicalStatement::Select(select) => {
			push_node(&mut nodes, 0, "Select", None, None);
			push_node(&mut nodes, 1, source_name(&select.source.kind), None, None);
			for (index, join) in select.joins.iter().enumerate() {
				push_node(
					&mut nodes,
					u32::try_from(index + 2).unwrap_or(u32::MAX),
					&format!("{:?}Join", join.kind),
					None,
					None,
				);
			}
			if select.filter.is_some() {
				let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
				push_node(&mut nodes, id, "Filter", None, None);
			}
			let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
			push_node(&mut nodes, id, "Project", None, None);
		}
		LogicalStatement::Match(statement) => {
			push_node(&mut nodes, 0, "Match", None, None);
			for _pattern in &statement.patterns {
				let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
				push_node(&mut nodes, id, "Pattern", Some(Cardinality::unknown(u64::MAX)), None);
			}
			if statement.return_graph.is_some() {
				let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
				push_node(&mut nodes, id, "GraphAssemble", None, None);
			}
		}
		LogicalStatement::Traverse(statement) => {
			push_node(&mut nodes, 0, "TraverseSource", None, None);
			for step in &statement.steps {
				let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
				push_node(&mut nodes, id, &format!("{:?}", step.kind), None, None);
			}
			if statement.returning.is_some() {
				let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
				push_node(&mut nodes, id, "Project", None, None);
			}
			if statement.return_graph.is_some() {
				let id = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
				push_node(&mut nodes, id, "GraphAssemble", None, None);
			}
		}
		LogicalStatement::ReturnGraph(_) => {
			push_node(&mut nodes, 0, "GraphAssemble", None, None);
		}
		LogicalStatement::BuildGraph(build) => {
			push_node(
				&mut nodes,
				0,
				if build.name.is_some() {
					"BuildGraph"
				} else {
					"GraphAssemble"
				},
				None,
				None,
			);
			push_node(&mut nodes, 1, "Source", None, None);
		}
		LogicalStatement::Schema(schema) => push_node(
			&mut nodes,
			0,
			match schema {
				crate::gql::ir::LogicalSchemaStatement::ShowChanges {
					..
				} => "ChangefeedRead",
				crate::gql::ir::LogicalSchemaStatement::InfoChangefeed {
					..
				} => "ChangefeedInfo",
				_ if schema.is_read() => "CatalogRead",
				_ => "CatalogMutation",
			},
			None,
			None,
		),
		LogicalStatement::Dml(_) | LogicalStatement::GraphDml(_) => {
			push_node(&mut nodes, 0, "TargetSelection", None, None);
			push_node(&mut nodes, 1, "Mutation", None, None);
		}
		_ => {}
	}
	nodes
}

fn physical_nodes(plan: &PhysicalPlan, snapshot: &PlannerSnapshot) -> Vec<Value> {
	match plan {
		PhysicalPlan::Select(select) => select_nodes(select, snapshot),
		PhysicalPlan::Match(plan) => plan
			.nodes
			.iter()
			.map(|planned| {
				let mut node = node_object(
					planned.node_id,
					match_operator_name(&planned.operator),
					Some(planned.cardinality),
					Some(planned.cost),
				);
				node.insert(
					"graph_id",
					plan.graph.map_or(Value::None, |graph| Value::from(graph.get())),
				);
				node.insert(
					"graph_binding",
					if plan.graph.is_some() {
						"resolved"
					} else {
						"deferred"
					},
				);
				if let Some(pattern) = planned.pattern_index {
					node.insert("pattern_index", u64::try_from(pattern).unwrap_or(u64::MAX));
				}
				if let Some(operation) = planned.operation_index {
					node.insert("operation_index", u64::try_from(operation).unwrap_or(u64::MAX));
				}
				project_match_operator(&mut node, &planned.operator);
				Value::Object(node)
			})
			.collect(),
		PhysicalPlan::Traversal(plan) => traversal_nodes(plan),
		PhysicalPlan::Write(plan) => plan
			.operators
			.iter()
			.enumerate()
			.map(|(index, operator)| {
				Value::Object(node_object(
					u32::try_from(index).unwrap_or(u32::MAX),
					&format!("{operator:?}"),
					Some(plan.cardinality),
					Some(plan.cost),
				))
			})
			.collect(),
		PhysicalPlan::GraphResult(plan) => graph_result_nodes(plan, snapshot),
		PhysicalPlan::Live(plan) => live_planning::project(plan),
		PhysicalPlan::Changefeed(plan) => {
			let mut node = node_object(
				0,
				if plan.mode.is_some() {
					"ChangefeedHistoryRead"
				} else {
					"ChangefeedInfo"
				},
				None,
				None,
			);
			changefeed::project_plan(&mut node, plan);
			vec![Value::Object(node)]
		}
	}
}

fn graph_result_nodes(plan: &PhysicalGraphResultPlan, snapshot: &PlannerSnapshot) -> Vec<Value> {
	let mut root = node_object(
		0,
		match plan.operator {
			GraphResultOperator::GraphAssemble => "GraphAssemble",
			GraphResultOperator::BuildGraph => "BuildGraph",
		},
		Some(Cardinality::exact(1)),
		Some(plan.cost),
	);
	root.insert("output_mode", format!("{:?}", plan.mode));
	root.insert("graph_name", plan.name.as_deref().map_or(Value::None, Value::from));
	root.insert("family", plan.family.map_or(Value::None, |family| Value::from(family.name())));
	root.insert("validator_bits", u64::from(plan.validator_bits));
	root.insert("root_selector", plan.has_root);
	root.insert("node_selector", plan.has_node_selector);
	root.insert("edge_selector", plan.has_edge_selector);
	root.insert("node_limit", plan.node_limit);
	root.insert("edge_limit", plan.edge_limit);
	root.insert("expected_nodes", Value::Object(cardinality_object(plan.expected_nodes)));
	root.insert("expected_edges", Value::Object(cardinality_object(plan.expected_edges)));
	root.insert("expected_scopes", Value::Object(cardinality_object(plan.expected_scopes)));
	let mut nodes = vec![Value::Object(root)];
	if let Some(source) = &plan.source {
		let mut source_nodes = physical_nodes(source, snapshot);
		for node in &mut source_nodes {
			if let Value::Object(node) = node
				&& let Some(Value::Number(id)) = node.get("node_id").cloned()
				&& let Ok(id) = u64::try_from(id)
			{
				node.insert("node_id", id.saturating_add(1));
			}
		}
		nodes.extend(source_nodes);
	}
	nodes
}

fn traversal_nodes(plan: &PhysicalTraversalPlan) -> Vec<Value> {
	plan.nodes
		.iter()
		.map(|planned| {
			let mut node = node_object(
				planned.node_id,
				traversal_operator_name(&planned.operator),
				Some(planned.cardinality),
				Some(planned.cost),
			);
			if let Some(step) = planned.step_index {
				node.insert("step_index", u64::try_from(step).unwrap_or(u64::MAX));
			}
			if let Some(hash) = planned.nested_plan_hash {
				node.insert("nested_plan_hash", hash);
			}
			node.insert("unordered", planned.unordered);
			if let Some((function, version, epoch, stateful)) = plan.custom_cost {
				node.insert("cost_function_id", function.get());
				node.insert("cost_function_version", version.get());
				node.insert("cost_schema_epoch", epoch);
				node.insert("cost_stateful", stateful);
				if let Some((mode, filters)) = &plan.custom_constraints {
					node.insert("custom_path_mode", format!("{mode:?}"));
					node.insert("custom_path_filters", format!("{filters:?}"));
				}
			}
			if let TraversalOperator::Step(kind) = &planned.operator {
				node.insert("barrier", kind.is_barrier());
				if *kind == TraverseStepKind::KHopEstimate {
					node.insert("approximate_count", true);
					node.insert("fallback", "exact_traversal");
				}
				if plan.custom_cost.is_some()
					&& matches!(
						kind,
						TraverseStepKind::WeightedShortestPath | TraverseStepKind::DistanceTo
					) {
					node.insert("refinement", "canonical full-state path");
					node.insert("fallback", "exact_traversal");
				}
			} else if let TraversalOperator::Tensor {
				kind,
				projection,
			} = &planned.operator
			{
				node.insert("barrier", true);
				node.insert("step", traversal_step_name(*kind));
				node.insert(
					"enumeration",
					matches!(
						kind,
						TraverseStepKind::AllShortestPaths
							| TraverseStepKind::AllWeightedShortestPaths
					),
				);
				project_tensor(&mut node, projection);
			} else if let TraversalOperator::NeighbourhoodSketch(index) = &planned.operator {
				node.insert("barrier", true);
				node.insert("graph_id", index.graph.get());
				node.insert("index_id", index.index.get());
				node.insert("index_version", index.version.get());
				node.insert("build_revision", index.revision);
				node.insert(
					"mode",
					if index.mode == crate::graph::TraversalSemantics::Atom {
						"ATOM"
					} else {
						"NODE"
					},
				);
				node.insert("direction", "OUT");
				node.insert("precision", u64::from(index.precision));
				node.insert("artifact_bytes", index.bytes);
				node.insert("approximate_count", true);
				node.insert("fallback", "exact_traversal");
			} else if let TraversalOperator::LandmarkShortest(index)
			| TraversalOperator::LandmarkWeighted {
				index,
				..
			} = &planned.operator
			{
				node.insert("barrier", true);
				node.insert("graph_id", index.graph.get());
				node.insert("index_id", index.index.get());
				node.insert("index_version", index.version.get());
				node.insert("build_revision", index.revision);
				node.insert("mode", "NODE");
				node.insert("direction", "OUT");
				let weighted =
					matches!(planned.operator, TraversalOperator::LandmarkWeighted { .. });
				node.insert(
					"cost",
					if weighted {
						if plan.custom_cost.is_some() {
							"COST"
						} else {
							"WEIGHT"
						}
					} else {
						"HOP"
					},
				);
				if let TraversalOperator::LandmarkWeighted {
					kind,
					..
				} = &planned.operator
				{
					node.insert("step", traversal_step_name(*kind));
				}
				node.insert("landmarks", u64::from(index.landmarks));
				node.insert("artifact_bytes", index.bytes);
				node.insert(
					"refinement",
					if weighted {
						if plan.custom_cost.is_some() {
							"ALT bound, canonical full-state path"
						} else {
							"ALT bound, canonical Dijkstra path"
						}
					} else {
						"A* length, canonical BFS path"
					},
				);
				node.insert("fallback", "exact_traversal");
			}
			Value::Object(node)
		})
		.collect()
}

fn traversal_operator_name(operator: &TraversalOperator) -> &'static str {
	match operator {
		TraversalOperator::Source(TraversalSourceOperator::ValueSource) => "ValueSource",
		TraversalOperator::Source(TraversalSourceOperator::MatchSource) => "MatchSource",
		TraversalOperator::Source(TraversalSourceOperator::SelectSource) => "SelectSource",
		TraversalOperator::Project => "Project",
		TraversalOperator::Step(kind) => traversal_step_name(*kind),
		TraversalOperator::Tensor {
			kind,
			..
		} => traversal_step_name(*kind),
		TraversalOperator::NeighbourhoodSketch(_) => "NeighbourhoodSketch",
		TraversalOperator::LandmarkShortest(_) => "LandmarkShortest",
		TraversalOperator::LandmarkWeighted {
			..
		} => "LandmarkWeighted",
	}
}

fn traversal_step_name(kind: TraverseStepKind) -> &'static str {
	match kind {
		TraverseStepKind::Vertices => "Vertices",
		TraverseStepKind::Edges => "Edges",
		TraverseStepKind::Out => "Out",
		TraverseStepKind::In => "In",
		TraverseStepKind::Both => "Both",
		TraverseStepKind::OutEdges => "OutEdges",
		TraverseStepKind::InEdges => "InEdges",
		TraverseStepKind::BothEdges => "BothEdges",
		TraverseStepKind::InVertex => "InVertex",
		TraverseStepKind::OutVertex => "OutVertex",
		TraverseStepKind::Hyper => "Hyper",
		TraverseStepKind::MetaInner => "MetaInner",
		TraverseStepKind::HasAttribute => "HasAttribute",
		TraverseStepKind::Has => "Has",
		TraverseStepKind::HasLabel => "HasLabel",
		TraverseStepKind::Where => "Where",
		TraverseStepKind::Deduplicate => "Deduplicate",
		TraverseStepKind::DeduplicateBy => "DeduplicateBy",
		TraverseStepKind::SimplePath => "SimplePath",
		TraverseStepKind::CyclicPath => "CyclicPath",
		TraverseStepKind::Map => "Map",
		TraverseStepKind::FlatMap => "FlatMap",
		TraverseStepKind::Algorithm => "Algorithm",
		TraverseStepKind::Values => "Values",
		TraverseStepKind::Select => "Select",
		TraverseStepKind::Match => "Match",
		TraverseStepKind::As => "As",
		TraverseStepKind::Project => "Project",
		TraverseStepKind::Unfold => "Unfold",
		TraverseStepKind::InitializeSack => "InitializeSack",
		TraverseStepKind::Sack => "Sack",
		TraverseStepKind::GetSack => "GetSack",
		TraverseStepKind::InitializeNamedSack => "InitializeNamedSack",
		TraverseStepKind::NamedSack => "NamedSack",
		TraverseStepKind::GetNamedSack => "GetNamedSack",
		TraverseStepKind::SideEffect => "SideEffect",
		TraverseStepKind::Path => "Path",
		TraverseStepKind::Emit => "Emit",
		TraverseStepKind::Limit => "Limit",
		TraverseStepKind::Skip => "Skip",
		TraverseStepKind::Repeat => "Repeat",
		TraverseStepKind::Branch => "Branch",
		TraverseStepKind::Choose => "Choose",
		TraverseStepKind::Coalesce => "Coalesce",
		TraverseStepKind::Order => "Order",
		TraverseStepKind::Group => "Group",
		TraverseStepKind::Fold => "Fold",
		TraverseStepKind::Count => "Count",
		TraverseStepKind::Sum => "Sum",
		TraverseStepKind::Minimum => "Minimum",
		TraverseStepKind::Maximum => "Maximum",
		TraverseStepKind::Average => "Average",
		TraverseStepKind::KHop => "KHop",
		TraverseStepKind::KHopEstimate => "KHopEstimate",
		TraverseStepKind::Reachable => "Reachable",
		TraverseStepKind::ShortestPath => "ShortestPath",
		TraverseStepKind::WeightedShortestPath => "WeightedShortestPath",
		TraverseStepKind::AllShortestPaths => "AllShortestPaths",
		TraverseStepKind::AllWeightedShortestPaths => "AllWeightedShortestPaths",
		TraverseStepKind::DistanceTo => "DistanceTo",
		TraverseStepKind::BellmanFord => "BellmanFord",
		TraverseStepKind::TopologicalSort => "TopologicalSort",
		TraverseStepKind::StronglyConnectedComponents => "StronglyConnectedComponents",
		TraverseStepKind::WeaklyConnectedComponents => "WeaklyConnectedComponents",
		TraverseStepKind::Condensation => "Condensation",
		TraverseStepKind::MinimumSpanningForest => "MinimumSpanningForest",
		TraverseStepKind::Cuts => "Cuts",
		TraverseStepKind::LowestCommonAncestor => "LowestCommonAncestor",
		TraverseStepKind::HasCycle => "HasCycle",
		TraverseStepKind::PageRank => "PageRank",
		TraverseStepKind::Degree => "Degree",
		TraverseStepKind::HarmonicCloseness => "HarmonicCloseness",
		TraverseStepKind::Betweenness => "Betweenness",
		TraverseStepKind::Eigenvector => "Eigenvector",
		TraverseStepKind::Katz => "Katz",
		TraverseStepKind::Triangles => "Triangles",
		TraverseStepKind::Clustering => "Clustering",
		TraverseStepKind::LabelPropagation => "LabelPropagation",
		TraverseStepKind::Louvain => "Louvain",
	}
}

fn match_operator_name(operator: &MatchOperator) -> &'static str {
	match operator {
		MatchOperator::MatchSeed => "MatchSeed",
		MatchOperator::MatchJoin => "MatchJoin",
		MatchOperator::FixedExpand {
			..
		} => "FixedExpand",
		MatchOperator::AtomExpand {
			..
		} => "AtomExpand",
		MatchOperator::ResolveNestedEndpoint => "ResolveNestedEndpoint",
		MatchOperator::PathEnumerate {
			..
		} => "PathEnumerate",
		MatchOperator::BreadthFirstShortest {
			..
		} => "BreadthFirstShortest",
		MatchOperator::DijkstraShortest {
			..
		} => "DijkstraShortest",
		MatchOperator::LandmarkShortest {
			..
		} => "LandmarkShortest",
		MatchOperator::TensorAllShortest {
			weighted: false,
			..
		} => "TensorAllShortest",
		MatchOperator::TensorAllShortest {
			weighted: true,
			..
		} => "TensorAllWeightedShortest",
		MatchOperator::HyperContains => "HyperContains",
		MatchOperator::HyperExpand {
			..
		} => "HyperExpand",
		MatchOperator::WithinJoin => "WithinJoin",
		MatchOperator::TransitiveWithin {
			..
		} => "TransitiveWithin",
		MatchOperator::OptionalFallback => "OptionalFallback",
		MatchOperator::BindingDistinct => "BindingDistinct",
		MatchOperator::MatchProject => "MatchProject",
		MatchOperator::ProjectionDistinct => "ProjectionDistinct",
		MatchOperator::MatchOrder => "MatchOrder",
	}
}

fn project_match_operator(node: &mut Object, operator: &MatchOperator) {
	match operator {
		MatchOperator::FixedExpand {
			direction,
			..
		}
		| MatchOperator::AtomExpand {
			direction,
			..
		}
		| MatchOperator::HyperExpand {
			direction,
			..
		} => {
			node.insert("direction", format!("{direction:?}"));
		}
		MatchOperator::PathEnumerate {
			direction,
			mode,
			min,
			max,
			..
		}
		| MatchOperator::BreadthFirstShortest {
			direction,
			mode,
			min,
			max,
		}
		| MatchOperator::DijkstraShortest {
			direction,
			mode,
			min,
			max,
			..
		} => {
			node.insert("direction", format!("{direction:?}"));
			node.insert("path_mode", format!("{mode:?}"));
			node.insert("minimum_depth", u64::from(*min));
			node.insert("maximum_depth", u64::from(*max));
		}
		MatchOperator::TensorAllShortest {
			projection,
			direction,
			mode,
			min,
			max,
			weighted,
			..
		} => {
			project_tensor(node, projection);
			node.insert("direction", format!("{direction:?}"));
			node.insert("path_mode", format!("{mode:?}"));
			node.insert("minimum_depth", u64::from(*min));
			node.insert("maximum_depth", u64::from(*max));
			node.insert("weighted", *weighted);
			node.insert("enumeration", "complete minimal alternatives");
		}
		MatchOperator::TransitiveWithin {
			min,
			max,
		} => {
			node.insert("minimum_depth", u64::from(*min));
			node.insert("maximum_depth", u64::from(*max));
		}
		MatchOperator::LandmarkShortest {
			direction,
			mode,
			min,
			max,
			binding,
		} => {
			node.insert("direction", format!("{direction:?}"));
			node.insert("path_mode", format!("{mode:?}"));
			node.insert("minimum_depth", u64::from(*min));
			node.insert("maximum_depth", u64::from(*max));
			node.insert("index_id", u64::from(binding.index.get()));
			node.insert("index_version", u64::from(binding.version.get()));
			node.insert("build_revision", binding.revision);
			node.insert("projection", format!("{:?}", binding.mode));
			node.insert("cost", format!("{:?}", binding.cost));
			node.insert("landmarks", u64::from(binding.landmarks));
			node.insert("artifact_bytes", binding.bytes);
			node.insert("post_filter", "exact bounded path search");
			node.insert("fallback", "unbound target or unavailable generation");
		}
		MatchOperator::MatchSeed
		| MatchOperator::MatchJoin
		| MatchOperator::ResolveNestedEndpoint
		| MatchOperator::HyperContains
		| MatchOperator::WithinJoin
		| MatchOperator::OptionalFallback
		| MatchOperator::BindingDistinct
		| MatchOperator::MatchProject
		| MatchOperator::ProjectionDistinct
		| MatchOperator::MatchOrder => {}
	}
	if let MatchOperator::FixedExpand {
		cost: Some((function, version, epoch, stateful)),
		..
	}
	| MatchOperator::AtomExpand {
		cost: Some((function, version, epoch, stateful)),
		..
	}
	| MatchOperator::HyperExpand {
		cost: Some((function, version, epoch, stateful)),
		..
	}
	| MatchOperator::PathEnumerate {
		cost: Some((function, version, epoch, stateful)),
		..
	}
	| MatchOperator::DijkstraShortest {
		cost: Some((function, version, epoch, stateful)),
		..
	}
	| MatchOperator::TensorAllShortest {
		cost: Some((function, version, epoch, stateful)),
		..
	} = operator
	{
		node.insert("cost_function_id", u64::from(function.get()));
		node.insert("cost_function_version", u64::from(version.get()));
		node.insert("cost_schema_epoch", *epoch);
		node.insert("cost_stateful", *stateful);
		node.insert("cost_execution", "ON_DEMAND");
	}
}

/// Projects logical tensor metadata without exposing pages or private payloads.
fn project_tensor(node: &mut Object, projection: &crate::gql::optimizer::TensorProjectionPlan) {
	node.insert(
		"projection",
		if projection.mode == crate::graph::TraversalSemantics::Atom {
			"ATOM"
		} else {
			"NODE"
		},
	);
	node.insert(
		"scope",
		projection.scope.map_or("DYNAMIC", |scope| {
			if scope == crate::graph::tensor::TensorScope::Local {
				"LOCAL"
			} else {
				"ROOTED"
			}
		}),
	);
	node.insert(
		"direction",
		projection.direction.map_or("DYNAMIC", |direction| match direction {
			crate::graph::TraversalDirection::Forward => "OUT",
			crate::graph::TraversalDirection::Backward => "IN",
			crate::graph::TraversalDirection::Both => "BOTH",
		}),
	);
	node.insert(
		"weights",
		projection.weights.map_or("DYNAMIC", |weights| {
			if weights == crate::graph::tensor::WeightSelection::Stored {
				"STORED"
			} else {
				"UNWEIGHTED"
			}
		}),
	);
	node.insert(
		"labels",
		projection.labels.as_ref().map_or(Value::None, |labels| {
			Value::from(Array::new(
				labels.iter().map(|label| Value::from(label.as_ref())).collect(),
			))
		}),
	);
	node.insert("parameters_hash", projection.parameters_hash);
	node.insert("fallback", "exact_on_demand");
	if let Some(binding) = &projection.binding {
		node.insert("graph_id", binding.graph.get());
		node.insert("index_id", binding.index.get());
		node.insert("index_version", binding.version.get());
		node.insert("build_revision", binding.revision);
		node.insert("fingerprint", binding.fingerprint);
		node.insert(
			"format",
			if binding.format == crate::datastore::TensorCacheFormat::Csr {
				"CSR"
			} else {
				"DENSE"
			},
		);
		node.insert("rows", binding.rows);
		node.insert("connectors", binding.connectors);
		node.insert("source_cells", binding.source_cells);
		node.insert("target_cells", binding.target_cells);
		node.insert("artifact_bytes", binding.bytes);
	} else {
		node.insert(
			"format",
			if projection.dynamic_binding {
				"DYNAMIC"
			} else {
				"ON_DEMAND"
			},
		);
	}
}

fn select_nodes(plan: &PhysicalSelectPlan, snapshot: &PlannerSnapshot) -> Vec<Value> {
	let mut nodes = Vec::new();
	for source in &plan.sources {
		let mut node = node_object(
			source.node_id,
			&source_operator_name(&source.operator),
			Some(source.cardinality),
			Some(source.cost),
		);
		node.insert("alias", source.alias.as_ref());
		if let SourceOperator::ScalarIndexScan {
			index,
			name,
			access,
			direction,
		} = &source.operator
		{
			node.insert("index_id", u64::from(index.get()));
			node.insert("index_name", name.as_ref());
			if let Some(index) = planner_index(snapshot, *index) {
				node.insert("index_state", format!("{:?}", index.state));
				node.insert("index_scope", format!("{:?}", index.scope));
				node.insert("index_schema_version", u64::from(index.version.get()));
			}
			node.insert("bounds", format!("{access:?}"));
			node.insert("direction", format!("{direction:?}"));
			node.insert("residual_predicate", plan.pushed_filter);
		} else if let SourceOperator::FulltextIndexScan {
			index,
			name,
		}
		| SourceOperator::HnswSearch {
			index,
			name,
		}
		| SourceOperator::DiskAnnSearch {
			index,
			name,
		}
		| SourceOperator::GeometryIndexScan {
			index,
			name,
			..
		} = &source.operator
		{
			node.insert("index_id", u64::from(index.get()));
			node.insert("index_name", name.as_ref());
			if let Some(index) = planner_index(snapshot, *index) {
				node.insert("index_state", format!("{:?}", index.state));
				node.insert("index_scope", format!("{:?}", index.scope));
				node.insert("index_schema_version", u64::from(index.version.get()));
			}
			if matches!(source.operator, SourceOperator::GeometryIndexScan { .. }) {
				node.insert("post_filter", "exact spatial predicate");
				node.insert("candidates", source.cardinality.expected);
				node.insert("delta", "replacement MBR / tombstone");
				node.insert(
					"fallback",
					"exact scan when automatic binding is unavailable or corrupt",
				);
			}
		}
		nodes.push(Value::Object(node));
	}
	for join in &plan.joins {
		let mut node = node_object(
			join.node_id,
			&format!("{:?}Join", join.operator),
			Some(join.cardinality),
			Some(join.cost),
		);
		node.insert("join_kind", format!("{:?}", join.kind));
		node.insert("build_side", format!("{:?}", join.build_side));
		node.insert(
			"children",
			Value::Array(
				join.left_sources
					.iter()
					.chain(std::iter::once(&join.right_source))
					.map(|source| Value::from(u64::try_from(*source).unwrap_or(u64::MAX)))
					.collect::<Vec<_>>()
					.into(),
			),
		);
		nodes.push(Value::Object(node));
	}
	nodes
}

fn node_object(
	id: u32,
	operator: &str,
	estimate: Option<Cardinality>,
	cost: Option<PlanCost>,
) -> Object {
	let mut node = Object::new();
	node.insert("node_id", u64::from(id));
	node.insert("operator", operator);
	node.insert("children", Value::Array(Vec::<Value>::new().into()));
	if let Some(estimate) = estimate {
		node.insert("estimate", Value::Object(cardinality_object(estimate)));
	}
	if let Some(cost) = cost {
		node.insert("cost", Value::Object(cost_object(cost)));
	}
	node
}

fn planner_index(
	snapshot: &PlannerSnapshot,
	index: crate::datastore::IndexId,
) -> Option<&crate::gql::optimizer::PlannerIndex> {
	snapshot
		.classes
		.values()
		.flat_map(|class| &class.indexes)
		.find(|candidate| candidate.id == index)
}

fn statistics_object(snapshot: &PlannerSnapshot) -> Object {
	let mut object = Object::new();
	object.insert("revision", snapshot.base_statistics.revision);
	object.insert("analyzed_revision", snapshot.base_statistics.analyzed_revision);
	object.insert("dirty_rows", snapshot.base_statistics.dirty_rows);
	object.insert(
		"property_histograms",
		Value::Array(
			snapshot
				.property_histograms
				.values()
				.map(|histogram| {
					let mut value = Object::new();
					value.insert("class_id", u64::from(histogram.key.class_id.get()));
					value.insert("field_id", u64::from(histogram.key.field_id.get()));
					value.insert("scope", format!("{:?}", histogram.key.scope));
					value.insert("method", format!("{:?}", histogram.method));
					Value::Object(value)
				})
				.collect::<Vec<_>>()
				.into(),
		),
	);
	object
}

fn ordering_projection(
	statement: &LogicalStatement,
	plan: &PhysicalPlan,
) -> (Vec<Value>, &'static str) {
	if let PhysicalPlan::Changefeed(plan) = plan {
		return (
			vec![Value::from("versionstamp"), Value::from("ordinal")],
			if plan.mode.is_some() {
				"whole_commit_order"
			} else {
				"not_applicable"
			},
		);
	}
	if let (LogicalStatement::Traverse(statement), PhysicalPlan::Traversal(plan)) =
		(statement, plan)
	{
		let ordered = statement.steps.iter().any(|step| step.kind == TraverseStepKind::Order);
		return (
			ordered.then(|| Value::from("tv::order")).into_iter().collect(),
			if ordered {
				"traversal_barrier_order"
			} else if plan.nodes.iter().any(|node| node.unordered) {
				"unordered_fast"
			} else {
				"encounter_order"
			},
		);
	}
	let LogicalStatement::Select(select) = statement else {
		return (Vec::new(), "not_applicable");
	};
	let required = select
		.order
		.iter()
		.map(|order| {
			Value::from(if order.random {
				"RAND".to_owned()
			} else {
				format!(
					"{:?} {:?}{}{}",
					order.direction,
					order.nulls,
					if order.collate {
						" COLLATE"
					} else {
						""
					},
					if order.numeric {
						" NUMERIC"
					} else {
						""
					}
				)
			})
		})
		.collect();
	let provided = match plan {
		PhysicalPlan::Select(_) if select.order.is_empty() => "stable_result_key",
		PhysicalPlan::Select(plan) if plan.requires_sort => "external_sort",
		PhysicalPlan::Select(_) => "index_order",
		_ => "not_applicable",
	};
	(required, provided)
}

fn push_node(
	nodes: &mut Vec<Value>,
	id: u32,
	operator: &str,
	estimate: Option<Cardinality>,
	cost: Option<PlanCost>,
) {
	nodes.push(Value::Object(node_object(id, operator, estimate, cost)));
}

fn cardinality_object(value: Cardinality) -> Object {
	let mut object = Object::new();
	object.insert("lower", value.lower);
	object.insert("expected", value.expected);
	object.insert("upper", value.upper);
	object
}

fn cost_object(value: PlanCost) -> Object {
	let mut object = Object::new();
	object.insert("startup", value.startup);
	object.insert("sequential", value.sequential);
	object.insert("random", value.random);
	object.insert("expression_cpu", value.expression);
	object.insert("memory", value.memory);
	object.insert("spill", value.spill);
	object.insert("risk", value.risk);
	object.insert("total", value.total());
	object
}

fn observation_object(value: &AnalyzeObservation) -> Object {
	let mut object = Object::new();
	object.insert("actual_rows", value.actual_rows);
	object.insert("pulls", value.pulls);
	object.insert("affected_rows", value.affected_rows);
	object.insert("elapsed_ns", value.elapsed_ns);
	object.insert("spill_bytes", value.spill_bytes);
	object.insert("spill_runs", value.spill_runs);
	object.insert("peak_accounted_memory", value.peak_memory);
	object.insert("path_expansions", value.path_expansions);
	object.insert("algorithm_work", value.match_metrics.algorithm_work);
	object.insert("algorithm_paths", value.match_metrics.algorithm_paths as u64);
	object.insert("tensor_cache_hits", value.match_metrics.tensor_cache_hits);
	object.insert("tensor_fallbacks", value.match_metrics.tensor_fallbacks);
	object.insert("tensor_cells", value.match_metrics.tensor_cells);
	object.insert("tensor_nonzeros", value.match_metrics.tensor_nonzeros);
	object.insert("tensor_iterations", value.match_metrics.tensor_iterations);
	object.insert("tensor_artifact_bytes", value.match_metrics.tensor_artifact_bytes);
	object.insert("landmark_lookups", value.match_metrics.landmark_lookups);
	object.insert("landmark_fallbacks", value.match_metrics.landmark_fallbacks);
	object.insert("landmark_artifact_bytes", value.match_metrics.landmark_artifact_bytes);
	object.insert("match_seeds", value.match_metrics.seeds);
	object.insert("frontier_peak", value.match_metrics.frontier_peak);
	object.insert("trace_nodes", value.match_metrics.trace_nodes);
	object.insert("graph_opens", value.match_metrics.graph_opens);
	object.insert("containment_links", value.match_metrics.containment_links);
	object.insert("hyper_assignments", value.match_metrics.hyper_assignments);
	object.insert("cancellation_checks", value.match_metrics.cancellation_checks);
	object.insert("physical_windows", value.match_metrics.physical_windows);
	object.insert("physical_resident_pages", value.match_metrics.physical_resident_pages);
	object.insert("physical_wal_pages", value.match_metrics.physical_wal_pages);
	object.insert("physical_sync_pages", value.match_metrics.physical_sync_pages);
	object.insert("physical_ioring_pages", value.match_metrics.physical_ioring_pages);
	object.insert("graph_source_rows", value.graph_assembly.source_rows);
	object.insert("graph_selected_nodes", value.graph_assembly.selected_nodes);
	object.insert("graph_selected_edges", value.graph_assembly.selected_edges);
	object.insert("graph_closure_nodes", value.graph_assembly.closure_nodes);
	object.insert("graph_closure_edges", value.graph_assembly.closure_edges);
	object.insert("graph_deduplicated_values", value.graph_assembly.deduplicated_values);
	object.insert("graph_final_nodes", value.graph_assembly.final_nodes);
	object.insert("graph_final_edges", value.graph_assembly.final_edges);
	object.insert("graph_child_graphs", value.graph_assembly.child_graphs);
	object.insert("graph_scope_slots", value.graph_assembly.scope_slots);
	object.insert("graph_validation_ns", value.graph_assembly.validation_ns);
	object.insert("spatial_candidates", value.spatial[0]);
	object.insert("spatial_refinements", value.spatial[1]);
	object.insert("spatial_fallbacks", value.spatial[2]);
	object.insert("spatial_tree_pages", value.spatial[3]);
	object.insert("spatial_delta_records", value.spatial[4]);
	object.insert("neighbourhood_sketch_reads", value.neighbourhood[0]);
	object.insert("neighbourhood_fallbacks", value.neighbourhood[1]);
	object.insert("neighbourhood_exact_work", value.neighbourhood[2]);
	object.insert("graph_artifact_bytes", value.neighbourhood[3]);
	object.insert("publication_state", value.publication);
	for (name, counter) in [
		"changefeed_batches",
		"changefeed_entries",
		"changefeed_gaps",
		"changefeed_groups_inspected",
		"changefeed_page_reads",
		"changefeed_bytes_read",
		"changefeed_segment_visits",
		"changefeed_work",
		"changefeed_peak_scratch_bytes",
	]
	.into_iter()
	.zip(value.changefeed)
	{
		object.insert(name, counter);
	}
	object
}

fn plan_warnings(plan: &PhysicalPlan) -> Vec<Value> {
	let warnings = match plan {
		PhysicalPlan::Select(plan) => &plan.warnings,
		PhysicalPlan::Match(plan) => &plan.warnings,
		PhysicalPlan::Traversal(plan) => &plan.warnings,
		PhysicalPlan::Write(_) | PhysicalPlan::Changefeed(_) | PhysicalPlan::Live(_) => {
			return Vec::new();
		}
		PhysicalPlan::GraphResult(plan) => &plan.warnings,
	};
	let mut values = warnings
		.iter()
		.map(|warning| {
			let (code, detail) = match warning {
				crate::gql::optimizer::PlanWarning::StatisticsMissing(detail) => {
					("statistics_missing", detail)
				}
				crate::gql::optimizer::PlanWarning::StatisticsStale(detail) => {
					("statistics_stale", detail)
				}
				crate::gql::optimizer::PlanWarning::IndexUnavailable(detail) => {
					("index_unavailable", detail)
				}
				crate::gql::optimizer::PlanWarning::ResourceRisk(detail) => {
					("resource_risk", detail)
				}
				crate::gql::optimizer::PlanWarning::ExecutionLimited(detail) => {
					("execution_limited", detail)
				}
			};
			Value::from(format!("{code}: {detail}"))
		})
		.collect::<Vec<_>>();
	if let PhysicalPlan::GraphResult(plan) = plan
		&& let Some(source) = &plan.source
	{
		values.extend(plan_warnings(source));
	}
	values
}

fn plan_rules(plan: &PhysicalPlan) -> Vec<Value> {
	let PhysicalPlan::Select(plan) = plan else {
		return Vec::new();
	};
	plan.rules.iter().map(|rule| Value::from(format!("{rule:?}"))).collect()
}

fn source_operator_name(operator: &SourceOperator) -> &'static str {
	match operator {
		SourceOperator::PointGet(_) => "PointGet",
		SourceOperator::ClassScan {
			..
		} => "ClassScan",
		SourceOperator::GraphClassScan {
			..
		} => "GraphClassScan",
		SourceOperator::ScalarIndexScan {
			..
		} => "ScalarIndexScan",
		SourceOperator::FulltextIndexScan {
			..
		} => "FulltextIndexScan",
		SourceOperator::HnswSearch {
			..
		} => "HnswSearch",
		SourceOperator::DiskAnnSearch {
			..
		} => "DiskAnnSearch",
		SourceOperator::CountLookup {
			..
		} => "CountLookup",
		SourceOperator::GeometryIndexScan {
			..
		} => "GeometryIndexScan",
		SourceOperator::GraphCatalogScan => "GraphCatalogScan",
		SourceOperator::Derived => "Derived",
		SourceOperator::AtomUnion {
			..
		} => "AtomUnionScan",
	}
}

fn source_name(source: &LogicalSelectSourceKind) -> &'static str {
	match source {
		LogicalSelectSourceKind::Empty {
			..
		} => "EmptySource",
		LogicalSelectSourceKind::Atoms {
			..
		} => "AtomSource",
		LogicalSelectSourceKind::AtomUnion {
			..
		} => "AtomUnionSource",
		LogicalSelectSourceKind::Graphs {
			..
		} => "GraphSource",
		LogicalSelectSourceKind::Derived(_) => "DerivedSource",
	}
}

fn statement_name(statement: &LogicalStatement) -> &'static str {
	if let Some((_, operation)) = live_planning::command(statement) {
		return operation.name();
	}
	match statement {
		LogicalStatement::Select(_) => "SELECT",
		LogicalStatement::Match(_) => "MATCH",
		LogicalStatement::Traverse(_) => "TRAVERSE",
		LogicalStatement::ReturnGraph(_) => "RETURN GRAPH",
		LogicalStatement::BuildGraph(_) => "BUILD GRAPH",
		LogicalStatement::Dml(statement) => match statement.kind {
			LogicalDmlKind::Create => "CREATE",
			LogicalDmlKind::Update => "UPDATE",
			LogicalDmlKind::Upsert => "UPSERT",
			LogicalDmlKind::Delete => "DELETE",
		},
		LogicalStatement::GraphDml(statement) => match statement {
			LogicalGraphDmlStatement::Attach {
				..
			} => "ATTACH",
			LogicalGraphDmlStatement::Detach {
				..
			} => "DETACH",
			LogicalGraphDmlStatement::Relate {
				..
			} => "RELATE",
		},
		LogicalStatement::Schema(statement) => match statement.statement_kind() {
			QueryStatementKind::ShowChanges => "SHOW CHANGES",
			QueryStatementKind::Info => "INFO",
			QueryStatementKind::Define => "DEFINE",
			QueryStatementKind::Remove => "REMOVE",
			QueryStatementKind::Alter => "ALTER",
			QueryStatementKind::Update => "UPDATE GRAPH",
			_ => "DDL",
		},
		_ => "STATEMENT",
	}
}

fn mode_name(mode: ExplainMode) -> &'static str {
	match mode {
		ExplainMode::Logical => "LOGICAL",
		ExplainMode::Full => "FULL",
		ExplainMode::Analyze => "ANALYZE",
	}
}

fn logical_mutates(statement: &LogicalStatement) -> bool {
	if let Some((schema, _)) = live_planning::command(statement) {
		return !schema.is_read();
	}
	if let LogicalStatement::Schema(schema) = statement {
		return !schema.is_read();
	}
	matches!(
		statement,
		LogicalStatement::Schema(_)
			| LogicalStatement::Dml(_)
			| LogicalStatement::GraphDml(_)
			| LogicalStatement::BuildGraph(crate::gql::ir::LogicalBuildGraph {
				name: Some(_),
				..
			})
	)
}

fn result_rows(value: &Value) -> u64 {
	match value {
		Value::Array(values) => u64::try_from(values.len()).unwrap_or(u64::MAX),
		Value::None => 0,
		_ => 1,
	}
}

fn compact_node(value: &Value) -> String {
	let Value::Object(object) = value else {
		return value_text(value);
	};
	let id = object.get("node_id").map_or_else(|| "?".to_owned(), value_text);
	let operator = object.get("operator").map_or_else(|| "?".to_owned(), value_text);
	let mut text = format!("#{id} {operator}");
	if operator.starts_with("Live") || object.get("live_command").is_some() {
		for field in [
			"live_command",
			"live_id",
			"definition_version",
			"snapshot_through",
			"read_strategy",
			"delivery",
			"since",
			"operand_binding",
			"sources",
			"output",
			"overflow",
			"queue_entries",
			"queue_bytes",
			"scratch_bytes",
			"work_limit",
			"descriptor_hash",
			"function_fingerprint",
		] {
			if let Some(value) = object.get(field) {
				text.push(' ');
				text.push_str(field);
				text.push('=');
				text.push_str(&value_text(value));
			}
		}
	}
	if operator.starts_with("Changefeed") {
		for field in [
			"source",
			"binding",
			"read_mode",
			"capture_revision",
			"capture_full",
			"snapshot_through",
			"purged_through",
			"since",
			"resume",
			"whole_commit_limit",
		] {
			if let Some(value) = object.get(field) {
				text.push(' ');
				text.push_str(field);
				text.push('=');
				text.push_str(&value_text(value));
			}
		}
	}
	text
}

fn value_text(value: &Value) -> String {
	match value {
		Value::String(value) => value.to_string(),
		Value::Number(value) => value.to_string(),
		_ => format!("{value:?}"),
	}
}

fn nanos(value: std::time::Duration) -> u64 {
	u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
}
