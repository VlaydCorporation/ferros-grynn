//! Crash-safe copy-on-write graph generations for the local datastore.

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
		lookup_primary_index,
		lookup_scalar_index,
		range_scalar_index,
		scan_primary_index,
		visit_primary_index,
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
	pub property_dictionary: PropertyDictionary,
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
			let Some(graph) =
				graphs.iter().find(|entry| &entry.record_id == record_id).map(|entry| &entry.graph)
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
			let plan = plan_graph_tree(&mut database, graph_id, record_id, graph)?;
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

fn remove_unreachable_graph_generations(root: &Path, files: &FileCatalog) -> FerrosGrynnResult<()> {
	let referenced = files
		.files
		.iter()
		.filter(|entry| entry.graph_id != GraphId::SYSTEM)
		.filter_map(|entry| entry.path.resolve(root).parent().map(Path::to_path_buf))
		.collect::<BTreeSet<_>>();
	let graphs = root.join("graphs");
	let graph_directories = match fs::read_dir(&graphs) {
		Ok(entries) => entries,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(source) => return Err(storage_io("enumerate graph directories", source)),
	};
	for graph_entry in graph_directories {
		let graph_entry =
			graph_entry.map_err(|source| storage_io("read graph directory entry", source))?;
		let graph_name = graph_entry.file_name();
		let Some(graph_name) = graph_name.to_str() else {
			continue;
		};
		if graph_name.len() != 16
			|| !graph_name.bytes().all(|byte| byte.is_ascii_hexdigit())
			|| !graph_entry
				.file_type()
				.map_err(|source| storage_io("inspect graph directory", source))?
				.is_dir()
		{
			continue;
		}
		let graph_path = graph_entry.path();
		for generation_entry in fs::read_dir(&graph_path)
			.map_err(|source| storage_io("enumerate graph generations", source))?
		{
			let generation_entry = generation_entry
				.map_err(|source| storage_io("read graph generation entry", source))?;
			let generation_name = generation_entry.file_name();
			let Some(generation_name) = generation_name.to_str() else {
				continue;
			};
			if generation_name.parse::<Uuid>().is_err()
				|| !generation_entry
					.file_type()
					.map_err(|source| storage_io("inspect graph generation", source))?
					.is_dir() || referenced.contains(&generation_entry.path())
			{
				continue;
			}
			fs::remove_dir_all(generation_entry.path())
				.map_err(|source| storage_io("remove unreachable graph generation", source))?;
		}
		let mut remaining = fs::read_dir(&graph_path)
			.map_err(|source| storage_io("inspect graph directory after collection", source))?;
		if remaining.next().is_none() {
			fs::remove_dir(&graph_path)
				.map_err(|source| storage_io("remove empty graph directory", source))?;
		}
	}
	Ok(())
}

/// Transaction-bound graph view backed by page-manager guards.
pub struct LocalGraphView<'tx> {
	store: &'tx LocalStore,
	transaction: Arc<LocalReadTransaction>,
	graph: Arc<LocalGraph>,
}

/// Node projection retaining the hot page pin while cold properties remain
/// loadable through the same storage snapshot.
pub struct LocalNodeRead<'tx> {
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
	id: AtomId,
	record_id: RecordId,
	label: Option<Text>,
	slot: NodeHotSlot,
	_hot_page: PageRead<'tx>,
}

impl NodeRead for LocalNodeRead<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		self.transaction.ensure_live()?;
		Ok(Cow::Borrowed(&self.record_id))
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		self.transaction.ensure_live()?;
		Ok(self.label.as_ref().map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		read_local_properties(self.store, self.transaction, self.graph, self.slot.properties)
			.map(Cow::Owned)
	}

	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		self.transaction.ensure_live()?;
		self.slot
			.subgraph_slot
			.map(|slot| {
				self.graph
					.children
					.get(&slot)
					.map(|child| Cow::Borrowed(child.reference()))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"node references an unknown nested graph slot",
						)
					})
			})
			.transpose()
	}

	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		let Some(slot) = self.slot.subgraph_slot else {
			return Ok(None);
		};
		let child = self.graph.children.get(&slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node references an unknown nested graph slot")
		})?;
		let transaction = self.transaction_owner.clone().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"nested graph contents require an owning transaction view",
			)
		})?;
		let graph = self.store.materialize_graph(transaction, child)?.into_general()?;
		Ok(Some(Cow::Owned(graph)))
	}
}

/// Edge projection retaining the hot page pin and resolving topology lazily.
pub struct LocalEdgeRead<'tx> {
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
	id: AtomId,
	record_id: RecordId,
	label: Option<Text>,
	slot: EdgeHotSlot,
	_hot_page: PageRead<'tx>,
}

impl EdgeRead for LocalEdgeRead<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		self.transaction.ensure_live()?;
		Ok(Cow::Borrowed(&self.record_id))
	}

	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>> {
		decode_local_topology(self.store, self.transaction, self.graph, &self.slot).map(Cow::Owned)
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		self.transaction.ensure_live()?;
		Ok(self.label.as_ref().map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		read_local_properties(self.store, self.transaction, self.graph, self.slot.properties)
			.map(Cow::Owned)
	}

	fn weight(&self) -> Option<crate::values::runtime::Number> {
		self.slot.weight.map(Into::into)
	}

	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		self.transaction.ensure_live()?;
		self.slot
			.transition_graph
			.map(|slot| {
				self.graph
					.children
					.get(&slot)
					.map(|child| Cow::Borrowed(child.reference()))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"edge references an unknown transition graph slot",
						)
					})
			})
			.transpose()
	}

	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		let Some(slot) = self.slot.transition_graph else {
			return Ok(None);
		};
		let child = self.graph.children.get(&slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"edge references an unknown transition graph slot",
			)
		})?;
		let transaction = self.transaction_owner.clone().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"transition graph contents require an owning transaction view",
			)
		})?;
		let graph = self.store.materialize_graph(transaction, child)?.into_general()?;
		Ok(Some(Cow::Owned(graph)))
	}
}

impl GraphRead for LocalGraphView<'_> {
	type Edge<'a>
		= LocalEdgeRead<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= std::vec::IntoIter<FerrosGrynnResult<AtomId>>
	where
		Self: 'a;
	type Node<'a>
		= LocalNodeRead<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= std::vec::IntoIter<FerrosGrynnResult<AtomId>>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.graph.identity
	}

	fn revision(&self) -> u64 {
		self.graph.revision
	}

	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		read_local_properties(
			self.store,
			&self.transaction,
			self.graph.as_ref(),
			self.graph.properties,
		)
		.map(Cow::Owned)
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		collect_local_ids(self.store, &self.transaction, self.graph.as_ref(), AtomKind::Node)
			.map(|ids| ids.into_iter().map(Ok).collect::<Vec<_>>().into_iter())
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		collect_local_ids(self.store, &self.transaction, self.graph.as_ref(), AtomKind::Edge)
			.map(|ids| ids.into_iter().map(Ok).collect::<Vec<_>>().into_iter())
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		if id.kind() != Some(AtomKind::Node) {
			return Ok(None);
		}
		let Some(record_id) =
			record_for_local_atom(self.store, &self.transaction, self.graph.as_ref(), id)?
		else {
			return Ok(None);
		};
		read_local_node(
			self.store,
			&self.transaction,
			self.graph.as_ref(),
			id,
			record_id,
			Some(self.transaction.clone()),
		)
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		if id.kind() != Some(AtomKind::Edge) {
			return Ok(None);
		}
		let Some(record_id) =
			record_for_local_atom(self.store, &self.transaction, self.graph.as_ref(), id)?
		else {
			return Ok(None);
		};
		read_local_edge(
			self.store,
			&self.transaction,
			self.graph.as_ref(),
			id,
			record_id,
			Some(self.transaction.clone()),
		)
	}

	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::DiskBacked
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.transaction.snapshot_id()
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.graph.reference)
	}

	fn node_count(&self) -> FerrosGrynnResult<usize> {
		self.transaction.ensure_live()?;
		usize::try_from(self.graph.superblock.node_count)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds usize"))
	}

	fn edge_count(&self) -> FerrosGrynnResult<usize> {
		self.transaction.ensure_live()?;
		usize::try_from(self.graph.superblock.edge_count)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds usize"))
	}

	fn atom_id(&self, reference: &AtomRef) -> FerrosGrynnResult<Option<AtomId>> {
		lookup_local_atom(self.store, &self.transaction, self.graph.as_ref(), reference)
	}
}

fn read_local_page<'tx>(
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &LocalGraph,
	kind: DataFileKind,
	page_no: PageNo,
) -> FerrosGrynnResult<PageRead<'tx>> {
	transaction.ensure_live()?;
	store.manager.read_page(&transaction.inner, graph.page_key(kind, page_no)?)
}

fn copy_local_page(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	kind: DataFileKind,
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let page = read_local_page(store, transaction, graph, kind, page_no)?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

fn copy_local_index_page(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	file_id: DataFileId,
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	transaction.ensure_live()?;
	let page =
		store.manager.read_page(&transaction.inner, graph.index_page_key(file_id, page_no))?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

fn visit_local_primary(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	visit: impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	visit_primary_index(
		|page_no, output| {
			copy_local_page(store, transaction, graph, DataFileKind::Index, page_no, output)
		},
		graph.primary_index_root,
		visit,
	)
}

fn decode_record_key(key: &[u8], context: &EncodeContext) -> FerrosGrynnResult<RecordId> {
	let stored = CanonicalCodec::decode(key, DecodeLimits::default())?;
	let Value::Record(record) = stored.into_runtime(&context.decoder())? else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"primary index key is not a canonical RecordId",
		));
	};
	Ok(*record)
}

fn encode_record_key(record: &RecordId, context: &mut EncodeContext) -> FerrosGrynnResult<Vec<u8>> {
	CanonicalCodec::encode(
		StorageValue::from_runtime(Value::Record(Box::new(record.clone())), context)?.as_ref(),
	)
}

fn local_query_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	record_id: RecordId,
	disk: DiskAtomRef,
) -> FerrosGrynnResult<LocalQueryAtom> {
	let atom_id = local_atom_id(store, transaction, graph, disk)?;
	let properties = match disk.kind() {
		AtomKind::Node => {
			read_local_node(store, transaction, graph, atom_id, record_id.clone(), None)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing node",
					)
				})?
				.properties()?
				.into_owned()
		}
		AtomKind::Edge => {
			read_local_edge(store, transaction, graph, atom_id, record_id.clone(), None)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing edge",
					)
				})?
				.properties()?
				.into_owned()
		}
	};
	Ok(LocalQueryAtom {
		atom_id,
		record_id,
		properties,
	})
}

fn lookup_local_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	reference: &AtomRef,
) -> FerrosGrynnResult<Option<AtomId>> {
	let mut context = graph.base_context()?;
	let key = encode_record_key(reference.id(), &mut context)?;
	let atom = lookup_primary_index(
		|page_no, output| {
			copy_local_page(store, transaction, graph, DataFileKind::Index, page_no, output)
		},
		graph.primary_index_root,
		&key,
	)?;
	let Some(atom) = atom else {
		return Ok(None);
	};
	if atom.kind() != reference.kind() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"primary index atom kind disagrees with its RecordId lookup",
		));
	}
	local_atom_id(store, transaction, graph, atom).map(Some)
}

fn record_for_local_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	id: AtomId,
) -> FerrosGrynnResult<Option<RecordId>> {
	let disk = DiskAtomRef::from_parts(
		id.kind().ok_or_else(|| FerrosGrynnError::invalid_argument("invalid AtomId kind"))?,
		id.slot(),
	)?;
	let context = graph.decode_context(store, transaction)?;
	Ok(context.registered_atom(disk).map(|reference| reference.id().clone()))
}

fn local_atom_id(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	atom: DiskAtomRef,
) -> FerrosGrynnResult<AtomId> {
	let (page_no, slot) = hot_slot_address(atom.slot())?;
	match atom.kind() {
		AtomKind::Node => {
			let page = read_local_page(store, transaction, graph, DataFileKind::NodesHot, page_no)?;
			let value = read_node_hot_slot(page.as_page(), page_no, slot)?;
			if value.flags.contains(NodeSlotFlags::TOMBSTONE) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"primary index references a node tombstone",
				));
			}
			AtomId::from_parts(AtomKind::Node, atom.slot(), value.generation)
		}
		AtomKind::Edge => {
			let page = read_local_page(store, transaction, graph, DataFileKind::EdgesHot, page_no)?;
			let value = read_edge_hot_slot(page.as_page(), page_no, slot)?;
			if value.flags.contains(EdgeSlotFlags::TOMBSTONE) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"primary index references an edge tombstone",
				));
			}
			AtomId::from_parts(AtomKind::Edge, atom.slot(), value.generation)
		}
	}
}

fn read_local_node<'tx>(
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	id: AtomId,
	record_id: RecordId,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
) -> FerrosGrynnResult<Option<LocalNodeRead<'tx>>> {
	let (page_no, slot_index) = hot_slot_address(id.slot())?;
	let page = read_local_page(store, transaction, graph, DataFileKind::NodesHot, page_no)?;
	let slot = read_node_hot_slot(page.as_page(), page_no, slot_index)?;
	if slot.flags.contains(NodeSlotFlags::TOMBSTONE) || slot.generation != id.generation() {
		return Ok(None);
	}
	validate_table_id(slot.table_id, &record_id, &graph.labels.table_names)?;
	let label = decode_label(slot.label_id, &graph.labels.node_labels)?;
	Ok(Some(LocalNodeRead {
		store,
		transaction,
		graph,
		transaction_owner,
		id,
		record_id,
		label,
		slot,
		_hot_page: page,
	}))
}

fn read_local_edge<'tx>(
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	id: AtomId,
	record_id: RecordId,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
) -> FerrosGrynnResult<Option<LocalEdgeRead<'tx>>> {
	let (page_no, slot_index) = hot_slot_address(id.slot())?;
	let page = read_local_page(store, transaction, graph, DataFileKind::EdgesHot, page_no)?;
	let slot = read_edge_hot_slot(page.as_page(), page_no, slot_index)?;
	if slot.flags.contains(EdgeSlotFlags::TOMBSTONE) || slot.generation != id.generation() {
		return Ok(None);
	}
	validate_table_id(slot.table_id, &record_id, &graph.labels.table_names)?;
	let label = decode_label(slot.label_id, &graph.labels.edge_labels)?;
	Ok(Some(LocalEdgeRead {
		store,
		transaction,
		graph,
		transaction_owner,
		id,
		record_id,
		label,
		slot,
		_hot_page: page,
	}))
}

fn collect_local_ids(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	kind: AtomKind,
) -> FerrosGrynnResult<Vec<AtomId>> {
	let mut ids = Vec::new();
	visit_local_slots(store, transaction, graph, kind, |slot, generation, tombstone| {
		if !tombstone {
			ids.push(AtomId::from_parts(kind, slot, generation)?);
		}
		Ok(())
	})?;
	Ok(ids)
}

fn visit_local_slots(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	kind: AtomKind,
	mut visit: impl FnMut(u64, u32, bool) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let (capacity, pages, file_kind) = match kind {
		AtomKind::Node => (
			graph.superblock.node_capacity,
			graph.superblock.node_hot_pages,
			DataFileKind::NodesHot,
		),
		AtomKind::Edge => (
			graph.superblock.edge_capacity,
			graph.superblock.edge_hot_pages,
			DataFileKind::EdgesHot,
		),
	};
	for page_index in 0..pages {
		let page_no = PageNo::new(page_index);
		let page = read_local_page(store, transaction, graph, file_kind, page_no)?;
		for within in 0..HOT_SLOTS_PER_PAGE {
			let slot = page_index
				.checked_mul(u64::try_from(HOT_SLOTS_PER_PAGE).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("hot slots per page exceed u64")
				})?)
				.and_then(|slot| slot.checked_add(within as u64))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("hot slot address overflows")
				})?;
			if slot >= capacity {
				break;
			}
			let (generation, tombstone) = match kind {
				AtomKind::Node => {
					let value = read_node_hot_slot(page.as_page(), page_no, within)?;
					(value.generation, value.flags.contains(NodeSlotFlags::TOMBSTONE))
				}
				AtomKind::Edge => {
					let value = read_edge_hot_slot(page.as_page(), page_no, within)?;
					(value.generation, value.flags.contains(EdgeSlotFlags::TOMBSTONE))
				}
			};
			visit(slot, generation, tombstone)?;
		}
	}
	Ok(())
}

fn read_local_properties(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	location: Option<PropertyLocation>,
) -> FerrosGrynnResult<PropertyMap> {
	let Some(location) = location else {
		return Ok(PropertyMap::new());
	};
	let record = match location {
		PropertyLocation::Inline {
			page_no,
			slot,
			length,
		} => {
			let page =
				read_local_page(store, transaction, graph, DataFileKind::Properties, page_no)?;
			read_property_record(page.as_page(), page_no, slot, length)?.to_vec()
		}
		PropertyLocation::Large {
			first_page,
			length,
		} => {
			let limits = DecodeLimits::default();
			let length_usize = usize::try_from(length).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("large property length exceeds usize")
			})?;
			if length_usize > limits.max_total_allocation {
				return Err(FerrosGrynnError::storage_decode_limit_exceeded(
					"large property bytes",
					limits.max_total_allocation as u64,
					length_usize as u64,
				));
			}
			let page_count = length_usize.div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
			let mut pages = Vec::with_capacity(page_count);
			for offset in 0..page_count {
				let page_no = first_page
					.get()
					.checked_add(u64::try_from(offset).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("large property page exceeds u64")
					})?)
					.map(PageNo::new)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("large property page overflows")
					})?;
				let page =
					read_local_page(store, transaction, graph, DataFileKind::Properties, page_no)?;
				let mut copy = AlignedPage::zeroed();
				copy.as_bytes_mut().copy_from_slice(page.as_bytes());
				pages.push(copy);
			}
			decode_large_property(first_page, &pages, length, limits.max_total_allocation)?
		}
	};
	let context = graph.decode_context(store, transaction)?;
	decode_property_map(&record, &context.decoder(), DecodeLimits::default())
}

fn read_local_refs(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	inline: &[Option<DiskAtomRef>],
	count: u32,
	overflow: Option<PageNo>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let count = usize::try_from(count).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("adjacency count exceeds usize")
	})?;
	let mut entries = inline.iter().flatten().copied().take(count).collect::<Vec<_>>();
	let mut next = overflow;
	let mut visited = BTreeSet::new();
	while entries.len() < count {
		let page_no = next.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("adjacency overflow ended too early")
		})?;
		if !visited.insert(page_no) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow contains a cycle",
			));
		}
		let page =
			read_local_page(store, transaction, graph, DataFileKind::AdjacencyOverflow, page_no)?;
		let (following, chunk) = decode_adjacency_overflow(page.as_page(), page_no)?;
		entries.extend(chunk);
		if entries.len() > count {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow exceeds its declared count",
			));
		}
		next = following;
	}
	if next.is_some() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"adjacency overflow contains entries beyond its declared count",
		));
	}
	Ok(entries)
}

fn decode_local_topology(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	edge: &EdgeHotSlot,
) -> FerrosGrynnResult<EdgeTopology> {
	if edge.flags.contains(EdgeSlotFlags::CROSS_LEVEL) {
		let source = decode_cross_level_side(
			store,
			transaction,
			graph,
			edge.source_count,
			&edge.source_inline,
			edge.source_graph,
			edge.source_graph_overflow,
			EndpointSide::Source,
		)?;
		let target = decode_cross_level_side(
			store,
			transaction,
			graph,
			edge.target_count,
			&edge.target_inline,
			edge.target_graph,
			edge.target_graph_overflow,
			EndpointSide::Target,
		)?;
		return topology_from_sides(edge.topology, source, target);
	}
	let source = read_local_refs(
		store,
		transaction,
		graph,
		&edge.source_inline,
		edge.source_count,
		edge.source_overflow,
	)?
	.into_iter()
	.map(|atom| local_atom_id(store, transaction, graph, atom).map(EdgeEndpoint::Local))
	.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let target = read_local_refs(
		store,
		transaction,
		graph,
		&edge.target_inline,
		edge.target_count,
		edge.target_overflow,
	)?
	.into_iter()
	.map(|atom| local_atom_id(store, transaction, graph, atom).map(EdgeEndpoint::Local))
	.collect::<FerrosGrynnResult<Vec<_>>>()?;
	topology_from_sides(edge.topology, source, target)
}

fn topology_from_sides(
	topology: EdgeTopologyCode,
	source: Vec<EdgeEndpoint>,
	target: Vec<EdgeEndpoint>,
) -> FerrosGrynnResult<EdgeTopology> {
	Ok(match topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::HyperDirected => {
			EdgeTopology::directed(source, target)
		}
		EdgeTopologyCode::Bidirectional => EdgeTopology::bidirectional(source, target),
		EdgeTopologyCode::Undirected | EdgeTopologyCode::HyperUndirected => {
			if !target.is_empty() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"undirected edge contains target endpoints",
				));
			}
			EdgeTopology::undirected(source)
		}
		EdgeTopologyCode::Loop => {
			if source.len() != 1 || target.len() != 1 || source != target {
				return Err(FerrosGrynnError::invalid_storage_file(
					"loop edge endpoints are inconsistent",
				));
			}
			EdgeTopology::directed(source, target)
		}
	})
}

fn decode_cross_level_side(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	count: u32,
	inline: &[Option<DiskAtomRef>; 2],
	graph_slot: Option<u32>,
	graph_overflow: Option<PageNo>,
	side: EndpointSide,
) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
	let count = usize::try_from(count)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("endpoint count exceeds usize"))?;
	let encoded = if let Some(first) = graph_overflow {
		let mut entries = Vec::with_capacity(count);
		let mut next = Some(first);
		let mut visited = BTreeSet::new();
		while entries.len() < count {
			let page_no = next.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("cross-level overflow ended too early")
			})?;
			if !visited.insert(page_no) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"cross-level overflow contains a cycle",
				));
			}
			let page = read_local_page(
				store,
				transaction,
				graph,
				DataFileKind::CrossLevelAdjacencyOverflow,
				page_no,
			)?;
			let (following, chunk) = decode_cross_level_overflow(page.as_page(), page_no)?;
			if chunk.iter().any(|entry| entry.side != side) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"cross-level overflow contains the wrong endpoint side",
				));
			}
			entries.extend(chunk);
			if entries.len() > count {
				return Err(FerrosGrynnError::invalid_storage_file(
					"cross-level overflow exceeds its declared count",
				));
			}
			next = following;
		}
		if next.is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"cross-level overflow contains entries beyond its declared count",
			));
		}
		entries
	} else {
		let slot = if count == 0 {
			return Ok(Vec::new());
		} else {
			graph_slot.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"inline cross-level endpoints have no graph slot",
				)
			})?
		};
		inline
			.iter()
			.take(count)
			.map(|atom| {
				atom.map(|atom| QualifiedDiskAtomRef {
					graph_slot: slot,
					side,
					atom,
				})
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("inline cross-level endpoint is absent")
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?
	};
	encoded
		.into_iter()
		.map(|endpoint| {
			if endpoint.graph_slot == 0 {
				local_atom_id(store, transaction, graph, endpoint.atom).map(EdgeEndpoint::Local)
			} else {
				resolve_qualified_atom(
					store,
					transaction,
					graph,
					endpoint.graph_slot,
					endpoint.atom,
				)
				.map(EdgeEndpoint::Nested)
			}
		})
		.collect()
}

fn resolve_qualified_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	graph_slot: u32,
	atom: DiskAtomRef,
) -> FerrosGrynnResult<QualifiedAtomId> {
	let mut scope = Vec::new();
	let target = find_qualified_atom(store, transaction, graph, graph_slot, atom, &mut scope)?
		.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"qualified endpoint references an unknown subgraph slot",
			)
		})?;
	QualifiedAtomId::new(scope, target)
}

fn find_qualified_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	graph_slot: u32,
	atom: DiskAtomRef,
	scope: &mut Vec<AtomId>,
) -> FerrosGrynnResult<Option<AtomId>> {
	for child in graph.children.values() {
		let container = local_atom_id(
			store,
			transaction,
			graph,
			child.parent_atom.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("nested graph has no parent atom")
			})?,
		)?;
		scope.push(container);
		if child.subgraph_slot == Some(graph_slot) {
			return local_atom_id(store, transaction, child, atom).map(Some);
		}
		if let Some(target) =
			find_qualified_atom(store, transaction, child, graph_slot, atom, scope)?
		{
			return Ok(Some(target));
		}
		scope.pop();
	}
	Ok(None)
}

const fn table_kind_accepts(table: TableKind, atom: AtomKind) -> bool {
	matches!(
		(table, atom),
		(TableKind::Any, _)
			| (TableKind::Normal, AtomKind::Node)
			| (TableKind::Relation, AtomKind::Edge)
	)
}

/// One graph generation ready to become reachable from the durable catalog.
struct BuiltGraphGeneration {
	catalog_entry: GraphCatalogEntry,
	file_entries: Vec<FileCatalogEntry>,
	registrations: Vec<DataFileRegistration>,
	superblock: GraphSuperblock,
	indexes: Vec<BuiltTableIndexBinding>,
}

struct BuiltGraphImage {
	catalog_entry: GraphCatalogEntry,
	superblock: GraphSuperblock,
	payloads: BTreeMap<DataFileKind, Vec<AlignedPage>>,
	indexes: Vec<BuiltScalarIndexImage>,
}

struct PlannedGraph {
	graph_id: GraphId,
	record_id: RecordId,
	kind: GraphKind,
	graph: Graph<Frozen>,
	parent_graph_id: Option<GraphId>,
	subgraph_slot: Option<u32>,
	parent_atom: Option<DiskAtomRef>,
	children: Vec<PlannedChild>,
}

struct PlannedChild {
	container: AtomId,
	slot: u32,
	graph: Box<PlannedGraph>,
}

struct BuiltScalarIndexImage {
	table: String,
	name: String,
	kind: TableIndexKind,
	fields: Vec<u32>,
	root: PageNo,
	pages: Vec<AlignedPage>,
}

struct BuiltTableIndexBinding {
	table: String,
	index: TableIndexCatalogEntry,
}

fn database_property_dictionary(
	database: &DatabaseCatalog,
) -> FerrosGrynnResult<PropertyDictionary> {
	let mut dictionary = PropertyDictionary::with_capacity(database.property_keys.len());
	for (expected, name) in database.property_keys.iter().enumerate() {
		let actual = dictionary.intern(name.clone())?;
		if usize::try_from(actual.get()).ok() != Some(expected) {
			return Err(FerrosGrynnError::invalid_storage_file("property dictionary is not dense"));
		}
	}
	Ok(dictionary)
}

fn allocate_graph_id(database: &mut DatabaseCatalog) -> FerrosGrynnResult<GraphId> {
	let graph_id = GraphId::new(database.next_graph_id);
	if graph_id == GraphId::SYSTEM {
		return Err(FerrosGrynnError::invalid_storage_file(
			"the durable graph identity clock is exhausted",
		));
	}
	database.next_graph_id = database.next_graph_id.checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("the durable graph identity clock is exhausted")
	})?;
	Ok(graph_id)
}

fn plan_graph_tree(
	database: &mut DatabaseCatalog,
	graph_id: GraphId,
	record_id: &RecordId,
	graph: &MaterializedGraph,
) -> FerrosGrynnResult<PlannedGraph> {
	let mut next_subgraph_slot = 1_u32;
	plan_graph(
		database,
		graph_id,
		record_id.clone(),
		graph.kind(),
		graph.clone().into_general()?,
		None,
		None,
		None,
		&mut next_subgraph_slot,
	)
}

fn plan_graph(
	database: &mut DatabaseCatalog,
	graph_id: GraphId,
	record_id: RecordId,
	kind: GraphKind,
	graph: Graph<Frozen>,
	parent_graph_id: Option<GraphId>,
	subgraph_slot: Option<u32>,
	parent_atom: Option<DiskAtomRef>,
	next_subgraph_slot: &mut u32,
) -> FerrosGrynnResult<PlannedGraph> {
	let mut nested = Vec::new();
	for id in GraphRead::node_ids(&graph)? {
		let id = id?;
		let node = graph.node(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested container disappeared")
		})?;
		if let Some(inner) = node.inner_graph() {
			nested.push((id, inner.clone()));
		}
	}
	for id in GraphRead::edge_ids(&graph)? {
		let id = id?;
		let edge = graph.edge(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("transition container disappeared")
		})?;
		if let Some(transition) = edge.transition_graph() {
			nested.push((id, transition.clone()));
		}
	}
	nested.sort_unstable_by_key(|(id, _graph)| *id);
	let mut children = Vec::with_capacity(nested.len());
	for (container, nested_graph) in nested {
		if *next_subgraph_slot == u32::MAX {
			return Err(FerrosGrynnError::graph_capacity_exceeded("subgraph"));
		}
		let slot = *next_subgraph_slot;
		*next_subgraph_slot = next_subgraph_slot
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("subgraph"))?;
		let child_id = allocate_graph_id(database)?;
		let child_record: RecordId =
			format!("{INTERNAL_NESTED_GRAPH_TABLE}:{}", child_id.get()).parse()?;
		let container_disk = DiskAtomRef::from_parts(
			container.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("nested container has an invalid kind")
			})?,
			container.slot(),
		)?;
		let child = plan_graph(
			database,
			child_id,
			child_record,
			GraphKind::Graph,
			nested_graph,
			Some(graph_id),
			Some(slot),
			Some(container_disk),
			next_subgraph_slot,
		)?;
		children.push(PlannedChild {
			container,
			slot,
			graph: Box::new(child),
		});
	}
	Ok(PlannedGraph {
		graph_id,
		record_id,
		kind,
		graph,
		parent_graph_id,
		subgraph_slot,
		parent_atom,
		children,
	})
}

fn graph_tree_ids(database: &DatabaseCatalog, root: GraphId) -> BTreeSet<GraphId> {
	let mut ids = BTreeSet::from([root]);
	loop {
		let before = ids.len();
		for graph in &database.graphs {
			if graph.parent_graph_id.is_some_and(|parent| ids.contains(&parent)) {
				ids.insert(graph.graph_id);
			}
		}
		if ids.len() == before {
			return ids;
		}
	}
}

fn build_graph_images(
	plan: &PlannedGraph,
	dictionary: &mut PropertyDictionary,
	tables: &[LocalCommitTable],
) -> FerrosGrynnResult<Vec<BuiltGraphImage>> {
	let mut images = vec![build_graph_image(plan, dictionary, tables)?];
	for child in &plan.children {
		images.extend(build_graph_images(&child.graph, dictionary, tables)?);
	}
	Ok(images)
}

fn encode_tables(
	tables: &[LocalCommitTable],
	database: &DatabaseCatalog,
	previous: &DatabaseCatalog,
	new_indexes: &[BuiltTableIndexBinding],
) -> FerrosGrynnResult<Vec<TableCatalogEntry>> {
	let graph_ids = database
		.graphs
		.iter()
		.map(|entry| (entry.record_id.as_str(), entry.graph_id))
		.collect::<BTreeMap<_, _>>();
	let mut encoded = Vec::with_capacity(tables.len());
	for table in tables {
		let graph_id =
			graph_ids.get(table.graph.to_string().as_str()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(format!(
					"table '{}' references a graph absent from the durable catalog",
					table.name
				))
			})?;
		let mut indexes = Vec::with_capacity(table.indexes.len());
		for definition in &table.indexes {
			let candidate = new_indexes
				.iter()
				.find(|candidate| {
					candidate.table == table.name && candidate.index.name == definition.name
				})
				.map(|candidate| &candidate.index)
				.or_else(|| {
					previous.tables.iter().find(|existing| existing.name == table.name).and_then(
						|existing| {
							existing.indexes.iter().find(|index| index.name == definition.name)
						},
					)
				})
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(format!(
						"scalar index '{}.{}' has no built physical generation",
						table.name, definition.name
					))
				})?;
			if candidate.kind != definition.kind || candidate.fields != definition.fields {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' physical definition is stale",
					table.name, definition.name
				)));
			}
			indexes.push(candidate.clone());
		}
		indexes.sort_unstable_by(|left, right| left.name.cmp(&right.name));
		encoded.push(TableCatalogEntry {
			name: table.name.clone(),
			graph_id,
			kind: table.kind,
			fields: table
				.fields
				.iter()
				.map(|field| TableFieldCatalogEntry {
					name: field.name.clone(),
					value_type: field.value_type.clone(),
					property_key: field.property_key,
				})
				.collect(),
			indexes,
		});
	}
	encoded.sort_unstable_by(|left, right| left.name.cmp(&right.name));
	Ok(encoded)
}

fn load_database(
	root: &Path,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	commit_ts: u64,
	database: &DatabaseCatalog,
	files: &FileCatalog,
) -> FerrosGrynnResult<LocalDatabaseSnapshot> {
	let dictionary = Arc::new(database_property_dictionary(database)?);
	let mut graphs = Vec::with_capacity(
		database.graphs.iter().filter(|entry| entry.parent_graph_id.is_none()).count(),
	);
	for entry in database.graphs.iter().filter(|entry| entry.parent_graph_id.is_none()) {
		let record_id: RecordId = entry.record_id.parse()?;
		graphs.push(LocalSnapshotGraph {
			record_id: record_id.clone(),
			graph: Arc::new(open_local_graph(
				root,
				manager,
				transaction,
				entry,
				database,
				files,
				&database.tables,
				record_id,
				dictionary.clone(),
			)?),
		});
	}
	let graph_records = database
		.graphs
		.iter()
		.map(|entry| (entry.graph_id, entry.record_id.as_str()))
		.collect::<BTreeMap<_, _>>();
	let mut tables = Vec::with_capacity(database.tables.len());
	for table in &database.tables {
		let graph = graph_records.get(&table.graph_id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("table references an unknown graph identity")
		})?;
		tables.push(LocalCommitTable {
			name: table.name.clone(),
			graph: graph.parse()?,
			kind: table.kind,
			fields: table
				.fields
				.iter()
				.map(|field| LocalCommitTableField {
					name: field.name.clone(),
					value_type: field.value_type.clone(),
					property_key: field.property_key,
				})
				.collect(),
			indexes: table
				.indexes
				.iter()
				.map(|index| LocalCommitTableIndex {
					name: index.name.clone(),
					kind: index.kind,
					fields: index.fields.clone(),
				})
				.collect(),
		});
	}
	Ok(LocalDatabaseSnapshot {
		commit_ts,
		property_dictionary: dictionary,
		graphs,
		tables,
	})
}

fn open_local_graph(
	root: &Path,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	entry: &GraphCatalogEntry,
	database: &DatabaseCatalog,
	files: &FileCatalog,
	tables: &[TableCatalogEntry],
	record_id: RecordId,
	property_dictionary: Arc<PropertyDictionary>,
) -> FerrosGrynnResult<LocalGraph> {
	let mut children = BTreeMap::new();
	for child in
		database.graphs.iter().filter(|child| child.parent_graph_id == Some(entry.graph_id))
	{
		let slot = child.subgraph_slot.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested graph has no subgraph slot")
		})?;
		let child_record: RecordId = child.record_id.parse()?;
		let graph = Arc::new(open_local_graph(
			root,
			manager,
			transaction,
			child,
			database,
			files,
			tables,
			child_record,
			property_dictionary.clone(),
		)?);
		if children.insert(slot, graph).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"nested graph slots are not unique",
			));
		}
	}
	let paths = graph_file_paths(root, entry, files)?;
	let file_ids = graph_file_ids(entry, files)?;
	let superblock_path = paths.get(&DataFileKind::GraphSuperblock).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph superblock file is missing")
	})?;
	let superblock = select_graph_superblock(superblock_path)?;
	if superblock.graph_id != entry.graph_id
		|| superblock.node_count > superblock.node_capacity
		|| superblock.edge_count > superblock.edge_capacity
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph superblock disagrees with its catalog or capacities",
		));
	}
	let labels_file = open_graph_file(&paths, DataFileKind::Labels)?;
	if labels_file.page_count().checked_mul(PAGE_SIZE as u64) != Some(superblock.labels_size_bytes)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"label file length disagrees with GraphSuperblock",
		));
	}
	let labels_id = file_ids
		.get(&DataFileKind::Labels)
		.copied()
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label file identity is missing"))?;
	let labels = decode_label_dictionary(|page_no, output| {
		let page = manager.read_page(
			transaction,
			super::types::PageKey::new(entry.graph_id, labels_id, page_no),
		)?;
		output.as_bytes_mut().copy_from_slice(page.as_bytes());
		Ok(())
	})?;
	let mut scalar_indexes = BTreeMap::<String, Vec<LocalScalarIndex>>::new();
	for table in tables.iter().filter(|table| table.graph_id == entry.graph_id) {
		for index in &table.indexes {
			if !files
				.files
				.iter()
				.any(|file| file.graph_id == entry.graph_id && file.file_id == index.file_id)
			{
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' references an unregistered file",
					table.name, index.name
				)));
			}
			let fields = index
				.fields
				.iter()
				.map(|property_key| {
					table
						.fields
						.iter()
						.find(|field| field.property_key == *property_key)
						.map(|field| (PropertyKeyId::new(*property_key), field.value_type.clone()))
						.ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"scalar index references an unknown table field",
							)
						})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			scalar_indexes.entry(table.name.clone()).or_default().push(LocalScalarIndex {
				name: index.name.clone(),
				kind: index.kind,
				fields,
				file_id: index.file_id,
				root: index.root,
			});
		}
	}
	let graph = LocalGraph {
		graph_id: entry.graph_id,
		subgraph_slot: entry.subgraph_slot,
		parent_atom: entry.parent_atom,
		reference: GraphRef::new(record_id, entry.kind, GraphStorageMode::DiskBacked),
		identity: uuid::Uuid::from_bytes(entry.identity).into(),
		revision: entry.revision,
		primary_index_root: entry.primary_index_root,
		properties: entry.properties,
		superblock,
		files: file_ids,
		labels,
		property_dictionary,
		decode_context: Mutex::new(None),
		scalar_indexes,
		children,
	};
	validate_local_subgraphs(manager, transaction, &graph)?;
	Ok(graph)
}

fn validate_local_subgraphs(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	graph: &LocalGraph,
) -> FerrosGrynnResult<()> {
	let file_id = graph.file_id(DataFileKind::Subgraphs)?;
	let mut page_no = PageNo::new(0);
	let mut visited = BTreeSet::new();
	let mut entries = BTreeMap::new();
	loop {
		if !visited.insert(page_no) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"subgraph directory contains a page cycle",
			));
		}
		let page = manager
			.read_page(transaction, super::types::PageKey::new(graph.graph_id, file_id, page_no))?;
		let (next, chunk) = decode_subgraph_directory(page.as_page(), page_no)?;
		for entry in chunk {
			if entry.slot == 0 || entries.insert(entry.slot, entry).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"subgraph directory contains a reserved or duplicate slot",
				));
			}
		}
		let Some(next) = next else {
			break;
		};
		page_no = next;
	}
	if entries.len() != graph.children.len()
		|| u64::try_from(entries.len()).ok() != Some(graph.superblock.subgraph_count)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"subgraph directory disagrees with the catalog or superblock",
		));
	}
	for (slot, child) in &graph.children {
		let entry = entries.get(slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested graph is absent from its directory")
		})?;
		if entry.node_capacity != child.superblock.node_capacity
			|| entry.edge_capacity != child.superblock.edge_capacity
			|| u64::from(entry.node_count) != child.superblock.node_count
			|| u64::from(entry.edge_count) != child.superblock.edge_count
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"subgraph directory counts disagree with the nested graph",
			));
		}
		let parent = child.parent_atom.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested graph has no parent atom")
		})?;
		let (hot_page, hot_slot) = hot_slot_address(parent.slot())?;
		match parent.kind() {
			AtomKind::Node => {
				let page = manager
					.read_page(transaction, graph.page_key(DataFileKind::NodesHot, hot_page)?)?;
				let value = read_node_hot_slot(page.as_page(), hot_page, hot_slot)?;
				if value.generation != entry.generation || value.subgraph_slot != Some(*slot) {
					return Err(FerrosGrynnError::invalid_storage_file(
						"node nested-graph binding disagrees with SubgraphDir",
					));
				}
			}
			AtomKind::Edge => {
				let page = manager
					.read_page(transaction, graph.page_key(DataFileKind::EdgesHot, hot_page)?)?;
				let value = read_edge_hot_slot(page.as_page(), hot_page, hot_slot)?;
				if value.generation != entry.generation || value.transition_graph != Some(*slot) {
					return Err(FerrosGrynnError::invalid_storage_file(
						"edge transition binding disagrees with SubgraphDir",
					));
				}
			}
		}
	}
	Ok(())
}

fn graph_file_ids(
	graph: &GraphCatalogEntry,
	files: &FileCatalog,
) -> FerrosGrynnResult<BTreeMap<DataFileKind, DataFileId>> {
	let mut ids = BTreeMap::new();
	for entry in files.files.iter().filter(|entry| entry.graph_id == graph.graph_id) {
		let kind = entry.file_id.kind().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph file has an unknown kind")
		})?;
		if kind == DataFileKind::Index && entry.file_id != graph.primary_index_file_id {
			continue;
		}
		if ids.insert(kind, entry.file_id).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"graph catalog contains duplicate file kinds",
			));
		}
	}
	for (kind, _name) in GRAPH_FILE_KINDS {
		if !ids.contains_key(&kind) {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"graph catalog is missing {kind:?}"
			)));
		}
	}
	Ok(ids)
}

struct PropertyStoreBuilder {
	next_page: u64,
	current: Option<PropertyPageBuilder>,
	current_page: Option<PageNo>,
	pages: Vec<(PageNo, AlignedPage)>,
}

impl PropertyStoreBuilder {
	fn new() -> Self {
		Self {
			next_page: 0,
			current: None,
			current_page: None,
			pages: Vec::new(),
		}
	}

	fn insert(&mut self, record: &[u8]) -> FerrosGrynnResult<PropertyLocation> {
		let length = u32::try_from(record.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("property record exceeds u32")
		})?;
		if record.len() > MAX_INLINE_PROPERTY_BYTES {
			let first_page = PageNo::new(self.next_page);
			let pages = encode_large_property(first_page, PageLsn::ZERO, record)?;
			self.next_page = self
				.next_page
				.checked_add(u64::try_from(pages.len()).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("large property page count exceeds u64")
				})?)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("property page clock is exhausted")
				})?;
			for (index, page) in pages.into_iter().enumerate() {
				let page_no = first_page
					.get()
					.checked_add(u64::try_from(index).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("property page index exceeds u64")
					})?)
					.map(PageNo::new)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("property page number overflows")
					})?;
				self.pages.push((page_no, page));
			}
			return Ok(PropertyLocation::Large {
				first_page,
				length,
			});
		}

		if self.current.is_none() {
			self.start_page()?;
		}
		if let Some(slot) = self
			.current
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("property page is absent"))?
			.try_insert(record)?
		{
			return Ok(PropertyLocation::Inline {
				page_no: self.current_page.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("property page number is absent")
				})?,
				slot,
				length,
			});
		}
		self.finish_current()?;
		self.start_page()?;
		let slot = self
			.current
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("property page is absent"))?
			.try_insert(record)?
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("inline property does not fit an empty page")
			})?;
		Ok(PropertyLocation::Inline {
			page_no: self.current_page.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("property page number is absent")
			})?,
			slot,
			length,
		})
	}

	fn start_page(&mut self) -> FerrosGrynnResult<()> {
		let page_no = PageNo::new(self.next_page);
		self.next_page = self.next_page.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("property page clock is exhausted")
		})?;
		self.current = Some(PropertyPageBuilder::new(page_no, PageLsn::ZERO));
		self.current_page = Some(page_no);
		Ok(())
	}

	fn finish_current(&mut self) -> FerrosGrynnResult<()> {
		if let Some(builder) = self.current.take() {
			let page_no = self.current_page.take().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("property page number is absent")
			})?;
			self.pages.push((page_no, builder.finish()?));
		}
		Ok(())
	}

	fn finish(mut self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.finish_current()?;
		self.pages.sort_unstable_by_key(|(page_no, _page)| *page_no);
		for (index, (page_no, _page)) in self.pages.iter().enumerate() {
			if page_no.get()
				!= u64::try_from(index).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("property page index exceeds u64")
				})? {
				return Err(FerrosGrynnError::invalid_storage_file(
					"property pages are not physically contiguous",
				));
			}
		}
		Ok(self.pages.into_iter().map(|(_page_no, page)| page).collect())
	}
}

#[derive(Default)]
struct OverflowStoreBuilder {
	pages: Vec<AlignedPage>,
}

impl OverflowStoreBuilder {
	fn append(&mut self, entries: &[DiskAtomRef]) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = PageNo::new(u64::try_from(self.pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("adjacency page count exceeds u64")
		})?);
		let chunks = entries.chunks(OVERFLOW_REFS_PER_PAGE).collect::<Vec<_>>();
		for (index, chunk) in chunks.iter().enumerate() {
			let page_no = first
				.get()
				.checked_add(u64::try_from(index).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("adjacency page index exceeds u64")
				})?)
				.map(PageNo::new)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("adjacency page number overflows")
				})?;
			let next = if index + 1 == chunks.len() {
				None
			} else {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("adjacency link overflows")
				})?))
			};
			self.pages.push(encode_adjacency_overflow(page_no, PageLsn::ZERO, next, chunk)?);
		}
		Ok(Some(first))
	}
}

#[derive(Default)]
struct NodeAdjacency {
	outgoing: Vec<DiskAtomRef>,
	incoming: Vec<DiskAtomRef>,
	undirected: Vec<DiskAtomRef>,
}

struct EncodedTopology {
	code: EdgeTopologyCode,
	source: Vec<EncodedEndpoint>,
	target: Vec<EncodedEndpoint>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct EncodedEndpoint {
	graph_slot: u32,
	atom: DiskAtomRef,
}

#[derive(Default)]
struct CrossLevelOverflowStoreBuilder {
	pages: Vec<AlignedPage>,
}

impl CrossLevelOverflowStoreBuilder {
	fn append(
		&mut self,
		entries: &[EncodedEndpoint],
		side: EndpointSide,
	) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = PageNo::new(u64::try_from(self.pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("cross-level page count exceeds u64")
		})?);
		let encoded = entries
			.iter()
			.map(|entry| QualifiedDiskAtomRef {
				graph_slot: entry.graph_slot,
				side,
				atom: entry.atom,
			})
			.collect::<Vec<_>>();
		let chunks = encoded.chunks(CROSS_LEVEL_REFS_PER_PAGE).collect::<Vec<_>>();
		for (index, chunk) in chunks.iter().enumerate() {
			let page_no = PageNo::new(
				first
					.get()
					.checked_add(u64::try_from(index).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("cross-level page index exceeds u64")
					})?)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("cross-level page number overflows")
					})?,
			);
			let next = if index + 1 < chunks.len() {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("cross-level page link overflows")
				})?))
			} else {
				None
			};
			self.pages.push(encode_cross_level_overflow(page_no, PageLsn::ZERO, next, chunk)?);
		}
		Ok(Some(first))
	}
}

fn build_graph_image(
	plan: &PlannedGraph,
	dictionary: &mut PropertyDictionary,
	tables: &[LocalCommitTable],
) -> FerrosGrynnResult<BuiltGraphImage> {
	let graph_id = plan.graph_id;
	let record_id = &plan.record_id;
	let graph = &plan.graph;
	let node_ids = GraphRead::node_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let edge_ids = GraphRead::edge_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut disk_refs = BTreeMap::new();
	let mut context = EncodeContext::new(dictionary.clone());
	let mut table_names = BTreeSet::new();
	let mut node_labels = BTreeSet::new();
	let mut edge_labels = BTreeSet::new();
	for id in node_ids.iter().chain(&edge_ids) {
		let atom = graph
			.atom_ref(*id)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph atom disappeared"))?;
		let disk = DiskAtomRef::from_parts(atom.kind(), id.slot())?;
		context.register_atom(atom.clone(), disk)?;
		table_names.insert(atom.id().table().to_owned());
		disk_refs.insert(*id, disk);
	}
	for table in &table_names {
		context.register_table_name(table.clone().into())?;
	}
	for id in &node_ids {
		let node = graph
			.node(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph node disappeared"))?;
		if let Some(label) = node.label() {
			node_labels.insert(label.to_owned());
		}
	}
	for id in &edge_ids {
		let edge = graph
			.edge(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph edge disappeared"))?;
		if let Some(label) = edge.label() {
			edge_labels.insert(label.to_owned());
		}
	}

	let mut index_entries = Vec::with_capacity(node_ids.len() + edge_ids.len());
	for id in node_ids.iter().chain(&edge_ids) {
		let atom = graph
			.atom_ref(*id)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph atom disappeared"))?;
		let key = CanonicalCodec::encode(
			StorageValue::from_runtime(Value::Record(Box::new(atom.id().clone())), &mut context)?
				.as_ref(),
		)?;
		index_entries.push(PrimaryIndexEntry {
			key,
			atom: disk_refs.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("atom has no assigned disk reference")
			})?,
		});
	}
	index_entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	let primary = build_primary_index(&index_entries, PageLsn::ZERO)?;

	let mut properties = PropertyStoreBuilder::new();
	let graph_properties =
		encode_optional_properties(graph.properties(), &mut context, &mut properties)?;
	let mut node_properties = BTreeMap::new();
	for id in &node_ids {
		let node = graph.node(*id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph node disappeared during property encoding",
			)
		})?;
		node_properties.insert(
			*id,
			encode_optional_properties(node.properties(), &mut context, &mut properties)?,
		);
	}
	let mut edge_properties = BTreeMap::new();
	for id in &edge_ids {
		let edge = graph.edge(*id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph edge disappeared during property encoding",
			)
		})?;
		edge_properties.insert(
			*id,
			encode_optional_properties(edge.properties(), &mut context, &mut properties)?,
		);
	}
	let property_pages = properties.finish()?;
	let indexes = build_scalar_indexes(record_id, graph, tables, &disk_refs, &mut context)?;

	let node_label_ids = dense_ids(node_labels)?;
	let edge_label_ids = dense_ids(edge_labels)?;
	let table_ids = context
		.table_names()
		.enumerate()
		.map(|(id, name)| {
			u32::try_from(id)
				.map(|id| (name.as_str().to_owned(), id))
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("table id exceeds u32"))
		})
		.collect::<FerrosGrynnResult<BTreeMap<_, _>>>()?;
	let labels = LabelDictionary {
		node_labels: node_label_ids.keys().cloned().collect(),
		edge_labels: edge_label_ids.keys().cloned().collect(),
		property_keys: context
			.property_dictionary()
			.iter()
			.map(|(_id, name)| name.as_str().to_owned())
			.collect(),
		table_names: context.table_names().map(|name| name.as_str().to_owned()).collect(),
		time_zones: context.time_zone_names().map(|name| name.as_str().to_owned()).collect(),
	};
	let label_pages = encode_label_dictionary(&labels, PageLsn::ZERO)?;
	*dictionary = context.property_dictionary().clone();

	let mut node_adjacency =
		node_ids.iter().map(|id| (*id, NodeAdjacency::default())).collect::<BTreeMap<_, _>>();
	let mut encoded_topologies = BTreeMap::new();
	for id in &edge_ids {
		let edge = graph
			.edge(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph edge disappeared"))?;
		let topology = encode_topology(edge.topology(), &disk_refs, plan)?;
		record_node_adjacency(
			edge.topology(),
			disk_refs.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge has no assigned disk reference")
			})?,
			&mut node_adjacency,
		)?;
		encoded_topologies.insert(*id, topology);
	}

	let node_capacity = u64::try_from(graph.slot_capacity(AtomKind::Node)).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("node slot capacity exceeds u64")
	})?;
	let edge_capacity = u64::try_from(graph.slot_capacity(AtomKind::Edge)).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("edge slot capacity exceeds u64")
	})?;
	let node_page_count = page_count_for_slots(node_capacity)?;
	let edge_page_count = page_count_for_slots(edge_capacity)?;
	let mut node_pages = make_hot_pages(node_page_count, new_node_hot_page)?;
	let mut edge_pages = make_hot_pages(edge_page_count, new_edge_hot_page)?;
	for slot in 0..graph.slot_capacity(AtomKind::Node) {
		let generation = graph.slot_generation(AtomKind::Node, slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node slot generation is absent")
		})?;
		let slot = u64::try_from(slot)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("node slot exceeds u64"))?;
		let (page_no, within_page) = hot_slot_address(slot)?;
		let page_index = checked_page_index(page_no, node_pages.len())?;
		write_node_hot_slot(
			&mut node_pages[page_index],
			within_page,
			&NodeHotSlot::tombstone(generation),
		)?;
	}
	for slot in 0..graph.slot_capacity(AtomKind::Edge) {
		let generation = graph.slot_generation(AtomKind::Edge, slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge slot generation is absent")
		})?;
		let slot = u64::try_from(slot)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge slot exceeds u64"))?;
		let (page_no, within_page) = hot_slot_address(slot)?;
		let page_index = checked_page_index(page_no, edge_pages.len())?;
		write_edge_hot_slot(
			&mut edge_pages[page_index],
			within_page,
			&EdgeHotSlot::tombstone(generation),
		)?;
	}
	let mut overflow = OverflowStoreBuilder::default();
	let mut cross_level_overflow = CrossLevelOverflowStoreBuilder::default();
	for id in &node_ids {
		let node = graph
			.node(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph node disappeared"))?;
		let adjacency = node_adjacency.get(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node adjacency was not initialized")
		})?;
		let subgraph_slot =
			plan.children.iter().find(|child| child.container == *id).map(|child| child.slot);
		let value = NodeHotSlot {
			generation: id.generation(),
			subgraph_slot,
			label_id: node
				.label()
				.map(|label| {
					node_label_ids.get(label).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("node label was not interned")
					})
				})
				.transpose()?,
			table_id: Some(table_ids.get(node.record_id().table()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("node table was not interned")
			})?),
			out_count: checked_u32(adjacency.outgoing.len(), "outgoing adjacency")?,
			in_count: checked_u32(adjacency.incoming.len(), "incoming adjacency")?,
			undirected_count: checked_u32(adjacency.undirected.len(), "undirected adjacency")?,
			flags: if subgraph_slot.is_some() {
				NodeSlotFlags::META
			} else {
				NodeSlotFlags::empty()
			},
			out_overflow: overflow.append(adjacency.outgoing.get(4..).unwrap_or(&[]))?,
			in_overflow: overflow.append(adjacency.incoming.get(4..).unwrap_or(&[]))?,
			undirected_overflow: overflow.append(adjacency.undirected.get(2..).unwrap_or(&[]))?,
			properties: node_properties.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("node property location is absent")
			})?,
			out_inline: inline_refs(&adjacency.outgoing),
			in_inline: inline_refs(&adjacency.incoming),
			undirected_inline: inline_refs(&adjacency.undirected),
		};
		let (page_no, slot) = hot_slot_address(id.slot())?;
		let page_index = checked_page_index(page_no, node_pages.len())?;
		write_node_hot_slot(&mut node_pages[page_index], slot, &value)?;
	}
	for (index, page) in node_pages.iter_mut().enumerate() {
		page.seal()?;
		page.validate(PageNo::new(u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("node page index exceeds u64")
		})?))?;
	}
	for id in &edge_ids {
		let edge = graph
			.edge(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph edge disappeared"))?;
		let topology = encoded_topologies.get(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge topology was not encoded")
		})?;
		let weight = edge
			.weight()
			.map(|weight| {
				weight.as_f64().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("edge weight cannot be represented as f64")
				})
			})
			.transpose()?;
		let cross_level =
			topology.source.iter().chain(&topology.target).any(|endpoint| endpoint.graph_slot != 0);
		let source = encode_hot_endpoint_side(
			&topology.source,
			EndpointSide::Source,
			cross_level,
			&mut overflow,
			&mut cross_level_overflow,
		)?;
		let target = encode_hot_endpoint_side(
			&topology.target,
			EndpointSide::Target,
			cross_level,
			&mut overflow,
			&mut cross_level_overflow,
		)?;
		let transition_graph =
			plan.children.iter().find(|child| child.container == *id).map(|child| child.slot);
		let mut flags = EdgeSlotFlags::empty();
		if transition_graph.is_some() {
			flags |= EdgeSlotFlags::META;
		}
		if cross_level {
			flags |= EdgeSlotFlags::CROSS_LEVEL;
		}
		let value = EdgeHotSlot {
			topology: topology.code,
			flags,
			has_transition: transition_graph.is_some(),
			generation: id.generation(),
			source_count: checked_u32(topology.source.len(), "edge sources")?,
			target_count: checked_u32(topology.target.len(), "edge targets")?,
			label_id: edge
				.label()
				.map(|label| {
					edge_label_ids.get(label).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("edge label was not interned")
					})
				})
				.transpose()?,
			table_id: Some(table_ids.get(edge.record_id().table()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge table was not interned")
			})?),
			weight,
			transition_graph,
			source_graph: source.graph_slot,
			target_graph: target.graph_slot,
			properties: edge_properties.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge property location is absent")
			})?,
			source_overflow: source.local_overflow,
			target_overflow: target.local_overflow,
			source_graph_overflow: source.graph_overflow,
			target_graph_overflow: target.graph_overflow,
			source_inline: source.inline,
			target_inline: target.inline,
		};
		let (page_no, slot) = hot_slot_address(id.slot())?;
		let page_index = checked_page_index(page_no, edge_pages.len())?;
		write_edge_hot_slot(&mut edge_pages[page_index], slot, &value)?;
	}
	for (index, page) in edge_pages.iter_mut().enumerate() {
		page.seal()?;
		page.validate(PageNo::new(u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("edge page index exceeds u64")
		})?))?;
	}

	let statistics = graph_statistics(&node_adjacency, edge_ids.len())?;
	let statistic_pages = vec![encode_graph_statistics(statistics, PageLsn::ZERO)?];
	let subgraph_entries = plan
		.children
		.iter()
		.map(|child| {
			Ok(SubgraphEntry {
				slot: child.slot,
				generation: child.container.generation(),
				node_capacity: u64::try_from(child.graph.graph.slot_capacity(AtomKind::Node))
					.map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("nested node capacity exceeds u64")
					})?,
				edge_capacity: u64::try_from(child.graph.graph.slot_capacity(AtomKind::Edge))
					.map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("nested edge capacity exceeds u64")
					})?,
				node_count: checked_u32(
					GraphRead::node_count(&child.graph.graph)?,
					"nested node count",
				)?,
				edge_count: checked_u32(
					GraphRead::edge_count(&child.graph.graph)?,
					"nested edge count",
				)?,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let subgraph_pages = encode_subgraph_pages(&subgraph_entries)?;
	let freelist_pages = vec![encode_freelist(PageNo::new(0), PageLsn::ZERO, 0, None, &[])?];
	let graph_name = record_id.id().to_string();
	let now = unix_time_ms()?;
	let superblock = GraphSuperblock {
		graph_id,
		generation: CheckpointGeneration::ZERO,
		backfilled_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
		created_at_ms: now,
		updated_at_ms: now,
		node_capacity,
		edge_capacity,
		node_count: u64::try_from(node_ids.len())
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds u64"))?,
		edge_count: u64::try_from(edge_ids.len())
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds u64"))?,
		subgraph_capacity: subgraph_entries.last().map_or(0, |entry| u64::from(entry.slot) + 1),
		subgraph_count: u64::try_from(subgraph_entries.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("subgraph count exceeds u64")
		})?,
		node_hot_pages: u64::try_from(node_pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("node page count exceeds u64")
		})?,
		edge_hot_pages: u64::try_from(edge_pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("edge page count exceeds u64")
		})?,
		properties_pages: u64::try_from(property_pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("property page count exceeds u64")
		})?,
		labels_size_bytes: u64::try_from(label_pages.len())
			.ok()
			.and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label file size overflows"))?,
		flags: GraphSuperblockFlags::empty(),
		schema_root_page: PageNo::NULL,
		stats_root_page: PageNo::new(0),
		graph_name,
	};

	let mut payloads = BTreeMap::new();
	payloads.insert(DataFileKind::GraphSuperblock, vec![superblock.encode()?]);
	payloads.insert(DataFileKind::NodesHot, node_pages);
	payloads.insert(DataFileKind::EdgesHot, edge_pages);
	payloads.insert(DataFileKind::AdjacencyOverflow, overflow.pages);
	payloads.insert(DataFileKind::CrossLevelAdjacencyOverflow, cross_level_overflow.pages);
	payloads.insert(DataFileKind::Properties, property_pages);
	payloads.insert(DataFileKind::Subgraphs, subgraph_pages);
	payloads.insert(DataFileKind::Labels, label_pages);
	payloads.insert(DataFileKind::Freelist, freelist_pages);
	payloads.insert(DataFileKind::Statistics, statistic_pages);
	payloads.insert(DataFileKind::Index, primary.pages);

	let identity = graph.identity();
	Ok(BuiltGraphImage {
		catalog_entry: GraphCatalogEntry {
			graph_id,
			parent_graph_id: plan.parent_graph_id,
			subgraph_slot: plan.subgraph_slot,
			parent_atom: plan.parent_atom,
			record_id: record_id.to_string(),
			name: record_id.id().to_string(),
			kind: plan.kind,
			created_lsn: Lsn::ZERO,
			identity: *identity.as_inner().as_bytes(),
			revision: graph.revision(),
			primary_index_root: primary.root,
			primary_index_file_id: DataFileId::for_kind(DataFileKind::Index),
			properties: graph_properties,
		},
		superblock,
		payloads,
		indexes,
	})
}

fn build_scalar_indexes(
	record_id: &RecordId,
	graph: &Graph<Frozen>,
	tables: &[LocalCommitTable],
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<BuiltScalarIndexImage>> {
	let mut built = Vec::new();
	for table in tables.iter().filter(|table| table.graph == *record_id) {
		for index in &table.indexes {
			let fields = index
				.fields
				.iter()
				.map(|property_key| {
					table
						.fields
						.iter()
						.find(|field| field.property_key == *property_key)
						.map(|field| (PropertyKeyId::new(*property_key), &field.value_type))
						.ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"scalar index references a field absent from its table",
							)
						})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let mut grouped = BTreeMap::<Vec<u8>, Vec<DiskAtomRef>>::new();
			for kind in [AtomKind::Node, AtomKind::Edge] {
				if !table_kind_accepts(table.kind, kind) {
					continue;
				}
				let ids = match kind {
					AtomKind::Node => {
						GraphRead::node_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?
					}
					AtomKind::Edge => {
						GraphRead::edge_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?
					}
				};
				for id in ids {
					let (atom_record, properties) = match kind {
						AtomKind::Node => {
							let node = graph.node(id).ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"indexed node disappeared during graph encoding",
								)
							})?;
							(node.record_id(), node.properties())
						}
						AtomKind::Edge => {
							let edge = graph.edge(id).ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"indexed edge disappeared during graph encoding",
								)
							})?;
							(edge.record_id(), edge.properties())
						}
					};
					if atom_record.table() != table.name {
						continue;
					}
					let missing = Value::None;
					let components = fields
						.iter()
						.map(|(property_key, value_type)| {
							(properties.get(*property_key).unwrap_or(&missing), *value_type)
						})
						.collect::<Vec<_>>();
					let key = encode_index_key(&components, context)?;
					let atom = disk_refs.get(&id).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"indexed atom has no assigned disk reference",
						)
					})?;
					grouped.entry(key).or_default().push(atom);
				}
			}
			let entries = grouped
				.into_iter()
				.map(|(key, mut atoms)| {
					atoms.sort_unstable();
					atoms.dedup();
					ScalarIndexEntry {
						key,
						atoms,
					}
				})
				.collect::<Vec<_>>();
			let mode = match index.kind {
				TableIndexKind::Standard => ScalarIndexMode::Standard,
				TableIndexKind::Unique => ScalarIndexMode::Unique,
			};
			let tree = build_scalar_index(&entries, mode, PageLsn::ZERO)?;
			built.push(BuiltScalarIndexImage {
				table: table.name.clone(),
				name: index.name.clone(),
				kind: index.kind,
				fields: index.fields.clone(),
				root: tree.root,
				pages: tree.pages,
			});
		}
	}
	built.sort_unstable_by(|left, right| {
		(left.table.as_str(), left.name.as_str()).cmp(&(right.table.as_str(), right.name.as_str()))
	});
	Ok(built)
}

fn write_new_graph_generation(
	root: &Path,
	image: BuiltGraphImage,
	next_file_instance_id: &mut u64,
) -> FerrosGrynnResult<BuiltGraphGeneration> {
	let graph_id = image.catalog_entry.graph_id;
	let generation_name = Uuid::now_v7().to_string();
	let relative_directory = format!("graphs/{:016x}/{generation_name}", graph_id.get());
	let absolute_directory =
		root.join(relative_directory.replace('/', std::path::MAIN_SEPARATOR_STR));
	fs::create_dir_all(&absolute_directory)
		.map_err(|source| storage_io("create graph generation directory", source))?;
	let mut catalog_entry = image.catalog_entry;
	let mut payloads = image.payloads;
	let mut file_entries = Vec::with_capacity(GRAPH_FILE_KINDS.len() + image.indexes.len());
	let mut registrations = Vec::with_capacity(GRAPH_FILE_KINDS.len() + image.indexes.len());
	for (kind, filename) in GRAPH_FILE_KINDS {
		let file_id = allocate_file_id(next_file_instance_id, kind)?;
		if kind == DataFileKind::Index {
			catalog_entry.primary_index_file_id = file_id;
		}
		let relative = format!("{relative_directory}/{filename}");
		let catalog_path = CatalogPath::parse(&relative)?;
		let absolute = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
		let pages = payloads.remove(&kind).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph generation payload is missing")
		})?;
		write_pages(&absolute, &pages)?;
		let role = if kind == DataFileKind::GraphSuperblock {
			DataFileRole::GraphSuperblock
		} else {
			DataFileRole::WalBackfilled
		};
		file_entries.push(FileCatalogEntry {
			graph_id,
			file_id,
			role,
			path: catalog_path,
		});
		registrations.push(DataFileRegistration {
			graph_id,
			file_id,
			role,
			path: absolute,
		});
	}
	if !payloads.is_empty() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph generation contains an unregistered base payload",
		));
	}
	let mut indexes = Vec::with_capacity(image.indexes.len());
	for (ordinal, index) in image.indexes.into_iter().enumerate() {
		let file_id = allocate_file_id(next_file_instance_id, DataFileKind::Index)?;
		let filename = format!("scalar_{ordinal:04}.fgidx");
		let relative = format!("{relative_directory}/{filename}");
		let catalog_path = CatalogPath::parse(&relative)?;
		let absolute = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
		write_pages(&absolute, &index.pages)?;
		file_entries.push(FileCatalogEntry {
			graph_id,
			file_id,
			role: DataFileRole::WalBackfilled,
			path: catalog_path,
		});
		registrations.push(DataFileRegistration {
			graph_id,
			file_id,
			role: DataFileRole::WalBackfilled,
			path: absolute,
		});
		indexes.push(BuiltTableIndexBinding {
			table: index.table,
			index: TableIndexCatalogEntry {
				name: index.name,
				kind: index.kind,
				fields: index.fields,
				file_id,
				root: index.root,
			},
		});
	}
	Ok(BuiltGraphGeneration {
		catalog_entry,
		file_entries,
		registrations,
		superblock: image.superblock,
		indexes,
	})
}

fn prepare_in_place_image(
	root: &Path,
	current: &GraphCatalogEntry,
	files: &FileCatalog,
	image: &mut BuiltGraphImage,
) -> FerrosGrynnResult<bool> {
	// ponytail: scalar indexes use immutable bulk generations; add incremental
	// WAL page maintenance only after indexed-write profiles justify it.
	if !image.indexes.is_empty() {
		return Ok(false);
	}
	let paths = graph_file_paths(root, current, files)?;
	let path = paths.get(&DataFileKind::GraphSuperblock).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph superblock file is missing")
	})?;
	let previous = select_graph_superblock(path)?;
	let next = &image.superblock;
	let stable_layout = previous.node_capacity == next.node_capacity
		&& previous.edge_capacity == next.edge_capacity
		&& previous.node_count == next.node_count
		&& previous.edge_count == next.edge_count
		&& previous.subgraph_capacity == next.subgraph_capacity
		&& previous.subgraph_count == next.subgraph_count
		&& previous.node_hot_pages == next.node_hot_pages
		&& previous.edge_hot_pages == next.edge_hot_pages
		&& previous.properties_pages == next.properties_pages
		&& previous.labels_size_bytes == next.labels_size_bytes
		&& previous.flags == next.flags
		&& previous.schema_root_page == next.schema_root_page
		&& previous.stats_root_page == next.stats_root_page;
	if !stable_layout {
		return Ok(false);
	}
	image.catalog_entry.created_lsn = current.created_lsn;
	image.catalog_entry.primary_index_file_id = current.primary_index_file_id;
	image.superblock.generation = previous.generation;
	image.superblock.backfilled_lsn = previous.backfilled_lsn;
	image.superblock.checkpoint_record_lsn = previous.checkpoint_record_lsn;
	image.superblock.created_at_ms = previous.created_at_ms;
	Ok(true)
}

fn stage_graph_image(
	manager: &SyncPageManager,
	transaction: &mut TxHandle,
	files: &FileCatalog,
	image: &BuiltGraphImage,
) -> FerrosGrynnResult<()> {
	let graph_id = image.catalog_entry.graph_id;
	let file_ids = graph_file_ids(&image.catalog_entry, files)?;
	for (kind, pages) in &image.payloads {
		if *kind == DataFileKind::GraphSuperblock {
			continue;
		}
		let file_id = file_ids.get(kind).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!("graph file {kind:?} is missing"))
		})?;
		let physical_pages = manager.file_page_count(graph_id, file_id)?;
		let required = u64::try_from(pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("graph page count exceeds u64")
		})?;
		manager.ensure_file_pages(graph_id, file_id, required)?;
		for (index, page) in pages.iter().enumerate() {
			let page_no = PageNo::new(u64::try_from(index).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("graph page index exceeds u64")
			})?);
			let key = super::types::PageKey::new(graph_id, file_id, page_no);
			if page_no.get() < physical_pages {
				let current = manager.read_page(transaction, key)?;
				let mut canonical = AlignedPage::zeroed();
				canonical.as_bytes_mut().copy_from_slice(current.as_bytes());
				canonical.assign_lsn(page_no, PageLsn::ZERO)?;
				if canonical.as_bytes() == page.as_bytes() {
					continue;
				}
			}
			manager.stage_frame(transaction, key, page)?;
		}
	}
	Ok(())
}

fn encode_optional_properties(
	properties: &PropertyMap,
	context: &mut EncodeContext,
	store: &mut PropertyStoreBuilder,
) -> FerrosGrynnResult<Option<PropertyLocation>> {
	if properties.is_empty() {
		return Ok(None);
	}
	let record = encode_property_map(properties, context)?;
	store.insert(&record).map(Some)
}

fn dense_ids(values: BTreeSet<String>) -> FerrosGrynnResult<BTreeMap<String, u32>> {
	values
		.into_iter()
		.enumerate()
		.map(|(id, value)| {
			u32::try_from(id)
				.map(|id| (value, id))
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("label id exceeds u32"))
		})
		.collect()
}

fn page_count_for_slots(capacity: u64) -> FerrosGrynnResult<usize> {
	if capacity == 0 {
		return Ok(0);
	}
	let slots = u64::try_from(HOT_SLOTS_PER_PAGE)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("hot slots exceed u64"))?;
	usize::try_from(capacity.div_ceil(slots))
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("hot page count exceeds usize"))
}

fn make_hot_pages(
	count: usize,
	new_page: fn(PageNo, PageLsn) -> FerrosGrynnResult<AlignedPage>,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	(0..count)
		.map(|index| {
			u64::try_from(index)
				.map(PageNo::new)
				.map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("hot page index exceeds u64")
				})
				.and_then(|page_no| new_page(page_no, PageLsn::ZERO))
		})
		.collect()
}

fn checked_page_index(page_no: PageNo, page_count: usize) -> FerrosGrynnResult<usize> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("page number exceeds usize"))?;
	if index >= page_count {
		return Err(FerrosGrynnError::invalid_storage_file("hot-slot address exceeds its file"));
	}
	Ok(index)
}

fn checked_u32(value: usize, field: &str) -> FerrosGrynnResult<u32> {
	u32::try_from(value)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file(format!("{field} exceeds u32")))
}

fn encode_subgraph_pages(entries: &[SubgraphEntry]) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let chunks = if entries.is_empty() {
		vec![&[][..]]
	} else {
		entries.chunks(SUBGRAPHS_PER_PAGE).collect::<Vec<_>>()
	};
	chunks
		.iter()
		.enumerate()
		.map(|(index, entries)| {
			let page_no = PageNo::new(u64::try_from(index).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("subgraph directory page exceeds u64")
			})?);
			let next = if index + 1 < chunks.len() {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("subgraph directory page link overflows")
				})?))
			} else {
				None
			};
			encode_subgraph_directory(page_no, PageLsn::ZERO, next, entries)
		})
		.collect()
}

fn inline_refs<const N: usize>(entries: &[DiskAtomRef]) -> [Option<DiskAtomRef>; N] {
	let mut inline = [None; N];
	for (target, value) in inline.iter_mut().zip(entries) {
		*target = Some(*value);
	}
	inline
}

struct EncodedHotEndpointSide {
	inline: [Option<DiskAtomRef>; 2],
	local_overflow: Option<PageNo>,
	graph_slot: Option<u32>,
	graph_overflow: Option<PageNo>,
}

fn encode_hot_endpoint_side(
	entries: &[EncodedEndpoint],
	side: EndpointSide,
	cross_level: bool,
	local: &mut OverflowStoreBuilder,
	qualified: &mut CrossLevelOverflowStoreBuilder,
) -> FerrosGrynnResult<EncodedHotEndpointSide> {
	if !cross_level {
		let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
		return Ok(EncodedHotEndpointSide {
			inline: inline_refs(&atoms),
			local_overflow: local.append(atoms.get(2..).unwrap_or(&[]))?,
			graph_slot: None,
			graph_overflow: None,
		});
	}
	let one_graph = entries
		.first()
		.map(|first| entries.iter().all(|entry| entry.graph_slot == first.graph_slot))
		.unwrap_or(true);
	if entries.len() <= 2 && one_graph {
		let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
		return Ok(EncodedHotEndpointSide {
			inline: inline_refs(&atoms),
			local_overflow: None,
			graph_slot: entries.first().map(|entry| entry.graph_slot),
			graph_overflow: None,
		});
	}
	Ok(EncodedHotEndpointSide {
		inline: [None; 2],
		local_overflow: None,
		graph_slot: None,
		graph_overflow: qualified.append(entries, side)?,
	})
}

fn encode_topology(
	topology: &EdgeTopology,
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	plan: &PlannedGraph,
) -> FerrosGrynnResult<EncodedTopology> {
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			let source = encode_endpoints(source, disk_refs, plan)?;
			let target = encode_endpoints(target, disk_refs, plan)?;
			let code = if source.len() == 1 && target.len() == 1 && source == target {
				EdgeTopologyCode::Loop
			} else if source.len() + target.len() == 2 {
				EdgeTopologyCode::Directed
			} else {
				EdgeTopologyCode::HyperDirected
			};
			Ok(EncodedTopology {
				code,
				source,
				target,
			})
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			let source = encode_endpoints(source, disk_refs, plan)?;
			let target = encode_endpoints(target, disk_refs, plan)?;
			if source.len() + target.len() != 2 {
				return Err(FerrosGrynnError::invalid_argument(
					"bidirectional hyperedges require a format extension",
				));
			}
			Ok(EncodedTopology {
				code: EdgeTopologyCode::Bidirectional,
				source,
				target,
			})
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			let source = encode_endpoints(endpoints, disk_refs, plan)?;
			let code = if source.len() == 2 {
				EdgeTopologyCode::Undirected
			} else {
				EdgeTopologyCode::HyperUndirected
			};
			Ok(EncodedTopology {
				code,
				source,
				target: Vec::new(),
			})
		}
	}
}

fn encode_endpoints(
	endpoints: &[EdgeEndpoint],
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	plan: &PlannedGraph,
) -> FerrosGrynnResult<Vec<EncodedEndpoint>> {
	endpoints
		.iter()
		.map(|endpoint| match endpoint {
			EdgeEndpoint::Local(id) => disk_refs
				.get(id)
				.copied()
				.map(|atom| EncodedEndpoint {
					graph_slot: 0,
					atom,
				})
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"edge endpoint references an unknown local atom",
					)
				}),
			EdgeEndpoint::Nested(qualified) => encode_qualified_endpoint(plan, qualified),
		})
		.collect()
}

fn encode_qualified_endpoint(
	plan: &PlannedGraph,
	qualified: &QualifiedAtomId,
) -> FerrosGrynnResult<EncodedEndpoint> {
	let mut current = plan;
	let mut graph_slot = None;
	for container in qualified.scope() {
		let child =
			current.children.iter().find(|child| child.container == *container).ok_or_else(
				|| {
					FerrosGrynnError::graph_constraint_violation(
						"metagraph",
						"qualified endpoint scope does not identify a nested graph",
					)
				},
			)?;
		graph_slot = Some(child.slot);
		current = &child.graph;
	}
	let atom = qualified.atom();
	let exists = match atom.kind() {
		Some(AtomKind::Node) => current.graph.node(atom).is_some(),
		Some(AtomKind::Edge) => current.graph.edge(atom).is_some(),
		None => false,
	};
	if !exists {
		return Err(FerrosGrynnError::graph_constraint_violation(
			"metagraph",
			"qualified endpoint atom is absent from its nested graph",
		));
	}
	Ok(EncodedEndpoint {
		graph_slot: graph_slot.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("qualified endpoint has no nested graph slot")
		})?,
		atom: DiskAtomRef::from_parts(
			atom.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("qualified endpoint has an invalid kind")
			})?,
			atom.slot(),
		)?,
	})
}

fn record_node_adjacency(
	topology: &EdgeTopology,
	edge: DiskAtomRef,
	nodes: &mut BTreeMap<AtomId, NodeAdjacency>,
) -> FerrosGrynnResult<()> {
	let mut append = |endpoint: &EdgeEndpoint,
	                  out: bool,
	                  input: bool,
	                  undirected: bool|
	 -> FerrosGrynnResult<()> {
		let Some(id) = endpoint.local_id() else {
			return Ok(());
		};
		if id.kind() != Some(AtomKind::Node) {
			return Ok(());
		}
		let adjacency = nodes.get_mut(&id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge references an unknown node")
		})?;
		if out {
			adjacency.outgoing.push(edge);
		}
		if input {
			adjacency.incoming.push(edge);
		}
		if undirected {
			adjacency.undirected.push(edge);
		}
		Ok(())
	};
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				append(endpoint, true, false, false)?;
			}
			for endpoint in target {
				append(endpoint, false, true, false)?;
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				append(endpoint, true, true, false)?;
			}
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				append(endpoint, false, false, true)?;
			}
		}
	}
	Ok(())
}

fn graph_statistics(
	nodes: &BTreeMap<AtomId, NodeAdjacency>,
	edge_count: usize,
) -> FerrosGrynnResult<GraphStatistics> {
	let total_out = nodes.values().try_fold(0_u64, |total, adjacency| {
		total
			.checked_add(u64::try_from(adjacency.outgoing.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("out-degree exceeds u64")
			})?)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("out-degree total overflows"))
	})?;
	let total_in = nodes.values().try_fold(0_u64, |total, adjacency| {
		total
			.checked_add(u64::try_from(adjacency.incoming.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("in-degree exceeds u64")
			})?)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("in-degree total overflows"))
	})?;
	let node_count = u64::try_from(nodes.len())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds u64"))?;
	let divisor = if node_count == 0 {
		1.0
	} else {
		node_count as f64
	};
	Ok(GraphStatistics {
		node_count,
		edge_count: u64::try_from(edge_count)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds u64"))?,
		average_out_degree: total_out as f64 / divisor,
		average_in_degree: total_in as f64 / divisor,
		maximum_out_degree: nodes
			.values()
			.map(|adjacency| adjacency.outgoing.len() as u64)
			.max()
			.unwrap_or(0),
		maximum_in_degree: nodes
			.values()
			.map(|adjacency| adjacency.incoming.len() as u64)
			.max()
			.unwrap_or(0),
	})
}

fn allocate_file_id(
	next_file_instance_id: &mut u64,
	kind: DataFileKind,
) -> FerrosGrynnResult<DataFileId> {
	let instance_id = *next_file_instance_id;
	let file_id = DataFileId::new(instance_id, kind).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("the physical-file identity clock is exhausted")
	})?;
	*next_file_instance_id = instance_id.checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("the physical-file identity clock is exhausted")
	})?;
	Ok(file_id)
}

fn unix_time_ms() -> FerrosGrynnResult<u64> {
	let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|source| {
		FerrosGrynnError::invalid_storage_file(format!(
			"system clock precedes Unix epoch: {source}"
		))
	})?;
	u64::try_from(duration.as_millis())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("Unix timestamp exceeds u64"))
}

fn write_pages(path: &Path, pages: &[AlignedPage]) -> FerrosGrynnResult<()> {
	let file = DirectFile::create_new(path)?;
	file.write_pages(PageNo::new(0), pages)?;
	file.sync_all()
}

fn storage_io(operation: &str, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

struct LoadedNodeSlot {
	id: AtomId,
	record_id: RecordId,
	value: NodeHotSlot,
	outgoing: Vec<DiskAtomRef>,
	incoming: Vec<DiskAtomRef>,
	undirected: Vec<DiskAtomRef>,
}

struct LoadedEdgeSlot {
	id: AtomId,
	record_id: RecordId,
	value: EdgeHotSlot,
	source: Vec<DiskAtomRef>,
	target: Vec<DiskAtomRef>,
}

struct DecodedNode {
	record_id: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
}

struct DecodedEdge {
	record_id: RecordId,
	topology: EdgeTopology,
	label: Option<Text>,
	properties: PropertyMap,
	weight: Option<crate::values::Number>,
}

struct DecodedGraph {
	identity: Uuid,
	revision: u64,
	kind: GraphKind,
	properties: PropertyMap,
	nodes: BTreeMap<AtomId, DecodedNode>,
	edges: BTreeMap<AtomId, DecodedEdge>,
}

impl GraphRead for DecodedGraph {
	type Edge<'a>
		= EdgeView<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= FallibleAtomIds<std::iter::Copied<std::collections::btree_map::Keys<'a, AtomId, DecodedEdge>>>
	where
		Self: 'a;
	type Node<'a>
		= NodeView<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= FallibleAtomIds<std::iter::Copied<std::collections::btree_map::Keys<'a, AtomId, DecodedNode>>>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.identity
	}

	fn revision(&self) -> u64 {
		self.revision
	}

	fn kind(&self) -> GraphKind {
		self.kind
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(&self.properties))
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(FallibleAtomIds::new(self.nodes.keys().copied()))
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(FallibleAtomIds::new(self.edges.keys().copied()))
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		Ok(self.nodes.get(&id).map(|node| {
			NodeView::new(id, &node.record_id, node.label.as_ref(), &node.properties, None)
		}))
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		Ok(self.edges.get(&id).map(|edge| {
			EdgeView::new(
				id,
				&edge.record_id,
				&edge.topology,
				edge.label.as_ref(),
				&edge.properties,
				edge.weight,
				None,
			)
		}))
	}

	fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::in_memory(self.revision)
	}
}

fn load_graph(
	root: &Path,
	entry: &GraphCatalogEntry,
	files: &FileCatalog,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<MaterializedGraph> {
	let paths = graph_file_paths(root, entry, files)?;
	let superblock_path = paths.get(&DataFileKind::GraphSuperblock).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph superblock file is missing")
	})?;
	let superblock = select_graph_superblock(superblock_path)?;
	if superblock.graph_id != entry.graph_id {
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph superblock identity disagrees with the database catalog",
		));
	}

	let labels_file = open_graph_file(&paths, DataFileKind::Labels)?;
	if labels_file.page_count().checked_mul(PAGE_SIZE as u64) != Some(superblock.labels_size_bytes)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"label file length disagrees with GraphSuperblock",
		));
	}
	let labels = decode_label_dictionary(|page_no, output| {
		labels_file.read_page(page_no, output).map(|_header| ())
	})?;
	let mut context = EncodeContext::new(dictionary.clone());
	for (expected, table) in labels.table_names.iter().enumerate() {
		let actual = context.register_table_name(table.clone().into())?;
		if usize::try_from(actual).ok() != Some(expected) {
			return Err(FerrosGrynnError::invalid_storage_file("table dictionary is not dense"));
		}
	}
	for (index, zone) in labels.time_zones.iter().enumerate() {
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

	let index_file = open_graph_file(&paths, DataFileKind::Index)?;
	let primary_entries = scan_primary_index(
		|page_no, output| index_file.read_page(page_no, output).map(|_header| ()),
		entry.primary_index_root,
	)?;
	let mut records = BTreeMap::new();
	for primary in primary_entries {
		let stored = CanonicalCodec::decode(&primary.key, DecodeLimits::default())?;
		let Value::Record(record) = stored.into_runtime(&context.decoder())? else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index key is not a canonical RecordId",
			));
		};
		if records.insert(primary.atom, *record).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index maps one atom more than once",
			));
		}
	}

	let node_file = open_graph_file(&paths, DataFileKind::NodesHot)?;
	let edge_file = open_graph_file(&paths, DataFileKind::EdgesHot)?;
	let adjacency_file = open_graph_file(&paths, DataFileKind::AdjacencyOverflow)?;
	if node_file.page_count() != superblock.node_hot_pages
		|| edge_file.page_count() != superblock.edge_hot_pages
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"hot-file length disagrees with GraphSuperblock",
		));
	}
	let node_pages = read_all_pages(&node_file)?;
	let edge_pages = read_all_pages(&edge_file)?;
	let mut live_atoms = BTreeMap::new();
	let mut loaded_nodes = Vec::new();
	for slot in 0..superblock.node_capacity {
		let (page_no, within) = hot_slot_address(slot)?;
		let value = read_node_hot_slot(
			&node_pages[checked_page_index(page_no, node_pages.len())?],
			page_no,
			within,
		)?;
		if value.flags.contains(NodeSlotFlags::TOMBSTONE) {
			continue;
		}
		if value.flags.contains(NodeSlotFlags::META) || value.subgraph_slot.is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"stage-4 graph generation contains a nested node",
			));
		}
		let disk = DiskAtomRef::from_parts(AtomKind::Node, slot)?;
		let record_id = records.get(&disk).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live node is absent from the primary index")
		})?;
		validate_table_id(value.table_id, &record_id, &labels.table_names)?;
		let id = AtomId::from_parts(AtomKind::Node, slot, value.generation)?;
		live_atoms.insert(disk, id);
		loaded_nodes.push(LoadedNodeSlot {
			id,
			record_id,
			outgoing: read_ref_array(
				&adjacency_file,
				&value.out_inline,
				value.out_count,
				value.out_overflow,
			)?,
			incoming: read_ref_array(
				&adjacency_file,
				&value.in_inline,
				value.in_count,
				value.in_overflow,
			)?,
			undirected: read_ref_array(
				&adjacency_file,
				&value.undirected_inline,
				value.undirected_count,
				value.undirected_overflow,
			)?,
			value,
		});
	}
	let mut loaded_edges = Vec::new();
	for slot in 0..superblock.edge_capacity {
		let (page_no, within) = hot_slot_address(slot)?;
		let value = read_edge_hot_slot(
			&edge_pages[checked_page_index(page_no, edge_pages.len())?],
			page_no,
			within,
		)?;
		if value.flags.contains(EdgeSlotFlags::TOMBSTONE) {
			continue;
		}
		if value.flags.intersects(EdgeSlotFlags::META | EdgeSlotFlags::CROSS_LEVEL)
			|| value.has_transition
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"stage-4 graph generation contains meta or cross-level incidence",
			));
		}
		let disk = DiskAtomRef::from_parts(AtomKind::Edge, slot)?;
		let record_id = records.get(&disk).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live edge is absent from the primary index")
		})?;
		validate_table_id(value.table_id, &record_id, &labels.table_names)?;
		let id = AtomId::from_parts(AtomKind::Edge, slot, value.generation)?;
		live_atoms.insert(disk, id);
		loaded_edges.push(LoadedEdgeSlot {
			id,
			record_id,
			source: read_ref_array(
				&adjacency_file,
				&value.source_inline,
				value.source_count,
				value.source_overflow,
			)?,
			target: read_ref_array(
				&adjacency_file,
				&value.target_inline,
				value.target_count,
				value.target_overflow,
			)?,
			value,
		});
	}
	if usize::try_from(superblock.node_count).ok() != Some(loaded_nodes.len())
		|| usize::try_from(superblock.edge_count).ok() != Some(loaded_edges.len())
		|| records.len() != live_atoms.len()
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph live counts disagree with hot pages or primary index",
		));
	}
	for (disk, record) in &records {
		let id = live_atoms.get(disk).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("primary index references a tombstone")
		})?;
		context.register_atom(AtomRef::new(disk.kind(), record.clone()), *disk)?;
		if id.kind() != Some(disk.kind()) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index kind disagrees with the hot page",
			));
		}
	}

	let properties_file = open_graph_file(&paths, DataFileKind::Properties)?;
	if properties_file.page_count() != superblock.properties_pages {
		return Err(FerrosGrynnError::invalid_storage_file(
			"property-file length disagrees with GraphSuperblock",
		));
	}
	let graph_properties = read_properties(&properties_file, entry.properties, &context)?;
	let mut nodes = BTreeMap::new();
	for node in &loaded_nodes {
		nodes.insert(node.id, DecodedNode {
			record_id: node.record_id.clone(),
			label: decode_label(node.value.label_id, &labels.node_labels)?,
			properties: read_properties(&properties_file, node.value.properties, &context)?,
		});
	}
	let mut edges = BTreeMap::new();
	for edge in &loaded_edges {
		edges.insert(edge.id, DecodedEdge {
			record_id: edge.record_id.clone(),
			topology: decode_topology(edge, &live_atoms)?,
			label: decode_label(edge.value.label_id, &labels.edge_labels)?,
			properties: read_properties(&properties_file, edge.value.properties, &context)?,
			weight: edge.value.weight.map(Into::into),
		});
	}
	validate_node_adjacency(&loaded_nodes, &loaded_edges, &edges)?;
	validate_auxiliary_files(&paths, &superblock)?;

	let decoded = DecodedGraph {
		identity: uuid::Uuid::from_bytes(entry.identity).into(),
		revision: entry.revision,
		kind: entry.kind,
		properties: graph_properties,
		nodes,
		edges,
	};
	materialize_graph(&decoded)
}

fn graph_file_paths(
	root: &Path,
	graph: &GraphCatalogEntry,
	files: &FileCatalog,
) -> FerrosGrynnResult<BTreeMap<DataFileKind, PathBuf>> {
	let mut paths = BTreeMap::new();
	for entry in files.files.iter().filter(|entry| entry.graph_id == graph.graph_id) {
		let kind = entry.file_id.kind().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph file has an unknown kind")
		})?;
		if kind == DataFileKind::Index && entry.file_id != graph.primary_index_file_id {
			continue;
		}
		if paths.insert(kind, entry.path.resolve(root)).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"graph catalog contains duplicate file kinds",
			));
		}
	}
	for (kind, _name) in GRAPH_FILE_KINDS {
		if !paths.contains_key(&kind) {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"graph catalog is missing {kind:?}"
			)));
		}
	}
	Ok(paths)
}

fn open_graph_file(
	paths: &BTreeMap<DataFileKind, PathBuf>,
	kind: DataFileKind,
) -> FerrosGrynnResult<DirectFile> {
	let path = paths.get(&kind).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(format!("graph file {kind:?} is missing"))
	})?;
	DirectFile::open_read_only(path)
}

fn read_all_pages(file: &DirectFile) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let count = usize::try_from(file.page_count()).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("file page count exceeds usize")
	})?;
	let mut pages = (0..count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
	file.read_pages(PageNo::new(0), &mut pages)?;
	Ok(pages)
}

fn validate_table_id(
	table_id: Option<u32>,
	record_id: &RecordId,
	tables: &[String],
) -> FerrosGrynnResult<()> {
	let id = table_id.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("live atom has no table dictionary id")
	})?;
	let index = usize::try_from(id)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("table id exceeds usize"))?;
	if tables.get(index).map(String::as_str) != Some(record_id.table()) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"atom table id disagrees with its primary RecordId",
		));
	}
	Ok(())
}

fn read_ref_array<const N: usize>(
	file: &DirectFile,
	inline: &[Option<DiskAtomRef>; N],
	count: u32,
	overflow: Option<PageNo>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let count = usize::try_from(count).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("adjacency count exceeds usize")
	})?;
	let mut entries = inline.iter().take(count.min(N)).flatten().copied().collect::<Vec<_>>();
	let mut next = overflow;
	let mut visited = BTreeSet::new();
	while let Some(page_no) = next {
		if !visited.insert(page_no) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow contains a cycle",
			));
		}
		let mut page = AlignedPage::zeroed();
		file.read_page(page_no, &mut page)?;
		let (following, chunk) = decode_adjacency_overflow(&page, page_no)?;
		entries.extend(chunk);
		if entries.len() > count {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow exceeds its declared count",
			));
		}
		next = following;
	}
	if entries.len() != count {
		return Err(FerrosGrynnError::invalid_storage_file(
			"adjacency overflow ended before its declared count",
		));
	}
	Ok(entries)
}

fn decode_label(id: Option<u32>, labels: &[String]) -> FerrosGrynnResult<Option<Text>> {
	id.map(|id| {
		let index = usize::try_from(id)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("label id exceeds usize"))?;
		labels
			.get(index)
			.cloned()
			.map(Into::into)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label id is out of range"))
	})
	.transpose()
}

fn read_properties(
	file: &DirectFile,
	location: Option<PropertyLocation>,
	context: &EncodeContext,
) -> FerrosGrynnResult<PropertyMap> {
	let Some(location) = location else {
		return Ok(PropertyMap::new());
	};
	let limits = DecodeLimits::default();
	let record = match location {
		PropertyLocation::Inline {
			page_no,
			slot,
			length,
		} => {
			let mut page = AlignedPage::zeroed();
			file.read_page(page_no, &mut page)?;
			read_property_record(&page, page_no, slot, length)?.to_vec()
		}
		PropertyLocation::Large {
			first_page,
			length,
		} => {
			let length_usize = usize::try_from(length).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("large property length exceeds usize")
			})?;
			let page_count = length_usize.div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
			let mut pages = (0..page_count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
			file.read_pages(first_page, &mut pages)?;
			decode_large_property(first_page, &pages, length, limits.max_total_allocation)?
		}
	};
	decode_property_map(&record, &context.decoder(), limits)
}

fn decode_topology(
	edge: &LoadedEdgeSlot,
	atoms: &BTreeMap<DiskAtomRef, AtomId>,
) -> FerrosGrynnResult<EdgeTopology> {
	let source = runtime_endpoints(&edge.source, atoms)?;
	let target = runtime_endpoints(&edge.target, atoms)?;
	Ok(match edge.value.topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::HyperDirected => {
			EdgeTopology::directed(source, target)
		}
		EdgeTopologyCode::Bidirectional => EdgeTopology::bidirectional(source, target),
		EdgeTopologyCode::Undirected | EdgeTopologyCode::HyperUndirected => {
			if !target.is_empty() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"undirected edge contains target endpoints",
				));
			}
			EdgeTopology::undirected(source)
		}
		EdgeTopologyCode::Loop => {
			if source.len() != 1 || target.len() != 1 || source != target {
				return Err(FerrosGrynnError::invalid_storage_file(
					"loop edge endpoints are inconsistent",
				));
			}
			EdgeTopology::directed(source, target)
		}
	})
}

fn runtime_endpoints(
	entries: &[DiskAtomRef],
	atoms: &BTreeMap<DiskAtomRef, AtomId>,
) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
	entries
		.iter()
		.map(|entry| {
			atoms.get(entry).copied().map(EdgeEndpoint::Local).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"edge endpoint references an unknown live atom",
				)
			})
		})
		.collect()
}

fn validate_node_adjacency(
	nodes: &[LoadedNodeSlot],
	edges: &[LoadedEdgeSlot],
	decoded_edges: &BTreeMap<AtomId, DecodedEdge>,
) -> FerrosGrynnResult<()> {
	let mut expected =
		nodes.iter().map(|node| (node.id, NodeAdjacency::default())).collect::<BTreeMap<_, _>>();
	for edge in edges {
		let topology = decoded_edges
			.get(&edge.id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("decoded edge is absent"))?;
		record_node_adjacency(
			&topology.topology,
			DiskAtomRef::from_parts(AtomKind::Edge, edge.id.slot())?,
			&mut expected,
		)?;
	}
	for node in nodes {
		let expected = expected.get(&node.id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("decoded node adjacency is absent")
		})?;
		if node.outgoing != expected.outgoing
			|| node.incoming != expected.incoming
			|| node.undirected != expected.undirected
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"node adjacency disagrees with edge topology",
			));
		}
	}
	Ok(())
}

fn validate_auxiliary_files(
	paths: &BTreeMap<DataFileKind, PathBuf>,
	superblock: &GraphSuperblock,
) -> FerrosGrynnResult<()> {
	let subgraphs = open_graph_file(paths, DataFileKind::Subgraphs)?;
	let freelist = open_graph_file(paths, DataFileKind::Freelist)?;
	let statistics = open_graph_file(paths, DataFileKind::Statistics)?;
	if subgraphs.page_count() != 1 || freelist.page_count() != 1 || statistics.page_count() != 1 {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stage-4 auxiliary files must contain one root page",
		));
	}
	let mut page = AlignedPage::zeroed();
	subgraphs.read_page(PageNo::new(0), &mut page)?;
	if decode_subgraph_directory(&page, PageNo::new(0))? != (None, Vec::new()) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stage-4 subgraph directory is not empty",
		));
	}
	freelist.read_page(PageNo::new(0), &mut page)?;
	let (_first, next, bitmap) = decode_freelist(&page, PageNo::new(0))?;
	if next.is_some() || bitmap.iter().any(|byte| *byte != 0) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"immutable generation freelist is not empty",
		));
	}
	statistics.read_page(PageNo::new(0), &mut page)?;
	let stats = decode_graph_statistics(&page)?;
	if stats.node_count != superblock.node_count || stats.edge_count != superblock.edge_count {
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics counts disagree with GraphSuperblock",
		));
	}
	Ok(())
}

fn materialize_graph(decoded: &DecodedGraph) -> FerrosGrynnResult<MaterializedGraph> {
	let graph = Graph::<Mutable>::from_read_preserving(decoded)?.freeze();
	match decoded.kind {
		GraphKind::Graph => Ok(graph.into()),
		GraphKind::Tree => Ok(Tree::<Frozen>::try_from(graph)?.into()),
		GraphKind::RootedTree => Ok(RootedTree::<Frozen>::try_from(graph)?.into()),
		GraphKind::Forest => Ok(Forest::<Frozen>::try_from(graph)?.into()),
		GraphKind::Dag => Ok(Dag::<Frozen>::try_from(graph)?.into()),
		GraphKind::Hypergraph => Ok(Hypergraph::<Frozen>::try_from(graph)?.into()),
		GraphKind::Metagraph => Ok(Metagraph::<Frozen>::try_from(graph)?.into()),
		GraphKind::Subgraph => Err(FerrosGrynnError::invalid_storage_file(
			"a named graph catalog cannot contain a Subgraph family",
		)),
	}
}
