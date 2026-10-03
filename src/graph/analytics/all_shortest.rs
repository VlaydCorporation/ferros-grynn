//! Bounded exact full-state shortest paths over qualified incidence.

use std::{cell::Cell, collections::VecDeque, sync::Arc};

use crate::{
	gql_status::FerrosGrynnResult,
	graph::{
		AtomOccurrenceRef,
		EndpointRole,
		GraphRef,
		Path,
		PathDirection,
		PathStep,
		SnapshotId,
		tensor::{
			EndpointTensor,
			GraphAlgorithmLimits,
			ProjectedRelation,
			ProjectionOptions,
			TensorArc,
			TensorAtom,
			control::{Control, invalid, overflow, resource},
		},
	},
	math::{add_numbers, number_is_finite},
	values::{Number, Uuid},
};

/// Orthogonal path uniqueness constraint, independent of vertex projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathMode {
	/// Vertices and connectors may repeat; weighted search needs a depth bound.
	Walk,
	/// Every qualified connector occurs at most once.
	#[default]
	Trail,
	/// Every qualified endpoint occurs at most once.
	Acyclic,
	/// Endpoints are distinct, except for a final return to the source.
	Simple,
}

/// Exact Number contribution selected at each expansion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum PathCost {
	/// Every connector transition costs one.
	#[default]
	Hop,
	/// Original connector weight; adjacency strength sums are never used.
	Stored,
	/// Caller callback, including prefix-dependent stateful contributions.
	Custom,
}

/// Validated shortest-path constraints and hard operation ceilings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AllShortestOptions {
	/// TRAIL by default.
	pub mode: PathMode,
	/// Hop by default; custom contributions always use full-state search.
	pub cost: PathCost,
	/// Minimum accepted number of connector transitions.
	pub min_depth: usize,
	/// Explicit finite maximum, required for weighted WALK.
	pub max_depth: Option<usize>,
	/// Scratch, work and complete-result path ceilings.
	pub limits: GraphAlgorithmLimits,
}

/// Borrowed exact prefix supplied to a Rust COST callback.
#[derive(Debug)]
#[non_exhaustive]
#[allow(
	clippy::partial_pub_fields,
	reason = "Borrowed COST inputs are public; admission state is immutable to callers"
)]
pub struct PathCostContext<'a> {
	/// Qualified connector identity.
	pub connector: &'a TensorAtom,
	/// Candidate transition, including direction, role and connector ordinal.
	pub transition: TensorArc,
	/// Qualified current endpoint.
	pub source: &'a TensorAtom,
	/// Qualified next endpoint.
	pub target: &'a TensorAtom,
	/// Exact accumulated Number cost before this contribution.
	pub prefix: Number,
	/// Complete prior transitions; no prefix dominance is applied.
	pub hops: &'a [TensorArc],
	/// Axis dictionary for the prefix transition ordinals.
	pub atoms: &'a [TensorAtom],
	/// Work already charged by this callback.
	work: Cell<u64>,
	/// Remaining operation-wide work before invoking this callback.
	remaining_work: u64,
}

impl PathCostContext<'_> {
	/// Charges callback work before executing it, against the search budget.
	///
	/// # Errors
	/// Rejects overflow or exhaustion of the operation-wide work ceiling.
	pub fn charge_work(&self, operations: u64) -> FerrosGrynnResult<()> {
		let next = self.work.get().checked_add(operations).ok_or_else(overflow)?;
		if next > self.remaining_work {
			return Err(resource("all-shortest COST work", self.remaining_work));
		}
		self.work.set(next);
		Ok(())
	}
}

/// Complete terminal prefix with shared callback work admission.
#[derive(Debug)]
#[non_exhaustive]
#[allow(
	clippy::partial_pub_fields,
	reason = "Predicate inputs are public; admission counters remain private"
)]
pub struct PathTerminalContext<'a> {
	/// Complete accepted transitions before terminal predicates.
	pub hops: &'a [TensorArc],
	/// Canonical qualified axis dictionary.
	pub atoms: &'a [TensorAtom],
	/// Exact terminal cost.
	pub cost: Number,
	/// Callback work charged before performing it.
	work: Cell<u64>,
	/// Remaining aggregate search admission.
	remaining_work: u64,
}

impl PathTerminalContext<'_> {
	/// Charges terminal predicate work against the same operation as COST.
	///
	/// # Errors
	/// Rejects overflow or exhausted work.
	pub fn charge_work(&self, operations: u64) -> FerrosGrynnResult<()> {
		let next = self.work.get().checked_add(operations).ok_or_else(overflow)?;
		if next > self.remaining_work {
			return Err(resource("all-shortest terminal work", self.remaining_work));
		}
		self.work.set(next);
		Ok(())
	}
}

/// Owned logical step, usable without a catalog GraphId or a live reader.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct TensorPathStep {
	/// Qualified endpoint.
	pub atom: TensorAtom,
	/// Qualified connector, absent on the seed step.
	pub via_edge: Option<TensorAtom>,
	/// Original incidence traversal direction.
	pub direction: PathDirection,
	/// Original endpoint role.
	pub role: EndpointRole,
}

/// Complete owned logical path with exact cost and primary scope metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorPath {
	/// Owned qualified steps in traversal order, including the source.
	steps: Vec<TensorPathStep>,
	/// Captured per-connector contributions in exact evaluation order.
	contributions: Vec<Number>,
	/// Exact left-to-right Number reduction of connector contributions.
	cost: Number,
	/// Primary requested graph origin captured by the tensor.
	origin: Uuid,
	/// Primary requested graph snapshot captured by the tensor.
	snapshot: SnapshotId,
}

impl TensorPath {
	/// Borrows immutable qualified logical steps in traversal order.
	pub fn steps(&self) -> &[TensorPathStep] {
		&self.steps
	}

	/// Borrows captured costs without invoking a stateful callback again.
	pub fn contributions(&self) -> &[Number] {
		&self.contributions
	}

	/// Returns the exact complete path cost.
	pub const fn cost(&self) -> Number {
		self.cost
	}

	/// Returns the captured path origin.
	pub const fn origin(&self) -> Uuid {
		self.origin
	}

	/// Returns the captured path snapshot.
	pub const fn snapshot(&self) -> SnapshotId {
		self.snapshot
	}

	/// Converts catalog-qualified steps to the existing public Path.
	///
	/// # Errors
	/// Runtime-only steps require explicit graph references; rejects admission.
	pub fn materialize_path(&self, limits: GraphAlgorithmLimits) -> FerrosGrynnResult<Path> {
		self.materialize_path_with_references(limits, &mut |_origin| {
			Err(invalid("standalone path requires an explicit GraphRef"))
		})
	}

	/// Converts standalone identities with caller-provided real graph metadata.
	///
	/// # Errors
	/// Propagates reference-resolution, checked size and admission errors.
	pub fn materialize_path_with_references(
		&self,
		limits: GraphAlgorithmLimits,
		reference: &mut impl FnMut(Uuid) -> FerrosGrynnResult<Arc<GraphRef>>,
	) -> FerrosGrynnResult<Path> {
		let mut check = || Ok(());
		let mut control = Control::new(limits, &mut check)?;
		self.materialize_with_control(reference, &mut control)
	}

	/// Shares one aggregate budget with snapshot capture and COST evaluation.
	pub(crate) fn materialize_with_control(
		&self,
		reference: &mut impl FnMut(Uuid) -> FerrosGrynnResult<Arc<GraphRef>>,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Path> {
		// Path::new moves its Vec into Arc storage; admit both live buffers.
		control.admit(self.steps.len().checked_mul(size_of::<PathStep>()).ok_or_else(overflow)?)?;
		let mut steps = control.array(self.steps.len())?;
		for step in &self.steps {
			control.step()?;
			let atom = occurrence(&step.atom, reference, control)?;
			let via = step
				.via_edge
				.as_ref()
				.map(|atom| occurrence(atom, reference, control))
				.transpose()?;
			steps.push(PathStep::new(atom, via, step.direction, step.role));
		}
		control.work(steps.len())?;
		Ok(Path::new(self.origin, self.snapshot, steps, Some(self.cost)))
	}
}

/// Admits identity cloning before constructing a public occurrence.
fn occurrence(
	atom: &TensorAtom,
	reference: &mut impl FnMut(Uuid) -> FerrosGrynnResult<Arc<GraphRef>>,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<AtomOccurrenceRef> {
	control.admit(atom.heap_bytes().checked_add(256).ok_or_else(overflow)?)?;
	match atom {
		TensorAtom::Occurrence(value) => Ok(value.clone()),
		TensorAtom::Runtime {
			graph,
			atom,
		} => {
			let graph = reference(*graph)?;
			control.admit(graph.name().len())?;
			Ok(AtomOccurrenceRef::new(atom.clone(), graph))
		}
	}
}

/// Fully proven, canonically ordered shortest results; no truncated success.
#[derive(Debug)]
pub struct AllShortestPaths {
	/// Complete retained paths, already proven minimal before enumeration.
	paths: Vec<TensorPath>,
	/// Admitted exact search and reduction operations.
	work: u64,
	/// Conservative retained and scratch admission.
	admitted_bytes: usize,
}

impl AllShortestPaths {
	/// Returns conservatively charged exact search operations.
	pub const fn work(&self) -> u64 {
		self.work
	}

	/// Returns conservative retained and scratch admission.
	pub const fn admitted_bytes(&self) -> usize {
		self.admitted_bytes
	}

	/// Enumerates complete results through a fallible SDK surface.
	pub fn enumerate(self) -> impl Iterator<Item = FerrosGrynnResult<TensorPath>> {
		self.paths.into_iter().map(Ok)
	}

	/// Returns the complete owned result without a second allocation.
	///
	/// # Errors
	/// Search errors precede construction; successful results are never
	/// partial.
	pub fn collect_all(self) -> FerrosGrynnResult<Vec<TensorPath>> {
		Ok(self.paths)
	}
}

/// One admitted DFS prefix frame; transitions are stored once in the
/// projection.
struct Frame {
	/// Current qualified endpoint ordinal.
	node: usize,
	/// Next outgoing transition ordinal.
	next: usize,
	/// Exclusive outgoing transition bound.
	end: usize,
	/// Exact accumulated cost.
	cost: Number,
	/// Whether terminal handling has already occurred.
	entered: bool,
}

/// Retains ordinals until minimality is proven, avoiding repeated identity
/// clones.
struct Found {
	/// Exact transition sequence including connector identity.
	hops: Vec<TensorArc>,
	/// Captured connector costs, including their original Number
	/// representation.
	contributions: Vec<Number>,
	/// Exact terminal cost.
	cost: Number,
}

/// Builds reverse CSR and an unconstrained hop lower bound without dense cells.
fn hop_bounds(
	projection: &ProjectedRelation,
	target: usize,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<Vec<usize>> {
	let n = projection.atoms.len();
	let mut offsets = control.filled(n.checked_add(1).ok_or_else(overflow)?, 0_usize)?;
	for arc in &projection.arcs {
		control.step()?;
		offsets[arc.target + 1] = offsets[arc.target + 1].checked_add(1).ok_or_else(overflow)?;
	}
	for row in 0..n {
		control.step()?;
		offsets[row + 1] = offsets[row + 1].checked_add(offsets[row]).ok_or_else(overflow)?;
	}
	let mut cursor = control.array(offsets.len())?;
	control.work(offsets.len())?;
	cursor.extend_from_slice(&offsets);
	let mut incoming = control.filled(projection.arcs.len(), 0_usize)?;
	for arc in &projection.arcs {
		control.step()?;
		incoming[cursor[arc.target]] = arc.source;
		cursor[arc.target] += 1;
	}
	let mut distances = control.filled(n, usize::MAX)?;
	let mut queue = VecDeque::new();
	control.admit(n.checked_mul(size_of::<usize>()).ok_or_else(overflow)?)?;
	queue.try_reserve_exact(n).map_err(|cause| {
		resource("all-shortest queue", control.limits.memory_bytes as u64)
			.with_technical_source(cause)
	})?;
	distances[target] = 0;
	queue.push_back(target);
	while let Some(node) = queue.pop_front() {
		control.step()?;
		for other in &incoming[offsets[node]..offsets[node + 1]] {
			control.step()?;
			if distances[*other] == usize::MAX {
				distances[*other] = distances[node].checked_add(1).ok_or_else(overflow)?;
				queue.push_back(*other);
			}
		}
	}
	Ok(distances)
}

impl EndpointTensor {
	/// Finds every minimal admissible path through the shared qualified
	/// projection.
	///
	/// # Errors
	/// Rejects foreign endpoints, weighted unbounded WALK, invalid costs or
	/// limits.
	pub fn all_shortest_paths(
		&self,
		source: &TensorAtom,
		target: &TensorAtom,
		projection: ProjectionOptions,
		options: AllShortestOptions,
	) -> FerrosGrynnResult<AllShortestPaths> {
		if options.cost == PathCost::Custom {
			return Err(invalid("custom all-shortest requires a COST callback"));
		}
		self.all_shortest_paths_with_cost(
			source,
			target,
			projection,
			options,
			&mut |_context| Err(invalid("all-shortest COST callback is absent")),
			&mut || Ok(()),
		)
	}

	/// Searches exact full states with caller-defined additive or stateful
	/// COST.
	///
	/// # Errors
	/// Propagates callback/check failures, overflow and hard resource
	/// rejection.
	pub fn all_shortest_paths_with_cost(
		&self,
		source: &TensorAtom,
		target: &TensorAtom,
		selection: ProjectionOptions,
		options: AllShortestOptions,
		cost: &mut impl FnMut(&PathCostContext<'_>) -> FerrosGrynnResult<Number>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<AllShortestPaths> {
		self.all_shortest_paths_with_filter(
			source,
			target,
			selection,
			options,
			cost,
			&mut |_context| Ok(true),
			&mut |_context| Ok(true),
			check,
		)
	}

	/// Applies branch and terminal eligibility before proving minimality.
	/// Terminal callbacks receive the complete prefix and its axis dictionary.
	///
	/// # Errors
	/// Propagates callback/check failures and rejects invalid costs or budgets.
	#[allow(
		clippy::too_many_arguments,
		reason = "Exact constraints, COST and cancellation remain explicit callbacks"
	)]
	pub fn all_shortest_paths_with_filter(
		&self,
		source: &TensorAtom,
		target: &TensorAtom,
		mut selection: ProjectionOptions,
		options: AllShortestOptions,
		cost: &mut impl FnMut(&PathCostContext<'_>) -> FerrosGrynnResult<Number>,
		branch: &mut impl FnMut(&PathCostContext<'_>) -> FerrosGrynnResult<bool>,
		terminal: &mut impl FnMut(&PathTerminalContext<'_>) -> FerrosGrynnResult<bool>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<AllShortestPaths> {
		if options.limits.max_paths == 0 {
			return Err(resource("all-shortest enumeration", 0));
		}
		if options.mode == PathMode::Walk
			&& options.cost != PathCost::Hop
			&& options.max_depth.is_none()
		{
			return Err(invalid("weighted all-shortest WALK requires finite max_depth"));
		}
		if options.max_depth.is_some_and(|max| options.min_depth > max) {
			return Err(invalid("all-shortest min_depth exceeds max_depth"));
		}
		selection.limits = options.limits;
		let mut control = Control::new(options.limits, check)?;
		let projection = self.project_relation(&selection, &mut control)?;
		let source = projection
			.atoms
			.binary_search(source)
			.map_err(|_cause| invalid("all-shortest source is outside the induced domain"))?;
		let target = projection
			.atoms
			.binary_search(target)
			.map_err(|_cause| invalid("all-shortest target is outside the induced domain"))?;
		if options.cost == PathCost::Stored {
			for arc in &projection.arcs {
				control.step()?;
				let weight = projection.connector_weights[arc.connector];
				if !number_is_finite(weight) || weight < Number::Int(0) {
					return Err(invalid("all-shortest requires finite nonnegative stored weights"));
				}
			}
		}
		let bounds = hop_bounds(&projection, target, &mut control)?;
		let mut found = Vec::new();
		if bounds[source] != usize::MAX {
			let natural = match options.mode {
				PathMode::Trail => self.connectors.len(),
				PathMode::Acyclic => projection.atoms.len().saturating_sub(1),
				PathMode::Simple => projection.atoms.len(),
				PathMode::Walk => {
					usize::try_from(options.limits.max_work).map_err(|_cause| overflow())?
				}
			};
			let max_depth = options.max_depth.map_or(natural, |max| max.min(natural));
			let mut bound = if options.cost == PathCost::Hop {
				options.min_depth.max(bounds[source])
			} else {
				max_depth
			};
			while bound <= max_depth {
				search(
					self,
					&projection,
					source,
					target,
					options,
					bound,
					&bounds,
					cost,
					branch,
					terminal,
					&mut found,
					&mut control,
				)?;
				if options.cost != PathCost::Hop || !found.is_empty() || bound == max_depth {
					break;
				}
				bound = bound.checked_add(1).ok_or_else(overflow)?;
			}
		}
		control.work(found.len())?;
		control.sort(
			found
				.len()
				.checked_mul(found.iter().map(|p| p.hops.len()).max().unwrap_or(0).max(1))
				.ok_or_else(overflow)?,
		)?;
		found.sort_unstable_by(|a, b| {
			a.hops
				.iter()
				.map(|h| (h.connector, h.target, h.direction, h.role))
				.cmp(b.hops.iter().map(|h| (h.connector, h.target, h.direction, h.role)))
		});
		let source_atom = &projection.atoms[source];
		let binding = self.root_binding();
		let mut paths = control.array(found.len())?;
		for path in found {
			let mut steps = control.array(path.hops.len().checked_add(1).ok_or_else(overflow)?)?;
			control.admit(source_atom.heap_bytes())?;
			steps.push(TensorPathStep {
				atom: source_atom.clone(),
				via_edge: None,
				direction: PathDirection::Forward,
				role: EndpointRole::Source,
			});
			for hop in path.hops {
				control.step()?;
				let atom = &projection.atoms[hop.target];
				let via = &self.connectors[hop.connector].atom;
				control
					.admit(atom.heap_bytes().checked_add(via.heap_bytes()).ok_or_else(overflow)?)?;
				steps.push(TensorPathStep {
					atom: atom.clone(),
					via_edge: Some(via.clone()),
					direction: hop.direction,
					role: hop.role,
				});
			}
			paths.push(TensorPath {
				steps,
				contributions: path.contributions,
				cost: path.cost,
				origin: binding.origin,
				snapshot: binding.snapshot,
			});
		}
		Ok(AllShortestPaths {
			paths,
			work: control.work,
			admitted_bytes: control.bytes,
		})
	}
}

/// Exhausts complete admissible prefixes; only hop lower bounds prune states.
#[allow(
	clippy::too_many_arguments,
	reason = "Exact state search retains explicit projection, constraints and caller controls"
)]
fn search(
	tensor: &EndpointTensor,
	projection: &ProjectedRelation,
	source: usize,
	target: usize,
	options: AllShortestOptions,
	depth: usize,
	bounds: &[usize],
	cost: &mut impl FnMut(&PathCostContext<'_>) -> FerrosGrynnResult<Number>,
	branch: &mut impl FnMut(&PathCostContext<'_>) -> FerrosGrynnResult<bool>,
	terminal: &mut impl FnMut(&PathTerminalContext<'_>) -> FerrosGrynnResult<bool>,
	found: &mut Vec<Found>,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<()> {
	let n = projection.atoms.len();
	let mut offsets = control.filled(n.checked_add(1).ok_or_else(overflow)?, 0_usize)?;
	for arc in &projection.arcs {
		control.step()?;
		offsets[arc.source + 1] = offsets[arc.source + 1].checked_add(1).ok_or_else(overflow)?;
	}
	for row in 0..n {
		control.step()?;
		offsets[row + 1] = offsets[row + 1].checked_add(offsets[row]).ok_or_else(overflow)?;
	}
	let mut seen = control.filled(n, 0_usize)?;
	let mut used = control.filled(tensor.connectors.len(), false)?;
	let mut hops = control.array::<TensorArc>(depth)?;
	let mut contributions = control.array::<Number>(depth)?;
	let mut stack = control.array(depth.checked_add(1).ok_or_else(overflow)?)?;
	stack.push(Frame {
		node: source,
		next: offsets[source],
		end: offsets[source + 1],
		cost: Number::Int(0),
		entered: false,
	});
	seen[source] = 1;
	let mut best = None;
	while let Some(frame) = stack.last_mut() {
		control.step()?;
		if !frame.entered {
			frame.entered = true;
			let accepted = if frame.node == target && hops.len() >= options.min_depth {
				let context = PathTerminalContext {
					hops: &hops,
					atoms: &projection.atoms,
					cost: frame.cost,
					work: Cell::new(0),
					remaining_work: control.limits.max_work - control.work,
				};
				let accepted = terminal(&context)?;
				control.work(usize::try_from(context.work.get()).map_err(|_cause| overflow())?)?;
				accepted
			} else {
				false
			};
			if accepted {
				if best.is_none_or(|best| frame.cost < best) {
					best = Some(frame.cost);
					found.clear();
				}
				if best == Some(frame.cost) {
					if found.len() == options.limits.max_paths {
						return Err(resource(
							"all-shortest paths",
							options.limits.max_paths as u64,
						));
					}
					control.admit(size_of::<Found>())?;
					found.try_reserve_exact(1).map_err(|cause| {
						resource("all-shortest results", options.limits.memory_bytes as u64)
							.with_technical_source(cause)
					})?;
					let mut sequence = control.array(hops.len())?;
					control.work(hops.len())?;
					sequence.extend_from_slice(&hops);
					let mut costs = control.array(contributions.len())?;
					control.work(contributions.len())?;
					costs.extend_from_slice(&contributions);
					found.push(Found {
						hops: sequence,
						contributions: costs,
						cost: frame.cost,
					});
				}
			}
		}
		let complete = hops.len() == depth
			|| frame.next == frame.end
			|| (options.mode == PathMode::Simple && frame.node == source && !hops.is_empty())
			|| (options.cost == PathCost::Hop
				&& (bounds[frame.node] == usize::MAX || bounds[frame.node] > depth - hops.len()));
		if complete {
			stack.pop();
			if let Some(hop) = hops.pop() {
				contributions.pop();
				seen[hop.target] -= 1;
				used[hop.connector] = false;
			}
			continue;
		}
		let hop = projection.arcs[frame.next];
		frame.next += 1;
		let allowed = match options.mode {
			PathMode::Walk => true,
			PathMode::Trail => !used[hop.connector],
			PathMode::Acyclic => seen[hop.target] == 0,
			PathMode::Simple => seen[hop.target] == 0 || hop.target == source,
		};
		if !allowed {
			continue;
		}
		let context = PathCostContext {
			connector: &tensor.connectors[hop.connector].atom,
			transition: hop,
			source: &projection.atoms[hop.source],
			target: &projection.atoms[hop.target],
			prefix: frame.cost,
			hops: &hops,
			atoms: &projection.atoms,
			work: Cell::new(0),
			remaining_work: control.limits.max_work - control.work,
		};
		if !branch(&context)? {
			control.work(usize::try_from(context.work.get()).map_err(|_cause| overflow())?)?;
			continue;
		}
		let contribution = match options.cost {
			PathCost::Hop => Number::Int(1),
			PathCost::Stored => projection.connector_weights[hop.connector],
			PathCost::Custom => cost(&context)?,
		};
		control.work(usize::try_from(context.work.get()).map_err(|_cause| overflow())?)?;
		if !number_is_finite(contribution) || contribution < Number::Int(0) {
			return Err(invalid("all-shortest requires finite nonnegative contributions"));
		}
		let next_cost = add_numbers(frame.cost, contribution)?;
		seen[hop.target] = seen[hop.target].checked_add(1).ok_or_else(overflow)?;
		used[hop.connector] = true;
		hops.push(hop);
		contributions.push(contribution);
		stack.push(Frame {
			node: hop.target,
			next: offsets[hop.target],
			end: offsets[hop.target + 1],
			cost: next_cost,
			entered: false,
		});
	}
	Ok(())
}
