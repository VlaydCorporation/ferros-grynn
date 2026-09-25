//! Variable-length graph path expansion.

use super::*;

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
}

/// Bounded TRAIL cursor that retains only its active DFS path.
pub(in crate::gql::execution) struct PathExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Variable-length expansion contract.
	expand: ResolvedPathExpand,
	/// Current upstream row.
	row: Option<Row>,
	/// Active DFS stack.
	frames: Vec<DfsFrame>,
	/// Active connector path aligned below the frame stack.
	hops: Vec<TraversalCandidate>,
}

impl<'a, Graph> PathExpandCursor<'a, Graph> {
	/// Creates an incremental path-expansion cursor.
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		index: Rc<TraversalIndex>,
		expand: ResolvedPathExpand,
	) -> Self {
		Self {
			input,
			graph,
			index,
			expand,
			row: None,
			frames: Vec::new(),
			hops: Vec::new(),
		}
	}
}

impl<Graph: GraphRead> PathExpandCursor<'_, Graph> {
	/// Initializes traversal state for the next upstream row.
	fn start_row(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next(control)? else {
			return Ok(false);
		};
		let source = row.current.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("MATCH path expansion has no source endpoint.")
		})?;
		let candidates = index_candidates(
			self.graph.as_ref(),
			self.index.as_ref(),
			source,
			self.expand.direction,
		)?;
		self.frames.clear();
		self.frames.push(DfsFrame {
			endpoint: source,
			candidates,
			next_candidate: 0,
			yielded: false,
		});
		self.hops.clear();
		self.row = Some(row);
		Ok(true)
	}

	/// Removes the deepest DFS frame and its incoming hop.
	fn backtrack(&mut self) {
		self.frames.pop();
		if self.frames.is_empty() {
			self.row = None;
		} else {
			self.hops.pop();
		}
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
						&self.frames,
						&self.hops,
						self.graph.as_ref(),
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
			if candidate.target.kind()
				!= Some(match self.expand.target.domain {
					EndpointDomain::Node => AtomKind::Node,
					EndpointDomain::Edge => AtomKind::Edge,
				})
				|| self.row.as_ref().is_some_and(|row| row.used_edges.contains(&candidate.edge))
				|| self.hops.iter().any(|hop| hop.edge == candidate.edge)
				|| !connector_matches(self.graph.as_ref(), candidate.edge, &self.expand.edge)?
			{
				continue;
			}
			let candidates = index_candidates(
				self.graph.as_ref(),
				self.index.as_ref(),
				candidate.target,
				self.expand.direction,
			)?;
			self.hops.push(candidate.clone());
			self.frames.push(DfsFrame {
				endpoint: candidate.target,
				candidates,
				next_candidate: 0,
				yielded: false,
			});
		}
	}
}

/// Materializes one public path value from the active DFS stack.
pub(in crate::gql::execution) fn path_row<Graph: GraphRead>(
	input: &Row,
	expand: &ResolvedPathExpand,
	frames: &[DfsFrame],
	hops: &[TraversalCandidate],
	graph: &Graph,
) -> FerrosGrynnResult<Option<Row>> {
	let Some(first) = frames.first().map(|frame| frame.endpoint) else {
		return Ok(None);
	};
	let Some(target) = frames.last().map(|frame| frame.endpoint) else {
		return Ok(None);
	};
	let mut row = input.clone();
	if !bind_atom(&mut row, &expand.target, target, graph)? {
		return Ok(None);
	}
	let first_ref = graph
		.atom_ref(first)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", first.to_string()))?;
	let mut steps = Vec::with_capacity(hops.len() + 1);
	steps.push(PathStep::new(first_ref, None, PathDirection::Forward, EndpointRole::Source));
	for hop in hops {
		let atom = graph
			.atom_ref(hop.target)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", hop.target.to_string()))?;
		let edge = graph
			.atom_ref(hop.edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", hop.edge.to_string()))?;
		steps.push(PathStep::new(atom, Some(edge), hop.direction, hop.role));
	}
	let path = Value::Path(Box::new(Path::new(graph.identity(), graph.snapshot_id(), steps, None)));
	if let Some(column) = expand.path_column {
		match row.values.get(column) {
			Some(Value::None) => row.values[column] = path,
			Some(existing) if existing == &path => {}
			_ => return Ok(None),
		}
	}
	for hop in hops {
		if !row.used_edges.contains(&hop.edge) {
			row.used_edges.push(hop.edge);
		}
	}
	row.current = Some(target);
	Ok(Some(row))
}
