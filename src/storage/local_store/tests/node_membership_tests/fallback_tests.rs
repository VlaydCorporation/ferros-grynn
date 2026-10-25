//! Large membership deltas exercise selected tree replacements in production.

use super::*;

/// A real leaf overflow must replace only selected trees and remain atomic.
#[test]
fn isolated_node_membership_sync_forced_tree_fallback_is_atomic() {
	fallback(IoMode::Sync);
}

/// Actual NativeAsync must reopen the same selected replacement roots.
#[cfg(feature = "native-async-io")]
#[test]
fn isolated_node_membership_native_async_forced_tree_fallback_is_atomic() {
	fallback(IoMode::NativeAsync);
}

/// Source trees are empty while all Base records already exist before DML.
fn fallback(io_mode: IoMode) {
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
	let key = define_membership_class(&mut seed, graph.id());
	let mut records = Vec::new();
	for index in 0..1024 {
		let record = format!("item:n{index}").parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(key, i64::from(index));
		seed.create_atom(record.clone(), properties).assert_ok();
		records.push(record);
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	let local = local_store(&store);
	let stamp = store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let source = local
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == graph.id())
		.assert_some()
		.membership;
	let reverse = local.state.lock()._database_catalog.atom_store.reverse_membership;
	let directories = generation_directories(&directory.0);
	for rejected in [true, false] {
		let mut write = store.begin();
		for record in &records {
			write.attach_node(graph.id(), record, None, None).assert_ok();
		}
		if rejected {
			{
				let _failure = GenerationFailureGuard::set(GenerationFailurePoint::BeforeWalCommit);
				write.commit().assert_err();
			}
			assert_eq!(store.inner.current_root().commit_ts, stamp);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(local.state.lock().file_catalog.files, files);
			assert_eq!(generation_directories(&directory.0), directories);
		} else {
			write.commit().assert_ok();
		}
	}
	{
		let state = local.state.lock();
		let next = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph.id())
			.assert_some();
		assert_ne!(
			next.membership.file_id, source.file_id,
			"forward leaf overflow used no fallback"
		);
		assert_ne!(
			state._database_catalog.atom_store.reverse_membership.file_id, reverse.file_id,
			"reverse leaf overflow used no fallback"
		);
		for entry in &files {
			let selected = (entry.space_id == crate::datastore::DataSpaceId::Graph(graph.id())
				&& matches!(
					entry.file_id.kind(),
					Some(DataFileKind::Index | DataFileKind::Statistics)
				))
				|| (entry.space_id == crate::datastore::DataSpaceId::Database
					&& entry.path.as_str().ends_with("/memberships.fgidx"));
			if !selected {
				assert!(state.file_catalog.files.contains(entry), "{}", entry.path.as_str());
			}
		}
		assert_eq!(state.graph_superblocks[&graph.id()].node_hot_pages, 11);
	}
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let root = store.inner.current_root();
	assert_eq!(root.statistics.graph(graph.id()).assert_some().node_count, 1024);
	for (slot, record) in records.iter().enumerate() {
		let local =
			root.membership(graph.id(), root.resolve_atom(record).assert_some()).assert_some();
		assert_eq!((local.slot(), local.generation()), (u64::try_from(slot).assert_ok(), 0));
	}
	let local = local_store(&store);
	let snapshot = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let class = root.schema.class("item").assert_some();
	let count = class
		.indexes()
		.find(|index| index.kind() == ClassIndexKind::Count)
		.assert_some()
		.id()
		.assert_some();
	assert!(matches!(
		local.physical_index(&snapshot, count).assert_ok().assert_some().entries,
		PhysicalIndexEntries::Count(1024)
	));
	let mut checked = 0;
	for index in class.indexes().filter(|index| {
		index.scope()
			== (IndexScope::Effective {
				graph: graph.id(),
			})
			&& matches!(index.kind(), ClassIndexKind::Standard | ClassIndexKind::Unique)
	}) {
		checked += 1;
		let physical =
			local.physical_index(&snapshot, index.id().assert_some()).assert_ok().assert_some();
		let PhysicalIndexEntries::Scalar(groups) = &physical.entries else {
			unreachable!("scalar fallback fixture changed kind");
		};
		assert_eq!(groups.len(), 1024);
		let mut slots =
			groups.iter().flat_map(|group| &group.atoms).map(|atom| atom.slot).collect::<Vec<_>>();
		slots.sort_unstable();
		assert_eq!(slots, (0..1024).collect::<Vec<_>>());
		let mut read = store.begin();
		let query = read.query_snapshot().assert_ok();
		for value in [0_usize, 512, 1023] {
			let mut cursor = read
				.query_index_probe_cursor(
					&query,
					"item",
					Some(&graph),
					index.id().assert_some(),
					&[Value::from(i64::try_from(value).assert_ok())],
				)
				.assert_ok();
			assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, records[value]);
			assert!(cursor.next_row().assert_ok().is_none());
		}
	}
	assert_eq!(checked, 2);
}
