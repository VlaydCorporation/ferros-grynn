//! Combined Base and graph-local NODE lifecycle uses existing physical files.

use super::*;
use crate::{
	datastore::Transaction,
	graph::GraphRef,
	storage::catalog::FileCatalogEntry,
	values::PropertyKeyId,
};

/// Combined transitions close the two direct coverage gaps from GNP-01.
mod combined_tests;
/// Boundaries, reuse, late replay and selected index fallback.
mod lifecycle_tests;
/// Independent physical images, index probes and publication page sets.
mod oracles;
/// Admission and writer failures retain a complete durable image.
mod recovery_tests;
/// Ownerless global lifecycles retain empty class-file and allocator authority.
mod transient_tests;
use oracles::{assert_hot_frames, assert_image};

/// The database must close before its temporary directory is removed.
struct Fixture {
	/// Current backend can be replaced by a cold reopen.
	store: Datastore,
	/// Both create and reopen must retain the requested transport.
	options: LocalOptions,
	/// Existing standalone graph receives the new global owner.
	graph: GraphRef,
	/// Every file in this graph must remain untouched.
	other: GraphRef,
	/// Prepared dictionary key avoids unrelated schema changes.
	key: PropertyKeyId,
	/// Outlives all database handles declared above.
	directory: TestDirectory,
}

/// Creating and attaching a NODE must not rebuild unrelated generations.
#[test]
fn global_node_create_attach_sync_retains_files_and_reopens() {
	create_attach(IoMode::Sync);
}

/// NativeAsync must actually remain selected after both cold opens.
#[cfg(feature = "native-async-io")]
#[test]
fn global_node_create_attach_native_async_retains_files_and_reopens() {
	create_attach(IoMode::NativeAsync);
}

/// Captures the complete source registry before either allocator changes.
fn create_attach(io_mode: IoMode) {
	let mut fixture = fixture(io_mode);
	let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
	let record = "item:new".parse::<RecordId>().assert_ok();
	let boundary = local_store(&fixture.store).wal.snapshot().visible_lsn();
	candidate(&fixture, &record, 10).commit().assert_ok();
	assert_fixed_files(&fixture, &files);
	assert_hot_frames(&fixture, boundary, &[0], &[0]);
	assert_image(&fixture, &[(record.clone(), 10)]);
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	assert_eq!(
		root.atom(global).assert_some().properties().get(fixture.key),
		Some(&Value::from(10))
	);
	assert!(root.membership(fixture.graph.id(), global).is_some());
	assert!(root.membership(fixture.other.id(), global).is_none());
	assert_eq!(root.statistics.graph(fixture.graph.id()).assert_some().node_count, 1);
	drop(root);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	assert!(
		root.membership(fixture.graph.id(), root.resolve_atom(&record).assert_some()).is_some()
	);
	drop(root);
	assert_image(&fixture, &[(record, 10)]);
}

/// The target class exists while its first graph-local bucket starts empty.
fn fixture(io_mode: IoMode) -> Fixture {
	reopen(fixture_with_anchor(io_mode, true))
}

/// Zero-atom classes need a warm seed without an unrelated Base control row.
fn fixture_with_anchor(io_mode: IoMode, anchor: bool) -> Fixture {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let key = define_membership_class(&mut seed, graph.id());
	if anchor {
		let record = "item:untouched".parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(key, -1_i64);
		seed.create_atom(record.clone(), properties).assert_ok();
		seed.attach_node(other.id(), &record, None, None).assert_ok();
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	Fixture {
		store,
		options,
		graph,
		other,
		key,
		directory,
	}
}

/// Replays both global and local allocations through the ordinary API.
fn candidate(fixture: &Fixture, record: &RecordId, value: i64) -> Transaction {
	let mut write = fixture.store.begin();
	let mut properties = PropertyMap::new();
	properties.insert(fixture.key, value);
	write.create_atom(record.clone(), properties).assert_ok();
	write.attach_node(fixture.graph.id(), record, None, None).assert_ok();
	write
}

/// Selected scalar/membership/class trees may use their verified fallback.
fn assert_fixed_files(fixture: &Fixture, files: &[FileCatalogEntry]) {
	let state = local_store(&fixture.store).state.lock();
	for entry in files {
		let selected = entry.file_id.kind() == Some(DataFileKind::Index)
			&& entry.space_id != DataSpaceId::Graph(fixture.other.id())
			|| (entry.space_id == DataSpaceId::Graph(fixture.graph.id())
				&& entry.file_id.kind() == Some(DataFileKind::Statistics));
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"rebuilt unrelated file: {}",
				entry.path.as_str()
			);
		}
	}
}

/// Consumes the previous handle before opening the same directory again.
fn reopen(mut fixture: Fixture) -> Fixture {
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	fixture.store.inner.maintenance.shutdown();
	assert_eq!(
		fixture.store.local_storage_status().assert_some().options().io_mode,
		fixture.options.io_mode
	);
	fixture
}
