//! Crash-safe copy-on-write graph generations for the local datastore.

mod commit;
mod gc;
mod graph;
mod materialize;
mod view;

use std::{
	borrow::Cow,
	collections::{BTreeMap, BTreeSet},
	fs,
	num::NonZeroUsize,
	ops::Bound,
	path::{Path, PathBuf},
	sync::Arc,
	time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
pub(crate) use view::LocalTableCursor;
pub use view::{LocalEdgeRead, LocalGraphView, LocalNodeRead};
#[allow(
	clippy::wildcard_imports,
	reason = "move-only module split keeps the existing private store helpers during stage 1"
)]
use {commit::*, gc::*, graph::*, view::*};

use super::{
	catalog::{
		CatalogPath,
		DatabaseCatalog,
		FileCatalog,
		FileCatalogEntry,
		GraphCatalogEntry,
		TableCatalogEntry,
		TableFieldCatalogEntry,
		TableIndexCatalogEntry,
		encode_catalog_update,
	},
	direct::DirectFile,
	file_registry::{DataFileRegistration, DataFileRole},
	graph_index::{
		LabelDictionary,
		PrimaryIndexEntry,
		ScalarIndexEntry,
		ScalarIndexMode,
		build_primary_index,
		build_scalar_index,
		decode_label_dictionary,
		encode_label_dictionary,
		leftmost_primary_leaf,
		lookup_primary_index,
		lookup_scalar_index,
		range_scalar_index,
		scan_primary_index,
		visit_primary_leaf_page,
	},
	graph_pages::{
		CROSS_LEVEL_REFS_PER_PAGE,
		EdgeHotSlot,
		EdgeSlotFlags,
		EdgeTopologyCode,
		EndpointSide,
		GraphStatistics,
		HOT_SLOTS_PER_PAGE,
		LARGE_PROPERTY_CHUNK_BYTES,
		MAX_INLINE_PROPERTY_BYTES,
		NodeHotSlot,
		NodeSlotFlags,
		OVERFLOW_REFS_PER_PAGE,
		PropertyLocation,
		PropertyPageBuilder,
		QualifiedDiskAtomRef,
		SUBGRAPHS_PER_PAGE,
		SubgraphEntry,
		decode_adjacency_overflow,
		decode_cross_level_overflow,
		decode_freelist,
		decode_graph_statistics,
		decode_large_property,
		decode_property_map,
		decode_subgraph_directory,
		encode_adjacency_overflow,
		encode_cross_level_overflow,
		encode_freelist,
		encode_graph_statistics,
		encode_large_property,
		encode_property_map,
		encode_subgraph_directory,
		hot_slot_address,
		new_edge_hot_page,
		new_node_hot_page,
		read_edge_hot_slot,
		read_node_hot_slot,
		read_property_record,
		write_edge_hot_slot,
		write_node_hot_slot,
	},
	hybrid_engine::{MAX_READ_SCOPE_OPERATIONS, ReadIntent},
	local::select_graph_superblock,
	page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
	page_io::PageIoMode,
	page_manager::{PageRead, SyncPageManager},
	superblock::{GraphSuperblock, GraphSuperblockFlags},
	types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn},
	wal::TxHandle,
};
use crate::{
	datastore::{
		CheckpointMode,
		INTERNAL_NESTED_GRAPH_TABLE,
		ScalarComparison,
		TableIndexKind,
		TableKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		PropertyDictionary,
		PropertyKeyId,
		PropertyMap,
		RecordId,
		Text,
		Type,
		Uuid,
		Value,
		graph::{
			AtomId,
			AtomKind,
			AtomRef,
			Dag,
			EdgeEndpoint,
			EdgeRead,
			EdgeTopology,
			EdgeView,
			FallibleAtomIds,
			Forest,
			Frozen,
			Graph,
			GraphKind,
			GraphRead,
			GraphRef,
			GraphStorageMode,
			Hypergraph,
			MaterializedGraph,
			Metagraph,
			Mutable,
			NodeRead,
			NodeView,
			QualifiedAtomId,
			RootedTree,
			SnapshotId,
			Subgraph,
			Tree,
		},
		storage::{
			DiskAtomRef,
			EncodeContext,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
			encode_index_key,
		},
	},
};

/// Files produced for every immutable graph generation.
const GRAPH_FILE_KINDS: [(DataFileKind, &str); 11] = [
	(DataFileKind::GraphSuperblock, "graph.fgb"),
	(DataFileKind::NodesHot, "nodes_hot.fgb"),
	(DataFileKind::EdgesHot, "edges_hot.fgb"),
	(DataFileKind::AdjacencyOverflow, "adjacency_overflow.fgb"),
	(DataFileKind::CrossLevelAdjacencyOverflow, "adj_over_cl.fgb"),
	(DataFileKind::Properties, "properties.fgb"),
	(DataFileKind::Subgraphs, "subgraphs.fgb"),
	(DataFileKind::Labels, "labels.fgb"),
	(DataFileKind::Freelist, "freelist.fgb"),
	(DataFileKind::Statistics, "statistics.fgb"),
	(DataFileKind::Index, "primary.fgidx"),
];

/// One graph supplied by the MVCC layer for a durable commit.
pub struct LocalCommitGraph {
	/// Stable catalog identity of the named graph.
	pub record_id: RecordId,
	/// Case-sensitive user-visible graph name.
	pub name: String,
	/// Immutable MVCC graph image being published.
	pub graph: Arc<MaterializedGraph>,
}

/// Opaque durable graph descriptor retained without materializing its atoms.
pub struct LocalGraph {
	graph_id: GraphId,
	subgraph_slot: Option<u32>,
	parent_atom: Option<DiskAtomRef>,
	reference: GraphRef,
	identity: Uuid,
	revision: u64,
	primary_index_root: PageNo,
	properties: Option<PropertyLocation>,
	superblock: GraphSuperblock,
	files: BTreeMap<DataFileKind, DataFileId>,
	labels: LabelDictionary,
	property_dictionary: Arc<PropertyDictionary>,
	decode_context: Mutex<Option<Arc<EncodeContext>>>,
	scalar_indexes: BTreeMap<String, Vec<LocalScalarIndex>>,
	children: BTreeMap<u32, Arc<LocalGraph>>,
}

#[derive(Clone)]
struct LocalScalarIndex {
	name: String,
	kind: TableIndexKind,
	fields: Vec<(PropertyKeyId, Type)>,
	file_id: DataFileId,
	root: PageNo,
}

impl LocalGraph {
	pub(crate) const fn identity(&self) -> Uuid {
		self.identity
	}

	pub(crate) const fn revision(&self) -> u64 {
		self.revision
	}

	pub(crate) const fn kind(&self) -> GraphKind {
		self.reference.kind()
	}

	pub(crate) const fn reference(&self) -> &GraphRef {
		&self.reference
	}

	fn file_id(&self, kind: DataFileKind) -> FerrosGrynnResult<DataFileId> {
		self.files.get(&kind).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!("graph file {kind:?} is absent"))
		})
	}

	fn page_key(
		&self,
		kind: DataFileKind,
		page_no: PageNo,
	) -> FerrosGrynnResult<super::types::PageKey> {
		Ok(super::types::PageKey::new(self.graph_id, self.file_id(kind)?, page_no))
	}

	fn index_page_key(&self, file_id: DataFileId, page_no: PageNo) -> super::types::PageKey {
		super::types::PageKey::new(self.graph_id, file_id, page_no)
	}

	fn base_context(&self) -> FerrosGrynnResult<EncodeContext> {
		let mut context = EncodeContext::new((*self.property_dictionary).clone());
		for (expected, table) in self.labels.table_names.iter().enumerate() {
			let actual = context.register_table_name(table.clone().into())?;
			if usize::try_from(actual).ok() != Some(expected) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"table dictionary is not dense",
				));
			}
		}
		for (index, zone) in self.labels.time_zones.iter().enumerate() {
			let actual = context.register_time_zone_name(zone.clone().into())?;
			let expected = u32::try_from(index)
				.ok()
				.and_then(|index| index.checked_add(1))
				.ok_or_else(|| FerrosGrynnError::invalid_storage_file("time-zone id overflows"))?;
			if actual != expected {
				return Err(FerrosGrynnError::invalid_storage_file(
					"time-zone dictionary is not dense",
				));
			}
		}
		Ok(context)
	}

	fn decode_context(
		&self,
		store: &LocalStore,
		transaction: &LocalReadTransaction,
	) -> FerrosGrynnResult<Arc<EncodeContext>> {
		let mut cached = self.decode_context.lock();
		if let Some(context) = cached.as_ref() {
			return Ok(context.clone());
		}
		let mut context = self.base_context()?;
		visit_local_primary(store, transaction, self, |key, atom| {
			let record = decode_record_key(key, &context)?;
			context.register_atom(AtomRef::new(atom.kind(), record), atom)
		})?;
		let context = Arc::new(context);
		*cached = Some(context.clone());
		Ok(context)
	}
}

/// One named graph recovered from the durable catalog.
pub struct LocalSnapshotGraph {
	pub record_id: RecordId,
	/// Case-sensitive name recovered from the existing catalog field.
	pub name: String,
	pub graph: Arc<LocalGraph>,
}

/// Storage snapshot registered for the lifetime of one datastore transaction.
pub struct LocalReadTransaction {
	inner: TxHandle,
	transaction_id: u64,
	started: Instant,
	max_snapshot_age: Duration,
}

impl LocalReadTransaction {
	fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::new(self.inner.snapshot().commit_ts().get(), self.inner.id().get())
	}

	pub(crate) fn ensure_live(&self) -> FerrosGrynnResult<()> {
		if self.started.elapsed() >= self.max_snapshot_age {
			self.inner.cancel();
			return Err(FerrosGrynnError::snapshot_expired(
				self.transaction_id,
				self.max_snapshot_age.as_secs(),
			));
		}
		if !self.inner.is_active() {
			return Err(FerrosGrynnError::snapshot_cancelled(self.transaction_id));
		}
		Ok(())
	}

	pub(crate) fn cancel(&self) {
		self.inner.cancel();
	}
}

/// One table-visible atom decoded without retaining the surrounding graph.
pub struct LocalQueryAtom {
	pub atom_id: AtomId,
	pub record_id: RecordId,
	pub properties: PropertyMap,
}

/// One durable table field supplied by the MVCC layer.
#[derive(Clone)]
pub struct LocalCommitTableField {
	/// Schema-visible field name.
	pub name: String,
	/// Runtime type persisted by the catalog codec.
	pub value_type: Type,
	/// Dense database property-key identifier.
	pub property_key: u32,
}

/// One scalar index supplied by the MVCC schema catalog.
#[derive(Clone)]
pub struct LocalCommitTableIndex {
	/// Table-local index name.
	pub name: String,
	/// Standard or unique key contract.
	pub kind: TableIndexKind,
	/// Composite fields as dense property-key identifiers.
	pub fields: Vec<u32>,
}

/// One durable table supplied by the MVCC layer.
#[derive(Clone)]
pub struct LocalCommitTable {
	/// Unique table name.
	pub name: String,
	/// Named graph containing the table atoms.
	pub graph: RecordId,
	/// Node/edge membership class.
	pub kind: TableKind,
	/// Ordered explicit fields after implicit `id`.
	pub fields: Vec<LocalCommitTableField>,
	/// Scalar indexes in definition order.
	pub indexes: Vec<LocalCommitTableIndex>,
}

/// Recovered local state used to seed the existing MVCC root.
pub struct LocalDatabaseSnapshot {
	/// Highest recovered durable logical commit.
	pub commit_ts: u64,
	/// Recovered append-only property dictionary.
	pub property_dictionary: Arc<PropertyDictionary>,
	/// Recovered named graphs.
	pub graphs: Vec<LocalSnapshotGraph>,
	/// Recovered table schemas.
	pub tables: Vec<LocalCommitTable>,
}

/// Durable commit clocks and any property names discovered inside nested
/// canonical values.
pub struct LocalCommitResult {
	/// Durable dictionary including names discovered while encoding values.
	pub property_dictionary: Arc<PropertyDictionary>,
	/// Fresh disk-backed descriptors for graphs changed by this commit.
	pub graphs: Vec<LocalSnapshotGraph>,
}

/// Catalog state changes only after its matching WAL commit is durable.
struct CatalogState {
	database: DatabaseCatalog,
	files: FileCatalog,
}

/// Windows-first local storage composition below `Datastore`.
pub struct LocalStore {
	/// Owned database directory used to resolve catalog-relative paths.
	root: PathBuf,
	/// Sync WAL/home transaction and recovery core.
	manager: SyncPageManager,
	/// Last catalog state made reachable by a durable root switch.
	catalogs: Mutex<CatalogState>,
	/// Unique WAL-only pages that trigger automatic passive maintenance.
	checkpoint_page_threshold: usize,
	/// Maximum interval between passive checkpoint attempts.
	checkpoint_interval: Duration,
	/// Monotonic time of the last checkpoint that advanced home state.
	last_checkpoint: Mutex<Instant>,
}

impl LocalStore {
	/// Creates and owns one new local database.
	pub fn create<PathLike: AsRef<Path>>(
		path: PathLike,
		database_name: &str,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		checkpoint_page_threshold: usize,
		checkpoint_interval: Duration,
	) -> FerrosGrynnResult<Self> {
		let root = path.as_ref().to_path_buf();
		let (manager, database, files) =
			SyncPageManager::create_cataloged(&root, database_name, pool_pages, io_mode)?;
		Ok(Self {
			root,
			manager,
			catalogs: Mutex::new(CatalogState {
				database,
				files,
			}),
			checkpoint_page_threshold,
			checkpoint_interval,
			last_checkpoint: Mutex::new(Instant::now()),
		})
	}

	/// Opens, recovers, and materializes one local database.
	pub fn open<PathLike: AsRef<Path>>(
		path: PathLike,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		checkpoint_page_threshold: usize,
		checkpoint_interval: Duration,
	) -> FerrosGrynnResult<(Self, LocalDatabaseSnapshot)> {
		let root = path.as_ref().to_path_buf();
		let (manager, database, files) =
			SyncPageManager::open_cataloged(&root, pool_pages, io_mode)?;
		let transaction = manager.begin_tx()?;
		let snapshot = load_database(
			&root,
			&manager,
			&transaction,
			manager.current_commit_ts(),
			&database,
			&files,
		)?;
		drop(transaction);
		remove_unreachable_graph_generations(&root, &files)?;
		Ok((
			Self {
				root,
				manager,
				catalogs: Mutex::new(CatalogState {
					database,
					files,
				}),
				checkpoint_page_threshold,
				checkpoint_interval,
				last_checkpoint: Mutex::new(Instant::now()),
			},
			snapshot,
		))
	}

	/// Registers the physical snapshot paired with one datastore transaction.
	pub(crate) fn begin_read(
		&self,
		transaction_id: u64,
		max_snapshot_age: Duration,
	) -> FerrosGrynnResult<LocalReadTransaction> {
		self.manager.begin_tx_with_max_age(max_snapshot_age).map(|inner| LocalReadTransaction {
			inner,
			transaction_id,
			started: Instant::now(),
			max_snapshot_age,
		})
	}

	/// Runs explicit checkpoint maintenance under the selected reader policy.
	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<()> {
		let advanced = match mode {
			CheckpointMode::Passive => self.manager.checkpoint()?,
			CheckpointMode::Restart => self.manager.restart_checkpoint()?,
		};
		if advanced {
			*self.last_checkpoint.lock() = Instant::now();
		}
		Ok(())
	}

	fn checkpoint_if_due(&self) -> FerrosGrynnResult<()> {
		let unbackfilled = self.manager.unbackfilled_page_count();
		if !checkpoint_due(
			unbackfilled,
			self.checkpoint_page_threshold,
			self.last_checkpoint.lock().elapsed(),
			self.checkpoint_interval,
		) {
			return Ok(());
		}
		self.checkpoint(CheckpointMode::Passive)
	}

	/// Binds one durable graph descriptor to an already registered snapshot.
	pub(crate) fn graph_view(
		&self,
		transaction: Arc<LocalReadTransaction>,
		graph: Arc<LocalGraph>,
	) -> LocalGraphView<'_> {
		LocalGraphView {
			store: self,
			transaction,
			graph,
		}
	}

	/// Materializes a durable graph only when a caller explicitly requests a
	/// mutable workspace.
	pub(crate) fn materialize_graph(
		&self,
		transaction: Arc<LocalReadTransaction>,
		graph: &Arc<LocalGraph>,
	) -> FerrosGrynnResult<MaterializedGraph> {
		let view = self.graph_view(transaction, graph.clone());
		let general = Graph::<Mutable>::from_read_preserving(&view)?.freeze();
		let mut materialized: MaterializedGraph = match graph.kind() {
			GraphKind::Graph => general.into(),
			GraphKind::Tree => Tree::try_from(general)?.into(),
			GraphKind::RootedTree => RootedTree::try_from(general)?.into(),
			GraphKind::Forest => Forest::try_from(general)?.into(),
			GraphKind::Dag => Dag::try_from(general)?.into(),
			GraphKind::Hypergraph => Hypergraph::try_from(general)?.into(),
			GraphKind::Metagraph => Metagraph::try_from(general)?.into(),
			GraphKind::Subgraph => Subgraph::from_graph(general).into(),
		};
		for kind in [AtomKind::Node, AtomKind::Edge] {
			visit_local_slots(
				self,
				&view.transaction,
				graph,
				kind,
				|slot, generation, tombstone| {
					if tombstone {
						materialized.restore_free_slot(kind, slot, generation)?;
					}
					Ok(())
				},
			)?;
		}
		Ok(materialized)
	}

	/// Builds a bounded owned query result from one local snapshot.
	pub(crate) fn materialize_query_graph(
		&self,
		view: &LocalGraphView<'_>,
		record_atom: impl FnMut(AtomId),
	) -> FerrosGrynnResult<(Graph<Mutable>, BTreeMap<AtomId, AtomRef>)> {
		materialize::materialize_query_graph(self, view, record_atom)
	}

	/// Visits matching primary-index records in stable RecordId order.
	pub(crate) fn visit_table(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table: &str,
		kind: TableKind,
		mut visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		let context = graph.base_context()?;
		visit_local_primary(self, transaction, graph, |key, disk| {
			let record_id = decode_record_key(key, &context)?;
			if record_id.table() != table || !table_kind_accepts(kind, disk.kind()) {
				return Ok(());
			}
			visit(local_query_atom(self, transaction, graph, record_id, disk)?)
		})
	}

	/// Visits candidates through the widest composite index covered by exact
	/// equality constraints. `false` keeps the caller on its scan fallback.
	pub(crate) fn visit_table_composite_index(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table: &str,
		kind: TableKind,
		equalities: &[(PropertyKeyId, &Value)],
		visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<bool> {
		let Some(index) = graph.scalar_indexes.get(table).and_then(|indexes| {
			indexes
				.iter()
				.filter(|index| {
					index.fields.len() > 1
						&& index.fields.iter().all(|(field, _value_type)| {
							equalities.iter().any(|(key, _)| key == field)
						})
				})
				.max_by_key(|index| index.fields.len())
		}) else {
			return Ok(false);
		};
		let mut context = graph.base_context()?;
		let components = index
			.fields
			.iter()
			.map(|(field, value_type)| {
				equalities
					.iter()
					.find(|(key, _value)| key == field)
					.map(|(_key, value)| (*value, value_type))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"selected composite index has no equality value",
						)
					})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let key = match encode_index_key(&components, &mut context) {
			Ok(key) => key,
			Err(_error) => return Ok(false),
		};
		let mode = match index.kind {
			TableIndexKind::Standard => ScalarIndexMode::Standard,
			TableIndexKind::Unique => ScalarIndexMode::Unique,
		};
		let atoms = lookup_scalar_index(
			|page_no, output| {
				copy_local_index_page(self, transaction, graph, index.file_id, page_no, output)
			},
			index.root,
			mode,
			&key,
		)?;
		self.visit_scalar_index_atoms(transaction, graph, table, kind, &index.name, atoms, visit)?;
		Ok(true)
	}

	/// Visits candidates from a usable single-field scalar index. `false`
	/// means that the caller must perform the normal table scan.
	pub(crate) fn visit_table_index(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table: &str,
		kind: TableKind,
		property_key: PropertyKeyId,
		comparison: ScalarComparison,
		value: &Value,
		visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<bool> {
		let Some(index) = graph.scalar_indexes.get(table).and_then(|indexes| {
			indexes
				.iter()
				.find(|index| index.fields.len() == 1 && index.fields[0].0 == property_key)
		}) else {
			return Ok(false);
		};
		let mut context = graph.base_context()?;
		let key = match encode_index_key(&[(value, &index.fields[0].1)], &mut context) {
			Ok(key) => key,
			Err(_error) => return Ok(false),
		};
		let mode = match index.kind {
			TableIndexKind::Standard => ScalarIndexMode::Standard,
			TableIndexKind::Unique => ScalarIndexMode::Unique,
		};
		let mut read_page = |page_no, output: &mut AlignedPage| {
			copy_local_index_page(self, transaction, graph, index.file_id, page_no, output)
		};
		let atoms = match comparison {
			ScalarComparison::Equal => lookup_scalar_index(&mut read_page, index.root, mode, &key)?,
			ScalarComparison::Less => range_scalar_index(
				&mut read_page,
				index.root,
				mode,
				Bound::Unbounded,
				Bound::Excluded(key.as_slice()),
			)?,
			ScalarComparison::LessOrEqual => range_scalar_index(
				&mut read_page,
				index.root,
				mode,
				Bound::Unbounded,
				Bound::Included(key.as_slice()),
			)?,
			ScalarComparison::Greater => range_scalar_index(
				&mut read_page,
				index.root,
				mode,
				Bound::Excluded(key.as_slice()),
				Bound::Unbounded,
			)?,
			ScalarComparison::GreaterOrEqual => range_scalar_index(
				&mut read_page,
				index.root,
				mode,
				Bound::Included(key.as_slice()),
				Bound::Unbounded,
			)?,
		};

		self.visit_scalar_index_atoms(transaction, graph, table, kind, &index.name, atoms, visit)?;
		Ok(true)
	}

	fn visit_scalar_index_atoms(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table: &str,
		kind: TableKind,
		index_name: &str,
		atoms: Vec<DiskAtomRef>,
		mut visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		let context = graph.decode_context(self, transaction)?;
		let mut seen = BTreeSet::new();
		for atom in atoms {
			if !seen.insert(atom) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"scalar index contains duplicate atom references",
				));
			}
			let reference = context.registered_atom(atom).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' references an atom absent from the primary index",
					table, index_name
				))
			})?;
			let record = reference.id();
			if record.table() != table || !table_kind_accepts(kind, atom.kind()) {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' references an atom outside its table",
					table, index_name
				)));
			}
			visit(local_query_atom(self, transaction, graph, record.clone(), atom)?)?;
		}
		Ok(())
	}

	/// Resolves one table record through the logarithmic primary lookup.
	pub(crate) fn lookup_record(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		record_id: &RecordId,
		kind: TableKind,
	) -> FerrosGrynnResult<Option<LocalQueryAtom>> {
		let mut context = graph.base_context()?;
		let key = encode_record_key(record_id, &mut context)?;
		let disk = lookup_primary_index(
			|page_no, output| {
				copy_local_page(self, transaction, graph, DataFileKind::Index, page_no, output)
			},
			graph.primary_index_root,
			&key,
		)?;
		let Some(disk) = disk.filter(|disk| table_kind_accepts(kind, disk.kind())) else {
			return Ok(None);
		};
		local_query_atom(self, transaction, graph, record_id.clone(), disk).map(Some)
	}

	/// Publishes one MVCC root by switching only the two fixed catalog heads in
	/// the frame WAL after every new graph file is durable.
	pub fn commit(
		&self,
		expected_commit_ts: u64,
		property_dictionary: &PropertyDictionary,
		graphs: &[LocalCommitGraph],
		tables: &[LocalCommitTable],
		changed_graphs: &[RecordId],
	) -> FerrosGrynnResult<LocalCommitResult> {
		self.checkpoint_if_due()?;
		let mut catalogs = self.catalogs.lock();
		if self.manager.current_commit_ts().checked_add(1) != Some(expected_commit_ts) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"MVCC and frame-WAL commit clocks diverged",
			));
		}
		let previous_database = catalogs.database.clone();
		let mut database = previous_database.clone();
		let mut files = catalogs.files.clone();
		let mut dictionary = database_property_dictionary(&database)?;
		for (expected_id, name) in property_dictionary.iter() {
			let actual_id = dictionary.intern(name.clone())?;
			if actual_id != expected_id {
				return Err(FerrosGrynnError::invalid_storage_file(
					"property dictionary changed an existing durable key id",
				));
			}
		}

		let mut registrations = Vec::new();
		let mut new_superblocks = Vec::new();
		let mut new_indexes = Vec::new();
		let mut transaction = self.manager.begin_tx()?;
		for record_id in changed_graphs {
			let record_text = record_id.to_string();
			let existing = database.graphs.iter().position(|entry| {
				entry.parent_graph_id.is_none() && entry.record_id == record_text
			});
			// ponytail: changed-graph lookup is linear in the catalog snapshot;
			// add a temporary string-keyed map only if bulk multi-graph commits
			// make this visible in profiles.
			let Some(commit_graph) = graphs.iter().find(|entry| &entry.record_id == record_id)
			else {
				if let Some(index) = existing {
					let graph_id = database.graphs[index].graph_id;
					let removed = graph_tree_ids(&database, graph_id);
					database.graphs.retain(|entry| !removed.contains(&entry.graph_id));
					files.files.retain(|entry| !removed.contains(&entry.graph_id));
				}
				continue;
			};
			let graph_id = match existing {
				Some(index) => database.graphs[index].graph_id,
				None => allocate_graph_id(&mut database)?,
			};
			let plan = plan_graph_tree(
				&mut database,
				graph_id,
				record_id,
				&commit_graph.name,
				&commit_graph.graph,
			)?;
			let mut images = build_graph_images(&plan, &mut dictionary, tables)?;
			let in_place = if images.len() == 1
				&& let Some(index) = existing
			{
				prepare_in_place_image(&self.root, &database.graphs[index], &files, &mut images[0])?
			} else {
				false
			};
			if in_place {
				let image = images.pop().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("planned graph image is absent")
				})?;
				stage_graph_image(&self.manager, &mut transaction, &files, &image)?;
				new_superblocks.push(image.superblock.clone());
				if let Some(index) = existing {
					database.graphs[index] = image.catalog_entry;
				}
			} else {
				let removed =
					existing.map(|_index| graph_tree_ids(&database, graph_id)).unwrap_or_default();
				files.files.retain(|entry| !removed.contains(&entry.graph_id));
				database.graphs.retain(|entry| {
					entry.graph_id == graph_id || !removed.contains(&entry.graph_id)
				});
				for image in images {
					let generation = write_new_graph_generation(
						&self.root,
						image,
						&mut files.next_file_instance_id,
					)?;
					files.files.extend(generation.file_entries);
					registrations.extend(generation.registrations);
					new_superblocks.push(generation.superblock);
					new_indexes.extend(generation.indexes);
					if generation.catalog_entry.graph_id == graph_id {
						if let Some(index) =
							database.graphs.iter().position(|entry| entry.graph_id == graph_id)
						{
							database.graphs[index] = generation.catalog_entry;
						} else {
							database.graphs.push(generation.catalog_entry);
						}
					} else {
						database.graphs.push(generation.catalog_entry);
					}
				}
			}
		}
		database.graphs.sort_unstable_by_key(|entry| entry.graph_id);
		files.files.sort_unstable_by_key(|entry| (entry.graph_id, entry.file_id));
		database.property_keys =
			dictionary.iter().map(|(_id, name)| name.as_str().to_owned()).collect();
		database.tables = encode_tables(tables, &database, &previous_database, &new_indexes)?;

		let graph_count = u32::try_from(database.graphs.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("durable graph count exceeds u32")
		})?;
		let (database_root, file_root, first_free_page) = self.manager.catalog_allocation()?;
		let update =
			encode_catalog_update(&database, &files, database_root, file_root, first_free_page)?;
		let registration_keys = registrations
			.iter()
			.map(|registration| (registration.graph_id, registration.file_id))
			.collect::<Vec<_>>();
		if let Err(error) = self.manager.register_files(registrations) {
			self.manager.abort_tx(transaction);
			return Err(error);
		}
		if let Err(error) = self.manager.stage_catalog_update(&mut transaction, update) {
			self.manager.abort_tx(transaction);
			self.manager.unregister_files(&registration_keys);
			return Err(error);
		}
		let dictionary = Arc::new(dictionary);
		let superblocks = new_superblocks
			.iter()
			.cloned()
			.map(|superblock| (superblock.graph_id, superblock))
			.collect::<BTreeMap<_, _>>();
		let graphs = match load_changed_graphs(
			&self.root,
			&self.manager,
			&transaction,
			&database,
			&files,
			changed_graphs,
			dictionary.clone(),
			&superblocks,
		) {
			Ok(graphs) => graphs,
			Err(error) => {
				self.manager.abort_tx(transaction);
				self.manager.unregister_files(&registration_keys);
				return Err(error);
			}
		};
		if let Err(error) = self.manager.commit_tx(transaction) {
			self.manager.unregister_files(&registration_keys);
			return Err(error);
		}
		let active_graphs = database.graphs.iter().map(|entry| entry.graph_id);
		self.manager.publish_graph_authority(
			new_superblocks,
			active_graphs,
			graph_count,
			files.next_file_instance_id,
		);
		catalogs.database = database;
		catalogs.files = files;
		Ok(LocalCommitResult {
			property_dictionary: dictionary,
			graphs,
		})
	}
}

fn checkpoint_due(
	unbackfilled_pages: usize,
	page_threshold: usize,
	elapsed: Duration,
	interval: Duration,
) -> bool {
	unbackfilled_pages != 0 && (unbackfilled_pages >= page_threshold || elapsed >= interval)
}

#[cfg(test)]
mod checkpoint_policy_tests {
	use std::time::Duration;

	use super::checkpoint_due;

	#[test]
	fn checkpoint_policy_requires_work_and_either_threshold() {
		assert!(!checkpoint_due(0, 4, Duration::from_secs(10), Duration::ZERO));
		assert!(!checkpoint_due(3, 4, Duration::from_secs(9), Duration::from_secs(10)));
		assert!(checkpoint_due(4, 4, Duration::ZERO, Duration::from_secs(10)));
		assert!(checkpoint_due(1, 4, Duration::from_secs(10), Duration::from_secs(10)));
	}
}
