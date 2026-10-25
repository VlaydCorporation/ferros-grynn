//! Plain family membership publication shares the checked incidence renderer.

use super::{support::*, *};

/// Surviving containers require a dense numeric scope remap.
mod child_scope_remap_tests;

/// Qualified surviving roles must follow the same direct scope remap.
mod qualified_child_scope_tests;

/// Wide child-scope changes preserve shared Hot frames and old physical pins.
mod qualified_child_wide_tests;

/// Generation ordering may revisit a Hot page after a later physical page.
mod qualified_child_generation_tests;

/// Qualified EDGE keys follow surviving child-scope changes.
mod qualified_edge_scope_tests;

/// Shared parent arenas retain each full path through a replaced leaf.
mod ancestor_leaf_tests;

/// Explicit selected-owner anchors keep stable leaf-replacement scopes.
mod selected_leaf_route_tests;

/// Direct directories grow at EOF and retain empty tails on shrink.
mod child_scope_directory_tests;

/// True NODE/EDGE child attach and detach must retain existing data files.
mod child_presence_tests;

/// Child presence resource admission, replay and recovery share exact source
/// expectations.
mod child_presence_failure_tests;

/// Shared/repeated children and final replay survive cold WAL reopen.
mod edge_child_rebind_lifecycle_tests;
/// Ordinary/qualified roles and E-E endpoint capabilities remain retained.
mod edge_child_rebind_matrix_tests;
/// Known and uncertain failures preserve complete arena/directory authority.
mod edge_child_rebind_recovery_tests;
/// Wide source roles share the fixed private preparation envelope.
mod edge_child_rebind_resource_tests;
/// Checksummed Hot and role-chain faults reject before publication.
mod edge_child_rebind_source_tests;
/// Accepted EDGE byte witnesses and owner-neutral child-only operations.
mod edge_child_rebind_support;
/// Existing EDGE transition rebinding retains numeric scope and old arena pins.
mod edge_child_rebind_tests;
/// Real containment source, untouched child pages and deliberate full
/// fallbacks.
mod metagraph;
/// A detached Base owner shares present leaf publication and old snapshot pins.
mod mixed_leaf_base_tests;
/// Stable qualified metadata composes with independent structural publication.
mod mixed_qualified_metadata_tests;
/// Repeated/shared bindings and replay preserve final cold authority.
mod node_child_rebind_lifecycle_tests;
/// Reached generation and WAL failure boundaries preserve atomic child images.
mod node_child_rebind_recovery_tests;
/// Canonical map copies share the caller's fixed logical envelope.
mod node_child_rebind_resource_tests;
/// Validly encoded source faults reject without publication.
mod node_child_rebind_source_tests;
/// Existing NODE child rebinding must retain data files and old arena pins.
mod node_child_rebind_tests;
/// Qualified source bounds, deep routes, retained pins and recovery.
mod qualified_guard_tests;
/// Qualified structural DML must retain the accepted physical generation.
mod qualified_publication_tests;
/// Same-child META rewires cover role representations and metadata composition.
mod transition_rewire_matrix_tests;
/// Ordinary overflow also keeps the first old-pin read after advancing
/// backfill.
mod transition_rewire_ordinary_pin_tests;
/// Old META and qualified pages retain their original snapshot authority.
mod transition_rewire_pin_tests;
/// Same-child rewires share resource, replay and failure barriers.
mod transition_rewire_recovery_tests;
/// False accepted bindings and validly encoded source identities reject
/// privately.
mod transition_rewire_source_tests;
/// Structural rewires may retain an existing META EDGE child binding.
mod transition_rewire_tests;
use crate::{
	storage::graph_pages::{decode_adjacency_overflow, hot_slot_address, read_edge_hot_slot},
	values::{PropertyKeyId, storage::DiskAtomRef},
};

/// Directed acyclic membership retains the accepted physical generation.
#[test]
fn dag_membership_sync_retains_files_and_reopens() {
	family_membership(IoMode::Sync, GraphKind::Dag);
}

/// NativeAsync remains selected across DAG publication and recovery.
#[cfg(feature = "native-async-io")]
#[test]
fn dag_membership_native_async_retains_files_and_reopens() {
	family_membership(IoMode::NativeAsync, GraphKind::Dag);
}

/// Forest extension and leaf removal preserve acyclicity.
#[test]
fn forest_membership_sync_retains_files_and_reopens() {
	family_membership(IoMode::Sync, GraphKind::Forest);
}

/// NativeAsync uses the same forest publication contract.
#[cfg(feature = "native-async-io")]
#[test]
fn forest_membership_native_async_retains_files_and_reopens() {
	family_membership(IoMode::NativeAsync, GraphKind::Forest);
}

/// A connected tree is extended and reduced atomically.
#[test]
fn tree_membership_sync_retains_files_and_reopens() {
	family_membership(IoMode::Sync, GraphKind::Tree);
}

/// NativeAsync reopens the connected tree with its declared family.
#[cfg(feature = "native-async-io")]
#[test]
fn tree_membership_native_async_retains_files_and_reopens() {
	family_membership(IoMode::NativeAsync, GraphKind::Tree);
}

/// Inferred and declared roots share the existing checked incidence publisher.
#[test]
fn rooted_tree_membership_sync_retains_files_and_reopens() {
	family_membership(IoMode::Sync, GraphKind::RootedTree);
}

/// NativeAsync preserves the rooted family and its source root declaration.
#[cfg(feature = "native-async-io")]
#[test]
fn rooted_tree_membership_native_async_retains_files_and_reopens() {
	family_membership(IoMode::NativeAsync, GraphKind::RootedTree);
}
/// Distinct hyperedge endpoints exceed the physical inline role capacity.
#[test]
fn hypergraph_membership_sync_retains_files_and_reopens() {
	family_membership(IoMode::Sync, GraphKind::Hypergraph);
}

/// NativeAsync preserves the same n-ary role order and overflow.
#[cfg(feature = "native-async-io")]
#[test]
fn hypergraph_membership_native_async_retains_files_and_reopens() {
	family_membership(IoMode::NativeAsync, GraphKind::Hypergraph);
}

/// Plain membership in a Metagraph preserves both genuine child containers.
#[test]
fn metagraph_membership_sync_retains_files_and_reopens() {
	family_membership(IoMode::Sync, GraphKind::Metagraph);
}

/// NativeAsync preserves unchanged containment through ordinary Metagraph DML.
#[cfg(feature = "native-async-io")]
#[test]
fn metagraph_membership_native_async_retains_files_and_reopens() {
	family_membership(IoMode::NativeAsync, GraphKind::Metagraph);
}

/// Invalid private topology never reaches physical publication.
#[test]
fn family_membership_sync_rejects_invalid_topology() {
	rejected_family_membership(IoMode::Sync);
}

/// NativeAsync preserves the same rejection boundary for every family.
#[cfg(feature = "native-async-io")]
#[test]
fn family_membership_native_async_rejects_invalid_topology() {
	rejected_family_membership(IoMode::NativeAsync);
}

/// Separate fixtures keep plain bulk references independent of overlay appends.
fn family_membership(io_mode: IoMode, kind: GraphKind) {
	for mixed in [false, true] {
		let (fixture, key) = family_fixture(io_mode, kind);
		if kind == GraphKind::RootedTree && mixed {
			declare_root(&fixture);
		}
		family_membership_case(fixture, key, mixed, false);
	}
	if kind == GraphKind::RootedTree {
		let (mut fixture, key) = family_fixture(io_mode, kind);
		declare_root(&fixture);
		let mut setup = fixture.store.begin();
		for (record, left, right) in
			[(&fixture.records[0], "node:a", "node:b"), (&fixture.records[1], "node:b", "node:c")]
		{
			let left = setup
				.membership(fixture.graph.id(), &left.parse().assert_ok())
				.assert_ok()
				.assert_some();
			let right = setup
				.membership(fixture.graph.id(), &right.parse().assert_ok())
				.assert_ok()
				.assert_some();
			setup
				.replace_occurrence_state(
					fixture.graph.id(),
					record,
					crate::datastore::OccurrenceState::edge(
						EdgeTopology::undirected([left, right]),
						None,
						None,
					),
				)
				.assert_ok();
		}
		setup.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		family_membership_case(fixture, key, true, true);
	}
}

/// One physical lifecycle oracle covers inferred/directed and
/// explicit/undirected roots.
fn family_membership_case(mut fixture: Fixture, key: PropertyKeyId, mixed: bool, undirected: bool) {
	let kind = fixture.graph.kind();
	if matches!(kind, GraphKind::RootedTree | GraphKind::Metagraph) {
		// A real Base change puts the pin above home without changing graph
		// namespaces.
		let mut advance = fixture.store.begin();
		advance.set_base_property(&"node:c".parse().assert_ok(), key, Value::from(31)).assert_ok();
		advance.commit().assert_ok();
	}
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	assert_bulk_image(&fixture);
	let source = fixture.store.inner.current_root();
	let pin =
		local(&fixture.store).capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
	if matches!(kind, GraphKind::RootedTree | GraphKind::Metagraph) {
		assert!(pin.0.snapshot.visible_lsn() > home);
	}
	let capacities = [AtomKind::Node, AtomKind::Edge].map(|kind| {
		u64::try_from(source.graph(fixture.graph.id()).assert_some().slot_capacity(kind))
			.assert_ok()
	});
	let witness = (kind == GraphKind::Metagraph).then(|| metagraph::Witness::capture(&fixture));
	let mut late = extension(&fixture, "node:d", 2, undirected);
	if mixed {
		late.set_occurrence_label(
			fixture.graph.id(),
			&"node:a".parse().assert_ok(),
			Some("A".into()),
		)
		.assert_ok();
		late.set_overlay(fixture.graph.id(), &"node:b".parse().assert_ok(), key, 777.into())
			.assert_ok();
	}
	// The independent branch consumes both provisional local allocator
	// slots.
	extension(&fixture, "node:e", 3, undirected).commit().assert_ok();
	assert_bulk_image(&fixture);
	if let Some(witness) = &witness {
		witness.assert_unchanged(&fixture);
	}
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	late.commit().assert_ok();
	assert_family_state(&fixture, key, mixed, witness.as_ref());
	assert_retained(&fixture, &files, mixed);
	let root = fixture.store.inner.current_root();
	let leaf = root
		.membership(
			fixture.graph.id(),
			root.resolve_atom(&"node:d".parse().assert_ok()).assert_some(),
		)
		.assert_some();
	let edge = root
		.membership(fixture.graph.id(), root.resolve_atom(&fixture.records[2]).assert_some())
		.assert_some();
	assert_eq!((leaf.slot(), edge.slot()), (capacities[0] + 1, capacities[1] + 1));
	drop(root);
	let mut remove = fixture.store.begin();
	remove.detach_occurrence(fixture.graph.id(), &"node:d".parse().assert_ok(), true).assert_ok();
	remove.commit().assert_ok();
	assert_family_state(&fixture, key, mixed, witness.as_ref());
	extension(&fixture, "node:d", 2, undirected).commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let reused_leaf = root
		.membership(
			fixture.graph.id(),
			root.resolve_atom(&"node:d".parse().assert_ok()).assert_some(),
		)
		.assert_some();
	let reused_edge = root
		.membership(fixture.graph.id(), root.resolve_atom(&fixture.records[2]).assert_some())
		.assert_some();
	assert_eq!(
		(reused_leaf.slot(), reused_leaf.generation()),
		(leaf.slot(), leaf.generation() + 1)
	);
	assert_eq!(
		(reused_edge.slot(), reused_edge.generation()),
		(edge.slot(), edge.generation() + 1)
	);
	drop(root);
	assert_family_state(&fixture, key, mixed, witness.as_ref());
	let mut transient = extension(&fixture, "node:f", 4, undirected);
	transient
		.detach_occurrence(fixture.graph.id(), &"node:f".parse().assert_ok(), true)
		.assert_ok();
	transient.commit().assert_ok();
	assert_family_state(&fixture, key, mixed, witness.as_ref());
	assert_retained(&fixture, &files, mixed);
	let advanced = fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	if matches!(kind, GraphKind::RootedTree | GraphKind::Metagraph) {
		assert!(advanced);
		let checkpoint = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
		assert!(checkpoint > home && checkpoint <= pin.0.snapshot.visible_lsn());
	}
	// First use of this old pin proves version retention, not cache-cold
	// I/O.
	let context = crate::datastore::index_context(&source.schema, &source.dictionary).assert_ok();
	let fields = local(&fixture.store)
		.physical_traversal_fields(&pin, fixture.graph.id(), fixture.nodes[1], &context.decoder())
		.assert_ok()
		.assert_some();
	assert_eq!(fields.base_properties.get(key), Some(&Value::from(20)));
	assert!(fields.overlay.is_none());
	drop(pin);
	drop(source);
	// Compare cold recovery with the accepted image, including freed
	// generations.
	let graph = fixture.graph.id();
	let image = |store: &Datastore| {
		let root = store.inner.current_root();
		let graph = root.graph(graph).assert_some();
		(
			root.commit_ts,
			graph.slot_capacity(AtomKind::Node),
			graph.slot_capacity(AtomKind::Edge),
			(0..graph.slot_capacity(AtomKind::Node))
				.map(|slot| graph.slot_generation(AtomKind::Node, slot))
				.collect::<Vec<_>>(),
			(0..graph.slot_capacity(AtomKind::Edge))
				.map(|slot| graph.slot_generation(AtomKind::Edge, slot))
				.collect::<Vec<_>>(),
			graph.occurrences().cloned().collect::<Vec<_>>(),
		)
	};
	let accepted = image(&fixture.store);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(image(&fixture.store), accepted);
	assert_eq!(fixture.graph.kind(), kind);
	assert_family_state(&fixture, key, mixed, witness.as_ref());
	assert_retained(&fixture, &files, mixed);
}

/// Source schema setup precedes every candidate pin and file-identity capture.
fn declare_root(fixture: &Fixture) {
	let mut definition = fixture.store.begin();
	definition
		.replace_graph_definition(
			"g",
			None,
			crate::graph::GraphTopologyConstraints::default(),
			Some("node:a".parse().assert_ok()),
		)
		.assert_ok();
	definition.commit().assert_ok();
}
/// Every seed is one connected chain, including the strict Tree family.
pub(super) fn family_fixture(io_mode: IoMode, kind: GraphKind) -> (Fixture, PropertyKeyId) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	seed.define_class(
		ClassSchema::new("node", AtomKind::Node, [ClassField::new("value", Type::Int)]).assert_ok(),
	)
	.assert_ok();
	seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let key = seed.intern_property("value").assert_ok();
	let graph = seed.define_graph("g", kind).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let records = ["edge:ab", "edge:bc", "edge:extension", "edge:spare", "edge:transient"]
		.map(|name| name.parse::<RecordId>().assert_ok())
		.to_vec();
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let mut nodes = Vec::new();
	for (index, name) in
		["node:a", "node:b", "node:c", "node:d", "node:e", "node:f"].into_iter().enumerate()
	{
		let record = name.parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(key, i64::try_from(index + 1).assert_ok() * 10);
		seed.create_atom(record.clone(), properties).assert_ok();
		if index < 3 {
			nodes.push(seed.attach_node(graph.id(), &record, None, None).assert_ok());
		}
		if index == 0 {
			seed.attach_node(other.id(), &record, None, None).assert_ok();
		}
	}
	for (index, pair) in nodes.windows(2).enumerate() {
		let topology = if matches!(kind, GraphKind::Forest | GraphKind::Tree) {
			EdgeTopology::undirected([pair[0], pair[1]])
		} else {
			EdgeTopology::directed([pair[0]], [pair[1]])
		};
		seed.attach_edge(graph.id(), &records[index], None, topology, None, None).assert_ok();
	}
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	let fixture = reopen(Fixture {
		store,
		options,
		graph,
		nodes: [nodes[0], nodes[1]],
		records,
		directory,
	});
	if kind == GraphKind::Metagraph {
		metagraph::with_containers(fixture, key)
	} else {
		(fixture, key)
	}
}

/// Returned local IDs remain private until final replay assigns accepted slots.
fn extension(fixture: &Fixture, leaf: &str, edge: usize, undirected: bool) -> Transaction {
	let mut write = fixture.store.begin();
	let c = write
		.membership(fixture.graph.id(), &"node:c".parse().assert_ok())
		.assert_ok()
		.assert_some();
	let leaf =
		write.attach_node(fixture.graph.id(), &leaf.parse().assert_ok(), None, None).assert_ok();
	let topology =
		if undirected || matches!(fixture.graph.kind(), GraphKind::Forest | GraphKind::Tree) {
			EdgeTopology::undirected([c, leaf])
		} else if fixture.graph.kind() == GraphKind::Hypergraph {
			EdgeTopology::directed([fixture.nodes[0], fixture.nodes[1], c], [leaf])
		} else {
			EdgeTopology::directed([c], [leaf])
		};
	write
		.attach_edge(fixture.graph.id(), &fixture.records[edge], None, topology, None, None)
		.assert_ok();
	write
}

/// Full Hot/topology comparison follows exact overlay payload validation.
fn assert_family_state(
	fixture: &Fixture,
	key: PropertyKeyId,
	mixed: bool,
	witness: Option<&metagraph::Witness>,
) {
	if let Some(witness) = witness {
		witness.assert_unchanged(fixture);
	}
	let root = fixture.store.inner.current_root();
	assert_eq!(
		root.graph(fixture.graph.id()).assert_some().reference().kind(),
		fixture.graph.kind()
	);
	if fixture.graph.kind() == GraphKind::RootedTree {
		// The mixed fixture declares its root before exercising metadata DML.
		let expected = mixed.then(|| "node:a".parse::<RecordId>().assert_ok());
		assert_eq!(root.schema.graph(fixture.graph.id()).assert_some().root(), expected.as_ref());
	}
	if fixture.graph.kind() == GraphKind::Hypergraph {
		assert_hyper_role(fixture, &root);
	}
	if !mixed {
		assert_bulk_image(fixture);
		return;
	}
	let record = "node:b".parse::<RecordId>().assert_ok();
	let owner =
		root.membership(fixture.graph.id(), root.resolve_atom(&record).assert_some()).assert_some();
	let pin =
		local(&fixture.store).capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	let fields = local(&fixture.store)
		.physical_traversal_fields(&pin, fixture.graph.id(), owner, &context.decoder())
		.assert_ok()
		.assert_some();
	assert_eq!(fields.base_properties.get(key), Some(&Value::from(20)));
	assert_eq!(
		fields.overlay.as_ref().assert_some().get(key),
		Some(&crate::datastore::OverlayValue::Value(777.into()))
	);
	assert_eq!(
		fields.overlay.as_ref(),
		root.occurrence(fixture.graph.id(), owner).assert_some().overlay()
	);
	let a = root
		.membership(
			fixture.graph.id(),
			root.resolve_atom(&"node:a".parse().assert_ok()).assert_some(),
		)
		.assert_some();
	assert_eq!(
		root.occurrence(fixture.graph.id(), a).assert_some().label().map(|label| label.as_str()),
		Some("A")
	);
	assert_bulk_image_with_overlay(fixture, owner);
}

/// Three distinct source endpoints require one physical overflow record.
fn assert_hyper_role(fixture: &Fixture, root: &crate::datastore::DatabaseRoot) {
	let graph = fixture.graph.id();
	let owner =
		root.membership(graph, root.resolve_atom(&fixture.records[3]).assert_some()).assert_some();
	let c = root
		.membership(graph, root.resolve_atom(&"node:c".parse().assert_ok()).assert_some())
		.assert_some();
	let leaf = root
		.membership(graph, root.resolve_atom(&"node:e".parse().assert_ok()).assert_some())
		.assert_some();
	let backend = local(&fixture.store);
	let state = backend.state.lock();
	let files =
		state.bindings.graphs.iter().find(|binding| binding.graph_id == graph).assert_some().files;
	let (number, within) = hot_slot_address(owner.slot()).assert_ok();
	let page = backend
		.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
		.assert_ok();
	let edge = read_edge_hot_slot(&page, number, within).assert_ok();
	assert_eq!((edge.source_count, edge.target_count), (3, 1));
	assert_eq!(
		edge.source_inline,
		fixture
			.nodes
			.map(|node| Some(DiskAtomRef::from_parts(AtomKind::Node, node.slot()).assert_ok()))
	);
	assert_eq!(edge.target_inline, [
		Some(DiskAtomRef::from_parts(AtomKind::Node, leaf.slot()).assert_ok()),
		None
	]);
	assert!(edge.target_overflow.is_none());
	let number = edge.source_overflow.assert_some();
	assert!(
		number.get() < state.graph_superblocks.get(&graph).assert_some().adjacency_overflow_pages
	);
	let page = backend
		.read_update_page(&state, PageKey::new(files.space(), files.adjacency, number))
		.assert_ok();
	let (next, endpoints) = decode_adjacency_overflow(&page, number).assert_ok();
	assert!(next.is_none());
	assert_eq!(endpoints, vec![DiskAtomRef::from_parts(AtomKind::Node, c.slot()).assert_ok()]);
}

/// Cycles, disconnection and duplicate hyperedge roles preserve root and WAL.
fn rejected_family_membership(io_mode: IoMode) {
	for kind in [
		GraphKind::Dag,
		GraphKind::Forest,
		GraphKind::Tree,
		GraphKind::RootedTree,
		GraphKind::Hypergraph,
	] {
		let (mut fixture, _key) = family_fixture(io_mode, kind);
		if kind == GraphKind::RootedTree {
			declare_root(&fixture);
		}
		let before = fixture.store.inner.current_root();
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = local(&fixture.store).wal.snapshot().visible_lsn();
		let directories = super::super::generation_directory_paths(&fixture.directory.0);
		let graph = fixture.graph.id();
		let c = before
			.membership(graph, before.resolve_atom(&"node:c".parse().assert_ok()).assert_some())
			.assert_some();
		let mut write = fixture.store.begin();
		let result = if kind == GraphKind::RootedTree {
			// b -> c remains a valid tree, but the declared root must still
			// exist.
			write
				.detach_occurrence(graph, &"node:a".parse().assert_ok(), true)
				.and_then(|_| write.commit())
		} else if kind == GraphKind::Tree {
			write.detach_occurrence(graph, &fixture.records[1], false).and_then(|_| write.commit())
		} else {
			let topology = match kind {
				GraphKind::Forest => EdgeTopology::undirected([fixture.nodes[0], c]),
				GraphKind::Hypergraph => {
					let leaf = write
						.attach_node(graph, &"node:d".parse().assert_ok(), None, None)
						.assert_ok();
					EdgeTopology::directed([fixture.nodes[0], fixture.nodes[0], c], [leaf])
				}
				_ => EdgeTopology::directed([c], [fixture.nodes[0]]),
			};
			write
				.attach_edge(graph, &fixture.records[2], None, topology, None, None)
				.and_then(|_| write.commit())
		};
		assert!(result.is_err(), "family accepted invalid topology: {kind:?}");
		assert!(std::sync::Arc::ptr_eq(&fixture.store.inner.current_root(), &before));
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		assert_eq!(local(&fixture.store).wal.snapshot().visible_lsn(), boundary);
		assert_eq!(super::super::generation_directory_paths(&fixture.directory.0), directories);
		drop(before);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_bulk_image(&fixture);
		assert_eq!(
			fixture.store.inner.current_root().graph(graph).assert_some().occurrences().len(),
			5
		);
	}
}

/// Only selected class trees, membership roots and graph statistics may rotate.
fn assert_retained(
	fixture: &Fixture,
	files: &[crate::storage::catalog::FileCatalogEntry],
	mixed: bool,
) {
	let backend = local(&fixture.store);
	let state = backend.state.lock();
	for entry in files {
		let selected = (entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& (matches!(
				entry.file_id.kind(),
				Some(DataFileKind::Index | DataFileKind::Statistics)
			) || (mixed
				&& (entry.file_id.kind() == Some(DataFileKind::LabelStatistics)
					|| entry.path.as_str().ends_with("/labels.fgb")))))
			|| (entry.space_id == DataSpaceId::Database
				&& entry.file_id.kind() == Some(DataFileKind::Index)
				&& entry.path.as_str().ends_with("/memberships.fgidx"));
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"FAMILY_MEMBERSHIP_REBUILT_DATA_FILES: {}",
				entry.path.as_str()
			);
		}
	}
}

/// Rewiring a plain EDGE in Metagraph leaves both real child containers intact.
#[test]
fn metagraph_rewire_sync_preserves_real_containment() {
	rewire_containment(IoMode::Sync);
}

/// NativeAsync preserves child identities, arena slots and parent directory
/// pages.
#[cfg(feature = "native-async-io")]
#[test]
fn metagraph_rewire_native_async_preserves_real_containment() {
	rewire_containment(IoMode::NativeAsync);
}

/// The witness owns distinct nonempty children and both NODE/EDGE containers.
fn rewire_containment(io_mode: IoMode) {
	let (mut fixture, _) = family_fixture(io_mode, GraphKind::Metagraph);
	let witness = metagraph::Witness::capture(&fixture);
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[0]).assert_some();
	let edge = root.membership(fixture.graph.id(), global).assert_some();
	drop(root);
	let [a, b] = fixture.nodes;
	let topologies = [
		EdgeTopology::directed([b], [a]),
		EdgeTopology::undirected([a, b]),
		EdgeTopology::directed([a], [a]),
	];
	for topology in &topologies {
		super::rewire_tests::write_topology(&fixture, topology.clone()).commit().assert_ok();
		super::rewire_tests::assert_topology(&fixture, global, edge, topology);
		super::rewire_tests::assert_retained(&fixture, &files, false);
		witness.assert_unchanged(&fixture);
		assert_bulk_image(&fixture);
	}
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	super::rewire_tests::assert_topology(&fixture, global, edge, &EdgeTopology::directed([a], [a]));
	super::rewire_tests::assert_retained(&fixture, &files, false);
	witness.assert_unchanged(&fixture);
	assert_bulk_image(&fixture);
}

/// Graph owners reuse the same source and publication contracts.
mod graph_owner_tests;
