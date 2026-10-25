//! Dense child-scope changes retain wide roles and their original physical pin.

use std::collections::BTreeSet;

use super::*;
use crate::{
	datastore::{GraphOccurrence, OccurrenceState, OccurrenceStateRef},
	graph::{EdgeEndpoint, QualifiedAtomId},
	storage::{
		graph_pages::{
			EdgeSlotFlags,
			EndpointSide,
			decode_cross_level_overflow,
			read_node_hot_slot,
		},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
	values::storage::DiskAtomRef,
};

/// Sync covers both container kinds and both directions of dense scope change.
#[test]
fn qualified_child_wide_sync_remaps_two_edges_and_retains_old_pin() {
	matrix(IoMode::Sync);
}

/// NativeAsync remains selected through WAL and home-image cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_wide_native_async_remaps_two_edges_and_retains_old_pin() {
	matrix(IoMode::NativeAsync);
}

/// Every case owns an isolated store and releases all old handles before
/// reopen.
fn matrix(mode: IoMode) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [true, false] {
			remap(mode, kind, attach);
		}
	}
}

/// Two unchanged connectors share one Hot frame and one qualified append tail.
fn remap(mode: IoMode, kind: AtomKind, attach: bool) {
	let (mut fixture, records, children) = child_scope_remap_tests::fixture(mode, kind, attach);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&records[1]).assert_some();
	let portal = root.membership(fixture.graph.id(), global).assert_some();
	let survivor = root.occurrence(fixture.graph.id(), portal).assert_some();
	let weight = match survivor.state().view() {
		OccurrenceStateRef::Edge {
			weight,
			..
		} => Some(*weight),
		OccurrenceStateRef::Node {
			..
		} => None,
	}
	.assert_some();
	let terminal = root
		.graph(children[1])
		.assert_some()
		.occurrences()
		.filter(|owner| owner.local_id().is_node())
		.map(GraphOccurrence::local_id)
		.collect::<Vec<_>>();
	assert_eq!(terminal.len(), 1);
	let terminal = terminal[0];
	let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([portal], terminal).assert_ok());
	let source = (0..2050)
		.map(|index| {
			if index % 2 == 0 {
				nested.clone()
			} else {
				EdgeEndpoint::Local(fixture.nodes[0])
			}
		})
		.collect::<Vec<_>>();
	let topology = EdgeTopology::directed(source, [nested.clone(), nested.clone(), nested]);
	drop(root);
	let spare = fixture.records[3].clone();
	let mut setup = fixture.store.begin();
	setup
		.attach_edge(
			fixture.graph.id(),
			&spare,
			None,
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	setup.commit().assert_ok();
	// A separate rewire leaves the accepted wide source in WAL, rather than
	// relying on the full-generation setup path to produce old WAL frames.
	let mut setup = fixture.store.begin();
	setup
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[1],
			OccurrenceState::edge(topology.clone(), weight, Some(children[1])),
		)
		.assert_ok();
	setup
		.replace_occurrence_state(
			fixture.graph.id(),
			&spare,
			OccurrenceState::edge(topology, Some(17_i64.into()), None),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let mut warm = fixture.store.begin();
	warm.intern_property("qualified_child_wide_pin_boundary").assert_ok();
	warm.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let occurrence = |record: &RecordId| {
		let global = root.resolve_atom(record).assert_some();
		let local = root.membership(fixture.graph.id(), global).assert_some();
		root.occurrence(fixture.graph.id(), local).assert_some().clone()
	};
	let owners = records.each_ref().map(occurrence);
	let connectors = [owners[1].clone(), occurrence(&spare)];
	assert_eq!(
		hot_slot_address(connectors[0].local_id().slot()).assert_ok().0,
		hot_slot_address(connectors[1].local_id().slot()).assert_ok().0
	);
	let source_scope = if attach {
		1
	} else {
		2
	};
	let final_scope = if attach {
		2
	} else {
		1
	};
	let keys = assert_roles(&fixture, &connectors, terminal, source_scope);
	assert_bulk_image(&fixture);
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let graph_files = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
	let capacity = header.subgraph_capacity;
	let node_capacity = header.node_capacity;
	let home = state.database_superblock.checkpoint_lsn;
	assert!(pin.0.snapshot.visible_lsn() > home);
	let append = store
		.visible_pages(&state, graph_files.space(), graph_files.cross_level_adjacency)
		.assert_ok();
	let pages = keys
		.iter()
		.map(|key| {
			(
				*key,
				state.registry.resolve(*key).assert_ok(),
				store.read_update_page(&state, *key).assert_ok(),
			)
		})
		.collect::<Vec<_>>();
	let mut untouched = Vec::new();
	for (file, count) in [
		(graph_files.nodes, header.node_hot_pages),
		(graph_files.adjacency, header.adjacency_overflow_pages),
	] {
		for number in 0..count {
			let key = PageKey::new(graph_files.space(), file, PageNo::new(number));
			untouched.push((key, store.read_update_page(&state, key).assert_ok()));
		}
	}
	drop(state);
	{
		let frames = store.wal.visible_frames(&keys, pin.0.snapshot);
		assert!(
			frames.frames().iter().all(|frame| frame.is_some_and(|frame| frame.frame_lsn() > home))
		);
	}
	child_scope_remap_tests::assert_current(&fixture, &owners, children, !attach, capacity);
	eprintln!("QUALIFIED_CHILD_WIDE_SOURCE_PASS {mode:?} {kind:?} {attach}");
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[0],
			child_presence_tests::with_child(owners[0].state(), attach.then_some(children[0])),
		)
		.assert_ok();
	write.commit().assert_ok();
	child_scope_remap_tests::assert_current(&fixture, &owners, children, attach, capacity);
	let final_keys = assert_roles(&fixture, &connectors, terminal, final_scope);
	let new_roles = final_keys
		.iter()
		.filter(|key| key.file_id() == graph_files.cross_level_adjacency)
		.map(|key| key.page_no().get())
		.collect::<BTreeSet<_>>();
	assert_eq!(new_roles, (append..append + 8).collect());
	{
		let state = store.state.lock();
		assert_eq!(
			state
				.graph_superblocks
				.get(&fixture.graph.id())
				.assert_some()
				.cross_level_adjacency_overflow_pages,
			append + 8
		);
		for (key, expected) in &untouched {
			let actual = store.read_update_page(&state, *key).assert_ok();
			if kind == AtomKind::Node && key.file_id() == graph_files.nodes {
				// Only the selected NODE may change its child metadata.
				for slot in 0..node_capacity {
					let (number, within) = hot_slot_address(slot).assert_ok();
					if number == key.page_no() && slot != owners[0].local_id().slot() {
						assert_eq!(
							read_node_hot_slot(&actual, number, within).assert_ok(),
							read_node_hot_slot(expected, number, within).assert_ok()
						);
					}
				}
			} else {
				assert_eq!(actual.as_bytes(), expected.as_bytes());
			}
		}
	}
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
	assert!(advanced < store.wal.snapshot().visible_lsn());
	// The first reads through this exact old pin occur after advancing
	// backfill.
	let mut page = AlignedPage::zeroed();
	for (key, file, expected) in &pages {
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
		assert_eq!(page.as_bytes(), expected.as_bytes());
	}
	eprintln!("QUALIFIED_CHILD_WIDE_PIN_PASS {mode:?} {kind:?} {attach}");
	drop(pages);
	drop(pin);
	drop(root);
	// Detach leaves unreachable old scope2 pages beyond the final arena.
	// Both WAL recovery and the subsequent home image must ignore those tails.
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_roles(&fixture, &connectors, terminal, final_scope);
	child_scope_remap_tests::assert_current(&fixture, &owners, children, attach, capacity);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_roles(&fixture, &connectors, terminal, final_scope);
	assert_retained(&fixture, &files, false);
	assert_bulk_image(&fixture);
	eprintln!("QUALIFIED_CHILD_WIDE_COLD_PASS {mode:?} {kind:?} {attach}");
}

/// Literal role order and dense slots are independent of the bulk planner.
fn assert_roles(
	fixture: &Fixture,
	owners: &[GraphOccurrence; 2],
	terminal: AtomId,
	scope: u32,
) -> Vec<PageKey> {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	let ceiling = state
		.graph_superblocks
		.get(&fixture.graph.id())
		.assert_some()
		.cross_level_adjacency_overflow_pages;
	let mut keys = BTreeSet::new();
	let mut role_keys = BTreeSet::new();
	for (index, owner) in owners.iter().enumerate() {
		let (number, within) = hot_slot_address(owner.local_id().slot()).assert_ok();
		let key = PageKey::new(files.space(), files.edges, number);
		keys.insert(key);
		let hot =
			read_edge_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
				.assert_ok();
		assert_eq!(hot.global_atom, Some(owner.global_id()));
		assert_eq!(hot.generation, owner.local_id().generation());
		assert!(hot.flags.contains(EdgeSlotFlags::CROSS_LEVEL));
		assert_eq!(hot.flags.contains(EdgeSlotFlags::META), index == 0);
		assert_eq!(hot.transition_subgraph_slot, (index == 0).then_some(scope));
		assert_eq!((hot.source_count, hot.target_count), (2050, 3));
		assert_eq!(hot.source_inline, [None; 2]);
		assert_eq!(hot.target_inline, [None; 2]);
		assert_eq!(hot.source_overflow, None);
		assert_eq!(hot.target_overflow, None);
		for (side, first, lengths) in [
			(EndpointSide::Source, hot.source_subgraph_overflow, &[1021, 1021, 8][..]),
			(EndpointSide::Target, hot.target_subgraph_overflow, &[3][..]),
		] {
			let mut current = first;
			let mut ordinal = 0;
			for length in lengths {
				let number = current.assert_some();
				assert!(number.get() < ceiling);
				let key = PageKey::new(files.space(), files.cross_level_adjacency, number);
				assert!(role_keys.insert(key));
				keys.insert(key);
				let (next, entries) = decode_cross_level_overflow(
					&store.read_update_page(&state, key).assert_ok(),
					number,
				)
				.assert_ok();
				assert_eq!(entries.len(), *length);
				for entry in entries {
					let nested = side == EndpointSide::Target || ordinal % 2 == 0;
					assert_eq!(entry.side, side);
					assert_eq!(
						entry.graph_slot,
						if nested {
							scope
						} else {
							0
						}
					);
					assert_eq!(
						entry.atom,
						DiskAtomRef::from_parts(
							AtomKind::Node,
							if nested {
								terminal.slot()
							} else {
								fixture.nodes[0].slot()
							}
						)
						.assert_ok()
					);
					ordinal += 1;
				}
				current = next;
			}
			assert_eq!(current, None);
		}
	}
	assert_eq!(role_keys.len(), 8);
	assert_eq!(keys.len(), 9);
	keys.into_iter().collect()
}
