//! Differential local relation and SCC checks across incidence projections.

use super::*;
use crate::{
	graph::{EdgeEndpoint, EdgeRead, GraphRead, MutableGraph, QualifiedAtomId, TraversalIndex},
	values::PropertyMap,
};

#[test]
fn restored_general_relation_accepts_cycles_and_checks_every_reference() {
	let vertices = (0..3)
		.map(|slot| AtomId::from_parts(crate::graph::AtomKind::Node, slot, 1).unwrap())
		.collect::<Vec<_>>();
	let restore = |offsets, targets, budget| {
		ProjectedRelation::from_csr(
			vertices.clone(),
			RelationCsr {
				offsets,
				targets,
			},
			true,
			budget,
			&mut || Ok(()),
		)
	};
	let valid = restore(vec![0, 2, 3, 4], vec![0, 1, 2, 0], 1 << 20).unwrap();
	assert!(valid.has_cross_level);
	assert_eq!(valid.reverse.offsets, [0, 2, 3, 4]);
	assert_eq!(valid.reverse.targets, [0, 2, 0, 1]);
	for (offsets, targets) in [
		(vec![0, 0], vec![]),
		(vec![1, 1, 1, 1], vec![0]),
		(vec![0, 2, 1, 2], vec![1, 2]),
		(vec![0, 1, 1, 1], vec![3]),
		(vec![0, 2, 2, 2], vec![1, 1]),
		(vec![0, 2, 2, 2], vec![2, 1]),
	] {
		assert!(restore(offsets, targets, 1 << 20).is_err());
	}
	assert!(restore(vec![0, 0, 0, 0], vec![], 1).is_err());
	assert!(
		ProjectedRelation::from_csr(
			vec![vertices[0]; 3],
			RelationCsr {
				offsets: vec![0; 4],
				targets: vec![]
			},
			false,
			1 << 20,
			&mut || Ok(())
		)
		.is_err()
	);
	assert!(
		ProjectedRelation::from_csr(
			vertices,
			RelationCsr {
				offsets: vec![0; 4],
				targets: vec![]
			},
			false,
			1 << 20,
			&mut || Err(FerrosGrynnError::query_cancelled())
		)
		.is_err()
	);
}

#[test]
fn restored_condensation_rejects_malformed_csr_and_cycles() {
	let representatives = (0..3)
		.map(|slot| AtomId::from_parts(crate::graph::AtomKind::Node, slot, 1).unwrap())
		.collect::<Vec<_>>();
	let restore = |offsets, targets, components, budget| {
		ProjectedCondensation::from_csr(
			components,
			representatives.clone(),
			RelationCsr {
				offsets,
				targets,
			},
			budget,
			&mut || Ok(()),
		)
	};
	let valid = restore(vec![0, 2, 3, 3], vec![1, 2, 2], vec![0, 1, 2], 1 << 20).unwrap();
	assert_eq!(valid.reverse.offsets, vec![0, 0, 1, 3]);
	assert_eq!(valid.reverse.targets, vec![0, 0, 1]);
	assert_eq!(valid.topological_order, vec![0, 1, 2]);
	for (offsets, targets) in [
		(vec![0, 1, 1], vec![1]),
		(vec![1, 1, 1, 1], vec![1]),
		(vec![0, 2, 1, 2], vec![1, 2]),
		(vec![0, 1, 1, 1], vec![3]),
		(vec![0, 1, 1, 1], vec![0]),
		(vec![0, 2, 2, 2], vec![1, 1]),
		(vec![0, 2, 2, 2], vec![2, 1]),
		(vec![0, 1, 2, 3], vec![1, 2, 0]),
	] {
		assert!(restore(offsets, targets, Vec::new(), 1 << 20).is_err());
	}
	assert!(restore(vec![0, 0, 0, 0], Vec::new(), vec![3], 1 << 20).is_err());
	assert!(restore(vec![0, 0, 0, 0], Vec::new(), Vec::new(), 1).is_err());
	assert!(
		ProjectedCondensation::from_csr(
			Vec::new(),
			representatives,
			RelationCsr {
				offsets: vec![0, 0, 0, 0],
				targets: Vec::new()
			},
			1 << 20,
			&mut || Err(FerrosGrynnError::query_cancelled()),
		)
		.is_err()
	);
}

#[test]
fn projected_relations_and_scc_match_exact_all_pairs() {
	for seed in 0..64_usize {
		let mut graph = MutableGraph::new();
		let nodes =
			(0..5).map(|_| graph.add_node(None, PropertyMap::new()).unwrap()).collect::<Vec<_>>();
		let mut bases = Vec::new();
		for edge in 0..5 {
			bases.push(
				graph
					.add_edge(
						EdgeTopology::directed([nodes[edge]], [nodes[(seed + edge * 3) % 5]]),
						Some(if edge % 2 == 0 {
							"keep".into()
						} else {
							"drop".into()
						}),
						PropertyMap::new(),
					)
					.unwrap(),
			);
		}
		for edge in 0..3 {
			let source = [nodes[(seed + edge) % 5], bases[edge]];
			let target = [nodes[(seed / 3 + edge * 2) % 5], bases[(edge + 1) % 5]];
			let topology = match (seed + edge) % 3 {
				0 => EdgeTopology::directed(source, target),
				1 => EdgeTopology::bidirectional(source, target),
				_ => EdgeTopology::undirected(
					source.into_iter().chain(target).collect::<BTreeSet<_>>(),
				),
			};
			graph.add_edge(topology, Some("keep".into()), PropertyMap::new()).unwrap();
		}
		let all_vertices = graph
			.node_ids()
			.unwrap()
			.chain(graph.edge_ids().unwrap())
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.unwrap();
		for filtered in [false, true] {
			let topologies = graph
				.edge_ids()
				.unwrap()
				.map(|id| {
					let id = id.unwrap();
					let edge = GraphRead::edge(&graph, id).unwrap().unwrap();
					(
						id,
						EdgeRead::topology(&edge).unwrap().into_owned(),
						EdgeRead::label_text(&edge).unwrap().map(|label| label.into_owned()),
					)
				})
				.filter(|(_, _, label)| {
					!filtered || label.as_ref().map(|label| label.as_str()) == Some("keep")
				})
				.collect::<Vec<_>>();
			for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
				let exact = TraversalIndex::new(&graph, mode).unwrap();
				for direction in [
					TraversalDirection::Forward,
					TraversalDirection::Backward,
					TraversalDirection::Both,
				] {
					let relation = ProjectedRelation::build(
						all_vertices.iter().copied(),
						topologies.iter().map(|(id, topology, _)| (*id, topology)),
						mode,
						direction,
						1 << 20,
						|atom| Ok(atom.raw()),
						&mut || Ok(()),
					)
					.unwrap();
					let condensation = relation.condensation(1 << 20, &mut || Ok(())).unwrap();
					let count = relation.vertices.len();
					let mut reach = vec![vec![false; count]; count];
					for (source, atom) in relation.vertices.iter().enumerate() {
						reach[source][source] = true;
						let mut expected = BTreeSet::new();
						for step in exact.steps(&EdgeEndpoint::Local(*atom), direction) {
							if filtered
								&& !topologies.iter().any(|(id, _, _)| *id == step.via_edge())
							{
								continue;
							}
							if let Some(target) = step
								.target()
								.local_id()
								.and_then(|target| relation.ordinal_by_atom.get(&target))
							{
								expected.insert(*target);
								reach[source][*target] = true;
							}
						}
						assert_eq!(
							relation.forward.neighbours(source).collect::<BTreeSet<_>>(),
							expected,
							"seed {seed}, mode {mode:?}, direction {direction:?}"
						);
					}
					for middle in 0..count {
						for source in 0..count {
							for target in 0..count {
								reach[source][target] |=
									reach[source][middle] && reach[middle][target];
							}
						}
					}
					for source in 0..count {
						for target in 0..count {
							assert_eq!(
								condensation.component_by_vertex[source]
									== condensation.component_by_vertex[target],
								reach[source][target] && reach[target][source],
								"seed {seed}"
							);
						}
					}
					let positions = condensation
						.topological_order
						.iter()
						.enumerate()
						.map(|(position, component)| (*component, position))
						.collect::<BTreeMap<_, _>>();
					for source in 0..condensation.representatives.len() {
						for target in condensation.forward.neighbours(source) {
							assert!(positions[&source] < positions[&target]);
						}
					}
					let reversed = ProjectedRelation::build(
						all_vertices.iter().rev().copied(),
						topologies.iter().rev().map(|(id, topology, _)| (*id, topology)),
						mode,
						direction,
						1 << 20,
						|atom| Ok(atom.raw()),
						&mut || Ok(()),
					)
					.unwrap();
					assert_eq!(
						reversed.condensation(1 << 20, &mut || Ok(())).unwrap(),
						condensation
					);
				}
			}
		}
	}
}

#[test]
fn relation_budgets_cancellation_and_qualified_endpoints_are_checked() {
	let node = AtomId::from_parts(crate::graph::AtomKind::Node, 0, 1).unwrap();
	let target = AtomId::from_parts(crate::graph::AtomKind::Node, 1, 1).unwrap();
	let edge = AtomId::from_parts(crate::graph::AtomKind::Edge, 0, 1).unwrap();
	let topology = EdgeTopology::directed([EdgeEndpoint::Local(node)], [EdgeEndpoint::Nested(
		QualifiedAtomId::new(vec![node], target).unwrap(),
	)]);
	let vertices = [node, target, edge];
	let relation = ProjectedRelation::build(
		vertices.into_iter(),
		[(edge, &topology)].into_iter(),
		TraversalSemantics::Node,
		TraversalDirection::Forward,
		4096,
		|atom| Ok(atom.raw()),
		&mut || Ok(()),
	)
	.unwrap();
	assert!(relation.has_cross_level);
	assert!(relation.forward.targets.is_empty());
	assert_eq!(relation.condensation(4096, &mut || Ok(())).unwrap().representatives.len(), 2);
	assert!(
		ProjectedRelation::build(
			vertices.into_iter(),
			[(edge, &topology)].into_iter(),
			TraversalSemantics::Node,
			TraversalDirection::Forward,
			100,
			|atom| Ok(atom.raw()),
			&mut || Ok(())
		)
		.is_err()
	);
	assert!(
		ProjectedRelation::build(
			vertices.into_iter(),
			[(edge, &topology)].into_iter(),
			TraversalSemantics::Node,
			TraversalDirection::Forward,
			4096,
			|atom| Ok(atom.raw()),
			&mut || Err(FerrosGrynnError::query_cancelled())
		)
		.is_err()
	);
	let malformed =
		EdgeTopology::directed([node], [
			AtomId::from_parts(crate::graph::AtomKind::Node, 42, 1).unwrap()
		]);
	assert!(
		ProjectedRelation::build(
			vertices.into_iter(),
			[(edge, &malformed)].into_iter(),
			TraversalSemantics::Node,
			TraversalDirection::Forward,
			4096,
			|atom| Ok(atom.raw()),
			&mut || Ok(())
		)
		.is_err()
	);
	let _empty = ProjectedRelation::build(
		[].into_iter(),
		[].into_iter(),
		TraversalSemantics::Atom,
		TraversalDirection::Forward,
		1024,
		|atom| Ok(atom.raw()),
		&mut || Ok(()),
	)
	.unwrap();
}
