use std::collections::{BTreeMap, BTreeSet};

use super::Transaction;
use crate::{
	datastore::{GlobalAtomId, OccurrenceState, OverlayValue},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		DirtyTarget,
		EdgeRead,
		EdgeTopology,
		GraphId,
		GraphRead,
		GraphWriteBack,
		MaterializationOrigin,
		NodeRead,
		remap_topology,
	},
	values::{Number, PropertyKeyId, PropertyMap, RecordId, Text, Value},
};

struct DesiredNode {
	local: AtomId,
	record: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
	nested_graph: Option<GraphId>,
}

struct DesiredEdge {
	local: AtomId,
	record: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
	topology: EdgeTopology,
	weight: Option<Number>,
	transition_graph: Option<GraphId>,
}

enum OverlayAction {
	Set(PropertyKeyId, Value),
	Hide(PropertyKeyId),
	Reset(PropertyKeyId),
}

impl<Reader: GraphRead> GraphWriteBack<Reader> for Transaction {
	fn flush(
		&mut self,
		graph: &Reader,
		origin: &MaterializationOrigin,
		dirty: &[DirtyTarget],
	) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|transaction| transaction.flush_materialized(graph, origin, dirty))
	}
}

impl Transaction {
	fn flush_materialized<Reader: GraphRead>(
		&mut self,
		graph: &Reader,
		origin: &MaterializationOrigin,
		dirty: &[DirtyTarget],
	) -> FerrosGrynnResult<()> {
		if dirty.iter().any(|target| matches!(target, DirtyTarget::Graph)) {
			return Err(FerrosGrynnError::query_feature_not_supported("graph-property write-back"));
		}
		let reference =
			origin.graph_ref().ok_or_else(FerrosGrynnError::graph_write_back_unavailable)?;
		let current =
			self.working.schema.graph(reference.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
			})?;
		if current.reference() != reference || origin.identity() != graph_identity(reference.id()) {
			return Err(FerrosGrynnError::graph_write_back_unavailable());
		}
		if origin.snapshot().timestamp() > self.snapshot.commit_ts {
			return Err(FerrosGrynnError::invalid_argument(
				"materialization origin is newer than the write-back transaction snapshot",
			));
		}

		let (nodes, edges, removed) = collect_delta(graph, origin, dirty)?;
		self.detach_removed(reference.id(), &removed)?;
		let mut locals = BTreeMap::new();
		for node in &nodes {
			self.ensure_writeback_atom(&node.record, AtomKind::Node)?;
			let local = self.ensure_writeback_node(reference.id(), node)?;
			locals.insert(node.local, local);
		}
		for edge in &edges {
			self.ensure_writeback_atom(&edge.record, AtomKind::Edge)?;
			let local = self.ensure_writeback_edge(reference.id(), edge)?;
			locals.insert(edge.local, local);
		}

		for node in &nodes {
			self.set_occurrence_label(reference.id(), &node.record, node.label.clone())?;
			self.replace_occurrence_state(reference.id(), &node.record, OccurrenceState::Node {
				nested_graph: node.nested_graph,
			})?;
			self.write_effective_properties(reference.id(), &node.record, &node.properties)?;
		}
		for edge in &edges {
			resolve_topology_locals(self, reference.id(), origin, &edge.topology, &mut locals)?;
			let topology = remap_topology(&edge.topology, &locals)?;
			self.set_occurrence_label(reference.id(), &edge.record, edge.label.clone())?;
			self.replace_occurrence_state(reference.id(), &edge.record, OccurrenceState::Edge {
				topology,
				weight: edge.weight,
				transition_graph: edge.transition_graph,
			})?;
			self.write_effective_properties(reference.id(), &edge.record, &edge.properties)?;
		}
		self.validate_graph_family(reference)
	}

	fn ensure_writeback_atom(
		&mut self,
		record: &RecordId,
		expected: AtomKind,
	) -> FerrosGrynnResult<GlobalAtomId> {
		if let Some(global) = self.working.resolve_atom(record) {
			let actual =
				self.working.atom(global).map(crate::datastore::BaseAtom::kind).ok_or_else(
					|| FerrosGrynnError::atom_does_not_exist("global", record.to_string()),
				)?;
			if actual != expected {
				return Err(FerrosGrynnError::atom_kind_mismatch(
					record.to_string(),
					actual.to_string(),
					expected.to_string(),
				));
			}
			return Ok(global);
		}
		self.create_atom(record.clone(), PropertyMap::new())?;
		self.working.resolve_atom(record).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("created atom is absent from the primary index")
		})
	}

	fn ensure_writeback_node(
		&mut self,
		graph: GraphId,
		node: &DesiredNode,
	) -> FerrosGrynnResult<AtomId> {
		let global = self.working.resolve_atom(&node.record).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("global", node.record.to_string())
		})?;
		match self.working.membership(graph, global) {
			Some(local) => Ok(local),
			None => self.attach_node(graph, &node.record, node.label.clone(), node.nested_graph),
		}
	}

	fn ensure_writeback_edge(
		&mut self,
		graph: GraphId,
		edge: &DesiredEdge,
	) -> FerrosGrynnResult<AtomId> {
		let global = self.working.resolve_atom(&edge.record).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("global", edge.record.to_string())
		})?;
		match self.working.membership(graph, global) {
			Some(local) => Ok(local),
			None => self.attach_edge(
				graph,
				&edge.record,
				edge.label.clone(),
				EdgeTopology::undirected(std::iter::empty::<AtomId>()),
				edge.weight,
				edge.transition_graph,
			),
		}
	}

	fn detach_removed(
		&mut self,
		graph: GraphId,
		removed: &[(AtomKind, RecordId)],
	) -> FerrosGrynnResult<()> {
		for (_kind, record) in removed
			.iter()
			.filter(|(kind, _record)| *kind == AtomKind::Edge)
			.chain(removed.iter().filter(|(kind, _record)| *kind == AtomKind::Node))
		{
			let Some(global) = self.working.resolve_atom(record) else {
				return Err(FerrosGrynnError::atom_does_not_exist("global", record.to_string()));
			};
			if self.working.membership(graph, global).is_some() {
				self.detach_occurrence(graph, record, true)?;
			}
		}
		Ok(())
	}

	fn write_effective_properties(
		&mut self,
		graph: GraphId,
		record: &RecordId,
		desired: &PropertyMap,
	) -> FerrosGrynnResult<()> {
		let global = self
			.working
			.resolve_atom(record)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", record.to_string()))?;
		let local = self.working.membership(graph, global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", record.to_string())
		})?;
		let base =
			self.working.atom(global).map(|atom| atom.properties().clone()).ok_or_else(|| {
				FerrosGrynnError::atom_does_not_exist("global", record.to_string())
			})?;
		let overlay = self
			.working
			.occurrence(graph, local)
			.and_then(crate::datastore::GraphOccurrence::overlay)
			.cloned();
		let keys = base
			.iter()
			.map(|(key, _value)| key)
			.chain(desired.iter().map(|(key, _value)| key))
			.chain(
				overlay
					.as_ref()
					.into_iter()
					.flat_map(|overlay| overlay.iter().map(|(key, _value)| key)),
			)
			.collect::<BTreeSet<_>>();
		let actions = keys
			.into_iter()
			.filter_map(|key| {
				overlay_action(key, base.get(key), desired.get(key), overlay.as_ref())
			})
			.collect::<Vec<_>>();
		for action in actions {
			match action {
				OverlayAction::Set(key, value) => self.set_overlay(graph, record, key, value)?,
				OverlayAction::Hide(key) => self.hide_property(graph, record, key)?,
				OverlayAction::Reset(key) => self.reset_property(graph, record, key)?,
			}
		}
		Ok(())
	}
}

fn collect_delta<Reader: GraphRead>(
	graph: &Reader,
	origin: &MaterializationOrigin,
	dirty: &[DirtyTarget],
) -> FerrosGrynnResult<(Vec<DesiredNode>, Vec<DesiredEdge>, Vec<(AtomKind, RecordId)>)> {
	let mut nodes = Vec::new();
	let mut edges = Vec::new();
	let mut removed = Vec::new();
	for target in dirty {
		let DirtyTarget::Atom(local) = target else {
			continue;
		};
		if let Some(node) = graph.node(*local)? {
			let record =
				checked_record(origin, *local, AtomKind::Node, node.record_id()?.into_owned())?;
			nodes.push(DesiredNode {
				local: *local,
				record,
				label: node.label_text()?.map(|label| label.into_owned()),
				properties: node.properties()?.into_owned(),
				nested_graph: node.inner_graph_ref()?.map(|graph| graph.id()),
			});
			continue;
		}
		if let Some(edge) = graph.edge(*local)? {
			let record =
				checked_record(origin, *local, AtomKind::Edge, edge.record_id()?.into_owned())?;
			edges.push(DesiredEdge {
				local: *local,
				record,
				label: edge.label_text()?.map(|label| label.into_owned()),
				properties: edge.properties()?.into_owned(),
				topology: edge.topology()?.into_owned(),
				weight: edge.weight(),
				transition_graph: edge.transition_graph_ref()?.map(|graph| graph.id()),
			});
			continue;
		}
		if let Some(source) = origin.source_atom(*local) {
			removed.push((source.kind(), source.id().clone()));
		}
	}
	Ok((nodes, edges, removed))
}

fn checked_record(
	origin: &MaterializationOrigin,
	local: AtomId,
	kind: AtomKind,
	record: RecordId,
) -> FerrosGrynnResult<RecordId> {
	let Some(source) = origin.source_atom(local) else {
		return Ok(record);
	};
	if source.kind() != kind || source.id() != &record {
		return Err(FerrosGrynnError::invalid_argument(
			"materialized atom identity differs from its immutable origin mapping",
		));
	}
	Ok(record)
}

fn resolve_topology_locals(
	transaction: &Transaction,
	graph: GraphId,
	origin: &MaterializationOrigin,
	topology: &EdgeTopology,
	locals: &mut BTreeMap<AtomId, AtomId>,
) -> FerrosGrynnResult<()> {
	for materialized in topology.endpoints().filter_map(|endpoint| match endpoint {
		crate::graph::EdgeEndpoint::Local(local) => Some(*local),
		crate::graph::EdgeEndpoint::Nested(qualified) => qualified.scope().first().copied(),
	}) {
		if locals.contains_key(&materialized) {
			continue;
		}
		let source = origin.source_atom(materialized).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("materialized endpoint", materialized.to_string())
		})?;
		let global = transaction.working.resolve_atom(source.id()).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("global endpoint", source.id().to_string())
		})?;
		let local = transaction.working.membership(graph, global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("graph endpoint", source.id().to_string())
		})?;
		locals.insert(materialized, local);
	}
	Ok(())
}

fn overlay_action(
	key: PropertyKeyId,
	base: Option<&Value>,
	desired: Option<&Value>,
	overlay: Option<&crate::datastore::PropertyOverlay>,
) -> Option<OverlayAction> {
	let current = overlay.and_then(|overlay| overlay.get(key));
	match (base, desired, current) {
		(Some(base), Some(desired), Some(_)) if base == desired => Some(OverlayAction::Reset(key)),
		(Some(base), Some(desired), None) if base == desired => None,
		(_, Some(desired), Some(OverlayValue::Value(current))) if current == desired => None,
		(_, Some(desired), _) => Some(OverlayAction::Set(key, desired.clone())),
		(Some(_), None, Some(OverlayValue::Hidden)) => None,
		(Some(_), None, _) => Some(OverlayAction::Hide(key)),
		(None, None, Some(_)) => Some(OverlayAction::Reset(key)),
		(None, None, None) => None,
	}
}

fn graph_identity(graph: GraphId) -> crate::values::Uuid {
	uuid::Uuid::from_u128(u128::from(graph.get())).into()
}
