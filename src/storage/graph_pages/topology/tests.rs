//! Resolution preserves role order, qualified identity and format-v1 errors.
use super::*;
use crate::{
	graph::{AtomId, AtomKind, QualifiedAtomId},
	test_assertions::AssertResult as _,
};

/// Maps local addresses and one nested scope without cloning runtime topology.
fn resolve(endpoint: &EdgeEndpoint) -> FerrosGrynnResult<EncodedEndpoint> {
	let (graph_slot, atom) = match endpoint {
		EdgeEndpoint::Local(atom) => (0, *atom),
		EdgeEndpoint::Nested(qualified) => (7, qualified.atom()),
	};
	Ok(EncodedEndpoint {
		graph_slot,
		atom: DiskAtomRef::from_parts(
			atom.kind()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("null fixture endpoint"))?,
			atom.slot(),
		)?,
	})
}

/// Concrete codes depend on complete resolved identities and role cardinality.
#[test]
fn topology_resolution_preserves_codes_order_and_qualified_scope() {
	let nodes = [0, 1, 2].map(|slot| AtomId::from_parts(AtomKind::Node, slot, 0).assert_ok());
	for (topology, code, sources, targets) in [
		(
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			EdgeTopologyCode::Directed,
			vec![0],
			vec![1],
		),
		(EdgeTopology::directed([nodes[0]], [nodes[0]]), EdgeTopologyCode::Loop, vec![0], vec![0]),
		(
			EdgeTopology::directed([nodes[0], nodes[1]], [nodes[2]]),
			EdgeTopologyCode::HyperDirected,
			vec![0, 1],
			vec![2],
		),
		(
			EdgeTopology::bidirectional([nodes[0]], [nodes[1]]),
			EdgeTopologyCode::Bidirectional,
			vec![0],
			vec![1],
		),
		(
			EdgeTopology::undirected(nodes[..2].iter().copied()),
			EdgeTopologyCode::Undirected,
			vec![0, 1],
			vec![],
		),
		(EdgeTopology::undirected(nodes), EdgeTopologyCode::HyperUndirected, vec![0, 1, 2], vec![]),
	] {
		let encoded = resolve_topology(&topology, resolve).assert_ok();
		assert_eq!(encoded.code, code);
		for (role, expected_slots) in [(&encoded.source, sources), (&encoded.target, targets)] {
			let expected: Vec<_> = expected_slots
				.into_iter()
				.map(|slot| EncodedEndpoint {
					graph_slot: 0,
					atom: DiskAtomRef::from_parts(AtomKind::Node, slot).assert_ok(),
				})
				.collect();
			assert_eq!(role, &expected);
		}
	}
	let topology = EdgeTopology::directed([EdgeEndpoint::Local(nodes[0])], [EdgeEndpoint::Nested(
		QualifiedAtomId::new([nodes[2]], nodes[0]).assert_ok(),
	)]);
	let encoded = resolve_topology(&topology, resolve).assert_ok();
	assert_eq!(encoded.code, EdgeTopologyCode::Directed);
	assert_eq!(encoded.source[0].atom, encoded.target[0].atom);
	assert_eq!((encoded.source[0].graph_slot, encoded.target[0].graph_slot), (0, 7));
}

/// Source failures stop before target resolution; unsupported arity resolves in
/// order.
#[test]
fn topology_resolution_preserves_error_boundary_and_source_then_target_order() {
	let nodes = [0, 1, 2].map(|slot| AtomId::from_parts(AtomKind::Node, slot, 0).assert_ok());
	let topology = EdgeTopology::directed([nodes[0], nodes[1]], [nodes[2]]);
	let mut visited = Vec::new();
	resolve_topology(&topology, |endpoint| {
		visited.push(endpoint.local_id());
		if endpoint.local_id() == Some(nodes[1]) {
			return Err(FerrosGrynnError::invalid_argument("fixture resolution rejection"));
		}
		resolve(endpoint)
	})
	.assert_err();
	assert_eq!(visited, vec![Some(nodes[0]), Some(nodes[1])]);
	visited.clear();
	resolve_topology(&EdgeTopology::bidirectional([nodes[0], nodes[1]], [nodes[2]]), |endpoint| {
		visited.push(endpoint.local_id());
		resolve(endpoint)
	})
	.assert_err();
	assert_eq!(visited, vec![Some(nodes[0]), Some(nodes[1]), Some(nodes[2])]);
}

/// Literal layouts guard both codecs' split boundary and full qualified roles.
#[test]
fn hot_side_plan_preserves_literal_inline_scope_and_overflow_layouts() {
	for (scopes, cross, inline, graph_slot, mode, overflow) in [
		(vec![], false, [None, None], None, "none", vec![]),
		(vec![0], false, [Some(0), None], None, "none", vec![]),
		(vec![0, 0], false, [Some(0), Some(1)], None, "none", vec![]),
		(vec![0, 0, 0], false, [Some(0), Some(1)], None, "local", vec![(0, 2)]),
		(vec![], true, [None, None], None, "none", vec![]),
		(vec![7], true, [Some(0), None], Some(7), "none", vec![]),
		(vec![0, 0], true, [Some(0), Some(1)], Some(0), "none", vec![]),
		(vec![7, 7], true, [Some(0), Some(1)], Some(7), "none", vec![]),
		(vec![7, 8], true, [None, None], None, "qualified", vec![(7, 0), (8, 1)]),
		(vec![0, 7], true, [None, None], None, "qualified", vec![(0, 0), (7, 1)]),
		(vec![7, 7, 7], true, [None, None], None, "qualified", vec![(7, 0), (7, 1), (7, 2)]),
	] {
		let entries = scopes
			.into_iter()
			.zip(0_u64..)
			.map(|(graph_slot, slot)| EncodedEndpoint {
				graph_slot,
				atom: DiskAtomRef::from_parts(AtomKind::Node, slot).assert_ok(),
			})
			.collect::<Vec<_>>();
		let plan = plan_hot_side(&entries, cross);
		assert_eq!(plan.inline.map(|entry| entry.map(DiskAtomRef::slot)), inline);
		assert_eq!(plan.graph_slot, graph_slot);
		let (actual_mode, actual) = match plan.overflow {
			HotSideOverflow::None => ("none", &[][..]),
			HotSideOverflow::LocalSuffix(entries) => ("local", entries),
			HotSideOverflow::QualifiedWhole(entries) => ("qualified", entries),
		};
		assert_eq!(actual_mode, mode);
		assert_eq!(
			actual.iter().map(|entry| (entry.graph_slot, entry.atom.slot())).collect::<Vec<_>>(),
			overflow
		);
		if !actual.is_empty() {
			let offset = if mode == "local" {
				2
			} else {
				0
			};
			assert!(std::ptr::eq(actual.as_ptr(), entries[offset..].as_ptr()));
		}
	}
}
