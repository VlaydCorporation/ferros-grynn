use std::{
	borrow::Cow,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use super::{QuerySnapshot, Transaction};
use crate::{
	datastore::{
		ConflictKey,
		DatabaseRoot,
		GraphOccurrence,
		GraphRoot,
		OccurrenceState,
		OverlayValue,
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
	transaction: &'tx Transaction,
	root: Arc<DatabaseRoot>,
	graph: Arc<GraphRoot>,
	reference: GraphRef,
	snapshot: SnapshotId,
	last_recorded: AtomicU64,
}

/// Snapshot-borrowed effective node projection returned by a transaction graph
/// view.
pub struct TransactionNodeRead<'view> {
	id: AtomId,
	record_id: &'view RecordId,
	label: Option<&'view Text>,
	properties: Cow<'view, PropertyMap>,
	inner_graph: Option<&'view GraphRef>,
}

impl NodeRead for TransactionNodeRead<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties.as_ref()))
	}

	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.inner_graph.map(Cow::Borrowed))
	}
}

/// Snapshot-borrowed effective edge projection returned by a transaction graph
/// view.
pub struct TransactionEdgeRead<'view> {
	id: AtomId,
	record_id: &'view RecordId,
	topology: &'view EdgeTopology,
	label: Option<&'view Text>,
	properties: Cow<'view, PropertyMap>,
	weight: Option<Number>,
	transition_graph: Option<&'view GraphRef>,
}

impl EdgeRead for TransactionEdgeRead<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		Ok(Cow::Borrowed(self.record_id))
	}

	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>> {
		Ok(Cow::Borrowed(self.topology))
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		Ok(self.label.map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(self.properties.as_ref()))
	}

	fn weight(&self) -> Option<Number> {
		self.weight
	}

	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		Ok(self.transition_graph.map(Cow::Borrowed))
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
			transaction: self,
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

	fn read_graph(&self, reference: &GraphRef) -> FerrosGrynnResult<Self::View<'_>> {
		self.ensure_live()?;
		let snapshot = QuerySnapshot {
			root: Arc::new(self.working.clone()),
		};
		self.query_graph(&snapshot, reference)
	}
}

impl TransactionGraphView<'_> {
	fn record_incidence(&self, id: AtomId) {
		self.transaction.record_read(ConflictKey::GraphIncidence(self.reference.id(), id));
	}

	fn record_occurrence(&self, occurrence: &GraphOccurrence) {
		let local = occurrence.local_id();
		if self.last_recorded.load(Ordering::Acquire) == local.raw() {
			return;
		}
		let graph = self.reference.id();
		let global = occurrence.global_id();
		self.transaction.record_reads([
			ConflictKey::BaseAtom(global),
			ConflictKey::GraphMembership(graph, global),
			ConflictKey::GraphOverlay(graph, global),
			ConflictKey::GraphIncidence(graph, local),
		]);
		// ponytail: cache one exact hot key; add a bounded per-view table only
		// if alternating point-read profiles justify its metadata and lookup
		// cost.
		self.last_recorded.store(local.raw(), Ordering::Release);
	}

	fn occurrence(&self, id: AtomId) -> FerrosGrynnResult<Option<&GraphOccurrence>> {
		Ok(self.graph.occurrence(id))
	}

	fn effective_properties<'view>(
		&'view self,
		occurrence: &'view GraphOccurrence,
	) -> FerrosGrynnResult<Cow<'view, PropertyMap>> {
		let atom = self.root.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph occurrence references an absent global atom",
			)
		})?;
		let Some(overlay) = occurrence.overlay() else {
			return Ok(Cow::Borrowed(atom.properties()));
		};
		let mut properties = atom.properties().clone();
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
		Ok(Cow::Owned(properties))
	}

	fn graph_reference<'view>(
		&'view self,
		id: Option<crate::graph::GraphId>,
	) -> FerrosGrynnResult<Option<&'view GraphRef>> {
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
		= std::vec::IntoIter<FerrosGrynnResult<AtomId>>
	where
		Self: 'a;
	type Node<'a>
		= TransactionNodeRead<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= std::vec::IntoIter<FerrosGrynnResult<AtomId>>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		uuid::Uuid::from_u128(u128::from(self.reference.id().get())).into()
	}

	fn revision(&self) -> u64 {
		self.snapshot.timestamp()
	}

	fn kind(&self) -> crate::graph::GraphKind {
		self.reference.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Owned(PropertyMap::new()))
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		self.transaction
			.record_read(ConflictKey::GraphKindRange(self.reference.id(), AtomKind::Node));
		Ok(self
			.graph
			.occurrences()
			.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Node))
			.map(|occurrence| Ok(occurrence.local_id()))
			.collect::<Vec<_>>()
			.into_iter())
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		self.transaction
			.record_read(ConflictKey::GraphKindRange(self.reference.id(), AtomKind::Edge));
		Ok(self
			.graph
			.occurrences()
			.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Edge))
			.map(|occurrence| Ok(occurrence.local_id()))
			.collect::<Vec<_>>()
			.into_iter())
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		let Some(occurrence) = self.occurrence(id)? else {
			self.record_incidence(id);
			return Ok(None);
		};
		let OccurrenceState::Node {
			nested_graph,
		} = occurrence.state()
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

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		let Some(occurrence) = self.occurrence(id)? else {
			self.record_incidence(id);
			return Ok(None);
		};
		let OccurrenceState::Edge {
			topology,
			weight,
			transition_graph,
		} = occurrence.state()
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

	fn storage_mode(&self) -> GraphStorageMode {
		self.reference.storage_mode()
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.snapshot
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.reference)
	}
}

#[cfg(test)]
mod tests {
	use std::borrow::Cow;

	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::{AtomKind, GraphKind},
		values::{PropertyKeyId, PropertyMap, RecordId, Value},
	};

	#[test]
	fn effective_projection_borrows_base_until_an_overlay_exists() {
		let store = Datastore::in_memory();
		let record = "person:shared".parse::<RecordId>().unwrap();
		let key = PropertyKeyId::new(1);
		let mut properties = PropertyMap::new();
		properties.insert(key, "base");
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		let graph = seed.define_graph("view", GraphKind::Graph).unwrap();
		seed.create_atom(record.clone(), properties).unwrap();
		let local = seed.attach_node(graph.id(), &record, None, None).unwrap();
		seed.commit().unwrap();

		let read = store.begin();
		let view = graph.read(&read).unwrap();
		let node = view.node(local).unwrap().unwrap();
		assert!(matches!(node.properties, Cow::Borrowed(_)));
		drop(view);
		drop(read);

		let mut update = store.begin();
		update.set_overlay(graph.id(), &record, key, Value::from("overlay")).unwrap();
		update.commit().unwrap();
		let read = store.begin();
		let view = graph.read(&read).unwrap();
		let node = view.node(local).unwrap().unwrap();
		assert!(matches!(node.properties, Cow::Owned(_)));
	}
}
