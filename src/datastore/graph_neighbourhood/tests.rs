//! Differential distinct-radius, sketch-union and landmark completion checks.

use super::*;
use crate::graph::{AtomId, AtomKind, EdgeTopology, TraversalDirection, TraversalSemantics};

#[test]
fn neighbourhood_artifact_publication_roundtrip_validation_and_snapshot_invalidation() {
	use crate::{
		datastore::{ClassIndexState, ClassSchema, Datastore, GraphIndex, GraphIndexDefinition},
		graph::GraphKind,
		values::{Object, PropertyMap, RecordId},
	};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = tx.define_graph("network", GraphKind::Graph).unwrap();
	let empty = tx.define_graph("empty", GraphKind::Graph).unwrap();
	let records = [
		"person:b",
		"person:a",
		"person:c",
		"person:hole",
		"link:ab",
		"link:bc",
		"link:ca",
		"link:connector",
	]
	.map(|record| record.parse::<RecordId>().unwrap());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let mut nodes = Vec::new();
	for record in records[..4].iter().rev() {
		nodes.push(tx.attach_node(graph.id(), record, None, None).unwrap());
	}
	tx.detach_occurrence(graph.id(), &records[3], false).unwrap();
	let a = nodes[2];
	let b = nodes[3];
	let c = nodes[1];
	let mut edges = Vec::new();
	for (record, source, target) in [(4, a, b), (5, b, c), (6, c, a)] {
		edges.push(
			tx.attach_edge(
				graph.id(),
				&records[record],
				Some("keep".into()),
				EdgeTopology::directed([source], [target]),
				None,
				None,
			)
			.unwrap(),
		);
	}
	tx.attach_edge(
		graph.id(),
		&records[7],
		Some("keep".into()),
		EdgeTopology::directed([edges[0]], [c]),
		None,
		None,
	)
	.unwrap();
	let mut names = Vec::new();
	for owner in ["network", "empty"] {
		for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
			for direction in [
				TraversalDirection::Forward,
				TraversalDirection::Backward,
				TraversalDirection::Both,
			] {
				for algorithm in [
					NeighbourhoodAlgorithm::Exact,
					NeighbourhoodAlgorithm::Sketch(12),
					NeighbourhoodAlgorithm::Landmark(2),
				] {
					let name = format!("near{}", names.len());
					tx.define_graph_index(
						owner,
						GraphIndex::new(
							&name,
							GraphIndexDefinition::Neighbourhood {
								radius: 4,
								algorithm,
							},
							Some(1 << 20),
						)
						.unwrap()
						.with_mode(mode)
						.with_direction(direction)
						.with_edge_labels(["keep"])
						.unwrap(),
					)
					.unwrap();
					names.push((owner, name));
				}
			}
		}
	}
	let automatic = tx
		.define_graph_index(
			"network",
			GraphIndex::new(
				"tiny_auto",
				GraphIndexDefinition::Neighbourhood {
					radius: 2,
					algorithm: NeighbourhoodAlgorithm::Auto,
				},
				Some(1),
			)
			.unwrap(),
		)
		.unwrap();
	tx.commit().unwrap();
	let before = store.inner.current_root();
	for (owner, name) in &names {
		let result = store
			.execute(&format!("REBUILD INDEX {name} ON GRAPH {owner};"), &Object::new())
			.unwrap();
		assert!(result[0].result.is_ok(), "{owner}/{name}: {:?}", result[0].result);
	}
	assert!(
		store.execute("REBUILD INDEX tiny_auto ON GRAPH network;", &Object::new()).unwrap()[0]
			.result
			.is_ok()
	);
	let ready = store.inner.current_root();
	assert!(before.neighbourhood_artifacts.is_empty());
	assert_eq!(ready.neighbourhood_artifacts.len(), names.len());
	assert!(!ready.neighbourhood_artifacts.contains_key(&automatic));
	for artifact in ready.neighbourhood_artifacts.values() {
		let owner = if artifact.layout.header.identity.graph == graph.id().get() {
			graph.id()
		} else {
			empty.id()
		};
		let schema = ready.schema.graph(owner).unwrap();
		let definition = schema
			.indexes()
			.find(|index| index.id().unwrap().get() == artifact.layout.header.identity.index)
			.unwrap();
		let logical = ready.graph(owner).unwrap();
		assert_eq!(definition.state(), ClassIndexState::Ready);
		let layout = artifact.layout.clone();
		let mut data: [Vec<u8>; 8] = std::array::from_fn(|_| Vec::new());
		for (section, bytes) in layout.sections.iter().zip(&mut data) {
			let mut element = vec![0; usize::from(section.width)];
			for ordinal in 0..section.count {
				artifact.encode_element(section.kind, ordinal, &mut element).unwrap();
				bytes.extend_from_slice(&element);
			}
		}
		let restored = NeighbourhoodArtifact::restore(
			logical,
			definition,
			layout.clone(),
			data.clone(),
			8 << 20,
			&mut || Ok(()),
		)
		.unwrap();
		assert_eq!(restored.relation.vertices, artifact.relation.vertices);
		assert_eq!(restored.relation.forward, artifact.relation.forward);
		assert_eq!(restored.relation.reverse, artifact.relation.reverse);
		for (section, bytes) in layout.sections.iter().zip(&data) {
			let mut output = vec![0; section.width as usize];
			for (ordinal, expected) in bytes.chunks_exact(section.width as usize).enumerate() {
				restored.encode_element(section.kind, ordinal as u64, &mut output).unwrap();
				assert_eq!(output, expected);
			}
		}
		assert!(
			NeighbourhoodArtifact::restore(
				logical,
				definition,
				layout.clone(),
				data.clone(),
				1,
				&mut || Ok(())
			)
			.is_err()
		);
		assert!(
			NeighbourhoodArtifact::restore(
				logical,
				definition,
				layout.clone(),
				data.clone(),
				8 << 20,
				&mut || Err(FerrosGrynnError::query_cancelled())
			)
			.is_err()
		);
		for section in 0..8 {
			if data[section].is_empty() {
				continue;
			}
			let mut corrupt = data.clone();
			if section == 7 {
				corrupt[section][0] ^= 1;
			} else {
				corrupt[section][..usize::from(layout.sections[section].width)].fill(0xff);
			}
			assert!(
				NeighbourhoodArtifact::restore(
					logical,
					definition,
					layout.clone(),
					corrupt,
					8 << 20,
					&mut || Ok(())
				)
				.is_err(),
				"section {section}"
			);
		}
	}
	let mut properties = store.begin();
	let key = properties.intern_property("unrelated_property").unwrap();
	properties.set_base_property(&records[0], key, crate::values::Value::from(7_i64)).unwrap();
	properties
		.set_overlay(graph.id(), &records[0], key, crate::values::Value::from(9_i64))
		.unwrap();
	properties.commit().unwrap();
	let after_properties = store.inner.current_root();
	for (id, artifact) in ready.neighbourhood_artifacts.iter() {
		assert!(std::sync::Arc::ptr_eq(artifact, &after_properties.neighbourhood_artifacts[id]));
	}
	let mut old = store.begin();
	let mut aborted = store.begin();
	aborted.set_occurrence_label(graph.id(), &records[4], Some("drop".into())).unwrap();
	assert!(
		aborted
			.graphs()
			.unwrap()
			.iter()
			.find(|schema| schema.id() == graph.id())
			.unwrap()
			.indexes()
			.all(|index| index.state() == ClassIndexState::Stale)
	);
	drop(aborted);
	assert_eq!(store.inner.current_root().neighbourhood_artifacts.len(), names.len());
	let mut changed = store.begin();
	changed.detach_occurrence(graph.id(), &records[7], false).unwrap();
	changed.commit().unwrap();
	assert!(
		store.inner.current_root().neighbourhood_artifacts.values().all(|artifact| artifact
			.layout
			.header
			.identity
			.graph
			== empty.id().get())
	);
	assert!(
		old.graphs()
			.unwrap()
			.iter()
			.flat_map(|schema| schema.indexes())
			.all(|index| index.state() == ClassIndexState::Ready)
	);
	assert_eq!(ready.neighbourhood_artifacts.len(), names.len());
}

/// Includes physical common pages and live source workspace separately.
fn limits(pages: u64) -> NeighbourhoodBuildLimits {
	NeighbourhoodBuildLimits {
		max_bytes: pages * 16_384,
		common_pages: 1,
		scratch_bytes: 8 << 20,
		live_scratch_bytes: 1 << 20,
	}
}

/// Independent Floyd-Warshall minimum-hop oracle over the checked projection.
fn oracle(relation: &ProjectedRelation) -> Vec<Vec<usize>> {
	let count = relation.vertices.len();
	let mut result = vec![vec![usize::MAX / 2; count]; count];
	for source in 0..count {
		result[source][source] = 0;
		for target in relation.forward.neighbours(source) {
			result[source][target] = 1;
		}
	}
	for middle in 0..count {
		for source in 0..count {
			for target in 0..count {
				result[source][target] =
					result[source][target].min(result[source][middle] + result[middle][target]);
			}
		}
	}
	result
}

#[test]
fn neighbourhood_sets_sketches_and_landmarks_match_every_source_radius() {
	let vertices = (0..8)
		.map(|slot| {
			AtomId::from_parts(
				if slot % 2 == 0 {
					AtomKind::Node
				} else {
					AtomKind::Edge
				},
				slot / 2,
				1,
			)
			.unwrap()
		})
		.collect::<Vec<_>>();
	for seed in 0..16_usize {
		let mut topologies = Vec::new();
		for source in 0..vertices.len() {
			for target in 0..vertices.len() {
				if source == target || (source * 17 + target * 13 + seed * 11) % 19 >= 4 {
					continue;
				}
				let topology = match seed % 3 {
					0 => EdgeTopology::directed([vertices[source]], [vertices[target]]),
					1 => EdgeTopology::bidirectional([vertices[source]], [vertices[target]]),
					_ => EdgeTopology::undirected([vertices[source], vertices[target]]),
				};
				topologies.push((vertices[1], topology));
			}
		}
		for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
			for direction in [
				TraversalDirection::Forward,
				TraversalDirection::Backward,
				TraversalDirection::Both,
			] {
				let relation = ProjectedRelation::build(
					vertices.iter().copied(),
					topologies.iter().map(|(edge, topology)| (*edge, topology)),
					mode,
					direction,
					8 << 20,
					|atom| Ok(atom.raw()),
					&mut || Ok(()),
				)
				.unwrap();
				let expected = oracle(&relation);
				for algorithm in [
					NeighbourhoodAlgorithm::Exact,
					NeighbourhoodAlgorithm::Landmark(1),
					NeighbourhoodAlgorithm::Landmark(3),
					NeighbourhoodAlgorithm::Sketch(4),
					NeighbourhoodAlgorithm::Sketch(12),
				] {
					let labels = build_neighbourhood_labels(
						&relation,
						algorithm,
						4,
						limits(1024),
						&mut || Ok(()),
					)
					.unwrap();
					for source in 0..relation.vertices.len() {
						for radius in 1..=4 {
							let members = expected[source]
								.iter()
								.enumerate()
								.filter_map(|(vertex, distance)| {
									(*distance <= radius as usize).then_some(vertex)
								})
								.collect::<Vec<_>>();
							match &labels {
								NeighbourhoodLabels::Exact(lists) => {
									assert_eq!(lists.row(source, radius, 4).unwrap(), members)
								}
								NeighbourhoodLabels::Landmark(lists) => {
									let actual = neighbourhood_members(
										&relation,
										Some(lists),
										source,
										radius,
										4,
										1000,
										limits(1024),
										&mut || Ok(()),
									)
									.unwrap();
									assert_eq!(
										actual, members,
										"seed={seed}, source={source}, radius={radius}, \
										 mode={mode:?}, direction={direction:?}"
									);
								}
								NeighbourhoodLabels::Sketch(sketches) => {
									let NeighbourhoodAlgorithm::Sketch(precision) = algorithm
									else {
										panic!("sketch algorithm expected")
									};
									let mut expected = HyperLogLog::new(precision).unwrap();
									for member in &members {
										expected.insert(&(*member as u64).to_le_bytes());
									}
									assert_eq!(
										sketches[(radius as usize - 1) * relation.vertices.len()
											+ source],
										expected
									);
								}
								NeighbourhoodLabels::OnDemand => panic!("explicit build fits"),
							}
						}
					}
				}
			}
		}
	}
}

#[test]
fn neighbourhood_dense_budgets_large_radius_and_refinement_are_hard_limits() {
	let vertices = (0..20)
		.map(|slot| AtomId::from_parts(AtomKind::Node, slot, 1).unwrap())
		.collect::<Vec<_>>();
	let edge = AtomId::from_parts(AtomKind::Edge, 0, 1).unwrap();
	let topology = EdgeTopology::undirected(vertices.clone());
	let relation = ProjectedRelation::build(
		vertices.iter().copied(),
		[(edge, &topology)].into_iter(),
		TraversalSemantics::Node,
		TraversalDirection::Both,
		8 << 20,
		|atom| Ok(atom.raw()),
		&mut || Ok(()),
	)
	.unwrap();
	for algorithm in [
		NeighbourhoodAlgorithm::Exact,
		NeighbourhoodAlgorithm::Landmark(20),
		NeighbourhoodAlgorithm::Sketch(12),
	] {
		assert!(
			build_neighbourhood_labels(&relation, algorithm, 4, limits(3), &mut || Ok(())).is_err()
		);
		assert!(
			build_neighbourhood_labels(&relation, algorithm, 5, limits(1024), &mut || Ok(()))
				.is_err()
		);
		assert!(
			build_neighbourhood_labels(
				&relation,
				algorithm,
				4,
				NeighbourhoodBuildLimits {
					scratch_bytes: 1,
					live_scratch_bytes: 0,
					..limits(1024)
				},
				&mut || Ok(())
			)
			.is_err()
		);
	}
	for radius in [1, 5, 100] {
		assert!(matches!(
			build_neighbourhood_labels(
				&relation,
				NeighbourhoodAlgorithm::Auto,
				radius,
				limits(3),
				&mut || Ok(())
			)
			.unwrap(),
			NeighbourhoodLabels::OnDemand
		));
	}
	assert!(
		build_neighbourhood_labels(
			&relation,
			NeighbourhoodAlgorithm::Sketch(0),
			1,
			limits(1024),
			&mut || Ok(())
		)
		.is_err()
	);
	assert!(
		build_neighbourhood_labels(
			&relation,
			NeighbourhoodAlgorithm::Exact,
			4,
			limits(4),
			&mut || Err(FerrosGrynnError::query_cancelled())
		)
		.is_err()
	);
	let NeighbourhoodLabels::Landmark(lists) = build_neighbourhood_labels(
		&relation,
		NeighbourhoodAlgorithm::Landmark(1),
		4,
		limits(4),
		&mut || Ok(()),
	)
	.unwrap() else {
		panic!("landmarks expected")
	};
	assert_eq!(
		neighbourhood_members(&relation, Some(&lists), 19, 0, 4, 0, limits(4), &mut || Ok(()))
			.unwrap(),
		[19]
	);
	assert!(
		neighbourhood_members(&relation, Some(&lists), 19, 2, 4, 0, limits(4), &mut || Ok(()))
			.is_err()
	);
	assert!(
		neighbourhood_members(&relation, Some(&lists), 20, 2, 4, 1000, limits(4), &mut || Ok(()))
			.is_err()
	);
	assert!(
		neighbourhood_members(&relation, Some(&lists), 19, 2, 4, 1000, limits(4), &mut || Err(
			FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
	assert!(
		build_neighbourhood_labels(
			&relation,
			NeighbourhoodAlgorithm::Exact,
			4,
			NeighbourhoodBuildLimits {
				max_bytes: 4 * 16_384 - 1,
				..limits(4)
			},
			&mut || Ok(())
		)
		.is_err()
	);
	assert!(
		build_neighbourhood_labels(
			&relation,
			NeighbourhoodAlgorithm::Exact,
			4,
			limits(4),
			&mut || Ok(())
		)
		.is_ok()
	);
}
