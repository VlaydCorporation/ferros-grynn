//! Local durability owner for the normalized AtomStore and graph spaces.

/// Correspondence files share ordinary publication and recovery.
mod bridge;
mod catalog;
mod changefeed;
/// Pinned whole-commit reading and metadata-only coverage preflight.
mod changefeed_reader;
mod checkpoint;
mod cleanup;
/// Authoritative invocation payloads and lifecycle markers share the source
/// WAL.
mod event_outbox;
mod files;
/// Shared append-only whole-group page packing for logical side-formats.
mod history_publication;
mod image;
mod index_delta;
mod layout;
/// Query-owned graph checkpoints use the same versioned LIVE history pages.
mod live_graph;
/// Exact computed result replay through shared pinned history transport.
mod live_reader;
mod open;
mod physical_index;
mod physical_traversal;
/// Checked streaming merge shared by immutable class and label arrays.
mod point_array;
/// Base Hot pages, property appends and allocator ceilings.
mod point_base;
/// Selected immutable class posting arrays.
mod point_class;
/// Retained endpoint headers follow the global database snapshot boundary.
mod point_endpoint;
/// Stable overlays and edge weights share the sparse publication barrier.
mod point_graph;
/// Shared NODE/EDGE membership and incident adjacency publication.
mod point_incidence;
/// Selected Primary/scalar images share the source publication barrier.
mod point_index;
/// Selected immutable label arrays and shared dense-name resolution.
mod point_label;
/// Forward/reverse membership leaves and selected physical-source fallbacks.
mod point_membership;
/// Shared label namespace and bounded physical-publication preparation.
mod point_namespace;
/// Final allocator states retain transient and reused occurrence slots.
mod point_occurrence;
/// Eager statistics roots and exact sparse child-counter publication.
mod point_statistics;
/// Sparse plans retain unchanged AtomStore and graph files.
mod point_update;
mod statistics;

#[cfg(test)]
/// Regression tests for local store behavior.
mod tests;

use std::{
	collections::{BTreeMap, BTreeSet},
	fs,
	num::NonZeroUsize,
	path::{Path, PathBuf},
	time::{Duration, Instant},
};

pub use changefeed_reader::LocalChangefeedSnapshot;
use parking_lot::Mutex;
pub use physical_index::{
	PhysicalIndexAtom,
	PhysicalIndexDelta,
	PhysicalIndexEntries,
	PhysicalIndexRead,
	PhysicalScalarGroup,
};
pub use physical_traversal::{
	LocalQuerySnapshot,
	PhysicalTraversalFieldsRead,
	PhysicalTraversalInnerRead,
	PhysicalTraversalRead,
};

use self::{
	checkpoint::checkpoint,
	cleanup::{
		RetiredGeneration,
		cleanup_orphan_changefeeds,
		cleanup_orphan_generations,
		cleanup_orphan_quarantines,
		cleanup_query_runs,
	},
	files::{
		create_file_images,
		create_generation_files,
		create_import_generation_files,
		stage_database_catalog,
		stage_file_images,
		stage_generation,
		stage_import_catalog,
	},
	image::{
		build_generation,
		build_import_generation,
		build_streamed_generation_with_reuse,
		point_value_context,
	},
	index_delta::IndexReusePlan,
	layout::{DATABASE_CATALOG_ROOT, FILE_CATALOG_ROOT, GenerationBindings, invalid},
	open::{OpenedLocalState, open_local_state},
	physical_index::{PhysicalIndexFile, bind_physical_indexes},
	statistics::PhysicalStatisticsFiles,
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
	job_catalog::{JobCatalog, MAX_JOB_CATALOG_BYTES},
	local::{DatabaseLock, write_database_superblock},
	metadata_cow::{select_metadata_bytes, write_metadata_bytes},
	page_io::{PageIo, PageIoMode},
	property_dictionary::plan_property_dictionary_update,
	quarantine::{QuarantineArchiveReader, QuarantineArtifact, quarantine_path},
	types::{CheckpointGeneration, CommitTs, Lsn, TxId},
	wal::FrameWal,
};
use crate::{
	datastore::{
		AnalyzedToken,
		CheckpointMode,
		DatabaseRoot,
		FunctionId,
		IndexId,
		LocalOptions,
		LocalStorageStatus,
		MaintenanceJobInfo,
		PublicationDelta,
		SchemaAnalyzer,
		SchemaVersion,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	values::{RecordId, Uuid, Vector},
};

/// Configured job catalog path value.
const JOB_CATALOG_PATH: &str = "admin/jobs.fgb";

/// Injected interruption point used to verify physical-generation publication.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum GenerationFailurePoint {
	/// Do not inject a generation-publication failure.
	None,
	/// Interrupt after building the candidate generation.
	AfterBuild,
	/// Interrupt after synchronizing the candidate files.
	AfterFileSync,
	/// Interrupt after staging the candidate catalog state.
	AfterCatalogStage,
	/// Interrupt immediately before the WAL commit boundary.
	BeforeWalCommit,
}

#[cfg(test)]
thread_local! {
	/// Publication boundary at which generation-building tests on this thread inject a failure.
	static GENERATION_FAILURE_POINT: std::cell::Cell<GenerationFailurePoint> =
		const { std::cell::Cell::new(GenerationFailurePoint::None) };
}

/// Physical bindings retained with one current catalog generation.
pub(super) struct GraphBinding {
	/// Identifier of the graph owning this state.
	pub(super) graph_id: GraphId,
	/// Generation identifying this version of the data.
	pub(super) generation: u64,
	/// Files associated with this graph binding.
	pub(super) files: GraphFileSet,
	/// Membership associated with this graph binding.
	pub(super) membership: PhysicalIndexBinding,
	/// Edge endpoint associated with this graph binding.
	pub(super) edge_endpoint: Option<PhysicalIndexBinding>,
	/// Arena associated with this graph binding.
	pub(super) arena: GraphArenaBindings,
}

/// Mutable catalog, physical file bindings and history handles owned by one
/// local datastore.
struct LocalState {
	/// Database superblock path associated with this local state.
	database_superblock_path: PathBuf,
	/// Database superblock associated with this local state.
	database_superblock: DatabaseSuperblock,
	/// File catalog associated with this local state.
	file_catalog: FileCatalog,
	/// Database catalog associated with this local state.
	_database_catalog: DatabaseCatalog,
	/// Shared segment descriptors retained by logical snapshots.
	changefeed_segments: crate::storage::ChangefeedSegments,
	/// Snapshot-exact ceilings for private computed results.
	live_segments: crate::storage::LiveHistorySegments,
	/// Preallocated query checkpoint references installed with the WAL commit.
	live_graph_segments:
		std::sync::Arc<BTreeMap<crate::live::LiveId, crate::storage::HistorySegment>>,
	/// Last committed invocation image used to encode immutable-payload deltas.
	event_outbox: std::sync::Arc<crate::datastore::EventOutbox>,
	/// Ordered authoritative journal segments, shared with immutable MVCC
	/// roots.
	event_outbox_segments: std::sync::Arc<[crate::storage::HistorySegment]>,
	/// Physical pins shared by exact query snapshots and feed readers.
	changefeed_files: changefeed::ChangefeedFiles,
	/// Bindings associated with this local state.
	bindings: GenerationBindings,
	/// Registry associated with this local state.
	registry: FileRegistry,
	/// Atom superblock associated with this local state.
	atom_superblock: AtomStoreSuperblock,
	/// Graph superblocks associated with this local state.
	graph_superblocks: BTreeMap<GraphId, GraphSuperblock>,
	/// Physical indexes associated with this local state.
	physical_indexes: BTreeMap<IndexId, std::sync::Arc<PhysicalIndexFile>>,
	/// Physical statistics associated with this local state.
	physical_statistics: Option<std::sync::Arc<PhysicalStatisticsFiles>>,
	/// Job catalog associated with this local state.
	job_catalog: JobCatalog,
	/// Retired generations associated with this local state.
	retired_generations: Vec<RetiredGeneration>,
	/// Retired individual feed files, unlinked only after all map pins
	/// disappear.
	retired_changefeeds: Vec<cleanup::RetiredChangefeedFile>,
	/// Old binding maps protected across multiple catalog replacements.
	old_changefeed_pins: Vec<
		std::sync::Weak<
			BTreeMap<
				super::types::DataFileId,
				std::sync::Arc<super::file_registry::RegisteredDataFile>,
			>,
		>,
	>,
	/// Generation cleanup failures associated with this local state.
	generation_cleanup_failures: u64,
	/// Last checkpoint associated with this local state.
	last_checkpoint: Instant,
}

/// One exclusively owned local database and its database-level frame WAL.
pub struct LocalStore {
	/// Root associated with this local store.
	root: PathBuf,
	/// Lock associated with this local store.
	_lock: DatabaseLock,
	/// Io associated with this local store.
	io: PageIo,
	/// Engine associated with this local store.
	engine: HybridReadEngine,
	/// Wal associated with this local store.
	wal: FrameWal,
	/// State associated with this local store.
	state: Mutex<LocalState>,
	/// Checkpoint page threshold associated with this local store.
	checkpoint_page_threshold: usize,
	/// Checkpoint interval associated with this local store.
	checkpoint_interval: Duration,
}

impl LocalStore {
	/// Returns already accepted mapping roots without loading any record
	/// payload.
	pub(crate) fn bridge_binding(&self) -> Option<crate::storage::BridgeCatalogBinding> {
		self.state.lock()._database_catalog.bridges
	}

	/// Captures effective budgets and cumulative pressure without exposing
	/// physical file identities.
	pub(crate) fn runtime_status(&self, options: LocalOptions) -> LocalStorageStatus {
		let state = self.state.lock();
		let (cache_misses, coalesced_misses, evictions, capacity_waits) =
			self.engine.slow_path_counts();
		let (registered_files, open_file_handles) = state.registry.handle_counts();
		LocalStorageStatus {
			options,
			resident_capacity_pages: self.engine.resident_capacity(),
			resident_pages: self.engine.resident_count(),
			retired_resident_pages: self.engine.retired_resident_count(),
			transient_capacity_pages: self.engine.transient_capacity(),
			scatter_extent_pages: self.engine.scatter_extent_capacity(),
			sparse_window_pages: self.engine.sparse_window_capacity(),
			fixed_metadata_bytes: self.engine.fixed_metadata_bytes(),
			home_source_versions: self.engine.source_count(),
			locator_entries: self.engine.locator_entry_count(),
			registered_files,
			open_file_handles,
			cache_misses,
			coalesced_misses,
			evictions,
			capacity_waits,
			wal_unbackfilled_pages: self.wal.unbackfilled_page_count(),
			pending_retired_generation_directories: state
				.retired_generations
				.iter()
				.map(RetiredGeneration::directory_count)
				.sum(),
			generation_cleanup_failures: state.generation_cleanup_failures,
		}
	}

	/// Clears the shared resident arena for an isolated prototype measurement.
	#[cfg(feature = "storage-engine-prototype-api")]
	pub(crate) fn clear_resident_for_prototype(&self) -> FerrosGrynnResult<usize> {
		self.engine.clear_resident_for_prototype()
	}

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

	/// Creates in directory.
	fn create_in_directory(
		root: &Path,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		checkpoint_page_threshold: usize,
		checkpoint_interval: Duration,
	) -> FerrosGrynnResult<(Self, DatabaseRoot)> {
		let lock = DatabaseLock::acquire(root)?;
		cleanup_external_index_sort(root)?;
		cleanup_query_runs(root)?;
		let io = PageIo::try_from(io_mode)?;
		let engine = HybridReadEngine::new(pool_pages);
		let database = DatabaseRoot::default();
		let generation = build_generation(&database, 1)?;
		create_generation_files(root, &generation, true, None)?;
		let job_catalog = JobCatalog::default();
		let job_path = root.join(JOB_CATALOG_PATH);
		let job_parent =
			job_path.parent().ok_or_else(|| invalid("job catalog path has no parent directory"))?;
		fs::create_dir_all(job_parent)
			.map_err(|source| io_failure("create job catalog directory", source))?;
		write_metadata_bytes(&job_path, &job_catalog.encode()?)?;
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
		let physical_indexes =
			bind_physical_indexes(&generation.database_catalog, &registry, None, &BTreeSet::new())?;
		let state = LocalState {
			database_superblock_path: superblock_path,
			database_superblock: superblock,
			file_catalog: generation.file_catalog,
			_database_catalog: generation.database_catalog,
			changefeed_segments: std::sync::Arc::clone(database.changefeed_segments()),
			live_segments: std::sync::Arc::clone(database.live_segments()),
			live_graph_segments: std::sync::Arc::clone(&database.live_catalog().graph_segments),
			event_outbox: std::sync::Arc::clone(database.event_outbox()),
			event_outbox_segments: std::sync::Arc::clone(database.event_outbox_segments()),
			changefeed_files: std::sync::Arc::default(),
			bindings: generation.bindings,
			registry,
			atom_superblock: generation.atom_superblock,
			graph_superblocks: generation.graph_superblocks,
			physical_indexes,
			physical_statistics: None,
			job_catalog,
			retired_generations: Vec::new(),
			retired_changefeeds: Vec::new(),
			old_changefeed_pins: Vec::new(),
			generation_cleanup_failures: 0,
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
		cleanup_query_runs(root)?;
		let io = PageIo::try_from(io_mode)?;
		let engine = HybridReadEngine::new(pool_pages);
		let (wal, opened) = open_local_state(root, &engine, io.file_open_mode())?;
		let job_catalog = select_metadata_bytes(
			&root.join(JOB_CATALOG_PATH),
			MAX_JOB_CATALOG_BYTES,
			JobCatalog::decode,
			|catalog| catalog.generation,
			"maintenance job catalog",
		)?;
		let OpenedLocalState {
			root: database,
			database_superblock,
			file_catalog,
			database_catalog,
			bindings,
			registry,
			atom_superblock,
			graph_superblocks,
			physical_statistics,
		} = opened;
		cleanup_orphan_generations(root, &file_catalog)?;
		cleanup_orphan_quarantines(root, &file_catalog)?;
		cleanup_orphan_changefeeds(root, &file_catalog)?;
		cleanup::cleanup_orphan_events(root, &file_catalog)?;
		let physical_indexes =
			bind_physical_indexes(&database_catalog, &registry, None, &BTreeSet::new())?;
		let changefeed_files = changefeed::bind_files(&file_catalog, &database_catalog, &registry)?;
		let state = LocalState {
			database_superblock_path: root.join(DATABASE_SUPERBLOCK_FILE),
			database_superblock,
			file_catalog,
			_database_catalog: database_catalog,
			changefeed_segments: std::sync::Arc::clone(database.changefeed_segments()),
			live_segments: std::sync::Arc::clone(database.live_segments()),
			live_graph_segments: std::sync::Arc::clone(&database.live_catalog().graph_segments),
			event_outbox: std::sync::Arc::clone(database.event_outbox()),
			event_outbox_segments: std::sync::Arc::clone(database.event_outbox_segments()),
			changefeed_files,
			bindings,
			registry,
			atom_superblock,
			graph_superblocks,
			physical_indexes,
			physical_statistics: Some(physical_statistics),
			job_catalog,
			retired_generations: Vec::new(),
			retired_changefeeds: Vec::new(),
			old_changefeed_pins: Vec::new(),
			generation_cleanup_failures: 0,
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

	/// Returns the recovered immutable maintenance ledger projection.
	pub(crate) fn maintenance_jobs(&self) -> Vec<MaintenanceJobInfo> {
		self.state.lock().job_catalog.jobs.clone()
	}

	/// Opens one catalog-reachable immutable rejected-item file.
	pub(crate) fn open_quarantine(&self, id: Uuid) -> FerrosGrynnResult<QuarantineArchiveReader> {
		let expected = format!("admin/quarantine/{id}.fgq");
		let registered = self.state.lock().file_catalog.files.iter().any(|entry| {
			entry.file_id.kind() == Some(crate::storage::types::DataFileKind::ImportQuarantine)
				&& entry.path.as_str() == expected
		});
		if !registered {
			return Err(FerrosGrynnError::invalid_argument("quarantine identity does not exist"));
		}
		let reader = QuarantineArchiveReader::open(&quarantine_path(&self.root, id))?;
		if reader.id() != id {
			return Err(invalid("quarantine path and file identity disagree"));
		}
		Ok(reader)
	}

	/// Removes one immutable rejected-item set through a catalog publication.
	pub(crate) fn remove_quarantine(
		&self,
		database: &DatabaseRoot,
		id: Uuid,
		analyze_function: &mut dyn FnMut(
			&SchemaAnalyzer,
			&str,
		) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
		distance_function: &mut dyn FnMut(
			FunctionId,
			SchemaVersion,
			&Vector,
			&Vector,
		) -> FerrosGrynnResult<f64>,
	) -> FerrosGrynnResult<()> {
		let expected = format!("admin/quarantine/{id}.fgq");
		if !self.state.lock().file_catalog.files.iter().any(|entry| {
			entry.file_id.kind() == Some(crate::storage::types::DataFileKind::ImportQuarantine)
				&& entry.path.as_str() == expected
		}) {
			return Err(FerrosGrynnError::invalid_argument("quarantine identity does not exist"));
		}
		self.persist_generation(
			database,
			None,
			None,
			false,
			None,
			Some(id),
			analyze_function,
			distance_function,
		)
		.map(drop)?;
		// The catalog publication is authoritative. A reader may still pin the
		// file on Windows; startup orphan cleanup retries physical removal.
		drop(fs::remove_file(quarantine_path(&self.root, id)));
		Ok(())
	}

	/// Replaces the durable job ledger through `FGJB` metadata COW.
	pub(crate) fn persist_maintenance_jobs(
		&self,
		jobs: &[MaintenanceJobInfo],
	) -> FerrosGrynnResult<()> {
		let mut state = self.state.lock();
		let generation = state
			.job_catalog
			.generation
			.checked_add(1)
			.ok_or_else(|| invalid("job catalog generation is exhausted"))?;
		let catalog = JobCatalog {
			generation,
			jobs: jobs.to_vec(),
		};
		write_metadata_bytes(&self.root.join(JOB_CATALOG_PATH), &catalog.encode()?)?;
		state.job_catalog = catalog;
		Ok(())
	}

	/// Persists one already validated MVCC root before it becomes visible.
	pub(crate) fn persist_root(
		&self,
		database: &DatabaseRoot,
		previous: Option<&DatabaseRoot>,
		delta: Option<&PublicationDelta>,
		graph_catalog_only: bool,
		dictionary_changed: bool,
		analyze_function: &mut dyn FnMut(
			&SchemaAnalyzer,
			&str,
		) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
		distance_function: &mut dyn FnMut(
			FunctionId,
			SchemaVersion,
			&Vector,
			&Vector,
		) -> FerrosGrynnResult<f64>,
	) -> FerrosGrynnResult<()> {
		if graph_catalog_only {
			self.persist_existing_files(database, dictionary_changed, None)
		} else if let (Some(previous), Some(delta)) = (previous, delta)
			&& point_update::PointUpdatePlan::eligible(previous, database, delta)?
		{
			self.persist_existing_files(database, dictionary_changed, Some((previous, delta)))
		} else {
			self.persist_generation(
				database,
				previous,
				delta.map(|delta| &delta.records),
				false,
				None,
				None,
				analyze_function,
				distance_function,
			)
			.map(drop)
		}
	}

	/// Publishes a catalog and optional sparse data plan through one WAL
	/// barrier.
	fn persist_existing_files(
		&self,
		database: &DatabaseRoot,
		dictionary_changed: bool,
		point_records: Option<(&DatabaseRoot, &PublicationDelta)>,
	) -> FerrosGrynnResult<()> {
		let mut state = self.state.lock();
		if !database.live_catalog().queries.is_empty() {
			sweep_retired_generations(&mut state);
		}
		self.wal.ensure_writable()?;
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

		let mut publication_budget = point_namespace::PublicationBudget::new();
		let mut context = point_value_context(
			database,
			&state._database_catalog.time_zones,
			&mut publication_budget,
		)?;
		let mut labels = point_records
			.map(|(previous, delta)| {
				point_namespace::LabelPlan::build_with_budget(
					self,
					&state,
					previous,
					database,
					delta,
					std::mem::replace(
						&mut publication_budget,
						point_namespace::PublicationBudget::new(),
					),
				)
			})
			.transpose()?;
		let point = point_records
			.map(|(previous, delta)| {
				point_update::PointUpdatePlan::build(
					self,
					&state,
					previous,
					database,
					delta,
					labels.as_mut().ok_or_else(|| invalid("point label plan is absent"))?,
					&mut context,
				)
			})
			.transpose()?;
		let mut point_indexes = match point_records {
			Some((previous, delta)) => point_index::PointIndexPlan::build(
				self,
				&state,
				previous,
				database,
				delta,
				point.as_ref().map(|plan| &plan.graph_arenas),
				labels.as_mut().ok_or_else(|| invalid("point label plan is absent"))?,
				&mut context,
			)?,
			None => None,
		};
		let endpoints = point_endpoint::EndpointRevisionPlan::build(
			self,
			&state,
			database,
			labels.as_mut().map_or(&mut publication_budget, |plan| &mut plan.budget),
			point_indexes.as_ref().map(|plan| &plan.endpoints),
		)?;
		let mut catalog = self::catalog::encode_point_catalog(
			database,
			state._database_catalog.atom_store,
			&state.bindings,
			point_indexes.as_ref(),
			point.as_ref(),
			&mut context,
		)?;
		let mut next_atom_superblock =
			point.as_ref().map_or(state.atom_superblock, |plan| plan.atom_superblock);
		let mut dictionary_update = None;
		if dictionary_changed {
			let update = plan_property_dictionary_update(
				&mut |number, destination: &mut super::page::AlignedPage| {
					self.read_update_page_into(
						&state,
						super::types::PageKey::new(
							crate::datastore::DataSpaceId::Database,
							state.bindings.atoms.property_dictionary,
							number,
						),
						destination,
					)
				},
				state.atom_superblock.property_dictionary_pages,
				database.dictionary.as_ref(),
				super::page::PageLsn::ZERO,
			)?;
			next_atom_superblock.property_dictionary_pages = update.page_ceiling;
			// Overlay-only and pure dictionary commits also use database
			// authority; a failed checkpoint can leave subordinate
			// home headers ahead.
			next_atom_superblock.page_lsn = super::page::PageLsn::ZERO;
			next_atom_superblock.checkpoint_generation = state.database_superblock.generation;
			next_atom_superblock.backfilled_lsn = state.database_superblock.checkpoint_lsn;
			next_atom_superblock.checkpoint_record_lsn =
				state.database_superblock.checkpoint_record_lsn;
			dictionary_update = Some(update);
		}

		let capture = catalog
			.changefeeds
			.iter()
			.any(|entry| database.change_history().contains_key(&(entry.id, database.commit_ts)));
		let live_capture = catalog
			.live_history
			.iter()
			.any(|entry| changefeed::fresh_live_groups(database, entry.id).next().is_some());
		let refresh_files = point_indexes.as_ref().is_some_and(|plan| plan.catalog.is_some())
			|| capture
			|| database.bridges.generation
				!= state._database_catalog.bridges.map_or(0, |binding| binding.generation)
			|| live_capture
			|| !database.live_catalog().prepared_graphs.is_empty()
			|| database.live_catalog().graph_segments != state.live_graph_segments
			|| !std::sync::Arc::ptr_eq(database.event_outbox(), &state.event_outbox)
			|| database.changefeed_segments() != &state.changefeed_segments
			|| database.live_segments() != &state.live_segments;
		let mut feed_files =
			point_indexes.as_mut().map_or_else(Vec::new, |plan| std::mem::take(&mut plan.files));
		let mut pending_mappings = None;
		let mut file_catalog = None;
		let mut segments = None;
		let mut live_segments = None;
		let mut live_graph_segments = None;
		let mut event_segments = None;
		if refresh_files {
			let mut files = point_indexes
				.as_ref()
				.and_then(|plan| plan.catalog.as_ref())
				.unwrap_or(&state.file_catalog)
				.clone();
			pending_mappings = bridge::prepare_catalog(
				database,
				&mut files,
				&mut catalog,
				&mut feed_files,
				&self.root,
			)?;
			changefeed::prepare(
				database,
				&state.file_catalog,
				&mut files,
				&mut catalog,
				&mut feed_files,
			)?;
			event_outbox::prepare(
				database,
				&state.event_outbox,
				&state.file_catalog,
				&mut files,
				&mut catalog,
				&mut feed_files,
			)?;
			image::append_catalog_pages(&mut feed_files, &files, &catalog)?;
			segments = Some(catalog::decode_changefeed_segments(&catalog));
			live_segments = Some(catalog::decode_live_segments(&catalog));
			live_graph_segments = Some(catalog::decode_live_graph_segments(&catalog));
			event_segments = Some(std::sync::Arc::from(catalog.event_outbox_segments.as_slice()));
			file_catalog = Some(files);
		}
		#[cfg(test)]
		inject_generation_failure(GenerationFailurePoint::AfterBuild)?;
		let mut tx = self.wal.begin_tx()?;
		endpoints.stage(&mut tx)?;
		if let Some(plan) = &point {
			plan.stage(&mut tx)?;
		}
		if let Some(plan) = &point_indexes {
			plan.stage(&mut tx)?;
		}
		if let Some(update) = &dictionary_update {
			update.stage(&mut tx, state.bindings.atoms.property_dictionary)?;
			tx.stage_page(
				super::types::PageKey::new(
					crate::datastore::DataSpaceId::Database,
					state.bindings.atoms.superblock,
					super::page::PageNo::new(0),
				),
				&next_atom_superblock.encode()?,
			)?;
		}
		if refresh_files {
			stage_file_images(&mut tx, &feed_files, false)?;
		} else {
			stage_database_catalog(&mut tx, &catalog)?;
		}
		let mut pending_feeds = cleanup::PendingChangefeedFiles::new(&self.root, &feed_files)?;
		let registry = if let Some(files) = &file_catalog {
			create_file_images(&self.root, &feed_files, false, false, Some(&state.registry))?;
			#[cfg(test)]
			inject_generation_failure(GenerationFailurePoint::AfterFileSync)?;
			Some(FileRegistry::open_with_reuse(
				files.registrations(&self.root),
				self.io.file_open_mode(),
				&changefeed::degradable(files),
				Some(&state.registry),
			)?)
		} else {
			None
		};
		let feed_pins = match (&file_catalog, &registry) {
			(Some(files), Some(registry)) => {
				event_outbox::validate_files(files, &catalog, registry)?;
				Some(changefeed::bind_files(files, &catalog, registry)?)
			}
			_ => None,
		};
		// Leaf-only publication also needs fresh owners; pinned old caches stay
		// immutable.
		let physical_indexes = point_indexes
			.as_ref()
			.map(|plan| {
				bind_physical_indexes(
					&catalog,
					registry.as_ref().unwrap_or(&state.registry),
					Some(&state.physical_indexes),
					&plan.changed,
				)
			})
			.transpose()?;
		let retired_feeds = if let Some(files) = &file_catalog {
			cleanup::RetiredChangefeedFile::between(&self.root, &state.file_catalog, files)?
		} else {
			Vec::new()
		};
		prepare_feed_retirement(&mut state, retired_feeds.len(), feed_pins.is_some())?;
		let retired_mappings = match &file_catalog {
			Some(files) => {
				RetiredGeneration::between(&self.root, &state.file_catalog, files, &state.registry)?
			}
			None => None,
		};
		prepare_generation_retirement(&mut state, retired_mappings.as_ref())?;
		#[cfg(test)]
		{
			inject_generation_failure(GenerationFailurePoint::AfterCatalogStage)?;
			inject_generation_failure(GenerationFailurePoint::BeforeWalCommit)?;
		}
		pending_feeds.preserve_for_recovery();
		if let Some(cleanup) = point_indexes.as_mut().and_then(|plan| plan.cleanup.as_mut()) {
			cleanup.preserve_for_recovery();
		}
		if let Some(cleanup) = &mut pending_mappings {
			cleanup.preserve_for_recovery();
		}
		let outcome = self.wal.commit_tx(tx)?;
		if outcome.commit_ts().get() != database.commit_ts {
			return Err(invalid("WAL published an unexpected logical commit timestamp"));
		}
		catalog::install_graph_metadata(&mut state.bindings.graphs, &catalog);
		state._database_catalog = catalog;
		if let Some(plan) = point_indexes {
			catalog::install_point_files(&mut state.bindings, plan);
		}
		if let Some(indexes) = physical_indexes {
			state.physical_indexes = indexes;
		}
		if let Some(retired) = retired_mappings {
			state.retired_generations.push(retired);
		}
		if let Some(files) = file_catalog {
			state.file_catalog = files;
		}
		if let Some(segments) = segments {
			state.changefeed_segments = segments;
		}
		if let Some(segments) = live_segments {
			state.live_segments = segments;
		}
		if let Some(segments) = live_graph_segments {
			state.live_graph_segments = segments;
		}
		if let Some(segments) = event_segments {
			state.event_outbox_segments = segments;
		}
		state.event_outbox = std::sync::Arc::clone(database.event_outbox());
		if let Some(registry) = registry {
			state.registry = registry;
		}
		if let Some(pins) = feed_pins {
			remember_feed_pins(&mut state);
			state.changefeed_files = pins;
		}
		state.retired_changefeeds.extend(retired_feeds);
		state.atom_superblock = next_atom_superblock;
		if let Some(plan) = &point {
			for (&graph, &header) in &plan.graph_superblocks {
				// Preparation validated each existing key under this same lock;
				// replacing its value cannot allocate a new map entry.
				state.graph_superblocks.insert(graph, header);
			}
		}
		if point.as_ref().is_some_and(|plan| plan.replaces_statistics) {
			// Counter preparation completed the candidate before WAL; old
			// snapshots retain their own lazy owner and its WAL pin.
			state.physical_statistics = None;
		}
		// Catalog-only publication keeps the immutable statistics files and
		// their deferred binding, including across LIVE ACK and CLOSE.
		if database.live_catalog().queries.is_empty() {
			sweep_retired_generations(&mut state);
		}
		Ok(())
	}

	/// Persists one import generation directly to unreachable home files and
	/// publishes only its catalogs through one WAL transaction.
	pub(crate) fn persist_import_root(
		&self,
		database: &DatabaseRoot,
		quarantine: Option<&mut QuarantineArtifact>,
		analyze_function: &mut dyn FnMut(
			&SchemaAnalyzer,
			&str,
		) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
		distance_function: &mut dyn FnMut(
			FunctionId,
			SchemaVersion,
			&Vector,
			&Vector,
		) -> FerrosGrynnResult<f64>,
	) -> FerrosGrynnResult<BTreeMap<IndexId, Duration>> {
		self.persist_generation(
			database,
			None,
			None,
			true,
			quarantine,
			None,
			analyze_function,
			distance_function,
		)
	}

	/// Returns the persist generation result.
	fn persist_generation(
		&self,
		database: &DatabaseRoot,
		previous: Option<&DatabaseRoot>,
		changed_records: Option<&std::collections::BTreeSet<RecordId>>,
		import: bool,
		mut quarantine: Option<&mut QuarantineArtifact>,
		removed_quarantine: Option<Uuid>,
		analyze_function: &mut dyn FnMut(
			&SchemaAnalyzer,
			&str,
		) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
		distance_function: &mut dyn FnMut(
			FunctionId,
			SchemaVersion,
			&Vector,
			&Vector,
		) -> FerrosGrynnResult<f64>,
	) -> FerrosGrynnResult<BTreeMap<IndexId, Duration>> {
		if database.statistics.auxiliary_deferred() {
			return Err(invalid("full generation requires prepared publication statistics"));
		}
		self.wal.ensure_writable()?;
		let mut reuse = match (previous, changed_records, import) {
			(Some(previous), Some(changed), false) => {
				self.prepare_index_reuse(previous, database, changed, analyze_function)?
			}
			_ => IndexReusePlan::default(),
		};
		let mut state = self.state.lock();
		if !import {
			reuse.retain_graph_artifacts(
				database,
				&state._database_catalog,
				&state.file_catalog,
			)?;
		}
		if !database.live_catalog().queries.is_empty() {
			sweep_retired_generations(&mut state);
		}
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
				analyze_function,
				distance_function,
			)?;
			(generation, Some(unpublished))
		} else {
			let (generation, unpublished) = build_streamed_generation_with_reuse(
				database,
				state.file_catalog.next_file_instance_id,
				&self.root,
				reuse,
				analyze_function,
				distance_function,
			)?;
			(generation, Some(unpublished))
		};
		generation.update_quarantines(
			&state.file_catalog,
			quarantine.as_ref().map(|artifact| artifact.id()),
			removed_quarantine,
		)?;
		changefeed::prepare(
			database,
			&state.file_catalog,
			&mut generation.file_catalog,
			&mut generation.database_catalog,
			&mut generation.files,
		)?;
		event_outbox::prepare(
			database,
			&state.event_outbox,
			&state.file_catalog,
			&mut generation.file_catalog,
			&mut generation.database_catalog,
			&mut generation.files,
		)?;
		if !generation.database_catalog.changefeeds.is_empty()
			|| !generation.database_catalog.live_history.is_empty()
			|| generation
				.database_catalog
				.live_queries
				.iter()
				.any(|entry| entry.graph_checkpoint.is_some())
			|| !generation.database_catalog.event_outbox_segments.is_empty()
		{
			generation.refresh_catalogs()?;
		}
		let segments = catalog::decode_changefeed_segments(&generation.database_catalog);
		let live_segments = catalog::decode_live_segments(&generation.database_catalog);
		let live_graph_segments = catalog::decode_live_graph_segments(&generation.database_catalog);
		let event_segments =
			std::sync::Arc::from(generation.database_catalog.event_outbox_segments.as_slice());
		#[cfg(test)]
		inject_generation_failure(GenerationFailurePoint::AfterBuild)?;
		let mut pending_feeds =
			cleanup::PendingChangefeedFiles::new(&self.root, &generation.files)?;
		let mut tx = self.wal.begin_tx()?;
		if import {
			stage_import_catalog(&mut tx, &generation)?;
		} else {
			stage_generation(&mut tx, &generation)?;
		}
		if import {
			create_import_generation_files(&self.root, &generation, &state.registry)?;
		} else {
			create_generation_files(&self.root, &generation, false, Some(&state.registry))?;
		}
		if let Some(artifact) = quarantine.as_deref_mut() {
			artifact.install(&self.root)?;
		}
		#[cfg(test)]
		inject_generation_failure(GenerationFailurePoint::AfterFileSync)?;
		let registry = FileRegistry::open_with_reuse(
			generation.file_catalog.registrations(&self.root),
			self.io.file_open_mode(),
			&changefeed::degradable(&generation.file_catalog),
			Some(&state.registry),
		)?;
		let feed_pins = changefeed::bind_files(
			&generation.file_catalog,
			&generation.database_catalog,
			&registry,
		)?;
		event_outbox::validate_files(
			&generation.file_catalog,
			&generation.database_catalog,
			&registry,
		)?;
		let physical_indexes = bind_physical_indexes(
			&generation.database_catalog,
			&registry,
			Some(&state.physical_indexes),
			&BTreeSet::new(),
		)?;
		let retired = RetiredGeneration::between(
			&self.root,
			&state.file_catalog,
			&generation.file_catalog,
			&state.registry,
		)?;
		let retired_feeds = cleanup::RetiredChangefeedFile::between(
			&self.root,
			&state.file_catalog,
			&generation.file_catalog,
		)?;
		prepare_generation_retirement(&mut state, retired.as_ref())?;
		prepare_feed_retirement(&mut state, retired_feeds.len(), true)?;
		#[cfg(test)]
		{
			inject_generation_failure(GenerationFailurePoint::AfterCatalogStage)?;
			inject_generation_failure(GenerationFailurePoint::BeforeWalCommit)?;
		}
		if let Some(generation) = &mut unpublished {
			generation.preserve_for_recovery();
		}
		if let Some(cleanup) = &mut generation.mapping_cleanup {
			cleanup.preserve_for_recovery();
		}
		pending_feeds.preserve_for_recovery();
		if let Some(artifact) = quarantine.as_deref_mut() {
			artifact.preserve_for_recovery();
		}
		let outcome = self.wal.commit_tx(tx)?;
		if let Some(generation) = &mut unpublished {
			generation.publish();
		}
		if let Some(artifact) = quarantine {
			artifact.publish();
		}
		if outcome.commit_ts().get() != database.commit_ts {
			return Err(invalid("WAL published an unexpected logical commit timestamp"));
		}
		state.file_catalog = generation.file_catalog;
		state._database_catalog = generation.database_catalog;
		state.changefeed_segments = segments;
		state.live_segments = live_segments;
		state.live_graph_segments = live_graph_segments;
		state.event_outbox_segments = event_segments;
		state.event_outbox = std::sync::Arc::clone(database.event_outbox());
		remember_feed_pins(&mut state);
		state.changefeed_files = feed_pins;
		state.retired_changefeeds.extend(retired_feeds);
		state.bindings = generation.bindings;
		state.registry = registry;
		state.atom_superblock = generation.atom_superblock;
		state.graph_superblocks = generation.graph_superblocks;
		state.physical_indexes = physical_indexes;
		state.physical_statistics = None;
		if let Some(retired) = retired {
			state.retired_generations.push(retired);
		}
		// LIVE handoff follows immediately after returning to datastore.
		// Its existing idle maintenance wake retires these generations later.
		if database.live_catalog().queries.is_empty() {
			sweep_retired_generations(&mut state);
		}
		Ok(std::mem::take(&mut generation.index_timings))
	}

	/// Runs an explicit database-wide checkpoint.
	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		let mut state = self.state.lock();
		let advanced = checkpoint(&self.wal, &mut state, mode)?;
		if advanced {
			state.last_checkpoint = Instant::now();
		}
		sweep_retired_generations(&mut state);
		Ok(advanced)
	}

	/// Returns the checkpoint if due result.
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

	/// Retries only unreferenced file cleanup from the existing idle worker.
	pub(crate) fn sweep_retired_changefeeds(&self) {
		sweep_retired_generations(&mut self.state.lock());
	}
}

/// Bounds pinned generation metadata and reserves its queue before WAL.
fn prepare_generation_retirement(
	state: &mut LocalState,
	additional: Option<&RetiredGeneration>,
) -> FerrosGrynnResult<()> {
	let count = state
		.retired_generations
		.len()
		.checked_add(usize::from(additional.is_some()))
		.ok_or_else(|| invalid("generation retirement backlog overflows"))?;
	let slots = state.retired_generations.capacity().max(count);
	let mut bytes = slots
		.checked_mul(std::mem::size_of::<RetiredGeneration>())
		.ok_or_else(|| invalid("generation retirement queue bytes overflow"))?;
	for generation in state.retired_generations.iter().chain(additional) {
		bytes = bytes
			.checked_add(generation.heap_bytes()?)
			.ok_or_else(|| invalid("generation retirement backlog bytes overflow"))?;
	}
	let maximum = crate::storage::external_sort_run_budget().min(256 * 1024 * 1024);
	if bytes > maximum {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"generation retirement backlog",
			bytes as u64,
			maximum as u64,
		));
	}
	state.retired_generations.try_reserve_exact(usize::from(additional.is_some())).map_err(
		|error| {
			invalid("generation retirement queue allocation failed").with_technical_source(error)
		},
	)?;
	let extra = state
		.retired_generations
		.capacity()
		.saturating_sub(slots)
		.checked_mul(std::mem::size_of::<RetiredGeneration>())
		.ok_or_else(|| invalid("generation retirement allocation bytes overflow"))?;
	let allocated = bytes
		.checked_add(extra)
		.ok_or_else(|| invalid("generation retirement allocated bytes overflow"))?;
	if allocated > maximum {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"generation retirement allocated backlog",
			allocated as u64,
			maximum as u64,
		));
	}
	Ok(())
}

/// Reserves retirement queues before committing; no fallible allocation
/// follows.
fn prepare_feed_retirement(
	state: &mut LocalState,
	additional: usize,
	replacing: bool,
) -> FerrosGrynnResult<()> {
	state.old_changefeed_pins.retain(|pin| pin.strong_count() != 0);
	let pins = usize::from(replacing && std::sync::Arc::strong_count(&state.changefeed_files) > 1);
	let files = state
		.retired_changefeeds
		.len()
		.checked_add(additional)
		.ok_or_else(|| invalid("feed retirement backlog overflows"))?;
	let maps = state
		.old_changefeed_pins
		.len()
		.checked_add(pins)
		.ok_or_else(|| invalid("feed pin backlog overflows"))?;
	let bytes = files
		.checked_mul(2048)
		.and_then(|bytes| bytes.checked_add(maps.checked_mul(32)?))
		.ok_or_else(|| invalid("feed retirement backlog bytes overflow"))?;
	let maximum = crate::storage::external_sort_run_budget().min(256 * 1024 * 1024);
	if bytes > maximum {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"feed retirement backlog",
			bytes as u64,
			maximum as u64,
		));
	}
	state
		.retired_changefeeds
		.try_reserve_exact(additional)
		.map_err(|_error| invalid("feed retirement queue allocation failed"))?;
	state
		.old_changefeed_pins
		.try_reserve_exact(pins)
		.map_err(|_error| invalid("feed pin queue allocation failed"))?;
	Ok(())
}

/// Preserves maps from every replacement, including readers older than one
/// generation.
fn remember_feed_pins(state: &mut LocalState) {
	if std::sync::Arc::strong_count(&state.changefeed_files) > 1 {
		state.old_changefeed_pins.push(std::sync::Arc::downgrade(&state.changefeed_files));
	}
}

/// Retries post-publication generation cleanup without changing commit
/// success. Failures remain observable and pending for the next maintenance
/// point.
fn sweep_retired_generations(state: &mut LocalState) {
	let mut failures = 0_u64;
	let mut attempts = 0_u32;
	state.old_changefeed_pins.retain(|pin| pin.strong_count() != 0);
	state.retired_changefeeds.retain(|file| {
		if attempts >= 1024 {
			return true;
		}
		attempts += 1;
		match file.sweep(&state.old_changefeed_pins) {
			Ok(removed) => !removed,
			Err(_error) => {
				failures = failures.saturating_add(1);
				true
			}
		}
	});
	if attempts == 1024 && !state.retired_changefeeds.is_empty() {
		let rotate = 1024 % state.retired_changefeeds.len();
		state.retired_changefeeds.rotate_left(rotate);
	}
	if let Ok(pinned) = cleanup::pin_retired_files(&state.retired_generations) {
		state.retired_generations.retain_mut(|generation| {
			failures = failures.saturating_add(generation.sweep(&pinned));
			!generation.is_empty()
		});
	} else {
		// Cleanup remains retryable when even its bounded pin list cannot be
		// reserved; the source commit has already been published.
		failures = failures.saturating_add(1);
	}
	state.generation_cleanup_failures = state.generation_cleanup_failures.saturating_add(failures);
}

/// Returns inject generation failure for this module.
#[cfg(test)]
fn inject_generation_failure(point: GenerationFailurePoint) -> FerrosGrynnResult<()> {
	GENERATION_FAILURE_POINT.with(|selected| {
		if selected.get() == point {
			Err(FerrosGrynnError::storage_io_failure("injected generation publication failure"))
		} else {
			Ok(())
		}
	})
}

/// Returns the database name result.
fn database_name(path: &Path) -> FerrosGrynnResult<String> {
	path.file_name()
		.and_then(|name| name.to_str())
		.filter(|name| !name.is_empty())
		.map(str::to_owned)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local database path has no UTF-8 name"))
}

/// Returns the io failure result.
fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
