#![allow(
	clippy::missing_docs_in_private_items,
	clippy::wildcard_imports,
	reason = "private VM operators share one closed traversal prelude and the module-level \
	          contract"
)]

//! Streaming GrynnQL traversal virtual machine.

mod algorithms;
mod all_shortest;
/// Qualified tensor-backed analytical barriers.
mod analytics;
mod barrier;
/// Regression coverage for fixed layout-shell admission.
#[cfg(test)]
mod layout_tests;
mod model;
mod navigation;
mod transform;

use std::{collections::VecDeque, rc::Rc, sync::Arc};

use model::Traverser;
use rapidhash::RapidHashMap;

use super::*;
use crate::gql::ir::NamedFunctionKey;

/// Compiled repeat body, stopping conditions and emission policy for a
/// traversal step.
#[derive(Clone)]
struct PreparedRepeat {
	/// Compiled closure evaluated for each repeat iteration.
	body: Closure,
	/// Optional fixed number of times the repeat body is evaluated.
	times: Option<usize>,
	/// Optional condition ending evaluation of the repeat body.
	until: Option<Closure>,
	/// Whether repeat iterations emit intermediate traversers.
	emit: bool,
}

/// Traversal operator with evaluated arguments and resolved analytical plans.
#[derive(Clone)]
struct PreparedStep {
	/// Selected operation or representation discriminator.
	kind: TraverseStepKind,
	/// Positional arguments supplied to the resolved operation.
	arguments: Vec<Value>,
	/// Resolved named-function identity used by the traversal step.
	named_function: Option<NamedFunctionKey>,
	/// Validated input type required by an analytical traversal operator.
	algorithm_input: Option<Type>,
	/// Resolved neighbourhood index selected for the traversal step.
	neighbourhood_index: Option<crate::datastore::IndexId>,
	/// Resolved landmark-path plan selected for the traversal step.
	landmark_path: Option<crate::datastore::QueryLandmarkPath>,
	/// Resolved tensor cache selected for analytical materialization.
	tensor_cache: Option<crate::datastore::QueryTensorCache>,
	/// Logical MATCH plan retained by the traversal operator.
	match_plan: Option<Box<LogicalMatch>>,
	/// Prepared repeat body and stopping policy, when this step repeats.
	repeat: Option<PreparedRepeat>,
}

/// Retained progress of a streaming, bounded, graph-scan or barrier traversal
/// stage.
enum StageState {
	/// Operator processes each traverser without retained bounded-stage state.
	Streaming,
	/// Remaining number of outputs permitted by a limit step.
	Limit(
		/// Remaining number of outputs permitted by a limit step.
		u64,
	),
	/// Remaining number of inputs discarded by a skip step.
	Skip(
		/// Remaining number of inputs discarded by a skip step.
		u64,
	),
	/// Optional active scan over graph atoms for the current input.
	GraphScan(
		/// Optional active scan over graph atoms for the current input.
		Option<GraphScan>,
	),
	/// Optional active materializing or sorting barrier.
	Barrier(
		/// Optional active materializing or sorting barrier.
		Option<barrier::BarrierCursor>,
	),
}

/// Traversal input and current offset while scanning one graph family.
#[derive(Clone)]
struct GraphScan {
	/// Input traverser retained while producing a graph scan result.
	input: Traverser,
	/// Owned graph reference identifying the operation scope.
	reference: Arc<GraphRef>,
	/// Selected operation or representation discriminator.
	kind: AtomKind,
	/// Current offset into the serialized records or graph scan.
	offset: usize,
}

/// Executes traverse for this module.
pub(super) fn execute_traverse<Read: QueryRead>(
	read: &mut Read,
	snapshot: Snapshot,
	plan: &LogicalTraverse,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
) -> FerrosGrynnResult<Value> {
	let mut result = Array::new(Vec::new());
	if let Some(value) = visit_traverse_values(
		read,
		snapshot,
		plan,
		slots,
		source,
		control,
		runtime,
		&mut |value| {
			result.push(value);
			Ok(())
		},
	)? {
		return Ok(value);
	}
	Ok(Value::Array(result))
}

/// Executes the prepared traversal and forwards produced values to the caller's
/// fallible visitor.
#[allow(
	clippy::too_many_arguments,
	reason = "traversal pull execution retains the VM and statement runtime context"
)]
pub(super) fn visit_traverse_values<Read: QueryRead>(
	read: &mut Read,
	snapshot: Snapshot,
	plan: &LogicalTraverse,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
	visit: &mut impl FnMut(Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Option<Value>> {
	let physical = if plan.steps.iter().any(|step| {
		step.kind.is_tensor_analytics()
			|| matches!(
				step.kind,
				TraverseStepKind::KHopEstimate
					| TraverseStepKind::ShortestPath
					| TraverseStepKind::WeightedShortestPath
					| TraverseStepKind::DistanceTo
					| TraverseStepKind::AllShortestPaths
					| TraverseStepKind::AllWeightedShortestPaths
			)
	}) {
		let demand = crate::gql::optimizer::PlannerDemand::for_traverse(plan);
		let snapshot = read.planner_snapshot(snapshot, &demand)?;
		let snapshot = if control.deterministic() {
			snapshot.deterministic_projection()
		} else {
			snapshot
		};
		Some(crate::gql::optimizer::plan_traverse(
			plan,
			&snapshot,
			control.barrier_memory_limit(),
			control.deterministic(),
			control.fast(),
		)?)
	} else {
		None
	};
	let graph = plan
		.graph
		.as_ref()
		.map(|reference| {
			select::resolve_graph_reference(read, snapshot, reference, None, slots, source, control)
		})
		.transpose()?;
	let graph = if graph.is_none() {
		let tensor_consumer = plan.steps.iter().any(|step| {
			step.kind.is_tensor_analytics()
				|| matches!(
					step.kind,
					TraverseStepKind::AllShortestPaths | TraverseStepKind::AllWeightedShortestPaths
				)
		});
		match &plan.source {
			LogicalTraverseSource::Match(statement) if tensor_consumer => {
				Some(resolve_match_graph(read, snapshot, statement, slots, source, control)?)
			}
			_ => None,
		}
	} else {
		graph
	};
	let (source_value, source_binding, retain_environment) = match &plan.source {
		LogicalTraverseSource::Expression(expression) => {
			let fields = RecordFields {
				read: &*read,
				snapshot,
				graph: graph.as_ref(),
			};
			(eval(expression, None, slots, source, Some(&fields), Some(control))?, None, false)
		}
		LogicalTraverseSource::Match(statement) => {
			let current = statement
				.bindings
				.columns
				.iter()
				.rev()
				.find(|column| matches!(column.ty, Type::AtomOccurrence(_)))
				.map(|column| column.name.clone());
			(execute_match(read, snapshot, statement, slots, source, control)?, current, true)
		}
		LogicalTraverseSource::Select(statement) => {
			(execute_select(read, snapshot, statement, slots, source, control)?, None, true)
		}
	};
	let source_queue = traversal_source(
		read,
		snapshot,
		source_value,
		graph.as_ref(),
		source_binding.as_deref(),
		retain_environment,
	)?;
	let prepared = {
		let fields = RecordFields {
			read: &*read,
			snapshot,
			graph: graph.as_ref(),
		};
		prepare_steps(plan, physical.as_ref(), slots, source, control, &fields)?
	};
	if source_queue.iter().any(|traverser| matches!(traverser.current, Value::GraphRef(_)))
		&& prepared.first().is_none_or(|step| {
			!matches!(step.kind, TraverseStepKind::Vertices | TraverseStepKind::Edges)
		}) {
		return Err(FerrosGrynnError::traversal_source_type_mismatch(
			"GraphRef followed by tv::V() or tv::E()".to_owned(),
			"GraphRef without a generating step".to_owned(),
		));
	}
	let path_tracking = plan.steps.iter().any(|step| {
		matches!(
			step.kind,
			TraverseStepKind::Path
				| TraverseStepKind::SimplePath
				| TraverseStepKind::CyclicPath
				| TraverseStepKind::ShortestPath
				| TraverseStepKind::WeightedShortestPath
		)
	});
	let mut machine = TraversalMachine::new(
		read,
		snapshot,
		prepared,
		source_queue,
		slots,
		source,
		control,
		runtime,
		path_tracking,
		plan.atom,
		plan.custom_cost,
		&plan.constraints,
		graph.clone(),
	)?;
	if let Some(graph_plan) = &plan.return_graph {
		let mut assembly = graph_result::GraphAssemblyAccumulator::new(graph, None, None, control);
		while let Some(traverser) = machine.next()? {
			control.check()?;
			assembly.record_source_row();
			let row = machine.return_row(&traverser, &plan.result);
			let fields = RecordFields {
				read: &*machine.read,
				snapshot,
				graph: None,
			};
			assembly.add_selectors(
				Some(&graph_plan.nodes),
				Some(&graph_plan.edges),
				graph_plan.root.as_ref(),
				|expression| {
					eval(expression, Some(&row), slots, source, Some(&fields), Some(control))
				},
			)?;
		}
		return assembly
			.finish(machine.read, snapshot, graph_plan.contract)
			.map(|graph| Some(Value::Graph(Box::new(graph))));
	}
	while let Some(traverser) = machine.next()? {
		control.check()?;
		let value = if let Some(projection) = &plan.returning {
			let row = machine.return_row(&traverser, &plan.result);
			let fields = RecordFields {
				read: &*machine.read,
				snapshot,
				graph: None,
			};
			eval(projection, Some(&row), slots, source, Some(&fields), Some(control))?
		} else {
			traverser.current
		};
		for _ in 0..traverser.bulk.get() {
			visit(value.clone())?;
		}
	}
	Ok(None)
}

/// Returns traversal source for this module.
fn traversal_source<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	value: Value,
	graph: Option<&GraphRef>,
	current_binding: Option<&str>,
	retain_environment: bool,
) -> FerrosGrynnResult<VecDeque<Traverser>> {
	let values = match value {
		Value::Array(values) => values.into_iter().collect(),
		Value::Set(values) => values.into_iter().collect(),
		Value::Range(range) => super::integer_range(range)?.collect(),
		value => VecDeque::from([value]),
	};
	let mut traversers: VecDeque<Traverser> = VecDeque::new();
	for value in values {
		let environment = retain_environment.then(|| match &value {
			Value::Object(object) => object.clone(),
			_ => Object::new(),
		});
		let value = if let (Some(binding), Value::Object(object)) = (current_binding, &value) {
			object.get(binding).cloned().unwrap_or(Value::None)
		} else {
			value
		};
		let current = match value {
			Value::Record(record) => {
				let graph = graph.ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"RecordId traversal sources require `IN GRAPH`.",
					)
				})?;
				let graph_view = read.graph(snapshot, graph)?;
				let atom = resolve_record_atom(&graph_view, &record)?;
				Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(
					atom,
					Arc::new(graph.clone()),
				)))
			}
			Value::Atom(atom) => {
				let graph = graph.ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"AtomRef traversal sources require `IN GRAPH`.",
					)
				})?;
				let graph_view = read.graph(snapshot, graph)?;
				if graph_view.atom_id(&atom)?.is_none() {
					return Err(FerrosGrynnError::atom_does_not_exist(
						atom.kind().to_string(),
						atom.id().to_string(),
					));
				}
				Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(
					*atom,
					Arc::new(graph.clone()),
				)))
			}
			Value::AtomOccurrence(occurrence) => {
				let graph_view = read.graph(snapshot, occurrence.graph())?;
				if graph_view.atom_id(occurrence.atom())?.is_none() {
					return Err(FerrosGrynnError::atom_does_not_exist(
						occurrence.atom().kind().to_string(),
						occurrence.atom().id().to_string(),
					));
				}
				Value::AtomOccurrence(occurrence)
			}
			value => value,
		};
		let mut traverser = Traverser::new(current);
		if let Some(environment) = environment {
			traverser.env = environment;
		}
		if let Some(last) = traversers.back_mut()
			&& last.can_merge(&traverser)
		{
			last.merge(&traverser)?;
		} else {
			traversers.push_back(traverser);
		}
	}
	Ok(traversers)
}

/// Prepares steps for this module.
fn prepare_steps(
	plan: &LogicalTraverse,
	physical: Option<&crate::gql::optimizer::PhysicalTraversalPlan>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	fields: &dyn AtomFieldRead,
) -> FerrosGrynnResult<Vec<PreparedStep>> {
	plan.steps
		.iter()
		.enumerate()
		.map(|(ordinal, step)| {
			let arguments = step
				.arguments
				.iter()
				.map(|argument| eval(argument, None, slots, source, Some(fields), Some(control)))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let repeat = step
				.repeat
				.as_ref()
				.map(|repeat| {
					let body = closure_value(eval(
						&repeat.body,
						None,
						slots,
						source,
						Some(fields),
						Some(control),
					)?)?;
					let times = repeat
						.times
						.as_ref()
						.map(|times| {
							eval_usize_value(eval(
								times,
								None,
								slots,
								source,
								Some(fields),
								Some(control),
							)?)
						})
						.transpose()?;
					let until = repeat
						.until
						.as_ref()
						.map(|until| {
							closure_value(eval(
								until,
								None,
								slots,
								source,
								Some(fields),
								Some(control),
							)?)
						})
						.transpose()?;
					Ok::<_, FerrosGrynnError>(PreparedRepeat {
						body,
						times,
						until,
						emit: repeat.emit,
					})
				})
				.transpose()?;
			Ok(PreparedStep {
				kind: step.kind,
				arguments,
				named_function: step.named_function,
				algorithm_input: step.algorithm_input.clone(),
				neighbourhood_index: physical.and_then(|plan| {
					plan.nodes.iter().find_map(|node| {
						if node.step_index != Some(ordinal) {
							return None;
						}
						match &node.operator {
							crate::gql::optimizer::TraversalOperator::NeighbourhoodSketch(
								index,
							) => Some(index.index),
							_ => None,
						}
					})
				}),
				landmark_path: physical.and_then(|plan| {
					plan.nodes.iter().find_map(|node| {
						if node.step_index == Some(ordinal)
							&& let crate::gql::optimizer::TraversalOperator::LandmarkShortest(index)
							| crate::gql::optimizer::TraversalOperator::LandmarkWeighted {
								index,
								..
							} = &node.operator
						{
							Some(index.clone())
						} else {
							None
						}
					})
				}),
				tensor_cache: physical.and_then(|plan| {
					plan.nodes.iter().find_map(|node| {
						if node.step_index == Some(ordinal)
							&& let crate::gql::optimizer::TraversalOperator::Tensor {
								projection,
								..
							} = &node.operator
						{
							projection.binding.clone()
						} else {
							None
						}
					})
				}),
				match_plan: step
					.match_plan
					.as_ref()
					.map(|plan| -> FerrosGrynnResult<_> {
						fields.admit_retained(plan.boxed_layout_bytes)?;
						Ok(plan.clone())
					})
					.transpose()?,
				repeat,
			})
		})
		.collect()
}

/// Resolves a public RecordId within exactly one graph membership scope.
fn resolve_record_atom<Graph: GraphRead>(
	graph: &Graph,
	record: &crate::values::RecordId,
) -> FerrosGrynnResult<crate::graph::AtomRef> {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		let atom = crate::graph::AtomRef::new(kind, record.clone());
		if graph.atom_id(&atom)?.is_some() {
			return Ok(atom);
		}
	}
	Err(FerrosGrynnError::atom_does_not_exist("atom", record.to_string()))
}

/// Returns closure value for this module.
fn closure_value(value: Value) -> FerrosGrynnResult<Closure> {
	match value {
		Value::Closure(closure) => Ok(*closure),
		value => Err(FerrosGrynnError::traversal_source_type_mismatch(
			"closure".to_owned(),
			value.kind().to_string(),
		)),
	}
}

/// Returns eval usize value for this module.
fn eval_usize_value(value: Value) -> FerrosGrynnResult<usize> {
	value
		.as_number()
		.and_then(|number| i64::try_from(number).ok())
		.and_then(|number| usize::try_from(number).ok())
		.ok_or_else(|| {
			FerrosGrynnError::traversal_source_type_mismatch(
				"non-negative integer".to_owned(),
				value.kind().to_string(),
			)
		})
}

/// Execution state driving traversers through prepared operators and bounded
/// queues.
struct TraversalMachine<'a, Read: QueryRead> {
	/// Transaction adapter used to resolve graph and record reads.
	read: &'a mut Read,
	/// Snapshot identity or retained read snapshot used by the operation.
	snapshot: Snapshot,
	/// Traversal or pattern steps retained in execution order.
	steps: Vec<PreparedStep>,
	/// Per-stage traverser queues connecting prepared operators.
	queues: Vec<VecDeque<Traverser>>,
	/// Progress retained for each prepared traversal stage.
	states: Vec<StageState>,
	/// Lexical values available to expressions and closures.
	slots: &'a [Value],
	/// Original query source used when reporting traversal diagnostics.
	source: &'a str,
	/// Cancellation, deadline and resource policy observed during traversal.
	control: &'a ExecutionControl,
	/// Mutable execution-control accounting for the current operation.
	runtime: &'a mut ControlRuntime,
	/// Arena retaining predecessor links for produced traversal paths.
	trace_arena: PathTraceArena,
	/// Memory admission retained by queued traversers.
	queue_memory: BarrierAccount,
	/// Memory admission retained by path-trace records.
	trace_memory: BarrierAccount,
	/// Whether traversal must retain the path used to reach each value.
	path_tracking: bool,
	/// Whether traversal treats edges as traversable atoms.
	atom_semantics: bool,
	/// Optional resolved cost function used when comparing weighted paths.
	custom_cost: Option<crate::gql::ir::LogicalPathCost>,
	/// Structural or path constraints applied to the selected graph operation.
	constraints: crate::syn::MatchConstraints,
	/// Declared graph root for qualified ROOTED domains.
	graph: Option<GraphRef>,
	/// Named side-effect values shared across the traversal execution.
	side_effects: Object,
	/// Stable graph-qualified occurrences associated with bound runtime atoms.
	occurrences: std::collections::BTreeMap<BoundAtom, AtomOccurrenceRef>,
	/// Cached local traversal indexes keyed by graph identity.
	local_indexes: std::collections::BTreeMap<crate::graph::GraphId, Option<Rc<TraversalIndex>>>,
	/// Cached scope-aware traversal indexes keyed by graph identity.
	scoped_indexes: std::collections::BTreeMap<crate::graph::GraphId, Rc<ScopedTraversalIndex>>,
}

impl<'a, Read: QueryRead> TraversalMachine<'a, Read> {
	/// Returns return row for `TraversalMachine<'a, Read>`.
	fn return_row(&self, traverser: &Traverser, schema: &Schema) -> Row {
		let values = schema
			.columns
			.iter()
			.enumerate()
			.map(|(index, column)| {
				if index == 0 {
					traverser.current.clone()
				} else {
					traverser
						.env
						.get(column.name.as_ref())
						.or_else(|| self.side_effects.get(column.name.as_ref()))
						.cloned()
						.unwrap_or(Value::None)
				}
			})
			.collect();
		Row::new(values)
	}

	#[allow(clippy::too_many_arguments, reason = "the VM owns one explicit statement context")]
	/// Prepares every stage before pulling the first VM traverser.
	///
	/// # Errors
	///
	/// Returns invalid count errors without constructing a partial machine.
	fn new(
		read: &'a mut Read,
		snapshot: Snapshot,
		steps: Vec<PreparedStep>,
		source_queue: VecDeque<Traverser>,
		slots: &'a [Value],
		source: &'a str,
		control: &'a ExecutionControl,
		runtime: &'a mut ControlRuntime,
		path_tracking: bool,
		atom_semantics: bool,
		custom_cost: Option<crate::gql::ir::LogicalPathCost>,
		constraints: &crate::syn::MatchConstraints,
		graph: Option<GraphRef>,
	) -> FerrosGrynnResult<Self> {
		let states = steps.iter().map(prepared_state).collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut queues = (0..=steps.len()).map(|_| VecDeque::new()).collect::<Vec<_>>();
		queues[0] = source_queue;
		Ok(Self {
			read,
			snapshot,
			steps,
			queues,
			states,
			slots,
			source,
			control,
			runtime,
			trace_arena: PathTraceArena::new(),
			queue_memory: control.barrier_account(),
			trace_memory: control.barrier_account(),
			path_tracking,
			atom_semantics,
			custom_cost,
			constraints: constraints.clone(),
			graph,
			side_effects: Object::new(),
			occurrences: std::collections::BTreeMap::new(),
			local_indexes: std::collections::BTreeMap::new(),
			scoped_indexes: std::collections::BTreeMap::new(),
		})
	}

	/// Advances this `TraversalMachine<'a, Read>` and returns its next element,
	/// if any.
	fn next(&mut self) -> FerrosGrynnResult<Option<Traverser>> {
		self.pull(self.steps.len())
	}

	/// Returns pull for `TraversalMachine<'a, Read>`.
	fn pull(&mut self, stage: usize) -> FerrosGrynnResult<Option<Traverser>> {
		self.control.check()?;
		if let Some(value) = self.queues[stage].pop_front() {
			if stage != 0 {
				self.queue_memory.release(value.estimated_heap_bytes());
			}
			return Ok(Some(value));
		}
		if stage == 0 {
			return Ok(None);
		}
		if matches!(self.states[stage - 1], StageState::Limit(0)) {
			return Ok(None);
		}
		let step = self.steps[stage - 1].clone();
		if matches!(self.states[stage - 1], StageState::GraphScan(Some(_))) {
			let output = self.resume_graph_scan(stage - 1)?;
			self.extend_queue(stage, output)?;
			if let Some(value) = self.queues[stage].pop_front() {
				return Ok(Some(value));
			}
		}
		if matches!(self.states[stage - 1], StageState::Barrier(_)) {
			let state = std::mem::replace(&mut self.states[stage - 1], StageState::Streaming);
			let StageState::Barrier(mut cursor) = state else {
				return Err(FerrosGrynnError::invalid_argument(
					"Traversal barrier state changed while it was being read.",
				));
			};
			if cursor.is_none() {
				cursor = Some(self.prepare_barrier(stage - 1, &step)?);
			}
			let next = cursor
				.as_mut()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("Traversal barrier cursor is unavailable.")
				})?
				.next(self);
			self.states[stage - 1] = StageState::Barrier(cursor);
			return next;
		}
		loop {
			let Some(input) = self.pull(stage - 1)? else {
				return Ok(None);
			};
			let output = self.apply_stream(stage - 1, &step, input)?;
			self.extend_queue(stage, output)?;
			if let Some(value) = self.queues[stage].pop_front() {
				return Ok(Some(value));
			}
		}
	}

	/// Extends queue for `TraversalMachine<'a, Read>`.
	fn extend_queue(
		&mut self,
		stage: usize,
		values: impl IntoIterator<Item = Traverser>,
	) -> FerrosGrynnResult<()> {
		let mut values = values.into_iter();
		let Some(first) = values.next() else {
			return Ok(());
		};
		let Some(second) = values.next() else {
			return self.append_adjacent(stage, std::iter::once(first));
		};
		if self.control.fast()
			&& !self.steps[stage..].iter().any(|step| step.kind == TraverseStepKind::SideEffect)
		{
			let mut merged = Vec::<Traverser>::new();
			let mut by_fingerprint = RapidHashMap::<u64, Vec<usize>>::default();
			let mut values = std::iter::once(first).chain(std::iter::once(second)).chain(values);
			let mut merge_memory = self.control.barrier_account();
			while let Some(value) = values.next() {
				let fingerprint = value.merge_fingerprint();
				let candidates = by_fingerprint.entry(fingerprint).or_default();
				if let Some(index) =
					candidates.iter().copied().find(|index| merged[*index].can_merge(&value))
				{
					merged[index].merge(&value)?;
				} else {
					let bytes =
						value
							.estimated_heap_bytes()
							.saturating_add(std::mem::size_of::<(u64, usize)>());
					if !merge_memory.reserve(bytes) {
						merge_memory.release_all();
						self.append_adjacent(stage, merged)?;
						return self.append_adjacent(stage, std::iter::once(value).chain(values));
					}
					candidates.push(merged.len());
					merged.push(value);
				}
			}
			merge_memory.release_all();
			return self.append_adjacent(stage, merged);
		}
		self.append_adjacent(
			stage,
			std::iter::once(first).chain(std::iter::once(second)).chain(values),
		)
	}

	/// Appends adjacent for `TraversalMachine<'a, Read>`.
	fn append_adjacent(
		&mut self,
		stage: usize,
		values: impl IntoIterator<Item = Traverser>,
	) -> FerrosGrynnResult<()> {
		for value in values {
			if let Some(last) = self.queues[stage].back_mut()
				&& last.can_merge(&value)
			{
				last.merge(&value)?;
			} else {
				if !self.queue_memory.reserve(value.estimated_heap_bytes()) {
					return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
						"stream queue".to_owned(),
						self.control.barrier_memory_limit() as u64,
					));
				}
				self.queues[stage].push_back(value);
			}
		}
		Ok(())
	}

	/// Pushes trace node for `TraversalMachine<'a, Read>`.
	fn push_trace_node(
		&mut self,
		previous: Option<PathTraceId>,
		step: PathStep,
		cumulative_cost: Option<Number>,
	) -> FerrosGrynnResult<PathTraceId> {
		let bytes = PathTraceArena::estimated_node_heap_bytes(&step);
		if !self.trace_memory.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"path trace".to_owned(),
				self.control.barrier_memory_limit() as u64,
			));
		}
		match self.trace_arena.push(previous, step, cumulative_cost) {
			Ok(id) => Ok(id),
			Err(error) => {
				self.trace_memory.release(bytes);
				Err(error)
			}
		}
	}
}

/// Builds one stage state without substituting invalid counts with zero.
///
/// # Errors
///
/// Returns missing, nonnumeric, fractional, negative or overflowing count
/// errors.
fn prepared_state(step: &PreparedStep) -> FerrosGrynnResult<StageState> {
	Ok(match step.kind {
		TraverseStepKind::Vertices | TraverseStepKind::Edges => StageState::GraphScan(None),
		TraverseStepKind::Limit | TraverseStepKind::Skip => {
			let value = step.arguments.first().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Traversal LIMIT/SKIP count is missing")
			})?;
			let count = value_u64(value)?;
			if matches!(step.kind, TraverseStepKind::Limit) {
				StageState::Limit(count)
			} else {
				StageState::Skip(count)
			}
		}
		kind if barrier::is_barrier(kind) => StageState::Barrier(None),
		_ => StageState::Streaming,
	})
}

/// Uses the common exact numeric conversion for a traversal count.
///
/// # Errors
///
/// Returns a type mismatch or the existing unsigned integer conversion error.
fn value_u64(value: &Value) -> FerrosGrynnResult<u64> {
	let number = value.as_number().ok_or_else(|| {
		FerrosGrynnError::traversal_source_type_mismatch(
			"non-negative integer".to_owned(),
			value.kind().to_string(),
		)
	})?;
	u64::try_from(number)
}

/// Returns expand value for this module.
fn expand_value(value: Value) -> FerrosGrynnResult<Vec<Value>> {
	match value {
		Value::Array(values) => Ok(values.into_iter().collect()),
		Value::Set(values) => Ok(values.into_iter().collect()),
		Value::Range(range) => Ok(super::integer_range(range)?.collect()),
		Value::None => Ok(Vec::new()),
		value => Err(FerrosGrynnError::traversal_source_type_mismatch(
			"array, set, or finite range".to_owned(),
			value.kind().to_string(),
		)),
	}
}
