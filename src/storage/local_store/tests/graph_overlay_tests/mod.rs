//! Sparse overlay publication must preserve physical identities and history.

use super::*;
use crate::{datastore::IoMode, values::PropertyKeyId};

/// Property-key growth shares the same sparse publication boundary.
mod dictionary_tests;
/// Key preparation fails atomically before scalar output or WAL.
mod key_resource_tests;
/// Label swaps keep dense namespaces while replacing selected immutable arrays.
pub(super) mod label_tests;
/// Existing-slot weight changes share overlay page preparation.
mod state_tests;

/// Sync overlays share the existing commit, checkpoint and recovery authority.
#[test]
fn overlay_point_sync_preserves_files_mixed_keys_and_snapshots() {
	overlay_point_publication(IoMode::Sync);
}

/// NativeAsync must use the requested mode after create and cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_point_native_async_preserves_files_mixed_keys_and_snapshots() {
	overlay_point_publication(IoMode::NativeAsync);
}

/// Exercises two node slots, an edge slot and successive WAL-only appends.
fn overlay_point_publication(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let mut options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	// Keep this bounded fixture below automatic page/timer triggers so the
	// explicit checkpoint, rather than an earlier commit, makes progress.
	options.checkpoint.unbackfilled_page_percent = 100;
	options.checkpoint.interval = Duration::from_secs(3600);
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let mut class = ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("value", Type::Int),
		ClassField::new("payload", Type::Any),
	])
	.assert_ok();
	for (name, kind, scope) in [
		("base", ClassIndexKind::Standard, IndexScope::Base),
		("effective", ClassIndexKind::Standard, IndexScope::Effective {
			graph: graph.id(),
		}),
		("unique", ClassIndexKind::Unique, IndexScope::Effective {
			graph: graph.id(),
		}),
		("other", ClassIndexKind::Standard, IndexScope::Effective {
			graph: other.id(),
		}),
	] {
		class =
			class.with_index(ClassIndex::new(name, kind, scope, ["value"]).assert_ok()).assert_ok();
	}
	seed.define_class(class).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let value = seed.intern_property("value").assert_ok();
	let payload = seed.intern_property("payload").assert_ok();
	let absent = seed.intern_property("absent").assert_ok();
	let records = ["item:a", "item:b"].map(|record| record.parse::<RecordId>().assert_ok());
	let mut nodes = Vec::new();
	for (index, record) in records.iter().enumerate() {
		let mut properties = PropertyMap::new();
		properties.insert(value, Value::from(10_i64 + i64::try_from(index).assert_ok()));
		properties.insert(payload, Value::from("base"));
		seed.create_atom(record.clone(), properties).assert_ok();
		nodes.push(seed.attach_node(graph.id(), record, Some("N".into()), None).assert_ok());
		seed.attach_node(other.id(), record, None, None).assert_ok();
	}
	let edge = "link:e".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(payload, Value::from("edge-base"));
	seed.create_atom(edge.clone(), properties).assert_ok();
	seed.attach_edge(
		graph.id(),
		&edge,
		Some("E".into()),
		EdgeTopology::directed([nodes[0]], [nodes[1]]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	// Open checkpoints recovered WAL. Append a real source change afterward
	// so the old pin is ahead of home authority without altering scalar keys.
	let mut source = store.begin();
	source.hide_property(graph.id(), &records[0], absent).assert_ok();
	source.commit().assert_ok();
	let before_files = local.state.lock().file_catalog.files.clone();
	let indexes = local
		.state
		.lock()
		._database_catalog
		.classes
		.iter()
		.flat_map(|class| &class.indexes)
		.map(|index| (index.name.as_str().to_owned(), index.id))
		.collect::<std::collections::BTreeMap<_, _>>();
	let mut old = store.begin();
	let old_snapshot = old.query_snapshot().assert_ok();
	let old_physical = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	let boundary = local.wal.snapshot().visible_lsn();
	assert!(boundary > local.state.lock().database_superblock.checkpoint_lsn);
	let mut write = store.begin();
	write.set_overlay(graph.id(), &records[0], value, 100.into()).assert_ok();
	write.set_overlay(graph.id(), &records[1], value, 101.into()).assert_ok();
	write.hide_property(graph.id(), &edge, payload).assert_ok();
	write.hide_property(graph.id(), &records[0], absent).assert_ok();
	write.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, before_files);
	assert_overlay_label_statistic(&store, graph.id(), AtomKind::Edge, "E", 1, 0);
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.collect::<Vec<_>>();
	assert!(
		frames.iter().any(|frame| frame.key().file_id().kind()
			== Some(crate::storage::types::DataFileKind::Overlays))
	);
	assert!(frames.iter().all(|frame| {
		frame.key().space_id() == crate::datastore::DataSpaceId::Graph(graph.id())
			|| frame.key().file_id().kind()
				== Some(crate::storage::types::DataFileKind::SystemCatalog)
	}));
	for name in ["effective", "unique"] {
		assert_overlay_index(&store, &graph, indexes[name], &records[0], 100);
		assert_overlay_missing_keys(&store, &graph, indexes[name], &[10, 11]);
	}
	let mut overlay_only = store.begin();
	overlay_only.set_overlay(graph.id(), &records[0], absent, Value::Null).assert_ok();
	overlay_only.commit().assert_ok();
	assert_overlay_label_property_count(&store, graph.id(), 5);
	assert_overlay_property(&store, graph.id(), &records[0], absent, Value::Null);
	let mut write = store.begin();
	write.reset_property(graph.id(), &records[0], absent).assert_ok();
	write.set_base_property(&records[0], value, 20.into()).assert_ok();
	write.set_overlay(graph.id(), &records[0], value, 30.into()).assert_ok();
	write.reset_property(graph.id(), &records[1], value).assert_ok();
	write.commit().assert_ok();
	assert_overlay_label_property_count(&store, graph.id(), 4);
	for name in ["effective", "unique"] {
		assert_overlay_index(&store, &graph, indexes[name], &records[0], 30);
		assert_overlay_index(&store, &graph, indexes[name], &records[1], 11);
		assert_overlay_missing_keys(&store, &graph, indexes[name], &[10, 100, 101]);
	}
	assert_overlay_index(&store, &other, indexes["other"], &records[0], 20);
	let mut write = store.begin();
	write
		.set_overlay(graph.id(), &records[0], payload, Value::from("x".repeat(48 * 1024)))
		.assert_ok();
	write.set_overlay(graph.id(), &edge, payload, Value::Null).assert_ok();
	write.commit().assert_ok();
	assert_overlay_property(
		&store,
		graph.id(),
		&records[0],
		payload,
		Value::from("x".repeat(48 * 1024)),
	);
	assert_overlay_property(&store, graph.id(), &edge, payload, Value::Null);
	assert_overlay_label_statistic(&store, graph.id(), AtomKind::Edge, "E", 1, 1);
	let mut write = store.begin();
	write.reset_property(graph.id(), &records[0], payload).assert_ok();
	write.reset_property(graph.id(), &records[0], absent).assert_ok();
	write.set_overlay(graph.id(), &records[0], value, 40.into()).assert_ok();
	write.set_base_property(&records[0], value, 25.into()).assert_ok();
	write.reset_property(graph.id(), &records[0], value).assert_ok();
	write.reset_property(graph.id(), &edge, payload).assert_ok();
	write.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, before_files);
	let authority_before = {
		let state = local.state.lock();
		(state.database_superblock.generation, state.database_superblock.checkpoint_lsn)
	};
	assert!(store.checkpoint(CheckpointMode::Passive).assert_ok());
	{
		let state = local.state.lock();
		assert!(state.database_superblock.generation > authority_before.0);
		assert!(state.database_superblock.checkpoint_lsn > authority_before.1);
	}
	// Publish against current checkpoint authority while an old reader pins
	// the previous graph header and index pages.
	let mut write = store.begin();
	write.set_overlay(graph.id(), &records[0], value, 26.into()).assert_ok();
	write.commit().assert_ok();
	{
		let state = local.state.lock();
		let header = state.graph_superblocks[&graph.id()];
		assert_eq!(header.checkpoint_generation, state.database_superblock.generation);
		assert_eq!(header.backfilled_lsn, state.database_superblock.checkpoint_lsn);
		assert_eq!(header.checkpoint_record_lsn, state.database_superblock.checkpoint_record_lsn);
		assert_eq!(state.file_catalog.files, before_files);
	}
	for name in ["effective", "unique"] {
		assert_overlay_index(&store, &graph, indexes[name], &records[0], 26);
		assert_overlay_missing_keys(&store, &graph, indexes[name], &[10, 25, 30, 40, 100, 101]);
	}
	assert_overlay_index(&store, &other, indexes["other"], &records[0], 25);
	// The old physical leaves are first decoded after checkpoint backfill.
	let root = store.inner.current_root();
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	let old_key = crate::values::storage::encode_index_key(
		&[(&Value::from(10_i64), &Type::Int)],
		&mut context,
	)
	.assert_ok();
	let new_key = crate::values::storage::encode_index_key(
		&[(&Value::from(26_i64), &Type::Int)],
		&mut context,
	)
	.assert_ok();
	drop(root);
	for name in ["effective", "unique"] {
		assert_overlay_snapshot_postings(local, &old_physical, indexes[name], &old_key, &[(
			AtomKind::Node,
			nodes[0].slot(),
		)]);
		assert_overlay_snapshot_postings(local, &old_physical, indexes[name], &new_key, &[]);
		let mut cursor = old
			.query_index_probe_cursor(&old_snapshot, "item", Some(&graph), indexes[name], &[
				10.into()
			])
			.assert_ok();
		assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, records[0]);
		assert!(cursor.next_row().assert_ok().is_none());
		let mut missing = old
			.query_index_probe_cursor(&old_snapshot, "item", Some(&graph), indexes[name], &[
				26.into()
			])
			.assert_ok();
		assert!(missing.next_row().assert_ok().is_none());
	}
	assert_eq!(old.effective_property(graph.id(), &records[0], value).assert_ok(), Some(10.into()));
	drop(old_physical);
	drop(old_snapshot);
	drop(old);
	drop(store);
	let mut reopened = Datastore::open_local(&directory.0, options).assert_ok();
	for checkpoint in [false, true] {
		if checkpoint {
			reopened.checkpoint(CheckpointMode::Restart).assert_ok();
			drop(reopened);
			reopened = Datastore::open_local(&directory.0, options).assert_ok();
		}
		assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
		for name in ["effective", "unique"] {
			assert_overlay_index(&reopened, &graph, indexes[name], &records[0], 26);
			assert_overlay_missing_keys(&reopened, &graph, indexes[name], &[
				10, 25, 30, 40, 100, 101,
			]);
		}
		assert_overlay_property(&reopened, graph.id(), &records[0], payload, Value::from("base"));
		assert_overlay_property(&reopened, graph.id(), &edge, payload, Value::from("edge-base"));
		assert_overlay_label_property_count(&reopened, graph.id(), 4);
		assert_overlay_label_statistic(&reopened, graph.id(), AtomKind::Edge, "E", 1, 1);
	}
}

/// Checks both the normal query adapter and explicitly decoded physical keys.
fn assert_overlay_index(
	store: &Datastore,
	graph: &crate::graph::GraphRef,
	index: crate::datastore::IndexId,
	record: &RecordId,
	value: i64,
) {
	let mut read = store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let mut cursor = read
		.query_index_probe_cursor(&snapshot, "item", Some(graph), index, &[value.into()])
		.assert_ok();
	assert_eq!(&cursor.next_row().assert_ok().assert_some().record_id, record);
	assert!(cursor.next_row().assert_ok().is_none());
	assert_overlay_index_postings(store, graph, index, value, Some(record));
}

/// The same absence oracle covers vacated and never-published temporary keys.
fn assert_overlay_missing_keys(
	store: &Datastore,
	graph: &crate::graph::GraphRef,
	index: crate::datastore::IndexId,
	values: &[i64],
) {
	for &value in values {
		assert_overlay_index_postings(store, graph, index, value, None);
	}
}

/// Eager query bindings may read RAM; a captured physical snapshot cannot.
fn assert_overlay_index_postings(
	store: &Datastore,
	graph: &crate::graph::GraphRef,
	index: crate::datastore::IndexId,
	value: i64,
	expected: Option<&RecordId>,
) {
	let root = store.inner.current_root();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let pin = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	let key = crate::values::storage::encode_index_key(
		&[(&Value::from(value), &Type::Int)],
		&mut context,
	)
	.assert_ok();
	let expected = expected
		.into_iter()
		.map(|record| {
			let global = root.atom_by_record(record).assert_some().id();
			let owner = root.membership(graph.id(), global).assert_some();
			(owner.kind().assert_some(), owner.slot())
		})
		.collect::<Vec<_>>();
	assert_overlay_snapshot_postings(local, &pin, index, &key, &expected);
}

/// Decodes all matching groups, including duplicates, under the supplied pin.
fn assert_overlay_snapshot_postings(
	local: &crate::storage::LocalStore,
	pin: &crate::storage::LocalQuerySnapshot,
	index: crate::datastore::IndexId,
	key: &[u8],
	expected: &[(AtomKind, u64)],
) {
	let decoded = local.physical_index(pin, index).assert_ok().assert_some();
	let groups = match &decoded.entries {
		crate::storage::PhysicalIndexEntries::Scalar(groups) => Some(groups),
		_ => None,
	}
	.assert_some();
	let actual = groups
		.iter()
		.filter(|group| group.key == key)
		.flat_map(|group| group.atoms.iter().map(|atom| (atom.kind, atom.slot)))
		.collect::<Vec<_>>();
	assert_eq!(actual, expected, "physical postings for canonical key {key:?}");
}

/// Checks the logical oracle independently of the physical page assertions.
fn assert_overlay_property(
	store: &Datastore,
	graph: crate::graph::GraphId,
	record: &RecordId,
	key: PropertyKeyId,
	value: Value,
) {
	let mut read = store.begin();
	assert_eq!(read.effective_property(graph, record, key).assert_ok(), Some(value.clone()));
	let owner = read.membership(graph, record).assert_ok().assert_some();
	let snapshot = read.query_snapshot().assert_ok();
	let root = store.inner.current_root();
	let reference = root.graph(graph).assert_some().reference();
	let physical =
		read.query_physical_fields(&snapshot, reference, owner).assert_ok().assert_some();
	assert_eq!(physical.properties.get(key), Some(&value));
}

/// Sync must recover one complete overlay/index image at every WAL boundary.
#[test]
fn overlay_point_sync_failures_publish_no_partial_image() {
	overlay_failure_contract(IoMode::Sync);
}

/// NativeAsync uses the same publication fence and recovery contract.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_point_native_async_failures_publish_no_partial_image() {
	overlay_failure_contract(IoMode::NativeAsync);
}

/// Seeds an existing graph with two independent Effective scalar bindings.
fn overlay_failure_fixture(
	io_mode: IoMode,
) -> (TestDirectory, Datastore, crate::graph::GraphRef, [PropertyKeyId; 2], [RecordId; 2]) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let mut class = ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("value", Type::Int),
		ClassField::new("payload", Type::Any),
	])
	.assert_ok();
	for (name, kind) in
		[("effective", ClassIndexKind::Standard), ("unique", ClassIndexKind::Unique)]
	{
		class = class
			.with_index(
				ClassIndex::new(
					name,
					kind,
					IndexScope::Effective {
						graph: graph.id(),
					},
					["value"],
				)
				.assert_ok(),
			)
			.assert_ok();
	}
	seed.define_class(class).assert_ok();
	let value = seed.intern_property("value").assert_ok();
	let payload = seed.intern_property("payload").assert_ok();
	let records = ["item:a", "item:b"].map(|record| record.parse::<RecordId>().assert_ok());
	for (index, record) in records.iter().enumerate() {
		let mut properties = PropertyMap::new();
		properties.insert(value, Value::from(i64::try_from(index).assert_ok()));
		properties.insert(payload, Value::from("base"));
		seed.create_atom(record.clone(), properties).assert_ok();
		seed.attach_node(graph.id(), record, Some("N".into()), None).assert_ok();
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	store.inner.maintenance.shutdown();
	(directory, store, graph, [value, payload], records)
}

/// Builds multiple Hot, overlay and index frames in the same candidate.
fn apply_overlay_failure_candidate(
	write: &mut crate::datastore::Transaction,
	graph: crate::graph::GraphId,
	keys: [PropertyKeyId; 2],
	records: &[RecordId; 2],
) {
	for (index, record) in records.iter().enumerate() {
		write
			.set_overlay(
				graph,
				record,
				keys[0],
				Value::from(100_i64 + i64::try_from(index).assert_ok()),
			)
			.assert_ok();
	}
	write.set_overlay(graph, &records[0], keys[1], Value::from("x".repeat(32 * 1024))).assert_ok();
	write.hide_property(graph, &records[1], keys[1]).assert_ok();
}

/// Compares decoded physical values and both scalar bindings after recovery.
fn assert_overlay_failure_image(
	store: &Datastore,
	graph: &crate::graph::GraphRef,
	keys: [PropertyKeyId; 2],
	records: &[RecordId; 2],
	published: bool,
) {
	let indexes = store.inner.current_root().indexes.graphs[&graph.id()]
		.explicit
		.values()
		.filter(|binding| matches!(binding.kind, ClassIndexKind::Standard | ClassIndexKind::Unique))
		.map(|binding| binding.id)
		.collect::<Vec<_>>();
	assert_eq!(indexes.len(), 2);
	for (offset, record) in records.iter().enumerate() {
		let value = i64::try_from(offset).assert_ok()
			+ if published {
				100
			} else {
				0
			};
		assert_overlay_property(store, graph.id(), record, keys[0], value.into());
		for index in &indexes {
			assert_overlay_index(store, graph, *index, record, value);
			let absent = i64::try_from(offset).assert_ok()
				+ if published {
					0
				} else {
					100
				};
			assert_overlay_index_postings(store, graph, *index, absent, None);
		}
	}
	let payload = if published {
		Value::from("x".repeat(32 * 1024))
	} else {
		Value::from("base")
	};
	assert_overlay_property(store, graph.id(), &records[0], keys[1], payload);
	assert_eq!(
		store.begin().effective_property(graph.id(), &records[1], keys[1]).assert_ok().is_none(),
		published
	);
	assert_overlay_label_property_count(
		store,
		graph.id(),
		if published {
			3
		} else {
			4
		},
	);
}

/// Known failures preserve the source; uncertain writers stay fenced until
/// reopen.
fn overlay_failure_contract(io_mode: IoMode) {
	use crate::storage::wal::{CommitFailureGuard, CommitFailurePoint};
	{
		let (_directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
		let local = match &store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let stamp = store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let ceiling = local.state.lock().graph_superblocks[&graph.id()].overlays_pages;
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let mut write = store.begin();
			apply_overlay_failure_candidate(&mut write, graph.id(), keys, &records);
			let failure = GenerationFailureGuard::set(point);
			write.commit().assert_err();
			drop(failure);
			assert_eq!(store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(local.state.lock().graph_superblocks[&graph.id()].overlays_pages, ceiling);
			assert_overlay_failure_image(&store, &graph, keys, &records, false);
		}
		let mut accepted = store.begin();
		apply_overlay_failure_candidate(&mut accepted, graph.id(), keys, &records);
		assert_eq!(accepted.commit().assert_ok().commit_ts(), stamp + 1);
		assert_overlay_failure_image(&store, &graph, keys, &records, true);
	}
	for point in [
		CommitFailurePoint::BeforeAppend,
		CommitFailurePoint::AfterAppend,
		CommitFailurePoint::AfterFlush,
		CommitFailurePoint::AfterSync,
	] {
		let (directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
		let options = LocalOptions {
			io_mode,
			buffer_pool_pages: 64,
			..LocalOptions::default()
		};
		let stamp = store.inner.current_root().commit_ts;
		let local = match &store.inner.backend {
			StoreBackend::Local(local) => Some(local),
			StoreBackend::InMemory => None,
		}
		.assert_some();
		let files = local.state.lock().file_catalog.files.clone();
		let ceiling = local.state.lock().graph_superblocks[&graph.id()].overlays_pages;
		let mut candidate = store.begin();
		apply_overlay_failure_candidate(&mut candidate, graph.id(), keys, &records);
		{
			let _failure = CommitFailureGuard::set(point);
			candidate.commit().assert_err();
		}
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(local.state.lock().graph_superblocks[&graph.id()].overlays_pages, ceiling);
		let mut retry = store.begin();
		apply_overlay_failure_candidate(&mut retry, graph.id(), keys, &records);
		retry.commit().assert_err();
		drop(store);
		let reopened = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
		let published = point != CommitFailurePoint::BeforeAppend;
		assert_eq!(reopened.inner.current_root().commit_ts, stamp + u64::from(published));
		assert_overlay_failure_image(&reopened, &graph, keys, &records, published);
		reopened.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(reopened);
		let cold = Datastore::open_local(&directory.0, options).assert_ok();
		assert_eq!(cold.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_overlay_failure_image(&cold, &graph, keys, &records, published);
	}
}

/// Independent writers and presence changes preserve unaffected file
/// identities.
#[test]
fn overlay_point_independent_replay_and_presence_updates_remain_exact() {
	overlay_presence_contract(IoMode::Sync);
}

/// NativeAsync uses the same counter pages and verifies actual reopen mode.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_point_native_async_presence_updates_remain_exact() {
	overlay_presence_contract(IoMode::NativeAsync);
}

/// Presence changes retain physical file identities and exact lazy counters.
fn overlay_presence_contract(io_mode: IoMode) {
	let (directory, store, graph, keys, records) = overlay_failure_fixture(io_mode);
	let options = LocalOptions {
		io_mode,
		buffer_pool_pages: 64,
		..LocalOptions::default()
	};
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let mut first = store.begin();
	let mut second = store.begin();
	first.set_overlay(graph.id(), &records[0], keys[0], 100.into()).assert_ok();
	second.set_overlay(graph.id(), &records[1], keys[0], 101.into()).assert_ok();
	first.commit().assert_ok();
	second.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_overlay_property(&store, graph.id(), &records[0], keys[0], 100.into());
	assert_overlay_property(&store, graph.id(), &records[1], keys[0], 101.into());
	assert_overlay_replayed_indexes(&store, &graph, &records);
	let old_root = store.inner.current_root();
	let old_physical = local.capture_query_snapshot(old_root.commit_ts).assert_ok().assert_some();
	let mut old_reader = store.begin();
	let boundary = local.wal.snapshot().visible_lsn();
	let mut hide = store.begin();
	hide.hide_property(graph.id(), &records[0], keys[1]).assert_ok();
	hide.commit().assert_ok();
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_overlay_label_property_count(&store, graph.id(), 3);
	let frames = local
		.wal
		.recovery_frames()
		.into_iter()
		.filter(|frame| frame.frame_lsn() > boundary)
		.collect::<Vec<_>>();
	assert_eq!(
		frames
			.iter()
			.filter(|frame| frame.key().file_id().kind()
				== Some(crate::storage::types::DataFileKind::LabelStatistics))
			.count(),
		1
	);
	assert!(
		frames.iter().all(|frame| frame.key().space_id()
			== crate::datastore::DataSpaceId::Graph(graph.id())
			|| frame.key().file_id().kind()
				== Some(crate::storage::types::DataFileKind::SystemCatalog))
	);
	assert_overlay_replayed_indexes(&store, &graph, &records);
	assert!(
		store.begin().effective_property(graph.id(), &records[0], keys[1]).assert_ok().is_none()
	);
	assert!(store.checkpoint(CheckpointMode::Passive).assert_ok());
	assert_eq!(
		old_reader.effective_property(graph.id(), &records[0], keys[1]).assert_ok(),
		Some(Value::from("base"))
	);
	let mut old_statistics = old_root.statistics.as_ref().clone();
	old_statistics.graph_mut(graph.id()).assert_some().labels.clear();
	local.load_planner_statistics(
		&old_physical,
		&old_root,
		&crate::datastore::QueryStatisticsDemand {
			graph_labels: std::collections::BTreeSet::from([graph.id()]),
			..crate::datastore::QueryStatisticsDemand::default()
		},
		&mut old_statistics,
	);
	assert_eq!(
		old_statistics.graph(graph.id()).assert_some().labels[&crate::stats::LabelStatisticKey {
			kind: AtomKind::Node,
			label: "N".into(),
		}]
			.property_count,
		4
	);
	drop(old_reader);
	drop(old_physical);
	drop(old_root);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	reopened.inner.maintenance.shutdown();
	assert!(
		reopened.begin().effective_property(graph.id(), &records[0], keys[1]).assert_ok().is_none()
	);
	assert_overlay_property(&reopened, graph.id(), &records[0], keys[0], 100.into());
	assert_overlay_property(&reopened, graph.id(), &records[1], keys[0], 101.into());
	assert_overlay_replayed_indexes(&reopened, &graph, &records);
	assert_overlay_label_property_count(&reopened, graph.id(), 3);
	let mut set = reopened.begin();
	set.set_overlay(graph.id(), &records[0], keys[1], Value::Null).assert_ok();
	set.commit().assert_ok();
	assert_overlay_label_property_count(&reopened, graph.id(), 4);
	assert_overlay_property(&reopened, graph.id(), &records[0], keys[1], Value::Null);
	let mut reset = reopened.begin();
	reset.reset_property(graph.id(), &records[0], keys[1]).assert_ok();
	reset.commit().assert_ok();
	assert_overlay_label_property_count(&reopened, graph.id(), 4);
	assert_overlay_property(&reopened, graph.id(), &records[0], keys[1], Value::from("base"));
	let mut mixed = reopened.begin();
	mixed.set_base_property(&records[0], keys[1], Value::from("new-base")).assert_ok();
	mixed.hide_property(graph.id(), &records[0], keys[1]).assert_ok();
	mixed.commit().assert_ok();
	assert_overlay_label_property_count(&reopened, graph.id(), 3);
	let local = match &reopened.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	assert_eq!(local.state.lock().file_catalog.files, files);
	reopened.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(reopened);
	let cold = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(cold.local_storage_status().assert_some().options().io_mode, io_mode);
	cold.inner.maintenance.shutdown();
	// Do not hydrate the source: replay must handle a deferred original root.
	assert!(cold.inner.current_root().statistics.auxiliary_deferred());
	let mut reset = cold.begin();
	reset.reset_property(graph.id(), &records[0], keys[1]).assert_ok();
	reset.commit().assert_ok();
	assert_overlay_label_property_count(&cold, graph.id(), 4);
	assert_overlay_property(&cold, graph.id(), &records[0], keys[1], Value::from("new-base"));
	// Base key-set changes share the counter writer without changing
	// membership.
	let mut old = cold.begin();
	let indexes = cold.inner.current_root().indexes.graphs[&graph.id()]
		.explicit
		.values()
		.filter(|binding| matches!(binding.kind, ClassIndexKind::Standard | ClassIndexKind::Unique))
		.map(|binding| binding.id)
		.collect::<Vec<_>>();
	let mut remove = cold.begin();
	remove.remove_base_property(&records[0], keys[0]).assert_ok();
	remove.commit().assert_ok();
	let local = match &cold.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	assert_eq!(local.state.lock().file_catalog.files, files);
	// The independent overlay writer still masks this indexed Base key.
	assert_overlay_label_property_count(&cold, graph.id(), 4);
	assert_overlay_property(&cold, graph.id(), &records[0], keys[0], 100.into());
	let mut reset = cold.begin();
	reset.reset_property(graph.id(), &records[0], keys[0]).assert_ok();
	reset.commit().assert_ok();
	assert_overlay_label_property_count(&cold, graph.id(), 3);
	for index in &indexes {
		assert_overlay_index_postings(&cold, &graph, *index, 100, None);
	}
	assert_eq!(
		old.effective_property(graph.id(), &records[0], keys[0]).assert_ok(),
		Some(100.into())
	);
	drop(old);
	let mut insert = cold.begin();
	insert.set_base_property(&records[0], keys[0], 100.into()).assert_ok();
	insert.commit().assert_ok();
	assert_overlay_label_property_count(&cold, graph.id(), 4);
	assert_overlay_replayed_indexes(&cold, &graph, &records);
	let mut masked = cold.begin();
	masked.set_overlay(graph.id(), &records[0], keys[1], Value::Null).assert_ok();
	masked.remove_base_property(&records[0], keys[1]).assert_ok();
	masked.commit().assert_ok();
	assert_overlay_label_property_count(&cold, graph.id(), 4);
	let mut reset = cold.begin();
	reset.reset_property(graph.id(), &records[0], keys[1]).assert_ok();
	reset.commit().assert_ok();
	assert_overlay_label_property_count(&cold, graph.id(), 3);
	assert!(
		cold.begin().effective_property(graph.id(), &records[0], keys[1]).assert_ok().is_none()
	);
	let mut insert = cold.begin();
	insert.set_base_property(&records[0], keys[1], Value::Null).assert_ok();
	insert.commit().assert_ok();
	assert_overlay_label_property_count(&cold, graph.id(), 4);
	assert_eq!(local.state.lock().file_catalog.files, files);
	cold.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(cold);
	let final_store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(final_store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_overlay_label_property_count(&final_store, graph.id(), 4);
	assert_overlay_property(&final_store, graph.id(), &records[0], keys[1], Value::Null);
	assert_overlay_replayed_indexes(&final_store, &graph, &records);
}

/// Hydrates deferred counters; callers also reopen to verify persisted bytes.
fn assert_overlay_label_property_count(
	store: &Datastore,
	graph: crate::graph::GraphId,
	count: u64,
) {
	assert_overlay_label_statistic(store, graph, AtomKind::Node, "N", 2, count);
}

/// Loads exact counters independently of the logical value oracle.
fn assert_overlay_label_statistic(
	store: &Datastore,
	graph: crate::graph::GraphId,
	kind: AtomKind,
	label: &str,
	atom_count: u64,
	property_count: u64,
) {
	let mut root = store.inner.current_root().as_ref().clone();
	store.inner.hydrate_statistics(&mut root).assert_ok();
	let key = crate::stats::LabelStatisticKey {
		kind,
		label: label.into(),
	};
	let statistic = root.statistics.graph(graph).assert_some().labels.get(&key).assert_some();
	assert_eq!(statistic.atom_count, atom_count);
	assert_eq!(statistic.property_count, property_count);
}

/// Replay must replace old postings in both bindings, including after fallback.
fn assert_overlay_replayed_indexes(
	store: &Datastore,
	graph: &crate::graph::GraphRef,
	records: &[RecordId; 2],
) {
	let root = store.inner.current_root();
	for index in root.indexes.graphs[&graph.id()]
		.explicit
		.values()
		.filter(|index| matches!(index.kind, ClassIndexKind::Standard | ClassIndexKind::Unique))
	{
		for (offset, record) in records.iter().enumerate() {
			let offset = i64::try_from(offset).assert_ok();
			assert_overlay_index(store, graph, index.id, record, 100 + offset);
			assert_overlay_index_postings(store, graph, index.id, offset, None);
		}
	}
}
