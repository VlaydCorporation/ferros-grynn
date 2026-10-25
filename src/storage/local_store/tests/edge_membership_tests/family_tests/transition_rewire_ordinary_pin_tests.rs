//! Old WAL-backed EDGE roles survive shrinking topology and advancing backfill.

use super::{
	transition_rewire_tests::{assert_state, portal, rewrite},
	*,
};
use crate::{
	datastore::OccurrenceState,
	storage::{
		graph_pages::{decode_adjacency_overflow, hot_slot_address, read_edge_hot_slot},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
};

/// Two old overflow pages stay readable after the live EDGE becomes binary.
#[test]
fn transition_rewire_sync_ordinary_pin_first_read_after_passive() {
	ordinary_pin(IoMode::Sync);
}

/// NativeAsync retains the same exact old Hot slot and ordered role pages.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_ordinary_pin_first_read_after_passive() {
	ordinary_pin(IoMode::NativeAsync);
}

/// These are retained snapshot reads; the source may already be resident.
fn ordinary_pin(io_mode: IoMode) {
	let (mut fixture, _) = family_fixture(io_mode, GraphKind::Metagraph);
	let witness = metagraph::Witness::capture(&fixture);
	let (global, edge, child) = portal(&fixture);
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let sources = (0..2050).map(|index| fixture.nodes[index % 2]).collect::<Vec<_>>();
	let old_topology = EdgeTopology::directed(sources, [fixture.nodes[1]]);
	rewrite(&fixture, OccurrenceState::edge(old_topology.clone(), None, Some(child)))
		.commit()
		.assert_ok();
	let source = fixture.store.inner.current_root();
	assert_state(
		&fixture,
		global,
		edge,
		&OccurrenceState::edge(old_topology.clone(), None, Some(child)),
	);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	assert!(pin.0.snapshot.visible_lsn() > home);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let graph_files = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	let old_ceiling =
		state.graph_superblocks.get(&fixture.graph.id()).assert_some().adjacency_overflow_pages;
	let (number, within) = hot_slot_address(edge.slot()).assert_ok();
	let hot_key = PageKey::new(graph_files.space(), graph_files.edges, number);
	let hot_file = state.registry.resolve(hot_key).assert_ok();
	let page = store.read_update_page(&state, hot_key).assert_ok();
	let old_hot = read_edge_hot_slot(&page, number, within).assert_ok();
	assert_eq!(old_hot.source_count, 2050);
	let mut current = old_hot.source_overflow;
	let mut roles = Vec::new();
	for _ in 0..2 {
		let number = current.assert_some();
		assert!(number.get() < old_ceiling);
		let key = PageKey::new(graph_files.space(), graph_files.adjacency, number);
		let page = store.read_update_page(&state, key).assert_ok();
		let expected = decode_adjacency_overflow(&page, number).assert_ok();
		current = expected.0;
		roles.push((key, state.registry.resolve(key).assert_ok(), expected));
	}
	assert_eq!(current, None);
	assert_eq!(roles.iter().map(|(_, _, (_, entries))| entries.len()).sum::<usize>(), 2048);
	drop(state);
	let mut keys = vec![hot_key];
	keys.extend(roles.iter().map(|(key, _, _)| *key));
	let old_hot_lsn = {
		let frames = store.wal.visible_frames(&keys, pin.0.snapshot);
		assert!(
			frames.frames().iter().all(|frame| frame.is_some_and(|frame| frame.frame_lsn() > home))
		);
		frames.frames()[0].assert_some().frame_lsn()
	};
	let final_topology = EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]);
	rewrite(&fixture, OccurrenceState::edge(final_topology.clone(), None, Some(child)))
		.commit()
		.assert_ok();
	assert_state(
		&fixture,
		global,
		edge,
		&OccurrenceState::edge(final_topology.clone(), None, Some(child)),
	);
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
	{
		let frames = store.wal.visible_frames(&[hot_key], store.wal.snapshot());
		assert!(frames.frames()[0].assert_some().frame_lsn() > old_hot_lsn);
	}
	// Re-expansion must append beyond the entire source prefix retained by the
	// old pin.
	rewrite(&fixture, OccurrenceState::edge(old_topology, None, Some(child))).commit().assert_ok();
	{
		let state = store.state.lock();
		let page = store.read_update_page(&state, hot_key).assert_ok();
		let hot = read_edge_hot_slot(&page, number, within).assert_ok();
		assert!(hot.source_overflow.assert_some().get() >= old_ceiling);
	}
	rewrite(&fixture, OccurrenceState::edge(final_topology.clone(), None, Some(child)))
		.commit()
		.assert_ok();
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
	// These are the first reads through this exact original pin, after
	// advancing backfill.
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
		assert_eq!(&decode_adjacency_overflow(&page, key.page_no()).assert_ok(), expected);
	}
	drop(hot_file);
	drop(roles);
	drop(pin);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, io_mode);
	assert_state(&fixture, global, edge, &OccurrenceState::edge(final_topology, None, Some(child)));
	assert_retained(&fixture, &files, false);
	witness.assert_structure(&fixture);
	assert_bulk_image(&fixture);
}
