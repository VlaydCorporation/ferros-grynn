use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

use super::{
	AtomStoreRoot,
	BaseAtom,
	ClassId,
	ClassIndexState,
	ClassSchema,
	ClassSchemaMode,
	ConflictKey,
	GlobalAtomId,
	GraphOccurrence,
	GraphRoot,
	GraphSchema,
	IndexId,
	IndexMaintenanceMode,
	IndexStoreRoot,
	OccurrenceState,
	OverlayValue,
	SchemaCatalog,
	index_context,
	rebuild_index_store,
	rebuild_lookup_index_store,
};
use crate::{
	datastore::OccurrenceStateRef,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeTopology, GraphId, GraphRef},
	stats::StatisticsStore,
	values::{Number, PropertyDictionary, PropertyKeyId, PropertyMap, RecordId, Text, Type, Value},
};

/// One immutable MVCC root spanning base atoms and every graph occurrence.
#[derive(Clone)]
pub struct DatabaseRoot {
	/// Commit ts associated with this database root.
	pub(crate) commit_ts: u64,
	/// Persistent origin for portable changefeed cursors.
	pub(crate) database_uuid: uuid::Uuid,
	/// Snapshot-owned correspondence authority, independent of primary atom
	/// maps.
	pub(crate) bridges: Arc<super::BridgeRegistry>,
	/// Nondecreasing UTC publication time in exact epoch nanoseconds.
	pub(crate) committed_at_ns: i128,
	/// One shared history binding, with selective inner copy-on-write maps.
	history_binding: Arc<HistoryRoot>,
	/// Schema associated with this database root.
	pub(crate) schema: Arc<SchemaCatalog>,
	/// Dictionary associated with this database root.
	pub(crate) dictionary: Arc<PropertyDictionary>,
	/// Atoms associated with this database root.
	pub(crate) atoms: Arc<AtomStoreRoot>,
	/// Indexes associated with this database root.
	pub(crate) indexes: Arc<IndexStoreRoot>,
	/// Statistics associated with this database root.
	pub(crate) statistics: Arc<StatisticsStore>,
	/// Immutable geometry page generations retained by RAM snapshots.
	pub(crate) resident_geometry: Arc<BTreeMap<IndexId, crate::storage::ResidentGeometryIndex>>,
	/// Immutable graph artifacts retained by the exact logical snapshot.
	pub(crate) reachability_artifacts: Arc<BTreeMap<IndexId, Arc<super::ReachabilityArtifact>>>,
	/// Immutable neighbourhood projections retained by the logical snapshot.
	pub(crate) neighbourhood_artifacts: Arc<BTreeMap<IndexId, Arc<super::NeighbourhoodArtifact>>>,
	/// Immutable directed landmark tables retained by the logical snapshot.
	pub(crate) path_artifacts: Arc<BTreeMap<IndexId, Arc<super::LandmarkPathArtifact>>>,
	/// Full qualified incidence generations retained by old logical snapshots.
	pub(crate) tensor_artifacts: Arc<BTreeMap<IndexId, Arc<super::TensorArtifact>>>,
	/// Graphs associated with this database root.
	pub(crate) graphs: BTreeMap<GraphId, Arc<GraphRoot>>,
	/// Versions associated with this database root.
	pub(super) versions: Arc<super::versions::VersionStore>,
}

/// Snapshot-owned historical state cloned as one handle by ordinary reads.
#[derive(Clone, Default)]
struct HistoryRoot {
	/// Accepted invocations share snapshot ownership with logical history.
	events: Arc<super::EventOutbox>,
	/// Snapshot-exact physical journal descriptors, separate from logical
	/// payloads.
	event_segments: Arc<[crate::storage::HistorySegment]>,
	/// Reactive definitions share snapshot ownership with retained history.
	live: Arc<super::LiveCatalog>,
	/// Definitions survive removal of the current schema owner.
	catalog: Arc<super::ChangefeedCatalog>,
	/// RAM history is independent of primary records and physical segments.
	history: ChangeHistory,
	/// Durable bounds do not load historical images.
	segments: crate::storage::ChangefeedSegments,
	/// RAM computed envelopes own results independently of primary records.
	live_history: LiveHistoryGroups,
	/// Private physical references remain separate from source history.
	live_segments: crate::storage::LiveHistorySegments,
}

/// Shared logical batches retained independently of the outer snapshot binding.
type ChangeHistory =
	Arc<BTreeMap<(crate::changefeed::ChangefeedId, u64), Arc<super::CapturedCommit>>>;

/// Shared exact computed envelopes retained independently of primary records.
pub(super) type LiveHistoryGroups = Arc<BTreeMap<(crate::live::LiveId, u64), Arc<[u8]>>>;

/// Complete normalized-state violations collected for `ASSERT INVARIANTS`.
pub struct InvariantViolationReport {
	/// Total associated with this invariant violation report.
	total: usize,
	/// Totals associated with this invariant violation report.
	totals: BTreeMap<&'static str, usize>,
	/// Details associated with this invariant violation report.
	details: Vec<(&'static str, String)>,
	/// Detail limit associated with this invariant violation report.
	detail_limit: usize,
	/// First associated with this invariant violation report.
	first: Option<FerrosGrynnError>,
}

impl InvariantViolationReport {
	/// Creates a new `InvariantViolationReport` value.
	pub(crate) fn new(detail_limit: usize) -> Self {
		Self {
			total: 0,
			totals: BTreeMap::new(),
			details: Vec::with_capacity(detail_limit.min(32)),
			detail_limit,
			first: None,
		}
	}

	/// Pushes invariant violation report.
	pub(crate) fn push(&mut self, category: &'static str, error: FerrosGrynnError) {
		self.total = self.total.saturating_add(1);
		*self.totals.entry(category).or_default() += 1;
		if self.details.len() < self.detail_limit {
			self.details.push((category, error.to_string()));
		}
		if self.first.is_none() {
			self.first = Some(error);
		}
	}

	/// Returns the total result.
	pub(crate) const fn total(&self) -> usize {
		self.total
	}

	/// Returns whether there are no logical elements.
	pub(crate) const fn is_empty(&self) -> bool {
		self.total == 0
	}

	/// Returns the totals result.
	pub(crate) fn totals(&self) -> &BTreeMap<&'static str, usize> {
		&self.totals
	}

	/// Returns the details result.
	pub(crate) fn details(&self) -> &[(&'static str, String)] {
		&self.details
	}

	/// Returns the first error result.
	pub(crate) fn first_error(&self) -> Option<&FerrosGrynnError> {
		self.first.as_ref()
	}

	/// Converts this value into first error.
	fn into_first_error(self) -> Option<FerrosGrynnError> {
		self.first
	}
}

impl Default for DatabaseRoot {
	/// Constructs the default initial state of `DatabaseRoot`.
	fn default() -> Self {
		Self {
			commit_ts: 0,
			database_uuid: uuid::Uuid::now_v7(),
			bridges: Arc::default(),
			committed_at_ns: 0,
			history_binding: Arc::default(),
			schema: Arc::new(SchemaCatalog::default()),
			dictionary: Arc::new(PropertyDictionary::new()),
			atoms: Arc::new(AtomStoreRoot::default()),
			indexes: Arc::new(IndexStoreRoot::default()),
			statistics: Arc::new(StatisticsStore::default()),
			resident_geometry: Arc::new(BTreeMap::new()),
			reachability_artifacts: Arc::new(BTreeMap::new()),
			neighbourhood_artifacts: Arc::new(BTreeMap::new()),
			path_artifacts: Arc::new(BTreeMap::new()),
			tensor_artifacts: Arc::new(BTreeMap::new()),
			graphs: BTreeMap::new(),
			versions: Arc::default(),
		}
	}
}

impl DatabaseRoot {
	/// Stamps implicit tombstones through the same FCW/SSI revision authority.
	pub(crate) fn publish_bridge_revisions(&mut self, revision: u64) {
		if self.bridges.touched.is_empty() {
			return;
		}
		let domains = std::mem::take(&mut Arc::make_mut(&mut self.bridges).touched);
		for (database, role) in domains {
			self.mark_version(ConflictKey::BridgeMappings(database, role), revision);
		}
	}

	/// Reads invocation authority from the exact immutable transaction root.
	pub(crate) fn event_outbox(&self) -> &Arc<super::EventOutbox> {
		&self.history_binding.events
	}

	/// Detaches only accepted invocation state on enqueue or marker
	/// publication.
	pub(crate) fn event_outbox_mut(&mut self) -> &mut Arc<super::EventOutbox> {
		&mut Arc::make_mut(&mut self.history_binding).events
	}

	/// Admits only the invocation namespace, independently of data and LIVE
	/// maps.
	pub(crate) fn event_revision_copy_bytes(&self) -> FerrosGrynnResult<usize> {
		self.versions.event_copy_bytes(1)
	}

	/// Completion retention bounds its FCW metadata as well as the outbox map.
	pub(super) fn retain_event_revisions(&mut self) {
		let outbox = Arc::clone(self.event_outbox());
		Arc::make_mut(&mut self.versions).retain_event_invocations(&outbox);
	}

	/// Borrows durable outbox reachability without loading payload pages.
	pub(crate) fn event_outbox_segments(&self) -> &Arc<[crate::storage::HistorySegment]> {
		&self.history_binding.event_segments
	}

	/// Associates a prepared/recovered physical journal with the logical
	/// snapshot.
	pub(crate) fn set_event_outbox_segments(
		&mut self,
		segments: Arc<[crate::storage::HistorySegment]>,
	) {
		Arc::make_mut(&mut self.history_binding).event_segments = segments;
	}

	/// Checks leased primary/schema/capture inputs independently of LIVE
	/// control commits and transfers the latest complete history binding.
	///
	/// # Errors
	///
	/// Returns a serialization failure for changed import inputs, or a
	/// resource failure before copying revision metadata.
	pub(super) fn rebase_import_controls(
		&mut self,
		base: &Self,
		current: &Self,
		transaction: u64,
		maximum: usize,
	) -> FerrosGrynnResult<()> {
		if base.database_uuid != current.database_uuid
			|| !Arc::ptr_eq(&base.atoms, &current.atoms)
			|| !Arc::ptr_eq(&base.dictionary, &current.dictionary)
			|| base.graphs.len() != current.graphs.len()
			|| base
				.graphs
				.iter()
				.zip(&current.graphs)
				.any(|((id, graph), (other, next))| id != other || !Arc::ptr_eq(graph, next))
			|| base.schema.class_versions != current.schema.class_versions
			|| base.changefeeds() != current.changefeeds()
			|| !base.versions.same_primary(&current.versions)
		{
			return Err(FerrosGrynnError::serialization_failure(
				transaction,
				"the import primary, schema or capture inputs changed before publication",
			));
		}
		let from = self.commit_ts;
		let stamp = current.commit_ts.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::serialization_failure(transaction, "the commit clock is exhausted")
		})?;
		let controls_changed = !self.versions.same_controls(&current.versions);
		let restamp = from != stamp;
		let restamp_primary = restamp && self.versions.primary_has_revision(from);
		if controls_changed || restamp_primary {
			let bytes = self.versions.rebase_allocation_bytes(restamp_primary)?;
			if bytes > maximum {
				return Err(FerrosGrynnError::storage_decode_limit_exceeded(
					"import revision scratch",
					bytes as u64,
					maximum as u64,
				));
			}
			// A committed close can equal the provisional stamp. Only data
			// revisions are restamped; current LIVE controls remain shared.
			Arc::make_mut(&mut self.versions).rebase(
				&current.versions,
				from,
				stamp,
				restamp_primary,
			);
		}
		for graph in
			self.graphs.values_mut().filter(|graph| restamp && graph.tensor_revision == from)
		{
			// Changed private graphs have no escaping runtime snapshot. Do
			// not silently deep-copy an unexpectedly retained import graph.
			let graph = Arc::get_mut(graph).ok_or_else(|| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"retained private import graph",
					maximum as u64,
				)
			})?;
			graph.tensor_revision = stamp;
		}
		self.history_binding = Arc::clone(&current.history_binding);
		self.commit_ts = stamp;
		self.advance_commit_time(current.committed_at_ns);
		self.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
		Ok(())
	}

	/// Borrows reactive definitions from this exact logical snapshot.
	pub(crate) fn live_catalog(&self) -> &Arc<super::LiveCatalog> {
		&self.history_binding.live
	}

	/// Detaches only the selected inner catalog on registry mutation.
	pub(crate) fn live_catalog_mut(&mut self) -> &mut Arc<super::LiveCatalog> {
		&mut Arc::make_mut(&mut self.history_binding).live
	}

	/// Shares already-computed historical envelopes without copying their
	/// images.
	pub(crate) fn live_history(&self) -> &LiveHistoryGroups {
		&self.history_binding.live_history
	}

	/// Detaches only the selected history table before a bounded publication.
	pub(crate) fn live_history_mut(&mut self) -> &mut LiveHistoryGroups {
		&mut Arc::make_mut(&mut self.history_binding).live_history
	}

	/// Returns private physical bounds without loading computed images.
	pub(crate) fn live_segments(&self) -> &crate::storage::LiveHistorySegments {
		&self.history_binding.live_segments
	}

	/// Detaches the selected physical descriptor map for bounded publication.
	pub(crate) fn live_segments_mut(&mut self) -> &mut crate::storage::LiveHistorySegments {
		&mut Arc::make_mut(&mut self.history_binding).live_segments
	}

	/// Borrows definitions from this exact logical root.
	#[inline]
	pub(crate) fn changefeeds(&self) -> &Arc<super::ChangefeedCatalog> {
		&self.history_binding.catalog
	}

	/// Detaches the outer binding before selective catalog COW.
	#[inline]
	pub(crate) fn changefeeds_mut(&mut self) -> &mut Arc<super::ChangefeedCatalog> {
		&mut Arc::make_mut(&mut self.history_binding).catalog
	}

	/// Borrows RAM batches without copying their images.
	#[inline]
	pub(crate) fn change_history(&self) -> &ChangeHistory {
		&self.history_binding.history
	}

	/// Detaches the binding before selective RAM-history COW.
	#[inline]
	pub(crate) fn change_history_mut(&mut self) -> &mut ChangeHistory {
		&mut Arc::make_mut(&mut self.history_binding).history
	}

	/// Borrows durable segment references from the same binding.
	#[inline]
	pub(crate) fn changefeed_segments(&self) -> &crate::storage::ChangefeedSegments {
		&self.history_binding.segments
	}

	/// Detaches the binding before selective segment-table COW.
	#[inline]
	pub(crate) fn changefeed_segments_mut(&mut self) -> &mut crate::storage::ChangefeedSegments {
		&mut Arc::make_mut(&mut self.history_binding).segments
	}

	/// Records UTC time without allowing a host clock rollback to reverse it.
	pub(crate) fn advance_commit_time(&mut self, observed_ns: i128) {
		self.committed_at_ns = self.committed_at_ns.max(observed_ns);
		if self
			.changefeeds()
			.definitions
			.values()
			.any(|entry| entry.closed_at == Some(self.commit_ts) && entry.closed_time_ns.is_none())
		{
			let commit_ts = self.commit_ts;
			let committed_at_ns = self.committed_at_ns;
			for entry in Arc::make_mut(self.changefeeds_mut()).definitions.values_mut() {
				if entry.closed_at == Some(commit_ts) && entry.closed_time_ns.is_none() {
					Arc::make_mut(entry).closed_time_ns = Some(committed_at_ns);
				}
			}
		}
	}

	/// Retains descriptors after their source owner disappears in this root.
	pub(crate) fn retire_missing_changefeeds(
		&mut self,
		published_at: u64,
	) -> FerrosGrynnResult<Vec<crate::changefeed::ChangefeedSource>> {
		let sources = self
			.changefeeds()
			.definitions
			.values()
			.filter(|entry| {
				entry.state != crate::changefeed::ChangefeedState::Retired
					&& match entry.source {
						crate::changefeed::ChangefeedSource::Class(class) => {
							self.schema.class_by_id(class).is_none()
						}
						crate::changefeed::ChangefeedSource::Graph(graph) => {
							self.schema.graph(graph).is_none()
						}
					}
			})
			.map(|entry| entry.id);
		let updates = sources
			.map(|id| {
				let mut definition = self
					.changefeeds()
					.definitions
					.get(&id)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("retained changefeed disappeared")
					})?
					.as_ref()
					.clone();
				definition.version = definition.version.next()?;
				definition.state = crate::changefeed::ChangefeedState::Retired;
				definition.published_at = published_at;
				definition.closed_at = Some(published_at);
				definition.closed_time_ns = None;
				definition.set_capture_epoch(published_at, None)?;
				Ok((id, Arc::new(definition)))
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut changed = Vec::with_capacity(updates.len());
		for (id, definition) in updates {
			changed.push(definition.source);
			Arc::make_mut(self.changefeeds_mut()).definitions.insert(id, definition);
		}
		Ok(changed)
	}

	/// Checks geometry constraints owned by a published class in one scope.
	fn validate_geometry_values<'value>(
		&self,
		class: ClassId,
		scope: super::IndexScope,
		value: impl FnMut(PropertyKeyId) -> Option<&'value Value>,
	) -> FerrosGrynnResult<()> {
		if let Some(class) = self.schema.class_by_id(class) {
			class.validate_geometry_values(scope, value)?;
		}
		Ok(())
	}

	/// Excludes every ready deferred user index from planning after a data
	/// mutation and returns every affected deferred definition.
	///
	/// Ready bindings are removed exactly once. Definitions which are already
	/// stale are still returned so that a concurrent catch-up observes later
	/// commits through the bounded pending-index queue.
	pub(crate) fn mark_deferred_indexes_stale(
		&mut self,
	) -> FerrosGrynnResult<Vec<(ClassId, IndexId, super::SchemaVersion)>> {
		if !self.schema.classes.values().any(|class| {
			class.indexes().any(|index| index.maintenance_mode() == IndexMaintenanceMode::Deferred)
		}) {
			return Ok(Vec::new());
		}
		let mut affected = Vec::new();
		let mut newly_stale = Vec::new();
		{
			let schema = Arc::make_mut(&mut self.schema);
			for (class_id, class) in &mut schema.classes {
				let class = Arc::make_mut(class);
				for index in &mut class.indexes {
					if index.maintenance_mode() != IndexMaintenanceMode::Deferred {
						continue;
					}
					let id = index.id().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"published deferred index is missing IndexId",
						)
					})?;
					affected.push((*class_id, id, index.version()));
					if index.state() == ClassIndexState::Ready {
						index.state = ClassIndexState::Stale;
						newly_stale.push(id);
					}
				}
			}
		}
		if !newly_stale.is_empty() {
			let indexes = Arc::make_mut(&mut self.indexes);
			for id in newly_stale {
				indexes.remove_explicit_index(id);
			}
		}
		Ok(affected)
	}

	/// Inserts atom.
	pub(crate) fn insert_atom(
		&mut self,
		class: &ClassSchema,
		record_id: RecordId,
		properties: PropertyMap,
	) -> FerrosGrynnResult<GlobalAtomId> {
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let prepared = self.indexes.prepare_base_insert(class, &properties, &mut context)?;
		Arc::make_mut(&mut self.statistics).insert_base(
			class
				.id()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("atom class has no ClassId"))?,
		)?;
		let id = Arc::make_mut(&mut self.atoms).insert(class, record_id, properties)?;
		Arc::make_mut(&mut self.indexes).apply_base_insert(prepared, id)?;
		Ok(id)
	}

	/// Inserts atom at.
	pub(crate) fn insert_atom_at(
		&mut self,
		id: GlobalAtomId,
		class: &ClassSchema,
		record_id: RecordId,
		properties: PropertyMap,
	) -> FerrosGrynnResult<()> {
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let prepared = self.indexes.prepare_base_insert(class, &properties, &mut context)?;
		Arc::make_mut(&mut self.statistics).insert_base(
			class
				.id()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("atom class has no ClassId"))?,
		)?;
		Arc::make_mut(&mut self.atoms).insert_at(id, class, record_id, properties)?;
		Arc::make_mut(&mut self.indexes).apply_base_insert(prepared, id)?;
		Ok(())
	}

	/// Returns the atom result.
	pub(crate) fn atom(&self, id: GlobalAtomId) -> Option<&BaseAtom> {
		self.atoms.atom(id)
	}

	/// Returns the atom arc result.
	pub(crate) fn atom_arc(&self, id: GlobalAtomId) -> Option<Arc<BaseAtom>> {
		self.atoms.atom_arc(id)
	}

	/// Returns the atom by record result.
	pub(crate) fn atom_by_record(&self, record_id: &RecordId) -> Option<&BaseAtom> {
		self.atoms.resolve(record_id).and_then(|id| self.atom(id))
	}

	/// Resolves atom.
	pub(crate) fn resolve_atom(&self, record_id: &RecordId) -> Option<GlobalAtomId> {
		self.atoms.resolve(record_id)
	}

	/// Sets base property.
	pub(crate) fn set_base_property(
		&mut self,
		global: GlobalAtomId,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<Option<Value>> {
		self.change_base_property(global, key, Some(value))
	}

	/// Removes base property.
	pub(crate) fn remove_base_property(
		&mut self,
		global: GlobalAtomId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		self.change_base_property(global, key, None)
	}

	/// Returns the change base property result.
	fn change_base_property(
		&mut self,
		global: GlobalAtomId,
		key: PropertyKeyId,
		value: Option<Value>,
	) -> FerrosGrynnResult<Option<Value>> {
		let before = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut after = before.clone();
		let previous = match &value {
			Some(value) => after.properties_mut().insert(key, value.clone()),
			None => after.properties_mut().remove(key),
		};
		let memberships = self
			.memberships(global)
			.into_iter()
			.flat_map(|memberships| memberships.iter())
			.map(|(graph, local)| {
				self.occurrence(*graph, *local)
					.cloned()
					.map(|occurrence| (*graph, occurrence))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"reverse membership references an absent occurrence",
						)
					})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let class = self.schema.class_by_id(after.class_id());
		if let Some(class) = &class {
			class.validate_geometry_values(super::IndexScope::Base, |key| {
				after.properties().get(key)
			})?;
		}
		for (graph, occurrence) in &memberships {
			self.validate_geometry_values(
				after.class_id(),
				super::IndexScope::Effective {
					graph: *graph,
				},
				|key| occurrence.effective_value(after.properties().get(key), key),
			)?;
		}
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let prepared = self.indexes.prepare_base_replace(
			&before,
			&after,
			memberships.iter().map(|(graph, occurrence)| Ok((*graph, occurrence))),
			&mut context,
		)?;
		let indexes = if prepared.is_empty() {
			None
		} else {
			let mut indexes = self.indexes.as_ref().clone();
			indexes.apply_replace(prepared)?;
			Some(indexes)
		};
		let mut statistics = self.statistics.as_ref().clone();
		statistics.replace_base_property(&before, &after, key, &memberships)?;
		// Complete fallible derived-state preparation before mutating primary
		// state. A unique private root can reuse its registry across setters;
		// shared snapshot/savepoint roots still detach through copy-on-write.
		let atom = Arc::make_mut(&mut self.atoms)
			.atom_mut(global)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		match value {
			Some(value) => {
				atom.properties_mut().insert(key, value);
			}
			None => {
				atom.properties_mut().remove(key);
			}
		}
		if let Some(indexes) = indexes {
			self.indexes = Arc::new(indexes);
		}
		self.statistics = Arc::new(statistics);
		if before != after {
			self.invalidate_path_cost_artifacts(None, false);
		}
		Ok(previous)
	}

	/// Removes atom.
	pub(crate) fn remove_atom(&mut self, global: GlobalAtomId) -> FerrosGrynnResult<BaseAtom> {
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		if self.memberships(global).is_some_and(|memberships| !memberships.is_empty()) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				"a global atom with graph memberships cannot be deleted without CASCADE",
			));
		}
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let mut indexes = self.indexes.as_ref().clone();
		indexes.remove_base(&atom, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.remove_base(atom.class_id())?;
		let mut atoms = self.atoms.as_ref().clone();
		let removed = atoms.remove(global)?;
		self.indexes = Arc::new(indexes);
		self.statistics = Arc::new(statistics);
		self.atoms = Arc::new(atoms);
		if !self.bridges.databases.is_empty() {
			Arc::make_mut(&mut self.bridges).tombstone_atom(atom.record_id())?;
		}
		Ok(removed)
	}

	/// Restores property name.
	pub(crate) fn restore_property_name(
		&mut self,
		key: PropertyKeyId,
		name: Text,
	) -> FerrosGrynnResult<()> {
		Arc::make_mut(&mut self.dictionary).restore(key, name)
	}

	/// Returns the add graph result.
	pub(crate) fn add_graph(&mut self, reference: GraphRef) -> FerrosGrynnResult<()> {
		if self.graphs.contains_key(&reference.id()) {
			return Err(FerrosGrynnError::graph_already_exists(reference.name().to_string()));
		}
		self.graphs.insert(reference.id(), Arc::new(GraphRoot::new(reference)));
		Ok(())
	}

	/// Returns the install class result.
	pub(crate) fn install_class(
		&mut self,
		mut class: ClassSchema,
		published_at: u64,
	) -> FerrosGrynnResult<ClassId> {
		let id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a published class requires ClassId")
		})?;
		if self.schema.class_names.contains_key(class.name().as_str())
			|| self.schema.classes.contains_key(&id)
		{
			return Err(FerrosGrynnError::class_already_exists(class.name().to_string()));
		}
		let mut dictionary = self.dictionary.as_ref().clone();
		for field in &mut class.fields {
			field.class_id = Some(id);
			field.key = Some(dictionary.intern(field.name.clone())?);
		}
		for index in &mut class.indexes {
			if index.state() == ClassIndexState::Ready && index.build_revision == 0 {
				index.build_revision = published_at;
			}
		}
		class.published_at = published_at;
		let mut indexes = self.indexes.as_ref().clone();
		indexes.register_class(&class)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.register_class(id);
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.class_names.insert(class.name().to_string(), id);
		catalog.classes.insert(id, Arc::new(class));
		catalog.class_versions.insert(id, published_at);
		self.dictionary = Arc::new(dictionary);
		self.indexes = Arc::new(indexes);
		self.statistics = Arc::new(statistics);
		self.mark_version(ConflictKey::ClassCatalogRange, published_at);
		Ok(id)
	}

	/// Returns the install graph result.
	pub(crate) fn install_graph(
		&mut self,
		mut schema: GraphSchema,
		published_at: u64,
	) -> FerrosGrynnResult<GraphRef> {
		let reference = schema.reference().clone();
		let mut indexes = self.indexes.as_ref().clone();
		indexes.register_graph(reference.id())?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics
			.register_graph(reference.id(), self.schema.classes().filter_map(|class| class.id()));
		{
			let catalog = Arc::make_mut(&mut self.schema);
			if catalog.graph_names.contains_key(reference.name().as_str())
				|| catalog.graphs.contains_key(&reference.id())
			{
				return Err(FerrosGrynnError::graph_already_exists(reference.name().to_string()));
			}
			schema.published_at = published_at;
			if !schema.is_internal() {
				catalog.graph_names.insert(reference.name().to_string(), reference.id());
			}
			catalog.graphs.insert(reference.id(), Arc::new(schema));
			catalog.graph_versions.insert(reference.id(), published_at);
		}
		self.add_graph(reference.clone())?;
		self.indexes = Arc::new(indexes);
		self.statistics = Arc::new(statistics);
		Ok(reference)
	}

	/// Returns the graph result.
	pub(crate) fn graph(&self, graph: GraphId) -> Option<&GraphRoot> {
		self.graphs.get(&graph).map(Arc::as_ref)
	}

	/// Installs one validated durable graph and restores its reverse
	/// memberships atomically.
	pub(crate) fn install_opened_graph(&mut self, graph: GraphRoot) -> FerrosGrynnResult<()> {
		let graph_id = graph.reference().id();
		let schema = self.schema.graph(graph_id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("durable graph has no schema-catalog definition")
		})?;
		if schema.reference() != graph.reference() || self.graphs.contains_key(&graph_id) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"durable graph identity is absent, duplicated, or disagrees with schema",
			));
		}
		let mut atoms = self.atoms.as_ref().clone();
		for occurrence in graph.occurrences() {
			atoms.restore_membership(occurrence.global_id(), graph_id, occurrence.local_id())?;
		}
		self.atoms = Arc::new(atoms);
		self.graphs.insert(graph_id, Arc::new(graph));
		Ok(())
	}

	/// Reconstructs all derived indexes and statistics after every normalized
	/// source participating in a durable snapshot has been installed.
	pub(crate) fn rebuild_derived_state(&mut self) -> FerrosGrynnResult<()> {
		let scratch = std::env::temp_dir().join("ferros-grynn-statistics");
		self.rebuild_derived_state_at(&scratch)
	}

	/// Reconstructs derived state while bounding histogram sort memory below a
	/// caller-owned scratch root.
	pub(crate) fn rebuild_derived_state_at(
		&mut self,
		scratch_root: &std::path::Path,
	) -> FerrosGrynnResult<()> {
		self.rebuild_indexes()?;
		self.rebuild_statistics_at(scratch_root)?;
		Ok(())
	}

	/// Reconstructs every schema-defined and engine-owned index.
	pub(crate) fn rebuild_indexes(&mut self) -> FerrosGrynnResult<()> {
		self.indexes = Arc::new(rebuild_index_store(self)?);
		Ok(())
	}

	/// Reconstructs engine-owned lookups while leaving immutable explicit
	/// indexes backed by their validated durable files.
	pub(crate) fn rebuild_lookup_indexes(&mut self) -> FerrosGrynnResult<()> {
		self.indexes = Arc::new(rebuild_lookup_index_store(self)?);
		Ok(())
	}

	/// Reconstructs exact counters and bulk histograms with bounded sort
	/// memory.
	pub(crate) fn rebuild_statistics_at(
		&mut self,
		scratch_root: &std::path::Path,
	) -> FerrosGrynnResult<()> {
		self.statistics = Arc::new(crate::stats::analyze_database_at(self, scratch_root)?);
		Ok(())
	}

	/// Reconstructs planner statistics using the maintenance-selected exact or
	/// sampled distribution policy.
	pub(crate) fn rebuild_statistics_with_mode_at(
		&mut self,
		scratch_root: &std::path::Path,
		mode: super::MaintenanceAnalyzeMode,
		target: super::MaintenanceTarget,
	) -> FerrosGrynnResult<()> {
		let method = match mode {
			super::MaintenanceAnalyzeMode::Full => crate::stats::AnalyzeMethod::Full,
			super::MaintenanceAnalyzeMode::Approximate {
				sample_count,
			} => crate::stats::AnalyzeMethod::Approximate {
				sample_count: std::num::NonZeroU32::new(sample_count).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("ANALYZE SAMPLE must be greater than zero")
				})?,
			},
			super::MaintenanceAnalyzeMode::Automatic => {
				return Err(FerrosGrynnError::invalid_argument(
					"automatic ANALYZE mode must be resolved before building statistics",
				));
			}
		};
		let statistics = match target {
			super::MaintenanceTarget::Database => {
				crate::stats::analyze_database_with_method_at(self, scratch_root, method)?
			}
			super::MaintenanceTarget::BaseClass(class) => {
				if self.schema.class_by_id(class).is_none() {
					return Err(FerrosGrynnError::class_does_not_exist(class.to_string()));
				}
				let mut statistics = (*self.statistics).clone();
				*statistics.base_mut() =
					crate::stats::analyze_base_with_method_at(self, scratch_root, method)?;
				statistics.mark_auxiliary_complete();
				statistics
			}
			super::MaintenanceTarget::Graph(graph)
			| super::MaintenanceTarget::EffectiveClass {
				graph,
				..
			} => {
				let mut statistics = (*self.statistics).clone();
				statistics.replace_graph(
					graph,
					crate::stats::analyze_graph_with_method_at(self, graph, scratch_root, method)?,
				);
				statistics.mark_auxiliary_complete();
				statistics
			}
			super::MaintenanceTarget::Index {
				..
			}
			| super::MaintenanceTarget::GraphIndex {
				..
			} => {
				return Err(FerrosGrynnError::invalid_argument(
					"ANALYZE does not accept an index maintenance target",
				));
			}
		};
		self.statistics = Arc::new(statistics);
		Ok(())
	}

	/// Verifies the complete normalized atom/occurrence state selected by
	/// `END IMPORT WITH VALIDATION`.
	pub(crate) fn validate_full_import_state(&self) -> FerrosGrynnResult<()> {
		let report = self.collect_invariant_violations(None, 1);
		report.into_first_error().map_or(Ok(()), Err)
	}

	/// Collects every independently observable normalized-state violation.
	pub(crate) fn collect_invariant_violations(
		&self,
		graph: Option<GraphId>,
		detail_limit: usize,
	) -> InvariantViolationReport {
		let mut report = InvariantViolationReport::new(detail_limit);
		if let Some(graph_id) = graph {
			self.collect_graph_violations(graph_id, true, &mut report);
			return report;
		}

		for atom in self.atoms.atoms() {
			if let Err(error) = self.validate_base_atom(atom) {
				report.push("base_atom", error);
			}
			for (graph_id, local) in self.atoms.memberships(atom.id()).into_iter().flatten() {
				match self.graph(*graph_id).and_then(|graph| graph.occurrence(*local)) {
					Some(occurrence) if occurrence.global_id() == atom.id() => {}
					Some(_) => report.push(
						"reverse_membership",
						invalid("reverse membership references the wrong global atom"),
					),
					None => report.push(
						"reverse_membership",
						invalid("reverse membership references an absent occurrence"),
					),
				}
			}
		}
		let graph_ids = self
			.graphs
			.keys()
			.copied()
			.chain(self.schema.all_graphs().map(|graph| graph.reference().id()))
			.collect::<BTreeSet<_>>();
		for graph_id in graph_ids {
			self.collect_graph_violations(graph_id, false, &mut report);
		}
		report
	}

	/// Collects graph violations.
	fn collect_graph_violations(
		&self,
		graph_id: GraphId,
		validate_base: bool,
		report: &mut InvariantViolationReport,
	) {
		let Some(graph) = self.graph(graph_id) else {
			report.push("graph_root", invalid("graph schema references an absent graph root"));
			return;
		};
		match self.schema.graph(graph_id) {
			Some(schema) if schema.reference() == graph.reference() => {}
			Some(_) => {
				report.push("graph_root", invalid("graph root identity disagrees with its schema"));
			}
			None => report.push("graph_root", invalid("graph root has no schema definition")),
		}
		for occurrence in graph.occurrences() {
			let Some(atom) = self.atom(occurrence.global_id()) else {
				report
					.push("occurrence", invalid("graph occurrence references an absent base atom"));
				continue;
			};
			if validate_base && let Err(error) = self.validate_base_atom(atom) {
				report.push("base_atom", error);
			}
			if self.membership(graph_id, atom.id()) != Some(occurrence.local_id()) {
				report.push(
					"reverse_membership",
					invalid("graph occurrence is absent from reverse memberships"),
				);
			}
			let Some(class) = self.schema.class_by_id(atom.class_id()) else {
				report.push("overlay", invalid("graph occurrence atom has no class"));
				continue;
			};
			if let Some(overlay) = occurrence.overlay()
				&& let Err(error) = validate_properties(
					&class,
					overlay.iter().filter_map(|(key, value)| match value {
						OverlayValue::Value(value)
							if self.dictionary.resolve(key).is_none_or(|name| {
								name.as_str() != crate::datastore::LEVEL_PATH_PROPERTY
							}) =>
						{
							Some((key, value))
						}
						OverlayValue::Value(_) => None,
						OverlayValue::Hidden => None,
					}),
				) {
				report.push("overlay", error);
			}
		}
		if validate_base {
			for atom in self.atoms.atoms() {
				let Some(local) =
					self.atoms.memberships(atom.id()).and_then(|items| items.get(&graph_id))
				else {
					continue;
				};
				match graph.occurrence(*local) {
					Some(occurrence) if occurrence.global_id() == atom.id() => {}
					Some(_) => report.push(
						"reverse_membership",
						invalid("reverse membership references the wrong global atom"),
					),
					None => report.push(
						"reverse_membership",
						invalid("reverse membership references an absent occurrence"),
					),
				}
			}
		}
	}

	/// Validates base atom.
	fn validate_base_atom(&self, atom: &BaseAtom) -> FerrosGrynnResult<()> {
		let class = self
			.schema
			.class_by_id(atom.class_id())
			.ok_or_else(|| invalid("base atom references an absent class"))?;
		if class.name().as_str() != atom.record_id().class() || class.kind() != atom.kind() {
			return Err(invalid("base atom identity disagrees with its class"));
		}
		validate_properties(&class, atom.properties().iter())
	}

	/// Keeps engine-owned lookup structures but defers every schema-defined
	/// index until import finalization.
	pub(crate) fn defer_explicit_indexes(&mut self, retain_unique: bool) {
		Arc::make_mut(&mut self.indexes).defer_explicit_indexes(retain_unique);
	}

	/// Attaches node.
	pub(crate) fn attach_node(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.require_kind(global, AtomKind::Node)?;
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
		self.validate_geometry_values(
			atom.class_id(),
			super::IndexScope::Effective {
				graph,
			},
			|key| atom.properties().get(key),
		)?;
		let prepared = self.indexes.prepare_occurrence_insert(graph, &atom, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.insert_fresh_occurrence(graph, &atom, label.as_ref(), None)?;
		let local = self.graph_mut(graph)?.attach_node(global, label, nested_graph)?;
		Arc::make_mut(&mut self.atoms).add_membership(global, graph, local);
		let occurrence = self.occurrence(graph, local).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("attached occurrence disappeared")
		})?;
		Arc::make_mut(&mut self.indexes).apply_occurrence_insert(graph, &occurrence, prepared)?;
		self.statistics = Arc::new(statistics);
		self.invalidate_relation_artifacts(graph);
		Ok(local)
	}

	/// Attaches node at.
	pub(crate) fn attach_node_at(
		&mut self,
		graph: GraphId,
		local: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		self.require_kind(global, AtomKind::Node)?;
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
		self.validate_geometry_values(
			atom.class_id(),
			super::IndexScope::Effective {
				graph,
			},
			|key| atom.properties().get(key),
		)?;
		let prepared = self.indexes.prepare_occurrence_insert(graph, &atom, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.insert_fresh_occurrence(graph, &atom, label.as_ref(), None)?;
		self.graph_mut(graph)?.attach_node_at(local, global, label, nested_graph)?;
		Arc::make_mut(&mut self.atoms).add_membership(global, graph, local);
		let occurrence = self.occurrence(graph, local).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("attached occurrence disappeared")
		})?;
		Arc::make_mut(&mut self.indexes).apply_occurrence_insert(graph, &occurrence, prepared)?;
		self.statistics = Arc::new(statistics);
		self.invalidate_relation_artifacts(graph);
		Ok(())
	}

	/// Attaches edge.
	pub(crate) fn attach_edge(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<AtomId> {
		self.require_kind(global, AtomKind::Edge)?;
		self.validate_qualified_incidence(graph, &topology)?;
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
		self.validate_geometry_values(
			atom.class_id(),
			super::IndexScope::Effective {
				graph,
			},
			|key| atom.properties().get(key),
		)?;
		let prepared = self.indexes.prepare_occurrence_insert(graph, &atom, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.insert_fresh_occurrence(graph, &atom, label.as_ref(), Some(&topology))?;
		let local = self.graph_mut(graph)?.attach_edge(
			global,
			label,
			topology,
			weight,
			transition_graph,
		)?;
		Arc::make_mut(&mut self.atoms).add_membership(global, graph, local);
		let occurrence = self.occurrence(graph, local).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("attached occurrence disappeared")
		})?;
		Arc::make_mut(&mut self.indexes).apply_occurrence_insert(graph, &occurrence, prepared)?;
		self.statistics = Arc::new(statistics);
		self.invalidate_relation_artifacts(graph);
		Ok(local)
	}

	/// Attaches edge at.
	pub(crate) fn attach_edge_at(
		&mut self,
		graph: GraphId,
		local: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		self.require_kind(global, AtomKind::Edge)?;
		self.validate_qualified_incidence(graph, &topology)?;
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
		self.validate_geometry_values(
			atom.class_id(),
			super::IndexScope::Effective {
				graph,
			},
			|key| atom.properties().get(key),
		)?;
		let prepared = self.indexes.prepare_occurrence_insert(graph, &atom, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.insert_fresh_occurrence(graph, &atom, label.as_ref(), Some(&topology))?;
		self.graph_mut(graph)?.attach_edge_at(
			local,
			global,
			label,
			topology,
			weight,
			transition_graph,
		)?;
		Arc::make_mut(&mut self.atoms).add_membership(global, graph, local);
		let occurrence = self.occurrence(graph, local).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("attached occurrence disappeared")
		})?;
		Arc::make_mut(&mut self.indexes).apply_occurrence_insert(graph, &occurrence, prepared)?;
		self.statistics = Arc::new(statistics);
		self.invalidate_relation_artifacts(graph);
		Ok(())
	}

	/// Restores one edge occurrence before the complete edge-endpoint set is
	/// available. The caller must validate the graph after every edge is
	/// installed.
	pub(crate) fn restore_edge_at(
		&mut self,
		graph: GraphId,
		local: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		self.require_kind(global, AtomKind::Edge)?;
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
		self.validate_geometry_values(
			atom.class_id(),
			super::IndexScope::Effective {
				graph,
			},
			|key| atom.properties().get(key),
		)?;
		let prepared = self.indexes.prepare_occurrence_insert(graph, &atom, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.insert_fresh_occurrence(graph, &atom, label.as_ref(), Some(&topology))?;
		self.graph_mut(graph)?.restore_edge_at(
			local,
			global,
			label,
			topology,
			weight,
			transition_graph,
		)?;
		Arc::make_mut(&mut self.atoms).add_membership(global, graph, local);
		let occurrence = self.occurrence(graph, local).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("restored occurrence disappeared")
		})?;
		Arc::make_mut(&mut self.indexes).apply_occurrence_insert(graph, &occurrence, prepared)?;
		self.statistics = Arc::new(statistics);
		self.invalidate_relation_artifacts(graph);
		Ok(())
	}

	/// Validates graph endpoints.
	pub(crate) fn validate_graph_endpoints(&self, graph: GraphId) -> FerrosGrynnResult<()> {
		self.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?
			.validate_restored_endpoints()
	}

	/// Detaches database root.
	pub(crate) fn detach(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
	) -> FerrosGrynnResult<Option<GraphOccurrence>> {
		self.detach_with_bridge_lifecycle(graph, global, true)
	}

	/// Retains mappings while atomic BUILD replaces a surviving occurrence.
	pub(crate) fn detach_for_replacement(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
	) -> FerrosGrynnResult<Option<GraphOccurrence>> {
		self.detach_with_bridge_lifecycle(graph, global, false)
	}

	/// Removes local membership and applies the requested mapping lifecycle.
	fn detach_with_bridge_lifecycle(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
		tombstone: bool,
	) -> FerrosGrynnResult<Option<GraphOccurrence>> {
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let occurrence =
			self.membership(graph, global).and_then(|local| self.occurrence(graph, local).cloned());
		let mut statistics = self.statistics.as_ref().clone();
		if let Some(occurrence) = &occurrence {
			let mut context = index_context(&self.schema, &self.dictionary)?;
			Arc::make_mut(&mut self.indexes).remove_occurrence(
				graph,
				&atom,
				occurrence,
				&mut context,
			)?;
			statistics.remove_occurrence(graph, &atom, occurrence)?;
		}
		let removed = self.graph_mut(graph)?.detach(global);
		if removed != occurrence {
			return Err(FerrosGrynnError::invalid_storage_file(
				"graph membership changed while detaching an occurrence",
			));
		}
		if removed.is_some() {
			if tombstone && !self.bridges.databases.is_empty() {
				Arc::make_mut(&mut self.bridges).tombstone_subject(
					&crate::bridge::BridgeMappingSubject::Relation {
						graph,
						edge: atom.record_id().clone(),
					},
				)?;
			}
			Arc::make_mut(&mut self.atoms).remove_membership(global, graph);
			self.statistics = Arc::new(statistics);
			self.invalidate_relation_artifacts(graph);
		}
		Ok(removed)
	}

	/// Returns the occurrence result.
	pub(crate) fn occurrence(&self, graph: GraphId, local: AtomId) -> Option<&GraphOccurrence> {
		self.graph(graph)?.occurrence(local)
	}

	/// Returns the membership result.
	pub(crate) fn membership(&self, graph: GraphId, global: GlobalAtomId) -> Option<AtomId> {
		self.graph(graph)?.local_id(global)
	}

	/// Returns the memberships result.
	pub(crate) fn memberships(&self, global: GlobalAtomId) -> Option<&BTreeMap<GraphId, AtomId>> {
		self.atoms.memberships(global)
	}

	/// Returns whether the inherits property condition holds.
	pub(crate) fn inherits_property(
		&self,
		graph: GraphId,
		global: GlobalAtomId,
		key: PropertyKeyId,
	) -> bool {
		self.membership(graph, global)
			.and_then(|local| self.occurrence(graph, local))
			.is_some_and(|occurrence| occurrence.inherits(key))
	}

	/// Sets overlay.
	pub(crate) fn set_overlay(
		&mut self,
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<()> {
		self.replace_occurrence(graph, local, |occurrence| {
			occurrence.set_overlay(key, OverlayValue::Value(value));
		})
	}

	/// Returns the hide property result.
	pub(crate) fn hide_property(
		&mut self,
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<()> {
		self.replace_occurrence(graph, local, |occurrence| {
			occurrence.set_overlay(key, OverlayValue::Hidden);
		})
	}

	/// Resets property.
	pub(crate) fn reset_property(
		&mut self,
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<()> {
		self.replace_occurrence(graph, local, |occurrence| occurrence.reset_overlay(key))
	}

	/// Sets occurrence label.
	pub(crate) fn set_occurrence_label(
		&mut self,
		graph: GraphId,
		local: AtomId,
		label: Option<Text>,
	) -> FerrosGrynnResult<()> {
		self.replace_occurrence(graph, local, |occurrence| occurrence.set_label(label))
	}

	/// Replaces occurrence state.
	pub(crate) fn replace_occurrence_state(
		&mut self,
		graph: GraphId,
		local: AtomId,
		state: OccurrenceState,
	) -> FerrosGrynnResult<()> {
		if let OccurrenceStateRef::Edge {
			topology,
			..
		} = state.view()
		{
			self.validate_qualified_incidence(graph, topology)?;
		}
		self.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?
			.validate_state(local, &state)?;
		self.replace_occurrence(graph, local, |occurrence| occurrence.replace_state(state))
	}

	/// Returns the effective property result.
	pub(crate) fn effective_property(
		&self,
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<&Value>> {
		let occurrence = self.occurrence(graph, local).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", local.to_string())
		})?;
		let base = self.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("global", occurrence.global_id().to_string())
		})?;
		Ok(occurrence.effective_value(base.properties().get(key), key))
	}

	/// Replaces occurrence.
	fn replace_occurrence(
		&mut self,
		graph: GraphId,
		local: AtomId,
		change: impl FnOnce(&mut GraphOccurrence),
	) -> FerrosGrynnResult<()> {
		let before = self.occurrence(graph, local).cloned().ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", local.to_string())
		})?;
		let atom = self.atom(before.global_id()).cloned().ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("global", before.global_id().to_string())
		})?;
		let mut after = before.clone();
		change(&mut after);
		let cost_changed = before != after;
		let weight_changed = matches!((before.state().view(), after.state().view()), (OccurrenceStateRef::Edge { weight: before, .. }, OccurrenceStateRef::Edge { weight: after, .. }) if before != after);
		let tensor_changed = before.label() != after.label() || before.state() != after.state();
		let class = self.schema.class_by_id(atom.class_id());
		let relation_changed = before.label() != after.label() && local.is_edge()
			|| match (before.state().view(), after.state().view()) {
				(
					OccurrenceStateRef::Edge {
						topology: before,
						transition_graph: before_graph,
						..
					},
					OccurrenceStateRef::Edge {
						topology: after,
						transition_graph: after_graph,
						..
					},
				) => before != after || before_graph != after_graph,
				(
					OccurrenceStateRef::Node {
						nested_graph: before,
					},
					OccurrenceStateRef::Node {
						nested_graph: after,
					},
				) => before != after,
				_ => true,
			};
		if let Some(class) = &class {
			class.validate_geometry_values(
				super::IndexScope::Effective {
					graph,
				},
				|key| after.effective_value(atom.properties().get(key), key),
			)?;
		}
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let prepared =
			self.indexes.prepare_occurrence_replace(graph, &atom, &before, &after, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.replace_occurrence(graph, &atom, &before, &after)?;
		if !prepared.is_empty() {
			Arc::make_mut(&mut self.indexes).apply_occurrence_replace(prepared)?;
		}
		self.graph_mut(graph)?.replace_occurrence(after);
		self.statistics = Arc::new(statistics);
		if relation_changed {
			self.invalidate_relation_artifacts(graph);
		}
		if cost_changed {
			self.invalidate_path_cost_artifacts(None, false);
		}
		if weight_changed {
			self.invalidate_path_cost_artifacts(Some(graph), true);
		}
		if tensor_changed {
			self.invalidate_tensor_artifacts(graph);
		}
		Ok(())
	}

	/// Returns the version result.
	pub(crate) fn version(&self, key: ConflictKey) -> u64 {
		if let ConflictKey::SchemaClass(class) = key {
			return self.schema.class_versions.get(&class).copied().unwrap_or(0);
		}
		self.versions.get(&key).copied().unwrap_or(0)
	}

	/// Marks version.
	pub(crate) fn mark_version(&mut self, key: ConflictKey, commit_ts: u64) {
		Arc::make_mut(&mut self.versions).insert(key, commit_ts);
		if let ConflictKey::GraphIncidence(graph, _) | ConflictKey::GraphMembership(graph, _) = key
			&& let Some(root) = self.graphs.get_mut(&graph)
		{
			Arc::make_mut(root).tensor_revision = commit_ts;
		}
	}

	/// Advances identity clocks.
	pub(crate) fn advance_identity_clocks(&mut self, next_schema_id: u64, next_graph_id: u64) {
		if self.schema.next_schema_id == next_schema_id
			&& self.schema.next_graph_id == next_graph_id
		{
			return;
		}
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.next_schema_id = next_schema_id;
		catalog.next_graph_id = next_graph_id;
	}

	/// Publishes statistics revision.
	pub(crate) fn publish_statistics_revision(&mut self, revision: u64) {
		if !self.statistics.has_revision_updates() {
			return;
		}
		Arc::make_mut(&mut self.statistics).publish_revision(revision);
	}

	/// Returns the graph mut result.
	fn graph_mut(&mut self, graph: GraphId) -> FerrosGrynnResult<&mut GraphRoot> {
		self.graphs
			.get_mut(&graph)
			.map(Arc::make_mut)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))
	}

	/// Excludes graph relation artifacts after a successful relation
	/// mutation.
	pub(super) fn invalidate_relation_artifacts(&mut self, graph: GraphId) {
		self.invalidate_tensor_artifacts(graph);
		let Some(schema) = self.schema.graph(graph) else {
			return;
		};
		if !schema.indexes().any(|index| {
			index.state() == ClassIndexState::Ready
				&& !index.is_on_demand()
				&& matches!(
					index.definition(),
					super::GraphIndexDefinition::Reachability { .. }
						| super::GraphIndexDefinition::Neighbourhood { .. }
						| super::GraphIndexDefinition::Path { .. }
				)
		}) {
			return;
		}
		if let Some(schema) = Arc::make_mut(&mut self.schema).graphs.get_mut(&graph) {
			for index in &mut Arc::make_mut(schema).indexes {
				if !index.is_on_demand()
					&& matches!(
						index.definition(),
						super::GraphIndexDefinition::Reachability { .. }
							| super::GraphIndexDefinition::Neighbourhood { .. }
							| super::GraphIndexDefinition::Path { .. }
					) {
					index.state = ClassIndexState::Stale;
					if let Some(id) = index.id() {
						Arc::make_mut(&mut self.reachability_artifacts).remove(&id);
						Arc::make_mut(&mut self.neighbourhood_artifacts).remove(&id);
						Arc::make_mut(&mut self.path_artifacts).remove(&id);
					}
				}
			}
		}
	}

	/// Invalidates every cache that captured this graph, including rooted
	/// owners.
	fn invalidate_tensor_artifacts(&mut self, graph: GraphId) {
		let relevant = |artifact: &super::TensorArtifact| {
			artifact.dependencies.iter().any(|(dependency, _)| *dependency == graph)
		};
		if !self.tensor_artifacts.values().any(|artifact| relevant(artifact)) {
			return;
		}
		let artifacts = Arc::make_mut(&mut self.tensor_artifacts);
		for schema in Arc::make_mut(&mut self.schema).graphs.values_mut() {
			if !schema.indexes().any(|index| {
				index
					.id()
					.and_then(|id| artifacts.get(&id))
					.is_some_and(|artifact| relevant(artifact))
			}) {
				continue;
			}
			for index in &mut Arc::make_mut(schema).indexes {
				if let Some(id) = index.id()
					&& artifacts.get(&id).is_some_and(|artifact| relevant(artifact))
				{
					index.state = ClassIndexState::Stale;
					artifacts.remove(&id);
				}
			}
		}
	}

	/// Excludes cost-dependent artifacts in the same private root. PURE
	/// functions may reference other records, so property changes
	/// conservatively cover all graphs.
	fn invalidate_path_cost_artifacts(&mut self, graph: Option<GraphId>, weighted: bool) {
		if self.path_artifacts.is_empty() {
			return;
		}
		let catalog = Arc::make_mut(&mut self.schema);
		for (id, definition) in &mut catalog.graphs {
			if graph.is_some_and(|selected| selected != *id) {
				continue;
			}
			let relevant = |index: &super::GraphIndex| {
				index.state() == ClassIndexState::Ready
					&& !index.is_on_demand()
					&& matches!(index.definition(), super::GraphIndexDefinition::Path { cost, .. } if if weighted { *cost == super::GraphIndexCost::Weighted } else { matches!(cost, super::GraphIndexCost::Function { .. }) })
			};
			if !definition.indexes().any(relevant) {
				continue;
			}
			for index in &mut Arc::make_mut(definition).indexes {
				if relevant(index) {
					index.state = ClassIndexState::Stale;
					if let Some(id) = index.id() {
						Arc::make_mut(&mut self.path_artifacts).remove(&id);
					}
				}
			}
		}
	}

	/// Returns the require kind result.
	fn require_kind(&self, global: GlobalAtomId, expected: AtomKind) -> FerrosGrynnResult<()> {
		let atom = self
			.atom(global)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		if atom.kind() != expected {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				atom.record_id().to_string(),
				atom.kind().to_string(),
				expected.to_string(),
			));
		}
		Ok(())
	}
}

/// Validates properties.
fn validate_properties<'value>(
	class: &ClassSchema,
	properties: impl IntoIterator<Item = (PropertyKeyId, &'value Value)>,
) -> FerrosGrynnResult<()> {
	for (key, value) in properties {
		let field = class.fields().find(|field| field.key == Some(key));
		if class.schema_mode() == ClassSchemaMode::Schemafull && field.is_none() {
			return Err(invalid("schemafull atom contains an undeclared property"));
		}
		if let Some(field) = field
			&& !compatible_type(field.value_type(), &value.value_type())
		{
			return Err(invalid("atom property value disagrees with its schema field"));
		}
	}
	Ok(())
}

/// Returns whether the compatible type condition holds.
fn compatible_type(expected: &Type, actual: &Type) -> bool {
	expected == actual
		|| matches!(expected, Type::Any)
		|| matches!(expected, Type::Number)
			&& matches!(actual, Type::Number | Type::Int | Type::Float | Type::Decimal)
		|| matches!(expected, Type::Option(inner) if matches!(actual, Type::None | Type::Null) || compatible_type(inner, actual))
		|| matches!(expected, Type::Union(types) if types.iter().any(|ty| compatible_type(ty, actual)))
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Regression tests for root behavior.
mod tests {
	use std::sync::Arc;

	use super::DatabaseRoot;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		datastore::{ClassId, ClassSchema, GraphRoot},
		graph::{AtomKind, EdgeTopology, GraphId, GraphKind, GraphRef, GraphStorageMode},
		values::{Number, PropertyKeyId, PropertyMap, RecordId, RecordIdPart, Value},
	};

	/// Returns class for this module.
	fn class(id: u32, name: &str, kind: AtomKind) -> ClassSchema {
		let mut class = ClassSchema::new(name, kind, []).assert_ok();
		class.id = ClassId::new(id);
		class
	}

	/// Returns graph for this module.
	fn graph(id: u64, name: &str) -> GraphRef {
		GraphRef::new(
			GraphId::new(id).assert_some(),
			name,
			GraphKind::Graph,
			GraphStorageMode::InMemory,
		)
		.assert_ok()
	}

	/// One component's COW must preserve both sibling handles and the old root.
	#[test]
	fn changefeed_binding_detaches_selectively() {
		use crate::test_assertions::AssertResult as _;

		let mut original = DatabaseRoot::default();
		let feed = crate::changefeed::ChangefeedId::allocate();
		let duration = "P1D"
			.parse::<crate::values::Duration>()
			.map_err(|error| {
				crate::gql_status::FerrosGrynnError::invalid_argument(error.to_string())
			})
			.assert_ok();
		Arc::make_mut(original.changefeeds_mut())
			.configure(
				crate::changefeed::ChangefeedSource::Class(
					ClassId::new(1)
						.ok_or_else(|| {
							crate::gql_status::FerrosGrynnError::invalid_argument(
								"fixture class identity",
							)
						})
						.assert_ok(),
				),
				feed,
				"item".into(),
				Some(crate::changefeed::ChangefeedConfig::new(duration).assert_ok()),
				0,
			)
			.assert_ok();
		for component in 0..3 {
			let mut changed = original.clone();
			match component {
				0 => {
					Arc::make_mut(changed.changefeeds_mut()).definitions.remove(&feed);
				}
				1 => Arc::make_mut(changed.change_history_mut()).clear(),
				_ => Arc::make_mut(changed.changefeed_segments_mut()).clear(),
			}
			assert_eq!(Arc::ptr_eq(original.changefeeds(), changed.changefeeds()), component != 0);
			assert_eq!(
				Arc::ptr_eq(original.change_history(), changed.change_history()),
				component != 1
			);
			assert_eq!(
				Arc::ptr_eq(original.changefeed_segments(), changed.changefeed_segments()),
				component != 2
			);
			assert!(original.changefeeds().definitions.contains_key(&feed));
			assert_eq!(changed.changefeeds().definitions.contains_key(&feed), component != 0);
		}
	}

	/// Verifies invariant report counts all violations and bounds details.
	#[test]
	fn invariant_report_counts_all_violations_and_bounds_details() {
		let mut root = DatabaseRoot::default();
		for (id, name) in [(1, "first"), (2, "second")] {
			let reference = graph(id, name);
			root.graphs.insert(reference.id(), Arc::new(GraphRoot::new(reference)));
		}

		let report = root.collect_invariant_violations(None, 1);
		assert_eq!(report.total(), 2);
		assert_eq!(report.totals().get("graph_root"), Some(&2));
		assert_eq!(report.details().len(), 1);
	}

	/// Verifies shares base and isolates lazy overlays.
	#[test]
	fn shares_base_and_isolates_lazy_overlays() {
		let name = PropertyKeyId::new(1);
		let mut properties = PropertyMap::new();
		properties.insert(name, "base");
		let mut root = DatabaseRoot::default();
		let person = class(1, "person", AtomKind::Node);
		let global = root
			.insert_atom(
				&person,
				RecordId::new("person", RecordIdPart::String("ada".into())).assert_ok(),
				properties,
			)
			.assert_ok();
		let first = graph(1, "first");
		let second = graph(2, "second");
		let first_id = first.id();
		let second_id = second.id();
		install_graph(&mut root, first, 10);
		install_graph(&mut root, second, 12);
		let first_local = root.attach_node(first_id, global, Some("A".into()), None).assert_ok();
		let second_local = root.attach_node(second_id, global, Some("B".into()), None).assert_ok();

		assert_eq!(
			root.effective_property(first_id, first_local, name).assert_ok(),
			Some(&Value::from("base"))
		);
		assert!(!root.occurrence(first_id, first_local).assert_some().has_overlay());
		root.set_overlay(first_id, first_local, name, Value::from("private")).assert_ok();
		root.set_base_property(global, name, Value::from("updated")).assert_ok();
		assert_eq!(
			root.effective_property(first_id, first_local, name).assert_ok(),
			Some(&Value::from("private"))
		);
		assert_eq!(
			root.effective_property(second_id, second_local, name).assert_ok(),
			Some(&Value::from("updated"))
		);

		root.hide_property(first_id, first_local, name).assert_ok();
		assert_eq!(root.effective_property(first_id, first_local, name).assert_ok(), None);
		root.reset_property(first_id, first_local, name).assert_ok();
		assert_eq!(
			root.effective_property(first_id, first_local, name).assert_ok(),
			Some(&Value::from("updated"))
		);
		assert!(!root.occurrence(first_id, first_local).assert_some().has_overlay());
	}

	/// Verifies global node and edge use distinct atom store slots.
	#[test]
	fn global_node_and_edge_use_distinct_atom_store_slots() {
		let mut root = DatabaseRoot::default();
		let node = class(1, "person", AtomKind::Node);
		let edge = class(2, "knows", AtomKind::Edge);
		let node_id = root
			.insert_atom(
				&node,
				RecordId::new("person", RecordIdPart::Int(1)).assert_ok(),
				PropertyMap::new(),
			)
			.assert_ok();
		let edge_id = root
			.insert_atom(
				&edge,
				RecordId::new("knows", RecordIdPart::Int(1)).assert_ok(),
				PropertyMap::new(),
			)
			.assert_ok();

		assert_ne!(node_id.slot(), edge_id.slot());
	}

	/// Verifies permits unattached property records.
	#[test]
	fn permits_unattached_property_records() {
		let mut root = DatabaseRoot::default();
		let property = class(1, "property", AtomKind::Node);
		let record = RecordId::new("property", RecordIdPart::Int(42)).assert_ok();
		let global = root.insert_atom(&property, record.clone(), PropertyMap::new()).assert_ok();

		assert_eq!(root.atom_by_record(&record).assert_some().id(), global);
		assert!(root.memberships(global).is_none());
	}

	/// Verifies one global edge has independent graph topology.
	#[test]
	fn one_global_edge_has_independent_graph_topology() {
		let mut root = DatabaseRoot::default();
		let person = class(1, "person", AtomKind::Node);
		let relation = class(2, "knows", AtomKind::Edge);
		let left = root
			.insert_atom(
				&person,
				RecordId::new("person", RecordIdPart::String("left".into())).assert_ok(),
				PropertyMap::new(),
			)
			.assert_ok();
		let right = root
			.insert_atom(
				&person,
				RecordId::new("person", RecordIdPart::String("right".into())).assert_ok(),
				PropertyMap::new(),
			)
			.assert_ok();
		let edge = root
			.insert_atom(
				&relation,
				RecordId::new("knows", RecordIdPart::String("edge".into())).assert_ok(),
				PropertyMap::new(),
			)
			.assert_ok();
		let first = graph(1, "first");
		let second = graph(2, "second");
		let first_id = first.id();
		let second_id = second.id();
		install_graph(&mut root, first, 10);
		install_graph(&mut root, second, 12);
		let first_left = root.attach_node(first_id, left, None, None).assert_ok();
		let first_right = root.attach_node(first_id, right, None, None).assert_ok();
		let second_left = root.attach_node(second_id, left, None, None).assert_ok();
		let second_right = root.attach_node(second_id, right, None, None).assert_ok();
		let first_edge = root
			.attach_edge(
				first_id,
				edge,
				Some("friend".into()),
				EdgeTopology::directed([first_left], [first_right]),
				Some(Number::from(1_i64)),
				None,
			)
			.assert_ok();
		let second_edge = root
			.attach_edge(
				second_id,
				edge,
				Some("blocked".into()),
				EdgeTopology::directed([second_right], [second_left]),
				Some(Number::from(2_i64)),
				None,
			)
			.assert_ok();

		assert_ne!(
			root.occurrence(first_id, first_edge).assert_some().state(),
			root.occurrence(second_id, second_edge).assert_some().state()
		);
		assert_eq!(root.memberships(edge).assert_some().len(), 2);
	}

	/// Returns install graph for this module.
	fn install_graph(root: &mut DatabaseRoot, reference: GraphRef, first_index: u32) {
		use crate::datastore::{GraphSchema, IndexId, SchemaVersion, SystemIndex, SystemIndexKind};

		let schema = GraphSchema::new(
			SchemaVersion::INITIAL,
			reference,
			SystemIndex::new(IndexId::new(first_index).assert_some(), SystemIndexKind::Label),
			SystemIndex::new(
				IndexId::new(first_index + 1).assert_some(),
				SystemIndexKind::Adjacency,
			),
			crate::values::PropertyMap::new(),
			crate::graph::GraphTopologyConstraints::empty(),
			None,
			None,
			0,
		);
		root.install_graph(schema, 0).assert_ok();
	}
}
