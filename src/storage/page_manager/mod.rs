//! Snapshot-aware synchronous WAL/home page manager.

use std::{
	cmp,
	fs,
	num::NonZeroUsize,
	ops::ControlFlow,
	path::{Path, PathBuf},
	sync::Arc,
};

use parking_lot::{Mutex, RwLock};
use rapidhash::{RapidHashMap, RapidHashSet};

use super::{
	catalog::{
		CatalogUpdatePages,
		DatabaseCatalog,
		FileCatalog,
		SYSTEM_CATALOG_PATH,
		encode_bootstrap_catalogs,
		read_catalogs_with,
	},
	consts::{DATABASE_SUPERBLOCK_FILE, WAL_DIRECTORY},
	direct::SYNC_BATCH_PAGES,
	file_registry::{DataFileRegistration, DataFileRole, FileRegistry, RegisteredDataFile},
	hybrid_engine::{
		AtomicPageGuard,
		HybridReadEngine,
		MAX_READ_SCOPE_OPERATIONS,
		ReadIntent,
		ReadRoute,
		ReadWindow,
		TransportCapabilities,
	},
	local::{
		DatabaseLock,
		select_database_superblock,
		select_graph_superblock,
		write_superblock_atomic,
	},
	page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
	page_cache::{PageCacheKey, PageCacheVersion},
	page_io::{FilePageExtent, PageIo, PageIoMode},
	superblock::{DatabaseSuperblock, GraphSuperblock},
	types::{CheckpointGeneration, CommitTs, GraphId, Lsn, PageKey, TxId},
	wal::{CommitOutcome, FrameWal, RecoverySeed, ResolvedFrame, TxHandle},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Synchronous manager composition and operations.
mod sync_manager;

pub(super) use sync_manager::SyncPageManager;

/// A page tied to the lifetime of the transaction that selected its version.
pub(super) enum PageRead<'tx> {
	/// Read-your-writes page borrowed from the private transaction write set.
	Staged(&'tx AlignedPage),
	/// Immutable WAL or home image pinned in the bounded R8 arena.
	Cached(AtomicPageGuard<'tx>),
}

impl PageRead<'_> {
	/// Returns the complete immutable aligned page.
	pub(super) fn as_page(&self) -> &AlignedPage {
		match self {
			Self::Staged(page) => page,
			Self::Cached(page) => page.as_page(),
		}
	}

	/// Returns the complete immutable page bytes.
	pub(super) fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
		self.as_page().as_bytes()
	}
}

/// Checkpoint authority published by the database superblock.
struct CheckpointAuthority {
	/// Database-wide clocks and home generation.
	database: DatabaseSuperblock,
	/// Latest selected checkpoint-owned metadata per graph.
	graphs: RapidHashMap<GraphId, GraphSuperblock>,
}

/// Converts the selected database authority into WAL recovery floors.
const fn recovery_seed(database: &DatabaseSuperblock) -> RecoverySeed {
	RecoverySeed::new(
		database.generation,
		database.checkpoint_lsn,
		database.checkpoint_record_lsn,
		database.durable_visible_lsn,
		database.next_lsn,
		database.commit_ts,
		database.next_tx_id,
		database.next_file_instance_id,
	)
}

/// Cleanup guard armed only after this process creates a new DB directory.
struct CreatedDatabaseDirectory {
	/// Exact newly-created target; never derived from persisted catalog bytes.
	path: PathBuf,
	/// Successful publication transfers ownership to the returned manager.
	armed: bool,
}

impl CreatedDatabaseDirectory {
	/// Arms cleanup for one exact directory created by the caller.
	const fn new(path: PathBuf) -> Self {
		Self {
			path,
			armed: true,
		}
	}

	/// Retains the completed database directory.
	const fn disarm(&mut self) {
		self.armed = false;
	}
}

impl Drop for CreatedDatabaseDirectory {
	/// Removes only incomplete content below the exact directory this call
	/// successfully created. All subsequently opened handles drop first.
	fn drop(&mut self) {
		if self.armed {
			drop(fs::remove_dir_all(&self.path));
		}
	}
}

/// Rejects reparse-point or symlink targets that leave the owned DB root.
fn validate_registration_paths(
	database_directory: &Path,
	registrations: &[DataFileRegistration],
) -> FerrosGrynnResult<()> {
	let root = fs::canonicalize(database_directory)
		.map_err(|source| local_io("canonicalize local database directory", source))?;
	for registration in registrations {
		let path = fs::canonicalize(&registration.path)
			.map_err(|source| local_io("canonicalize cataloged data file", source))?;
		if !path.starts_with(&root) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"cataloged data file resolves outside the local database directory",
			));
		}
	}
	Ok(())
}

/// Preserves a native local-layout failure behind the storage status boundary.
fn local_io(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
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

/// Adds an in-file page offset without wrapping a persistent address.
fn checked_page_add(first: PageNo, offset: usize) -> FerrosGrynnResult<PageNo> {
	let offset = u64::try_from(offset).map_err(|source| {
		FerrosGrynnError::invalid_storage_page(
			first.get(),
			format!("page-range offset does not fit u64: {source}"),
		)
	})?;
	first.get().checked_add(offset).map(PageNo::new).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_page(first.get(), "page-range address overflow")
	})
}

/// Produces one key in the same graph and immutable file.
fn contiguous_key(first: PageKey, offset: usize) -> FerrosGrynnResult<PageKey> {
	Ok(PageKey::new(first.graph_id(), first.file_id(), checked_page_add(first.page_no(), offset)?))
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

	/// Unique not-yet-created path owned by one cataloged-database test.
	struct TestDatabasePath(PathBuf);

	impl TestDatabasePath {
		/// Reserves a process-unique path without creating it.
		fn new(label: &str) -> FerrosGrynnResult<Self> {
			let suffix = NEXT_TEST_DATABASE.fetch_add(1, Ordering::Relaxed);
			let path = std::env::current_dir()
				.map_err(|source| test_io("resolve cataloged manager test directory", source))?
				.join(format!(".ferros-grynn-{label}-{}-{suffix}", std::process::id()));
			Ok(Self(path))
		}

		/// Returns the exact test path.
		fn path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TestDatabasePath {
		/// Removes the directory only when the tested creation produced it.
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.0));
		}
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
			SyncPageManager::open_local(
				&self.directory,
				self.registrations(),
				capacity,
				PageIoMode::Sync,
			)
		}

		/// Returns one physical key in a fixture file.
		fn key(&self, index: usize) -> PageKey {
			PageKey::new(TEST_GRAPH, self.files[index].0, PageNo::new(0))
		}
	}

	/// Catalog bootstrap is sufficient to reopen an empty database without
	/// caller-supplied physical registrations.
	#[test]
	fn cataloged_database_create_reopen_and_exclusive_create() -> Result<(), Box<dyn Error>> {
		let path = TestDatabasePath::new("cataloged-manager")?;
		let capacity = NonZeroUsize::new(8)
			.ok_or_else(|| io::Error::other("cataloged test capacity must be non-zero"))?;
		let (manager, database_catalog, file_catalog) = SyncPageManager::create_cataloged(
			path.path(),
			"cataloged",
			capacity,
			PageIoMode::Sync,
		)?;
		assert!(database_catalog.graphs.is_empty());
		assert_eq!(file_catalog.files.len(), 1);
		assert!(
			SyncPageManager::create_cataloged(
				path.path(),
				"replacement",
				capacity,
				PageIoMode::Sync,
			)
			.is_err()
		);
		drop(manager);

		let (reopened, database_catalog, file_catalog) =
			SyncPageManager::open_cataloged(path.path(), capacity, PageIoMode::Sync)?;
		assert!(database_catalog.graphs.is_empty());
		assert_eq!(file_catalog.files.len(), 1);
		assert_eq!(
			select_database_superblock(&path.path().join(DATABASE_SUPERBLOCK_FILE))?.db_name,
			"cataloged"
		);
		drop(reopened);
		Ok(())
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
		assert_eq!(manager.pool.resident_capacity(), 8);
		Ok(())
	}

	/// Restart mode invalidates existing readers and checkpoints the complete
	/// durable tail captured before a new reader may register.
	#[test]
	fn restart_checkpoint_cancels_reader_and_advances_to_tail() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let manager = fixture.open()?;
		let old_reader = manager.begin_tx()?;
		let mut writer = manager.begin_tx()?;
		manager.stage_frame(&mut writer, fixture.key(0), &marked_page(0, 0, 12)?)?;
		let commit = manager.commit_tx(writer)?;

		assert!(manager.restart_checkpoint()?);
		assert!(!old_reader.is_active());
		let authority =
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?;
		assert_eq!(authority.checkpoint_lsn, commit.commit_lsn());
		Ok(())
	}

	/// Passive maintenance drops already expired registrations without sleeps.
	#[test]
	fn passive_checkpoint_cancels_expired_reader() -> Result<(), Box<dyn Error>> {
		let file_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file id"))?;
		let fixture = TestDatabase::create(&[file_id])?;
		let manager = fixture.open()?;
		let expired = manager.begin_tx_with_max_age(std::time::Duration::ZERO)?;
		let mut writer = manager.begin_tx()?;
		manager.stage_frame(&mut writer, fixture.key(0), &marked_page(0, 0, 13)?)?;
		let commit = manager.commit_tx(writer)?;

		assert!(manager.checkpoint()?);
		assert!(!expired.is_active());
		let authority =
			select_database_superblock(&fixture.directory.join(DATABASE_SUPERBLOCK_FILE))?;
		assert_eq!(authority.checkpoint_lsn, commit.commit_lsn());
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
