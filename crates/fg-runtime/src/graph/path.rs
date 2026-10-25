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

/// Direction in which a path step traverses an incidence.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum PathDirection {
	/// Traverse the incidence in its forward direction.
	Forward,
	/// Traverse the incidence opposite to its forward direction.
	Reverse,
	/// Traverse an incidence without a selected directed orientation.
	Undirected,
}

/// Role of an endpoint within a directed or undirected incidence.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum EndpointRole {
	/// Endpoint is on the source side of the incidence.
	Source,
	/// Endpoint is on the target side of the incidence.
	Target,
	/// Endpoint has no source-or-target role.
	Undirected,
}

/// Snapshot-stable atom occurrence and optional edge used to reach it.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct PathStep {
	/// Atom associated with this path step.
	atom: AtomOccurrenceRef,
	/// Via edge associated with this path step.
	via_edge: Option<AtomOccurrenceRef>,
	/// Direction associated with this path step.
	direction: PathDirection,
	/// Role associated with this path step.
	role: EndpointRole,
}

impl PathStep {
	/// Constructs `PathStep` from the supplied initial state.
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

	/// Borrows the stored atom of this `PathStep`.
	pub const fn atom(&self) -> &AtomOccurrenceRef {
		&self.atom
	}

	/// Returns via edge for `PathStep`.
	pub const fn via_edge(&self) -> Option<&AtomOccurrenceRef> {
		self.via_edge.as_ref()
	}

	/// Returns the stored direction of this `PathStep`.
	pub const fn direction(&self) -> PathDirection {
		self.direction
	}

	/// Returns the stored role of this `PathStep`.
	pub const fn role(&self) -> EndpointRole {
		self.role
	}
}

/// Owned snapshot-qualified sequence of atom occurrences with an optional
/// accumulated cost.
#[derive(Debug, Clone)]
pub struct Path {
	/// Origin associated with this path.
	origin: Uuid,
	/// Snapshot associated with this path.
	snapshot: SnapshotId,
	/// Steps associated with this path.
	steps: Arc<[PathStep]>,
	/// Length associated with this path.
	length: usize,
	/// Cost associated with this path.
	cost: Option<Number>,
}

impl Path {
	/// Constructs `Path` from the supplied initial state.
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

	/// Returns the stored origin of this `Path`.
	pub const fn origin(&self) -> Uuid {
		self.origin
	}

	/// Returns the stored snapshot of this `Path`.
	pub const fn snapshot(&self) -> SnapshotId {
		self.snapshot
	}

	/// Borrows the stored steps of this `Path`.
	pub fn steps(&self) -> &[PathStep] {
		&self.steps
	}

	/// Returns the stored length of this `Path`.
	pub const fn length(&self) -> usize {
		self.length
	}

	/// Returns the stored cost of this `Path`.
	pub const fn cost(&self) -> Option<Number> {
		self.cost
	}

	/// Estimates retained steps, public identifiers and graph references
	/// without allocation. Shared references are charged conservatively.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		self.steps.iter().fold(
			self.steps.len().saturating_mul(size_of::<PathStep>()),
			|bytes, step| {
				let occurrence_bytes = |atom: &AtomOccurrenceRef| {
					atom.atom()
						.id()
						.estimated_heap_bytes()
						.saturating_add(size_of::<super::GraphRef>())
						.saturating_add(atom.graph().name().len())
				};
				bytes
					.saturating_add(occurrence_bytes(step.atom()))
					.saturating_add(step.via_edge().map_or(0, occurrence_bytes))
			},
		)
	}

	/// Returns atoms for `Path`.
	pub fn atoms(&self) -> impl Iterator<Item = &AtomOccurrenceRef> {
		self.steps.iter().flat_map(|step| step.via_edge().into_iter().chain([step.atom()]))
	}

	/// Returns nodes for `Path`.
	pub fn nodes(&self) -> impl Iterator<Item = &AtomOccurrenceRef> {
		self.atoms().filter(|atom| atom.atom().kind() == AtomKind::Node)
	}

	/// Returns edges for `Path`.
	pub fn edges(&self) -> impl Iterator<Item = &AtomOccurrenceRef> {
		self.steps.iter().filter_map(PathStep::via_edge)
	}

	/// Returns resolve for `Path`.
	///
	/// # Errors
	///
	/// Returns `graph_identity_mismatch`, `snapshot_unavailable`,
	/// `invalid_argument` diagnostics and propagates failures from called
	/// operations.
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
	/// Compares this `Path` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		(self.origin, self.snapshot, &self.steps, self.cost)
			== (other.origin, other.snapshot, &other.steps, other.cost)
	}
}

impl Eq for Path {}

impl PartialOrd for Path {
	/// Compares this `Path` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Path {
	/// Compares this `Path` with another value using its ordering contract.
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
	/// Feeds this `Path` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.origin.hash(state);
		self.snapshot.hash(state);
		self.steps.hash(state);
		self.cost.hash(state);
	}
}

/// Node or edge view obtained by resolving a stable atom occurrence.
#[derive(Debug)]
#[non_exhaustive]
pub enum ResolvedAtom<Node, Edge> {
	/// Resolved borrowed node view.
	Node(
		/// Resolved borrowed node view.
		Node,
	),
	/// Resolved borrowed edge view.
	Edge(
		/// Resolved borrowed edge view.
		Edge,
	),
}

impl<Node, Edge> ResolvedAtom<Node, Edge> {
	/// Returns kind for `ResolvedAtom<Node, Edge>`.
	pub const fn kind(&self) -> AtomKind {
		match self {
			Self::Node(_) => AtomKind::Node,
			Self::Edge(_) => AtomKind::Edge,
		}
	}
}

/// Resolved atom view and optional incoming edge for one path step.
#[derive(Debug)]
pub struct ResolvedPathStep<Node, Edge> {
	/// Atom associated with this resolved path step.
	atom: ResolvedAtom<Node, Edge>,
	/// Via edge associated with this resolved path step.
	via_edge: Option<Edge>,
	/// Direction associated with this resolved path step.
	direction: PathDirection,
	/// Role associated with this resolved path step.
	role: EndpointRole,
}

impl<Node, Edge> ResolvedPathStep<Node, Edge> {
	/// Borrows the stored atom of this `ResolvedPathStep<Node, Edge>`.
	pub const fn atom(&self) -> &ResolvedAtom<Node, Edge> {
		&self.atom
	}

	/// Returns via edge for `ResolvedPathStep<Node, Edge>`.
	pub const fn via_edge(&self) -> Option<&Edge> {
		self.via_edge.as_ref()
	}

	/// Returns the stored direction of this `ResolvedPathStep<Node, Edge>`.
	pub const fn direction(&self) -> PathDirection {
		self.direction
	}

	/// Returns the stored role of this `ResolvedPathStep<Node, Edge>`.
	pub const fn role(&self) -> EndpointRole {
		self.role
	}
}

/// Materialized path views together with the path cost, when known.
#[derive(Debug)]
pub struct ResolvedPath<Node, Edge> {
	/// Steps associated with this resolved path.
	steps: Vec<ResolvedPathStep<Node, Edge>>,
	/// Cost associated with this resolved path.
	cost: Option<Number>,
}

impl<Node, Edge> ResolvedPath<Node, Edge> {
	/// Borrows the stored steps of this `ResolvedPath<Node, Edge>`.
	pub fn steps(&self) -> &[ResolvedPathStep<Node, Edge>] {
		&self.steps
	}

	/// Returns the stored cost of this `ResolvedPath<Node, Edge>`.
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
/// Path trace id state.
pub struct PathTraceId(
	/// Ordinal locating one prefix node in the execution path-trace arena.
	u32,
);

#[allow(dead_code, reason = "the query traversal VM consumes trace nodes in integration stage 6")]
#[derive(Debug, Clone)]
/// Path trace node state.
struct PathTraceNode {
	/// Previous associated with this path trace node.
	previous: Option<PathTraceId>,
	/// Step associated with this path trace node.
	step: PathStep,
	/// Cumulative cost associated with this path trace node.
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
pub struct PathTraceArena {
	/// Nodes associated with this path trace arena.
	nodes: Vec<PathTraceNode>,
}

#[allow(dead_code, reason = "the query traversal VM consumes this API in integration stage 6")]
impl PathTraceArena {
	/// Creates a new `PathTraceArena` value.
	pub fn new() -> Self {
		Self::default()
	}

	/// Estimates the bytes retained by one additional trace node.
	pub fn estimated_node_heap_bytes(step: &PathStep) -> usize {
		std::mem::size_of::<PathTraceNode>()
			.saturating_add(step.atom().atom().id().estimated_heap_bytes())
			.saturating_add(
				step.via_edge().map_or(0, |edge| edge.atom().id().estimated_heap_bytes()),
			)
	}

	/// Pushes path trace arena.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn push(
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

	/// Materializes path trace arena.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn materialize(
		&self,
		end: PathTraceId,
		origin: Uuid,
		snapshot: SnapshotId,
	) -> FerrosGrynnResult<Path> {
		self.materialize_with_admission(end, origin, snapshot, &mut |_| Ok(()), &mut || Ok(()))
	}

	/// Reserves the complete output before cloning any trace steps.
	///
	/// # Errors
	/// Rejects absent traces, cancellation, size overflow or memory admission.
	#[allow(
		clippy::impl_trait_in_params,
		reason = "Exact-version internal call signatures remain unchanged during extraction"
	)]
	pub fn materialize_with_admission(
		&self,
		end: PathTraceId,
		origin: Uuid,
		snapshot: SnapshotId,
		admit: &mut impl FnMut(usize) -> FerrosGrynnResult<()>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Path> {
		let mut cursor = Some(end);
		let mut count = 0usize;
		let mut bytes = std::mem::size_of::<Path>().saturating_add(64);
		while let Some(id) = cursor {
			check()?;
			let node = self.nodes.get(id.0 as usize).ok_or_else(|| {
				FerrosGrynnError::graph_constraint_violation(
					"path trace",
					"the terminal trace node does not exist",
				)
			})?;
			count = count
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("path steps"))?;
			bytes = Self::estimated_node_heap_bytes(&node.step)
				.checked_mul(2)
				.and_then(|node_bytes| bytes.checked_add(node_bytes))
				.ok_or_else(|| {
					FerrosGrynnError::operation_overflow("path materialization bytes")
				})?;
			cursor = node.previous;
		}
		admit(bytes)?;
		let mut cursor = Some(end);
		let mut steps = Vec::with_capacity(count);
		let mut cost = None;
		while let Some(id) = cursor {
			check()?;
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

/// Resolves atom.
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

/// Resolves edge.
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

/// Operations shared with the engine and value codecs.
pub trait PathInternal: Sized {
	/// Estimates retained steps, public identifiers and graph references
	/// without allocation. Shared references are charged conservatively.
	fn estimated_heap_bytes(&self) -> usize;
}
impl PathInternal for Path {
	fn estimated_heap_bytes(&self) -> usize {
		<Path>::estimated_heap_bytes(self)
	}
}

#[cfg(test)]
/// Regression tests for path behavior.
mod tests {
	use std::sync::Arc;

	use super::{EndpointRole, PathDirection, PathStep, PathTraceArena};
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
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

	/// Returns graph ref for this module.
	fn graph_ref() -> Arc<GraphRef> {
		Arc::new(
			GraphRef::new(
				GraphId::new(1).assert_some(),
				"path-test",
				GraphKind::Graph,
				GraphStorageMode::InMemory,
			)
			.assert_ok(),
		)
	}

	/// Verifies path value heap estimate includes steps and long public
	/// identifiers.
	#[test]
	fn path_value_heap_estimate_includes_steps_and_long_public_identifiers() {
		let record = crate::values::RecordId::new(
			"person",
			crate::values::RecordIdPart::String("x".repeat(4096).into()),
		)
		.assert_ok();
		let graph = Graph::<Mutable>::new();
		let step = PathStep::new(
			AtomOccurrenceRef::new(
				super::super::AtomRef::new(super::super::AtomKind::Node, record),
				graph_ref(),
			),
			None,
			PathDirection::Forward,
			EndpointRole::Source,
		);
		let path = super::Path::new(graph.identity(), graph.snapshot_id(), vec![step], None);
		let value = crate::values::Value::Path(Box::new(path));
		assert!(value.estimated_heap_bytes() > 4096);
		assert_eq!(value.estimated_heap_bytes(), value.estimated_heap_bytes());
	}

	/// Verifies trace arena shares prefixes and materializes order.
	#[test]
	fn trace_arena_shares_prefixes_and_materializes_order() {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let edge = graph
			.add_edge(
				super::super::EdgeTopology::directed([first], [second]),
				None,
				PropertyMap::new(),
			)
			.assert_ok();
		let first_ref = graph.atom_ref(first).assert_ok().assert_some();
		let second_ref = graph.atom_ref(second).assert_ok().assert_some();
		let edge_ref = graph.atom_ref(edge).assert_ok().assert_some();
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
			.assert_ok();
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
			.assert_ok();
		let path = arena.materialize(end, graph.identity(), graph.snapshot_id()).assert_ok();
		let mut checked = 0;
		let mut admitted = 0;
		assert!(
			arena
				.materialize_with_admission(
					end,
					graph.identity(),
					graph.snapshot_id(),
					&mut |bytes| {
						admitted = bytes;
						Err(super::FerrosGrynnError::invalid_argument("test admission rejection"))
					},
					&mut || {
						checked += 1;
						Ok(())
					},
				)
				.is_err()
		);
		assert_eq!(checked, 2);
		assert!(admitted > 2 * std::mem::size_of::<PathStep>());

		assert_eq!(path.length(), 1);
		assert_eq!(path.nodes().count(), 2);
		assert_eq!(path.edges().count(), 1);
		assert_eq!(path.cost(), Some(3.into()));
		assert_eq!(path.resolve(&graph).assert_ok().steps().len(), 2);
	}

	/// Verifies resolver rejects a different graph identity.
	#[test]
	fn resolver_rejects_a_different_graph_identity() {
		let graph = Graph::<Mutable>::new();
		let other = Graph::<Mutable>::new();
		let path = super::Path::new(graph.identity(), graph.snapshot_id(), Vec::new(), None);
		let error = path.resolve(&other).assert_err();
		assert_eq!(error.status().code().to_string(), "FG-G1F05");
	}
}
