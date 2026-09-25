use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
	time::Instant,
};

use parking_lot::Mutex;
use rapidhash::RapidHashMap;

use super::{
	ClassId,
	ClassSchema,
	ConflictKey,
	DatabaseRoot,
	Datastore,
	GlobalAtomId,
	GraphSchema,
	ImportConstraintMode,
	ImportIndexMode,
	ImportViolationPolicy,
	IndexId,
	InvariantViolationReport,
	IsolationLevel,
	OccurrenceState,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
	TransactionAccess,
	TransactionOptions,
	consts::{
		BASE_ATOM_WRITE_BYTES,
		EFFECTIVE_FANOUT_WRITE_BYTES,
		PROPERTY_DICTIONARY_WRITE_BYTES,
		SCHEMA_WRITE_BYTES,
	},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult, formatting::DiagnosticValue},
	graph::{AtomId, AtomKind, EdgeTopology, GraphId, GraphKind, GraphRef, SnapshotId},
	values::{Number, PropertyKeyId, PropertyMap, RecordId, Text, Value},
};

mod commit;
mod graph;
mod graph_view;
mod import;
mod query;
mod schema;
mod writeback;

#[cfg(test)]
mod tests;

pub use graph_view::TransactionGraphView;
pub(crate) use query::{
	QuerySnapshot,
	QueryStorageCursor,
	QueryStorageRow,
	ScalarComparison,
	ScalarIndexConstraint,
};

/// Owned database-global atom value returned by a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtomSnapshot {
	atom: Arc<super::BaseAtom>,
}

impl AtomSnapshot {
	/// Returns the stable class-qualified identity.
	pub fn record_id(&self) -> &RecordId {
		self.atom.record_id()
	}

	/// Returns the immutable class identity.
	pub fn class_id(&self) -> ClassId {
		self.atom.class_id()
	}

	/// Returns whether the atom is a node or an edge.
	pub fn kind(&self) -> AtomKind {
		self.atom.kind()
	}

	/// Returns database-global base properties.
	pub fn properties(&self) -> &PropertyMap {
		self.atom.properties()
	}
}

/// Timestamp published by a successful transaction commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
	transaction: u64,
	commit_ts: u64,
}

impl CommitResult {
	/// Returns the process-local transaction identity.
	pub const fn transaction(self) -> u64 {
		self.transaction
	}

	/// Returns the database-wide commit timestamp.
	pub const fn commit_ts(self) -> u64 {
		self.commit_ts
	}
}

#[derive(Debug, Clone)]
enum Mutation {
	InternProperty {
		key: PropertyKeyId,
		name: Text,
	},
	DefineClass(ClassSchema),
	DefineGraph(GraphSchema),
	ReplaceClass(ClassSchema),
	RemoveClass(ClassId),
	RemoveGraph(GraphId),
	CreateAtom {
		id: GlobalAtomId,
		class_id: ClassId,
		record_id: RecordId,
		properties: PropertyMap,
	},
	SetBaseProperty {
		global: GlobalAtomId,
		key: PropertyKeyId,
		value: Value,
	},
	RemoveBaseProperty {
		global: GlobalAtomId,
		key: PropertyKeyId,
	},
	DeleteAtom {
		global: GlobalAtomId,
	},
	AttachNode {
		graph: GraphId,
		local: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		nested_graph: Option<GraphId>,
	},
	AttachEdge {
		graph: GraphId,
		local: AtomId,
		global: GlobalAtomId,
		label: Option<Text>,
		topology: EdgeTopology,
		weight: Option<Number>,
		transition_graph: Option<GraphId>,
	},
	Detach {
		graph: GraphId,
		global: GlobalAtomId,
	},
	SetOverlay {
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
		value: Value,
	},
	HideProperty {
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
	},
	ResetProperty {
		graph: GraphId,
		local: AtomId,
		key: PropertyKeyId,
	},
	SetOccurrenceLabel {
		graph: GraphId,
		local: AtomId,
		label: Option<Text>,
	},
	ReplaceOccurrenceState {
		graph: GraphId,
		local: AtomId,
		state: OccurrenceState,
	},
}

/// Transaction-private normalized root with FCW and conservative SSI
/// validation.
pub struct Transaction {
	store: Datastore,
	id: u64,
	started: Instant,
	options: TransactionOptions,
	snapshot: Arc<DatabaseRoot>,
	working: DatabaseRoot,
	reads: Mutex<RapidHashMap<ConflictKey, u64>>,
	missing_records: Mutex<BTreeSet<RecordId>>,
	// ponytail: one-key cache avoids the steady repeated-read lock; replace it
	// with a bounded set only if alternating point-read profiles justify it.
	last_base_read: Option<GlobalAtomId>,
	range_writes: BTreeSet<ConflictKey>,
	writes: BTreeMap<ConflictKey, u64>,
	requirements: BTreeMap<ConflictKey, u64>,
	fanout_reservations: BTreeSet<(GlobalAtomId, GraphId, PropertyKeyId)>,
	write_bytes: usize,
	mutations: Vec<Mutation>,
	import_constraints: Option<ImportConstraintMode>,
	import_graph_scope: Option<GraphId>,
	rollback_mutation_errors: bool,
	atomic_mutation_depth: usize,
	gql_statement_depth: usize,
	gql_aborted: bool,
	gql_bindings: crate::values::Object,
}

pub(crate) struct TransactionSavepoint {
	working: DatabaseRoot,
	reads: RapidHashMap<ConflictKey, u64>,
	missing_records: BTreeSet<RecordId>,
	last_base_read: Option<GlobalAtomId>,
	range_writes: BTreeSet<ConflictKey>,
	writes: BTreeMap<ConflictKey, u64>,
	requirements: BTreeMap<ConflictKey, u64>,
	fanout_reservations: BTreeSet<(GlobalAtomId, GraphId, PropertyKeyId)>,
	write_bytes: usize,
	mutation_len: usize,
	gql_bindings: crate::values::Object,
}

impl Transaction {
	pub(crate) fn new(
		store: Datastore,
		id: u64,
		options: TransactionOptions,
		snapshot: Arc<DatabaseRoot>,
	) -> Self {
		Self {
			store,
			id,
			started: Instant::now(),
			options,
			working: (*snapshot).clone(),
			snapshot,
			reads: Mutex::new(RapidHashMap::default()),
			missing_records: Mutex::new(BTreeSet::new()),
			last_base_read: None,
			range_writes: BTreeSet::new(),
			writes: BTreeMap::new(),
			requirements: BTreeMap::new(),
			fanout_reservations: BTreeSet::new(),
			write_bytes: 0,
			mutations: Vec::new(),
			import_constraints: None,
			import_graph_scope: None,
			rollback_mutation_errors: true,
			atomic_mutation_depth: 0,
			gql_statement_depth: 0,
			gql_aborted: false,
			gql_bindings: crate::values::Object::new(),
		}
	}

	pub(crate) fn new_import(
		store: Datastore,
		id: u64,
		options: TransactionOptions,
		snapshot: Arc<DatabaseRoot>,
		indexes: ImportIndexMode,
		constraints: ImportConstraintMode,
		on_violation: ImportViolationPolicy,
		graph_scope: Option<GraphId>,
	) -> Self {
		let mut transaction = Self::new(store, id, options, snapshot);
		if indexes == ImportIndexMode::Deferred {
			transaction
				.working
				.defer_explicit_indexes(constraints == ImportConstraintMode::Immediate);
		}
		transaction.import_constraints = Some(constraints);
		transaction.import_graph_scope = graph_scope;
		transaction.rollback_mutation_errors = on_violation == ImportViolationPolicy::Skip;
		transaction
	}

	/// Returns the process-local transaction identity.
	pub const fn id(&self) -> u64 {
		self.id
	}

	/// Returns the currently visible MVCC snapshot identity.
	pub fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::new(self.snapshot.commit_ts, self.id)
	}

	/// Returns the isolation and access policy selected at begin time.
	pub const fn options(&self) -> TransactionOptions {
		self.options
	}

	/// Returns bytes already reserved by the logical write-set preflight.
	pub const fn write_set_bytes(&self) -> usize {
		self.write_bytes
	}

	/// Parses, analyzes, and executes a GrynQL program in this transaction.
	///
	/// # Errors
	///
	/// Returns the first parser, analysis, execution, or datastore error.
	pub fn execute(
		&mut self,
		source: &str,
		variables: &crate::values::Object,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		self.execute_with_settings(source, variables, crate::gql::ParserSettings::default())
	}

	/// Executes a GrynQL program with explicit parser settings.
	///
	/// # Errors
	///
	/// Returns the first parser, analysis, execution, or datastore error.
	pub fn execute_with_settings(
		&mut self,
		source: &str,
		variables: &crate::values::Object,
		settings: crate::gql::ParserSettings,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		crate::gql::execute_transaction(self, source, variables, settings)
	}

	/// Executes GrynQL with explicit parser and bounded-runtime controls.
	///
	/// # Errors
	///
	/// Returns parse or structural failures. An uncaught GrynQL failure aborts
	/// this transaction; statement failures are retained in `QueryResult`.
	pub fn execute_with_options(
		&mut self,
		source: &str,
		variables: &crate::values::Object,
		options: crate::gql::QueryExecutionOptions,
	) -> FerrosGrynnResult<Vec<crate::gql::QueryResult>> {
		crate::gql::execute_transaction_with_options(self, source, variables, options)
	}

	pub(crate) fn run_gql_statement<T>(
		&mut self,
		execute: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		self.ensure_live()?;
		if self.gql_statement_depth != 0 {
			return execute(self);
		}
		self.refresh_read_committed()?;
		self.gql_statement_depth = 1;
		let result = execute(self);
		self.gql_statement_depth = 0;
		result
	}

	pub(crate) fn abort_gql(&mut self) {
		self.gql_aborted = true;
	}

	pub(crate) const fn gql_bindings(&self) -> &crate::values::Object {
		&self.gql_bindings
	}

	pub(crate) fn replace_gql_bindings(&mut self, bindings: crate::values::Object) {
		self.gql_bindings = bindings;
	}

	pub(crate) fn gql_savepoint(&self) -> TransactionSavepoint {
		TransactionSavepoint {
			working: self.working.clone(),
			reads: self.reads.lock().clone(),
			missing_records: self.missing_records.lock().clone(),
			last_base_read: self.last_base_read,
			range_writes: self.range_writes.clone(),
			writes: self.writes.clone(),
			requirements: self.requirements.clone(),
			fanout_reservations: self.fanout_reservations.clone(),
			write_bytes: self.write_bytes,
			mutation_len: self.mutations.len(),
			gql_bindings: self.gql_bindings.clone(),
		}
	}

	pub(crate) fn restore_gql_savepoint(&mut self, savepoint: TransactionSavepoint) {
		self.working = savepoint.working;
		*self.reads.lock() = savepoint.reads;
		*self.missing_records.lock() = savepoint.missing_records;
		self.last_base_read = savepoint.last_base_read;
		self.range_writes = savepoint.range_writes;
		self.writes = savepoint.writes;
		self.requirements = savepoint.requirements;
		self.fanout_reservations = savepoint.fanout_reservations;
		self.write_bytes = savepoint.write_bytes;
		self.mutations.truncate(savepoint.mutation_len);
		self.gql_bindings = savepoint.gql_bindings;
	}

	pub(crate) fn assert_invariants(
		&mut self,
		graph: Option<&GraphRef>,
		detail_limit: usize,
	) -> FerrosGrynnResult<usize> {
		let graphs = if let Some(graph) = graph {
			vec![graph.clone()]
		} else {
			self.working.schema.graphs().map(|graph| graph.reference().clone()).collect()
		};
		let mut report =
			self.working.collect_invariant_violations(graph.map(GraphRef::id), detail_limit);
		for graph in &graphs {
			if let Err(error) = self.validate_graph_family(graph) {
				report.push("graph_family", error);
			}
		}
		if !report.is_empty() {
			return Err(invariant_assertion_error(&report));
		}
		Ok(graphs.len())
	}

	/// Executes one statement-sized mutation with rollback to the transaction's
	/// prior private state when validation or execution fails.
	pub(crate) fn atomic_mutation<T>(
		&mut self,
		mutate: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		if self.atomic_mutation_depth != 0 {
			return mutate(self);
		}
		self.prepare_mutation()?;
		self.atomic_mutation_depth = 1;
		if !self.rollback_mutation_errors {
			let result = mutate(self);
			self.atomic_mutation_depth = 0;
			return result;
		}
		let savepoint = self.gql_savepoint();
		let result = mutate(self);
		self.atomic_mutation_depth = 0;
		match result {
			Ok(value) => Ok(value),
			Err(error) => {
				self.restore_gql_savepoint(savepoint);
				Err(error)
			}
		}
	}

	/// Publishes a new immutable class definition.
	///
	/// # Errors
	///
	/// Returns a schema, read-only, snapshot-age, identity-capacity, or
	/// write-set-limit error.
	pub fn define_class(&mut self, mut class: ClassSchema) -> FerrosGrynnResult<ClassId> {
		self.prepare_schema_mutation()?;
		if class.id.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"define_class requires an unpublished class",
			));
		}
		if self.working.schema.class(class.name().as_str()).is_some() {
			return Err(FerrosGrynnError::class_already_exists(class.name().to_string()));
		}
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;

		let id = ClassId::from_schema_id(self.store.inner.allocate_schema_id()?);
		class.id = Some(id);
		for field in &mut class.fields {
			if field.id.is_some() {
				return Err(FerrosGrynnError::invalid_argument(
					"define_class requires unpublished fields",
				));
			}
			field.id = Some(self.store.inner.allocate_schema_id()?);
			field.class_id = Some(id);
			field.key = Some(self.intern_property(field.name().as_str())?);
		}
		for index in &mut class.indexes {
			if index.id.is_some() {
				return Err(FerrosGrynnError::invalid_argument(
					"define_class requires unpublished indexes",
				));
			}
			index.id = Some(IndexId::from_schema_id(self.store.inner.allocate_schema_id()?));
		}
		self.working.install_class(class.clone(), self.snapshot.commit_ts)?;
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::DefineClass(class));
		Ok(id)
	}

	/// Publishes an empty named graph and its system indexes.
	///
	/// # Errors
	///
	/// Returns a graph-name, read-only, snapshot-age, identity-capacity, or
	/// write-set-limit error.
	pub fn define_graph<IntoText: Into<Text>>(
		&mut self,
		name: IntoText,
		kind: GraphKind,
	) -> FerrosGrynnResult<GraphRef> {
		self.prepare_schema_mutation()?;
		let name = name.into();
		if self.working.schema.named_graph(name.as_str()).is_some() {
			return Err(FerrosGrynnError::graph_already_exists(name.to_string()));
		}
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let graph = self.store.inner.allocate_graph_id()?;
		let reference = GraphRef::new(graph, name, kind, self.store.inner.graph_storage_mode())?;
		let label = SystemIndex::new(
			IndexId::from_schema_id(self.store.inner.allocate_schema_id()?),
			SystemIndexKind::Label,
		);
		let adjacency = SystemIndex::new(
			IndexId::from_schema_id(self.store.inner.allocate_schema_id()?),
			SystemIndexKind::Adjacency,
		);
		let schema = GraphSchema::new(
			SchemaVersion::INITIAL,
			reference.clone(),
			label,
			adjacency,
			self.snapshot.commit_ts,
		);
		self.working.install_graph(schema.clone(), self.snapshot.commit_ts)?;
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::DefineGraph(schema));
		Ok(reference)
	}

	/// Resolves or appends one database-global property name.
	pub(crate) fn intern_property(&mut self, name: &str) -> FerrosGrynnResult<PropertyKeyId> {
		self.prepare_mutation()?;
		if let Some(key) = self.working.dictionary.get(name) {
			return Ok(key);
		}
		let conflict = ConflictKey::PropertyDictionary;
		if !self.writes.contains_key(&conflict) {
			self.ensure_capacity(PROPERTY_DICTIONARY_WRITE_BYTES)?;
		}
		let name = Text::from(name);
		let key = Arc::make_mut(&mut self.working.dictionary).intern(name.clone())?;
		self.insert_write(conflict, PROPERTY_DICTIONARY_WRITE_BYTES);
		self.mutations.push(Mutation::InternProperty {
			key,
			name,
		});
		Ok(key)
	}

	/// Creates one database-global atom in the class named by its `RecordId`.
	///
	/// # Errors
	///
	/// Returns an error when the class is absent, the identity already exists,
	/// the transaction is read-only, or its resource limit is exceeded.
	pub fn create_atom(
		&mut self,
		record_id: RecordId,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomSnapshot> {
		self.atomic_mutation(move |transaction| {
			transaction.create_atom_inner(record_id, properties)
		})
	}

	fn create_atom_inner(
		&mut self,
		record_id: RecordId,
		properties: PropertyMap,
	) -> FerrosGrynnResult<AtomSnapshot> {
		let class = self
			.working
			.schema
			.class(record_id.class())
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(record_id.class()))?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		self.ensure_capacity(BASE_ATOM_WRITE_BYTES)?;
		let global = self.working.insert_atom(&class, record_id.clone(), properties.clone())?;
		let key = ConflictKey::BaseAtom(global);
		self.insert_write(key, BASE_ATOM_WRITE_BYTES);
		self.mark_range_change(ConflictKey::BaseClassRange(class_id));
		self.mutations.push(Mutation::CreateAtom {
			id: global,
			class_id,
			record_id,
			properties,
		});
		self.atom_snapshot(global)
	}

	/// Reads an atom's database-global identity and base properties.
	///
	/// # Errors
	///
	/// Returns a snapshot-age or read-committed refresh error.
	pub fn atom(&mut self, record_id: &RecordId) -> FerrosGrynnResult<Option<AtomSnapshot>> {
		self.prepare_read()?;
		let Some(global) = self.working.resolve_atom(record_id) else {
			self.record_missing(record_id);
			return Ok(None);
		};
		let atom = self.atom_snapshot(global)?;
		if self.last_base_read != Some(global) {
			self.record_read(ConflictKey::BaseAtom(global));
			self.last_base_read = Some(global);
		}
		Ok(Some(atom))
	}

	/// Replaces one database-global base property after a bounded fanout
	/// preflight over this atom's reverse memberships.
	///
	/// # Errors
	///
	/// Returns an atom, read-only, snapshot-age, or write-set-limit error.
	pub fn set_base_property(
		&mut self,
		record_id: &RecordId,
		key: PropertyKeyId,
		value: Value,
	) -> FerrosGrynnResult<Option<Value>> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let (class_id, _kind) = self.atom_domain(global)?;
		let write = ConflictKey::BaseAtom(global);
		let fanout = self.inherited_memberships(global, key);
		let new_fanout = fanout
			.iter()
			.filter(|graph| !self.fanout_reservations.contains(&(global, **graph, key)))
			.count();
		let additional = (if self.writes.contains_key(&write) {
			0
		} else {
			BASE_ATOM_WRITE_BYTES
		})
		.checked_add(
			new_fanout
				.checked_mul(EFFECTIVE_FANOUT_WRITE_BYTES)
				.ok_or_else(|| self.limit_error(usize::MAX))?,
		)
		.ok_or_else(|| self.limit_error(usize::MAX))?;
		self.ensure_capacity(additional)?;

		let previous = self.working.set_base_property(global, key, value.clone())?;
		self.insert_write(write, BASE_ATOM_WRITE_BYTES);
		self.mark_range_change(ConflictKey::BaseClassRange(class_id));
		for graph in self
			.working
			.memberships(global)
			.into_iter()
			.flat_map(|memberships| memberships.keys().copied())
			.collect::<Vec<_>>()
		{
			self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		}
		for graph in fanout {
			if self.fanout_reservations.insert((global, graph, key)) {
				self.write_bytes += EFFECTIVE_FANOUT_WRITE_BYTES;
			}
		}
		self.mutations.push(Mutation::SetBaseProperty {
			global,
			key,
			value,
		});
		Ok(previous)
	}

	/// Removes one database-global base property.
	///
	/// # Errors
	///
	/// Returns an atom, read-only, snapshot-age, uniqueness, or write-set-limit
	/// error.
	pub fn remove_base_property(
		&mut self,
		record_id: &RecordId,
		key: PropertyKeyId,
	) -> FerrosGrynnResult<Option<Value>> {
		self.prepare_mutation()?;
		let global = self.require_atom(record_id)?;
		let (class_id, _kind) = self.atom_domain(global)?;
		if self.working.atom(global).and_then(|atom| atom.properties().get(key)).is_none() {
			return Ok(None);
		}
		let write = ConflictKey::BaseAtom(global);
		let fanout = self.inherited_memberships(global, key);
		let new_fanout = fanout
			.iter()
			.filter(|graph| !self.fanout_reservations.contains(&(global, **graph, key)))
			.count();
		let additional = (if self.writes.contains_key(&write) {
			0
		} else {
			BASE_ATOM_WRITE_BYTES
		})
		.checked_add(
			new_fanout
				.checked_mul(EFFECTIVE_FANOUT_WRITE_BYTES)
				.ok_or_else(|| self.limit_error(usize::MAX))?,
		)
		.ok_or_else(|| self.limit_error(usize::MAX))?;
		self.ensure_capacity(additional)?;
		let previous = self.working.remove_base_property(global, key)?;
		self.insert_write(write, BASE_ATOM_WRITE_BYTES);
		self.mark_range_change(ConflictKey::BaseClassRange(class_id));
		for graph in self
			.working
			.memberships(global)
			.into_iter()
			.flat_map(|memberships| memberships.keys().copied())
			.collect::<Vec<_>>()
		{
			self.mark_range_change(ConflictKey::GraphClassRange(graph, class_id));
		}
		for graph in fanout {
			if self.fanout_reservations.insert((global, graph, key)) {
				self.write_bytes += EFFECTIVE_FANOUT_WRITE_BYTES;
			}
		}
		self.mutations.push(Mutation::RemoveBaseProperty {
			global,
			key,
		});
		Ok(previous)
	}

	/// Deletes a database-global atom without graph memberships.
	///
	/// # Errors
	///
	/// Returns an error when the atom is absent, still attached, read-only, or
	/// exceeds the write-set limit.
	pub fn delete_atom(&mut self, record_id: &RecordId) -> FerrosGrynnResult<()> {
		self.delete_atom_cascade(record_id, false)
	}

	/// Deletes one database-global atom and, when requested, every graph-local
	/// occurrence that depends on it.
	///
	/// # Errors
	///
	/// Returns an atom, membership, incidence, read-only, or write-set-limit
	/// error without retaining a partial transaction-private deletion.
	pub fn delete_atom_cascade(
		&mut self,
		record_id: &RecordId,
		cascade: bool,
	) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|transaction| transaction.delete_atom_inner(record_id, cascade))
	}

	fn delete_atom_inner(&mut self, record_id: &RecordId, cascade: bool) -> FerrosGrynnResult<()> {
		let global = self.require_atom(record_id)?;
		let (class_id, _kind) = self.atom_domain(global)?;
		let write = ConflictKey::BaseAtom(global);
		if !self.writes.contains_key(&write) {
			self.ensure_capacity(BASE_ATOM_WRITE_BYTES)?;
		}
		let memberships = self
			.working
			.memberships(global)
			.map(|memberships| memberships.keys().copied().collect::<Vec<_>>())
			.unwrap_or_else(Vec::new);
		if cascade
			&& let Some(scope) = self.import_graph_scope
			&& memberships.iter().any(|graph| *graph != scope)
		{
			return Err(FerrosGrynnError::graph_constraint_violation(
				"scoped import",
				"DELETE CASCADE would remove occurrences from another graph",
			));
		}
		if !cascade && !memberships.is_empty() {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				"DELETE requires CASCADE while the atom has graph memberships",
			));
		}
		for graph in memberships {
			self.detach_occurrence(graph, record_id, true)?;
		}
		self.working.remove_atom(global)?;
		self.insert_write(write, BASE_ATOM_WRITE_BYTES);
		self.mark_range_change(ConflictKey::BaseClassRange(class_id));
		self.mutations.push(Mutation::DeleteAtom {
			global,
		});
		Ok(())
	}

	fn prepare_read(&mut self) -> FerrosGrynnResult<()> {
		self.ensure_live()?;
		if self.gql_statement_depth == 0 {
			self.refresh_read_committed()?;
		}
		Ok(())
	}

	fn prepare_mutation(&mut self) -> FerrosGrynnResult<()> {
		self.ensure_live()?;
		self.store.inner.ensure_write_allowed(self.id)?;
		if self.options.access == TransactionAccess::ReadOnly {
			return Err(FerrosGrynnError::read_only_transaction(self.id));
		}
		if self.gql_statement_depth == 0 {
			self.refresh_read_committed()?;
		}
		Ok(())
	}

	fn prepare_schema_mutation(&mut self) -> FerrosGrynnResult<()> {
		if self.import_constraints.is_some() {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"schema mutation inside BEGIN IMPORT",
			));
		}
		self.prepare_mutation()
	}

	fn ensure_live(&self) -> FerrosGrynnResult<()> {
		if self.gql_aborted {
			return Err(FerrosGrynnError::transaction_aborted(self.id));
		}
		if self.store.inner.import_owner.load(std::sync::atomic::Ordering::Acquire) == self.id {
			return Ok(());
		}
		let limits = self.store.inner.transaction_limits();
		if self.started.elapsed() > limits.max_snapshot_age {
			return Err(FerrosGrynnError::snapshot_expired(
				self.id,
				limits.max_snapshot_age.as_secs(),
			));
		}
		Ok(())
	}

	fn refresh_read_committed(&mut self) -> FerrosGrynnResult<()> {
		if self.options.isolation != IsolationLevel::ReadCommitted {
			return Ok(());
		}
		let current = self.store.inner.current_root();
		if current.commit_ts == self.snapshot.commit_ts {
			return Ok(());
		}
		self.validate_write_requirements(&current)?;
		let mut working = (*current).clone();
		Self::apply_mutations(&mut working, &self.mutations, current.commit_ts).map_err(
			|error| {
				FerrosGrynnError::serialization_failure(
					self.id,
					"a read-committed statement could not rebase its prior writes",
				)
				.with_cause(error)
			},
		)?;
		self.snapshot = current;
		self.working = working;
		self.reads.lock().clear();
		self.missing_records.lock().clear();
		self.last_base_read = None;
		Ok(())
	}

	fn require_atom(&self, record_id: &RecordId) -> FerrosGrynnResult<GlobalAtomId> {
		self.working
			.resolve_atom(record_id)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", record_id.to_string()))
	}

	fn atom_snapshot(&self, global: GlobalAtomId) -> FerrosGrynnResult<AtomSnapshot> {
		let atom = self
			.working
			.atom_arc(global)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		Ok(AtomSnapshot {
			atom,
		})
	}

	fn atom_domain(&self, global: GlobalAtomId) -> FerrosGrynnResult<(ClassId, AtomKind)> {
		let atom = self
			.working
			.atom(global)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?;
		Ok((atom.class_id(), atom.kind()))
	}

	fn inherited_memberships(&self, global: GlobalAtomId, key: PropertyKeyId) -> Vec<GraphId> {
		self.working
			.memberships(global)
			.into_iter()
			.flat_map(|memberships| memberships.keys().copied())
			.filter(|graph| self.working.inherits_property(*graph, global, key))
			.collect()
	}

	fn record_read(&self, key: ConflictKey) {
		self.record_reads([key]);
	}

	fn record_reads<const COUNT: usize>(&self, keys: [ConflictKey; COUNT]) {
		let mut reads = self.reads.lock();
		for key in keys {
			reads.entry(key).or_insert_with(|| self.snapshot.version(key));
		}
	}

	fn record_missing(&self, record: &RecordId) {
		self.missing_records.lock().insert(record.clone());
	}

	fn mark_range_change(&mut self, key: ConflictKey) {
		self.range_writes.insert(key);
	}

	fn record_requirement(&mut self, key: ConflictKey) {
		self.requirements.entry(key).or_insert_with(|| self.snapshot.version(key));
	}

	fn insert_write(&mut self, key: ConflictKey, cost: usize) {
		if self.writes.contains_key(&key) {
			return;
		}
		self.writes.insert(key, self.snapshot.version(key));
		self.write_bytes += cost;
	}

	fn ensure_capacity(&self, additional: usize) -> FerrosGrynnResult<()> {
		let actual = self.write_bytes.checked_add(additional).unwrap_or(usize::MAX);
		if actual > self.store.inner.transaction_limits().max_write_set_bytes {
			return Err(self.limit_error(actual));
		}
		Ok(())
	}

	fn limit_error(&self, actual: usize) -> FerrosGrynnError {
		let limit = self.store.inner.transaction_limits().max_write_set_bytes;
		FerrosGrynnError::transaction_write_set_limit_exceeded(
			self.id,
			u64::try_from(actual).unwrap_or(u64::MAX),
			u64::try_from(limit).unwrap_or(u64::MAX),
		)
	}
}

fn invariant_assertion_error(report: &InvariantViolationReport) -> FerrosGrynnError {
	let totals = report
		.totals()
		.iter()
		.map(|(category, total)| {
			(
				(*category).to_owned(),
				DiagnosticValue::Unsigned(u64::try_from(*total).unwrap_or(u64::MAX)),
			)
		})
		.collect();
	let details = report
		.details()
		.iter()
		.map(|(category, message)| {
			DiagnosticValue::Map(BTreeMap::from([
				("category".to_owned(), DiagnosticValue::String((*category).to_owned())),
				("message".to_owned(), DiagnosticValue::String(message.clone())),
			]))
		})
		.collect();
	let mut error =
		FerrosGrynnError::invariant_assertion_failed(format!("{} violation(s)", report.total()))
			.with_extension(
				"violation_total",
				DiagnosticValue::Unsigned(u64::try_from(report.total()).unwrap_or(u64::MAX)),
			)
			.with_extension("violation_totals", DiagnosticValue::Map(totals))
			.with_extension("violation_details", DiagnosticValue::List(details));
	if let Some(cause) = report.first_error() {
		error = error.with_cause(cause.clone());
	}
	error
}
