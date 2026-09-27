use std::collections::VecDeque;

use crate::{
	datastore::{ClassSchema, Datastore, Transaction},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Number, Object, PropertyMap, RecordId, Value},
};

#[derive(Clone, Copy)]
enum OracleMode {
	Walk,
	Trail,
	Acyclic,
	Simple,
}

impl OracleMode {
	fn source(self) -> &'static str {
		match self {
			Self::Walk => "WALK",
			Self::Trail => "TRAIL",
			Self::Acyclic => "ACYCLIC",
			Self::Simple => "SIMPLE",
		}
	}
}

fn query_lengths(transaction: &mut Transaction, source: &str) -> Vec<usize> {
	transaction.execute(source, &Object::new()).unwrap()[0]
		.result
		.as_ref()
		.unwrap()
		.value()
		.as_array()
		.unwrap()
		.iter()
		.map(|row| match row.as_object().unwrap().get("length").unwrap() {
			Value::Number(Number::Int(value)) => usize::try_from(*value).unwrap(),
			value => panic!("path length must be an integer, got {value:?}"),
		})
		.collect()
}

fn oracle_path_count(
	edges: &[(usize, usize)],
	source: usize,
	target: usize,
	mode: OracleMode,
) -> usize {
	fn visit(
		edges: &[(usize, usize)],
		target: usize,
		mode: OracleMode,
		nodes: &mut Vec<usize>,
		used_edges: &mut Vec<usize>,
		count: &mut usize,
	) {
		let current = *nodes.last().unwrap();
		if current == target {
			*count += 1;
		}
		if used_edges.len() == 3 {
			return;
		}
		for (edge, &(from, to)) in edges.iter().enumerate() {
			if from != current {
				continue;
			}
			let closes_simple = matches!(mode, OracleMode::Simple) && to == nodes[0];
			let allowed = match mode {
				OracleMode::Walk => true,
				OracleMode::Trail => !used_edges.contains(&edge),
				OracleMode::Acyclic => !nodes.contains(&to),
				OracleMode::Simple => closes_simple || !nodes.contains(&to),
			};
			if !allowed {
				continue;
			}
			used_edges.push(edge);
			nodes.push(to);
			if closes_simple {
				if to == target {
					*count += 1;
				}
			} else {
				visit(edges, target, mode, nodes, used_edges, count);
			}
			nodes.pop();
			used_edges.pop();
		}
	}

	let mut count = 0;
	visit(edges, target, mode, &mut vec![source], &mut Vec::new(), &mut count);
	count
}

fn oracle_shortest(edges: &[(usize, usize)], source: usize, target: usize) -> Option<usize> {
	let mut distance = [usize::MAX; 3];
	let mut queue = VecDeque::from([source]);
	distance[source] = 0;
	while let Some(current) = queue.pop_front() {
		if current == target {
			return Some(distance[current]);
		}
		for &(_, next) in edges.iter().filter(|&&(from, _)| from == current) {
			if distance[next] == usize::MAX {
				distance[next] = distance[current] + 1;
				queue.push_back(next);
			}
		}
	}
	None
}

#[test]
fn generated_small_graphs_match_the_path_mode_and_shortest_oracle() {
	const CANDIDATE_EDGES: [(usize, usize); 4] = [(0, 0), (0, 1), (1, 0), (1, 2)];
	const MASKS: [u8; 8] = [0, 1, 3, 5, 7, 9, 13, 15];

	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("oracle_node", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("oracle_edge", AtomKind::Edge, []).unwrap()).unwrap();
	let nodes = ["oracle_node:n0", "oracle_node:n1", "oracle_node:n2"]
		.map(|value| value.parse::<RecordId>().unwrap());
	for node in &nodes {
		seed.create_atom(node.clone(), PropertyMap::new()).unwrap();
	}
	let edge_ids = MASKS.map(|mask| {
		CANDIDATE_EDGES.map(|(from, to)| {
			format!("oracle_edge:m{mask}_{from}_{to}").parse::<RecordId>().unwrap()
		})
	});
	for edges in &edge_ids {
		for edge in edges {
			seed.create_atom(edge.clone(), PropertyMap::new()).unwrap();
		}
	}
	for (mask_index, mask) in MASKS.into_iter().enumerate() {
		let graph = seed.define_graph(format!("oracle_{mask}"), GraphKind::Graph).unwrap();
		let local = nodes
			.iter()
			.map(|node| seed.attach_node(graph.id(), node, None, None).unwrap())
			.collect::<Vec<_>>();
		for (edge_index, &(from, to)) in CANDIDATE_EDGES.iter().enumerate() {
			if mask & (1 << edge_index) != 0 {
				seed.attach_edge(
					graph.id(),
					&edge_ids[mask_index][edge_index],
					None,
					EdgeTopology::directed([local[from]], [local[to]]),
					None,
					None,
				)
				.unwrap();
			}
		}
	}
	seed.commit().unwrap();

	let mut transaction = store.begin();
	for mask in MASKS {
		let edges = CANDIDATE_EDGES
			.into_iter()
			.enumerate()
			.filter_map(|(edge, endpoints)| (mask & (1 << edge) != 0).then_some(endpoints))
			.collect::<Vec<_>>();
		for source in 0..3 {
			for target in 0..3 {
				for mode in
					[OracleMode::Walk, OracleMode::Trail, OracleMode::Acyclic, OracleMode::Simple]
				{
					let query = format!(
						"MATCH REPEATABLE ELEMENTS path = (oracle_node:n{source} AS \
						 a)-[oracle_edge*0..3 {}]->(oracle_node:n{target} AS b) IN GRAPH \
						 oracle_{mask} RETURN path::length(path) AS length",
						mode.source(),
					);
					assert_eq!(
						query_lengths(&mut transaction, &query).len(),
						oracle_path_count(&edges, source, target, mode),
						"mask={mask:#06b}, source={source}, target={target}, mode={}",
						mode.source(),
					);
				}
				let query = format!(
					"MATCH REPEATABLE ELEMENTS path = (oracle_node:n{source} AS \
					 a)-[oracle_edge*0..3 SHORTEST]->(oracle_node:n{target} AS b) IN GRAPH \
					 oracle_{mask} RETURN path::length(path) AS length",
				);
				let actual = query_lengths(&mut transaction, &query);
				match oracle_shortest(&edges, source, target) {
					Some(expected) => assert_eq!(actual, [expected], "mask={mask:#06b}"),
					None => assert!(actual.is_empty(), "mask={mask:#06b}"),
				}
			}
		}
	}
}
