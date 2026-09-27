//! Bounded pull execution for graph containment joins.

use std::collections::{BTreeSet, VecDeque};

use super::*;

/// Direct or bounded transitive containment relation after expression
/// resolution.
pub(in crate::gql::execution) struct ResolvedWithin {
	pub(in crate::gql::execution) child: ResolvedMatchBinding,
	pub(in crate::gql::execution) owner: ResolvedMatchBinding,
	pub(in crate::gql::execution) min: u32,
	pub(in crate::gql::execution) max: u32,
}

struct PendingRow {
	row: Row,
	bytes: usize,
}

/// Joins visible meta owners to atoms in their direct or transitive child
/// graphs. Rows are buffered only up to the shared statement memory budget.
pub(in crate::gql::execution) struct WithinCursor<'a, Read: QueryRead> {
	input: Box<dyn RowCursor + 'a>,
	read: &'a Read,
	snapshot: Snapshot,
	root: Arc<GraphRef>,
	fields: &'a MatchGraphFields<'a, Read>,
	relation: ResolvedWithin,
	mode: crate::syn::MatchMode,
	predicate: MatchPredicateContext<'a>,
	pending: VecDeque<PendingRow>,
	account: Option<BarrierAccount>,
}

impl<'a, Read: QueryRead> WithinCursor<'a, Read> {
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		read: &'a Read,
		snapshot: Snapshot,
		root: Arc<GraphRef>,
		fields: &'a MatchGraphFields<'a, Read>,
		relation: ResolvedWithin,
		mode: crate::syn::MatchMode,
		predicate: MatchPredicateContext<'a>,
	) -> Self {
		Self {
			input,
			read,
			snapshot,
			root,
			fields,
			relation,
			mode,
			predicate,
			pending: VecDeque::new(),
			account: None,
		}
	}

	fn fill(&mut self, input: &Row, control: &ExecutionControl) -> FerrosGrynnResult<()> {
		control.record_match_graph_open();
		let graph = self.read.graph(self.snapshot, &self.root)?;
		let candidates = match self
			.relation
			.owner
			.column
			.and_then(|column| input.atoms.get(column).copied().flatten())
		{
			Some(owner) if owner.graph == self.root.id() => vec![owner.atom],
			Some(_) => Vec::new(),
			None => self.read.match_candidates(self.snapshot, &self.root, &self.relation.owner)?,
		};
		for owner in candidates {
			control.record_path_expansion()?;
			control.record_match_containment_link();
			if !atom_matches(&graph, owner, &self.relation.owner)? {
				continue;
			}
			let mut row = input.clone();
			if !bind_match_position(
				&mut row,
				&self.relation.owner,
				owner,
				&graph,
				&self.root,
				self.mode,
				self.predicate,
				control,
			)? {
				continue;
			}
			let Some(child) = nested_graph_reference(&graph, owner)? else {
				continue;
			};
			let mut visited = BTreeSet::from([BoundAtom::new(self.root.id(), owner)]);
			let account = self.account.get_or_insert_with(|| control.barrier_account());
			reserve_containment_state(account, control)?;
			let result = descend_within(
				self.read,
				self.snapshot,
				self.fields,
				&self.relation,
				self.mode,
				self.predicate,
				&row,
				child,
				1,
				&mut visited,
				&mut self.pending,
				account,
				control,
			);
			account.release(containment_state_bytes());
			result?;
		}
		Ok(())
	}
}

impl<Read: QueryRead> RowCursor for WithinCursor<'_, Read> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if let Some(pending) = self.pending.pop_front() {
				if let Some(account) = &mut self.account {
					account.release(pending.bytes);
				}
				return Ok(Some(pending.row));
			}
			let Some(input) = self.input.next(control)? else {
				return Ok(None);
			};
			self.fill(&input, control)?;
		}
	}
}

#[allow(clippy::too_many_arguments, reason = "the recursive walk keeps one explicit bounded state")]
fn descend_within<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	relation: &ResolvedWithin,
	mode: crate::syn::MatchMode,
	predicate: MatchPredicateContext<'_>,
	base: &Row,
	reference: GraphRef,
	depth: u32,
	visited: &mut BTreeSet<BoundAtom>,
	pending: &mut VecDeque<PendingRow>,
	account: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	control.check()?;
	let reference = fields.register(reference);
	control.record_match_graph_open();
	let graph = read.graph(snapshot, &reference)?;
	if depth >= relation.min {
		let candidates = match relation
			.child
			.column
			.and_then(|column| base.atoms.get(column).copied().flatten())
		{
			Some(child) if child.graph == reference.id() => vec![child.atom],
			Some(_) => Vec::new(),
			None => read.match_candidates(snapshot, &reference, &relation.child)?,
		};
		for child in candidates {
			control.record_path_expansion()?;
			control.record_match_containment_link();
			if !atom_matches(&graph, child, &relation.child)? {
				continue;
			}
			let mut row = base.clone();
			if bind_match_position(
				&mut row,
				&relation.child,
				child,
				&graph,
				&reference,
				mode,
				predicate,
				control,
			)? {
				let bytes = row.estimated_heap_bytes();
				if !account.reserve(bytes) {
					return Err(FerrosGrynnError::control_limit_exceeded(
						"MATCH containment memory",
						u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
					));
				}
				pending.push_back(PendingRow {
					row,
					bytes,
				});
			}
		}
	}
	if depth == relation.max {
		return Ok(());
	}
	for owner in graph.node_ids()?.chain(graph.edge_ids()?) {
		let owner = owner?;
		let occurrence = BoundAtom::new(reference.id(), owner);
		if visited.contains(&occurrence) {
			continue;
		}
		let Some(child) = nested_graph_reference(&graph, owner)? else {
			continue;
		};
		control.record_path_expansion()?;
		control.record_match_containment_link();
		reserve_containment_state(account, control)?;
		visited.insert(occurrence);
		let result = descend_within(
			read,
			snapshot,
			fields,
			relation,
			mode,
			predicate,
			base,
			child,
			depth + 1,
			visited,
			pending,
			account,
			control,
		);
		visited.remove(&occurrence);
		account.release(containment_state_bytes());
		result?;
	}
	Ok(())
}

fn containment_state_bytes() -> usize {
	std::mem::size_of::<BoundAtom>().saturating_add(std::mem::size_of::<usize>().saturating_mul(4))
}

fn reserve_containment_state(
	account: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	if account.reserve(containment_state_bytes()) {
		return Ok(());
	}
	Err(FerrosGrynnError::control_limit_exceeded(
		"MATCH containment memory",
		u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
	))
}

pub(in crate::gql::execution) fn nested_graph_reference<Graph: GraphRead>(
	graph: &Graph,
	owner: AtomId,
) -> FerrosGrynnResult<Option<GraphRef>> {
	match owner.kind() {
		Some(AtomKind::Node) => graph
			.node(owner)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", owner.to_string()))?
			.inner_graph_ref()
			.map(|value| value.map(std::borrow::Cow::into_owned)),
		Some(AtomKind::Edge) => graph
			.edge(owner)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", owner.to_string()))?
			.transition_graph_ref()
			.map(|value| value.map(std::borrow::Cow::into_owned)),
		None => Err(FerrosGrynnError::invalid_storage_file(
			"MATCH containment owner has an invalid atom kind",
		)),
	}
}
