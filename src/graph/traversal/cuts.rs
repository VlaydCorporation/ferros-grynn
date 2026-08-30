use std::collections::hash_map;

use rapidhash::{RapidHashMap, RapidHashSet};

use super::{algorithm_failure, connectivity::collect_node_ids, spanning::binary_edges};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomId, GraphRead},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutSet {
	articulation_points: Box<[AtomId]>,
	bridges: Box<[AtomId]>,
}

impl CutSet {
	pub fn articulation_points(&self) -> &[AtomId] {
		&self.articulation_points
	}

	pub fn bridges(&self) -> &[AtomId] {
		&self.bridges
	}
}

pub fn find_cuts<R: GraphRead>(graph: &R) -> FerrosGrynnResult<CutSet> {
	let nodes = collect_node_ids(graph)?;
	let mut adjacency = nodes
		.iter()
		.copied()
		.map(|id| (id, Vec::<(AtomId, AtomId)>::new()))
		.collect::<RapidHashMap<_, _>>();
	for edge in binary_edges(graph)? {
		adjacency.entry(edge.left).or_default().push((edge.right, edge.edge));
		adjacency.entry(edge.right).or_default().push((edge.left, edge.edge));
	}

	let mut discovered = RapidHashMap::default();
	let mut low = RapidHashMap::default();
	let mut parent = RapidHashMap::default();
	let mut parent_edge = RapidHashMap::default();
	let mut children = RapidHashMap::default();
	let mut articulation = RapidHashSet::default();
	let mut bridges = Vec::new();
	let mut time = 0_usize;

	for root in nodes {
		if discovered.contains_key(&root) {
			continue;
		}
		discovered.insert(root, time);
		low.insert(root, time);
		time += 1;
		let mut stack = vec![CutFrame {
			node: root,
			next: 0,
		}];

		while !stack.is_empty() {
			let last = stack.len() - 1;
			let node = stack[last].node;
			let neighbors = adjacency.get(&node).map_or(&[][..], Vec::as_slice);
			if stack[last].next < neighbors.len() {
				let (next, edge) = neighbors[stack[last].next];
				stack[last].next += 1;
				if parent_edge.get(&node).copied() == Some(edge) {
					continue;
				}

				if let hash_map::Entry::Vacant(entry) = discovered.entry(next) {
					parent.insert(next, node);
					parent_edge.insert(next, edge);
					*children.entry(node).or_insert(0_usize) += 1;
					entry.insert(time);
					low.insert(next, time);
					time += 1;
					stack.push(CutFrame {
						node: next,
						next: 0,
					});
				} else if let (Some(next_discovery), Some(node_low)) =
					(discovered.get(&next).copied(), low.get_mut(&node))
				{
					*node_low = (*node_low).min(next_discovery);
				}
				continue;
			}

			let Some(frame) = stack.pop() else {
				return Err(algorithm_failure("cut search", "DFS stack became inconsistent"));
			};
			let node = frame.node;
			let Some(parent_node) = parent.get(&node).copied() else {
				if children.get(&node).copied().unwrap_or(0) > 1 {
					articulation.insert(node);
				}
				continue;
			};
			let Some(node_low) = low.get(&node).copied() else {
				return Err(algorithm_failure("cut search", "missing low-link value"));
			};
			let Some(parent_discovery) = discovered.get(&parent_node).copied() else {
				return Err(algorithm_failure("cut search", "missing parent discovery value"));
			};
			if parent.contains_key(&parent_node) && node_low >= parent_discovery {
				articulation.insert(parent_node);
			}
			if node_low > parent_discovery {
				let Some(edge) = parent_edge.get(&node).copied() else {
					return Err(algorithm_failure("cut search", "missing parent edge"));
				};
				bridges.push(edge);
			}
			let Some(parent_low) = low.get_mut(&parent_node) else {
				return Err(algorithm_failure("cut search", "missing parent low-link value"));
			};
			*parent_low = (*parent_low).min(node_low);
		}
	}

	let mut articulation = articulation.into_iter().collect::<Vec<_>>();
	articulation.sort_unstable();
	bridges.sort_unstable();
	bridges.dedup();
	Ok(CutSet {
		articulation_points: articulation.into_boxed_slice(),
		bridges: bridges.into_boxed_slice(),
	})
}

#[derive(Debug, Clone, Copy)]
struct CutFrame {
	node: AtomId,
	next: usize,
}
