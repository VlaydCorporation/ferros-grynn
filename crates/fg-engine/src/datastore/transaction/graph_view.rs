use std::{
	borrow::Cow,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use fg_runtime::__internal::EdgeEndpointInternal as _;

use super::{QuerySnapshot, Transaction};
use crate::{
	datastore::{
		ConflictKey,
		DatabaseRoot,
		GraphOccurrence,
		GraphRoot,
		OccurrenceStateRef,
		OverlayValue,
		occurrence::PropertyOverlay,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		EdgeRead,
		EdgeTopology,
		GraphRead,
		GraphRef,
		GraphRefReader,
		GraphStorageMode,
		NodeRead,
		SnapshotId,
	},
	values::{Number, PropertyMap, RecordId, Text, Uuid},
};

/// Snapshot-bound normalized graph view over global atoms and graph-local
/// occurrences.
pub struct TransactionGraphView<'tx> {
	/// Optional SSI observer; maintenance captures retain only the immutable
	/// root.
	transaction: Option<&'tx Transaction>,
	/// Root associated with this transaction graph view.
	root: Arc<DatabaseRoot>,
	/// Graph associated with this transaction graph view.
	graph: Arc<GraphRoot>,
	/// Reference associated with this transaction graph view.
	reference: GraphRef,
	/// Snapshot associated with this transaction graph view.
	snapshot: SnapshotId,
	/// Last recorded associated with this transaction graph view.
	last_recorded: AtomicU64,
}

/// Snapshot-borrowed effective node projection returned by a transaction graph
/// view.
pub struct TransactionNodeRead<'view> {
	/// Identifier used to locate this value within its owning subsystem.
	id: AtomId,
	/// Stable record identifier of the associated atom.
	record_id: &'view RecordId,
	/// Label associated with this transaction node read.
	label: Option<&'view Text>,
	/// Properties associated with this transaction node read.
	properties: EffectiveProperties<'view>,
	/// Inner graph associated with this transaction node read.
	inner_graph: Option<&'view GraphRef>,
}

impl NodeRead for TransactionNodeRead<'_> {
	/// Returns the stored id of this `TransactionNodeRead<'_>`.
	fn id(&self) -> AtomId {
		self.id
	}

	/// Records id for `TransactionNodeRead<'_>`.
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	/// Returns label text for `TransactionNodeRead<'_>`.
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	/// Returns properties for `TransactionNodeRead<'_>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(self.properties.resolve())
	}

	/// Returns inner graph ref for `TransactionNodeRead<'_>`.
	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.inner_graph.map(Cow::Borrowed))
	}
}

/// Snapshot-borrowed effective edge projection returned by a transaction graph
/// view.
pub struct TransactionEdgeRead<'view> {
	/// Identifier used to locate this value within its owning subsystem.
	id: AtomId,
	/// Stable record identifier of the associated atom.
	record_id: &'view RecordId,
	/// Topology associated with this transaction edge read.
	topology: &'view EdgeTopology,
	/// Label associated with this transaction edge read.
	label: Option<&'view Text>,
	/// Properties associated with this transaction edge read.
	properties: EffectiveProperties<'view>,
	/// Weight associated with this transaction edge read.
	weight: Option<Number>,
	/// Transition graph associated with this transaction edge read.
	transition_graph: Option<&'view GraphRef>,
}

impl EdgeRead for TransactionEdgeRead<'_> {
	/// Returns the stored id of this `TransactionEdgeRead<'_>`.
	fn id(&self) -> AtomId {
		self.id
	}

	/// Records id for `TransactionEdgeRead<'_>`.
	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	/// Returns topology for `TransactionEdgeRead<'_>`.
	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>> {
		Ok(Cow::Borrowed(self.topology))
	}

	/// Returns label text for `TransactionEdgeRead<'_>`.
	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	/// Returns properties for `TransactionEdgeRead<'_>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(self.properties.resolve())
	}

	/// Returns the stored weight of this `TransactionEdgeRead<'_>`.
	fn weight(&self) -> Option<Number> {
		self.weight
	}

	/// Returns transition graph ref for `TransactionEdgeRead<'_>`.
	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.transition_graph.map(Cow::Borrowed))
	}
}

/// Metadata readers retain property references; only properties() materializes
/// an effective overlay. Tensor/topology callers never clone unrelated values.
struct EffectiveProperties<'a> {
	/// Borrowed original base values; metadata reads do not copy them.
	base: &'a PropertyMap,
	/// Sparse effective changes, materialized only on explicit property access.
	overlay: Option<&'a PropertyOverlay>,
}

impl EffectiveProperties<'_> {
	/// Borrows the base or applies the sparse overlay to an owned value map.
	fn resolve(&self) -> Cow<'_, PropertyMap> {
		let Some(overlay) = self.overlay else {
			return Cow::Borrowed(self.base);
		};
		let mut properties = self.base.clone();
		for (key, value) in overlay.iter() {
			match value {
				OverlayValue::Value(value) => {
					properties.insert(key, value.clone());
				}
				OverlayValue::Hidden => {
					properties.remove(key);
				}
			}
		}
		Cow::Owned(properties)
	}
}

impl Transaction {
	/// Opens the exact graph version captured by a query snapshot.
	pub(crate) fn query_graph<'tx>(
		&'tx self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<TransactionGraphView<'tx>> {
		self.ensure_live()?;
		let graph =
			snapshot.root.graphs.get(&reference.id()).cloned().ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
			})?;
		if graph.reference() != reference {
			return Err(FerrosGrynnError::invalid_storage_file(
				"query graph reference disagrees with the snapshot catalog",
			));
		}
		Ok(TransactionGraphView {
			transaction: Some(self),
			root: snapshot.root.clone(),
			graph,
			reference: reference.clone(),
			snapshot: SnapshotId::new(snapshot.root.commit_ts, self.id),
			last_recorded: AtomicU64::new(AtomId::NULL.raw()),
		})
	}
}

impl GraphRefReader for Transaction {
	type View<'tx>
		= TransactionGraphView<'tx>
	where
		Self: 'tx;

	/// Reads graph for `Transaction`.
	fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>> {
		self.ensure_live()?;
		let snapshot = QuerySnapshot {
			root: Arc::new(self.working.clone()),
			physical: None,
		};
		self.query_graph(&snapshot, reference)
	}
}

impl TransactionGraphView<'_> {
	/// Opens an immutable maintenance/recovery root through the same graph
	/// reader.
	pub(crate) fn for_root(
		root: Arc<DatabaseRoot>,
		reference: &GraphRef,
		snapshot: SnapshotId,
	) -> FerrosGrynnResult<Self> {
		let graph =
			root.graphs.get(&reference.id()).cloned().ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
			})?;
		if graph.reference() != reference {
			return Err(FerrosGrynnError::invalid_storage_file(
				"tensor graph reference differs from root",
			));
		}
		Ok(Self {
			transaction: None,
			root,
			graph,
			reference: reference.clone(),
			snapshot,
			last_recorded: AtomicU64::new(AtomId::NULL.raw()),
		})
	}

	/// Retains transaction predicates while maintenance reads stay
	/// observational.
	fn record_read(&self, key: ConflictKey) {
		if let Some(transaction) = self.transaction {
			transaction.record_read(key);
		}
	}

	/// Records independently versioned occurrence keys through the shared
	/// reader.
	fn record_reads<const COUNT: usize>(&self, keys: [ConflictKey; COUNT]) {
		if let Some(transaction) = self.transaction {
			transaction.record_reads(keys);
		}
	}

	/// Records incidence.
	fn record_incidence(&self, id: AtomId) {
		self.record_read(ConflictKey::GraphIncidence(self.reference.id(), id));
	}

	/// Records occurrence.
	fn record_occurrence(&self, occurrence: &GraphOccurrence) {
		let local = occurrence.local_id();
		if self.last_recorded.load(Ordering::Acquire) == local.raw() {
			return;
		}
		let graph = self.reference.id();
		let global = occurrence.global_id();
		self.record_reads([
			ConflictKey::BaseAtom(global),
			ConflictKey::GraphMembership(graph, global),
			ConflictKey::GraphOverlay(graph, global),
			ConflictKey::GraphIncidence(graph, local),
		]);
		// shortcut: cache one exact hot key; add a bounded per-view table only
		// if alternating point-read profiles justify its metadata and lookup
		// cost.
		self.last_recorded.store(local.raw(), Ordering::Release);
	}

	/// Returns the occurrence result.
	fn occurrence(&self, id: AtomId) -> Option<&GraphOccurrence> {
		self.graph.occurrence(id)
	}

	/// Returns the effective properties result.
	fn effective_properties<'view>(
		&'view self,
		occurrence: &'view GraphOccurrence,
	) -> FerrosGrynnResult<EffectiveProperties<'view>> {
		let atom = self.root.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph occurrence references an absent global atom",
			)
		})?;
		Ok(EffectiveProperties {
			base: atom.properties(),
			overlay: occurrence.overlay(),
		})
	}

	/// Returns the graph reference result.
	fn graph_reference(
		&self,
		id: Option<crate::graph::GraphId>,
	) -> FerrosGrynnResult<Option<&GraphRef>> {
		id.map(|id| {
			self.root.schema.graphs.get(&id).map(|schema| schema.reference()).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"graph occurrence references an absent nested graph",
				)
			})
		})
		.transpose()
	}
}

impl GraphRead for TransactionGraphView<'_> {
	type Edge<'a>
		= TransactionEdgeRead<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;
	type Node<'a>
		= TransactionNodeRead<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>
	where
		Self: 'a;

	/// Returns identity for `TransactionGraphView<'_>`.
	fn identity(&self) -> Uuid {
		uuid::Uuid::from_u128(u128::from(self.reference.id().get())).into()
	}

	/// Returns revision for `TransactionGraphView<'_>`.
	fn revision(&self) -> u64 {
		self.snapshot.timestamp()
	}

	/// Returns kind for `TransactionGraphView<'_>`.
	fn kind(&self) -> crate::graph::GraphKind {
		self.reference.kind()
	}

	/// Returns properties for `TransactionGraphView<'_>`.
	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		self.record_read(ConflictKey::GraphDefinition(self.reference.id()));
		let schema = self.root.schema.graphs.get(&self.reference.id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph view has no schema definition")
		})?;
		Ok(Cow::Borrowed(schema.properties()))
	}

	/// Returns topology constraints for `TransactionGraphView<'_>`.
	fn topology_constraints(&self) -> crate::graph::GraphTopologyConstraints {
		self.root
			.schema
			.graphs
			.get(&self.reference.id())
			.map_or(crate::graph::GraphTopologyConstraints::empty(), |schema| {
				schema.topology_constraints()
			})
	}

	/// Returns node ids for `TransactionGraphView<'_>`.
	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.record_read(ConflictKey::GraphKindRange(self.reference.id(), AtomKind::Node));
		Ok(Box::new(
			self.graph
				.occurrences()
				.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Node))
				.map(|occurrence| Ok(occurrence.local_id())),
		))
	}

	/// Returns edge ids for `TransactionGraphView<'_>`.
	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.record_read(ConflictKey::GraphKindRange(self.reference.id(), AtomKind::Edge));
		Ok(Box::new(
			self.graph
				.occurrences()
				.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Edge))
				.map(|occurrence| Ok(occurrence.local_id())),
		))
	}

	/// Returns edge endpoint connectors for `TransactionGraphView<'_>`.
	fn edge_endpoint_connectors(
		&self,
		endpoint: &crate::graph::EdgeEndpoint,
	) -> FerrosGrynnResult<Option<Vec<AtomId>>> {
		if endpoint.kind() != Some(AtomKind::Edge)
			|| self
				.root
				.schema
				.graph(self.reference.id())
				.and_then(|schema| schema.edge_endpoint_index())
				.is_none()
		{
			return Ok(None);
		}
		self.record_read(ConflictKey::GraphKindRange(self.reference.id(), AtomKind::Edge));
		if let Some(local) = endpoint.local_id() {
			self.record_incidence(local);
		}
		let mut connectors = Vec::new();
		for &(connector, _side) in self.graph.edge_endpoint_postings(endpoint) {
			if connectors.last() == Some(&connector) {
				continue;
			}
			if connectors.len() == 262_144 {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"edge endpoint connectors",
					262_144,
				));
			}
			self.record_incidence(connector);
			connectors.push(connector);
		}
		Ok(Some(connectors))
	}

	/// Returns node for `TransactionGraphView<'_>`.
	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		let Some(occurrence) = self.occurrence(id) else {
			self.record_incidence(id);
			return Ok(None);
		};
		let OccurrenceStateRef::Node {
			nested_graph,
		} = occurrence.state().view()
		else {
			self.record_incidence(id);
			return Ok(None);
		};
		let atom = self.root.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"node occurrence references an absent global atom",
			)
		})?;
		self.record_occurrence(occurrence);
		Ok(Some(TransactionNodeRead {
			id,
			record_id: atom.record_id(),
			label: occurrence.label(),
			properties: self.effective_properties(occurrence)?,
			inner_graph: self.graph_reference(*nested_graph)?,
		}))
	}

	/// Returns edge for `TransactionGraphView<'_>`.
	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		let Some(occurrence) = self.occurrence(id) else {
			self.record_incidence(id);
			return Ok(None);
		};
		let OccurrenceStateRef::Edge {
			topology,
			weight,
			transition_graph,
		} = occurrence.state().view()
		else {
			self.record_incidence(id);
			return Ok(None);
		};
		let atom = self.root.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"edge occurrence references an absent global atom",
			)
		})?;
		self.record_occurrence(occurrence);
		Ok(Some(TransactionEdgeRead {
			id,
			record_id: atom.record_id(),
			topology,
			label: occurrence.label(),
			properties: self.effective_properties(occurrence)?,
			weight: *weight,
			transition_graph: self.graph_reference(*transition_graph)?,
		}))
	}

	/// Returns storage mode for `TransactionGraphView<'_>`.
	fn storage_mode(&self) -> GraphStorageMode {
		self.reference.storage_mode()
	}

	/// Returns the stored snapshot of this `TransactionGraphView<'_>`.
	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	/// Returns graph ref for `TransactionGraphView<'_>`.
	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.reference)
	}
}

#[cfg(test)]
/// Regression tests for graph view behavior.
mod tests {
	use std::borrow::Cow;

	use super::*;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::{AtomKind, GraphKind},
		values::{PropertyKeyId, PropertyMap, RecordId, Value},
	};

	/// Verifies effective projection borrows base until an overlay exists.
	#[test]
	fn effective_projection_borrows_base_until_an_overlay_exists() {
		let store = Datastore::in_memory();
		let record = "person:shared".parse::<RecordId>().assert_ok();
		let key = PropertyKeyId::new(1);
		let mut properties = PropertyMap::new();
		properties.insert(key, "base");
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("view", GraphKind::Graph).assert_ok();
		seed.create_atom(record.clone(), properties).assert_ok();
		let local = seed.attach_node(graph.id(), &record, None, None).assert_ok();
		seed.commit().assert_ok();

		let read = store.begin();
		let view = graph.read(&read).assert_ok();
		let node = view.node(local).assert_ok().assert_some();
		assert!(matches!(node.properties().assert_ok(), Cow::Borrowed(_)));
		drop(view);
		drop(read);

		let mut update = store.begin();
		update.set_overlay(graph.id(), &record, key, Value::from("overlay")).assert_ok();
		update.commit().assert_ok();
		let read = store.begin();
		let view = graph.read(&read).assert_ok();
		let node = view.node(local).assert_ok().assert_some();
		assert!(node.properties.overlay.is_some());
		assert!(matches!(node.properties().assert_ok(), Cow::Owned(_)));
	}
}
