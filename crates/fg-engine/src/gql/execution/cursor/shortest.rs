//! Canonical bounded shortest-path cursors for MATCH objectives.

use std::{
	cmp::Ordering,
	collections::{BTreeSet, BinaryHeap, VecDeque},
	mem::size_of,
};

use fg_runtime::__internal::RecordIdInternal as _;

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
/// Canonical hop state.
struct CanonicalHop {
	/// Edge associated with this canonical hop.
	edge: crate::graph::AtomRef,
	/// Target associated with this canonical hop.
	target: crate::graph::AtomRef,
	/// Direction associated with this canonical hop.
	direction: PathDirection,
	/// Role associated with this canonical hop.
	role: EndpointRole,
}

#[derive(Debug, Clone)]
/// Shortest-path frontier entry retaining its endpoint, candidate hops, cost
/// and resource charge.
struct SearchState {
	/// Endpoint associated with this search state.
	endpoint: AtomId,
	/// Hops associated with this search state.
	hops: Vec<TraversalCandidate>,
	/// Canonical associated with this search state.
	canonical: Vec<CanonicalHop>,
	/// Cost associated with this search state.
	cost: Number,
	/// Admissible completion bound used only for frontier ordering.
	priority: Number,
	/// Serial associated with this search state.
	serial: u64,
	/// Accounted bytes associated with this search state.
	accounted_bytes: usize,
}

impl SearchState {
	/// Returns the estimated heap bytes result.
	fn estimated_heap_bytes(&self) -> usize {
		let identities = self.canonical.iter().fold(0_usize, |bytes, hop| {
			bytes.saturating_add(canonical_atom_bytes(&hop.edge, &hop.target))
		});
		size_of::<Self>()
			.saturating_add(self.hops.capacity().saturating_mul(size_of::<TraversalCandidate>()))
			.saturating_add(self.canonical.capacity().saturating_mul(size_of::<CanonicalHop>()))
			.saturating_add(identities)
	}
}

/// Estimates canonical identities by borrowing their records, without clones.
pub(in crate::gql::execution) fn canonical_atom_bytes(
	edge: &crate::graph::AtomRef,
	target: &crate::graph::AtomRef,
) -> usize {
	(2 * size_of::<crate::graph::AtomRef>())
		.saturating_add(edge.id().estimated_heap_bytes())
		.saturating_add(target.id().estimated_heap_bytes())
}

/// Admits both complete buffers before allocation or prefix cloning.
///
/// # Errors
/// Returns typed overflow, admission or allocation errors.
pub(in crate::gql::execution) fn extend_path_buffers<Hop: Clone, Canonical: Clone>(
	hops: &[Hop],
	next_hop: Hop,
	canonical: &[Canonical],
	next_canonical: Canonical,
	retained_bytes: usize,
	admit: &mut impl FnMut(usize) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<(Vec<Hop>, Vec<Canonical>, usize)> {
	let overflow = || FerrosGrynnError::operation_overflow("MATCH path buffer bytes");
	let hop_count = hops.len().checked_add(1).ok_or_else(overflow)?;
	let canonical_count = canonical.len().checked_add(1).ok_or_else(overflow)?;
	let bytes = hop_count
		.checked_mul(size_of::<Hop>())
		.and_then(|bytes| {
			canonical_count
				.checked_mul(size_of::<Canonical>())
				.and_then(|other| bytes.checked_add(other))
		})
		.and_then(|bytes| bytes.checked_add(retained_bytes))
		.ok_or_else(overflow)?;
	admit(bytes)?;
	let allocation = |cause| {
		FerrosGrynnError::control_limit_exceeded("MATCH path buffer allocation", bytes as u64)
			.with_technical_source(cause)
	};
	let mut next_hops = Vec::new();
	next_hops.try_reserve_exact(hop_count).map_err(allocation)?;
	let mut next_sequence = Vec::new();
	next_sequence.try_reserve_exact(canonical_count).map_err(allocation)?;
	next_hops.extend_from_slice(hops);
	next_hops.push(next_hop);
	next_sequence.extend_from_slice(canonical);
	next_sequence.push(next_canonical);
	Ok((next_hops, next_sequence, bytes))
}

impl PartialEq for SearchState {
	/// Compares this `SearchState` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.priority, self.cost, &self.canonical, self.serial)
			== (other.priority, other.cost, &other.canonical, other.serial)
	}
}

impl Eq for SearchState {}

impl PartialOrd for SearchState {
	/// Compares this `SearchState` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for SearchState {
	/// Compares this `SearchState` with another value using its ordering
	/// contract.
	fn cmp(&self, other: &Self) -> Ordering {
		other
			.priority
			.cmp(&self.priority)
			.then_with(|| other.cost.cmp(&self.cost))
			.then_with(|| other.canonical.cmp(&self.canonical))
			.then_with(|| other.serial.cmp(&self.serial))
	}
}

/// Search frontier alternatives.
enum SearchFrontier {
	/// Breadth case of search frontier.
	Breadth(
		/// Breadth case of search frontier.
		VecDeque<SearchState>,
	),
	/// Weighted case of search frontier.
	Weighted(
		/// Weighted case of search frontier.
		BinaryHeap<SearchState>,
	),
}

/// Exact terminal row, ordered by runtime cost and the complete canonical path.
pub(in crate::gql::execution) struct CompletedPath<State> {
	/// Full state retained until pending lower bounds prove this result final.
	pub(in crate::gql::execution) state: State,
	/// Validated path bindings and public values.
	pub(in crate::gql::execution) row: Row,
	/// Row bytes admitted in addition to the retained state.
	pub(in crate::gql::execution) row_bytes: usize,
}

impl<State: PartialEq> PartialEq for CompletedPath<State> {
	/// Compares this `CompletedPath<State>` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.state == other.state
	}
}

impl<State: Eq> Eq for CompletedPath<State> {}

impl<State: Ord> PartialOrd for CompletedPath<State> {
	/// Compares this `CompletedPath<State>` with another value when they are
	/// ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl<State: Ord> Ord for CompletedPath<State> {
	/// Compares this `CompletedPath<State>` with another value using its
	/// ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.state.cmp(&other.state)
	}
}

/// Bounds the runtime reduction, including integer-to-float conversion.
/// Three rounding events per remaining hop are conservative for the supported
/// numeric matrix: exact integers, 96-bit decimal addition, or binary64 casts
/// and addition. Bernoulli's inequality bounds their accumulated relative loss;
/// the smallest subnormal covers underflow. Every floating operation here is
/// rounded down, and overflow uses a finite lower bound.
pub(in crate::gql::execution) fn completion_bound(
	prefix: Number,
	suffix: Number,
	remaining: u32,
) -> FerrosGrynnResult<Number> {
	if remaining == 0 {
		return Ok(prefix);
	}
	if !crate::math::number_is_finite(prefix) {
		return Ok(Number::Int(0));
	}
	let prefix = crate::graph::DistanceInterval::from_number(prefix)?.lower;
	let suffix = crate::graph::DistanceInterval::from_number(suffix)?.lower;
	let sum = (prefix + suffix).next_down().max(0.0);
	let rounds = 3.0 * f64::from(remaining);
	let factor = (1.0 - rounds * f64::EPSILON).next_down().max(0.0);
	let relative = (sum * factor).next_down().max(0.0);
	let underflow = (rounds * f64::from_bits(1)).next_up();
	Ok(Number::Float((relative - underflow).next_down().max(0.0)))
}

/// A float prefix never narrows again. An integer is monotone only if a later
/// float cast is exact or upward; decimals retain the completion-bound proof.
pub(in crate::gql::execution) fn monotone_path_prefix(cost: Number) -> bool {
	fg_runtime::__internal::monotone_path_prefix(cost)
}

impl SearchFrontier {
	/// Creates a new `SearchFrontier` value.
	fn new(objective: crate::syn::MatchPathObjective) -> Self {
		if objective.is_weighted() {
			Self::Weighted(BinaryHeap::new())
		} else {
			Self::Breadth(VecDeque::new())
		}
	}

	/// Pushes search frontier.
	fn push(&mut self, state: SearchState) {
		match self {
			Self::Breadth(frontier) => frontier.push_back(state),
			Self::Weighted(frontier) => frontier.push(state),
		}
	}

	/// Pops search frontier.
	fn pop(&mut self) -> Option<SearchState> {
		match self {
			Self::Breadth(frontier) => frontier.pop_front(),
			Self::Weighted(frontier) => frontier.pop(),
		}
	}

	/// Clears search frontier.
	fn clear(&mut self) {
		match self {
			Self::Breadth(frontier) => frontier.clear(),
			Self::Weighted(frontier) => frontier.clear(),
		}
	}

	/// Returns the number of logical elements.
	fn len(&self) -> usize {
		match self {
			Self::Breadth(frontier) => frontier.len(),
			Self::Weighted(frontier) => frontier.len(),
		}
	}

	/// The smallest pending admissible cost, including canonical tie work.
	fn minimum_priority(&self) -> Option<Number> {
		match self {
			Self::Breadth(frontier) => frontier.front().map(|state| state.priority),
			Self::Weighted(frontier) => frontier.peek().map(|state| state.priority),
		}
	}
}

/// Emits one canonical shortest path for each matching source-target pair.
pub(in crate::gql::execution) struct ShortestPathCursor<'a, Read: QueryRead, Graph> {
	/// Input associated with this shortest path cursor.
	input: Box<dyn RowCursor + 'a>,
	/// Graph associated with this shortest path cursor.
	graph: Rc<Graph>,
	/// Transaction adapter for snapshot-exact lower bounds.
	read: &'a Read,
	/// Immutable read root captured by this cursor.
	snapshot: Snapshot,
	/// Optional compatible Ready generation selected by the planner.
	landmark: Option<crate::datastore::QueryLandmarkPath>,
	/// Bound local target, absent for all-target traversal.
	heuristic_target: Option<AtomId>,
	/// Terminal states await proof against every pending completion bound.
	completed: BinaryHeap<CompletedPath<SearchState>>,
	/// Graph ref associated with this shortest path cursor.
	graph_ref: Arc<GraphRef>,
	/// Index associated with this shortest path cursor.
	index: Rc<TraversalIndex>,
	/// Seed algorithms retain their incidence relation, including undirected
	/// edges.
	relation_direction: Option<TraversalDirection>,
	/// Expand associated with this shortest path cursor.
	expand: ResolvedPathExpand,
	/// Predicate associated with this shortest path cursor.
	predicate: MatchPredicateContext<'a>,
	/// Trace associated with this shortest path cursor.
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	/// Row associated with this shortest path cursor.
	row: Option<Row>,
	/// Source associated with this shortest path cursor.
	source: Option<AtomId>,
	/// Bound target associated with this shortest path cursor.
	bound_target: bool,
	/// Frontier associated with this shortest path cursor.
	frontier: SearchFrontier,
	/// Seen targets associated with this shortest path cursor.
	seen_targets: BTreeSet<crate::graph::AtomRef>,
	/// Memory admission account retained with the owned data.
	account: Option<BarrierAccount>,
	/// Next serial associated with this shortest path cursor.
	next_serial: u64,
}

impl<'a, Read: QueryRead, Graph: GraphRead> ShortestPathCursor<'a, Read, Graph> {
	#[allow(
		clippy::too_many_arguments,
		reason = "the concrete shortest operator keeps all physical-plan inputs explicit"
	)]
	/// Creates a new `ShortestPathCursor<'a, Read, Graph>` value.
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		graph_ref: Arc<GraphRef>,
		index: Rc<TraversalIndex>,
		expand: ResolvedPathExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
		read: &'a Read,
		snapshot: Snapshot,
		landmark: Option<crate::datastore::QueryLandmarkPath>,
	) -> Self {
		let objective = expand.objective.unwrap_or(crate::syn::MatchPathObjective::Shortest);
		Self {
			input,
			graph,
			read,
			snapshot,
			landmark,
			heuristic_target: None,
			completed: BinaryHeap::new(),
			graph_ref,
			index,
			relation_direction: None,
			expand,
			predicate,
			trace,
			row: None,
			source: None,
			bound_target: false,
			frontier: SearchFrontier::new(objective),
			seen_targets: BTreeSet::new(),
			account: None,
			next_serial: 0,
		}
	}

	/// Uses the traversal projection instead of a MATCH arrow topology filter.
	pub(in crate::gql::execution) fn with_relation_direction(
		mut self,
		direction: TraversalDirection,
	) -> Self {
		self.relation_direction = Some(direction);
		self
	}

	/// Pushes state.
	fn push_state(
		&mut self,
		mut state: SearchState,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		if state.accounted_bytes == 0 {
			state.accounted_bytes = state.estimated_heap_bytes();
			let account = self.account.get_or_insert_with(|| control.barrier_account());
			if !account.reserve(state.accounted_bytes) {
				return Err(FerrosGrynnError::control_limit_exceeded(
					"MATCH frontier memory",
					u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
				));
			}
		}
		self.frontier.push(state);
		control.record_match_frontier(self.frontier.len());
		Ok(())
	}

	/// Returns the start row result.
	fn start_row(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next(control)? else {
			return Ok(false);
		};
		let source = row
			.current
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH shortest path has no source endpoint.")
			})?
			.atom;
		self.bound_target = self
			.expand
			.target
			.column
			.and_then(|column| row.atoms.get(column).copied().flatten())
			.is_some();
		self.heuristic_target = if self.landmark.is_some()
			|| self.expand.objective == Some(crate::syn::MatchPathObjective::WeightedShortest)
		{
			if let Some(bound) = self
				.expand
				.target
				.column
				.and_then(|column| row.atoms.get(column).copied().flatten())
				.filter(|bound| bound.graph == self.graph_ref.id())
			{
				Some(bound.atom)
			} else if let ResolvedMatchSelector::Record(record) = &self.expand.target.selector {
				self.graph.atom_id(&crate::graph::AtomRef::new(
					self.expand.target.domain.atom_kind(),
					record.clone(),
				))?
			} else {
				None
			}
		} else {
			None
		};
		self.frontier.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
		self.seen_targets.clear();
		self.completed.clear();
		self.frontier = if self.heuristic_target.is_some() {
			SearchFrontier::Weighted(BinaryHeap::new())
		} else {
			SearchFrontier::new(
				self.expand.objective.unwrap_or(crate::syn::MatchPathObjective::Shortest),
			)
		};
		self.next_serial = 1;
		self.source = Some(source);
		self.row = Some(row);
		let priority = self.priority(source, Number::Int(0), self.expand.max, control, true)?;
		self.push_state(
			SearchState {
				endpoint: source,
				hops: Vec::new(),
				canonical: Vec::new(),
				cost: Number::Int(0),
				priority,
				serial: 0,
				accounted_bytes: 0,
			},
			control,
		)?;
		Ok(true)
	}

	/// Finishes row.
	fn finish_row(&mut self) {
		self.frontier.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
		self.seen_targets.clear();
		self.completed.clear();
		self.heuristic_target = None;
		self.source = None;
		self.row = None;
	}

	/// Every path state can be revisited with a better prefix. Only the bound
	/// affects heap order; depth and uniqueness remain exact path constraints.
	fn priority(
		&self,
		source: AtomId,
		cost: Number,
		remaining: u32,
		control: &ExecutionControl,
		initial: bool,
	) -> FerrosGrynnResult<Number> {
		if let (Some(binding), Some(target)) = (&self.landmark, self.heuristic_target)
			&& let Some(bound) =
				self.read.path_lower_bound(self.snapshot, binding, source, target)?
		{
			control.record_landmark_lookup(if initial {
				binding.bytes
			} else {
				0
			});
			return if self.expand.objective
				== Some(crate::syn::MatchPathObjective::WeightedShortest)
			{
				completion_bound(cost, bound, remaining)
			} else {
				add_path_cost(cost, bound)
			};
		}
		if initial {
			control.record_landmark_fallback();
		}
		if self.expand.objective == Some(crate::syn::MatchPathObjective::WeightedShortest) {
			completion_bound(cost, Number::Int(0), remaining)
		} else {
			Ok(cost)
		}
	}
}

impl<Read: QueryRead, Graph: GraphRead> ShortestPathCursor<'_, Read, Graph> {
	/// Returns the ordered candidates result.
	fn ordered_candidates(
		&self,
		endpoint: AtomId,
	) -> FerrosGrynnResult<Vec<(CanonicalHop, TraversalCandidate)>> {
		let candidates = if let Some(direction) = self.relation_direction {
			self.index
				.steps(&EdgeEndpoint::Local(endpoint), direction)
				.filter_map(|step| {
					step.target().local_id().map(|target| TraversalCandidate {
						target,
						edge: step.via_edge(),
						direction: step.direction(),
						role: step.role(),
					})
				})
				.collect()
		} else {
			index_candidates(
				self.graph.as_ref(),
				self.index.as_ref(),
				endpoint,
				self.expand.direction,
			)?
		};
		let mut candidates = candidates
			.into_iter()
			.map(|candidate| {
				let edge = self.graph.atom_ref(candidate.edge)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("edge", candidate.edge.to_string())
				})?;
				let target = self.graph.atom_ref(candidate.target)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("atom", candidate.target.to_string())
				})?;
				Ok((
					CanonicalHop {
						edge,
						target,
						direction: candidate.direction,
						role: candidate.role,
					},
					candidate,
				))
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		candidates.sort_by(|left, right| left.0.cmp(&right.0));
		Ok(candidates)
	}

	/// Returns the expand state result.
	fn expand_state(
		&mut self,
		state: &SearchState,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		let source = self.source.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("MATCH shortest path lost its source endpoint.")
		})?;
		if state.hops.len() >= self.expand.max as usize
			|| (self.expand.path_mode == crate::syn::MatchPathMode::Simple
				&& !state.hops.is_empty()
				&& state.endpoint == source)
		{
			return Ok(());
		}
		let fields =
			(self.relation_direction.is_some() && !self.expand.filters.is_empty()).then(|| {
				MatchGraphFields::new(
					self.read,
					self.snapshot,
					self.read.property_dictionary(self.snapshot),
					self.graph_ref.clone(),
				)
			});
		for (canonical, candidate) in self.ordered_candidates(state.endpoint)? {
			control.record_path_expansion()?;
			if let Some(fields) = &fields {
				let edge = self.graph.edge(candidate.edge)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("edge", candidate.edge.to_string())
				})?;
				let scoped = ScopedTraversalCandidate {
					target: BoundAtom::new(self.graph_ref.id(), candidate.target),
					edge: BoundAtom::new(self.graph_ref.id(), candidate.edge),
					direction: candidate.direction,
					role: candidate.role,
					topology: edge.topology()?.direction(),
					cross_level: false,
				};
				if !scoped_filters_match(
					Some(BoundAtom::new(self.graph_ref.id(), state.endpoint)),
					&self.expand.filters,
					&scoped,
					self.graph.as_ref(),
					self.graph.as_ref(),
					self.read,
					self.snapshot,
					fields,
				)? {
					continue;
				}
			}
			let allowed = path_candidate_allowed(
				self.row.as_ref().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH shortest path lost its input row.")
				})?,
				source,
				&state.hops,
				&self.expand,
				&candidate,
				self.graph.as_ref(),
				&self.graph_ref,
				self.predicate,
				control,
			)?;
			if !allowed {
				continue;
			}
			let cost = add_path_cost(
				state.cost,
				path_hop_cost(
					self.graph.as_ref(),
					&self.graph_ref,
					&self.expand,
					&candidate,
					state.cost,
					self.predicate,
					control,
				)?,
			)?;
			let identities = state
				.canonical
				.iter()
				.chain(std::iter::once(&canonical))
				.fold(size_of::<SearchState>(), |bytes, hop| {
					bytes.saturating_add(canonical_atom_bytes(&hop.edge, &hop.target))
				});
			let account = self.account.get_or_insert_with(|| control.barrier_account());
			let (hops, sequence, accounted_bytes) = extend_path_buffers(
				&state.hops,
				candidate.clone(),
				&state.canonical,
				canonical,
				identities,
				&mut |bytes| {
					if account.reserve(bytes) {
						Ok(())
					} else {
						Err(FerrosGrynnError::control_limit_exceeded(
							"MATCH frontier memory",
							control.barrier_memory_limit() as u64,
						))
					}
				},
			)?;
			let serial = self.next_serial;
			self.next_serial = self.next_serial.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("MATCH shortest-path frontier serial")
			})?;
			let remaining = self.expand.max.saturating_sub(hops.len() as u32);
			self.push_state(
				SearchState {
					endpoint: candidate.target,
					hops,
					canonical: sequence,
					cost,
					priority: self.priority(candidate.target, cost, remaining, control, false)?,
					serial,
					accounted_bytes,
				},
				control,
			)?;
		}
		Ok(())
	}

	/// Returns the output row result.
	fn output_row(
		&mut self,
		state: &SearchState,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Option<Row>> {
		if state.hops.len() < self.expand.min as usize
			|| state.hops.len() > self.expand.max as usize
			|| !atom_matches(self.graph.as_ref(), state.endpoint, &self.expand.target)?
		{
			return Ok(None);
		}
		let target = self.graph.atom_ref(state.endpoint)?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("atom", state.endpoint.to_string())
		})?;
		if self.seen_targets.contains(&target) {
			return Ok(None);
		}
		let row = path_row(
			self.row.as_ref().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH shortest path lost its input row.")
			})?,
			&self.expand,
			self.source.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH shortest path lost its source endpoint.")
			})?,
			state.endpoint,
			&state.hops,
			(self.graph.as_ref(), &self.graph_ref),
			self.predicate,
			control,
			self.trace.as_ref(),
		)?;
		if row.is_some()
			&& self.heuristic_target.is_none()
			&& self.expand.objective != Some(crate::syn::MatchPathObjective::WeightedShortest)
		{
			self.seen_targets.insert(target);
		}
		Ok(row)
	}
}

impl<Read: QueryRead, Graph: GraphRead> RowCursor for ShortestPathCursor<'_, Read, Graph> {
	/// Advances this `ShortestPathCursor<'_, Read, Graph>` and returns its next
	/// element, if any.
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if self.row.is_none() && !self.start_row(control)? {
				return Ok(None);
			}
			if self.completed.peek().is_some_and(|best| {
				self.frontier.minimum_priority().is_none_or(|pending| pending > best.state.cost)
			}) {
				let best = self.completed.pop().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH shortest path lost its best result")
				})?;
				let target = self.graph.atom_ref(best.state.endpoint)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("atom", best.state.endpoint.to_string())
				})?;
				if let Some(account) = &mut self.account {
					account.release(best.state.accounted_bytes.saturating_add(best.row_bytes));
				}
				if !self.seen_targets.insert(target) {
					continue;
				}
				if self.bound_target || self.heuristic_target.is_some() {
					self.finish_row();
				}
				return Ok(Some(best.row));
			}
			let Some(mut state) = self.frontier.pop() else {
				self.finish_row();
				continue;
			};
			let weighted =
				self.expand.objective == Some(crate::syn::MatchPathObjective::WeightedShortest);
			if weighted
				&& self.heuristic_target.is_some()
				&& monotone_path_prefix(state.cost)
				&& self.completed.peek().is_some_and(|best| {
					(state.cost, &state.canonical) >= (best.state.cost, &best.state.canonical)
				}) {
				if let Some(account) = &mut self.account {
					account.release(state.accounted_bytes);
				}
				continue;
			}
			let improves = weighted
				|| self.completed.peek().is_none_or(|best| {
					(state.cost, &state.canonical) < (best.state.cost, &best.state.canonical)
				});
			let output = if improves {
				self.output_row(&state, control)?
			} else {
				None
			};
			if self.heuristic_target.is_some() || weighted {
				if weighted
					|| (output.is_none()
						&& self.completed.peek().is_none_or(|best| state.cost < best.state.cost))
				{
					self.expand_state(&state, control)?;
				}
				if let Some(row) = output {
					let bytes = row
						.estimated_heap_bytes()
						.saturating_add(size_of::<CompletedPath<SearchState>>());
					let account = self.account.get_or_insert_with(|| control.barrier_account());
					if !account.reserve(bytes) {
						return Err(FerrosGrynnError::control_limit_exceeded(
							"MATCH best path memory",
							control.barrier_memory_limit() as u64,
						));
					}
					if !weighted {
						while let Some(previous) = self.completed.pop() {
							account.release(
								previous.state.accounted_bytes.saturating_add(previous.row_bytes),
							);
						}
					}
					state.priority = state.cost;
					self.completed.push(CompletedPath {
						state,
						row,
						row_bytes: bytes,
					});
					continue;
				}
				if let Some(account) = &mut self.account {
					account.release(state.accounted_bytes);
				}
				continue;
			}
			if output.is_some() && self.bound_target {
				self.finish_row();
				return Ok(output);
			}
			self.expand_state(&state, control)?;
			if let Some(account) = &mut self.account {
				account.release(state.accounted_bytes);
			}
			if output.is_some() {
				return Ok(output);
			}
		}
	}
}

#[cfg(test)]
/// Regression tests for shortest behavior.
mod numeric_tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies path buffer admission precedes all prefix clones.
	#[test]
	fn path_buffer_admission_precedes_all_prefix_clones() {
		/// Test payload counting clones to verify admission occurs before
		/// duplication.
		struct CountedClone(
			/// Shared counter incremented by each payload clone.
			Rc<std::cell::Cell<usize>>,
			/// Distinct payload byte used to identify the copied path element.
			u8,
		);
		impl Clone for CountedClone {
			/// Returns an owned clone of this `CountedClone`.
			fn clone(&self) -> Self {
				self.0.set(self.0.get() + 1);
				Self(self.0.clone(), self.1)
			}
		}
		let clones = Rc::new(std::cell::Cell::new(0));
		let hops = [CountedClone(clones.clone(), 1), CountedClone(clones.clone(), 2)];
		let canonical = [CountedClone(clones.clone(), 3)];
		let mut admitted = false;
		let error = extend_path_buffers(
			&hops,
			CountedClone(clones.clone(), 4),
			&canonical,
			CountedClone(clones.clone(), 5),
			64,
			&mut |_| {
				admitted = true;
				Err(FerrosGrynnError::control_limit_exceeded("test admission", 1))
			},
		);
		assert!(error.is_err());
		assert!(admitted);
		assert_eq!(clones.get(), 0);
		let (grown_hops, grown_canonical, bytes) = extend_path_buffers(
			&hops,
			CountedClone(clones.clone(), 4),
			&canonical,
			CountedClone(clones.clone(), 5),
			64,
			&mut |bytes| {
				assert_eq!(clones.get(), 0);
				assert_eq!(bytes, 64 + 5 * size_of::<CountedClone>());
				Ok(())
			},
		)
		.assert_ok();
		assert_eq!(bytes, 64 + 5 * size_of::<CountedClone>());
		assert_eq!(clones.get(), 3);
		assert_eq!(grown_hops.iter().map(|item| item.1).collect::<Vec<_>>(), [1, 2, 4]);
		assert_eq!(grown_canonical.iter().map(|item| item.1).collect::<Vec<_>>(), [3, 5]);
		let admission_calls = std::cell::Cell::new(0);
		assert!(
			extend_path_buffers(
				&hops,
				CountedClone(clones.clone(), 4),
				&canonical,
				CountedClone(clones.clone(), 5),
				usize::MAX,
				&mut |_| {
					admission_calls.set(admission_calls.get() + 1);
					Ok(())
				},
			)
			.is_err()
		);
		assert_eq!(clones.get(), 3);
		assert_eq!(admission_calls.get(), 0, "overflow must precede admission");
	}

	/// Verifies completion bounds enclose runtime reductions at numeric
	/// boundaries.
	#[test]
	fn completion_bounds_enclose_runtime_reductions_at_numeric_boundaries() {
		use crate::test_assertions::AssertResult as _;

		let numbers = [
			Number::Int(0),
			Number::Int(1),
			Number::Int(9_007_199_254_740_993),
			Number::Int(i64::MAX),
			Number::Float(f64::from_bits(1)),
			Number::Float(f64::MIN_POSITIVE),
			Number::Float(0.1),
			Number::Float(f64::MAX),
			Number::Decimal(rust_decimal::Decimal::MAX),
			Number::Decimal(
				"0.0000000000000000000000000001"
					.parse()
					.map_err(|cause| {
						FerrosGrynnError::invalid_argument("test decimal")
							.with_technical_source(cause)
					})
					.assert_ok(),
			),
		];
		for prefix in numbers {
			for first in numbers {
				for second in numbers {
					let mut cost = prefix;
					let mut suffix = crate::graph::DistanceInterval::ZERO;
					for length in 1..=30 {
						let edge = if length % 2 == 0 {
							first
						} else {
							second
						};
						let Ok(next) = add_path_cost(cost, edge) else {
							break;
						};
						cost = next;
						suffix = suffix
							.add(crate::graph::DistanceInterval::from_number(edge).assert_ok());
						let bound = completion_bound(prefix, Number::Float(suffix.lower), length)
							.assert_ok();
						assert!(
							bound <= cost,
							"prefix={prefix:?} first={first:?} second={second:?} length={length} \
							 bound={bound:?} cost={cost:?}"
						);
					}
				}
			}
		}
		let prefix = Number::Int(9_007_199_254_740_993);
		let actual = add_path_cost(prefix, Number::Float(0.0)).assert_ok();
		assert!(actual < prefix);
		assert!(completion_bound(prefix, Number::Int(0), 1).assert_ok() <= actual);
	}
}
