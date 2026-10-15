//! All-pairs soundness, exact refinement and byte-budget algorithm selection.

use super::*;
#[cfg(test)]
use crate::test_assertions::{AssertOption as _, AssertResult as _};
use crate::{
	datastore::{ClassIndexState, ClassSchema, Datastore, GraphIndex, GraphIndexDefinition},
	graph::{
		AtomId,
		AtomKind,
		EdgeTopology,
		ProjectedRelation,
		TraversalDirection,
		TraversalSemantics,
	},
	values::{Object, PropertyMap, RecordId},
};

/// Verifies reachability publication private invalidation abort and old
/// snapshots.
#[test]
fn reachability_publication_private_invalidation_abort_and_old_snapshots() {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("network", GraphKind::Graph).assert_ok();
	let records =
		["person:a", "person:b", "link:ab"].map(|record| record.parse::<RecordId>().assert_ok());
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = tx.attach_node(graph.id(), &records[0], None, None).assert_ok();
	let b = tx.attach_node(graph.id(), &records[1], None, None).assert_ok();
	tx.attach_edge(
		graph.id(),
		&records[2],
		Some("keep".into()),
		EdgeTopology::directed([a], [b]),
		None,
		None,
	)
	.assert_ok();
	let id = tx
		.define_graph_index(
			"network",
			GraphIndex::new(
				"reach",
				GraphIndexDefinition::Reachability {
					algorithm: ReachabilityAlgorithm::Grail(2),
				},
				Some(1 << 20),
			)
			.assert_ok(),
		)
		.assert_ok();
	tx.commit().assert_ok();
	let before_build = store.inner.current_root();
	assert_eq!(
		before_build.schema.graph(graph.id()).assert_some().indexes().next().assert_some().state(),
		ClassIndexState::Stale
	);
	let result = store.execute("REBUILD INDEX reach ON GRAPH network;", &Object::new()).assert_ok();
	assert!(result[0].result.is_ok(), "{:?}", result[0].result);
	let ready = store.inner.current_root();
	let artifact = ready.reachability_artifacts.get(&id).assert_some();
	let source = artifact.node_components[a.slot() as usize] as usize;
	let target = artifact.node_components[b.slot() as usize] as usize;
	assert!(
		artifact
			.labels
			.reachable(&artifact.condensation, source, target, 100, 8192, &mut || Ok(()))
			.assert_ok()
			.reachable
	);
	assert!(before_build.reachability_artifacts.is_empty());
	let mut old = store.begin();
	let mut abort = store.begin();
	abort.set_occurrence_label(graph.id(), &records[2], Some("drop".into())).assert_ok();
	assert_eq!(
		abort.graphs().assert_ok()[0].indexes().next().assert_some().state(),
		ClassIndexState::Stale
	);
	drop(abort);
	assert_eq!(
		store.begin().graphs().assert_ok()[0].indexes().next().assert_some().state(),
		ClassIndexState::Ready
	);
	let mut changed = store.begin();
	changed.detach_occurrence(graph.id(), &records[2], false).assert_ok();
	assert_eq!(
		changed.graphs().assert_ok()[0].indexes().next().assert_some().state(),
		ClassIndexState::Stale
	);
	changed.commit().assert_ok();
	assert!(store.inner.current_root().reachability_artifacts.is_empty());
	assert_eq!(
		old.graphs().assert_ok()[0].indexes().next().assert_some().state(),
		ClassIndexState::Ready
	);
	assert!(
		artifact
			.labels
			.reachable(&artifact.condensation, source, target, 100, 8192, &mut || Ok(()))
			.assert_ok()
			.reachable
	);
}

/// Builds the shared projection, using fixed durable keys independent of
/// iteration.
fn relation(
	vertices: &[AtomId],
	topologies: &[(AtomId, EdgeTopology)],
	mode: TraversalSemantics,
	direction: TraversalDirection,
) -> ProjectedCondensation {
	ProjectedRelation::build(
		vertices.iter().copied(),
		topologies.iter().map(|(connector, topology)| (*connector, topology)),
		mode,
		direction,
		8 << 20,
		|atom| Ok(atom.raw()),
		&mut || Ok(()),
	)
	.assert_ok()
	.condensation(8 << 20, &mut || Ok(()))
	.assert_ok()
}

/// Independent transitive closure of the emitted component relation.
fn oracle(dag: &ProjectedCondensation) -> Vec<Vec<bool>> {
	let count = dag.representatives.len();
	let mut result = vec![vec![false; count]; count];
	for (source, row) in result.iter_mut().enumerate() {
		row[source] = true;
		for target in dag.forward.neighbours(source) {
			row[target] = true;
		}
	}
	for middle in 0..count {
		for source in 0..count {
			for target in 0..count {
				result[source][target] |= result[source][middle] && result[middle][target];
			}
		}
	}
	result
}

/// Common physical/header pages and live source scratch remain separate.
fn limits(pages: u64) -> ReachabilityBuildLimits {
	ReachabilityBuildLimits {
		max_bytes: pages * PAGE_BYTES,
		common_pages: 1,
		scratch_bytes: 8 << 20,
		live_scratch_bytes: 1 << 20,
	}
}

/// Verifies five algorithms and refinement match all pairs for projected
/// graphs.
#[test]
fn five_algorithms_and_refinement_match_all_pairs_for_projected_graphs() {
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
			.assert_ok()
		})
		.collect::<Vec<_>>();
	let algorithms = [
		ReachabilityAlgorithm::Grail(1),
		ReachabilityAlgorithm::Grail(2),
		ReachabilityAlgorithm::Grail(32),
		ReachabilityAlgorithm::Ferrari(1),
		ReachabilityAlgorithm::Ferrari(4),
		ReachabilityAlgorithm::Ferrari(255),
		ReachabilityAlgorithm::Bfl(64),
		ReachabilityAlgorithm::Bfl(160),
		ReachabilityAlgorithm::Bfl(4096),
		ReachabilityAlgorithm::ChainCover,
	];
	let mut maybe_count = 0;
	for seed in 0..96_usize {
		let mut topologies = Vec::new();
		for source in 0..vertices.len() {
			for target in 0..vertices.len() {
				if source == target || (source * 17 + target * 13 + seed * 11) % 19 >= 4 {
					continue;
				}
				let topology = if seed % 3 == 0 {
					EdgeTopology::directed([vertices[source]], [vertices[target]])
				} else if seed % 3 == 1 {
					EdgeTopology::bidirectional([vertices[source]], [vertices[target]])
				} else {
					EdgeTopology::undirected([vertices[source], vertices[target]])
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
				let dag = relation(&vertices, &topologies, mode, direction);
				let expected = oracle(&dag);
				for algorithm in algorithms {
					let labels = build_reachability_labels(
						&dag,
						GraphKind::Graph,
						algorithm,
						limits(1024),
						&mut || Ok(()),
					)
					.assert_ok();
					for (source, row) in expected.iter().enumerate() {
						for (target, &expected_reachable) in row.iter().enumerate() {
							match labels.decision(source, target).assert_ok() {
								ReachabilityDecision::Yes => {
									assert!(expected_reachable);
									if matches!(
										algorithm,
										ReachabilityAlgorithm::Grail(_)
											| ReachabilityAlgorithm::Bfl(_)
									) {
										assert_eq!(source, target);
									}
								}
								ReachabilityDecision::No => assert!(!expected_reachable),
								ReachabilityDecision::Maybe => maybe_count += 1,
							}
							let actual = labels
								.reachable(&dag, source, target, 10_000, 8 << 20, &mut || Ok(()))
								.assert_ok();
							assert_eq!(
								actual.reachable, expected_reachable,
								"seed {seed}, {mode:?}, {direction:?}, {algorithm:?}"
							);
						}
					}
					assert!(labels.decision(usize::MAX, 0).is_err());
				}
			}
		}
	}
	assert!(maybe_count > 0);
}

/// Verifies tree cover is exact for both orientations both mode and both
/// direction.
#[test]
fn tree_cover_is_exact_for_both_orientations_both_mode_and_both_direction() {
	let vertices = (0..9)
		.map(|slot| {
			AtomId::from_parts(
				if slot < 6 {
					AtomKind::Node
				} else {
					AtomKind::Edge
				},
				slot,
				1,
			)
			.assert_ok()
		})
		.collect::<Vec<_>>();
	for reversed in [false, true] {
		let topologies = [(0, 1), (0, 2), (1, 3), (1, 4)]
			.into_iter()
			.map(|(source, target)| {
				let (source, target) = if reversed {
					(target, source)
				} else {
					(source, target)
				};
				(vertices[6], EdgeTopology::directed([vertices[source]], [vertices[target]]))
			})
			.collect::<Vec<_>>();
		for mode in [TraversalSemantics::Node, TraversalSemantics::Atom] {
			for direction in [
				TraversalDirection::Forward,
				TraversalDirection::Backward,
				TraversalDirection::Both,
			] {
				let dag = relation(&vertices, &topologies, mode, direction);
				let expected = oracle(&dag);
				for family in [GraphKind::Tree, GraphKind::RootedTree, GraphKind::Forest] {
					let labels = build_reachability_labels(
						&dag,
						family,
						ReachabilityAlgorithm::TreeCover,
						limits(1024),
						&mut || Ok(()),
					)
					.assert_ok();
					for (source, row) in expected.iter().enumerate() {
						for (target, &expected_reachable) in row.iter().enumerate() {
							assert_eq!(
								labels.decision(source, target).assert_ok()
									== ReachabilityDecision::Yes,
								expected_reachable,
								"reversed {reversed}, {mode:?}, {direction:?}, {family:?}, \
								 {source}->{target}: {labels:?}"
							);
						}
					}
				}
				assert!(
					build_reachability_labels(
						&dag,
						GraphKind::Graph,
						ReachabilityAlgorithm::TreeCover,
						limits(1024),
						&mut || Ok(())
					)
					.is_err()
				);
			}
		}
	}
}

/// Verifies auto selects only fitting full page layouts and explicit budgets
/// fail.
#[test]
fn auto_selects_only_fitting_full_page_layouts_and_explicit_budgets_fail() {
	let vertices = (0..100)
		.map(|slot| AtomId::from_parts(AtomKind::Node, slot, 1).assert_ok())
		.collect::<Vec<_>>();
	let edge = AtomId::from_parts(AtomKind::Edge, 0, 1).assert_ok();
	let topologies = (1..100)
		.map(|target| (edge, EdgeTopology::directed([vertices[0]], [vertices[target]])))
		.collect::<Vec<_>>();
	let dag =
		relation(&vertices, &topologies, TraversalSemantics::Node, TraversalDirection::Forward);
	assert!(matches!(
		build_reachability_labels(
			&dag,
			GraphKind::Graph,
			ReachabilityAlgorithm::Auto,
			limits(7),
			&mut || Ok(())
		)
		.assert_ok(),
		ReachabilityLabels::ChainCover { .. }
	));
	assert!(matches!(
		build_reachability_labels(
			&dag,
			GraphKind::Graph,
			ReachabilityAlgorithm::Auto,
			limits(3),
			&mut || Ok(())
		)
		.assert_ok(),
		ReachabilityLabels::Ferrari { .. }
	));
	assert!(matches!(
		build_reachability_labels(
			&dag,
			GraphKind::Graph,
			ReachabilityAlgorithm::Auto,
			limits(2),
			&mut || Ok(())
		)
		.assert_ok(),
		ReachabilityLabels::Bfl { .. }
	));
	assert!(matches!(
		build_reachability_labels(
			&dag,
			GraphKind::Graph,
			ReachabilityAlgorithm::Auto,
			limits(1),
			&mut || Ok(())
		)
		.assert_ok(),
		ReachabilityLabels::OnDemand { .. }
	));
	for algorithm in [
		ReachabilityAlgorithm::ChainCover,
		ReachabilityAlgorithm::Ferrari(4),
		ReachabilityAlgorithm::Grail(2),
		ReachabilityAlgorithm::Bfl(160),
		ReachabilityAlgorithm::TreeCover,
	] {
		assert!(
			build_reachability_labels(&dag, GraphKind::Tree, algorithm, limits(1), &mut || Ok(()))
				.is_err()
		);
	}
	let mut small_scratch = limits(1024);
	small_scratch.scratch_bytes = 1;
	assert!(
		build_reachability_labels(
			&dag,
			GraphKind::Graph,
			ReachabilityAlgorithm::ChainCover,
			small_scratch,
			&mut || Ok(())
		)
		.is_err()
	);
	assert!(
		build_reachability_labels(
			&dag,
			GraphKind::Graph,
			ReachabilityAlgorithm::Grail(2),
			limits(1024),
			&mut || Err(FerrosGrynnError::query_cancelled())
		)
		.is_err()
	);
	let labels = build_reachability_labels(
		&dag,
		GraphKind::Graph,
		ReachabilityAlgorithm::Bfl(160),
		limits(1024),
		&mut || Ok(()),
	)
	.assert_ok();
	assert!(labels.reachable(&dag, 0, 1, 0, 8 << 20, &mut || Ok(())).is_err());
	assert!(labels.reachable(&dag, 0, 1, 1000, 1, &mut || Ok(())).is_err());
}
