//! Child membership counts must agree with every containing directory.

use super::{support::local, *};
use crate::storage::{
	graph_pages::{SubgraphEntry, decode_subgraph_directory},
	image_io::read_pinned_page_cached,
	page::AlignedPage,
};

/// A public Graph inside a NODE still has a physical parent directory.
#[test]
fn child_node_membership_sync_keeps_parent_directory() {
	child_membership(IoMode::Sync, AtomKind::Node, AtomKind::Node, false);
}

/// NativeAsync preserves the NODE container through cold recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn child_node_membership_native_async_keeps_parent_directory() {
	child_membership(IoMode::NativeAsync, AtomKind::Node, AtomKind::Node, false);
}

/// An EDGE transition graph has the same child count obligation.
#[test]
fn child_edge_membership_sync_keeps_parent_directory() {
	child_membership(IoMode::Sync, AtomKind::Edge, AtomKind::Node, false);
}

/// NativeAsync preserves the EDGE container through cold recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn child_edge_membership_native_async_keeps_parent_directory() {
	child_membership(IoMode::NativeAsync, AtomKind::Edge, AtomKind::Node, false);
}

/// NODE growth retains the NODE container's original directory file.
#[test]
fn bounded_child_node_in_node_sync_retains_files() {
	child_membership(IoMode::Sync, AtomKind::Node, AtomKind::Node, true);
}

/// NODE growth retains the EDGE container's original directory file.
#[test]
fn bounded_child_node_in_edge_sync_retains_files() {
	child_membership(IoMode::Sync, AtomKind::Edge, AtomKind::Node, true);
}

/// EDGE growth updates the NODE container's copied EDGE counters.
#[test]
fn bounded_child_edge_in_node_sync_retains_files() {
	child_membership(IoMode::Sync, AtomKind::Node, AtomKind::Edge, true);
}

/// EDGE growth retains both transition-container and child Hot identities.
#[test]
fn bounded_child_edge_in_edge_sync_retains_files() {
	child_membership(IoMode::Sync, AtomKind::Edge, AtomKind::Edge, true);
}

/// NativeAsync preserves a NODE child's directory through a NODE container.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_node_in_node_native_async_retains_files() {
	child_membership(IoMode::NativeAsync, AtomKind::Node, AtomKind::Node, true);
}

/// NativeAsync preserves a NODE child's directory through an EDGE container.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_node_in_edge_native_async_retains_files() {
	child_membership(IoMode::NativeAsync, AtomKind::Edge, AtomKind::Node, true);
}

/// NativeAsync preserves an EDGE child's directory through a NODE container.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_edge_in_node_native_async_retains_files() {
	child_membership(IoMode::NativeAsync, AtomKind::Node, AtomKind::Edge, true);
}

/// NativeAsync preserves an EDGE child's directory through an EDGE container.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_edge_in_edge_native_async_retains_files() {
	child_membership(IoMode::NativeAsync, AtomKind::Edge, AtomKind::Edge, true);
}

/// Real ATTACH, DETACH and reuse exercise the accepted physical source arena.
fn child_membership(
	io_mode: IoMode,
	container_kind: AtomKind,
	membership_kind: AtomKind,
	bounded: bool,
) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let mut store = Datastore::create_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("node", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	let parent = seed.define_graph("parent", GraphKind::Metagraph).assert_ok();
	let [owner, existing, pending, portal] =
		["node:owner", "node:a", "node:pending", "edge:portal"]
			.map(|name| name.parse::<RecordId>().assert_ok());
	for record in [&owner, &existing, &pending, &portal] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let first = seed.attach_node(child.id(), &existing, None, None).assert_ok();
	let parent_node = seed
		.attach_node(
			parent.id(),
			&owner,
			None,
			(container_kind == AtomKind::Node).then_some(child.id()),
		)
		.assert_ok();
	let container = if container_kind == AtomKind::Node {
		parent_node
	} else {
		seed.attach_edge(
			parent.id(),
			&portal,
			None,
			EdgeTopology::directed([parent_node], [parent_node]),
			None,
			Some(child.id()),
		)
		.assert_ok()
	};
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_parent_directory(&store, parent.id(), child.id(), container);
	for (step, detach) in [false, true, false].into_iter().enumerate() {
		if bounded {
			// A pin at the cold home boundary cannot permit Passive progress.
			let mut advance = store.begin();
			advance
				.set_occurrence_label(child.id(), &existing, Some(format!("pin{step}").into()))
				.assert_ok();
			advance.commit().assert_ok();
		}
		let source = store.inner.current_root();
		let backend = local(&store);
		let pin = backend.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
		let source_entry = assert_parent_directory(&store, parent.id(), child.id(), container);
		let (parent_files, child_files, directory_file, parent_header) = {
			let state = backend.state.lock();
			let parent_files = state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == parent.id())
				.assert_some()
				.files;
			let child_files = state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == child.id())
				.assert_some()
				.files;
			let file = state
				.registry
				.resolve_file(parent_files.space(), parent_files.subgraphs)
				.assert_ok();
			(
				parent_files,
				child_files,
				file,
				*state.graph_superblocks.get(&parent.id()).assert_some(),
			)
		};
		let boundary = backend.wal.snapshot().visible_lsn();
		let home_boundary = backend.state.lock().database_superblock.checkpoint_lsn;
		let record = if membership_kind == AtomKind::Node {
			&pending
		} else {
			&portal
		};
		let mut write = store.begin();
		if detach {
			write.detach_occurrence(child.id(), record, false).assert_ok();
		} else {
			let attached = if membership_kind == AtomKind::Node {
				write.attach_node(child.id(), record, None, None).assert_ok()
			} else {
				write
					.attach_edge(
						child.id(),
						record,
						None,
						EdgeTopology::directed([first], [first]),
						None,
						None,
					)
					.assert_ok()
			};
			assert_eq!(
				(attached.slot(), attached.generation()),
				(u64::from(membership_kind == AtomKind::Node), u32::from(step == 2))
			);
		}
		write.commit().assert_ok();
		let root = store.inner.current_root();
		assert_eq!(
			root.membership(child.id(), root.resolve_atom(&existing).assert_some()),
			Some(first)
		);
		assert_eq!(
			root.membership(child.id(), root.resolve_atom(record).assert_some()).is_none(),
			detach
		);
		drop(root);
		assert_parent_directory(&store, parent.id(), child.id(), container);
		if bounded {
			let state = backend.state.lock();
			for file in
				parent_files.regular_files().into_iter().chain([parent_files.superblock]).chain([
					child_files.nodes,
					child_files.edges,
					child_files.adjacency,
					child_files.cross_level_adjacency,
					child_files.overlays,
					child_files.subgraphs,
					child_files.labels,
					child_files.superblock,
				]) {
				let space = if parent_files.regular_files().contains(&file)
					|| file == parent_files.superblock
				{
					parent_files.space()
				} else {
					child_files.space()
				};
				assert!(state.registry.contains(space, file), "CHILD_DIRECTORY_REBUILT_DATA_FILES");
			}
			assert_eq!(state.graph_superblocks.get(&parent.id()), Some(&parent_header));
			drop(state);
			let number = PageNo::new(0);
			let directory_key = PageKey::new(parent_files.space(), parent_files.subgraphs, number);
			let mut watched = parent_files
				.regular_files()
				.into_iter()
				.chain([parent_files.superblock])
				.map(|file| PageKey::new(parent_files.space(), file, number))
				.collect::<Vec<_>>();
			watched.extend(
				[child_files.nodes, child_files.edges, child_files.superblock]
					.map(|file| PageKey::new(child_files.space(), file, number)),
			);
			let mut expected = [
				directory_key,
				PageKey::new(child_files.space(), child_files.nodes, number),
				PageKey::new(child_files.space(), child_files.superblock, number),
			]
			.into_iter()
			.collect::<BTreeSet<_>>();
			if membership_kind == AtomKind::Edge {
				expected.insert(PageKey::new(child_files.space(), child_files.edges, number));
			}
			let visible = backend.wal.visible_frames(&watched, backend.wal.snapshot());
			let changed = visible
				.frames()
				.iter()
				.flatten()
				.filter(|frame| frame.frame_lsn() > boundary)
				.map(|frame| frame.key())
				.collect::<BTreeSet<_>>();
			assert_eq!(changed, expected);
			drop(visible);
			for checkpoint in [false, true] {
				if checkpoint {
					assert!(store.checkpoint(CheckpointMode::Passive).assert_ok());
					let watermark = backend.state.lock().database_superblock.checkpoint_lsn;
					assert!(watermark > home_boundary);
					assert!(watermark <= pin.0.snapshot.visible_lsn());
				}
				let mut page = AlignedPage::zeroed();
				read_pinned_page_cached(
					&backend.wal,
					&directory_file,
					&backend.engine,
					pin.0.snapshot,
					pin.0.home_generation,
					pin.0.home_ceiling,
					directory_key,
					&mut page,
				)
				.assert_ok();
				let (next, entries) = decode_subgraph_directory(&page, number).assert_ok();
				assert_eq!(next, None);
				assert_eq!(entries, [source_entry]);
				assert_parent_directory(&store, parent.id(), child.id(), container);
			}
		}
		drop(pin);
		drop(source);
		store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(store);
		store = Datastore::open_local(&directory.0, options).assert_ok();
		store.inner.maintenance.shutdown();
		assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
		assert_parent_directory(&store, parent.id(), child.id(), container);
	}
}

/// Current binding lookup also works when the fallback replaced every file.
fn assert_parent_directory(
	store: &Datastore,
	parent: GraphId,
	child: GraphId,
	container: AtomId,
) -> SubgraphEntry {
	let root = store.inner.current_root();
	let graph = root.graph(child).assert_some();
	let expected = (
		u64::try_from(graph.slot_capacity(AtomKind::Node)).assert_ok(),
		u64::try_from(graph.slot_capacity(AtomKind::Edge)).assert_ok(),
		u32::try_from(
			graph
				.occurrences()
				.filter(|entry| entry.local_id().kind() == Some(AtomKind::Node))
				.count(),
		)
		.assert_ok(),
		u32::try_from(
			graph
				.occurrences()
				.filter(|entry| entry.local_id().kind() == Some(AtomKind::Edge))
				.count(),
		)
		.assert_ok(),
	);
	let backend = local(store);
	let state = backend.state.lock();
	let binding =
		state.bindings.graphs.iter().find(|binding| binding.graph_id == parent).assert_some();
	assert_eq!(state.graph_superblocks.get(&parent).assert_some().subgraphs_pages, 1);
	let number = PageNo::new(0);
	let page = backend
		.read_update_page(
			&state,
			PageKey::new(binding.files.space(), binding.files.subgraphs, number),
		)
		.assert_ok();
	let (next, entries) = decode_subgraph_directory(&page, number).assert_ok();
	assert!(next.is_none());
	assert_eq!(entries.len(), 1);
	let entry = &entries[0];
	assert_eq!(binding.arena.graph_by_slot().get(&entry.slot), Some(&child));
	assert_eq!(binding.arena.scope_by_slot().get(&entry.slot).assert_some().as_ref(), &[container]);
	assert_eq!(
		(entry.node_capacity, entry.edge_capacity, entry.node_count, entry.edge_count),
		expected,
		"CHILD_MEMBERSHIP_LEFT_STALE_SUBGRAPH_DIRECTORY"
	);
	*entry
}
