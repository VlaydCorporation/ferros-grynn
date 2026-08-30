#[allow(
	clippy::wildcard_imports,
	reason = "the facade owns imports during the move-only split"
)]
use super::*;

pub(crate) struct QueryStorageRow {
	pub(crate) record_id: RecordId,
	pub(crate) atom_id: AtomId,
	pub(crate) values: Vec<Value>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScalarComparison {
	/// Exact equality.
	Equal,
	/// Strictly less than.
	Less,
	/// Less than or equal.
	LessOrEqual,
	/// Strictly greater than.
	Greater,
	/// Greater than or equal.
	GreaterOrEqual,
}

/// One scalar predicate that may be satisfied by a durable table index.
#[derive(Debug, Clone)]
pub(crate) struct ScalarIndexConstraint {
	/// Query row column; zero is the synthetic record id.
	pub(crate) column: usize,
	/// Ordered comparison to apply.
	pub(crate) comparison: ScalarComparison,
	/// Runtime scalar encoded into an order-preserving index key.
	pub(crate) value: Value,
}

pub(crate) struct QuerySnapshot {
	pub(super) root: Arc<DatabaseRoot>,
}

impl QuerySnapshot {
	pub(crate) fn commit_ts(&self) -> u64 {
		self.root.commit_ts
	}

	/// Returns the property dictionary captured by this query snapshot.
	pub(crate) fn property_dictionary(&self) -> Arc<PropertyDictionary> {
		self.root.dictionary.clone()
	}
}

pub(crate) trait PredicateEvaluator {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool>;
}

#[allow(
	clippy::multiple_inherent_impl,
	reason = "transaction responsibilities are split into cohesive modules"
)]
impl Transaction {
	pub(crate) fn query_snapshot(&self) -> FerrosGrynnResult<QuerySnapshot> {
		self.ensure_live()?;
		Ok(QuerySnapshot {
			root: self.read_root(),
		})
	}

	pub(crate) fn query_table(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<Arc<TableSchema>> {
		self.ensure_live()?;
		self.record_read(&snapshot.root, ReadKey::Table(name.to_owned()));
		match self.tables.get(name) {
			Some(Some(schema)) => Ok(Arc::new(schema.clone())),
			Some(None) => Err(FerrosGrynnError::table_does_not_exist(name)),
			None => snapshot
				.root
				.tables
				.get(name)
				.cloned()
				.ok_or_else(|| FerrosGrynnError::table_does_not_exist(name)),
		}
	}

	/// Resolves a named graph against transaction-local writes and one
	/// snapshot.
	pub(crate) fn query_named_graph(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<GraphRef> {
		self.ensure_live()?;
		if let Some((record_id, write)) = self
			.graphs
			.iter()
			.find(|(_record_id, write)| write.name == name && write.value.is_some())
		{
			let kind = write
				.value
				.as_ref()
				.map(MutableGraphValue::kind)
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?;
			self.record_read(&snapshot.root, ReadKey::GraphCatalog(record_id.clone()));
			return Ok(GraphRef::new(
				record_id.clone(),
				kind,
				self.store.inner.graph_storage_mode(),
			));
		}
		if self.graphs.values().any(|write| write.name == name && write.value.is_none()) {
			return Err(FerrosGrynnError::graph_does_not_exist(name));
		}
		let record_id = snapshot
			.root
			.graph_names
			.get(name)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?;
		let stored = snapshot.root.graphs.get(record_id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"named graph catalog points to an absent graph record",
			)
		})?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(record_id.clone()));
		Ok(GraphRef::new(
			record_id.clone(),
			stored.graph.kind(),
			self.store.inner.graph_storage_mode(),
		))
	}

	pub(crate) fn query_rows(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		constraints: &[ScalarIndexConstraint],
	) -> FerrosGrynnResult<Vec<QueryStorageRow>> {
		self.ensure_live()?;
		let schema = self.query_table(snapshot, table)?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(schema.graph.id().clone()));
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let graph = write.value.as_ref().map(MutableGraphValue::snapshot).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
			return materialize_rows(&graph, &schema);
		}
		let stored =
			snapshot.root.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => materialize_rows(graph, &schema),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				let mut rows = Vec::new();
				let collect = |atom: LocalQueryAtom, rows: &mut Vec<QueryStorageRow>| {
					rows.push(row_from_properties(
						atom.atom_id,
						&atom.record_id,
						&atom.properties,
						&schema,
					));
					Ok(())
				};
				let equalities = constraints
					.iter()
					.filter(|constraint| matches!(constraint.comparison, ScalarComparison::Equal))
					.filter_map(|constraint| {
						constraint
							.column
							.checked_sub(1)
							.and_then(|column| schema.fields.get(column))
							.and_then(|field| field.key)
							.map(|key| (key, &constraint.value))
					})
					.collect::<Vec<_>>();
				let mut indexed = local.visit_table_composite_index(
					&transaction,
					graph,
					schema.name.as_str(),
					schema.kind,
					&equalities,
					|atom| collect(atom, &mut rows),
				)?;
				for constraint in constraints {
					if indexed {
						break;
					}
					let field = constraint
						.column
						.checked_sub(1)
						.and_then(|column| schema.fields.get(column))
						.and_then(|field| field.key);
					if let Some(property_key) = field {
						indexed = local.visit_table_index(
							&transaction,
							graph,
							schema.name.as_str(),
							schema.kind,
							property_key,
							constraint.comparison,
							&constraint.value,
							|atom| collect(atom, &mut rows),
						)?;
					}
				}
				if !indexed {
					local.visit_table(
						&transaction,
						graph,
						schema.name.as_str(),
						schema.kind,
						|atom| collect(atom, &mut rows),
					)?;
				}
				rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
				Ok(rows)
			}
		}
	}

	/// Resolves one record without scanning the table when the graph comes from
	/// a published snapshot. Transaction-private graph writes use the compact
	/// materialized fallback because they have not built a durable index yet.
	pub(crate) fn query_point(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		self.ensure_live()?;
		let schema = self.query_table(snapshot, record.table())?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(schema.graph.id().clone()));
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let Some(graph) = write.value.as_ref().map(MutableGraphValue::snapshot) else {
				return Ok(None);
			};
			let atom =
				if let Some(atom) = graph.atom_id(&AtomRef::new(AtomKind::Node, record.clone()))? {
					Some(atom)
				} else {
					graph.atom_id(&AtomRef::new(AtomKind::Edge, record.clone()))?
				};
			return Ok(atom.and_then(|atom| materialize_atom(&graph, &schema, atom)));
		}
		let stored =
			snapshot.root.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => Ok(stored
				.record_index
				.get(&record.to_string())
				.copied()
				.and_then(|atom| materialize_atom(graph, &schema, atom))),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				Ok(local.lookup_record(&transaction, graph, record, schema.kind)?.map(|atom| {
					row_from_properties(atom.atom_id, &atom.record_id, &atom.properties, &schema)
				}))
			}
		}
	}

	pub(crate) fn register_query_predicate(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		rows: &[QueryStorageRow],
		evaluator: Arc<dyn PredicateEvaluator>,
	) -> FerrosGrynnResult<()> {
		if self.options.isolation != IsolationLevel::Serializable
			|| self.options.access == TransactionAccess::ReadOnly
		{
			return Ok(());
		}
		let schema = self.query_table(snapshot, table)?;
		for row in rows {
			if evaluator.matches(row)? {
				self.record_read(
					&snapshot.root,
					ReadKey::Atom(schema.graph.id().clone(), row.atom_id),
				);
			}
		}
		self.predicates.lock().push(PredicateRead {
			table: table.to_owned(),
			root: snapshot.root.clone(),
			matcher: PredicateMatcher::Query(evaluator),
		});
		Ok(())
	}

	pub(crate) fn register_query_point(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		record: &RecordId,
		row: Option<&QueryStorageRow>,
	) -> FerrosGrynnResult<()> {
		if self.options.isolation != IsolationLevel::Serializable
			|| self.options.access == TransactionAccess::ReadOnly
		{
			return Ok(());
		}
		let schema = self.query_table(snapshot, table)?;
		if let Some(row) = row {
			self.record_read(&snapshot.root, ReadKey::Atom(schema.graph.id().clone(), row.atom_id));
		}
		self.predicates.lock().push(PredicateRead {
			table: table.to_owned(),
			root: snapshot.root.clone(),
			matcher: PredicateMatcher::Record(record.clone()),
		});
		Ok(())
	}
}

fn materialize_rows(
	graph: &MaterializedGraph,
	schema: &TableSchema,
) -> FerrosGrynnResult<Vec<QueryStorageRow>> {
	let mut rows = Vec::new();
	if matches!(schema.kind, TableKind::Any | TableKind::Normal) {
		for id in GraphRead::node_ids(graph)? {
			let id = id?;
			if let Some(node) = graph.node(id)
				&& node.record_id().table() == schema.name.as_str()
			{
				rows.push(row_from_properties(id, node.record_id(), node.properties(), schema));
			}
		}
	}
	if matches!(schema.kind, TableKind::Any | TableKind::Relation) {
		for id in GraphRead::edge_ids(graph)? {
			let id = id?;
			if let Some(edge) = graph.edge(id)
				&& edge.record_id().table() == schema.name.as_str()
			{
				rows.push(row_from_properties(id, edge.record_id(), edge.properties(), schema));
			}
		}
	}
	rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
	Ok(rows)
}

pub(super) fn materialize_atom(
	graph: &MaterializedGraph,
	schema: &TableSchema,
	atom: AtomId,
) -> Option<QueryStorageRow> {
	match atom.kind()? {
		AtomKind::Node if matches!(schema.kind, TableKind::Any | TableKind::Normal) => {
			let node = graph.node(atom)?;
			if node.record_id().table() != schema.name.as_str() {
				return None;
			}
			Some(row_from_properties(atom, node.record_id(), node.properties(), schema))
		}
		AtomKind::Edge if matches!(schema.kind, TableKind::Any | TableKind::Relation) => {
			let edge = graph.edge(atom)?;
			if edge.record_id().table() != schema.name.as_str() {
				return None;
			}
			Some(row_from_properties(atom, edge.record_id(), edge.properties(), schema))
		}
		AtomKind::Node | AtomKind::Edge => None,
	}
}

pub(super) fn row_from_properties(
	atom_id: AtomId,
	record_id: &RecordId,
	properties: &PropertyMap,
	schema: &TableSchema,
) -> QueryStorageRow {
	let mut values = Vec::with_capacity(schema.fields.len() + 1);
	values.push(Value::Record(Box::new(record_id.clone())));
	values.extend(schema.fields.iter().map(|field| {
		field.key.and_then(|key| properties.get(key)).cloned().unwrap_or(Value::None)
	}));
	QueryStorageRow {
		record_id: record_id.clone(),
		atom_id,
		values,
	}
}
