//! Untouched real containment is a separate oracle from plain membership Hot
//! rows.

use super::*;
use crate::{
	datastore::{GraphOccurrence, OccurrenceState},
	storage::{
		catalog::FileCatalogEntry,
		graph_pages::{
			GraphArenaBindings,
			SubgraphEntry,
			build_graph_image,
			decode_subgraph_directory,
		},
		page::{PageLsn, PageNo},
		types::{DataFileId, Lsn, PageKey},
	},
	values::storage::EncodeContext,
};

/// Directory framing and entries are compared independently of page
/// LSN/checksum.
type Directory = Vec<(Option<PageNo>, Vec<SubgraphEntry>)>;

/// Adds genuine, isolated NODE and EDGE containers before any candidate
/// capture.
pub(super) fn with_containers(
	mut fixture: Fixture,
	key: PropertyKeyId,
) -> (Fixture, PropertyKeyId) {
	let mut setup = fixture.store.begin();
	let children = ["child_node", "child_edge"]
		.map(|name| setup.define_graph(name, GraphKind::Graph).assert_ok());
	for (index, name) in
		["node:container", "node:child_node", "node:child_edge"].into_iter().enumerate()
	{
		let mut properties = PropertyMap::new();
		properties.insert(key, i64::try_from(index + 90).assert_ok());
		setup.create_atom(name.parse().assert_ok(), properties).assert_ok();
	}
	for (child, name) in children.iter().zip(["node:child_node", "node:child_edge"]) {
		setup.attach_node(child.id(), &name.parse().assert_ok(), None, None).assert_ok();
	}
	let container = setup
		.attach_node(
			fixture.graph.id(),
			&"node:container".parse().assert_ok(),
			None,
			Some(children[0].id()),
		)
		.assert_ok();
	let portal = "edge:portal".parse::<RecordId>().assert_ok();
	setup.create_atom(portal.clone(), PropertyMap::new()).assert_ok();
	setup
		.attach_edge(
			fixture.graph.id(),
			&portal,
			None,
			EdgeTopology::directed([container], [container]),
			None,
			Some(children[1].id()),
		)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	(fixture, key)
}

/// Complete source containment, child identities and publication boundary.
pub(super) struct Witness {
	/// The same logical arena must appear in binding, catalog and full bulk
	/// image.
	arena: GraphArenaBindings,
	/// Only containment counters are stable; ordinary NODE/EDGE ceilings may
	/// grow.
	ceilings: (u64, u64, u64),
	/// Existing parent directory identity, even when parent class trees rotate.
	directory_file: DataFileId,
	/// Ordered source links and all direct entries.
	directory: Directory,
	/// Both source containers retain identity, labels, overlays and child
	/// bindings.
	containers: [GraphOccurrence; 2],
	/// Separate nonempty children prevent empty or shared-child fixtures
	/// passing.
	children: [crate::graph::GraphId; 2],
	/// Every child-owned catalog entry remains identical.
	child_files: Vec<FileCatalogEntry>,
	/// All source child pages and the parent directory must receive no new
	/// frames.
	keys: Vec<PageKey>,
	/// Captured after setup, before independent and replayed candidate writes.
	boundary: Lsn,
}

impl Witness {
	/// Validates the source itself before using it as an unchanged-data
	/// witness.
	pub(super) fn capture(fixture: &Fixture) -> Self {
		let root = fixture.store.inner.current_root();
		let children = ["child_node", "child_edge"]
			.map(|name| root.schema.named_graph(name).assert_some().reference().id());
		assert_ne!(children[0], children[1]);
		for child in children {
			assert_eq!(root.graph(child).assert_some().occurrences().len(), 1);
		}
		let containers = ["node:container", "edge:portal"].map(|name| {
			let global = root.resolve_atom(&name.parse().assert_ok()).assert_some();
			let local = root.membership(fixture.graph.id(), global).assert_some();
			root.occurrence(fixture.graph.id(), local).assert_some().clone()
		});
		for (container, child) in containers.iter().zip(children) {
			assert_eq!(container.child_graph(), Some(child));
		}
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding =
			state.bindings.graphs.iter().find(|b| b.graph_id == fixture.graph.id()).assert_some();
		let arena = binding.arena.clone();
		assert_eq!(arena.graph_by_slot().len(), 3);
		for (container, child) in containers.iter().zip(children) {
			let (slot, _) = arena
				.scope_by_slot()
				.iter()
				.find(|(_, scope)| scope.as_ref() == &[container.local_id()][..])
				.assert_some();
			assert_ne!(*slot, 0);
			assert_eq!(arena.graph_by_slot().get(slot), Some(&child));
		}
		let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
		let ceilings = (header.subgraph_capacity, header.subgraph_count, header.subgraphs_pages);
		let directory_file = binding.files.subgraphs;
		let child_files = state
			.file_catalog
			.files
			.iter()
			.filter(|entry| {
				children.iter().any(|child| entry.space_id == DataSpaceId::Graph(*child))
			})
			.cloned()
			.collect::<Vec<_>>();
		assert!(!child_files.is_empty());
		let snapshot = store.wal.snapshot();
		let mut keys = (0..header.subgraphs_pages)
			.map(|number| PageKey::new(binding.files.space(), directory_file, PageNo::new(number)))
			.collect::<Vec<_>>();
		for entry in &child_files {
			let Some(count) = super::super::watched_page_count(store, &state, entry) else {
				continue;
			};
			keys.extend(
				(0..count)
					.map(|number| PageKey::new(entry.space_id, entry.file_id, PageNo::new(number))),
			);
		}
		drop(state);
		let directory = directory(fixture);
		assert_eq!(directory.iter().map(|(_, entries)| entries.len()).sum::<usize>(), 2);
		let witness = Self {
			arena,
			ceilings,
			directory_file,
			directory,
			containers,
			children,
			child_files,
			keys,
			boundary: snapshot.visible_lsn(),
		};
		witness.assert_unchanged(fixture);
		witness
	}

	/// Compares source/current and an independently rebuilt full image after
	/// each phase.
	pub(super) fn assert_unchanged(&self, fixture: &Fixture) {
		let root = fixture.store.inner.current_root();
		for source in &self.containers {
			assert_eq!(root.occurrence(fixture.graph.id(), source.local_id()), Some(source));
		}
		drop(root);
		self.assert_structure(fixture);
	}

	/// Metadata may vary while arena, directory and child images remain stable.
	pub(super) fn assert_structure(&self, fixture: &Fixture) {
		let root = fixture.store.inner.current_root();
		let mut context = EncodeContext::new(root.dictionary.as_ref().clone());
		let expected =
			build_graph_image(&root, fixture.graph.id(), &mut context, PageLsn::ZERO).assert_ok();
		assert_eq!(expected.arena, self.arena);
		let expected_directory = expected
			.subgraphs
			.iter()
			.enumerate()
			.map(|(number, page)| {
				decode_subgraph_directory(page, PageNo::new(u64::try_from(number).assert_ok()))
					.assert_ok()
			})
			.collect::<Directory>();
		assert_eq!(directory(fixture), self.directory);
		assert_eq!(expected_directory, self.directory);
		let store = local(&fixture.store);
		let state = store.state.lock();
		let binding =
			state.bindings.graphs.iter().find(|b| b.graph_id == fixture.graph.id()).assert_some();
		assert_eq!(binding.arena, self.arena);
		assert_eq!(
			binding.files.subgraphs, self.directory_file,
			"METAGRAPH_MEMBERSHIP_REBUILT_CONTAINMENT_FILES"
		);
		assert_eq!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|g| g.id == fixture.graph.id())
				.assert_some()
				.arena,
			self.arena
		);
		let header = state.graph_superblocks.get(&fixture.graph.id()).assert_some();
		assert_eq!(
			(header.subgraph_capacity, header.subgraph_count, header.subgraphs_pages),
			self.ceilings
		);
		let child_files = state
			.file_catalog
			.files
			.iter()
			.filter(|entry| {
				self.children.iter().any(|child| entry.space_id == DataSpaceId::Graph(*child))
			})
			.cloned()
			.collect::<Vec<_>>();
		assert_eq!(child_files, self.child_files);
		drop(state);
		let visible = store.wal.visible_frames(&self.keys, store.wal.snapshot());
		assert!(visible.frames().iter().flatten().all(|frame| frame.frame_lsn() <= self.boundary));
	}
}

/// Reads physical directory pages without deriving their contents from the
/// root.
fn directory(fixture: &Fixture) -> Directory {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding =
		state.bindings.graphs.iter().find(|b| b.graph_id == fixture.graph.id()).assert_some();
	let ceiling = state.graph_superblocks.get(&fixture.graph.id()).assert_some().subgraphs_pages;
	(0..ceiling)
		.map(|number| {
			let number = PageNo::new(number);
			let page = store
				.read_update_page(
					&state,
					PageKey::new(binding.files.space(), binding.files.subgraphs, number),
				)
				.assert_ok();
			decode_subgraph_directory(&page, number).assert_ok()
		})
		.collect()
}

/// Sync retains container metadata identities and rebuilds real removal.
#[test]
fn metagraph_container_fallback_sync_remains_complete() {
	container_fallback(IoMode::Sync);
}

/// NativeAsync preserves both sparse metadata and complete removal fallback.
#[cfg(feature = "native-async-io")]
#[test]
fn metagraph_container_fallback_native_async_remains_complete() {
	container_fallback(IoMode::NativeAsync);
}

/// Owned source graph occurrence and Base payload must both survive full
/// fallback.
type ChildImage = Vec<(GraphOccurrence, PropertyMap)>;

/// Existing children permit mixed metadata; actual removal retains full
/// publication.
fn container_fallback(io_mode: IoMode) {
	let (mut fixture, _) = family_fixture(io_mode, GraphKind::Metagraph);
	let source = Witness::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let children = source.children.map(|child| (child, child_image(&root, child)));
	drop(root);
	for remove in [false, true] {
		let original = local(&fixture.store)
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.find(|b| b.graph_id == fixture.graph.id())
			.assert_some()
			.files
			.nodes;
		let mut write = fixture.store.begin();
		if remove {
			write
				.detach_occurrence(fixture.graph.id(), &"node:container".parse().assert_ok(), true)
				.assert_ok();
		} else {
			write
				.attach_node(fixture.graph.id(), &"node:d".parse().assert_ok(), None, None)
				.assert_ok();
			write
				.set_occurrence_label(
					fixture.graph.id(),
					&"node:container".parse().assert_ok(),
					Some("container-updated".into()),
				)
				.assert_ok();
		}
		write.commit().assert_ok();
		let current = local(&fixture.store)
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.find(|b| b.graph_id == fixture.graph.id())
			.assert_some()
			.files
			.nodes;
		if remove {
			assert_ne!(current, original);
		} else {
			assert_eq!(current, original);
		}
		assert_fallback_result(&fixture, remove, &source, &children);
		assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		assert_fallback_result(&fixture, remove, &source, &children);
		assert_bulk_image(&fixture);
	}
}

/// Captures exact occurrence identity/state and resolved Base values, not just
/// counts.
fn child_image(root: &crate::datastore::DatabaseRoot, child: crate::graph::GraphId) -> ChildImage {
	root.graph(child)
		.assert_some()
		.occurrences()
		.map(|occurrence| {
			(
				occurrence.clone(),
				root.atom(occurrence.global_id()).assert_some().properties().clone(),
			)
		})
		.collect()
}

/// Checks requested effects before and after recovery, separately from bulk
/// agreement.
fn assert_fallback_result(
	fixture: &Fixture,
	removed: bool,
	source: &Witness,
	children: &[(crate::graph::GraphId, ChildImage); 2],
) {
	let root = fixture.store.inner.current_root();
	let membership = |name: &str| {
		let global = root.resolve_atom(&name.parse().assert_ok()).assert_some();
		root.membership(fixture.graph.id(), global)
	};
	assert!(membership("node:d").is_some());
	if removed {
		assert!(membership("node:container").is_none());
		assert!(membership("edge:portal").is_none());
	} else {
		let local = membership("node:container").assert_some();
		let container = root.occurrence(fixture.graph.id(), local).assert_some();
		assert_eq!(local, source.containers[0].local_id());
		assert_eq!(container.label().map(|label| label.as_str()), Some("container-updated"));
		assert_eq!(container.child_graph(), Some(source.children[0]));
		let portal = membership("edge:portal").assert_some();
		assert_eq!(root.occurrence(fixture.graph.id(), portal), Some(&source.containers[1]));
	}
	for (child, expected) in children {
		assert!(!expected.is_empty());
		assert_eq!(&child_image(&root, *child), expected);
	}
}

/// Stable container metadata must share the ordinary membership publication.
#[test]
fn metagraph_mixed_container_metadata_sync_retains_files_and_pins() {
	mixed_container_metadata(IoMode::Sync, false);
}

/// NativeAsync combines both container kinds with NODE/EDGE Hot-page changes.
#[cfg(feature = "native-async-io")]
#[test]
fn metagraph_mixed_container_metadata_native_async_retains_files_and_pins() {
	mixed_container_metadata(IoMode::NativeAsync, false);
}

/// The metadata NODE container also receives the new EDGE adjacency.
#[test]
fn metagraph_mixed_container_endpoint_sync_retains_files_and_pins() {
	mixed_container_metadata(IoMode::Sync, true);
}

/// NativeAsync merges derived adjacency and metadata within one container slot.
#[cfg(feature = "native-async-io")]
#[test]
fn metagraph_mixed_container_endpoint_native_async_retains_files_and_pins() {
	mixed_container_metadata(IoMode::NativeAsync, true);
}

/// A metadata-only setup creates genuine old WAL-backed container versions.
fn mixed_container_metadata(io_mode: IoMode, endpoint_overlap: bool) {
	use crate::storage::{
		graph_pages::{hot_slot_address, read_edge_hot_slot, read_node_hot_slot},
		image_io::read_pinned_page_cached,
		page::AlignedPage,
	};
	let (mut fixture, key) = family_fixture(io_mode, GraphKind::Metagraph);
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let records =
		["node:container", "edge:portal"].map(|name| name.parse::<RecordId>().assert_ok());
	let root = fixture.store.inner.current_root();
	let container = root
		.membership(fixture.graph.id(), root.resolve_atom(&records[0]).assert_some())
		.assert_some();
	let child = root.schema.named_graph("child_edge").assert_some().reference().id();
	drop(root);
	let mut setup = fixture.store.begin();
	for (record, label, value) in
		[(&records[0], "container-old", 111_i64), (&records[1], "portal-old", 222)]
	{
		setup.set_occurrence_label(fixture.graph.id(), record, Some(label.into())).assert_ok();
		setup.set_overlay(fixture.graph.id(), record, key, value.into()).assert_ok();
	}
	setup
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[1],
			OccurrenceState::edge(
				EdgeTopology::directed([container], [container]),
				Some(2_i64.into()),
				Some(child),
			),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let source = Witness::capture(&fixture);
	let root = fixture.store.inner.current_root();
	let children = source.children.map(|child| (child, child_image(&root, child)));
	let owners = source.containers.each_ref().map(GraphOccurrence::local_id);
	assert_container_metadata_source(&source, &children, key, container, child);
	let mut old = fixture.store.begin();
	let query = old.query_snapshot().assert_ok();
	for (owner, value) in owners.into_iter().zip([111_i64, 222]) {
		let fields =
			old.query_physical_fields(&query, &fixture.graph, owner).assert_ok().assert_some();
		assert_eq!(fields.properties.len(), 1);
		assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(value)));
	}
	assert_bulk_image_with_overlays(&fixture, &owners);
	super::super::super::graph_overlay_tests::label_tests::assert_label_arrays(
		&fixture.store,
		fixture.graph.id(),
	);
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
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
	let (node_page, node_within) = hot_slot_address(owners[0].slot()).assert_ok();
	let (edge_page, edge_within) = hot_slot_address(owners[1].slot()).assert_ok();
	assert_eq!(node_page, PageNo::new(0));
	assert_eq!(edge_page, PageNo::new(0));
	let keys = [
		PageKey::new(graph_files.space(), graph_files.nodes, node_page),
		PageKey::new(graph_files.space(), graph_files.edges, edge_page),
	];
	let handles = keys.map(|key| state.registry.resolve(key).assert_ok());
	let old_node = read_node_hot_slot(
		&store.read_update_page(&state, keys[0]).assert_ok(),
		node_page,
		node_within,
	)
	.assert_ok();
	let old_edge = read_edge_hot_slot(
		&store.read_update_page(&state, keys[1]).assert_ok(),
		edge_page,
		edge_within,
	)
	.assert_ok();
	assert_eq!(old_edge.weight(), Some(2.0));
	drop(state);
	let old_versions = {
		let visible = store.wal.visible_frames(&keys, pin.0.snapshot);
		std::array::from_fn::<_, 2, _>(|index| {
			let lsn = visible.frames()[index].assert_some().frame_lsn();
			assert!(lsn > home);
			lsn
		})
	};
	let mut write = extension(&fixture, "node:d", 2, false);
	if endpoint_overlap {
		let node = write
			.membership(fixture.graph.id(), &"node:d".parse().assert_ok())
			.assert_ok()
			.assert_some();
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[2],
				OccurrenceState::edge(EdgeTopology::directed([container], [node]), None, None),
			)
			.assert_ok();
	}
	for (record, label, value) in
		[(&records[0], "container-new", 777_i64), (&records[1], "portal-new", 333)]
	{
		write.set_occurrence_label(fixture.graph.id(), record, Some(label.into())).assert_ok();
		write.set_overlay(fixture.graph.id(), record, key, value.into()).assert_ok();
	}
	write
		.replace_occurrence_state(
			fixture.graph.id(),
			&records[1],
			OccurrenceState::edge(
				EdgeTopology::directed([container], [container]),
				Some(7_i64.into()),
				Some(child),
			),
		)
		.assert_ok();
	write.commit().assert_ok();
	let current_files = store
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.files;
	assert_eq!(
		(current_files.nodes, current_files.edges),
		(graph_files.nodes, graph_files.edges),
		"MIXED_CONTAINER_METADATA_REBUILT_FILES"
	);
	super::super::rewire_tests::assert_retained(&fixture, &files, true);
	let accepted =
		assert_mixed_container_metadata(&fixture, &source, key, &children, endpoint_overlap);
	{
		let visible = store.wal.visible_frames(&keys, store.wal.snapshot());
		for (frame, old) in visible.frames().iter().zip(old_versions) {
			assert!(frame.assert_some().frame_lsn() > old);
		}
	}
	for checkpoint in [false, true] {
		if checkpoint {
			assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
			let advanced = store.state.lock().database_superblock.checkpoint_lsn;
			assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
		}
		for (index, owner) in owners.into_iter().enumerate() {
			let fields =
				old.query_physical_fields(&query, &fixture.graph, owner).assert_ok().assert_some();
			assert_eq!(fields.properties.len(), 1);
			assert_eq!(
				fields.properties.get(key),
				Some(&crate::values::Value::from([111_i64, 222][index]))
			);
			let inner = old
				.query_physical_inner_graph(&query, &fixture.graph, owner)
				.assert_ok()
				.assert_some();
			assert_eq!(inner.graph, Some(source.children[index]));
		}
		let mut page = AlignedPage::zeroed();
		for (key, handle) in keys.iter().zip(&handles) {
			read_pinned_page_cached(
				&store.wal,
				handle,
				&store.engine,
				pin.0.snapshot,
				pin.0.home_generation,
				pin.0.home_ceiling,
				*key,
				&mut page,
			)
			.assert_ok();
			if *key == keys[0] {
				assert_eq!(read_node_hot_slot(&page, node_page, node_within).assert_ok(), old_node);
			} else {
				assert_eq!(read_edge_hot_slot(&page, edge_page, edge_within).assert_ok(), old_edge);
			}
		}
	}
	drop(handles);
	drop(pin);
	drop(query);
	drop(old);
	drop(root);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(
		assert_mixed_container_metadata(&fixture, &source, key, &children, endpoint_overlap),
		accepted
	);
	super::super::rewire_tests::assert_retained(&fixture, &files, true);
}

/// Literal source payloads and state keep the retained-child oracle
/// independent.
fn assert_container_metadata_source(
	source: &Witness,
	children: &[(crate::graph::GraphId, ChildImage); 2],
	key: PropertyKeyId,
	container: AtomId,
	child: crate::graph::GraphId,
) {
	for ((_, image), value) in children.iter().zip([91_i64, 92]) {
		assert_eq!(image.len(), 1);
		assert_eq!(image[0].1.len(), 1);
		assert_eq!(image[0].1.get(key), Some(&crate::values::Value::from(value)));
	}
	assert_eq!(source.containers[0].label().map(|label| label.as_str()), Some("container-old"));
	assert_eq!(source.containers[1].label().map(|label| label.as_str()), Some("portal-old"));
	assert_eq!(
		source.containers[1].state(),
		&OccurrenceState::edge(
			EdgeTopology::directed([container], [container]),
			Some(2_i64.into()),
			Some(child)
		)
	);
}

/// Literal accepted state and child payloads are independent of the bulk
/// writer.
fn assert_mixed_container_metadata(
	fixture: &Fixture,
	source: &Witness,
	key: PropertyKeyId,
	children: &[(crate::graph::GraphId, ChildImage); 2],
	endpoint_overlap: bool,
) -> [AtomId; 2] {
	let root = fixture.store.inner.current_root();
	let owners = source.containers.each_ref().map(GraphOccurrence::local_id);
	for (index, (name, label, value)) in
		[("node:container", "container-new", 777_i64), ("edge:portal", "portal-new", 333)]
			.into_iter()
			.enumerate()
	{
		let original = &source.containers[index];
		assert_eq!(root.resolve_atom(&name.parse().assert_ok()), Some(original.global_id()));
		assert_eq!(root.membership(fixture.graph.id(), original.global_id()), Some(owners[index]));
		let occurrence = root.occurrence(fixture.graph.id(), owners[index]).assert_some();
		assert_eq!(occurrence.global_id(), original.global_id());
		assert_eq!(occurrence.label().map(|label| label.as_str()), Some(label));
		let expected = if index == 0 {
			OccurrenceState::node(Some(source.children[0]))
		} else {
			OccurrenceState::edge(
				EdgeTopology::directed([owners[0]], [owners[0]]),
				Some(7_i64.into()),
				Some(source.children[1]),
			)
		};
		assert_eq!(occurrence.state(), &expected);
		let mut read = fixture.store.begin();
		let query = read.query_snapshot().assert_ok();
		let fields = read
			.query_physical_fields(&query, &fixture.graph, owners[index])
			.assert_ok()
			.assert_some();
		assert_eq!(fields.properties.len(), 1);
		assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(value)));
	}
	for (child, image) in children {
		assert_eq!(child_image(&root, *child), *image);
	}
	let membership = |record: &RecordId| {
		root.membership(fixture.graph.id(), root.resolve_atom(record).assert_some()).assert_some()
	};
	let node = membership(&"node:d".parse().assert_ok());
	let edge = membership(&fixture.records[2]);
	let c = membership(&"node:c".parse().assert_ok());
	let from = if endpoint_overlap {
		owners[0]
	} else {
		c
	};
	assert!(node.is_node() && edge.is_edge());
	assert_eq!(
		root.occurrence(fixture.graph.id(), edge).assert_some().state(),
		&OccurrenceState::edge(EdgeTopology::directed([from], [node]), None, None)
	);
	drop(root);
	source.assert_structure(fixture);
	assert_bulk_image_with_overlays(fixture, &owners);
	super::super::super::graph_overlay_tests::label_tests::assert_label_arrays(
		&fixture.store,
		fixture.graph.id(),
	);
	[node, edge]
}

/// Reuses the exact nonuniform leaf setup from the original NODE regression.
pub(super) fn with_replacement(mut fixture: Fixture) -> (Fixture, crate::graph::GraphId) {
	let mut setup = fixture.store.begin();
	let replacement = setup.define_graph("replacement", GraphKind::Graph).assert_ok().id();
	let nodes = ["node:d", "node:e"].map(|name| {
		setup.attach_node(replacement, &name.parse().assert_ok(), None, None).assert_ok()
	});
	setup
		.attach_edge(
			replacement,
			&fixture.records[4],
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	(fixture, replacement)
}

/// Produces a singleton incidence candidate without publishing its private
/// root.
pub(super) fn leaf_roots(
	fixture: &Fixture,
	owner: AtomId,
	state: OccurrenceState,
) -> (
	crate::datastore::DatabaseRoot,
	crate::datastore::DatabaseRoot,
	crate::datastore::PublicationDelta,
) {
	let mut before = fixture.store.inner.current_root().as_ref().clone();
	local(&fixture.store).hydrate_statistics(&mut before).assert_ok();
	let mut after = before.clone();
	after.replace_occurrence_state(fixture.graph.id(), owner, state).assert_ok();
	let delta = crate::datastore::PublicationDelta {
		records: BTreeSet::new(),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::from([(fixture.graph.id(), owner)]),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	};
	(before, after, delta)
}

/// Uses the actual shared label coordinator and graph builder for resource
/// tests.
///
/// # Errors
/// Preserves source, admission, codec and I/O failures without publication.
pub(super) fn prepare_leaf(
	store: &crate::storage::local_store::LocalStore,
	roots: (&crate::datastore::DatabaseRoot, &crate::datastore::DatabaseRoot),
	delta: &crate::datastore::PublicationDelta,
) -> crate::gql_status::FerrosGrynnResult<crate::storage::local_store::point_graph::GraphUpdatePlan>
{
	let state = store.state.lock();
	let mut labels = crate::storage::local_store::point_namespace::LabelPlan::build(
		store, &state, roots.0, roots.1, delta,
	)?;
	let mut context = crate::storage::atom_store_image::value_context(
		&roots.1.schema,
		roots.1.dictionary.as_ref().clone(),
	)?;
	crate::storage::local_store::point_graph::GraphUpdatePlan::build(
		store,
		&state,
		roots.0,
		roots.1,
		delta,
		&mut labels,
		&mut context,
	)
}

/// Seeds genuine containers while preserving the existing Metagraph fixture.
pub(super) fn container_fixture(mode: IoMode, family: GraphKind) -> (Fixture, PropertyKeyId) {
	assert!(matches!(family, GraphKind::Graph | GraphKind::Metagraph));
	let (fixture, key) = family_fixture(mode, family);
	let (fixture, key) = if family == GraphKind::Graph {
		with_containers(fixture, key)
	} else {
		(fixture, key)
	};
	assert_eq!(fixture.graph.kind(), family);
	assert_eq!(
		fixture
			.store
			.inner
			.current_root()
			.schema
			.graph(fixture.graph.id())
			.assert_some()
			.reference()
			.kind(),
		family
	);
	(fixture, key)
}
