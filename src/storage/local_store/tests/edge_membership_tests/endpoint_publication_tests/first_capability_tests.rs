//! First local E-E capability creation retains existing data-file identities.

use super::*;

/// First-capability paths include persistent empty images after transient
/// roles.
#[derive(Clone, Copy)]
pub(super) enum Change {
	/// Attach a previously absent local connector.
	Attach,
	/// Rewire an existing NODE-only connector to an EDGE endpoint.
	Rewire,
	/// Remove the first connector before the same transaction commits.
	AttachDetach,
	/// Restore the original NODE-only topology before commit.
	RewireRestore,
}

impl Change {
	/// Only rewire cases start with an existing connector occurrence.
	const fn has_connector(self) -> bool {
		matches!(self, Self::Rewire | Self::RewireRestore)
	}

	/// The capability remains even when its last posting disappears privately.
	const fn keeps_posting(self) -> bool {
		matches!(self, Self::Attach | Self::Rewire)
	}
}

/// First E-E attachment must retain AtomStore and graph data files.
#[test]
fn endpoint_first_sync_attach_retains_data_files() {
	first_capability(IoMode::Sync, Change::Attach, true);
}

/// NativeAsync publishes the first binding without a full data generation.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_attach_retains_data_files() {
	first_capability(IoMode::NativeAsync, Change::Attach, true);
}

/// An existing connector can establish the capability through a rewire.
#[test]
fn endpoint_first_sync_rewire_retains_data_files() {
	first_capability(IoMode::Sync, Change::Rewire, true);
}

/// NativeAsync preserves old NODE adjacency while first E-E roles publish.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_rewire_retains_data_files() {
	first_capability(IoMode::NativeAsync, Change::Rewire, true);
}

/// Transient attachment must still create an empty durable endpoint image.
#[test]
fn endpoint_first_sync_attach_detach_keeps_empty_capability() {
	first_capability(IoMode::Sync, Change::AttachDetach, true);
}

/// NativeAsync reopens the same empty capability after transient attachment.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_attach_detach_keeps_empty_capability() {
	first_capability(IoMode::NativeAsync, Change::AttachDetach, true);
}

/// Restoring NODE-only roles must not lose the newly allocated capability.
#[test]
fn endpoint_first_sync_rewire_restore_keeps_empty_capability() {
	first_capability(IoMode::Sync, Change::RewireRestore, true);
}

/// NativeAsync publishes an empty tree even when final postings are unchanged.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_rewire_restore_keeps_empty_capability() {
	first_capability(IoMode::NativeAsync, Change::RewireRestore, true);
}

/// The unchanged full publisher validates the semantic fixture before
/// admission.
#[test]
fn endpoint_first_existing_full_publisher_semantic_control() {
	let modes = [
		IoMode::Sync,
		#[cfg(feature = "native-async-io")]
		IoMode::NativeAsync,
	];
	for mode in modes {
		for change in [Change::Attach, Change::Rewire, Change::AttachDetach, Change::RewireRestore]
		{
			first_capability(mode, change, false);
		}
	}
}

/// Starts without any capability and warms only catalog pages into real WAL.
pub(super) fn first_fixture(io_mode: IoMode, change: Change) -> (Fixture, AtomId, Option<AtomId>) {
	let mut fixture = fixture(io_mode, 3, 1);
	let source = fixture.store.inner.current_root();
	let global = source.resolve_atom(&fixture.records[0]).assert_some();
	let endpoint = source.membership(fixture.graph.id(), global).assert_some();
	drop(source);
	let mut seed = fixture.store.begin();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let node = "node:a".parse::<RecordId>().assert_ok();
	seed.attach_node(other.id(), &node, None, None).assert_ok();
	let connector = change.has_connector().then(|| {
		seed.attach_edge(
			fixture.graph.id(),
			&fixture.records[1],
			None,
			EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok()
	});
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let mut warm = fixture.store.begin();
	warm.intern_property("first_endpoint_source").assert_ok();
	warm.commit().assert_ok();
	assert!(
		fixture
			.store
			.inner
			.current_root()
			.schema
			.graph(fixture.graph.id())
			.assert_some()
			.edge_endpoint_index()
			.is_none()
	);
	assert!(
		local(&fixture.store)
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.edge_endpoint
			.is_none()
	);
	(fixture, endpoint, connector)
}

/// Reads a real binding/header even when no endpoint postings remain.
pub(super) fn assert_first_image(fixture: &Fixture, endpoint: AtomId, connector: Option<AtomId>) {
	let raw = |edge: AtomId| DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok().raw();
	let rows = connector
		.into_iter()
		.map(|connector| (0, raw(endpoint), raw(connector), 0))
		.collect::<Vec<_>>();
	assert_postings(fixture, &rows);
	let steps =
		connector.into_iter().map(|connector| (connector, fixture.nodes[1])).collect::<Vec<_>>();
	assert_walk(fixture, endpoint, &steps);
	assert_bulk_image(fixture);
	let store = local(&fixture.store);
	let state = store.state.lock();
	let root = fixture.store.inner.current_root();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let capability =
		root.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().assert_some();
	assert_eq!(capability.version().get(), 1);
	let space = DataSpaceId::Graph(fixture.graph.id());
	let pages = store.visible_pages(&state, space, binding.file_id).assert_ok();
	let page = store
		.read_update_page(&state, PageKey::new(space, binding.file_id, binding.root))
		.assert_ok();
	let header =
		decode_header(&page, fixture.graph.id(), capability, root.commit_ts, pages).assert_ok();
	assert_eq!(binding.root, PageNo::new(0));
	assert_eq!(header.root, PageNo::new(1));
	assert_eq!(pages, 2);
}

/// Old NODE-only snapshots cannot acquire the connector's new E-E topology.
fn assert_old_node_walk(
	read: &Transaction,
	snapshot: &crate::datastore::QuerySnapshot,
	fixture: &Fixture,
	endpoint: AtomId,
	connector: Option<AtomId>,
) {
	let walk = read
		.query_physical_traversal(snapshot, &fixture.graph, fixture.nodes[1], true, false, false)
		.assert_ok()
		.assert_some();
	let actual = walk.candidates.iter().map(|step| (step.edge, step.target)).collect::<Vec<_>>();
	let expected =
		connector.into_iter().map(|connector| (connector, fixture.nodes[0])).collect::<Vec<_>>();
	assert_eq!(actual, expected);
	// Backward distinguishes Attach too: the new connector targets this node.
	let backward = read
		.query_physical_traversal(snapshot, &fixture.graph, fixture.nodes[1], false, true, false)
		.assert_ok()
		.assert_some();
	let actual =
		backward.candidates.iter().map(|step| (step.edge, step.target)).collect::<Vec<_>>();
	assert_eq!(actual, [(endpoint, fixture.nodes[0])]);
}

/// Every accepted first capability advances its graph version exactly once.
fn first_capability(io_mode: IoMode, change: Change, require_point: bool) {
	let (mut fixture, endpoint, previous_connector) = first_fixture(io_mode, change);
	let source = fixture.store.inner.current_root();
	let old_schema = source.schema.graph(fixture.graph.id()).assert_some();
	let files = local(&fixture.store).state.lock().file_catalog.files.clone();
	let home = local(&fixture.store).state.lock().database_superblock.checkpoint_lsn;
	let mut old = fixture.store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	assert_old_node_walk(&old, &snapshot, &fixture, endpoint, previous_connector);
	let topology = EdgeTopology::directed([endpoint], [fixture.nodes[1]]);
	let mut write = fixture.store.begin();
	let connector = if change.has_connector() {
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[1],
				OccurrenceState::edge(topology, None, None),
			)
			.assert_ok();
		previous_connector.assert_some()
	} else {
		write
			.attach_edge(fixture.graph.id(), &fixture.records[1], None, topology, None, None)
			.assert_ok()
	};
	match change {
		Change::AttachDetach => {
			write.detach_occurrence(fixture.graph.id(), &fixture.records[1], false).assert_ok();
		}
		Change::RewireRestore => {
			write
				.replace_occurrence_state(
					fixture.graph.id(),
					&fixture.records[1],
					OccurrenceState::edge(
						EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
						None,
						None,
					),
				)
				.assert_ok();
		}
		Change::Attach | Change::Rewire => {}
	}
	write.commit().assert_ok();
	if require_point {
		assert_data_files(&fixture, &files);
	}
	let schema = fixture.store.inner.current_root().schema.graph(fixture.graph.id()).assert_some();
	assert_eq!(schema.version().get(), old_schema.version().get() + 1);
	let capability = schema.edge_endpoint_index().assert_some();
	assert_first_image(&fixture, endpoint, change.keeps_posting().then_some(connector));
	assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
	assert!(local(&fixture.store).state.lock().database_superblock.checkpoint_lsn > home);
	assert_old_node_walk(&old, &snapshot, &fixture, endpoint, previous_connector);
	assert!(old_schema.edge_endpoint_index().is_none());
	drop(snapshot);
	drop(old);
	drop(source);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(
		fixture
			.store
			.inner
			.current_root()
			.schema
			.graph(fixture.graph.id())
			.assert_some()
			.edge_endpoint_index(),
		Some(capability)
	);
	assert_first_image(&fixture, endpoint, change.keeps_posting().then_some(connector));
	if require_point {
		assert_data_files(&fixture, &files);
	}
}
