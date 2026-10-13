use std::collections::{BTreeMap, BTreeSet};

use crate::{
	datastore::GlobalAtomId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeTopology, GraphId, GraphRef, SlotAllocator},
	values::{Number, PropertyKeyId, Text, Value},
};

/// One explicit graph-local property decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverlayValue {
	/// Replaces the database-global base value in this graph.
	Value(Value),
	/// Suppresses the database-global base value in this graph.
	Hidden,
}

/// Sparse property changes owned by one graph occurrence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PropertyOverlay {
	entries: BTreeMap<PropertyKeyId, OverlayValue>,
}

impl PropertyOverlay {
	pub(crate) fn from_entries(
		entries: impl IntoIterator<Item = (PropertyKeyId, OverlayValue)>,
	) -> FerrosGrynnResult<Self> {
		let mut overlay = Self::default();
		for (key, value) in entries {
			if overlay.entries.insert(key, value).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"property overlay contains a duplicate key",
				));
			}
		}
		if overlay.entries.is_empty() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"an empty property overlay must not be stored",
			));
		}
		Ok(overlay)
	}

	pub(crate) fn get(&self, key: PropertyKeyId) -> Option<&OverlayValue> {
		self.entries.get(&key)
	}

	pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = (PropertyKeyId, &OverlayValue)> {
		self.entries.iter().map(|(key, value)| (*key, value))
	}

	fn insert(&mut self, key: PropertyKeyId, value: OverlayValue) {
		self.entries.insert(key, value);
	}

	fn remove(&mut self, key: PropertyKeyId) {
		self.entries.remove(&key);
	}

	fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

/// Graph-local structural state of one global atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OccurrenceState {
	/// Node-local nested-graph binding.
	Node {
		/// Optional nested graph owned by this occurrence.
		nested_graph: Option<GraphId>,
	},
	/// Edge-local incidence and attributes.
	Edge {
		/// Endpoints expressed with graph-local atom IDs.
		topology: EdgeTopology,
		/// Optional graph-local weight.
		weight: Option<Number>,
		/// Optional transition graph owned by this occurrence.
		transition_graph: Option<GraphId>,
	},
}

/// One graph-local occurrence of a database-global atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphOccurrence {
	local_id: AtomId,
	global_id: GlobalAtomId,
	label: Option<Text>,
	overlay: Option<PropertyOverlay>,
	state: OccurrenceState,
}

impl GraphOccurrence {
	pub(crate) const fn local_id(&self) -> AtomId {
		self.local_id
	}

	pub(crate) const fn global_id(&self) -> GlobalAtomId {
		self.global_id
	}

	pub(crate) const fn label(&self) -> Option<&Text> {
		self.label.as_ref()
	}

	pub(crate) const fn state(&self) -> &OccurrenceState {
		&self.state
	}

	/// Resolves the container binding for either node or edge owners.
	pub(crate) const fn child_graph(&self) -> Option<GraphId> {
		match &self.state {
			OccurrenceState::Node {
				nested_graph,
			} => *nested_graph,
			OccurrenceState::Edge {
				transition_graph,
				..
			} => *transition_graph,
		}
	}

	pub(crate) const fn has_overlay(&self) -> bool {
		self.overlay.is_some()
	}

	pub(crate) const fn overlay(&self) -> Option<&PropertyOverlay> {
		self.overlay.as_ref()
	}

	pub(crate) fn set_label(&mut self, label: Option<Text>) {
		self.label = label;
	}

	pub(crate) fn replace_state(&mut self, state: OccurrenceState) {
		self.state = state;
	}

	pub(crate) fn replace_overlay(&mut self, overlay: Option<PropertyOverlay>) {
		self.overlay = overlay;
	}

	pub(crate) fn inherits(&self, key: PropertyKeyId) -> bool {
		self.overlay.as_ref().and_then(|overlay| overlay.get(key)).is_none()
	}

	pub(crate) fn set_overlay(&mut self, key: PropertyKeyId, value: OverlayValue) {
		self.overlay.get_or_insert_with(PropertyOverlay::default).insert(key, value);
	}

	pub(crate) fn reset_overlay(&mut self, key: PropertyKeyId) {
		let Some(overlay) = self.overlay.as_mut() else {
			return;
		};
		overlay.remove(key);
		if overlay.is_empty() {
			self.overlay = None;
		}
	}

	pub(crate) fn effective_value<'a>(
		&'a self,
		base: Option<&'a Value>,
		key: PropertyKeyId,
	) -> Option<&'a Value> {
		match self.overlay.as_ref().and_then(|overlay| overlay.get(key)) {
			Some(OverlayValue::Value(value)) => Some(value),
			Some(OverlayValue::Hidden) => None,
			None => base,
		}
	}
}

/// All occurrences and local address allocation for one graph.
#[derive(Debug, Clone)]
pub(crate) struct GraphRoot {
	/// Durable logical incidence revision, independent of checkpoint LSN.
	pub(crate) tensor_revision: u64,
	reference: GraphRef,
	nodes: SlotAllocator,
	edges: SlotAllocator,
	by_global: BTreeMap<GlobalAtomId, AtomId>,
	occurrences: BTreeMap<AtomId, GraphOccurrence>,
	/// Created at the first edge endpoint; keys retain the complete nested
	/// scope.
	edge_endpoints: Option<BTreeMap<crate::graph::EdgeEndpoint, BTreeSet<(AtomId, bool)>>>,
}

impl GraphRoot {
	pub(crate) fn new(reference: GraphRef) -> Self {
		Self {
			tensor_revision: 0,
			reference,
			nodes: SlotAllocator::new(AtomKind::Node),
			edges: SlotAllocator::new(AtomKind::Edge),
			by_global: BTreeMap::new(),
			occurrences: BTreeMap::new(),
			edge_endpoints: None,
		}
	}

	pub(crate) const fn reference(&self) -> &GraphRef {
		&self.reference
	}

	pub(crate) fn local_id(&self, global: GlobalAtomId) -> Option<AtomId> {
		self.by_global.get(&global).copied()
	}

	pub(crate) fn occurrence(&self, local: AtomId) -> Option<&GraphOccurrence> {
		self.occurrences.get(&local).filter(|occurrence| occurrence.local_id == local)
	}

	pub(crate) fn occurrence_mut(&mut self, local: AtomId) -> Option<&mut GraphOccurrence> {
		self.occurrences.get_mut(&local).filter(|occurrence| occurrence.local_id == local)
	}

	/// Returns canonical connector/source-target postings for one qualified
	/// endpoint.
	pub(crate) fn edge_endpoint_postings(
		&self,
		endpoint: &crate::graph::EdgeEndpoint,
	) -> impl Iterator<Item = &(AtomId, bool)> {
		self.edge_endpoints
			.as_ref()
			.and_then(|postings| postings.get(endpoint))
			.into_iter()
			.flatten()
	}

	/// Streams all synchronous endpoint postings for durable generation
	/// construction.
	pub(crate) fn edge_endpoint_entries(
		&self,
	) -> impl Iterator<Item = (&crate::graph::EdgeEndpoint, AtomId, bool)> {
		self.edge_endpoints.iter().flat_map(|postings| postings.iter()).flat_map(
			|(endpoint, connectors)| {
				connectors.iter().map(move |&(connector, side)| (endpoint, connector, side))
			},
		)
	}

	/// Replaces one occurrence and its endpoint postings together after
	/// validation.
	pub(crate) fn replace_occurrence(&mut self, after: GraphOccurrence) {
		let local = after.local_id();
		if let Some(before) = self.occurrences.get(&local) {
			let before_topology = match before.state() {
				OccurrenceState::Edge {
					topology,
					..
				} => Some(topology),
				OccurrenceState::Node {
					..
				} => None,
			};
			let after_topology = match after.state() {
				OccurrenceState::Edge {
					topology,
					..
				} => Some(topology),
				OccurrenceState::Node {
					..
				} => None,
			};
			if before_topology == after_topology {
				self.occurrences.insert(local, after);
				return;
			}
			if let Some(topology) = before_topology.cloned() {
				self.update_edge_endpoint_postings(local, &topology, false);
			}
			if let Some(topology) = after_topology {
				self.update_edge_endpoint_postings(local, topology, true);
			}
		}
		self.occurrences.insert(local, after);
	}

	/// Maintains one exact posting per connector and actual endpoint side.
	fn update_edge_endpoint_postings(
		&mut self,
		connector: AtomId,
		topology: &EdgeTopology,
		insert: bool,
	) {
		for (endpoint, side) in topology.edge_endpoint_sides() {
			if insert {
				self.edge_endpoints
					.get_or_insert_with(BTreeMap::new)
					.entry(endpoint.clone())
					.or_default()
					.insert((connector, side));
			} else if let Some(postings) = &mut self.edge_endpoints {
				if let Some(connectors) = postings.get_mut(endpoint) {
					connectors.remove(&(connector, side));
				}
				if postings.get(endpoint).is_some_and(BTreeSet::is_empty) {
					postings.remove(endpoint);
				}
			}
		}
	}

	pub(crate) fn attach_node(
		&mut self,
		global: GlobalAtomId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.attach(global, |local_id| GraphOccurrence {
			local_id,
			global_id: global,
			label,
			overlay: None,
			state: OccurrenceState::Node {
				nested_graph,
			},
		})
	}

	pub(crate) fn attach_node_at(
		&mut self,
		local_id: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		self.attach_at(local_id, global, |local_id| GraphOccurrence {
			local_id,
			global_id: global,
			label,
			overlay: None,
			state: OccurrenceState::Node {
				nested_graph,
			},
		})
	}

	pub(crate) fn attach_edge(
		&mut self,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.validate_endpoints(&topology)?;
		self.attach(global, |local_id| GraphOccurrence {
			local_id,
			global_id: global,
			label,
			overlay: None,
			state: OccurrenceState::Edge {
				topology,
				weight,
				transition_graph,
			},
		})
	}

	pub(crate) fn attach_edge_at(
		&mut self,
		local_id: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		self.validate_endpoints(&topology)?;
		self.attach_at(local_id, global, |local_id| GraphOccurrence {
			local_id,
			global_id: global,
			label,
			overlay: None,
			state: OccurrenceState::Edge {
				topology,
				weight,
				transition_graph,
			},
		})
	}

	/// Restores an edge before all generation-aware endpoint slots are
	/// installed.
	///
	/// The caller must invoke [`Self::validate_restored_endpoints`] after the
	/// complete graph image has been restored.
	pub(crate) fn restore_edge_at(
		&mut self,
		local_id: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		self.attach_at(local_id, global, |local_id| GraphOccurrence {
			local_id,
			global_id: global,
			label,
			overlay: None,
			state: OccurrenceState::Edge {
				topology,
				weight,
				transition_graph,
			},
		})
	}

	/// Validates every local endpoint after two-phase durable restoration.
	pub(crate) fn validate_restored_endpoints(&self) -> FerrosGrynnResult<()> {
		for occurrence in self.occurrences.values() {
			if let OccurrenceState::Edge {
				topology,
				..
			} = occurrence.state()
			{
				self.validate_endpoints(topology)?;
			}
		}
		Ok(())
	}

	fn attach(
		&mut self,
		global: GlobalAtomId,
		build: impl FnOnce(AtomId) -> GraphOccurrence,
	) -> FerrosGrynnResult<AtomId> {
		if self.by_global.contains_key(&global) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.reference.name().to_string(),
				format!("global atom '{global}' already has an occurrence"),
			));
		}
		let allocator = match global.kind() {
			Some(AtomKind::Node) => &mut self.nodes,
			Some(AtomKind::Edge) => &mut self.edges,
			None => {
				return Err(FerrosGrynnError::invalid_argument(
					"an occurrence requires a valid global atom kind",
				));
			}
		};
		let local = allocator.alloc()?;
		let occurrence = build(local);
		if let OccurrenceState::Edge {
			topology,
			..
		} = occurrence.state()
		{
			self.update_edge_endpoint_postings(local, topology, true);
		}
		self.by_global.insert(global, local);
		self.occurrences.insert(local, occurrence);
		Ok(local)
	}

	fn attach_at(
		&mut self,
		local: AtomId,
		global: GlobalAtomId,
		build: impl FnOnce(AtomId) -> GraphOccurrence,
	) -> FerrosGrynnResult<()> {
		if self.by_global.contains_key(&global)
			|| self.occurrences.contains_key(&local)
			|| local.kind() != global.kind()
		{
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.reference.name().to_string(),
				"restored occurrence identity is already occupied or has the wrong kind",
			));
		}
		let allocator = match global.kind() {
			Some(AtomKind::Node) => &mut self.nodes,
			Some(AtomKind::Edge) => &mut self.edges,
			None => {
				return Err(FerrosGrynnError::invalid_argument(
					"an occurrence requires a valid global atom kind",
				));
			}
		};
		allocator.restore(local)?;
		let occurrence = build(local);
		if let OccurrenceState::Edge {
			topology,
			..
		} = occurrence.state()
		{
			self.update_edge_endpoint_postings(local, topology, true);
		}
		self.by_global.insert(global, local);
		self.occurrences.insert(local, occurrence);
		Ok(())
	}

	fn validate_endpoints(&self, topology: &EdgeTopology) -> FerrosGrynnResult<()> {
		for endpoint in topology.endpoints().filter_map(|endpoint| endpoint.local_id()) {
			if !self.occurrences.contains_key(&endpoint) {
				return Err(FerrosGrynnError::atom_does_not_exist(
					"endpoint",
					endpoint.to_string(),
				));
			}
		}
		Ok(())
	}

	pub(crate) fn validate_state(
		&self,
		local: AtomId,
		state: &OccurrenceState,
	) -> FerrosGrynnResult<()> {
		match (local.kind(), state) {
			(
				Some(AtomKind::Node),
				OccurrenceState::Node {
					..
				},
			) => Ok(()),
			(
				Some(AtomKind::Edge),
				OccurrenceState::Edge {
					topology,
					..
				},
			) => self.validate_endpoints(topology),
			_ => Err(FerrosGrynnError::atom_kind_mismatch(
				local.to_string(),
				local.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
				match state {
					OccurrenceState::Node {
						..
					} => AtomKind::Node.to_string(),
					OccurrenceState::Edge {
						..
					} => AtomKind::Edge.to_string(),
				},
			)),
		}
	}

	pub(crate) fn detach(&mut self, global: GlobalAtomId) -> Option<GraphOccurrence> {
		let local = self.by_global.remove(&global)?;
		let occurrence = self.occurrences.remove(&local)?;
		if let OccurrenceState::Edge {
			topology,
			..
		} = occurrence.state()
		{
			self.update_edge_endpoint_postings(local, topology, false);
		}
		match local.kind() {
			Some(AtomKind::Node) => {
				self.nodes.free(local);
			}
			Some(AtomKind::Edge) => {
				self.edges.free(local);
			}
			None => {}
		}
		Some(occurrence)
	}

	/// Builds a dependency-ordered occurrence removal plan.
	pub(crate) fn detach_plan(
		&self,
		global: GlobalAtomId,
		cascade: bool,
	) -> FerrosGrynnResult<Vec<GlobalAtomId>> {
		let local = self.local_id(global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
		})?;
		let dependencies = self.incident_edges(local);
		if !cascade && !dependencies.is_empty() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				self.reference.name().to_string(),
				"DETACH requires CASCADE while graph-local incidences reference the occurrence",
			));
		}
		let mut visited = BTreeSet::new();
		let mut plan = Vec::new();
		self.append_detach_plan(global, &mut visited, &mut plan)?;
		Ok(plan)
	}

	fn append_detach_plan(
		&self,
		global: GlobalAtomId,
		visited: &mut BTreeSet<GlobalAtomId>,
		plan: &mut Vec<GlobalAtomId>,
	) -> FerrosGrynnResult<()> {
		if !visited.insert(global) {
			return Ok(());
		}
		let local = self.local_id(global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
		})?;
		for dependency in self.incident_edges(local) {
			self.append_detach_plan(dependency, visited, plan)?;
		}
		plan.push(global);
		Ok(())
	}

	fn incident_edges(&self, endpoint: AtomId) -> Vec<GlobalAtomId> {
		if endpoint.kind() == Some(AtomKind::Edge) {
			return self
				.edge_endpoint_postings(&crate::graph::EdgeEndpoint::Local(endpoint))
				.filter_map(|&(connector, _)| {
					self.occurrence(connector).map(GraphOccurrence::global_id)
				})
				.collect::<BTreeSet<_>>()
				.into_iter()
				.collect();
		}
		self.occurrences
			.values()
			.filter_map(|occurrence| match occurrence.state() {
				OccurrenceState::Edge {
					topology,
					..
				} if topology
					.endpoints()
					.any(|candidate| candidate.local_id() == Some(endpoint)) =>
				{
					Some(occurrence.global_id())
				}
				_ => None,
			})
			.collect()
	}

	pub(crate) fn occurrences(&self) -> impl ExactSizeIterator<Item = &GraphOccurrence> + Clone {
		self.occurrences.values()
	}

	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		match kind {
			AtomKind::Node => self.nodes.capacity(),
			AtomKind::Edge => self.edges.capacity(),
		}
	}

	pub(crate) fn slot_generation(&self, kind: AtomKind, slot: usize) -> Option<u32> {
		match kind {
			AtomKind::Node => self.nodes.generation(slot),
			AtomKind::Edge => self.edges.generation(slot),
		}
	}

	pub(crate) fn restore_free_slot(
		&mut self,
		kind: AtomKind,
		slot: u64,
		generation: u32,
	) -> FerrosGrynnResult<()> {
		match kind {
			AtomKind::Node => self.nodes.restore_free(slot, generation),
			AtomKind::Edge => self.edges.restore_free(slot, generation),
		}
	}
}
