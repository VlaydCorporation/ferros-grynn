//! Lifecycle label images must use the same sparse publication as plain slots.

use super::*;

/// Existing NODE Base owners keep database-global files when attached with a
/// label.
#[test]
fn label_lifecycle_node_sync_preserves_sparse_files() {
	labelled_attach(IoMode::Sync, AtomKind::Node);
}

/// Actual NativeAsync has the same labelled NODE publication contract.
#[cfg(feature = "native-async-io")]
#[test]
fn label_lifecycle_node_native_async_preserves_sparse_files() {
	labelled_attach(IoMode::NativeAsync, AtomKind::Node);
}

/// A labelled EDGE changes only its Hot slot and touched endpoint adjacency.
#[test]
fn label_lifecycle_edge_sync_preserves_sparse_files() {
	labelled_attach(IoMode::Sync, AtomKind::Edge);
}

/// Actual NativeAsync preserves the labelled EDGE image after cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn label_lifecycle_edge_native_async_preserves_sparse_files() {
	labelled_attach(IoMode::NativeAsync, AtomKind::Edge);
}

/// Both an existing name and a leading insertion use source/final namespaces.
fn labelled_attach(io_mode: IoMode, kind: AtomKind) {
	for label in ["M", "A"] {
		let (mut fixture, pending) = membership_fixture(io_mode);
		let graph = fixture.graph.id();
		let local = local_store(&fixture.store);
		let atoms = local.state.lock().bindings.atoms.atoms_hot;
		let files = local.state.lock().file_catalog.files.clone();
		let arrays = capture_cold_label_arrays(&fixture.store, graph);
		let record = match kind {
			AtomKind::Node => &pending[0],
			AtomKind::Edge => &pending[1],
		};
		let mut old = fixture.store.begin();
		assert!(old.membership(graph, record).assert_ok().is_none());
		let mut write = fixture.store.begin();
		match kind {
			AtomKind::Node => {
				write.attach_node(graph, record, Some(label.into()), None).assert_ok();
			}
			AtomKind::Edge => {
				write
					.attach_edge(
						graph,
						record,
						Some(label.into()),
						EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
						None,
						None,
					)
					.assert_ok();
			}
		}
		write.commit().assert_ok();
		assert_eq!(
			local.state.lock().bindings.atoms.atoms_hot,
			atoms,
			"labelled lifecycle rebuilt database-global files"
		);
		super::super::super::namespace_publication_tests::assert_namespace_files(&fixture, &files);
		assert!(old.membership(graph, record).assert_ok().is_none());
		assert!(fixture.store.begin().membership(graph, record).assert_ok().is_some());
		oracles::assert_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		for array in &arrays {
			array.assert_unchanged();
		}
		drop(arrays);
		drop(old);
		fixture = reopen_checkpointed(fixture, io_mode);
		oracles::assert_image(&fixture);
	}
}

/// Removing the last labelled NODE updates the dictionary ceiling without a
/// suffix.
#[test]
fn label_lifecycle_last_node_label_sync_shrinks_dictionary() {
	last_label(IoMode::Sync, AtomKind::Node);
}

/// Actual NativeAsync restores the empty NODE namespace from the final header.
#[cfg(feature = "native-async-io")]
#[test]
fn label_lifecycle_last_node_label_native_async_shrinks_dictionary() {
	last_label(IoMode::NativeAsync, AtomKind::Node);
}

/// Unlabelled endpoints do not keep the removed EDGE namespace alive.
#[test]
fn label_lifecycle_last_edge_label_sync_shrinks_dictionary() {
	last_label(IoMode::Sync, AtomKind::Edge);
}

/// Actual NativeAsync uses the same final EDGE dictionary ceiling.
#[cfg(feature = "native-async-io")]
#[test]
fn label_lifecycle_last_edge_label_native_async_shrinks_dictionary() {
	last_label(IoMode::NativeAsync, AtomKind::Edge);
}

/// The empty companion graph isolates the three-page to one-page transition.
fn last_label(io_mode: IoMode, kind: AtomKind) {
	let (mut fixture, pending) = membership_fixture(io_mode);
	let graph = fixture
		.store
		.inner
		.current_root()
		.schema
		.named_graph("other")
		.assert_some()
		.reference()
		.id();
	let record = match kind {
		AtomKind::Node => &pending[0],
		AtomKind::Edge => &pending[1],
	};
	let mut seed = fixture.store.begin();
	match kind {
		AtomKind::Node => {
			seed.attach_node(graph, record, Some("M".into()), None).assert_ok();
		}
		AtomKind::Edge => {
			let left = seed.attach_node(graph, &fixture.records[0], None, None).assert_ok();
			let right = seed.attach_node(graph, &fixture.records[1], None, None).assert_ok();
			seed.attach_edge(
				graph,
				record,
				Some("M".into()),
				EdgeTopology::directed([left], [right]),
				None,
				None,
			)
			.assert_ok();
		}
	}
	seed.commit().assert_ok();
	fixture = reopen_checkpointed(fixture, io_mode);
	let local = local_store(&fixture.store);
	let atoms = local.state.lock().bindings.atoms.atoms_hot;
	assert_eq!(local.state.lock().graph_superblocks[&graph].labels_pages, 3);
	let arrays = capture_cold_label_arrays(&fixture.store, graph);
	let mut old = fixture.store.begin();
	assert!(old.membership(graph, record).assert_ok().is_some());
	let mut remove = fixture.store.begin();
	remove.detach_occurrence(graph, record, false).assert_ok();
	remove.commit().assert_ok();
	assert_eq!(
		local.state.lock().bindings.atoms.atoms_hot,
		atoms,
		"labelled lifecycle rebuilt database-global files"
	);
	assert_eq!(local.state.lock().graph_superblocks[&graph].labels_pages, 1);
	assert_label_arrays(&fixture.store, graph);
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	for array in &arrays {
		array.assert_unchanged();
	}
	drop(arrays);
	assert!(old.membership(graph, record).assert_ok().is_some());
	drop(old);
	fixture = reopen_checkpointed(fixture, io_mode);
	assert!(fixture.store.begin().membership(graph, record).assert_ok().is_none());
	assert_eq!(local_store(&fixture.store).state.lock().graph_superblocks[&graph].labels_pages, 1);
	assert_label_arrays(&fixture.store, graph);
}

/// Pins old files using logical postings; the first physical read follows
/// Passive.
pub(super) fn capture_cold_label_arrays(
	store: &Datastore,
	graph: GraphId,
) -> Vec<LabelArraySnapshot> {
	use crate::storage::{local_store::point_label, scoped_index_image::label_filename};
	let root = store.inner.current_root();
	let local = local_store(store);
	let state = local.state.lock();
	root.indexes
		.graphs
		.get(&graph)
		.assert_some()
		.labels
		.iter()
		.map(|((kind, name), members)| {
			let id =
				point_label::label_id(&root, graph, *kind, Some(name)).assert_ok().assert_some();
			let suffix = format!("/{}", label_filename(*kind, id));
			let entry = state
				.file_catalog
				.files
				.iter()
				.find(|entry| {
					entry.space_id == DataSpaceId::Graph(graph)
						&& entry.path.as_str().ends_with(&suffix)
				})
				.assert_some();
			let file = state.registry.resolve_file(entry.space_id, entry.file_id).assert_ok();
			let mut atoms =
				members.iter().map(|atom| local_address(*atom).assert_ok()).collect::<Vec<_>>();
			atoms.sort_unstable();
			LabelArraySnapshot {
				file,
				kind: *kind,
				atoms,
			}
		})
		.collect()
}
