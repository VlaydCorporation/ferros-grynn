//! Catalog-first recovery and normalized MVCC-root reconstruction.

#[cfg(test)]
use std::{
	cell::Cell,
	time::{Duration, Instant},
};
use std::{collections::BTreeMap, path::Path, sync::Arc};

use rapidhash::RapidHashSet;

use super::{
	GraphBinding,
	catalog::{decode_catalog_values, decode_schema},
	layout::{DATABASE_CATALOG_ROOT, FILE_CATALOG_ROOT, GenerationBindings, invalid},
	statistics::{PhysicalStatisticsFiles, load_statistics_roots},
};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	storage::{
		atom_store_image::{
			AtomStoreCheckpointAuthority,
			AtomStoreFileSet,
			open_durable_atom_store_image,
			value_context,
		},
		catalog::{CatalogKind, DatabaseCatalog, FileCatalog, decode_page_chain},
		consts::WAL_DIRECTORY,
		database_superblock::DatabaseSuperblock,
		direct::DirectFileOpenMode,
		file_registry::FileRegistry,
		graph_store::{
			GraphCheckpointAuthority,
			GraphFileSet,
			ReverseMembershipFile,
			open_durable_graph_image,
			open_reverse_membership_image,
			validate_reverse_memberships,
		},
		graph_superblock::GraphSuperblock,
		hybrid_engine::HybridReadEngine,
		image_io::read_page_cached,
		local::select_database_superblock,
		page::{AlignedPage, PageNo},
		types::{DataFileId, DataFileKind, PageKey, StorageSnapshot},
		wal::{FrameWal, RecoverySeed},
	},
};

/// Fully opened logical and physical state at one durable WAL boundary.
pub(super) struct OpenedLocalState {
	pub(super) root: DatabaseRoot,
	pub(super) database_superblock: DatabaseSuperblock,
	pub(super) file_catalog: FileCatalog,
	pub(super) database_catalog: DatabaseCatalog,
	pub(super) bindings: GenerationBindings,
	pub(super) registry: FileRegistry,
	pub(super) atom_superblock: crate::storage::atom_store_superblock::AtomStoreSuperblock,
	pub(super) graph_superblocks: BTreeMap<crate::graph::GraphId, GraphSuperblock>,
	pub(super) physical_statistics: Arc<PhysicalStatisticsFiles>,
}

/// Test-only phase timings for one complete local reopen.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct StartupTimings {
	pub(super) catalog: Duration,
	pub(super) registry: Duration,
	pub(super) materialization: Duration,
	pub(super) derived: Duration,
	pub(super) indexes: Duration,
	pub(super) statistics: Duration,
	pub(super) validation: Duration,
}

#[cfg(test)]
thread_local! {
	static LAST_STARTUP_TIMINGS: Cell<StartupTimings> = Cell::new(StartupTimings {
		catalog: Duration::ZERO,
		registry: Duration::ZERO,
		materialization: Duration::ZERO,
		derived: Duration::ZERO,
		indexes: Duration::ZERO,
		statistics: Duration::ZERO,
		validation: Duration::ZERO,
	});
}

/// Returns the latest reopen timings recorded on this test thread.
#[cfg(test)]
pub(super) fn last_startup_timings() -> StartupTimings {
	LAST_STARTUP_TIMINGS.get()
}

/// Opens the database authority, recovers WAL metadata, and materializes the
/// current normalized root through `HybridReadEngine`.
pub(super) fn open_local_state(
	root_path: &Path,
	engine: &HybridReadEngine,
	open_mode: DirectFileOpenMode,
) -> FerrosGrynnResult<(FrameWal, OpenedLocalState)> {
	#[cfg(test)]
	let catalog_started = Instant::now();
	let database_superblock =
		select_database_superblock(&root_path.join(super::DATABASE_SUPERBLOCK_FILE))?;
	let seed = RecoverySeed::new(
		database_superblock.generation,
		database_superblock.checkpoint_lsn,
		database_superblock.checkpoint_record_lsn,
		database_superblock.durable_visible_lsn,
		database_superblock.next_lsn,
		database_superblock.commit_ts,
		database_superblock.next_tx_id,
		database_superblock.next_file_instance_id,
	);
	let wal = FrameWal::open_with_seed(root_path.join(WAL_DIRECTORY), seed)?;
	let bootstrap = FileCatalog::bootstrap()?;
	let bootstrap_registry = FileRegistry::open(
		bootstrap.registrations(root_path),
		open_mode,
		&RapidHashSet::default(),
	)?;
	let snapshot = wal.snapshot();
	let file_catalog = read_catalog(
		&wal,
		&bootstrap_registry,
		engine,
		snapshot,
		&database_superblock,
		FILE_CATALOG_ROOT,
		CatalogKind::Files,
	)
	.and_then(|bytes| FileCatalog::decode(&bytes))?;
	let database_catalog = read_catalog(
		&wal,
		&bootstrap_registry,
		engine,
		snapshot,
		&database_superblock,
		DATABASE_CATALOG_ROOT,
		CatalogKind::Database,
	)
	.and_then(|bytes| DatabaseCatalog::decode(&bytes))?;
	#[cfg(test)]
	let catalog = catalog_started.elapsed();
	#[cfg(test)]
	let registry_started = Instant::now();
	let registry = FileRegistry::open(
		file_catalog.registrations(root_path),
		open_mode,
		&RapidHashSet::default(),
	)?;
	#[cfg(test)]
	let registry_duration = registry_started.elapsed();
	#[cfg(test)]
	let materialization_started = Instant::now();
	let bindings = bindings(&file_catalog, &database_catalog)?;
	let mut schema = decode_schema(&database_catalog)?;
	let authority = AtomStoreCheckpointAuthority::new(
		database_superblock.generation,
		database_superblock.system_backfilled_lsn,
		database_superblock.checkpoint_record_lsn,
	);
	let (atoms, dictionary, atom_superblock) = open_durable_atom_store_image(
		&wal,
		&registry,
		Some(engine),
		snapshot,
		authority,
		bindings.atoms,
		&schema,
	)?;
	decode_catalog_values(&database_catalog, &dictionary, &mut schema)?;

	let mut database = DatabaseRoot::default();
	database.commit_ts = snapshot.commit_ts().get();
	database.schema = Arc::new(schema);
	database.dictionary = Arc::new(dictionary);
	database.atoms = Arc::new(atoms);
	let mut decode = value_context(&database.schema, database.dictionary.as_ref().clone())?;
	for zone in &database_catalog.time_zones {
		decode.register_time_zone_name(zone.clone())?;
	}
	let decode = decode.decoder();
	let graph_authority = GraphCheckpointAuthority::new(
		database_superblock.generation,
		database_superblock.checkpoint_lsn,
		database_superblock.checkpoint_record_lsn,
	);
	let mut graph_superblocks = BTreeMap::new();
	let mut pending = bindings.graphs.iter().collect::<Vec<_>>();
	while !pending.is_empty() {
		let index = pending
			.iter()
			.position(|binding| {
				binding
					.arena
					.graph_by_slot
					.iter()
					.all(|(slot, child)| *slot == 0 || database.graph(*child).is_some())
			})
			.ok_or_else(|| invalid("graph arenas contain a cycle or an absent child graph"))?;
		let binding = pending.remove(index);
		let reference = database
			.schema
			.graph(binding.graph_id)
			.ok_or_else(|| invalid("graph binding has no schema definition"))?
			.reference()
			.clone();
		let (graph, superblock) = open_durable_graph_image(
			&wal,
			&registry,
			Some(engine),
			snapshot,
			graph_authority,
			binding.files,
			reference,
			binding.arena.clone(),
			&database,
			&decode,
		)?;
		database.install_opened_graph(graph)?;
		graph_superblocks.insert(binding.graph_id, superblock);
	}
	let reverse = open_reverse_membership_image(
		&wal,
		&registry,
		Some(engine),
		snapshot,
		database_superblock.generation,
		database_superblock.system_backfilled_lsn,
		bindings.reverse,
	)?;
	validate_reverse_memberships(&reverse, &database)?;
	#[cfg(test)]
	let materialization = materialization_started.elapsed();
	#[cfg(test)]
	let statistics_started = Instant::now();
	let (statistics_store, physical_statistics) = load_statistics_roots(
		&wal,
		&registry,
		engine,
		snapshot,
		&database_superblock,
		&file_catalog,
	)?;
	database.statistics = Arc::new(statistics_store);
	#[cfg(test)]
	let statistics = statistics_started.elapsed();
	#[cfg(test)]
	let derived_started = Instant::now();
	#[cfg(test)]
	let indexes_started = Instant::now();
	database.rebuild_lookup_indexes()?;
	#[cfg(test)]
	let indexes = indexes_started.elapsed();
	#[cfg(test)]
	let derived = derived_started.elapsed();
	#[cfg(test)]
	let validation_started = Instant::now();
	validate_opened_root(&database, &database_catalog, &file_catalog, snapshot)?;
	#[cfg(test)]
	LAST_STARTUP_TIMINGS.set(StartupTimings {
		catalog,
		registry: registry_duration,
		materialization,
		derived,
		indexes,
		statistics,
		validation: validation_started.elapsed(),
	});

	Ok((wal, OpenedLocalState {
		root: database,
		database_superblock,
		file_catalog,
		database_catalog,
		bindings,
		registry,
		atom_superblock,
		graph_superblocks,
		physical_statistics,
	}))
}

fn read_catalog(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	authority: &DatabaseSuperblock,
	root: PageNo,
	kind: CatalogKind,
) -> FerrosGrynnResult<Vec<u8>> {
	let file = DataFileId::for_kind(DataFileKind::SystemCatalog);
	decode_page_chain(
		&mut |page_no, output: &mut AlignedPage| {
			read_page_cached(
				wal,
				registry,
				engine,
				snapshot,
				authority.generation,
				authority.system_backfilled_lsn,
				PageKey::new(DataSpaceId::Database, file, page_no),
				output,
			)
		},
		root,
		kind,
	)
}

fn bindings(
	files: &FileCatalog,
	database: &DatabaseCatalog,
) -> FerrosGrynnResult<GenerationBindings> {
	let atoms = AtomStoreFileSet::from_catalog(
		files,
		database.atom_store.primary.file_id,
		database.atom_store.primary.root,
	)?;
	let reverse = ReverseMembershipFile::from_catalog(
		files,
		database.atom_store.reverse_membership.file_id,
		database.atom_store.reverse_membership.root,
	)?;
	let graphs = database
		.graphs
		.iter()
		.map(|graph| {
			Ok(GraphBinding {
				graph_id: graph.id,
				generation: graph.generation,
				files: GraphFileSet::from_catalog(
					files,
					graph.id,
					graph.membership.file_id,
					graph.membership.root,
				)?,
				membership: graph.membership,
				arena: graph.arena.clone(),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let physical_indexes = database
		.classes
		.iter()
		.flat_map(|class| class.indexes.iter())
		.filter_map(|index| index.physical.map(|physical| (index.id, physical)))
		.collect();
	let physical_deltas = database
		.classes
		.iter()
		.flat_map(|class| class.indexes.iter())
		.filter_map(|index| index.delta.map(|delta| (index.id, delta)))
		.collect();
	Ok(GenerationBindings {
		atoms,
		reverse,
		graphs,
		physical_indexes,
		physical_deltas,
	})
}

fn validate_opened_root(
	root: &DatabaseRoot,
	database: &DatabaseCatalog,
	files: &FileCatalog,
	snapshot: StorageSnapshot,
) -> FerrosGrynnResult<()> {
	if root.commit_ts != snapshot.commit_ts().get()
		|| database.atom_store.generation > root.commit_ts
		|| usize::try_from(database.graphs.len()).ok() != Some(root.graphs.len())
		|| database.graphs.iter().any(|graph| graph.generation > root.commit_ts)
		|| files.next_file_instance_id < database.atom_store.primary.file_id.instance_id()
	{
		return Err(invalid("catalogs, WAL clocks, and normalized root disagree"));
	}
	Ok(())
}
