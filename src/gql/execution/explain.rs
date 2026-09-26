//! EXPLAIN planning, projection, and optional single execution.

use super::*;
use crate::{
	gql::optimizer::{
		Cardinality,
		PhysicalPlan,
		PhysicalSelectPlan,
		PlanCost,
		PlannerDemand,
		PlannerSnapshot,
		SourceOperator,
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
	let demand = PlannerDemand::for_statement(&explain.statement);
	let planner_snapshot = read.planner_snapshot(snapshot, &demand)?;
	let deterministic_snapshot;
	let planning_snapshot = if control.deterministic() {
		deterministic_snapshot = planner_snapshot.deterministic_projection();
		&deterministic_snapshot
	} else {
		&planner_snapshot
	};
	let plan =
		physical_plan(&explain.statement, planning_snapshot, control.barrier_memory_limit())?;
	let planning_ns = nanos(planning_started.elapsed());
	let observation = if explain.mode == ExplainMode::Analyze {
		let mutation_before = read.mutation_count();
		let pulls_before = control.row_pulls();
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
			ControlFlow::Value(value) => value,
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
		LogicalStatement::Select(select) => super::select::fingerprint::fingerprints(select).0,
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
			observation,
		),
		ExplainFormat::Text => explain_text(
			explain.mode,
			&explain.statement,
			&plan,
			&planner_snapshot,
			snapshot,
			seed,
			planning_ns,
			observation,
		),
	};
	Ok(ControlFlow::Value(value))
}

fn physical_plan(
	statement: &LogicalStatement,
	snapshot: &PlannerSnapshot,
	barrier_bytes: usize,
) -> FerrosGrynnResult<PhysicalPlan> {
	match statement {
		LogicalStatement::Select(select) => {
			crate::gql::optimizer::prepare_select(select, snapshot, barrier_bytes)
				.map(|(_logical, plan)| PhysicalPlan::Select(plan))
		}
		LogicalStatement::Match(statement) => {
			crate::gql::optimizer::plan_match(statement, snapshot).map(PhysicalPlan::Match)
		}
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
	observation: Option<AnalyzeObservation>,
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
	observation: Option<AnalyzeObservation>,
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
			 peak_memory={} publication={}",
			observation.actual_rows,
			observation.pulls,
			observation.affected_rows,
			observation.elapsed_ns,
			observation.spill_bytes,
			observation.spill_runs,
			observation.peak_memory,
			observation.publication,
		));
	}
	Value::from(lines.join("\n"))
}

fn logical_nodes(statement: &LogicalStatement) -> Vec<Value> {
	let mut nodes = Vec::new();
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
		}
		LogicalStatement::Schema(_) => push_node(&mut nodes, 0, "CatalogMutation", None, None),
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
		PhysicalPlan::Match(plan) => {
			let mut node = node_object(0, "Match", Some(plan.cardinality), Some(plan.cost));
			node.insert(
				"pattern_order",
				Value::Array(
					plan.pattern_order
						.iter()
						.map(|value| Value::from(u64::try_from(*value).unwrap_or(u64::MAX)))
						.collect::<Vec<_>>()
						.into(),
				),
			);
			vec![Value::Object(node)]
		}
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
				format!("{:?} {:?}", order.direction, order.nulls)
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

fn observation_object(value: AnalyzeObservation) -> Object {
	let mut object = Object::new();
	object.insert("actual_rows", value.actual_rows);
	object.insert("pulls", value.pulls);
	object.insert("affected_rows", value.affected_rows);
	object.insert("elapsed_ns", value.elapsed_ns);
	object.insert("spill_bytes", value.spill_bytes);
	object.insert("spill_runs", value.spill_runs);
	object.insert("peak_accounted_memory", value.peak_memory);
	object.insert("publication_state", value.publication);
	object
}

fn plan_warnings(plan: &PhysicalPlan) -> Vec<Value> {
	let warnings = match plan {
		PhysicalPlan::Select(plan) => &plan.warnings,
		PhysicalPlan::Match(plan) => &plan.warnings,
		PhysicalPlan::Write(_) => return Vec::new(),
	};
	warnings
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
			};
			Value::from(format!("{code}: {detail}"))
		})
		.collect()
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
		SourceOperator::CountLookup {
			..
		} => "CountLookup",
		SourceOperator::GraphCatalogScan => "GraphCatalogScan",
		SourceOperator::Derived => "Derived",
	}
}

fn source_name(source: &LogicalSelectSourceKind) -> &'static str {
	match source {
		LogicalSelectSourceKind::Atoms {
			..
		} => "AtomSource",
		LogicalSelectSourceKind::Graphs {
			..
		} => "GraphSource",
		LogicalSelectSourceKind::Derived(_) => "DerivedSource",
	}
}

fn statement_name(statement: &LogicalStatement) -> &'static str {
	match statement {
		LogicalStatement::Select(_) => "SELECT",
		LogicalStatement::Match(_) => "MATCH",
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
	matches!(
		statement,
		LogicalStatement::Schema(_) | LogicalStatement::Dml(_) | LogicalStatement::GraphDml(_)
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
	format!("#{id} {operator}")
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
