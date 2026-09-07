#[allow(
	clippy::wildcard_imports,
	reason = "the manager implementation uses its facade contracts"
)]
use super::*;

/// Windows-first synchronous transaction, WAL, cache, checkpoint, and recovery
/// core.
pub(in crate::storage) struct SyncPageManager {
	/// Exclusive database ownership retained for this manager's lifetime.
	_database_lock: DatabaseLock,
	/// Main database-superblock path used as the checkpoint commit point.
	database_path: PathBuf,
	/// Trusted, sealed mapping from persisted identities to open files.
	registry: RwLock<FileRegistry>,
	/// Database-level frame WAL and snapshot registry.
	pub(super) wal: FrameWal,
	/// Fixed-budget immutable page cache and physical read router.
	pub(super) pool: HybridReadEngine,
	/// User-selected physical page transport.
	page_io: PageIo,
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
	/// Returns the currently durable logical commit clock.
	pub(in crate::storage) fn current_commit_ts(&self) -> u64 {
		self.wal.snapshot().commit_ts().get()
	}

	/// Creates a new cataloged local database without replacing an existing
	/// directory, then opens its recovery core while retaining ownership.
	///
	/// # Errors
	///
	/// Returns a typed filesystem, Direct-I/O, catalog, superblock, WAL, or
	/// manager initialization error. A failed creation removes only the exact
	/// directory successfully created by this call.
	pub(in crate::storage) fn create_cataloged(
		database_directory: impl AsRef<Path>,
		database_name: &str,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
	) -> FerrosGrynnResult<(Self, DatabaseCatalog, FileCatalog)> {
		let database_directory = database_directory.as_ref();
		fs::create_dir(database_directory)
			.map_err(|source| local_io("create local database directory", source))?;
		let mut creation = CreatedDatabaseDirectory::new(database_directory.to_path_buf());
		let database_lock = DatabaseLock::acquire(database_directory)?;

		let schema_directory = database_directory.join("schema");
		fs::create_dir(&schema_directory)
			.map_err(|source| local_io("create local schema directory", source))?;
		let database_catalog = DatabaseCatalog::empty();
		let file_catalog = FileCatalog::bootstrap()?;
		let catalogs = encode_bootstrap_catalogs(&database_catalog, &file_catalog)?;
		let catalog_path = database_directory.join(SYSTEM_CATALOG_PATH);
		let catalog_file = crate::storage::direct::DirectFile::create_new(&catalog_path)?;
		catalog_file.write_pages(PageNo::new(0), &catalogs.pages)?;
		catalog_file.sync_all()?;
		drop(catalog_file);

		let database = DatabaseSuperblock {
			db_name: database_name.to_owned(),
			generation: CheckpointGeneration::ZERO,
			checkpoint_lsn: Lsn::ZERO,
			checkpoint_record_lsn: Lsn::ZERO,
			durable_visible_lsn: Lsn::ZERO,
			next_lsn: Lsn::FIRST,
			commit_ts: CommitTs::ZERO,
			next_tx_id: TxId::new(1),
			next_file_instance_id: 1,
			system_backfilled_lsn: Lsn::ZERO,
			graph_catalog_root_page: catalogs.database_root,
			file_catalog_root_page: catalogs.file_root,
			graph_count: 0,
		};
		let database_file = crate::storage::direct::DirectFile::create_new(
			database_directory.join(DATABASE_SUPERBLOCK_FILE),
		)?;
		database_file.write_page(PageNo::new(0), &database.encode()?)?;
		database_file.sync_all()?;
		drop(database_file);
		drop(FrameWal::create_new(database_directory.join(WAL_DIRECTORY))?);

		let registrations = file_catalog.registrations(database_directory);
		validate_registration_paths(database_directory, &registrations)?;
		let manager = Self::open_owned(
			database_directory,
			registrations,
			pool_pages,
			io_mode,
			database_lock,
			database,
		)?;
		creation.disarm();
		Ok((manager, database_catalog, file_catalog))
	}

	/// Opens a database whose sealed registry is discovered from its durable
	/// file catalog rather than supplied by a caller.
	///
	/// # Errors
	///
	/// Returns an ownership, catalog, path-containment, registry, WAL,
	/// recovery, validation, or checkpoint error.
	pub(in crate::storage) fn open_cataloged(
		database_directory: impl AsRef<Path>,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
	) -> FerrosGrynnResult<(Self, DatabaseCatalog, FileCatalog)> {
		let database_directory = database_directory.as_ref();
		let database_lock = DatabaseLock::acquire(database_directory)?;
		let mut database =
			select_database_superblock(&database_directory.join(DATABASE_SUPERBLOCK_FILE))?;
		let catalog_file = crate::storage::direct::DirectFile::open_read_only(
			database_directory.join(SYSTEM_CATALOG_PATH),
		)?;
		let wal = FrameWal::open_with_seed(
			database_directory.join(WAL_DIRECTORY),
			recovery_seed(&database),
		)?;
		let catalog_tx = wal.begin_tx()?;
		let (database_catalog, file_catalog) = read_catalogs_with(
			|page_no, page| {
				let key = PageKey::new(
					GraphId::SYSTEM,
					crate::storage::catalog::SYSTEM_CATALOG_FILE_ID,
					page_no,
				);
				let visible = wal.visible_frames(&[key], catalog_tx.snapshot());
				if let Some(frame) = visible.frames()[0] {
					visible.read_frame(frame, page).map(|_header| ())
				} else {
					catalog_file.read_page(page_no, page).map(|_header| ())
				}
			},
			database.graph_catalog_root_page,
			database.file_catalog_root_page,
		)?;
		FrameWal::abort_tx(catalog_tx);
		drop(catalog_file);
		database.graph_count = u32::try_from(database_catalog.graphs.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("durable graph count exceeds u32")
		})?;
		database.next_file_instance_id =
			database.next_file_instance_id.max(file_catalog.next_file_instance_id);
		let registrations = file_catalog.registrations(database_directory);
		validate_registration_paths(database_directory, &registrations)?;
		let manager = Self::open_owned_with_wal(
			database_directory,
			registrations,
			pool_pages,
			io_mode,
			database_lock,
			database,
			wal,
		)?;
		Ok((manager, database_catalog, file_catalog))
	}

	/// Opens a sealed local layout, recovers committed WAL frames, validates
	/// home files, and checkpoints recovery before publishing the manager.
	///
	/// This remains crate-private because [`crate::datastore::Datastore`] owns
	/// durable catalog discovery and the public local composition root.
	///
	/// # Errors
	///
	/// Returns a typed ownership, superblock, registry, WAL, Direct-I/O,
	/// recovery, validation, or checkpoint error.
	pub(in crate::storage) fn open_local(
		database_directory: impl AsRef<Path>,
		registrations: Vec<DataFileRegistration>,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
	) -> FerrosGrynnResult<Self> {
		let database_directory = database_directory.as_ref();
		let database_lock = DatabaseLock::acquire(database_directory)?;
		let database_path = database_directory.join(DATABASE_SUPERBLOCK_FILE);
		let database = select_database_superblock(&database_path)?;
		Self::open_owned(
			database_directory,
			registrations,
			pool_pages,
			io_mode,
			database_lock,
			database,
		)
	}

	/// Initializes the manager after ownership, database authority, and trusted
	/// registrations have already been selected.
	fn open_owned(
		database_directory: &Path,
		registrations: Vec<DataFileRegistration>,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		database_lock: DatabaseLock,
		database: DatabaseSuperblock,
	) -> FerrosGrynnResult<Self> {
		let wal = FrameWal::open_with_seed(
			database_directory.join(WAL_DIRECTORY),
			recovery_seed(&database),
		)?;
		Self::open_owned_with_wal(
			database_directory,
			registrations,
			pool_pages,
			io_mode,
			database_lock,
			database,
			wal,
		)
	}

	/// Initializes the manager with an already scanned WAL used for catalog
	/// discovery before the sealed registry exists.
	fn open_owned_with_wal(
		database_directory: &Path,
		registrations: Vec<DataFileRegistration>,
		pool_pages: NonZeroUsize,
		io_mode: PageIoMode,
		database_lock: DatabaseLock,
		database: DatabaseSuperblock,
		wal: FrameWal,
	) -> FerrosGrynnResult<Self> {
		let database_path = database_directory.join(DATABASE_SUPERBLOCK_FILE);

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

		let page_io = PageIo::new(io_mode)?;
		let registry = FileRegistry::open(registrations, page_io.file_open_mode())?;
		let manager = Self {
			_database_lock: database_lock,
			database_path,
			registry: RwLock::new(registry),
			wal,
			pool: HybridReadEngine::new(pool_pages),
			page_io,
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
	pub(in crate::storage) fn begin_tx(&self) -> FerrosGrynnResult<TxHandle> {
		self.wal.begin_tx()
	}

	/// Starts a storage transaction with a bounded snapshot lifetime.
	pub(in crate::storage) fn begin_tx_with_max_age(
		&self,
		max_age: std::time::Duration,
	) -> FerrosGrynnResult<TxHandle> {
		self.wal.begin_tx_with_max_age(Some(max_age))
	}

	/// Reads one page using read-your-writes, visible WAL, then home order.
	///
	/// # Errors
	///
	/// Returns an error for a foreign transaction, unknown file, WAL or Direct
	/// I/O failure, malformed page, or cache inconsistency.
	pub(in crate::storage) fn read_page<'read>(
		&'read self,
		tx: &'read TxHandle,
		key: PageKey,
	) -> FerrosGrynnResult<PageRead<'read>> {
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
				self.page_io.read_page(file.file()?, key.page_no(), output)
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
	pub(in crate::storage) fn read_pages<'read>(
		&'read self,
		tx: &'read TxHandle,
		keys: &[PageKey],
	) -> FerrosGrynnResult<Vec<PageRead<'read>>> {
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

		for (position, cache_key) in home_positions.into_iter().zip(home_keys) {
			let key = cache_key.page_key();
			let file = self.transactional_file(key)?;
			let guard = self.pool.get_or_load(cache_key, |output| {
				self.page_io.read_page(file.file()?, key.page_no(), output)
			})?;
			output[position] = Some(PageRead::Cached(guard));
		}
		collect_page_reads(output)
	}

	/// Visits one contiguous physical range without retaining cold scan pages.
	///
	/// Staged and exact WAL versions keep precedence. Exact home versions are
	/// probed in R8 first; only contiguous cold runs use the bypass transport.
	/// Callback page borrows cannot escape the bounded cursor window.
	///
	/// # Errors
	///
	/// Returns transaction, range, WAL, resident, Direct-I/O, scatter,
	/// validation, or callback errors.
	pub(in crate::storage) fn visit_contiguous_pages(
		&self,
		tx: &TxHandle,
		first_key: PageKey,
		page_count: usize,
		intent: ReadIntent,
		mut visit: impl FnMut(PageKey, &AlignedPage) -> FerrosGrynnResult<ControlFlow<()>>,
	) -> FerrosGrynnResult<()> {
		self.wal.validate_tx(tx)?;
		if page_count == 0 {
			return Ok(());
		}
		let file = self.transactional_file(first_key)?;
		let home_version = self.home_cache_version();
		let home_source_key = PageCacheKey::new(
			PageKey::new(first_key.graph_id(), first_key.file_id(), PageNo::new(0)),
			home_version,
		);
		let source_pages = usize::try_from(file.file()?.page_count()).map_err(|source| {
			FerrosGrynnError::invalid_storage_file(format!(
				"home source page count exceeds usize: {source}"
			))
		})?;
		let source_pages = NonZeroUsize::new(source_pages).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("home source contains no complete pages")
		})?;
		let home_locator = self.pool.home_locator(home_source_key, source_pages)?;
		self.pool.retire_home_versions(home_source_key)?;
		let mut consumed = 0;
		while consumed < page_count {
			let window_len = (page_count - consumed).min(MAX_READ_SCOPE_OPERATIONS);
			let window_first = contiguous_key(first_key, consumed)?;
			let keys = (0..window_len)
				.map(|offset| contiguous_key(window_first, offset))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let visible = self.wal.visible_frames(&keys, tx.snapshot());
			let home_keys =
				keys.iter().map(|key| PageCacheKey::new(*key, home_version)).collect::<Vec<_>>();
			let mut scope = self.pool.read_scope()?;
			let resolved_home = scope.resolve_home_window(&home_locator, &home_keys)?;
			let mut position = 0;
			while position < keys.len() {
				let key = keys[position];
				if let Some(page) = tx.staged_page(key) {
					if visit(key, page)?.is_break() {
						return Ok(());
					}
					position += 1;
					continue;
				}
				if let Some(frame) = visible.frames()[position] {
					let cache_key = PageCacheKey::new(
						key,
						PageCacheVersion::Wal(PageLsn::new(frame.frame_lsn().get())),
					);
					let page = self
						.pool
						.get_or_load(cache_key, |output| visible.read_frame(frame, output))?;
					if visit(key, page.as_page())?.is_break() {
						return Ok(());
					}
					position += 1;
					continue;
				}

				let resident_start = position;
				let mut resident_handles = Vec::new();
				while position < keys.len() {
					let candidate = keys[position];
					if tx.staged_page(candidate).is_some() || visible.frames()[position].is_some() {
						break;
					}
					let Some(handle) = resolved_home[position] else {
						break;
					};
					resident_handles.push(handle);
					position += 1;
				}
				if !resident_handles.is_empty() {
					let mut table = scope.prepare_resolved_table(&resident_handles)?;
					if let Some(window) =
						scope.get_resolved_window(&mut table, resident_handles.len())?
					{
						for offset in 0..window.len() {
							let page = window.page(offset).ok_or_else(|| {
								FerrosGrynnError::storage_io_failure(
									"resolved resident window lost its prepared frame",
								)
							})?;
							if visit(keys[resident_start + offset], page)?.is_break() {
								return Ok(());
							}
						}
						continue;
					}
				}
				position = resident_start;

				let run_start = position;
				position += 1;
				while position < keys.len() {
					let candidate = keys[position];
					if tx.staged_page(candidate).is_some()
						|| visible.frames()[position].is_some()
						|| resolved_home[position].is_some()
					{
						break;
					}
					position += 1;
				}
				if self
					.visit_cold_home_run(
						&file,
						keys[run_start],
						position - run_start,
						intent,
						&mut visit,
					)?
					.is_break()
				{
					return Ok(());
				}
			}
			consumed += window_len;
		}
		Ok(())
	}

	/// Reads one pre-resolved cold home run without admitting its pages.
	fn visit_cold_home_run(
		&self,
		file: &Arc<RegisteredDataFile>,
		first_key: PageKey,
		page_count: usize,
		intent: ReadIntent,
		visit: &mut impl FnMut(PageKey, &AlignedPage) -> FerrosGrynnResult<ControlFlow<()>>,
	) -> FerrosGrynnResult<ControlFlow<()>> {
		let route = self.pool.route(
			false,
			intent,
			ReadWindow::Contiguous {
				pages: page_count,
			},
			self.page_io.mode(),
			TransportCapabilities::current(),
		);
		#[cfg(windows)]
		if let ReadRoute::ScatterBypass {
			..
		} = route
		{
			let mut cursor = self.pool.scatter_cursor(
				Arc::clone(file.file()?),
				first_key.page_no(),
				page_count,
			)?;
			while let Some(extent) = cursor.next_extent() {
				let extent = extent?;
				for (offset, page) in extent.pages().iter().enumerate() {
					let key = PageKey::new(
						first_key.graph_id(),
						first_key.file_id(),
						checked_page_add(extent.first_page(), offset)?,
					);
					if visit(key, page)?.is_break() {
						drop(cursor);
						return Ok(ControlFlow::Break(()));
					}
				}
			}
			cursor.finish()?;
			return Ok(ControlFlow::Continue(()));
		}

		let mut completed = 0;
		let mut scratch = self.read_scratch.lock();
		while completed < page_count {
			let extent_len = (page_count - completed).min(scratch.len());
			let first_page = checked_page_add(first_key.page_no(), completed)?;
			let pages = &mut scratch[..extent_len];
			file.file()?.read_pages_raw(first_page, pages)?;
			for (offset, page) in pages.iter().enumerate() {
				page.validate(checked_page_add(first_page, offset)?)?;
			}
			for (offset, page) in pages.iter().enumerate() {
				let key = PageKey::new(
					first_key.graph_id(),
					first_key.file_id(),
					checked_page_add(first_page, offset)?,
				);
				if visit(key, page)?.is_break() {
					return Ok(ControlFlow::Break(()));
				}
			}
			completed += extent_len;
		}
		Ok(ControlFlow::Continue(()))
	}

	/// Stages one complete page image in a transaction-private write set.
	///
	/// # Errors
	///
	/// Returns an error for a foreign transaction, graph-superblock key,
	/// unknown file, invalid page, or write-set limit exhaustion.
	pub(in crate::storage) fn stage_frame(
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
	pub(in crate::storage) fn commit_tx(&self, tx: TxHandle) -> FerrosGrynnResult<CommitOutcome> {
		self.wal.commit_tx(tx)
	}

	/// Discards an in-memory transaction write set.
	#[expect(
		clippy::unused_self,
		reason = "transaction lifecycle operations intentionally stay on the manager"
	)]
	pub(in crate::storage) fn abort_tx(&self, tx: TxHandle) {
		FrameWal::abort_tx(tx);
	}

	/// Opens newly created trusted files so the same transaction may address
	/// them before their catalog head becomes visible.
	pub(in crate::storage) fn register_files(
		&self,
		registrations: Vec<DataFileRegistration>,
	) -> FerrosGrynnResult<()> {
		let database_directory = self.database_path.parent().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("database superblock has no parent directory")
		})?;
		validate_registration_paths(database_directory, &registrations)?;
		self.registry.write().insert(registrations, self.page_io.file_open_mode())
	}

	/// Drops newly opened files when their catalog-root transaction did not
	/// commit.
	pub(in crate::storage) fn unregister_files(
		&self,
		keys: &[(GraphId, crate::storage::types::DataFileId)],
	) {
		self.registry.write().remove(keys);
	}

	/// Writes fresh immutable catalog tails, synchronizes them, and stages only
	/// the fixed root-page switch in the caller's WAL transaction.
	pub(in crate::storage) fn stage_catalog_update(
		&self,
		tx: &mut TxHandle,
		update: CatalogUpdatePages,
	) -> FerrosGrynnResult<()> {
		let file = self.transactional_file(PageKey::new(
			GraphId::SYSTEM,
			crate::storage::catalog::SYSTEM_CATALOG_FILE_ID,
			PageNo::new(0),
		))?;
		let file = file.file()?;
		if update.required_page_count > file.page_count() {
			file.resize_pages(update.required_page_count)?;
		}
		for (page_no, page) in
			update.database_pages.iter().skip(1).chain(update.file_pages.iter().skip(1))
		{
			file.write_page(*page_no, page)?;
		}
		if update.database_pages.len() > 1 || update.file_pages.len() > 1 {
			file.sync_data()?;
		}
		for pages in [&update.database_pages, &update.file_pages] {
			let (page_no, page) = pages.first().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("catalog update has no root page")
			})?;
			self.stage_frame(
				tx,
				PageKey::new(
					GraphId::SYSTEM,
					crate::storage::catalog::SYSTEM_CATALOG_FILE_ID,
					*page_no,
				),
				page,
			)?;
		}
		Ok(())
	}

	/// Returns stable catalog roots and the next append-only tail page.
	pub(in crate::storage) fn catalog_allocation(
		&self,
	) -> FerrosGrynnResult<(PageNo, PageNo, PageNo)> {
		let authority = self.authority.read();
		let file = self.transactional_file(PageKey::new(
			GraphId::SYSTEM,
			crate::storage::catalog::SYSTEM_CATALOG_FILE_ID,
			PageNo::new(0),
		))?;
		Ok((
			authority.database.graph_catalog_root_page,
			authority.database.file_catalog_root_page,
			PageNo::new(file.file()?.page_count()),
		))
	}

	/// Returns the physical page count of one registered WAL-backfilled file.
	pub(in crate::storage) fn file_page_count(
		&self,
		graph_id: GraphId,
		file_id: crate::storage::types::DataFileId,
	) -> FerrosGrynnResult<u64> {
		Ok(self
			.transactional_file(PageKey::new(graph_id, file_id, PageNo::new(0)))?
			.file()?
			.page_count())
	}

	/// Returns the physical or snapshot-visible exclusive page high-water.
	pub(in crate::storage) fn visible_file_page_count(
		&self,
		tx: &TxHandle,
		graph_id: GraphId,
		file_id: crate::storage::types::DataFileId,
	) -> FerrosGrynnResult<u64> {
		self.wal.validate_tx(tx)?;
		let physical = self.file_page_count(graph_id, file_id)?;
		Ok(physical
			.max(self.wal.visible_file_page_count(tx.snapshot(), graph_id, file_id).unwrap_or(0))
			.max(tx.staged_file_page_count(graph_id, file_id).unwrap_or(0)))
	}

	/// Extends one registered WAL-backfilled file before frames may address its
	/// new tail. Existing bytes are never truncated.
	pub(in crate::storage) fn ensure_file_pages(
		&self,
		graph_id: GraphId,
		file_id: crate::storage::types::DataFileId,
		required: u64,
	) -> FerrosGrynnResult<()> {
		let registration =
			self.transactional_file(PageKey::new(graph_id, file_id, PageNo::new(0)))?;
		let file = registration.file()?;
		if file.page_count() < required {
			file.resize_pages(required)?;
		}
		Ok(())
	}

	/// Publishes checkpoint-owned metadata only after the matching catalog and
	/// graph pages are durable in the frame WAL.
	pub(in crate::storage) fn publish_graph_authority(
		&self,
		graphs: impl IntoIterator<Item = GraphSuperblock>,
		active_graphs: impl IntoIterator<Item = GraphId>,
		graph_count: u32,
		next_file_instance_id: u64,
	) {
		let mut authority = self.authority.write();
		for graph in graphs {
			authority.graphs.insert(graph.graph_id, graph);
		}
		let active = active_graphs.into_iter().collect::<RapidHashSet<_>>();
		authority.graphs.retain(|graph_id, _graph| active.contains(graph_id));
		authority.database.graph_count = graph_count;
		authority.database.next_file_instance_id =
			authority.database.next_file_instance_id.max(next_file_instance_id);
	}

	/// Returns the current WAL-authoritative metadata for one graph.
	pub(in crate::storage) fn graph_superblock(
		&self,
		graph_id: GraphId,
	) -> FerrosGrynnResult<GraphSuperblock> {
		self.authority.read().graphs.get(&graph_id).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph catalog references missing authoritative metadata",
			)
		})
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
	pub(in crate::storage) fn checkpoint(&self) -> FerrosGrynnResult<bool> {
		self.checkpoint_inner(false)
	}

	/// Cancels existing storage snapshots and checkpoints the complete durable
	/// tail captured before new readers may register.
	pub(in crate::storage) fn restart_checkpoint(&self) -> FerrosGrynnResult<bool> {
		self.checkpoint_inner(true)
	}

	fn checkpoint_inner(&self, restart: bool) -> FerrosGrynnResult<bool> {
		let _checkpoint = self.checkpoint_mutex.lock();
		let target = if restart {
			self.wal.capture_restart_checkpoint_target()
		} else {
			self.wal.capture_checkpoint_target()
		};
		let Some(target) = target else {
			return Ok(false);
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
		next_database.next_file_instance_id =
			next_database.next_file_instance_id.max(clocks.next_file_instance_id);
		next_database.system_backfilled_lsn = checkpoint.target();
		next_database.encode_into(superblock_page)?;
		write_superblock_atomic(&self.database_path, superblock_page)?;
		drop(superblock_scratch);

		{
			let mut authority = self.authority.write();
			authority.database = next_database;
			authority.graphs = next_graphs;
		}
		self.wal.publish_checkpoint(&checkpoint)?;
		Ok(true)
	}

	/// Returns the number of unique durable pages still represented only in
	/// WAL.
	pub(in crate::storage) fn unbackfilled_page_count(&self) -> usize {
		self.wal.unbackfilled_page_count()
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
	fn transactional_file(
		&self,
		key: PageKey,
	) -> FerrosGrynnResult<std::sync::Arc<RegisteredDataFile>> {
		let file = self.registry.read().resolve(key)?;
		if file.role() != DataFileRole::WalBackfilled {
			return Err(FerrosGrynnError::invalid_storage_file(
				"transactional access targeted checkpoint-owned metadata",
			));
		}
		Ok(file)
	}

	/// Resolves the sole checkpoint-owned metadata file for one graph.
	fn graph_superblock_file(
		&self,
		graph_id: GraphId,
	) -> FerrosGrynnResult<std::sync::Arc<RegisteredDataFile>> {
		self.registry
			.read()
			.snapshot()
			.into_iter()
			.filter(|((registered_graph, _file_id), file)| {
				*registered_graph == graph_id && file.role() == DataFileRole::GraphSuperblock
			})
			.max_by_key(|((_, file_id), _file)| file_id.instance_id())
			.map(|(_key, file)| file)
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"graph checkpoint authority has no registered GraphSuperblock",
				)
			})
	}

	/// Copies checkpoint-selected frames into contiguous home extents and syncs
	/// each touched physical file exactly once.
	pub(super) fn backfill_frames(&self, frames: &[ResolvedFrame]) -> FerrosGrynnResult<()> {
		let mut touched = RapidHashSet::default();
		let mut scratch = self.checkpoint_scratch.lock();
		let mut start = 0;
		while start < frames.len() {
			let mut buffer_pages = 0;
			let mut io_extents = Vec::new();
			while start < frames.len() && buffer_pages < scratch.len() {
				let first = frames[start];
				let remaining_buffer = scratch.len() - buffer_pages;
				let mut end = start + 1;
				while end < frames.len() && end - start < remaining_buffer {
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
				let buffer_end = buffer_pages + extent.len();
				for (frame, page) in
					extent.iter().copied().zip(&mut scratch[buffer_pages..buffer_end])
				{
					self.wal.read_frame(frame, page)?;
				}
				let file = self.transactional_file(first.key())?;
				io_extents.push(FilePageExtent::new(
					Arc::clone(file.file()?),
					first.key().page_no(),
					buffer_pages,
					extent.len(),
				));
				touched.insert((first.key().graph_id(), first.key().file_id()));
				buffer_pages = buffer_end;
				start = end;
			}
			self.page_io.write_extents(&io_extents, &scratch[..buffer_pages])?;
		}
		for (graph_id, file_id) in touched {
			self.registry.read().resolve_file(graph_id, file_id)?.file()?.sync_data()?;
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
			let file = self.transactional_file(key)?;
			self.page_io.write_page(file.file()?, key.page_no(), page)?;
			touched.insert((key.graph_id(), key.file_id()));
		}
		for (graph_id, file_id) in touched {
			self.registry.read().resolve_file(graph_id, file_id)?.file()?.sync_data()?;
		}
		Ok(())
	}

	/// Validates every initialized transactional home page after WAL replay.
	fn validate_home_files(&self) -> FerrosGrynnResult<()> {
		let mut scratch = self.checkpoint_scratch.lock();
		let registrations = self.registry.read().snapshot();
		for ((_graph_id, file_id), registration) in registrations {
			if registration.role() != DataFileRole::WalBackfilled {
				continue;
			}
			let file = registration.file()?;
			// Statistics distributions are intentionally lazy. Startup proves
			// the graph root only; ordinary page reads validate auxiliary
			// checksums.
			let validation_pages = match file_id.kind() {
				Some(DataFileKind::Statistics) => file.page_count().min(1),
				Some(DataFileKind::LabelStatistics | DataFileKind::PropertyStatistics) => 0,
				_ => file.page_count(),
			};
			let mut first = 0_u64;
			while first < validation_pages {
				let remaining = validation_pages - first;
				let count =
					usize::try_from(remaining.min(scratch.len() as u64)).map_err(|source| {
						FerrosGrynnError::storage_io_failure(format!(
							"home validation extent does not fit usize: {source}"
						))
					})?;
				let pages = &mut scratch[..count];
				let extent = FilePageExtent::new(Arc::clone(file), PageNo::new(first), 0, count);
				self.page_io.read_extents(std::slice::from_ref(&extent), pages)?;
				for (offset, page) in pages.iter_mut().enumerate() {
					let offset = u64::try_from(offset).map_err(|source| {
						FerrosGrynnError::storage_io_failure(format!(
							"home validation offset does not fit u64: {source}"
						))
					})?;
					page.validate(PageNo::new(first + offset))?;
				}
				first += count as u64;
			}
		}
		Ok(())
	}
}
