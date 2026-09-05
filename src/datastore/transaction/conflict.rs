#[allow(
	clippy::wildcard_imports,
	reason = "the facade owns imports during the move-only split"
)]
use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReadKey {
	GraphCatalog(RecordId),
	GraphProperties(RecordId),
	NodeSet(RecordId),
	EdgeSet(RecordId),
	Atom(RecordId, AtomId),
	Table(String),
}

pub(in crate::datastore) struct PredicateRead {
	pub(super) table: String,
	pub(super) root: Arc<DatabaseRoot>,
	pub(super) matcher: PredicateMatcher,
}

pub(super) enum PredicateMatcher {
	Query(Arc<dyn PredicateEvaluator>),
	Record(RecordId),
}

impl PredicateMatcher {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		match self {
			Self::Query(evaluator) => evaluator.matches(row),
			Self::Record(record) => Ok(record == &row.record_id),
		}
	}
}

#[allow(
	clippy::multiple_inherent_impl,
	reason = "transaction responsibilities are split into cohesive modules"
)]
impl Transaction {
	pub(super) fn record_read(&self, root: &DatabaseRoot, key: ReadKey) {
		if self.options.access == TransactionAccess::ReadOnly {
			return;
		}
		let version = root.version(&key);
		self.reads.lock().entry(key).or_insert(version);
	}

	pub(super) fn validate_serializable(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		// ponytail: conservative SSI aborts every outgoing rw-antidependency.
		// It is serializable; retain dangerous-structure history only if
		// measured abort rates justify the machinery.
		for (key, observed) in self.reads.lock().iter() {
			if current.version(key) > *observed {
				return Err(FerrosGrynnError::serialization_failure(
					self.id,
					"a value read from the snapshot was concurrently changed",
				));
			}
		}
		for predicate in self.predicates.lock().iter() {
			self.validate_predicate(current, predicate)?;
		}
		Ok(())
	}

	fn validate_predicate(
		&self,
		current: &DatabaseRoot,
		predicate: &PredicateRead,
	) -> FerrosGrynnResult<()> {
		let schema = match self.tables.get(&predicate.table) {
			Some(Some(schema)) => schema.clone(),
			Some(None) => return Ok(()),
			None => current
				.tables
				.get(&predicate.table)
				.map(|schema| schema.as_ref().clone())
				.ok_or_else(|| FerrosGrynnError::table_does_not_exist(&predicate.table))?,
		};
		let Some(stored) = current.graphs.get(schema.graph.id()) else {
			return Ok(());
		};
		let changed = stored
			.changes
			.iter()
			.filter_map(|(timestamp, atom)| {
				(*timestamp > predicate.root.commit_ts).then_some(*atom)
			})
			.collect::<BTreeSet<_>>();
		for atom in changed {
			let before = self.predicate_row(&predicate.root, &schema, atom)?;
			let after = self.predicate_row(current, &schema, atom)?;
			let before_matches = before
				.as_ref()
				.map(|row| predicate.matcher.matches(row))
				.transpose()?
				.unwrap_or(false);
			let after_matches = after
				.as_ref()
				.map(|row| predicate.matcher.matches(row))
				.transpose()?
				.unwrap_or(false);
			if before_matches != after_matches {
				return Err(FerrosGrynnError::serialization_failure(
					self.id,
					"a row entered or left a previously evaluated predicate",
				));
			}
		}
		Ok(())
	}

	fn predicate_row(
		&self,
		root: &DatabaseRoot,
		schema: &TableSchema,
		atom: AtomId,
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		if let Some(write) = self.graphs.get(schema.graph.id()) {
			if let Some(local) = &write.local
				&& let Some((record, node)) = local.delta.created_node(atom)
			{
				return Ok((record.table() == schema.name.as_str()
					&& matches!(schema.kind, TableKind::Any | TableKind::Normal))
				.then(|| row_from_properties(atom, record, &node.properties, schema)));
			}
			let dirty = write
				.value
				.as_ref()
				.is_some_and(|value| value.dirty().contains(&DirtyTarget::Atom(atom)));
			if dirty {
				return Ok(write
					.value
					.as_ref()
					.and_then(|value| materialize_atom(&value.snapshot(), schema, atom)));
			}
		}
		let Some(stored) = root.graphs.get(schema.graph.id()) else {
			return Ok(None);
		};
		match &stored.graph {
			StoredGraphValue::Materialized(graph) => Ok(materialize_atom(graph, schema, atom)),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				let transaction = self.local_snapshot()?;
				let view = local.graph_view(transaction, graph.clone());
				let row = match atom.kind() {
					Some(AtomKind::Node)
						if matches!(schema.kind, TableKind::Any | TableKind::Normal) =>
					{
						view.node(atom)?
							.map(|node| {
								let record = node.record_id()?.into_owned();
								let properties = node.properties()?.into_owned();
								Ok::<_, FerrosGrynnError>((record, properties))
							})
							.transpose()?
					}
					Some(AtomKind::Edge)
						if matches!(schema.kind, TableKind::Any | TableKind::Relation) =>
					{
						view.edge(atom)?
							.map(|edge| {
								let record = edge.record_id()?.into_owned();
								let properties = edge.properties()?.into_owned();
								Ok::<_, FerrosGrynnError>((record, properties))
							})
							.transpose()?
					}
					_ => None,
				};
				Ok(row.and_then(|(record, properties)| {
					(record.table() == schema.name.as_str())
						.then(|| row_from_properties(atom, &record, &properties, schema))
				}))
			}
		}
	}

	pub(super) fn validate_writes(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		for (id, write) in &self.graphs {
			if let Some(local) = &write.local {
				let stored = current
					.graphs
					.get(id)
					.ok_or_else(|| self.write_conflict("the graph was concurrently removed"))?;
				let StoredGraphValue::Local(latest) = &stored.graph else {
					return Err(self.write_conflict("the graph storage representation changed"));
				};
				if latest.identity() != local.base.identity()
					|| latest.node_capacity() != local.base.node_capacity()
				{
					return Err(
						self.write_conflict("the graph slot layout was concurrently changed")
					);
				}
				for target in local.delta.dirty() {
					let DirtyTarget::Atom(atom) = target else {
						continue;
					};
					if stored.atom_versions.get(atom) > write.base_ts {
						return Err(
							self.write_conflict("the same graph atom was concurrently changed")
						);
					}
				}
				if local.base.kind() != stored.graph.kind() {
					return Err(self.write_conflict("the graph family was concurrently changed"));
				}
				continue;
			}
			match (&write.base, &write.value) {
				(None, Some(_)) => {
					if current.graphs.contains_key(id)
						|| current.graph_catalog_versions.get(id).copied().unwrap_or(0)
							> write.base_ts
					{
						return Err(self.write_conflict("a graph name was concurrently created"));
					}
					if current.graph_names.get(&write.name).is_some_and(|record_id| {
						record_id != id
							&& !self.graphs.get(record_id).is_some_and(|pending| !pending.exists())
					}) {
						return Err(self.write_conflict("a named graph was concurrently created"));
					}
				}
				(Some(_), None) => {
					let stored = current
						.graphs
						.get(id)
						.ok_or_else(|| self.write_conflict("the graph was concurrently removed"))?;
					if stored.last_commit_ts > write.base_ts {
						return Err(self.write_conflict("the graph was concurrently changed"));
					}
				}
				(Some(base), Some(value)) => {
					let stored = current
						.graphs
						.get(id)
						.ok_or_else(|| self.write_conflict("the graph was concurrently removed"))?;
					for target in value.dirty() {
						let version = match target {
							DirtyTarget::Graph => stored.properties_version,
							DirtyTarget::Atom(atom) => stored.atom_versions.get(atom),
						};
						if version > write.base_ts {
							return Err(
								self.write_conflict("the same graph atom was concurrently changed")
							);
						}
					}
					if base.kind() != stored.graph.kind() {
						return Err(
							self.write_conflict("the graph family was concurrently changed")
						);
					}
				}
				(None, None) => {}
			}
		}
		for name in self.tables.keys() {
			if current.table_versions.get(name).copied().unwrap_or(0) > self.start_ts {
				return Err(self.write_conflict("the same table schema was concurrently changed"));
			}
		}
		Ok(())
	}

	fn write_conflict(&self, reason: &'static str) -> FerrosGrynnError {
		FerrosGrynnError::serialization_failure(self.id, reason)
	}

	pub(super) fn apply_graph_write(
		&self,
		next: &mut DatabaseRoot,
		current: &DatabaseRoot,
		id: RecordId,
		mut write: GraphWrite,
		commit_ts: u64,
		local_commits: &mut Vec<LocalCommitGraph>,
	) -> FerrosGrynnResult<()> {
		if !write.force_persist
			&& let Some(local) = write.local.take()
		{
			if local.delta.is_empty() {
				return Ok(());
			}
			let latest = current
				.graphs
				.get(&id)
				.ok_or_else(|| self.write_conflict("the graph was concurrently removed"))?;
			if let StoredGraphValue::Local(base) = &latest.graph
				&& base.supports_node_append_delta()
			{
				let dirty = local.delta.dirty().collect::<Vec<_>>();
				let nodes = local.delta.into_commit_nodes();
				let mut stored = latest.clone();
				for target in dirty {
					let DirtyTarget::Atom(atom) = target else {
						continue;
					};
					stored.atom_versions.insert(atom, commit_ts);
					stored.changes.push((commit_ts, atom));
				}
				stored.name = write.name.clone();
				stored.node_set_version = commit_ts;
				stored.last_commit_ts = commit_ts;
				next.graph_names.insert(write.name.clone(), id.clone());
				next.graphs.insert(id.clone(), stored);
				local_commits.push(LocalCommitGraph {
					record_id: id,
					name: write.name,
					change: LocalCommitGraphChange::AppendNodes {
						base: base.clone(),
						nodes,
					},
				});
				return Ok(());
			}
			write.local = Some(local);
		}
		write.ensure_materialized(&self.store)?;
		let created = write.is_created();
		let name = write.name.clone();
		let Some(value) = write.value else {
			next.graphs.remove(&id);
			if next.graph_names.get(&name) == Some(&id) {
				next.graph_names.remove(&name);
			}
			next.graph_catalog_versions.insert(id, commit_ts);
			return Ok(());
		};
		let dirty = if write.base.is_some() {
			value.dirty()
		} else {
			all_dirty(&value.snapshot())?
		};
		if dirty.is_empty() && write.base.is_some() && !write.force_persist {
			return Ok(());
		}

		let graph = match (&write.base, current.graphs.get(&id)) {
			(Some(base), Some(latest)) if latest.last_commit_ts > write.base_ts => {
				let latest = self.materialize_stored_graph(&latest.graph)?;
				merge_materialized(self.id, base, value, &latest, &dirty)?
			}
			_ => value.freeze(),
		};
		let graph = Arc::new(graph);
		let mut stored = current.graphs.get(&id).cloned().unwrap_or_else(|| StoredGraph {
			name: name.clone(),
			graph: StoredGraphValue::Materialized(graph.clone()),
			record_index: Arc::new(BTreeMap::new()),
			last_commit_ts: 0,
			properties_version: 0,
			node_set_version: 0,
			edge_set_version: 0,
			atom_versions: AtomVersions::default(),
			changes: CowVec::new(),
		});
		for target in &dirty {
			match target {
				DirtyTarget::Graph => stored.properties_version = commit_ts,
				DirtyTarget::Atom(atom) => {
					stored.atom_versions.insert(*atom, commit_ts);
					stored.changes.push((commit_ts, *atom));
					let before = write.base.as_ref().is_some_and(|base| base.contains(*atom));
					let after = graph.contains(*atom);
					if before != after {
						match atom.kind() {
							Some(AtomKind::Node) => stored.node_set_version = commit_ts,
							Some(AtomKind::Edge) => stored.edge_set_version = commit_ts,
							None => {}
						}
					}
				}
			}
		}
		if created {
			stored.properties_version = commit_ts;
			stored.node_set_version = commit_ts;
			stored.edge_set_version = commit_ts;
		}
		stored.name = name.clone();
		stored.graph = StoredGraphValue::Materialized(graph.clone());
		stored.record_index = Arc::new(build_record_index(&graph)?);
		stored.last_commit_ts = commit_ts;
		next.graph_names.insert(name, id.clone());
		next.graphs.insert(id.clone(), stored);
		if created {
			next.graph_catalog_versions.insert(id, commit_ts);
		}
		Ok(())
	}
}

pub(super) fn all_dirty(graph: &MaterializedGraph) -> FerrosGrynnResult<Vec<DirtyTarget>> {
	let mut dirty = Vec::with_capacity(graph.node_count() + graph.edge_count() + 1);
	dirty.push(DirtyTarget::Graph);
	for atom in GraphRead::node_ids(graph)? {
		dirty.push(DirtyTarget::Atom(atom?));
	}
	for atom in GraphRead::edge_ids(graph)? {
		dirty.push(DirtyTarget::Atom(atom?));
	}
	Ok(dirty)
}

/// Builds the runtime mirror of the durable primary RecordId index.
fn build_record_index(graph: &MaterializedGraph) -> FerrosGrynnResult<BTreeMap<String, AtomId>> {
	let mut index = BTreeMap::new();
	for atom in GraphRead::node_ids(graph)?.chain(GraphRead::edge_ids(graph)?) {
		let atom = atom?;
		let reference = graph.atom_ref(atom)?.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live graph atom has no RecordId")
		})?;
		if index.insert(reference.id().to_string(), atom).is_some() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"record id",
				format!("duplicate RecordId '{}' in one graph", reference.id()),
			));
		}
	}
	Ok(index)
}

fn merge_materialized(
	transaction: u64,
	base: &MaterializedGraph,
	changed: MutableGraphValue,
	latest: &MaterializedGraph,
	dirty: &[DirtyTarget],
) -> FerrosGrynnResult<MaterializedGraph> {
	if changed.kind() != base.kind() || base.kind() != latest.kind() {
		return Err(FerrosGrynnError::serialization_failure(
			transaction,
			"the graph family changed while merging a transaction",
		));
	}
	let base = base.clone().into_general()?;
	let latest = latest.clone().into_general()?;
	match changed {
		MutableGraphValue::Graph(value) => {
			Ok(merge_transaction_graph(&base, value, latest, dirty)?.into())
		}
		MutableGraphValue::Tree(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Tree::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::RootedTree(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(RootedTree::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Forest(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Forest::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Dag(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Dag::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Hypergraph(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph()?, latest, dirty)?;
			Ok(Hypergraph::<Frozen>::try_from(merged)?.into())
		}
		MutableGraphValue::Metagraph(value) => {
			let merged = merge_transaction_graph(&base, value.into_graph(), latest, dirty)?;
			Ok(Metagraph::<Frozen>::try_from(merged)?.into())
		}
	}
}
