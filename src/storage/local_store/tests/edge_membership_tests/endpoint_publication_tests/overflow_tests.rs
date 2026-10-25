//! WAL-backed local EDGE endpoint roles survive overflow shrink and backfill.

use super::{super::rewire_tests::assert_topology, *};
use crate::storage::{
	graph_pages::{decode_adjacency_overflow, hot_slot_address, read_edge_hot_slot},
	image_io::read_pinned_page_cached,
	page::AlignedPage,
};

/// Two old overflow pages stay readable after the live EDGE becomes binary.
#[test]
fn endpoint_selected_sync_wal_overflow_pin_survives_shrink_and_passive() {
	wal_overflow_pin(IoMode::Sync);
}

/// NativeAsync retains the same exact old Hot slot and ordered role pages.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_wal_overflow_pin_survives_shrink_and_passive() {
	wal_overflow_pin(IoMode::NativeAsync);
}

/// These are retained snapshot reads; the source may already be resident.
fn wal_overflow_pin(io_mode: IoMode) {
	let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let sources =
		(0..2050).map(|index| [endpoint, fixture.nodes[0]][index % 2]).collect::<Vec<_>>();
	let old_topology = EdgeTopology::directed(sources.clone(), [fixture.nodes[1]]);
	let mut write = fixture.store.begin();
	let edge = write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			old_topology.clone(),
			None,
			None,
		)
		.assert_ok();
	write.commit().assert_ok();
	assert_bulk_image(&fixture);
	let raw = |edge: AtomId| DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok().raw();
	let rows = [(0, raw(endpoint), raw(connector), 0), (0, raw(endpoint), raw(edge), 0)];
	assert_postings(&fixture, &rows);
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&fixture.records[2]).assert_some();
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
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let endpoint_key = PageKey::new(graph_files.space(), binding.file_id, binding.root);
	let endpoint_file = state.registry.resolve(endpoint_key).assert_ok();
	let capability =
		source.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().assert_some();
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
	let physical_sources = old_hot
		.source_inline
		.into_iter()
		.flatten()
		.chain(roles.iter().flat_map(|(_, _, (_, entries))| entries.iter().copied()))
		.collect::<Vec<_>>();
	let literal_sources = sources
		.iter()
		.map(|source| {
			DiskAtomRef::from_parts(source.kind().assert_some(), source.slot()).assert_ok()
		})
		.collect::<Vec<_>>();
	assert_eq!(physical_sources, literal_sources);
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
	let final_topology = EdgeTopology::directed([endpoint], [fixture.nodes[0]]);
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[2],
			OccurrenceState::edge(final_topology.clone(), None, None),
		)
		.assert_ok();
	write.commit().assert_ok();
	assert_eq!(
		store
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.edge_endpoint,
		Some(binding)
	);
	assert_postings(&fixture, &rows);
	assert_walk(&fixture, endpoint, &[(connector, fixture.nodes[1]), (edge, fixture.nodes[0])]);
	assert_topology(&fixture, global, edge, &final_topology);
	assert_data_files(&fixture, &files);
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
		let old_rows = postings(
			|number, page| {
				read_pinned_page_cached(
					&store.wal,
					&endpoint_file,
					&store.engine,
					pin.0.snapshot,
					pin.0.home_generation,
					pin.0.home_ceiling,
					PageKey::new(graph_files.space(), binding.file_id, number),
					page,
				)
			},
			fixture.graph.id(),
			capability,
			source.commit_ts,
			endpoint_file.page_count(),
		);
		assert_eq!(old_rows, rows);
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
	drop(endpoint_file);
	drop(hot_file);
	drop(roles);
	drop(pin);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_topology(&fixture, global, edge, &final_topology);
	assert_postings(&fixture, &rows);
	assert_walk(&fixture, endpoint, &[(connector, fixture.nodes[1]), (edge, fixture.nodes[0])]);
	assert_data_files(&fixture, &files);
	assert_bulk_image(&fixture);
}
