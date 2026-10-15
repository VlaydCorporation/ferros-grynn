//! Shared validation for orthogonal durable graph contracts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{
	AtomId,
	AtomKind,
	EdgeRead,
	GraphRead,
	GraphTopologyConstraint,
	GraphTopologyConstraints,
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Validates every orthogonal topology constraint against one immutable graph.
pub fn validate_topology_constraints<Reader: GraphRead>(
	graph: &Reader,
	constraints: GraphTopologyConstraints,
) -> FerrosGrynnResult<()> {
	if constraints.iter().next().is_none() {
		return Ok(());
	}
	let nodes = graph.node_ids()?.collect::<FerrosGrynnResult<BTreeSet<_>>>()?;
	let mut pairs = BTreeSet::new();
	let mut adjacency = BTreeMap::<AtomId, Vec<AtomId>>::new();
	for node in &nodes {
		adjacency.insert(*node, Vec::new());
	}
	for edge in graph.edge_ids()? {
		let edge = edge?;
		let edge = graph
			.edge(edge)?
			.ok_or_else(|| invalid("a graph constraint observed a missing edge"))?;
		let topology = edge.topology()?;
		let Some((left, right, _direction)) = topology.local_binary() else {
			return Err(invalid(
				"SIMPLE, BIPARTITE, and COMPLETE require binary local node-to-node edges",
			));
		};
		if left.kind() != Some(AtomKind::Node)
			|| right.kind() != Some(AtomKind::Node)
			|| !nodes.contains(&left)
			|| !nodes.contains(&right)
		{
			return Err(invalid("SIMPLE, BIPARTITE, and COMPLETE require existing node endpoints"));
		}
		let pair = ordered_pair(left, right);
		if constraints.contains(GraphTopologyConstraint::Simple)
			&& (left == right || !pairs.insert(pair))
		{
			return Err(invalid("SIMPLE rejects self-loops and repeated endpoint pairs"));
		}
		pairs.insert(pair);
		adjacency.get_mut(&left).ok_or_else(|| invalid("left endpoint is absent"))?.push(right);
		adjacency.get_mut(&right).ok_or_else(|| invalid("right endpoint is absent"))?.push(left);
	}
	if constraints.contains(GraphTopologyConstraint::Bipartite) {
		validate_bipartite(&adjacency)?;
	}
	if constraints.contains(GraphTopologyConstraint::Complete) {
		let node_count = u128::try_from(nodes.len())
			.map_err(|_error| invalid("node count cannot be represented"))?;
		let expected = node_count.saturating_mul(node_count.saturating_sub(1)) / 2;
		if u128::try_from(pairs.len()).unwrap_or(u128::MAX) != expected {
			return Err(invalid("COMPLETE requires every unordered pair of distinct nodes"));
		}
	}
	Ok(())
}

/// Validates bipartite.
fn validate_bipartite(adjacency: &BTreeMap<AtomId, Vec<AtomId>>) -> FerrosGrynnResult<()> {
	let mut colors = BTreeMap::<AtomId, bool>::new();
	for start in adjacency.keys().copied() {
		if colors.contains_key(&start) {
			continue;
		}
		colors.insert(start, false);
		let mut queue = VecDeque::from([start]);
		while let Some(node) = queue.pop_front() {
			let color = colors[&node];
			for neighbor in &adjacency[&node] {
				match colors.get(neighbor) {
					Some(existing) if *existing == color => {
						return Err(invalid("BIPARTITE rejects an odd cycle"));
					}
					Some(_) => {}
					None => {
						colors.insert(*neighbor, !color);
						queue.push_back(*neighbor);
					}
				}
			}
		}
	}
	Ok(())
}

/// Returns the ordered pair result.
fn ordered_pair(left: AtomId, right: AtomId) -> (AtomId, AtomId) {
	if left <= right {
		(left, right)
	} else {
		(right, left)
	}
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("graph topology", reason.into())
}

#[cfg(test)]
/// Regression tests for validate behavior.
mod tests {
	use super::validate_topology_constraints;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{
		graph::{EdgeTopology, Graph, GraphTopologyConstraint, GraphTopologyConstraints, Mutable},
		values::PropertyMap,
	};

	/// Verifies rejects an odd cycle for bipartite contract.
	#[test]
	fn rejects_an_odd_cycle_for_bipartite_contract() {
		let mut graph = Graph::<Mutable>::new();
		let a = graph.add_node(None, PropertyMap::new()).assert_ok();
		let b = graph.add_node(None, PropertyMap::new()).assert_ok();
		let c = graph.add_node(None, PropertyMap::new()).assert_ok();
		graph.add_edge(EdgeTopology::undirected([a, b]), None, PropertyMap::new()).assert_ok();
		graph.add_edge(EdgeTopology::undirected([b, c]), None, PropertyMap::new()).assert_ok();
		graph.add_edge(EdgeTopology::undirected([c, a]), None, PropertyMap::new()).assert_ok();
		let mut constraints = GraphTopologyConstraints::empty();
		constraints.insert(GraphTopologyConstraint::Bipartite);
		assert!(validate_topology_constraints(&graph, constraints).is_err());
	}
}
