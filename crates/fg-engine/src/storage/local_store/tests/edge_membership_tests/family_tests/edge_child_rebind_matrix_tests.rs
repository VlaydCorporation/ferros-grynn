//! Child-only EDGE publication retains every accepted role representation.

use fg_runtime::__internal::EdgeEndpointInternal as _;

use super::{
	edge_child_rebind_support::{Source, edge_change, endpoint_row},
	metagraph::with_replacement,
	transition_rewire_tests::{portal, rewrite},
	*,
};
use crate::{
	datastore::OccurrenceState,
	graph::{EdgeEndpoint, GraphId, QualifiedAtomId},
	storage::graph_pages::{EndpointSide, decode_cross_level_overflow},
};

/// Sync retains local/qualified roles, both side tags and E-E capabilities.
#[test]
fn edge_child_rebind_sync_role_matrix_retains_source_bytes() {
	role_matrix(IoMode::Sync);
}

/// NativeAsync reopens the exact same retained source and final child binding.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_child_rebind_native_async_role_matrix_retains_source_bytes() {
	role_matrix(IoMode::NativeAsync);
}

/// All role and metadata setup precedes the captured child-only candidate.
pub(super) fn shape_fixture_with_family(
	mode: IoMode,
	shape: usize,
	family: GraphKind,
) -> (Fixture, PropertyKeyId, GraphId, EdgeTopology) {
	let (fixture, key) = super::metagraph::container_fixture(mode, family);
	let (mut fixture, replacement) = with_replacement(fixture);
	let [a, b] = fixture.nodes;
	let short = qualified_guard_tests::short_topology(&fixture);
	let wide = qualified_guard_tests::wide_topology(&fixture);
	let topology = match shape {
		0 => EdgeTopology::directed(
			(0..2050).map(|index| {
				if index % 2 == 0 {
					a
				} else {
					b
				}
			}),
			(0..514).map(|index| {
				if index % 2 == 0 {
					b
				} else {
					a
				}
			}),
		),
		1 => wide,
		2 => EdgeTopology::directed([EdgeEndpoint::Local(b)], wide.source().iter().cloned()),
		3 => EdgeTopology::undirected([
			short.source()[0].clone(),
			EdgeEndpoint::Local(a),
			short.source()[0].clone(),
		]),
		4 => EdgeTopology::bidirectional([short.source()[0].clone()], [EdgeEndpoint::Local(b)]),
		5 => {
			let root = fixture.store.inner.current_root();
			let global = root.resolve_atom(&fixture.records[0]).assert_some();
			let endpoint = root.membership(fixture.graph.id(), global).assert_some();
			EdgeTopology::directed([endpoint], [endpoint])
		}
		6 => {
			let root = fixture.store.inner.current_root();
			let child = root.schema.named_graph("child_node").assert_some().reference().id();
			let node_global =
				root.resolve_atom(&"node:child_node".parse().assert_ok()).assert_some();
			let terminal = root.membership(child, node_global).assert_some();
			let container_global =
				root.resolve_atom(&"node:container".parse().assert_ok()).assert_some();
			let container = root.membership(fixture.graph.id(), container_global).assert_some();
			drop(root);
			let mut setup = fixture.store.begin();
			let edge = setup
				.attach_edge(
					child,
					&fixture.records[3],
					None,
					EdgeTopology::directed([terminal], [terminal]),
					None,
					None,
				)
				.assert_ok();
			setup.commit().assert_ok();
			let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([container], edge).assert_ok());
			EdgeTopology::directed([nested.clone(), EdgeEndpoint::Local(a), nested], [short
				.source()[0]
				.clone()])
		}
		_ => {
			assert_eq!(shape, 7);
			EdgeTopology::directed(wide.source().iter().cloned(), Vec::<EdgeEndpoint>::new())
		}
	};
	let (_, _, child) = portal(&fixture);
	let mut setup = rewrite(
		&fixture,
		OccurrenceState::edge(topology.clone(), Some(11_i64.into()), Some(child)),
	);
	setup
		.set_occurrence_label(
			fixture.graph.id(),
			&"edge:portal".parse().assert_ok(),
			Some("leaf-matrix".into()),
		)
		.assert_ok();
	setup
		.set_overlay(fixture.graph.id(), &"edge:portal".parse().assert_ok(), key, 777_i64.into())
		.assert_ok();
	setup.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
	(fixture, key, replacement, topology)
}

/// Independent decoding checks role order, multiplicity and exact page count.
fn role(source: &Source, side: EndpointSide) -> (Vec<(u32, DiskAtomRef)>, usize, usize) {
	let hot = &source.hot;
	let (count, inline, local, qualified) = match side {
		EndpointSide::Source => {
			(hot.source_count, hot.source_inline, hot.source_overflow, hot.source_subgraph_overflow)
		}
		EndpointSide::Target => {
			(hot.target_count, hot.target_inline, hot.target_overflow, hot.target_subgraph_overflow)
		}
	};
	let mut result = Vec::new();
	let mut local_pages = 0;
	let mut qualified_pages = 0;
	let files = source.graph_files;
	if let Some(first) = qualified {
		assert_eq!(inline, [None; 2]);
		assert_eq!(local, None);
		let mut current = Some(first);
		let mut visited = BTreeSet::new();
		while let Some(number) = current {
			assert!(visited.insert(number));
			let (next, entries) = decode_cross_level_overflow(
				source.page(PageKey::new(files.space(), files.cross_level_adjacency, number)),
				number,
			)
			.assert_ok();
			assert!(!entries.is_empty());
			for entry in entries {
				assert_eq!(entry.side, side);
				result.push((entry.graph_slot, entry.atom));
			}
			assert!(result.len() <= usize::try_from(count).assert_ok());
			qualified_pages += 1;
			current = next;
		}
	} else {
		let scope = hot.endpoint_graph_slot(side).unwrap_or(0);
		result.extend(inline.into_iter().flatten().map(|atom| (scope, atom)));
		let mut current = local;
		let mut visited = BTreeSet::new();
		while let Some(number) = current {
			assert!(visited.insert(number));
			let (next, entries) = decode_adjacency_overflow(
				source.page(PageKey::new(files.space(), files.adjacency, number)),
				number,
			)
			.assert_ok();
			assert!(!entries.is_empty());
			result.extend(entries.into_iter().map(|atom| (0, atom)));
			assert!(result.len() <= usize::try_from(count).assert_ok());
			local_pages += 1;
			current = next;
		}
	}
	assert_eq!(result.len(), usize::try_from(count).assert_ok());
	(result, local_pages, qualified_pages)
}

/// Literal payload and postings assertions precede offset-normalizing bulk.
fn assert_payloads(
	fixture: &Fixture,
	source: &Source,
	key: PropertyKeyId,
	topology: &EdgeTopology,
) {
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let fields = read
		.query_physical_fields(&snapshot, &fixture.graph, source.owner.1)
		.assert_ok()
		.assert_some();
	assert_eq!(fields.properties.len(), 1);
	assert_eq!(fields.properties.get(key), Some(&crate::values::Value::from(777_i64)));
	let rows = topology
		.source()
		.iter()
		.map(|endpoint| (endpoint, 0_u8))
		.chain(topology.target().iter().map(|endpoint| (endpoint, 1_u8)))
		.filter(|(endpoint, _)| endpoint.kind() == Some(AtomKind::Edge))
		.map(|(endpoint, side)| {
			let (scope, atom) = endpoint_row(source, endpoint);
			(
				scope,
				atom.raw(),
				DiskAtomRef::from_parts(AtomKind::Edge, source.owner.1.slot()).assert_ok().raw(),
				side,
			)
		})
		.collect::<BTreeSet<_>>();
	if !rows.is_empty() {
		super::super::endpoint_publication_tests::assert_postings(
			fixture,
			&rows.into_iter().collect::<Vec<_>>(),
		);
	}
}

/// Eight isolated accepted sources keep only one final GraphId different.
fn role_matrix(mode: IoMode) {
	role_matrix_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn role_matrix_with_family(mode: IoMode, family: GraphKind) {
	for shape in 0..8 {
		let (mut fixture, key, replacement, topology) =
			shape_fixture_with_family(mode, shape, family);
		let source = Source::capture(&fixture);
		assert_eq!(source.hot.weight(), Some(11.0));
		assert_eq!(source.hot.transition_subgraph_slot, Some(source.slot));
		let expected_source = topology
			.source()
			.iter()
			.map(|endpoint| endpoint_row(&source, endpoint))
			.collect::<Vec<_>>();
		let expected_target = topology
			.target()
			.iter()
			.map(|endpoint| endpoint_row(&source, endpoint))
			.collect::<Vec<_>>();
		let (actual_source, source_local, source_qualified) = role(&source, EndpointSide::Source);
		let (actual_target, target_local, target_qualified) = role(&source, EndpointSide::Target);
		assert_eq!(actual_source, expected_source);
		assert_eq!(actual_target, expected_target);
		let counts = match shape {
			0 => (2, 1, 0, 0),
			1 | 7 => (0, 0, 3, 0),
			2 => (0, 0, 0, 3),
			3 | 6 => (0, 0, 1, 0),
			_ => (0, 0, 0, 0),
		};
		assert_eq!((source_local, target_local, source_qualified, target_qualified), counts);
		assert_payloads(&fixture, &source, key, &topology);
		edge_change(&fixture, replacement).commit().assert_ok();
		source.assert_current(&fixture, replacement, (2, 1, 2, 1));
		assert_payloads(&fixture, &source, key, &topology);
		assert_bulk_image_with_overlays(&fixture, &[source.owner.1]);
		// Recover from WAL directly; no checkpoint may conceal a split arena.
		fixture = reopen(fixture);
		assert_eq!(fixture.store.local_storage_status().assert_some().options().io_mode, mode);
		source.assert_current(&fixture, replacement, (2, 1, 2, 1));
		assert_payloads(&fixture, &source, key, &topology);
		assert_bulk_image_with_overlays(&fixture, &[source.owner.1]);
	}
}
