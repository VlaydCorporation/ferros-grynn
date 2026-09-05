#[allow(
	clippy::wildcard_imports,
	reason = "the datastore facade owns imports during the move-only split"
)]
use super::*;

/// Serializable validation and first-committer-wins checks.
mod conflict;
/// Snapshot-bound graph readers.
mod graph_view;
/// Transaction-private graph and schema mutation.
mod mutation;
/// Query snapshots, predicates, and storage rows.
mod query;

pub(crate) use conflict::ReadKey;
use conflict::{PredicateMatcher, PredicateRead, all_dirty};
pub use graph_view::{TransactionEdgeRead, TransactionGraphView, TransactionNodeRead};
pub use mutation::{AtomRefTopology, GraphMutationView, GraphWriteView};
use mutation::{GraphWrite, MutableGraphValue, ensure_store_reference};
pub(crate) use query::{
	PredicateEvaluator,
	QuerySnapshot,
	QueryStorageCursor,
	QueryStorageRow,
	ScalarComparison,
	ScalarIndexConstraint,
};
use query::{materialize_atom, row_from_properties};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
	transaction: u64,
	commit_ts: u64,
}

impl CommitResult {
	pub const fn transaction(self) -> u64 {
		self.transaction
	}

	pub const fn commit_ts(self) -> u64 {
		self.commit_ts
	}
}

pub struct Transaction {
	pub(super) store: Datastore,
	pub(super) id: u64,
	pub(super) start_ts: u64,
	pub(super) started: Instant,
	pub(super) options: TransactionOptions,
	pub(super) root: Arc<DatabaseRoot>,
	pub(super) storage_tx: Mutex<Option<Arc<LocalReadTransaction>>>,
	pub(super) storage_error: Option<String>,
	pub(super) reads: Mutex<BTreeMap<ReadKey, u64>>,
	pub(super) predicates: Mutex<Vec<PredicateRead>>,
	pub(super) graphs: BTreeMap<RecordId, GraphWrite>,
	pub(super) tables: BTreeMap<String, Option<TableSchema>>,
}

impl Drop for Transaction {
	fn drop(&mut self) {
		self.store.inner.active.lock().remove(&self.id);
	}
}

impl Transaction {
	pub const fn id(&self) -> u64 {
		self.id
	}

	pub fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::new(self.read_root().commit_ts, self.id)
	}

	pub const fn options(&self) -> TransactionOptions {
		self.options
	}

	pub fn execute(
		&self,
		source: &str,
		variables: &crate::values::Object,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		self.execute_with_settings(source, variables, crate::gql::ParserSettings::default())
	}

	pub fn execute_with_settings(
		&self,
		source: &str,
		variables: &crate::values::Object,
		settings: crate::gql::ParserSettings,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		crate::gql::execute_transaction(self, source, variables, settings)
	}

	pub fn commit(mut self) -> FerrosGrynnResult<CommitResult> {
		self.ensure_live()?;
		let limits = self.store.inner.transaction_limits();
		let estimated = self.estimated_write_set_bytes()?;
		if estimated > limits.max_write_set_bytes {
			return Err(FerrosGrynnError::transaction_write_set_limit_exceeded(
				self.id,
				u64::try_from(estimated).unwrap_or(u64::MAX),
				u64::try_from(limits.max_write_set_bytes).unwrap_or(u64::MAX),
			));
		}
		if self.options.access == TransactionAccess::ReadOnly
			&& (!self.graphs.is_empty() || !self.tables.is_empty())
		{
			return Err(FerrosGrynnError::read_only_transaction(self.id));
		}

		let _commit = self.store.inner.commit.lock();
		self.ensure_live()?;
		let current = self.store.current_root();
		self.validate_writes(&current)?;
		if self.options.isolation == IsolationLevel::Serializable
			&& self.options.access != TransactionAccess::ReadOnly
		{
			self.validate_serializable(&current)?;
		}
		if self.graphs.is_empty() && self.tables.is_empty() {
			return Ok(CommitResult {
				transaction: self.id,
				commit_ts: current.commit_ts,
			});
		}

		let commit_ts = current.commit_ts.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::serialization_failure(self.id, "the commit clock is exhausted")
		})?;
		// ponytail: cloning the shallow catalog is O(graphs + tables); use a
		// persistent map only after catalog size appears in profiles.
		let mut next = (*current).clone();
		let mut local_commits = Vec::new();
		for (id, write) in std::mem::take(&mut self.graphs) {
			self.apply_graph_write(&mut next, &current, id, write, commit_ts, &mut local_commits)?;
		}
		for (name, schema) in std::mem::take(&mut self.tables) {
			match schema {
				Some(schema) => {
					if !next.graphs.contains_key(schema.graph.id()) {
						return Err(FerrosGrynnError::graph_does_not_exist(
							schema.graph.id().to_string(),
						));
					}
					next.tables.insert(name.clone(), Arc::new(schema));
				}
				None => {
					next.tables.remove(&name);
				}
			}
			next.table_versions.insert(name, commit_ts);
		}
		for schema in next.tables.values() {
			let stored = next.graphs.get(schema.graph.id()).ok_or_else(|| {
				FerrosGrynnError::graph_constraint_violation(
					"catalog",
					format!(
						"table '{}' references missing graph '{}'",
						schema.name,
						schema.graph.id()
					),
				)
			})?;
			if stored.graph.kind() != schema.graph.kind() {
				return Err(FerrosGrynnError::graph_constraint_violation(
					"catalog",
					format!(
						"table '{}' expects graph family '{}' but found '{}'",
						schema.name,
						schema.graph.kind().name(),
						stored.graph.kind().name()
					),
				));
			}
		}
		let mut changed_graphs = next
			.graphs
			.iter()
			.filter(|(id, after)| {
				current
					.graphs
					.get(*id)
					.is_none_or(|before| before.last_commit_ts != after.last_commit_ts)
			})
			.map(|(id, _stored)| id.clone())
			.collect::<Vec<_>>();
		changed_graphs
			.extend(current.graphs.keys().filter(|id| !next.graphs.contains_key(*id)).cloned());
		next.commit_ts = commit_ts;
		if let Some(persisted) =
			self.store.inner.persist_root(commit_ts, &next, &changed_graphs, local_commits)?
		{
			next.dictionary = persisted.property_dictionary;
			for entry in persisted.graphs {
				let stored = next.graphs.get_mut(&entry.record_id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"durable commit returned an unknown graph descriptor",
					)
				})?;
				stored.graph = StoredGraphValue::Local(entry.graph);
				stored.record_index = Arc::new(BTreeMap::new());
			}
		}
		let mut published = self.store.inner.current.write();
		let mut active = self.store.inner.active.lock();
		active.retain(|_, snapshot| snapshot.started.elapsed() <= limits.max_snapshot_age);
		let oldest_snapshot =
			active.values().map(|snapshot| snapshot.start_ts).min().unwrap_or(commit_ts);
		drop(active);
		for stored in next.graphs.values_mut() {
			if stored.changes.len() > 4_096 {
				stored.changes.retain(|(timestamp, _)| *timestamp > oldest_snapshot);
			}
		}
		*published = Arc::new(next);
		Ok(CommitResult {
			transaction: self.id,
			commit_ts,
		})
	}

	pub fn abort(self) {}

	fn ensure_live(&self) -> FerrosGrynnResult<()> {
		if let Some(error) = &self.storage_error {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"failed to register the local storage snapshot: {error}"
			)));
		}
		let limits = self.store.inner.transaction_limits();
		if self.started.elapsed() > limits.max_snapshot_age {
			let transaction = self.storage_tx.lock().take();
			if let Some(transaction) = transaction {
				transaction.cancel();
			}
			return Err(FerrosGrynnError::snapshot_expired(
				self.id,
				limits.max_snapshot_age.as_secs(),
			));
		}
		let transaction = self.storage_tx.lock().as_ref().cloned();
		if let Some(transaction) = transaction {
			transaction.ensure_live()?;
		}
		Ok(())
	}

	fn local_snapshot(&self) -> FerrosGrynnResult<Arc<LocalReadTransaction>> {
		self.ensure_live()?;
		self.storage_tx.lock().as_ref().cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("local transaction has no storage snapshot")
		})
	}

	fn materialize_stored_graph(
		&self,
		graph: &StoredGraphValue,
	) -> FerrosGrynnResult<Arc<MaterializedGraph>> {
		match graph {
			StoredGraphValue::Materialized(graph) => Ok(graph.clone()),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let limits = self.store.inner.transaction_limits();
				let transaction = Arc::new(local.begin_read(self.id, limits.max_snapshot_age)?);
				local.materialize_graph(transaction, graph).map(Arc::new)
			}
		}
	}

	fn read_root(&self) -> Arc<DatabaseRoot> {
		if self.options.isolation == IsolationLevel::ReadCommitted {
			self.store.current_root()
		} else {
			self.root.clone()
		}
	}

	fn visible_graph_kind(&self, id: &RecordId) -> Option<GraphKind> {
		if let Some(write) = self.graphs.get(id) {
			return write.kind();
		}
		self.read_root().graphs.get(id).map(|stored| stored.graph.kind())
	}

	fn visible_table(&self, name: &str) -> Option<Arc<TableSchema>> {
		match self.tables.get(name) {
			Some(Some(schema)) => Some(Arc::new(schema.clone())),
			Some(None) => None,
			None => self.read_root().tables.get(name).cloned(),
		}
	}

	fn estimated_write_set_bytes(&self) -> FerrosGrynnResult<usize> {
		let mut estimated = 0_usize;
		for write in self.graphs.values() {
			if let Some(local) = &write.local {
				estimated = estimated.saturating_add(local.delta.estimated_write_set_bytes());
				continue;
			}
			let Some(value) = write.value.as_ref() else {
				estimated = estimated.saturating_add(CATALOG_DELETE_WRITE_BYTES);
				continue;
			};
			let snapshot = value.snapshot();
			let dirty = if write.base.is_some() {
				value.dirty()
			} else {
				all_dirty(&snapshot)?
			};
			estimated =
				estimated.saturating_add(dirty.len().saturating_mul(DIRTY_TARGET_WRITE_BYTES));
			for target in dirty {
				match target {
					DirtyTarget::Graph => {
						estimated =
							estimated.saturating_add(snapshot.properties().estimated_heap_bytes());
					}
					DirtyTarget::Atom(atom) => match atom.kind() {
						Some(AtomKind::Node) => {
							if let Some(node) = snapshot.node(atom) {
								estimated = estimated
									.saturating_add(node.record_id().estimated_heap_bytes())
									.saturating_add(node.label_text().map_or(0, Text::len))
									.saturating_add(node.properties().estimated_heap_bytes());
								if let Some(inner) = node.inner_graph() {
									estimated = estimated.saturating_add(
										crate::graph::estimated_nested_graph_heap_bytes(inner)?,
									);
								}
							}
						}
						Some(AtomKind::Edge) => {
							if let Some(edge) = snapshot.edge(atom) {
								estimated = estimated
									.saturating_add(edge.record_id().estimated_heap_bytes())
									.saturating_add(edge.label_text().map_or(0, Text::len))
									.saturating_add(edge.properties().estimated_heap_bytes())
									.saturating_add(
										edge.topology()
											.endpoint_count()
											.saturating_mul(std::mem::size_of::<EdgeEndpoint>()),
									);
								if let Some(transition) = edge.transition_graph() {
									estimated = estimated.saturating_add(
										crate::graph::estimated_nested_graph_heap_bytes(transition)?,
									);
								}
							}
						}
						None => {}
					},
				}
			}
		}
		for schema in self.tables.values() {
			estimated = estimated.saturating_add(schema.as_ref().map_or(
				CATALOG_DELETE_WRITE_BYTES,
				|schema| {
					TABLE_SCHEMA_WRITE_BYTES
						.saturating_add(schema.fields.len().saturating_mul(TABLE_FIELD_WRITE_BYTES))
				},
			));
		}
		Ok(estimated)
	}
}
