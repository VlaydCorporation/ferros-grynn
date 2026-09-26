use std::{
	collections::{BTreeSet, btree_set},
	ops::Bound,
	sync::Arc,
};

use super::Transaction;
use crate::{
	datastore::{
		ClassIndex,
		ClassSchema,
		DatabaseRoot,
		GlobalAtomId,
		GraphOccurrence,
		GraphRoot,
		GraphSchema,
		IndexId,
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
	BaseOwned(std::vec::IntoIter<GlobalAtomId>),
	Graph {
		graph: GraphId,
		ids: btree_set::Iter<'snapshot, AtomId>,
	},
	GraphOwned {
		graph: GraphId,
		ids: std::vec::IntoIter<AtomId>,
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
			QueryCursorSource::BaseOwned(ids) => {
				let Some(id) = ids.next() else {
					return Ok(None);
				};
				let atom = self.root.atom(id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scalar index references an absent global atom",
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
			QueryCursorSource::GraphOwned {
				graph,
				ids,
			} => {
				let Some(local) = ids.next() else {
					return Ok(None);
				};
				let graph = self.root.graph(*graph).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scalar index references an absent graph",
					)
				})?;
				let occurrence = graph.occurrence(local).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scalar index references an absent occurrence",
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
	pub(crate) values: Vec<Value>,
}

/// Bounded ordered-index window used by LIMIT and keyset continuation.
#[derive(Debug, Clone)]
pub(crate) struct ScalarIndexWindow {
	pub(crate) after: Option<(Vec<Value>, RecordId)>,
	pub(crate) maximum: usize,
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

	/// Returns planner statistics from the same immutable statement snapshot.
	pub(crate) fn query_statistics<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
	) -> FerrosGrynnResult<&'snapshot crate::stats::StatisticsStore> {
		self.ensure_live()?;
		Ok(snapshot.root.statistics.as_ref())
	}

	/// Resolves one transaction-visible physical binding without exposing its
	/// addresses or storage representation to the query layer.
	pub(crate) fn query_index_binding(
		&self,
		snapshot: &QuerySnapshot,
		class: &ClassSchema,
		index: &ClassIndex,
	) -> FerrosGrynnResult<(bool, Option<u64>)> {
		self.ensure_live()?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let index_id = index.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published index is missing IndexId")
		})?;
		match index.scope() {
			crate::datastore::IndexScope::Base => planner_binding_summary(
				snapshot.root.indexes.base_index(index_id),
				class_id,
				index_id,
				index,
			),
			crate::datastore::IndexScope::Effective {
				graph,
			} => planner_binding_summary(
				snapshot.root.indexes.graph_index(graph, index_id),
				class_id,
				index_id,
				index,
			),
		}
	}

	pub(crate) fn query_row_cursor<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
		class: &str,
		constraints: &[ScalarIndexConstraint],
		graph: Option<&GraphRef>,
		index: Option<(IndexId, bool)>,
		window: Option<&ScalarIndexWindow>,
	) -> FerrosGrynnResult<QueryStorageCursor<'snapshot>> {
		let class = self.query_class(snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let source = if let Some(reference) = graph {
			let graph_root = query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				reference.id(),
				class_id,
			));
			if let Some((index, reverse)) = index {
				let binding =
					snapshot.root.indexes.graph_index(reference.id(), index).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"planned effective index has no transaction-visible binding",
						)
					})?;
				let ids = query_index_addresses(
					&snapshot.root,
					&class,
					binding,
					constraints,
					reverse,
					window,
					|local| graph_record_id(&snapshot.root, graph_root, local),
				)?;
				QueryCursorSource::GraphOwned {
					graph: reference.id(),
					ids: ids.into_iter(),
				}
			} else {
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
			}
		} else {
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			if let Some((index, reverse)) = index {
				let binding = snapshot.root.indexes.base_index(index).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"planned base index has no transaction-visible binding",
					)
				})?;
				let ids = query_index_addresses(
					&snapshot.root,
					&class,
					binding,
					constraints,
					reverse,
					window,
					|global| base_record_id(&snapshot.root, global),
				)?;
				QueryCursorSource::BaseOwned(ids.into_iter())
			} else {
				snapshot
					.root
					.indexes
					.classes
					.get(&class_id)
					.map(|ids| QueryCursorSource::Base(ids.iter()))
					.unwrap_or(QueryCursorSource::Empty)
			}
		};
		Ok(QueryStorageCursor {
			root: &snapshot.root,
			class,
			source,
		})
	}

	/// Opens one exact scalar-index probe while preserving scan-equivalent
	/// predicate tracking for SSI.
	pub(crate) fn query_index_probe_cursor<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: IndexId,
		values: &[Value],
	) -> FerrosGrynnResult<QueryStorageCursor<'snapshot>> {
		let class = self.query_class(snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let mut context =
			crate::datastore::index_context(&snapshot.root.schema, &snapshot.root.dictionary)?;
		let source = if let Some(reference) = graph {
			query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				reference.id(),
				class_id,
			));
			let binding =
				snapshot.root.indexes.graph_index(reference.id(), index).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"planned effective index has no transaction-visible binding",
					)
				})?;
			let key = exact_index_probe_key(binding, values, &mut context)?;
			QueryCursorSource::GraphOwned {
				graph: reference.id(),
				ids: binding.entries.lookup(&key).into_iter(),
			}
		} else {
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			let binding = snapshot.root.indexes.base_index(index).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned base index has no transaction-visible binding",
				)
			})?;
			let key = exact_index_probe_key(binding, values, &mut context)?;
			QueryCursorSource::BaseOwned(binding.entries.lookup(&key).into_iter())
		};
		Ok(QueryStorageCursor {
			root: &snapshot.root,
			class,
			source,
		})
	}

	pub(crate) fn query_count_index(
		&self,
		snapshot: &QuerySnapshot,
		index: IndexId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64> {
		self.ensure_live()?;
		if let Some(graph) = graph {
			query_graph_root(&snapshot.root, graph)?;
			let binding =
				snapshot.root.indexes.graph_index(graph.id(), index).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"planned effective Count index has no transaction-visible binding",
					)
				})?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				graph.id(),
				binding.class_id,
			));
			return binding.entries.count().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned Count lookup selected a scalar index",
				)
			});
		} else {
			let binding = snapshot.root.indexes.base_index(index).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned base Count index has no transaction-visible binding",
				)
			})?;
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(binding.class_id));
			binding.entries.count().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned Count lookup selected a scalar index",
				)
			})
		}
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

fn exact_index_probe_key<Address: Copy + Ord>(
	binding: &crate::datastore::IndexBinding<Address>,
	values: &[Value],
	context: &mut crate::values::storage::EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	if values.len() != binding.fields.len() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"planned index probe does not cover the complete composite key",
		));
	}
	let components = values
		.iter()
		.zip(&binding.fields)
		.map(|(value, field)| (value, &field.value_type))
		.collect::<Vec<_>>();
	crate::values::storage::encode_index_key(&components, context)
}

fn query_index_addresses<Address: Copy + Ord>(
	root: &DatabaseRoot,
	class: &ClassSchema,
	binding: &crate::datastore::IndexBinding<Address>,
	constraints: &[ScalarIndexConstraint],
	reverse: bool,
	window: Option<&ScalarIndexWindow>,
	mut record_id: impl FnMut(Address) -> FerrosGrynnResult<RecordId>,
) -> FerrosGrynnResult<Vec<Address>> {
	if binding.kind == crate::datastore::ClassIndexKind::Count {
		return Err(FerrosGrynnError::invalid_storage_file("Count index reached the row cursor"));
	}
	let mut columns = Vec::with_capacity(binding.fields.len());
	for field in &binding.fields {
		let position = class
			.fields()
			.position(|candidate| candidate.key == Some(field.key))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"scalar index field is absent from its owning class",
				)
			})?;
		columns.push(position.saturating_add(5));
	}
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary)?;
	let after = window
		.and_then(|window| window.after.as_ref())
		.map(|(values, record)| -> FerrosGrynnResult<_> {
			let key = exact_index_probe_key(binding, values, &mut context)?;
			Ok((key, record))
		})
		.transpose()?;
	let maximum = window.map(|window| window.maximum);
	let mut prefixes = vec![Vec::<Value>::new()];
	let mut equality_fields = 0;
	for column in &columns {
		let Some(constraint) = constraints.iter().find(|constraint| {
			constraint.column == *column && matches!(constraint.comparison, ScalarComparison::Equal)
		}) else {
			break;
		};
		let mut next = Vec::new();
		for prefix in &prefixes {
			for value in &constraint.values {
				let mut key = prefix.clone();
				key.push(value.clone());
				next.push(key);
			}
		}
		prefixes = next;
		equality_fields += 1;
	}
	if prefixes.is_empty() {
		return Ok(Vec::new());
	}
	let mut groups = std::collections::BTreeMap::<Vec<u8>, BTreeSet<Address>>::new();
	if equality_fields == binding.fields.len() {
		for key in prefixes {
			let components = key
				.iter()
				.zip(binding.fields.iter())
				.map(|(value, field)| (value, &field.value_type))
				.collect::<Vec<_>>();
			let key = crate::values::storage::encode_index_key(&components, &mut context)?;
			groups.entry(key.clone()).or_default().extend(binding.entries.lookup(&key));
		}
		return ordered_index_addresses(
			groups,
			reverse,
			after.as_ref().map(|(key, record)| (key.as_slice(), *record)),
			maximum,
			&mut record_id,
		);
	}
	let range_column = columns[equality_fields];
	let ranges = constraints
		.iter()
		.filter(|constraint| {
			constraint.column == range_column
				&& !matches!(constraint.comparison, ScalarComparison::Equal)
		})
		.collect::<Vec<_>>();
	if equality_fields == 0 && ranges.is_empty() {
		return visit_ordered_index_addresses(
			&binding.entries,
			reverse,
			after.as_ref().map(|(key, record)| (key.as_slice(), *record)),
			maximum,
			&mut record_id,
		);
	}
	for prefix in prefixes {
		let components = prefix
			.iter()
			.zip(binding.fields[..equality_fields].iter())
			.map(|(value, field)| (value, &field.value_type))
			.collect::<Vec<_>>();
		let encoded_prefix =
			crate::values::storage::encode_index_prefix(&components, &mut context)?;
		let mut lower = if encoded_prefix.is_empty() {
			Bound::Unbounded
		} else {
			Bound::Included(encoded_prefix.clone())
		};
		let mut upper = next_prefix(&encoded_prefix).map_or(Bound::Unbounded, Bound::Excluded);
		for constraint in &ranges {
			let Some(value) = constraint.values.first() else {
				continue;
			};
			let field = &binding.fields[equality_fields];
			let component = [(value, &field.value_type)];
			let encoded = if equality_fields + 1 == binding.fields.len() {
				crate::values::storage::encode_index_key(&component, &mut context)?
			} else {
				crate::values::storage::encode_index_prefix(&component, &mut context)?
			};
			let mut bound = encoded_prefix.clone();
			bound.extend(encoded);
			match constraint.comparison {
				ScalarComparison::Equal => {}
				ScalarComparison::GreaterOrEqual => {
					lower = tighter_lower(lower, Bound::Included(bound));
				}
				ScalarComparison::Greater => {
					let bound = if equality_fields + 1 == binding.fields.len() {
						Bound::Excluded(bound)
					} else {
						next_prefix(&bound).map_or(Bound::Unbounded, Bound::Included)
					};
					lower = tighter_lower(lower, bound);
				}
				ScalarComparison::Less => {
					upper = tighter_upper(upper, Bound::Excluded(bound));
				}
				ScalarComparison::LessOrEqual => {
					let bound = if equality_fields + 1 == binding.fields.len() {
						Bound::Included(bound)
					} else {
						next_prefix(&bound).map_or(Bound::Unbounded, Bound::Excluded)
					};
					upper = tighter_upper(upper, bound);
				}
			}
		}
		for (key, addresses) in binding.entries.groups(lower, upper) {
			groups.entry(key).or_default().extend(addresses);
		}
	}
	ordered_index_addresses(
		groups,
		reverse,
		after.as_ref().map(|(key, record)| (key.as_slice(), *record)),
		maximum,
		&mut record_id,
	)
}

fn ordered_index_addresses<Address: Copy + Ord>(
	groups: std::collections::BTreeMap<Vec<u8>, BTreeSet<Address>>,
	reverse: bool,
	after: Option<(&[u8], &RecordId)>,
	maximum: Option<usize>,
	record_id: &mut impl FnMut(Address) -> FerrosGrynnResult<RecordId>,
) -> FerrosGrynnResult<Vec<Address>> {
	let mut groups = groups.into_iter().collect::<Vec<_>>();
	if reverse {
		groups.reverse();
	}
	let mut output = Vec::new();
	let mut seen = BTreeSet::new();
	for (key, addresses) in groups {
		if !key_is_after(&key, reverse, after.map(|(key, _record)| key)) {
			continue;
		}
		let mut posting = addresses
			.into_iter()
			.map(|address| Ok((record_id(address)?, address)))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		posting.sort_unstable();
		for (record, address) in posting {
			if after.is_some_and(|(after_key, after_record)| {
				key.as_slice() == after_key && record <= *after_record
			}) {
				continue;
			}
			if seen.insert(address) {
				output.push(address);
				if maximum.is_some_and(|maximum| output.len() >= maximum) {
					return Ok(output);
				}
			}
		}
	}
	Ok(output)
}

fn visit_ordered_index_addresses<Address: Copy + Ord>(
	entries: &crate::datastore::IndexEntries<Address>,
	reverse: bool,
	after: Option<(&[u8], &RecordId)>,
	maximum: Option<usize>,
	record_id: &mut impl FnMut(Address) -> FerrosGrynnResult<RecordId>,
) -> FerrosGrynnResult<Vec<Address>> {
	let lower = if reverse {
		Bound::Unbounded
	} else {
		after.map_or(Bound::Unbounded, |(key, _record)| Bound::Included(key.to_vec()))
	};
	let upper = if reverse {
		after.map_or(Bound::Unbounded, |(key, _record)| Bound::Included(key.to_vec()))
	} else {
		Bound::Unbounded
	};
	let mut output = Vec::with_capacity(maximum.unwrap_or(0).min(1_024));
	let mut failure = None;
	entries.visit_groups(lower, upper, reverse, |key, addresses| {
		let posting = addresses
			.into_iter()
			.map(|address| Ok((record_id(address)?, address)))
			.collect::<FerrosGrynnResult<Vec<_>>>();
		let mut posting = match posting {
			Ok(posting) => posting,
			Err(error) => {
				failure = Some(error);
				return false;
			}
		};
		posting.sort_unstable();
		for (record, address) in posting {
			if after.is_some_and(|(after_key, after_record)| {
				key == after_key && record <= *after_record
			}) {
				continue;
			}
			output.push(address);
			if maximum.is_some_and(|maximum| output.len() >= maximum) {
				return false;
			}
		}
		true
	});
	match failure {
		Some(error) => Err(error),
		None => Ok(output),
	}
}

fn key_is_after(key: &[u8], reverse: bool, after: Option<&[u8]>) -> bool {
	after.is_none_or(|after| {
		if reverse {
			key <= after
		} else {
			key >= after
		}
	})
}

fn base_record_id(root: &DatabaseRoot, global: GlobalAtomId) -> FerrosGrynnResult<RecordId> {
	root.atom(global).map(|atom| atom.record_id().clone()).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("scalar index references an absent global atom")
	})
}

fn graph_record_id(
	root: &DatabaseRoot,
	graph: &GraphRoot,
	local: AtomId,
) -> FerrosGrynnResult<RecordId> {
	let global = graph.occurrence(local).map(GraphOccurrence::global_id).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(
			"effective scalar index references an absent graph occurrence",
		)
	})?;
	base_record_id(root, global)
}

fn next_prefix(prefix: &[u8]) -> Option<Vec<u8>> {
	let mut next = prefix.to_vec();
	for index in (0..next.len()).rev() {
		if next[index] != u8::MAX {
			next[index] += 1;
			next.truncate(index + 1);
			return Some(next);
		}
	}
	None
}

fn tighter_lower(current: Bound<Vec<u8>>, candidate: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
	match (&current, &candidate) {
		(Bound::Unbounded, _) => candidate,
		(_, Bound::Unbounded) => current,
		(Bound::Included(left), Bound::Included(right))
		| (Bound::Included(left), Bound::Excluded(right))
		| (Bound::Excluded(left), Bound::Included(right))
		| (Bound::Excluded(left), Bound::Excluded(right))
			if right > left =>
		{
			candidate
		}
		(Bound::Included(left), Bound::Excluded(right)) if right == left => candidate,
		_ => current,
	}
}

fn tighter_upper(current: Bound<Vec<u8>>, candidate: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
	match (&current, &candidate) {
		(Bound::Unbounded, _) => candidate,
		(_, Bound::Unbounded) => current,
		(Bound::Included(left), Bound::Included(right))
		| (Bound::Included(left), Bound::Excluded(right))
		| (Bound::Excluded(left), Bound::Included(right))
		| (Bound::Excluded(left), Bound::Excluded(right))
			if right < left =>
		{
			candidate
		}
		(Bound::Included(left), Bound::Excluded(right)) if right == left => candidate,
		_ => current,
	}
}

fn planner_binding_summary<Address: Copy + Ord>(
	binding: Option<&crate::datastore::IndexBinding<Address>>,
	class_id: crate::datastore::ClassId,
	index_id: crate::datastore::IndexId,
	index: &ClassIndex,
) -> FerrosGrynnResult<(bool, Option<u64>)> {
	let Some(binding) = binding else {
		return Ok((false, None));
	};
	if binding.id != index_id
		|| binding.class_id != class_id
		|| binding.version != index.version()
		|| binding.kind != index.kind()
		|| binding.fields.len() != index.fields().len()
	{
		return Err(FerrosGrynnError::invalid_storage_file(format!(
			"physical binding for index '{}' disagrees with the schema catalog",
			index.name()
		)));
	}
	Ok((true, binding.entries.count()))
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
