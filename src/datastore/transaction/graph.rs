use super::{Mutation, QuerySnapshot, Transaction};
use crate::{
	datastore::{
		ConflictKey,
		GlobalAtomId,
		OccurrenceState,
		consts::{
			GRAPH_INCIDENCE_WRITE_BYTES,
			GRAPH_MEMBERSHIP_WRITE_BYTES,
			GRAPH_OVERLAY_WRITE_BYTES,
			SCHEMA_WRITE_BYTES,
		},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomRef,
		EdgeTopology,
		FrozenDag,
		FrozenForest,
		FrozenGraph,
		FrozenHypergraph,
		FrozenRootedTree,
		FrozenTree,
		GraphId,
		GraphKind,
		GraphRead,
		GraphRef,
		query_to_memory,
		validate_nested_references,
		validate_topology_constraints,
	},
	values::{AtomKind, Number, PropertyKeyId, RecordId, Text, Value},
};

#[cfg(test)]
mod tests;

impl Transaction {
	/// Attaches a global node to one graph and returns its graph-local handle.
	///
	/// # Errors
	///
	/// Returns an atom, graph, kind, duplicate-membership, read-only, or
	/// write-set-limit error.
	pub fn attach_node(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.logical_operation(|transaction| {
			transaction.attach_node_inner(graph, record_id, label, nested_graph)
		})
	}

	/// Attaches a node under the current outer operation.
	fn attach_node_inner(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		self.note_change_target(global, Some(graph), ConflictKey::GraphMembership(graph, global))?;
		let (class_id, kind) = self.atom_domain(global)?;
		if self.working.membership(graph, global).is_some() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				graph.to_string(),
				format!("atom '{record_id}' is already attached"),
			));
		}
		let additional = GRAPH_MEMBERSHIP_WRITE_BYTES
			.checked_add(GRAPH_INCIDENCE_WRITE_BYTES)
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		self.ensure_capacity(additional)?;
		let local = self.working.attach_node(graph, global, label.clone(), nested_graph)?;
		self.insert_write(
			ConflictKey::GraphMembership(graph, global),
			GRAPH_MEMBERSHIP_WRITE_BYTES,
		);
		self.insert_write(ConflictKey::GraphIncidence(graph, local), GRAPH_INCIDENCE_WRITE_BYTES);
		self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
		self.mutations.push(Mutation::AttachNode {
			graph,
			local,
			global,
			label,
			nested_graph,
		});
		Ok(local)
	}

	/// Attaches a global edge with graph-local topology and attributes.
	///
	/// # Errors
	///
	/// Returns an atom, endpoint, graph, kind, duplicate-membership,
	/// read-only, or write-set-limit error.
	pub fn attach_edge(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.logical_operation(|transaction| {
			transaction.attach_edge_inner(
				graph,
				record_id,
				label,
				topology,
				weight,
				transition_graph,
			)
		})
	}

	/// Attaches a connector under the current outer operation.
	fn attach_edge_inner(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		self.note_change_target(global, Some(graph), ConflictKey::GraphMembership(graph, global))?;
		let (class_id, kind) = self.atom_domain(global)?;
		if self.working.membership(graph, global).is_some() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				graph.to_string(),
				format!("atom '{record_id}' is already attached"),
			));
		}
		let endpoints = topology
			.endpoints()
			.filter_map(|endpoint| endpoint.local_id())
			.map(|local| {
				self.working
					.occurrence(graph, local)
					.map(|occurrence| (local, occurrence.global_id()))
					.ok_or_else(|| {
						FerrosGrynnError::atom_does_not_exist("endpoint", local.to_string())
					})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let endpoint_capability = self.prepare_edge_endpoint_capability(graph, &topology)?;
		let posting_bytes = endpoint_posting_write_bytes(&topology)?;
		let incidence_bytes = GRAPH_INCIDENCE_WRITE_BYTES
			.checked_add(posting_bytes)
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		let additional = GRAPH_MEMBERSHIP_WRITE_BYTES
			.checked_add(incidence_bytes)
			.and_then(|bytes| {
				bytes.checked_add(if endpoint_capability.is_some() {
					SCHEMA_WRITE_BYTES
				} else {
					0
				})
			})
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		let anchor = self.prepare_qualified_anchor(graph, record_id, &topology, additional)?;
		let pending = additional.saturating_add(anchor.as_ref().map_or(0, |anchor| anchor.bytes));
		self.observe_qualified_topology(graph, &topology, pending)?;
		self.ensure_capacity(pending)?;
		let local = self.working.attach_edge(
			graph,
			global,
			label.clone(),
			topology.clone(),
			weight,
			transition_graph,
		)?;
		if let Some(capability) = endpoint_capability {
			self.publish_graph_replacement(capability, SCHEMA_WRITE_BYTES)?;
		}
		for (endpoint, endpoint_global) in endpoints {
			self.record_requirement(ConflictKey::GraphMembership(graph, endpoint_global));
			self.record_read(ConflictKey::GraphIncidence(graph, endpoint));
		}
		self.insert_write(
			ConflictKey::GraphMembership(graph, global),
			GRAPH_MEMBERSHIP_WRITE_BYTES,
		);
		self.insert_write(ConflictKey::GraphIncidence(graph, local), incidence_bytes);
		self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
		self.mutations.push(Mutation::AttachEdge {
			graph,
			local,
			global,
			label,
			topology,
			weight,
			transition_graph,
		});
		self.retain_qualified_anchor(anchor);
		Ok(local)
	}

	/// Detaches one occurrence, optionally removing dependent edge occurrences
	/// in dependency order.
	///
	/// # Errors
	///
	/// Returns an atom, graph, incidence, read-only, or write-set-limit error.
	pub fn detach_occurrence(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		cascade: bool,
	) -> FerrosGrynnResult<()> {
		self.logical_operation(|transaction| {
			transaction.detach_occurrence_inner(graph, record_id, cascade)
		})
	}

	/// Preflights all cascade targets before the first occurrence is removed.
	fn detach_occurrence_inner(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		cascade: bool,
	) -> FerrosGrynnResult<()> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let plan = self
			.working
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?
			.detach_plan(global, cascade)?;
		self.detach_occurrence_plan(graph, plan)
	}

	/// Executes an already checked exact deletion set, including E-E cycles.
	pub(super) fn detach_occurrence_plan(
		&mut self,
		graph: GraphId,
		plan: Vec<super::GlobalAtomId>,
	) -> FerrosGrynnResult<()> {
		let mut additional = 0_usize;
		let mut removals = Vec::with_capacity(plan.len());
		for global in plan {
			let (class_id, kind) = self.atom_domain(global)?;
			let local = self.working.membership(graph, global).ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
			})?;
			let occurrence = self.working.occurrence(graph, local).ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("occurrence", local.to_string())
			})?;
			let has_overlay = occurrence.has_overlay();
			let posting_bytes = match occurrence.state() {
				OccurrenceState::Edge {
					topology,
					..
				} => endpoint_posting_write_bytes(topology)?,
				OccurrenceState::Node {
					..
				} => 0,
			};
			additional = additional
				.checked_add(posting_bytes)
				.ok_or_else(|| self.limit_error(usize::MAX))?;
			for (key, cost) in [
				(ConflictKey::GraphMembership(graph, global), GRAPH_MEMBERSHIP_WRITE_BYTES),
				(ConflictKey::GraphIncidence(graph, local), GRAPH_INCIDENCE_WRITE_BYTES),
			] {
				if !self.writes.contains_key(&key) {
					additional =
						additional.checked_add(cost).ok_or_else(|| self.limit_error(usize::MAX))?;
				}
			}
			if has_overlay && !self.writes.contains_key(&ConflictKey::GraphOverlay(graph, global)) {
				additional = additional
					.checked_add(GRAPH_OVERLAY_WRITE_BYTES)
					.ok_or_else(|| self.limit_error(usize::MAX))?;
			}
			removals.push((global, local, has_overlay, class_id, kind, posting_bytes));
		}
		self.ensure_capacity(additional)?;
		for (global, _, _, _, _, _) in &removals {
			self.note_change_target(
				*global,
				Some(graph),
				ConflictKey::GraphMembership(graph, *global),
			)?;
		}
		self.ensure_capacity(additional)?;
		for (global, local, has_overlay, class_id, kind, posting_bytes) in removals {
			self.working.detach(graph, global)?.ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
			})?;
			self.insert_write(
				ConflictKey::GraphMembership(graph, global),
				GRAPH_MEMBERSHIP_WRITE_BYTES,
			);
			self.insert_write(
				ConflictKey::GraphIncidence(graph, local),
				GRAPH_INCIDENCE_WRITE_BYTES,
			);
			self.write_bytes += posting_bytes;
			if has_overlay {
				self.insert_write(
					ConflictKey::GraphOverlay(graph, global),
					GRAPH_OVERLAY_WRITE_BYTES,
				);
			}
			self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
			self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
			self.mutations.push(Mutation::Detach {
				graph,
				global,
			});
		}
		Ok(())
	}

	/// Validates the current transaction-private topology against its declared
	/// graph family.
	pub(crate) fn validate_graph_family(&mut self, graph: &GraphRef) -> FerrosGrynnResult<()> {
		let snapshot = self.query_snapshot()?;
		self.validate_graph_family_snapshot(&snapshot, graph)
	}

	pub(super) fn validate_graph_family_in_root(
		&self,
		root: std::sync::Arc<crate::datastore::DatabaseRoot>,
		graph: &GraphRef,
	) -> FerrosGrynnResult<()> {
		self.validate_graph_family_snapshot(
			&QuerySnapshot {
				root,
				physical: None,
			},
			graph,
		)
	}

	fn validate_graph_family_snapshot(
		&self,
		snapshot: &QuerySnapshot,
		graph: &GraphRef,
	) -> FerrosGrynnResult<()> {
		let schema = snapshot
			.root
			.schema
			.graph(graph.id())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.name().to_string()))?;
		let view = self.query_graph(&snapshot, graph)?;
		match graph.kind() {
			GraphKind::Graph => Ok(()),
			GraphKind::Dag => query_to_memory::<FrozenDag, _>(&view).map(|_graph| ()),
			GraphKind::Forest => query_to_memory::<FrozenForest, _>(&view).map(|_graph| ()),
			GraphKind::Tree => query_to_memory::<FrozenTree, _>(&view).map(|_graph| ()),
			GraphKind::RootedTree => {
				let materialized = query_to_memory::<FrozenGraph, _>(&view)?;
				if let Some(root) = schema.root() {
					let root = view
						.atom_id(&AtomRef::new(AtomKind::Node, root.clone()))?
						.ok_or_else(|| {
							FerrosGrynnError::graph_constraint_violation(
								graph.to_string(),
								"the declared rooted-tree root is not a root-graph node",
							)
						})?;
					FrozenRootedTree::try_from_graph_with_root(materialized, root).map(|_graph| ())
				} else {
					FrozenRootedTree::try_from(materialized).map(|_graph| ())
				}
			}
			GraphKind::Hypergraph => query_to_memory::<FrozenHypergraph, _>(&view).map(|_graph| ()),
			GraphKind::Metagraph => validate_nested_references(&view),
			GraphKind::Subgraph => Err(FerrosGrynnError::invalid_argument(
				"a catalog graph cannot use the derived SUBGRAPH family",
			)),
		}?;
		validate_topology_constraints(&view, schema.topology_constraints())
	}

	/// Resolves one graph-local occurrence of a global atom.
	///
	/// # Errors
	///
	/// Returns a snapshot-age or read-committed refresh error.
	pub fn membership(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
	) -> FerrosGrynnResult<Option<AtomId>> {
		self.prepare_read()?;
		let Some(global) = self.working.resolve_atom(record_id) else {
			return Ok(None);
		};
		let local = self.working.membership(graph, global);
		self.record_read(ConflictKey::BaseAtom(global));
		self.record_read(ConflictKey::GraphMembership(graph, global));
		Ok(local)
	}

	/// Stores a graph-local property override.
	///
	/// # Errors
	///
	/// Returns an atom, membership, read-only, snapshot-age, or resource error.
	pub fn set_overlay(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<()> {
		self.logical_operation(|transaction| {
			transaction.change_overlay(graph, record_id, key, Some(value))
		})
	}

	/// Computes and stores the canonical system-managed LevelPath overlay.
	pub(crate) fn store_level_path(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
	) -> FerrosGrynnResult<Value> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let value = self.working.level_path_value(graph, global)?;
		let key = self.intern_property(crate::datastore::LEVEL_PATH_PROPERTY)?;
		self.set_overlay(graph, record_id, key, value.clone())?;
		Ok(value)
	}

	/// Hides an inherited base property in one graph.
	///
	/// # Errors
	///
	/// Returns an atom, membership, read-only, snapshot-age, or resource error.
	pub fn hide_property(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<()> {
		self.logical_operation(|transaction| {
			transaction.change_overlay(graph, record_id, key, None)
		})
	}

	/// Removes a graph-local property decision and resumes base inheritance.
	///
	/// # Errors
	///
	/// Returns an atom, membership, read-only, snapshot-age, or resource error.
	pub fn reset_property(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<()> {
		self.logical_operation(|transaction| {
			transaction.reset_property_inner(graph, record_id, key)
		})
	}

	/// Records inherited no-ops before the overlay equality fast path.
	fn reset_property_inner(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<()> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let local = self.require_membership(graph, global)?;
		self.note_change_target(global, Some(graph), ConflictKey::GraphOverlay(graph, global))?;
		if self.working.inherits_property(graph, global, key) {
			return Ok(());
		}
		self.reserve_overlay_write(graph, global)?;
		self.working.reset_property(graph, local, key)?;
		let (class_id, _kind) = self.atom_domain(global)?;
		self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		self.mutations.push(Mutation::ResetProperty {
			graph,
			local,
			key,
		});
		Ok(())
	}

	/// Changes the optional label of one graph occurrence.
	///
	/// # Errors
	///
	/// Returns an atom, membership, read-only, snapshot-age, or resource error.
	pub fn set_occurrence_label(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		label: Option<Text>,
	) -> FerrosGrynnResult<()> {
		self.logical_operation(|transaction| {
			transaction.set_occurrence_label_inner(graph, record_id, label)
		})
	}

	/// Records a label call before equal labels return without mutations.
	fn set_occurrence_label_inner(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		label: Option<Text>,
	) -> FerrosGrynnResult<()> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let local = self.require_membership(graph, global)?;
		self.note_change_target(global, Some(graph), ConflictKey::GraphIncidence(graph, local))?;
		if self
			.working
			.occurrence(graph, local)
			.is_some_and(|occurrence| occurrence.label() == label.as_ref())
		{
			return Ok(());
		}
		if !self.writes.contains_key(&ConflictKey::GraphIncidence(graph, local)) {
			self.ensure_capacity(GRAPH_INCIDENCE_WRITE_BYTES)?;
		}
		self.working.set_occurrence_label(graph, local, label.clone())?;
		self.insert_write(ConflictKey::GraphIncidence(graph, local), GRAPH_INCIDENCE_WRITE_BYTES);
		let (class_id, kind) = self.atom_domain(global)?;
		self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
		self.mutations.push(Mutation::SetOccurrenceLabel {
			graph,
			local,
			label,
		});
		Ok(())
	}

	pub(crate) fn replace_occurrence_state(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		state: OccurrenceState,
	) -> FerrosGrynnResult<()> {
		self.logical_operation(|transaction| {
			transaction.replace_occurrence_state_inner(graph, record_id, state)
		})
	}

	/// Records topology/weight calls before equal states are skipped.
	fn replace_occurrence_state_inner(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		state: OccurrenceState,
	) -> FerrosGrynnResult<()> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let local = self.require_membership(graph, global)?;
		self.note_change_target(global, Some(graph), ConflictKey::GraphIncidence(graph, local))?;
		if self
			.working
			.occurrence(graph, local)
			.is_some_and(|occurrence| occurrence.state() == &state)
		{
			// Equal physical slots may explicitly select a new logical child
			// route.
			if let OccurrenceState::Edge {
				topology,
				..
			} = &state
			{
				if topology.is_cross_level() {
					self.observe_qualified_topology(graph, topology, 0)?;
					let anchor = self.prepare_qualified_anchor(graph, record_id, topology, 0)?;
					self.retain_qualified_anchor(anchor);
				}
			}
			return Ok(());
		}
		let endpoint_capability = if let OccurrenceState::Edge {
			topology,
			..
		} = &state
		{
			self.prepare_edge_endpoint_capability(graph, topology)?
		} else {
			None
		};
		let posting_bytes = if let OccurrenceState::Edge {
			topology,
			..
		} = &state
		{
			endpoint_posting_write_bytes(topology)?
		} else {
			0
		};
		let incidence_bytes = GRAPH_INCIDENCE_WRITE_BYTES
			.checked_add(posting_bytes)
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		let nested = match &state {
			OccurrenceState::Node {
				nested_graph,
			} => *nested_graph,
			OccurrenceState::Edge {
				transition_graph,
				..
			} => *transition_graph,
		};
		if let Some(nested) = nested {
			if self.working.graph(nested).is_none() {
				return Err(FerrosGrynnError::graph_does_not_exist(nested.to_string()));
			}
		}
		let pending = if !self.writes.contains_key(&ConflictKey::GraphIncidence(graph, local)) {
			incidence_bytes.saturating_add(if endpoint_capability.is_some() {
				SCHEMA_WRITE_BYTES
			} else {
				0
			})
		} else {
			posting_bytes.saturating_add(if endpoint_capability.is_some() {
				SCHEMA_WRITE_BYTES
			} else {
				0
			})
		};
		let anchor = if let OccurrenceState::Edge {
			topology,
			..
		} = &state
		{
			self.prepare_qualified_anchor(graph, record_id, topology, pending)?
		} else {
			None
		};
		let pending = pending.saturating_add(anchor.as_ref().map_or(0, |anchor| anchor.bytes));
		if let OccurrenceState::Edge {
			topology,
			..
		} = &state
		{
			self.observe_qualified_topology(graph, topology, pending)?;
		}
		self.ensure_capacity(pending)?;
		if let OccurrenceState::Edge {
			topology,
			..
		} = &state
		{
			for endpoint in topology.endpoints().filter_map(|endpoint| endpoint.local_id()) {
				let endpoint_global = self
					.working
					.occurrence(graph, endpoint)
					.map(crate::datastore::GraphOccurrence::global_id)
					.ok_or_else(|| {
						FerrosGrynnError::atom_does_not_exist("endpoint", endpoint.to_string())
					})?;
				self.record_requirement(ConflictKey::GraphMembership(graph, endpoint_global));
				self.record_read(ConflictKey::GraphIncidence(graph, endpoint));
			}
		}
		self.working.replace_occurrence_state(graph, local, state.clone())?;
		if let Some(capability) = endpoint_capability {
			self.publish_graph_replacement(capability, SCHEMA_WRITE_BYTES)?;
		}
		if self.writes.contains_key(&ConflictKey::GraphIncidence(graph, local)) {
			self.write_bytes += posting_bytes;
		}
		self.insert_write(ConflictKey::GraphIncidence(graph, local), incidence_bytes);
		let (class_id, kind) = self.atom_domain(global)?;
		self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
		self.mutations.push(Mutation::ReplaceOccurrenceState {
			graph,
			local,
			state,
		});
		self.retain_qualified_anchor(anchor);
		Ok(())
	}

	/// Reads one effective property after applying graph-local override/hidden
	/// semantics to the base atom.
	///
	/// # Errors
	///
	/// Returns an atom, membership, snapshot-age, or refresh error.
	pub fn effective_property(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		self.prepare_read()?;
		let global = self.require_atom(record_id)?;
		let local = self.require_membership(graph, global)?;
		let value = self.working.effective_property(graph, local, key)?.cloned();
		self.record_read(ConflictKey::BaseAtom(global));
		self.record_read(ConflictKey::GraphMembership(graph, global));
		self.record_read(ConflictKey::GraphOverlay(graph, global));
		Ok(value)
	}

	fn change_overlay(
		&mut self,
		graph: GraphId,
		record_id: &RecordId,
		key: PropertyKeyId,
		value: Option<Value>,
	) -> FerrosGrynnResult<()> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let local = self.require_membership(graph, global)?;
		self.note_change_target(global, Some(graph), ConflictKey::GraphOverlay(graph, global))?;
		if let Some(value) = &value {
			self.intern_value_keys(value)?;
		}
		self.reserve_overlay_write(graph, global)?;
		match value {
			Some(value) => {
				self.working.set_overlay(graph, local, key, value.clone())?;
				self.mutations.push(Mutation::SetOverlay {
					graph,
					local,
					key,
					value,
				});
			}
			None => {
				self.working.hide_property(graph, local, key)?;
				self.mutations.push(Mutation::HideProperty {
					graph,
					local,
					key,
				});
			}
		}
		let (class_id, _kind) = self.atom_domain(global)?;
		self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		Ok(())
	}

	fn reserve_overlay_write(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
	) -> FerrosGrynnResult<()> {
		let key = ConflictKey::GraphOverlay(graph, global);
		if !self.writes.contains_key(&key) {
			self.ensure_capacity(GRAPH_OVERLAY_WRITE_BYTES)?;
		}
		self.record_requirement(ConflictKey::GraphMembership(graph, global));
		self.insert_write(key, GRAPH_OVERLAY_WRITE_BYTES);
		Ok(())
	}

	/// Records the exact membership prerequisite for replay and conflict
	/// validation.
	pub(super) fn require_membership(
		&self,
		graph: GraphId,
		global: GlobalAtomId,
	) -> FerrosGrynnResult<AtomId> {
		self.working
			.membership(graph, global)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string()))
	}

	/// Reserves the shared identity before topology publication without
	/// exposing an artifact.
	fn prepare_edge_endpoint_capability(
		&self,
		graph: GraphId,
		topology: &EdgeTopology,
	) -> FerrosGrynnResult<Option<crate::datastore::GraphSchema>> {
		let schema = self
			.working
			.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		if topology.edge_endpoint_sides().next().is_none() || schema.edge_endpoint_index().is_some()
		{
			return Ok(None);
		}
		let index = crate::datastore::SystemIndex::new(
			crate::datastore::IndexId::from_schema_id(self.store.inner.allocate_schema_id()?),
			crate::datastore::SystemIndexKind::EdgeEndpoint,
		);
		schema
			.replace_indexes(schema.indexes().cloned().collect())?
			.with_edge_endpoint_index(index)
			.map(Some)
	}

	/// Qualified owner bindings and terminals are structural prerequisites even
	/// under Snapshot isolation; their tracked allocations precede mutation.
	fn observe_qualified_topology(
		&mut self,
		graph: GraphId,
		topology: &EdgeTopology,
		pending: usize,
	) -> FerrosGrynnResult<()> {
		let count = topology.endpoints().try_fold(0_usize, |count, endpoint| {
			let crate::graph::EdgeEndpoint::Nested(qualified) = endpoint else {
				return Ok(count);
			};
			if qualified.scope().len() > 64 {
				return Err(FerrosGrynnError::invalid_argument(
					"edge endpoint scope exceeds 64 levels",
				));
			}
			count
				.checked_add(qualified.scope().len() + 1)
				.ok_or_else(|| self.limit_error(usize::MAX))
		})?;
		if count == 0 {
			return Ok(());
		}
		let scratch = count
			.checked_mul(size_of::<(GraphId, AtomId, GlobalAtomId)>())
			.ok_or_else(|| self.limit_error(usize::MAX))?;
		self.ensure_capacity(
			count
				.checked_mul(576)
				.and_then(|bytes| bytes.checked_add(scratch))
				.and_then(|bytes| bytes.checked_add(pending))
				.ok_or_else(|| self.limit_error(usize::MAX))?,
		)?;
		let mut dependencies = Vec::new();
		dependencies
			.try_reserve_exact(count)
			.map_err(|error| self.limit_error(scratch).with_technical_source(error))?;
		for endpoint in topology.endpoints().filter(|endpoint| endpoint.is_cross_level()) {
			let (terminal_graph, terminal) = self
				.working
				.resolve_incidence_endpoint(graph, endpoint, |owner_graph, owner| {
					dependencies.push((owner_graph, owner.local_id(), owner.global_id()));
					Ok(())
				})
				.map_err(|error| {
					FerrosGrynnError::graph_constraint_violation(
						graph.to_string(),
						"qualified endpoint owner chain or terminal is absent",
					)
					.with_cause(error)
				})?;
			dependencies.push((terminal_graph, terminal.local_id(), terminal.global_id()));
		}
		for (graph, local, global) in dependencies {
			for key in [
				ConflictKey::GraphDefinition(graph),
				ConflictKey::GraphMembership(graph, global),
				ConflictKey::GraphIncidence(graph, local),
			] {
				let bytes = usize::from(!self.requirements.contains_key(&key)) * 128
					+ usize::from(!self.reads.lock().contains_key(&key)) * 64;
				self.record_requirement(key);
				self.record_read(key);
				self.write_bytes += bytes;
			}
		}
		Ok(())
	}
}

/// Admits exact posting keys, connector identities, and B-tree node overhead
/// before mutation.
pub(super) fn endpoint_posting_write_bytes(topology: &EdgeTopology) -> FerrosGrynnResult<usize> {
	let mut bytes = 0_usize;
	for (endpoint, _) in topology.edge_endpoint_sides() {
		let depth = match endpoint {
			crate::graph::EdgeEndpoint::Local(_) => 0,
			crate::graph::EdgeEndpoint::Nested(qualified) => qualified.scope().len(),
		};
		if depth > 64 {
			return Err(FerrosGrynnError::invalid_argument(
				"edge endpoint scope exceeds 64 levels",
			));
		}
		bytes = bytes.checked_add(128 + depth * size_of::<AtomId>()).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("edge endpoint posting bytes overflow")
		})?;
	}
	Ok(bytes)
}
