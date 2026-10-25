//! Local EDGE endpoints publish selected trees with unchanged data files.

/// Publication, recovery and snapshot guards for the selected endpoint tree.
mod guards;
/// Retained source validation at the WAL/header trust boundary.
mod source_tests;

/// Literal direction and endpoint-role physical query oracles.
mod direction_tests;
/// Old mixed-kind WAL role pages survive overflow shrink and backfill.
mod overflow_tests;
/// Snapshot ownership and recovery retire only unreferenced endpoint trees.
mod retirement_tests;

/// First capability identities and persistent empty endpoint images.
mod first_capability_tests;

/// First-capability failures, budgets and independent accepted replay.
mod first_capability_guards;
/// Accepted source consistency rejects before any unreachable output.
mod first_capability_source_tests;

use std::ops::Bound;

use super::{support::*, *};
use crate::{
	datastore::{OccurrenceState, SystemIndex},
	gql_status::FerrosGrynnResult,
	storage::{
		catalog::FileCatalogEntry,
		edge_endpoint_index::decode_header,
		graph_index::visit_unique_range,
		image_io::read_pinned_page_cached,
		page::{AlignedPage, PageType},
	},
	values::storage::DiskAtomRef,
};

/// The existing capability must avoid rebuilding AtomStore and graph Hot files.
#[test]
fn edge_endpoint_sync_dml_retains_data_files_and_old_tree() {
	endpoint_dml(IoMode::Sync, true);
}

/// NativeAsync must retain the same publication and snapshot contract.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_endpoint_native_async_dml_retains_data_files_and_old_tree() {
	endpoint_dml(IoMode::NativeAsync, true);
}

/// The unchanged full publisher remains a semantic control for the RED phase.
#[test]
fn edge_endpoint_existing_capability_dml_semantic_control() {
	endpoint_dml(IoMode::Sync, false);
}

/// Reuses the ordinary incidence fixture and publishes a real E-E capability.
fn endpoint_fixture(io_mode: IoMode) -> (Fixture, AtomId, AtomId) {
	let mut fixture = fixture(io_mode, 3, 1);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&fixture.records[0]).assert_some();
	let endpoint = root.membership(fixture.graph.id(), global).assert_some();
	drop(root);
	let mut seed = fixture.store.begin();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let node = "node:a".parse::<RecordId>().assert_ok();
	seed.attach_node(other.id(), &node, None, None).assert_ok();
	let connector = seed
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[1],
			None,
			EdgeTopology::directed([endpoint], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	seed.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	// A catalog-only commit creates a genuinely WAL-backed source header.
	let mut warm = fixture.store.begin();
	warm.intern_property("endpoint_header_source").assert_ok();
	warm.commit().assert_ok();
	(fixture, endpoint, connector)
}

/// Literal tuples are independent of the runtime endpoint-posting builder.
pub(super) type Posting = (u32, u64, u64, u8);

/// Checks the complete concrete tree, including key/value and reserved bytes.
fn postings(
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	graph: GraphId,
	capability: SystemIndex,
	revision: u64,
	pages: u64,
) -> Vec<Posting> {
	let mut page = Box::new(AlignedPage::zeroed());
	read(PageNo::new(0), &mut page).assert_ok();
	let header = decode_header(&page, graph, capability, revision, pages).assert_ok();
	let mut result = Vec::new();
	visit_unique_range(
		&mut read,
		header.root,
		PageType::IndexEdgeEndpointEntry,
		&Bound::Unbounded,
		&Bound::Unbounded,
		&mut |key, value: DiskAtomRef| {
			assert_eq!(key.len(), 24);
			assert_eq!(&key[21..], &[0; 3]);
			assert!(key[20] <= 1);
			let scope = u32::from_be_bytes(key[..4].try_into().assert_ok());
			let endpoint = u64::from_be_bytes(key[4..12].try_into().assert_ok());
			let connector = u64::from_be_bytes(key[12..20].try_into().assert_ok());
			assert_eq!(value.raw(), connector);
			assert_eq!(value.kind(), AtomKind::Edge);
			result.push((scope, endpoint, connector, key[20]));
			Ok(())
		},
	)
	.assert_ok();
	assert_eq!(header.posting_count, result.len() as u64);
	result
}

/// Checks the live binding rather than accepting a materialized query fallback.
pub(super) fn assert_postings(fixture: &Fixture, expected: &[Posting]) {
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
	assert_eq!(binding.root, PageNo::new(0));
	let space = DataSpaceId::Graph(fixture.graph.id());
	let actual = postings(
		|number, page| {
			store.read_update_page_into(&state, PageKey::new(space, binding.file_id, number), page)
		},
		fixture.graph.id(),
		capability,
		root.commit_ts,
		store.visible_pages(&state, space, binding.file_id).assert_ok(),
	);
	assert_eq!(actual, expected);
}

/// Endpoint replacement may change selected graph indexes, never unrelated
/// data.
fn assert_data_files(fixture: &Fixture, previous: &[FileCatalogEntry]) {
	let store = local(&fixture.store);
	let state = store.state.lock();
	for entry in previous {
		let selected = entry.space_id == DataSpaceId::Graph(fixture.graph.id())
			&& matches!(entry.file_id.kind(), Some(DataFileKind::Index | DataFileKind::Statistics));
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"LOCAL_EDGE_ENDPOINT_REBUILT_DATA_FILES: {}",
				entry.path.as_str()
			);
		}
	}
}

/// A real local connector target has a literal physical traversal candidate.
fn assert_walk(fixture: &Fixture, endpoint: AtomId, expected: &[(AtomId, AtomId)]) {
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let walk = read
		.query_physical_traversal(&snapshot, &fixture.graph, endpoint, true, false, true)
		.assert_ok()
		.assert_some();
	let mut actual =
		walk.candidates.iter().map(|step| (step.edge, step.target)).collect::<Vec<_>>();
	actual.sort();
	let mut expected = expected.to_vec();
	expected.sort();
	assert_eq!(actual, expected);
}

/// Attach preserves old WAL headers; subsequent rewires/detaches retain
/// capability.
fn endpoint_dml(io_mode: IoMode, require_point: bool) {
	let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
	let endpoint_ref = DiskAtomRef::from_parts(AtomKind::Edge, endpoint.slot()).assert_ok().raw();
	let connector_ref = DiskAtomRef::from_parts(AtomKind::Edge, connector.slot()).assert_ok().raw();
	let old_rows = [(0, endpoint_ref, connector_ref, 0)];
	assert_postings(&fixture, &old_rows);
	assert_walk(&fixture, endpoint, &[(connector, fixture.nodes[1])]);
	let root = fixture.store.inner.current_root();
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let state = store.state.lock();
	let previous = state.file_catalog.files.clone();
	let home = state.database_superblock.checkpoint_lsn;
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let header_key =
		PageKey::new(DataSpaceId::Graph(fixture.graph.id()), binding.file_id, binding.root);
	let file = state.registry.resolve(header_key).assert_ok();
	let capability =
		root.schema.graph(fixture.graph.id()).assert_some().edge_endpoint_index().assert_some();
	drop(state);
	{
		let frames = store.wal.visible_frames(&[header_key], pin.0.snapshot);
		assert!(frames.frames()[0].assert_some().frame_lsn() > home);
	}
	let mut write = fixture.store.begin();
	let added = write
		.attach_edge(
			fixture.graph.id(),
			&fixture.records[2],
			None,
			EdgeTopology::directed([endpoint], [fixture.nodes[0]]),
			None,
			None,
		)
		.assert_ok();
	write.commit().assert_ok();
	if require_point {
		assert_data_files(&fixture, &previous);
	}
	let added_ref = DiskAtomRef::from_parts(AtomKind::Edge, added.slot()).assert_ok().raw();
	let rows = [(0, endpoint_ref, connector_ref, 0), (0, endpoint_ref, added_ref, 0)];
	assert_postings(&fixture, &rows);
	assert_walk(&fixture, endpoint, &[(connector, fixture.nodes[1]), (added, fixture.nodes[0])]);
	assert_bulk_image(&fixture);
	for checkpoint in [false, true] {
		if checkpoint {
			assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
			let advanced = store.state.lock().database_superblock.checkpoint_lsn;
			assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
		}
		let actual = postings(
			|number, page| {
				read_pinned_page_cached(
					&store.wal,
					&file,
					&store.engine,
					pin.0.snapshot,
					pin.0.home_generation,
					pin.0.home_ceiling,
					PageKey::new(DataSpaceId::Graph(fixture.graph.id()), binding.file_id, number),
					page,
				)
			},
			fixture.graph.id(),
			capability,
			root.commit_ts,
			file.page_count(),
		);
		assert_eq!(actual, old_rows);
	}
	drop(file);
	drop(pin);
	drop(root);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_postings(&fixture, &rows);
	finish_dml(&fixture, endpoint, [connector_ref, added_ref], require_point);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_postings(&fixture, &[]);
	assert_bulk_image(&fixture);
	if require_point {
		assert_data_files(&fixture, &previous);
	}
}

/// Swapped/duplicate roles and the last deletion use exact canonical postings.
fn finish_dml(fixture: &Fixture, endpoint: AtomId, connectors: [u64; 2], require_point: bool) {
	let endpoint_ref = DiskAtomRef::from_parts(AtomKind::Edge, endpoint.slot()).assert_ok().raw();
	for (topology, sides) in [
		(EdgeTopology::directed([fixture.nodes[0]], [endpoint]), vec![1]),
		(EdgeTopology::directed([endpoint, endpoint], [endpoint, endpoint]), vec![0, 1]),
	] {
		let previous = local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut write = fixture.store.begin();
		write
			.replace_occurrence_state(
				fixture.graph.id(),
				&fixture.records[2],
				OccurrenceState::edge(topology, None, None),
			)
			.assert_ok();
		write.commit().assert_ok();
		if require_point {
			assert_data_files(fixture, &previous);
		}
		let mut expected = vec![(0, endpoint_ref, connectors[0], 0)];
		expected.extend(sides.into_iter().map(|side| (0, endpoint_ref, connectors[1], side)));
		assert_postings(fixture, &expected);
		assert_bulk_image(fixture);
	}
	for index in [1, 2] {
		let previous = local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut write = fixture.store.begin();
		write.detach_occurrence(fixture.graph.id(), &fixture.records[index], false).assert_ok();
		write.commit().assert_ok();
		if require_point {
			assert_data_files(fixture, &previous);
		}
		let expected = if index == 1 {
			vec![(0, endpoint_ref, connectors[1], 0), (0, endpoint_ref, connectors[1], 1)]
		} else {
			Vec::new()
		};
		assert_postings(fixture, &expected);
		assert_bulk_image(fixture);
	}
}
