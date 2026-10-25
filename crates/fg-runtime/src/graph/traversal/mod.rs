/// Connectivity, cycle detection and topological ordering.
mod connectivity;
/// Articulation points and bridge cuts.
mod cuts;
/// Reusable traversal indexes and incidence-step projection.
pub mod index;
mod landmark;
/// Binary-lifting lowest-common-ancestor lookup.
mod lca;
mod relation;

/// Checked weighted and unweighted shortest-path algorithms.
pub mod shortest_paths;
/// Spanning-tree and spanning-forest algorithms.
mod spanning;

pub use connectivity::{
	Condensation,
	bfs,
	bfs_with_check,
	condensation,
	condensation_with_check,
	dfs,
	has_cycle_directed,
	has_cycle_directed_with_check,
	has_cycle_undirected,
	has_cycle_undirected_with_check,
	is_connected,
	strongly_connected_components,
	strongly_connected_components_with_check,
	topological_sort,
	topological_sort_with_check,
	weakly_connected_components,
	weakly_connected_components_with_check,
};
pub use cuts::{CutSet, find_cuts, find_cuts_with_check};
pub use index::{
	TraversalDirection,
	TraversalIndex,
	TraversalSemantics,
	TraversalStep,
	visit_endpoint_steps,
	visit_incidence_targets,
};
pub use landmark::{
	DirectedLandmarks,
	DistanceInterval,
	LandmarkBuildLimits,
	LandmarkDistanceRows,
};
pub use lca::BinaryLiftingIndex;
pub use relation::{ProjectedCondensation, ProjectedRelation, RelationCsr};
pub use shortest_paths::{
	ShortestPaths,
	bellman_ford,
	bellman_ford_with_check,
	dijkstra,
	dijkstra_to_with_control,
};
pub use spanning::{
	SpanningForest,
	minimum_spanning_forest,
	minimum_spanning_forest_with_check,
	minimum_spanning_tree,
};

use crate::gql_status::FerrosGrynnError;

/// Returns the algorithm failure result.
fn algorithm_failure(algorithm: &'static str, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation(algorithm, reason.into())
}

#[cfg(test)]
/// Regression tests for traversal behavior.
mod tests {
	use std::sync::Arc;

	use super::{
		BinaryLiftingIndex,
		TraversalDirection,
		TraversalIndex,
		TraversalSemantics,
		bellman_ford,
		bfs,
		bfs_with_check,
		condensation,
		dijkstra,
		find_cuts,
		minimum_spanning_tree,
		strongly_connected_components,
		topological_sort,
	};
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		gql_status::FerrosGrynnError,
		values::runtime::{
			Number,
			PropertyMap,
			graph::{
				AtomKind,
				EdgeEndpoint,
				EdgeTopology,
				Graph,
				GraphId,
				GraphKind,
				GraphRef,
				GraphStorageMode,
				Mutable,
				RootedTree,
				TreeOrientation,
			},
		},
	};

	/// Verifies traversal index distinguishes node walk and atom walk.
	#[test]
	fn traversal_index_distinguishes_node_walk_and_atom_walk() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).assert_ok();
		let target = graph.add_node(None, PropertyMap::new()).assert_ok();
		let relation = graph
			.add_edge(EdgeTopology::directed([node], [target]), None, PropertyMap::new())
			.assert_ok();
		let annotation = graph
			.add_edge(EdgeTopology::directed([node], [relation]), None, PropertyMap::new())
			.assert_ok();

		let nodes = TraversalIndex::new(&graph, TraversalSemantics::Node).assert_ok();
		assert_eq!(nodes.steps(&EdgeEndpoint::Local(node), TraversalDirection::Forward).count(), 1,);
		let atoms = TraversalIndex::new(&graph, TraversalSemantics::Atom).assert_ok();
		let reached = atoms
			.breadth_first(EdgeEndpoint::Local(node), TraversalDirection::Forward, Some(1))
			.assert_ok();
		assert!(reached.contains(&EdgeEndpoint::Local(relation)));
		assert!(atoms.contains(&EdgeEndpoint::Local(annotation)));
	}

	/// Verifies bfs topology and components are correct.
	#[test]
	fn bfs_topology_and_components_are_correct() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let third = graph.add_node(None, PropertyMap::new()).assert_ok();
		graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.assert_ok();
		graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.assert_ok();

		assert_eq!(bfs(&graph, first, TraversalDirection::Forward, None).assert_ok(), vec![
			first, second, third
		],);
		assert_eq!(topological_sort(&graph).assert_ok(), vec![first, second, third],);
		assert_eq!(strongly_connected_components(&graph).assert_ok().len(), 3);
	}

	/// Verifies controlled bfs propagates a frontier check failure.
	#[test]
	fn controlled_bfs_propagates_a_frontier_check_failure() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.assert_ok();
		let mut checks = 0;

		let error = bfs_with_check(&graph, first, TraversalDirection::Forward, None, &mut || {
			checks += 1;
			if checks == 5 {
				Err(FerrosGrynnError::query_cancelled())
			} else {
				Ok(())
			}
		})
		.assert_err();

		assert_eq!(checks, 5);
		assert_eq!(error.status().code().to_string(), "FG-50F05");
	}

	/// Verifies scc keeps one way siblings separate.
	#[test]
	fn scc_keeps_one_way_siblings_separate() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let third = graph.add_node(None, PropertyMap::new()).assert_ok();
		for (source, target) in [(first, second), (first, third), (second, third)] {
			graph
				.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
				.assert_ok();
		}
		let components = strongly_connected_components(&graph).assert_ok();
		assert_eq!(components.len(), 3);
		assert!(components.iter().all(|component| component.len() == 1));
	}

	/// Verifies condensation collapses cycles and deduplicates bidirectional
	/// steps.
	#[test]
	fn condensation_collapses_cycles_and_deduplicates_bidirectional_steps() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let third = graph.add_node(None, PropertyMap::new()).assert_ok();
		graph
			.add_edge(EdgeTopology::bidirectional([first], [second]), None, PropertyMap::new())
			.assert_ok();
		graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.assert_ok();

		let index = TraversalIndex::new(&graph, TraversalSemantics::Node).assert_ok();
		assert_eq!(index.steps(&EdgeEndpoint::Local(first), TraversalDirection::Both).count(), 1,);

		let components = strongly_connected_components(&graph).assert_ok();
		assert_eq!(components.len(), 2);
		let result = condensation(&graph).assert_ok();
		assert_eq!(result.component_of(first), result.component_of(second));
		assert_ne!(result.component_of(first), result.component_of(third));
		assert_eq!(result.edges().len(), 1);
	}

	/// Verifies shortest paths validate weights and materialize path.
	#[test]
	fn shortest_paths_validate_weights_and_materialize_path() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let third = graph.add_node(None, PropertyMap::new()).assert_ok();
		let direct = graph
			.add_edge(EdgeTopology::directed([first], [third]), None, PropertyMap::new())
			.assert_ok();
		graph.set_edge_weight(direct, Some(Number::Float(10.0))).assert_ok();
		let first_edge = graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.assert_ok();
		graph.set_edge_weight(first_edge, Some(Number::Float(2.0))).assert_ok();
		let second_edge = graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.assert_ok();
		graph.set_edge_weight(second_edge, Some(Number::Float(3.0))).assert_ok();

		let graph = graph.freeze();
		let shortest = dijkstra(&graph, first, TraversalDirection::Forward).assert_ok();
		assert_eq!(shortest.distance_to(third), Some(Number::Float(5.0)));
		assert_eq!(shortest.path_to(third), Some(vec![first, second, third]));
		let graph_ref = Arc::new(
			GraphRef::new(
				GraphId::new(1).assert_some(),
				"shortest-test",
				GraphKind::Graph,
				GraphStorageMode::InMemory,
			)
			.assert_ok(),
		);
		assert_eq!(
			shortest.materialize_path(&graph, graph_ref, third).assert_ok().assert_some().length(),
			2,
		);
	}

	/// Verifies bellman ford reports negative cycle as domain error.
	#[test]
	fn bellman_ford_reports_negative_cycle_as_domain_error() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		for (source, target) in [(left, right), (right, left)] {
			let edge = graph
				.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
				.assert_ok();
			graph.set_edge_weight(edge, Some(Number::Float(-1.0))).assert_ok();
		}
		assert!(bellman_ford(&graph, left, TraversalDirection::Forward).is_err());
		assert!(dijkstra(&graph, left, TraversalDirection::Forward).is_err());
	}

	/// Verifies spanning tree cuts and lca cover tree algorithms.
	#[test]
	fn spanning_tree_cuts_and_lca_cover_tree_algorithms() {
		let mut graph = Graph::<Mutable>::new();
		let root = graph.add_node(None, PropertyMap::new()).assert_ok();
		let left = graph.add_node(None, PropertyMap::new()).assert_ok();
		let right = graph.add_node(None, PropertyMap::new()).assert_ok();
		let left_edge = graph
			.add_edge(EdgeTopology::undirected([root, left]), None, PropertyMap::new())
			.assert_ok();
		let right_edge = graph
			.add_edge(EdgeTopology::undirected([root, right]), None, PropertyMap::new())
			.assert_ok();
		let spanning = minimum_spanning_tree(&graph).assert_ok();
		assert_eq!(spanning.edges().len(), 2);
		let cuts = find_cuts(&graph).assert_ok();
		assert_eq!(cuts.articulation_points(), &[root]);
		assert_eq!(cuts.bridges().len(), 2);
		assert!(cuts.bridges().contains(&left_edge));
		assert!(cuts.bridges().contains(&right_edge));

		let mut tree =
			RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
				.assert_ok();
		let root = tree.root();
		let (left, _) =
			tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
		let (right, _) =
			tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
		let (grandchild, _) =
			tree.add_child(left, None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
		let lca = BinaryLiftingIndex::from_rooted_tree(&tree).assert_ok();
		assert_eq!(lca.lowest_common_ancestor(grandchild, right).assert_ok(), Some(root));
		assert_eq!(lca.lowest_common_ancestor(grandchild, left).assert_ok(), Some(left));
		assert_eq!(lca.depth(grandchild).assert_ok(), 2);
		assert_eq!(lca.position(root).assert_ok(), 0);
		assert_eq!(root.kind(), Some(AtomKind::Node));
	}
}
