//! Crash-safe copy-on-write graph generations for the local datastore.

mod commit;
/// Canonical format-v1 graph-generation filenames.
pub(in crate::storage) mod file_layout;
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
pub use view::{LocalEdgeRead, LocalGraphView, LocalNodeRead};
pub(crate) use view::{LocalNodeAdjacency, LocalTableCursor};
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
		ParameterCatalogEntry,
		SystemIndexCatalogEntry,
		TableCatalogEntry,
		TableFieldCatalogEntry,
		TableIndexCatalogEntry,
		encode_catalog_update,
	},
	direct::DirectFile,
	file_registry::{DataFileRegistration, DataFileRole},
	graph_index::{
		CountIndexValue,
		ExternalIndexSorter,
		LabelDictionary,
		PrimaryIndexEntry,
		ScalarIndexMode,
		build_label_index,
		build_primary_index,
		build_primary_index_sorted,
		build_scalar_index_sorted,
		cleanup_external_index_sort,
		decode_count_index,
		decode_label_dictionary,
		encode_count_index,
		encode_label_dictionary,
		leftmost_primary_leaf,
		lookup_primary_index,
		lookup_scalar_index,
		range_scalar_index,
		scan_primary_index,
		visit_label_index_pages,
		visit_primary_leaf_page,
	},
	graph_pages::{
		CROSS_LEVEL_REFS_PER_PAGE,
		EdgeHotSlot,
		EdgeSlotFlags,
		EdgeTopologyCode,
		EndpointSide,
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
		StatisticsLayout,
		SubgraphEntry,
		decode_adjacency_overflow,
		decode_cross_level_overflow,
		decode_degree_statistics,
		decode_freelist,
		decode_label_statistics_file,
		decode_large_property,
		decode_property_map,
		decode_property_statistics,
		decode_statistics_summary,
		decode_subgraph_directory,
		encode_adjacency_overflow,
		encode_cross_level_overflow,
		encode_freelist,
		encode_large_property,
		encode_property_map,
		encode_statistics,
		encode_statistics_delta,
		encode_subgraph_directory,
		hot_slot_address,
		new_edge_hot_page,
		new_node_hot_page,
		read_edge_hot_slot,
		read_node_hot_slot,
		read_property_record,
		statistics_bulk_layout_matches,
		statistics_layout,
		validate_statistics_file_lengths,
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
		IndexId,
		ScalarComparison,
		TableIndexKind,
		TableKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::{
		DegreeHistogram,
		DegreeHistogramKey,
		GraphStatistics,
		LabelStatistic,
		LabelStatisticKey,
		PropertyHistogram,
		PropertyHistogramKey,
		StatisticField,
		analyze_graph,
	},
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

/// One graph supplied by the MVCC layer for a durable commit.
pub struct LocalCommitGraph {
	/// Stable catalog identity of the named graph.
	pub record_id: RecordId,
	/// Case-sensitive user-visible graph name.
	pub name: String,
	/// Versioned schema metadata for this named root graph.
	pub schema: LocalCommitGraphSchema,
	/// Physical change being published.
	pub change: LocalCommitGraphChange,
}

/// Versioned schema metadata attached to one named root graph.
#[derive(Clone, Copy)]
pub struct LocalCommitGraphSchema {
	/// Stable graph-definition identity.
	pub id: u32,
	/// Semantic graph-definition version.
	pub version: u32,
	/// Engine-owned label-index definition.
	pub label_index: LocalCommitSystemIndex,
	/// Engine-owned adjacency-index definition.
	pub adjacency_index: LocalCommitSystemIndex,
}

/// Versioned identity of one engine-owned system index.
#[derive(Clone, Copy)]
pub struct LocalCommitSystemIndex {
	/// Stable logical index identity from the shared schema clock.
	pub id: IndexId,
	/// Semantic definition version.
	pub version: u32,
}

/// Graph payload accepted by one local durable commit.
pub enum LocalCommitGraphChange {
	/// Complete immutable graph image used by the compatibility path.
	Materialized {
		/// Complete graph value.
		graph: Arc<MaterializedGraph>,
		/// Complete explicit table membership for live root atoms.
		memberships: Vec<LocalCommitTableMembership>,
	},
	/// Append-only node delta applied without materializing the existing graph.
	AppendNodes {
		/// Exact durable graph version on which the delta is based.
		base: Arc<LocalGraph>,
		/// New nodes in stable slot order.
		nodes: Vec<LocalCommitNode>,
	},
	/// Final states of transaction-touched atoms applied as page deltas.
	AtomDelta {
		/// Exact durable graph version on which the delta is based.
		base: Arc<LocalGraph>,
		/// Bounded atom changes in stable slot order.
		delta: LocalCommitAtomDelta,
	},
}

/// One append-only node supplied by the MVCC layer.
pub struct LocalCommitNode {
	/// Stable in-graph slot allocated by the transaction.
	pub id: AtomId,
	/// Public record identity stored in the primary index.
	pub record_id: RecordId,
	/// Optional node label.
	pub label: Option<Text>,
	/// Owned property payload.
	pub properties: PropertyMap,
	/// Explicit table schema identity; absent for an unassigned atom.
	pub table_id: Option<u32>,
}

/// Final transaction-owned atom states supplied to the local page encoder.
pub struct LocalCommitAtomDelta {
	/// Revision after every successful logical mutation in this transaction.
	pub revision: u64,
	/// Allocated node-slot boundary after the mutation.
	pub node_capacity: u64,
	/// Allocated edge-slot boundary after the mutation.
	pub edge_capacity: u64,
	/// Changed node states in stable slot order.
	pub nodes: Vec<LocalCommitNodeChange>,
	/// Changed edge states in stable slot order.
	pub edges: Vec<LocalCommitEdgeChange>,
	/// Final adjacency for every node affected by an edge mutation.
	pub adjacency: Vec<LocalCommitNodeAdjacency>,
	/// Explicit membership changes applied after atom values.
	pub memberships: Vec<LocalCommitTableMembership>,
}

/// One generation-aware table-membership update.
#[derive(Clone, Copy)]
pub struct LocalCommitTableMembership {
	/// Stable atom identity in the root graph.
	pub id: AtomId,
	/// Target table schema identity, or `None` to clear membership.
	pub table_id: Option<u32>,
}

/// Final role-separated adjacency for one live local node.
pub struct LocalCommitNodeAdjacency {
	/// Stable node identity.
	pub id: AtomId,
	/// Edges for which the node has an outgoing role.
	pub outgoing: Vec<AtomId>,
	/// Edges for which the node has an incoming role.
	pub incoming: Vec<AtomId>,
	/// Edges for which the node has an undirected role.
	pub undirected: Vec<AtomId>,
}

/// One final node state in a local atom delta.
pub enum LocalCommitNodeChange {
	/// A created node or the final value of an updated existing node.
	Upsert {
		/// Stable generation-aware atom identity.
		id: AtomId,
		/// Stable public record identity.
		record_id: RecordId,
		/// Optional final label.
		label: Option<Text>,
		/// Complete final property map.
		properties: PropertyMap,
		/// Whether this transaction allocated the slot.
		created: bool,
	},
	/// A tombstone for an atom that was live or allocated in this transaction.
	Remove {
		/// Stable pre-removal atom identity.
		id: AtomId,
		/// Whether this transaction allocated the slot.
		created: bool,
	},
}

/// One final edge state in a local atom delta.
pub enum LocalCommitEdgeChange {
	/// A created edge or the final value of an updated existing edge.
	Upsert {
		/// Stable generation-aware atom identity.
		id: AtomId,
		/// Stable public record identity.
		record_id: RecordId,
		/// Complete final incidence.
		topology: EdgeTopology,
		/// Optional final label.
		label: Option<Text>,
		/// Complete final property map.
		properties: PropertyMap,
		/// Optional final numeric weight.
		weight: Option<crate::values::runtime::Number>,
		/// Whether this transaction allocated the slot.
		created: bool,
		/// Whether an existing edge's incidence changed.
		topology_changed: bool,
	},
	/// A tombstone for an edge that was live or allocated in this transaction.
	Remove {
		/// Stable pre-removal atom identity.
		id: AtomId,
		/// Whether this transaction allocated the slot.
		created: bool,
	},
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
	/// Eager graph counters decoded for this exact durable graph revision.
	statistics: GraphStatistics,
	/// Split-file ranges declared by the eager statistics root.
	statistics_layout: StatisticsLayout,
	/// Immutable auxiliary statistics loaded on first use through a snapshot.
	statistics_parts: Mutex<LazyStatisticsParts>,
	property_dictionary: Arc<PropertyDictionary>,
	decode_context: Mutex<Option<Arc<EncodeContext>>>,
	scalar_indexes: BTreeMap<String, Vec<LocalScalarIndex>>,
	/// Whether durable schema contains any scalar index, including one whose
	/// side-file was unavailable at open time.
	has_declared_scalar_indexes: bool,
	count_indexes: BTreeMap<u32, LocalCountIndex>,
	label_indexes: BTreeMap<(AtomKind, u32), DataFileId>,
	children: BTreeMap<u32, Arc<LocalGraph>>,
}

/// Successfully decoded lazy statistics parts for one exact local graph.
// ponytail: one mutex serializes only each part's first load; split it only if
// concurrent cold statistics reads show measurable contention.
#[derive(Default)]
struct LazyStatisticsParts {
	/// Exact per-label counters decoded from `label_stats.fgb`.
	labels: Option<Arc<BTreeMap<LabelStatisticKey, LabelStatistic>>>,
	/// Degree histograms decoded from the tail of `graph_stats.fgb`.
	degrees: Option<Arc<BTreeMap<DegreeHistogramKey, DegreeHistogram>>>,
	/// Property histograms decoded from `prop_stats.fgb`.
	properties: Option<Arc<BTreeMap<PropertyHistogramKey, PropertyHistogram>>>,
	/// Fully assembled logical statistics after all three parts were requested.
	complete: Option<Arc<GraphStatistics>>,
}

#[derive(Clone)]
struct LocalScalarIndex {
	table_id: u32,
	name: String,
	kind: TableIndexKind,
	fields: Vec<(PropertyKeyId, Type)>,
	file_id: DataFileId,
	root: PageNo,
}

#[derive(Clone, Copy)]
struct LocalCountIndex {
	file_id: DataFileId,
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

	pub(crate) const fn node_capacity(&self) -> u64 {
		self.superblock.node_capacity
	}

	pub(crate) const fn edge_capacity(&self) -> u64 {
		self.superblock.edge_capacity
	}

	pub(crate) fn supports_atom_delta(&self) -> bool {
		!self.has_declared_scalar_indexes
	}

	/// Returns eagerly decoded graph counters for this durable revision.
	pub(crate) const fn statistics_summary(&self) -> &GraphStatistics {
		&self.statistics
	}

	/// Loads exact label counters without touching degree or property files.
	fn label_statistics(
		&self,
		manager: &SyncPageManager,
		transaction: &TxHandle,
	) -> FerrosGrynnResult<Arc<BTreeMap<LabelStatisticKey, LabelStatistic>>> {
		let mut parts = self.statistics_parts.lock();
		if let Some(labels) = &parts.labels {
			return Ok(Arc::clone(labels));
		}
		let labels = Arc::new(decode_label_statistics_file(
			|page_no, output| {
				let page = manager.read_page(
					transaction,
					self.page_key(DataFileKind::LabelStatistics, page_no)?,
				)?;
				output.as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			},
			self.statistics_layout,
			&self.labels.node_labels,
			&self.labels.edge_labels,
		)?);
		parts.labels = Some(Arc::clone(&labels));
		Ok(labels)
	}

	/// Loads a mutable point-DML snapshot while leaving bulk files untouched.
	fn statistics_with_labels(
		&self,
		manager: &SyncPageManager,
		transaction: &TxHandle,
	) -> FerrosGrynnResult<GraphStatistics> {
		let mut statistics = self.statistics.clone();
		statistics.labels = (*self.label_statistics(manager, transaction)?).clone();
		Ok(statistics)
	}

	/// Loads every statistics part through the caller's exact storage snapshot.
	fn complete_statistics(
		&self,
		manager: &SyncPageManager,
		transaction: &TxHandle,
	) -> FerrosGrynnResult<Arc<GraphStatistics>> {
		if let Some(statistics) = &self.statistics_parts.lock().complete {
			return Ok(Arc::clone(statistics));
		}
		let labels = self.label_statistics(manager, transaction)?;
		let degrees = {
			let mut parts = self.statistics_parts.lock();
			if let Some(degrees) = &parts.degrees {
				Arc::clone(degrees)
			} else {
				let degrees = Arc::new(decode_degree_statistics(
					|page_no, output| {
						let page = manager.read_page(
							transaction,
							self.page_key(DataFileKind::Statistics, page_no)?,
						)?;
						output.as_bytes_mut().copy_from_slice(page.as_bytes());
						Ok(())
					},
					self.statistics_layout,
					&self.labels.node_labels,
					&self.labels.edge_labels,
				)?);
				parts.degrees = Some(Arc::clone(&degrees));
				degrees
			}
		};
		let properties = {
			let mut parts = self.statistics_parts.lock();
			if let Some(properties) = &parts.properties {
				Arc::clone(properties)
			} else {
				let properties = Arc::new(decode_property_statistics(
					|page_no, output| {
						let page = manager.read_page(
							transaction,
							self.page_key(DataFileKind::PropertyStatistics, page_no)?,
						)?;
						output.as_bytes_mut().copy_from_slice(page.as_bytes());
						Ok(())
					},
					self.statistics_layout,
				)?);
				parts.properties = Some(Arc::clone(&properties));
				properties
			}
		};
		let mut statistics = self.statistics.clone();
		statistics.labels = (*labels).clone();
		statistics.degree_histograms = (*degrees).clone();
		statistics.property_histograms = (*properties).clone();
		let statistics = Arc::new(statistics);
		let mut parts = self.statistics_parts.lock();
		if let Some(cached) = &parts.complete {
			return Ok(Arc::clone(cached));
		}
		parts.complete = Some(Arc::clone(&statistics));
		Ok(statistics)
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
	/// Recovered versioned schema metadata.
	pub schema: LocalCommitGraphSchema,
	/// Complete explicit membership recovered from live hot slots.
	pub memberships: Vec<LocalCommitTableMembership>,
	/// Eager counters for the exact graph revision; auxiliary distributions
	/// remain transaction-bound and lazy in `graph`.
	pub statistics: GraphStatistics,
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
	pub table_id: Option<u32>,
}

/// One durable table field supplied by the MVCC layer.
#[derive(Clone)]
pub struct LocalCommitTableField {
	/// Stable field-definition identity.
	pub id: u32,
	/// Semantic field-definition version.
	pub version: u32,
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
	/// Stable index-definition identity.
	pub id: IndexId,
	/// Semantic index-definition version.
	pub version: u32,
	/// Table-local index name.
	pub name: String,
	/// Physical table-index contract.
	pub kind: TableIndexKind,
	/// Composite fields as dense property-key identifiers; empty for Count.
	pub fields: Vec<u32>,
}

/// One durable table supplied by the MVCC layer.
#[derive(Clone)]
pub struct LocalCommitTable {
	/// Stable table-definition identity.
	pub id: u32,
	/// Semantic table-definition version.
	pub version: u32,
	/// Unique table name.
	pub name: String,
	/// Named graph containing the table atoms.
	pub graph: RecordId,
	/// Node/edge membership class.
	pub kind: TableKind,
	/// Ordered explicit fields after implicit `id`.
	pub fields: Vec<LocalCommitTableField>,
	/// User-declared indexes in definition order.
	pub indexes: Vec<LocalCommitTableIndex>,
	/// Engine-owned primary-index definition.
	pub primary_index: LocalCommitSystemIndex,
}

/// One durable global database parameter supplied by the MVCC layer.
#[derive(Clone)]
pub struct LocalCommitParameter {
	/// Stable parameter-definition identity.
	pub id: u32,
	/// Semantic parameter-definition version.
	pub version: u32,
	/// Parameter name without the query-language `$` prefix.
	pub name: String,
	/// Owned runtime value encoded canonically by the storage layer.
	pub value: Value,
}

/// Recovered local state used to seed the existing MVCC root.
pub struct LocalDatabaseSnapshot {
	/// Highest recovered durable logical commit.
	pub commit_ts: u64,
	/// First schema identity not allocated before this snapshot.
	pub next_schema_id: u64,
	/// Recovered append-only property dictionary.
	pub property_dictionary: Arc<PropertyDictionary>,
	/// Recovered named graphs.
	pub graphs: Vec<LocalSnapshotGraph>,
	/// Recovered table schemas.
	pub tables: Vec<LocalCommitTable>,
	/// Recovered global database parameters.
	pub parameters: Vec<LocalCommitParameter>,
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
		cleanup_external_index_sort(&root)?;
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
		let _catalogs = self.catalogs.lock();
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
				|slot, generation, tombstone, _table_id| {
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

	/// Reads an exact table-wide Count index, or returns `None` when the table
	/// has no declared Count index.
	pub(crate) fn table_count_index(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table_id: u32,
	) -> FerrosGrynnResult<Option<u64>> {
		let Some(index) = graph.count_indexes.get(&table_id) else {
			return Ok(None);
		};
		let mut page = AlignedPage::zeroed();
		copy_local_index_page(self, transaction, graph, index.file_id, PageNo::new(0), &mut page)?;
		let value = decode_count_index(&page)?;
		if value.table_id != table_id {
			return Err(FerrosGrynnError::invalid_storage_file(
				"Count index table identity disagrees with its catalog binding",
			));
		}
		Ok(Some(value.atom_count))
	}

	/// Visits one exact label side-file. `false` requests the caller's normal
	/// graph scan because a committed mutation marked the generation stale.
	pub(crate) fn visit_label_index(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		kind: AtomKind,
		label: &str,
		mut visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<bool> {
		if graph.superblock.flags.contains(GraphSuperblockFlags::LABEL_INDEX_STALE) {
			return Ok(false);
		}
		let labels = match kind {
			AtomKind::Node => &graph.labels.node_labels,
			AtomKind::Edge => &graph.labels.edge_labels,
		};
		let Some(label_id) = labels.iter().position(|candidate| candidate == label) else {
			return Ok(true);
		};
		let label_id = u32::try_from(label_id).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("label identity exceeds u32")
		})?;
		let Some(file_id) = graph.label_indexes.get(&(kind, label_id)).copied() else {
			return Ok(false);
		};
		let context = graph.decode_context(self, transaction)?;
		visit_label_index_pages(
			|page_no, output| {
				copy_local_index_page(self, transaction, graph, file_id, page_no, output)
			},
			kind,
			|disk| {
				let reference = context.registered_atom(disk).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"label index references an atom absent from the primary index",
					)
				})?;
				let atom =
					local_query_atom(self, transaction, graph, reference.id().clone(), disk)?;
				let (page_no, within) = hot_slot_address(atom.atom_id.slot())?;
				let page = self.manager.read_page(
					&transaction.inner,
					graph.page_key(
						match kind {
							AtomKind::Node => DataFileKind::NodesHot,
							AtomKind::Edge => DataFileKind::EdgesHot,
						},
						page_no,
					)?,
				)?;
				let actual_label = match kind {
					AtomKind::Node => read_node_hot_slot(page.as_page(), page_no, within)?.label_id,
					AtomKind::Edge => read_edge_hot_slot(page.as_page(), page_no, within)?.label_id,
				};
				if actual_label != Some(label_id) {
					return Err(FerrosGrynnError::invalid_storage_file(
						"label index entry disagrees with its hot atom slot",
					));
				}
				visit(atom)
			},
		)?;
		Ok(true)
	}

	/// Visits matching primary-index records in stable RecordId order.
	pub(crate) fn visit_table(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table_id: u32,
		kind: TableKind,
		mut visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		let context = graph.base_context()?;
		visit_local_primary(self, transaction, graph, |key, disk| {
			let record_id = decode_record_key(key, &context)?;
			if !table_kind_accepts(kind, disk.kind()) {
				return Ok(());
			}
			let atom = local_query_atom(self, transaction, graph, record_id, disk)?;
			if atom.table_id == Some(table_id) {
				visit(atom)?;
			}
			Ok(())
		})
	}

	/// Visits candidates through the widest composite index covered by exact
	/// equality constraints. `false` keeps the caller on its scan fallback.
	pub(crate) fn visit_table_composite_index(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table_id: u32,
		table: &str,
		kind: TableKind,
		equalities: &[(PropertyKeyId, &Value)],
		visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<bool> {
		let Some(index) = graph.scalar_indexes.get(table).and_then(|indexes| {
			indexes
				.iter()
				.filter(|index| {
					index.table_id == table_id
						&& index.fields.len() > 1
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
			TableIndexKind::Count => return Ok(false),
		};
		let atoms = lookup_scalar_index(
			|page_no, output| {
				copy_local_index_page(self, transaction, graph, index.file_id, page_no, output)
			},
			index.root,
			mode,
			&key,
		)?;
		self.visit_scalar_index_atoms(
			transaction,
			graph,
			table_id,
			table,
			kind,
			&index.name,
			atoms,
			visit,
		)?;
		Ok(true)
	}

	/// Visits candidates from a usable single-field scalar index. `false`
	/// means that the caller must perform the normal table scan.
	pub(crate) fn visit_table_index(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table_id: u32,
		table: &str,
		kind: TableKind,
		property_key: PropertyKeyId,
		comparison: ScalarComparison,
		value: &Value,
		visit: impl FnMut(LocalQueryAtom) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<bool> {
		let Some(index) = graph.scalar_indexes.get(table).and_then(|indexes| {
			indexes.iter().find(|index| {
				index.table_id == table_id
					&& index.fields.len() == 1
					&& index.fields[0].0 == property_key
			})
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
			TableIndexKind::Count => return Ok(false),
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

		self.visit_scalar_index_atoms(
			transaction,
			graph,
			table_id,
			table,
			kind,
			&index.name,
			atoms,
			visit,
		)?;
		Ok(true)
	}

	fn visit_scalar_index_atoms(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		table_id: u32,
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
			if !table_kind_accepts(kind, atom.kind()) {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' references an atom outside its table",
					table, index_name
				)));
			}
			let atom = local_query_atom(self, transaction, graph, record.clone(), atom)?;
			if atom.table_id != Some(table_id) {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' references an atom outside its table",
					table, index_name
				)));
			}
			visit(atom)?;
		}
		Ok(())
	}

	/// Resolves one table record through the logarithmic primary lookup.
	pub(crate) fn lookup_record(
		&self,
		transaction: &LocalReadTransaction,
		graph: &Arc<LocalGraph>,
		record_id: &RecordId,
		table_id: u32,
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
		let atom = local_query_atom(self, transaction, graph, record_id.clone(), disk)?;
		Ok((atom.table_id == Some(table_id)).then_some(atom))
	}

	/// Publishes one MVCC root by switching only the two fixed catalog heads in
	/// the frame WAL after every new graph file is durable.
	pub fn commit(
		&self,
		expected_commit_ts: u64,
		next_schema_id: u64,
		property_dictionary: &PropertyDictionary,
		graphs: &[LocalCommitGraph],
		tables: &[LocalCommitTable],
		parameters: &[LocalCommitParameter],
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
		database.next_schema_id = next_schema_id;
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
		let mut retired_superblocks = Vec::new();
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
			if !matches!(commit_graph.change, LocalCommitGraphChange::Materialized { .. }) {
				let index = existing.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"atom delta targets a graph absent from the durable catalog",
					)
				})?;
				let current = database.graphs[index].clone();
				let delta = match &commit_graph.change {
					LocalCommitGraphChange::AppendNodes {
						base,
						nodes,
					} => build_node_append_delta(
						&self.manager,
						&transaction,
						&current,
						record_id,
						&commit_graph.name,
						base,
						nodes,
						&mut dictionary,
					)?,
					LocalCommitGraphChange::AtomDelta {
						base,
						delta,
					} => build_atom_delta(
						&self.manager,
						&transaction,
						&current,
						record_id,
						&commit_graph.name,
						base,
						delta,
						&mut dictionary,
					)?,
					LocalCommitGraphChange::Materialized {
						..
					} => unreachable!(),
				};
				let removed_subgraph_slots = delta.removed_subgraph_slots.clone();
				let mut next_file_instance_id = files.next_file_instance_id;
				let generation = write_delta_graph_generation(
					&self.root,
					&self.manager,
					&mut transaction,
					&current,
					&files,
					delta,
					&mut next_file_instance_id,
				)?;
				files.next_file_instance_id = next_file_instance_id;
				let previous_superblock = files
					.files
					.iter()
					.find(|entry| {
						entry.graph_id == current.graph_id
							&& entry.file_id.kind() == Some(DataFileKind::GraphSuperblock)
					})
					.map(|entry| (entry.graph_id, entry.file_id))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"graph delta has no checkpoint-owned superblock",
						)
					})?;
				files.files.retain(|entry| {
					entry.graph_id != current.graph_id
						|| entry.file_id.kind() != Some(DataFileKind::GraphSuperblock)
				});
				files.files.extend(generation.file_entries);
				registrations.extend(generation.registrations);
				retired_superblocks.push(previous_superblock);
				new_superblocks.push(generation.superblock);
				if !removed_subgraph_slots.is_empty() {
					let mut removed_graphs = BTreeSet::new();
					for child in database.graphs.iter().filter(|entry| {
						entry.parent_graph_id == Some(current.graph_id)
							&& entry
								.subgraph_slot
								.is_some_and(|slot| removed_subgraph_slots.contains(&slot))
					}) {
						removed_graphs.extend(graph_tree_ids(&database, child.graph_id));
					}
					database.graphs.retain(|entry| !removed_graphs.contains(&entry.graph_id));
					files.files.retain(|entry| !removed_graphs.contains(&entry.graph_id));
				}
				let root_index = database
					.graphs
					.iter()
					.position(|entry| entry.graph_id == current.graph_id)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"atom delta removed its root graph catalog entry",
						)
					})?;
				database.graphs[root_index] = generation.catalog_entry;
				continue;
			}
			let LocalCommitGraphChange::Materialized {
				graph,
				memberships,
			} = &commit_graph.change
			else {
				return Err(FerrosGrynnError::invalid_storage_file(
					"unsupported local graph commit change",
				));
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
				graph,
				commit_graph.schema,
			)?;
			let mut images =
				build_graph_images(&self.root, &plan, &mut dictionary, tables, memberships)?;
			let in_place = if images.len() == 1
				&& let Some(index) = existing
			{
				prepare_in_place_image(
					&self.root,
					&self.manager,
					&transaction,
					&database.graphs[index],
					&files,
					&mut images[0],
				)?
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
		database.tables = encode_tables(tables, &database, &previous_database, &new_indexes)?;
		database.parameters = encode_parameters(parameters, &mut dictionary)?;
		database.property_keys =
			dictionary.iter().map(|(_id, name)| name.as_str().to_owned()).collect();

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
		self.manager.unregister_files(&retired_superblocks);
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
