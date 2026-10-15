use std::collections::{BTreeMap, BTreeSet};

use crate::{
	datastore::GlobalAtomId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeTopology, GraphId, GraphRef, SlotAllocator},
	values::{Number, PropertyKeyId, Text, Value},
};

/// One explicit graph-local property decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayValue {
	/// Replaces the database-global base value in this graph.
	Value(
		/// Replaces the database-global base value in this graph.
		Value,
	),
	/// Suppresses the database-global base value in this graph.
	Hidden,
}

/// Sparse property changes owned by one graph occurrence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PropertyOverlay {
	/// Entries associated with this property overlay.
	entries: BTreeMap<PropertyKeyId, OverlayValue>,
}

impl PropertyOverlay {
	/// Creates from entries.
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

	/// Returns property overlay.
	pub(crate) fn get(&self, key: PropertyKeyId) -> Option<&OverlayValue> {
		self.entries.get(&key)
	}

	/// Returns an iterator over the stored elements.
	pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = (PropertyKeyId, &OverlayValue)> {
		self.entries.iter().map(|(key, value)| (*key, value))
	}

	/// Inserts property overlay.
	fn insert(&mut self, key: PropertyKeyId, value: OverlayValue) {
		self.entries.insert(key, value);
	}

	/// Removes property overlay.
	fn remove(&mut self, key: PropertyKeyId) {
		self.entries.remove(&key);
	}

	/// Returns whether there are no logical elements.
	fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}
}

/// Closed graph-local structural state of one global atom.
///
/// A missing topology identifies a node, whose weight is always absent. An edge
/// always owns its topology inline. The child binding is a nested graph for a
/// node and a transition graph for an edge. Only the shape-preserving
/// constructors below can create this state outside this module.
#[derive(Clone, PartialEq, Eq)]
pub struct OccurrenceState {
	/// Inline edge topology, or no topology for a node occurrence.
	topology: Option<EdgeTopology>,
	/// Graph-local edge weight; always absent for a node occurrence.
	weight: Option<Number>,
	/// Optional child graph interpreted according to the occurrence kind.
	child_graph: Option<GraphId>,
}

/// Borrowed structural projection with the same node/edge field names as the
/// original owned state, without moving or cloning incidence storage.
#[derive(Debug, Clone, Copy)]
pub enum OccurrenceStateRef<'a> {
	/// A node occurrence and its optional nested graph.
	Node {
		/// Optional nested graph borrowed from the node occurrence.
		nested_graph: &'a Option<GraphId>,
	},
	/// An edge occurrence and its inline structural payload.
	Edge {
		/// Endpoints borrowed from the existing inline topology.
		topology: &'a EdgeTopology,
		/// Optional graph-local weight borrowed from the edge occurrence.
		weight: &'a Option<Number>,
		/// Optional transition graph borrowed from the edge occurrence.
		transition_graph: &'a Option<GraphId>,
	},
}

impl OccurrenceState {
	/// Creates a node shape without allocating topology or admitting an edge
	/// weight.
	pub(crate) const fn node(nested_graph: Option<GraphId>) -> Self {
		Self {
			topology: None,
			weight: None,
			child_graph: nested_graph,
		}
	}

	/// Creates an edge shape by moving its existing topology inline.
	/// Endpoint and catalog checks remain at the existing GraphRoot boundaries.
	pub(crate) const fn edge(
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> Self {
		Self {
			topology: Some(topology),
			weight,
			child_graph: transition_graph,
		}
	}

	/// Borrows the node/edge shape without allocating or exposing mutable tags.
	pub(crate) const fn view(&self) -> OccurrenceStateRef<'_> {
		match &self.topology {
			None => OccurrenceStateRef::Node {
				nested_graph: &self.child_graph,
			},
			Some(topology) => OccurrenceStateRef::Edge {
				topology,
				weight: &self.weight,
				transition_graph: &self.child_graph,
			},
		}
	}
}

impl std::fmt::Debug for OccurrenceState {
	/// Retains the original Node/Edge Debug representation, including pretty
	/// output.
	///
	/// # Errors
	///
	/// Returns an error reported by the supplied formatter.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		std::fmt::Debug::fmt(&self.view(), f)
	}
}

// The former edge payload occupies 248 bytes on the supported 64-bit targets.
// Keep the complete owned state within that existing inline payload budget.
#[cfg(target_pointer_width = "64")]
/// Compile-time guard preserving the existing 248-byte inline occurrence-state
/// budget.
const _: () = assert!(std::mem::size_of::<OccurrenceState>() <= 248);

/// One graph-local occurrence of a database-global atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphOccurrence {
	/// Local id associated with this graph occurrence.
	local_id: AtomId,
	/// Global id associated with this graph occurrence.
	global_id: GlobalAtomId,
	/// Label associated with this graph occurrence.
	label: Option<Text>,
	/// Overlay associated with this graph occurrence.
	overlay: Option<PropertyOverlay>,
	/// State associated with this graph occurrence.
	state: OccurrenceState,
}

impl GraphOccurrence {
	/// Returns the local id result.
	pub(crate) const fn local_id(&self) -> AtomId {
		self.local_id
	}

	/// Returns the global id result.
	pub(crate) const fn global_id(&self) -> GlobalAtomId {
		self.global_id
	}

	/// Returns the label result.
	pub(crate) const fn label(&self) -> Option<&Text> {
		self.label.as_ref()
	}

	/// Returns the state result.
	pub(crate) const fn state(&self) -> &OccurrenceState {
		&self.state
	}

	/// Resolves the container binding for either node or edge owners.
	pub(crate) const fn child_graph(&self) -> Option<GraphId> {
		match self.state.view() {
			OccurrenceStateRef::Node {
				nested_graph,
			} => *nested_graph,
			OccurrenceStateRef::Edge {
				transition_graph,
				..
			} => *transition_graph,
		}
	}

	/// Returns whether this value has overlay.
	pub(crate) const fn has_overlay(&self) -> bool {
		self.overlay.is_some()
	}

	/// Returns the overlay result.
	pub(crate) const fn overlay(&self) -> Option<&PropertyOverlay> {
		self.overlay.as_ref()
	}

	/// Sets label.
	pub(crate) fn set_label(&mut self, label: Option<Text>) {
		self.label = label;
	}

	/// Replaces state.
	pub(crate) fn replace_state(&mut self, state: OccurrenceState) {
		self.state = state;
	}

	/// Replaces overlay.
	pub(crate) fn replace_overlay(&mut self, overlay: Option<PropertyOverlay>) {
		self.overlay = overlay;
	}

	/// Returns whether the inherits condition holds.
	pub(crate) fn inherits(&self, key: PropertyKeyId) -> bool {
		self.overlay.as_ref().and_then(|overlay| overlay.get(key)).is_none()
	}

	/// Sets overlay.
	pub(crate) fn set_overlay(&mut self, key: PropertyKeyId, value: OverlayValue) {
		self.overlay.get_or_insert_with(PropertyOverlay::default).insert(key, value);
	}

	/// Resets overlay.
	pub(crate) fn reset_overlay(&mut self, key: PropertyKeyId) {
		let Some(overlay) = self.overlay.as_mut() else {
			return;
		};
		overlay.remove(key);
		if overlay.is_empty() {
			self.overlay = None;
		}
	}

	/// Returns the effective value result.
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
pub struct GraphRoot {
	/// Durable logical incidence revision, independent of checkpoint LSN.
	pub(crate) tensor_revision: u64,
	/// Reference associated with this graph root.
	reference: GraphRef,
	/// Nodes associated with this graph root.
	nodes: SlotAllocator,
	/// Edges associated with this graph root.
	edges: SlotAllocator,
	/// By global associated with this graph root.
	by_global: BTreeMap<GlobalAtomId, AtomId>,
	/// Occurrences associated with this graph root.
	occurrences: BTreeMap<AtomId, GraphOccurrence>,
	/// Created at the first edge endpoint; keys retain the complete nested
	/// scope.
	edge_endpoints: Option<BTreeMap<crate::graph::EdgeEndpoint, BTreeSet<(AtomId, bool)>>>,
}

impl GraphRoot {
	/// Creates a new `GraphRoot` value.
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

	/// Returns the reference result.
	pub(crate) const fn reference(&self) -> &GraphRef {
		&self.reference
	}

	/// Returns the local id result.
	pub(crate) fn local_id(&self, global: GlobalAtomId) -> Option<AtomId> {
		self.by_global.get(&global).copied()
	}

	/// Returns the occurrence result.
	pub(crate) fn occurrence(&self, local: AtomId) -> Option<&GraphOccurrence> {
		self.occurrences.get(&local).filter(|occurrence| occurrence.local_id == local)
	}

	/// Returns the occurrence mut result.
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
			let before_topology = match before.state().view() {
				OccurrenceStateRef::Edge {
					topology,
					..
				} => Some(topology),
				OccurrenceStateRef::Node {
					..
				} => None,
			};
			let after_topology = match after.state().view() {
				OccurrenceStateRef::Edge {
					topology,
					..
				} => Some(topology),
				OccurrenceStateRef::Node {
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

	/// Attaches node.
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
			state: OccurrenceState::node(nested_graph),
		})
	}

	/// Attaches node at.
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
			state: OccurrenceState::node(nested_graph),
		})
	}

	/// Attaches edge.
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
			state: OccurrenceState::edge(topology, weight, transition_graph),
		})
	}

	/// Attaches edge at.
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
			state: OccurrenceState::edge(topology, weight, transition_graph),
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
			state: OccurrenceState::edge(topology, weight, transition_graph),
		})
	}

	/// Validates every local endpoint after two-phase durable restoration.
	pub(crate) fn validate_restored_endpoints(&self) -> FerrosGrynnResult<()> {
		for occurrence in self.occurrences.values() {
			if let OccurrenceStateRef::Edge {
				topology,
				..
			} = occurrence.state().view()
			{
				self.validate_endpoints(topology)?;
			}
		}
		Ok(())
	}

	/// Attaches graph root.
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
		if let OccurrenceStateRef::Edge {
			topology,
			..
		} = occurrence.state().view()
		{
			self.update_edge_endpoint_postings(local, topology, true);
		}
		self.by_global.insert(global, local);
		self.occurrences.insert(local, occurrence);
		Ok(local)
	}

	/// Attaches at.
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
		if let OccurrenceStateRef::Edge {
			topology,
			..
		} = occurrence.state().view()
		{
			self.update_edge_endpoint_postings(local, topology, true);
		}
		self.by_global.insert(global, local);
		self.occurrences.insert(local, occurrence);
		Ok(())
	}

	/// Validates endpoints.
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

	/// Validates state.
	pub(crate) fn validate_state(
		&self,
		local: AtomId,
		state: &OccurrenceState,
	) -> FerrosGrynnResult<()> {
		match (local.kind(), state.view()) {
			(
				Some(AtomKind::Node),
				OccurrenceStateRef::Node {
					..
				},
			) => Ok(()),
			(
				Some(AtomKind::Edge),
				OccurrenceStateRef::Edge {
					topology,
					..
				},
			) => self.validate_endpoints(topology),
			_ => Err(FerrosGrynnError::atom_kind_mismatch(
				local.to_string(),
				local.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
				match state.view() {
					OccurrenceStateRef::Node {
						..
					} => AtomKind::Node.to_string(),
					OccurrenceStateRef::Edge {
						..
					} => AtomKind::Edge.to_string(),
				},
			)),
		}
	}

	/// Detaches graph root.
	pub(crate) fn detach(&mut self, global: GlobalAtomId) -> Option<GraphOccurrence> {
		let local = self.by_global.remove(&global)?;
		let occurrence = self.occurrences.remove(&local)?;
		if let OccurrenceStateRef::Edge {
			topology,
			..
		} = occurrence.state().view()
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

	/// Appends detach plan.
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

	/// Returns the incident edges result.
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
			.filter_map(|occurrence| match occurrence.state().view() {
				OccurrenceStateRef::Edge {
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

	/// Returns the occurrences result.
	pub(crate) fn occurrences(&self) -> impl ExactSizeIterator<Item = &GraphOccurrence> + Clone {
		self.occurrences.values()
	}

	/// Returns the slot capacity result.
	pub(crate) fn slot_capacity(&self, kind: AtomKind) -> usize {
		match kind {
			AtomKind::Node => self.nodes.capacity(),
			AtomKind::Edge => self.edges.capacity(),
		}
	}

	/// Returns the slot generation result.
	pub(crate) fn slot_generation(&self, kind: AtomKind, slot: usize) -> Option<u32> {
		match kind {
			AtomKind::Node => self.nodes.generation(slot),
			AtomKind::Edge => self.edges.generation(slot),
		}
	}

	/// Restores free slot.
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
