//! Runtime MATCH selectors and seed binding.

use super::*;

#[derive(Clone)]
pub(in crate::gql::execution) enum ResolvedMatchSelector {
	Any,
	Class(Box<str>),
	Record(RecordId),
}

#[derive(Clone)]
pub(in crate::gql::execution) struct ResolvedMatchBinding {
	pub(in crate::gql::execution) domain: EndpointDomain,
	pub(in crate::gql::execution) selector: ResolvedMatchSelector,
	pub(in crate::gql::execution) column: Option<usize>,
	pub(in crate::gql::execution) label: Option<Box<str>>,
	pub(in crate::gql::execution) properties: Vec<(Box<str>, TypedExpr)>,
	pub(in crate::gql::execution) predicate: Option<TypedExpr>,
	pub(in crate::gql::execution) meta: bool,
}

/// Statement-local evaluator used by inline MATCH constraints.
#[derive(Clone, Copy)]
pub(in crate::gql::execution) struct MatchPredicateContext<'a> {
	pub(in crate::gql::execution) slots: &'a [Value],
	pub(in crate::gql::execution) source: &'a str,
	pub(in crate::gql::execution) fields: &'a dyn AtomFieldRead,
}

#[derive(Clone)]
pub(in crate::gql::execution) struct ResolvedExpand {
	pub(in crate::gql::execution) edge: ResolvedMatchBinding,
	pub(in crate::gql::execution) target: ResolvedMatchBinding,
	pub(in crate::gql::execution) direction: crate::syn::MatchDirection,
	pub(in crate::gql::execution) mode: crate::syn::MatchMode,
	pub(in crate::gql::execution) filters: Vec<crate::syn::MatchPathFilter>,
}

#[derive(Clone)]
pub(in crate::gql::execution) struct ResolvedPathExpand {
	pub(in crate::gql::execution) edge: ResolvedMatchBinding,
	pub(in crate::gql::execution) target: ResolvedMatchBinding,
	pub(in crate::gql::execution) direction: crate::syn::MatchDirection,
	pub(in crate::gql::execution) min: u32,
	pub(in crate::gql::execution) max: u32,
	pub(in crate::gql::execution) path_column: Option<usize>,
	pub(in crate::gql::execution) match_mode: crate::syn::MatchMode,
	pub(in crate::gql::execution) path_mode: crate::syn::MatchPathMode,
	pub(in crate::gql::execution) objective: Option<crate::syn::MatchPathObjective>,
	pub(in crate::gql::execution) filters: Vec<crate::syn::MatchPathFilter>,
}

pub(in crate::gql::execution) fn resolve_match_binding(
	binding: &MatchBinding,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<ResolvedMatchBinding> {
	let selector = match &binding.selector {
		LogicalMatchSelector::Any => ResolvedMatchSelector::Any,
		LogicalMatchSelector::Class(class) => ResolvedMatchSelector::Class(class.clone()),
		LogicalMatchSelector::Record(record) => {
			ResolvedMatchSelector::Record(RecordId::from_value(
				record.class.as_ref(),
				eval(&record.key, None, slots, source, None, Some(control))?,
			)?)
		}
	};
	Ok(ResolvedMatchBinding {
		domain: binding.domain,
		selector,
		column: binding.column,
		label: binding.label.clone(),
		properties: binding.properties.clone(),
		predicate: binding.predicate.clone(),
		meta: binding.meta,
	})
}

pub(in crate::gql::execution) fn resolve_expand(
	expand: &Expand,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	mode: crate::syn::MatchMode,
) -> FerrosGrynnResult<ResolvedExpand> {
	Ok(ResolvedExpand {
		edge: resolve_match_binding(&expand.edge, slots, source, control)?,
		target: resolve_match_binding(&expand.target, slots, source, control)?,
		direction: expand.direction,
		mode,
		filters: expand.filters.clone(),
	})
}

pub(in crate::gql::execution) fn resolve_path_expand(
	expand: &PathExpand,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	match_mode: crate::syn::MatchMode,
) -> FerrosGrynnResult<ResolvedPathExpand> {
	Ok(ResolvedPathExpand {
		edge: resolve_match_binding(&expand.edge, slots, source, control)?,
		target: resolve_match_binding(&expand.target, slots, source, control)?,
		direction: expand.direction,
		min: expand.min,
		max: expand.max,
		path_column: expand.path_column,
		match_mode,
		path_mode: expand.mode,
		objective: expand.objective,
		filters: expand.filters.clone(),
	})
}

/// Pull cursor that binds the first endpoint of a pattern.
pub(in crate::gql::execution) struct SeedCursor<'a, Graph> {
	input: Box<dyn RowCursor + 'a>,
	graph: Rc<Graph>,
	graph_ref: Arc<GraphRef>,
	binding: ResolvedMatchBinding,
	mode: crate::syn::MatchMode,
	predicate: MatchPredicateContext<'a>,
	ids: Vec<AtomId>,
	row: Option<Row>,
	bound: Option<BoundAtom>,
	next_id: usize,
}

impl<'a, Graph> SeedCursor<'a, Graph> {
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		graph_ref: Arc<GraphRef>,
		binding: ResolvedMatchBinding,
		mode: crate::syn::MatchMode,
		predicate: MatchPredicateContext<'a>,
		ids: Vec<AtomId>,
	) -> Self {
		Self {
			input,
			graph,
			graph_ref,
			binding,
			mode,
			predicate,
			ids,
			row: None,
			bound: None,
			next_id: 0,
		}
	}
}

impl<Graph: GraphRead> RowCursor for SeedCursor<'_, Graph> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if self.row.is_none() {
				let Some(row) = self.input.next(control)? else {
					return Ok(None);
				};
				self.bound =
					self.binding.column.and_then(|column| row.atoms.get(column).copied().flatten());
				self.row = Some(row);
				self.next_id = 0;
			}
			let from_bound = self.bound.is_some();
			let candidate = if let Some(bound) = self.bound {
				(self.next_id == 0 && bound.graph == self.graph_ref.id()).then_some(bound.atom)
			} else {
				self.ids.get(self.next_id).copied()
			};
			let Some(candidate) = candidate else {
				self.row = None;
				continue;
			};
			self.next_id += 1;
			if (from_bound || self.binding.meta)
				&& !atom_matches(self.graph.as_ref(), candidate, &self.binding)?
			{
				continue;
			}
			let mut row = self.row.as_ref().cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH seed lost its input row.")
			})?;
			if !bind_match_position(
				&mut row,
				&self.binding,
				candidate,
				self.graph.as_ref(),
				&self.graph_ref,
				self.mode,
				self.predicate,
				control,
			)? {
				continue;
			}
			control.record_match_seed();
			row.current = Some(BoundAtom::new(self.graph_ref.id(), candidate));
			return Ok(Some(row));
		}
	}
}

/// Applies clause uniqueness, binds a public occurrence, and evaluates inline
/// property and WHERE constraints.
pub(in crate::gql::execution) fn bind_match_position<Graph: GraphRead>(
	row: &mut Row,
	binding: &ResolvedMatchBinding,
	atom: AtomId,
	graph: &Graph,
	graph_ref: &Arc<GraphRef>,
	mode: crate::syn::MatchMode,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<bool> {
	let prebound = binding.column.and_then(|column| row.atoms.get(column).copied().flatten());
	let occurrence = BoundAtom::new(graph_ref.id(), atom);
	if prebound.is_none() {
		let repeated = match mode {
			crate::syn::MatchMode::RepeatableElements => false,
			crate::syn::MatchMode::DifferentEdges => {
				atom.kind() == Some(AtomKind::Edge) && row.used_edges.contains(&occurrence)
			}
			crate::syn::MatchMode::DifferentAtoms => row.used_atoms.contains(&occurrence),
		};
		if repeated {
			return Ok(false);
		}
	}
	if !bind_atom(row, binding, atom, graph, graph_ref)? {
		return Ok(false);
	}
	for (field, expected) in &binding.properties {
		let actual = predicate.fields.field(occurrence, field)?;
		if actual == Value::None
			|| actual
				!= eval(
					expected,
					Some(row),
					predicate.slots,
					predicate.source,
					Some(predicate.fields),
					Some(control),
				)? {
			return Ok(false);
		}
	}
	if let Some(expression) = &binding.predicate
		&& !eval(
			expression,
			Some(row),
			predicate.slots,
			predicate.source,
			Some(predicate.fields),
			Some(control),
		)?
		.is_truthy()
	{
		return Ok(false);
	}
	if prebound.is_none() {
		match mode {
			crate::syn::MatchMode::RepeatableElements => {}
			crate::syn::MatchMode::DifferentEdges if atom.kind() == Some(AtomKind::Edge) => {
				row.used_edges.push(occurrence);
			}
			crate::syn::MatchMode::DifferentAtoms => row.used_atoms.push(occurrence),
			crate::syn::MatchMode::DifferentEdges => {}
		}
	}
	Ok(true)
}

pub(in crate::gql::execution) fn atom_matches<Graph: GraphRead>(
	graph: &Graph,
	atom: AtomId,
	binding: &ResolvedMatchBinding,
) -> FerrosGrynnResult<bool> {
	if atom.kind()
		!= Some(match binding.domain {
			EndpointDomain::Node => AtomKind::Node,
			EndpointDomain::Edge => AtomKind::Edge,
		}) {
		return Ok(false);
	}
	if !matches!(binding.selector, ResolvedMatchSelector::Any) {
		let Some(reference) = graph.atom_ref(atom)? else {
			return Ok(false);
		};
		let selected = match &binding.selector {
			ResolvedMatchSelector::Any => true,
			ResolvedMatchSelector::Class(class) => reference.id().class() == class.as_ref(),
			ResolvedMatchSelector::Record(record) => reference.id() == record,
		};
		if !selected {
			return Ok(false);
		}
	}
	if binding.meta {
		let is_meta = match binding.domain {
			EndpointDomain::Node => graph.node(atom)?.map_or(Ok(false), |node| node.is_meta())?,
			EndpointDomain::Edge => graph.edge(atom)?.map_or(Ok(false), |edge| edge.is_meta())?,
		};
		if !is_meta {
			return Ok(false);
		}
	}
	let Some(expected) = binding.label.as_deref() else {
		return Ok(true);
	};
	match binding.domain {
		EndpointDomain::Node => {
			let Some(node) = graph.node(atom)? else {
				return Ok(false);
			};
			Ok(node.label_text()?.is_some_and(|label| label.as_str() == expected))
		}
		EndpointDomain::Edge => {
			let Some(edge) = graph.edge(atom)? else {
				return Ok(false);
			};
			Ok(edge.label_text()?.is_some_and(|label| label.as_str() == expected))
		}
	}
}

pub(in crate::gql::execution) fn bind_atom<Graph: GraphRead>(
	row: &mut Row,
	binding: &ResolvedMatchBinding,
	atom: AtomId,
	graph: &Graph,
	graph_ref: &Arc<GraphRef>,
) -> FerrosGrynnResult<bool> {
	let Some(column) = binding.column else {
		return Ok(true);
	};
	let occurrence = BoundAtom::new(graph_ref.id(), atom);
	if let Some(existing) = row.atoms.get(column).copied().flatten() {
		return Ok(existing == occurrence);
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
	row.values[column] =
		Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(reference, graph_ref.clone())));
	row.atoms[column] = Some(occurrence);
	Ok(true)
}
