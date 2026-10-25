//! Production node membership preserves slot generations and old physical
//! reads.

use super::*;
use crate::{
	graph::GraphId,
	storage::{graph_index::visit_label_index_pages, types::PageKey},
	values::storage::DiskAtomRef,
};

/// Slots 101/102 cross a Hot page; reused slots retain physical class postings.
#[test]
fn isolated_node_membership_sync_boundaries_reuse_transient_and_snapshots() {
	boundaries(IoMode::Sync);
}

/// Actual NativeAsync must publish the same finite page set and generations.
#[cfg(feature = "native-async-io")]
#[test]
fn isolated_node_membership_native_async_boundaries_reuse_transient_and_snapshots() {
	boundaries(IoMode::NativeAsync);
}

/// Keeps all Base identities fixed while graph-local allocations change.
fn boundaries(io_mode: IoMode) {
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
	let records = (0..105)
		.map(|index| format!("item:n{index}").parse::<RecordId>().assert_ok())
		.collect::<Vec<_>>();
	for (index, record) in records.iter().enumerate() {
		let mut properties = PropertyMap::new();
		properties.insert(value, i64::try_from(index).assert_ok());
		seed.create_atom(record.clone(), properties).assert_ok();
		if index < 101 {
			seed.attach_node(graph.id(), record, None, None).assert_ok();
		}
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let local = local_store(&store);
	let old_root = store.inner.current_root();
	let old = local.capture_query_snapshot(old_root.commit_ts).assert_ok().assert_some();
	let count = old_root
		.schema
		.class("item")
		.assert_some()
		.indexes()
		.find(|index| index.kind() == ClassIndexKind::Count)
		.assert_some()
		.id()
		.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let mut append = store.begin();
	for (index, record) in records.iter().enumerate().take(103).skip(101) {
		let node = append.attach_node(graph.id(), record, None, None).assert_ok();
		assert_eq!((node.slot(), node.generation()), (u64::try_from(index).assert_ok(), 0));
	}
	append.commit().assert_ok();
	assert_fixed_files(&store, graph.id(), &files);
	assert_class_slots(&store, graph.id(), 103);
	let selected_class_file = class_file(&store, graph.id());
	let mut reuse = store.begin();
	for index in [100, 0] {
		reuse.detach_occurrence(graph.id(), &records[index], false).assert_ok();
	}
	for (record, expected_slot) in [(&records[0], 0), (&records[103], 100)] {
		let node = reuse.attach_node(graph.id(), record, None, None).assert_ok();
		assert_eq!((node.slot(), node.generation()), (expected_slot, 1));
	}
	reuse.commit().assert_ok();
	assert_eq!(class_file(&store, graph.id()), selected_class_file);
	let mut transient = store.begin();
	let temporary = transient.attach_node(graph.id(), &records[104], None, None).assert_ok();
	assert_eq!((temporary.slot(), temporary.generation()), (103, 0));
	transient.detach_occurrence(graph.id(), &records[104], false).assert_ok();
	transient.commit().assert_ok();
	assert_eq!(class_file(&store, graph.id()), selected_class_file);
	assert_class_slots(&store, graph.id(), 103);
	assert_effective_probes(&store, &graph, &records);
	assert_fixed_files(&store, graph.id(), &files);
	let current = local
		.capture_query_snapshot(store.inner.current_root().commit_ts)
		.assert_ok()
		.assert_some();
	assert!(matches!(
		local.physical_index(&current, count).assert_ok().assert_some().entries,
		PhysicalIndexEntries::Count(103)
	));
	store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert!(matches!(
		local.physical_index(&old, count).assert_ok().assert_some().entries,
		PhysicalIndexEntries::Count(101)
	));
	drop(current);
	drop(old);
	drop(old_root);
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let root = store.inner.current_root();
	let owner = root.graph(graph.id()).assert_some();
	assert_eq!(owner.slot_capacity(AtomKind::Node), 104);
	for slot in [0, 100, 103] {
		assert_eq!(owner.slot_generation(AtomKind::Node, slot), Some(1));
	}
	for index in [100, 104] {
		assert!(
			root.membership(graph.id(), root.resolve_atom(&records[index]).assert_some()).is_none()
		);
	}
	assert_eq!(root.statistics.graph(graph.id()).assert_some().node_count, 103);
	assert_class_slots(&store, graph.id(), 103);
	assert_effective_probes(&store, &graph, &records);
}

/// Resolves one class array and rejects duplicate canonical ownership.
fn class_file(store: &Datastore, graph: GraphId) -> crate::storage::types::DataFileId {
	let root = store.inner.current_root();
	let class = root.schema.class("item").assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	let state = local_store(store).state.lock();
	let mut files = state.file_catalog.files.iter().filter(|entry| {
		entry.space_id == DataSpaceId::Graph(graph) && entry.path.as_str().ends_with(&suffix)
	});
	let file = files.next().assert_some().file_id;
	assert!(files.next().is_none());
	file
}

/// Independently expects a dense physical slot population after same-class
/// reuse.
fn assert_class_slots(store: &Datastore, graph: GraphId, count: u64) {
	let local = local_store(store);
	let file = class_file(store, graph);
	let state = local.state.lock();
	let mut actual = Vec::new();
	visit_label_index_pages::<DiskAtomRef>(
		|number, page| {
			local.read_update_page_into(
				&state,
				PageKey::new(DataSpaceId::Graph(graph), file, number),
				page,
			)
		},
		AtomKind::Node,
		|atom| {
			actual.push(atom.slot());
			Ok(())
		},
	)
	.assert_ok();
	assert_eq!(actual, (0..count).collect::<Vec<_>>());
	assert_eq!(
		state.graph_superblocks[&graph].node_hot_pages,
		if count > 102 {
			2
		} else {
			1
		}
	);
}

/// Hot, metadata, AtomStore and Base scalar identities remain unchanged.
fn assert_fixed_files(
	store: &Datastore,
	graph: GraphId,
	files: &[crate::storage::catalog::FileCatalogEntry],
) {
	let state = local_store(store).state.lock();
	for entry in files {
		let selected = entry.file_id.kind() == Some(DataFileKind::Index)
			&& (entry.space_id == DataSpaceId::Graph(graph)
				|| entry.path.as_str().ends_with("/memberships.fgidx"));
		if !selected {
			assert!(state.file_catalog.files.contains(entry), "{}", entry.path.as_str());
		}
	}
}

/// Independent values distinguish removed and inserted owners of reused slots.
fn assert_effective_probes(
	store: &Datastore,
	graph: &crate::graph::GraphRef,
	records: &[RecordId],
) {
	let root = store.inner.current_root();
	let class = root.schema.class("item").assert_some();
	let mut read = store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let physical_snapshot =
		local_store(store).capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let mut checked = 0;
	for index in class.indexes().filter(|index| {
		index.scope()
			== (IndexScope::Effective {
				graph: graph.id(),
			})
			&& matches!(index.kind(), ClassIndexKind::Standard | ClassIndexKind::Unique)
	}) {
		checked += 1;
		let physical = local_store(store)
			.physical_index(&physical_snapshot, index.id().assert_some())
			.assert_ok()
			.assert_some();
		let PhysicalIndexEntries::Scalar(groups) = &physical.entries else {
			unreachable!("scalar membership fixture changed kind");
		};
		assert_eq!(groups.len(), 103);
		let mut slots =
			groups.iter().flat_map(|group| &group.atoms).map(|atom| atom.slot).collect::<Vec<_>>();
		slots.sort_unstable();
		assert_eq!(slots, (0..103).collect::<Vec<_>>());
		for value in [0_usize, 100, 103, 104] {
			let mut cursor = read
				.query_index_probe_cursor(
					&snapshot,
					"item",
					Some(graph),
					index.id().assert_some(),
					&[Value::from(i64::try_from(value).assert_ok())],
				)
				.assert_ok();
			let first = cursor.next_row().assert_ok();
			if matches!(value, 100 | 104) {
				assert!(first.is_none());
			} else {
				assert_eq!(first.assert_some().record_id, records[value]);
			}
			assert!(cursor.next_row().assert_ok().is_none());
		}
	}
	assert_eq!(checked, 2);
}
