//! Statement-local adjacency for traversal across nested graph occurrences.

use std::{
	collections::{BTreeMap, BTreeSet, VecDeque},
	mem::size_of,
	sync::Arc,
};

use super::*;

/// One connector branch whose endpoint and connector may belong to different
/// graph occurrences.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::gql::execution) struct ScopedTraversalCandidate {
	pub(in crate::gql::execution) target: BoundAtom,
	pub(in crate::gql::execution) edge: BoundAtom,
	pub(in crate::gql::execution) direction: PathDirection,
	pub(in crate::gql::execution) role: EndpointRole,
	topology: EdgeDirection,
	pub(in crate::gql::execution) cross_level: bool,
}

/// Bounded adjacency for all graph occurrences reachable from one MATCH root.
///
/// The index is built only for a linear pattern that can traverse a qualified
/// endpoint. It preserves reverse incidence, which cannot be recovered by
/// scanning only the child graph that owns the current endpoint.
pub(in crate::gql::execution) struct ScopedTraversalIndex {
	forward: BTreeMap<BoundAtom, Vec<ScopedTraversalCandidate>>,
	backward: BTreeMap<BoundAtom, Vec<ScopedTraversalCandidate>>,
	undirected: BTreeMap<BoundAtom, Vec<ScopedTraversalCandidate>>,
	_account: BarrierAccount,
	memory_limit: usize,
}

impl ScopedTraversalIndex {
	/// Builds deterministic scoped adjacency in the statement snapshot.
	pub(in crate::gql::execution) fn build<Read: QueryRead>(
		read: &Read,
		snapshot: Snapshot,
		root: Arc<GraphRef>,
		fields: &MatchGraphFields<'_, Read>,
		semantics: TraversalSemantics,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Self> {
		let mut index = Self {
			forward: BTreeMap::new(),
			backward: BTreeMap::new(),
			undirected: BTreeMap::new(),
			_account: control.barrier_account(),
			memory_limit: control.barrier_memory_limit(),
		};
		let mut build_account = control.barrier_account();
		reserve_graph_scope(&mut build_account, control)?;
		let mut queued = BTreeSet::from([root.id()]);
		let mut queue = VecDeque::from([root]);
		while let Some(reference) = queue.pop_front() {
			control.check()?;
			control.record_match_graph_open();
			let graph = read.graph(snapshot, &reference)?;

			for owner in graph.node_ids()?.chain(graph.edge_ids()?) {
				let owner = owner?;
				if let Some(child) = nested_graph_reference(&graph, owner)? {
					let child = fields.register(child);
					if queued.insert(child.id()) {
						reserve_graph_scope(&mut build_account, control)?;
						queue.push_back(child);
					}
				}
			}

			for edge_id in graph.edge_ids()? {
				let edge_id = edge_id?;
				control.check()?;
				let edge = graph.edge(edge_id)?.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"MATCH scoped-index edge iterator yielded an absent edge",
					)
				})?;
				let topology = edge.topology()?;
				index.register_topology(
					read,
					snapshot,
					fields,
					&reference,
					BoundAtom::new(reference.id(), edge_id),
					&topology,
					semantics,
				)?;
			}
		}
		build_account.release_all();
		Ok(index)
	}

	/// Returns branches accepted by the requested incidence direction.
	pub(in crate::gql::execution) fn candidates(
		&self,
		source: BoundAtom,
		direction: crate::syn::MatchDirection,
	) -> Vec<ScopedTraversalCandidate> {
		let empty: &[ScopedTraversalCandidate] = &[];
		match direction {
			crate::syn::MatchDirection::Forward => {
				self.forward.get(&source).map_or(empty, Vec::as_slice).to_vec()
			}
			crate::syn::MatchDirection::Backward => {
				self.backward.get(&source).map_or(empty, Vec::as_slice).to_vec()
			}
			crate::syn::MatchDirection::Undirected => {
				self.undirected.get(&source).map_or(empty, Vec::as_slice).to_vec()
			}
			crate::syn::MatchDirection::Bidirectional => {
				let mut result = self
					.forward
					.get(&source)
					.map_or(empty, Vec::as_slice)
					.iter()
					.filter(|candidate| candidate.topology == EdgeDirection::Bidirectional)
					.cloned()
					.collect::<Vec<_>>();
				for candidate in self
					.backward
					.get(&source)
					.map_or(empty, Vec::as_slice)
					.iter()
					.filter(|candidate| candidate.topology == EdgeDirection::Bidirectional)
				{
					if !result.contains(candidate) {
						result.push(candidate.clone());
					}
				}
				result
			}
		}
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "topology registration keeps snapshot and graph scope explicit"
	)]
	fn register_topology<Read: QueryRead>(
		&mut self,
		read: &Read,
		snapshot: Snapshot,
		fields: &MatchGraphFields<'_, Read>,
		parent: &Arc<GraphRef>,
		edge: BoundAtom,
		topology: &EdgeTopology,
		semantics: TraversalSemantics,
	) -> FerrosGrynnResult<()> {
		let source = topology
			.source()
			.iter()
			.map(|endpoint| resolve_scoped_endpoint(read, snapshot, fields, parent, endpoint))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let target = topology
			.target()
			.iter()
			.map(|endpoint| resolve_scoped_endpoint(read, snapshot, fields, parent, endpoint))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		match topology.direction() {
			EdgeDirection::Directed => {
				self.connect_sets(
					edge,
					&source,
					&target,
					EdgeDirection::Directed,
					false,
					semantics,
				)?;
			}
			EdgeDirection::Bidirectional => {
				self.connect_sets(
					edge,
					&source,
					&target,
					EdgeDirection::Bidirectional,
					true,
					semantics,
				)?;
			}
			EdgeDirection::Undirected => {
				for (position, left) in source.iter().enumerate() {
					for right in source.iter().skip(position + 1) {
						if accepts_scoped_endpoint(*left, semantics)
							&& accepts_scoped_endpoint(*right, semantics)
						{
							self.push_undirected(*left, *right, edge)?;
							self.push_undirected(*right, *left, edge)?;
						}
					}
				}
			}
		}
		Ok(())
	}

	fn connect_sets(
		&mut self,
		edge: BoundAtom,
		source: &[BoundAtom],
		target: &[BoundAtom],
		topology: EdgeDirection,
		bidirectional: bool,
		semantics: TraversalSemantics,
	) -> FerrosGrynnResult<()> {
		for left in source {
			for right in target {
				if !accepts_scoped_endpoint(*left, semantics)
					|| !accepts_scoped_endpoint(*right, semantics)
				{
					continue;
				}
				self.push_directed(
					*left,
					*right,
					edge,
					topology,
					PathDirection::Forward,
					EndpointRole::Target,
					PathDirection::Reverse,
					EndpointRole::Source,
				)?;
				if bidirectional {
					self.push_directed(
						*right,
						*left,
						edge,
						topology,
						PathDirection::Reverse,
						EndpointRole::Source,
						PathDirection::Forward,
						EndpointRole::Target,
					)?;
				}
			}
		}
		Ok(())
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "forward and reverse path metadata form one scoped incidence pair"
	)]
	fn push_directed(
		&mut self,
		from: BoundAtom,
		to: BoundAtom,
		edge: BoundAtom,
		topology: EdgeDirection,
		forward_direction: PathDirection,
		forward_role: EndpointRole,
		backward_direction: PathDirection,
		backward_role: EndpointRole,
	) -> FerrosGrynnResult<()> {
		let cross_level = from.graph != to.graph;
		self.reserve_candidate()?;
		self.forward.entry(from).or_default().push(ScopedTraversalCandidate {
			target: to,
			edge,
			direction: forward_direction,
			role: forward_role,
			topology,
			cross_level,
		});
		self.reserve_candidate()?;
		self.backward.entry(to).or_default().push(ScopedTraversalCandidate {
			target: from,
			edge,
			direction: backward_direction,
			role: backward_role,
			topology,
			cross_level,
		});
		Ok(())
	}

	fn push_undirected(
		&mut self,
		from: BoundAtom,
		to: BoundAtom,
		edge: BoundAtom,
	) -> FerrosGrynnResult<()> {
		self.reserve_candidate()?;
		self.undirected.entry(from).or_default().push(ScopedTraversalCandidate {
			target: to,
			edge,
			direction: PathDirection::Undirected,
			role: EndpointRole::Undirected,
			topology: EdgeDirection::Undirected,
			cross_level: from.graph != to.graph,
		});
		Ok(())
	}

	fn reserve_candidate(&mut self) -> FerrosGrynnResult<()> {
		let bytes = size_of::<ScopedTraversalCandidate>()
			.saturating_add(size_of::<BoundAtom>())
			.saturating_add(size_of::<usize>().saturating_mul(4));
		if self._account.reserve(bytes) {
			return Ok(());
		}
		Err(FerrosGrynnError::control_limit_exceeded(
			"MATCH scoped adjacency memory",
			u64::try_from(self.memory_limit).unwrap_or(u64::MAX),
		))
	}
}

fn reserve_graph_scope(
	account: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let bytes = size_of::<crate::graph::GraphId>()
		.saturating_add(size_of::<Arc<GraphRef>>())
		.saturating_add(size_of::<usize>().saturating_mul(4));
	if account.reserve(bytes) {
		return Ok(());
	}
	Err(FerrosGrynnError::control_limit_exceeded(
		"MATCH scoped graph cache memory",
		u64::try_from(control.barrier_memory_limit()).unwrap_or(u64::MAX),
	))
}

/// Resolves a local or qualified endpoint and validates every owner in the
/// nested scope.
pub(in crate::gql::execution) fn resolve_scoped_endpoint<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	fields: &MatchGraphFields<'_, Read>,
	parent: &Arc<GraphRef>,
	endpoint: &EdgeEndpoint,
) -> FerrosGrynnResult<BoundAtom> {
	match endpoint {
		EdgeEndpoint::Local(atom) => Ok(BoundAtom::new(parent.id(), *atom)),
		EdgeEndpoint::Nested(qualified) => {
			let mut reference = parent.clone();
			for owner in qualified.scope() {
				let graph = read.graph(snapshot, &reference)?;
				let nested = nested_graph_reference(&graph, *owner)?.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"qualified MATCH endpoint scope is not a meta owner",
					)
				})?;
				reference = fields.register(nested);
			}
			let graph = read.graph(snapshot, &reference)?;
			if !graph.contains(qualified.atom())? {
				return Err(FerrosGrynnError::invalid_storage_file(
					"qualified MATCH endpoint references an absent nested atom",
				));
			}
			Ok(BoundAtom::new(reference.id(), qualified.atom()))
		}
	}
}

fn accepts_scoped_endpoint(endpoint: BoundAtom, semantics: TraversalSemantics) -> bool {
	semantics == TraversalSemantics::Atom || endpoint.atom.kind() == Some(AtomKind::Node)
}
