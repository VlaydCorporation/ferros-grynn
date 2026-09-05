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

/// Backend-neutral bounded row source used by the hybrid integration path.
pub(crate) struct QueryStorageCursor<'store> {
	source: QueryStorageSource<'store>,
}

/// Concrete cursor state without virtual dispatch inside page loops.
enum QueryStorageSource<'store> {
	/// Slot-wise scan of an immutable in-memory graph root.
	Materialized(MaterializedTableCursor),
	/// Bounded primary-leaf scan of one local graph generation.
	Local {
		cursor: LocalTableCursor<'store>,
		schema: Arc<TableSchema>,
	},
	/// Already bounded candidates returned by a scalar index.
	Indexed(std::vec::IntoIter<QueryStorageRow>),
}

/// Allocation-free slot cursor for Mode 1 and materialized graph writes.
struct MaterializedTableCursor {
	graph: Arc<MaterializedGraph>,
	schema: Arc<TableSchema>,
	kind_index: usize,
	next_slot: usize,
}

impl MaterializedTableCursor {
	/// Creates a cursor positioned before the first permitted atom kind.
	const fn new(graph: Arc<MaterializedGraph>, schema: Arc<TableSchema>) -> Self {
		Self {
			graph,
			schema,
			kind_index: 0,
			next_slot: 0,
		}
	}

	/// Returns the next row without materializing the remaining graph.
	fn next_row(&mut self) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		while self.kind_index < 2 {
			let kind = if self.kind_index == 0 {
				AtomKind::Node
			} else {
				AtomKind::Edge
			};
			let allowed = match kind {
				AtomKind::Node => matches!(self.schema.kind, TableKind::Any | TableKind::Normal),
				AtomKind::Edge => matches!(self.schema.kind, TableKind::Any | TableKind::Relation),
			};
			if !allowed {
				self.kind_index += 1;
				self.next_slot = 0;
				continue;
			}
			let capacity = self.graph.slot_capacity(kind);
			while self.next_slot < capacity {
				let slot = self.next_slot;
				self.next_slot += 1;
				let Some(generation) = self.graph.slot_generation(kind, slot) else {
					continue;
				};
				let slot = u64::try_from(slot).map_err(|source| {
					FerrosGrynnError::invalid_storage_file(format!(
						"in-memory atom slot exceeds u64: {source}"
					))
				})?;
				let atom = AtomId::from_parts(kind, slot, generation)?;
				if let Some(row) = materialize_atom(&self.graph, &self.schema, atom) {
					return Ok(Some(row));
				}
			}
			self.kind_index += 1;
			self.next_slot = 0;
		}
		Ok(None)
	}
}

impl QueryStorageCursor<'_> {
	/// Pulls one owned storage row from the selected backend.
	pub(crate) fn next_row(&mut self) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		match &mut self.source {
			QueryStorageSource::Materialized(cursor) => cursor.next_row(),
			QueryStorageSource::Local {
				cursor,
				schema,
			} => cursor.next_atom().map(|atom| {
				atom.map(|atom| {
					row_from_properties(atom.atom_id, &atom.record_id, &atom.properties, schema)
				})
			}),
			QueryStorageSource::Indexed(rows) => Ok(rows.next()),
		}
	}
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
		if let Some((record_id, write)) =
			self.graphs.iter().find(|(_record_id, write)| write.name == name && write.exists())
		{
			let kind = write.kind().ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?;
			self.record_read(&snapshot.root, ReadKey::GraphCatalog(record_id.clone()));
			return Ok(GraphRef::new(
				record_id.clone(),
				kind,
				self.store.inner.graph_storage_mode(),
			));
		}
		if self.graphs.values().any(|write| write.name == name && !write.exists()) {
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

	/// Collects the streaming path only for legacy root characterization tests.
	#[cfg(test)]
	pub(crate) fn query_rows(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		constraints: &[ScalarIndexConstraint],
	) -> FerrosGrynnResult<Vec<QueryStorageRow>> {
		let mut cursor = self.query_row_cursor(snapshot, table, constraints)?;
		let mut rows = Vec::new();
		while let Some(row) = cursor.next_row()? {
			rows.push(row);
		}
		rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
		Ok(rows)
	}

	/// Opens a bounded backend-neutral query row cursor.
	pub(crate) fn query_row_cursor<'store>(
		&'store self,
		snapshot: &QuerySnapshot,
		table: &str,
		constraints: &[ScalarIndexConstraint],
	) -> FerrosGrynnResult<QueryStorageCursor<'store>> {
		self.ensure_live()?;
		let schema = self.query_table(snapshot, table)?;
		self.record_read(&snapshot.root, ReadKey::GraphCatalog(schema.graph.id().clone()));
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			let graph = write.snapshot(&self.store)?;
			return Ok(QueryStorageCursor {
				source: QueryStorageSource::Materialized(MaterializedTableCursor::new(
					Arc::new(graph),
					schema,
				)),
			});
		}
		let stored =
			snapshot.root.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string())
			})?;
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => Ok(QueryStorageCursor {
				source: QueryStorageSource::Materialized(MaterializedTableCursor::new(
					graph.clone(),
					schema,
				)),
			}),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				if let Some(rows) =
					indexed_local_rows(local, &transaction, graph, &schema, constraints)?
				{
					return Ok(QueryStorageCursor {
						source: QueryStorageSource::Indexed(rows.into_iter()),
					});
				}
				Ok(QueryStorageCursor {
					source: QueryStorageSource::Local {
						cursor: LocalTableCursor::open(
							local,
							transaction,
							graph.clone(),
							schema.name.as_str().to_owned(),
							schema.kind,
						)?,
						schema,
					},
				})
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
			if !write.exists() {
				return Ok(None);
			}
			let graph = write.snapshot(&self.store)?;
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

	/// Registers the conservative predicate before a streaming scan starts and
	/// returns the graph identity needed for row-by-row read tracking.
	pub(crate) fn register_streaming_query_predicate(
		&self,
		snapshot: &QuerySnapshot,
		table: &str,
		evaluator: Arc<dyn PredicateEvaluator>,
	) -> FerrosGrynnResult<Option<RecordId>> {
		if self.options.isolation != IsolationLevel::Serializable
			|| self.options.access == TransactionAccess::ReadOnly
		{
			return Ok(None);
		}
		let schema = self.query_table(snapshot, table)?;
		self.predicates.lock().push(PredicateRead {
			table: table.to_owned(),
			root: snapshot.root.clone(),
			matcher: PredicateMatcher::Query(evaluator),
		});
		Ok(Some(schema.graph.id().clone()))
	}

	/// Records one actually pulled atom when it satisfies the registered query
	/// predicate.
	pub(crate) fn register_streamed_query_row(
		&self,
		snapshot: &QuerySnapshot,
		graph: &RecordId,
		row: &QueryStorageRow,
		evaluator: &dyn PredicateEvaluator,
	) -> FerrosGrynnResult<()> {
		if evaluator.matches(row)? {
			self.record_read(&snapshot.root, ReadKey::Atom(graph.clone(), row.atom_id));
		}
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

/// Returns bounded scalar-index candidates or `None` for a physical table
/// scan.
fn indexed_local_rows(
	local: &LocalStore,
	transaction: &Arc<LocalReadTransaction>,
	graph: &Arc<LocalGraph>,
	schema: &Arc<TableSchema>,
	constraints: &[ScalarIndexConstraint],
) -> FerrosGrynnResult<Option<Vec<QueryStorageRow>>> {
	let mut rows = Vec::new();
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
		transaction,
		graph,
		schema.name.as_str(),
		schema.kind,
		&equalities,
		|atom| {
			rows.push(row_from_properties(atom.atom_id, &atom.record_id, &atom.properties, schema));
			Ok(())
		},
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
				transaction,
				graph,
				schema.name.as_str(),
				schema.kind,
				property_key,
				constraint.comparison,
				&constraint.value,
				|atom| {
					rows.push(row_from_properties(
						atom.atom_id,
						&atom.record_id,
						&atom.properties,
						schema,
					));
					Ok(())
				},
			)?;
		}
	}
	if !indexed {
		return Ok(None);
	}
	rows.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
	Ok(Some(rows))
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
