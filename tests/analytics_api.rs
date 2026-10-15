#![cfg(test)]

//! Direct qualified analytic SDK checks, without executing query text.
#![allow(
	clippy::unwrap_used,
	reason = "Unexpected SDK errors fail analytic test setup immediately"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn::{
	graph::{
		EdgeTopology,
		Graph,
		Mutable,
		analytics::{
			self,
			AnalyticsOrientation,
			CentralityOptions,
			ClusteringOptions,
			KatzOptions,
			LabelPropagationOptions,
			LouvainOptions,
			PageRankOptions,
			SpectralOptions,
			TriangleOptions,
			WeightSelection,
		},
		tensor::{
			EndpointTensor,
			GraphAlgorithmLimits,
			ProjectionOptions,
			TensorOptions,
			TensorProjection,
		},
	},
	values::{Number, PropertyMap},
};

/// Captures the common immutable input through the standalone GraphRead SDK.
fn capture(graph: &Graph<Mutable>) -> TensorProjection {
	EndpointTensor::from_graph(graph, TensorOptions::default())
		.unwrap()
		.project(ProjectionOptions::default())
		.unwrap()
}

/// Verifies analytics api skewed sparse rows preserve counts under either
/// canonical order.
#[test]
fn analytics_api_skewed_sparse_rows_preserve_counts_under_either_canonical_order() {
	for last in [false, true] {
		let mut graph = Graph::<Mutable>::with_capacity(512, 511);
		let hub = graph
			.add_node_with_id(
				if last {
					"n:z"
				} else {
					"n:a"
				}
				.parse()
				.unwrap(),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		for ordinal in 1..512 {
			let leaf = graph
				.add_node_with_id(
					format!("n:m{ordinal:04}").parse().unwrap(),
					None,
					PropertyMap::new(),
				)
				.unwrap();
			graph
				.add_edge(EdgeTopology::undirected([hub, leaf]), None, PropertyMap::new())
				.unwrap();
		}
		let projection = capture(&graph);
		let mut triangles = TriangleOptions::default();
		triangles.limits.max_work = 100_000;
		assert_eq!(
			analytics::triangles(&projection, triangles, &mut || Ok(())).unwrap().totals.undirected,
			0
		);
		let mut clustering = ClusteringOptions::default();
		clustering.limits.max_work = 100_000;
		let result = analytics::clustering(&projection, clustering, &mut || Ok(())).unwrap();
		assert_eq!((result.mean, result.transitivity), (0.0, 0.0));
	}
}

/// Asserts a deliberately explicit numerical tolerance for floating scores.
fn close(actual: f64, expected: f64) {
	assert!((actual - expected).abs() < 1.0e-9, "{actual} != {expected}");
}

/// Verifies analytics api degree pagerank and distances distinguish strength
/// from cost.
#[test]
fn analytics_api_degree_pagerank_and_distances_distinguish_strength_from_cost() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for name in ["a", "b", "c", "isolated"] {
		nodes.push(
			graph
				.add_node_with_id(format!("n:{name}").parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
		);
	}
	for (source, target, weight) in [(0, 1, 2), (0, 1, 5), (0, 2, 1), (1, 2, 1), (2, 1, 1)] {
		let edge = graph
			.add_edge(
				EdgeTopology::directed([nodes[source]], [nodes[target]]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		graph.set_edge_weight(edge, Some(Number::Int(weight))).unwrap();
	}
	let projection = capture(&graph);
	let degrees =
		analytics::degree(&projection, GraphAlgorithmLimits::default(), &mut || Ok(())).unwrap();
	assert_eq!(degrees.values[0].1.outgoing, 3);
	assert_eq!(degrees.values[0].1.outgoing_strength, Number::Int(8));
	assert_eq!(degrees.values[1].1.incoming, 3);
	assert_eq!(degrees.values[1].1.incoming_strength, Number::Int(8));
	for (selection, share) in
		[(WeightSelection::Unweighted, 2.0 / 3.0), (WeightSelection::Stored, 7.0 / 8.0)]
	{
		let mut options = PageRankOptions::default();
		options.weights = selection;
		let ranks = analytics::page_rank(&projection, options, &mut || Ok(())).unwrap();
		let q = 1.0 / 21.0;
		let b = (q * (1.0 + 0.85 * share) + 0.85 * (1.0 - 2.0 * q)) / 1.85;
		for (actual, expected) in
			ranks.values.iter().map(|(_, value)| *value).zip([q, b, 1.0 - 2.0 * q - b, q])
		{
			close(actual, expected);
		}
		close(ranks.values.iter().map(|(_, value)| value).sum(), 1.0);
		assert!(ranks.convergence.unwrap().residual <= options.iteration.tolerance);
	}
	let mut options = CentralityOptions::default();
	options.weights = WeightSelection::Stored;
	let closeness = analytics::harmonic_closeness(&projection, options, &mut || Ok(())).unwrap();
	close(closeness.values[0].1, 0.5);
	close(closeness.values[3].1, 0.0);
	options.normalized = false;
	let brandes = analytics::betweenness(&projection, options, &mut || Ok(())).unwrap();
	close(brandes.values[1].1, 0.0);
	close(brandes.values[2].1, 0.5);
	drop(projection);
	drop(graph);
	assert_eq!(closeness.values.len(), 4);
}

/// Verifies analytics api generated brandes and harmonic match exhaustive
/// oracles.
#[test]
fn analytics_api_generated_brandes_and_harmonic_match_exhaustive_oracles() {
	for bits in 0_u64..64 {
		let mut graph = Graph::<Mutable>::new();
		let mut nodes = Vec::new();
		for node in 0..3 {
			nodes.push(
				graph
					.add_node_with_id(
						format!("n:{node}").parse().unwrap(),
						None,
						PropertyMap::new(),
					)
					.unwrap(),
			);
		}
		let mut adjacency = [[false; 3]; 3];
		let mut bit = 0;
		for source in 0..3 {
			for target in 0..3 {
				if source == target {
					continue;
				}
				if bits & (1 << bit) != 0 {
					graph
						.add_edge(
							EdgeTopology::directed([nodes[source]], [nodes[target]]),
							None,
							PropertyMap::new(),
						)
						.unwrap();
					adjacency[source][target] = true;
				}
				bit += 1;
			}
		}
		let projection = capture(&graph);
		let mut expected_brandes = [0.0; 3];
		let mut expected_harmonic = [0.0; 3];
		for source in 0..3 {
			for target in 0..3 {
				if source == target {
					continue;
				}
				if adjacency[source][target] {
					expected_harmonic[source] += 0.5;
					continue;
				}
				let middle = 3 - source - target;
				if adjacency[source][middle] && adjacency[middle][target] {
					expected_harmonic[source] += 0.25;
					expected_brandes[middle] += 1.0;
				}
			}
		}
		let mut options = CentralityOptions::default();
		options.normalized = false;
		let brandes = analytics::betweenness(&projection, options, &mut || Ok(())).unwrap();
		let harmonic = analytics::harmonic_closeness(
			&projection,
			CentralityOptions::default(),
			&mut || Ok(()),
		)
		.unwrap();
		for row in 0..3 {
			close(brandes.values[row].1, expected_brandes[row]);
			close(harmonic.values[row].1, expected_harmonic[row]);
		}
	}
}

/// Verifies analytics api parallel shortest paths and undirected degrees are
/// exact.
#[test]
fn analytics_api_parallel_shortest_paths_and_undirected_degrees_are_exact() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for node in 0..4 {
		nodes.push(
			graph
				.add_node_with_id(format!("n:{node}").parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
		);
	}
	for (source, target) in [(0, 1), (0, 1), (0, 2), (1, 3), (2, 3)] {
		graph
			.add_edge(
				EdgeTopology::directed([nodes[source]], [nodes[target]]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
	}
	let mut options = CentralityOptions::default();
	options.normalized = false;
	let scores = analytics::betweenness(&capture(&graph), options, &mut || Ok(())).unwrap();
	close(scores.values[1].1, 2.0 / 3.0);
	close(scores.values[2].1, 1.0 / 3.0);
	let mut undirected = Graph::<Mutable>::new();
	let a = undirected.add_node(None, PropertyMap::new()).unwrap();
	let b = undirected.add_node(None, PropertyMap::new()).unwrap();
	undirected.add_edge(EdgeTopology::undirected([a, b]), None, PropertyMap::new()).unwrap();
	let values =
		analytics::degree(&capture(&undirected), GraphAlgorithmLimits::default(), &mut || Ok(()))
			.unwrap();
	for (_, value) in values.values {
		assert_eq!((value.incoming, value.outgoing, value.total), (1, 1, 1));
		assert_eq!(value.total_strength, Number::Int(1));
	}
}

/// Verifies analytics api spectral controls report convergence and original
/// residuals.
#[test]
fn analytics_api_spectral_controls_report_convergence_and_original_residuals() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:a".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:b".parse().unwrap(), None, PropertyMap::new()).unwrap();
	graph.add_edge(EdgeTopology::bidirectional([a], [b]), None, PropertyMap::new()).unwrap();
	let projection = capture(&graph);
	let eigen =
		analytics::eigenvector(&projection, SpectralOptions::default(), &mut || Ok(())).unwrap();
	for (_, value) in eigen.values {
		close(value, 1.0 / 2.0_f64.sqrt());
	}
	let katz = analytics::katz(&projection, KatzOptions::default(), &mut || Ok(())).unwrap();
	for (_, value) in katz.values {
		close(value, 2.0);
	}
	let mut invalid = KatzOptions::default();
	invalid.alpha = Some(1.0);
	assert!(analytics::katz(&projection, invalid, &mut || Ok(())).is_err());
	let mut short = KatzOptions::default();
	short.spectral.iteration.max_iterations = 1;
	let error = analytics::katz(&projection, short, &mut || Ok(())).unwrap_err();
	assert!(format!("{error:?}").contains("GraphAlgorithmDidNotConverge"));
	let empty = capture(&Graph::<Mutable>::new());
	assert!(
		analytics::degree(&empty, GraphAlgorithmLimits::default(), &mut || Ok(()))
			.unwrap()
			.values
			.is_empty()
	);
	assert!(
		analytics::page_rank(&empty, PageRankOptions::default(), &mut || Ok(()))
			.unwrap()
			.values
			.is_empty()
	);
	assert!(
		analytics::eigenvector(&empty, SpectralOptions::default(), &mut || Ok(()))
			.unwrap()
			.values
			.is_empty()
	);
	assert!(
		analytics::katz(&empty, KatzOptions::default(), &mut || Ok(())).unwrap().values.is_empty()
	);
}

/// Verifies analytics api rejects path counter overflow nonpositive costs and
/// limits.
#[test]
fn analytics_api_rejects_path_counter_overflow_nonpositive_costs_and_limits() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for _node in 0..65 {
		nodes.push(graph.add_node(None, PropertyMap::new()).unwrap());
	}
	for pair in nodes.windows(2) {
		for _ in 0..2 {
			graph
				.add_edge(EdgeTopology::directed([pair[0]], [pair[1]]), None, PropertyMap::new())
				.unwrap();
		}
	}
	let projection = capture(&graph);
	assert!(
		analytics::betweenness(&projection, CentralityOptions::default(), &mut || Ok(())).is_err()
	);
	assert!(
		analytics::harmonic_closeness(&projection, CentralityOptions::default(), &mut || Ok(()))
			.is_ok()
	);
	let mut limited = GraphAlgorithmLimits::default();
	limited.max_work = 1;
	assert!(analytics::degree(&projection, limited, &mut || Ok(())).is_err());
	assert!(
		analytics::page_rank(&projection, PageRankOptions::default(), &mut || Err(
			ferros_grynn::gql_status::FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
	let mut zero = Graph::<Mutable>::new();
	let a = zero.add_node(None, PropertyMap::new()).unwrap();
	let b = zero.add_node(None, PropertyMap::new()).unwrap();
	let edge = zero.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new()).unwrap();
	zero.set_edge_weight(edge, Some(Number::Int(0))).unwrap();
	let mut options = CentralityOptions::default();
	options.weights = WeightSelection::Stored;
	assert!(analytics::harmonic_closeness(&capture(&zero), options, &mut || Ok(())).is_err());
	assert!(analytics::betweenness(&capture(&zero), options, &mut || Ok(())).is_err());
	zero.set_edge_weight(edge, Some(Number::Int(-1))).unwrap();
	let mut rank = PageRankOptions::default();
	rank.weights = WeightSelection::Stored;
	assert!(analytics::page_rank(&capture(&zero), rank, &mut || Ok(())).is_err());
}

/// Verifies analytics api large numeric distances reject lossy domains.
#[test]
fn analytics_api_large_numeric_distances_reject_lossy_domains() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for name in ["s", "t", "u", "v"] {
		nodes.push(
			graph
				.add_node_with_id(format!("n:{name}").parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
		);
	}
	let large = (1_i64 << 53) + 1;
	for (source, target, weight) in [
		(0, 1, Number::Int(large)),
		(0, 2, Number::Int(large)),
		(2, 1, Number::Float(0.25)),
		(1, 3, Number::Int(1)),
	] {
		let edge = graph
			.add_edge(
				EdgeTopology::directed([nodes[source]], [nodes[target]]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		graph.set_edge_weight(edge, Some(weight)).unwrap();
	}
	let mut options = CentralityOptions::default();
	options.weights = WeightSelection::Stored;
	for result in [
		analytics::harmonic_closeness(&capture(&graph), options, &mut || Ok(())),
		analytics::betweenness(&capture(&graph), options, &mut || Ok(())),
	] {
		assert!(format!("{:?}", result.unwrap_err()).contains("OperationOverflow"));
	}
	let mut rounded = Graph::<Mutable>::new();
	let a = rounded.add_node(None, PropertyMap::new()).unwrap();
	let b = rounded.add_node(None, PropertyMap::new()).unwrap();
	let c = rounded.add_node(None, PropertyMap::new()).unwrap();
	for (source, target, cost) in [(a, b, 1.0e20), (b, c, 1.0)] {
		let edge = rounded
			.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
			.unwrap();
		rounded.set_edge_weight(edge, Some(Number::Float(cost))).unwrap();
	}
	assert!(analytics::harmonic_closeness(&capture(&rounded), options, &mut || Ok(())).is_err());
	assert!(analytics::betweenness(&capture(&rounded), options, &mut || Ok(())).is_err());
}

/// Verifies analytics api katz scales strengths before large intermediates.
#[test]
fn analytics_api_katz_scales_strengths_before_large_intermediates() {
	let mut graph = Graph::<Mutable>::new();
	let node = graph.add_node(None, PropertyMap::new()).unwrap();
	let edge =
		graph.add_edge(EdgeTopology::directed([node], [node]), None, PropertyMap::new()).unwrap();
	graph.set_edge_weight(edge, Some(Number::Float(1.0e308))).unwrap();
	let mut options = KatzOptions::default();
	options.spectral.weights = WeightSelection::Stored;
	let result = analytics::katz(&capture(&graph), options, &mut || Ok(())).unwrap();
	close(result.values[0].1, 2.0);
	graph.set_edge_weight(edge, Some(Number::Int(1))).unwrap();
	graph.add_edge(EdgeTopology::directed([node], [node]), None, PropertyMap::new()).unwrap();
	options.alpha = Some(0.0);
	options.beta = 1.0e308;
	let result = analytics::katz(&capture(&graph), options, &mut || Ok(())).unwrap();
	assert_eq!(result.values[0].1.partial_cmp(&options.beta), Some(std::cmp::Ordering::Equal));
}

/// Verifies analytics api weighted generated paths match independent
/// enumeration.
#[test]
fn analytics_api_weighted_generated_paths_match_independent_enumeration() {
	for bits in 0_u64..64 {
		let mut graph = Graph::<Mutable>::new();
		let mut nodes = Vec::new();
		for node in 0..3 {
			nodes.push(
				graph
					.add_node_with_id(
						format!("n:{node}").parse().unwrap(),
						None,
						PropertyMap::new(),
					)
					.unwrap(),
			);
		}
		let mut costs = [[None; 3]; 3];
		let mut multiplicity = [[0_u64; 3]; 3];
		let mut bit = 0;
		for source in 0..3 {
			for target in 0..3 {
				if source == target {
					continue;
				}
				if bits & (1 << bit) != 0 {
					let cost = 1 + ((bits + bit) % 4) as i64;
					let count = 1 + ((bits >> bit) & 1);
					for _ in 0..count {
						let edge = graph
							.add_edge(
								EdgeTopology::directed([nodes[source]], [nodes[target]]),
								None,
								PropertyMap::new(),
							)
							.unwrap();
						graph.set_edge_weight(edge, Some(Number::Int(cost))).unwrap();
					}
					costs[source][target] = Some(cost);
					multiplicity[source][target] = count;
				}
				bit += 1;
			}
		}
		let mut expected_harmonic = [0.0; 3];
		let mut expected_brandes = [0.0; 3];
		for source in 0..3 {
			for target in 0..3 {
				if source == target {
					continue;
				}
				let middle = 3 - source - target;
				let via = costs[source][middle].zip(costs[middle][target]).map(|(a, b)| a + b);
				let minimum = costs[source][target].into_iter().chain(via).min();
				if let Some(minimum) = minimum {
					expected_harmonic[source] += 0.5 / minimum as f64;
					if via == Some(minimum) {
						let count = multiplicity[source][middle] * multiplicity[middle][target];
						let direct = if costs[source][target] == Some(minimum) {
							multiplicity[source][target]
						} else {
							0
						};
						expected_brandes[middle] += count as f64 / (count + direct) as f64;
					}
				}
			}
		}
		let projection = capture(&graph);
		let mut options = CentralityOptions::default();
		options.weights = WeightSelection::Stored;
		let harmonic = analytics::harmonic_closeness(&projection, options, &mut || Ok(())).unwrap();
		options.normalized = false;
		let brandes = analytics::betweenness(&projection, options, &mut || Ok(())).unwrap();
		for node in 0..3 {
			close(harmonic.values[node].1, expected_harmonic[node]);
			close(brandes.values[node].1, expected_brandes[node]);
		}
	}
}

/// Verifies analytics api full state preserves rounded prefix ties and mixed
/// parallel costs.
#[test]
fn analytics_api_full_state_preserves_rounded_prefix_ties_and_mixed_parallel_costs() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for name in ["a", "b", "s", "t", "u"] {
		nodes.push(
			graph
				.add_node_with_id(format!("n:{name}").parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
		);
	}
	for (source, target, cost) in
		[(2, 0, 1.0), (0, 4, 1.0), (2, 1, 1.0000000000000004), (1, 4, 1.0), (4, 3, 1.0e16)]
	{
		let edge = graph
			.add_edge(
				EdgeTopology::directed([nodes[source]], [nodes[target]]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		graph.set_edge_weight(edge, Some(Number::Float(cost))).unwrap();
	}
	let mut options = CentralityOptions::default();
	options.weights = WeightSelection::Stored;
	options.normalized = false;
	let result = analytics::betweenness(&capture(&graph), options, &mut || Ok(())).unwrap();
	for (node, score) in [(0, 1.5), (1, 0.5), (4, 3.0)] {
		close(result.values[node].1, score);
	}
	let mut mixed = Graph::<Mutable>::new();
	let s = mixed.add_node_with_id("n:s".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let t = mixed.add_node_with_id("n:t".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let v = mixed.add_node_with_id("n:v".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let large = (1_i64 << 53) + 1;
	for (source, target, cost) in [
		(s, v, Number::Int(large)),
		(v, t, Number::Int(2)),
		(v, t, Number::Float(2.1)),
		(s, t, Number::Float((1_u64 << 53) as f64 + 2.0)),
	] {
		let edge = mixed
			.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
			.unwrap();
		mixed.set_edge_weight(edge, Some(cost)).unwrap();
	}
	let result = analytics::betweenness(&capture(&mixed), options, &mut || Ok(())).unwrap();
	close(result.values[2].1, 0.5);
	options.limits.max_work = 10;
	assert!(analytics::betweenness(&capture(&graph), options, &mut || Ok(())).is_err());
}

/// Verifies analytics api centrality distances exclude repeated projected
/// vertices.
#[test]
fn analytics_api_centrality_distances_exclude_repeated_projected_vertices() {
	let mut graph = Graph::<Mutable>::new();
	let s = graph.add_node_with_id("n:s".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let t = graph.add_node_with_id("n:t".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let v = graph.add_node_with_id("n:v".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let big = 1_i64 << 60;
	for (source, target, cost) in [
		(s, v, Number::Int(big + 255)),
		(v, v, Number::Float(2.1)),
		(v, t, Number::Int(258)),
		(s, t, Number::Int(big + 513)),
	] {
		let edge = graph
			.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
			.unwrap();
		graph.set_edge_weight(edge, Some(cost)).unwrap();
	}
	let mut options = CentralityOptions::default();
	options.weights = WeightSelection::Stored;
	options.normalized = false;
	let result = analytics::betweenness(&capture(&graph), options, &mut || Ok(())).unwrap();
	close(result.values[2].1, 0.5);
}

/// Verifies analytics api pagerank normalizes subnormal strengths before
/// multiplication.
#[test]
fn analytics_api_pagerank_normalizes_subnormal_strengths_before_multiplication() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:a".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:b".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let edge = graph.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new()).unwrap();
	let mut options = PageRankOptions::default();
	options.weights = WeightSelection::Stored;
	for weight in [1.0, f64::from_bits(1)] {
		graph.set_edge_weight(edge, Some(Number::Float(weight))).unwrap();
		let result = analytics::page_rank(&capture(&graph), options, &mut || Ok(())).unwrap();
		close(result.values[0].1, 1.0 / 2.85);
		close(result.values[1].1, 1.85 / 2.85);
	}
}

/// Verifies analytics api bidirectional hyperedge allows connector reuse across
/// projected pairs.
#[test]
fn analytics_api_bidirectional_hyperedge_allows_connector_reuse_across_projected_pairs() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:a".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:b".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let t = graph.add_node_with_id("n:t".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let edge =
		graph.add_edge(EdgeTopology::bidirectional([a, b], [t]), None, PropertyMap::new()).unwrap();
	let mut options = CentralityOptions::default();
	options.weights = WeightSelection::Stored;
	options.normalized = false;
	for weight in [Number::Int(1), Number::Float(1.0)] {
		graph.set_edge_weight(edge, Some(weight)).unwrap();
		let projection = capture(&graph);
		let scores = analytics::betweenness(&projection, options, &mut || Ok(())).unwrap();
		close(scores.values[2].1, 2.0);
		let harmonic = analytics::harmonic_closeness(&projection, options, &mut || Ok(())).unwrap();
		close(harmonic.values[0].1, 1.5);
	}
}

/// Verifies analytics api directed motifs and clustering match independent
/// dense oracle.
#[test]
fn analytics_api_directed_motifs_and_clustering_match_independent_dense_oracle() {
	for bits in 0_u64..64 {
		let mut graph = Graph::<Mutable>::new();
		let mut nodes = Vec::new();
		for i in 0..3 {
			nodes.push(
				graph
					.add_node_with_id(format!("n:{i}").parse().unwrap(), None, PropertyMap::new())
					.unwrap(),
			);
		}
		let mut adjacency = [[0_u64; 3]; 3];
		let mut bit = 0;
		for a in 0..3 {
			graph
				.add_edge(EdgeTopology::directed([nodes[a]], [nodes[a]]), None, PropertyMap::new())
				.unwrap();
			for b in 0..3 {
				if a == b {
					continue;
				}
				if bits & (1 << bit) != 0 {
					for _ in 0..2 {
						graph
							.add_edge(
								EdgeTopology::directed([nodes[a]], [nodes[b]]),
								None,
								PropertyMap::new(),
							)
							.unwrap();
					}
					adjacency[a][b] = 1;
				}
				bit += 1;
			}
		}
		let mut triangle = TriangleOptions::default();
		triangle.orientation = AnalyticsOrientation::Directed;
		let projection = capture(&graph);
		let motifs = analytics::triangles(&projection, triangle, &mut || Ok(())).unwrap();
		let mut clustering = ClusteringOptions::default();
		clustering.orientation = AnalyticsOrientation::Directed;
		let coefficients = analytics::clustering(&projection, clustering, &mut || Ok(())).unwrap();
		let mut cycles = [0; 3];
		let mut transitive = [0; 3];
		let mut numerator = 0.0;
		let mut denominator = 0.0;
		let mut mean = 0.0;
		for a in 0..3 {
			let mut cube = 0_u64;
			for b in 0..3 {
				for c in 0..3 {
					cycles[a] += adjacency[a][b] * adjacency[b][c] * adjacency[c][a];
					if adjacency[a][b] * adjacency[b][c] * adjacency[a][c] != 0 {
						for node in [a, b, c] {
							transitive[node] += 1;
						}
					}
					cube += (adjacency[a][b] + adjacency[b][a])
						* (adjacency[b][c] + adjacency[c][b])
						* (adjacency[c][a] + adjacency[a][c]);
				}
			}
			let out: u64 = adjacency[a].iter().sum();
			let incoming: u64 = (0..3).map(|b| adjacency[b][a]).sum();
			let reciprocal: u64 = (0..3).map(|b| adjacency[a][b] * adjacency[b][a]).sum();
			let total = out + incoming;
			let possible = 2 * (total * total.saturating_sub(1) - 2 * reciprocal);
			let expected = if possible == 0 {
				0.0
			} else {
				cube as f64 / possible as f64
			};
			close(coefficients.local.values[a].1, expected);
			mean += expected / 3.0;
			numerator += cube as f64;
			denominator += possible as f64;
		}
		close(coefficients.mean, mean);
		close(
			coefficients.transitivity,
			if denominator == 0.0 {
				0.0
			} else {
				numerator / denominator
			},
		);
		for a in 0..3 {
			assert_eq!(motifs.local.values[a].1.cycles, cycles[a]);
			assert_eq!(motifs.local.values[a].1.transitive, transitive[a]);
		}
		assert_eq!(motifs.totals.cycles, cycles.iter().sum::<u64>() / 3);
		assert_eq!(motifs.totals.transitive, transitive.iter().sum::<u64>() / 3);
	}
}

/// Verifies analytics api weighted triangle clustering and orientation limits.
#[test]
fn analytics_api_weighted_triangle_clustering_and_orientation_limits() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for i in 0..4 {
		nodes.push(
			graph
				.add_node_with_id(format!("n:{i}").parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
		);
	}
	for (a, b, w) in [(0, 1, 1), (1, 2, 2), (0, 2, 4)] {
		let edge = graph
			.add_edge(EdgeTopology::undirected([nodes[a], nodes[b]]), None, PropertyMap::new())
			.unwrap();
		graph.set_edge_weight(edge, Some(Number::Int(w))).unwrap();
	}
	let projection = capture(&graph);
	let motifs =
		analytics::triangles(&projection, TriangleOptions::default(), &mut || Ok(())).unwrap();
	assert_eq!(motifs.totals.undirected, 1);
	let binary =
		analytics::clustering(&projection, ClusteringOptions::default(), &mut || Ok(())).unwrap();
	close(binary.mean, 0.75);
	close(binary.transitivity, 1.0);
	let mut weighted = ClusteringOptions::default();
	weighted.weights = WeightSelection::Stored;
	let coefficients = analytics::clustering(&projection, weighted, &mut || Ok(())).unwrap();
	for (_, value) in coefficients.local.values.iter().take(3) {
		close(*value, 0.5);
	}
	close(coefficients.mean, 0.375);
	close(coefficients.transitivity, 0.5);
	weighted.orientation = AnalyticsOrientation::Directed;
	let directed = analytics::clustering(&projection, weighted, &mut || Ok(())).unwrap();
	for (a, b) in coefficients.local.values.iter().zip(&directed.local.values) {
		close(a.1, b.1);
	}
	let mut limited = TriangleOptions::default();
	limited.limits.max_work = 1;
	assert!(analytics::triangles(&projection, limited, &mut || Ok(())).is_err());
	let mut asymmetric = Graph::<Mutable>::new();
	let a = asymmetric.add_node(None, PropertyMap::new()).unwrap();
	let b = asymmetric.add_node(None, PropertyMap::new()).unwrap();
	asymmetric.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new()).unwrap();
	assert!(
		analytics::triangles(&capture(&asymmetric), TriangleOptions::default(), &mut || Ok(()))
			.is_err()
	);
}

/// Verifies analytics api label propagation votes are exact and canonical.
#[test]
fn analytics_api_label_propagation_votes_are_exact_and_canonical() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:a".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:b".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let c = graph.add_node_with_id("n:c".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let big = 1_i64 << 53;
	for (target, weight) in [(b, big), (c, big + 1)] {
		let edge = graph
			.add_edge(EdgeTopology::directed([a], [target]), None, PropertyMap::new())
			.unwrap();
		graph.set_edge_weight(edge, Some(Number::Int(weight))).unwrap();
	}
	let projection = capture(&graph);
	let mut options = LabelPropagationOptions::default();
	options.weights = WeightSelection::Stored;
	options.votes = ferros_grynn::graph::TraversalDirection::Forward;
	let result = analytics::label_propagation(&projection, options, &mut || Ok(())).unwrap();
	assert_eq!(result.assignments.values[0].1, result.assignments.values[2].1);
	assert_ne!(result.assignments.values[0].1, result.assignments.values[1].1);
	options.votes = ferros_grynn::graph::TraversalDirection::Backward;
	let result = analytics::label_propagation(&projection, options, &mut || Ok(())).unwrap();
	assert!(result.assignments.values.iter().all(|(_, group)| group == &projection.atoms()[0]));
	options.max_sweeps = 1;
	assert!(analytics::label_propagation(&projection, options, &mut || Ok(())).is_err());
	let mut ties = Graph::<Mutable>::new();
	let a = ties.add_node_with_id("n:a".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = ties.add_node_with_id("n:b".parse().unwrap(), None, PropertyMap::new()).unwrap();
	for (source, target, weight) in [(a, a, 0.5), (b, b, 0.5), (a, b, 1.0)] {
		let edge = ties
			.add_edge(EdgeTopology::directed([source], [target]), None, PropertyMap::new())
			.unwrap();
		ties.set_edge_weight(edge, Some(Number::Float(weight))).unwrap();
	}
	options = LabelPropagationOptions::default();
	options.weights = WeightSelection::Stored;
	let result = analytics::label_propagation(&capture(&ties), options, &mut || Ok(())).unwrap();
	assert_ne!(result.assignments.values[0].1, result.assignments.values[1].1);
}

/// Verifies analytics api louvain aggregates and reports checked limits.
#[test]
fn analytics_api_louvain_aggregates_and_reports_checked_limits() {
	let mut graph = Graph::<Mutable>::new();
	let mut nodes = Vec::new();
	for i in 0..4 {
		nodes.push(
			graph
				.add_node_with_id(format!("n:{i}").parse().unwrap(), None, PropertyMap::new())
				.unwrap(),
		);
	}
	for (a, b) in [(0, 1), (2, 3)] {
		graph
			.add_edge(EdgeTopology::undirected([nodes[a], nodes[b]]), None, PropertyMap::new())
			.unwrap();
	}
	let projection = capture(&graph);
	let options = LouvainOptions::default();
	let result = analytics::louvain(&projection, options, &mut || Ok(())).unwrap();
	close(result.modularity.unwrap(), 0.5);
	assert_eq!(result.assignments.values[0].1, projection.atoms()[0]);
	assert_eq!(result.assignments.values[1].1, projection.atoms()[0]);
	assert_eq!(result.assignments.values[2].1, projection.atoms()[2]);
	assert_eq!(result.assignments.values[3].1, projection.atoms()[2]);
	assert!(result.levels >= 2);
	let mut limited = options;
	limited.max_sweeps = 1;
	assert!(analytics::louvain(&projection, limited, &mut || Ok(())).is_err());
	limited = options;
	limited.max_levels = 1;
	assert!(analytics::louvain(&projection, limited, &mut || Ok(())).is_err());
	limited = options;
	limited.limits.memory_bytes = 1024;
	assert!(analytics::louvain(&projection, limited, &mut || Ok(())).is_err());
}

/// Verifies analytics api directed louvain modularity matches independent dense
/// objective.
#[test]
fn analytics_api_directed_louvain_modularity_matches_independent_dense_objective() {
	for bits in 0_u64..64 {
		let mut graph = Graph::<Mutable>::new();
		let mut nodes = Vec::new();
		for i in 0..3 {
			nodes.push(
				graph
					.add_node_with_id(format!("n:{i}").parse().unwrap(), None, PropertyMap::new())
					.unwrap(),
			);
		}
		let mut matrix = [[0.0; 3]; 3];
		let mut bit = 0;
		for a in 0..3 {
			for b in 0..3 {
				let selected = if a == b {
					bits & 1 != 0
				} else {
					let selected = bits & (1 << bit) != 0;
					bit += 1;
					selected
				};
				if selected {
					let weight = 1 + ((bits as usize + a + b) % 4) as i64;
					let edge = graph
						.add_edge(
							EdgeTopology::directed([nodes[a]], [nodes[b]]),
							None,
							PropertyMap::new(),
						)
						.unwrap();
					graph.set_edge_weight(edge, Some(Number::Int(weight))).unwrap();
					matrix[a][b] = weight as f64;
				}
			}
		}
		let mut options = LouvainOptions::default();
		options.orientation = AnalyticsOrientation::Directed;
		options.weights = WeightSelection::Stored;
		let outcome = analytics::louvain(&capture(&graph), options, &mut || Ok(()));
		assert!(outcome.is_ok(), "bits={bits}: {outcome:?}");
		let result = outcome.unwrap();
		let total: f64 = matrix.iter().flat_map(|row| row.iter()).sum();
		let outgoing: Vec<f64> = matrix.iter().map(|row| row.iter().sum()).collect();
		let incoming: Vec<f64> = (0..3).map(|b| (0..3).map(|a| matrix[a][b]).sum()).collect();
		let mut expected = 0.0;
		let mut initial = 0.0;
		if total > 0.0 {
			for a in 0..3 {
				for b in 0..3 {
					let contribution =
						matrix[a][b] / total - outgoing[a] / total * incoming[b] / total;
					if a == b {
						initial += contribution;
					}
					if result.assignments.values[a].1 == result.assignments.values[b].1 {
						expected += contribution;
					}
				}
			}
		}
		close(result.modularity.unwrap(), expected);
		assert!(expected + 1.0e-12 >= initial);
	}
}

/// Verifies analytics api structural empty rows obey work and cancellation
/// preflight.
#[test]
fn analytics_api_structural_empty_rows_obey_work_and_cancellation_preflight() {
	let mut graph = Graph::<Mutable>::new();
	for _ in 0..128 {
		graph.add_node(None, PropertyMap::new()).unwrap();
	}
	let projection = capture(&graph);
	let mut triangles = TriangleOptions::default();
	triangles.limits.max_work = 1;
	assert!(analytics::triangles(&projection, triangles, &mut || Ok(())).is_err());
	triangles.limits.max_work = 3 * 128;
	assert!(analytics::triangles(&projection, triangles, &mut || Ok(())).is_err());
	let mut clustering = ClusteringOptions::default();
	clustering.limits.max_work = 1;
	assert!(analytics::clustering(&projection, clustering, &mut || Ok(())).is_err());
	let mut lpa = LabelPropagationOptions::default();
	lpa.limits.max_work = 1;
	assert!(analytics::label_propagation(&projection, lpa, &mut || Ok(())).is_err());
	let mut louvain = LouvainOptions::default();
	louvain.limits.max_work = 1;
	assert!(analytics::louvain(&projection, louvain, &mut || Ok(())).is_err());
	assert!(
		analytics::triangles(&projection, TriangleOptions::default(), &mut || Err(
			ferros_grynn::gql_status::FerrosGrynnError::query_cancelled()
		))
		.is_err()
	);
}

/// Verifies analytics api louvain certifies moves hidden by floating
/// cancellation.
#[test]
fn analytics_api_louvain_certifies_moves_hidden_by_floating_cancellation() {
	for (matrix, half) in
		[vec![vec![1073741825_i64, 0, 0], vec![1073741824, 0, 0], vec![1073741824, 3, 3]], vec![
			vec![1073741825, 0, 0, 1125899906842625],
			vec![0, 1073741824, 3, 0],
			vec![1125899906842625, 1, 1, 1073741825],
			vec![1125899906842624, 1125899906842624, 0, 0],
		]]
		.into_iter()
		.flat_map(|matrix| [(matrix.clone(), false), (matrix, true)])
	{
		let mut graph = Graph::<Mutable>::new();
		let nodes: Vec<_> = (0..matrix.len())
			.map(|i| {
				graph
					.add_node_with_id(format!("n:{i}").parse().unwrap(), None, PropertyMap::new())
					.unwrap()
			})
			.collect();
		for (a, row) in matrix.iter().enumerate() {
			for (b, weight) in row.iter().copied().enumerate() {
				if weight != 0 {
					let edge = graph
						.add_edge(
							EdgeTopology::directed([nodes[a]], [nodes[b]]),
							None,
							PropertyMap::new(),
						)
						.unwrap();
					graph
						.set_edge_weight(
							edge,
							Some(if half {
								Number::Float(weight as f64 / 2.0)
							} else {
								Number::Int(weight)
							}),
						)
						.unwrap();
				}
			}
		}
		let projection = capture(&graph);
		let mut options = LouvainOptions::default();
		options.orientation = AnalyticsOrientation::Directed;
		options.weights = WeightSelection::Stored;
		let result = analytics::louvain(&projection, options, &mut || Ok(())).unwrap();
		let labels: Vec<_> = result
			.assignments
			.values
			.iter()
			.map(|(_, label)| projection.atoms().iter().position(|atom| atom == label).unwrap())
			.collect();
		let total: i128 = matrix.iter().flatten().map(|w| i128::from(*w)).sum();
		for node in 0..nodes.len() {
			let out_node: i128 = matrix[node].iter().map(|w| i128::from(*w)).sum();
			let in_node: i128 = matrix.iter().map(|row| i128::from(row[node])).sum();
			let score = |group| {
				let mut outgoing = 0_i128;
				let mut incoming = 0_i128;
				let mut incident = 0_i128;
				for other in 0..nodes.len() {
					if other != node && labels[other] == group {
						outgoing += matrix[other].iter().map(|w| i128::from(*w)).sum::<i128>();
						incoming += matrix.iter().map(|row| i128::from(row[other])).sum::<i128>();
						incident +=
							i128::from(matrix[node][other]) + i128::from(matrix[other][node]);
					}
				}
				incident * total - out_node * incoming - in_node * outgoing
			};
			for candidate in 0..=nodes.len() {
				assert!(
					score(candidate) <= score(labels[node]),
					"matrix={matrix:?}, labels={labels:?}, node={node}, candidate={candidate}"
				);
			}
		}
	}
}
