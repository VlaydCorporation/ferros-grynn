//! Adapters from traversal barriers to the shared graph algorithms.

use std::{
	borrow::Cow,
	collections::{BTreeMap, BTreeSet, VecDeque},
	sync::Arc,
};

use super::*;
use crate::{
	graph::{
		AtomRef,
		Condensation,
		CutSet,
		EdgeTopology,
		GraphKind,
		GraphStorageMode,
		SnapshotId,
		SpanningForest,
		bellman_ford_with_check,
		bfs_with_check,
		condensation_with_check,
		dijkstra_with_check,
		find_cuts_with_check,
		has_cycle_directed_with_check,
		has_cycle_undirected_with_check,
		minimum_spanning_forest_with_check,
		strongly_connected_components_with_check,
		topological_sort_with_check,
		weakly_connected_components_with_check,
	},
	values::{PropertyMap, Uuid},
};

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	pub(super) fn execute_algorithm(
		&self,
		kind: TraverseStepKind,
		step: &PreparedStep,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		match kind {
			TraverseStepKind::KHop
			| TraverseStepKind::Reachable
			| TraverseStepKind::ShortestPath
			| TraverseStepKind::WeightedShortestPath
			| TraverseStepKind::DistanceTo
			| TraverseStepKind::BellmanFord
			| TraverseStepKind::LowestCommonAncestor => self.execute_seed_algorithm(kind, step, input),
			TraverseStepKind::TopologicalSort
			| TraverseStepKind::StronglyConnectedComponents
			| TraverseStepKind::WeaklyConnectedComponents
			| TraverseStepKind::Condensation
			| TraverseStepKind::MinimumSpanningForest
			| TraverseStepKind::Cuts
			| TraverseStepKind::HasCycle => self.execute_induced_algorithm(kind, input),
			_ => Err(FerrosGrynnError::invalid_argument(
				"A non-algorithm step reached the traversal algorithm adapter.",
			)),
		}
	}

	fn execute_seed_algorithm(
		&self,
		kind: TraverseStepKind,
		step: &PreparedStep,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let mut output = Vec::with_capacity(input.len());
		for mut traverser in input {
			self.control.check()?;
			let occurrence = occurrence(&traverser.current)?.clone();
			if occurrence.atom().kind() != AtomKind::Node {
				return Err(domain_error("seed-local graph algorithms require node occurrences"));
			}
			let graph_ref = Arc::new(occurrence.graph().clone());
			let graph = self.read.graph(self.snapshot, &graph_ref)?;
			let control = self.control;
			let mut check = || control.check();
			let source = graph.atom_id(occurrence.atom())?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("node", occurrence.atom().id().to_string())
			})?;
			traverser.current = match kind {
				TraverseStepKind::KHop => {
					let depth = non_negative_usize(step.arguments.first())?;
					let nodes = bfs_with_check(
						&graph,
						source,
						TraversalDirection::Forward,
						Some(depth),
						&mut check,
					)?;
					Value::Array(occurrence_values(&graph, graph_ref, nodes)?.into())
				}
				TraverseStepKind::Reachable => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					Value::Bool(
						bfs_with_check(
							&graph,
							source,
							TraversalDirection::Forward,
							None,
							&mut check,
						)?
						.contains(&target),
					)
				}
				TraverseStepKind::ShortestPath => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					unweighted_path(&graph, graph_ref, source, target, &mut check)?
						.map_or(Value::None, |path| Value::Path(Box::new(path)))
				}
				TraverseStepKind::WeightedShortestPath => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					dijkstra_with_check(&graph, source, TraversalDirection::Forward, &mut check)?
						.materialize_path(&graph, graph_ref, target)?
						.map_or(Value::None, |path| Value::Path(Box::new(path)))
				}
				TraverseStepKind::DistanceTo => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					dijkstra_with_check(&graph, source, TraversalDirection::Forward, &mut check)?
						.distance_to(target)
						.map_or(Value::None, Value::Number)
				}
				TraverseStepKind::BellmanFord => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					bellman_ford_with_check(
						&graph,
						source,
						TraversalDirection::Forward,
						&mut check,
					)?
					.materialize_path(&graph, graph_ref, target)?
					.map_or(Value::None, |path| Value::Path(Box::new(path)))
				}
				TraverseStepKind::LowestCommonAncestor => {
					let target = target_id(&graph, occurrence.graph(), step.arguments.first())?;
					lowest_common_ancestor(&graph, source, target, &mut check)?
						.map(|atom| occurrence_value(&graph, graph_ref, atom))
						.transpose()?
						.unwrap_or(Value::None)
				}
				_ => unreachable!("seed algorithm kind was matched above"),
			};
			traverser.bulk = std::num::NonZeroU64::MIN;
			output.push(traverser);
		}
		Ok(output)
	}

	fn execute_induced_algorithm(
		&self,
		kind: TraverseStepKind,
		input: Vec<Traverser>,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let (graph_ref, atoms) = induced_atoms(&input)?;
		let graph = self.read.graph(self.snapshot, &graph_ref)?;
		let mut nodes = BTreeSet::new();
		for atom in atoms {
			let id = graph.atom_id(&atom)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("node", atom.id().to_string())
			})?;
			if !id.is_node() {
				return Err(domain_error("induced graph algorithms require node occurrences"));
			}
			nodes.insert(id);
		}
		let control = self.control;
		let mut check = || control.check();
		let edges = induced_edges(&graph, &nodes, &mut check)?;
		let induced = InducedGraph {
			inner: &graph,
			nodes,
			edges,
		};
		let value = match kind {
			TraverseStepKind::TopologicalSort => Value::Array(
				occurrence_values(
					&induced,
					graph_ref.clone(),
					topological_sort_with_check(&induced, &mut check)?,
				)?
				.into(),
			),
			TraverseStepKind::StronglyConnectedComponents => components_value(
				&induced,
				graph_ref.clone(),
				strongly_connected_components_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::WeaklyConnectedComponents => components_value(
				&induced,
				graph_ref.clone(),
				weakly_connected_components_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::Condensation => condensation_value(
				&induced,
				graph_ref.clone(),
				condensation_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::MinimumSpanningForest => spanning_value(
				&induced,
				graph_ref.clone(),
				minimum_spanning_forest_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::Cuts => cuts_value(
				&induced,
				graph_ref.clone(),
				find_cuts_with_check(&induced, &mut check)?,
			)?,
			TraverseStepKind::HasCycle => {
				Value::Bool(if graph_is_directed(&induced, &mut check)? {
					has_cycle_directed_with_check(&induced, &mut check)?
				} else {
					has_cycle_undirected_with_check(&induced, &mut check)?
				})
			}
			_ => unreachable!("induced algorithm kind was matched above"),
		};
		Ok(vec![Traverser::new(value)])
	}
}

fn occurrence(value: &Value) -> FerrosGrynnResult<&AtomOccurrenceRef> {
	value.as_atom_occurrence().ok_or_else(|| {
		FerrosGrynnError::traversal_source_type_mismatch(
			"node occurrence".to_owned(),
			value.kind().to_string(),
		)
	})
}

fn non_negative_usize(value: Option<&Value>) -> FerrosGrynnResult<usize> {
	let value = value.ok_or_else(|| FerrosGrynnError::invalid_argument("Missing depth."))?;
	value
		.as_number()
		.and_then(|number| i64::try_from(number).ok())
		.and_then(|number| usize::try_from(number).ok())
		.ok_or_else(|| {
			FerrosGrynnError::traversal_source_type_mismatch(
				"non-negative integer".to_owned(),
				value.kind().to_string(),
			)
		})
}

fn target_id<Graph: GraphRead>(
	graph: &Graph,
	scope: &GraphRef,
	value: Option<&Value>,
) -> FerrosGrynnResult<AtomId> {
	let value = value.ok_or_else(|| FerrosGrynnError::invalid_argument("Missing target atom."))?;
	let atom = match value {
		Value::AtomOccurrence(target) => {
			if target.graph().id() != scope.id() {
				return Err(domain_error("algorithm source and target belong to different graphs"));
			}
			target.atom()
		}
		Value::Atom(atom) => atom,
		_ => {
			return Err(FerrosGrynnError::traversal_source_type_mismatch(
				"atom or atom occurrence".to_owned(),
				value.kind().to_string(),
			));
		}
	};
	graph
		.atom_id(atom)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", atom.id().to_string()))
}

fn occurrence_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	id: AtomId,
) -> FerrosGrynnResult<Value> {
	let atom = graph
		.atom_ref(id)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", id.to_string()))?;
	Ok(Value::AtomOccurrence(Box::new(AtomOccurrenceRef::new(atom, reference))))
}

fn occurrence_values<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	ids: impl IntoIterator<Item = AtomId>,
) -> FerrosGrynnResult<Vec<Value>> {
	ids.into_iter().map(|id| occurrence_value(graph, reference.clone(), id)).collect()
}

#[allow(
	clippy::mutable_key_type,
	reason = "AtomRef ordering uses immutable atom kind and RecordId fields"
)]
fn induced_atoms(input: &[Traverser]) -> FerrosGrynnResult<(Arc<GraphRef>, BTreeSet<AtomRef>)> {
	let first = input
		.first()
		.ok_or_else(|| domain_error("an induced graph algorithm requires at least one node"))?;
	let first = occurrence(&first.current)?;
	let graph = Arc::new(first.graph().clone());
	let mut atoms = BTreeSet::new();
	for traverser in input {
		let occurrence = occurrence(&traverser.current)?;
		if occurrence.graph().id() != graph.id() {
			return Err(domain_error("an induced graph cannot span multiple graph identities"));
		}
		if occurrence.atom().kind() != AtomKind::Node {
			return Err(domain_error("an induced graph accepts node occurrences only"));
		}
		atoms.insert(occurrence.atom().clone());
	}
	Ok((graph, atoms))
}

fn induced_edges<Graph: GraphRead>(
	graph: &Graph,
	nodes: &BTreeSet<AtomId>,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<BTreeSet<AtomId>> {
	let mut edges = BTreeSet::new();
	for (candidate, edge) in graph.edge_ids()?.enumerate() {
		if candidate & 1023 == 0 {
			check()?;
		}
		let edge = edge?;
		let edge_view = graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?;
		let topology = edge_view.topology()?;
		let endpoints = binary_local_endpoints(topology.as_ref());
		if endpoints.is_some_and(|(left, right)| nodes.contains(&left) && nodes.contains(&right)) {
			edges.insert(edge);
		}
	}
	Ok(edges)
}

fn binary_local_endpoints(topology: &EdgeTopology) -> Option<(AtomId, AtomId)> {
	let endpoints = match topology {
		EdgeTopology::Directed {
			source,
			target,
		}
		| EdgeTopology::Bidirectional {
			source,
			target,
		} if source.len() == 1 && target.len() == 1 => (&source[0], &target[0]),
		EdgeTopology::Undirected {
			endpoints,
		} if endpoints.len() == 2 => (&endpoints[0], &endpoints[1]),
		_ => return None,
	};
	Some((endpoints.0.local_id()?, endpoints.1.local_id()?))
}

fn components_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	components: Vec<Vec<AtomId>>,
) -> FerrosGrynnResult<Value> {
	components
		.into_iter()
		.map(|component| {
			occurrence_values(graph, reference.clone(), component)
				.map(|values| Value::Array(values.into()))
		})
		.collect::<FerrosGrynnResult<Array>>()
		.map(Value::Array)
}

fn condensation_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	value: Condensation,
) -> FerrosGrynnResult<Value> {
	let components = value
		.components()
		.iter()
		.map(|component| {
			occurrence_values(graph, reference.clone(), component.iter().copied())
				.map(|values| Value::Array(values.into()))
		})
		.collect::<FerrosGrynnResult<Array>>()?;
	let edges = value
		.edges()
		.iter()
		.map(|(source, target)| {
			Value::Array(Array::from(vec![Value::from(*source), Value::from(*target)]))
		})
		.collect::<Array>();
	let mut object = Object::new();
	object.insert("components", Value::Array(components));
	object.insert("edges", Value::Array(edges));
	Ok(Value::Object(object))
}

fn spanning_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	value: SpanningForest,
) -> FerrosGrynnResult<Value> {
	let mut object = Object::new();
	object.insert(
		"edges",
		Value::Array(occurrence_values(graph, reference, value.edges().iter().copied())?.into()),
	);
	object.insert("total_weight", Value::Number(value.total_weight()));
	object.insert("component_count", Value::from(value.component_count()));
	Ok(Value::Object(object))
}

fn cuts_value<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	value: CutSet,
) -> FerrosGrynnResult<Value> {
	let mut object = Object::new();
	object.insert(
		"articulation_points",
		Value::Array(
			occurrence_values(
				graph,
				reference.clone(),
				value.articulation_points().iter().copied(),
			)?
			.into(),
		),
	);
	object.insert(
		"bridges",
		Value::Array(occurrence_values(graph, reference, value.bridges().iter().copied())?.into()),
	);
	Ok(Value::Object(object))
}

fn graph_is_directed<Graph: GraphRead>(
	graph: &Graph,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<bool> {
	for (candidate, edge) in graph.edge_ids()?.enumerate() {
		if candidate & 1023 == 0 {
			check()?;
		}
		let edge = edge?;
		let edge_view = graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?;
		let topology = edge_view.topology()?;
		if !matches!(topology.as_ref(), EdgeTopology::Undirected { .. }) {
			return Ok(true);
		}
	}
	Ok(false)
}

fn unweighted_path<Graph: GraphRead>(
	graph: &Graph,
	reference: Arc<GraphRef>,
	source: AtomId,
	target: AtomId,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Option<Path>> {
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let mut queue = VecDeque::from([source]);
	let mut predecessor = BTreeMap::<AtomId, (AtomId, crate::graph::TraversalStep)>::new();
	let mut seen = BTreeSet::from([source]);
	while let Some(current) = queue.pop_front() {
		check()?;
		if current == target {
			break;
		}
		for (candidate, step) in
			index.steps(&EdgeEndpoint::Local(current), TraversalDirection::Forward).enumerate()
		{
			if candidate & 1023 == 0 {
				check()?;
			}
			let Some(next) = step.target().local_id().filter(|id| id.is_node()) else {
				continue;
			};
			if seen.insert(next) {
				predecessor.insert(next, (current, step.clone()));
				queue.push_back(next);
			}
		}
	}
	if !seen.contains(&target) {
		return Ok(None);
	}
	let mut chain = Vec::new();
	let mut current = target;
	while current != source {
		let (parent, step) = predecessor.get(&current).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Shortest-path predecessor chain is incomplete.")
		})?;
		chain.push((current, step));
		current = parent;
	}
	chain.reverse();
	let source_atom = graph
		.atom_ref(source)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", source.to_string()))?;
	let mut steps = vec![PathStep::new(
		AtomOccurrenceRef::new(source_atom, reference.clone()),
		None,
		PathDirection::Forward,
		EndpointRole::Source,
	)];
	for (node, link) in chain {
		let atom = graph
			.atom_ref(node)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("node", node.to_string()))?;
		let edge = graph.atom_ref(link.via_edge())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("edge", link.via_edge().to_string())
		})?;
		steps.push(PathStep::new(
			AtomOccurrenceRef::new(atom, reference.clone()),
			Some(AtomOccurrenceRef::new(edge, reference.clone())),
			link.direction(),
			link.role(),
		));
	}
	Ok(Some(Path::new(
		graph.identity(),
		graph.snapshot_id(),
		steps,
		Some(Number::from(predecessor_distance(&predecessor, source, target)?)),
	)))
}

fn predecessor_distance(
	predecessor: &BTreeMap<AtomId, (AtomId, crate::graph::TraversalStep)>,
	source: AtomId,
	mut target: AtomId,
) -> FerrosGrynnResult<usize> {
	let mut distance = 0_usize;
	while target != source {
		target = predecessor.get(&target).map(|(parent, _)| *parent).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Shortest-path chain is incomplete.")
		})?;
		distance = distance
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("path length"))?;
	}
	Ok(distance)
}

fn lowest_common_ancestor<Graph: GraphRead>(
	graph: &Graph,
	left: AtomId,
	right: AtomId,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Option<AtomId>> {
	if !matches!(graph.kind(), GraphKind::Tree | GraphKind::RootedTree) {
		return Err(domain_error("tv::lca requires a tree or rooted-tree graph"));
	}
	let mut parents = BTreeMap::<AtomId, AtomId>::new();
	for (candidate, edge) in graph.edge_ids()?.enumerate() {
		if candidate & 1023 == 0 {
			check()?;
		}
		let edge = edge?;
		let edge_view = graph
			.edge(edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?;
		let topology = edge_view.topology()?;
		let EdgeTopology::Directed {
			source,
			target,
		} = topology.as_ref()
		else {
			return Err(domain_error("tv::lca requires directed parent-child edges"));
		};
		if source.len() != 1 || target.len() != 1 {
			return Err(domain_error("tv::lca requires binary parent-child edges"));
		}
		let (Some(parent), Some(child)) = (source[0].local_id(), target[0].local_id()) else {
			return Err(domain_error("tv::lca does not accept cross-level edges"));
		};
		if parents.insert(child, parent).is_some() {
			return Err(domain_error("tv::lca requires at most one parent per node"));
		}
	}
	let mut left_ancestors = BTreeSet::new();
	let mut current = Some(left);
	while let Some(node) = current {
		check()?;
		if !left_ancestors.insert(node) {
			return Err(domain_error("tv::lca detected a cycle in the parent relation"));
		}
		current = parents.get(&node).copied();
	}
	let mut seen = BTreeSet::new();
	let mut current = Some(right);
	while let Some(node) = current {
		check()?;
		if left_ancestors.contains(&node) {
			return Ok(Some(node));
		}
		if !seen.insert(node) {
			return Err(domain_error("tv::lca detected a cycle in the parent relation"));
		}
		current = parents.get(&node).copied();
	}
	Ok(None)
}

fn domain_error(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("traversal algorithm", reason)
}

struct InducedGraph<'a, Graph: GraphRead> {
	inner: &'a Graph,
	nodes: BTreeSet<AtomId>,
	edges: BTreeSet<AtomId>,
}

impl<Graph: GraphRead> GraphRead for InducedGraph<'_, Graph> {
	type Edge<'a>
		= Graph::Edge<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;
	type Node<'a>
		= Graph::Node<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.inner.identity()
	}

	fn revision(&self) -> u64 {
		self.inner.revision()
	}

	fn kind(&self) -> GraphKind {
		self.inner.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.inner.properties()
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(Box::new(self.nodes.iter().copied().map(Ok)))
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(Box::new(self.edges.iter().copied().map(Ok)))
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		if self.nodes.contains(&id) {
			self.inner.node(id)
		} else {
			Ok(None)
		}
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		if self.edges.contains(&id) {
			self.inner.edge(id)
		} else {
			Ok(None)
		}
	}

	fn storage_mode(&self) -> GraphStorageMode {
		self.inner.storage_mode()
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.inner.snapshot_id()
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		self.inner.graph_ref()
	}
}
