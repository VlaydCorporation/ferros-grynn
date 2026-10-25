//! Stable role bytes and old snapshots survive mixed metadata publication.

use super::*;
use crate::{
	graph::EdgeEndpoint,
	storage::{
		graph_pages::{
			EdgeSlotFlags,
			decode_cross_level_overflow,
			hot_slot_address,
			read_edge_hot_slot,
		},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	},
};

/// Distinct accepted layouts exercise the shared scalar patcher.
#[derive(Clone, Copy, Debug)]
enum Shape {
	/// Mixed local/qualified roles occupy three accepted overflow pages.
	Wide,
	/// Two roles with different scopes cannot share one inline scope tag.
	MixedPair,
	/// Existing E-E capability remains available while another EDGE rewires.
	LocalEdge,
	/// A qualified META owner keeps its own accepted transition binding.
	Meta,
}

/// Sync checks every supported metadata layout against literal and bulk
/// oracles.
#[test]
fn mixed_metadata_sync_layout_matrix_keeps_roles_and_old_pin() {
	for shape in [Shape::Wide, Shape::MixedPair, Shape::LocalEdge, Shape::Meta] {
		mixed_layout(IoMode::Sync, shape);
	}
}

/// NativeAsync must preserve the same source versions through cold recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_metadata_native_async_layout_matrix_keeps_roles_and_old_pin() {
	for shape in [Shape::Wide, Shape::MixedPair, Shape::LocalEdge, Shape::Meta] {
		mixed_layout(IoMode::NativeAsync, shape);
	}
}

/// Each accepted layout has a metadata-only control before its mixed commit.
fn mixed_layout(mode: IoMode, shape: Shape) {
	let (mut fixture, key, topology, child) = layout_fixture(mode, shape);
	let witness = metagraph::Witness::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[2]).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	let independent_global = root.resolve_atom(&fixture.records[3]).assert_some();
	let old_independent = root.membership(fixture.graph.id(), independent_global);
	assert_eq!(old_independent.is_some(), matches!(shape, Shape::LocalEdge | Shape::Meta));
	drop(root);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let control = OccurrenceState::edge(topology.clone(), Some(3_i64.into()), child);
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(fixture.graph.id(), &fixture.records[2], control.clone())
		.assert_ok();
	write
		.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("qualified-old".into()))
		.assert_ok();
	write.set_overlay(fixture.graph.id(), &fixture.records[2], key, 111_i64.into()).assert_ok();
	write.commit().assert_ok();
	assert_metadata(&fixture, global, edge, key, &control, "qualified-old", 111);
	assert_retained(&fixture, &files, true);
	assert_containment(&witness, &fixture, shape);
	assert_bulk_image_with_overlays(&fixture, &[edge]);

	let source = fixture.store.inner.current_root();
	let store = local(&fixture.store);
	let home = store.state.lock().database_superblock.checkpoint_lsn;
	let pin = store.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	assert!(pin.0.snapshot.visible_lsn() > home);
	let boundary = pin.0.snapshot.visible_lsn();
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let graph_files = state
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
	let (number, within) = hot_slot_address(edge.slot()).assert_ok();
	let hot_key = PageKey::new(graph_files.space(), graph_files.edges, number);
	let hot_file = state.registry.resolve(hot_key).assert_ok();
	let old_hot =
		read_edge_hot_slot(&store.read_update_page(&state, hot_key).assert_ok(), number, within)
			.assert_ok();
	assert_eq!(old_hot.weight(), Some(3.0));
	if matches!(shape, Shape::Meta) {
		assert!(old_hot.flags.contains(EdgeSlotFlags::META | EdgeSlotFlags::CROSS_LEVEL));
		assert!(old_hot.transition_subgraph_slot.is_some());
	}
	let mut roles = Vec::new();
	for mut next in [old_hot.source_subgraph_overflow, old_hot.target_subgraph_overflow] {
		let mut count = 0;
		while let Some(page_no) = next {
			count += 1;
			assert!(count <= 3 && page_no.get() < ceiling);
			let key = PageKey::new(graph_files.space(), graph_files.cross_level_adjacency, page_no);
			let page = store.read_update_page(&state, key).assert_ok();
			next = decode_cross_level_overflow(&page, page_no).assert_ok().0;
			roles.push((key, state.registry.resolve(key).assert_ok(), page));
		}
	}
	assert_eq!(roles.len(), match shape {
		Shape::Wide => 3,
		Shape::MixedPair => 1,
		_ => 0,
	});
	drop(state);
	{
		let frames = store.wal.visible_frames(&[hot_key], pin.0.snapshot);
		assert!(frames.frames()[0].assert_some().frame_lsn() > home);
	}
	let expected = OccurrenceState::edge(topology, Some(7_i64.into()), child);
	let independent_state = OccurrenceState::edge(
		EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
		None,
		None,
	);
	let mut write = fixture.store.begin();
	write
		.replace_occurrence_state(fixture.graph.id(), &fixture.records[2], expected.clone())
		.assert_ok();
	write
		.set_occurrence_label(fixture.graph.id(), &fixture.records[2], Some("qualified-new".into()))
		.assert_ok();
	write.set_overlay(fixture.graph.id(), &fixture.records[2], key, 777_i64.into()).assert_ok();
	let independent = if let Some(local) = old_independent {
		assert_eq!(hot_slot_address(local.slot()).assert_ok().0, number);
		write
			.replace_occurrence_state(fixture.graph.id(), &fixture.records[3], independent_state)
			.assert_ok();
		local
	} else {
		write
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[3],
				None,
				EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
				None,
				None,
			)
			.assert_ok()
	};
	write.commit().assert_ok();
	assert_metadata(&fixture, global, edge, key, &expected, "qualified-new", 777);
	assert_independent(&fixture, independent_global, independent);
	assert_retained(&fixture, &files, true);
	assert_containment(&witness, &fixture, shape);
	assert_bulk_image_with_overlays(&fixture, &[edge]);
	{
		let state = store.state.lock();
		assert_eq!(
			state
				.graph_superblocks
				.get(&fixture.graph.id())
				.assert_some()
				.cross_level_adjacency_overflow_pages,
			ceiling
		);
		let mut current = read_edge_hot_slot(
			&store.read_update_page(&state, hot_key).assert_ok(),
			number,
			within,
		)
		.assert_ok();
		// Literal metadata was checked above; every remaining physical field is
		// stable.
		let current_root = fixture.store.inner.current_root();
		let label = |root: &crate::datastore::DatabaseRoot| {
			crate::storage::local_store::point_label::label_id(
				root,
				fixture.graph.id(),
				AtomKind::Edge,
				root.occurrence(fixture.graph.id(), edge).assert_some().label(),
			)
			.assert_ok()
		};
		current.replace_label(number, label(&current_root), label(&source)).assert_ok();
		current.replace_weight(number, Some(7.0), old_hot.weight()).assert_ok();
		current.overlay = old_hot.overlay;
		assert_eq!(current, old_hot);
	}
	let keys = roles.iter().map(|(key, _, _)| *key).collect::<Vec<_>>();
	{
		let frames = store.wal.visible_frames(&keys, store.wal.snapshot());
		assert!(frames.frames().iter().flatten().all(|frame| frame.frame_lsn() <= boundary));
	}
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	let advanced = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(advanced > home && advanced <= boundary);
	// No read through this exact pin occurs before the advancing checkpoint.
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
		assert_eq!(page.as_bytes(), expected.as_bytes());
	}
	drop(roles);
	drop(hot_file);
	drop(pin);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	assert_metadata(&fixture, global, edge, key, &expected, "qualified-new", 777);
	assert_independent(&fixture, independent_global, independent);
	assert_retained(&fixture, &files, true);
	assert_containment(&witness, &fixture, shape);
	assert_bulk_image_with_overlays(&fixture, &[edge]);
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	assert_eq!(
		read.query_physical_inner_graph(&snapshot, &fixture.graph, edge)
			.assert_ok()
			.assert_some()
			.graph,
		child
	);
}

/// Portal metadata may change; every containment binding and child image must
/// remain stable.
fn assert_containment(witness: &metagraph::Witness, fixture: &Fixture, shape: Shape) {
	if matches!(shape, Shape::Meta) {
		witness.assert_structure(fixture);
	} else {
		witness.assert_unchanged(fixture);
	}
}

/// Establishes capabilities and arena paths before any metadata or snapshot
/// control.
fn layout_fixture(
	mode: IoMode,
	shape: Shape,
) -> (Fixture, PropertyKeyId, EdgeTopology, Option<crate::graph::GraphId>) {
	let (mut fixture, key) = family_fixture(mode, GraphKind::Metagraph);
	let root = fixture.store.inner.current_root();
	let endpoint_global = root.resolve_atom(&fixture.records[0]).assert_some();
	let endpoint = root.membership(fixture.graph.id(), endpoint_global).assert_some();
	let child = matches!(shape, Shape::Meta)
		.then(|| root.schema.named_graph("child_edge").assert_some().reference().id());
	drop(root);
	let qualified = qualified_guard_tests::short_topology(&fixture);
	let topology = match shape {
		Shape::Wide => qualified_guard_tests::wide_topology(&fixture),
		Shape::MixedPair => EdgeTopology::directed(
			[qualified.source()[0].clone(), EdgeEndpoint::Local(fixture.nodes[0])],
			[EdgeEndpoint::Local(fixture.nodes[1])],
		),
		Shape::LocalEdge => EdgeTopology::directed([endpoint], [fixture.nodes[0]]),
		Shape::Meta => qualified,
	};
	let rewires = matches!(shape, Shape::LocalEdge | Shape::Meta);
	if matches!(shape, Shape::Meta) {
		// Reuse the accepted portal so the two-container witness remains exact.
		fixture.records[2] = "edge:portal".parse().assert_ok();
	}
	let mut setup = fixture.store.begin();
	if matches!(shape, Shape::Meta) {
		setup
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[2],
				OccurrenceState::edge(topology.clone(), None, child),
			)
			.assert_ok();
	} else {
		setup
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[2],
				None,
				topology.clone(),
				None,
				child,
			)
			.assert_ok();
	}
	if rewires {
		setup
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[3],
				None,
				EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				None,
				None,
			)
			.assert_ok();
	}
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	(fixture, key, topology, child)
}
