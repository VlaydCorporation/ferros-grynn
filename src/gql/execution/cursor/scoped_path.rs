//! Variable and shortest paths across nested graph occurrences.

use std::{
	cmp::Ordering,
	collections::{BTreeSet, BinaryHeap, VecDeque},
	mem::size_of,
	rc::Rc,
};

use super::*;

struct ScopedDfsFrame {
	endpoint: BoundAtom,
	candidates: Vec<ScopedTraversalCandidate>,
	next_candidate: usize,
	yielded: bool,
	accounted_bytes: usize,
}

/// Incremental DFS over scoped adjacency. Only the active path and one
/// candidate list per active depth are retained.
pub(in crate::gql::execution) struct ScopedPathExpandCursor<'a, Read: QueryRead> {
	input: Box<dyn RowCursor + 'a>,
	read: &'a Read,
	snapshot: Snapshot,
	fields: &'a MatchGraphFields<'a, Read>,
	index: Rc<ScopedTraversalIndex>,
	expand: ResolvedPathExpand,
	predicate: MatchPredicateContext<'a>,
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	origin: crate::values::Uuid,
	snapshot_id: crate::graph::SnapshotId,
	row: Option<Row>,
	frames: Vec<ScopedDfsFrame>,
	hops: Vec<ScopedTraversalCandidate>,
	account: Option<BarrierAccount>,
}

impl<'a, Read: QueryRead> ScopedPathExpandCursor<'a, Read> {
	#[allow(
		clippy::too_many_arguments,
		reason = "the physical path operator keeps its planned inputs explicit"
	)]
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		read: &'a Read,
		snapshot: Snapshot,
		fields: &'a MatchGraphFields<'a, Read>,
		index: Rc<ScopedTraversalIndex>,
		expand: ResolvedPathExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
		origin: crate::values::Uuid,
		snapshot_id: crate::graph::SnapshotId,
	) -> Self {
		Self {
			input,
			read,
			snapshot,
			fields,
			index,
			expand,
			predicate,
			trace,
			origin,
			snapshot_id,
			row: None,
			frames: Vec::new(),
			hops: Vec::new(),
			account: None,
		}
	}

	fn start_row(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next(control)? else {
			return Ok(false);
		};
		let source = row.current.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("scoped MATCH path has no source endpoint")
		})?;
		self.clear_frames();
		self.push_frame(source, self.index.candidates(source, self.expand.direction), control)?;
		self.hops.clear();
		self.row = Some(row);
		Ok(true)
	}

	fn push_frame(
		&mut self,
		endpoint: BoundAtom,
		candidates: Vec<ScopedTraversalCandidate>,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		let bytes = size_of::<ScopedDfsFrame>().saturating_add(
			candidates.capacity().saturating_mul(size_of::<ScopedTraversalCandidate>()),
		);
		let account = self.account.get_or_insert_with(|| control.barrier_account());
		if !account.reserve(bytes) {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"MATCH frontier memory",
				u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
			));
		}
		self.frames.push(ScopedDfsFrame {
			endpoint,
			candidates,
			next_candidate: 0,
			yielded: false,
			accounted_bytes: bytes,
		});
		control.record_match_frontier(self.frames.len());
		Ok(())
	}

	fn pop_frame(&mut self) {
		if let Some(frame) = self.frames.pop()
			&& let Some(account) = &mut self.account
		{
			account.release(frame.accounted_bytes);
		}
		if self.frames.is_empty() {
			self.row = None;
		} else {
			self.hops.pop();
		}
	}

	fn clear_frames(&mut self) {
		self.frames.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
	}
}

impl<Read: QueryRead> RowCursor for ScopedPathExpandCursor<'_, Read> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if self.row.is_none() && !self.start_row(control)? {
				return Ok(None);
			}
			let depth = self.hops.len();
			let emit = {
				let frame = self.frames.last_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("scoped MATCH DFS stack is empty")
				})?;
				let emit = !frame.yielded
					&& depth >= self.expand.min as usize
					&& depth <= self.expand.max as usize;
				frame.yielded = true;
				emit
			};
			if emit {
				let endpoint = self.frames.last().map(|frame| frame.endpoint).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("scoped MATCH DFS stack is empty")
				})?;
				if scoped_atom_matches(
					self.read,
					self.snapshot,
					self.fields,
					endpoint,
					&self.expand.target,
				)? && let Some(row) = scoped_path_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("scoped MATCH path lost its input row")
					})?,
					&self.expand,
					self.frames.first().map(|frame| frame.endpoint).ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"scoped MATCH path has no source endpoint",
						)
					})?,
					endpoint,
					&self.hops,
					self.read,
					self.snapshot,
					self.fields,
					self.predicate,
					control,
					self.trace.as_ref(),
					self.origin,
					self.snapshot_id,
				)? {
					return Ok(Some(row));
				}
			}

			if depth >= self.expand.max as usize {
				self.pop_frame();
				continue;
			}
			let candidate = {
				let frame = self.frames.last_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("scoped MATCH DFS stack is empty")
				})?;
				let candidate = frame.candidates.get(frame.next_candidate).cloned();
				frame.next_candidate += usize::from(candidate.is_some());
				candidate
			};
			let Some(candidate) = candidate else {
				self.pop_frame();
				continue;
			};
			control.record_path_expansion()?;
			let source = self.frames.last().map(|frame| frame.endpoint).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("scoped MATCH DFS stack is empty")
			})?;
			if !scoped_path_candidate_allowed(
				self.row.as_ref().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("scoped MATCH path lost its input row")
				})?,
				self.frames.first().map(|frame| frame.endpoint).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("scoped MATCH path has no source endpoint")
				})?,
				source,
				&self.hops,
				&self.expand,
				&candidate,
				self.read,
				self.snapshot,
				self.fields,
				self.predicate,
				control,
			)? {
				continue;
			}
			let closes_simple = self.expand.path_mode == crate::syn::MatchPathMode::Simple
				&& self.frames.first().is_some_and(|frame| frame.endpoint == candidate.target);
			let candidates = if closes_simple {
				Vec::new()
			} else {
				self.index.candidates(candidate.target, self.expand.direction)
			};
			self.hops.push(candidate.clone());
			if let Err(error) = self.push_frame(candidate.target, candidates, control) {
				self.hops.pop();
				return Err(error);
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalScopedHop {
	edge_graph: crate::graph::GraphId,
	edge: crate::graph::AtomRef,
	target_graph: crate::graph::GraphId,
	target: crate::graph::AtomRef,
	direction: PathDirection,
	role: EndpointRole,
}

#[derive(Debug, Clone)]
struct ScopedSearchState {
	endpoint: BoundAtom,
	hops: Vec<ScopedTraversalCandidate>,
	canonical: Vec<CanonicalScopedHop>,
	cost: Number,
	/// Conservative bound on every remaining runtime numeric reduction.
	priority: Number,
	serial: u64,
	accounted_bytes: usize,
}

impl ScopedSearchState {
	fn estimated_heap_bytes(&self) -> usize {
		let identities = self.canonical.iter().fold(0_usize, |bytes, hop| {
			bytes.saturating_add(canonical_atom_bytes(&hop.edge, &hop.target))
		});
		size_of::<Self>()
			.saturating_add(
				self.hops.capacity().saturating_mul(size_of::<ScopedTraversalCandidate>()),
			)
			.saturating_add(
				self.canonical.capacity().saturating_mul(size_of::<CanonicalScopedHop>()),
			)
			.saturating_add(identities)
	}
}

impl PartialEq for ScopedSearchState {
	fn eq(&self, other: &Self) -> bool {
		(self.priority, self.cost, &self.canonical, self.serial)
			== (other.priority, other.cost, &other.canonical, other.serial)
	}
}

impl Eq for ScopedSearchState {}

impl PartialOrd for ScopedSearchState {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for ScopedSearchState {
	fn cmp(&self, other: &Self) -> Ordering {
		other
			.priority
			.cmp(&self.priority)
			.then_with(|| other.cost.cmp(&self.cost))
			.then_with(|| other.canonical.cmp(&self.canonical))
			.then_with(|| other.serial.cmp(&self.serial))
	}
}

enum ScopedSearchFrontier {
	Breadth(VecDeque<ScopedSearchState>),
	Weighted(BinaryHeap<ScopedSearchState>),
}

impl ScopedSearchFrontier {
	fn new(objective: crate::syn::MatchPathObjective) -> Self {
		match objective {
			crate::syn::MatchPathObjective::Shortest
			| crate::syn::MatchPathObjective::AllShortest => Self::Breadth(VecDeque::new()),
			crate::syn::MatchPathObjective::WeightedShortest
			| crate::syn::MatchPathObjective::AllWeightedShortest => Self::Weighted(BinaryHeap::new()),
		}
	}

	fn push(&mut self, state: ScopedSearchState) {
		match self {
			Self::Breadth(frontier) => frontier.push_back(state),
			Self::Weighted(frontier) => frontier.push(state),
		}
	}

	fn pop(&mut self) -> Option<ScopedSearchState> {
		match self {
			Self::Breadth(frontier) => frontier.pop_front(),
			Self::Weighted(frontier) => frontier.pop(),
		}
	}

	fn clear(&mut self) {
		match self {
			Self::Breadth(frontier) => frontier.clear(),
			Self::Weighted(frontier) => frontier.clear(),
		}
	}

	fn len(&self) -> usize {
		match self {
			Self::Breadth(frontier) => frontier.len(),
			Self::Weighted(frontier) => frontier.len(),
		}
	}

	/// The smallest pending bound; equality still requires canonical tie work.
	fn minimum_priority(&self) -> Option<Number> {
		match self {
			Self::Breadth(frontier) => frontier.front().map(|state| state.priority),
			Self::Weighted(frontier) => frontier.peek().map(|state| state.priority),
		}
	}
}

/// Canonical BFS/Dijkstra over scoped adjacency.
pub(in crate::gql::execution) struct ScopedShortestPathCursor<'a, Read: QueryRead> {
	input: Box<dyn RowCursor + 'a>,
	read: &'a Read,
	snapshot: Snapshot,
	fields: &'a MatchGraphFields<'a, Read>,
	index: Rc<ScopedTraversalIndex>,
	expand: ResolvedPathExpand,
	predicate: MatchPredicateContext<'a>,
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	origin: crate::values::Uuid,
	snapshot_id: crate::graph::SnapshotId,
	row: Option<Row>,
	source: Option<BoundAtom>,
	bound_target: bool,
	frontier: ScopedSearchFrontier,
	/// Qualified terminal rows awaiting proof against pending completion
	/// bounds.
	completed: BinaryHeap<CompletedPath<ScopedSearchState>>,
	seen_targets: BTreeSet<BoundAtom>,
	account: Option<BarrierAccount>,
	next_serial: u64,
}

impl<'a, Read: QueryRead> ScopedShortestPathCursor<'a, Read> {
	#[allow(
		clippy::too_many_arguments,
		reason = "the concrete shortest operator keeps its physical inputs explicit"
	)]
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		read: &'a Read,
		snapshot: Snapshot,
		fields: &'a MatchGraphFields<'a, Read>,
		index: Rc<ScopedTraversalIndex>,
		expand: ResolvedPathExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
		origin: crate::values::Uuid,
		snapshot_id: crate::graph::SnapshotId,
	) -> Self {
		let objective = expand.objective.unwrap_or(crate::syn::MatchPathObjective::Shortest);
		Self {
			input,
			read,
			snapshot,
			fields,
			index,
			expand,
			predicate,
			trace,
			origin,
			snapshot_id,
			row: None,
			source: None,
			bound_target: false,
			frontier: ScopedSearchFrontier::new(objective),
			completed: BinaryHeap::new(),
			seen_targets: BTreeSet::new(),
			account: None,
			next_serial: 0,
		}
	}

	fn push_state(
		&mut self,
		mut state: ScopedSearchState,
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

	fn start_row(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next(control)? else {
			return Ok(false);
		};
		let source = row.current.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("scoped MATCH shortest path has no source endpoint")
		})?;
		control.record_landmark_fallback();
		self.bound_target = self
			.expand
			.target
			.column
			.and_then(|column| row.atoms.get(column).copied().flatten())
			.is_some();
		self.clear_row();
		self.next_serial = 1;
		self.source = Some(source);
		self.row = Some(row);
		self.push_state(
			ScopedSearchState {
				endpoint: source,
				hops: Vec::new(),
				canonical: Vec::new(),
				cost: Number::Int(0),
				priority: Number::Int(0),
				serial: 0,
				accounted_bytes: 0,
			},
			control,
		)?;
		Ok(true)
	}

	fn clear_row(&mut self) {
		self.frontier.clear();
		self.completed.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
		self.seen_targets.clear();
		self.source = None;
		self.row = None;
	}

	fn ordered_candidates(
		&self,
		endpoint: BoundAtom,
	) -> FerrosGrynnResult<Vec<(CanonicalScopedHop, ScopedTraversalCandidate)>> {
		let mut candidates = self
			.index
			.candidates(endpoint, self.expand.direction)
			.into_iter()
			.map(|candidate| {
				let edge_ref = self.fields.reference(candidate.edge.graph)?;
				let edge_graph = self.read.graph(self.snapshot, &edge_ref)?;
				let edge = edge_graph.atom_ref(candidate.edge.atom)?.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scoped MATCH shortest connector disappeared",
					)
				})?;
				let target_ref = self.fields.reference(candidate.target.graph)?;
				let target_graph = self.read.graph(self.snapshot, &target_ref)?;
				let target = target_graph.atom_ref(candidate.target.atom)?.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scoped MATCH shortest endpoint disappeared",
					)
				})?;
				Ok((
					CanonicalScopedHop {
						edge_graph: candidate.edge.graph,
						edge,
						target_graph: candidate.target.graph,
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

	fn expand_state(
		&mut self,
		state: &ScopedSearchState,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		let source = self.source.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("scoped MATCH shortest lost its source endpoint")
		})?;
		if state.hops.len() >= self.expand.max as usize
			|| (self.expand.path_mode == crate::syn::MatchPathMode::Simple
				&& !state.hops.is_empty()
				&& state.endpoint == source)
		{
			return Ok(());
		}
		for (canonical, candidate) in self.ordered_candidates(state.endpoint)? {
			control.record_path_expansion()?;
			if !scoped_path_candidate_allowed(
				self.row.as_ref().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("scoped MATCH shortest lost its row")
				})?,
				source,
				state.endpoint,
				&state.hops,
				&self.expand,
				&candidate,
				self.read,
				self.snapshot,
				self.fields,
				self.predicate,
				control,
			)? {
				continue;
			}
			let cost = add_path_cost(
				state.cost,
				scoped_hop_cost(
					self.read,
					self.snapshot,
					self.fields,
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
				.fold(size_of::<ScopedSearchState>(), |bytes, hop| {
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
			let priority = if self.expand.objective
				== Some(crate::syn::MatchPathObjective::WeightedShortest)
			{
				completion_bound(
					cost,
					Number::Int(0),
					self.expand.max.saturating_sub(hops.len() as u32),
				)?
			} else {
				cost
			};
			let serial = self.next_serial;
			self.next_serial = self.next_serial.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("scoped MATCH shortest frontier serial")
			})?;
			self.push_state(
				ScopedSearchState {
					endpoint: candidate.target,
					hops,
					canonical: sequence,
					cost,
					priority,
					serial,
					accounted_bytes,
				},
				control,
			)?;
		}
		Ok(())
	}

	fn output_row(
		&mut self,
		state: &ScopedSearchState,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Option<Row>> {
		if state.hops.len() < self.expand.min as usize
			|| state.hops.len() > self.expand.max as usize
			|| !scoped_atom_matches(
				self.read,
				self.snapshot,
				self.fields,
				state.endpoint,
				&self.expand.target,
			)? {
			return Ok(None);
		}
		if self.seen_targets.contains(&state.endpoint) {
			return Ok(None);
		}
		let row = scoped_path_row(
			self.row.as_ref().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("scoped MATCH shortest lost its row")
			})?,
			&self.expand,
			self.source.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("scoped MATCH shortest lost its source")
			})?,
			state.endpoint,
			&state.hops,
			self.read,
			self.snapshot,
			self.fields,
			self.predicate,
			control,
			self.trace.as_ref(),
			self.origin,
			self.snapshot_id,
		)?;
		if row.is_some()
			&& self.expand.objective != Some(crate::syn::MatchPathObjective::WeightedShortest)
		{
			self.seen_targets.insert(state.endpoint);
		}
		Ok(row)
	}
}

impl<Read: QueryRead> RowCursor for ScopedShortestPathCursor<'_, Read> {
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
					FerrosGrynnError::invalid_argument("scoped MATCH shortest lost its best result")
				})?;
				if let Some(account) = &mut self.account {
					account.release(best.state.accounted_bytes.saturating_add(best.row_bytes));
				}
				if !self.seen_targets.insert(best.state.endpoint) {
					continue;
				}
				if self.bound_target {
					self.clear_row();
				}
				return Ok(Some(best.row));
			}
			let Some(mut state) = self.frontier.pop() else {
				self.clear_row();
				continue;
			};
			let weighted =
				self.expand.objective == Some(crate::syn::MatchPathObjective::WeightedShortest);
			if weighted
				&& self.bound_target
				&& monotone_path_prefix(state.cost)
				&& self.completed.peek().is_some_and(|best| {
					(state.cost, &state.canonical) >= (best.state.cost, &best.state.canonical)
				}) {
				if let Some(account) = &mut self.account {
					account.release(state.accounted_bytes);
				}
				continue;
			}
			let output = self.output_row(&state, control)?;
			if weighted {
				self.expand_state(&state, control)?;
				if let Some(row) = output {
					let bytes = row
						.estimated_heap_bytes()
						.saturating_add(size_of::<CompletedPath<ScopedSearchState>>());
					let account = self.account.get_or_insert_with(|| control.barrier_account());
					if !account.reserve(bytes) {
						return Err(FerrosGrynnError::control_limit_exceeded(
							"scoped MATCH best path memory",
							control.barrier_memory_limit() as u64,
						));
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
				self.clear_row();
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

fn scoped_atom_matches<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	atom: BoundAtom,
	binding: &ResolvedMatchBinding,
) -> FerrosGrynnResult<bool> {
	let reference = fields.reference(atom.graph)?;
	let graph = read.graph(snapshot, &reference)?;
	atom_matches(&graph, atom.atom, binding)
}

#[allow(
	clippy::too_many_arguments,
	reason = "one scoped branch validates path, clause, filter, and inline predicates"
)]
pub(in crate::gql::execution) fn scoped_path_candidate_allowed<Read: QueryRead>(
	row: &Row,
	source: BoundAtom,
	current: BoundAtom,
	hops: &[ScopedTraversalCandidate],
	expand: &ResolvedPathExpand,
	candidate: &ScopedTraversalCandidate,
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<bool> {
	let endpoint_seen =
		source == candidate.target || hops.iter().any(|hop| hop.target == candidate.target);
	let edge_seen = hops.iter().any(|hop| hop.edge == candidate.edge);
	let path_allows = match expand.path_mode {
		crate::syn::MatchPathMode::Walk => true,
		crate::syn::MatchPathMode::Trail => !edge_seen,
		crate::syn::MatchPathMode::Acyclic => !endpoint_seen,
		crate::syn::MatchPathMode::Simple => !endpoint_seen || source == candidate.target,
	};
	if !path_allows {
		return Ok(false);
	}
	let clause_allows = match expand.match_mode {
		crate::syn::MatchMode::RepeatableElements => true,
		crate::syn::MatchMode::DifferentEdges => {
			!row.used_edges.contains(&candidate.edge) && !edge_seen
		}
		crate::syn::MatchMode::DifferentAtoms => {
			candidate.edge != candidate.target
				&& !row.used_atoms.contains(&candidate.edge)
				&& !row.used_atoms.contains(&candidate.target)
				&& !edge_seen
				&& !endpoint_seen
				&& !hops.iter().any(|hop| hop.target == candidate.edge)
				&& !hops.iter().any(|hop| hop.edge == candidate.target)
		}
	};
	if !clause_allows {
		return Ok(false);
	}
	let edge_ref = fields.reference(candidate.edge.graph)?;
	let edge_graph = read.graph(snapshot, &edge_ref)?;
	if !connector_matches(&edge_graph, candidate.edge.atom, &expand.edge)? {
		return Ok(false);
	}
	let target_ref = fields.reference(candidate.target.graph)?;
	let target_graph = read.graph(snapshot, &target_ref)?;
	if !scoped_filters_match(
		Some(current),
		&expand.filters,
		candidate,
		&edge_graph,
		&target_graph,
		read,
		snapshot,
		fields,
	)? {
		return Ok(false);
	}
	let mut probe = row.clone();
	bind_match_position(
		&mut probe,
		&expand.edge,
		candidate.edge.atom,
		&edge_graph,
		&edge_ref,
		crate::syn::MatchMode::RepeatableElements,
		predicate,
		control,
	)
}

#[allow(
	clippy::too_many_arguments,
	reason = "path materialization needs exact graph scope for each retained hop"
)]
pub(in crate::gql::execution) fn scoped_path_row<Read: QueryRead>(
	input: &Row,
	expand: &ResolvedPathExpand,
	first: BoundAtom,
	target: BoundAtom,
	hops: &[ScopedTraversalCandidate],
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
	trace: Option<&Rc<RefCell<MatchPathTrace>>>,
	origin: crate::values::Uuid,
	snapshot_id: crate::graph::SnapshotId,
) -> FerrosGrynnResult<Option<Row>> {
	let Some(mut row) = scoped_path_binding(
		input, expand, target, hops, read, snapshot, fields, predicate, control,
	)?
	else {
		return Ok(None);
	};
	let mut cost = Number::Int(0);
	for hop in hops {
		let edge_ref = fields.reference(hop.edge.graph)?;
		let edge_graph = read.graph(snapshot, &edge_ref)?;
		let endpoint_ref = fields.reference(hop.target.graph)?;
		let endpoint_graph = read.graph(snapshot, &endpoint_ref)?;
		let contribution =
			scoped_hop_cost(read, snapshot, fields, expand, hop, cost, predicate, control)?;
		cost = add_path_cost(cost, contribution)?;
		append_scoped_trace_hop(
			&mut row,
			hop,
			&edge_graph,
			&edge_ref,
			&endpoint_graph,
			&endpoint_ref,
			trace,
			contribution,
			control,
		)?;
	}
	row.current = Some(target);
	if trace.is_some() {
		return Ok(Some(row));
	}
	let Some(column) = expand.path_column else {
		return Ok(Some(row));
	};
	let first_ref = fields.reference(first.graph)?;
	let first_graph = read.graph(snapshot, &first_ref)?;
	let first_atom = first_graph.atom_ref(first.atom)?.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("scoped MATCH path seed disappeared")
	})?;
	let mut steps = Vec::with_capacity(hops.len() + 1);
	steps.push(PathStep::new(
		AtomOccurrenceRef::new(first_atom, first_ref),
		None,
		PathDirection::Forward,
		EndpointRole::Source,
	));
	for hop in hops {
		let edge_ref = fields.reference(hop.edge.graph)?;
		let edge_graph = read.graph(snapshot, &edge_ref)?;
		let edge = edge_graph.atom_ref(hop.edge.atom)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("scoped MATCH connector disappeared")
		})?;
		let target_ref = fields.reference(hop.target.graph)?;
		let target_graph = read.graph(snapshot, &target_ref)?;
		let atom = target_graph.atom_ref(hop.target.atom)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("scoped MATCH endpoint disappeared")
		})?;
		steps.push(PathStep::new(
			AtomOccurrenceRef::new(atom, target_ref),
			Some(AtomOccurrenceRef::new(edge, edge_ref)),
			hop.direction,
			hop.role,
		));
	}
	let path = Value::Path(Box::new(Path::new(origin, snapshot_id, steps, Some(cost))));
	match row.values.get(column) {
		Some(Value::None) => row.values[column] = path,
		Some(existing) if existing == &path => {}
		_ => return Ok(None),
	}
	Ok(Some(row))
}

/// Binds an eligible terminal without materializing traces or evaluating COST.
#[allow(
	clippy::too_many_arguments,
	reason = "Qualified terminal predicates share the complete MATCH context"
)]
pub(in crate::gql::execution) fn scoped_path_binding<Read: QueryRead>(
	input: &Row,
	expand: &ResolvedPathExpand,
	target: BoundAtom,
	hops: &[ScopedTraversalCandidate],
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<Row>> {
	let mut row = input.clone();
	for (position, hop) in hops.iter().enumerate() {
		match expand.match_mode {
			crate::syn::MatchMode::RepeatableElements => {}
			crate::syn::MatchMode::DifferentEdges => {
				if !row.used_edges.contains(&hop.edge) {
					row.used_edges.push(hop.edge);
				}
			}
			crate::syn::MatchMode::DifferentAtoms => {
				if !row.used_atoms.contains(&hop.edge) {
					row.used_atoms.push(hop.edge);
				}
				if position + 1 != hops.len() && !row.used_atoms.contains(&hop.target) {
					row.used_atoms.push(hop.target);
				}
			}
		}
	}
	let target_ref = fields.reference(target.graph)?;
	let target_graph = read.graph(snapshot, &target_ref)?;
	if !bind_match_position(
		&mut row,
		&expand.target,
		target.atom,
		&target_graph,
		&target_ref,
		expand.match_mode,
		predicate,
		control,
	)? {
		return Ok(None);
	}
	row.current = Some(target);
	Ok(Some(row))
}

/// Resolves the connector's qualified occurrence before invoking shared cost
/// semantics; graph-local weights and UDF fields use the same snapshot.
#[allow(
	clippy::too_many_arguments,
	reason = "scoped costs preserve the read root, occurrence and evaluator context explicitly"
)]
pub(in crate::gql::execution) fn scoped_hop_cost<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	expand: &ResolvedPathExpand,
	candidate: &ScopedTraversalCandidate,
	prefix: Number,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Number> {
	if expand.custom_cost.is_none()
		&& !expand.objective.is_some_and(crate::syn::MatchPathObjective::is_weighted)
	{
		return Ok(Number::Int(1));
	}
	let reference = fields.reference(candidate.edge.graph)?;
	let graph = read.graph(snapshot, &reference)?;
	if let Some(cost) = expand.custom_cost {
		let edge = graph.atom_ref(candidate.edge.atom)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("scoped MATCH connector disappeared")
		})?;
		return custom_path_contribution(
			cost,
			AtomOccurrenceRef::new(edge, reference),
			prefix,
			predicate,
			control,
		);
	}
	path_edge_weight(&graph, expand, candidate.edge.atom)
}
