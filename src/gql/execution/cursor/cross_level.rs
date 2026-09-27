//! Cross-graph fixed expansion through qualified nested endpoints.

use super::*;

/// Expands one fixed connector while preserving the graph scope of both the
/// connector and its selected endpoint.
pub(in crate::gql::execution) struct CrossLevelExpandCursor<'a, Read: QueryRead> {
	input: Box<dyn RowCursor + 'a>,
	read: &'a Read,
	snapshot: Snapshot,
	fields: &'a MatchGraphFields<'a, Read>,
	index: Rc<ScopedTraversalIndex>,
	expand: ResolvedExpand,
	predicate: MatchPredicateContext<'a>,
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	row: Option<Row>,
	candidates: Vec<ScopedTraversalCandidate>,
	next_candidate: usize,
}

impl<'a, Read: QueryRead> CrossLevelExpandCursor<'a, Read> {
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		read: &'a Read,
		snapshot: Snapshot,
		fields: &'a MatchGraphFields<'a, Read>,
		index: Rc<ScopedTraversalIndex>,
		expand: ResolvedExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
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
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		}
	}

	fn emit(
		&self,
		input: &Row,
		candidate: &ScopedTraversalCandidate,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Option<Row>> {
		let edge_ref = self.fields.reference(candidate.edge.graph)?;
		control.record_match_graph_open();
		let edge_graph = self.read.graph(self.snapshot, &edge_ref)?;
		let target_ref = self.fields.reference(candidate.target.graph)?;
		control.record_match_graph_open();
		let target_graph = self.read.graph(self.snapshot, &target_ref)?;
		if !atom_matches(&edge_graph, candidate.edge.atom, &self.expand.edge)?
			|| !atom_matches(&target_graph, candidate.target.atom, &self.expand.target)?
			|| !scoped_filters_match(
				input.current,
				&self.expand.filters,
				candidate,
				&edge_graph,
				&target_graph,
				self.read,
				self.snapshot,
				self.fields,
			)? {
			return Ok(None);
		}
		let mut row = input.clone();
		if !bind_match_position(
			&mut row,
			&self.expand.edge,
			candidate.edge.atom,
			&edge_graph,
			&edge_ref,
			self.expand.mode,
			self.predicate,
			control,
		)? || !bind_match_position(
			&mut row,
			&self.expand.target,
			candidate.target.atom,
			&target_graph,
			&target_ref,
			self.expand.mode,
			self.predicate,
			control,
		)? {
			return Ok(None);
		}
		append_scoped_trace_hop(
			&mut row,
			candidate,
			&edge_graph,
			&edge_ref,
			&target_graph,
			&target_ref,
			self.trace.as_ref(),
			Number::Int(1),
			control,
		)?;
		row.current = Some(candidate.target);
		Ok(Some(row))
	}
}

impl<Read: QueryRead> RowCursor for CrossLevelExpandCursor<'_, Read> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				control.record_path_expansion()?;
				if let Some(row) = self.emit(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("cross-level MATCH lost its input row")
					})?,
					&candidate,
					control,
				)? {
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next(control)? else {
				return Ok(None);
			};
			let source = row.current.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("cross-level MATCH has no source endpoint")
			})?;
			self.candidates = self.index.candidates(source, self.expand.direction);
			self.next_candidate = 0;
			self.row = Some(row);
		}
	}
}

pub(in crate::gql::execution) fn scoped_filters_match<
	EdgeGraph: GraphRead,
	TargetGraph: GraphRead,
	Read: QueryRead,
>(
	source: Option<BoundAtom>,
	filters: &[crate::syn::MatchPathFilter],
	candidate: &ScopedTraversalCandidate,
	edge_graph: &EdgeGraph,
	target_graph: &TargetGraph,
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
) -> FerrosGrynnResult<bool> {
	let edge = edge_graph.edge(candidate.edge.atom)?.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("cross-level MATCH connector disappeared")
	})?;
	let topology = edge.topology()?;
	for filter in filters {
		let accepted = match filter {
			crate::syn::MatchPathFilter::Weighted => edge.weight().is_some(),
			crate::syn::MatchPathFilter::HyperEdges => topology.is_hyper(),
			crate::syn::MatchPathFilter::MetaEdges => edge.is_meta()?,
			crate::syn::MatchPathFilter::CrossLevel => candidate.cross_level,
			crate::syn::MatchPathFilter::MetaVertices => {
				scoped_atom_is_meta(source, read, snapshot, fields)?
					&& local_atom_is_meta(target_graph, candidate.target.atom)?
			}
			crate::syn::MatchPathFilter::Meta => {
				edge.is_meta()?
					&& scoped_atom_is_meta(source, read, snapshot, fields)?
					&& local_atom_is_meta(target_graph, candidate.target.atom)?
			}
		};
		if !accepted {
			return Ok(false);
		}
	}
	Ok(true)
}

fn scoped_atom_is_meta<Read: QueryRead>(
	atom: Option<BoundAtom>,
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
) -> FerrosGrynnResult<bool> {
	let Some(atom) = atom else {
		return Ok(false);
	};
	let reference = fields.reference(atom.graph)?;
	let graph = read.graph(snapshot, &reference)?;
	local_atom_is_meta(&graph, atom.atom)
}

fn local_atom_is_meta<Graph: GraphRead>(graph: &Graph, atom: AtomId) -> FerrosGrynnResult<bool> {
	match atom.kind() {
		Some(AtomKind::Node) => graph.node(atom)?.map_or(Ok(false), |node| node.is_meta()),
		Some(AtomKind::Edge) => graph.edge(atom)?.map_or(Ok(false), |edge| edge.is_meta()),
		None => Ok(false),
	}
}

#[allow(clippy::too_many_arguments, reason = "a path step has separate edge and endpoint scopes")]
pub(in crate::gql::execution) fn append_scoped_trace_hop<
	EdgeGraph: GraphRead,
	TargetGraph: GraphRead,
>(
	row: &mut Row,
	candidate: &ScopedTraversalCandidate,
	edge_graph: &EdgeGraph,
	edge_ref: &Arc<GraphRef>,
	target_graph: &TargetGraph,
	target_ref: &Arc<GraphRef>,
	arena: Option<&Rc<RefCell<MatchPathTrace>>>,
	weight: Number,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let (Some(previous), Some(arena)) = (row.trace, arena) else {
		return Ok(());
	};
	let atom = target_graph.atom_ref(candidate.target.atom)?.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("cross-level MATCH target disappeared")
	})?;
	let edge = edge_graph.atom_ref(candidate.edge.atom)?.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("cross-level MATCH connector disappeared")
	})?;
	let Value::Number(cost) =
		binary(BinaryOperator::Add, Value::Number(row.trace_cost), Value::Number(weight))?
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"MATCH path cost addition did not return a number",
		));
	};
	let step = PathStep::new(
		AtomOccurrenceRef::new(atom, target_ref.clone()),
		Some(AtomOccurrenceRef::new(edge, edge_ref.clone())),
		candidate.direction,
		candidate.role,
	);
	row.trace = Some(arena.borrow_mut().push(Some(previous), step, Some(cost), control)?);
	row.trace_cost = cost;
	Ok(())
}
