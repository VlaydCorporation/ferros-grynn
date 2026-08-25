//! Snapshot-aware synchronous WAL/home page manager.

use std::{
	cmp,
	num::NonZeroUsize,
	path::{Path, PathBuf},
};

use parking_lot::{Mutex, RwLock};
use rapidhash::{RapidHashMap, RapidHashSet};

use super::{
	buffer_pool::{BufferPool, PageCacheKey, PageCacheVersion, PageGuard},
	direct::SYNC_BATCH_PAGES,
	file_registry::{DataFileRegistration, DataFileRole, FileRegistry, RegisteredDataFile},
	local::{
		DatabaseLock,
		select_database_superblock,
		select_graph_superblock,
		write_superblock_atomic,
	},
	page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
	superblock::{DatabaseSuperblock, GraphSuperblock},
	types::{GraphId, PageKey},
	wal::{CommitOutcome, FrameWal, RecoverySeed, ResolvedFrame, TxHandle},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Fixed database-superblock filename inside one local database directory.
const DATABASE_SUPERBLOCK_FILE: &str = "db.fgb";
/// Fixed frame-WAL directory inside one local database directory.
const WAL_DIRECTORY: &str = "wal";

/// A page tied to the lifetime of the transaction that selected its version.
pub(super) enum PageRead<'tx> {
	/// Read-your-writes page borrowed from the private transaction write set.
	Staged(&'tx AlignedPage),
	/// Immutable WAL or home image pinned in the bounded buffer pool.
	Cached(PageGuard),
}

impl PageRead<'_> {
	/// Returns the complete immutable page bytes.
	pub(super) fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		match self {
			Self::Staged(page) => page.as_bytes(),
			Self::Cached(page) => page.as_bytes(),
		}
	}
}

/// Checkpoint authority published by the database superblock.
struct CheckpointAuthority {
	/// Database-wide clocks and home generation.
	database: DatabaseSuperblock,
	/// Latest selected checkpoint-owned metadata per graph.
	graphs: RapidHashMap<GraphId, GraphSuperblock>,
}

/// Windows-first synchronous transaction, WAL, cache, checkpoint, and recovery
/// core.
pub(super) struct SyncPageManager {
	/// Exclusive database ownership retained for this manager's lifetime.
	_database_lock: DatabaseLock,
	/// Main database-superblock path used as the checkpoint commit point.
	database_path: PathBuf,
	/// Trusted, sealed mapping from persisted identities to open files.
	registry: FileRegistry,
	/// Database-level frame WAL and snapshot registry.
	wal: FrameWal,
	/// Fixed-budget immutable page cache.
	pool: BufferPool,
	/// Reusable calibrated extent for mixed home-file batch reads.
	read_scratch: Mutex<Vec<AlignedPage>>,
	/// Reusable calibrated extent for recovery and checkpoint backfill.
	checkpoint_scratch: Mutex<Vec<AlignedPage>>,
	/// Serializes complete passive-checkpoint attempts.
	checkpoint_mutex: Mutex<()>,
	/// Last database-authoritative generation and graph metadata.
	authority: RwLock<CheckpointAuthority>,
}

impl SyncPageManager {
	/// Opens a sealed local layout, recovers committed WAL frames, validates
	/// home files, and checkpoints recovery before publishing the manager.
	///
	/// This is crate-private because durable catalog discovery and the public
	/// `Datastore::open_local` composition root belong to stage 4.
	///
	/// # Errors
	///
	/// Returns a typed ownership, superblock, registry, WAL, Direct-I/O,
	/// recovery, validation, or checkpoint error.
	pub(super) fn open_local(
		database_directory: impl AsRef<Path>,
		registrations: Vec<DataFileRegistration>,
		pool_pages: NonZeroUsize,
	) -> FerrosGrynnResult<Self> {
		let database_directory = database_directory.as_ref();
		let database_lock = DatabaseLock::acquire(database_directory)?;
		let database_path = database_directory.join(DATABASE_SUPERBLOCK_FILE);
		let database = select_database_superblock(&database_path)?;

		let mut graphs = RapidHashMap::default();
		for registration in &registrations {
			if registration.role != DataFileRole::GraphSuperblock {
				continue;
			}
			let graph = select_graph_superblock(&registration.path)?;
			if graph.graph_id != registration.graph_id {
				return Err(FerrosGrynnError::invalid_storage_file(
					"registered GraphSuperblock identity does not match its page",
				));
			}
			if graphs.insert(graph.graph_id, graph).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"multiple GraphSuperblock files registered for one graph",
				));
			}
		}
		for registration in &registrations {
			if registration.role == DataFileRole::WalBackfilled
				&& registration.graph_id != GraphId::SYSTEM
				&& !graphs.contains_key(&registration.graph_id)
			{
				return Err(FerrosGrynnError::invalid_storage_file(
					"graph data file has no registered GraphSuperblock",
				));
			}
		}
		if usize::try_from(database.graph_count).ok() != Some(graphs.len()) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"database graph_count does not match the sealed registry",
			));
		}
		let registered_next_file_instance = registrations
			.iter()
			.map(|registration| registration.file_id.instance_id() + 1)
			.max()
			.unwrap_or(0);
		if database.next_file_instance_id < registered_next_file_instance {
			return Err(FerrosGrynnError::invalid_storage_file(
				"database next_file_instance_id would reuse a registered file identity",
			));
		}

		let registry = FileRegistry::open(registrations)?;
		let seed = RecoverySeed::new(
			database.generation,
			database.checkpoint_lsn,
			database.checkpoint_record_lsn,
			database.durable_visible_lsn,
			database.next_lsn,
			database.commit_ts,
			database.next_tx_id,
			database.next_file_instance_id,
		);
		let wal = FrameWal::open_with_seed(database_directory.join(WAL_DIRECTORY), seed)?;
		let manager = Self {
			_database_lock: database_lock,
			database_path,
			registry,
			wal,
			pool: BufferPool::new(pool_pages),
			read_scratch: Mutex::new(aligned_extent()),
			checkpoint_scratch: Mutex::new(aligned_extent()),
			checkpoint_mutex: Mutex::new(()),
			authority: RwLock::new(CheckpointAuthority {
				database,
				graphs,
			}),
		};
		manager.recover_home_files()?;
		manager.validate_home_files()?;
		manager.checkpoint()?;
		Ok(manager)
	}

	/// Starts and registers one storage transaction.
	///
	/// # Errors
	///
	/// Returns an error when the transaction-identity clock is exhausted.
	pub(super) fn begin_tx(&self) -> FerrosGrynnResult<TxHandle> {
		self.wal.begin_tx()
	}

	/// Reads one page using read-your-writes, visible WAL, then home order.
	///
	/// # Errors
	///
	/// Returns an error for a foreign transaction, unknown file, WAL or Direct
	/// I/O failure, malformed page, or cache inconsistency.
	pub(super) fn read_page<'tx>(
		&self,
		tx: &'tx TxHandle,
		key: PageKey,
	) -> FerrosGrynnResult<PageRead<'tx>> {
		self.wal.validate_tx(tx)?;
		if let Some(page) = tx.staged_page(key) {
			return Ok(PageRead::Staged(page));
		}
		let visible = self.wal.visible_frames(&[key], tx.snapshot());
		if let Some(frame) = visible.frames()[0] {
			let cache_key = PageCacheKey::new(
				key,
				PageCacheVersion::Wal(PageLsn::new(frame.frame_lsn().get())),
			);
			return self
				.pool
				.get_or_load(cache_key, |output| visible.read_frame(frame, output))
				.map(PageRead::Cached);
		}
		drop(visible);

		let version = self.home_cache_version();
		let file = self.transactional_file(key)?;
		self.pool
			.get_or_load(PageCacheKey::new(key, version), |output| {
				file.file()?.read_page(key.page_no(), output)
			})
			.map(PageRead::Cached)
	}

	/// Reads a mixed page batch in input order while preserving duplicates.
	///
	/// Home misses are grouped by file and adjacent page number by the fixed
	/// buffer pool. WAL locations remain protected by one shared prune gate.
	///
	/// # Errors
	///
	/// Returns the scalar read errors, or an error when the unique home batch
	/// exceeds the fixed pool capacity.
	pub(super) fn read_pages<'tx>(
		&self,
		tx: &'tx TxHandle,
		keys: &[PageKey],
	) -> FerrosGrynnResult<Vec<PageRead<'tx>>> {
		self.wal.validate_tx(tx)?;
		if keys.is_empty() {
			return Ok(Vec::new());
		}

		let mut output = std::iter::repeat_with(|| None).take(keys.len()).collect::<Vec<_>>();
		let mut unresolved_keys = Vec::with_capacity(keys.len());
		let mut unresolved_positions = Vec::with_capacity(keys.len());
		for (position, key) in keys.iter().copied().enumerate() {
			if let Some(page) = tx.staged_page(key) {
				output[position] = Some(PageRead::Staged(page));
			} else {
				unresolved_keys.push(key);
				unresolved_positions.push(position);
			}
		}
		if unresolved_keys.is_empty() {
			return collect_page_reads(output);
		}
		let unique_cached_pages =
			unresolved_keys.iter().copied().collect::<RapidHashSet<_>>().len();
		if unique_cached_pages > self.pool.capacity() {
			return Err(FerrosGrynnError::storage_io_failure(
				"unique page batch exceeds buffer-pool capacity",
			));
		}

		let visible = self.wal.visible_frames(&unresolved_keys, tx.snapshot());
		let mut home_keys = Vec::new();
		let mut home_positions = Vec::new();
		let home_version = self.home_cache_version();
		for (unresolved_index, frame) in visible.frames().iter().copied().enumerate() {
			let key = unresolved_keys[unresolved_index];
			let position = unresolved_positions[unresolved_index];
			if let Some(frame) = frame {
				let cache_key = PageCacheKey::new(
					key,
					PageCacheVersion::Wal(PageLsn::new(frame.frame_lsn().get())),
				);
				let guard =
					self.pool.get_or_load(cache_key, |page| visible.read_frame(frame, page))?;
				output[position] = Some(PageRead::Cached(guard));
			} else {
				self.transactional_file(key)?;
				home_keys.push(PageCacheKey::new(key, home_version));
				home_positions.push(position);
			}
		}
		drop(visible);

		if !home_keys.is_empty() {
			let mut scratch = self.read_scratch.lock();
			let guards =
				self.pool.get_or_load_batch(&home_keys, &mut scratch, |first, pages| {
					let file = self.transactional_file(first)?;
					file.file()?.read_pages_raw(first.page_no(), pages)
				})?;
			for (position, guard) in home_positions.into_iter().zip(guards) {
				output[position] = Some(PageRead::Cached(guard));
			}
		}
		collect_page_reads(output)
	}

	/// Stages one complete page image in a transaction-private write set.
	///
	/// # Errors
	///
	/// Returns an error for a foreign transaction, graph-superblock key,
	/// unknown file, invalid page, or write-set limit exhaustion.
	pub(super) fn stage_frame(
		&self,
		tx: &mut TxHandle,
		key: PageKey,
		page: &AlignedPage,
	) -> FerrosGrynnResult<()> {
		self.wal.validate_tx(tx)?;
		self.transactional_file(key)?;
		tx.stage_page(key, page)
	}

	/// Commits a transaction through the frame WAL's single durability barrier.
	///
	/// # Errors
	///
	/// Returns validation, conflict, WAL append, or synchronization errors.
	pub(super) fn commit_tx(&self, tx: TxHandle) -> FerrosGrynnResult<CommitOutcome> {
		self.wal.commit_tx(tx)
	}

	/// Discards an in-memory transaction write set.
	#[expect(
		clippy::unused_self,
		reason = "transaction lifecycle operations intentionally stay on the manager"
	)]
	pub(super) fn abort_tx(&self, tx: TxHandle) {
		FrameWal::abort_tx(tx);
	}

	/// Runs one non-blocking passive checkpoint when reader-safe progress
	/// exists.
	///
	/// The database-superblock replacement is the only commit point. Errors
	/// before it leave WAL index/pruning untouched; errors from pruning after
	/// it leave an authoritative checkpoint plus safely retained segments.
	///
	/// # Errors
	///
	/// Returns WAL, file routing, Direct-I/O, superblock, atomic-replace, or
	/// prune errors.
	pub(super) fn checkpoint(&self) -> FerrosGrynnResult<()> {
		let _checkpoint = self.checkpoint_mutex.lock();
		let Some(target) = self.wal.capture_checkpoint_target() else {
			return Ok(());
		};
		let authority = self.authority.read();
		let generation_floor = authority
			.graphs
			.values()
			.map(|graph| graph.generation)
			.fold(authority.database.generation, cmp::max);
		drop(authority);
		let ticket = self.wal.begin_checkpoint(target, generation_floor)?;
		self.backfill_frames(ticket.frames())?;
		let checkpoint = self.wal.finish_checkpoint(ticket)?;

		let authority = self.authority.read();
		let mut next_database = authority.database.clone();
		let mut next_graphs = authority.graphs.clone();
		drop(authority);
		for graph in next_graphs.values_mut() {
			graph.generation = checkpoint.generation();
			graph.backfilled_lsn = checkpoint.target();
			graph.checkpoint_record_lsn = checkpoint.checkpoint_record_lsn();
		}
		let mut graph_ids = next_graphs.keys().copied().collect::<Vec<_>>();
		graph_ids.sort_unstable();
		let mut superblock_scratch = self.checkpoint_scratch.lock();
		let superblock_page = superblock_scratch.first_mut().ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("checkpoint extent is unexpectedly empty")
		})?;
		for graph_id in graph_ids {
			let graph = next_graphs.get(&graph_id).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"checkpoint graph disappeared from immutable authority",
				)
			})?;
			let registration = self.graph_superblock_file(graph_id)?;
			graph.encode_into(superblock_page)?;
			write_superblock_atomic(registration.path(), superblock_page)?;
		}

		let clocks = checkpoint.clocks();
		next_database.generation = checkpoint.generation();
		next_database.checkpoint_lsn = checkpoint.target();
		next_database.checkpoint_record_lsn = checkpoint.checkpoint_record_lsn();
		next_database.durable_visible_lsn = clocks.durable_visible_lsn;
		next_database.next_lsn = clocks.next_lsn;
		next_database.commit_ts = clocks.commit_ts;
		next_database.next_tx_id = clocks.next_tx_id;
		next_database.next_file_instance_id = clocks.next_file_instance_id;
		next_database.system_backfilled_lsn = checkpoint.target();
		next_database.encode_into(superblock_page)?;
		write_superblock_atomic(&self.database_path, superblock_page)?;
		drop(superblock_scratch);

		{
			let mut authority = self.authority.write();
			authority.database = next_database;
			authority.graphs = next_graphs;
		}
		self.wal.publish_checkpoint(&checkpoint)
	}

	/// Returns the logical home-cache generation and checkpoint ceiling.
	fn home_cache_version(&self) -> PageCacheVersion {
		let authority = self.authority.read();
		PageCacheVersion::Home {
			generation: authority.database.generation,
			ceiling: authority.database.checkpoint_lsn,
		}
	}

	/// Resolves a transactional file and rejects checkpoint-owned metadata.
	fn transactional_file(&self, key: PageKey) -> FerrosGrynnResult<&RegisteredDataFile> {
		let file = self.registry.resolve(key)?;
		if file.role() != DataFileRole::WalBackfilled {
			return Err(FerrosGrynnError::invalid_storage_file(
				"transactional access targeted checkpoint-owned metadata",
			));
		}
		Ok(file)
	}

	/// Resolves the sole checkpoint-owned metadata file for one graph.
	fn graph_superblock_file(&self, graph_id: GraphId) -> FerrosGrynnResult<&RegisteredDataFile> {
		self.registry
			.iter()
			.find_map(|((registered_graph, _file_id), file)| {
				(registered_graph == graph_id && file.role() == DataFileRole::GraphSuperblock)
					.then_some(file)
			})
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"graph checkpoint authority has no registered GraphSuperblock",
				)
			})
	}

	/// Copies checkpoint-selected frames into contiguous home extents and syncs
	/// each touched physical file exactly once.
	fn backfill_frames(&self, frames: &[ResolvedFrame]) -> FerrosGrynnResult<()> {
		let mut touched = RapidHashSet::default();
		let mut scratch = self.checkpoint_scratch.lock();
		let mut start = 0;
		while start < frames.len() {
			let first = frames[start];
			let mut end = start + 1;
			while end < frames.len() && end - start < scratch.len() {
				let previous = frames[end - 1].key();
				let current = frames[end].key();
				let adjacent = previous
					.page_no()
					.get()
					.checked_add(1)
					.is_some_and(|next| next == current.page_no().get());
				if current.graph_id() != first.key().graph_id()
					|| current.file_id() != first.key().file_id()
					|| !adjacent
				{
					break;
				}
				end += 1;
			}
			let extent = &frames[start..end];
			for (frame, page) in extent.iter().copied().zip(&mut scratch[..extent.len()]) {
				self.wal.read_frame(frame, page)?;
			}
			let file = self.transactional_file(first.key())?;
			file.file()?.write_pages(first.key().page_no(), &scratch[..extent.len()])?;
			touched.insert((first.key().graph_id(), first.key().file_id()));
			start = end;
		}
		for (graph_id, file_id) in touched {
			self.registry.resolve_file(graph_id, file_id)?.file()?.sync_data()?;
		}
		Ok(())
	}

	/// Replays every committed post-checkpoint frame in ascending frame LSN.
	fn recover_home_files(&self) -> FerrosGrynnResult<()> {
		let frames = self.wal.recovery_frames();
		let mut touched = RapidHashSet::default();
		let mut scratch = self.checkpoint_scratch.lock();
		let page = scratch.first_mut().ok_or_else(|| {
			FerrosGrynnError::storage_io_failure("recovery extent is unexpectedly empty")
		})?;
		for frame in frames {
			self.wal.read_frame(frame, page)?;
			let key = frame.key();
			self.transactional_file(key)?.file()?.write_page(key.page_no(), page)?;
			touched.insert((key.graph_id(), key.file_id()));
		}
		for (graph_id, file_id) in touched {
			self.registry.resolve_file(graph_id, file_id)?.file()?.sync_data()?;
		}
		Ok(())
	}

	/// Validates every initialized transactional home page after WAL replay.
	fn validate_home_files(&self) -> FerrosGrynnResult<()> {
		let mut scratch = self.checkpoint_scratch.lock();
		for ((_graph_id, _file_id), registration) in self.registry.iter() {
			if registration.role() != DataFileRole::WalBackfilled {
				continue;
			}
			let file = registration.file()?;
			let mut first = 0_u64;
			while first < file.page_count() {
				let remaining = file.page_count() - first;
				let count =
					usize::try_from(remaining.min(scratch.len() as u64)).map_err(|source| {
						FerrosGrynnError::storage_io_failure(format!(
							"home validation extent does not fit usize: {source}"
						))
					})?;
				let pages = &mut scratch[..count];
				file.read_pages_raw(PageNo::new(first), pages)?;
				for (offset, page) in pages.iter_mut().enumerate() {
					let offset = u64::try_from(offset).map_err(|source| {
						FerrosGrynnError::storage_io_failure(format!(
							"home validation offset does not fit u64: {source}"
						))
					})?;
					page.validate_mut(PageNo::new(first + offset))?;
				}
				first += count as u64;
			}
		}
		Ok(())
	}
}

/// Allocates one fixed calibrated aligned extent during manager initialization.
fn aligned_extent() -> Vec<AlignedPage> {
	(0..SYNC_BATCH_PAGES).map(|_| AlignedPage::zeroed()).collect()
}

/// Converts fully populated optional output slots without panicking.
fn collect_page_reads(reads: Vec<Option<PageRead<'_>>>) -> FerrosGrynnResult<Vec<PageRead<'_>>> {
	reads
		.into_iter()
		.map(|read| {
			read.ok_or_else(|| {
				FerrosGrynnError::storage_io_failure("page-manager batch left an unresolved output")
			})
		})
		.collect()
}

#[cfg(all(test, windows))]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible native storage fixtures return Result while assertions report contract \
	          failures"
)]
mod tests {
	use std::{
		error::Error,
		fs,
		io,
		path::Path,
		sync::{
			Arc,
			Barrier,
			atomic::{AtomicU64, Ordering},
		},
		thread,
	};

	use super::*;
	use crate::storage::{
		direct::DirectFile,
		page::{PAGE_HEADER_SIZE, PageFlags, PageType},
		superblock::GraphSuperblockFlags,
		types::{CheckpointGeneration, CommitTs, DataFileId, DataFileKind, Lsn, TxId},
	};

	/// Monotonic suffix for collision-resistant manager fixtures.
	static NEXT_TEST_DATABASE: AtomicU64 = AtomicU64::new(0);
	/// Single graph used by focused local-storage fixtures.
	const TEST_GRAPH: GraphId = GraphId::new(1);

	/// Complete local layout removed on success and unwind.
	struct TestDatabase {
		/// Unique database directory.
		directory: PathBuf,
		/// Transactional files included in the sealed registry.
		files: Vec<(DataFileId, PathBuf)>,
		/// Checkpoint-owned graph metadata path.
		graph_superblock: PathBuf,
	}

	impl TestDatabase {
		/// Creates a generation-zero database with one initialized page per
		/// file.
		fn create(file_ids: &[DataFileId]) -> FerrosGrynnResult<Self> {
			let suffix = NEXT_TEST_DATABASE.fetch_add(1, Ordering::Relaxed);
			let directory = std::env::current_dir()
				.map_err(|source| test_io("resolve manager test directory", source))?
				.join(format!(".ferros-grynn-manager-{}-{suffix}", std::process::id()));
			fs::create_dir(&directory)
				.map_err(|source| test_io("create manager test directory", source))?;
			let database = DatabaseSuperblock {
				db_name: "test".into(),
				generation: CheckpointGeneration::ZERO,
				checkpoint_lsn: Lsn::ZERO,
				checkpoint_record_lsn: Lsn::ZERO,
				durable_visible_lsn: Lsn::ZERO,
				next_lsn: Lsn::FIRST,
				commit_ts: CommitTs::ZERO,
				next_tx_id: TxId::new(1),
				next_file_instance_id: file_ids
					.iter()
					.map(|file_id| file_id.instance_id() + 1)
					.max()
					.unwrap_or(1)
					.max(1),
				system_backfilled_lsn: Lsn::ZERO,
				graph_catalog_root_page: PageNo::NULL,
				file_catalog_root_page: PageNo::NULL,
				graph_count: 1,
			};
			write_initial_page(&directory.join(DATABASE_SUPERBLOCK_FILE), &database.encode()?)?;
			let graph_superblock = directory.join("graph.fgb");
			write_initial_page(&graph_superblock, &test_graph_superblock()?.encode()?)?;

			let mut files = Vec::with_capacity(file_ids.len());
			for file_id in file_ids.iter().copied() {
				let path = directory.join(format!("{:016x}.fgd", file_id.get()));
				let file = DirectFile::create_new(&path)?;
				file.write_page(PageNo::new(0), &marked_page(0, 0, 0)?)?;
				file.sync_data()?;
				files.push((file_id, path));
			}
			drop(FrameWal::create_new(directory.join(WAL_DIRECTORY))?);
			Ok(Self {
				directory,
				files,
				graph_superblock,
			})
		}

		/// Builds fresh registration values because opening consumes the
		/// vector.
		fn registrations(&self) -> Vec<DataFileRegistration> {
			let mut registrations = vec![DataFileRegistration {
				graph_id: TEST_GRAPH,
				file_id: DataFileId::for_kind(DataFileKind::GraphSuperblock),
				role: DataFileRole::GraphSuperblock,
				path: self.graph_superblock.clone(),
			}];
			registrations.extend(self.files.iter().map(|(file_id, path)| DataFileRegistration {
				graph_id: TEST_GRAPH,
				file_id: *file_id,
				role: DataFileRole::WalBackfilled,
				path: path.clone(),
			}));
			registrations
		}

		/// Opens the integrated manager with a bounded eight-frame pool.
		fn open(&self) -> FerrosGrynnResult<SyncPageManager> {
			self.open_with_capacity(8)
		}

		/// Opens the integrated manager with an explicit test frame budget.
		fn open_with_capacity(&self, pages: usize) -> FerrosGrynnResult<SyncPageManager> {
			let capacity = NonZeroUsize::new(pages).ok_or_else(|| {
				FerrosGrynnError::storage_io_failure("manager test capacity is zero")
			})?;
			SyncPageManager::open_local(&self.directory, self.registrations(), capacity)
		}

		/// Returns one physical key in a fixture file.
		fn key(&self, index: usize) -> PageKey {
			PageKey::new(TEST_GRAPH, self.files[index].0, PageNo::new(0))
		}
	}

	impl Drop for TestDatabase {
		/// Removes only this uniquely named fixture directory.
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.directory));
		}
	}

	/// Builds one valid graph superblock at generation zero.
	fn test_graph_superblock() -> FerrosGrynnResult<GraphSuperblock> {
		let graph = GraphSuperblock {
			graph_id: TEST_GRAPH,
			generation: CheckpointGeneration::ZERO,
			backfilled_lsn: Lsn::ZERO,
			checkpoint_record_lsn: Lsn::ZERO,
			created_at_ms: 1,
			updated_at_ms: 1,
			node_capacity: 0,
			edge_capacity: 0,
			node_count: 0,
			edge_count: 0,
			subgraph_capacity: 0,
			subgraph_count: 0,
			node_hot_pages: 0,
			edge_hot_pages: 0,
			properties_pages: 0,
			labels_size_bytes: 0,
			flags: GraphSuperblockFlags::empty(),
			schema_root_page: PageNo::NULL,
			stats_root_page: PageNo::NULL,
			graph_name: "graph".into(),
		};
		graph.encode().map(|_page| graph)
	}

	/// Creates a checksummed page with a deterministic marker.
	fn marked_page(page_no: u64, page_lsn: u64, marker: u8) -> FerrosGrynnResult<AlignedPage> {
		let mut page = AlignedPage::new(
			PageType::StorageSuperblock,
			PageFlags::empty(),
			PageLsn::new(page_lsn),
			PageNo::new(page_no),
		);
		page.as_bytes_mut()[PAGE_HEADER_SIZE] = marker;
		page.seal()?;
		Ok(page)
	}

	/// Corrupts one payload byte without updating the page checksum.
	fn tear_home_page(path: &Path) -> FerrosGrynnResult<()> {
		let file = DirectFile::open_read_write(path)?;
		let mut page = AlignedPage::zeroed();
		file.read_page_raw(PageNo::new(0), &mut page)?;
		page.as_bytes_mut()[PAGE_HEADER_SIZE] ^= 0xff;
		file.write_page_raw(PageNo::new(0), &page)?;
		file.sync_all()
	}

	/// Creates one initial superblock without exercising the replacement path.
	fn write_initial_page(path: &Path, page: &AlignedPage) -> FerrosGrynnResult<()> {
		let file = DirectFile::create_new(path)?;
		file.write_page(PageNo::new(0), page)?;
		file.sync_all()
	}

	/// Wraps fixture-only standard-library I/O in the project error boundary.
	fn test_io(operation: &str, source: io::Error) -> FerrosGrynnError {
		FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
	}

	/// An old reader keeps the passive watermark at home until it drops.
	#[test]
	fn old_reader_survives_checkpoint_and_next_checkpoint_advances() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let manager = fixture.open()?;
		let key = fixture.key(0);
		let old_reader = manager.begin_tx()?;
		let mut writer = manager.begin_tx()?;
		manager.stage_frame(&mut writer, key, &marked_page(0, 0, 7)?)?;
		let commit = manager.commit_tx(writer)?;
		assert_eq!(manager.read_page(&old_reader, key)?.as_bytes()[PAGE_HEADER_SIZE], 0);
		manager.checkpoint()?;
		assert_eq!(
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?
				.generation,
			CheckpointGeneration::ZERO
		);
		drop(old_reader);
		manager.checkpoint()?;
		let authority =
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?;
		assert_eq!(authority.generation, CheckpointGeneration::new(1));
		assert_eq!(authority.checkpoint_lsn, commit.commit_lsn());

		let fresh = manager.begin_tx()?;
		let pages = manager.read_pages(&fresh, &[key, key])?;
		assert_eq!(pages[0].as_bytes()[PAGE_HEADER_SIZE], 7);
		assert!(std::ptr::eq(pages[0].as_bytes(), pages[1].as_bytes()));
		assert_eq!(manager.pool.stats().page_buffer_allocations(), 8);
		Ok(())
	}

	/// Mixed batches preserve input order and duplicates across staged, WAL,
	/// and home resolution paths.
	#[test]
	fn mixed_batch_preserves_sources_order_and_duplicates() -> Result<(), Box<dyn Error>> {
		let file_ids = [1_u64, 2, 3]
			.into_iter()
			.map(|instance| {
				DataFileId::new(instance, DataFileKind::Index)
					.ok_or_else(|| io::Error::other("invalid mixed-batch file identity"))
			})
			.collect::<Result<Vec<_>, _>>()?;
		let fixture = TestDatabase::create(&file_ids)?;
		let manager = fixture.open()?;

		let mut committed = manager.begin_tx()?;
		manager.stage_frame(&mut committed, fixture.key(1), &marked_page(0, 0, 11)?)?;
		manager.commit_tx(committed)?;
		let mut reader = manager.begin_tx()?;
		manager.stage_frame(&mut reader, fixture.key(2), &marked_page(0, 0, 22)?)?;
		let keys = [fixture.key(2), fixture.key(1), fixture.key(0), fixture.key(1), fixture.key(0)];
		let pages = manager.read_pages(&reader, &keys)?;
		assert_eq!(
			pages.iter().map(|page| page.as_bytes()[PAGE_HEADER_SIZE]).collect::<Vec<_>>(),
			[22, 11, 0, 11, 0]
		);
		assert!(std::ptr::eq(pages[1].as_bytes(), pages[3].as_bytes()));
		assert!(std::ptr::eq(pages[2].as_bytes(), pages[4].as_bytes()));
		manager.checkpoint()?;
		assert_eq!(pages[2].as_bytes()[PAGE_HEADER_SIZE], 0);
		Ok(())
	}

	/// A mixed batch larger than the fixed frame budget fails before pinning
	/// its own partial result and cannot deadlock on capacity it already owns.
	#[test]
	fn mixed_batch_rejects_more_unique_pages_than_pool_capacity() -> Result<(), Box<dyn Error>> {
		let file_ids = [1_u64, 2, 3]
			.into_iter()
			.map(|instance| {
				DataFileId::new(instance, DataFileKind::Index)
					.ok_or_else(|| io::Error::other("invalid capacity-test file identity"))
			})
			.collect::<Result<Vec<_>, _>>()?;
		let fixture = TestDatabase::create(&file_ids)?;
		let manager = fixture.open_with_capacity(2)?;
		let reader = manager.begin_tx()?;

		assert!(
			manager.read_pages(&reader, &[fixture.key(0), fixture.key(1), fixture.key(2)]).is_err()
		);
		Ok(())
	}

	/// Recovery independently routes colliding index page numbers and repairs a
	/// torn home image from its retained full frame.
	#[test]
	fn recovery_routes_file_instances_and_repairs_torn_home() -> Result<(), Box<dyn Error>> {
		let first_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid first index id"))?;
		let second_id = DataFileId::new(2, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid second index id"))?;
		let fixture = TestDatabase::create(&[first_id, second_id])?;
		let wal = FrameWal::open(fixture.directory.join(WAL_DIRECTORY))?;
		let mut tx = wal.begin_tx()?;
		tx.stage_page(fixture.key(0), &marked_page(0, 0, 11)?)?;
		tx.stage_page(fixture.key(1), &marked_page(0, 0, 22)?)?;
		wal.commit_tx(tx)?;
		drop(wal);
		let recovered_wal = FrameWal::open(fixture.directory.join(WAL_DIRECTORY))?;
		let recovered_frames = recovered_wal.recovery_frames();
		assert_eq!(recovered_frames.len(), 2);
		for (frame, marker) in recovered_frames.iter().copied().zip([11, 22]) {
			let mut page = AlignedPage::zeroed();
			recovered_wal.read_frame(frame, &mut page)?;
			assert_eq!(page.as_bytes()[PAGE_HEADER_SIZE], marker);
		}
		drop(recovered_wal);
		tear_home_page(&fixture.files[0].1)?;

		let manager = fixture.open()?;
		let reader = manager.begin_tx()?;
		assert_eq!(manager.read_page(&reader, fixture.key(0))?.as_bytes()[PAGE_HEADER_SIZE], 11);
		assert_eq!(manager.read_page(&reader, fixture.key(1))?.as_bytes()[PAGE_HEADER_SIZE], 22);
		assert_eq!(
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?
				.generation,
			CheckpointGeneration::new(1)
		);
		Ok(())
	}

	/// Corruption without a post-checkpoint retained frame is fatal.
	#[test]
	fn recovery_rejects_torn_home_without_newer_frame() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let manager = fixture.open()?;
		let mut tx = manager.begin_tx()?;
		manager.stage_frame(&mut tx, fixture.key(0), &marked_page(0, 0, 3)?)?;
		manager.commit_tx(tx)?;
		manager.checkpoint()?;
		drop(manager);
		tear_home_page(&fixture.files[0].1)?;
		assert!(fixture.open().is_err());
		Ok(())
	}

	/// Database allocation clocks may not lag identities already present in
	/// the sealed physical layout.
	#[test]
	fn open_rejects_file_instance_clock_reuse() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(42, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let database_path = fixture.directory.join(DATABASE_SUPERBLOCK_FILE);
		let mut database = select_database_superblock(&database_path)?;
		database.next_file_instance_id = 42;
		write_superblock_atomic(&database_path, &database.encode()?)?;
		assert!(fixture.open().is_err());
		Ok(())
	}

	/// A crash after a durable graph temporary but before database replacement
	/// leaves the database authoritative; recovery completes a newer
	/// generation.
	#[test]
	fn partial_graph_generation_is_not_database_authority() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let manager = fixture.open()?;
		let mut tx = manager.begin_tx()?;
		manager.stage_frame(&mut tx, fixture.key(0), &marked_page(0, 0, 4)?)?;
		manager.commit_tx(tx)?;
		let target = manager
			.wal
			.capture_checkpoint_target()
			.ok_or_else(|| io::Error::other("checkpoint target was not captured"))?;
		let ticket = manager.wal.begin_checkpoint(target, CheckpointGeneration::ZERO)?;
		manager.backfill_frames(ticket.frames())?;
		let checkpoint = manager.wal.finish_checkpoint(ticket)?;
		let mut partial = test_graph_superblock()?;
		partial.generation = checkpoint.generation();
		partial.backfilled_lsn = checkpoint.target();
		partial.checkpoint_record_lsn = checkpoint.checkpoint_record_lsn();
		write_initial_page(
			&fixture.graph_superblock.with_extension("fgb.tmp"),
			&partial.encode()?,
		)?;
		assert_eq!(
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?
				.generation,
			CheckpointGeneration::ZERO
		);
		drop(manager);

		let recovered = fixture.open()?;
		assert_eq!(
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?
				.generation,
			CheckpointGeneration::new(2)
		);
		assert_eq!(
			select_graph_superblock(&fixture.graph_superblock)?.generation,
			CheckpointGeneration::new(2)
		);
		drop(recovered);
		Ok(())
	}

	/// The checkpoint mutex makes two simultaneous passive requests publish one
	/// generation rather than duplicate durable markers.
	#[test]
	fn concurrent_checkpoints_publish_once() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let manager = Arc::new(fixture.open()?);
		let mut tx = manager.begin_tx()?;
		manager.stage_frame(&mut tx, fixture.key(0), &marked_page(0, 0, 8)?)?;
		manager.commit_tx(tx)?;

		let barrier = Arc::new(Barrier::new(3));
		let mut workers = Vec::new();
		for _worker in 0..2 {
			let manager = Arc::clone(&manager);
			let barrier = Arc::clone(&barrier);
			workers.push(thread::spawn(move || {
				barrier.wait();
				manager.checkpoint()
			}));
		}
		barrier.wait();
		for worker in workers {
			worker.join().map_err(|_panic| io::Error::other("checkpoint worker panicked"))??;
		}
		assert_eq!(
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?
				.generation,
			CheckpointGeneration::new(1)
		);
		Ok(())
	}
}
