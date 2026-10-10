//! Read-only adapter for evaluation against a fixed candidate MVCC root.

use std::{collections::BTreeMap, mem::size_of, path::PathBuf, sync::Arc};

use super::{
	AtomFieldRead,
	ExecutionControl,
	FerrosGrynnError,
	FerrosGrynnResult,
	GraphRef,
	IndexScanWindow,
	LogicalDmlStatement,
	LogicalGraphDmlStatement,
	LogicalSchemaStatement,
	Number,
	PhysicalFulltextBatch,
	PhysicalInnerGraphBatch,
	PhysicalOccurrenceFieldsBatch,
	PhysicalTraversalBatch,
	PhysicalVectorHit,
	PropertyDictionary,
	PropertyMap,
	QueryCatalog,
	QueryRead,
	QueryStorageCursor,
	QueryStorageRow,
	RecordId,
	ResolvedMatchBinding,
	RootFields,
	Row,
	RowCursor,
	Snapshot,
	TypedExpr,
	Value,
};
use crate::{
	datastore::{
		BaseAtom,
		ClassId,
		ConflictKey,
		DatabaseRoot,
		GraphOccurrence,
		IndexId,
		OverlayValue,
		TransactionGraphView,
	},
	gql::{
		optimizer::{
			PlannerClass,
			PlannerDemand,
			PlannerField,
			PlannerGraph,
			PlannerSnapshot,
			PlannerStatisticsMeta,
		},
		root_catalog::RootCatalog,
	},
	graph::{AtomId, AtomKind, AtomOccurrenceRef, GraphId, GraphRead, SnapshotId},
};

/// Candidate roots are borrowed only during bounded evaluation, never delivery
/// waits.
pub(in crate::gql) struct RootRead<'a> {
	/// The complete replayed commit image.
	root: &'a Arc<DatabaseRoot>,
	/// Shared ordinary schema/function resolution.
	catalog: RootCatalog<'a>,
	/// Existing bounded scalar/PURE value resolver.
	fields: RootFields<'a>,
	/// Records candidate dependencies separately from transaction SSI reads.
	observe: &'a dyn Fn(ConflictKey),
	/// Marks actual global class scans separately from schema-only range reads.
	class_scan: Option<&'a dyn Fn(ClassId)>,
	/// Existing query runtime cancellation and memory account.
	control: &'a ExecutionControl,
	/// Database-owned directory for ordinary SELECT barriers.
	scratch_root: PathBuf,
}

impl<'a> RootRead<'a> {
	/// Reuses one scalar admission account across compilation and evaluation.
	pub(in crate::gql) fn fields(&self) -> &dyn AtomFieldRead {
		&self.fields
	}

	/// Pins one immutable logical root without opening a transaction.
	pub(in crate::gql) fn new(
		root: &'a Arc<DatabaseRoot>,
		control: &'a ExecutionControl,
		observe: &'a dyn Fn(ConflictKey),
		work: &'a dyn Fn(u64) -> FerrosGrynnResult<()>,
		scratch_root: PathBuf,
	) -> Self {
		Self {
			root,
			catalog: RootCatalog(root),
			fields: RootFields::new(root, control, observe, work),
			observe,
			class_scan: None,
			control,
			scratch_root,
		}
	}

	/// Enables the LIVE recorder to distinguish scans from schema metadata.
	pub(in crate::gql) fn with_class_scan_observer(mut self, observe: &'a dyn Fn(ClassId)) -> Self {
		self.class_scan = Some(observe);
		self
	}

	/// Checks scalar and row work together with cancellation before allocation.
	fn work(&self, amount: u64) -> FerrosGrynnResult<()> {
		self.control.check()?;
		self.fields.work(amount)
	}

	/// Prevents a caller from mixing logical snapshots on the same adapter.
	fn check_snapshot(&self, snapshot: Snapshot) -> FerrosGrynnResult<()> {
		if snapshot.0 != self.root.commit_ts {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE reader snapshot does not match its root",
			));
		}
		self.control.check()
	}

	/// Checks conservative copy and container costs before constructing
	/// payloads.
	fn admit_copies(&self, bytes: usize, copies: usize, overhead: usize) -> FerrosGrynnResult<()> {
		self.fields.admit(
			bytes
				.checked_mul(copies)
				.and_then(|bytes| bytes.checked_add(overhead))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE owned payload"))?,
		)
	}

	/// Admits every copy made by ordinary row construction and public
	/// projection.
	fn admit_row(
		&self,
		atom: &BaseAtom,
		occurrence: Option<&GraphOccurrence>,
	) -> FerrosGrynnResult<()> {
		self.work(1)?;
		(self.observe)(ConflictKey::BaseAtom(atom.id()));
		(self.observe)(ConflictKey::SchemaClass(atom.class_id()));
		(self.observe)(ConflictKey::PropertyDictionary);
		let class = self.root.schema.class_by_id(atom.class_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("root row references an absent class")
		})?;
		let fields = class.fields().len();
		self.fields.admit(
			fields
				.checked_add(8)
				.and_then(|count| count.checked_mul(4 * size_of::<Value>() + 128))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE row shape"))?,
		)?;
		self.admit_copies(
			crate::values::runtime::estimated_record_id_heap_bytes(atom.record_id()),
			4,
			0,
		)?;
		for (key, value) in atom.properties().iter() {
			let name = self.root.dictionary.resolve(key).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("root property has no dictionary name")
			})?;
			self.admit_copies(name.len(), 4, 512)?;
			for _ in 0..4 {
				self.fields.admit_value(value)?;
			}
		}
		if let Some(occurrence) = occurrence {
			self.fields.admit(occurrence.label().map_or(0, |name| name.len()))?;
			if let Some(overlay) = occurrence.overlay() {
				for (key, value) in overlay.iter() {
					let name = self.root.dictionary.resolve(key).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"root overlay has no dictionary name",
						)
					})?;
					self.admit_copies(name.len(), 4, 512)?;
					if let OverlayValue::Value(value) = value {
						for _ in 0..4 {
							self.fields.admit_value(value)?;
						}
					}
				}
			}
		}
		for field in class.fields() {
			self.work(1)?;
			if let Some(key) = field.key {
				let value = occurrence.map_or_else(
					|| atom.properties().get(key),
					|occurrence| occurrence.effective_value(atom.properties().get(key), key),
				);
				if let Some(value) = value {
					self.fields.admit_value(value)?;
				}
			}
		}
		Ok(())
	}

	/// Resolves absent records and memberships before producing owned row
	/// values.
	fn point_parts(
		&self,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<(&BaseAtom, Option<&GraphOccurrence>)>> {
		self.work(1)?;
		let Some(atom) = self.root.atom_by_record(record) else {
			if let Some(class) = self.root.schema.class(record.class()).and_then(|class| class.id())
			{
				(self.observe)(ConflictKey::SchemaClass(class));
				(self.observe)(ConflictKey::BaseClassRange(class));
			} else {
				(self.observe)(ConflictKey::ClassCatalogRange);
			}
			return Ok(None);
		};
		(self.observe)(ConflictKey::BaseAtom(atom.id()));
		let occurrence = if let Some(graph) = graph {
			QueryRead::graph(self, Snapshot(self.root.commit_ts), graph)?;
			(self.observe)(ConflictKey::GraphMembership(graph.id(), atom.id()));
			(self.observe)(ConflictKey::GraphOverlay(graph.id(), atom.id()));
			let Some(local) = self.root.membership(graph.id(), atom.id()) else {
				return Ok(None);
			};
			(self.observe)(ConflictKey::GraphIncidence(graph.id(), local));
			Some(self.root.occurrence(graph.id(), local).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("root membership has no occurrence")
			})?)
		} else {
			None
		};
		Ok(Some((atom, occurrence)))
	}

	/// Materializes a complete row only for consumers requiring all fields.
	fn point(
		&self,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		let Some((atom, occurrence)) = self.point_parts(record, graph)? else {
			return Ok(None);
		};
		self.admit_row(atom, occurrence)?;
		QueryStorageRow::point_in_root(self.root, record, graph)
	}
}

impl QueryRead for RootRead<'_> {
	type Graph<'a>
		= TransactionGraphView<'a>
	where
		Self: 'a;
	type Savepoint = ();

	fn live_binding_value(
		&self,
		snapshot: Snapshot,
		id: crate::live::LiveBindingId,
	) -> FerrosGrynnResult<&Value> {
		self.check_snapshot(snapshot)?;
		self.fields.observe(crate::datastore::ConflictKey::LiveBinding(id));
		self.root.live_catalog().binding_value(id)
	}

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		self.control.check()?;
		Ok(Snapshot(self.root.commit_ts))
	}

	fn refresh_snapshot(&mut self) -> FerrosGrynnResult<Snapshot> {
		self.snapshot()
	}

	fn mutation_count(&self) -> usize {
		0
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn bounded_fields(&self) -> Option<&dyn AtomFieldRead> {
		Some(&self.fields)
	}

	fn planner_snapshot(
		&self,
		snapshot: Snapshot,
		demand: &PlannerDemand,
	) -> FerrosGrynnResult<PlannerSnapshot> {
		self.check_snapshot(snapshot)?;
		let mut result = PlannerSnapshot {
			revision: snapshot.0,
			..PlannerSnapshot::default()
		};
		for name in &demand.classes {
			self.work(1)?;
			let class = self.catalog.class_definition(name)?;
			let id = class.id().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("root class has no identity")
			})?;
			self.fields.admit(size_of::<PlannerClass>() + class.name().len() + 128)?;
			self.fields.admit(
				class
					.fields()
					.len()
					.checked_mul(size_of::<PlannerField>() + 128)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE planner fields"))?,
			)?;
			let mut fields = Vec::new();
			for field in class.fields() {
				self.fields.admit(field.name().len())?;
				crate::gql::live_lowering::admit_type_shape(
					field.value_type(),
					0,
					1,
					&mut |bytes, work| {
						self.work(work)?;
						self.fields.admit(bytes)
					},
				)?;
				fields.push(PlannerField {
					id: field.id().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("root field has no identity")
					})?,
					name: field.name().clone(),
					value_type: field.value_type().clone(),
				});
			}
			let mut graph_rows = BTreeMap::new();
			for graph in &demand.graphs {
				self.work(1)?;
				self.fields.admit(128)?;
				graph_rows.insert(
					*graph,
					self.root
						.indexes
						.graphs
						.get(graph)
						.and_then(|index| index.classes.get(&id))
						.map_or(0, |ids| ids.len() as u64),
				);
			}
			result.classes.insert(class.name().clone(), PlannerClass {
				id,
				version: class.version(),
				name: class.name().clone(),
				fields,
				// Candidate roots have no published physical generation. The
				// ordinary optimizer therefore chooses exact scan/refinement.
				indexes: Vec::new(),
				base_rows: self.root.indexes.classes.get(&id).map_or(0, |ids| ids.len() as u64),
				graph_rows,
			});
		}
		let mut visit_graph = |graph: &crate::datastore::GraphSchema| -> FerrosGrynnResult<()> {
			self.fields.admit(size_of::<PlannerGraph>() + graph.name().len() + 128)?;
			let statistics = self.root.statistics.graph(graph.id());
			result.graphs.insert(graph.id(), PlannerGraph {
				reference: graph.reference().clone(),
				version: graph.version(),
				statistics: PlannerStatisticsMeta::default(),
				node_rows: statistics.map_or(0, |statistics| statistics.node_count),
				edge_rows: statistics.map_or(0, |statistics| statistics.edge_count),
				total_out_degree: statistics.map_or(0, |statistics| statistics.total_out_degree),
				total_in_degree: statistics.map_or(0, |statistics| statistics.total_in_degree),
				total_undirected_degree: statistics
					.map_or(0, |statistics| statistics.total_undirected_degree),
				maximum_out_degree: statistics
					.map_or(0, |statistics| statistics.maximum_out_degree),
				maximum_in_degree: statistics.map_or(0, |statistics| statistics.maximum_in_degree),
				labels: BTreeMap::new(),
				neighbourhood_sketches: Vec::new(),
				landmark_paths: Vec::new(),
				tensor_caches: Vec::new(),
			});
			Ok(())
		};
		if demand.all_graphs {
			for graph in self.root.schema.all_graphs() {
				self.work(1)?;
				visit_graph(graph)?;
			}
		} else {
			for id in &demand.graphs {
				self.work(1)?;
				if let Some(graph) = self.root.schema.graph(*id) {
					visit_graph(&graph)?;
				}
			}
		}
		Ok(result)
	}

	fn scan<'a>(
		&'a self,
		snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		_predicate: Option<&TypedExpr>,
		index: Option<(IndexId, bool)>,
		window: Option<IndexScanWindow<'_>>,
		_slots: &[Value],
		_source: &str,
		full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		self.check_snapshot(snapshot)?;
		if index.is_some() || window.is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"root LIVE scan received an unpublished index plan",
			));
		}
		self.work(1)?;
		let class = self.catalog.class_definition(class)?;
		let id = class
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("root class has no identity"))?;
		(self.observe)(ConflictKey::PropertyDictionary);
		if let Some(graph) = graph {
			(self.observe)(ConflictKey::GraphDefinition(graph.id()));
			(self.observe)(ConflictKey::GraphClassRange(graph.id(), id));
		} else {
			(self.observe)(ConflictKey::BaseClassRange(id));
		}
		let rows = QueryStorageCursor::scan_in_root(self.root, class, graph)?;
		if graph.is_none()
			&& let Some(observe) = self.class_scan
		{
			observe(id);
		}
		self.fields.admit(size_of::<RootCursor<'_, '_>>())?;
		Ok(Box::new(RootCursor {
			rows,
			read: self,
			full_row,
		}))
	}

	fn get(
		&self,
		snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<Row>> {
		self.check_snapshot(snapshot)?;
		self.point(record, graph)?
			.map(|row| super::adapter::storage_row(row, &self.root.dictionary, true))
			.transpose()
	}

	fn base_atom(
		&self,
		snapshot: Snapshot,
		record: &RecordId,
	) -> FerrosGrynnResult<Option<(AtomKind, PropertyMap)>> {
		self.check_snapshot(snapshot)?;
		Ok(self.point(record, None)?.map(|row| (row.kind, row.properties)))
	}

	fn record_field(
		&self,
		snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
		_control: Option<&ExecutionControl>,
	) -> FerrosGrynnResult<Value> {
		self.check_snapshot(snapshot)?;
		let mut parts = self.point_parts(record, graph)?;
		if parts.is_none() && graph.is_some() {
			parts = self.point_parts(record, None)?;
		}
		let (atom, occurrence) = parts
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("record", record.to_string()))?;
		(self.observe)(ConflictKey::SchemaClass(atom.class_id()));
		(self.observe)(ConflictKey::PropertyDictionary);
		QueryStorageRow::field_from_atom(
			self.root,
			atom,
			occurrence,
			field,
			|bytes| self.fields.admit(bytes),
			|value| self.fields.copy_value(value),
		)
	}

	fn graph(
		&self,
		snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'_>> {
		self.check_snapshot(snapshot)?;
		self.work(1)?;
		(self.observe)(ConflictKey::GraphDefinition(reference.id()));
		(self.observe)(ConflictKey::GraphKindRange(reference.id(), AtomKind::Node));
		(self.observe)(ConflictKey::GraphKindRange(reference.id(), AtomKind::Edge));
		self.fields.admit(size_of::<TransactionGraphView<'_>>() + reference.name().len())?;
		TransactionGraphView::for_root(
			self.root.clone(),
			reference,
			SnapshotId::new(snapshot.0, snapshot.0),
		)
	}

	fn graph_reference(&self, snapshot: Snapshot, graph: GraphId) -> FerrosGrynnResult<GraphRef> {
		self.check_snapshot(snapshot)?;
		(self.observe)(ConflictKey::GraphDefinition(graph));
		let graph = self
			.root
			.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		self.fields.admit(graph.name().len() + size_of::<GraphRef>())?;
		Ok(graph.reference().clone())
	}

	fn graph_references(&self, snapshot: Snapshot) -> FerrosGrynnResult<Vec<GraphRef>> {
		self.check_snapshot(snapshot)?;
		(self.observe)(ConflictKey::GraphCatalogRange);
		let mut references = Vec::new();
		for graph in self.root.schema.all_graphs() {
			self.work(1)?;
			self.fields.admit(size_of::<GraphRef>())?;
			references.push(self.graph_reference(snapshot, graph.id())?);
		}
		Ok(references)
	}

	fn named_graph_contract(
		&self,
		snapshot: Snapshot,
		name: &str,
	) -> FerrosGrynnResult<Option<(GraphRef, crate::graph::GraphTopologyConstraints)>> {
		self.check_snapshot(snapshot)?;
		(self.observe)(ConflictKey::GraphCatalogRange);
		self.root
			.schema
			.named_graph(name)
			.map(|graph| {
				self.graph_reference(snapshot, graph.id())
					.map(|reference| (reference, graph.topology_constraints()))
			})
			.transpose()
	}

	fn level_path(
		&self,
		snapshot: Snapshot,
		occurrence: &AtomOccurrenceRef,
	) -> FerrosGrynnResult<Value> {
		self.check_snapshot(snapshot)?;
		QueryRead::graph(self, snapshot, occurrence.graph())?;
		self.fields.level_path(occurrence)
	}

	fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
		self.root.dictionary.clone()
	}

	fn spill_context(
		&self,
		snapshot: Snapshot,
	) -> FerrosGrynnResult<crate::values::storage::EncodeContext> {
		self.check_snapshot(snapshot)?;
		(self.observe)(ConflictKey::PropertyDictionary);
		(self.observe)(ConflictKey::GraphCatalogRange);
		for (_key, name) in self.root.dictionary.iter() {
			self.work(1)?;
			self.admit_copies(name.len(), 2, 256)?;
		}
		for class in self.root.schema.classes() {
			self.work(1)?;
			self.admit_copies(class.name().len(), 2, 256)?;
		}
		for graph in self.root.schema.all_graphs() {
			self.work(1)?;
			self.admit_copies(graph.name().len(), 2, 2 * size_of::<GraphRef>() + 256)?;
		}
		// The existing context encoder is shared with physical builders and
		// preserves exact public identities in temporary SELECT runs.
		crate::datastore::index_context(&self.root.schema, &self.root.dictionary)
	}

	fn query_scratch_root(&self) -> PathBuf {
		self.scratch_root.clone()
	}

	fn count_index(
		&self,
		_snapshot: Snapshot,
		_index: IndexId,
		_graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64> {
		unavailable_index()
	}

	fn probe_index<'a>(
		&'a self,
		_snapshot: Snapshot,
		_class: &str,
		_graph: Option<&GraphRef>,
		_index: IndexId,
		_values: &[Value],
		_full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		unavailable_index()
	}

	fn fulltext_index(
		&self,
		_snapshot: Snapshot,
		_class: &str,
		_graph: Option<&GraphRef>,
		_index: IndexId,
		_terms: &std::collections::BTreeSet<Box<str>>,
	) -> FerrosGrynnResult<Option<PhysicalFulltextBatch>> {
		Ok(None)
	}

	fn geometry_index(
		&self,
		_snapshot: Snapshot,
		_class: &str,
		_graph: Option<&GraphRef>,
		_index: IndexId,
		_query: &crate::values::Geometry,
		_admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Option<crate::datastore::QueryGeometryIndexRead>> {
		Ok(None)
	}

	fn vector_index(
		&self,
		_snapshot: Snapshot,
		_class: &str,
		_graph: Option<&GraphRef>,
		_index: IndexId,
		_query: &crate::values::Vector,
		_k: usize,
		_breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<PhysicalVectorHit>>> {
		Ok(None)
	}

	fn path_lower_bound(
		&self,
		_snapshot: Snapshot,
		_binding: &crate::datastore::QueryLandmarkPath,
		_source: AtomId,
		_target: AtomId,
	) -> FerrosGrynnResult<Option<Number>> {
		Ok(None)
	}

	fn physical_traversal_candidates(
		&self,
		_snapshot: Snapshot,
		_graph: &GraphRef,
		_source: AtomId,
		_directions: &[crate::syn::MatchDirection],
	) -> FerrosGrynnResult<Option<PhysicalTraversalBatch>> {
		Ok(None)
	}

	fn physical_inner_graph(
		&self,
		_snapshot: Snapshot,
		_graph: &GraphRef,
		_owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalInnerGraphBatch>> {
		Ok(None)
	}

	fn physical_occurrence_fields(
		&self,
		_snapshot: Snapshot,
		_graph: &GraphRef,
		_owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalOccurrenceFieldsBatch>> {
		Ok(None)
	}

	fn graph_rows(
		&self,
		snapshot: Snapshot,
		references: Option<&[GraphRef]>,
	) -> FerrosGrynnResult<Vec<Row>> {
		self.check_snapshot(snapshot)?;
		(self.observe)(ConflictKey::GraphCatalogRange);
		let mut rows = Vec::new();
		let mut visit = |schema: &crate::datastore::GraphSchema| -> FerrosGrynnResult<()> {
			self.work(1)?;
			let graph = QueryRead::graph(self, snapshot, schema.reference())?;
			self.admit_copies(
				schema.name().len(),
				2,
				2 * size_of::<Row>() + 20 * size_of::<Value>() + 1024,
			)?;
			for (key, value) in schema.properties().iter() {
				self.fields.admit_value(value)?;
				self.fields
					.admit(self.root.dictionary.resolve(key).map_or(0, |name| name.len()) + 128)?;
			}
			let properties = super::graph_properties::property_map_to_object(
				schema.properties(),
				&self.root.dictionary,
			)?;
			rows.push(
				Row::new(vec![
					Value::from(schema.reference().clone()),
					Value::from(schema.name().clone()),
					Value::from(schema.reference().kind().name()),
					Value::from(super::graph_properties::storage_mode_name(
						schema.reference().storage_mode(),
					)),
					Value::from(graph.revision()),
					Value::from(graph.node_count()?),
					Value::from(graph.edge_count()?),
					Value::Object(properties),
				])
				.with_stable(vec![Value::from(schema.reference().clone())]),
			);
			Ok(())
		};
		if let Some(references) = references {
			for reference in references {
				let schema = self
					.root
					.schema
					.graph(reference.id())
					.filter(|schema| schema.reference() == reference)
					.ok_or_else(|| {
						FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
					})?;
				visit(&schema)?;
			}
		} else {
			for schema in self.root.schema.graphs() {
				visit(schema)?;
			}
		}
		Ok(rows)
	}

	fn match_candidates(
		&self,
		_snapshot: Snapshot,
		_graph: &GraphRef,
		_binding: &ResolvedMatchBinding,
	) -> FerrosGrynnResult<Vec<AtomId>> {
		Err(FerrosGrynnError::query_feature_not_supported("MATCH source in LIVE logical recipe"))
	}

	fn k_hop_estimate(
		&self,
		_snapshot: Snapshot,
		_occurrence: &AtomOccurrenceRef,
		_mode: crate::graph::TraversalSemantics,
		_radius: u32,
		_index: Option<IndexId>,
		_scratch_bytes: usize,
		_max_work: u64,
		_admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
		_check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<crate::datastore::QueryNeighbourhoodEstimate> {
		Err(FerrosGrynnError::query_feature_not_supported(
			"traversal source in LIVE logical recipe",
		))
	}

	fn savepoint(&self) -> Self::Savepoint {}

	fn restore_savepoint(&mut self, _savepoint: Self::Savepoint) -> FerrosGrynnResult<()> {
		read_only()
	}

	fn assert_invariants(
		&mut self,
		_graph: Option<&GraphRef>,
		_detail_limit: usize,
	) -> FerrosGrynnResult<usize> {
		read_only()
	}

	fn execute_schema(
		&mut self,
		_statement: &LogicalSchemaStatement,
		_slots: &[Value],
		_source: &str,
		_control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		read_only()
	}

	fn execute_dml(
		&mut self,
		_statement: &LogicalDmlStatement,
		_slots: &[Value],
		_source: &str,
		_control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		read_only()
	}

	fn execute_graph_dml(
		&mut self,
		_statement: &LogicalGraphDmlStatement,
		_slots: &[Value],
		_source: &str,
		_control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		read_only()
	}

	fn install_built_graph(
		&mut self,
		_name: &str,
		_mode: crate::syn::DefinitionMode,
		_contract_explicit: bool,
		_graph: crate::graph::MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		read_only()
	}
}

impl QueryCatalog for RootRead<'_> {
	fn admit_analysis(&self, bytes: usize, work: u64) -> FerrosGrynnResult<()> {
		self.work(work)?;
		self.fields.admit(bytes)
	}

	fn schema_epoch(&self) -> u64 {
		self.root.commit_ts
	}

	fn class_definition(
		&self,
		class: &str,
	) -> FerrosGrynnResult<Arc<crate::datastore::ClassSchema>> {
		self.work(1)?;
		if self.root.schema.class(class).is_none() {
			(self.observe)(ConflictKey::ClassCatalogRange);
		}
		let class = self.catalog.class_definition(class)?;
		if let Some(id) = class.id() {
			(self.observe)(ConflictKey::SchemaClass(id));
			(self.observe)(ConflictKey::BaseClassRange(id));
		}
		Ok(class)
	}

	fn schema(&self, class: &str) -> FerrosGrynnResult<super::Schema> {
		let class = self.class_definition(class)?;
		self.fields.admit(
			class
				.fields()
				.len()
				.checked_add(5)
				.and_then(|count| count.checked_mul(2 * size_of::<super::Column>() + 128))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE read schema"))?,
		)?;
		for field in class.fields() {
			self.fields.admit(field.name().len() + class.name().len())?;
			crate::gql::live_lowering::admit_type_shape(
				field.value_type(),
				0,
				1,
				&mut |bytes, work| {
					self.work(work)?;
					self.fields.admit(bytes)
				},
			)?;
		}
		Ok(super::adapter::query_schema(&class))
	}

	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<crate::datastore::ClassSchemaMode> {
		self.class_definition(class).map(|class| class.schema_mode())
	}

	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind> {
		self.class_definition(class).map(|class| class.kind())
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		self.graph_if_exists(name)?
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name.to_owned()))
	}

	fn graph_if_exists(&self, name: &str) -> FerrosGrynnResult<Option<GraphRef>> {
		self.work(1)?;
		(self.observe)(ConflictKey::GraphCatalogRange);
		self.root
			.schema
			.named_graph(name)
			.map(|graph| {
				QueryRead::graph_reference(self, Snapshot(self.root.commit_ts), graph.id())
			})
			.transpose()
	}

	fn functions(
		&self,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<crate::datastore::SchemaFunction>>> {
		(self.observe)(ConflictKey::FunctionCatalogRange);
		let mut functions = Vec::new();
		for function in self.root.schema.functions() {
			self.work(1)?;
			if function.namespace().as_str() == namespace && function.name().as_str() == name {
				self.fields.admit(2 * size_of::<Arc<crate::datastore::SchemaFunction>>())?;
				if let Some(id) = function.id() {
					(self.observe)(ConflictKey::SchemaFunction(id));
				}
				functions.push(function.clone());
			}
		}
		Ok(functions)
	}

	fn function(
		&self,
		id: crate::datastore::FunctionId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaFunction>>> {
		(self.observe)(ConflictKey::SchemaFunction(id));
		for function in self.root.schema.functions() {
			self.work(1)?;
			if function.id() == Some(id) {
				return Ok(Some(function.clone()));
			}
		}
		Ok(None)
	}

	fn analyzer(
		&self,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>> {
		self.work(1)?;
		(self.observe)(ConflictKey::AnalyzerCatalogRange);
		let result = self.catalog.analyzer(name)?;
		if let Some(id) = result.as_ref().and_then(|analyzer| analyzer.id()) {
			(self.observe)(ConflictKey::SchemaAnalyzer(id));
		}
		Ok(result)
	}

	fn analyzer_by_id(
		&self,
		id: crate::datastore::AnalyzerId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>> {
		self.work(1)?;
		(self.observe)(ConflictKey::SchemaAnalyzer(id));
		self.catalog.analyzer_by_id(id)
	}
}

/// A missing candidate-root index is an invalid plan, not a silent rescan.
fn unavailable_index<T>() -> FerrosGrynnResult<T> {
	Err(FerrosGrynnError::invalid_storage_file(
		"LIVE candidate root has no published index binding",
	))
}

/// Root scanning delegates row construction to the same datastore cursor.
struct RootCursor<'read, 'root> {
	/// Borrowed primary class postings, never a collected identity vector.
	rows: QueryStorageCursor<'read>,
	/// Shared candidate root and checked admission policy.
	read: &'read RootRead<'root>,
	/// Compact/full layout selected by the existing physical SELECT plan.
	full_row: bool,
}

impl RowCursor for RootCursor<'_, '_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		let row = self
			.rows
			.next_row_admitted(|atom, occurrence| self.read.admit_row(atom, occurrence))?;
		row.map(|row| {
			control.record_pull();
			super::adapter::storage_row(row, &self.read.root.dictionary, self.full_row)
		})
		.transpose()
	}
}

/// Mutation entry points remain unreachable after ordinary deterministic
/// analysis.
fn read_only<T>() -> FerrosGrynnResult<T> {
	Err(FerrosGrynnError::query_feature_not_supported("mutation in root-bound LIVE evaluation"))
}
