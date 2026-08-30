#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Snapshot(pub(super) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
	pub(super) values: Vec<Value>,
	/// Internal atom identities aligned with MATCH binding columns.
	pub(super) atoms: Vec<Option<AtomId>>,
	/// Connector edges already consumed by DIFFERENT EDGES semantics.
	used_edges: Vec<AtomId>,
	/// Endpoint from which the next linear expansion starts.
	current: Option<AtomId>,
}

impl Row {
	pub(super) const fn new(values: Vec<Value>) -> Self {
		let atoms = Vec::new();
		Self {
			values,
			atoms,
			used_edges: Vec::new(),
			current: None,
		}
	}

	/// Creates an empty MATCH binding row with `count` columns.
	pub(super) fn bindings(count: usize) -> Self {
		Self {
			values: vec![Value::None; count],
			atoms: vec![None; count],
			used_edges: Vec::new(),
			current: None,
		}
	}
}

pub(super) trait RowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>>;
}

pub(super) trait QueryRead {
	/// Graph view tied to the requested query snapshot.
	type Graph<'a>: GraphRead + 'a
	where
		Self: 'a;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot>;

	fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog;

	fn scan<'a>(
		&'a self,
		snapshot: Snapshot,
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>>;

	fn get(&self, snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>>;

	/// Opens a graph without refreshing the statement snapshot.
	fn graph<'a>(
		&'a self,
		snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'a>>;

	/// Returns the property dictionary captured by the statement snapshot.
	fn property_dictionary(&self, snapshot: Snapshot) -> Arc<PropertyDictionary>;
}

/// Pull cursor that binds the first endpoint of a pattern.
pub(super) struct SeedCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Endpoint constraints and optional alias column.
	binding: MatchBinding,
	/// Candidate atom identifiers for an unbound endpoint.
	ids: Vec<AtomId>,
	/// Current upstream row.
	row: Option<Row>,
	/// Alias-constrained atom, when already bound by another pattern.
	bound: Option<AtomId>,
	/// Next candidate position.
	next_id: usize,
}

impl<'a, Graph: GraphRead> SeedCursor<'a, Graph> {
	/// Collects the endpoint domain once and creates a seed cursor.
	pub(super) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		binding: MatchBinding,
	) -> FerrosGrynnResult<Self> {
		let ids = match binding.domain {
			EndpointDomain::Node => graph.node_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?,
			EndpointDomain::Edge => graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?,
		};
		Ok(Self {
			input,
			graph,
			binding,
			ids,
			row: None,
			bound: None,
			next_id: 0,
		})
	}
}

impl<Graph: GraphRead> RowCursor for SeedCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if self.row.is_none() {
				let Some(row) = self.input.next()? else {
					return Ok(None);
				};
				self.bound =
					self.binding.column.and_then(|column| row.atoms.get(column).copied().flatten());
				self.row = Some(row);
				self.next_id = 0;
			}
			let candidate = if let Some(bound) = self.bound {
				(self.next_id == 0).then_some(bound)
			} else {
				self.ids.get(self.next_id).copied()
			};
			let Some(candidate) = candidate else {
				self.row = None;
				continue;
			};
			self.next_id += 1;
			if !atom_matches(self.graph.as_ref(), candidate, &self.binding)? {
				continue;
			}
			let mut row = self.row.as_ref().cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH seed lost its input row.")
			})?;
			if !bind_atom(&mut row, &self.binding, candidate, self.graph.as_ref())? {
				continue;
			}
			row.current = Some(candidate);
			return Ok(Some(row));
		}
	}
}

/// Applies endpoint-domain and alternative-label constraints.
pub(super) fn atom_matches<Graph: GraphRead>(
	graph: &Graph,
	atom: AtomId,
	binding: &MatchBinding,
) -> FerrosGrynnResult<bool> {
	if atom.kind()
		!= Some(match binding.domain {
			EndpointDomain::Node => AtomKind::Node,
			EndpointDomain::Edge => AtomKind::Edge,
		}) {
		return Ok(false);
	}
	if binding.labels.is_empty() {
		return Ok(true);
	}
	match binding.domain {
		EndpointDomain::Node => {
			let Some(node) = graph.node(atom)? else {
				return Ok(false);
			};
			let label = node.label_text()?;
			Ok(label.is_some_and(|label| {
				binding.labels.iter().any(|candidate| candidate.as_ref() == label.as_str())
			}))
		}
		EndpointDomain::Edge => {
			let Some(edge) = graph.edge(atom)? else {
				return Ok(false);
			};
			let label = edge.label_text()?;
			Ok(label.is_some_and(|label| {
				binding.labels.iter().any(|candidate| candidate.as_ref() == label.as_str())
			}))
		}
	}
}

/// Binds an atom or checks equality with an existing alias binding.
pub(super) fn bind_atom<Graph: GraphRead>(
	row: &mut Row,
	binding: &MatchBinding,
	atom: AtomId,
	graph: &Graph,
) -> FerrosGrynnResult<bool> {
	let Some(column) = binding.column else {
		return Ok(true);
	};
	if let Some(existing) = row.atoms.get(column).copied().flatten() {
		return Ok(existing == atom);
	}
	if !matches!(row.values.get(column), Some(Value::None)) {
		return Ok(false);
	}
	let reference = graph.atom_ref(atom)?.ok_or_else(|| {
		let kind = match atom.kind() {
			Some(AtomKind::Node) => "node",
			Some(AtomKind::Edge) => "edge",
			None => "atom",
		};
		FerrosGrynnError::atom_does_not_exist(kind, atom.to_string())
	})?;
	row.values[column] = Value::Atom(Box::new(reference));
	row.atoms[column] = Some(atom);
	Ok(true)
}

#[derive(Clone)]
/// One traversable connector candidate.
pub(super) struct TraversalCandidate {
	/// Candidate target endpoint.
	target: AtomId,
	/// Connector edge used by the hop.
	edge: AtomId,
	/// Direction recorded in the public path step.
	direction: PathDirection,
	/// Incidence role recorded in the public path step.
	role: EndpointRole,
}

/// Single-hop cursor that scans graph edge identifiers once.
pub(super) struct EdgeExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Fixed expansion contract.
	expand: Expand,
	/// Connector identifiers scanned for each source row.
	edges: Vec<AtomId>,
	/// Current upstream row.
	row: Option<Row>,
	/// Candidates for the current source endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next candidate position.
	next_candidate: usize,
}

impl<'a, Graph: GraphRead> EdgeExpandCursor<'a, Graph> {
	/// Captures the edge-id scan and creates a fixed-hop cursor.
	pub(super) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		expand: Expand,
	) -> FerrosGrynnResult<Self> {
		let edges = graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Self {
			input,
			graph,
			expand,
			edges,
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		})
	}
}

impl<Graph: GraphRead> RowCursor for EdgeExpandCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				if let Some(row) = expand_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
				)? {
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next()? else {
				return Ok(None);
			};
			let source = row.current.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH expand has no source endpoint.")
			})?;
			self.candidates = edge_candidates(
				self.graph.as_ref(),
				&self.edges,
				source,
				self.expand.direction,
				&self.expand.edge.labels,
			)?;
			self.next_candidate = 0;
			self.row = Some(row);
		}
	}
}

/// Fixed-hop cursor backed by a shared traversal index.
pub(super) struct IndexExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Fixed expansion contract.
	expand: Expand,
	/// Current upstream row.
	row: Option<Row>,
	/// Candidates for the current source endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next candidate position.
	next_candidate: usize,
}

impl<'a, Graph> IndexExpandCursor<'a, Graph> {
	/// Creates a fixed-hop cursor over an existing index.
	pub(super) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		index: Rc<TraversalIndex>,
		expand: Expand,
	) -> Self {
		Self {
			input,
			graph,
			index,
			expand,
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		}
	}
}

impl<Graph: GraphRead> RowCursor for IndexExpandCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				if !connector_matches(
					self.graph.as_ref(),
					candidate.edge,
					&self.expand.edge.labels,
				)? {
					continue;
				}
				if let Some(row) = expand_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
				)? {
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next()? else {
				return Ok(None);
			};
			let source = row.current.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH expand has no source endpoint.")
			})?;
			self.candidates = index_candidates(
				self.graph.as_ref(),
				self.index.as_ref(),
				source,
				self.expand.direction,
			)?;
			self.next_candidate = 0;
			self.row = Some(row);
		}
	}
}

/// Applies bindings and DIFFERENT EDGES checks to one hop.
pub(super) fn expand_row<Graph: GraphRead>(
	input: &Row,
	expand: &Expand,
	candidate: &TraversalCandidate,
	graph: &Graph,
) -> FerrosGrynnResult<Option<Row>> {
	if !atom_matches(graph, candidate.target, &expand.target)? {
		return Ok(None);
	}
	let edge_bound =
		expand.edge.column.and_then(|column| input.atoms.get(column).copied().flatten());
	if edge_bound.is_some_and(|edge| edge != candidate.edge)
		|| edge_bound.is_none() && input.used_edges.contains(&candidate.edge)
	{
		return Ok(None);
	}
	let mut row = input.clone();
	if !bind_atom(&mut row, &expand.edge, candidate.edge, graph)?
		|| !bind_atom(&mut row, &expand.target, candidate.target, graph)?
	{
		return Ok(None);
	}
	if !row.used_edges.contains(&candidate.edge) {
		row.used_edges.push(candidate.edge);
	}
	row.current = Some(candidate.target);
	Ok(Some(row))
}

/// Applies connector-label alternatives lazily.
pub(super) fn connector_matches<Graph: GraphRead>(
	graph: &Graph,
	edge: AtomId,
	labels: &[Box<str>],
) -> FerrosGrynnResult<bool> {
	if labels.is_empty() {
		return Ok(true);
	}
	let label = graph
		.edge(edge)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?
		.label_text()?
		.map(|label| label.into_owned());
	Ok(label
		.is_some_and(|label| labels.iter().any(|candidate| candidate.as_ref() == label.as_str())))
}

/// Finds one-hop candidates by scanning connector edges.
pub(super) fn edge_candidates<Graph: GraphRead>(
	graph: &Graph,
	edges: &[AtomId],
	source: AtomId,
	direction: crate::syn::MatchDirection,
	labels: &[Box<str>],
) -> FerrosGrynnResult<Vec<TraversalCandidate>> {
	let mut candidates = Vec::new();
	for edge_id in edges {
		if !connector_matches(graph, *edge_id, labels)? {
			continue;
		}
		let edge = graph
			.edge(*edge_id)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge_id.to_string()))?;
		let topology = edge.topology()?;
		match topology.as_ref() {
			EdgeTopology::Directed {
				source: from,
				target: to,
			} => match direction {
				crate::syn::MatchDirection::Forward => push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					from,
					to,
					PathDirection::Forward,
					EndpointRole::Target,
				),
				crate::syn::MatchDirection::Backward => push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					to,
					from,
					PathDirection::Reverse,
					EndpointRole::Source,
				),
				crate::syn::MatchDirection::Undirected
				| crate::syn::MatchDirection::Bidirectional => {}
			},
			EdgeTopology::Bidirectional {
				source: from,
				target: to,
			} if direction != crate::syn::MatchDirection::Undirected => {
				push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					from,
					to,
					PathDirection::Forward,
					EndpointRole::Target,
				);
				push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					to,
					from,
					PathDirection::Reverse,
					EndpointRole::Source,
				);
			}
			EdgeTopology::Undirected {
				endpoints,
			} if direction == crate::syn::MatchDirection::Undirected => {
				for (index, endpoint) in endpoints.iter().enumerate() {
					if endpoint.local_id() != Some(source) {
						continue;
					}
					for target in endpoints
						.iter()
						.enumerate()
						.filter(|(target_index, _target)| *target_index != index)
						.filter_map(|(_target_index, target)| target.local_id())
					{
						candidates.push(TraversalCandidate {
							target,
							edge: *edge_id,
							direction: PathDirection::Undirected,
							role: EndpointRole::Undirected,
						});
					}
				}
			}
			EdgeTopology::Bidirectional {
				..
			}
			| EdgeTopology::Undirected {
				..
			} => {}
		}
	}
	Ok(candidates)
}

/// Appends all local target endpoints when the source occurs in `from`.
pub(super) fn push_endpoint_pairs(
	candidates: &mut Vec<TraversalCandidate>,
	source: AtomId,
	edge: AtomId,
	from: &[EdgeEndpoint],
	to: &[EdgeEndpoint],
	direction: PathDirection,
	role: EndpointRole,
) {
	if !from.iter().any(|endpoint| endpoint.local_id() == Some(source)) {
		return;
	}
	for target in to.iter().filter_map(EdgeEndpoint::local_id) {
		candidates.push(TraversalCandidate {
			target,
			edge,
			direction,
			role,
		});
	}
}

/// Converts traversal-index steps into MATCH candidates.
pub(super) fn index_candidates<Graph: GraphRead>(
	graph: &Graph,
	index: &TraversalIndex,
	source: AtomId,
	direction: crate::syn::MatchDirection,
) -> FerrosGrynnResult<Vec<TraversalCandidate>> {
	let traversal = match direction {
		crate::syn::MatchDirection::Forward => TraversalDirection::Forward,
		crate::syn::MatchDirection::Backward => TraversalDirection::Backward,
		crate::syn::MatchDirection::Undirected | crate::syn::MatchDirection::Bidirectional => {
			TraversalDirection::Both
		}
	};
	let endpoint = EdgeEndpoint::Local(source);
	let mut candidates = Vec::new();
	for step in index.steps(&endpoint, traversal) {
		let Some(target) = step.target().local_id() else {
			continue;
		};
		let edge = graph.edge(step.via_edge())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("edge", step.via_edge().to_string())
		})?;
		if !direction_accepts(direction, edge.topology()?.direction()) {
			continue;
		}
		candidates.push(TraversalCandidate {
			target,
			edge: step.via_edge(),
			direction: step.direction(),
			role: step.role(),
		});
	}
	Ok(candidates)
}

/// Checks whether an edge topology satisfies the requested MATCH direction.
pub(super) const fn direction_accepts(
	requested: crate::syn::MatchDirection,
	actual: EdgeDirection,
) -> bool {
	match requested {
		crate::syn::MatchDirection::Forward | crate::syn::MatchDirection::Backward => {
			matches!(actual, EdgeDirection::Directed | EdgeDirection::Bidirectional)
		}
		crate::syn::MatchDirection::Undirected => matches!(actual, EdgeDirection::Undirected),
		crate::syn::MatchDirection::Bidirectional => {
			matches!(actual, EdgeDirection::Bidirectional)
		}
	}
}

/// Incremental depth-first traversal state for one endpoint.
pub(super) struct DfsFrame {
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
pub(super) struct PathExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Variable-length expansion contract.
	expand: PathExpand,
	/// Current upstream row.
	row: Option<Row>,
	/// Active DFS stack.
	frames: Vec<DfsFrame>,
	/// Active connector path aligned below the frame stack.
	hops: Vec<TraversalCandidate>,
}

impl<'a, Graph> PathExpandCursor<'a, Graph> {
	/// Creates an incremental path-expansion cursor.
	pub(super) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		index: Rc<TraversalIndex>,
		expand: PathExpand,
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
	fn start_row(&mut self) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next()? else {
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
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if self.row.is_none() && !self.start_row()? {
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
				}) || self.row.as_ref().is_some_and(|row| row.used_edges.contains(&candidate.edge))
				|| self.hops.iter().any(|hop| hop.edge == candidate.edge)
				|| !connector_matches(
					self.graph.as_ref(),
					candidate.edge,
					&self.expand.edge_labels,
				)? {
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
pub(super) fn path_row<Graph: GraphRead>(
	input: &Row,
	expand: &PathExpand,
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

pub(super) struct ScanCursor<'a> {
	pub(super) inner: Box<dyn RowCursor + 'a>,
}

impl RowCursor for ScanCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		self.inner.next()
	}
}

pub(super) struct OneRowCursor {
	pub(super) row: Option<Row>,
}

impl RowCursor for OneRowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.row.take())
	}
}

/// Resolves public fields for an atom bound in a MATCH row.
pub(super) trait AtomFieldRead {
	/// Reads `id`, `label`, or a dictionary-backed property.
	fn field(&self, atom: AtomId, field: &str) -> FerrosGrynnResult<Value>;
}

/// Atom field resolver sharing the current graph and property dictionary.
pub(super) struct GraphFields<Graph> {
	/// Snapshot graph view.
	pub(super) graph: Rc<Graph>,
	/// Snapshot property dictionary.
	pub(super) dictionary: Arc<PropertyDictionary>,
}

impl<Graph: GraphRead> AtomFieldRead for GraphFields<Graph> {
	fn field(&self, atom: AtomId, field: &str) -> FerrosGrynnResult<Value> {
		match atom.kind() {
			Some(AtomKind::Node) => {
				let node = self.graph.node(atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("node", atom.to_string())
				})?;
				match field {
					"id" => Ok(Value::Record(Box::new(node.record_id()?.into_owned()))),
					"label" => Ok(node
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(node.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			Some(AtomKind::Edge) => {
				let edge = self.graph.edge(atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("edge", atom.to_string())
				})?;
				match field {
					"id" => Ok(Value::Record(Box::new(edge.record_id()?.into_owned()))),
					"label" => Ok(edge
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(edge.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			None => Err(FerrosGrynnError::atom_does_not_exist("atom", atom.to_string())),
		}
	}
}

pub(super) struct FilterCursor<'a> {
	pub(super) input: Box<dyn RowCursor + 'a>,
	pub(super) predicate: &'a TypedExpr,
	pub(super) slots: &'a [Value],
	pub(super) source: &'a str,
	/// Optional MATCH atom-field resolver.
	pub(super) fields: Option<&'a dyn AtomFieldRead>,
}

impl RowCursor for FilterCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		while let Some(row) = self.input.next()? {
			if eval(self.predicate, Some(&row), self.slots, self.source, self.fields)?.is_truthy() {
				return Ok(Some(row));
			}
		}
		Ok(None)
	}
}

pub(super) struct ProjectCursor<'a> {
	pub(super) input: Box<dyn RowCursor + 'a>,
	pub(super) expressions: &'a [TypedExpr],
	pub(super) slots: &'a [Value],
	pub(super) source: &'a str,
	/// Optional MATCH atom-field resolver.
	pub(super) fields: Option<&'a dyn AtomFieldRead>,
}

impl RowCursor for ProjectCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.input.next()? else {
			return Ok(None);
		};
		let values = self
			.expressions
			.iter()
			.map(|expression| eval(expression, Some(&row), self.slots, self.source, self.fields))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Some(Row::new(values)))
	}
}

pub(super) struct LimitCursor<'a> {
	pub(super) input: Box<dyn RowCursor + 'a>,
	pub(super) remaining: Option<usize>,
	pub(super) skip: usize,
}

impl RowCursor for LimitCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		while self.skip > 0 {
			if self.input.next()?.is_none() {
				return Ok(None);
			}
			self.skip -= 1;
		}
		if self.remaining == Some(0) {
			return Ok(None);
		}
		let row = self.input.next()?;
		if row.is_some()
			&& let Some(remaining) = &mut self.remaining
		{
			*remaining -= 1;
		}
		Ok(row)
	}
}
