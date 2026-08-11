use std::{
	cmp::Ordering,
	collections::{BTreeSet, BinaryHeap, VecDeque, hash_map},
};

use num_traits::ToPrimitive;
use rapidhash::{RapidHashMap, RapidHashSet};

use super::{
	AtomId,
	AtomKind,
	EdgeEndpoint,
	EdgeTopology,
	EndpointRole,
	Forest,
	GraphRead,
	Path,
	PathDirection,
	PathStep,
	RootedTree,
	SnapshotId,
	Tree,
	general::{atom_does_not_exist, atom_kind_mismatch},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, Uuid},
};

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum TraversalDirection {
	Forward,
	Backward,
	Both,
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum TraversalSemantics {
	/// Only vertex endpoints participate; edge atoms remain valid connectors.
	Node,
	/// Vertices and edge atoms may both be endpoints of a traversal step.
	Atom,
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct TraversalStep {
	target: EdgeEndpoint,
	via_edge: AtomId,
	direction: PathDirection,
	role: EndpointRole,
}

impl TraversalStep {
	pub const fn target(&self) -> &EdgeEndpoint {
		&self.target
	}

	pub const fn via_edge(&self) -> AtomId {
		self.via_edge
	}

	pub const fn direction(&self) -> PathDirection {
		self.direction
	}

	pub const fn role(&self) -> EndpointRole {
		self.role
	}
}

/// Reusable endpoint adjacency for query traversal.
///
/// `Node` semantics keeps meta-vertices opaque, but it can still contain a
/// qualified nested vertex reached by an explicit cross-level edge. `Atom`
/// additionally permits V-E, E-V and E-E endpoint transitions.
#[derive(Debug, Clone)]
pub struct TraversalIndex {
	identity: Uuid,
	revision: u64,
	semantics: TraversalSemantics,
	forward: RapidHashMap<EdgeEndpoint, Vec<TraversalStep>>,
	backward: RapidHashMap<EdgeEndpoint, Vec<TraversalStep>>,
}

impl TraversalIndex {
	pub fn new<R: GraphRead>(graph: &R, semantics: TraversalSemantics) -> FerrosGrynnResult<Self> {
		let mut index = Self {
			identity: graph.identity(),
			revision: graph.revision(),
			semantics,
			forward: RapidHashMap::default(),
			backward: RapidHashMap::default(),
		};

		for id in graph.node_ids() {
			index.register_endpoint(EdgeEndpoint::Local(id));
		}
		if semantics == TraversalSemantics::Atom {
			for id in graph.edge_ids() {
				index.register_endpoint(EdgeEndpoint::Local(id));
			}
		}

		for id in graph.edge_ids() {
			let edge = graph.edge(id).ok_or_else(|| {
				algorithm_failure(
					"traversal index",
					format!("edge iterator yielded missing edge {id}"),
				)
			})?;
			index.register_topology(id, edge.topology());
		}
		Ok(index)
	}

	pub const fn identity(&self) -> Uuid {
		self.identity
	}

	pub const fn revision(&self) -> u64 {
		self.revision
	}

	pub const fn semantics(&self) -> TraversalSemantics {
		self.semantics
	}

	pub fn contains(&self, endpoint: &EdgeEndpoint) -> bool {
		self.forward.contains_key(endpoint)
	}

	pub fn steps<'index>(
		&'index self,
		endpoint: &EdgeEndpoint,
		direction: TraversalDirection,
	) -> impl DoubleEndedIterator<Item = &'index TraversalStep> + 'index {
		let empty: &[TraversalStep] = &[];
		let forward = self.forward.get(endpoint).map_or(empty, Vec::as_slice);
		let backward = self.backward.get(endpoint).map_or(empty, Vec::as_slice);
		let (first, second) = match direction {
			TraversalDirection::Forward => (forward, empty),
			TraversalDirection::Backward => (backward, empty),
			TraversalDirection::Both => (forward, backward),
		};
		deduplicated_steps(first, second)
	}

	pub fn breadth_first(
		&self,
		root: EdgeEndpoint,
		direction: TraversalDirection,
		max_depth: Option<usize>,
	) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
		self.ensure_endpoint(&root)?;
		let mut visited = RapidHashSet::default();
		let mut queue = VecDeque::from([(root.clone(), 0_usize)]);
		let mut order = Vec::new();
		visited.insert(root);

		while let Some((endpoint, depth)) = queue.pop_front() {
			order.push(endpoint.clone());
			if max_depth.is_some_and(|limit| depth >= limit) {
				continue;
			}
			for step in self.steps(&endpoint, direction) {
				if visited.insert(step.target.clone()) {
					queue.push_back((step.target.clone(), depth + 1));
				}
			}
		}
		Ok(order)
	}

	pub fn depth_first(
		&self,
		root: EdgeEndpoint,
		direction: TraversalDirection,
		max_depth: Option<usize>,
	) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
		self.ensure_endpoint(&root)?;
		let mut visited = RapidHashSet::default();
		let mut stack = vec![(root, 0_usize)];
		let mut order = Vec::new();

		while let Some((endpoint, depth)) = stack.pop() {
			if !visited.insert(endpoint.clone()) {
				continue;
			}
			order.push(endpoint.clone());
			if max_depth.is_some_and(|limit| depth >= limit) {
				continue;
			}
			for step in self.steps(&endpoint, direction).rev() {
				if !visited.contains(&step.target) {
					stack.push((step.target.clone(), depth + 1));
				}
			}
		}
		Ok(order)
	}

	fn register_topology(&mut self, edge: AtomId, topology: &EdgeTopology) {
		match topology {
			EdgeTopology::Directed {
				source,
				target,
			} => self.connect_sets(edge, source, target, false),
			EdgeTopology::Bidirectional {
				source,
				target,
			} => self.connect_sets(edge, source, target, true),
			EdgeTopology::Undirected {
				endpoints,
			} => {
				for (index, left) in endpoints.iter().enumerate() {
					for right in &endpoints[index + 1..] {
						self.connect(
							edge,
							left,
							right,
							PathDirection::Undirected,
							EndpointRole::Undirected,
							PathDirection::Undirected,
							EndpointRole::Undirected,
						);
						self.connect(
							edge,
							right,
							left,
							PathDirection::Undirected,
							EndpointRole::Undirected,
							PathDirection::Undirected,
							EndpointRole::Undirected,
						);
					}
				}
			}
		}
	}

	fn connect_sets(
		&mut self,
		edge: AtomId,
		source: &[EdgeEndpoint],
		target: &[EdgeEndpoint],
		bidirectional: bool,
	) {
		for left in source {
			for right in target {
				self.connect(
					edge,
					left,
					right,
					PathDirection::Forward,
					EndpointRole::Target,
					PathDirection::Reverse,
					EndpointRole::Source,
				);
				if bidirectional {
					self.connect(
						edge,
						right,
						left,
						PathDirection::Reverse,
						EndpointRole::Source,
						PathDirection::Forward,
						EndpointRole::Target,
					);
				}
			}
		}
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "forward and reverse path metadata form one incidence pair"
	)]
	fn connect(
		&mut self,
		edge: AtomId,
		from: &EdgeEndpoint,
		to: &EdgeEndpoint,
		forward_direction: PathDirection,
		forward_role: EndpointRole,
		backward_direction: PathDirection,
		backward_role: EndpointRole,
	) {
		if !self.accepts(from) || !self.accepts(to) {
			return;
		}
		self.register_endpoint(from.clone());
		self.register_endpoint(to.clone());
		self.forward.entry(from.clone()).or_default().push(TraversalStep {
			target: to.clone(),
			via_edge: edge,
			direction: forward_direction,
			role: forward_role,
		});
		self.backward.entry(to.clone()).or_default().push(TraversalStep {
			target: from.clone(),
			via_edge: edge,
			direction: backward_direction,
			role: backward_role,
		});
	}

	fn accepts(&self, endpoint: &EdgeEndpoint) -> bool {
		self.semantics == TraversalSemantics::Atom
			|| endpoint_kind(endpoint) == Some(AtomKind::Node)
	}

	fn register_endpoint(&mut self, endpoint: EdgeEndpoint) {
		self.forward.entry(endpoint.clone()).or_default();
		self.backward.entry(endpoint).or_default();
	}

	fn ensure_endpoint(&self, endpoint: &EdgeEndpoint) -> FerrosGrynnResult<()> {
		if self.contains(endpoint) {
			return Ok(());
		}
		Err(FerrosGrynnError::atom_does_not_exist(
			endpoint_kind(endpoint).map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
			format!("{endpoint:?}"),
		))
	}
}

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
	topological_sort_with_index(graph, &index).ok_or_else(|| {
		algorithm_failure("topological sort", "the local vertex projection contains a cycle")
	})
}

pub fn strongly_connected_components<R: GraphRead>(
	graph: &R,
) -> FerrosGrynnResult<Vec<Vec<AtomId>>> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let nodes = graph.node_ids().collect::<Vec<_>>();
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
	for source in graph.node_ids() {
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
	for start in graph.node_ids() {
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

#[derive(Debug, Clone)]
struct Predecessor {
	from: AtomId,
	step: TraversalStep,
}

#[derive(Debug, Clone)]
pub struct ShortestPaths {
	origin: Uuid,
	snapshot: SnapshotId,
	source: AtomId,
	distances: RapidHashMap<AtomId, f64>,
	predecessors: RapidHashMap<AtomId, Predecessor>,
}

impl ShortestPaths {
	pub const fn source(&self) -> AtomId {
		self.source
	}

	pub fn distance_to(&self, target: AtomId) -> Option<f64> {
		self.distances.get(&target).copied()
	}

	pub fn path_to(&self, target: AtomId) -> Option<Vec<AtomId>> {
		self.reconstruct(target)
			.map(|path| path.into_iter().map(|(atom, _)| atom).collect::<Vec<_>>())
	}

	pub fn edge_path_to(&self, target: AtomId) -> Option<Vec<AtomId>> {
		self.reconstruct(target).map(|path| {
			path.into_iter()
				.filter_map(|(_, predecessor)| predecessor.map(|value| value.step.via_edge))
				.collect::<Vec<_>>()
		})
	}

	pub fn materialize_path<R: GraphRead>(
		&self,
		graph: &R,
		target: AtomId,
	) -> FerrosGrynnResult<Option<Path>> {
		if graph.identity() != self.origin {
			return Err(FerrosGrynnError::graph_identity_mismatch(
				self.origin.to_string(),
				graph.identity().to_string(),
			));
		}
		if graph.snapshot_id() != self.snapshot {
			return Err(FerrosGrynnError::snapshot_unavailable(
				self.origin.to_string(),
				self.snapshot.to_string(),
			));
		}
		let Some(path) = self.reconstruct(target) else {
			return Ok(None);
		};
		let mut steps = Vec::with_capacity(path.len());
		for (index, (atom, predecessor)) in path.into_iter().enumerate() {
			let atom_ref = graph.atom_ref(atom).ok_or_else(|| atom_does_not_exist(atom))?;
			if index == 0 {
				steps.push(PathStep::new(
					atom_ref,
					None,
					PathDirection::Forward,
					EndpointRole::Source,
				));
				continue;
			}
			let predecessor = predecessor.ok_or_else(|| {
				algorithm_failure("shortest path", "predecessor chain is incomplete")
			})?;
			let edge_ref = graph
				.atom_ref(predecessor.step.via_edge)
				.ok_or_else(|| atom_does_not_exist(predecessor.step.via_edge))?;
			steps.push(PathStep::new(
				atom_ref,
				Some(edge_ref),
				predecessor.step.direction,
				predecessor.step.role,
			));
		}
		Ok(Some(Path::new(
			self.origin,
			self.snapshot,
			steps,
			self.distance_to(target).map(Number::Float),
		)))
	}

	fn reconstruct(&self, target: AtomId) -> Option<Vec<(AtomId, Option<Predecessor>)>> {
		self.distances.contains_key(&target).then_some(())?;
		let mut path = Vec::new();
		let mut current = target;
		for _ in 0..=self.predecessors.len() {
			if current == self.source {
				path.push((current, None));
				path.reverse();
				return Some(path);
			}
			let predecessor = self.predecessors.get(&current)?.clone();
			path.push((current, Some(predecessor.clone())));
			current = predecessor.from;
		}
		None
	}
}

#[derive(Debug, Clone, Copy)]
struct QueueEntry {
	distance: f64,
	node: AtomId,
}

impl PartialEq for QueueEntry {
	fn eq(&self, other: &Self) -> bool {
		self.distance.total_cmp(&other.distance) == Ordering::Equal && self.node == other.node
	}
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for QueueEntry {
	fn cmp(&self, other: &Self) -> Ordering {
		other.distance.total_cmp(&self.distance).then_with(|| other.node.cmp(&self.node))
	}
}

pub fn dijkstra<R: GraphRead>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
) -> FerrosGrynnResult<ShortestPaths> {
	ensure_node(graph, source)?;
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let weights = collect_weights(graph, false, "Dijkstra")?;
	let mut distances = RapidHashMap::default();
	let mut predecessors = RapidHashMap::default();
	let mut queue = BinaryHeap::from([QueueEntry {
		distance: 0.0,
		node: source,
	}]);
	distances.insert(source, 0.0);

	while let Some(QueueEntry {
		distance,
		node,
	}) = queue.pop()
	{
		if distances.get(&node).is_some_and(|known| distance > *known) {
			continue;
		}
		for step in local_steps(&index, node, direction) {
			let Some(target) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let Some(weight) = weights.get(&step.via_edge).copied() else {
				return Err(algorithm_failure(
					"Dijkstra",
					format!("missing weight for edge {}", step.via_edge),
				));
			};
			let candidate = distance + weight;
			if !candidate.is_finite() {
				return Err(algorithm_failure("Dijkstra", "distance overflowed f64"));
			}
			if distances.get(&target).is_none_or(|known| candidate < *known) {
				distances.insert(target, candidate);
				predecessors.insert(target, Predecessor {
					from: node,
					step: step.clone(),
				});
				queue.push(QueueEntry {
					distance: candidate,
					node: target,
				});
			}
		}
	}
	Ok(shortest_paths(graph, source, distances, predecessors))
}

pub fn bellman_ford<R: GraphRead>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
) -> FerrosGrynnResult<ShortestPaths> {
	ensure_node(graph, source)?;
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let weights = collect_weights(graph, true, "Bellman-Ford")?;
	let arcs = local_arcs(graph, &index, &weights, direction)?;
	let mut distances = RapidHashMap::default();
	let mut predecessors = RapidHashMap::default();
	distances.insert(source, 0.0);

	for _ in 0..graph.node_count().saturating_sub(1) {
		let mut changed = false;
		for arc in &arcs {
			let Some(distance) = distances.get(&arc.from).copied() else {
				continue;
			};
			let candidate = distance + arc.weight;
			if !candidate.is_finite() {
				return Err(algorithm_failure("Bellman-Ford", "distance overflowed f64"));
			}
			if distances.get(&arc.to).is_none_or(|known| candidate < *known) {
				distances.insert(arc.to, candidate);
				predecessors.insert(arc.to, Predecessor {
					from: arc.from,
					step: arc.step.clone(),
				});
				changed = true;
			}
		}
		if !changed {
			break;
		}
	}

	for arc in &arcs {
		let Some(distance) = distances.get(&arc.from).copied() else {
			continue;
		};
		if distances.get(&arc.to).is_none_or(|known| distance + arc.weight < *known) {
			return Err(algorithm_failure(
				"Bellman-Ford",
				"a negative-weight cycle is reachable from the source",
			));
		}
	}
	Ok(shortest_paths(graph, source, distances, predecessors))
}

pub fn has_cycle_directed<R: GraphRead>(graph: &R) -> FerrosGrynnResult<bool> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	Ok(topological_sort_with_index(graph, &index).is_none())
}

pub fn has_cycle_undirected<R: GraphRead>(graph: &R) -> FerrosGrynnResult<bool> {
	let nodes = graph.node_ids().collect::<Vec<_>>();
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
	let nodes = graph.node_ids().collect::<Vec<_>>();
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
	let mut adjacency = graph
		.node_ids()
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

	for root in graph.node_ids() {
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

#[derive(Debug, Clone)]
pub struct BinaryLiftingIndex {
	identity: Uuid,
	revision: u64,
	nodes: Box<[AtomId]>,
	positions: RapidHashMap<AtomId, usize>,
	depths: Box<[usize]>,
	roots: Box<[usize]>,
	ancestors: Box<[Box<[Option<usize>]>]>,
}

impl BinaryLiftingIndex {
	pub fn from_forest<State>(forest: &Forest<State>) -> FerrosGrynnResult<Self> {
		Self::build(forest, |id| forest.parent(id))
	}

	pub fn from_tree<State>(tree: &Tree<State>) -> FerrosGrynnResult<Self> {
		Self::build(tree, |id| tree.parent(id))
	}

	pub fn from_rooted_tree<State>(tree: &RootedTree<State>) -> FerrosGrynnResult<Self> {
		Self::build(tree, |id| tree.parent(id))
	}

	pub const fn identity(&self) -> Uuid {
		self.identity
	}

	pub const fn revision(&self) -> u64 {
		self.revision
	}

	pub fn validate<R: GraphRead>(&self, graph: &R) -> FerrosGrynnResult<()> {
		if graph.identity() != self.identity {
			return Err(FerrosGrynnError::graph_identity_mismatch(
				self.identity.to_string(),
				graph.identity().to_string(),
			));
		}
		if graph.revision() != self.revision {
			return Err(FerrosGrynnError::graph_workspace_stale(
				self.identity.to_string(),
				self.revision,
				graph.identity().to_string(),
				graph.revision(),
			));
		}
		Ok(())
	}

	pub fn depth(&self, atom: AtomId) -> FerrosGrynnResult<usize> {
		let index = self.position(atom)?;
		Ok(self.depths[index])
	}

	pub fn lowest_common_ancestor(
		&self,
		left: AtomId,
		right: AtomId,
	) -> FerrosGrynnResult<Option<AtomId>> {
		let mut left = self.position(left)?;
		let mut right = self.position(right)?;
		if self.roots[left] != self.roots[right] {
			return Ok(None);
		}
		if self.depths[left] < self.depths[right] {
			std::mem::swap(&mut left, &mut right);
		}
		let difference = self.depths[left] - self.depths[right];
		for level in 0..self.ancestors.len() {
			if difference & (1_usize << level) != 0 {
				let Some(ancestor) = self.ancestors[level][left] else {
					return Ok(None);
				};
				left = ancestor;
			}
		}
		if left == right {
			return Ok(Some(self.nodes[left]));
		}
		for level in (0..self.ancestors.len()).rev() {
			let left_ancestor = self.ancestors[level][left];
			let right_ancestor = self.ancestors[level][right];
			if left_ancestor != right_ancestor {
				let (Some(next_left), Some(next_right)) = (left_ancestor, right_ancestor) else {
					continue;
				};
				left = next_left;
				right = next_right;
			}
		}
		Ok(self.ancestors[0][left].map(|index| self.nodes[index]))
	}

	fn build<R: GraphRead>(
		graph: &R,
		mut parent_of: impl FnMut(AtomId) -> Option<AtomId>,
	) -> FerrosGrynnResult<Self> {
		let nodes = graph.node_ids().collect::<Vec<_>>();
		let positions = nodes
			.iter()
			.enumerate()
			.map(|(index, id)| (*id, index))
			.collect::<RapidHashMap<_, _>>();
		let mut parents = Vec::with_capacity(nodes.len());
		for node in &nodes {
			let parent = parent_of(*node)
				.map(|parent| {
					positions.get(&parent).copied().ok_or_else(|| {
						algorithm_failure(
							"binary lifting",
							format!("parent {parent} is absent from the graph"),
						)
					})
				})
				.transpose()?;
			parents.push(parent);
		}

		let mut depths = vec![None; nodes.len()];
		let mut roots = vec![None; nodes.len()];
		for start in 0..nodes.len() {
			if depths[start].is_some() {
				continue;
			}
			let mut chain = Vec::new();
			let mut seen = RapidHashSet::default();
			let mut current = start;
			while depths[current].is_none() {
				if !seen.insert(current) {
					return Err(algorithm_failure(
						"binary lifting",
						"the parent relation contains a cycle",
					));
				}
				chain.push(current);
				let Some(parent) = parents[current] else {
					depths[current] = Some(0);
					roots[current] = Some(current);
					break;
				};
				current = parent;
			}
			while let Some(child) = chain.pop() {
				if depths[child].is_some() {
					continue;
				}
				let Some(parent) = parents[child] else {
					depths[child] = Some(0);
					roots[child] = Some(child);
					continue;
				};
				let Some(parent_depth) = depths[parent] else {
					return Err(algorithm_failure("binary lifting", "parent depth is unavailable"));
				};
				let Some(root) = roots[parent] else {
					return Err(algorithm_failure(
						"binary lifting",
						"component root is unavailable",
					));
				};
				depths[child] = Some(parent_depth + 1);
				roots[child] = Some(root);
			}
		}

		let mut levels = 1_usize;
		let mut width = 1_usize;
		while width < nodes.len() {
			levels += 1;
			width = width.saturating_mul(2);
		}
		let mut ancestors = Vec::with_capacity(levels);
		ancestors.push(parents.into_boxed_slice());
		for level in 1..levels {
			let previous = &ancestors[level - 1];
			let next = previous
				.iter()
				.map(|ancestor| ancestor.and_then(|index| previous[index]))
				.collect::<Vec<_>>()
				.into_boxed_slice();
			ancestors.push(next);
		}
		Ok(Self {
			identity: graph.identity(),
			revision: graph.revision(),
			nodes: nodes.into_boxed_slice(),
			positions,
			depths: depths
				.into_iter()
				.collect::<Option<Vec<_>>>()
				.ok_or_else(|| algorithm_failure("binary lifting", "node depth is unavailable"))?
				.into_boxed_slice(),
			roots: roots
				.into_iter()
				.collect::<Option<Vec<_>>>()
				.ok_or_else(|| {
					algorithm_failure("binary lifting", "component root is unavailable")
				})?
				.into_boxed_slice(),
			ancestors: ancestors.into_boxed_slice(),
		})
	}

	fn position(&self, atom: AtomId) -> FerrosGrynnResult<usize> {
		if atom.kind() != Some(AtomKind::Node) {
			return Err(atom_kind_mismatch(atom, AtomKind::Node));
		}
		self.positions.get(&atom).copied().ok_or_else(|| atom_does_not_exist(atom))
	}
}

#[derive(Debug, Clone)]
struct WeightedArc {
	from: AtomId,
	to: AtomId,
	weight: f64,
	step: TraversalStep,
}

#[derive(Debug, Clone, Copy)]
struct BinaryEdge {
	edge: AtomId,
	left: AtomId,
	right: AtomId,
}

#[derive(Debug, Clone, Copy)]
struct CutFrame {
	node: AtomId,
	next: usize,
}

#[derive(Debug, Clone)]
struct DisjointSets {
	parents: Vec<usize>,
	ranks: Vec<u8>,
}

impl DisjointSets {
	fn new(len: usize) -> Self {
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

	fn union(&mut self, left: usize, right: usize) -> bool {
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

const fn endpoint_kind(endpoint: &EdgeEndpoint) -> Option<AtomKind> {
	match endpoint {
		EdgeEndpoint::Local(id) => id.kind(),
		EdgeEndpoint::Nested(qualified) => qualified.atom().kind(),
	}
}

fn ensure_node<R: GraphRead>(graph: &R, id: AtomId) -> FerrosGrynnResult<()> {
	if id.kind() != Some(AtomKind::Node) {
		return Err(atom_kind_mismatch(id, AtomKind::Node));
	}
	graph.node(id).map(|_| ()).ok_or_else(|| atom_does_not_exist(id))
}

fn local_steps(
	index: &TraversalIndex,
	node: AtomId,
	direction: TraversalDirection,
) -> impl DoubleEndedIterator<Item = &TraversalStep> + '_ {
	let empty: &[TraversalStep] = &[];
	let endpoint = EdgeEndpoint::Local(node);
	let forward = index.forward.get(&endpoint).map_or(empty, Vec::as_slice);
	let backward = index.backward.get(&endpoint).map_or(empty, Vec::as_slice);
	let (first, second) = match direction {
		TraversalDirection::Forward => (forward, empty),
		TraversalDirection::Backward => (backward, empty),
		TraversalDirection::Both => (forward, backward),
	};
	deduplicated_steps(first, second)
}

fn deduplicated_steps<'index>(
	first: &'index [TraversalStep],
	second: &'index [TraversalStep],
) -> impl DoubleEndedIterator<Item = &'index TraversalStep> + 'index {
	first.iter().chain(second.iter().filter(move |step| !first.contains(step)))
}

fn topological_sort_with_index<R: GraphRead>(
	graph: &R,
	index: &TraversalIndex,
) -> Option<Vec<AtomId>> {
	let nodes = graph.node_ids().collect::<Vec<_>>();
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
			let value = indegree.get_mut(&target)?;
			*value = value.checked_sub(1)?;
			if *value == 0 {
				queue.push_back(target);
			}
		}
	}
	(order.len() == nodes.len()).then_some(order)
}

fn collect_weights<R: GraphRead>(
	graph: &R,
	allow_negative: bool,
	algorithm: &'static str,
) -> FerrosGrynnResult<RapidHashMap<AtomId, f64>> {
	let mut weights = RapidHashMap::default();
	for id in graph.edge_ids() {
		let edge = graph.edge(id).ok_or_else(|| {
			algorithm_failure(algorithm, format!("edge iterator yielded missing edge {id}"))
		})?;
		let weight = match edge.weight() {
			None => 1.0,
			Some(Number::Int(value)) => value as f64,
			Some(Number::Float(value)) => value,
			Some(Number::Decimal(value)) => value.to_f64().ok_or_else(|| {
				algorithm_failure(algorithm, format!("edge {id} weight is outside f64 range"))
			})?,
		};
		if !weight.is_finite() {
			return Err(algorithm_failure(algorithm, format!("edge {id} has a non-finite weight")));
		}
		if !allow_negative && weight < 0.0 {
			return Err(algorithm_failure(
				algorithm,
				format!("edge {id} has negative weight {weight}"),
			));
		}
		weights.insert(id, weight);
	}
	Ok(weights)
}

fn local_arcs<R: GraphRead>(
	graph: &R,
	index: &TraversalIndex,
	weights: &RapidHashMap<AtomId, f64>,
	direction: TraversalDirection,
) -> FerrosGrynnResult<Vec<WeightedArc>> {
	let mut arcs = Vec::new();
	for from in graph.node_ids() {
		for step in local_steps(index, from, direction) {
			let Some(to) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let Some(weight) = weights.get(&step.via_edge).copied() else {
				return Err(algorithm_failure(
					"shortest path",
					format!("missing weight for edge {}", step.via_edge),
				));
			};
			arcs.push(WeightedArc {
				from,
				to,
				weight,
				step: step.clone(),
			});
		}
	}
	Ok(arcs)
}

fn shortest_paths<R: GraphRead>(
	graph: &R,
	source: AtomId,
	distances: RapidHashMap<AtomId, f64>,
	predecessors: RapidHashMap<AtomId, Predecessor>,
) -> ShortestPaths {
	ShortestPaths {
		origin: graph.identity(),
		snapshot: graph.snapshot_id(),
		source,
		distances,
		predecessors,
	}
}

fn binary_edges<R: GraphRead>(graph: &R) -> FerrosGrynnResult<Vec<BinaryEdge>> {
	let mut edges = Vec::with_capacity(graph.edge_count());
	for id in graph.edge_ids() {
		let edge = graph.edge(id).ok_or_else(|| {
			algorithm_failure("binary graph algorithm", format!("missing edge {id}"))
		})?;
		let Some((left, right, direction)) = edge.topology().local_binary() else {
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
