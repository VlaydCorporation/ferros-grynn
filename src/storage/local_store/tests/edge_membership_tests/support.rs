//! A full bulk image is the independent oracle for sparse incidence
//! publication.

use super::*;
use crate::{
	storage::{
		atom_page::PropertyRef,
		graph_pages::{
			EdgeHotSlot,
			EdgeSlotFlags,
			EndpointSide,
			QualifiedDiskAtomRef,
			build_graph_image,
			decode_adjacency_overflow,
			decode_cross_level_overflow,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
		},
		local_store::LocalStore,
		page::{PageLsn, PageNo},
		types::PageKey,
	},
	values::storage::{DiskAtomRef, EncodeContext},
};

/// Owns a complete source while keeping its directory alive through reopen.
pub(super) struct Fixture {
	/// Replaced explicitly for cold recovery, before its directory is dropped.
	pub(super) store: Datastore,
	/// The controlling I/O mode is asserted after every create/open.
	pub(super) options: LocalOptions,
	/// Existing standalone graph contains only fixed Base NODE owners.
	pub(super) graph: GraphRef,
	/// Endpoint local handles remain stable across EDGE-only mutations.
	pub(super) nodes: [AtomId; 2],
	/// All Base edge identities exist before the measured publication slice.
	pub(super) records: Vec<RecordId>,
	/// Outlives every datastore and physical snapshot in the fixture.
	pub(super) directory: TestDirectory,
}

/// Builds a full source, then starts the candidate from a cold physical root.
pub(super) fn fixture(io_mode: IoMode, edges: usize, attached: usize) -> Fixture {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("node", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let nodes = ["node:a", "node:b"].map(|name| {
		let record = name.parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record, None, None).assert_ok()
	});
	let records = (0..edges)
		.map(|index| format!("edge:e{index}").parse::<RecordId>().assert_ok())
		.collect::<Vec<_>>();
	for (index, record) in records.iter().enumerate() {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		if index < attached {
			seed.attach_edge(
				graph.id(),
				record,
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		}
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	reopen(Fixture {
		store,
		options,
		graph,
		nodes,
		records,
		directory,
	})
}

/// Reopens with identical settings and stops unrelated maintenance work.
pub(super) fn reopen(mut fixture: Fixture) -> Fixture {
	drop(fixture.store);
	fixture.store = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
	assert_eq!(
		fixture.store.local_storage_status().assert_some().options().io_mode,
		fixture.options.io_mode
	);
	fixture.store.inner.maintenance.shutdown();
	fixture
}

/// Accesses the exact local backend without accepting a materialized fallback.
pub(super) fn local(store: &Datastore) -> &LocalStore {
	match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some()
}

/// Prepares the first previously unattached plain edge with stable endpoints.
pub(super) fn candidate(fixture: &Fixture, index: usize) -> Transaction {
	let mut write = fixture.store.begin();
	write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[index],
			None,
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	write
}

/// Checks complete Hot records and role vectors against the existing bulk
/// writer.
pub(super) fn assert_bulk_image(fixture: &Fixture) {
	assert_bulk_records(fixture, &[]);
}

/// Keeps full Hot/topology checks after the caller decoded this NODE overlay.
pub(super) fn assert_bulk_image_with_overlay(fixture: &Fixture, owner: AtomId) {
	assert_eq!(owner.kind(), Some(AtomKind::Node));
	assert_bulk_records(fixture, &[owner]);
}

/// Normalizes addresses only after the caller decoded each listed payload.
pub(super) fn assert_bulk_image_with_overlays(fixture: &Fixture, owners: &[AtomId]) {
	assert_bulk_records(fixture, owners);
}

/// Only independently decoded owners may have different append addresses.
fn assert_bulk_records(fixture: &Fixture, overlays: &[AtomId]) {
	let root = fixture.store.inner.current_root();
	let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
	let expected =
		build_graph_image(&root, fixture.graph.id(), &mut context, PageLsn::ZERO).assert_ok();
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some();
	let files = binding.files;
	assert_eq!(binding.arena, expected.arena);
	let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
	assert_eq!(
		(header.node_capacity, header.edge_capacity, header.node_count, header.edge_count),
		(
			expected.superblock.node_capacity,
			expected.superblock.edge_capacity,
			expected.superblock.node_count,
			expected.superblock.edge_count
		)
	);
	let actual_page = |number: PageNo| {
		assert!(number.get() < header.adjacency_overflow_pages);
		let page = store
			.read_update_page(&state, PageKey::new(files.space(), files.adjacency, number))
			.assert_ok();
		decode_adjacency_overflow(&page, number).assert_ok()
	};
	let control_page = |number: PageNo| {
		decode_adjacency_overflow(
			&expected.adjacency[usize::try_from(number.get()).assert_ok()],
			number,
		)
		.assert_ok()
	};
	let actual_qualified = |number: PageNo| {
		assert!(number.get() < header.cross_level_adjacency_overflow_pages);
		let page = store
			.read_update_page(
				&state,
				PageKey::new(files.space(), files.cross_level_adjacency, number),
			)
			.assert_ok();
		decode_cross_level_overflow(&page, number).assert_ok()
	};
	let control_qualified = |number: PageNo| {
		assert!(number.get() < expected.superblock.cross_level_adjacency_overflow_pages);
		decode_cross_level_overflow(
			&expected.cross_level_adjacency[usize::try_from(number.get()).assert_ok()],
			number,
		)
		.assert_ok()
	};
	for slot in 0..header.node_capacity {
		let (number, within) = hot_slot_address(slot).assert_ok();
		let page = store
			.read_update_page(&state, PageKey::new(files.space(), files.nodes, number))
			.assert_ok();
		let actual = read_node_hot_slot(&page, number, within).assert_ok();
		let mut control = read_node_hot_slot(
			&expected.nodes[usize::try_from(number.get()).assert_ok()],
			number,
			within,
		)
		.assert_ok();
		for (actual, control) in [
			(
				(actual.out_count, actual.out_inline, actual.out_overflow),
				(control.out_count, control.out_inline, control.out_overflow),
			),
			(
				(actual.in_count, actual.in_inline, actual.in_overflow),
				(control.in_count, control.in_inline, control.in_overflow),
			),
		] {
			assert_eq!(side(actual, actual_page), side(control, control_page));
		}
		assert_eq!(
			side(
				(actual.undirected_count, actual.undirected_inline, actual.undirected_overflow),
				actual_page
			),
			side(
				(control.undirected_count, control.undirected_inline, control.undirected_overflow),
				control_page
			)
		);
		control.out_overflow = actual.out_overflow;
		control.in_overflow = actual.in_overflow;
		control.undirected_overflow = actual.undirected_overflow;
		if let Some(owner) = overlays
			.iter()
			.find(|owner| owner.kind() == Some(AtomKind::Node) && owner.slot() == slot)
		{
			assert_eq!(control.generation, owner.generation());
			assert_ne!(control.overlay, PropertyRef::None);
			assert_ne!(actual.overlay, PropertyRef::None);
			// The append-only publisher and fresh bulk image may choose
			// different offsets.
			control.overlay = actual.overlay;
		}
		assert_eq!(actual, control);
	}
	for slot in 0..header.edge_capacity {
		let (number, within) = hot_slot_address(slot).assert_ok();
		let page = store
			.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
			.assert_ok();
		let actual = read_edge_hot_slot(&page, number, within).assert_ok();
		let mut control = read_edge_hot_slot(
			&expected.edges[usize::try_from(number.get()).assert_ok()],
			number,
			within,
		)
		.assert_ok();
		for role in [EndpointSide::Source, EndpointSide::Target] {
			assert_eq!(
				edge_role(&actual, role, &actual_page, &actual_qualified),
				edge_role(&control, role, &control_page, &control_qualified)
			);
		}
		for (actual, control) in [
			(actual.source_overflow, &mut control.source_overflow),
			(actual.target_overflow, &mut control.target_overflow),
			(actual.source_subgraph_overflow, &mut control.source_subgraph_overflow),
			(actual.target_subgraph_overflow, &mut control.target_subgraph_overflow),
		] {
			assert_eq!(actual.is_some(), control.is_some());
			*control = actual;
		}
		if let Some(owner) = overlays
			.iter()
			.find(|owner| owner.kind() == Some(AtomKind::Edge) && owner.slot() == slot)
		{
			assert_eq!(control.generation, owner.generation());
			assert_ne!(control.overlay, PropertyRef::None);
			assert_ne!(actual.overlay, PropertyRef::None);
			control.overlay = actual.overlay;
		}
		assert_eq!(actual, control);
	}
}

/// Expands only trusted fixture chains; the independent bulk writer is
/// authority.
fn side<const N: usize>(
	source: (u32, [Option<DiskAtomRef>; N], Option<PageNo>),
	mut read: impl FnMut(PageNo) -> (Option<PageNo>, Vec<DiskAtomRef>),
) -> Vec<DiskAtomRef> {
	let (count, inline, mut head) = source;
	let mut entries = inline.into_iter().flatten().collect::<Vec<_>>();
	let mut seen = std::collections::BTreeSet::new();
	while let Some(number) = head {
		assert!(seen.insert(number), "fixture chain contains a cycle");
		let (next, tail) = read(number);
		entries.extend(tail);
		head = next;
	}
	assert_eq!(entries.len(), usize::try_from(count).assert_ok());
	entries
}

/// Decodes persisted role addressing without using the production layout
/// planner.
///
/// # Panics
/// Panics when a fixture contains inconsistent counts, scopes, chains or side
/// tags.
fn edge_role(
	hot: &EdgeHotSlot,
	role: EndpointSide,
	read_local: impl FnMut(PageNo) -> (Option<PageNo>, Vec<DiskAtomRef>),
	mut read_qualified: impl FnMut(PageNo) -> (Option<PageNo>, Vec<QualifiedDiskAtomRef>),
) -> Vec<QualifiedDiskAtomRef> {
	let (count, inline, local, qualified) = match role {
		EndpointSide::Source => {
			(hot.source_count, hot.source_inline, hot.source_overflow, hot.source_subgraph_overflow)
		}
		EndpointSide::Target => {
			(hot.target_count, hot.target_inline, hot.target_overflow, hot.target_subgraph_overflow)
		}
	};
	let graph_slot = hot.endpoint_graph_slot(role);
	if !hot.flags.contains(EdgeSlotFlags::CROSS_LEVEL) {
		assert_eq!(graph_slot, None);
		assert_eq!(qualified, None);
		return side((count, inline, local), read_local)
			.into_iter()
			.map(|atom| QualifiedDiskAtomRef {
				graph_slot: 0,
				side: role,
				atom,
			})
			.collect();
	}
	assert_eq!(local, None);
	let count = usize::try_from(count).assert_ok();
	let mut entries = Vec::new();
	if let Some(graph_slot) = graph_slot {
		assert_eq!(qualified, None);
		entries.extend(inline.into_iter().flatten().map(|atom| QualifiedDiskAtomRef {
			graph_slot,
			side: role,
			atom,
		}));
	} else {
		assert_eq!(inline, [None; 2]);
		let mut visited = std::collections::BTreeSet::new();
		let mut current = qualified;
		while let Some(number) = current {
			assert!(visited.insert(number), "fixture qualified chain contains a cycle");
			assert!(entries.len() < count, "fixture qualified chain exceeds its count");
			let (next, tail) = read_qualified(number);
			assert!(!tail.is_empty(), "fixture qualified chain contains an empty page");
			assert!(tail.len() <= count - entries.len());
			assert!(
				tail.iter().all(|entry| entry.side == role),
				"fixture qualified chain has the wrong side"
			);
			entries.extend(tail);
			current = next;
		}
	}
	assert_eq!(entries.len(), count);
	entries
}
