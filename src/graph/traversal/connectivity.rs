use std::collections::{BTreeSet, VecDeque};

use rapidhash::{RapidHashMap, RapidHashSet};

use super::{
	algorithm_failure,
	index::{TraversalDirection, TraversalIndex, TraversalSemantics, local_steps},
	spanning::{DisjointSets, binary_edges},
};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{
		AtomId,
		AtomKind,
		GraphRead,
		general::{atom_does_not_exist, atom_kind_mismatch},
	},
};

pub fn bfs<R: GraphRead>(
	graph: &R,
	root: AtomId,
	direction: TraversalDirection,
	max_depth: Option<usize>,
) -> FerrosGrynnResult<Vec<AtomId>> {
	ensure_node(graph, root)?;
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let mut visited = RapidHashSet::default();
	let mut queue = VecDeque::from([(root, 0_usize)]);
	let mut order = Vec::new();
	visited.insert(root);

	while let Some((node, depth)) = queue.pop_front() {
		order.push(node);
		if max_depth.is_some_and(|limit| depth >= limit) {
			continue;
		}
		for step in local_steps(&index, node, direction) {
			let target = step.target.local_id().filter(|id| id.is_node());
			if let Some(target) = target.filter(|target| visited.insert(*target)) {
				queue.push_back((target, depth + 1));
			}
		}
	}
	Ok(order)
}

pub fn dfs<R: GraphRead>(
	graph: &R,
	root: AtomId,
	direction: TraversalDirection,
	max_depth: Option<usize>,
) -> FerrosGrynnResult<Vec<AtomId>> {
	ensure_node(graph, root)?;
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let mut visited = RapidHashSet::default();
	let mut stack = vec![(root, 0_usize)];
	let mut order = Vec::new();

	while let Some((node, depth)) = stack.pop() {
		if !visited.insert(node) {
			continue;
		}
		order.push(node);
		if max_depth.is_some_and(|limit| depth >= limit) {
			continue;
		}
		for target in local_steps(&index, node, direction)
			.filter_map(|step| step.target.local_id().filter(|id| id.is_node()))
			.rev()
		{
			if !visited.contains(&target) {
				stack.push((target, depth + 1));
			}
		}
	}
	Ok(order)
}

pub fn topological_sort<R: GraphRead>(graph: &R) -> FerrosGrynnResult<Vec<AtomId>> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	topological_sort_with_index(graph, &index)?.ok_or_else(|| {
		algorithm_failure("topological sort", "the local vertex projection contains a cycle")
	})
}

pub fn strongly_connected_components<R: GraphRead>(
	graph: &R,
) -> FerrosGrynnResult<Vec<Vec<AtomId>>> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let nodes = collect_node_ids(graph)?;
	let mut visited = RapidHashSet::default();
	let mut finish = Vec::with_capacity(nodes.len());

	for start in nodes.iter().copied() {
		if !visited.insert(start) {
			continue;
		}
		let mut stack = vec![(start, false)];
		while let Some((node, returning)) = stack.pop() {
			if returning {
				finish.push(node);
				continue;
			}
			stack.push((node, true));
			for target in local_steps(&index, node, TraversalDirection::Forward)
				.filter_map(|step| step.target.local_id().filter(|id| id.is_node()))
				.rev()
			{
				if visited.insert(target) {
					stack.push((target, false));
				}
			}
		}
	}

	let mut assigned = RapidHashSet::default();
	let mut components = Vec::new();
	for start in finish.into_iter().rev() {
		if !assigned.insert(start) {
			continue;
		}
		let mut component = Vec::new();
		let mut stack = vec![start];
		while let Some(node) = stack.pop() {
			component.push(node);
			for step in local_steps(&index, node, TraversalDirection::Backward) {
				if let Some(target) = step
					.target
					.local_id()
					.filter(|id| id.is_node())
					.filter(|target| assigned.insert(*target))
				{
					stack.push(target);
				}
			}
		}
		components.push(component);
	}
	Ok(components)
}

/// Condensation of the local vertex projection into strongly connected
/// components.
///
/// Component indices address [`Condensation::components`]. Cross-level
/// endpoints are intentionally not collapsed into the current graph because
/// they belong to another graph identity.
#[derive(Debug, Clone)]
pub struct Condensation {
	component_of: RapidHashMap<AtomId, usize>,
	components: Box<[Box<[AtomId]>]>,
	edges: Box<[(usize, usize)]>,
}

impl Condensation {
	pub fn component_of(&self, atom: AtomId) -> Option<usize> {
		self.component_of.get(&atom).copied()
	}

	pub fn components(&self) -> &[Box<[AtomId]>] {
		&self.components
	}

	pub fn edges(&self) -> &[(usize, usize)] {
		&self.edges
	}
}

/// Builds the acyclic component graph used by `tv::condensation()`.
pub fn condensation<R: GraphRead>(graph: &R) -> FerrosGrynnResult<Condensation> {
	let components = strongly_connected_components(graph)?;
	let mut component_of = RapidHashMap::default();
	for (component, atoms) in components.iter().enumerate() {
		for atom in atoms {
			component_of.insert(*atom, component);
		}
	}

	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let mut edges = BTreeSet::new();
	for source in graph.node_ids()? {
		let source = source?;
		let source_component = component_of.get(&source).copied().ok_or_else(|| {
			algorithm_failure("condensation", format!("node {source} has no component"))
		})?;
		for step in local_steps(&index, source, TraversalDirection::Forward) {
			let Some(target) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let target_component = component_of.get(&target).copied().ok_or_else(|| {
				algorithm_failure("condensation", format!("node {target} has no component"))
			})?;
			if source_component != target_component {
				edges.insert((source_component, target_component));
			}
		}
	}

	Ok(Condensation {
		component_of,
		components: components
			.into_iter()
			.map(Vec::into_boxed_slice)
			.collect::<Vec<_>>()
			.into_boxed_slice(),
		edges: edges.into_iter().collect::<Vec<_>>().into_boxed_slice(),
	})
}

pub fn weakly_connected_components<R: GraphRead>(graph: &R) -> FerrosGrynnResult<Vec<Vec<AtomId>>> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let mut visited = RapidHashSet::default();
	let mut components = Vec::new();
	for start in graph.node_ids()? {
		let start = start?;
		if !visited.insert(start) {
			continue;
		}
		let mut component = Vec::new();
		let mut queue = VecDeque::from([start]);
		while let Some(node) = queue.pop_front() {
			component.push(node);
			for step in local_steps(&index, node, TraversalDirection::Both) {
				if let Some(target) = step
					.target
					.local_id()
					.filter(|id| id.is_node())
					.filter(|target| visited.insert(*target))
				{
					queue.push_back(target);
				}
			}
		}
		components.push(component);
	}
	Ok(components)
}

pub fn is_connected<R: GraphRead>(graph: &R) -> FerrosGrynnResult<bool> {
	Ok(weakly_connected_components(graph)?.len() <= 1)
}

pub fn has_cycle_directed<R: GraphRead>(graph: &R) -> FerrosGrynnResult<bool> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	Ok(topological_sort_with_index(graph, &index)?.is_none())
}

pub fn has_cycle_undirected<R: GraphRead>(graph: &R) -> FerrosGrynnResult<bool> {
	let nodes = collect_node_ids(graph)?;
	let positions =
		nodes.iter().enumerate().map(|(index, id)| (*id, index)).collect::<RapidHashMap<_, _>>();
	let mut sets = DisjointSets::new(nodes.len());
	for edge in binary_edges(graph)? {
		if edge.left == edge.right {
			return Ok(true);
		}
		let Some(left) = positions.get(&edge.left).copied() else {
			return Err(atom_does_not_exist(edge.left));
		};
		let Some(right) = positions.get(&edge.right).copied() else {
			return Err(atom_does_not_exist(edge.right));
		};
		if !sets.union(left, right) {
			return Ok(true);
		}
	}
	Ok(false)
}

pub(super) fn ensure_node<R: GraphRead>(graph: &R, id: AtomId) -> FerrosGrynnResult<()> {
	if id.kind() != Some(AtomKind::Node) {
		return Err(atom_kind_mismatch(id, AtomKind::Node));
	}
	graph.node(id)?.map(|_| ()).ok_or_else(|| atom_does_not_exist(id))
}

pub(super) fn collect_node_ids<R: GraphRead>(graph: &R) -> FerrosGrynnResult<Vec<AtomId>> {
	graph.node_ids()?.collect()
}

pub(super) fn topological_sort_with_index<R: GraphRead>(
	graph: &R,
	index: &TraversalIndex,
) -> FerrosGrynnResult<Option<Vec<AtomId>>> {
	let nodes = collect_node_ids(graph)?;
	let mut indegree = nodes.iter().map(|node| (*node, 0_usize)).collect::<RapidHashMap<_, _>>();
	for source in &nodes {
		for step in local_steps(index, *source, TraversalDirection::Forward) {
			if let Some(target) = step.target.local_id().filter(|id| id.is_node()) {
				*indegree.entry(target).or_insert(0) += 1;
			}
		}
	}
	let mut queue = nodes
		.iter()
		.copied()
		.filter(|node| indegree.get(node).copied() == Some(0))
		.collect::<VecDeque<_>>();
	let mut order = Vec::with_capacity(nodes.len());
	while let Some(node) = queue.pop_front() {
		order.push(node);
		for step in local_steps(index, node, TraversalDirection::Forward) {
			let Some(target) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let Some(value) = indegree.get_mut(&target) else {
				return Ok(None);
			};
			let Some(next) = value.checked_sub(1) else {
				return Ok(None);
			};
			*value = next;
			if *value == 0 {
				queue.push_back(target);
			}
		}
	}
	Ok((order.len() == nodes.len()).then_some(order))
}
