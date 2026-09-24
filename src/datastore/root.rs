use std::{collections::BTreeMap, sync::Arc};

use super::{
	AtomStoreRoot,
	BaseAtom,
	ClassId,
	ClassSchema,
	ClassSchemaMode,
	ConflictKey,
	GlobalAtomId,
	GraphOccurrence,
	GraphRoot,
	GraphSchema,
	IndexId,
	IndexStoreRoot,
	OccurrenceState,
	OverlayValue,
	SchemaCatalog,
	index_context,
	rebuild_index_store,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeTopology, GraphId, GraphRef},
	stats::StatisticsStore,
	values::{Number, PropertyDictionary, PropertyKeyId, PropertyMap, RecordId, Text, Type, Value},
};

/// One immutable MVCC root spanning base atoms and every graph occurrence.
#[derive(Clone)]
pub(crate) struct DatabaseRoot {
	pub(crate) commit_ts: u64,
	pub(crate) schema: Arc<SchemaCatalog>,
	pub(crate) dictionary: Arc<PropertyDictionary>,
	pub(crate) atoms: Arc<AtomStoreRoot>,
	pub(crate) indexes: Arc<IndexStoreRoot>,
	pub(crate) statistics: Arc<StatisticsStore>,
	pub(crate) graphs: BTreeMap<GraphId, Arc<GraphRoot>>,
	pub(crate) versions: BTreeMap<ConflictKey, u64>,
}

impl Default for DatabaseRoot {
	fn default() -> Self {
		Self {
			commit_ts: 0,
			schema: Arc::new(SchemaCatalog::default()),
			dictionary: Arc::new(PropertyDictionary::new()),
			atoms: Arc::new(AtomStoreRoot::default()),
			indexes: Arc::new(IndexStoreRoot::default()),
			statistics: Arc::new(StatisticsStore::default()),
			graphs: BTreeMap::new(),
			versions: BTreeMap::new(),
		}
	}
}

impl DatabaseRoot {
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

	pub(crate) fn atom(&self, id: GlobalAtomId) -> Option<&BaseAtom> {
		self.atoms.atom(id)
	}

	pub(crate) fn atom_arc(&self, id: GlobalAtomId) -> Option<Arc<BaseAtom>> {
		self.atoms.atom_arc(id)
	}

	pub(crate) fn atom_by_record(&self, record_id: &RecordId) -> Option<&BaseAtom> {
		self.atoms.resolve(record_id).and_then(|id| self.atom(id))
	}

	pub(crate) fn resolve_atom(&self, record_id: &RecordId) -> Option<GlobalAtomId> {
		self.atoms.resolve(record_id)
	}

	pub(crate) fn set_base_property(
		&mut self,
		global: GlobalAtomId,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<Option<Value>> {
		self.change_base_property(global, key, Some(value))
	}

	pub(crate) fn remove_base_property(
		&mut self,
		global: GlobalAtomId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		self.change_base_property(global, key, None)
	}

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
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let prepared =
			self.indexes.prepare_base_replace(&before, &after, &memberships, &mut context)?;
		let mut indexes = self.indexes.as_ref().clone();
		indexes.apply_replace(prepared)?;
		let mut atoms = self.atoms.as_ref().clone();
		let atom = atoms
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
		let mut statistics = self.statistics.as_ref().clone();
		statistics.replace_base_property(&before, &after, key, &memberships)?;
		self.indexes = Arc::new(indexes);
		self.atoms = Arc::new(atoms);
		self.statistics = Arc::new(statistics);
		Ok(previous)
	}

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
		Ok(removed)
	}

	pub(crate) fn restore_property_name(
		&mut self,
		key: PropertyKeyId,
		name: Text,
	) -> FerrosGrynnResult<()> {
		Arc::make_mut(&mut self.dictionary).restore(key, name)
	}

	pub(crate) fn add_graph(&mut self, reference: GraphRef) -> FerrosGrynnResult<()> {
		if self.graphs.contains_key(&reference.id()) {
			return Err(FerrosGrynnError::graph_already_exists(reference.name().to_string()));
		}
		self.graphs.insert(reference.id(), Arc::new(GraphRoot::new(reference)));
		Ok(())
	}

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
		Ok(id)
	}

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
			catalog.graph_names.insert(reference.name().to_string(), reference.id());
			catalog.graphs.insert(reference.id(), Arc::new(schema));
			catalog.graph_versions.insert(reference.id(), published_at);
		}
		self.add_graph(reference.clone())?;
		self.indexes = Arc::new(indexes);
		self.statistics = Arc::new(statistics);
		Ok(reference)
	}

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

	/// Reconstructs exact counters and bulk histograms with bounded sort
	/// memory.
	pub(crate) fn rebuild_statistics_at(
		&mut self,
		scratch_root: &std::path::Path,
	) -> FerrosGrynnResult<()> {
		self.statistics = Arc::new(crate::stats::analyze_database_at(self, scratch_root)?);
		Ok(())
	}

	/// Verifies the complete normalized atom/occurrence state selected by
	/// `END IMPORT WITH VALIDATION`.
	pub(crate) fn validate_full_import_state(&self) -> FerrosGrynnResult<()> {
		for atom in self.atoms.atoms() {
			let class = self
				.schema
				.class_by_id(atom.class_id())
				.ok_or_else(|| invalid("base atom references an absent class"))?;
			if class.name().as_str() != atom.record_id().class() || class.kind() != atom.kind() {
				return Err(invalid("base atom identity disagrees with its class"));
			}
			validate_properties(&class, atom.properties().iter())?;
			for (graph_id, local) in self.atoms.memberships(atom.id()).into_iter().flatten() {
				let occurrence =
					self.graph(*graph_id).and_then(|graph| graph.occurrence(*local)).ok_or_else(
						|| invalid("reverse membership references an absent occurrence"),
					)?;
				if occurrence.global_id() != atom.id() {
					return Err(invalid("reverse membership references the wrong global atom"));
				}
			}
		}
		for (graph_id, graph) in &self.graphs {
			let schema = self
				.schema
				.graph(*graph_id)
				.ok_or_else(|| invalid("graph root has no schema definition"))?;
			if schema.reference() != graph.reference() {
				return Err(invalid("graph root identity disagrees with its schema"));
			}
			for occurrence in graph.occurrences() {
				let atom = self
					.atom(occurrence.global_id())
					.ok_or_else(|| invalid("graph occurrence references an absent base atom"))?;
				if self.membership(*graph_id, atom.id()) != Some(occurrence.local_id()) {
					return Err(invalid("graph occurrence is absent from reverse memberships"));
				}
				let class = self
					.schema
					.class_by_id(atom.class_id())
					.ok_or_else(|| invalid("graph occurrence atom has no class"))?;
				if let Some(overlay) = occurrence.overlay() {
					validate_properties(
						&class,
						overlay.iter().filter_map(|(key, value)| match value {
							OverlayValue::Value(value) => Some((key, value)),
							OverlayValue::Hidden => None,
						}),
					)?;
				}
			}
		}
		Ok(())
	}

	/// Keeps engine-owned lookup structures but defers every schema-defined
	/// index until import finalization.
	pub(crate) fn defer_explicit_indexes(&mut self, retain_unique: bool) {
		Arc::make_mut(&mut self.indexes).defer_explicit_indexes(retain_unique);
	}

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
		Ok(local)
	}

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
		Ok(())
	}

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
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
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
		Ok(local)
	}

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
		let atom = self
			.atom(global)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		let mut context = index_context(&self.schema, &self.dictionary)?;
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
		Ok(())
	}

	pub(crate) fn detach(
		&mut self,
		graph: GraphId,
		global: GlobalAtomId,
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
			Arc::make_mut(&mut self.atoms).remove_membership(global, graph);
			self.statistics = Arc::new(statistics);
		}
		Ok(removed)
	}

	pub(crate) fn occurrence(&self, graph: GraphId, local: AtomId) -> Option<&GraphOccurrence> {
		self.graph(graph)?.occurrence(local)
	}

	pub(crate) fn membership(&self, graph: GraphId, global: GlobalAtomId) -> Option<AtomId> {
		self.graph(graph)?.local_id(global)
	}

	pub(crate) fn memberships(&self, global: GlobalAtomId) -> Option<&BTreeMap<GraphId, AtomId>> {
		self.atoms.memberships(global)
	}

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

	pub(crate) fn reset_property(
		&mut self,
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<()> {
		self.replace_occurrence(graph, local, |occurrence| occurrence.reset_overlay(key))
	}

	pub(crate) fn set_occurrence_label(
		&mut self,
		graph: GraphId,
		local: AtomId,
		label: Option<Text>,
	) -> FerrosGrynnResult<()> {
		self.replace_occurrence(graph, local, |occurrence| occurrence.set_label(label))
	}

	pub(crate) fn replace_occurrence_state(
		&mut self,
		graph: GraphId,
		local: AtomId,
		state: OccurrenceState,
	) -> FerrosGrynnResult<()> {
		self.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?
			.validate_state(local, &state)?;
		self.replace_occurrence(graph, local, |occurrence| occurrence.replace_state(state))
	}

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

	pub(crate) fn base_index_lookup(&self, index: IndexId, key: &[u8]) -> Vec<GlobalAtomId> {
		self.indexes.base_index(index).map_or_else(Vec::new, |index| index.entries.lookup(key))
	}

	pub(crate) fn effective_index_lookup(
		&self,
		graph: GraphId,
		index: IndexId,
		key: &[u8],
	) -> Vec<AtomId> {
		self.indexes
			.graph_index(graph, index)
			.map_or_else(Vec::new, |index| index.entries.lookup(key))
	}

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
		let mut context = index_context(&self.schema, &self.dictionary)?;
		let prepared =
			self.indexes.prepare_occurrence_replace(graph, &atom, &before, &after, &mut context)?;
		let mut statistics = self.statistics.as_ref().clone();
		statistics.replace_occurrence(graph, &atom, &before, &after)?;
		Arc::make_mut(&mut self.indexes).apply_replace(prepared)?;
		*self.occurrence_mut(graph, local)? = after;
		self.statistics = Arc::new(statistics);
		Ok(())
	}

	pub(crate) fn version(&self, key: ConflictKey) -> u64 {
		self.versions.get(&key).copied().unwrap_or(0)
	}

	pub(crate) fn mark_version(&mut self, key: ConflictKey, commit_ts: u64) {
		self.versions.insert(key, commit_ts);
	}

	pub(crate) fn advance_identity_clocks(&mut self, next_schema_id: u64, next_graph_id: u64) {
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.next_schema_id = next_schema_id;
		catalog.next_graph_id = next_graph_id;
	}

	pub(crate) fn publish_statistics_revision(&mut self, revision: u64) {
		Arc::make_mut(&mut self.statistics).publish_revision(revision);
	}

	fn graph_mut(&mut self, graph: GraphId) -> FerrosGrynnResult<&mut GraphRoot> {
		self.graphs
			.get_mut(&graph)
			.map(Arc::make_mut)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))
	}

	fn occurrence_mut(
		&mut self,
		graph: GraphId,
		local: AtomId,
	) -> FerrosGrynnResult<&mut GraphOccurrence> {
		self.graph_mut(graph)?
			.occurrence_mut(local)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("occurrence", local.to_string()))
	}

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

fn compatible_type(expected: &Type, actual: &Type) -> bool {
	expected == actual
		|| matches!(expected, Type::Any)
		|| matches!(expected, Type::Number)
			&& matches!(actual, Type::Number | Type::Int | Type::Float | Type::Decimal)
		|| matches!(expected, Type::Option(inner) if matches!(actual, Type::None | Type::Null) || compatible_type(inner, actual))
		|| matches!(expected, Type::Union(types) if types.iter().any(|ty| compatible_type(ty, actual)))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::DatabaseRoot;
	use crate::{
		datastore::{ClassId, ClassSchema},
		graph::{AtomKind, EdgeTopology, GraphId, GraphKind, GraphRef, GraphStorageMode},
		values::{Number, PropertyKeyId, PropertyMap, RecordId, RecordIdPart, Value},
	};

	fn class(id: u32, name: &str, kind: AtomKind) -> ClassSchema {
		let mut class = ClassSchema::new(name, kind, []).unwrap();
		class.id = ClassId::new(id);
		class
	}

	fn graph(id: u64, name: &str) -> GraphRef {
		GraphRef::new(GraphId::new(id).unwrap(), name, GraphKind::Graph, GraphStorageMode::InMemory)
			.unwrap()
	}

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
				RecordId::new("person", RecordIdPart::String("ada".into())).unwrap(),
				properties,
			)
			.unwrap();
		let first = graph(1, "first");
		let second = graph(2, "second");
		let first_id = first.id();
		let second_id = second.id();
		install_graph(&mut root, first, 10);
		install_graph(&mut root, second, 12);
		let first_local = root.attach_node(first_id, global, Some("A".into()), None).unwrap();
		let second_local = root.attach_node(second_id, global, Some("B".into()), None).unwrap();

		assert_eq!(
			root.effective_property(first_id, first_local, name).unwrap(),
			Some(&Value::from("base"))
		);
		assert!(!root.occurrence(first_id, first_local).unwrap().has_overlay());
		root.set_overlay(first_id, first_local, name, Value::from("private")).unwrap();
		root.set_base_property(global, name, Value::from("updated")).unwrap();
		assert_eq!(
			root.effective_property(first_id, first_local, name).unwrap(),
			Some(&Value::from("private"))
		);
		assert_eq!(
			root.effective_property(second_id, second_local, name).unwrap(),
			Some(&Value::from("updated"))
		);

		root.hide_property(first_id, first_local, name).unwrap();
		assert_eq!(root.effective_property(first_id, first_local, name).unwrap(), None);
		root.reset_property(first_id, first_local, name).unwrap();
		assert_eq!(
			root.effective_property(first_id, first_local, name).unwrap(),
			Some(&Value::from("updated"))
		);
		assert!(!root.occurrence(first_id, first_local).unwrap().has_overlay());
	}

	#[test]
	fn global_node_and_edge_use_distinct_atom_store_slots() {
		let mut root = DatabaseRoot::default();
		let node = class(1, "person", AtomKind::Node);
		let edge = class(2, "knows", AtomKind::Edge);
		let node_id = root
			.insert_atom(
				&node,
				RecordId::new("person", RecordIdPart::Int(1)).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();
		let edge_id = root
			.insert_atom(
				&edge,
				RecordId::new("knows", RecordIdPart::Int(1)).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();

		assert_ne!(node_id.slot(), edge_id.slot());
	}

	#[test]
	fn permits_unattached_property_records() {
		let mut root = DatabaseRoot::default();
		let property = class(1, "property", AtomKind::Node);
		let record = RecordId::new("property", RecordIdPart::Int(42)).unwrap();
		let global = root.insert_atom(&property, record.clone(), PropertyMap::new()).unwrap();

		assert_eq!(root.atom_by_record(&record).unwrap().id(), global);
		assert!(root.memberships(global).is_none());
	}

	#[test]
	fn one_global_edge_has_independent_graph_topology() {
		let mut root = DatabaseRoot::default();
		let person = class(1, "person", AtomKind::Node);
		let relation = class(2, "knows", AtomKind::Edge);
		let left = root
			.insert_atom(
				&person,
				RecordId::new("person", RecordIdPart::String("left".into())).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();
		let right = root
			.insert_atom(
				&person,
				RecordId::new("person", RecordIdPart::String("right".into())).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();
		let edge = root
			.insert_atom(
				&relation,
				RecordId::new("knows", RecordIdPart::String("edge".into())).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();
		let first = graph(1, "first");
		let second = graph(2, "second");
		let first_id = first.id();
		let second_id = second.id();
		install_graph(&mut root, first, 10);
		install_graph(&mut root, second, 12);
		let first_left = root.attach_node(first_id, left, None, None).unwrap();
		let first_right = root.attach_node(first_id, right, None, None).unwrap();
		let second_left = root.attach_node(second_id, left, None, None).unwrap();
		let second_right = root.attach_node(second_id, right, None, None).unwrap();
		let first_edge = root
			.attach_edge(
				first_id,
				edge,
				Some("friend".into()),
				EdgeTopology::directed([first_left], [first_right]),
				Some(Number::from(1_i64)),
				None,
			)
			.unwrap();
		let second_edge = root
			.attach_edge(
				second_id,
				edge,
				Some("blocked".into()),
				EdgeTopology::directed([second_right], [second_left]),
				Some(Number::from(2_i64)),
				None,
			)
			.unwrap();

		assert_ne!(
			root.occurrence(first_id, first_edge).unwrap().state(),
			root.occurrence(second_id, second_edge).unwrap().state()
		);
		assert_eq!(root.memberships(edge).unwrap().len(), 2);
	}

	fn install_graph(root: &mut DatabaseRoot, reference: GraphRef, first_index: u32) {
		use crate::datastore::{GraphSchema, IndexId, SchemaVersion, SystemIndex, SystemIndexKind};

		let schema = GraphSchema::new(
			SchemaVersion::INITIAL,
			reference,
			SystemIndex::new(IndexId::new(first_index).unwrap(), SystemIndexKind::Label),
			SystemIndex::new(IndexId::new(first_index + 1).unwrap(), SystemIndexKind::Adjacency),
			0,
		);
		root.install_graph(schema, 0).unwrap();
	}
}
