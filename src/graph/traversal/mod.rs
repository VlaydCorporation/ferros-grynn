mod connectivity;
mod cuts;
mod index;
mod lca;
mod shortest_paths;
mod spanning;

pub use connectivity::{
	Condensation,
	bfs,
	condensation,
	dfs,
	has_cycle_directed,
	has_cycle_undirected,
	is_connected,
	strongly_connected_components,
	topological_sort,
	weakly_connected_components,
};
pub use cuts::{CutSet, find_cuts};
pub use index::{TraversalDirection, TraversalIndex, TraversalSemantics, TraversalStep};
pub use lca::BinaryLiftingIndex;
pub use shortest_paths::{ShortestPaths, bellman_ford, dijkstra};
pub use spanning::{SpanningForest, minimum_spanning_forest, minimum_spanning_tree};

use crate::gql_status::FerrosGrynnError;

fn algorithm_failure(algorithm: &'static str, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation(algorithm, reason.into())
}

#[cfg(test)]
mod tests {
	use super::{
		BinaryLiftingIndex,
		TraversalDirection,
		TraversalIndex,
		TraversalSemantics,
		bellman_ford,
		bfs,
		condensation,
		dijkstra,
		find_cuts,
		minimum_spanning_tree,
		strongly_connected_components,
		topological_sort,
	};
	use crate::values::runtime::{
		Number,
		PropertyMap,
		graph::{
			AtomKind,
			EdgeEndpoint,
			EdgeTopology,
			Graph,
			Mutable,
			RootedTree,
			TreeOrientation,
		},
	};

	#[test]
	fn traversal_index_distinguishes_node_walk_and_atom_walk() {
		let mut graph = Graph::<Mutable>::new();
		let node = graph.add_node(None, PropertyMap::new()).unwrap();
		let target = graph.add_node(None, PropertyMap::new()).unwrap();
		let relation = graph
			.add_edge(EdgeTopology::directed([node], [target]), None, PropertyMap::new())
			.unwrap();
		let annotation = graph
			.add_edge(EdgeTopology::directed([node], [relation]), None, PropertyMap::new())
			.unwrap();

		let nodes = TraversalIndex::new(&graph, TraversalSemantics::Node).unwrap();
		assert_eq!(nodes.steps(&EdgeEndpoint::Local(node), TraversalDirection::Forward).count(), 1,);
		let atoms = TraversalIndex::new(&graph, TraversalSemantics::Atom).unwrap();
		let reached = atoms
			.breadth_first(EdgeEndpoint::Local(node), TraversalDirection::Forward, Some(1))
			.unwrap();
		assert!(reached.contains(&EdgeEndpoint::Local(relation)));
		assert!(atoms.contains(&EdgeEndpoint::Local(annotation)));
	}

	#[test]
	fn bfs_topology_and_components_are_correct() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).unwrap();
		let second = graph.add_node(None, PropertyMap::new()).unwrap();
		let third = graph.add_node(None, PropertyMap::new()).unwrap();
		graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.unwrap();
		graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.unwrap();

		assert_eq!(bfs(&graph, first, TraversalDirection::Forward, None).unwrap(), vec![
			first, second, third
		],);
		assert_eq!(topological_sort(&graph).unwrap(), vec![first, second, third],);
		assert_eq!(strongly_connected_components(&graph).unwrap().len(), 3);
	}

	#[test]
	fn condensation_collapses_cycles_and_deduplicates_bidirectional_steps() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).unwrap();
		let second = graph.add_node(None, PropertyMap::new()).unwrap();
		let third = graph.add_node(None, PropertyMap::new()).unwrap();
		graph
			.add_edge(EdgeTopology::bidirectional([first], [second]), None, PropertyMap::new())
			.unwrap();
		graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.unwrap();

		let index = TraversalIndex::new(&graph, TraversalSemantics::Node).unwrap();
		assert_eq!(index.steps(&EdgeEndpoint::Local(first), TraversalDirection::Both).count(), 1,);

		let components = strongly_connected_components(&graph).unwrap();
		assert_eq!(components.len(), 2);
		let result = condensation(&graph).unwrap();
		assert_eq!(result.component_of(first), result.component_of(second));
		assert_ne!(result.component_of(first), result.component_of(third));
		assert_eq!(result.edges().len(), 1);
	}

	#[test]
	fn shortest_paths_validate_weights_and_materialize_path() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).unwrap();
		let second = graph.add_node(None, PropertyMap::new()).unwrap();
		let third = graph.add_node(None, PropertyMap::new()).unwrap();
		let direct = graph
			.add_edge(EdgeTopology::directed([first], [third]), None, PropertyMap::new())
			.unwrap();
		graph.set_edge_weight(direct, Some(Number::Float(10.0))).unwrap();
		let first_edge = graph
			.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
			.unwrap();
		graph.set_edge_weight(first_edge, Some(Number::Float(2.0))).unwrap();
		let second_edge = graph
			.add_edge(EdgeTopology::directed([second], [third]), None, PropertyMap::new())
			.unwrap();
		graph.set_edge_weight(second_edge, Some(Number::Float(3.0))).unwrap();

		let graph = graph.freeze();
		let shortest = dijkstra(&graph, first, TraversalDirection::Forward).unwrap();
		assert_eq!(shortest.distance_to(third), Some(5.0));
		assert_eq!(shortest.path_to(third), Some(vec![first, second, third]));
		assert_eq!(shortest.materialize_path(&graph, third).unwrap().unwrap().length(), 2);
	}

	#[test]
	fn bellman_ford_reports_negative_cycle_as_domain_error() {
		let mut graph = Graph::<Mutable>::new();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		for (source, target) in [(left, right), (right, left)] {
			let edge = graph
				.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
				.unwrap();
			graph.set_edge_weight(edge, Some(Number::Float(-1.0))).unwrap();
		}
		assert!(bellman_ford(&graph, left, TraversalDirection::Forward).is_err());
		assert!(dijkstra(&graph, left, TraversalDirection::Forward).is_err());
	}

	#[test]
	fn spanning_tree_cuts_and_lca_cover_tree_algorithms() {
		let mut graph = Graph::<Mutable>::new();
		let root = graph.add_node(None, PropertyMap::new()).unwrap();
		let left = graph.add_node(None, PropertyMap::new()).unwrap();
		let right = graph.add_node(None, PropertyMap::new()).unwrap();
		let left_edge = graph
			.add_edge(EdgeTopology::undirected([root, left]), None, PropertyMap::new())
			.unwrap();
		let right_edge = graph
			.add_edge(EdgeTopology::undirected([root, right]), None, PropertyMap::new())
			.unwrap();
		let spanning = minimum_spanning_tree(&graph).unwrap();
		assert_eq!(spanning.edges().len(), 2);
		let cuts = find_cuts(&graph).unwrap();
		assert_eq!(cuts.articulation_points(), &[root]);
		assert_eq!(cuts.bridges().len(), 2);
		assert!(cuts.bridges().contains(&left_edge));
		assert!(cuts.bridges().contains(&right_edge));

		let mut tree =
			RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
				.unwrap();
		let root = tree.root();
		let (left, _) =
			tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
		let (right, _) =
			tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
		let (grandchild, _) =
			tree.add_child(left, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
		let lca = BinaryLiftingIndex::from_rooted_tree(&tree).unwrap();
		assert_eq!(lca.lowest_common_ancestor(grandchild, right).unwrap(), Some(root));
		assert_eq!(lca.lowest_common_ancestor(grandchild, left).unwrap(), Some(left));
		assert_eq!(lca.depth(grandchild).unwrap(), 2);
		assert_eq!(lca.position(root).unwrap(), 0);
		assert_eq!(root.kind(), Some(AtomKind::Node));
	}
}
