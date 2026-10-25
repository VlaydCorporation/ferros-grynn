use rapidhash::RapidHashMap;

use super::{
	algorithm_failure,
	connectivity::collect_node_ids_with_check,
	shortest_paths::collect_weights_with_check,
};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, EdgeRead, GraphRead, general::atom_does_not_exist},
	math::add_numbers,
	values::Number,
};

/// Selected spanning edges, combined weight and connected-component count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanningForest {
	/// Edges associated with this spanning forest.
	edges: Box<[AtomId]>,
	/// Total weight associated with this spanning forest.
	total_weight: Number,
	/// Components associated with this spanning forest.
	components: usize,
}

impl SpanningForest {
	/// Borrows the stored edges of this `SpanningForest`.
	pub fn edges(&self) -> &[AtomId] {
		&self.edges
	}

	/// Returns the stored total weight of this `SpanningForest`.
	pub const fn total_weight(&self) -> Number {
		self.total_weight
	}

	/// Returns the stored components of this `SpanningForest`.
	pub const fn component_count(&self) -> usize {
		self.components
	}
}

/// Returns minimum spanning forest for this module.
///
/// # Errors
///
/// Propagates validation or execution failures from the underlying operation.
pub fn minimum_spanning_forest<R: GraphRead>(graph: &R) -> FerrosGrynnResult<SpanningForest> {
	minimum_spanning_forest_with_check(graph, &mut || Ok(()))
}

/// Returns the minimum spanning forest with check result.
///
/// # Errors
/// Propagates validation, resource-admission or callback errors from the
/// operation.
pub fn minimum_spanning_forest_with_check<R: GraphRead, Check>(
	graph: &R,
	check: &mut Check,
) -> FerrosGrynnResult<SpanningForest>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let nodes = collect_node_ids_with_check(graph, check)?;
	let positions =
		nodes.iter().enumerate().map(|(index, id)| (*id, index)).collect::<RapidHashMap<_, _>>();
	let weights = collect_weights_with_check(graph, true, "minimum spanning forest", check)?;
	let mut candidates = binary_edges_with_check(graph, check)?
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
		left_weight.cmp(right_weight).then_with(|| left.edge.cmp(&right.edge))
	});

	let mut sets = DisjointSets::new(nodes.len());
	let mut selected = Vec::with_capacity(nodes.len().saturating_sub(1));
	let mut total_weight = Number::Int(0);
	let mut components = nodes.len();
	for (candidate, (weight, edge)) in candidates.into_iter().enumerate() {
		check_candidate(check, candidate)?;
		let Some(left) = positions.get(&edge.left).copied() else {
			return Err(atom_does_not_exist(edge.left));
		};
		let Some(right) = positions.get(&edge.right).copied() else {
			return Err(atom_does_not_exist(edge.right));
		};
		if sets.union(left, right) {
			selected.push(edge.edge);
			total_weight = add_numbers(total_weight, weight)?;
			components = components.saturating_sub(1);
		}
	}
	Ok(SpanningForest {
		edges: selected.into_boxed_slice(),
		total_weight,
		components,
	})
}

/// Returns minimum spanning tree for this module.
///
/// # Errors
///
/// Propagates errors returned by the underlying operation.
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
/// Binary edge state.
pub(super) struct BinaryEdge {
	/// Edge associated with this binary edge.
	pub(super) edge: AtomId,
	/// Left associated with this binary edge.
	pub(super) left: AtomId,
	/// Right associated with this binary edge.
	pub(super) right: AtomId,
}

#[derive(Debug, Clone)]
/// Disjoint sets state.
pub(super) struct DisjointSets {
	/// Parents associated with this disjoint sets.
	parents: Vec<usize>,
	/// Ranks associated with this disjoint sets.
	ranks: Vec<u8>,
}

impl DisjointSets {
	/// Creates a new `DisjointSets` value.
	pub(super) fn new(len: usize) -> Self {
		Self {
			parents: (0..len).collect(),
			ranks: vec![0; len],
		}
	}

	/// Finds disjoint sets.
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

	/// Returns whether the union condition holds.
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

/// Returns the binary edges with check result.
pub(super) fn binary_edges_with_check<R: GraphRead, Check>(
	graph: &R,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<BinaryEdge>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let mut edges = Vec::with_capacity(graph.edge_count()?);
	for (candidate, id) in graph.edge_ids()?.enumerate() {
		check_candidate(check, candidate)?;
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

/// Checks candidate.
fn check_candidate<Check>(check: &mut Check, candidate: usize) -> FerrosGrynnResult<()>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	if candidate & 1023 == 0 {
		check()?;
	}
	Ok(())
}
