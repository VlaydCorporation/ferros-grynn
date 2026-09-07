#[allow(
	clippy::wildcard_imports,
	reason = "the facade owns imports during the move-only split"
)]
use super::*;

#[allow(
	clippy::multiple_inherent_impl,
	reason = "transaction responsibilities are split into cohesive modules"
)]
impl Transaction {
	/// Opens a graph from the exact catalog root captured for the query.
	pub(crate) fn query_graph<'tx>(
		&'tx self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<TransactionGraphView<'tx>> {
		self.graph_view_at(snapshot.root.clone(), reference)
	}

	/// Builds a graph view without refreshing the transaction snapshot.
	fn graph_view_at<'tx>(
		&'tx self,
		root: Arc<DatabaseRoot>,
		reference: &GraphRef,
	) -> FerrosGrynnResult<TransactionGraphView<'tx>> {
		self.ensure_live()?;
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		self.record_read(&root, ReadKey::GraphCatalog(reference.id().clone()));
		let graph = if let Some(write) = self.graphs.get(reference.id()) {
			let graph = Arc::new(write.snapshot(&self.store)?);
			TransactionGraphSource::Materialized(graph)
		} else {
			let stored = root.graphs.get(reference.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.id().to_string())
			})?;
			match &stored.graph {
				StoredGraphValue::Materialized(graph) => {
					TransactionGraphSource::Materialized(graph.clone())
				}
				StoredGraphValue::Local(graph) => {
					let local = self.store.inner.backend.local().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"local graph is attached to an in-memory backend",
						)
					})?;
					TransactionGraphSource::Local(
						local.graph_view(self.local_snapshot()?, graph.clone()),
					)
				}
			}
		};
		if graph.kind() != reference.kind() {
			return Err(FerrosGrynnError::conversion_error(
				reference.kind().name(),
				graph.kind().name(),
			));
		}
		Ok(TransactionGraphView {
			graph,
			reference: reference.clone(),
			snapshot: SnapshotId::new(root.commit_ts, self.id),
			root,
			reads: &self.reads,
		})
	}

	/// Materializes an owned general graph through the bounded local path.
	///
	/// The result owns every materialized atom and releases local page state
	/// before returning.
	#[doc(hidden)]
	pub fn materialize_query_graph(
		&self,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Graph<Frozen>> {
		let view = self.graph_view_at(self.read_root(), reference)?;
		let TransactionGraphSource::Local(local) = &view.graph else {
			return crate::graph::query_to_memory(&view);
		};
		view.record(ReadKey::GraphProperties(reference.id().clone()));
		view.record(ReadKey::NodeSet(reference.id().clone()));
		view.record(ReadKey::EdgeSet(reference.id().clone()));
		let (mut graph, source_atoms) = local.materialize_query_graph(|atom| {
			view.record(ReadKey::Atom(reference.id().clone(), atom));
		})?;
		graph.finish_query_materialization(&view, source_atoms);
		Ok(graph.freeze())
	}
}

impl GraphRefReader for Transaction {
	type View<'tx>
		= TransactionGraphView<'tx>
	where
		Self: 'tx;

	fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>> {
		let root = self.read_root();
		self.graph_view_at(root, reference)
	}
}

enum TransactionGraphSource<'tx> {
	Materialized(Arc<MaterializedGraph>),
	Local(LocalGraphView<'tx>),
}

impl TransactionGraphSource<'_> {
	fn kind(&self) -> GraphKind {
		match self {
			Self::Materialized(graph) => graph.kind(),
			Self::Local(graph) => graph.kind(),
		}
	}
}

/// Snapshot-bound graph view that preserves the selected storage backend.
pub struct TransactionGraphView<'tx> {
	graph: TransactionGraphSource<'tx>,
	reference: GraphRef,
	snapshot: SnapshotId,
	root: Arc<DatabaseRoot>,
	reads: &'tx Mutex<BTreeMap<ReadKey, u64>>,
}

/// Backend-neutral node read returned by a transaction graph view.
#[non_exhaustive]
#[expect(
	clippy::large_enum_variant,
	reason = "boxing a page-guard-backed node would allocate on every point read"
)]
pub enum TransactionNodeRead<'tx> {
	/// Borrowed projection from an in-memory graph.
	Memory(NodeView<'tx>),
	/// Page-guard-backed projection from a local graph.
	Local(LocalNodeRead<'tx>),
}

impl NodeRead for TransactionNodeRead<'_> {
	fn id(&self) -> AtomId {
		match self {
			Self::Memory(node) => node.id(),
			Self::Local(node) => node.id(),
		}
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		match self {
			Self::Memory(node) => NodeRead::record_id(node),
			Self::Local(node) => node.record_id(),
		}
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		match self {
			Self::Memory(node) => NodeRead::label_text(node),
			Self::Local(node) => node.label_text(),
		}
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		match self {
			Self::Memory(node) => NodeRead::properties(node),
			Self::Local(node) => node.properties(),
		}
	}

	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		match self {
			Self::Memory(node) => NodeRead::inner_graph_ref(node),
			Self::Local(node) => node.inner_graph_ref(),
		}
	}

	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		match self {
			Self::Memory(node) => NodeRead::inner_graph(node),
			Self::Local(node) => node.inner_graph(),
		}
	}
}

/// Backend-neutral edge read returned by a transaction graph view.
#[non_exhaustive]
#[expect(
	clippy::large_enum_variant,
	reason = "boxing a page-guard-backed edge would allocate on every point read"
)]
pub enum TransactionEdgeRead<'tx> {
	/// Borrowed projection from an in-memory graph.
	Memory(EdgeView<'tx>),
	/// Page-guard-backed projection from a local graph.
	Local(LocalEdgeRead<'tx>),
}

impl EdgeRead for TransactionEdgeRead<'_> {
	fn id(&self) -> AtomId {
		match self {
			Self::Memory(edge) => edge.id(),
			Self::Local(edge) => edge.id(),
		}
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		match self {
			Self::Memory(edge) => EdgeRead::record_id(edge),
			Self::Local(edge) => edge.record_id(),
		}
	}

	fn topology(&self) -> FerrosGrynnResult<Cow<'_, crate::values::runtime::graph::EdgeTopology>> {
		match self {
			Self::Memory(edge) => EdgeRead::topology(edge),
			Self::Local(edge) => edge.topology(),
		}
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		match self {
			Self::Memory(edge) => EdgeRead::label_text(edge),
			Self::Local(edge) => edge.label_text(),
		}
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		match self {
			Self::Memory(edge) => EdgeRead::properties(edge),
			Self::Local(edge) => edge.properties(),
		}
	}

	fn weight(&self) -> Option<crate::values::runtime::Number> {
		match self {
			Self::Memory(edge) => edge.weight(),
			Self::Local(edge) => edge.weight(),
		}
	}

	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		match self {
			Self::Memory(edge) => EdgeRead::transition_graph_ref(edge),
			Self::Local(edge) => edge.transition_graph_ref(),
		}
	}

	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		match self {
			Self::Memory(edge) => EdgeRead::transition_graph(edge),
			Self::Local(edge) => edge.transition_graph(),
		}
	}
}

impl TransactionGraphView<'_> {
	/// Returns complete planner statistics for the selected backend snapshot.
	pub(crate) fn statistics(&self) -> FerrosGrynnResult<GraphStatistics> {
		match &self.graph {
			TransactionGraphSource::Materialized(_) => {
				self.root.statistics.graph(self.reference.id()).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph statistics are absent from the MVCC root",
					)
				})
			}
			TransactionGraphSource::Local(graph) => {
				let mut statistics = (*graph.statistics()?).clone();
				statistics.tables = self
					.root
					.statistics
					.graph(self.reference.id())
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"graph statistics are absent from the MVCC root",
						)
					})?
					.tables
					.clone();
				Ok(statistics)
			}
		}
	}

	fn record(&self, key: ReadKey) {
		let version = self.root.version(&key);
		self.reads.lock().entry(key).or_insert(version);
	}
}

impl GraphRead for TransactionGraphView<'_> {
	type Edge<'a>
		= TransactionEdgeRead<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;
	type Node<'a>
		= TransactionNodeRead<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => graph.identity(),
			TransactionGraphSource::Local(graph) => graph.identity(),
		}
	}

	fn revision(&self) -> u64 {
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => graph.revision(),
			TransactionGraphSource::Local(graph) => graph.revision(),
		}
	}

	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.record(ReadKey::GraphProperties(self.reference.id().clone()));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => Ok(Cow::Borrowed(graph.properties())),
			TransactionGraphSource::Local(graph) => graph.properties(),
		}
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.record(ReadKey::NodeSet(self.reference.id().clone()));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(Box::new(GraphRead::node_ids(graph.as_ref())?))
			}
			TransactionGraphSource::Local(graph) => Ok(Box::new(graph.node_ids()?)),
		}
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.record(ReadKey::EdgeSet(self.reference.id().clone()));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(Box::new(GraphRead::edge_ids(graph.as_ref())?))
			}
			TransactionGraphSource::Local(graph) => Ok(Box::new(graph.edge_ids()?)),
		}
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		self.record(ReadKey::Atom(self.reference.id().clone(), id));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(graph.node(id).map(TransactionNodeRead::Memory))
			}
			TransactionGraphSource::Local(graph) => {
				Ok(graph.node(id)?.map(TransactionNodeRead::Local))
			}
		}
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		self.record(ReadKey::Atom(self.reference.id().clone(), id));
		match &self.graph {
			TransactionGraphSource::Materialized(graph) => {
				Ok(graph.edge(id).map(TransactionEdgeRead::Memory))
			}
			TransactionGraphSource::Local(graph) => {
				Ok(graph.edge(id)?.map(TransactionEdgeRead::Local))
			}
		}
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.reference)
	}
}
