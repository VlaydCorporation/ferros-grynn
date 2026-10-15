#![allow(
	clippy::missing_docs_in_private_items,
	reason = "private fixed-point helpers directly implement the documented traversal plan \
	          encoding"
)]

//! Fixed-point physical planning for the streaming traversal VM.

use super::{
	Cardinality,
	CostWeights,
	PhysicalTraversalNode,
	PhysicalTraversalPlan,
	PlanCost,
	PlanWarning,
	PlannerGraph,
	PlannerSnapshot,
	TraversalOperator,
	TraversalSourceOperator,
	scan_cost,
};
use crate::{
	gql::ir::{
		LogicalGraphReference,
		LogicalTraverse,
		LogicalTraverseSource,
		TraverseStepKind,
		TypedExpr,
		TypedExprKind,
	},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	values::Value,
};

/// Builds the physical traversal sequence and conservative resource estimates.
pub(in crate::gql) fn plan_traverse(
	statement: &LogicalTraverse,
	snapshot: &PlannerSnapshot,
	barrier_bytes: usize,
	deterministic: bool,
	fast: bool,
) -> FerrosGrynnResult<PhysicalTraversalPlan> {
	let (source_operator, mut cardinality, mut cost, mut warnings, source_plan_hash) =
		match &statement.source {
			LogicalTraverseSource::Expression(expression) => {
				let cardinality = expression_cardinality(expression);
				(
					TraversalSourceOperator::Value,
					cardinality,
					scan_cost(cardinality, 1, CostWeights::default()),
					Vec::new(),
					None,
				)
			}
			LogicalTraverseSource::Match(child) => {
				let plan = super::plan_match(child, snapshot, deterministic)?;
				let hash = super::PhysicalPlan::Match(plan.clone()).hash();
				(
					TraversalSourceOperator::Match,
					plan.cardinality,
					plan.cost,
					plan.warnings,
					Some(hash),
				)
			}
			LogicalTraverseSource::Select(child) => {
				let (_logical, plan) = super::prepare_select(child, snapshot, barrier_bytes)?;
				let hash = super::PhysicalPlan::Select(plan.clone()).hash();
				(
					TraversalSourceOperator::Select,
					plan.cardinality,
					plan.cost,
					plan.warnings,
					Some(hash),
				)
			}
		};
	let has_side_effect =
		statement.steps.iter().any(|step| step.kind == TraverseStepKind::SideEffect);
	if fast && has_side_effect {
		warnings.push(PlanWarning::ExecutionLimited(
			"FAST keeps traversal side effects in source order".into(),
		));
	}
	let graph = static_graph(statement).and_then(|graph| snapshot.graph(graph));
	let mut nodes = vec![PhysicalTraversalNode {
		node_id: 0,
		step_index: None,
		operator: TraversalOperator::Source(source_operator),
		unordered: fast && !has_side_effect,
		cardinality,
		cost,
		nested_plan_hash: source_plan_hash,
	}];
	for (step_index, step) in statement.steps.iter().enumerate() {
		let input = cardinality;
		let (step_cost, warning, nested_plan_hash) = if let Some(child) = &step.match_plan {
			let child = super::plan_match(child, snapshot, deterministic)?;
			cardinality = correlated_cardinality(input, child.cardinality);
			warnings.extend(child.warnings.iter().cloned());
			(
				repeated_cost(child.cost, input.expected.max(1)),
				None,
				Some(super::PhysicalPlan::Match(child).hash()),
			)
		} else {
			cardinality = estimate_step(step.kind, &step.arguments, input, graph);
			let (cost, warning) = step_cost(step.kind, input, barrier_bytes);
			(cost, warning, None)
		};
		cost = cost.saturating_add(step_cost);
		warnings.extend(warning);
		let ordered_from_here = statement.steps[step_index..]
			.iter()
			.any(|candidate| candidate.kind == TraverseStepKind::SideEffect);
		nodes.push(PhysicalTraversalNode {
			node_id: u32::try_from(step_index + 1).unwrap_or(u32::MAX),
			step_index: Some(step_index),
			operator: if step.kind.is_tensor_analytics()
				|| matches!(
					step.kind,
					TraverseStepKind::AllShortestPaths | TraverseStepKind::AllWeightedShortestPaths
				) {
				TraversalOperator::Tensor {
					kind: step.kind,
					projection: tensor_projection(statement, step, graph),
				}
			} else if step.kind == TraverseStepKind::KHopEstimate {
				graph
					.and_then(|graph| {
						constant_u64(step.arguments.first()).and_then(|radius| {
							graph
								.neighbourhood_sketches
								.iter()
								.filter(|index| {
									index.mode
										== if statement.atom {
											crate::graph::TraversalSemantics::Atom
										} else {
											crate::graph::TraversalSemantics::Node
										}
										&& index.direction
											== crate::graph::TraversalDirection::Forward
										&& radius != 0
										&& radius <= u64::from(index.radius)
								})
								.min_by_key(|index| {
									(std::cmp::Reverse(index.precision), index.bytes, index.index)
								})
								.copied()
						})
					})
					.map_or(
						TraversalOperator::Step(step.kind),
						TraversalOperator::NeighbourhoodSketch,
					)
			} else if matches!(
				step.kind,
				TraverseStepKind::ShortestPath
					| TraverseStepKind::WeightedShortestPath
					| TraverseStepKind::DistanceTo
			) {
				graph
					.and_then(|graph| {
						graph
							.landmark_paths
							.iter()
							.filter(|index| {
								index.mode == crate::graph::TraversalSemantics::Node
									&& index.direction == crate::graph::TraversalDirection::Forward
									&& statement.custom_cost.is_none_or(|cost| !cost.stateful)
									&& index.cost
										== if step.kind == TraverseStepKind::ShortestPath {
											crate::datastore::GraphIndexCost::Hop
										} else if let Some(cost) = statement.custom_cost {
											crate::datastore::GraphIndexCost::Function {
												function: cost.function.id,
												version: cost.function.version,
											}
										} else {
											crate::datastore::GraphIndexCost::Weighted
										}
									&& index.edge_labels.is_empty()
							})
							.min_by_key(|index| {
								(std::cmp::Reverse(index.landmarks), index.bytes, index.index)
							})
							.cloned()
					})
					.map_or(TraversalOperator::Step(step.kind), |index| {
						if step.kind == TraverseStepKind::ShortestPath {
							TraversalOperator::LandmarkShortest(index)
						} else {
							TraversalOperator::LandmarkWeighted {
								kind: step.kind,
								index,
							}
						}
					})
			} else {
				TraversalOperator::Step(step.kind)
			},
			unordered: fast && !ordered_from_here && step.kind != TraverseStepKind::Order,
			cardinality,
			cost: step_cost,
			nested_plan_hash,
		});
	}
	if statement.returning.is_some() {
		nodes.push(PhysicalTraversalNode {
			node_id: u32::try_from(nodes.len()).unwrap_or(u32::MAX),
			step_index: None,
			operator: TraversalOperator::Project,
			unordered: false,
			cardinality,
			cost: PlanCost::default(),
			nested_plan_hash: None,
		});
	}
	warnings.sort_by_key(|warning| format!("{warning:?}"));
	warnings.dedup();
	Ok(PhysicalTraversalPlan {
		nodes,
		cardinality,
		cost,
		warnings,
		custom_cost: statement.custom_cost.map(|cost| {
			(cost.function.id, cost.function.version, cost.function.schema_epoch, cost.stateful)
		}),
		custom_constraints: statement.custom_cost.map(|_| {
			(
				statement.constraints.mode.unwrap_or(crate::syn::MatchPathMode::Trail),
				statement.constraints.filters.clone(),
			)
		}),
	})
}

/// Captures static projection fields while hashing complete dynamic arguments.
fn tensor_projection(
	statement: &LogicalTraverse,
	step: &crate::gql::ir::LogicalTraverseStep,
	graph: Option<&PlannerGraph>,
) -> super::TensorProjectionPlan {
	use std::hash::{Hash, Hasher};

	use crate::graph::{
		TraversalDirection,
		TraversalSemantics,
		tensor::{TensorScope, WeightSelection},
	};
	let all_paths = matches!(
		step.kind,
		TraverseStepKind::AllShortestPaths | TraverseStepKind::AllWeightedShortestPaths
	);
	let options = step.arguments.get(usize::from(all_paths));
	let scope = match static_option_text(options, "scope", "LOCAL") {
		Some("LOCAL") => Some(TensorScope::Local),
		Some("ROOTED") => Some(TensorScope::Rooted),
		_ => None,
	};
	let direction = match static_option_text(options, "direction", "OUT") {
		Some("OUT") => Some(TraversalDirection::Forward),
		Some("IN") => Some(TraversalDirection::Backward),
		Some("BOTH") => Some(TraversalDirection::Both),
		_ => None,
	};
	let weights = if step.kind == TraverseStepKind::Degree {
		Some(WeightSelection::Stored)
	} else {
		match static_option_text(options, "weights", "UNWEIGHTED") {
			Some("UNWEIGHTED") => Some(WeightSelection::Unweighted),
			Some("STORED") => Some(WeightSelection::Stored),
			_ => None,
		}
	};
	let mut writer = super::CanonicalHasher::versioned(
		super::PLAN_ENCODING_VERSION,
		b"tensor-traversal-arguments",
	);
	for argument in &step.arguments {
		super::fingerprint::hash_expr(argument, &mut writer);
	}
	if all_paths {
		statement.constraints.mode.hash(&mut writer);
		statement.constraints.filters.hash(&mut writer);
	}
	super::TensorProjectionPlan {
		mode: if statement.atom {
			TraversalSemantics::Atom
		} else {
			TraversalSemantics::Node
		},
		scope,
		direction,
		weights,
		labels: static_labels(options),
		parameters_hash: writer.finish(),
		dynamic_binding: graph.is_none() || scope.is_none(),
		binding: graph.and_then(|graph| {
			graph
				.tensor_caches
				.iter()
				.filter(|binding| Some(binding.scope) == scope)
				.min_by_key(|binding| (binding.bytes, binding.index))
				.cloned()
		}),
	}
}

/// Planning-time knowledge of one traversal option without expression
/// evaluation.
enum StaticOption<'a> {
	/// The options object or selected field requires runtime evaluation.
	Dynamic,
	/// The option is known to be omitted, so its documented default applies.
	Absent,
	/// The selected option is an existing literal, including an explicit NONE.
	Literal(
		/// The selected option is an existing literal, including an explicit
		/// NONE.
		&'a Value,
	),
}

/// Classifies one option while borrowing literal values from the typed
/// expression.
fn static_option_value<'a>(options: Option<&'a TypedExpr>, key: &str) -> StaticOption<'a> {
	match options.map(|value| &value.kind) {
		None => StaticOption::Absent,
		Some(TypedExprKind::Constant(Value::Object(object))) => {
			object.get(key).map_or(StaticOption::Absent, StaticOption::Literal)
		}
		Some(TypedExprKind::Object(fields)) => {
			match fields.iter().find(|(name, _value)| name.as_ref() == key) {
				None => StaticOption::Absent,
				Some((_name, value)) => {
					if let TypedExprKind::Constant(value) = &value.kind {
						StaticOption::Literal(value)
					} else {
						StaticOption::Dynamic
					}
				}
			}
		}
		_ => StaticOption::Dynamic,
	}
}

/// Distinguishes a known default from runtime selection.
fn static_option_text<'a>(
	options: Option<&'a TypedExpr>,
	key: &str,
	default: &'a str,
) -> Option<&'a str> {
	match static_option_value(options, key) {
		StaticOption::Dynamic => None,
		StaticOption::Absent => Some(default),
		StaticOption::Literal(value) => value.as_str(),
	}
}

/// Copies only known canonical label filters into physical metadata.
fn static_labels(options: Option<&TypedExpr>) -> Option<Vec<Box<str>>> {
	let mut labels: Vec<Box<str>> = if let Some(TypedExprKind::Object(fields)) =
		options.map(|options| &options.kind)
		&& let Some((_key, value)) = fields.iter().find(|(key, _value)| key.as_ref() == "labels")
		&& let TypedExprKind::Array(values) = &value.kind
	{
		values
			.iter()
			.map(|value| match &value.kind {
				TypedExprKind::Constant(value) => value.as_str().map(Into::into),
				_ => None,
			})
			.collect::<Option<Vec<_>>>()?
	} else {
		match static_option_value(options, "labels") {
			StaticOption::Absent => Some(Vec::new()),
			StaticOption::Literal(Value::Array(values)) => {
				values.iter().map(|value| value.as_str().map(Into::into)).collect()
			}
			StaticOption::Dynamic | StaticOption::Literal(_) => None,
		}?
	};
	labels.sort_unstable();
	labels.dedup();
	Some(labels)
}

/// Returns correlated cardinality for this module.
fn correlated_cardinality(input: Cardinality, child: Cardinality) -> Cardinality {
	Cardinality::new(
		input.lower.saturating_mul(child.lower),
		input.expected.saturating_mul(child.expected),
		input.upper.saturating_mul(child.upper),
	)
}

/// Returns repeated cost for this module.
fn repeated_cost(cost: PlanCost, repetitions: u64) -> PlanCost {
	PlanCost {
		startup: cost.startup.saturating_mul(repetitions),
		sequential: cost.sequential.saturating_mul(repetitions),
		random: cost.random.saturating_mul(repetitions),
		expression: cost.expression.saturating_mul(repetitions),
		memory: cost.memory,
		spill: cost.spill.saturating_mul(repetitions),
		risk: cost.risk.saturating_mul(repetitions),
	}
}

/// Returns expression cardinality for this module.
fn expression_cardinality(expression: &TypedExpr) -> Cardinality {
	match &expression.kind {
		TypedExprKind::Constant(Value::Array(values)) => exact_len(values.len()),
		TypedExprKind::Constant(Value::Set(values)) => exact_len(values.len()),
		TypedExprKind::Constant(Value::None) => Cardinality::exact(0),
		TypedExprKind::Constant(_) => Cardinality::exact(1),
		_ => Cardinality::unknown(1_024),
	}
}

/// Returns exact len for this module.
fn exact_len(length: usize) -> Cardinality {
	Cardinality::exact(u64::try_from(length).unwrap_or(u64::MAX))
}

/// Returns static graph for this module.
fn static_graph(statement: &LogicalTraverse) -> Option<GraphId> {
	statement.graph.as_ref().and_then(static_graph_reference).or_else(|| match &statement.source {
		LogicalTraverseSource::Match(statement) => static_graph_reference(&statement.graph),
		LogicalTraverseSource::Expression(expression) => match &expression.kind {
			TypedExprKind::Constant(Value::GraphRef(graph)) => Some(graph.id()),
			_ => None,
		},
		LogicalTraverseSource::Select(_) => None,
	})
}

/// Returns static graph reference for this module.
fn static_graph_reference(reference: &LogicalGraphReference) -> Option<GraphId> {
	match reference {
		LogicalGraphReference::Static(graph) => Some(graph.id()),
		LogicalGraphReference::Expression(_) => None,
	}
}

/// Estimates step for this module.
fn estimate_step(
	kind: TraverseStepKind,
	arguments: &[TypedExpr],
	input: Cardinality,
	graph: Option<&PlannerGraph>,
) -> Cardinality {
	match kind {
		TraverseStepKind::Vertices => graph.map_or_else(
			|| Cardinality::unknown(input.upper.max(1_024)),
			|graph| Cardinality::exact(graph.node_rows),
		),
		TraverseStepKind::Edges => graph.map_or_else(
			|| Cardinality::unknown(input.upper.max(1_024)),
			|graph| Cardinality::exact(graph.edge_rows),
		),
		TraverseStepKind::Out
		| TraverseStepKind::OutEdges
		| TraverseStepKind::In
		| TraverseStepKind::InEdges
		| TraverseStepKind::Both
		| TraverseStepKind::BothEdges
		| TraverseStepKind::Hyper => expand_cardinality(kind, input, graph),
		TraverseStepKind::FlatMap
		| TraverseStepKind::AllShortestPaths
		| TraverseStepKind::AllWeightedShortestPaths
		| TraverseStepKind::Algorithm
		| TraverseStepKind::Unfold
		| TraverseStepKind::Branch
		| TraverseStepKind::Repeat => multiply(input, 4, 32),
		TraverseStepKind::Values => multiply(
			input,
			u64::try_from(arguments.len().max(1)).unwrap_or(u64::MAX),
			u64::try_from(arguments.len().max(1)).unwrap_or(u64::MAX),
		),
		TraverseStepKind::HasAttribute
		| TraverseStepKind::Has
		| TraverseStepKind::HasLabel
		| TraverseStepKind::Where
		| TraverseStepKind::SimplePath
		| TraverseStepKind::CyclicPath
		| TraverseStepKind::Deduplicate
		| TraverseStepKind::DeduplicateBy => fraction(input, 1, 2),
		TraverseStepKind::Limit => cap(input, constant_u64(arguments.first())),
		TraverseStepKind::Skip => skip(input, constant_u64(arguments.first())),
		TraverseStepKind::Group => fraction(input, 1, 2),
		TraverseStepKind::Fold
		| TraverseStepKind::Count
		| TraverseStepKind::Sum
		| TraverseStepKind::Minimum
		| TraverseStepKind::Maximum
		| TraverseStepKind::Average
		| TraverseStepKind::KHop
		| TraverseStepKind::Reachable
		| TraverseStepKind::ShortestPath
		| TraverseStepKind::WeightedShortestPath
		| TraverseStepKind::DistanceTo
		| TraverseStepKind::BellmanFord
		| TraverseStepKind::TopologicalSort
		| TraverseStepKind::StronglyConnectedComponents
		| TraverseStepKind::WeaklyConnectedComponents
		| TraverseStepKind::Condensation
		| TraverseStepKind::MinimumSpanningForest
		| TraverseStepKind::Cuts
		| TraverseStepKind::LowestCommonAncestor
		| TraverseStepKind::HasCycle
		| TraverseStepKind::PageRank
		| TraverseStepKind::Degree
		| TraverseStepKind::HarmonicCloseness
		| TraverseStepKind::Betweenness
		| TraverseStepKind::Eigenvector
		| TraverseStepKind::Katz
		| TraverseStepKind::Triangles
		| TraverseStepKind::Clustering
		| TraverseStepKind::LabelPropagation
		| TraverseStepKind::Louvain => Cardinality::exact(1),
		TraverseStepKind::KHopEstimate => input,
		TraverseStepKind::InVertex
		| TraverseStepKind::OutVertex
		| TraverseStepKind::MetaInner
		| TraverseStepKind::Map
		| TraverseStepKind::Select
		| TraverseStepKind::Match
		| TraverseStepKind::As
		| TraverseStepKind::Project
		| TraverseStepKind::InitializeSack
		| TraverseStepKind::Sack
		| TraverseStepKind::GetSack
		| TraverseStepKind::InitializeNamedSack
		| TraverseStepKind::NamedSack
		| TraverseStepKind::GetNamedSack
		| TraverseStepKind::SideEffect
		| TraverseStepKind::Path
		| TraverseStepKind::Emit
		| TraverseStepKind::Choose
		| TraverseStepKind::Coalesce
		| TraverseStepKind::Order => input,
	}
}

/// Returns expand cardinality for this module.
fn expand_cardinality(
	kind: TraverseStepKind,
	input: Cardinality,
	graph: Option<&PlannerGraph>,
) -> Cardinality {
	let (total, maximum) = graph.map_or((4, 32), |graph| match kind {
		TraverseStepKind::Out | TraverseStepKind::OutEdges => {
			(graph.total_out_degree, graph.maximum_out_degree.max(1))
		}
		TraverseStepKind::In | TraverseStepKind::InEdges => {
			(graph.total_in_degree, graph.maximum_in_degree.max(1))
		}
		_ => (
			graph
				.total_out_degree
				.saturating_add(graph.total_in_degree)
				.saturating_add(graph.total_undirected_degree),
			graph.maximum_out_degree.saturating_add(graph.maximum_in_degree).max(1),
		),
	});
	let population = graph.map_or(1, |graph| graph.node_rows.max(1));
	let average = total.div_ceil(population).max(1);
	multiply(input, average, maximum)
}

/// Returns multiply for this module.
fn multiply(input: Cardinality, expected: u64, upper: u64) -> Cardinality {
	Cardinality::new(0, input.expected.saturating_mul(expected), input.upper.saturating_mul(upper))
}

/// Returns fraction for this module.
fn fraction(input: Cardinality, numerator: u64, denominator: u64) -> Cardinality {
	Cardinality::new(0, input.expected.saturating_mul(numerator).div_ceil(denominator), input.upper)
}

/// Returns cap for this module.
fn cap(input: Cardinality, limit: Option<u64>) -> Cardinality {
	limit.map_or(input, |limit| {
		Cardinality::new(input.lower.min(limit), input.expected.min(limit), input.upper.min(limit))
	})
}

/// Returns skip for this module.
fn skip(input: Cardinality, count: Option<u64>) -> Cardinality {
	count.map_or(input, |count| {
		Cardinality::new(
			input.lower.saturating_sub(count),
			input.expected.saturating_sub(count),
			input.upper.saturating_sub(count),
		)
	})
}

/// Returns constant u64 for this module.
fn constant_u64(expression: Option<&TypedExpr>) -> Option<u64> {
	let TypedExprKind::Constant(Value::Number(number)) = &expression?.kind else {
		return None;
	};
	i64::try_from(*number).ok().and_then(|value| u64::try_from(value).ok())
}

/// Returns step cost for this module.
fn step_cost(
	kind: TraverseStepKind,
	input: Cardinality,
	barrier_bytes: usize,
) -> (PlanCost, Option<PlanWarning>) {
	let weights = CostWeights::default();
	let mut cost = scan_cost(input, 1, weights);
	if !kind.is_barrier() {
		return (cost, None);
	}
	let bytes = input.expected.saturating_mul(128);
	let budget = u64::try_from(barrier_bytes).unwrap_or(u64::MAX);
	cost.memory = bytes.min(budget).div_ceil(1_024).saturating_mul(weights.memory_kib);
	cost.spill = bytes.saturating_sub(budget).div_ceil(1_024).saturating_mul(weights.spill_kib);
	let warning = (bytes > budget).then(|| {
		PlanWarning::ResourceRisk(
			format!("tv::{kind:?} is expected to spill {} bytes", bytes - budget).into(),
		)
	});
	(cost, warning)
}
