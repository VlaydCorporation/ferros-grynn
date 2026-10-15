//! Runtime MATCH selectors and seed binding.

use super::*;

#[derive(Clone)]
/// Resolved match selector alternatives.
pub(in crate::gql::execution) enum ResolvedMatchSelector {
	/// Any case of resolved match selector.
	Any,
	/// Class case of resolved match selector.
	Class(
		/// Class case of resolved match selector.
		Box<str>,
	),
	/// Record case of resolved match selector.
	Record(
		/// Record case of resolved match selector.
		RecordId,
	),
}

#[derive(Clone)]
/// Resolved match binding state.
pub(in crate::gql::execution) struct ResolvedMatchBinding {
	/// Domain associated with this resolved match binding.
	pub(in crate::gql::execution) domain: EndpointDomain,
	/// Selector associated with this resolved match binding.
	pub(in crate::gql::execution) selector: ResolvedMatchSelector,
	/// Column associated with this resolved match binding.
	pub(in crate::gql::execution) column: Option<usize>,
	/// Label associated with this resolved match binding.
	pub(in crate::gql::execution) label: Option<Box<str>>,
	/// Properties associated with this resolved match binding.
	pub(in crate::gql::execution) properties: Vec<(Box<str>, TypedExpr)>,
	/// Predicate associated with this resolved match binding.
	pub(in crate::gql::execution) predicate: Option<TypedExpr>,
	/// Meta associated with this resolved match binding.
	pub(in crate::gql::execution) meta: bool,
}

/// Statement-local evaluator used by inline MATCH constraints.
#[derive(Clone, Copy)]
pub(in crate::gql::execution) struct MatchPredicateContext<'a> {
	/// Slots associated with this match predicate context.
	pub(in crate::gql::execution) slots: &'a [Value],
	/// Source associated with this match predicate context.
	pub(in crate::gql::execution) source: &'a str,
	/// Fields associated with this match predicate context.
	pub(in crate::gql::execution) fields: &'a dyn AtomFieldRead,
}

#[derive(Clone)]
/// Resolved expand state.
pub(in crate::gql::execution) struct ResolvedExpand {
	/// Edge associated with this resolved expand.
	pub(in crate::gql::execution) edge: ResolvedMatchBinding,
	/// Target associated with this resolved expand.
	pub(in crate::gql::execution) target: ResolvedMatchBinding,
	/// Direction associated with this resolved expand.
	pub(in crate::gql::execution) direction: crate::syn::MatchDirection,
	/// Mode associated with this resolved expand.
	pub(in crate::gql::execution) mode: crate::syn::MatchMode,
	/// Versioned contribution shared by local and scoped fixed expansion.
	pub(in crate::gql::execution) custom_cost: Option<crate::gql::ir::LogicalPathCost>,
	/// Filters associated with this resolved expand.
	pub(in crate::gql::execution) filters: Vec<crate::syn::MatchPathFilter>,
}

#[derive(Clone)]
/// Resolved path expand state.
pub(in crate::gql::execution) struct ResolvedPathExpand {
	/// Edge associated with this resolved path expand.
	pub(in crate::gql::execution) edge: ResolvedMatchBinding,
	/// Target associated with this resolved path expand.
	pub(in crate::gql::execution) target: ResolvedMatchBinding,
	/// Direction associated with this resolved path expand.
	pub(in crate::gql::execution) direction: crate::syn::MatchDirection,
	/// Min associated with this resolved path expand.
	pub(in crate::gql::execution) min: u32,
	/// Max associated with this resolved path expand.
	pub(in crate::gql::execution) max: u32,
	/// Path column associated with this resolved path expand.
	pub(in crate::gql::execution) path_column: Option<usize>,
	/// Match mode associated with this resolved path expand.
	pub(in crate::gql::execution) match_mode: crate::syn::MatchMode,
	/// Path mode associated with this resolved path expand.
	pub(in crate::gql::execution) path_mode: crate::syn::MatchPathMode,
	/// Objective associated with this resolved path expand.
	pub(in crate::gql::execution) objective: Option<crate::syn::MatchPathObjective>,
	/// Captured cost plan used by every exact path consumer.
	pub(in crate::gql::execution) custom_cost: Option<crate::gql::ir::LogicalPathCost>,
	/// Filters associated with this resolved path expand.
	pub(in crate::gql::execution) filters: Vec<crate::syn::MatchPathFilter>,
}

/// Resolves match binding.
pub(in crate::gql::execution) fn resolve_match_binding(
	binding: &MatchBinding,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<ResolvedMatchBinding> {
	let selector = match &binding.selector {
		LogicalMatchSelector::Any => ResolvedMatchSelector::Any,
		LogicalMatchSelector::Class(class) => ResolvedMatchSelector::Class(class.clone()),
		LogicalMatchSelector::Record(record) => {
			ResolvedMatchSelector::Record(RecordId::from_value(
				record.class.as_ref(),
				eval(
					&record.key,
					None,
					predicate.slots,
					predicate.source,
					Some(predicate.fields),
					Some(control),
				)?,
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

/// Resolves expand.
pub(in crate::gql::execution) fn resolve_expand(
	expand: &Expand,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
	mode: crate::syn::MatchMode,
) -> FerrosGrynnResult<ResolvedExpand> {
	Ok(ResolvedExpand {
		edge: resolve_match_binding(&expand.edge, predicate, control)?,
		target: resolve_match_binding(&expand.target, predicate, control)?,
		direction: expand.direction,
		mode,
		custom_cost: expand.custom_cost,
		filters: expand.filters.clone(),
	})
}

/// Resolves path expand.
pub(in crate::gql::execution) fn resolve_path_expand(
	expand: &PathExpand,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
	match_mode: crate::syn::MatchMode,
	max_depth: u32,
) -> FerrosGrynnResult<ResolvedPathExpand> {
	Ok(ResolvedPathExpand {
		edge: resolve_match_binding(&expand.edge, predicate, control)?,
		target: resolve_match_binding(&expand.target, predicate, control)?,
		direction: expand.direction,
		min: expand.min,
		max: expand.maximum(max_depth),
		path_column: expand.path_column,
		match_mode,
		path_mode: expand.mode,
		objective: expand.objective,
		custom_cost: expand.custom_cost,
		filters: expand.filters.clone(),
	})
}

/// Pull cursor that binds the first endpoint of a pattern.
pub(in crate::gql::execution) struct SeedCursor<'a, Graph> {
	/// Input associated with this seed cursor.
	input: Box<dyn RowCursor + 'a>,
	/// Graph associated with this seed cursor.
	graph: Rc<Graph>,
	/// Graph ref associated with this seed cursor.
	graph_ref: Arc<GraphRef>,
	/// Binding associated with this seed cursor.
	binding: ResolvedMatchBinding,
	/// Mode associated with this seed cursor.
	mode: crate::syn::MatchMode,
	/// Predicate associated with this seed cursor.
	predicate: MatchPredicateContext<'a>,
	/// Ids associated with this seed cursor.
	ids: Vec<AtomId>,
	/// Row associated with this seed cursor.
	row: Option<Row>,
	/// Bound associated with this seed cursor.
	bound: Option<BoundAtom>,
	/// Next id associated with this seed cursor.
	next_id: usize,
}

impl<'a, Graph> SeedCursor<'a, Graph> {
	/// Creates a new `SeedCursor<'a, Graph>` value.
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
	/// Advances this `SeedCursor<'_, Graph>` and returns its next element, if
	/// any.
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

/// Returns the atom matches result.
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

/// Binds atom.
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
