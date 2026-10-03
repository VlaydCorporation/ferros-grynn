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

	/// Admits adjacency growth before constructing an owned index. The estimate
	/// includes both directions, hash table growth, endpoint clones and vector
	/// reallocation peaks, including qualified scopes.
	///
	/// # Errors
	/// Returns graph read, cancellation, checked-size or caller budget errors.
	pub(crate) fn new_with_admission<R: GraphRead>(
		graph: &R,
		semantics: TraversalSemantics,
		admit: &mut impl FnMut(usize) -> FerrosGrynnResult<()>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		let overflow = || FerrosGrynnError::operation_overflow("traversal adjacency bytes");
		admit(graph.node_count()?.checked_mul(512).ok_or_else(overflow)?)?;
		if semantics == TraversalSemantics::Atom {
			admit(graph.edge_count()?.checked_mul(512).ok_or_else(overflow)?)?;
		}
		for id in graph.edge_ids()? {
			check()?;
			let id = id?;
			let edge = graph.edge(id)?.ok_or_else(|| {
				algorithm_failure("traversal index", format!("missing edge {id}"))
			})?;
			let topology = edge.topology()?;
			let accepts = |endpoint: &&EdgeEndpoint| {
				semantics == TraversalSemantics::Atom
					|| endpoint_kind(endpoint) == Some(AtomKind::Node)
			};
			let sources = topology.source().iter().filter(accepts).count();
			let targets = topology.target().iter().filter(accepts).count();
			let nested = topology.endpoints().filter(|endpoint| endpoint.is_cross_level()).count();
			let mut scope_bytes = 0;
			for endpoint in topology.endpoints() {
				if let EdgeEndpoint::Nested(qualified) = endpoint {
					scope_bytes = scope_bytes.max(
						qualified
							.scope()
							.len()
							.checked_mul(size_of::<AtomId>())
							.ok_or_else(overflow)?,
					);
				}
			}
			let pairs = match topology.as_ref() {
				EdgeTopology::Undirected {
					..
				} => sources.checked_mul(sources.saturating_sub(1)),
				EdgeTopology::Directed {
					..
				} => sources.checked_mul(targets),
				EdgeTopology::Bidirectional {
					..
				} => sources.checked_mul(targets).and_then(|pairs| pairs.checked_mul(2)),
			}
			.ok_or_else(overflow)?;
			let endpoint_bytes = scope_bytes
				.checked_mul(4)
				.and_then(|bytes| bytes.checked_add(512))
				.ok_or_else(overflow)?;
			let step_bytes = scope_bytes
				.checked_add(size_of::<TraversalStep>())
				.and_then(|bytes| bytes.checked_mul(8))
				.ok_or_else(overflow)?;
			admit(
				nested
					.checked_mul(endpoint_bytes)
					.and_then(|bytes| {
						pairs.checked_mul(step_bytes).and_then(|steps| bytes.checked_add(steps))
					})
					.ok_or_else(overflow)?,
			)?;
		}
		Self::build(graph, semantics, true, check)
	}

	/// Builds only one indexed edge frontier, retaining the shared incidence
	/// semantics.
	pub(crate) fn for_edge_endpoint<R: GraphRead>(
		graph: &R,
		endpoint: &EdgeEndpoint,
	) -> FerrosGrynnResult<Option<Self>> {
		let Some(connectors) = graph.edge_endpoint_connectors(endpoint)? else {
			return Ok(None);
		};
		let mut index = Self {
			identity: graph.identity(),
			revision: graph.revision(),
			semantics: TraversalSemantics::Atom,
			has_nested_endpoints: false,
			forward: RapidHashMap::default(),
			backward: RapidHashMap::default(),
		};
		let mut count = 0_usize;
		for connector in connectors {
			let edge = graph
				.edge(connector)?
				.ok_or_else(|| algorithm_failure("edge endpoint", "connector is absent"))?;
			let topology = edge.topology()?;
			for direction in [TraversalDirection::Forward, TraversalDirection::Backward] {
				visit_endpoint_steps(
					connector,
					topology.as_ref(),
					endpoint,
					TraversalSemantics::Atom,
					direction,
					|step| {
						if count == 262_144 {
							return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
								"edge endpoint candidates",
								262_144,
							));
						}
						count += 1;
						index.has_nested_endpoints |= step.target.is_cross_level();
						let target = if direction == TraversalDirection::Forward {
							&mut index.forward
						} else {
							&mut index.backward
						};
						target.entry(endpoint.clone()).or_default().push(step);
						Ok(())
					},
				)?;
			}
		}
		Ok(Some(index))
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
		self.breadth_first_with_control(
			root,
			direction,
			max_depth,
			&mut |_| Ok(()),
			&mut || Ok(()),
			&mut || Ok(()),
		)
	}

	/// Reuses the projection BFS with admission and per-branch execution
	/// control.
	///
	/// # Errors
	/// Propagates missing endpoints, capacity, memory and cancellation
	/// failures.
	pub(crate) fn breadth_first_with_control(
		&self,
		root: EdgeEndpoint,
		direction: TraversalDirection,
		max_depth: Option<usize>,
		admit: &mut impl FnMut(usize) -> FerrosGrynnResult<()>,
		check: &mut impl FnMut() -> FerrosGrynnResult<()>,
		visit: &mut impl FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
		check()?;
		self.ensure_endpoint(&root)?;
		let bytes = self
			.forward
			.len()
			.checked_add(self.backward.len())
			.and_then(|count| count.checked_mul(3 * std::mem::size_of::<EdgeEndpoint>() + 96))
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::operation_overflow("projection BFS bytes")
			})?;
		admit(bytes)?;
		let mut visited = RapidHashSet::default();
		let mut queue = VecDeque::from([(root.clone(), 0_usize)]);
		let mut order = Vec::new();
		visited.insert(root);

		while let Some((endpoint, depth)) = queue.pop_front() {
			check()?;
			order.push(endpoint.clone());
			if max_depth.is_some_and(|limit| depth >= limit) {
				continue;
			}
			for step in self.steps(&endpoint, direction) {
				check()?;
				visit()?;
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

/// Visits one endpoint's exact incidence without constructing the other
/// endpoint pairs.
pub fn visit_endpoint_steps(
	edge: AtomId,
	topology: &EdgeTopology,
	endpoint: &EdgeEndpoint,
	semantics: TraversalSemantics,
	direction: TraversalDirection,
	mut visit: impl FnMut(TraversalStep) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	if semantics == TraversalSemantics::Node && endpoint_kind(endpoint) != Some(AtomKind::Node) {
		return Ok(());
	}
	let mut emit = |target: &EdgeEndpoint, direction, role| {
		if semantics == TraversalSemantics::Node && endpoint_kind(target) != Some(AtomKind::Node) {
			return Ok(());
		}
		visit(TraversalStep {
			target: target.clone(),
			via_edge: edge,
			topology: topology.direction(),
			direction,
			role,
		})
	};
	visit_incidence_targets(
		topology.direction(),
		topology.source(),
		topology.target(),
		endpoint,
		direction,
		&mut emit,
	)
}

/// Shares direction and repeated-endpoint rules with ordinal tensor views.
#[allow(
	clippy::redundant_pub_crate,
	reason = "Re-exported at crate scope for exact tensor projection"
)]
pub(crate) fn visit_incidence_targets<T: Eq>(
	topology: EdgeDirection,
	source: &[T],
	target: &[T],
	endpoint: &T,
	direction: TraversalDirection,
	mut emit: impl FnMut(&T, PathDirection, EndpointRole) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	if topology == EdgeDirection::Undirected {
		let occurrences = source.iter().filter(|candidate| *candidate == endpoint).count();
		for target in source {
			for _ in 0..occurrences.saturating_sub(usize::from(target == endpoint)) {
				emit(target, PathDirection::Undirected, EndpointRole::Undirected)?;
			}
		}
		return Ok(());
	}
	for forward in [true, false] {
		if !forward
			&& direction == TraversalDirection::Both
			&& topology == EdgeDirection::Bidirectional
		{
			continue;
		}
		if (forward && direction == TraversalDirection::Backward)
			|| (!forward && direction == TraversalDirection::Forward)
		{
			continue;
		}
		if (forward || topology == EdgeDirection::Bidirectional) && source.contains(endpoint) {
			for target in target {
				emit(target, PathDirection::Forward, EndpointRole::Target)?;
			}
		}
		if (!forward || topology == EdgeDirection::Bidirectional) && target.contains(endpoint) {
			for target in source {
				emit(target, PathDirection::Reverse, EndpointRole::Source)?;
			}
		}
	}
	Ok(())
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn endpoint_kernel_matches_full_incidence_for_every_direction_and_mode() {
		let node = AtomId::from_parts(AtomKind::Node, 1, 1).unwrap();
		let edge = AtomId::from_parts(AtomKind::Edge, 1, 1).unwrap();
		let other = AtomId::from_parts(AtomKind::Edge, 2, 1).unwrap();
		let nested = crate::graph::QualifiedAtomId::new(vec![node], edge).unwrap();
		let sources = [EdgeEndpoint::Local(node), EdgeEndpoint::Local(edge)];
		let targets = [EdgeEndpoint::Local(other), EdgeEndpoint::Nested(nested)];
		for topology in [
			EdgeTopology::directed(sources.clone(), targets.clone()),
			EdgeTopology::bidirectional(sources.clone(), targets.clone()),
			EdgeTopology::undirected(sources.into_iter().chain(targets)),
			EdgeTopology::directed([edge], [edge]),
		] {
			for semantics in [TraversalSemantics::Node, TraversalSemantics::Atom] {
				let mut index = TraversalIndex {
					identity: Uuid::default(),
					revision: 0,
					semantics,
					has_nested_endpoints: false,
					forward: RapidHashMap::default(),
					backward: RapidHashMap::default(),
				};
				index.register_topology(other, &topology);
				for endpoint in topology.endpoints() {
					for direction in [
						TraversalDirection::Forward,
						TraversalDirection::Backward,
						TraversalDirection::Both,
					] {
						let expected =
							index.steps(endpoint, direction).cloned().collect::<Vec<_>>();
						let mut actual = Vec::new();
						visit_endpoint_steps(
							other,
							&topology,
							endpoint,
							semantics,
							direction,
							|step| {
								actual.push(step);
								Ok(())
							},
						)
						.unwrap();
						assert_eq!(
							actual, expected,
							"{topology:?} {endpoint:?} {semantics:?} {direction:?}"
						);
					}
				}
			}
		}
	}
}
