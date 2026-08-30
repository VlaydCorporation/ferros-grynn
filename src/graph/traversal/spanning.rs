use rapidhash::RapidHashMap;

use super::{algorithm_failure, connectivity::collect_node_ids, shortest_paths::collect_weights};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, EdgeRead, GraphRead, general::atom_does_not_exist},
};

#[derive(Debug, Clone, PartialEq)]
pub struct SpanningForest {
	edges: Box<[AtomId]>,
	total_weight: f64,
	components: usize,
}

impl SpanningForest {
	pub fn edges(&self) -> &[AtomId] {
		&self.edges
	}

	pub const fn total_weight(&self) -> f64 {
		self.total_weight
	}

	pub const fn component_count(&self) -> usize {
		self.components
	}
}

pub fn minimum_spanning_forest<R: GraphRead>(graph: &R) -> FerrosGrynnResult<SpanningForest> {
	let nodes = collect_node_ids(graph)?;
	let positions =
		nodes.iter().enumerate().map(|(index, id)| (*id, index)).collect::<RapidHashMap<_, _>>();
	let weights = collect_weights(graph, true, "minimum spanning forest")?;
	let mut candidates = binary_edges(graph)?
		.into_iter()
		.map(|edge| {
			let weight = weights.get(&edge.edge).copied().ok_or_else(|| {
				algorithm_failure(
					"minimum spanning forest",
					format!("missing weight for edge {}", edge.edge),
				)
			})?;
			Ok((weight, edge))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	candidates.sort_by(|(left_weight, left), (right_weight, right)| {
		left_weight.total_cmp(right_weight).then_with(|| left.edge.cmp(&right.edge))
	});

	let mut sets = DisjointSets::new(nodes.len());
	let mut selected = Vec::with_capacity(nodes.len().saturating_sub(1));
	let mut total_weight = 0.0;
	let mut components = nodes.len();
	for (weight, edge) in candidates {
		let Some(left) = positions.get(&edge.left).copied() else {
			return Err(atom_does_not_exist(edge.left));
		};
		let Some(right) = positions.get(&edge.right).copied() else {
			return Err(atom_does_not_exist(edge.right));
		};
		if sets.union(left, right) {
			selected.push(edge.edge);
			total_weight += weight;
			if !total_weight.is_finite() {
				return Err(algorithm_failure(
					"minimum spanning forest",
					"total weight overflowed f64",
				));
			}
			components = components.saturating_sub(1);
		}
	}
	Ok(SpanningForest {
		edges: selected.into_boxed_slice(),
		total_weight,
		components,
	})
}

pub fn minimum_spanning_tree<R: GraphRead>(graph: &R) -> FerrosGrynnResult<SpanningForest> {
	let result = minimum_spanning_forest(graph)?;
	if result.component_count() > 1 {
		return Err(algorithm_failure(
			"minimum spanning tree",
			"the local vertex projection is disconnected",
		));
	}
	Ok(result)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct BinaryEdge {
	pub(super) edge: AtomId,
	pub(super) left: AtomId,
	pub(super) right: AtomId,
}

#[derive(Debug, Clone)]
pub(super) struct DisjointSets {
	parents: Vec<usize>,
	ranks: Vec<u8>,
}

impl DisjointSets {
	pub(super) fn new(len: usize) -> Self {
		Self {
			parents: (0..len).collect(),
			ranks: vec![0; len],
		}
	}

	fn find(&mut self, mut value: usize) -> usize {
		let mut root = value;
		while self.parents[root] != root {
			root = self.parents[root];
		}
		while self.parents[value] != value {
			let parent = self.parents[value];
			self.parents[value] = root;
			value = parent;
		}
		root
	}

	pub(super) fn union(&mut self, left: usize, right: usize) -> bool {
		let mut left = self.find(left);
		let mut right = self.find(right);
		if left == right {
			return false;
		}
		if self.ranks[left] < self.ranks[right] {
			std::mem::swap(&mut left, &mut right);
		}
		self.parents[right] = left;
		if self.ranks[left] == self.ranks[right] {
			self.ranks[left] = self.ranks[left].saturating_add(1);
		}
		true
	}
}

pub(super) fn binary_edges<R: GraphRead>(graph: &R) -> FerrosGrynnResult<Vec<BinaryEdge>> {
	let mut edges = Vec::with_capacity(graph.edge_count()?);
	for id in graph.edge_ids()? {
		let id = id?;
		let edge = graph.edge(id)?.ok_or_else(|| {
			algorithm_failure("binary graph algorithm", format!("missing edge {id}"))
		})?;
		let topology = edge.topology()?;
		let Some((left, right, direction)) = topology.local_binary() else {
			return Err(algorithm_failure(
				"binary graph algorithm",
				format!("edge {id} is hyper, meta or cross-level"),
			));
		};
		if left.kind() != Some(AtomKind::Node) || right.kind() != Some(AtomKind::Node) {
			return Err(algorithm_failure(
				"binary graph algorithm",
				format!("edge {id} has a non-vertex endpoint"),
			));
		}
		let _ = direction;
		edges.push(BinaryEdge {
			edge: id,
			left,
			right,
		});
	}
	Ok(edges)
}
