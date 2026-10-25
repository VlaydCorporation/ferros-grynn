//! The ordinary WAL barrier owns membership failure and recovery semantics.

use super::*;
use crate::{
	datastore::Transaction,
	graph::GraphRef,
	storage::wal::{CommitFailureGuard, CommitFailurePoint},
};

/// Keeps database handles ahead of their temporary directory owner.
struct Fixture {
	/// Current backend is replaced only by an explicit cold reopen.
	store: Datastore,
	/// Reopen preserves the controlling transport and builder budget.
	options: LocalOptions,
	/// The source graph starts empty.
	graph: GraphRef,
	/// Existing Base identity never changes in the candidate.
	record: RecordId,
	/// Must outlive the datastore above.
	directory: TestDirectory,
}

/// Known rejection and uncertain append recovery cover the same Sync candidate.
#[test]
fn isolated_node_membership_sync_failures_publish_one_complete_image() {
	failures(IoMode::Sync);
}

/// NativeAsync must preserve the identical atomic recovery boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn isolated_node_membership_native_async_failures_publish_one_complete_image() {
	failures(IoMode::NativeAsync);
}

/// A small logical builder budget rejects before selected output creation.
#[test]
fn isolated_node_membership_small_budget_preserves_root_wal_and_files() {
	let mut fixture = fixture(IoMode::Sync);
	fixture.options.maintenance.max_memory_bytes = NonZeroUsize::new(32 * 1024).assert_some();
	fixture = reopen(fixture);
	let local = local_store(&fixture.store);
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let error = candidate(&fixture).commit().assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	assert_image(&fixture, false);
}

/// An old writer must publish its replayed slot instead of its private slot.
#[test]
fn isolated_node_membership_late_writer_preserves_independent_allocation() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let other = "item:b".parse::<RecordId>().assert_ok();
		let mut base = fixture.store.begin();
		let key = base.intern_property("value").assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(key, 20_i64);
		base.create_atom(other.clone(), properties).assert_ok();
		base.commit().assert_ok();
		let late = candidate(&fixture);
		let mut independent = fixture.store.begin();
		let first = independent.attach_node(fixture.graph.id(), &other, None, None).assert_ok();
		assert_eq!(first.slot(), 0);
		independent.commit().assert_ok();
		late.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		let root = fixture.store.inner.current_root();
		let graph = fixture.graph.id();
		let actual =
			root.membership(graph, root.resolve_atom(&fixture.record).assert_some()).assert_some();
		assert_eq!((actual.slot(), actual.generation()), (1, 0));
		assert_eq!(root.membership(graph, root.resolve_atom(&other).assert_some()), Some(first));
		assert_eq!(root.statistics.graph(graph).assert_some().node_count, 2);
	}
}

/// A cold missing bucket cannot hide nonempty malformed physical source data.
#[test]
fn isolated_node_membership_invalid_empty_class_source_publishes_nothing() {
	use crate::{
		storage::{
			graph_index::build_label_index,
			page::{PageLsn, PageNo},
		},
		values::storage::DiskAtomRef,
	};
	let mut fixture = fixture(IoMode::Sync);
	candidate(&fixture).commit().assert_ok();
	let mut remove = fixture.store.begin();
	remove.detach_occurrence(fixture.graph.id(), &fixture.record, false).assert_ok();
	remove.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	let local = local_store(&fixture.store);
	let class =
		fixture.store.inner.current_root().schema.class("item").assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	let state = local.state.lock();
	let entry = state
		.file_catalog
		.files
		.iter()
		.find(|entry| {
			entry.space_id == DataSpaceId::Graph(fixture.graph.id())
				&& entry.path.as_str().ends_with(&suffix)
		})
		.assert_some();
	let pages = build_label_index(
		&[DiskAtomRef::from_parts(AtomKind::Node, 0).assert_ok()],
		AtomKind::Node,
		PageLsn::ZERO,
	)
	.assert_ok();
	state
		.registry
		.resolve_file(entry.space_id, entry.file_id)
		.assert_ok()
		.file()
		.assert_ok()
		.write_page_raw(PageNo::new(0), &pages[0])
		.assert_ok();
	drop(state);
	fixture = reopen(fixture);
	let local = local_store(&fixture.store);
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let mut write = fixture.store.begin();
	let node = write.attach_node(fixture.graph.id(), &fixture.record, None, None).assert_ok();
	assert_eq!((node.slot(), node.generation()), (0, 1));
	write.commit().assert_err();
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	let root = fixture.store.inner.current_root();
	assert!(
		root.membership(fixture.graph.id(), root.resolve_atom(&fixture.record).assert_some())
			.is_none()
	);
}

/// Failure guards leave the old root/bindings intact until recovery accepts
/// WAL.
fn failures(io_mode: IoMode) {
	{
		let fixture = fixture(io_mode);
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let write = candidate(&fixture);
			{
				let _failure = GenerationFailureGuard::set(point);
				write.commit().assert_err();
			}
			assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			assert_image(&fixture, false);
		}
		candidate(&fixture).commit().assert_ok();
		assert_image(&fixture, true);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let mut fixture = fixture(io_mode);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let files = local_store(&fixture.store).state.lock().file_catalog.files.clone();
		{
			let _failure = CommitFailureGuard::set(point);
			candidate(&fixture).commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local_store(&fixture.store).state.lock().file_catalog.files, files);
		assert_image(&fixture, false);
		fixture = reopen(fixture);
		let accepted = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp + u64::from(accepted));
		assert_image(&fixture, accepted);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_image(&fixture, accepted);
	}
}

/// Full seed publication precedes every ordinary membership candidate.
fn fixture(io_mode: IoMode) -> Fixture {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let value = define_membership_class(&mut seed, graph.id());
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(value, 10_i64);
	seed.create_atom(record.clone(), properties).assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	reopen(Fixture {
		store,
		options,
		graph,
		record,
		directory,
	})
}

/// Prepares the same single target for success and every injected failure.
fn candidate(fixture: &Fixture) -> Transaction {
	let mut write = fixture.store.begin();
	let local = write.attach_node(fixture.graph.id(), &fixture.record, None, None).assert_ok();
	assert_eq!((local.slot(), local.generation()), (0, 0));
	write
}

/// Checks independent membership/Hot and scoped Count results through the
/// backend.
fn assert_image(fixture: &Fixture, accepted: bool) {
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.record).assert_some();
	let graph = fixture.graph.id();
	assert_eq!(root.membership(graph, global).is_some(), accepted);
	assert_eq!(
		root.graph(graph).assert_some().slot_capacity(AtomKind::Node),
		usize::from(accepted)
	);
	assert_eq!(root.statistics.graph(graph).assert_some().node_count, u64::from(accepted));
	let count = root
		.schema
		.class("item")
		.assert_some()
		.indexes()
		.find(|index| index.kind() == ClassIndexKind::Count)
		.assert_some()
		.id()
		.assert_some();
	let local = local_store(&fixture.store);
	let snapshot = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let read = local.physical_index(&snapshot, count).assert_ok().assert_some();
	let PhysicalIndexEntries::Count(count) = read.entries else {
		unreachable!("Count fixture changed kind");
	};
	assert_eq!(count, u64::from(accepted));
}

/// Closes the old backend before opening and verifies actual transport
/// selection.
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
