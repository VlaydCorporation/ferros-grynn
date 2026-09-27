//! Row sources, field resolvers, and relational cursor adapters.

use super::*;

pub(in crate::gql::execution) struct ScanCursor<'a> {
	pub(in crate::gql::execution) inner: Box<dyn RowCursor + 'a>,
}

impl RowCursor for ScanCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		self.inner.next(control)
	}
}

pub(in crate::gql::execution) struct OneRowCursor {
	pub(in crate::gql::execution) row: Option<Row>,
}

impl RowCursor for OneRowCursor {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		Ok(self.row.take())
	}
}

/// Resolves public fields for an atom bound in a MATCH row.
pub(in crate::gql::execution) trait AtomFieldRead {
	/// Reads `id`, `label`, or a dictionary-backed property.
	fn field(&self, atom: BoundAtom, field: &str) -> FerrosGrynnResult<Value>;

	/// Returns the inner/transition graph owned by one occurrence.
	fn inner_graph(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value>;

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
	fn field(&self, atom: BoundAtom, _field: &str) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::invalid_argument(format!(
			"SELECT row has no graph-local atom binding for {}.",
			atom.atom
		)))
	}

	fn inner_graph(&self, _occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		Err(FerrosGrynnError::invalid_argument("graph::inner requires a MATCH atom occurrence"))
	}

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		self.read.record_field(self.snapshot, record, self.graph, field)
	}
}

/// Atom field resolver with a lazy cache of snapshot-visible graph references.
pub(in crate::gql::execution) struct MatchGraphFields<'a, Read> {
	pub(in crate::gql::execution) read: &'a Read,
	pub(in crate::gql::execution) snapshot: Snapshot,
	/// Snapshot property dictionary.
	pub(in crate::gql::execution) dictionary: Arc<PropertyDictionary>,
	references: RefCell<std::collections::BTreeMap<crate::graph::GraphId, Arc<GraphRef>>>,
}

impl<'a, Read: QueryRead> MatchGraphFields<'a, Read> {
	pub(in crate::gql::execution) fn new(
		read: &'a Read,
		snapshot: Snapshot,
		dictionary: Arc<PropertyDictionary>,
		root: Arc<GraphRef>,
	) -> Self {
		let references = std::collections::BTreeMap::from([(root.id(), root)]);
		Self {
			read,
			snapshot,
			dictionary,
			references: RefCell::new(references),
		}
	}

	pub(in crate::gql::execution) fn register(&self, reference: GraphRef) -> Arc<GraphRef> {
		let graph = reference.id();
		if let Some(reference) = self.references.borrow().get(&graph) {
			return reference.clone();
		}
		let reference = Arc::new(reference);
		self.references.borrow_mut().insert(graph, reference.clone());
		reference
	}

	pub(in crate::gql::execution) fn reference(
		&self,
		graph: crate::graph::GraphId,
	) -> FerrosGrynnResult<Arc<GraphRef>> {
		if let Some(reference) = self.references.borrow().get(&graph) {
			return Ok(reference.clone());
		}
		self.read.graph_reference(self.snapshot, graph).map(|reference| self.register(reference))
	}
}

impl<Read: QueryRead> AtomFieldRead for MatchGraphFields<'_, Read> {
	fn field(&self, atom: BoundAtom, field: &str) -> FerrosGrynnResult<Value> {
		let reference = self.reference(atom.graph)?;
		if field == "graph" {
			return Ok(Value::GraphRef(Box::new(reference.as_ref().clone())));
		}
		let graph = self.read.graph(self.snapshot, &reference)?;
		match atom.atom.kind() {
			Some(AtomKind::Node) => {
				let node = graph.node(atom.atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("node", atom.atom.to_string())
				})?;
				let record = node.record_id()?;
				match field {
					"id" => Ok(Value::Record(Box::new(record.clone().into_owned()))),
					"class" => Ok(Value::from(record.class())),
					"kind" => Ok(Value::from("node")),
					"label" => Ok(node
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					"properties" => Ok(Value::Object(properties_object(
						node.properties()?.as_ref(),
						&self.dictionary,
					)?)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(node.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			Some(AtomKind::Edge) => {
				let edge = graph.edge(atom.atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("edge", atom.atom.to_string())
				})?;
				let record = edge.record_id()?;
				match field {
					"id" => Ok(Value::Record(Box::new(record.clone().into_owned()))),
					"class" => Ok(Value::from(record.class())),
					"kind" => Ok(Value::from("edge")),
					"label" => Ok(edge
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					"properties" => Ok(Value::Object(properties_object(
						edge.properties()?.as_ref(),
						&self.dictionary,
					)?)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(edge.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			None => Err(FerrosGrynnError::atom_does_not_exist("atom", atom.atom.to_string())),
		}
	}

	fn inner_graph(&self, occurrence: &AtomOccurrenceRef) -> FerrosGrynnResult<Value> {
		let owner = self.register(occurrence.graph().clone());
		let graph = self.read.graph(self.snapshot, &owner)?;
		let atom = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				occurrence.atom().kind().to_string(),
				occurrence.atom().id().to_string(),
			)
		})?;
		let nested = match atom.kind() {
			Some(AtomKind::Node) => graph
				.node(atom)?
				.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", atom.to_string()))?
				.inner_graph_ref()?
				.map(|value| value.into_owned()),
			Some(AtomKind::Edge) => graph
				.edge(atom)?
				.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", atom.to_string()))?
				.transition_graph_ref()?
				.map(|value| value.into_owned()),
			None => None,
		};
		Ok(nested.map_or(Value::None, |reference| {
			let reference = self.register(reference);
			Value::GraphRef(Box::new(reference.as_ref().clone()))
		}))
	}

	fn record_field(&self, record: &RecordId, field: &str) -> FerrosGrynnResult<Value> {
		self.read.record_field(self.snapshot, record, None, field)
	}
}

fn properties_object(
	properties: &crate::values::PropertyMap,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<Object> {
	properties
		.iter()
		.map(|(key, value)| {
			dictionary.resolve(key).cloned().map(|name| (name, value.clone())).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"MATCH property key is absent from the property dictionary",
				)
			})
		})
		.collect()
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
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		while let Some(row) = self.input.next(control)? {
			if eval(
				self.predicate,
				Some(&row),
				self.slots,
				self.source,
				self.fields,
				Some(control),
			)?
			.is_truthy()
			{
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
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.input.next(control)? else {
			return Ok(None);
		};
		let values = self
			.expressions
			.iter()
			.map(|expression| {
				eval(expression, Some(&row), self.slots, self.source, self.fields, Some(control))
			})
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
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		while self.skip > 0 {
			if self.input.next(control)?.is_none() {
				return Ok(None);
			}
			self.skip -= 1;
		}
		if self.remaining == Some(0) {
			return Ok(None);
		}
		let row = self.input.next(control)?;
		if row.is_some()
			&& let Some(remaining) = &mut self.remaining
		{
			*remaining -= 1;
		}
		Ok(row)
	}
}
