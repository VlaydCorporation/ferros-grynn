use std::{collections::btree_set, sync::Arc};
use super::Transaction;
use crate::{
	datastore::{
		ClassSchema,
		DatabaseRoot,
		GlobalAtomId,
		GraphOccurrence,
		GraphRoot,
		GraphSchema,
		OverlayValue,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, GraphId, GraphRef},
	values::{AtomKind, PropertyDictionary, PropertyMap, RecordId, Text, Value},
};

/// One owned atom row passed from datastore cursors to query execution.
#[derive(Clone)]
pub(crate) struct QueryStorageRow {
	pub(crate) record_id: RecordId,
	pub(crate) kind: AtomKind,
	pub(crate) local_id: Option<AtomId>,
	pub(crate) label: Option<Text>,
	pub(crate) values: Vec<Value>,
	pub(crate) base_properties: PropertyMap,
	pub(crate) properties: PropertyMap,
}

enum QueryCursorSource<'snapshot> {
	Empty,
	Base(btree_set::Iter<'snapshot, GlobalAtomId>),
	Graph {
		graph: GraphId,
		ids: btree_set::Iter<'snapshot, AtomId>,
	},
}

/// Snapshot-bound cursor over database-global atoms of one class.
pub(crate) struct QueryStorageCursor<'snapshot> {
	root: &'snapshot DatabaseRoot,
	class: Arc<ClassSchema>,
	source: QueryCursorSource<'snapshot>,
}

impl QueryStorageCursor<'_> {
	/// Pulls one owned row without materializing the remaining class.
	pub(crate) fn next_row(&mut self) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		let row = match &mut self.source {
			QueryCursorSource::Empty => return Ok(None),
			QueryCursorSource::Base(ids) => {
				let Some(id) = ids.next().copied() else {
					return Ok(None);
				};
				let atom = self.root.atom(id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"class index references an absent global atom",
					)
				})?;
				row_from_atom(atom, &self.class)
			}
			QueryCursorSource::Graph {
				graph,
				ids,
			} => {
				let Some(local) = ids.next().copied() else {
					return Ok(None);
				};
				let graph = self.root.graph(*graph).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph class index references an absent graph",
					)
				})?;
				let occurrence = graph.occurrence(local).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph class index references an absent occurrence",
					)
				})?;
				row_from_occurrence(&self.root, occurrence, &self.class)?
			}
		};
		Ok(Some(row))
	}
}

/// Scalar comparison available to a class-index probe.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ScalarComparison {
	Equal,
	Less,
	LessOrEqual,
	Greater,
	GreaterOrEqual,
}

/// One scalar predicate that may be answered by a class index.
#[derive(Debug, Clone)]
pub(crate) struct ScalarIndexConstraint {
	pub(crate) column: usize,
	pub(crate) comparison: ScalarComparison,
	pub(crate) value: Value,
}

/// Immutable root captured for one query program.
#[derive(Clone)]
pub(crate) struct QuerySnapshot {
	pub(super) root: Arc<DatabaseRoot>,
}

impl QuerySnapshot {
	pub(crate) fn commit_ts(&self) -> u64 {
		self.root.commit_ts
	}
}

impl Transaction {
	pub(crate) fn query_scratch_root(&self) -> std::path::PathBuf {
		self.store.inner.query_scratch_root()
	}

	pub(crate) fn query_property_dictionary(&self) -> Arc<PropertyDictionary> {
		self.working.dictionary.clone()
	}

	pub(crate) fn query_snapshot(&mut self) -> FerrosGrynnResult<QuerySnapshot> {
		self.prepare_read()?;
		Ok(QuerySnapshot {
			root: Arc::new(self.working.clone()),
		})
	}

	pub(crate) fn query_class(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<Arc<ClassSchema>> {
		self.ensure_live()?;
		snapshot.root.schema.class(name).ok_or_else(|| FerrosGrynnError::class_does_not_exist(name))
	}

	pub(crate) fn query_named_graph(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<GraphRef> {
		self.ensure_live()?;
		snapshot
			.root
			.schema
			.named_graph(name)
			.map(|schema| schema.reference().clone())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))
	}

	pub(crate) fn query_schema_graphs(
		&self,
		snapshot: &QuerySnapshot,
	) -> FerrosGrynnResult<Vec<GraphSchema>> {
		self.ensure_live()?;
		self.record_read(crate::datastore::ConflictKey::GraphCatalogRange);
		Ok(snapshot.root.schema.graphs().map(|graph| graph.as_ref().clone()).collect())
	}

	pub(crate) fn query_schema_classes(
		&self,
		snapshot: &QuerySnapshot,
	) -> FerrosGrynnResult<Vec<ClassSchema>> {
		self.ensure_live()?;
		Ok(snapshot.root.schema.classes().map(|class| class.as_ref().clone()).collect())
	}

	pub(crate) fn query_row_cursor<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
		class: &str,
		_constraints: &[ScalarIndexConstraint],
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<QueryStorageCursor<'snapshot>> {
		let class = self.query_class(snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let source = if let Some(reference) = graph {
			query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				reference.id(),
				class_id,
			));
			snapshot
				.root
				.indexes
				.graphs
				.get(&reference.id())
				.and_then(|indexes| indexes.classes.get(&class_id))
				.map(|ids| QueryCursorSource::Graph {
					graph: reference.id(),
					ids: ids.iter(),
				})
				.unwrap_or(QueryCursorSource::Empty)
		} else {
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			snapshot
				.root
				.indexes
				.classes
				.get(&class_id)
				.map(|ids| QueryCursorSource::Base(ids.iter()))
				.unwrap_or(QueryCursorSource::Empty)
		};
		Ok(QueryStorageCursor {
			root: &snapshot.root,
			class,
			source,
		})
	}

	/// Selects graph-local MATCH seeds through exact, class, and label indexes.
	pub(crate) fn query_match_candidates(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
		kind: AtomKind,
		class: Option<&str>,
		record: Option<&RecordId>,
		label: Option<&str>,
	) -> FerrosGrynnResult<Vec<AtomId>> {
		self.ensure_live()?;
		let graph = query_graph_root(&snapshot.root, reference)?;
		self.record_read(crate::datastore::ConflictKey::GraphKindRange(
			reference.id(),
			AtomKind::Node,
		));
		self.record_read(crate::datastore::ConflictKey::GraphKindRange(
			reference.id(),
			AtomKind::Edge,
		));
		if let Some(record) = record {
			self.track_query_point(snapshot, record, Some(reference))?;
			let Some(atom) = snapshot.root.atom_by_record(record) else {
				return Ok(Vec::new());
			};
			if atom.kind() != kind {
				return Ok(Vec::new());
			}
			let Some(local) = graph.local_id(atom.id()) else {
				return Ok(Vec::new());
			};
			if let Some(expected) = label {
				let occurrence = graph.occurrence(local).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph membership references an absent occurrence",
					)
				})?;
				if occurrence.label().map(Text::as_str) != Some(expected) {
					return Ok(Vec::new());
				}
			}
			return Ok(vec![local]);
		}

		let class_ids = class
			.map(|class| {
				let schema = self.query_class(snapshot, class)?;
				if schema.kind() != kind {
					return Err(FerrosGrynnError::invalid_argument(
						"MATCH selector class kind disagrees with its pattern delimiter.",
					));
				}
				let class_id = schema.id().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
				})?;
				Ok(snapshot.root.indexes.graph_class(reference.id(), class_id))
			})
			.transpose()?;
		let label_ids = label.map(|label| {
			snapshot.root.indexes.graph_label(reference.id(), kind, &Text::from(label))
		});
		Ok(match (class_ids, label_ids) {
			(Some(classes), Some(labels)) => intersect_sorted(classes, labels),
			(Some(classes), None) => classes,
			(None, Some(labels)) => labels,
			(None, None) => graph
				.occurrences()
				.filter(|occurrence| occurrence.local_id().kind() == Some(kind))
				.map(GraphOccurrence::local_id)
				.collect(),
		})
	}

	pub(crate) fn query_point(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		self.ensure_live()?;
		self.track_query_point(snapshot, record, graph)?;
		let Some(atom) = snapshot.root.atom_by_record(record) else {
			return Ok(None);
		};
		let class = snapshot.root.schema.class_by_id(atom.class_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("global atom references an absent class")
		})?;
		if let Some(reference) = graph {
			let graph = query_graph_root(&snapshot.root, reference)?;
			let Some(local) = graph.local_id(atom.id()) else {
				return Ok(None);
			};
			let occurrence = graph.occurrence(local).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"graph membership references an absent occurrence",
				)
			})?;
			return row_from_occurrence(&snapshot.root, occurrence, &class).map(Some);
		}
		Ok(Some(row_from_atom(atom, &class)))
	}

	/// Resolves one record field under the normative base/effective reference
	/// rules.
	pub(crate) fn query_record_field(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		let row = match graph {
			Some(graph) => match self.query_point(snapshot, record, Some(graph))? {
				Some(row) => Some(row),
				None => self.query_point(snapshot, record, None)?,
			},
			None => self.query_point(snapshot, record, None)?,
		}
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("record", record.to_string()))?;

		match field {
			"id" => Ok(Value::Record(Box::new(row.record_id))),
			"label" => Ok(row.label.map(Value::String).unwrap_or(Value::None)),
			_ => {
				let Some(key) = snapshot.root.dictionary.get(field) else {
					return Ok(Value::None);
				};
				Ok(row.properties.get(key).cloned().unwrap_or(Value::None))
			}
		}
	}

	fn track_query_point(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<()> {
		let Some(global) = snapshot.root.resolve_atom(record) else {
			self.record_missing(record);
			return Ok(());
		};
		self.record_read(crate::datastore::ConflictKey::BaseAtom(global));
		if let Some(reference) = graph {
			let graph = query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphMembership(
				reference.id(),
				global,
			));
			self.record_read(crate::datastore::ConflictKey::GraphOverlay(reference.id(), global));
			if let Some(local) = graph.local_id(global) {
				self.record_read(crate::datastore::ConflictKey::GraphIncidence(
					reference.id(),
					local,
				));
			}
		}
		Ok(())
	}
}

fn row_from_atom(atom: &crate::datastore::BaseAtom, class: &ClassSchema) -> QueryStorageRow {
	let properties = atom.properties().clone();
	let mut values = Vec::with_capacity(class.fields().len() + 1);
	values.push(Value::Record(Box::new(atom.record_id().clone())));
	values.extend(class.fields().map(|field| {
		field.key.and_then(|key| properties.get(key).cloned()).unwrap_or(Value::None)
	}));
	QueryStorageRow {
		record_id: atom.record_id().clone(),
		kind: atom.kind(),
		local_id: None,
		label: None,
		values,
		base_properties: properties.clone(),
		properties,
	}
}

fn row_from_occurrence(
	root: &DatabaseRoot,
	occurrence: &GraphOccurrence,
	class: &ClassSchema,
) -> FerrosGrynnResult<QueryStorageRow> {
	let atom = root.atom(occurrence.global_id()).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph occurrence references an absent global atom")
	})?;
	let base_properties = atom.properties().clone();
	let mut properties = base_properties.clone();
	if let Some(overlay) = occurrence.overlay() {
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
	}
	let mut values = Vec::with_capacity(class.fields().len() + 1);
	values.push(Value::Record(Box::new(atom.record_id().clone())));
	values.extend(class.fields().map(|field| {
		field.key.and_then(|key| properties.get(key).cloned()).unwrap_or(Value::None)
	}));
	Ok(QueryStorageRow {
		record_id: atom.record_id().clone(),
		kind: atom.kind(),
		local_id: Some(occurrence.local_id()),
		label: occurrence.label().cloned(),
		values,
		base_properties,
		properties,
	})
}

fn query_graph_root<'root>(
	root: &'root DatabaseRoot,
	reference: &GraphRef,
) -> FerrosGrynnResult<&'root GraphRoot> {
	let graph = root
		.graph(reference.id())
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.name().to_string()))?;
	if graph.reference() != reference {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query graph reference disagrees with the snapshot catalog",
		));
	}
	Ok(graph)
}

fn intersect_sorted(left: Vec<AtomId>, right: Vec<AtomId>) -> Vec<AtomId> {
	let (mut left_index, mut right_index) = (0, 0);
	let mut intersection = Vec::with_capacity(left.len().min(right.len()));
	while let (Some(left_id), Some(right_id)) = (left.get(left_index), right.get(right_index)) {
		match left_id.cmp(right_id) {
			std::cmp::Ordering::Less => left_index += 1,
			std::cmp::Ordering::Greater => right_index += 1,
			std::cmp::Ordering::Equal => {
				intersection.push(*left_id);
				left_index += 1;
				right_index += 1;
			}
		}
	}
	intersection
}
