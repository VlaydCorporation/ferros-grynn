//! Correctness checks for the stage 3.2 production candidate.

use std::{
	error::Error,
	mem::size_of,
	num::NonZeroUsize,
	sync::{
		Arc,
		atomic::{AtomicUsize, Ordering},
		mpsc,
	},
	thread,
	time::{Duration, Instant},
};
#[cfg(windows)]
use std::{
	io::{Read, Seek, SeekFrom, Write},
	path::Path,
};

#[cfg(windows)]
use ferros_grynn::{
	datastore::{
		CheckpointMode,
		Datastore,
		GraphWriteView,
		LocalOptions,
		TableField,
		TableKind,
		TableSchema,
	},
	graph::{
		AtomKind,
		AtomRef,
		EdgeEndpoint,
		EdgeTopology,
		Graph,
		GraphRead,
		GraphStorageMode,
		Mutable,
		NodeRead,
	},
	storage_engine_prototype_api::direct::{DirectFile, DirectFileOpenMode},
	values::{Object, PropertyMap, Text, Type, Value},
};
use ferros_grynn::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage_engine_prototype_api::{
		buffer_pool::{PageCacheKey, PageCacheVersion},
		hybrid::{
			HybridReadEngine,
			MAX_READ_SCOPE_OPERATIONS,
			ReadIntent,
			ReadRoute,
			ReadWindow,
			TransportCapabilities,
			transient_pages,
		},
		page::{AlignedPage, PAGE_HEADER_SIZE, PageFlags, PageHeader, PageLsn, PageNo, PageType},
		page_io::PageIoMode,
		types::{CheckpointGeneration, DataFileId, DataFileKind, GraphId, Lsn, PageKey},
	},
};

/// Unique local-database path removed after the end-to-end test.
#[cfg(windows)]
struct TestDirectory(std::path::PathBuf);

#[cfg(windows)]
impl TestDirectory {
	/// Reserves a path without creating it before `Datastore::create_local`.
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgh-{:016x}", uuid::Uuid::new_v4().as_u128() as u64)),
		)
	}
}

#[cfg(windows)]
impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(std::fs::remove_dir_all(&self.0));
	}
}

/// Unique Direct-I/O file removed after a transport test.
#[cfg(windows)]
struct TestFile(std::path::PathBuf);

#[cfg(windows)]
impl TestFile {
	/// Reserves a file path without opening it.
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fghs-{:016x}.fgd", uuid::Uuid::new_v4().as_u128() as u64)),
		)
	}
}

#[cfg(windows)]
impl Drop for TestFile {
	fn drop(&mut self) {
		drop(std::fs::remove_file(&self.0));
	}
}

/// Writes sealed pages whose first payload word equals their page number.
#[cfg(windows)]
fn write_numbered_pages(path: &Path, page_count: usize) -> Result<(), Box<dyn Error>> {
	let writer = DirectFile::create_new(path)?;
	writer.resize_pages(page_count as u64)?;
	let mut pages = Vec::with_capacity(page_count);
	for page_no in 0..page_count {
		let mut page = AlignedPage::new(
			PageType::NodeHot,
			PageFlags::empty(),
			PageLsn::new(1),
			PageNo::new(page_no as u64),
		);
		page.as_bytes_mut()[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + size_of::<u64>()]
			.copy_from_slice(&(page_no as u64).to_le_bytes());
		page.seal()?;
		pages.push(page);
	}
	writer.write_pages(PageNo::new(0), &pages)?;
	writer.sync_data()?;
	Ok(())
}

/// Creates one exact home-page key.
fn home_key(file_instance: u64, page_no: u64) -> FerrosGrynnResult<PageCacheKey> {
	home_key_at(file_instance, page_no, 11)
}

/// Creates one exact home-page key at an explicit checkpoint generation.
fn home_key_at(
	file_instance: u64,
	page_no: u64,
	generation: u64,
) -> FerrosGrynnResult<PageCacheKey> {
	let file_id = DataFileId::new(file_instance, DataFileKind::NodesHot).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("test data-file identity is out of range")
	})?;
	Ok(PageCacheKey::new(
		PageKey::new(GraphId::new(7), file_id, PageNo::new(page_no)),
		PageCacheVersion::Home {
			generation: CheckpointGeneration::new(generation),
			ceiling: Lsn::new(100),
		},
	))
}

/// Creates one exact WAL-page key.
fn wal_key(file_instance: u64, page_no: u64, page_lsn: u64) -> FerrosGrynnResult<PageCacheKey> {
	let file_id = DataFileId::new(file_instance, DataFileKind::NodesHot).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("test data-file identity is out of range")
	})?;
	Ok(PageCacheKey::new(
		PageKey::new(GraphId::new(7), file_id, PageNo::new(page_no)),
		PageCacheVersion::Wal(PageLsn::new(page_lsn)),
	))
}

/// Fills one frame with a valid page matching its exact key.
fn load_valid(key: PageCacheKey, target: &mut AlignedPage) -> FerrosGrynnResult<PageHeader> {
	let page_lsn = match key.version() {
		PageCacheVersion::Wal(page_lsn) => page_lsn,
		PageCacheVersion::Home {
			ceiling,
			..
		} => PageLsn::new(ceiling.get()),
	};
	let mut page = AlignedPage::new(PageType::NodeHot, PageFlags::empty(), page_lsn, key.page_no());
	page.as_bytes_mut()[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + size_of::<u64>()]
		.copy_from_slice(&key.page_key().file_id().get().to_le_bytes());
	page.seal()?;
	let header = page.validate(key.page_no())?;
	*target = page;
	Ok(header)
}

#[test]
fn route_table_uses_only_pre_submission_fallbacks() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(4_096).ok_or("zero capacity")?);
	let all = TransportCapabilities {
		scatter: true,
		io_ring: true,
	};

	assert_eq!(
		engine.route(true, ReadIntent::Point, ReadWindow::Single, PageIoMode::Sync, all),
		ReadRoute::AtomicArena
	);
	assert_eq!(
		engine.route(false, ReadIntent::Point, ReadWindow::Single, PageIoMode::Sync, all),
		ReadRoute::SyncDirect {
			admit: true,
		}
	);
	assert_eq!(
		engine.route(
			false,
			ReadIntent::SequentialScan,
			ReadWindow::Contiguous {
				pages: 400,
			},
			PageIoMode::Sync,
			all,
		),
		ReadRoute::ScatterBypass {
			extent_pages: 128,
			prefetch_depth: 1,
		}
	);
	assert_eq!(
		engine.route(
			false,
			ReadIntent::IndexTraversal,
			ReadWindow::Sparse {
				operations: 64,
				cold_pages: 40,
			},
			PageIoMode::NativeAsync,
			all,
		),
		ReadRoute::IoRingSparse {
			queue_depth: 32,
		}
	);
	assert_eq!(
		engine.route(
			false,
			ReadIntent::IndexTraversal,
			ReadWindow::Sparse {
				operations: MAX_READ_SCOPE_OPERATIONS + 1,
				cold_pages: 32,
			},
			PageIoMode::NativeAsync,
			all,
		),
		ReadRoute::SyncDirect {
			admit: false,
		}
	);
	Ok(())
}

#[test]
fn transient_budget_is_bounded_by_ten_percent_and_288_pages() -> Result<(), Box<dyn Error>> {
	assert_eq!(transient_pages(9), 0);
	let engine = HybridReadEngine::new(NonZeroUsize::new(4_096).ok_or("zero capacity")?);
	assert_eq!(engine.transient_capacity(), 288);
	assert_eq!(engine.scatter_extent_capacity(), 128);
	assert_eq!(engine.sparse_window_capacity(), 32);
	assert!(engine.scatter_extent_capacity() * 2 + engine.sparse_window_capacity() <= 288);
	assert_eq!(transient_pages(65_536), 288);
	Ok(())
}

#[test]
fn exact_versions_and_files_share_one_resident_budget() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(3).ok_or("zero capacity")?);
	let keys = [home_key(1, 3)?, home_key(2, 3)?, wal_key(1, 3, 73)?];
	for key in keys {
		let guard = engine.get_or_load(key, |page| load_valid(key, page))?;
		assert_eq!(guard.as_page().validate(key.page_no())?.page_no(), key.page_no());
	}
	assert_eq!(engine.resident_count(), 3);
	assert_eq!(engine.resident_capacity(), 3);
	Ok(())
}

#[test]
fn production_r8_coalesces_one_miss() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(2).ok_or("zero capacity")?);
	let key = home_key(1, 17)?;
	let loaders = AtomicUsize::new(0);
	let (started_tx, started_rx) = mpsc::channel();
	let (release_tx, release_rx) = mpsc::channel();
	let (done_tx, done_rx) = mpsc::channel();

	thread::scope(|scope| {
		let done = done_tx.clone();
		let engine_ref = &engine;
		let loaders_ref = &loaders;
		scope.spawn(move || {
			let result = engine_ref
				.get_or_load(key, |page| {
					loaders_ref.fetch_add(1, Ordering::Relaxed);
					started_tx
						.send(())
						.map_err(|error| FerrosGrynnError::storage_io_failure(error.to_string()))?;
					release_rx
						.recv()
						.map_err(|error| FerrosGrynnError::storage_io_failure(error.to_string()))?;
					load_valid(key, page)
				})
				.map(drop);
			let _ = done.send(result);
		});
		assert!(started_rx.recv_timeout(Duration::from_secs(1)).is_ok());

		let engine_ref = &engine;
		let loaders_ref = &loaders;
		scope.spawn(move || {
			let result = engine_ref
				.get_or_load(key, |page| {
					loaders_ref.fetch_add(1, Ordering::Relaxed);
					load_valid(key, page)
				})
				.map(drop);
			let _ = done_tx.send(result);
		});
		let deadline = Instant::now() + Duration::from_secs(1);
		while engine.slow_path_counts().1 == 0 && Instant::now() < deadline {
			thread::yield_now();
		}
		assert_eq!(engine.slow_path_counts().1, 1);
		assert!(release_tx.send(()).is_ok());
	});
	done_rx.recv_timeout(Duration::from_secs(1))??;
	done_rx.recv_timeout(Duration::from_secs(1))??;
	assert_eq!(loaders.load(Ordering::Relaxed), 1);
	Ok(())
}

#[test]
fn production_r8_waits_for_the_last_pin() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::MIN);
	let first = home_key(1, 1)?;
	let second = home_key(1, 2)?;
	let pinned = engine.get_or_load(first, |page| load_valid(first, page))?;
	let (entered_tx, entered_rx) = mpsc::channel();
	let (loader_tx, loader_rx) = mpsc::channel();
	let (done_tx, done_rx) = mpsc::channel();

	thread::scope(|scope| {
		scope.spawn(|| {
			let _ = entered_tx.send(());
			let result = engine
				.get_or_load(second, |page| {
					let _ = loader_tx.send(());
					load_valid(second, page)
				})
				.map(drop);
			let _ = done_tx.send(result);
		});
		assert!(entered_rx.recv_timeout(Duration::from_secs(1)).is_ok());
		assert!(matches!(
			loader_rx.recv_timeout(Duration::from_millis(30)),
			Err(mpsc::RecvTimeoutError::Timeout)
		));
		drop(pinned);
		assert!(loader_rx.recv_timeout(Duration::from_secs(1)).is_ok());
	});
	done_rx.recv_timeout(Duration::from_secs(1))??;
	assert!(engine.slow_path_counts().3 >= 1);
	Ok(())
}

#[test]
fn production_r8_rolls_back_a_panicking_loader() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::MIN);
	let key = home_key(1, 10)?;
	let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		let _ = engine.get_or_load(key, |_| -> FerrosGrynnResult<PageHeader> {
			panic!("intentional production-candidate loader panic");
		});
	}));
	assert!(panicked.is_err());
	assert!(engine.resolve(key).is_none());
	drop(engine.get_or_load(key, |page| load_valid(key, page))?);
	Ok(())
}

#[test]
fn corrupted_load_is_never_published() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(1).ok_or("zero capacity")?);
	let key = home_key(1, 0)?;
	let result = engine.get_or_load(key, |target| {
		let header = load_valid(key, target)?;
		target.as_bytes_mut()[PAGE_HEADER_SIZE] ^= 0xff;
		Ok(header)
	});
	assert!(result.is_err());
	assert_eq!(engine.resident_count(), 0);
	assert!(engine.get_if_resident(key)?.is_none());
	Ok(())
}

#[test]
fn stale_handle_fails_after_frame_reuse() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(1).ok_or("zero capacity")?);
	let old_key = home_key(1, 0)?;
	let new_key = home_key(1, 1)?;
	let old_guard = engine.get_or_load(old_key, |page| load_valid(old_key, page))?;
	drop(old_guard);
	let handle = engine.resolve(old_key).ok_or("resident key was not resolved")?;
	let new_guard = engine.get_or_load(new_key, |page| load_valid(new_key, page))?;
	drop(new_guard);
	assert!(engine.get_if_resident(old_key)?.is_none());
	let mut scope = engine.read_scope()?;
	assert!(scope.get_resolved(&handle)?.is_none());
	Ok(())
}

#[test]
fn read_scope_rotates_at_operation_and_source_boundaries() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(2).ok_or("zero capacity")?);
	let first = home_key(1, 0)?;
	let second = home_key(2, 0)?;
	for key in [first, second] {
		drop(engine.get_or_load(key, |page| load_valid(key, page))?);
	}
	let first_handle = engine.resolve(first).ok_or("first key was not resolved")?;
	let second_handle = engine.resolve(second).ok_or("second key was not resolved")?;
	let mut scope = engine.read_scope()?;
	for _ in 0..MAX_READ_SCOPE_OPERATIONS {
		let page = scope.get_resolved(&first_handle)?.ok_or("resolved page became stale")?;
		assert_eq!(page.as_page().validate(first.page_no())?.page_no(), first.page_no());
	}
	assert_eq!(scope.operations(), MAX_READ_SCOPE_OPERATIONS);
	drop(scope.get_resolved(&first_handle)?.ok_or("rotated page became stale")?);
	assert_eq!(scope.operations(), 1);
	drop(scope.get_resolved(&second_handle)?.ok_or("second source became stale")?);
	assert_eq!(scope.operations(), 1);
	Ok(())
}

#[test]
fn resolved_page_window_is_ordered_and_source_bounded() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(3).ok_or("zero capacity")?);
	let first = home_key(1, 0)?;
	let second = home_key(1, 1)?;
	let foreign = home_key(2, 0)?;
	for key in [first, second, foreign] {
		drop(engine.get_or_load(key, |page| load_valid(key, page))?);
	}
	let first_handle = engine.resolve(first).ok_or("first key was not resolved")?;
	let second_handle = engine.resolve(second).ok_or("second key was not resolved")?;
	let foreign_handle = engine.resolve(foreign).ok_or("foreign key was not resolved")?;
	let mut scope = engine.read_scope()?;
	let mut table = scope.prepare_resolved_table(&[second_handle, first_handle])?;
	let window =
		scope.get_resolved_window(&mut table, 200)?.ok_or("resident window became stale")?;
	assert_eq!(window.len(), 2);
	assert_eq!(
		window.page(0).ok_or("missing second page")?.validate(second.page_no())?.page_no(),
		second.page_no()
	);
	assert_eq!(
		window.page(1).ok_or("missing first page")?.validate(first.page_no())?.page_no(),
		first.page_no()
	);
	drop(window);
	assert_eq!(scope.operations(), 200);
	let mut replacement = vec![first_handle];
	scope.update_resolved_table_owned(&mut table, &mut replacement)?;
	assert!(replacement.is_empty());
	drop(scope.get_resolved_window(&mut table, 100)?.ok_or("repeated resident page became stale")?);
	assert_eq!(scope.operations(), 100);
	assert!(scope.prepare_resolved_table(&[first_handle, foreign_handle]).is_err());
	let mut empty = scope.prepare_resolved_table(&[])?;
	assert!(scope.get_resolved_window(&mut empty, 1).is_err());
	Ok(())
}

#[test]
fn reusable_page_table_revalidates_after_eviction() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(1).ok_or("zero capacity")?);
	let first = home_key(1, 0)?;
	let second = home_key(1, 1)?;
	drop(engine.get_or_load(first, |page| load_valid(first, page))?);
	let handle = engine.resolve(first).ok_or("first key was not resolved")?;
	let mut read_scope = engine.read_scope()?;
	let mut table = read_scope.prepare_resolved_table(&[handle])?;
	drop(read_scope.get_resolved_window(&mut table, 1)?.ok_or("initial page table was stale")?);

	thread::scope(|threads| -> Result<(), Box<dyn Error>> {
		let (done_tx, done_rx) = mpsc::channel();
		let engine = &engine;
		threads.spawn(move || {
			let outcome = engine.get_or_load(second, |page| load_valid(second, page)).map(drop);
			let _ = done_tx.send(outcome);
		});
		let deadline = Instant::now() + Duration::from_secs(2);
		while engine.slow_path_counts().3 == 0 {
			if Instant::now() >= deadline {
				return Err("evicting loader did not reach bounded epoch backpressure".into());
			}
			thread::yield_now();
		}
		assert!(read_scope.get_resolved_window(&mut table, 1)?.is_none());
		drop(read_scope);
		done_rx.recv_timeout(Duration::from_secs(2))??;
		Ok(())
	})?;
	Ok(())
}

#[test]
fn home_locator_prunes_stale_slots_and_empty_chunks() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(1).ok_or("zero capacity")?);
	let first = home_key(1, 0)?;
	let distant = home_key(1, 2_048)?;
	let locator = engine.home_locator(first, NonZeroUsize::new(4_096).ok_or("zero pages")?)?;

	drop(engine.get_or_load(first, |page| load_valid(first, page))?);
	// Populate through the engine-owned arena via a bounded home window.
	let scope = engine.read_scope()?;
	let first_handles = scope.resolve_home_window(&locator, &[first])?;
	assert!(first_handles[0].is_some());
	assert_eq!(locator.entry_count(), 1);
	assert_eq!(locator.chunk_count(), 1);
	drop(scope);

	drop(engine.get_or_load(distant, |page| load_valid(distant, page))?);
	let scope = engine.read_scope()?;
	let distant_handles = scope.resolve_home_window(&locator, &[distant])?;
	assert!(distant_handles[0].is_some());
	assert_eq!(locator.entry_count(), 1);
	assert_eq!(locator.chunk_count(), 1);
	assert_eq!(engine.locator_entry_count(), 1);
	Ok(())
}

#[test]
fn source_registry_retires_old_home_epoch_but_not_active_scope() -> Result<(), Box<dyn Error>> {
	let engine = HybridReadEngine::new(NonZeroUsize::new(2).ok_or("zero capacity")?);
	let old_key = home_key_at(1, 0, 11)?;
	let current_key = home_key_at(1, 0, 12)?;
	let pages = NonZeroUsize::new(16).ok_or("zero pages")?;
	let old_locator = engine.home_locator(old_key, pages)?;
	let current_locator = engine.home_locator(current_key, pages)?;
	assert_eq!(engine.source_count(), 2);
	assert_eq!(engine.retire_home_versions(current_key)?, 1);
	assert_eq!(engine.source_count(), 1);
	assert_eq!(old_locator.page_count(), 16);
	assert_eq!(current_locator.page_count(), 16);
	assert!(engine.home_locator(wal_key(1, 0, 9)?, pages).is_err());
	Ok(())
}

#[cfg(windows)]
#[test]
fn selected_r8_path_survives_checkpoint_and_reopen() -> Result<(), Box<dyn Error>> {
	const NODE_COUNT: usize = 1_500;
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 320;
	let store = Datastore::create_local(&directory.0, options)?;
	let payload_key = store.intern_property("payload")?;
	let payload = Text::from("x".repeat(40_000));
	let mut graph = Graph::<Mutable>::new();
	let mut first_properties = PropertyMap::new();
	first_properties.insert(payload_key, payload.clone());
	let first = graph.add_node_with_id("hybrid:node-0".parse()?, None, first_properties)?;
	for index in 1..NODE_COUNT {
		graph.add_node_with_id(
			format!("hybrid:node-{index}").parse()?,
			None,
			PropertyMap::new(),
		)?;
	}
	let mut create = store.begin();
	let reference = create.create_graph("graph:hybrid-e2e".parse()?, graph.freeze().into())?;
	create.define_table(TableSchema::new("hybrid", reference.clone(), TableKind::Normal, [
		TableField::new("payload", Type::String),
	])?)?;
	create.commit()?;
	store.checkpoint(CheckpointMode::Restart)?;
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options)?;
	let read = reopened.begin();
	let view = reference.read(&read)?;
	assert_eq!(view.node_count()?, NODE_COUNT);
	assert_eq!(view.node_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?.len(), NODE_COUNT);
	let first_node = view.node(first)?.ok_or("first node disappeared after reopen")?;
	assert_eq!(first_node.properties()?.get(payload_key), Some(&Value::from(payload)));
	let result = reopened.execute("SELECT VALUE id FROM hybrid LIMIT 1", &Object::new())?;
	let statement = result
		.first()
		.ok_or_else(|| std::io::Error::other("SELECT returned no statement result"))?;
	let outcome =
		statement.result.as_ref().map_err(|error| std::io::Error::other(error.to_string()))?;
	let Value::Array(rows) = outcome.value() else {
		return Err("SELECT did not return an array".into());
	};
	assert_eq!(rows.len(), 1);
	Ok(())
}

#[cfg(windows)]
#[test]
fn streaming_limit_keeps_local_serializable_predicate() -> Result<(), Box<dyn Error>> {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default())?;
	let age = store.intern_property("age")?;
	let mut graph = Graph::<Mutable>::new();
	for index in 0..64 {
		let mut properties = PropertyMap::new();
		properties.insert(age, Value::from(18_i64 + index));
		graph.add_node_with_id(format!("person:initial-{index}").parse()?, None, properties)?;
	}
	let mut create = store.begin();
	let reference = create.create_graph("graph:streaming-ssi".parse()?, graph.freeze().into())?;
	create.define_table(TableSchema::new("person", reference.clone(), TableKind::Normal, [
		TableField::new("age", Type::Int),
	])?)?;
	create.commit()?;

	let reader = store.begin();
	let result =
		reader.execute("SELECT VALUE id FROM person WHERE age >= 18 LIMIT 1", &Object::new())?;
	let Value::Array(rows) = result[0]
		.result
		.as_ref()
		.map_err(|error| std::io::Error::other(error.to_string()))?
		.value()
	else {
		return Err("SELECT did not return an array".into());
	};
	assert_eq!(rows.len(), 1);

	let mut writer = store.begin();
	if let GraphWriteView::Graph(graph) = writer.edit_graph(&reference)? {
		let mut properties = PropertyMap::new();
		properties.insert(age, Value::from(21_i64));
		graph.add_node_with_id("person:phantom".parse()?, None, properties)?;
	}
	writer.commit()?;
	let error = match reader.commit() {
		Ok(_snapshot) => return Err("serializable reader accepted a matching phantom".into()),
		Err(error) => error,
	};
	assert_eq!(error.status().code().to_string(), "FG-40001");
	Ok(())
}

#[cfg(windows)]
#[test]
fn mode3_materialization_is_owned_two_pass_and_origin_aware() -> Result<(), Box<dyn Error>> {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default())?;
	let key = store.intern_property("value")?;
	let mut graph = Graph::<Mutable>::new();
	let mut first_properties = PropertyMap::new();
	first_properties.insert(key, Value::from("first"));
	let first = graph.add_node_with_id("item:first".parse()?, None, first_properties)?;
	let second = graph.add_node_with_id("item:second".parse()?, None, PropertyMap::new())?;
	let first_edge = graph.add_edge_with_id(
		"link:first".parse()?,
		EdgeTopology::directed([first], [second]),
		None,
		PropertyMap::new(),
	)?;
	graph.add_edge_with_id(
		"link:second".parse()?,
		EdgeTopology::directed([EdgeEndpoint::Local(first_edge)], [EdgeEndpoint::Local(second)]),
		None,
		PropertyMap::new(),
	)?;
	let mut create = store.begin();
	let reference = create.create_graph("graph:mode3".parse()?, graph.freeze().into())?;
	create.commit()?;
	drop(store);

	let store = Datastore::open_local(&directory.0, LocalOptions::default())?;
	let reader = store.begin();
	let hybrid = reader.materialize_query_graph(&reference)?;
	drop(reader);
	drop(store);

	assert_eq!(hybrid.storage_mode(), GraphStorageMode::Hybrid);
	assert_eq!(hybrid.node_count(), 2);
	assert_eq!(hybrid.edge_count(), 2);
	let origin = hybrid.materialization_origin().ok_or("Mode 3 result lost its origin")?;
	assert_eq!(origin.graph_ref(), Some(&reference));
	let first_local = hybrid
		.atom_id(&AtomRef::new(AtomKind::Node, "item:first".parse()?))?
		.ok_or("first node was not materialized")?;
	assert_eq!(
		hybrid.node(first_local).ok_or("first node is absent")?.properties().get(key),
		Some(&Value::from("first"))
	);
	let first_edge_local = hybrid
		.atom_id(&AtomRef::new(AtomKind::Edge, "link:first".parse()?))?
		.ok_or("first edge was not materialized")?;
	let second_edge_local = hybrid
		.atom_id(&AtomRef::new(AtomKind::Edge, "link:second".parse()?))?
		.ok_or("second edge was not materialized")?;
	let second_edge = hybrid.edge(second_edge_local).ok_or("second edge is absent")?;
	assert!(
		second_edge
			.topology()
			.endpoints()
			.any(|endpoint| { *endpoint == EdgeEndpoint::Local(first_edge_local) })
	);

	let mutable = hybrid.thaw();
	assert_eq!(mutable.storage_mode(), GraphStorageMode::Hybrid);
	assert!(mutable.materialization_origin().is_some());
	Ok(())
}

#[cfg(windows)]
#[test]
fn scatter_prefetch_is_ordered_bounded_and_bypasses_resident_frames() -> Result<(), Box<dyn Error>>
{
	const PAGE_COUNT: usize = 40;
	let path = TestFile::new();
	write_numbered_pages(&path.0, PAGE_COUNT)?;

	let file =
		Arc::new(DirectFile::open_read_write_with_mode(&path.0, DirectFileOpenMode::Overlapped)?);
	let engine = HybridReadEngine::new(NonZeroUsize::new(320).ok_or("zero capacity")?);
	let mut cursor = engine.scatter_cursor(Arc::clone(&file), PageNo::new(0), PAGE_COUNT)?;
	let mut expected = 0_u64;
	while let Some(extent) = cursor.next_extent() {
		let extent = extent?;
		assert_eq!(extent.first_page(), PageNo::new(expected));
		for page in extent.pages() {
			let value = u64::from_le_bytes(
				page.as_bytes()[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + size_of::<u64>()]
					.try_into()?,
			);
			assert_eq!(value, expected);
			expected += 1;
		}
	}
	cursor.finish()?;
	assert_eq!(expected, PAGE_COUNT as u64);
	assert_eq!(engine.resident_count(), 0);
	drop(file);
	Ok(())
}

#[cfg(windows)]
#[test]
fn scatter_bypass_preserves_an_existing_resident_hot_set() -> Result<(), Box<dyn Error>> {
	const HOT_PAGES: usize = 4;
	const PAGE_COUNT: usize = 12;
	let path = TestFile::new();
	write_numbered_pages(&path.0, PAGE_COUNT)?;

	let preload = DirectFile::open_read_only(&path.0)?;
	let scatter =
		Arc::new(DirectFile::open_read_write_with_mode(&path.0, DirectFileOpenMode::Overlapped)?);
	let engine = HybridReadEngine::new(NonZeroUsize::new(320).ok_or("zero capacity")?);
	for raw_page in 0..HOT_PAGES {
		let key = home_key(1, raw_page as u64)?;
		drop(engine.get_or_load(key, |page| {
			let page_no = PageNo::new(raw_page as u64);
			preload.read_page_raw(page_no, page)?;
			page.validate(page_no)
		})?);
	}

	let mut cursor = engine.scatter_cursor(
		Arc::clone(&scatter),
		PageNo::new(HOT_PAGES as u64),
		PAGE_COUNT - HOT_PAGES,
	)?;
	while let Some(extent) = cursor.next_extent() {
		for page in extent?.pages() {
			std::hint::black_box(page.as_bytes()[PAGE_HEADER_SIZE]);
		}
	}
	cursor.finish()?;

	assert_eq!(engine.resident_count(), HOT_PAGES);
	for raw_page in 0..HOT_PAGES {
		assert!(engine.get_if_resident(home_key(1, raw_page as u64)?)?.is_some());
	}
	for raw_page in HOT_PAGES..PAGE_COUNT {
		assert!(engine.get_if_resident(home_key(1, raw_page as u64)?)?.is_none());
	}
	Ok(())
}

#[cfg(windows)]
#[test]
fn scatter_corruption_is_terminal_before_extent_publication() -> Result<(), Box<dyn Error>> {
	let path = TestFile::new();
	write_numbered_pages(&path.0, 2)?;
	let mut ordinary = std::fs::OpenOptions::new().read(true).write(true).open(&path.0)?;
	ordinary.seek(SeekFrom::Start(PAGE_HEADER_SIZE as u64))?;
	let mut byte = [0_u8; 1];
	ordinary.read_exact(&mut byte)?;
	ordinary.seek(SeekFrom::Start(PAGE_HEADER_SIZE as u64))?;
	ordinary.write_all(&[byte[0] ^ 0xff])?;
	ordinary.sync_data()?;
	drop(ordinary);

	let file =
		Arc::new(DirectFile::open_read_write_with_mode(&path.0, DirectFileOpenMode::Overlapped)?);
	let engine = HybridReadEngine::new(NonZeroUsize::new(320).ok_or("zero capacity")?);
	let mut cursor = engine.scatter_cursor(Arc::clone(&file), PageNo::new(0), 2)?;
	assert!(cursor.next_extent().ok_or("scatter ended before reporting corruption")?.is_err());
	assert!(cursor.next_extent().is_none());
	drop(cursor);
	assert_eq!(engine.resident_count(), 0);
	drop(file);
	Ok(())
}

#[cfg(all(windows, feature = "native-async-io"))]
#[test]
fn sparse_ioring_preserves_order_duplicates_and_bypasses_resident_frames()
-> Result<(), Box<dyn Error>> {
	let path = TestFile::new();
	write_numbered_pages(&path.0, 8)?;
	let file =
		Arc::new(DirectFile::open_read_write_with_mode(&path.0, DirectFileOpenMode::Overlapped)?);
	let engine = HybridReadEngine::new(NonZeroUsize::new(320).ok_or("zero capacity")?);
	let requested = [
		(Arc::clone(&file), PageNo::new(7)),
		(Arc::clone(&file), PageNo::new(1)),
		(Arc::clone(&file), PageNo::new(7)),
	];
	let batch = engine.read_sparse_ioring(&requested)?;
	assert_eq!(batch.len(), requested.len());
	for (index, (page, expected)) in batch.pages().iter().zip([7_u64, 1, 7]).enumerate() {
		assert_eq!(batch.page_no(index), Some(PageNo::new(expected)));
		let value = u64::from_le_bytes(
			page.as_bytes()[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + size_of::<u64>()].try_into()?,
		);
		assert_eq!(value, expected);
	}
	assert_eq!(engine.resident_count(), 0);
	Ok(())
}
