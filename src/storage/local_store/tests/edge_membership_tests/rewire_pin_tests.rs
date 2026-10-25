//! Old WAL-backed EDGE roles survive shrinking topology and advancing backfill.

use super::{
	rewire_tests::{assert_retained, assert_topology, write_topology},
	support::*,
	*,
};
use crate::storage::{
	graph_pages::{decode_adjacency_overflow, hot_slot_address, read_edge_hot_slot},
	image_io::read_pinned_page_cached,
	page::AlignedPage,
};

/// Two old overflow pages stay readable after the live EDGE becomes binary.
#[test]
fn edge_rewire_sync_wal_overflow_pin_survives_shrink_and_passive() {
	wal_overflow_pin(IoMode::Sync);
}

/// NativeAsync retains the same exact old Hot slot and ordered role pages.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_wal_overflow_pin_survives_shrink_and_passive() {
	wal_overflow_pin(IoMode::NativeAsync);
}

/// These are retained snapshot reads; the source may already be resident.
fn wal_overflow_pin(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 1, 1);
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let sources = (0..2050).map(|index| fixture.nodes[index % 2]).collect::<Vec<_>>();
	let old_topology = EdgeTopology::directed(sources, [fixture.nodes[1]]);
	write_topology(&fixture, old_topology.clone()).commit().assert_ok();
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&fixture.records[0]).assert_some();
	let edge = source.membership(fixture.graph.id(), global).assert_some();
	assert_topology(&fixture, global, edge, &old_topology);
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
	write_topology(&fixture, final_topology.clone()).commit().assert_ok();
	assert_topology(&fixture, global, edge, &final_topology);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	{
		let frames = store.wal.visible_frames(&[hot_key], store.wal.snapshot());
		assert!(frames.frames()[0].assert_some().frame_lsn() > old_hot_lsn);
	}
	for checkpoint in [false, true] {
		if checkpoint {
			assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
			let advanced = store.state.lock().database_superblock.checkpoint_lsn;
			assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
		}
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
	}
	drop(hot_file);
	drop(roles);
	drop(pin);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_topology(&fixture, global, edge, &final_topology);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
}
