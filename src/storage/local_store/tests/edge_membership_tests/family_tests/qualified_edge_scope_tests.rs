//! Unchanged qualified EDGE terminals require remapped physical endpoint keys.

use super::{super::endpoint_publication_tests::assert_postings, *};
use crate::{
	datastore::{GraphOccurrence, OccurrenceState, OccurrenceStateRef, SystemIndex},
	gql_status::FerrosGrynnResult,
	graph::{EdgeEndpoint, QualifiedAtomId},
	storage::{
		edge_endpoint_index::{decode_header, lookup},
		graph_pages::{EdgeSlotFlags, EndpointSide},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
	values::storage::DiskAtomRef,
};

/// Sync Node attach remaps an unchanged qualified EDGE posting.
#[test]
fn qualified_edge_scope_sync_node_attach_retains_files_and_old_keys() {
	remap(IoMode::Sync, AtomKind::Node, true);
}

/// NativeAsync Node attach remaps an unchanged qualified EDGE posting.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_scope_native_async_node_attach_retains_files_and_old_keys() {
	remap(IoMode::NativeAsync, AtomKind::Node, true);
}

/// Sync Node detach remaps an unchanged qualified EDGE posting.
#[test]
fn qualified_edge_scope_sync_node_detach_retains_files_and_old_keys() {
	remap(IoMode::Sync, AtomKind::Node, false);
}

/// NativeAsync Node detach remaps an unchanged qualified EDGE posting.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_scope_native_async_node_detach_retains_files_and_old_keys() {
	remap(IoMode::NativeAsync, AtomKind::Node, false);
}

/// Sync Edge attach remaps an unchanged qualified EDGE posting.
#[test]
fn qualified_edge_scope_sync_edge_attach_retains_files_and_old_keys() {
	remap(IoMode::Sync, AtomKind::Edge, true);
}

/// NativeAsync Edge attach remaps an unchanged qualified EDGE posting.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_scope_native_async_edge_attach_retains_files_and_old_keys() {
	remap(IoMode::NativeAsync, AtomKind::Edge, true);
}

/// Sync Edge detach remaps an unchanged qualified EDGE posting.
#[test]
fn qualified_edge_scope_sync_edge_detach_retains_files_and_old_keys() {
	remap(IoMode::Sync, AtomKind::Edge, false);
}

/// NativeAsync Edge detach remaps an unchanged qualified EDGE posting.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_scope_native_async_edge_detach_retains_files_and_old_keys() {
	remap(IoMode::NativeAsync, AtomKind::Edge, false);
}

/// Exact physical rows prove the literal key and exclude the obsolete scope.
pub(super) fn assert_lookup(
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	graph: GraphId,
	capability: SystemIndex,
	revision: u64,
	pages: u64,
	(scope, terminal, expected): (u32, DiskAtomRef, &[(DiskAtomRef, bool)]),
) {
	let mut page = AlignedPage::zeroed();
	read(PageNo::new(0), &mut page).assert_ok();
	let header = decode_header(&page, graph, capability, revision, pages).assert_ok();
	assert_eq!(header.posting_count, expected.len() as u64);
	assert_eq!(lookup(&mut read, header, scope, terminal).assert_ok().as_slice(), expected);
	assert!(lookup(&mut read, header, 3 - scope, terminal).assert_ok().is_empty());
}

/// The surviving owner changes only physical scopes, preserving its logical
/// state.
fn assert_roles(fixture: &Fixture, owner: &GraphOccurrence, terminal: AtomId, scope: u32) {
	let root = fixture.store.inner.current_root();
	assert_eq!(root.occurrence(fixture.graph.id(), owner.local_id()), Some(owner));
	let store = local(&fixture.store);
	let state = store.state.lock();
	let files = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	let (number, within) = hot_slot_address(owner.local_id().slot()).assert_ok();
	let key = PageKey::new(files.space(), files.edges, number);
	let hot = read_edge_hot_slot(&store.read_update_page(&state, key).assert_ok(), number, within)
		.assert_ok();
	assert_eq!(hot.global_atom, Some(owner.global_id()));
	assert_eq!(hot.generation, owner.local_id().generation());
	assert!(hot.flags.contains(EdgeSlotFlags::CROSS_LEVEL));
	assert!(hot.flags.contains(EdgeSlotFlags::META));
	assert_eq!(hot.transition_subgraph_slot, Some(scope));
	let weight = match owner.state().view() {
		OccurrenceStateRef::Edge {
			weight,
			..
		} => Some(*weight),
		OccurrenceStateRef::Node {
			..
		} => None,
	}
	.assert_some();
	assert_eq!(hot.weight(), weight.map(f64::try_from).transpose().assert_ok());
	assert_eq!((hot.source_count, hot.target_count), (1, 1));
	assert_eq!(hot.source_inline, [
		Some(DiskAtomRef::from_parts(AtomKind::Edge, terminal.slot()).assert_ok()),
		None
	]);
	assert_eq!(hot.target_inline, [
		Some(DiskAtomRef::from_parts(AtomKind::Node, fixture.nodes[1].slot()).assert_ok()),
		None
	]);
	assert_eq!(hot.endpoint_graph_slot(EndpointSide::Source), Some(scope));
	assert_eq!(hot.endpoint_graph_slot(EndpointSide::Target), Some(0));
	assert_eq!((hot.source_overflow, hot.target_overflow), (None, None));
	assert_eq!((hot.source_subgraph_overflow, hot.target_subgraph_overflow), (None, None));
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let space = DataSpaceId::Graph(fixture.graph.id());
	let capability =
		root.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().assert_some();
	let terminal = DiskAtomRef::from_parts(AtomKind::Edge, terminal.slot()).assert_ok();
	let connector = DiskAtomRef::from_parts(AtomKind::Edge, owner.local_id().slot()).assert_ok();
	assert_lookup(
		|number, page| {
			store.read_update_page_into(&state, PageKey::new(space, binding.file_id, number), page)
		},
		fixture.graph.id(),
		capability,
		root.commit_ts,
		store.visible_pages(&state, space, binding.file_id).assert_ok(),
		(scope, terminal, &[(connector, false)]),
	);
	drop(state);
	assert_postings(fixture, &[(scope, terminal.raw(), connector.raw(), 0)]);
}

/// The existing public leaf gains one EDGE; no Base identity is created.
fn remap(mode: IoMode, kind: AtomKind, attach: bool) {
	let (mut fixture, records, children) = child_scope_remap_tests::fixture(mode, kind, attach);
	let root = fixture.store.inner.current_root();
	let survivor = root
		.membership(fixture.graph.id(), root.resolve_atom(&records[1]).assert_some())
		.assert_some();
	let owner = root.occurrence(fixture.graph.id(), survivor).assert_some();
	let weight = match owner.state().view() {
		OccurrenceStateRef::Edge {
			weight,
			..
		} => Some(*weight),
		OccurrenceStateRef::Node {
			..
		} => None,
	}
	.assert_some();
	let terminals = root
		.graph(children[1])
		.assert_some()
		.occurrences()
		.filter(|owner| owner.local_id().is_node())
		.map(GraphOccurrence::local_id)
		.collect::<Vec<_>>();
	assert_eq!(terminals.len(), 1);
	drop(root);
	let mut setup = fixture.store.begin();
	let terminal = setup
		.attach_edge(
			children[1],
			&fixture.records[4],
			None,
			EdgeTopology::directed([terminals[0]], [terminals[0]]),
			None,
			None,
		)
		.assert_ok();
	let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([survivor], terminal).assert_ok());
	setup
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[1],
			OccurrenceState::edge(
				EdgeTopology::directed([nested], [EdgeEndpoint::Local(fixture.nodes[1])]),
				weight,
				Some(children[1]),
			),
		)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	// A distinct catalog publication leaves the accepted endpoint header in
	// WAL.
	let mut warm = fixture.store.begin();
	warm.intern_property("qualified_edge_scope_pin_boundary").assert_ok();
	warm.commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let owners = records.each_ref().map(|record| {
		let global = root.resolve_atom(record).assert_some();
		root.occurrence(
			fixture.graph.id(),
			root.membership(fixture.graph.id(), global).assert_some(),
		)
		.assert_some()
		.clone()
	});
	let terminal = root
		.membership(children[1], root.resolve_atom(&fixture.records[4]).assert_some())
		.assert_some();
	assert_eq!(terminal.kind(), Some(AtomKind::Edge));
	let source_scope = if attach {
		1
	} else {
		2
	};
	let final_scope = 3 - source_scope;
	let counts = [(1, 0, 1, 0), (1, 1, 1, 1)];
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == fixture.graph.id())
		.assert_some();
	let old_arena = binding.arena.clone();
	let endpoint = binding.edge_endpoint.assert_some();
	let space = DataSpaceId::Graph(fixture.graph.id());
	let tree_key = PageKey::new(space, endpoint.file_id, endpoint.root);
	let tree_file = state.registry.resolve(tree_key).assert_ok();
	let (number, _) = hot_slot_address(owners[1].local_id().slot()).assert_ok();
	let hot_key = PageKey::new(space, binding.files.edges, number);
	let hot_file = state.registry.resolve(hot_key).assert_ok();
	let hot_bytes = store.read_update_page(&state, hot_key).assert_ok();
	let capacity = state.graph_superblocks.get(&fixture.graph.id()).assert_some().subgraph_capacity;
	let home = state.database_superblock.checkpoint_lsn;
	let capability =
		root.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().assert_some();
	assert!(pin.0.snapshot.visible_lsn() > home);
	assert!(
		store.wal.visible_frames(&[tree_key], pin.0.snapshot).frames()[0].assert_some().frame_lsn()
			> home
	);
	drop(state);
	child_scope_remap_tests::assert_current_with_counts(
		&fixture, &owners, children, !attach, capacity, counts,
	);
	assert_roles(&fixture, &owners[1], terminal, source_scope);
	assert_bulk_image(&fixture);
	eprintln!("QUALIFIED_EDGE_SCOPE_SOURCE_PASS {mode:?} {kind:?} {attach}");
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[0],
			child_presence_tests::with_child(owners[0].state(), attach.then_some(children[0])),
		)
		.assert_ok();
	write.commit().assert_ok();
	child_scope_remap_tests::assert_current_with_counts(
		&fixture, &owners, children, attach, capacity, counts,
	);
	assert_roles(&fixture, &owners[1], terminal, final_scope);
	assert_bulk_image(&fixture);
	eprintln!("QUALIFIED_EDGE_SCOPE_LOGICAL_PASS {mode:?} {kind:?} {attach}");
	assert_retained(&fixture, &files, false);
	eprintln!("QUALIFIED_EDGE_SCOPE_RETAINED_PASS {mode:?} {kind:?} {attach}");
	assert!(
		!local(&fixture.store)
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.find(|entry| entry.graph_id == fixture.graph.id())
			.assert_some()
			.arena
			.same_owner(&old_arena)
	);
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
	assert!(advanced < store.wal.snapshot().visible_lsn());
	// The first old-tree and old-Hot reads occur only after backfill advances.
	assert_lookup(
		|number, page| {
			read_pinned_page_cached(
				&store.wal,
				&tree_file,
				&store.engine,
				pin.0.snapshot,
				pin.0.home_generation,
				pin.0.home_ceiling,
				PageKey::new(space, endpoint.file_id, number),
				page,
			)
		},
		fixture.graph.id(),
		capability,
		root.commit_ts,
		tree_file.page_count(),
		(source_scope, DiskAtomRef::from_parts(AtomKind::Edge, terminal.slot()).assert_ok(), &[(
			DiskAtomRef::from_parts(AtomKind::Edge, owners[1].local_id().slot()).assert_ok(),
			false,
		)]),
	);
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
	assert_eq!(page.as_bytes(), hot_bytes.as_bytes());
	eprintln!("QUALIFIED_EDGE_SCOPE_PIN_PASS {mode:?} {kind:?} {attach}");
	drop(tree_file);
	drop(hot_file);
	drop(pin);
	drop(root);
	fixture = reopen(fixture);
	for home_image in [false, true] {
		if home_image {
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = reopen(fixture);
		}
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		child_scope_remap_tests::assert_current_with_counts(
			&fixture, &owners, children, attach, capacity, counts,
		);
		assert_roles(&fixture, &owners[1], terminal, final_scope);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
	eprintln!("QUALIFIED_EDGE_SCOPE_COLD_PASS {mode:?} {kind:?} {attach}");
}

/// Sync keeps root-local endpoint keys even when another dense scope moves.
#[test]
fn root_local_edge_scope_sync_retains_endpoint_binding() {
	root_local(IoMode::Sync);
}

/// NativeAsync keeps the same file/root with only a newer header revision.
#[cfg(feature = "native-async-io")]
#[test]
fn root_local_edge_scope_native_async_retains_endpoint_binding() {
	root_local(IoMode::NativeAsync);
}

/// Arena changes alone cannot select an unrelated root-local posting tree.
fn root_local(mode: IoMode) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [true, false] {
			let (fixture, records, children) = child_scope_remap_tests::fixture(mode, kind, attach);
			let root = fixture.store.inner.current_root();
			let endpoint = root
				.membership(
					fixture.graph.id(),
					root.resolve_atom(&fixture.records[0]).assert_some(),
				)
				.assert_some();
			drop(root);
			let mut setup = fixture.store.begin();
			let connector = setup
				.attach_edge(
					fixture.graph.id(),
					&fixture.records[3],
					None,
					EdgeTopology::directed([endpoint], [fixture.nodes[1]]),
					None,
					None,
				)
				.assert_ok();
			setup.commit().assert_ok();
			let store = local(&fixture.store);
			let binding = store
				.state
				.lock()
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == fixture.graph.id())
				.assert_some()
				.edge_endpoint
				.assert_some();
			let row = (
				0,
				DiskAtomRef::from_parts(AtomKind::Edge, endpoint.slot()).assert_ok().raw(),
				DiskAtomRef::from_parts(AtomKind::Edge, connector.slot()).assert_ok().raw(),
				0,
			);
			assert_postings(&fixture, &[row]);
			let fixture = child_scope_remap_tests::publish(fixture, records, children, attach);
			assert_eq!(
				local(&fixture.store)
					.state
					.lock()
					.bindings
					.graphs
					.iter()
					.find(|entry| entry.graph_id == fixture.graph.id())
					.assert_some()
					.edge_endpoint,
				Some(binding)
			);
			assert_postings(&fixture, &[row]);
			eprintln!("ROOT_LOCAL_EDGE_SCOPE_RETAINED_PASS {mode:?} {kind:?} {attach}");
		}
	}
}
