//! Complete qualified shortest alternatives through the public tensor kernel.

use std::{
	cell::RefCell,
	collections::{BTreeMap, VecDeque},
};

use fg_runtime::__internal::{
	RecordIdInternal as _,
	TensorAtomInternal as _,
	TensorPathInternal as _,
};

use super::{
	scoped_path::{scoped_path_binding, scoped_path_candidate_allowed},
	*,
};
use crate::graph::{
	analytics::{AllShortestOptions, PathCost, PathMode},
	tensor::{
		EndpointTensor,
		ProjectionOptions,
		TensorArc,
		TensorAtom,
		TensorOptions,
		TensorScope,
		control::{Control, invalid, overflow, resource},
	},
};

/// Uses existing MATCH eligibility before minimizing each qualified pair.
pub(in crate::gql::execution) struct TensorShortestPathCursor<'a, Read: QueryRead> {
	/// Upstream binding rows in encounter order.
	input: Box<dyn RowCursor + 'a>,
	/// Immutable query snapshot reader.
	read: &'a Read,
	/// Current statement snapshot identity.
	snapshot: Snapshot,
	/// Existing qualified graph/reference resolver.
	fields: &'a MatchGraphFields<'a, Read>,
	/// Explicit LOCAL/ROOTED root.
	root: Arc<GraphRef>,
	/// Analyzed bindings, filters and path constraints.
	expand: ResolvedPathExpand,
	/// Compiled inline predicates and lexical slots.
	predicate: MatchPredicateContext<'a>,
	/// Optional whole-pattern named trace.
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	/// NODE or ATOM projected endpoints.
	semantics: TraversalSemantics,
	/// Qualified capture domain.
	scope: TensorScope,
	/// MATCH arrow topology is stricter than general traversal direction.
	topology_filter: bool,
	/// Optional traversal connector-label selection.
	labels: std::collections::BTreeSet<crate::values::Text>,
	/// Logical seed multiplicity consumes the statement enumeration quota.
	multiplicity: u64,
	/// Immutable generation chosen before upstream row pull.
	binding: Option<crate::datastore::QueryTensorCache>,
	/// One immutable cache/on-demand incidence capture per segment.
	tensor: Option<EndpointTensor>,
	/// Shared reservation retained with the captured tensor.
	tensor_account: Option<BarrierAccount>,
	/// Complete proven alternatives for the active input row.
	rows: VecDeque<Row>,
	/// Retained row/capacity reservation until the batch is drained.
	rows_account: Option<BarrierAccount>,
}

impl<'a, Read: QueryRead> TensorShortestPathCursor<'a, Read> {
	/// Keeps planned scope and the immutable query root explicit.
	#[allow(
		clippy::too_many_arguments,
		reason = "The cursor retains existing MATCH inputs and projection controls"
	)]
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		read: &'a Read,
		snapshot: Snapshot,
		fields: &'a MatchGraphFields<'a, Read>,
		root: Arc<GraphRef>,
		expand: ResolvedPathExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
		semantics: TraversalSemantics,
		scope: TensorScope,
		topology_filter: bool,
		labels: std::collections::BTreeSet<crate::values::Text>,
		multiplicity: u64,
		binding: Option<crate::datastore::QueryTensorCache>,
	) -> Self {
		Self {
			input,
			read,
			snapshot,
			fields,
			root,
			expand,
			predicate,
			trace,
			semantics,
			scope,
			topology_filter,
			labels,
			multiplicity,
			binding,
			tensor: None,
			tensor_account: None,
			rows: VecDeque::new(),
			rows_account: None,
		}
	}

	/// Captures shared cache/on-demand incidence once for this segment.
	fn capture(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<()> {
		if self.tensor.is_some() {
			return Ok(());
		}
		let limits = control.graph_algorithm_limits();
		if limits.memory_bytes == 0 {
			return Err(resource("all-shortest memory", 0));
		}
		if limits.max_paths == 0 {
			return Err(resource("all-shortest enumeration", 0));
		}
		if limits.max_work == 0 {
			return Err(resource("all-shortest work", 0));
		}
		control.record_graph_work(limits.max_work)?;
		let mut account = control.barrier_account();
		if !account.reserve(limits.memory_bytes) {
			return Err(resource("all-shortest tensor", limits.memory_bytes as u64));
		}
		let (tensor, binding) = self.read.endpoint_tensor_selected(
			self.snapshot,
			&self.root,
			{
				let split_field_0 = self.scope;
				let split_field_1 = limits;
				let mut split_options: TensorOptions = TensorOptions::default();
				split_options.scope = split_field_0;
				split_options.limits = split_field_1;
				split_options
			},
			self.binding.as_ref(),
			&mut || control.check(),
		)?;
		control.record_tensor_capture(&tensor, binding.as_ref());
		account.release(limits.memory_bytes - tensor.admitted_bytes());
		control.release_graph_work(limits.max_work - tensor.work());
		self.tensor = Some(tensor);
		self.tensor_account = Some(account);
		Ok(())
	}

	/// Proves the complete alternatives for one upstream row before row pull.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash \
		          identities"
	)]
	fn start(&mut self, row: Row, execution: &ExecutionControl) -> FerrosGrynnResult<()> {
		self.capture(execution)?;
		let tensor = self.tensor.as_ref().ok_or_else(|| invalid("tensor capture is absent"))?;
		let limits = execution.graph_algorithm_limits();
		if limits.memory_bytes == 0 {
			return Err(resource("all-shortest memory", 0));
		}
		if limits.max_paths == 0 {
			return Err(resource("all-shortest enumeration", 0));
		}
		if limits.max_work == 0 {
			return Err(resource("all-shortest work", 0));
		}
		execution.record_graph_work(limits.max_work)?;
		let mut account = execution.barrier_account();
		if !account.reserve(limits.memory_bytes) {
			return Err(resource("all-shortest scratch", limits.memory_bytes as u64));
		}
		let mut check = || execution.check();
		let mut control = Control::new(limits, &mut check)?;
		let scratch = limits.memory_bytes / 4;
		control.admit(scratch)?;
		let evaluator = self.read.tensor_cost_evaluator(self.snapshot, scratch)?;
		if let Some(cost) = self.expand.custom_cost {
			control.admit(
				1024 * size_of::<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>(),
			)?;
			for (dependency, _version) in
				evaluator.dependencies(cost.function.id, cost.function.version)?
			{
				control.step()?;
				self.read
					.observe_tensor_cost(crate::datastore::ConflictKey::SchemaFunction(dependency));
			}
		}
		let mut bindings = BTreeMap::new();
		for atom in tensor.atoms() {
			control.step()?;
			control.admit(atom.heap_bytes().checked_add(256).ok_or_else(overflow)?)?;
			let occurrence =
				atom.occurrence().ok_or_else(|| invalid("query tensor is not qualified"))?;
			let reference = self.fields.reference(occurrence.graph().id())?;
			let graph = self.read.graph(self.snapshot, &reference)?;
			let id = graph
				.atom_id(occurrence.atom())?
				.ok_or_else(|| invalid("tensor occurrence disappeared"))?;
			bindings.insert(atom.clone(), BoundAtom::new(reference.id(), id));
		}
		let source = row.current.ok_or_else(|| invalid("all-shortest row has no source"))?;
		let source_atom = bindings
			.iter()
			.find(|(_, bound)| **bound == source)
			.map(|(atom, _)| atom)
			.ok_or_else(|| invalid("all-shortest source is outside scope"))?;
		let bound_target =
			self.expand.target.column.and_then(|column| row.atoms.get(column)).copied().flatten();
		let mut targets = control.array(tensor.atoms().len())?;
		for atom in tensor.domain_atoms() {
			control.step()?;
			let target = bindings[atom];
			if bound_target.is_some_and(|bound| bound != target) {
				continue;
			}
			let reference = self.fields.reference(target.graph)?;
			let graph = self.read.graph(self.snapshot, &reference)?;
			if atom_matches(&graph, target.atom, &self.expand.target)? {
				targets.push(atom);
			}
		}
		let natural = match self.expand.path_mode {
			crate::syn::MatchPathMode::Trail => tensor.connectors().len(),
			crate::syn::MatchPathMode::Acyclic => tensor.atoms().len().saturating_sub(1),
			crate::syn::MatchPathMode::Simple => tensor.atoms().len(),
			crate::syn::MatchPathMode::Walk
				if self.expand.min == 0
					&& !self
						.expand
						.objective
						.is_some_and(crate::syn::MatchPathObjective::is_weighted) =>
			{
				tensor.atoms().len().saturating_sub(1)
			}
			crate::syn::MatchPathMode::Walk => {
				usize::try_from(self.expand.max).map_err(|_cause| overflow())?
			}

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported syntax variant at the engine boundary.",
				));
			}
		};
		let depth = usize::try_from(self.expand.max).map_err(|_cause| overflow())?.min(natural);
		let row_bytes = row.estimated_heap_bytes();
		let row_copy_work = u64::try_from(row_bytes).map_err(|_cause| overflow())?;
		// Input, one reusable predicate clone and old/new uniqueness capacities
		// overlap. This bound is separate from the PURE evaluator account.
		control.admit(probe_bytes(&row, depth)?)?;
		let prefix = RefCell::new(control.array::<ScopedTraversalCandidate>(depth)?);
		let candidate = |arc: TensorArc, atoms: &[TensorAtom]| -> ScopedTraversalCandidate {
			let current = bindings[&atoms[arc.source()]];
			let target = bindings[&atoms[arc.target()]];
			let connector = &tensor.connectors()[arc.connector()];
			ScopedTraversalCandidate {
				target,
				edge: bindings[connector.atom()],
				direction: arc.direction(),
				role: arc.role(),
				topology: connector.direction(),
				cross_level: current.graph != target.graph,
			}
		};
		let mut output = Vec::new();
		for target in targets {
			reserve_phase(&mut account, execution, &mut control)?;
			for label in &self.labels {
				control.admit(label.len().checked_add(128).ok_or_else(overflow)?)?;
			}
			let selection = {
				let split_field_0 = self.semantics;
				let split_field_1 = self.labels.clone();
				let split_field_2 = match self.expand.direction {
					crate::syn::MatchDirection::Forward => TraversalDirection::Forward,
					crate::syn::MatchDirection::Backward => TraversalDirection::Backward,
					_ => TraversalDirection::Both,
				};
				let mut split_options: ProjectionOptions = ProjectionOptions::default();
				split_options.mode = split_field_0;
				split_options.labels = split_field_1;
				split_options.direction = split_field_2;
				split_options
			};
			let weighted =
				self.expand.objective.is_some_and(crate::syn::MatchPathObjective::is_weighted);
			let mut path_limits = remaining(&control)?;
			path_limits.max_paths = execution.graph_algorithm_limits().max_paths
				/ usize::try_from(self.multiplicity).map_err(|_cause| overflow())?;
			if path_limits.max_paths == 0 {
				return Err(resource("all-shortest enumeration", 0));
			}
			let options = {
				let split_field_0 =
					fg_syntax::__internal::EnumChoice::choose(&(self.expand.path_mode), [
						PathMode::Walk,
						PathMode::Trail,
						PathMode::Acyclic,
						PathMode::Simple,
					]);
				let split_field_1 = if weighted {
					PathCost::Custom
				} else {
					PathCost::Hop
				};
				let split_field_2 = self.expand.min as usize;
				let split_field_3 = Some(depth);
				let split_field_4 = path_limits;
				let mut split_options: AllShortestOptions = AllShortestOptions::default();
				split_options.mode = split_field_0;
				split_options.cost = split_field_1;
				split_options.min_depth = split_field_2;
				split_options.max_depth = split_field_3;
				split_options.limits = split_field_4;
				split_options
			};
			let result = tensor.all_shortest_paths_with_filter(
				source_atom,
				target,
				selection,
				options,
				&mut |context| {
					context.charge_work(1)?;
					if let Some(cost) = self.expand.custom_cost {
						evaluator.path_contribution(
							cost.function.id,
							cost.function.version,
							context
								.connector
								.occurrence()
								.ok_or_else(|| invalid("COST connector has no occurrence"))?,
							context.prefix,
							&|key| self.read.observe_tensor_cost(key),
							&|work| {
								execution.check()?;
								context.charge_work(work)
							},
						)
					} else {
						path_edge_weight(
							&self.read.graph(
								self.snapshot,
								context
									.connector
									.occurrence()
									.ok_or_else(|| invalid("connector has no occurrence"))?
									.graph(),
							)?,
							&self.expand,
							bindings[context.connector].atom,
						)
					}
				},
				&mut |context| {
					context.charge_work(
						u64::try_from(context.hops.len())
							.map_err(|_cause| overflow())?
							.checked_add(1)
							.ok_or_else(overflow)?,
					)?;
					context.charge_work(row_copy_work)?;
					execution.record_path_expansion()?;
					let hop = candidate(context.transition, context.atoms);
					if self.topology_filter
						&& !direction_accepts(self.expand.direction, hop.topology)
					{
						return Ok(false);
					}
					let mut prefix = prefix.borrow_mut();
					prefix.clear();
					prefix.extend(context.hops.iter().map(|arc| candidate(*arc, context.atoms)));
					let observe = |key| self.read.observe_tensor_cost(key);
					let work = |work| {
						execution.check()?;
						context.charge_work(work)
					};
					let fields = evaluator.fields(&observe, &work);
					let predicate = MatchPredicateContext {
						fields: &fields,
						..self.predicate
					};
					scoped_path_candidate_allowed(
						&row,
						source,
						bindings[context.source],
						&prefix,
						&self.expand,
						&hop,
						self.read,
						self.snapshot,
						self.fields,
						predicate,
						execution,
					)
				},
				&mut |context| {
					context.charge_work(terminal_probe_work(&row, context.hops.len())?)?;
					let mut prefix = prefix.borrow_mut();
					prefix.clear();
					prefix.extend(context.hops.iter().map(|arc| candidate(*arc, context.atoms)));
					let observe = |key| self.read.observe_tensor_cost(key);
					let work = |work| {
						execution.check()?;
						context.charge_work(work)
					};
					let fields = evaluator.fields(&observe, &work);
					let predicate = MatchPredicateContext {
						fields: &fields,
						..self.predicate
					};
					Ok(scoped_path_binding(
						&row,
						&self.expand,
						bindings[target],
						&prefix,
						self.read,
						self.snapshot,
						self.fields,
						predicate,
						execution,
					)?
					.is_some())
				},
				&mut || control.poll(),
			)?;
			control.work(usize::try_from(result.work()).map_err(|_cause| overflow())?)?;
			control.admit(result.admitted_bytes())?;
			let paths = result.collect_all()?;
			execution.record_graph_paths(
				paths
					.len()
					.checked_mul(usize::try_from(self.multiplicity).map_err(|_cause| overflow())?)
					.ok_or_else(overflow)?,
			)?;
			control.admit(
				paths
					.len()
					.checked_mul(row.estimated_heap_bytes().checked_add(4096).ok_or_else(overflow)?)
					.ok_or_else(overflow)?,
			)?;
			output.try_reserve_exact(paths.len()).map_err(|cause| {
				resource("all-shortest rows", limits.memory_bytes as u64)
					.with_technical_source(cause)
			})?;
			for path in paths {
				reserve_phase(&mut account, execution, &mut control)?;
				let mut prefix = prefix.borrow_mut();
				prefix.clear();
				for step in path.steps().iter().skip(1) {
					let edge = step
						.via_edge
						.as_ref()
						.ok_or_else(|| invalid("path connector is absent"))?;
					prefix.push(ScopedTraversalCandidate {
						target: bindings[&step.atom],
						edge: bindings[edge],
						direction: step.direction,
						role: step.role,
						topology: tensor.connectors()[tensor
							.connector_ordinal(edge)
							.ok_or_else(|| invalid("path connector is outside tensor"))?]
						.direction(),
						cross_level: false,
					});
				}
				let contributions = path.contributions();
				control.work(contributions.len())?;
				let trace_cost = if self.trace.is_some() {
					contributions
						.iter()
						.try_fold(row.trace_cost, |cost, value| add_path_cost(cost, *value))?
				} else {
					path.cost()
				};
				let bound = {
					let observe = |key| self.read.observe_tensor_cost(key);
					control.work(
						usize::try_from(terminal_probe_work(&row, prefix.len())?)
							.map_err(|_cause| overflow())?,
					)?;
					let admission = RefCell::new(&mut control);
					let work = |work| {
						execution.check()?;
						admission
							.borrow_mut()
							.work(usize::try_from(work).map_err(|_cause| overflow())?)
					};
					let fields = evaluator.fields(&observe, &work);
					let predicate = MatchPredicateContext {
						fields: &fields,
						..self.predicate
					};
					scoped_path_binding(
						&row,
						&self.expand,
						bindings[target],
						&prefix,
						self.read,
						self.snapshot,
						self.fields,
						predicate,
						execution,
					)?
				};
				if let Some(mut result) = bound {
					let path = path.materialize_with_control(
						&mut |_origin| Err(invalid("query path has a runtime-only identity")),
						&mut control,
					)?;
					let cost = path.cost().ok_or_else(|| invalid("shortest cost is absent"))?;
					if let Some(trace) = &self.trace {
						for step in path.steps().iter().skip(1) {
							control.admit(
								step.atom()
									.atom()
									.id()
									.estimated_heap_bytes()
									.checked_add(
										step.via_edge().map_or(0, |edge| {
											edge.atom().id().estimated_heap_bytes()
										}),
									)
									.and_then(|bytes| bytes.checked_add(256))
									.ok_or_else(overflow)?,
							)?;
						}
						// Reserve a complete scratch/owned-path bound before
						// the separate trace account grows.
						account.release_all();
						if !account.reserve(control.bytes) {
							return Err(resource(
								"all-shortest retained rows",
								limits.memory_bytes as u64,
							));
						}
						for (position, step) in path.steps().iter().enumerate().skip(1) {
							result.trace = Some(trace.borrow_mut().push(
								result.trace,
								step.clone(),
								(position + 1 == path.steps().len()).then_some(trace_cost),
								execution,
							)?);
						}
						result.trace_cost = trace_cost;
					} else {
						result.trace_cost = cost;
						if let Some(column) = self.expand.path_column {
							result.values[column] = Value::from(path);
						}
					}
					output.push(result);
				}
			}
		}
		execution.release_graph_work(limits.max_work - control.work);
		account.release_all();
		if !account.reserve(control.bytes) {
			return Err(resource("all-shortest retained rows", limits.memory_bytes as u64));
		}
		self.rows = output.into();
		self.rows_account = Some(account);
		Ok(())
	}
}

impl<Read: QueryRead> RowCursor for TensorShortestPathCursor<'_, Read> {
	/// Advances this `TensorShortestPathCursor<'_, Read>` and returns its next
	/// element, if any.
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if let Some(row) = self.rows.pop_front() {
				if let Some(account) = &mut self.rows_account {
					account.release(row.estimated_heap_bytes());
				}
				return Ok(Some(row));
			}
			self.rows_account = None;
			let Some(row) = self.input.next(control)? else {
				return Ok(None);
			};
			self.start(row, control)?;
		}
	}
}

/// Covers the input, reusable probe clone and uniqueness growth overlap.
fn probe_bytes(row: &Row, depth: usize) -> FerrosGrynnResult<usize> {
	row.estimated_heap_bytes()
		.checked_mul(4)
		.and_then(|bytes| bytes.checked_add(depth.checked_mul(16 * size_of::<BoundAtom>())?))
		.ok_or_else(overflow)
}

/// Admits row copies and repeated uniqueness scans before terminal binding.
fn terminal_probe_work(row: &Row, depth: usize) -> FerrosGrynnResult<u64> {
	let extent = row
		.used_edges
		.len()
		.checked_add(row.used_atoms.len().checked_mul(2).ok_or_else(overflow)?)
		.and_then(|length| length.checked_add(depth.checked_mul(2)?))
		.and_then(|length| length.checked_add(1))
		.ok_or_else(overflow)?;
	let work = depth
		.checked_mul(extent)
		.and_then(|work| work.checked_add(row.estimated_heap_bytes()))
		.ok_or_else(overflow)?;
	u64::try_from(work).map_err(|_cause| overflow())
}

/// Preserves one budget across qualified resolution, search and retained rows.
fn remaining(
	control: &Control<'_>,
) -> FerrosGrynnResult<crate::graph::tensor::GraphAlgorithmLimits> {
	let mut limits = control.limits;
	limits.memory_bytes = limits
		.memory_bytes
		.checked_sub(control.bytes)
		.filter(|value| *value > 0)
		.ok_or_else(|| resource("all-shortest bytes", limits.memory_bytes as u64))?;
	limits.max_work = limits
		.max_work
		.checked_sub(control.work)
		.filter(|value| *value > 0)
		.ok_or_else(|| resource("all-shortest work", limits.max_work))?;
	Ok(limits)
}

/// Reserves every live operation buffer and its remaining scratch before a
/// phase.
fn reserve_phase(
	account: &mut BarrierAccount,
	execution: &ExecutionControl,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<()> {
	account.release_all();
	let bytes = control.limits.memory_bytes.min(execution.barrier_memory_available());
	if control.bytes > bytes || !account.reserve(bytes) {
		return Err(resource("all-shortest phase memory", bytes as u64));
	}
	control.limits.memory_bytes = bytes;
	Ok(())
}

#[cfg(test)]
/// Regression tests for tensor shortest behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies tensor probe admission covers wide row clones and quadratic
	/// scans.
	#[test]
	fn tensor_probe_admission_covers_wide_row_clones_and_quadratic_scans() {
		let row = Row::bindings(4096);
		let limits = {
			let split_field_0 = row.estimated_heap_bytes() * 3;
			let mut split_options: crate::graph::tensor::GraphAlgorithmLimits =
				crate::graph::tensor::GraphAlgorithmLimits::default();
			split_options.memory_bytes = split_field_0;
			split_options
		};
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check).assert_ok();
		assert!(control.admit(probe_bytes(&row, 1).assert_ok()).is_err());
		assert!(
			terminal_probe_work(&row, 100).assert_ok()
				>= (row.estimated_heap_bytes() + 20_100) as u64
		);
		assert!(probe_bytes(&row, usize::MAX).is_err());
		assert!(terminal_probe_work(&row, usize::MAX).is_err());
	}
}
