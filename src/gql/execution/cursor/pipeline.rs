//! Row sources, field resolvers, and relational cursor adapters.

use super::*;

pub(in crate::gql::execution) struct ScanCursor<'a> {
	pub(in crate::gql::execution) inner: Box<dyn RowCursor + 'a>,
}

impl RowCursor for ScanCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		self.inner.next()
	}
}

pub(in crate::gql::execution) struct OneRowCursor {
	pub(in crate::gql::execution) row: Option<Row>,
}

impl RowCursor for OneRowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.row.take())
	}
}

/// Resolves public fields for an atom bound in a MATCH row.
pub(in crate::gql::execution) trait AtomFieldRead {
	/// Reads `id`, `label`, or a dictionary-backed property.
	fn field(&self, atom: AtomId, field: &str) -> FerrosGrynnResult<Value>;

	/// Reads a field through a stable database record reference.
	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value>;
}

/// Record-reference resolver bound to one statement snapshot and graph scope.
pub(in crate::gql::execution) struct RecordFields<'a, Read> {
	pub(in crate::gql::execution) read: &'a Read,
	pub(in crate::gql::execution) snapshot: Snapshot,
	pub(in crate::gql::execution) graph: Option<&'a GraphRef>,
}

impl<Read: QueryRead> AtomFieldRead for RecordFields<'_, Read> {
	fn field(&self, atom: AtomId, _field: &str) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::invalid_argument(format!(
			"SELECT row has no graph-local atom binding for {atom}."
		)))
	}

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		self.read.record_field(self.snapshot, record, self.graph, field)
	}
}

/// Atom field resolver sharing the current graph and property dictionary.
pub(in crate::gql::execution) struct GraphFields<'a, Read, Graph> {
	/// Snapshot graph view.
	pub(in crate::gql::execution) graph: Rc<Graph>,
	/// Snapshot property dictionary.
	pub(in crate::gql::execution) dictionary: Arc<PropertyDictionary>,
	/// Resolver for stable record references found in graph properties.
	pub(in crate::gql::execution) records: RecordFields<'a, Read>,
}

impl<Read: QueryRead, Graph: GraphRead> AtomFieldRead for GraphFields<'_, Read, Graph> {
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

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		self.records.record_field(record, field)
	}
}

pub(in crate::gql::execution) struct FilterCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) predicate: &'a TypedExpr,
	pub(in crate::gql::execution) slots: &'a [Value],
	pub(in crate::gql::execution) source: &'a str,
	/// Optional MATCH atom-field resolver.
	pub(in crate::gql::execution) fields: Option<&'a dyn AtomFieldRead>,
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

pub(in crate::gql::execution) struct ProjectCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) expressions: &'a [TypedExpr],
	pub(in crate::gql::execution) slots: &'a [Value],
	pub(in crate::gql::execution) source: &'a str,
	/// Optional MATCH atom-field resolver.
	pub(in crate::gql::execution) fields: Option<&'a dyn AtomFieldRead>,
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

pub(in crate::gql::execution) struct LimitCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) remaining: Option<usize>,
	pub(in crate::gql::execution) skip: usize,
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
