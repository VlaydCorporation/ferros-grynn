//! Local durability owner for the normalized AtomStore and graph spaces.

mod catalog;
mod checkpoint;
mod cleanup;
mod files;
mod image;
mod layout;
mod open;
mod statistics;

#[cfg(test)]
mod tests;

use std::{
	collections::BTreeMap,
	fs,
	num::NonZeroUsize,
	path::{Path, PathBuf},
	time::{Duration, Instant},
};

use parking_lot::Mutex;

use self::{
	checkpoint::checkpoint,
	cleanup::cleanup_orphan_generations,
	files::{
		create_generation_files,
		create_import_generation_files,
		stage_generation,
		stage_import_catalog,
	},
	image::{build_generation, build_import_generation},
	layout::{DATABASE_CATALOG_ROOT, FILE_CATALOG_ROOT, GenerationBindings, invalid},
	open::{OpenedLocalState, open_local_state},
};
use super::{
	atom_store_superblock::AtomStoreSuperblock,
	catalog::{DatabaseCatalog, FileCatalog, PhysicalIndexBinding},
	consts::{DATABASE_SUPERBLOCK_FILE, WAL_DIRECTORY},
	database_superblock::DatabaseSuperblock,
	file_registry::FileRegistry,
	graph_index::cleanup_external_index_sort,
	graph_pages::GraphArenaBindings,
	graph_store::GraphFileSet,
	graph_superblock::GraphSuperblock,
	hybrid_engine::HybridReadEngine,
	local::{DatabaseLock, write_database_superblock},
	page_io::{PageIo, PageIoMode},
	types::{CheckpointGeneration, CommitTs, Lsn, TxId},
	wal::FrameWal,
};
use crate::{
	datastore::{CheckpointMode, DatabaseRoot, IndexId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ImportFailurePoint {
	None,
	AfterBuild,
	AfterFileSync,
	AfterCatalogStage,
	BeforeWalCommit,
}

#[cfg(test)]
thread_local! {
	static IMPORT_FAILURE_POINT: std::cell::Cell<ImportFailurePoint> =
		const { std::cell::Cell::new(ImportFailurePoint::None) };
}

/// Physical bindings retained with one current catalog generation.
pub(super) struct GraphBinding {
	pub(super) graph_id: GraphId,
	pub(super) files: GraphFileSet,
	pub(super) membership: PhysicalIndexBinding,
	pub(super) arena: GraphArenaBindings,
}

struct LocalState {
	database_superblock_path: PathBuf,
	database_superblock: DatabaseSuperblock,
	file_catalog: FileCatalog,
	_database_catalog: DatabaseCatalog,
	bindings: GenerationBindings,
	registry: FileRegistry,
	atom_superblock: AtomStoreSuperblock,
	graph_superblocks: BTreeMap<GraphId, GraphSuperblock>,
	last_checkpoint: Instant,
}

/// One exclusively owned local database and its database-level frame WAL.
pub(crate) struct LocalStore {
	root: PathBuf,
	_lock: DatabaseLock,
	io: PageIo,
	engine: HybridReadEngine,
	wal: FrameWal,
	state: Mutex<LocalState>,
	checkpoint_page_threshold: usize,
	checkpoint_interval: Duration,
}

impl LocalStore {
	/// Returns the database-local root used by bounded import scratch sessions.
	pub(crate) fn import_scratch_root(&self) -> &Path {
		&self.root
	}

	/// Creates a new empty local database without overwriting an existing path.
	pub(crate) fn create(
		root: &Path,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		checkpoint_page_threshold: usize,
		checkpoint_interval: Duration,
	) -> FerrosGrynnResult<(Self, DatabaseRoot)> {
		fs::create_dir(root)
			.map_err(|source| io_failure("create local database directory", source))?;
		let result = Self::create_in_directory(
			root,
			pool_pages,
			io_mode,
			checkpoint_page_threshold,
			checkpoint_interval,
		);
		if result.is_err() {
			drop(fs::remove_dir_all(root));
		}
		result
	}

	fn create_in_directory(
		root: &Path,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		checkpoint_page_threshold: usize,
		checkpoint_interval: Duration,
	) -> FerrosGrynnResult<(Self, DatabaseRoot)> {
		let lock = DatabaseLock::acquire(root)?;
		cleanup_external_index_sort(root)?;
		let io = PageIo::new(io_mode)?;
		let engine = HybridReadEngine::new(pool_pages);
		let database = DatabaseRoot::default();
		let generation = build_generation(&database, 1)?;
		create_generation_files(root, &generation, true)?;
		let name = database_name(root)?;
		let superblock = DatabaseSuperblock {
			name,
			generation: CheckpointGeneration::ZERO,
			checkpoint_lsn: Lsn::ZERO,
			checkpoint_record_lsn: Lsn::ZERO,
			durable_visible_lsn: Lsn::ZERO,
			next_lsn: Lsn::FIRST,
			commit_ts: CommitTs::ZERO,
			next_tx_id: TxId::new(1),
			next_file_instance_id: generation.file_catalog.next_file_instance_id,
			system_backfilled_lsn: Lsn::ZERO,
			database_catalog_root: DATABASE_CATALOG_ROOT,
			file_catalog_root: FILE_CATALOG_ROOT,
			graph_count: 0,
		};
		let superblock_path = root.join(DATABASE_SUPERBLOCK_FILE);
		write_database_superblock(&superblock_path, &superblock)?;
		let wal = FrameWal::create_new(root.join(WAL_DIRECTORY))?;
		let registry = FileRegistry::open(
			generation.file_catalog.registrations(root),
			io.file_open_mode(),
			&rapidhash::RapidHashSet::default(),
		)?;
		let state = LocalState {
			database_superblock_path: superblock_path,
			database_superblock: superblock,
			file_catalog: generation.file_catalog,
			_database_catalog: generation.database_catalog,
			bindings: generation.bindings,
			registry,
			atom_superblock: generation.atom_superblock,
			graph_superblocks: generation.graph_superblocks,
			last_checkpoint: Instant::now(),
		};
		Ok((
			Self {
				root: root.to_path_buf(),
				_lock: lock,
				io,
				engine,
				wal,
				state: Mutex::new(state),
				checkpoint_page_threshold,
				checkpoint_interval,
			},
			database,
		))
	}

	/// Opens, recovers, and checkpoints one existing local database.
	pub(crate) fn open(
		root: &Path,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		checkpoint_page_threshold: usize,
		checkpoint_interval: Duration,
	) -> FerrosGrynnResult<(Self, DatabaseRoot)> {
		let lock = DatabaseLock::acquire(root)?;
		cleanup_external_index_sort(root)?;
		let io = PageIo::new(io_mode)?;
		let engine = HybridReadEngine::new(pool_pages);
		let (wal, opened) = open_local_state(root, &engine, io.file_open_mode())?;
		let OpenedLocalState {
			root: database,
			database_superblock,
			file_catalog,
			database_catalog,
			bindings,
			registry,
			atom_superblock,
			graph_superblocks,
		} = opened;
		cleanup_orphan_generations(root, &file_catalog)?;
		let state = LocalState {
			database_superblock_path: root.join(DATABASE_SUPERBLOCK_FILE),
			database_superblock,
			file_catalog,
			_database_catalog: database_catalog,
			bindings,
			registry,
			atom_superblock,
			graph_superblocks,
			last_checkpoint: Instant::now(),
		};
		let store = Self {
			root: root.to_path_buf(),
			_lock: lock,
			io,
			engine,
			wal,
			state: Mutex::new(state),
			checkpoint_page_threshold,
			checkpoint_interval,
		};
		store.checkpoint(CheckpointMode::Passive)?;
		Ok((store, database))
	}

	/// Persists one already validated MVCC root before it becomes visible.
	pub(crate) fn persist_root(&self, database: &DatabaseRoot) -> FerrosGrynnResult<()> {
		self.persist_generation(database, false).map(drop)
	}

	/// Persists one import generation directly to unreachable home files and
	/// publishes only its catalogs through one WAL transaction.
	pub(crate) fn persist_import_root(
		&self,
		database: &DatabaseRoot,
	) -> FerrosGrynnResult<BTreeMap<IndexId, Duration>> {
		self.persist_generation(database, true)
	}

	fn persist_generation(
		&self,
		database: &DatabaseRoot,
		import: bool,
	) -> FerrosGrynnResult<BTreeMap<IndexId, Duration>> {
		let mut state = self.state.lock();
		self.checkpoint_if_due(&mut state)?;
		let expected = self
			.wal
			.snapshot()
			.commit_ts()
			.get()
			.checked_add(1)
			.ok_or_else(|| invalid("local commit clock is exhausted"))?;
		if database.commit_ts != expected {
			return Err(invalid("MVCC and frame-WAL commit clocks diverged"));
		}
		let (mut generation, mut unpublished) = if import {
			let (generation, unpublished) = build_import_generation(
				database,
				state.file_catalog.next_file_instance_id,
				&self.root,
			)?;
			(generation, Some(unpublished))
		} else {
			(build_generation(database, state.file_catalog.next_file_instance_id)?, None)
		};
		#[cfg(test)]
		if import {
			inject_import_failure(ImportFailurePoint::AfterBuild)?;
		}
		if import {
			create_import_generation_files(&self.root, &generation)?;
		} else {
			create_generation_files(&self.root, &generation, false)?;
		}
		#[cfg(test)]
		if import {
			inject_import_failure(ImportFailurePoint::AfterFileSync)?;
		}
		let registry = FileRegistry::open(
			generation.file_catalog.registrations(&self.root),
			self.io.file_open_mode(),
			&rapidhash::RapidHashSet::default(),
		)?;
		let mut tx = self.wal.begin_tx()?;
		if import {
			stage_import_catalog(&mut tx, &generation)?;
		} else {
			stage_generation(&mut tx, &generation)?;
		}
		#[cfg(test)]
		if import {
			inject_import_failure(ImportFailurePoint::AfterCatalogStage)?;
			inject_import_failure(ImportFailurePoint::BeforeWalCommit)?;
		}
		let outcome = self.wal.commit_tx(tx)?;
		if let Some(generation) = &mut unpublished {
			generation.publish();
		}
		if outcome.commit_ts().get() != database.commit_ts {
			return Err(invalid("WAL published an unexpected logical commit timestamp"));
		}
		state.file_catalog = generation.file_catalog;
		state._database_catalog = generation.database_catalog;
		state.bindings = generation.bindings;
		state.registry = registry;
		state.atom_superblock = generation.atom_superblock;
		state.graph_superblocks = generation.graph_superblocks;
		Ok(std::mem::take(&mut generation.index_timings))
	}

	/// Runs an explicit database-wide checkpoint.
	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		let mut state = self.state.lock();
		let advanced = checkpoint(&self.wal, &mut state, mode)?;
		if advanced {
			state.last_checkpoint = Instant::now();
		}
		Ok(advanced)
	}

	fn checkpoint_if_due(&self, state: &mut LocalState) -> FerrosGrynnResult<()> {
		let pending = self.wal.unbackfilled_page_count();
		if pending != 0
			&& (pending >= self.checkpoint_page_threshold
				|| state.last_checkpoint.elapsed() >= self.checkpoint_interval)
		{
			checkpoint(&self.wal, state, CheckpointMode::Passive)?;
			state.last_checkpoint = Instant::now();
		}
		Ok(())
	}
}

#[cfg(test)]
fn inject_import_failure(point: ImportFailurePoint) -> FerrosGrynnResult<()> {
	IMPORT_FAILURE_POINT.with(|selected| {
		if selected.get() == point {
			Err(FerrosGrynnError::storage_io_failure("injected import publication failure"))
		} else {
			Ok(())
		}
	})
}

fn database_name(path: &Path) -> FerrosGrynnResult<String> {
	path.file_name()
		.and_then(|name| name.to_str())
		.filter(|name| !name.is_empty())
		.map(str::to_owned)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local database path has no UTF-8 name"))
}

fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
