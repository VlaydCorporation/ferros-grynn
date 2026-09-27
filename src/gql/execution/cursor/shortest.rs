//! Canonical bounded shortest-path cursors for MATCH objectives.

use std::{
	cmp::Ordering,
	collections::{BTreeSet, BinaryHeap, VecDeque},
	mem::size_of,
};

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalHop {
	edge: crate::graph::AtomRef,
	target: crate::graph::AtomRef,
	direction: PathDirection,
	role: EndpointRole,
}

#[derive(Debug, Clone)]
struct SearchState {
	endpoint: AtomId,
	hops: Vec<TraversalCandidate>,
	canonical: Vec<CanonicalHop>,
	cost: Number,
	serial: u64,
	accounted_bytes: usize,
}

impl SearchState {
	fn estimated_heap_bytes(&self) -> usize {
		let identities = self.canonical.iter().fold(0_usize, |bytes, hop| {
			bytes
				.saturating_add(Value::from(hop.edge.clone()).estimated_heap_bytes())
				.saturating_add(Value::from(hop.target.clone()).estimated_heap_bytes())
		});
		size_of::<Self>()
			.saturating_add(self.hops.capacity().saturating_mul(size_of::<TraversalCandidate>()))
			.saturating_add(self.canonical.capacity().saturating_mul(size_of::<CanonicalHop>()))
			.saturating_add(identities)
	}
}

impl PartialEq for SearchState {
	fn eq(&self, other: &Self) -> bool {
		(self.cost, &self.canonical, self.serial) == (other.cost, &other.canonical, other.serial)
	}
}

impl Eq for SearchState {}

impl PartialOrd for SearchState {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for SearchState {
	fn cmp(&self, other: &Self) -> Ordering {
		other
			.cost
			.cmp(&self.cost)
			.then_with(|| other.canonical.cmp(&self.canonical))
			.then_with(|| other.serial.cmp(&self.serial))
	}
}

enum SearchFrontier {
	Breadth(VecDeque<SearchState>),
	Weighted(BinaryHeap<SearchState>),
}

impl SearchFrontier {
	fn new(objective: crate::syn::MatchPathObjective) -> Self {
		match objective {
			crate::syn::MatchPathObjective::Shortest => Self::Breadth(VecDeque::new()),
			crate::syn::MatchPathObjective::WeightedShortest => Self::Weighted(BinaryHeap::new()),
		}
	}

	fn push(&mut self, state: SearchState) {
		match self {
			Self::Breadth(frontier) => frontier.push_back(state),
			Self::Weighted(frontier) => frontier.push(state),
		}
	}

	fn pop(&mut self) -> Option<SearchState> {
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
}

/// Emits one canonical shortest path for each matching source-target pair.
pub(in crate::gql::execution) struct ShortestPathCursor<'a, Graph> {
	input: Box<dyn RowCursor + 'a>,
	graph: Rc<Graph>,
	graph_ref: Arc<GraphRef>,
	index: Rc<TraversalIndex>,
	expand: ResolvedPathExpand,
	predicate: MatchPredicateContext<'a>,
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	row: Option<Row>,
	source: Option<AtomId>,
	bound_target: bool,
	frontier: SearchFrontier,
	seen_targets: BTreeSet<crate::graph::AtomRef>,
	account: Option<BarrierAccount>,
	next_serial: u64,
}

impl<'a, Graph> ShortestPathCursor<'a, Graph> {
	#[allow(
		clippy::too_many_arguments,
		reason = "the concrete shortest operator keeps all physical-plan inputs explicit"
	)]
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		graph_ref: Arc<GraphRef>,
		index: Rc<TraversalIndex>,
		expand: ResolvedPathExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
	) -> Self {
		let objective = expand.objective.unwrap_or(crate::syn::MatchPathObjective::Shortest);
		Self {
			input,
			graph,
			graph_ref,
			index,
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

	fn push_state(
		&mut self,
		mut state: SearchState,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		state.accounted_bytes = state.estimated_heap_bytes();
		let account = self.account.get_or_insert_with(|| control.barrier_account());
		if !account.reserve(state.accounted_bytes) {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"MATCH frontier memory",
				u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
			));
		}
		self.frontier.push(state);
		control.record_match_frontier(self.frontier.len());
		Ok(())
	}

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
		self.frontier.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
		self.seen_targets.clear();
		self.next_serial = 1;
		self.source = Some(source);
		self.row = Some(row);
		self.push_state(
			SearchState {
				endpoint: source,
				hops: Vec::new(),
				canonical: Vec::new(),
				cost: Number::Int(0),
				serial: 0,
				accounted_bytes: 0,
			},
			control,
		)?;
		Ok(true)
	}

	fn finish_row(&mut self) {
		self.frontier.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
		self.seen_targets.clear();
		self.source = None;
		self.row = None;
	}
}

impl<Graph: GraphRead> ShortestPathCursor<'_, Graph> {
	fn ordered_candidates(
		&self,
		endpoint: AtomId,
	) -> FerrosGrynnResult<Vec<(CanonicalHop, TraversalCandidate)>> {
		let mut candidates = index_candidates(
			self.graph.as_ref(),
			self.index.as_ref(),
			endpoint,
			self.expand.direction,
		)?
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
		for (canonical, candidate) in self.ordered_candidates(state.endpoint)? {
			control.record_path_expansion()?;
			let allowed = candidate.target.kind() == Some(self.expand.target.domain.atom_kind())
				&& path_candidate_allowed(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"MATCH shortest path lost its input row.",
						)
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
				path_hop_weight(self.graph.as_ref(), &self.expand, &candidate)?,
			)?;
			let mut hops = state.hops.clone();
			hops.push(candidate.clone());
			let mut sequence = state.canonical.clone();
			sequence.push(canonical);
			let serial = self.next_serial;
			self.next_serial = self.next_serial.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("MATCH shortest-path frontier serial")
			})?;
			self.push_state(
				SearchState {
					endpoint: candidate.target,
					hops,
					canonical: sequence,
					cost,
					serial,
					accounted_bytes: 0,
				},
				control,
			)?;
		}
		Ok(())
	}

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
			self.graph.as_ref(),
			&self.graph_ref,
			self.predicate,
			control,
			self.trace.as_ref(),
		)?;
		if row.is_some() {
			self.seen_targets.insert(target);
		}
		Ok(row)
	}
}

impl<Graph: GraphRead> RowCursor for ShortestPathCursor<'_, Graph> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if self.row.is_none() && !self.start_row(control)? {
				return Ok(None);
			}
			let Some(state) = self.frontier.pop() else {
				self.finish_row();
				continue;
			};
			let output = self.output_row(&state, control)?;
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
