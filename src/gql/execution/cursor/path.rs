//! Variable-length graph path expansion.

use std::mem::size_of;

use super::*;

/// Prefix-sharing path trace charged to the statement-wide barrier budget.
pub(in crate::gql::execution) struct MatchPathTrace {
	arena: PathTraceArena,
	account: BarrierAccount,
	memory_limit: usize,
}

impl MatchPathTrace {
	/// Creates an empty trace using the shared statement memory account.
	pub(in crate::gql::execution) fn new(control: &ExecutionControl) -> Self {
		Self {
			arena: PathTraceArena::new(),
			account: control.barrier_account(),
			memory_limit: control.barrier_memory_limit(),
		}
	}

	/// Appends one trace node after reserving its retained memory.
	pub(in crate::gql::execution) fn push(
		&mut self,
		previous: Option<PathTraceId>,
		step: PathStep,
		cumulative_cost: Option<Number>,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<PathTraceId> {
		let bytes = size_of::<PathStep>()
			.saturating_add(size_of::<Option<PathTraceId>>())
			.saturating_add(size_of::<Option<Number>>())
			.saturating_add(step.atom().atom().id().estimated_heap_bytes())
			.saturating_add(
				step.via_edge().map(|edge| edge.atom().id().estimated_heap_bytes()).unwrap_or(0),
			);
		if !self.account.reserve(bytes) {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"MATCH path trace memory",
				u64::try_from(self.memory_limit).unwrap_or(u64::MAX),
			));
		}
		match self.arena.push(previous, step, cumulative_cost) {
			Ok(id) => {
				control.record_match_trace_node();
				Ok(id)
			}
			Err(error) => {
				self.account.release(bytes);
				Err(error)
			}
		}
	}

	/// Materializes one terminal trace into its public path value.
	pub(in crate::gql::execution) fn materialize(
		&self,
		end: PathTraceId,
		origin: crate::values::Uuid,
		snapshot: crate::graph::SnapshotId,
	) -> FerrosGrynnResult<Path> {
		self.arena.materialize(end, origin, snapshot)
	}
}

/// Starts one named path after its seed endpoint has been bound.
pub(in crate::gql::execution) struct PathTraceStartCursor<'a, Graph> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) graph: Rc<Graph>,
	pub(in crate::gql::execution) graph_ref: Arc<GraphRef>,
	pub(in crate::gql::execution) arena: Rc<RefCell<MatchPathTrace>>,
}

impl<Graph: GraphRead> RowCursor for PathTraceStartCursor<'_, Graph> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		let Some(mut row) = self.input.next(control)? else {
			return Ok(None);
		};
		let atom = row
			.current
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("named MATCH path has no seed endpoint")
			})?
			.atom;
		let reference = self
			.graph
			.atom_ref(atom)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", atom.to_string()))?;
		let step = PathStep::new(
			AtomOccurrenceRef::new(reference, self.graph_ref.clone()),
			None,
			PathDirection::Forward,
			EndpointRole::Source,
		);
		row.trace =
			Some(self.arena.borrow_mut().push(None, step, Some(Number::Int(0)), control)?);
		row.trace_cost = Number::Int(0);
		Ok(Some(row))
	}
}

/// Materializes a complete named linear pattern after all fixed and variable
/// segments have appended their trace steps.
pub(in crate::gql::execution) struct PathBindCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) arena: Rc<RefCell<MatchPathTrace>>,
	pub(in crate::gql::execution) column: usize,
	pub(in crate::gql::execution) origin: crate::values::Uuid,
	pub(in crate::gql::execution) snapshot: crate::graph::SnapshotId,
}

impl RowCursor for PathBindCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		let Some(mut row) = self.input.next(control)? else {
			return Ok(None);
		};
		let trace = row.trace.take().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("named MATCH path has no terminal trace")
		})?;
		let path = Value::Path(Box::new(self.arena.borrow().materialize(
			trace,
			self.origin,
			self.snapshot,
		)?));
		match row.values.get(self.column) {
			Some(Value::None) => row.values[self.column] = path,
			Some(existing) if existing == &path => {}
			_ => return Ok(None),
		}
		row.trace_cost = Number::Int(0);
		Ok(Some(row))
	}
}

/// Appends one accepted connector/endpoint pair to an active named path.
pub(in crate::gql::execution) fn append_trace_hop<Graph: GraphRead>(
	row: &mut Row,
	candidate: &TraversalCandidate,
	graph: &Graph,
	graph_ref: &Arc<GraphRef>,
	arena: Option<&Rc<RefCell<MatchPathTrace>>>,
	weight: Number,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let (Some(previous), Some(arena)) = (row.trace, arena) else {
		return Ok(());
	};
	let atom = graph.atom_ref(candidate.target)?.ok_or_else(|| {
		FerrosGrynnError::atom_does_not_exist("atom", candidate.target.to_string())
	})?;
	let edge = graph
		.atom_ref(candidate.edge)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", candidate.edge.to_string()))?;
	let Value::Number(cost) =
		binary(BinaryOperator::Add, Value::Number(row.trace_cost), Value::Number(weight))?
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"MATCH path cost addition did not return a number",
		));
	};
	let step = PathStep::new(
		AtomOccurrenceRef::new(atom, graph_ref.clone()),
		Some(AtomOccurrenceRef::new(edge, graph_ref.clone())),
		candidate.direction,
		candidate.role,
	);
	row.trace = Some(arena.borrow_mut().push(previous.into(), step, Some(cost), control)?);
	row.trace_cost = cost;
	Ok(())
}

/// Incremental depth-first traversal state for one endpoint.
pub(in crate::gql::execution) struct DfsFrame {
	/// Endpoint represented by this stack frame.
	endpoint: AtomId,
	/// Outgoing candidates computed for the endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next outgoing candidate position.
	next_candidate: usize,
	/// Whether the path ending at this endpoint was already emitted.
	yielded: bool,
	/// Bytes retained by this frame and its incoming hop.
	accounted_bytes: usize,
}

/// Bounded TRAIL cursor that retains only its active DFS path.
pub(in crate::gql::execution) struct PathExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared public graph identity for materialized path occurrences.
	graph_ref: Arc<GraphRef>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Variable-length expansion contract.
	expand: ResolvedPathExpand,
	/// Evaluator for inline property and WHERE constraints.
	predicate: MatchPredicateContext<'a>,
	/// Prefix-sharing arena used only by an observable named path.
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	/// Current upstream row.
	row: Option<Row>,
	/// Active DFS stack.
	frames: Vec<DfsFrame>,
	/// Active connector path aligned below the frame stack.
	hops: Vec<TraversalCandidate>,
	/// Statement-wide memory charged by active DFS state.
	account: Option<BarrierAccount>,
}

impl<'a, Graph> PathExpandCursor<'a, Graph> {
	/// Creates an incremental path-expansion cursor.
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		graph_ref: Arc<GraphRef>,
		index: Rc<TraversalIndex>,
		expand: ResolvedPathExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
	) -> Self {
		Self {
			input,
			graph,
			graph_ref,
			index,
			expand,
			predicate,
			trace,
			row: None,
			frames: Vec::new(),
			hops: Vec::new(),
			account: None,
		}
	}
}

impl<Graph: GraphRead> PathExpandCursor<'_, Graph> {
	/// Initializes traversal state for the next upstream row.
	fn start_row(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next(control)? else {
			return Ok(false);
		};
		let source = row
			.current
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH path expansion has no source endpoint.")
			})?
			.atom;
		let candidates = index_candidates(
			self.graph.as_ref(),
			self.index.as_ref(),
			source,
			self.expand.direction,
		)?;
		self.frames.clear();
		self.hops.clear();
		if let Some(account) = &mut self.account {
			account.release_all();
		}
		self.push_frame(source, candidates, false, control)?;
		self.row = Some(row);
		Ok(true)
	}

	/// Charges and appends one active DFS frame.
	fn push_frame(
		&mut self,
		endpoint: AtomId,
		candidates: Vec<TraversalCandidate>,
		incoming_hop: bool,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		let bytes = size_of::<DfsFrame>()
			.saturating_add(candidates.capacity().saturating_mul(size_of::<TraversalCandidate>()))
			.saturating_add(
				usize::from(incoming_hop).saturating_mul(size_of::<TraversalCandidate>()),
			);
		let account = self.account.get_or_insert_with(|| control.barrier_account());
		if !account.reserve(bytes) {
			return Err(FerrosGrynnError::control_limit_exceeded(
				"MATCH DFS frontier memory",
				u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
			));
		}
		self.frames.push(DfsFrame {
			endpoint,
			candidates,
			next_candidate: 0,
			yielded: false,
			accounted_bytes: bytes,
		});
		control.record_match_frontier(self.frames.len());
		Ok(())
	}

	/// Removes the deepest DFS frame and its incoming hop.
	fn backtrack(&mut self) {
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

	/// Applies clause/path uniqueness and connector-local constraints before a
	/// branch enters the active DFS path.
	fn accepts_candidate(
		&self,
		candidate: &TraversalCandidate,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<bool>
	where
		Graph: GraphRead,
	{
		let row = self
			.row
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH path lost its input row."))?;
		path_candidate_allowed(
			row,
			self.frames.first().map(|frame| frame.endpoint).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH path has no source endpoint.")
			})?,
			&self.hops,
			&self.expand,
			candidate,
			self.graph.as_ref(),
			&self.graph_ref,
			self.predicate,
			control,
		)
	}
}

impl<Graph: GraphRead> RowCursor for PathExpandCursor<'_, Graph> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if self.row.is_none() && !self.start_row(control)? {
				return Ok(None);
			}
			if self.frames.is_empty() {
				self.row = None;
				continue;
			}

			let depth = self.hops.len();
			let emit = {
				let frame = self.frames.last_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH DFS stack is empty.")
				})?;
				let emit = !frame.yielded
					&& depth >= self.expand.min as usize
					&& depth <= self.expand.max as usize;
				frame.yielded = true;
				emit
			};
			if emit {
				let endpoint = self.frames.last().map(|frame| frame.endpoint).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH DFS stack is empty.")
				})?;
				if atom_matches(self.graph.as_ref(), endpoint, &self.expand.target)?
					&& let Some(row) = path_row(
						self.row.as_ref().ok_or_else(|| {
							FerrosGrynnError::invalid_argument("MATCH path lost its input row.")
						})?,
						&self.expand,
						self.frames.first().map(|frame| frame.endpoint).ok_or_else(|| {
							FerrosGrynnError::invalid_argument("MATCH path has no source endpoint.")
						})?,
						endpoint,
						&self.hops,
						self.graph.as_ref(),
						&self.graph_ref,
						self.predicate,
						control,
						self.trace.as_ref(),
					)? {
					return Ok(Some(row));
				}
			}

			if depth >= self.expand.max as usize {
				self.backtrack();
				continue;
			}
			let candidate = {
				let frame = self.frames.last_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH DFS stack is empty.")
				})?;
				let candidate = frame.candidates.get(frame.next_candidate).cloned();
				frame.next_candidate += usize::from(candidate.is_some());
				candidate
			};
			let Some(candidate) = candidate else {
				self.backtrack();
				continue;
			};
			control.record_path_expansion()?;
			if candidate.target.kind()
				!= Some(match self.expand.target.domain {
					EndpointDomain::Node => AtomKind::Node,
					EndpointDomain::Edge => AtomKind::Edge,
				})
				|| !self.accepts_candidate(&candidate, control)?
			{
				continue;
			}
			let closes_simple = self.expand.path_mode == crate::syn::MatchPathMode::Simple
				&& self.frames.first().is_some_and(|frame| frame.endpoint == candidate.target);
			let candidates = if closes_simple {
				Vec::new()
			} else {
				index_candidates(
					self.graph.as_ref(),
					self.index.as_ref(),
					candidate.target,
					self.expand.direction,
				)?
			};
			self.push_frame(candidate.target, candidates, true, control)?;
			self.hops.push(candidate);
		}
	}
}

/// Materializes one public path value from the active DFS stack.
pub(in crate::gql::execution) fn path_row<Graph: GraphRead>(
	input: &Row,
	expand: &ResolvedPathExpand,
	first: AtomId,
	target: AtomId,
	hops: &[TraversalCandidate],
	graph: &Graph,
	graph_ref: &Arc<GraphRef>,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
	trace: Option<&Rc<RefCell<MatchPathTrace>>>,
) -> FerrosGrynnResult<Option<Row>> {
	let mut row = input.clone();
	let graph_id = graph_ref.id();
	for (index, hop) in hops.iter().enumerate() {
		let edge = BoundAtom::new(graph_id, hop.edge);
		let target = BoundAtom::new(graph_id, hop.target);
		match expand.match_mode {
			crate::syn::MatchMode::RepeatableElements => {}
			crate::syn::MatchMode::DifferentEdges => {
				if !row.used_edges.contains(&edge) {
					row.used_edges.push(edge);
				}
			}
			crate::syn::MatchMode::DifferentAtoms => {
				if !row.used_atoms.contains(&edge) {
					row.used_atoms.push(edge);
				}
				if index + 1 != hops.len() && !row.used_atoms.contains(&target) {
					row.used_atoms.push(target);
				}
			}
		}
	}
	if !bind_match_position(
		&mut row,
		&expand.target,
		target,
		graph,
		graph_ref,
		expand.match_mode,
		predicate,
		control,
	)? {
		return Ok(None);
	}
	for hop in hops {
		append_trace_hop(
			&mut row,
			hop,
			graph,
			graph_ref,
			trace,
			path_hop_weight(graph, expand, hop)?,
			control,
		)?;
	}
	row.current = Some(BoundAtom::new(graph_ref.id(), target));
	if trace.is_some() {
		return Ok(Some(row));
	}
	let Some(column) = expand.path_column else {
		return Ok(Some(row));
	};
	let first_ref = graph
		.atom_ref(first)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", first.to_string()))?;
	let mut steps = Vec::with_capacity(hops.len() + 1);
	steps.push(PathStep::new(
		AtomOccurrenceRef::new(first_ref, graph_ref.clone()),
		None,
		PathDirection::Forward,
		EndpointRole::Source,
	));
	for hop in hops {
		let atom = graph
			.atom_ref(hop.target)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", hop.target.to_string()))?;
		let edge = graph
			.atom_ref(hop.edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", hop.edge.to_string()))?;
		steps.push(PathStep::new(
			AtomOccurrenceRef::new(atom, graph_ref.clone()),
			Some(AtomOccurrenceRef::new(edge, graph_ref.clone())),
			hop.direction,
			hop.role,
		));
	}
	let path = Value::Path(Box::new(Path::new(
		graph.identity(),
		graph.snapshot_id(),
		steps,
		Some(path_cost(graph, expand, hops)?),
	)));
	match row.values.get(column) {
		Some(Value::None) => row.values[column] = path,
		Some(existing) if existing == &path => {}
		_ => return Ok(None),
	}
	Ok(Some(row))
}

/// Applies path-local and clause-wide uniqueness plus connector predicates to
/// one candidate before it enters a traversal frontier.
#[allow(
	clippy::too_many_arguments,
	reason = "the helper keeps one shared uniqueness contract for DFS, BFS, and Dijkstra"
)]
pub(in crate::gql::execution) fn path_candidate_allowed<Graph: GraphRead>(
	row: &Row,
	source: AtomId,
	hops: &[TraversalCandidate],
	expand: &ResolvedPathExpand,
	candidate: &TraversalCandidate,
	graph: &Graph,
	graph_ref: &Arc<GraphRef>,
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
			!row.used_edges.contains(&BoundAtom::new(graph_ref.id(), candidate.edge)) && !edge_seen
		}
		crate::syn::MatchMode::DifferentAtoms => {
			candidate.edge != candidate.target
				&& !row.used_atoms.contains(&BoundAtom::new(graph_ref.id(), candidate.edge))
				&& !row.used_atoms.contains(&BoundAtom::new(graph_ref.id(), candidate.target))
				&& !edge_seen
				&& !endpoint_seen
				&& !hops.iter().any(|hop| hop.target == candidate.edge)
				&& !hops.iter().any(|hop| hop.edge == candidate.target)
		}
	};
	if !clause_allows || !connector_matches(graph, candidate.edge, &expand.edge)? {
		return Ok(false);
	}
	let edge = graph
		.edge(candidate.edge)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", candidate.edge.to_string()))?;
	if expand.filters.contains(&crate::syn::MatchPathFilter::Weighted) && edge.weight().is_none() {
		return Ok(false);
	}
	let mut probe = row.clone();
	bind_match_position(
		&mut probe,
		&expand.edge,
		candidate.edge,
		graph,
		graph_ref,
		crate::syn::MatchMode::RepeatableElements,
		predicate,
		control,
	)
}

/// Returns the exact contribution of one hop to the active path objective.
pub(in crate::gql::execution) fn path_hop_weight<Graph: GraphRead>(
	graph: &Graph,
	expand: &ResolvedPathExpand,
	candidate: &TraversalCandidate,
) -> FerrosGrynnResult<Number> {
	if expand.objective != Some(crate::syn::MatchPathObjective::WeightedShortest) {
		return Ok(Number::Int(1));
	}
	let weight = graph
		.edge(candidate.edge)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", candidate.edge.to_string()))?
		.weight()
		.unwrap_or(Number::Int(1));
	let invalid = match weight {
		Number::Float(value) => !value.is_finite() || value < 0.0,
		_ => weight < Number::Int(0),
	};
	if invalid {
		return Err(FerrosGrynnError::invalid_argument(
			"WEIGHTED SHORTEST requires finite, non-negative edge weights.",
		));
	}
	Ok(weight)
}

/// Adds path weights through the same exact numeric matrix as GrynnQL.
pub(in crate::gql::execution) fn add_path_cost(
	left: Number,
	right: Number,
) -> FerrosGrynnResult<Number> {
	let Value::Number(result) =
		binary(BinaryOperator::Add, Value::Number(left), Value::Number(right))?
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"MATCH path cost addition did not return a number",
		));
	};
	Ok(result)
}

fn path_cost<Graph: GraphRead>(
	graph: &Graph,
	expand: &ResolvedPathExpand,
	hops: &[TraversalCandidate],
) -> FerrosGrynnResult<Number> {
	hops.iter().try_fold(Number::Int(0), |cost, hop| {
		add_path_cost(cost, path_hop_weight(graph, expand, hop)?)
	})
}
