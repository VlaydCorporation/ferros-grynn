//! Direct child aliases cross a directory page without rotating data files.

use std::collections::BTreeMap;

use super::*;
use crate::{
	datastore::{GlobalAtomId, GraphOccurrence, OccurrenceState},
	graph::GraphId,
	storage::{
		graph_pages::{
			EdgeSlotFlags,
			GraphArenaBindings,
			NodeSlotFlags,
			SUBGRAPHS_PER_PAGE,
			SubgraphEntry,
			decode_subgraph_directory,
			read_node_hot_slot,
		},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
};

/// Sync growth and shrink retain the allocated empty tail across WAL recovery.
#[test]
fn child_scope_directory_sync_crosses_and_retains_empty_tail() {
	crossing(IoMode::Sync);
}

/// NativeAsync is checked after creation and each no-checkpoint reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_directory_native_async_crosses_and_retains_empty_tail() {
	crossing(IoMode::NativeAsync);
}

/// Fixture order is literal: selected NODE, fresh NODE aliases, EDGE portal.
fn direct(
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	extras: &[GraphOccurrence],
	present: bool,
) -> Vec<(AtomId, GlobalAtomId, GraphId)> {
	let mut entries = Vec::new();
	if present {
		entries.push((owners[0].local_id(), owners[0].global_id(), children[0]));
	}
	entries.extend(extras.iter().map(|owner| (owner.local_id(), owner.global_id(), children[0])));
	entries.push((owners[1].local_id(), owners[1].global_id(), children[1]));
	entries
}

/// Literal ordered owners define maps independently of the canonical builder.
fn expected_arena(
	graph: GraphId,
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	extras: &[GraphOccurrence],
	present: bool,
) -> GraphArenaBindings {
	let mut graphs = BTreeMap::from([(0, graph)]);
	let mut scopes = BTreeMap::from([(0, Box::<[AtomId]>::default())]);
	for (index, (owner, _, child)) in
		direct(owners, children, extras, present).into_iter().enumerate()
	{
		let slot = u32::try_from(index + 1).assert_ok();
		graphs.insert(slot, child);
		scopes.insert(slot, vec![owner].into_boxed_slice());
	}
	GraphArenaBindings::new(graphs, scopes)
}

/// Literal slots and unchanged leaf counters define both current and old pages.
fn expected_entries(
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	extras: &[GraphOccurrence],
	present: bool,
) -> Vec<SubgraphEntry> {
	direct(owners, children, extras, present)
		.into_iter()
		.enumerate()
		.map(|(index, (owner, _, _))| SubgraphEntry {
			slot: u32::try_from(index + 1).assert_ok(),
			generation: owner.generation(),
			node_capacity: 1,
			edge_capacity: 0,
			node_count: 1,
			edge_count: 0,
		})
		.collect()
}

/// Every live entry and all retained empty-page links are checked literally.
fn assert_crossing(
	fixture: &Fixture,
	owners: &[GraphOccurrence; 2],
	children: [GraphId; 2],
	extras: &[GraphOccurrence],
	present: bool,
	pages: u64,
) {
	let root = fixture.store.inner.current_root();
	let selected = root.occurrence(fixture.graph.id(), owners[0].local_id()).assert_some();
	assert_eq!(selected.global_id(), owners[0].global_id());
	assert_eq!(selected.label(), owners[0].label());
	assert_eq!(selected.overlay(), owners[0].overlay());
	assert_eq!(selected.state(), &OccurrenceState::node(present.then_some(children[0])));
	for owner in extras.iter().chain(std::iter::once(&owners[1])) {
		assert_eq!(root.occurrence(fixture.graph.id(), owner.local_id()), Some(owner));
		assert_eq!(root.membership(fixture.graph.id(), owner.global_id()), Some(owner.local_id()));
	}
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == fixture.graph.id())
		.assert_some();
	assert_eq!(
		binding.arena,
		expected_arena(fixture.graph.id(), owners, children, extras, present)
	);
	assert!(
		binding.arena.same_owner(
			&state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == fixture.graph.id())
				.assert_some()
				.arena
		)
	);
	let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
	assert_eq!(header.subgraphs_pages, pages);
	assert_eq!(
		header.subgraph_count,
		u64::try_from(SUBGRAPHS_PER_PAGE + usize::from(present)).assert_ok()
	);
	assert_eq!(
		header.subgraph_capacity,
		u64::try_from(SUBGRAPHS_PER_PAGE + usize::from(pages == 2)).assert_ok()
	);
	let (number, within) = hot_slot_address(owners[0].local_id().slot()).assert_ok();
	let key = PageKey::new(binding.files.space(), binding.files.nodes, number);
	let selected =
		read_node_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
			.assert_ok();
	assert_eq!(selected.generation, owners[0].local_id().generation());
	assert_eq!(selected.global_atom, Some(owners[0].global_id()));
	assert_eq!(selected.flags.contains(NodeSlotFlags::META), present);
	assert_eq!(selected.subgraph_slot, present.then_some(1));
	let entries = expected_entries(owners, children, extras, present);
	for (index, (owner, global, _)) in
		direct(owners, children, extras, present).into_iter().enumerate()
	{
		let slot = u32::try_from(index + 1).assert_ok();
		let (number, within) = hot_slot_address(owner.slot()).assert_ok();
		let file = if owner.is_node() {
			binding.files.nodes
		} else {
			binding.files.edges
		};
		let page = store
			.read_update_page(&state, PageKey::new(binding.files.space(), file, number))
			.assert_ok();
		if owner.is_node() {
			let hot = read_node_hot_slot(&page, number, within).assert_ok();
			assert_eq!((hot.generation, hot.global_atom), (owner.generation(), Some(global)));
			assert!(hot.flags.contains(NodeSlotFlags::META));
			assert_eq!(hot.subgraph_slot, Some(slot));
		} else {
			let hot = read_edge_hot_slot(&page, number, within).assert_ok();
			assert_eq!((hot.generation, hot.global_atom), (owner.generation(), Some(global)));
			assert!(hot.flags.contains(EdgeSlotFlags::META));
			assert_eq!(hot.transition_subgraph_slot, Some(slot));
		}
	}
	for ordinal in 0..pages {
		let number = PageNo::new(ordinal);
		let next = (ordinal + 1 < pages).then(|| PageNo::new(ordinal + 1));
		let begin = (usize::try_from(ordinal).assert_ok() * SUBGRAPHS_PER_PAGE).min(entries.len());
		let end = (begin + SUBGRAPHS_PER_PAGE).min(entries.len());
		let page = store
			.read_update_page(
				&state,
				PageKey::new(binding.files.space(), binding.files.subgraphs, number),
			)
			.assert_ok();
		assert_eq!(
			decode_subgraph_directory(&page, number).assert_ok(),
			(next, entries[begin..end].to_vec())
		);
	}
}

/// Growth511 then shrink510 keeps two pages, complete old pins and exact files.
fn crossing(mode: IoMode) {
	let (mut fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let records =
		["node:container", "edge:portal"].map(|name| name.parse::<RecordId>().assert_ok());
	let root = fixture.store.inner.current_root();
	let owners = records.each_ref().map(|record| {
		let global = root.resolve_atom(record).assert_some();
		let local = root.membership(fixture.graph.id(), global).assert_some();
		root.occurrence(fixture.graph.id(), local).assert_some().clone()
	});
	let children = owners.each_ref().map(|owner| owner.child_graph().assert_some());
	drop(root);
	let extra_records = (0..SUBGRAPHS_PER_PAGE - 1)
		.map(|index| format!("node:directory_{index}").parse::<RecordId>().assert_ok())
		.collect::<Vec<_>>();
	let mut setup = fixture.store.begin();
	setup
		.replace_occurrence_state(fixture.graph.id(), &records[0], OccurrenceState::node(None))
		.assert_ok();
	for record in &extra_records {
		let mut properties = PropertyMap::new();
		properties.insert(key, 0_i64);
		setup.create_atom(record.clone(), properties).assert_ok();
		setup.attach_node(fixture.graph.id(), record, None, Some(children[0])).assert_ok();
	}
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let mut extras = extra_records
		.iter()
		.map(|record| {
			let global = root.resolve_atom(record).assert_some();
			let owner = root.membership(fixture.graph.id(), global).assert_some();
			root.occurrence(fixture.graph.id(), owner).assert_some().clone()
		})
		.collect::<Vec<_>>();
	extras.sort_unstable_by_key(|owner| owner.local_id().raw());
	assert!(extras.windows(2).all(|pair| pair[0].local_id().raw() < pair[1].local_id().raw()));
	assert!(owners[0].local_id().raw() < extras.first().assert_some().local_id().raw());
	assert!(extras.last().assert_some().local_id().raw() < owners[1].local_id().raw());
	drop(root);
	assert_crossing(&fixture, &owners, children, &extras, false, 1);
	assert_bulk_image(&fixture);
	for present in [true, false] {
		let mut warm = fixture.store.begin();
		warm.intern_property(&format!("directory_remap_pin_{present}")).assert_ok();
		warm.commit().assert_ok();
		let store = local(&fixture.store);
		let state = store.state.lock();
		let home = state.database_superblock.checkpoint_lsn;
		let boundary = store.wal.snapshot().visible_lsn();
		assert!(boundary > home);
		let files = state.file_catalog.files.clone();
		let old_arena = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.arena
			.clone();
		drop(state);
		let mut old_read = fixture.store.begin();
		let old_snapshot = old_read.query_snapshot().assert_ok();
		// Capture trusted handles without reading directory pages before
		// backfill.
		let directory_pin = store
			.capture_query_snapshot(fixture.store.inner.current_root().commit_ts)
			.assert_ok()
			.assert_some();
		assert_eq!(directory_pin.0.snapshot.visible_lsn(), boundary);
		let old_pages = if present {
			1
		} else {
			2
		};
		let directory_sources = {
			let state = store.state.lock();
			let binding = state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == fixture.graph.id())
				.assert_some();
			(0..old_pages)
				.map(|ordinal| {
					let key = PageKey::new(
						binding.files.space(),
						binding.files.subgraphs,
						PageNo::new(ordinal),
					);
					(key, state.registry.resolve(key).assert_ok())
				})
				.collect::<Vec<_>>()
		};
		let old_entries = expected_entries(&owners, children, &extras, !present);
		assert_eq!(old_entries.len(), SUBGRAPHS_PER_PAGE + usize::from(!present));
		let mut write = fixture.store.begin();
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				&records[0],
				OccurrenceState::node(present.then_some(children[0])),
			)
			.assert_ok();
		write.commit().assert_ok();
		assert_crossing(&fixture, &owners, children, &extras, present, 2);
		assert_bulk_image(&fixture);
		assert_retained(&fixture, &files, false);
		assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
		let advanced = store.state.lock().database_superblock.checkpoint_lsn;
		assert!(
			advanced > home
				&& advanced <= boundary
				&& advanced < store.wal.snapshot().visible_lsn()
		);
		// The first old-directory read occurs after Passive advances home
		// authority.
		for (ordinal, (key, file)) in directory_sources.iter().enumerate() {
			let mut page = AlignedPage::zeroed();
			read_pinned_page_cached(
				&store.wal,
				file,
				&store.engine,
				directory_pin.0.snapshot,
				directory_pin.0.home_generation,
				directory_pin.0.home_ceiling,
				*key,
				&mut page,
			)
			.assert_ok();
			let begin = (ordinal * SUBGRAPHS_PER_PAGE).min(old_entries.len());
			let end = (begin + SUBGRAPHS_PER_PAGE).min(old_entries.len());
			let next = (ordinal + 1 < directory_sources.len())
				.then(|| PageNo::new(u64::try_from(ordinal + 1).assert_ok()));
			assert_eq!(
				decode_subgraph_directory(&page, key.page_no()).assert_ok(),
				(next, old_entries[begin..end].to_vec())
			);
		}
		for (owner, expected) in [
			(&owners[0], (!present).then_some(children[0])),
			(&owners[1], Some(children[1])),
			(extras.last().assert_some(), Some(children[0])),
		] {
			assert_eq!(
				old_read
					.query_physical_inner_graph(&old_snapshot, &fixture.graph, owner.local_id())
					.assert_ok()
					.assert_some()
					.graph,
				expected
			);
		}
		assert_eq!(
			old_arena,
			expected_arena(fixture.graph.id(), &owners, children, &extras, !present)
		);
		drop(directory_sources);
		drop(directory_pin);
		drop(old_snapshot);
		drop(old_read);
		fixture = reopen(fixture);
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		assert_crossing(&fixture, &owners, children, &extras, present, 2);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
		eprintln!("CHILD_SCOPE_DIRECTORY_PHASE_PASS {mode:?} {present}");
	}
}
