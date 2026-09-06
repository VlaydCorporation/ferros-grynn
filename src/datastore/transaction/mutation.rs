use rapidhash::RapidHashMap;

#[allow(
	clippy::wildcard_imports,
	reason = "the facade owns imports during the move-only split"
)]
use super::*;

pub(in crate::datastore) struct GraphWrite {
	/// Catalog name published with this graph write.
	pub(super) name: String,
	/// Versioned catalog definition retained across graph-value changes.
	pub(super) schema: GraphSchema,
	pub(super) base: Option<Arc<MaterializedGraph>>,
	pub(super) local: Option<LocalGraphWrite>,
	pub(super) base_ts: u64,
	pub(super) value: Option<MutableGraphValue>,
	/// Explicit table-membership changes keyed by generation-aware atom ID.
	pub(super) memberships: BTreeMap<AtomId, Option<SchemaId>>,
	/// Rebuilds durable secondary indexes even when graph pages are unchanged.
	pub(super) force_persist: bool,
}

pub(in crate::datastore) struct LocalGraphWrite {
	pub(super) base: Arc<LocalGraph>,
	pub(super) snapshot: Arc<LocalReadTransaction>,
	pub(super) delta: LocalGraphDelta,
}

pub(in crate::datastore) struct LocalGraphDelta {
	kind: GraphKind,
	next_node_slot: u64,
	next_edge_slot: u64,
	next_revision: u64,
	records: RapidHashMap<RecordId, Option<AtomId>>,
	nodes: RapidHashMap<AtomId, LocalNodeMutation>,
	edges: RapidHashMap<AtomId, LocalEdgeMutation>,
}

struct LocalNodeMutation {
	record_id: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
	created: bool,
	removed: bool,
	dirty: bool,
}

struct LocalEdgeMutation {
	record_id: RecordId,
	topology: EdgeTopology,
	previous_topology: Option<EdgeTopology>,
	label: Option<Text>,
	properties: PropertyMap,
	weight: Option<crate::values::runtime::Number>,
	created: bool,
	removed: bool,
	dirty: bool,
	topology_changed: bool,
}

impl LocalGraphDelta {
	fn new(kind: GraphKind, next_node_slot: u64, next_edge_slot: u64, next_revision: u64) -> Self {
		Self {
			kind,
			next_node_slot,
			next_edge_slot,
			next_revision,
			records: RapidHashMap::default(),
			nodes: RapidHashMap::default(),
			edges: RapidHashMap::default(),
		}
	}

	fn bump_revision(&mut self) -> FerrosGrynnResult<()> {
		self.next_revision = self
			.next_revision
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("revision".to_owned()))?;
		Ok(())
	}

	fn record_is_live(
		&self,
		base: &LocalGraphView<'_>,
		record_id: &RecordId,
	) -> FerrosGrynnResult<bool> {
		if let Some(id) = self.records.get(record_id) {
			return Ok(id.is_some());
		}
		base.contains_record_id(record_id)
	}

	fn atom_id(
		&mut self,
		base: &LocalGraphView<'_>,
		reference: &AtomRef,
	) -> FerrosGrynnResult<AtomId> {
		if let Some(id) = self.records.get(reference.id()).copied() {
			return id.filter(|id| id.kind() == Some(reference.kind())).ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist(
					reference.kind().to_string(),
					reference.id().to_string(),
				)
			});
		}
		let id = base.atom_id(reference)?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				reference.kind().to_string(),
				reference.id().to_string(),
			)
		})?;
		self.records.insert(reference.id().clone(), Some(id));
		Ok(id)
	}

	fn node_mut<'a>(
		&'a mut self,
		base: &LocalGraphView<'_>,
		reference: &AtomRef,
	) -> FerrosGrynnResult<(AtomId, &'a mut LocalNodeMutation)> {
		let id = self.atom_id(base, reference)?;
		if id.kind() != Some(AtomKind::Node) {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				reference.id().to_string(),
				reference.kind().to_string(),
				AtomKind::Node.to_string(),
			));
		}
		if !self.nodes.contains_key(&id) {
			let node = base.node(id)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist(
					AtomKind::Node.to_string(),
					reference.id().to_string(),
				)
			})?;
			self.nodes.insert(id, LocalNodeMutation {
				record_id: node.record_id()?.into_owned(),
				label: node.label_text()?.map(Cow::into_owned),
				properties: node.properties()?.into_owned(),
				created: false,
				removed: false,
				dirty: false,
			});
		}
		let node = self
			.nodes
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("node mutation state is absent"))?;
		if node.removed {
			return Err(FerrosGrynnError::atom_does_not_exist(
				AtomKind::Node.to_string(),
				reference.id().to_string(),
			));
		}
		Ok((id, node))
	}

	fn edge_mut<'a>(
		&'a mut self,
		base: &LocalGraphView<'_>,
		reference: &AtomRef,
	) -> FerrosGrynnResult<(AtomId, &'a mut LocalEdgeMutation)> {
		let id = self.atom_id(base, reference)?;
		if id.kind() != Some(AtomKind::Edge) {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				reference.id().to_string(),
				reference.kind().to_string(),
				AtomKind::Edge.to_string(),
			));
		}
		if !self.edges.contains_key(&id) {
			let edge = base.edge(id)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist(
					AtomKind::Edge.to_string(),
					reference.id().to_string(),
				)
			})?;
			self.edges.insert(id, LocalEdgeMutation {
				record_id: edge.record_id()?.into_owned(),
				topology: edge.topology()?.into_owned(),
				previous_topology: None,
				label: edge.label_text()?.map(Cow::into_owned),
				properties: edge.properties()?.into_owned(),
				weight: edge.weight(),
				created: false,
				removed: false,
				dirty: false,
				topology_changed: false,
			});
		}
		let edge = self
			.edges
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("edge mutation state is absent"))?;
		if edge.removed {
			return Err(FerrosGrynnError::atom_does_not_exist(
				AtomKind::Edge.to_string(),
				reference.id().to_string(),
			));
		}
		Ok((id, edge))
	}

	fn create_node(
		&mut self,
		base: &LocalGraphView<'_>,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		if matches!(self.kind, GraphKind::Tree | GraphKind::RootedTree) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.kind.name(),
				"an isolated node would violate the single-tree invariant",
			));
		}
		if self.record_is_live(base, &record_id)? {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"record id",
				format!("duplicate RecordId '{record_id}' in one graph"),
			));
		}
		let id = AtomId::from_parts(AtomKind::Node, self.next_node_slot, 0)?;
		let next_node_slot = self
			.next_node_slot
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded(AtomKind::Node.to_string()))?;
		self.next_node_slot = next_node_slot;
		self.bump_revision()?;
		let reference = AtomRef::new(AtomKind::Node, record_id.clone());
		self.records.insert(record_id.clone(), Some(id));
		self.nodes.insert(id, LocalNodeMutation {
			record_id,
			label,
			properties,
			created: true,
			removed: false,
			dirty: true,
		});
		Ok(reference)
	}

	fn create_edge(
		&mut self,
		base: &LocalGraphView<'_>,
		record_id: RecordId,
		topology: &AtomRefTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		if self.record_is_live(base, &record_id)? {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"record id",
				format!("duplicate RecordId '{record_id}' in one graph"),
			));
		}
		let topology = self.resolve_topology(base, topology)?;
		self.validate_topology(base, &topology, None)?;
		let id = AtomId::from_parts(AtomKind::Edge, self.next_edge_slot, 0)?;
		self.next_edge_slot = self
			.next_edge_slot
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded(AtomKind::Edge.to_string()))?;
		self.bump_revision()?;
		let reference = AtomRef::new(AtomKind::Edge, record_id.clone());
		self.records.insert(record_id.clone(), Some(id));
		self.edges.insert(id, LocalEdgeMutation {
			record_id,
			topology,
			previous_topology: None,
			label,
			properties,
			weight: None,
			created: true,
			removed: false,
			dirty: true,
			topology_changed: true,
		});
		Ok(reference)
	}

	fn set_label(
		&mut self,
		base: &LocalGraphView<'_>,
		atom: &AtomRef,
		label: Option<Text>,
	) -> FerrosGrynnResult<Option<Text>> {
		let previous = match atom.kind() {
			AtomKind::Node => {
				let (_id, node) = self.node_mut(base, atom)?;
				node.dirty = true;
				std::mem::replace(&mut node.label, label)
			}
			AtomKind::Edge => {
				let (_id, edge) = self.edge_mut(base, atom)?;
				edge.dirty = true;
				std::mem::replace(&mut edge.label, label)
			}
		};
		self.bump_revision()?;
		Ok(previous)
	}

	fn set_property(
		&mut self,
		base: &LocalGraphView<'_>,
		atom: &AtomRef,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<Option<Value>> {
		let previous = match atom.kind() {
			AtomKind::Node => {
				let (_id, node) = self.node_mut(base, atom)?;
				node.dirty = true;
				node.properties.insert(key, value)
			}
			AtomKind::Edge => {
				let (_id, edge) = self.edge_mut(base, atom)?;
				edge.dirty = true;
				edge.properties.insert(key, value)
			}
		};
		self.bump_revision()?;
		Ok(previous)
	}

	fn remove_property(
		&mut self,
		base: &LocalGraphView<'_>,
		atom: &AtomRef,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		let previous = match atom.kind() {
			AtomKind::Node => {
				let (_id, node) = self.node_mut(base, atom)?;
				node.dirty = true;
				node.properties.remove(key)
			}
			AtomKind::Edge => {
				let (_id, edge) = self.edge_mut(base, atom)?;
				edge.dirty = true;
				edge.properties.remove(key)
			}
		};
		self.bump_revision()?;
		Ok(previous)
	}

	fn replace_incidence(
		&mut self,
		base: &LocalGraphView<'_>,
		edge: &AtomRef,
		topology: &AtomRefTopology,
	) -> FerrosGrynnResult<()> {
		if edge.kind() != AtomKind::Edge {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				edge.id().to_string(),
				edge.kind().to_string(),
				AtomKind::Edge.to_string(),
			));
		}
		if matches!(self.kind, GraphKind::Tree | GraphKind::RootedTree | GraphKind::Forest) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.kind.name(),
				"generic incidence replacement cannot preserve tree parent invariants",
			));
		}
		let id = self.atom_id(base, edge)?;
		let topology = self.resolve_topology(base, topology)?;
		self.validate_topology(base, &topology, Some(id))?;
		let (_id, state) = self.edge_mut(base, edge)?;
		if state.previous_topology.is_none() {
			state.previous_topology = Some(state.topology.clone());
		}
		state.topology = topology;
		state.topology_changed = true;
		state.dirty = true;
		self.bump_revision()
	}

	fn remove_atom(&mut self, base: &LocalGraphView<'_>, atom: &AtomRef) -> FerrosGrynnResult<()> {
		let id = self.atom_id(base, atom)?;
		if matches!(self.kind, GraphKind::Tree | GraphKind::RootedTree) {
			self.validate_tree_removal(base, id)?;
		}
		let dependent = self.dependent_edges(base, id)?;
		for edge in dependent {
			self.mark_edge_removed(base, edge)?;
		}
		match id.kind() {
			Some(AtomKind::Node) => {
				if !self.nodes.contains_key(&id) {
					let _ = self.node_mut(base, atom)?;
				}
				let node = self.nodes.get_mut(&id).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("node mutation state is absent")
				})?;
				node.removed = true;
				node.dirty = true;
				self.records.insert(node.record_id.clone(), None);
			}
			Some(AtomKind::Edge) => self.mark_edge_removed(base, id)?,
			None => {
				return Err(FerrosGrynnError::invalid_argument("invalid AtomId kind"));
			}
		}
		self.bump_revision()
	}

	fn mark_edge_removed(
		&mut self,
		base: &LocalGraphView<'_>,
		id: AtomId,
	) -> FerrosGrynnResult<()> {
		if !self.edges.contains_key(&id) {
			let edge = base.edge(id)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist(AtomKind::Edge.to_string(), id.to_string())
			})?;
			self.edges.insert(id, LocalEdgeMutation {
				record_id: edge.record_id()?.into_owned(),
				topology: edge.topology()?.into_owned(),
				previous_topology: None,
				label: edge.label_text()?.map(Cow::into_owned),
				properties: edge.properties()?.into_owned(),
				weight: edge.weight(),
				created: false,
				removed: false,
				dirty: false,
				topology_changed: false,
			});
		}
		let edge = self
			.edges
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("edge mutation state is absent"))?;
		edge.removed = true;
		edge.dirty = true;
		self.records.insert(edge.record_id.clone(), None);
		Ok(())
	}

	fn resolve_topology(
		&mut self,
		base: &LocalGraphView<'_>,
		topology: &AtomRefTopology,
	) -> FerrosGrynnResult<EdgeTopology> {
		let mut resolve =
			|reference: &AtomRef| self.atom_id(base, reference).map(EdgeEndpoint::Local);
		Ok(match topology {
			AtomRefTopology::Directed {
				source,
				target,
			} => EdgeTopology::directed(
				source.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
				target.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			AtomRefTopology::Bidirectional {
				source,
				target,
			} => EdgeTopology::bidirectional(
				source.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
				target.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			AtomRefTopology::Undirected {
				endpoints,
			} => EdgeTopology::undirected(
				endpoints.iter().map(resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
		})
	}

	fn validate_topology(
		&self,
		base: &LocalGraphView<'_>,
		topology: &EdgeTopology,
		ignored_edge: Option<AtomId>,
	) -> FerrosGrynnResult<()> {
		if topology.endpoint_count() == 0 {
			return Err(FerrosGrynnError::edge_creation_error()
				.with_cause(FerrosGrynnError::empty_edge_endpoints()));
		}
		for group in [topology.source(), topology.target()] {
			let mut seen = BTreeSet::new();
			for endpoint in group {
				if !seen.insert(endpoint) {
					return Err(FerrosGrynnError::edge_creation_error().with_cause(
						FerrosGrynnError::graph_constraint_violation(
							self.kind.name(),
							"an incidence role cannot repeat the same endpoint",
						),
					));
				}
			}
		}
		if matches!(topology, EdgeTopology::Bidirectional { .. }) && topology.endpoint_count() != 2
		{
			return Err(FerrosGrynnError::invalid_argument(
				"bidirectional hyperedges require a format extension",
			));
		}
		match self.kind {
			GraphKind::Dag => {
				let Some((source, target, EdgeDirection::Directed)) = topology.local_binary()
				else {
					return Err(FerrosGrynnError::graph_constraint_violation(
						GraphKind::Dag.name(),
						"DAG edges must be directed binary node-to-node edges",
					));
				};
				if source == target || self.reachable(base, target, source, ignored_edge)? {
					return Err(FerrosGrynnError::graph_constraint_violation(
						GraphKind::Dag.name(),
						"the edge would create a directed cycle",
					));
				}
			}
			GraphKind::Forest => {
				let orientation = self.forest_orientation(base, Some(topology), ignored_edge)?;
				let (parent, child) = forest_pair(topology, orientation)?;
				if parent == child
					|| self.forest_parent(base, child, orientation, ignored_edge)?.is_some()
					|| self.forest_component_root(base, parent, orientation, ignored_edge)? == child
				{
					return Err(FerrosGrynnError::graph_constraint_violation(
						GraphKind::Forest.name(),
						"the edge would attach a non-root child or create a cycle",
					));
				}
			}
			GraphKind::Tree | GraphKind::RootedTree => {
				return Err(FerrosGrynnError::graph_constraint_violation(
					self.kind.name(),
					"use the family materialization API to add a child and edge atomically",
				));
			}
			GraphKind::Graph
			| GraphKind::Hypergraph
			| GraphKind::Metagraph
			| GraphKind::Subgraph => {}
		}
		Ok(())
	}

	fn effective_adjacency(
		&self,
		base: &LocalGraphView<'_>,
		node: AtomId,
	) -> FerrosGrynnResult<crate::storage::LocalNodeAdjacency> {
		let mut adjacency = if self.nodes.get(&node).is_some_and(|state| state.created) {
			crate::storage::LocalNodeAdjacency {
				outgoing: Vec::new(),
				incoming: Vec::new(),
				undirected: Vec::new(),
			}
		} else {
			base.node_adjacency(node)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist(AtomKind::Node.to_string(), node.to_string())
			})?
		};
		for (edge_id, edge) in &self.edges {
			if !edge.dirty || (!edge.created && !edge.removed && !edge.topology_changed) {
				continue;
			}
			for role in
				[&mut adjacency.outgoing, &mut adjacency.incoming, &mut adjacency.undirected]
			{
				role.retain(|candidate| candidate != edge_id);
			}
			if edge.removed {
				continue;
			}
			append_adjacency_roles(&mut adjacency, node, *edge_id, &edge.topology);
		}
		adjacency.outgoing.sort_unstable();
		adjacency.incoming.sort_unstable();
		adjacency.undirected.sort_unstable();
		Ok(adjacency)
	}

	fn effective_edge_ids(&self, base: &LocalGraphView<'_>) -> FerrosGrynnResult<Vec<AtomId>> {
		let mut ids = base.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
		ids.extend(self.edges.keys().copied());
		ids.sort_unstable();
		ids.dedup();
		Ok(ids)
	}

	fn effective_topology(
		&self,
		base: &LocalGraphView<'_>,
		edge: AtomId,
	) -> FerrosGrynnResult<Option<EdgeTopology>> {
		if let Some(state) = self.edges.get(&edge) {
			return Ok((!state.removed).then(|| state.topology.clone()));
		}
		base.edge(edge)?.map(|edge| edge.topology().map(Cow::into_owned)).transpose()
	}

	fn reachable(
		&self,
		base: &LocalGraphView<'_>,
		start: AtomId,
		goal: AtomId,
		ignored_edge: Option<AtomId>,
	) -> FerrosGrynnResult<bool> {
		let mut pending = vec![start];
		let mut seen = BTreeSet::new();
		while let Some(node) = pending.pop() {
			if node == goal {
				return Ok(true);
			}
			if !seen.insert(node) {
				continue;
			}
			for edge in self.effective_adjacency(base, node)?.outgoing {
				if Some(edge) == ignored_edge {
					continue;
				}
				let Some(topology) = self.effective_topology(base, edge)? else {
					continue;
				};
				if let Some((source, target, EdgeDirection::Directed)) = topology.local_binary()
					&& source == node
				{
					pending.push(target);
				}
			}
		}
		Ok(false)
	}

	fn forest_orientation(
		&self,
		base: &LocalGraphView<'_>,
		candidate: Option<&EdgeTopology>,
		ignored_edge: Option<AtomId>,
	) -> FerrosGrynnResult<TreeOrientation> {
		let mut inbound = RapidHashMap::<AtomId, u32>::default();
		let mut outbound = RapidHashMap::<AtomId, u32>::default();
		let mut direction = None;
		let topologies = self
			.effective_edge_ids(base)?
			.into_iter()
			.filter(|edge| Some(*edge) != ignored_edge)
			.map(|edge| self.effective_topology(base, edge))
			.collect::<FerrosGrynnResult<Vec<_>>>()?
			.into_iter()
			.flatten()
			.chain(candidate.cloned());
		for topology in topologies {
			let Some((left, right, edge_direction)) = topology.local_binary() else {
				return Err(FerrosGrynnError::graph_constraint_violation(
					GraphKind::Forest.name(),
					"forest edges must be local binary node-to-node edges",
				));
			};
			if edge_direction == EdgeDirection::Bidirectional
				|| direction.is_some_and(|expected| expected != edge_direction)
			{
				return Err(FerrosGrynnError::graph_constraint_violation(
					GraphKind::Forest.name(),
					"a forest cannot mix edge orientations",
				));
			}
			direction = Some(edge_direction);
			if edge_direction == EdgeDirection::Directed {
				*outbound.entry(left).or_default() += 1;
				*inbound.entry(right).or_default() += 1;
			}
		}
		match direction {
			None | Some(EdgeDirection::Undirected) => Ok(TreeOrientation::Undirected),
			Some(EdgeDirection::Directed) if inbound.values().all(|degree| *degree <= 1) => {
				Ok(TreeOrientation::OutArborescence)
			}
			Some(EdgeDirection::Directed) if outbound.values().all(|degree| *degree <= 1) => {
				Ok(TreeOrientation::InArborescence)
			}
			Some(EdgeDirection::Directed | EdgeDirection::Bidirectional) => {
				Err(FerrosGrynnError::graph_constraint_violation(
					GraphKind::Forest.name(),
					"directed edges do not form consistently oriented arborescences",
				))
			}
		}
	}

	fn forest_parent(
		&self,
		base: &LocalGraphView<'_>,
		node: AtomId,
		orientation: TreeOrientation,
		ignored_edge: Option<AtomId>,
	) -> FerrosGrynnResult<Option<AtomId>> {
		let adjacency = self.effective_adjacency(base, node)?;
		for edge in
			adjacency.outgoing.into_iter().chain(adjacency.incoming).chain(adjacency.undirected)
		{
			if Some(edge) == ignored_edge {
				continue;
			}
			let Some(topology) = self.effective_topology(base, edge)? else {
				continue;
			};
			let (parent, child) = forest_pair(&topology, orientation)?;
			if child == node {
				return Ok(Some(parent));
			}
		}
		Ok(None)
	}

	fn forest_component_root(
		&self,
		base: &LocalGraphView<'_>,
		mut node: AtomId,
		orientation: TreeOrientation,
		ignored_edge: Option<AtomId>,
	) -> FerrosGrynnResult<AtomId> {
		let mut seen = BTreeSet::new();
		while seen.insert(node) {
			let Some(parent) = self.forest_parent(base, node, orientation, ignored_edge)? else {
				return Ok(node);
			};
			node = parent;
		}
		Err(FerrosGrynnError::graph_constraint_violation(
			GraphKind::Forest.name(),
			"the stored forest contains a cycle",
		))
	}

	fn validate_tree_removal(
		&self,
		base: &LocalGraphView<'_>,
		id: AtomId,
	) -> FerrosGrynnResult<()> {
		if id.kind() != Some(AtomKind::Node) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.kind.name(),
				"only a leaf vertex can be removed without breaking the tree invariant",
			));
		}
		let orientation = self.forest_orientation(base, None, None)?;
		let adjacency = self.effective_adjacency(base, id)?;
		for edge in
			adjacency.outgoing.into_iter().chain(adjacency.incoming).chain(adjacency.undirected)
		{
			let Some(topology) = self.effective_topology(base, edge)? else {
				continue;
			};
			let (parent, _child) = forest_pair(&topology, orientation)?;
			if parent == id {
				return Err(FerrosGrynnError::graph_constraint_violation(
					self.kind.name(),
					"only a leaf vertex can be removed without breaking the tree invariant",
				));
			}
		}
		let created = self.nodes.values().filter(|node| node.created && !node.removed).count();
		let removed = self.nodes.values().filter(|node| node.removed && !node.created).count();
		let live = base.node_count()?.saturating_add(created).saturating_sub(removed);
		if live <= 1 {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.kind.name(),
				"a tree cannot become empty",
			));
		}
		if self.kind == GraphKind::RootedTree
			&& self.forest_parent(base, id, orientation, None)?.is_none()
		{
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.kind.name(),
				"the root vertex cannot be removed",
			));
		}
		Ok(())
	}

	fn dependent_edges(
		&self,
		base: &LocalGraphView<'_>,
		id: AtomId,
	) -> FerrosGrynnResult<BTreeSet<AtomId>> {
		if matches!(
			self.kind,
			GraphKind::Dag | GraphKind::Forest | GraphKind::Tree | GraphKind::RootedTree
		) {
			if id.kind() == Some(AtomKind::Edge) {
				return Ok(BTreeSet::from([id]));
			}
			let adjacency = self.effective_adjacency(base, id)?;
			return Ok(adjacency
				.outgoing
				.into_iter()
				.chain(adjacency.incoming)
				.chain(adjacency.undirected)
				.collect());
		}
		let mut removed = BTreeSet::from([id]);
		let mut edges = BTreeSet::new();
		loop {
			let mut changed = false;
			for edge in self.effective_edge_ids(base)? {
				if edges.contains(&edge) {
					continue;
				}
				let Some(topology) = self.effective_topology(base, edge)? else {
					continue;
				};
				let dependent = topology.endpoints().any(|endpoint| match endpoint {
					EdgeEndpoint::Local(atom) => removed.contains(atom),
					EdgeEndpoint::Nested(qualified) => qualified
						.scope()
						.first()
						.is_some_and(|container| removed.contains(container)),
				});
				if dependent {
					edges.insert(edge);
					removed.insert(edge);
					changed = true;
				}
			}
			if !changed {
				break;
			}
		}
		Ok(edges)
	}

	pub(super) fn dirty(&self) -> impl ExactSizeIterator<Item = DirtyTarget> + '_ {
		let mut dirty = self
			.nodes
			.iter()
			.filter(|(_id, node)| node.dirty)
			.map(|(id, _node)| DirtyTarget::Atom(*id))
			.chain(
				self.edges
					.iter()
					.filter(|(_id, edge)| edge.dirty)
					.map(|(id, _edge)| DirtyTarget::Atom(*id)),
			)
			.collect::<Vec<_>>();
		for edge in self.edges.values().filter(|edge| edge.dirty) {
			for topology in edge.previous_topology.iter().chain(std::iter::once(&edge.topology)) {
				for endpoint in topology.endpoints() {
					if let EdgeEndpoint::Local(id) = endpoint {
						dirty.push(DirtyTarget::Atom(*id));
					}
				}
			}
		}
		dirty.sort_unstable();
		dirty.dedup();
		dirty.into_iter()
	}

	pub(super) fn estimated_write_set_bytes(
		&self,
		base: &LocalGraphView<'_>,
	) -> FerrosGrynnResult<usize> {
		let nodes = self.nodes.iter().map(|(_id, node)| {
			DIRTY_TARGET_WRITE_BYTES
				.saturating_add(std::mem::size_of::<LocalNodeMutation>())
				.saturating_add(node.record_id.estimated_heap_bytes())
				.saturating_add(node.label.as_ref().map_or(0, Text::len))
				.saturating_add(node.properties.estimated_heap_bytes())
		});
		let edges = self.edges.iter().map(|(_id, edge)| {
			DIRTY_TARGET_WRITE_BYTES
				.saturating_add(std::mem::size_of::<LocalEdgeMutation>())
				.saturating_add(edge.record_id.estimated_heap_bytes())
				.saturating_add(edge.label.as_ref().map_or(0, Text::len))
				.saturating_add(edge.properties.estimated_heap_bytes())
				.saturating_add(crate::graph::estimated_topology_heap_bytes(&edge.topology))
				.saturating_add(
					edge.previous_topology
						.as_ref()
						.map_or(0, crate::graph::estimated_topology_heap_bytes),
				)
		});
		let records = self
			.records
			.keys()
			.map(RecordId::estimated_heap_bytes)
			.fold(0_usize, usize::saturating_add);
		let mut estimated = nodes.chain(edges).fold(records, usize::saturating_add);
		for id in self.affected_adjacency_nodes() {
			if self.nodes.get(&id).is_some_and(|node| node.removed) {
				continue;
			}
			let adjacency = self.effective_adjacency(base, id)?;
			estimated = estimated
				.saturating_add(std::mem::size_of::<crate::storage::LocalCommitNodeAdjacency>())
				.saturating_add(
					adjacency
						.outgoing
						.len()
						.saturating_add(adjacency.incoming.len())
						.saturating_add(adjacency.undirected.len())
						.saturating_mul(std::mem::size_of::<AtomId>()),
				);
		}
		Ok(estimated)
	}

	fn affected_adjacency_nodes(&self) -> BTreeSet<AtomId> {
		let mut affected = self
			.nodes
			.iter()
			.filter(|(_id, node)| node.created && !node.removed)
			.map(|(id, _node)| *id)
			.collect::<BTreeSet<_>>();
		for edge in self.edges.values().filter(|edge| edge.dirty) {
			for topology in edge.previous_topology.iter().chain(std::iter::once(&edge.topology)) {
				for endpoint in topology.endpoints() {
					if let EdgeEndpoint::Local(id) = endpoint
						&& id.kind() == Some(AtomKind::Node)
					{
						affected.insert(*id);
					}
				}
			}
		}
		affected
	}

	/// Revalidates topology-sensitive mutations against the commit-time graph
	/// view.
	pub(super) fn validate_against(&self, base: &LocalGraphView<'_>) -> FerrosGrynnResult<()> {
		if !matches!(self.kind, GraphKind::Dag | GraphKind::Forest) {
			return Ok(());
		}
		for (id, edge) in self.edges.iter().filter(|(_id, edge)| {
			edge.dirty && !edge.removed && (edge.created || edge.topology_changed)
		}) {
			self.validate_topology(base, &edge.topology, Some(*id))?;
		}
		Ok(())
	}

	/// Returns a changed atom's final record, properties and removal state.
	pub(super) fn changed_atom(&self, id: AtomId) -> Option<(&RecordId, &PropertyMap, bool)> {
		match id.kind() {
			Some(AtomKind::Node) => self
				.nodes
				.get(&id)
				.filter(|node| node.dirty)
				.map(|node| (&node.record_id, &node.properties, node.removed)),
			Some(AtomKind::Edge) => self
				.edges
				.get(&id)
				.filter(|edge| edge.dirty)
				.map(|edge| (&edge.record_id, &edge.properties, edge.removed)),
			None => None,
		}
	}

	pub(super) fn is_empty(&self) -> bool {
		!self.nodes.values().any(|node| node.dirty) && !self.edges.values().any(|edge| edge.dirty)
	}

	pub(super) fn changes_node_membership(&self) -> bool {
		self.nodes.values().any(|node| node.dirty && (node.created || node.removed))
	}

	pub(super) fn changes_edge_membership(&self) -> bool {
		self.edges.values().any(|edge| edge.dirty && (edge.created || edge.removed))
	}

	pub(super) fn removed_atoms(&self) -> impl Iterator<Item = AtomId> + '_ {
		let nodes =
			self.nodes.iter().filter_map(|(id, node)| (node.dirty && node.removed).then_some(*id));
		let edges =
			self.edges.iter().filter_map(|(id, edge)| (edge.dirty && edge.removed).then_some(*id));
		nodes.chain(edges)
	}

	pub(super) fn into_commit_nodes(self) -> Vec<crate::storage::LocalCommitNode> {
		let mut nodes = self
			.nodes
			.into_iter()
			.filter(|(_id, node)| node.dirty && node.created && !node.removed)
			.map(|(id, node)| crate::storage::LocalCommitNode {
				id,
				record_id: node.record_id,
				label: node.label,
				properties: node.properties,
				table_id: None,
			})
			.collect::<Vec<_>>();
		nodes.sort_unstable_by_key(|node| node.id);
		nodes
	}

	pub(super) fn is_node_append(&self) -> bool {
		!self.edges.values().any(|edge| edge.dirty)
			&& self.nodes.values().all(|node| !node.dirty || (node.created && !node.removed))
	}

	pub(super) fn into_commit_delta(
		self,
		base: &LocalGraphView<'_>,
		memberships: Vec<crate::storage::LocalCommitTableMembership>,
	) -> FerrosGrynnResult<crate::storage::LocalCommitAtomDelta> {
		let affected_nodes = self.affected_adjacency_nodes();
		let mut adjacency = Vec::with_capacity(affected_nodes.len());
		for id in affected_nodes {
			if self.nodes.get(&id).is_some_and(|node| node.removed) {
				continue;
			}
			let roles = self.effective_adjacency(base, id)?;
			adjacency.push(crate::storage::LocalCommitNodeAdjacency {
				id,
				outgoing: roles.outgoing,
				incoming: roles.incoming,
				undirected: roles.undirected,
			});
		}
		let mut nodes = self
			.nodes
			.into_iter()
			.filter(|(_id, node)| node.dirty)
			.map(|(id, node)| {
				if node.removed {
					crate::storage::LocalCommitNodeChange::Remove {
						id,
						created: node.created,
					}
				} else {
					crate::storage::LocalCommitNodeChange::Upsert {
						id,
						record_id: node.record_id,
						label: node.label,
						properties: node.properties,
						created: node.created,
					}
				}
			})
			.collect::<Vec<_>>();
		nodes.sort_unstable_by_key(|node| match node {
			crate::storage::LocalCommitNodeChange::Upsert {
				id,
				..
			}
			| crate::storage::LocalCommitNodeChange::Remove {
				id,
				..
			} => *id,
		});
		let mut edges = self
			.edges
			.into_iter()
			.filter(|(_id, edge)| edge.dirty)
			.map(|(id, edge)| {
				if edge.removed {
					crate::storage::LocalCommitEdgeChange::Remove {
						id,
						created: edge.created,
					}
				} else {
					crate::storage::LocalCommitEdgeChange::Upsert {
						id,
						record_id: edge.record_id,
						topology: edge.topology,
						label: edge.label,
						properties: edge.properties,
						weight: edge.weight,
						created: edge.created,
						topology_changed: edge.topology_changed,
					}
				}
			})
			.collect::<Vec<_>>();
		edges.sort_unstable_by_key(|edge| match edge {
			crate::storage::LocalCommitEdgeChange::Upsert {
				id,
				..
			}
			| crate::storage::LocalCommitEdgeChange::Remove {
				id,
				..
			} => *id,
		});
		Ok(crate::storage::LocalCommitAtomDelta {
			revision: self.next_revision,
			node_capacity: self.next_node_slot,
			edge_capacity: self.next_edge_slot,
			nodes,
			edges,
			adjacency,
			memberships,
		})
	}

	fn apply_to(&self, value: &mut MutableGraphValue) -> FerrosGrynnResult<()> {
		for node in self.nodes.values().filter(|node| node.dirty && node.removed && !node.created) {
			let reference = AtomRef::new(AtomKind::Node, node.record_id.clone());
			if value.atom_id(&reference).is_ok() {
				value.remove_atom(&reference)?;
			}
		}
		for edge in self.edges.values().filter(|edge| edge.dirty && edge.removed && !edge.created) {
			let reference = AtomRef::new(AtomKind::Edge, edge.record_id.clone());
			if value.atom_id(&reference).is_ok() {
				value.remove_atom(&reference)?;
			}
		}
		for node in self.nodes.values().filter(|node| node.dirty && !node.removed) {
			let reference = AtomRef::new(AtomKind::Node, node.record_id.clone());
			if node.created {
				value.create_node(
					node.record_id.clone(),
					node.label.clone(),
					node.properties.clone(),
				)?;
			} else {
				value.set_atom_label(&reference, node.label.clone())?;
				*value.atom_properties_mut(&reference)? = node.properties.clone();
			}
		}
		for edge in self.edges.values().filter(|edge| edge.dirty && !edge.removed) {
			let reference = AtomRef::new(AtomKind::Edge, edge.record_id.clone());
			if edge.created {
				let topology = self.topology_as_refs(value, &edge.topology)?;
				value.create_edge(
					edge.record_id.clone(),
					&topology,
					edge.label.clone(),
					edge.properties.clone(),
				)?;
			} else {
				if edge.topology_changed {
					let topology = self.topology_as_refs(value, &edge.topology)?;
					value.replace_incidence(&reference, &topology)?;
				}
				value.set_atom_label(&reference, edge.label.clone())?;
				*value.atom_properties_mut(&reference)? = edge.properties.clone();
			}
		}
		Ok(())
	}

	fn topology_as_refs(
		&self,
		value: &MutableGraphValue,
		topology: &EdgeTopology,
	) -> FerrosGrynnResult<AtomRefTopology> {
		let mut resolve = |endpoint: &EdgeEndpoint| match endpoint {
			EdgeEndpoint::Local(id) => self.atom_ref_for_snapshot(value, *id),
			EdgeEndpoint::Nested(_) => Err(FerrosGrynnError::invalid_argument(
				"transaction snapshot cannot remap a nested point-mutation endpoint",
			)),
		};
		Ok(match topology {
			EdgeTopology::Directed {
				source,
				target,
			} => AtomRefTopology::Directed {
				source: source.iter().map(&mut resolve).collect::<FerrosGrynnResult<_>>()?,
				target: target.iter().map(&mut resolve).collect::<FerrosGrynnResult<_>>()?,
			},
			EdgeTopology::Bidirectional {
				source,
				target,
			} => AtomRefTopology::Bidirectional {
				source: source.iter().map(&mut resolve).collect::<FerrosGrynnResult<_>>()?,
				target: target.iter().map(&mut resolve).collect::<FerrosGrynnResult<_>>()?,
			},
			EdgeTopology::Undirected {
				endpoints,
			} => AtomRefTopology::Undirected {
				endpoints: endpoints.iter().map(resolve).collect::<FerrosGrynnResult<_>>()?,
			},
		})
	}

	fn atom_ref_for_snapshot(
		&self,
		value: &MutableGraphValue,
		id: AtomId,
	) -> FerrosGrynnResult<AtomRef> {
		if let Some(node) = self.nodes.get(&id) {
			return Ok(AtomRef::new(AtomKind::Node, node.record_id.clone()));
		}
		if let Some(edge) = self.edges.get(&id) {
			return Ok(AtomRef::new(AtomKind::Edge, edge.record_id.clone()));
		}
		value
			.atom_ref(id)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("endpoint", id.to_string()))
	}
}

impl GraphWrite {
	#[cfg(test)]
	pub(in crate::datastore) const fn is_delta_backed(&self) -> bool {
		self.local.is_some() && self.value.is_none()
	}

	pub(super) const fn exists(&self) -> bool {
		self.value.is_some() || self.local.is_some()
	}

	pub(super) const fn is_created(&self) -> bool {
		self.base.is_none() && self.local.is_none()
	}

	pub(super) fn kind(&self) -> Option<GraphKind> {
		self.value
			.as_ref()
			.map(MutableGraphValue::kind)
			.or_else(|| self.local.as_ref().map(|local| local.delta.kind))
			.or_else(|| self.base.as_ref().map(|base| base.kind()))
	}

	pub(super) fn snapshot(&self, store: &Datastore) -> FerrosGrynnResult<MaterializedGraph> {
		if let Some(value) = &self.value {
			return Ok(value.snapshot());
		}
		let local = self
			.local
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(self.name.clone()))?;
		let storage = store.inner.backend.local().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"local graph is attached to an in-memory backend",
			)
		})?;
		let base = storage.materialize_graph(local.snapshot.clone(), &local.base)?;
		let mut value = MutableGraphValue::from_frozen(&base)?;
		local.delta.apply_to(&mut value)?;
		Ok(value.snapshot())
	}

	fn atom_id(&mut self, store: &Datastore, reference: &AtomRef) -> FerrosGrynnResult<AtomId> {
		if let Some(value) = &self.value {
			return value.atom_id(reference);
		}
		let local = self
			.local
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(self.name.clone()))?;
		let storage = store.inner.backend.local().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"local graph is attached to an in-memory backend",
			)
		})?;
		let view = storage.graph_view(local.snapshot.clone(), local.base.clone());
		local.delta.atom_id(&view, reference)
	}

	pub(super) fn ensure_materialized(&mut self, store: &Datastore) -> FerrosGrynnResult<()> {
		let Some(local) = self.local.as_ref() else {
			return Ok(());
		};
		let storage = store.inner.backend.local().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"local graph is attached to an in-memory backend",
			)
		})?;
		let base = Arc::new(storage.materialize_graph(local.snapshot.clone(), &local.base)?);
		let mut value = MutableGraphValue::from_frozen(&base)?;
		local.delta.apply_to(&mut value)?;
		self.base = Some(base);
		self.value = Some(value);
		self.local = None;
		Ok(())
	}
}

pub(super) enum MutableGraphValue {
	Graph(Graph<Mutable>),
	Tree(Tree<Mutable>),
	RootedTree(RootedTree<Mutable>),
	Forest(Forest<Mutable>),
	Dag(Dag<Mutable>),
	Hypergraph(Hypergraph<Mutable>),
	Metagraph(Metagraph<Mutable>),
}

/// Endpoint incidence addressed by stable public atom references.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AtomRefTopology {
	/// Directed incidence from `source` to `target`.
	Directed {
		/// Source endpoints.
		source: Vec<AtomRef>,
		/// Target endpoints.
		target: Vec<AtomRef>,
	},
	/// Bidirectional incidence between the source and target groups.
	Bidirectional {
		/// First endpoint group.
		source: Vec<AtomRef>,
		/// Second endpoint group.
		target: Vec<AtomRef>,
	},
	/// Undirected incidence over one endpoint group.
	Undirected {
		/// Unordered endpoints.
		endpoints: Vec<AtomRef>,
	},
}

/// Backend-neutral transaction-private point-mutation view.
///
/// Atom identity crosses the API boundary only as [`AtomRef`]. Local operations
/// retain an atom delta; this API never silently materializes a local graph.
pub struct GraphMutationView<'a> {
	store: Datastore,
	kind: GraphKind,
	write: &'a mut GraphWrite,
}

impl GraphMutationView<'_> {
	#[cfg(test)]
	pub(in crate::datastore) const fn is_delta_backed(&self) -> bool {
		self.write.is_delta_backed()
	}

	/// Returns the structural family of the graph being changed.
	pub const fn kind(&self) -> GraphKind {
		self.kind
	}

	fn materialized(&mut self) -> FerrosGrynnResult<&mut MutableGraphValue> {
		self.write.ensure_materialized(&self.store)?;
		self.write
			.value
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(self.write.name.clone()))
	}

	/// Creates a node with a caller-selected stable record ID.
	///
	/// # Errors
	///
	/// Returns a graph constraint error for a duplicate record ID or for graph
	/// families that cannot add an isolated node without violating invariants.
	pub fn create_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.create_node(&base, record_id, label, properties);
		}
		self.materialized()?.create_node(record_id, label, properties)
	}

	/// Creates an edge with stable record IDs for both the edge and endpoints.
	///
	/// # Errors
	///
	/// Returns an error when an endpoint is absent, the record ID is already in
	/// use, or the incidence violates the graph-family constraints.
	pub fn create_edge(
		&mut self,
		record_id: RecordId,
		topology: &AtomRefTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.create_edge(&base, record_id, topology, label, properties);
		}
		self.materialized()?.create_edge(record_id, topology, label, properties)
	}

	/// Replaces an atom label and returns the previous value.
	///
	/// # Errors
	///
	/// Returns an error when the referenced atom does not exist.
	pub fn set_label(
		&mut self,
		atom: &AtomRef,
		label: Option<Text>,
	) -> FerrosGrynnResult<Option<Text>> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.set_label(&base, atom, label);
		}
		self.materialized()?.set_atom_label(atom, label)
	}

	/// Inserts or replaces one atom property and returns the previous value.
	///
	/// # Errors
	///
	/// Returns an error when the referenced atom does not exist.
	pub fn set_property(
		&mut self,
		atom: &AtomRef,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<Option<Value>> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.set_property(&base, atom, key, value);
		}
		Ok(self.materialized()?.atom_properties_mut(atom)?.insert(key, value))
	}

	/// Removes one atom property and returns its previous value when present.
	///
	/// # Errors
	///
	/// Returns an error when the referenced atom does not exist.
	pub fn remove_property(
		&mut self,
		atom: &AtomRef,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.remove_property(&base, atom, key);
		}
		Ok(self.materialized()?.atom_properties_mut(atom)?.remove(key))
	}

	/// Replaces the complete incidence of an existing edge.
	///
	/// # Errors
	///
	/// Returns an error when an atom is absent or the new incidence violates
	/// the graph-family constraints.
	pub fn replace_incidence(
		&mut self,
		edge: &AtomRef,
		topology: &AtomRefTopology,
	) -> FerrosGrynnResult<()> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.replace_incidence(&base, edge, topology);
		}
		self.materialized()?.replace_incidence(edge, topology)
	}

	/// Removes an atom. Removing a node also removes dependent edges.
	///
	/// # Errors
	///
	/// Returns an error when the atom is absent or removal violates a family
	/// invariant.
	pub fn remove_atom(&mut self, atom: &AtomRef) -> FerrosGrynnResult<()> {
		if let Some(local) = self.write.local.as_mut() {
			let base = local_graph_view(&self.store, local)?;
			return local.delta.remove_atom(&base, atom);
		}
		self.materialized()?.remove_atom(atom)
	}
}

fn local_graph_view<'store>(
	store: &'store Datastore,
	local: &LocalGraphWrite,
) -> FerrosGrynnResult<LocalGraphView<'store>> {
	let storage = store.inner.backend.local().ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("local graph is attached to an in-memory backend")
	})?;
	Ok(storage.graph_view(local.snapshot.clone(), local.base.clone()))
}

/// A transaction-private family-specific graph write view.
///
/// This is a blind-write surface. Reads that influence application decisions
/// in a serializable transaction must be performed through [`GraphRef::read`]
/// before borrowing this view so they enter the MVCC read set.
#[non_exhaustive]
pub enum GraphWriteView<'a> {
	Graph(&'a mut Graph<Mutable>),
	Tree(&'a mut Tree<Mutable>),
	RootedTree(&'a mut RootedTree<Mutable>),
	Forest(&'a mut Forest<Mutable>),
	Dag(&'a mut Dag<Mutable>),
	Hypergraph(&'a mut Hypergraph<Mutable>),
	Metagraph(&'a mut Metagraph<Mutable>),
}

impl GraphWriteView<'_> {
	pub const fn kind(&self) -> GraphKind {
		match self {
			Self::Graph(_) => GraphKind::Graph,
			Self::Tree(_) => GraphKind::Tree,
			Self::RootedTree(_) => GraphKind::RootedTree,
			Self::Forest(_) => GraphKind::Forest,
			Self::Dag(_) => GraphKind::Dag,
			Self::Hypergraph(_) => GraphKind::Hypergraph,
			Self::Metagraph(_) => GraphKind::Metagraph,
		}
	}

	pub const fn as_graph_mut(&mut self) -> Option<&mut Graph<Mutable>> {
		match self {
			Self::Graph(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_tree_mut(&mut self) -> Option<&mut Tree<Mutable>> {
		match self {
			Self::Tree(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_rooted_tree_mut(&mut self) -> Option<&mut RootedTree<Mutable>> {
		match self {
			Self::RootedTree(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_forest_mut(&mut self) -> Option<&mut Forest<Mutable>> {
		match self {
			Self::Forest(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_dag_mut(&mut self) -> Option<&mut Dag<Mutable>> {
		match self {
			Self::Dag(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_hypergraph_mut(&mut self) -> Option<&mut Hypergraph<Mutable>> {
		match self {
			Self::Hypergraph(value) => Some(value),
			_ => None,
		}
	}

	pub const fn as_metagraph_mut(&mut self) -> Option<&mut Metagraph<Mutable>> {
		match self {
			Self::Metagraph(value) => Some(value),
			_ => None,
		}
	}
}

impl MutableGraphValue {
	pub(super) fn from_frozen(value: &MaterializedGraph) -> FerrosGrynnResult<Self> {
		match value {
			MaterializedGraph::Graph(value) => Ok(Self::Graph(value.clone().thaw())),
			MaterializedGraph::Tree(value) => Ok(Self::Tree(value.clone().thaw())),
			MaterializedGraph::RootedTree(value) => Ok(Self::RootedTree(value.clone().thaw())),
			MaterializedGraph::Forest(value) => Ok(Self::Forest(value.clone().thaw())),
			MaterializedGraph::Dag(value) => Ok(Self::Dag(value.clone().thaw()?)),
			MaterializedGraph::Hypergraph(value) => Ok(Self::Hypergraph(value.clone().thaw())),
			MaterializedGraph::Metagraph(value) => Ok(Self::Metagraph(value.clone().thaw())),
			MaterializedGraph::Subgraph(_) => Err(FerrosGrynnError::invalid_argument(
				"a materialized subgraph cannot be stored as a named graph",
			)),
		}
	}

	pub(super) const fn kind(&self) -> GraphKind {
		match self {
			Self::Graph(_) => GraphKind::Graph,
			Self::Tree(_) => GraphKind::Tree,
			Self::RootedTree(_) => GraphKind::RootedTree,
			Self::Forest(_) => GraphKind::Forest,
			Self::Dag(_) => GraphKind::Dag,
			Self::Hypergraph(_) => GraphKind::Hypergraph,
			Self::Metagraph(_) => GraphKind::Metagraph,
		}
	}

	fn atom_id(&self, reference: &AtomRef) -> FerrosGrynnResult<AtomId> {
		let id = match self {
			Self::Graph(value) => value.atom_id(reference)?,
			Self::Tree(value) => value.atom_id(reference)?,
			Self::RootedTree(value) => value.atom_id(reference)?,
			Self::Forest(value) => value.atom_id(reference)?,
			Self::Dag(value) => value.atom_id(reference)?,
			Self::Hypergraph(value) => value.atom_id(reference)?,
			Self::Metagraph(value) => value.atom_id(reference)?,
		};
		id.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				reference.kind().to_string(),
				reference.id().to_string(),
			)
		})
	}

	fn atom_ref(&self, id: AtomId) -> FerrosGrynnResult<Option<AtomRef>> {
		match self {
			Self::Graph(value) => GraphRead::atom_ref(value, id),
			Self::Tree(value) => GraphRead::atom_ref(value, id),
			Self::RootedTree(value) => GraphRead::atom_ref(value, id),
			Self::Forest(value) => GraphRead::atom_ref(value, id),
			Self::Dag(value) => GraphRead::atom_ref(value, id),
			Self::Hypergraph(value) => GraphRead::atom_ref(value, id),
			Self::Metagraph(value) => GraphRead::atom_ref(value, id),
		}
	}

	fn resolve_topology(&self, topology: &AtomRefTopology) -> FerrosGrynnResult<EdgeTopology> {
		let mut resolve = |reference: &AtomRef| self.atom_id(reference).map(EdgeEndpoint::Local);
		Ok(match topology {
			AtomRefTopology::Directed {
				source,
				target,
			} => EdgeTopology::directed(
				source.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
				target.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			AtomRefTopology::Bidirectional {
				source,
				target,
			} => EdgeTopology::bidirectional(
				source.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
				target.iter().map(&mut resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
			AtomRefTopology::Undirected {
				endpoints,
			} => EdgeTopology::undirected(
				endpoints.iter().map(resolve).collect::<FerrosGrynnResult<Vec<_>>>()?,
			),
		})
	}

	fn create_node(
		&mut self,
		record_id: RecordId,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		let reference = AtomRef::new(AtomKind::Node, record_id.clone());
		match self {
			Self::Graph(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Forest(value) => value.add_root_with_id(record_id, label, properties)?,
			Self::Dag(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Hypergraph(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Metagraph(value) => value.add_node_with_id(record_id, label, properties)?,
			Self::Tree(_) | Self::RootedTree(_) => {
				return Err(FerrosGrynnError::graph_constraint_violation(
					self.kind().name(),
					"an isolated node would violate the single-tree invariant",
				));
			}
		};
		Ok(reference)
	}

	fn create_edge(
		&mut self,
		record_id: RecordId,
		topology: &AtomRefTopology,
		label: Option<Text>,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomRef> {
		let resolved = self.resolve_topology(topology)?;
		let reference = AtomRef::new(AtomKind::Edge, record_id.clone());
		match self {
			Self::Graph(value) => {
				value.add_edge_with_id(record_id, resolved, label, properties)?;
			}
			Self::Dag(value) => {
				let (source, target) = directed_pair(&resolved, GraphKind::Dag)?;
				value.add_edge_with_id(source, target, record_id, label, properties)?;
			}
			Self::Hypergraph(value) => {
				value.add_edge_with_id(record_id, resolved, label, properties)?;
			}
			Self::Metagraph(value) => {
				value.add_edge_with_id(record_id, resolved, label, properties)?;
			}
			Self::Forest(value) => {
				let (parent, child) = forest_pair(&resolved, value.orientation())?;
				value.connect_with_id(parent, child, record_id, label, properties)?;
			}
			Self::Tree(_) | Self::RootedTree(_) => {
				return Err(FerrosGrynnError::graph_constraint_violation(
					self.kind().name(),
					"use the family write view to add a child and edge atomically",
				));
			}
		}
		Ok(reference)
	}

	fn atom_properties_mut(&mut self, reference: &AtomRef) -> FerrosGrynnResult<&mut PropertyMap> {
		let id = self.atom_id(reference)?;
		match self {
			Self::Graph(value) => atom_properties_mut(value, id),
			Self::Tree(value) => atom_properties_mut(value, id),
			Self::RootedTree(value) => atom_properties_mut(value, id),
			Self::Forest(value) => atom_properties_mut(value, id),
			Self::Dag(value) => atom_properties_mut(value, id),
			Self::Hypergraph(value) => atom_properties_mut(value, id),
			Self::Metagraph(value) => atom_properties_mut(value, id),
		}
	}

	fn set_atom_label(
		&mut self,
		reference: &AtomRef,
		label: Option<Text>,
	) -> FerrosGrynnResult<Option<Text>> {
		let id = self.atom_id(reference)?;
		let current = match self {
			Self::Graph(value) => atom_label_mut(value, id),
			Self::Tree(value) => atom_label_mut(value, id),
			Self::RootedTree(value) => atom_label_mut(value, id),
			Self::Forest(value) => atom_label_mut(value, id),
			Self::Dag(value) => atom_label_mut(value, id),
			Self::Hypergraph(value) => atom_label_mut(value, id),
			Self::Metagraph(value) => atom_label_mut(value, id),
		}?;
		Ok(std::mem::replace(current, label))
	}

	fn replace_incidence(
		&mut self,
		reference: &AtomRef,
		topology: &AtomRefTopology,
	) -> FerrosGrynnResult<()> {
		if reference.kind() != AtomKind::Edge {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				reference.id().to_string(),
				reference.kind().to_string(),
				AtomKind::Edge.to_string(),
			));
		}
		let id = self.atom_id(reference)?;
		let resolved = self.resolve_topology(topology)?;
		match self {
			Self::Graph(value) => value.replace_edge_topology(id, resolved),
			Self::Dag(value) => {
				let (source, target) = directed_pair(&resolved, GraphKind::Dag)?;
				value.replace_edge_incidence(id, source, target)
			}
			Self::Hypergraph(value) => value.replace_edge_incidence(id, resolved),
			Self::Metagraph(value) => value.replace_edge_incidence(id, resolved),
			Self::Tree(_) | Self::RootedTree(_) | Self::Forest(_) => {
				Err(FerrosGrynnError::graph_constraint_violation(
					self.kind().name(),
					"generic incidence replacement cannot preserve tree parent invariants",
				))
			}
		}
	}

	fn remove_atom(&mut self, reference: &AtomRef) -> FerrosGrynnResult<()> {
		let id = self.atom_id(reference)?;
		match self {
			Self::Graph(value) => GraphMut::remove_atom(value, id),
			Self::Tree(value) => GraphMut::remove_atom(value, id),
			Self::RootedTree(value) => GraphMut::remove_atom(value, id),
			Self::Forest(value) => GraphMut::remove_atom(value, id),
			Self::Dag(value) => GraphMut::remove_atom(value, id),
			Self::Hypergraph(value) => GraphMut::remove_atom(value, id),
			Self::Metagraph(value) => GraphMut::remove_atom(value, id),
		}
	}

	pub(super) fn dirty(&self) -> Vec<DirtyTarget> {
		match self {
			Self::Graph(value) => value.transaction_dirty(),
			Self::Tree(value) => value.transaction_dirty(),
			Self::RootedTree(value) => value.transaction_dirty(),
			Self::Forest(value) => value.transaction_dirty(),
			Self::Dag(value) => value.transaction_dirty(),
			Self::Hypergraph(value) => value.transaction_dirty(),
			Self::Metagraph(value) => value.transaction_dirty(),
		}
	}

	pub(super) fn snapshot(&self) -> MaterializedGraph {
		match self {
			Self::Graph(value) => value.transaction_snapshot().into(),
			Self::Tree(value) => value.transaction_snapshot().into(),
			Self::RootedTree(value) => value.transaction_snapshot().into(),
			Self::Forest(value) => value.transaction_snapshot().into(),
			Self::Dag(value) => value.transaction_snapshot().into(),
			Self::Hypergraph(value) => value.transaction_snapshot().into(),
			Self::Metagraph(value) => value.transaction_snapshot().into(),
		}
	}

	pub(super) fn freeze(self) -> MaterializedGraph {
		match self {
			Self::Graph(value) => value.freeze().into(),
			Self::Tree(value) => value.freeze().into(),
			Self::RootedTree(value) => value.freeze().into(),
			Self::Forest(value) => value.freeze().into(),
			Self::Dag(value) => value.freeze().into(),
			Self::Hypergraph(value) => value.freeze().into(),
			Self::Metagraph(value) => value.freeze().into(),
		}
	}

	const fn view(&mut self) -> GraphWriteView<'_> {
		match self {
			Self::Graph(value) => GraphWriteView::Graph(value),
			Self::Tree(value) => GraphWriteView::Tree(value),
			Self::RootedTree(value) => GraphWriteView::RootedTree(value),
			Self::Forest(value) => GraphWriteView::Forest(value),
			Self::Dag(value) => GraphWriteView::Dag(value),
			Self::Hypergraph(value) => GraphWriteView::Hypergraph(value),
			Self::Metagraph(value) => GraphWriteView::Metagraph(value),
		}
	}
}

fn atom_properties_mut(
	graph: &mut impl GraphMut,
	id: AtomId,
) -> FerrosGrynnResult<&mut PropertyMap> {
	match id.kind() {
		Some(AtomKind::Node) => graph.node_properties_mut(id),
		Some(AtomKind::Edge) => graph.edge_properties_mut(id),
		None => Err(FerrosGrynnError::atom_does_not_exist("null", id.to_string())),
	}
}

fn atom_label_mut(graph: &mut impl GraphMut, id: AtomId) -> FerrosGrynnResult<&mut Option<Text>> {
	match id.kind() {
		Some(AtomKind::Node) => graph.node_label_mut(id),
		Some(AtomKind::Edge) => graph.edge_label_mut(id),
		None => Err(FerrosGrynnError::atom_does_not_exist("null", id.to_string())),
	}
}

fn append_adjacency_roles(
	adjacency: &mut crate::storage::LocalNodeAdjacency,
	node: AtomId,
	edge: AtomId,
	topology: &EdgeTopology,
) {
	let mut append = |endpoint: &EdgeEndpoint, outgoing: bool, incoming: bool, undirected: bool| {
		if endpoint.local_id() != Some(node) || node.kind() != Some(AtomKind::Node) {
			return;
		}
		if outgoing {
			adjacency.outgoing.push(edge);
		}
		if incoming {
			adjacency.incoming.push(edge);
		}
		if undirected {
			adjacency.undirected.push(edge);
		}
	};
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				append(endpoint, true, false, false);
			}
			for endpoint in target {
				append(endpoint, false, true, false);
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				append(endpoint, true, true, false);
			}
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				append(endpoint, false, false, true);
			}
		}
	}
}

fn directed_pair(topology: &EdgeTopology, kind: GraphKind) -> FerrosGrynnResult<(AtomId, AtomId)> {
	match topology.local_binary() {
		Some((source, target, EdgeDirection::Directed)) => Ok((source, target)),
		_ => Err(FerrosGrynnError::graph_constraint_violation(
			kind.name(),
			"the graph family requires one directed local source and target",
		)),
	}
}

fn forest_pair(
	topology: &EdgeTopology,
	orientation: TreeOrientation,
) -> FerrosGrynnResult<(AtomId, AtomId)> {
	let Some((source, target, direction)) = topology.local_binary() else {
		return Err(FerrosGrynnError::graph_constraint_violation(
			GraphKind::Forest.name(),
			"a forest edge requires two local endpoints",
		));
	};
	match (orientation, direction) {
		(TreeOrientation::Undirected, EdgeDirection::Undirected)
		| (TreeOrientation::OutArborescence, EdgeDirection::Directed) => Ok((source, target)),
		(TreeOrientation::InArborescence, EdgeDirection::Directed) => Ok((target, source)),
		_ => Err(FerrosGrynnError::graph_constraint_violation(
			GraphKind::Forest.name(),
			"edge direction disagrees with the forest orientation",
		)),
	}
}

#[allow(
	clippy::multiple_inherent_impl,
	reason = "transaction responsibilities are split into cohesive modules"
)]
impl Transaction {
	/// Creates a graph using its record value as the default catalog name.
	///
	/// # Errors
	///
	/// Returns an error under the same conditions as
	/// [`Self::create_named_graph`].
	pub fn create_graph(
		&mut self,
		id: RecordId,
		graph: MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		let name = id.id().to_string();
		self.create_named_graph(name, id, graph)
	}

	/// Creates a graph with an explicit case-sensitive catalog name.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable, the record or
	/// name already exists, or the name violates the named-graph bounds.
	pub fn create_named_graph<IntoText: Into<Text>>(
		&mut self,
		name: IntoText,
		id: RecordId,
		graph: MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let name = name.into();
		validate_graph_name(name.as_str())?;
		if id.table() == INTERNAL_NESTED_GRAPH_TABLE {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"RecordId table '{INTERNAL_NESTED_GRAPH_TABLE}' is reserved for nested graphs"
			)));
		}
		if self.visible_graph_kind(&id).is_some() {
			return Err(FerrosGrynnError::graph_already_exists(id.to_string()));
		}
		if self.graphs.iter().any(|(record_id, write)| {
			record_id != &id && write.exists() && write.name == name.as_str()
		}) {
			return Err(FerrosGrynnError::graph_already_exists(name.as_str()));
		}
		let root = self.read_root();
		if let Some(record_id) = root.schema.graph_names.get(name.as_str())
			&& record_id != &id
			&& !self.graphs.get(record_id).is_some_and(|write| !write.exists())
		{
			return Err(FerrosGrynnError::graph_already_exists(name.as_str()));
		}
		let kind = graph.kind();
		let value = MutableGraphValue::from_frozen(&graph)?;
		let reference = GraphRef::new(id.clone(), kind, self.store.inner.graph_storage_mode());
		let schema = GraphSchema::new(
			self.store.inner.allocate_schema_id()?,
			SchemaVersion::INITIAL,
			name.clone(),
			reference.clone(),
			SystemIndex::new(self.store.inner.allocate_schema_id()?, SystemIndexKind::Label),
			SystemIndex::new(self.store.inner.allocate_schema_id()?, SystemIndexKind::Adjacency),
			0,
		);
		self.graphs.insert(id.clone(), GraphWrite {
			name: name.as_str().to_owned(),
			schema,
			base: None,
			local: None,
			base_ts: root.commit_ts,
			value: Some(value),
			memberships: BTreeMap::new(),
			force_persist: false,
		});
		Ok(reference)
	}

	pub fn remove_graph(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		if self.graphs.get(reference.id()).is_some_and(GraphWrite::is_created) {
			self.graphs.remove(reference.id());
			return Ok(());
		}
		self.ensure_graph_write(reference)?;
		if let Some(write) = self.graphs.get_mut(reference.id()) {
			write.value = None;
		}
		Ok(())
	}

	/// Opens the compatibility full-family mutation API.
	///
	/// Local graphs are materialized in full. Prefer [`Self::mutate_graph`] for
	/// bounded point mutations or [`Self::edit_graph_materialized`] when full
	/// family access is intentional.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is read-only, expired or
	/// cancelled, the graph reference belongs to another backend, or the graph
	/// is absent.
	#[deprecated(note = "use mutate_graph for bounded point edits or edit_graph_materialized for \
	                     explicit full materialization")]
	pub fn edit_graph(&mut self, reference: &GraphRef) -> FerrosGrynnResult<GraphWriteView<'_>> {
		self.edit_graph_materialized(reference)
	}

	/// Explicitly materializes a complete graph for family-specific mutation.
	///
	/// This operation has memory and I/O cost proportional to the complete
	/// local graph. Use [`Self::mutate_graph`] for bounded atom changes.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is read-only, expired or
	/// cancelled, the graph reference belongs to another backend, or the graph
	/// is absent.
	pub fn edit_graph_materialized(
		&mut self,
		reference: &GraphRef,
	) -> FerrosGrynnResult<GraphWriteView<'_>> {
		self.ensure_writable()?;
		self.ensure_live()?;
		self.ensure_graph_write(reference)?;
		self.graphs
			.get_mut(reference.id())
			.and_then(|write| write.value.as_mut())
			.map(MutableGraphValue::view)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))
	}

	/// Opens the backend-neutral point-mutation surface for one graph.
	///
	/// This is a blind-write surface. Reads that influence serializable
	/// decisions must be performed through [`GraphRef::read`] before this
	/// mutable borrow.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is read-only, expired or
	/// cancelled, the graph reference belongs to another backend, or the graph
	/// is absent.
	pub fn mutate_graph(
		&mut self,
		reference: &GraphRef,
	) -> FerrosGrynnResult<GraphMutationView<'_>> {
		self.ensure_writable()?;
		self.ensure_live()?;
		self.ensure_graph_mutation_write(reference)?;
		let store = self.store.clone();
		let write = self
			.graphs
			.get_mut(reference.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		let kind = write
			.kind()
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		Ok(GraphMutationView {
			store,
			kind,
			write,
		})
	}

	pub fn define_table(&mut self, mut schema: TableSchema) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		if self.visible_table(schema.name.as_str()).is_some() {
			return Err(FerrosGrynnError::table_already_exists(schema.name.as_str()));
		}
		self.validate_table_schema(&mut schema)?;
		schema.id = Some(self.store.inner.allocate_schema_id()?);
		schema.version = SchemaVersion::INITIAL;
		schema.primary_index = Some(SystemIndex::new(
			self.store.inner.allocate_schema_id()?,
			SystemIndexKind::Primary,
		));
		for field in &mut schema.fields {
			field.id = Some(self.store.inner.allocate_schema_id()?);
			field.version = SchemaVersion::INITIAL;
		}
		for index in &mut schema.indexes {
			index.id = Some(self.store.inner.allocate_schema_id()?);
			index.version = SchemaVersion::INITIAL;
		}
		if !schema.indexes.is_empty() {
			self.ensure_graph_write(&schema.graph)?;
			if let Some(write) = self.graphs.get_mut(schema.graph.id()) {
				write.force_persist = true;
			}
		}
		self.tables.insert(schema.name.as_str().to_owned(), Some(schema));
		Ok(())
	}

	/// Replaces one table definition while retaining stable catalog identities.
	///
	/// Fields and user indexes with the same name retain their identities;
	/// changed definitions advance their individual versions. Moving a table to
	/// another graph is intentionally rejected because its migration semantics
	/// are not part of the current storage contract.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable, the table or
	/// graph is absent, the graph changes, a property is not interned, or a
	/// schema identity/version clock is exhausted.
	pub fn overwrite_table(&mut self, mut schema: TableSchema) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let previous = self
			.visible_table(schema.name.as_str())
			.ok_or_else(|| FerrosGrynnError::table_does_not_exist(schema.name.as_str()))?;
		if previous.graph != schema.graph {
			return Err(FerrosGrynnError::invalid_argument(
				"overwriting a table cannot move it to another graph",
			));
		}
		self.validate_table_schema(&mut schema)?;
		schema.id = previous.id;
		schema.version = previous.version.next()?;
		schema.primary_index = previous.primary_index;
		for field in &mut schema.fields {
			if let Some(old) = previous.fields.iter().find(|old| old.name == field.name) {
				field.id = old.id;
				field.version = if old.value_type == field.value_type {
					old.version
				} else {
					old.version.next()?
				};
			} else {
				field.id = Some(self.store.inner.allocate_schema_id()?);
				field.version = SchemaVersion::INITIAL;
			}
		}
		for index in &mut schema.indexes {
			if let Some(old) = previous.indexes.iter().find(|old| old.name == index.name) {
				index.id = old.id;
				let field_contract_changed = index.fields.iter().any(|name| {
					let old_field = previous.fields.iter().find(|field| field.name == *name);
					let new_field = schema.fields.iter().find(|field| field.name == *name);
					old_field
						.zip(new_field)
						.is_none_or(|(old, new)| old.value_type != new.value_type)
				});
				index.version = if old.kind == index.kind
					&& old.fields == index.fields
					&& !field_contract_changed
				{
					old.version
				} else {
					old.version.next()?
				};
			} else {
				index.id = Some(self.store.inner.allocate_schema_id()?);
				index.version = SchemaVersion::INITIAL;
			}
		}
		if previous.fields != schema.fields || previous.indexes != schema.indexes {
			self.ensure_graph_write(&schema.graph)?;
			if let Some(write) = self.graphs.get_mut(schema.graph.id()) {
				write.force_persist = true;
			}
		}
		self.tables.insert(schema.name.as_str().to_owned(), Some(schema));
		Ok(())
	}

	pub fn remove_table(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let schema =
			self.visible_table(name).ok_or_else(|| FerrosGrynnError::table_does_not_exist(name))?;
		// ponytail: table removal rebuilds the owning graph so concurrent
		// membership additions cannot survive in hot slots. Stage 7 may replace
		// this with incremental primary/scalar-index maintenance.
		self.ensure_graph_write(&schema.graph)?;
		if let Some(write) = self.graphs.get_mut(schema.graph.id()) {
			write.force_persist = true;
		}
		self.tables.insert(name.to_owned(), None);
		Ok(())
	}

	pub fn table(&self, name: &str) -> FerrosGrynnResult<Arc<TableSchema>> {
		self.ensure_live()?;
		let root = self.read_root();
		self.record_read(&root, ReadKey::Table(name.to_owned()));
		match self.tables.get(name) {
			Some(Some(schema)) => Ok(Arc::new(schema.clone())),
			Some(None) => Err(FerrosGrynnError::table_does_not_exist(name)),
			None => {
				root.schema.table(name).ok_or_else(|| FerrosGrynnError::table_does_not_exist(name))
			}
		}
	}

	/// Assigns one graph atom to exactly one transaction-visible table.
	///
	/// The atom's public [`RecordId`] does not determine membership. Scalar
	/// indexes still require the explicit full-materialization API until stage
	/// 7 adds incremental index maintenance.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable or live, the
	/// graph, atom, or table is absent, the table belongs to another graph, or
	/// the atom kind violates the table kind.
	pub fn assign_atom_to_table(
		&mut self,
		graph: &GraphRef,
		atom: &AtomRef,
		table: &str,
	) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let schema = self
			.visible_table(table)
			.ok_or_else(|| FerrosGrynnError::table_does_not_exist(table))?;
		if schema.graph != *graph {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"table '{}' belongs to graph '{}', not '{}'",
				table,
				schema.graph.id(),
				graph.id()
			)));
		}
		let kind_matches = matches!(
			(schema.kind, atom.kind()),
			(TableKind::Any, _)
				| (TableKind::Normal, AtomKind::Node)
				| (TableKind::Relation, AtomKind::Edge)
		);
		if !kind_matches {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				atom.id().to_string(),
				atom.kind().to_string(),
				match schema.kind {
					TableKind::Any => "node or edge",
					TableKind::Normal => "node",
					TableKind::Relation => "edge",
				}
				.to_owned(),
			));
		}
		let table_id = schema.id.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"transaction-visible table has no schema identity",
			)
		})?;
		self.ensure_graph_mutation_write(graph)?;
		let store = self.store.clone();
		let write = self
			.graphs
			.get_mut(graph.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.id().to_string()))?;
		let atom_id = write.atom_id(&store, atom)?;
		if let Some(local) = write.local.as_mut() {
			local.delta.bump_revision()?;
		}
		write.memberships.insert(atom_id, Some(table_id));
		Ok(())
	}

	/// Clears the explicit table membership of one graph atom.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable or live, or the
	/// graph or atom is absent.
	pub fn clear_atom_table(&mut self, graph: &GraphRef, atom: &AtomRef) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		self.ensure_graph_mutation_write(graph)?;
		let store = self.store.clone();
		let write = self
			.graphs
			.get_mut(graph.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.id().to_string()))?;
		let atom_id = write.atom_id(&store, atom)?;
		if let Some(local) = write.local.as_mut() {
			local.delta.bump_revision()?;
		}
		write.memberships.insert(atom_id, None);
		Ok(())
	}

	/// Publishes a new global database parameter.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable or live, the name
	/// already exists, or the schema identity clock is exhausted.
	pub fn define_parameter(&mut self, mut parameter: SchemaParameter) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		if self.visible_parameter(parameter.name.as_str()).is_some() {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"schema parameter '{}' already exists",
				parameter.name
			)));
		}
		parameter.id = Some(self.store.inner.allocate_schema_id()?);
		parameter.version = SchemaVersion::INITIAL;
		self.parameters.insert(parameter.name.as_str().to_owned(), Some(parameter));
		Ok(())
	}

	/// Replaces a global database parameter while retaining its stable
	/// identity.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable or live, the
	/// parameter is absent, or its version is exhausted.
	pub fn overwrite_parameter(&mut self, mut parameter: SchemaParameter) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		let previous = self.visible_parameter(parameter.name.as_str()).ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!(
				"schema parameter '{}' does not exist",
				parameter.name
			))
		})?;
		parameter.id = previous.id;
		parameter.version = previous.version.next()?;
		self.parameters.insert(parameter.name.as_str().to_owned(), Some(parameter));
		Ok(())
	}

	/// Removes one global database parameter.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is not writable or live, or the
	/// parameter is absent.
	pub fn remove_parameter(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.ensure_writable()?;
		self.ensure_live()?;
		if self.visible_parameter(name).is_none() {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"schema parameter '{name}' does not exist"
			)));
		}
		self.parameters.insert(name.to_owned(), None);
		Ok(())
	}

	/// Reads one transaction-visible global database parameter.
	///
	/// # Errors
	///
	/// Returns an error when the transaction is no longer live or the parameter
	/// is absent.
	pub fn parameter(&self, name: &str) -> FerrosGrynnResult<Arc<SchemaParameter>> {
		self.ensure_live()?;
		let root = self.read_root();
		self.record_read(&root, ReadKey::Parameter(name.to_owned()));
		match self.parameters.get(name) {
			Some(Some(parameter)) => Ok(Arc::new(parameter.clone())),
			Some(None) => Err(FerrosGrynnError::invalid_argument(format!(
				"schema parameter '{name}' does not exist"
			))),
			None => root.schema.parameter(name).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"schema parameter '{name}' does not exist"
				))
			}),
		}
	}

	fn validate_table_schema(&self, schema: &mut TableSchema) -> FerrosGrynnResult<()> {
		ensure_store_reference(&schema.graph, self.store.inner.graph_storage_mode())?;
		let graph_kind = self
			.visible_graph_kind(schema.graph.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(schema.graph.id().to_string()))?;
		if graph_kind != schema.graph.kind() {
			return Err(FerrosGrynnError::conversion_error(
				schema.graph.kind().name(),
				graph_kind.name(),
			));
		}
		let dictionary = self.read_root().dictionary.clone();
		for field in &mut schema.fields {
			field.key = Some(dictionary.get(field.name.as_str()).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"property '{}' must be interned before defining its table schema",
					field.name
				))
			})?);
		}
		Ok(())
	}

	fn ensure_writable(&self) -> FerrosGrynnResult<()> {
		if self.options.access == TransactionAccess::ReadOnly {
			return Err(FerrosGrynnError::read_only_transaction(self.id));
		}
		Ok(())
	}

	fn ensure_graph_write(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		if let Some(write) = self.graphs.get_mut(reference.id()) {
			write.ensure_materialized(&self.store)?;
			return Ok(());
		}
		let root = self.read_root();
		let stored = root
			.graphs
			.get(reference.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		let schema = root.schema.graph(reference.id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("stored graph has no schema-catalog definition")
		})?;
		if stored.graph.kind() != reference.kind() {
			return Err(FerrosGrynnError::conversion_error(
				reference.kind().name(),
				stored.graph.kind().name(),
			));
		}
		let base = match &stored.graph {
			StoredGraphValue::Materialized(graph) => graph.clone(),
			StoredGraphValue::Local(graph) => {
				let local = self.store.inner.backend.local().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"local graph is attached to an in-memory backend",
					)
				})?;
				Arc::new(local.materialize_graph(self.local_snapshot()?, graph)?)
			}
		};
		self.graphs.insert(reference.id().clone(), GraphWrite {
			name: stored.name.clone(),
			schema: schema.as_ref().clone(),
			base: Some(base.clone()),
			local: None,
			base_ts: root.commit_ts,
			value: Some(MutableGraphValue::from_frozen(&base)?),
			memberships: BTreeMap::new(),
			force_persist: false,
		});
		Ok(())
	}

	fn ensure_graph_mutation_write(&mut self, reference: &GraphRef) -> FerrosGrynnResult<()> {
		ensure_store_reference(reference, self.store.inner.graph_storage_mode())?;
		if self.graphs.contains_key(reference.id()) {
			return Ok(());
		}
		let root = self.read_root();
		let stored = root
			.graphs
			.get(reference.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))?;
		let schema = root.schema.graph(reference.id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("stored graph has no schema-catalog definition")
		})?;
		if stored.graph.kind() != reference.kind() {
			return Err(FerrosGrynnError::conversion_error(
				reference.kind().name(),
				stored.graph.kind().name(),
			));
		}
		let (base, local, value) = match &stored.graph {
			StoredGraphValue::Materialized(graph) => {
				(Some(graph.clone()), None, Some(MutableGraphValue::from_frozen(graph)?))
			}
			StoredGraphValue::Local(graph) => {
				if !graph.supports_atom_delta() {
					return Err(FerrosGrynnError::invalid_argument(
						"bounded point mutation is unavailable while scalar indexes require a \
						 full rebuild; use edit_graph_materialized explicitly",
					));
				}
				(
					None,
					Some(LocalGraphWrite {
						base: graph.clone(),
						snapshot: self.local_snapshot()?,
						delta: LocalGraphDelta::new(
							graph.kind(),
							graph.node_capacity(),
							graph.edge_capacity(),
							graph.revision(),
						),
					}),
					None,
				)
			}
		};
		self.graphs.insert(reference.id().clone(), GraphWrite {
			name: stored.name.clone(),
			schema: schema.as_ref().clone(),
			base,
			local,
			base_ts: root.commit_ts,
			value,
			memberships: BTreeMap::new(),
			force_persist: false,
		});
		Ok(())
	}
}

/// Validates the public catalog-name bounds.
pub(super) fn ensure_store_reference(
	reference: &GraphRef,
	expected: GraphStorageMode,
) -> FerrosGrynnResult<()> {
	if reference.storage_mode() != expected {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"graph reference mode {:?} does not belong to this {:?} datastore",
			reference.storage_mode(),
			expected
		)));
	}
	Ok(())
}
