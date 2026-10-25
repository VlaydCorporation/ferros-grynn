//! Explicit qualified anchors permit stable-scope public leaf replacement.

use std::sync::Arc;

use super::{metagraph::with_replacement, *};
use crate::{
	datastore::OccurrenceState,
	graph::{EdgeEndpoint, QualifiedAtomId},
	storage::{
		graph_pages::decode_subgraph_directory,
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
};

/// Sync NODE container preserves a qualified NODE terminal's physical scope.
#[test]
fn selected_leaf_route_sync_node_node_retains_files() {
	replace(IoMode::Sync, AtomKind::Node, AtomKind::Node, true);
}

/// NativeAsync NODE container publishes the same complete final arena.
#[cfg(feature = "native-async-io")]
#[test]
fn selected_leaf_route_native_async_node_node_retains_files() {
	replace(IoMode::NativeAsync, AtomKind::Node, AtomKind::Node, true);
}

/// Sync NODE container keeps the qualified E-E key and endpoint file.
#[test]
fn selected_leaf_route_sync_node_edge_retains_files() {
	replace(IoMode::Sync, AtomKind::Node, AtomKind::Edge, true);
}

/// NativeAsync NODE container retains old qualified E-E snapshot authority.
#[cfg(feature = "native-async-io")]
#[test]
fn selected_leaf_route_native_async_node_edge_retains_files() {
	replace(IoMode::NativeAsync, AtomKind::Node, AtomKind::Edge, true);
}

/// Sync EDGE container preserves a qualified NODE terminal's physical scope.
#[test]
fn selected_leaf_route_sync_edge_node_retains_files() {
	replace(IoMode::Sync, AtomKind::Edge, AtomKind::Node, true);
}

/// NativeAsync EDGE container publishes the same complete final arena.
#[cfg(feature = "native-async-io")]
#[test]
fn selected_leaf_route_native_async_edge_node_retains_files() {
	replace(IoMode::NativeAsync, AtomKind::Edge, AtomKind::Node, true);
}

/// Sync EDGE container keeps the qualified E-E key and endpoint file.
#[test]
fn selected_leaf_route_sync_edge_edge_retains_files() {
	replace(IoMode::Sync, AtomKind::Edge, AtomKind::Edge, true);
}

/// NativeAsync EDGE container retains old qualified E-E snapshot authority.
#[cfg(feature = "native-async-io")]
#[test]
fn selected_leaf_route_native_async_edge_edge_retains_files() {
	replace(IoMode::NativeAsync, AtomKind::Edge, AtomKind::Edge, true);
}

/// Equal local bits cannot silently select a different terminal GraphId.
#[test]
fn selected_leaf_route_sync_unanchored_replacement_publishes_nothing() {
	for owner in [AtomKind::Node, AtomKind::Edge] {
		for terminal in [AtomKind::Node, AtomKind::Edge] {
			replace(IoMode::Sync, owner, terminal, false);
		}
	}
}

/// NativeAsync rejects the same logical retarget before any WAL publication.
#[cfg(feature = "native-async-io")]
#[test]
fn selected_leaf_route_native_async_unanchored_replacement_publishes_nothing() {
	for owner in [AtomKind::Node, AtomKind::Edge] {
		for terminal in [AtomKind::Node, AtomKind::Edge] {
			replace(IoMode::NativeAsync, owner, terminal, false);
		}
	}
}

/// Distinct published leaves share terminal bits but have different contents.
fn replace(mode: IoMode, owner_kind: AtomKind, terminal_kind: AtomKind, anchored: bool) {
	let (fixture, _) = family_fixture(mode, GraphKind::Metagraph);
	let (mut fixture, replacement) = with_replacement(fixture);
	let record = child_presence_tests::record(owner_kind);
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&record).assert_some();
	let owner = source.membership(fixture.graph.id(), global).assert_some();
	let occurrence = source.occurrence(fixture.graph.id(), owner).assert_some().clone();
	let child = occurrence.child_graph().assert_some();
	let node = source.graph(child).assert_some().occurrences().next().assert_some().local_id();
	assert!(node.is_node());
	assert_ne!(child, replacement);
	drop(source);
	let mut setup = fixture.store.begin();
	let terminal = if terminal_kind == AtomKind::Edge {
		setup
			.attach_edge(
				child,
				&fixture.records[4],
				None,
				EdgeTopology::directed([node], [node]),
				None,
				None,
			)
			.assert_ok()
	} else {
		node
	};
	let route = EdgeEndpoint::Nested(QualifiedAtomId::new([owner], terminal).assert_ok());
	let topology = EdgeTopology::directed([route], [EdgeEndpoint::Local(fixture.nodes[1])]);
	let connector = setup
		.attach_edge(fixture.graph.id(), &fixture.records[3], None, topology.clone(), None, None)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let mut warm = fixture.store.begin();
	warm.intern_property("selected_leaf_route_pin_source").assert_ok();
	warm.commit().assert_ok();
	let source = fixture.store.inner.current_root();
	assert!(source.occurrence(replacement, terminal).is_some());
	let connector_source = source.occurrence(fixture.graph.id(), connector).assert_some().clone();
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == fixture.graph.id())
		.assert_some();
	let arena = binding.arena.clone();
	let slot = arena.direct_child_slot(&source, fixture.graph.id(), owner).assert_ok();
	assert_eq!(
		slot,
		if owner_kind == AtomKind::Node {
			1
		} else {
			2
		}
	);
	let directory_key =
		PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
	let directory_file = state.registry.resolve(directory_key).assert_ok();
	let (next, entries) = decode_subgraph_directory(
		&store.read_update_page(&state, directory_key).assert_ok(),
		PageNo::new(0),
	)
	.assert_ok();
	assert_eq!(next, None);
	assert_eq!(entries.len(), 2);
	let old_counts = if terminal_kind == AtomKind::Edge {
		(1, 1, 1, 1)
	} else {
		(1, 0, 1, 0)
	};
	let selected = entries.iter().find(|entry| entry.slot == slot).assert_some();
	assert_eq!(
		(selected.node_capacity, selected.edge_capacity, selected.node_count, selected.edge_count),
		old_counts
	);
	let endpoint = binding.edge_endpoint;
	assert_eq!(endpoint.is_some(), terminal_kind == AtomKind::Edge);
	let home = state.database_superblock.checkpoint_lsn;
	let boundary = store.wal.snapshot().visible_lsn();
	assert!(boundary > home);
	drop(state);
	let mut old = fixture.store.begin();
	let old_query = old.query_snapshot().assert_ok();
	let final_state = child_presence_tests::with_child(occurrence.state(), Some(replacement));
	let mut write = fixture.store.begin();
	write.replace_occurrence_state(fixture.graph.id(), &record, final_state.clone()).assert_ok();
	if !anchored {
		let error = write.commit().assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::GraphConstraintViolation { .. }
		));
		assert!(Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.state.lock().file_catalog.files, files);
		assert_bulk_image(&fixture);
		eprintln!("SELECTED_LEAF_UNANCHORED_PASS {mode:?} {owner_kind:?} {terminal_kind:?}");
		return;
	}
	// Equal topology explicitly accepts the new terminal identity without a
	// second physical mutation.
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&fixture.records[3],
			OccurrenceState::edge(topology, None, None),
		)
		.assert_ok();
	write.commit().assert_ok();
	let assert_current = |fixture: &Fixture| {
		let root = fixture.store.inner.current_root();
		assert_eq!(root.occurrence(fixture.graph.id(), owner).assert_some().state(), &final_state);
		assert_eq!(root.occurrence(fixture.graph.id(), connector), Some(&connector_source));
		let mut read = fixture.store.begin();
		let query = read.query_snapshot().assert_ok();
		assert_eq!(
			read.query_physical_inner_graph(&query, &fixture.graph, owner)
				.assert_ok()
				.assert_some()
				.graph,
			Some(replacement)
		);
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|entry| entry.graph_id == fixture.graph.id())
			.assert_some();
		let mut expected = arena.graph_by_slot().clone();
		assert_eq!(expected.insert(slot, replacement), Some(child));
		assert_eq!(binding.arena.graph_by_slot(), &expected);
		assert_eq!(binding.arena.scope_by_slot(), arena.scope_by_slot());
		let key = PageKey::new(binding.files.space(), binding.files.subgraphs, PageNo::new(0));
		let (next, actual) = decode_subgraph_directory(
			&store.read_update_page(&state, key).assert_ok(),
			PageNo::new(0),
		)
		.assert_ok();
		let mut expected = entries.clone();
		let selected = expected.iter_mut().find(|entry| entry.slot == slot).assert_some();
		(
			selected.node_capacity,
			selected.edge_capacity,
			selected.node_count,
			selected.edge_count,
		) = (2, 1, 2, 1);
		assert_eq!((next, actual), (None, expected));
		if terminal_kind == AtomKind::Edge {
			let current = binding.edge_endpoint.assert_some();
			let capability = root
				.schema
				.graph(fixture.graph.id())
				.assert_some()
				.edge_endpoint_index()
				.assert_some();
			qualified_edge_scope_tests::assert_lookup(
				|number, page| {
					store.read_update_page_into(
						&state,
						PageKey::new(binding.files.space(), current.file_id, number),
						page,
					)
				},
				fixture.graph.id(),
				capability,
				root.commit_ts,
				store.visible_pages(&state, binding.files.space(), current.file_id).assert_ok(),
				(slot, DiskAtomRef::from_parts(AtomKind::Edge, terminal.slot()).assert_ok(), &[(
					DiskAtomRef::from_parts(AtomKind::Edge, connector.slot()).assert_ok(),
					false,
				)]),
			);
		}
		drop(state);
		assert_bulk_image(fixture);
	};
	assert_current(&fixture);
	eprintln!("SELECTED_LEAF_LOGICAL_PASS {mode:?} {owner_kind:?} {terminal_kind:?}");
	assert_retained(&fixture, &files, false);
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
		endpoint
	);
	eprintln!("SELECTED_LEAF_RETAINED_PASS {mode:?} {owner_kind:?} {terminal_kind:?}");
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	assert_eq!(
		old.query_physical_inner_graph(&old_query, &fixture.graph, owner)
			.assert_ok()
			.assert_some()
			.graph,
		Some(child)
	);
	let mut page = AlignedPage::zeroed();
	read_pinned_page_cached(
		&store.wal,
		&directory_file,
		&store.engine,
		pin.0.snapshot,
		pin.0.home_generation,
		pin.0.home_ceiling,
		directory_key,
		&mut page,
	)
	.assert_ok();
	assert_eq!(
		decode_subgraph_directory(&page, PageNo::new(0)).assert_ok(),
		(None, entries.clone())
	);
	eprintln!("SELECTED_LEAF_PIN_PASS {mode:?} {owner_kind:?} {terminal_kind:?}");
	drop(directory_file);
	drop(pin);
	drop(old_query);
	drop(old);
	drop(source);
	fixture = reopen(fixture);
	for checkpoint in [false, true] {
		if checkpoint {
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = reopen(fixture);
		}
		assert_current(&fixture);
		assert_retained(&fixture, &files, false);
	}
	eprintln!("SELECTED_LEAF_COLD_PASS {mode:?} {owner_kind:?} {terminal_kind:?}");
}
