//! Three-page qualified roles remain readable through the exact original
//! snapshot.

use super::{
	qualified_guard_tests::{short_topology, wide_topology},
	transition_rewire_tests::{assert_state, portal, rewrite},
	*,
};
use crate::{
	datastore::OccurrenceState,
	storage::{
		graph_pages::{
			EndpointSide,
			decode_cross_level_overflow,
			hot_slot_address,
			read_edge_hot_slot,
		},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
};

/// Sync first reads the old pin after qualified shrink and advancing Passive.
#[test]
fn transition_rewire_sync_multipage_pin_survives_shrink_and_passive() {
	multipage_pin(IoMode::Sync);
}

/// NativeAsync keeps the original header and file handle across append reuse.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_multipage_pin_survives_shrink_and_passive() {
	multipage_pin(IoMode::NativeAsync);
}

/// This proves old-version retention; current-source reads may warm shared
/// caches.
fn multipage_pin(mode: IoMode) {
	let (mut fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let witness = metagraph::Witness::capture(&fixture);
	let (global, edge, child) = portal(&fixture);
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let topology = wide_topology(&fixture);
	rewrite(&fixture, OccurrenceState::edge(topology.clone(), None, Some(child)))
		.commit()
		.assert_ok();
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	let source = fixture.store.inner.current_root();
	assert_state(
		&fixture,
		global,
		edge,
		&OccurrenceState::edge(topology.clone(), None, Some(child)),
	);
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	assert!(pin.0.snapshot.visible_lsn() > home);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let graph_files = binding.files;
	let old_ceiling = state
		.graph_superblocks
		.get(&fixture.graph.id())
		.assert_some()
		.cross_level_adjacency_overflow_pages;
	let scope = *binding
		.arena
		.scope_by_slot()
		.iter()
		.find(|(_, path)| path.len() == 1 && path[0].kind() == Some(AtomKind::Node))
		.assert_some()
		.0;
	assert_ne!(scope, 0);
	let (number, within) = hot_slot_address(edge.slot()).assert_ok();
	let hot_key = PageKey::new(graph_files.space(), graph_files.edges, number);
	let hot_file = state.registry.resolve(hot_key).assert_ok();
	let page = store.read_update_page(&state, hot_key).assert_ok();
	let old_hot = read_edge_hot_slot(&page, number, within).assert_ok();
	assert_eq!(old_hot.source_count, 2050);
	assert_eq!(old_hot.source_inline, [None; 2]);
	let mut current = old_hot.source_subgraph_overflow;
	let mut roles = Vec::new();
	for _ in 0..3 {
		let number = current.assert_some();
		assert!(number.get() < old_ceiling);
		let key = PageKey::new(graph_files.space(), graph_files.cross_level_adjacency, number);
		let page = store.read_update_page(&state, key).assert_ok();
		let expected = decode_cross_level_overflow(&page, number).assert_ok();
		current = expected.0;
		roles.push((key, state.registry.resolve(key).assert_ok(), expected));
	}
	assert_eq!(current, None);
	let entries = roles.iter().flat_map(|(_, _, (_, entries))| entries).collect::<Vec<_>>();
	assert_eq!(entries.len(), 2050);
	for (index, entry) in entries.iter().enumerate() {
		assert_eq!(entry.side, EndpointSide::Source);
		assert_eq!(
			entry.graph_slot,
			if index % 2 == 0 {
				scope
			} else {
				0
			}
		);
		assert_eq!(entry.atom.kind(), AtomKind::Node);
		assert_eq!(
			entry.atom.slot(),
			if index % 2 == 0 {
				0
			} else {
				fixture.nodes[0].slot()
			}
		);
	}
	drop(entries);
	drop(state);
	let mut keys = vec![hot_key];
	keys.extend(roles.iter().map(|(key, _, _)| *key));
	{
		let frames = store.wal.visible_frames(&keys, pin.0.snapshot);
		assert!(
			frames.frames().iter().all(|frame| frame.is_some_and(|frame| frame.frame_lsn() > home))
		);
	}
	rewrite(&fixture, OccurrenceState::edge(short_topology(&fixture), None, Some(child)))
		.commit()
		.assert_ok();
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	// A new wide role must append after every page retained by the old header.
	rewrite(&fixture, OccurrenceState::edge(topology, None, Some(child))).commit().assert_ok();
	{
		let state = store.state.lock();
		let page = store.read_update_page(&state, hot_key).assert_ok();
		let hot = read_edge_hot_slot(&page, number, within).assert_ok();
		assert!(hot.source_subgraph_overflow.assert_some().get() >= old_ceiling);
		assert_eq!(
			state
				.graph_superblocks
				.get(&fixture.graph.id())
				.assert_some()
				.cross_level_adjacency_overflow_pages,
			old_ceiling + 3
		);
	}
	rewrite(&fixture, OccurrenceState::edge(short_topology(&fixture), None, Some(child)))
		.commit()
		.assert_ok();
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
	// First reads through this pin occur after backfill; no newer header is
	// used.
	let mut page = AlignedPage::zeroed();
	read_pinned_page_cached(
		&store.wal,
		&hot_file,
		&store.engine,
		pin.0.snapshot,
		pin.0.home_generation,
		pin.0.home_ceiling,
		hot_key,
		&mut page,
	)
	.assert_ok();
	assert_eq!(read_edge_hot_slot(&page, number, within).assert_ok(), old_hot);
	for (key, file, expected) in &roles {
		assert!(key.page_no().get() < old_ceiling);
		read_pinned_page_cached(
			&store.wal,
			file,
			&store.engine,
			pin.0.snapshot,
			pin.0.home_generation,
			pin.0.home_ceiling,
			*key,
			&mut page,
		)
		.assert_ok();
		assert_eq!(&decode_cross_level_overflow(&page, key.page_no()).assert_ok(), expected);
	}
	drop(roles);
	drop(hot_file);
	drop(pin);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	let root = fixture.store.inner.current_root();
	assert_eq!(
		root.occurrence(fixture.graph.id(), edge).assert_some().state(),
		&OccurrenceState::edge(short_topology(&fixture), None, Some(child))
	);
}
