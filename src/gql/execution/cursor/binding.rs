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
}

#[derive(Clone)]
pub(in crate::gql::execution) struct ResolvedExpand {
	pub(in crate::gql::execution) edge: ResolvedMatchBinding,
	pub(in crate::gql::execution) target: ResolvedMatchBinding,
	pub(in crate::gql::execution) direction: crate::syn::MatchDirection,
}

#[derive(Clone)]
pub(in crate::gql::execution) struct ResolvedPathExpand {
	pub(in crate::gql::execution) edge: ResolvedMatchBinding,
	pub(in crate::gql::execution) target: ResolvedMatchBinding,
	pub(in crate::gql::execution) direction: crate::syn::MatchDirection,
	pub(in crate::gql::execution) min: u32,
	pub(in crate::gql::execution) max: u32,
	pub(in crate::gql::execution) path_column: Option<usize>,
}

pub(in crate::gql::execution) fn resolve_match_binding(
	binding: &MatchBinding,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<ResolvedMatchBinding> {
	let selector = match &binding.selector {
		LogicalMatchSelector::Any => ResolvedMatchSelector::Any,
		LogicalMatchSelector::Class(class) => ResolvedMatchSelector::Class(class.clone()),
		LogicalMatchSelector::Record(record) => {
			ResolvedMatchSelector::Record(RecordId::from_value(
				record.class.as_ref(),
				eval(&record.key, None, slots, source, None)?,
			)?)
		}
	};
	Ok(ResolvedMatchBinding {
		domain: binding.domain,
		selector,
		column: binding.column,
		label: binding.label.clone(),
	})
}

pub(in crate::gql::execution) fn resolve_expand(
	expand: &Expand,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<ResolvedExpand> {
	Ok(ResolvedExpand {
		edge: resolve_match_binding(&expand.edge, slots, source)?,
		target: resolve_match_binding(&expand.target, slots, source)?,
		direction: expand.direction,
	})
}

pub(in crate::gql::execution) fn resolve_path_expand(
	expand: &PathExpand,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<ResolvedPathExpand> {
	Ok(ResolvedPathExpand {
		edge: resolve_match_binding(&expand.edge, slots, source)?,
		target: resolve_match_binding(&expand.target, slots, source)?,
		direction: expand.direction,
		min: expand.min,
		max: expand.max,
		path_column: expand.path_column,
	})
}

/// Pull cursor that binds the first endpoint of a pattern.
pub(in crate::gql::execution) struct SeedCursor<'a, Graph> {
	input: Box<dyn RowCursor + 'a>,
	graph: Rc<Graph>,
	binding: ResolvedMatchBinding,
	ids: Vec<AtomId>,
	row: Option<Row>,
	bound: Option<AtomId>,
	next_id: usize,
}

impl<'a, Graph> SeedCursor<'a, Graph> {
	pub(in crate::gql::execution) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		binding: ResolvedMatchBinding,
		ids: Vec<AtomId>,
	) -> Self {
		Self {
			input,
			graph,
			binding,
			ids,
			row: None,
			bound: None,
			next_id: 0,
		}
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
			let from_bound = self.bound.is_some();
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
			if from_bound && !atom_matches(self.graph.as_ref(), candidate, &self.binding)? {
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
