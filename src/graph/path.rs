use std::{
	cmp::Ordering,
	hash::{Hash, Hasher},
	sync::Arc,
};

use super::{AtomKind, AtomOccurrenceRef, AtomRef, GraphRead, SnapshotId};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{Number, Uuid},
};

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum PathDirection {
	Forward,
	Reverse,
	Undirected,
}

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EndpointRole {
	Source,
	Target,
	Undirected,
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct PathStep {
	atom: AtomOccurrenceRef,
	via_edge: Option<AtomOccurrenceRef>,
	direction: PathDirection,
	role: EndpointRole,
}

impl PathStep {
	pub const fn new(
		atom: AtomOccurrenceRef,
		via_edge: Option<AtomOccurrenceRef>,
		direction: PathDirection,
		role: EndpointRole,
	) -> Self {
		Self {
			atom,
			via_edge,
			direction,
			role,
		}
	}

	pub const fn atom(&self) -> &AtomOccurrenceRef {
		&self.atom
	}

	pub const fn via_edge(&self) -> Option<&AtomOccurrenceRef> {
		self.via_edge.as_ref()
	}

	pub const fn direction(&self) -> PathDirection {
		self.direction
	}

	pub const fn role(&self) -> EndpointRole {
		self.role
	}
}

#[derive(Debug, Clone)]
pub struct Path {
	origin: Uuid,
	snapshot: SnapshotId,
	steps: Arc<[PathStep]>,
	length: usize,
	cost: Option<Number>,
}

impl Path {
	pub fn new<Steps: Into<Arc<[PathStep]>>>(
		origin: Uuid,
		snapshot: SnapshotId,
		steps: Steps,
		cost: Option<Number>,
	) -> Self {
		let steps = steps.into();
		let length = steps.iter().filter(|step| step.via_edge.is_some()).count();
		Self {
			origin,
			snapshot,
			steps,
			length,
			cost,
		}
	}

	pub const fn origin(&self) -> Uuid {
		self.origin
	}

	pub const fn snapshot(&self) -> SnapshotId {
		self.snapshot
	}

	pub fn steps(&self) -> &[PathStep] {
		&self.steps
	}

	pub const fn length(&self) -> usize {
		self.length
	}

	pub const fn cost(&self) -> Option<Number> {
		self.cost
	}

	pub fn atoms(&self) -> impl Iterator<Item = &AtomOccurrenceRef> {
		self.steps.iter().flat_map(|step| step.via_edge().into_iter().chain([step.atom()]))
	}

	pub fn nodes(&self) -> impl Iterator<Item = &AtomOccurrenceRef> {
		self.atoms().filter(|atom| atom.atom().kind() == AtomKind::Node)
	}

	pub fn edges(&self) -> impl Iterator<Item = &AtomOccurrenceRef> {
		self.steps.iter().filter_map(PathStep::via_edge)
	}

	pub fn resolve<'graph, R: GraphRead>(
		&self,
		graph: &'graph R,
	) -> FerrosGrynnResult<ResolvedPath<R::Node<'graph>, R::Edge<'graph>>> {
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
		let graph_id = self.steps.first().map(|step| step.atom().graph().id());
		if self.steps.iter().any(|step| {
			Some(step.atom().graph().id()) != graph_id
				|| step.via_edge().is_some_and(|edge| Some(edge.graph().id()) != graph_id)
		}) {
			return Err(FerrosGrynnError::invalid_argument(
				"a cross-graph path cannot be resolved through one graph view",
			));
		}
		if let (Some(expected), Some(actual)) =
			(graph_id, graph.graph_ref().map(|value| value.id()))
			&& expected != actual
		{
			return Err(FerrosGrynnError::graph_identity_mismatch(
				expected.to_string(),
				actual.to_string(),
			));
		}

		let mut steps = Vec::with_capacity(self.steps.len());
		for step in self.steps.iter() {
			let atom = resolve_atom(graph, step.atom().atom())?;
			let via_edge = step
				.via_edge()
				.map(|reference| resolve_edge(graph, reference.atom()))
				.transpose()?;
			steps.push(ResolvedPathStep {
				atom,
				via_edge,
				direction: step.direction(),
				role: step.role(),
			});
		}
		Ok(ResolvedPath {
			steps,
			cost: self.cost,
		})
	}
}

impl PartialEq for Path {
	fn eq(&self, other: &Self) -> bool {
		(self.origin, self.snapshot, &self.steps, self.cost)
			== (other.origin, other.snapshot, &other.steps, other.cost)
	}
}

impl Eq for Path {}

impl PartialOrd for Path {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Path {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.origin, self.snapshot, &self.steps, self.cost).cmp(&(
			other.origin,
			other.snapshot,
			&other.steps,
			other.cost,
		))
	}
}

impl Hash for Path {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.origin.hash(state);
		self.snapshot.hash(state);
		self.steps.hash(state);
		self.cost.hash(state);
	}
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ResolvedAtom<Node, Edge> {
	Node(Node),
	Edge(Edge),
}

impl<Node, Edge> ResolvedAtom<Node, Edge> {
	pub const fn kind(&self) -> AtomKind {
		match self {
			Self::Node(_) => AtomKind::Node,
			Self::Edge(_) => AtomKind::Edge,
		}
	}
}

#[derive(Debug)]
pub struct ResolvedPathStep<Node, Edge> {
	atom: ResolvedAtom<Node, Edge>,
	via_edge: Option<Edge>,
	direction: PathDirection,
	role: EndpointRole,
}

impl<Node, Edge> ResolvedPathStep<Node, Edge> {
	pub const fn atom(&self) -> &ResolvedAtom<Node, Edge> {
		&self.atom
	}

	pub const fn via_edge(&self) -> Option<&Edge> {
		self.via_edge.as_ref()
	}

	pub const fn direction(&self) -> PathDirection {
		self.direction
	}

	pub const fn role(&self) -> EndpointRole {
		self.role
	}
}

#[derive(Debug)]
pub struct ResolvedPath<Node, Edge> {
	steps: Vec<ResolvedPathStep<Node, Edge>>,
	cost: Option<Number>,
}

impl<Node, Edge> ResolvedPath<Node, Edge> {
	pub fn steps(&self) -> &[ResolvedPathStep<Node, Edge>] {
		&self.steps
	}

	pub const fn cost(&self) -> Option<Number> {
		self.cost
	}
}

#[allow(dead_code, reason = "the query traversal VM consumes trace IDs in integration stage 6")]
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[allow(
	clippy::redundant_pub_crate,
	reason = "the crate-private query VM will store trace IDs outside this sealed runtime module"
)]
pub(crate) struct PathTraceId(u32);

#[allow(dead_code, reason = "the query traversal VM consumes trace nodes in integration stage 6")]
#[derive(Debug, Clone)]
struct PathTraceNode {
	previous: Option<PathTraceId>,
	step: PathStep,
	cumulative_cost: Option<Number>,
}

/// Prefix-sharing execution arena. It is created only when the physical plan
/// requests path tracking.
#[allow(
	dead_code,
	reason = "the query traversal VM consumes the trace arena in integration stage 6"
)]
#[derive(Debug, Default)]
#[allow(
	clippy::redundant_pub_crate,
	reason = "the crate-private query VM will build traces outside this sealed runtime module"
)]
pub(crate) struct PathTraceArena {
	nodes: Vec<PathTraceNode>,
}

#[allow(dead_code, reason = "the query traversal VM consumes this API in integration stage 6")]
impl PathTraceArena {
	pub(crate) fn new() -> Self {
		Self::default()
	}

	pub(crate) fn push(
		&mut self,
		previous: Option<PathTraceId>,
		step: PathStep,
		cumulative_cost: Option<Number>,
	) -> FerrosGrynnResult<PathTraceId> {
		if previous.is_some_and(|id| id.0 as usize >= self.nodes.len()) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"path trace",
				"the previous trace node does not exist",
			));
		}
		let id = u32::try_from(self.nodes.len())
			.map(PathTraceId)
			.map_err(|_conversion_error| FerrosGrynnError::graph_capacity_exceeded("path trace"))?;
		self.nodes.push(PathTraceNode {
			previous,
			step,
			cumulative_cost,
		});
		Ok(id)
	}

	pub(crate) fn materialize(
		&self,
		end: PathTraceId,
		origin: Uuid,
		snapshot: SnapshotId,
	) -> FerrosGrynnResult<Path> {
		let mut cursor = Some(end);
		let mut steps = Vec::new();
		let mut cost = None;
		while let Some(id) = cursor {
			let node = self.nodes.get(id.0 as usize).ok_or_else(|| {
				FerrosGrynnError::graph_constraint_violation(
					"path trace",
					"the terminal trace node does not exist",
				)
			})?;
			if cost.is_none() {
				cost = node.cumulative_cost;
			}
			steps.push(node.step.clone());
			cursor = node.previous;
		}
		steps.reverse();
		Ok(Path::new(origin, snapshot, Arc::<[PathStep]>::from(steps), cost))
	}
}

fn resolve_atom<'graph, R: GraphRead>(
	graph: &'graph R,
	reference: &AtomRef,
) -> FerrosGrynnResult<ResolvedAtom<R::Node<'graph>, R::Edge<'graph>>> {
	let id = graph.atom_id(reference)?.ok_or_else(|| {
		FerrosGrynnError::atom_does_not_exist(
			reference.kind().to_string(),
			reference.id().to_string(),
		)
	})?;
	match reference.kind() {
		AtomKind::Node => graph.node(id)?.map(ResolvedAtom::Node).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("node", reference.id().to_string())
		}),
		AtomKind::Edge => graph.edge(id)?.map(ResolvedAtom::Edge).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("edge", reference.id().to_string())
		}),
	}
}

fn resolve_edge<'graph, R: GraphRead>(
	graph: &'graph R,
	reference: &AtomRef,
) -> FerrosGrynnResult<R::Edge<'graph>> {
	if reference.kind() != AtomKind::Edge {
		return Err(FerrosGrynnError::atom_kind_mismatch(
			reference.id().to_string(),
			reference.kind().to_string(),
			AtomKind::Edge.to_string(),
		));
	}
	let id = graph
		.atom_id(reference)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", reference.id().to_string()))?;
	graph
		.edge(id)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", reference.id().to_string()))
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::{EndpointRole, PathDirection, PathStep, PathTraceArena};
	use crate::values::{
		PropertyMap,
		runtime::graph::{
			AtomOccurrenceRef,
			Graph,
			GraphId,
			GraphKind,
			GraphRead,
			GraphRef,
			GraphStorageMode,
			Mutable,
		},
	};

	fn graph_ref() -> Arc<GraphRef> {
		Arc::new(
			GraphRef::new(
				GraphId::new(1).unwrap(),
				"path-test",
				GraphKind::Graph,
				GraphStorageMode::InMemory,
			)
			.unwrap(),
		)
	}

	#[test]
	fn trace_arena_shares_prefixes_and_materializes_order() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).unwrap();
		let second = graph.add_node(None, PropertyMap::new()).unwrap();
		let edge = graph
			.add_edge(
				super::super::EdgeTopology::directed([first], [second]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		let first_ref = graph.atom_ref(first).unwrap().unwrap();
		let second_ref = graph.atom_ref(second).unwrap().unwrap();
		let edge_ref = graph.atom_ref(edge).unwrap().unwrap();
		let graph_ref = graph_ref();
		let mut arena = PathTraceArena::new();
		let prefix = arena
			.push(
				None,
				PathStep::new(
					AtomOccurrenceRef::new(first_ref, graph_ref.clone()),
					None,
					PathDirection::Forward,
					EndpointRole::Source,
				),
				Some(1.into()),
			)
			.unwrap();
		let end = arena
			.push(
				Some(prefix),
				PathStep::new(
					AtomOccurrenceRef::new(second_ref, graph_ref.clone()),
					Some(AtomOccurrenceRef::new(edge_ref, graph_ref)),
					PathDirection::Forward,
					EndpointRole::Target,
				),
				Some(3.into()),
			)
			.unwrap();
		let path = arena.materialize(end, graph.identity(), graph.snapshot_id()).unwrap();

		assert_eq!(path.length(), 1);
		assert_eq!(path.nodes().count(), 2);
		assert_eq!(path.edges().count(), 1);
		assert_eq!(path.cost(), Some(3.into()));
		assert_eq!(path.resolve(&graph).unwrap().steps().len(), 2);
	}

	#[test]
	fn resolver_rejects_a_different_graph_identity() {
		let graph = Graph::<Mutable>::new();
		let other = Graph::<Mutable>::new();
		let path = super::Path::new(graph.identity(), graph.snapshot_id(), Vec::new(), None);
		let error = path.resolve(&other).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-G1F05");
	}
}
