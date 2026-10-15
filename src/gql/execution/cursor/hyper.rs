//! Pull execution for hyperedge endpoint assignments.

use super::*;

/// One resolved HEAD, TAIL, or unqualified endpoint group.
pub(in crate::gql::execution) struct ResolvedHyperContains {
	/// Side associated with this resolved hyper contains.
	pub(in crate::gql::execution) side: crate::syn::MatchHyperSide,
	/// Endpoints associated with this resolved hyper contains.
	pub(in crate::gql::execution) endpoints: Vec<ResolvedMatchBinding>,
}

/// Cartesian-product position used to assign candidate atoms to the bindings of
/// a hyperedge match.
struct AssignmentState {
	/// Base associated with this assignment state.
	base: Row,
	/// Bindings associated with this assignment state.
	bindings: Vec<ResolvedMatchBinding>,
	/// Candidates associated with this assignment state.
	candidates: Vec<Vec<BoundAtom>>,
	/// Indices associated with this assignment state.
	indices: Vec<usize>,
	/// Done associated with this assignment state.
	done: bool,
}

impl AssignmentState {
	/// Creates a new `AssignmentState` value.
	fn new(
		base: Row,
		bindings: Vec<ResolvedMatchBinding>,
		candidates: Vec<Vec<BoundAtom>>,
	) -> Self {
		let done = candidates.iter().any(Vec::is_empty);
		let indices = vec![0; candidates.len()];
		Self {
			base,
			bindings,
			candidates,
			indices,
			done,
		}
	}

	/// Returns the current result.
	fn current(&self) -> Option<Vec<BoundAtom>> {
		(!self.done).then(|| {
			self.candidates
				.iter()
				.zip(&self.indices)
				.map(|(values, index)| values[*index])
				.collect()
		})
	}

	/// Advances assignment state.
	fn advance(&mut self) {
		if self.candidates.is_empty() {
			self.done = true;
			return;
		}
		for position in (0..self.indices.len()).rev() {
			self.indices[position] += 1;
			if self.indices[position] < self.candidates[position].len() {
				return;
			}
			self.indices[position] = 0;
		}
		self.done = true;
	}
}

/// Assigns hyperedge endpoint positions without materializing the Cartesian
/// product.
pub(in crate::gql::execution) struct HyperContainsCursor<'a, Read: QueryRead, Graph> {
	/// Input associated with this hyper contains cursor.
	input: Box<dyn RowCursor + 'a>,
	/// Read associated with this hyper contains cursor.
	read: &'a Read,
	/// Snapshot associated with this hyper contains cursor.
	snapshot: Snapshot,
	/// Fields associated with this hyper contains cursor.
	fields: &'a MatchGraphFields<'a, Read>,
	/// Graph associated with this hyper contains cursor.
	graph: Rc<Graph>,
	/// Graph ref associated with this hyper contains cursor.
	graph_ref: Arc<GraphRef>,
	/// Edge associated with this hyper contains cursor.
	edge: ResolvedMatchBinding,
	/// Contains associated with this hyper contains cursor.
	contains: Vec<ResolvedHyperContains>,
	/// Mode associated with this hyper contains cursor.
	mode: crate::syn::MatchMode,
	/// Predicate associated with this hyper contains cursor.
	predicate: MatchPredicateContext<'a>,
	/// Edges associated with this hyper contains cursor.
	edges: Vec<AtomId>,
	/// Row associated with this hyper contains cursor.
	row: Option<Row>,
	/// Next edge associated with this hyper contains cursor.
	next_edge: usize,
	/// Assignment associated with this hyper contains cursor.
	assignment: Option<AssignmentState>,
}

impl<'a, Read: QueryRead, Graph: GraphRead> HyperContainsCursor<'a, Read, Graph> {
	/// Creates a new `HyperContainsCursor<'a, Read, Graph>` value.
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		read: &'a Read,
		snapshot: Snapshot,
		fields: &'a MatchGraphFields<'a, Read>,
		graph_view: (Rc<Graph>, Arc<GraphRef>),
		edge: ResolvedMatchBinding,
		contains: Vec<ResolvedHyperContains>,
		mode: crate::syn::MatchMode,
		predicate: MatchPredicateContext<'a>,
	) -> FerrosGrynnResult<Self> {
		let (graph, graph_ref) = graph_view;

		let edges = graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Self {
			input,
			read,
			snapshot,
			fields,
			graph,
			graph_ref,
			edge,
			contains,
			mode,
			predicate,
			edges,
			row: None,
			next_edge: 0,
			assignment: None,
		})
	}

	/// Returns the assignment result.
	fn assignment(
		&self,
		input: &Row,
		edge_id: AtomId,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Option<AssignmentState>> {
		if !atom_matches(self.graph.as_ref(), edge_id, &self.edge)? {
			return Ok(None);
		}
		let edge = self
			.graph
			.edge(edge_id)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge_id.to_string()))?;
		let topology = edge.topology()?;
		if !topology.is_hyper() {
			return Ok(None);
		}
		let mut base = input.clone();
		if !bind_match_position(
			&mut base,
			&self.edge,
			edge_id,
			self.graph.as_ref(),
			&self.graph_ref,
			self.mode,
			self.predicate,
			control,
		)? {
			return Ok(None);
		}

		let mut bindings = Vec::new();
		let mut candidates = Vec::new();
		for group in &self.contains {
			let endpoints: &[EdgeEndpoint] = match (&*topology, group.side) {
				(
					EdgeTopology::Undirected {
						endpoints,
					},
					crate::syn::MatchHyperSide::Any,
				) => endpoints,
				(
					EdgeTopology::Undirected {
						..
					},
					_,
				) => &[],
				(_, crate::syn::MatchHyperSide::Any) => {
					// HEAD and TAIL are concatenated below for the unqualified
					// form. A small allocation keeps the
					// cursor state simple and bounded by edge arity.
					let all = topology
						.endpoints()
						.map(|endpoint| {
							resolve_scoped_endpoint(
								self.read,
								self.snapshot,
								self.fields,
								&self.graph_ref,
								endpoint,
							)
						})
						.collect::<FerrosGrynnResult<Vec<_>>>()?;
					for binding in &group.endpoints {
						bindings.push(binding.clone());
						candidates.push(all.clone());
					}
					continue;
				}
				(_, crate::syn::MatchHyperSide::Head) => topology.source(),
				(_, crate::syn::MatchHyperSide::Tail) => topology.target(),
			};
			let scoped = endpoints
				.iter()
				.map(|endpoint| {
					resolve_scoped_endpoint(
						self.read,
						self.snapshot,
						self.fields,
						&self.graph_ref,
						endpoint,
					)
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			for binding in &group.endpoints {
				bindings.push(binding.clone());
				candidates.push(scoped.clone());
			}
		}
		Ok(Some(AssignmentState::new(base, bindings, candidates)))
	}
}

impl<Read: QueryRead, Graph: GraphRead> RowCursor for HyperContainsCursor<'_, Read, Graph> {
	/// Advances this `HyperContainsCursor<'_, Read, Graph>` and returns its
	/// next element, if any.
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if let Some(assignment) = &mut self.assignment {
				while let Some(atoms) = assignment.current() {
					assignment.advance();
					control.record_path_expansion()?;
					control.record_match_hyper_assignment();
					let mut row = assignment.base.clone();
					let mut accepted = true;
					for (binding, atom) in assignment.bindings.iter().zip(atoms) {
						let reference = self.fields.reference(atom.graph)?;
						control.record_match_graph_open();
						let graph = self.read.graph(self.snapshot, &reference)?;
						if !atom_matches(&graph, atom.atom, binding)?
							|| !bind_match_position(
								&mut row,
								binding,
								atom.atom,
								&graph,
								&reference,
								self.mode,
								self.predicate,
								control,
							)? {
							accepted = false;
							break;
						}
					}
					if accepted {
						return Ok(Some(row));
					}
				}
				self.assignment = None;
			}
			if self.row.is_none() {
				self.row = self.input.next(control)?;
				self.next_edge = 0;
				if self.row.is_none() {
					return Ok(None);
				}
			}
			while let Some(edge) = self.edges.get(self.next_edge).copied() {
				self.next_edge += 1;
				if let Some(assignment) = self.assignment(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH HYPER lost its input row.")
					})?,
					edge,
					control,
				)? {
					self.assignment = Some(assignment);
					break;
				}
			}
			if self.assignment.is_none() {
				self.row = None;
			}
		}
	}
}
