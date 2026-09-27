use std::collections::VecDeque;

use rapidhash::{RapidHashMap, RapidHashSet};

use super::algorithm_failure;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		EdgeDirection,
		EdgeEndpoint,
		EdgeRead,
		EdgeTopology,
		EndpointRole,
		GraphRead,
		PathDirection,
	},
	values::Uuid,
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
	pub(super) target: EdgeEndpoint,
	pub(super) via_edge: AtomId,
	pub(super) topology: EdgeDirection,
	pub(super) direction: PathDirection,
	pub(super) role: EndpointRole,
}

impl TraversalStep {
	pub const fn target(&self) -> &EdgeEndpoint {
		&self.target
	}

	pub const fn via_edge(&self) -> AtomId {
		self.via_edge
	}

	pub const fn topology(&self) -> EdgeDirection {
		self.topology
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
	has_nested_endpoints: bool,
	forward: RapidHashMap<EdgeEndpoint, Vec<TraversalStep>>,
	backward: RapidHashMap<EdgeEndpoint, Vec<TraversalStep>>,
}

impl TraversalIndex {
	pub fn new<R: GraphRead>(graph: &R, semantics: TraversalSemantics) -> FerrosGrynnResult<Self> {
		Self::build(graph, semantics, true, &mut || Ok(()))
	}

	pub(crate) fn new_for_expansion<R: GraphRead>(
		graph: &R,
		semantics: TraversalSemantics,
	) -> FerrosGrynnResult<Self> {
		Self::build(graph, semantics, false, &mut || Ok(()))
	}

	pub(super) fn new_with_check<R: GraphRead, Check>(
		graph: &R,
		semantics: TraversalSemantics,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		Self::build(graph, semantics, true, check)
	}

	fn build<R: GraphRead, Check>(
		graph: &R,
		semantics: TraversalSemantics,
		register_isolated: bool,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		let mut index = Self {
			identity: graph.identity(),
			revision: graph.revision(),
			semantics,
			has_nested_endpoints: false,
			forward: RapidHashMap::default(),
			backward: RapidHashMap::default(),
		};

		if register_isolated {
			for id in graph.node_ids()? {
				check()?;
				index.register_endpoint(EdgeEndpoint::Local(id?));
			}
			if semantics == TraversalSemantics::Atom {
				for id in graph.edge_ids()? {
					check()?;
					index.register_endpoint(EdgeEndpoint::Local(id?));
				}
			}
		}

		for id in graph.edge_ids()? {
			check()?;
			let id = id?;
			let edge = graph.edge(id)?.ok_or_else(|| {
				algorithm_failure(
					"traversal index",
					format!("edge iterator yielded missing edge {id}"),
				)
			})?;
			let topology = edge.topology()?;
			index.register_topology(id, topology.as_ref());
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

	pub(crate) const fn has_nested_endpoints(&self) -> bool {
		self.has_nested_endpoints
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
			} => self.connect_sets(edge, source, target, EdgeDirection::Directed),
			EdgeTopology::Bidirectional {
				source,
				target,
			} => self.connect_sets(edge, source, target, EdgeDirection::Bidirectional),
			EdgeTopology::Undirected {
				endpoints,
			} => {
				for (index, left) in endpoints.iter().enumerate() {
					for right in &endpoints[index + 1..] {
						self.connect(
							edge,
							left,
							right,
							EdgeDirection::Undirected,
							PathDirection::Undirected,
							EndpointRole::Undirected,
							PathDirection::Undirected,
							EndpointRole::Undirected,
						);
						self.connect(
							edge,
							right,
							left,
							EdgeDirection::Undirected,
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
		topology: EdgeDirection,
	) {
		for left in source {
			for right in target {
				self.connect(
					edge,
					left,
					right,
					topology,
					PathDirection::Forward,
					EndpointRole::Target,
					PathDirection::Reverse,
					EndpointRole::Source,
				);
				if topology == EdgeDirection::Bidirectional {
					self.connect(
						edge,
						right,
						left,
						topology,
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
		topology: EdgeDirection,
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
			topology,
			direction: forward_direction,
			role: forward_role,
		});
		self.backward.entry(to.clone()).or_default().push(TraversalStep {
			target: from.clone(),
			via_edge: edge,
			topology,
			direction: backward_direction,
			role: backward_role,
		});
	}

	fn accepts(&self, endpoint: &EdgeEndpoint) -> bool {
		self.semantics == TraversalSemantics::Atom
			|| endpoint_kind(endpoint) == Some(AtomKind::Node)
	}

	fn register_endpoint(&mut self, endpoint: EdgeEndpoint) {
		self.has_nested_endpoints |= matches!(endpoint, EdgeEndpoint::Nested(_));
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

const fn endpoint_kind(endpoint: &EdgeEndpoint) -> Option<AtomKind> {
	match endpoint {
		EdgeEndpoint::Local(id) => id.kind(),
		EdgeEndpoint::Nested(qualified) => qualified.atom().kind(),
	}
}

pub(super) fn local_steps(
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
