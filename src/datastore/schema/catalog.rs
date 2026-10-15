use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
	num::NonZeroU32,
	sync::Arc,
};

use super::{
	AnalyzerId,
	ClassSchema,
	EventDefinition,
	EventId,
	EventTarget,
	GraphIndex,
	SchemaAnalyzer,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphTopologyConstraints,
	values::runtime::{GraphId, GraphRef, PropertyMap, RecordId, Text, Value},
};

/// Stable identity of one schema-catalog object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SchemaId(
	/// Positive identity allocated in the datastore's shared schema namespace.
	NonZeroU32,
);

impl SchemaId {
	/// Creates an identity from its durable representation.
	pub const fn new(value: u32) -> Option<Self> {
		if value == u32::MAX {
			return None;
		}
		match NonZeroU32::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Creates from storage.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("schema identity is reserved"))
	}
}

impl Display for SchemaId {
	/// Formats `SchemaId` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Stable identity of one atom class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ClassId(
	/// Schema-namespace identity assigned to this class definition.
	SchemaId,
);

impl ClassId {
	/// Creates a class identity from its durable representation.
	pub const fn new(value: u32) -> Option<Self> {
		match SchemaId::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Creates from schema id.
	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	/// Creates from storage.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("class identity is reserved"))
	}
}

impl From<ClassId> for SchemaId {
	/// Converts `ClassId` into `SchemaId`.
	fn from(value: ClassId) -> Self {
		value.0
	}
}

impl Display for ClassId {
	/// Formats `ClassId` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Stable identity of one index definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IndexId(
	/// Schema-namespace identity assigned to this index definition.
	SchemaId,
);

impl IndexId {
	/// Creates an index identity from its durable representation.
	pub const fn new(value: u32) -> Option<Self> {
		match SchemaId::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Creates from schema id.
	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	/// Creates from storage.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("index identity is reserved"))
	}
}

impl From<IndexId> for SchemaId {
	/// Converts `IndexId` into `SchemaId`.
	fn from(value: IndexId) -> Self {
		value.0
	}
}

impl Display for IndexId {
	/// Formats `IndexId` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Stable identity of one named function overload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct FunctionId(
	/// Schema-namespace identity assigned to this stored function definition.
	SchemaId,
);

impl FunctionId {
	/// Creates a function identity from its durable representation.
	pub const fn new(value: u32) -> Option<Self> {
		match SchemaId::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Creates from schema id.
	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	/// Creates from storage.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("function identity is reserved"))
	}
}

impl From<FunctionId> for SchemaId {
	/// Converts `FunctionId` into `SchemaId`.
	fn from(value: FunctionId) -> Self {
		value.0
	}
}

impl Display for FunctionId {
	/// Formats `FunctionId` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Monotonic semantic version of one schema-catalog object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SchemaVersion(
	/// Positive version distinguishing successive definitions of one schema
	/// object.
	NonZeroU32,
);

impl SchemaVersion {
	/// Initial version assigned when a definition is first published.
	pub const INITIAL: Self = Self(NonZeroU32::MIN);

	/// Creates a version from its non-zero durable representation.
	pub const fn new(value: u32) -> Option<Self> {
		match NonZeroU32::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u32 {
		self.0.get()
	}

	/// Returns the next schema version.
	pub(crate) fn next(self) -> FerrosGrynnResult<Self> {
		self.get()
			.checked_add(1)
			.and_then(Self::new)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("schema version is exhausted"))
	}

	/// Creates from storage.
	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("schema version zero is reserved")
		})
	}
}

impl Display for SchemaVersion {
	/// Formats `SchemaVersion` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Engine-owned graph index capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemIndexKind {
	/// Enumeration of graph occurrences by their optional label.
	Label,
	/// Role-separated graph adjacency.
	Adjacency,
	/// Exact qualified edge-endpoint connector postings.
	EdgeEndpoint,
}

/// One engine-owned index definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemIndex {
	/// Identifier used to locate this value within its owning subsystem.
	id: IndexId,
	/// Version associated with this system index.
	version: SchemaVersion,
	/// Kind associated with this system index.
	kind: SystemIndexKind,
}

impl SystemIndex {
	/// Creates a new `SystemIndex` value.
	pub(crate) const fn new(id: IndexId, kind: SystemIndexKind) -> Self {
		Self {
			id,
			version: SchemaVersion::INITIAL,
			kind,
		}
	}

	/// Creates from storage.
	pub(crate) const fn from_storage(
		id: IndexId,
		version: SchemaVersion,
		kind: SystemIndexKind,
	) -> Self {
		Self {
			id,
			version,
			kind,
		}
	}

	/// Returns the stable catalog identity.
	pub const fn id(self) -> IndexId {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(self) -> SchemaVersion {
		self.version
	}

	/// Returns the engine-owned index capability.
	pub const fn kind(self) -> SystemIndexKind {
		self.kind
	}
}

/// Published schema definition of one named graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSchema {
	/// Version associated with this graph schema.
	version: SchemaVersion,
	/// Reference associated with this graph schema.
	reference: GraphRef,
	/// System indexes associated with this graph schema.
	system_indexes: Vec<SystemIndex>,
	/// User definitions in stable identity order.
	pub(crate) indexes: Vec<GraphIndex>,
	/// Properties associated with this graph schema.
	properties: PropertyMap,
	/// Topology constraints associated with this graph schema.
	topology_constraints: GraphTopologyConstraints,
	/// Root associated with this graph schema.
	root: Option<RecordId>,
	/// Owner root associated with this graph schema.
	owner_root: Option<GraphId>,
	/// Published at associated with this graph schema.
	pub(crate) published_at: u64,
}

impl GraphSchema {
	/// Creates a new `GraphSchema` value.
	pub(crate) fn new(
		version: SchemaVersion,
		reference: GraphRef,
		label: SystemIndex,
		adjacency: SystemIndex,
		properties: PropertyMap,
		topology_constraints: GraphTopologyConstraints,
		root: Option<RecordId>,
		owner_root: Option<GraphId>,
		published_at: u64,
	) -> Self {
		Self {
			version,
			reference,
			system_indexes: vec![label, adjacency],
			indexes: Vec::new(),
			properties,
			topology_constraints,
			root,
			owner_root,
			published_at,
		}
	}

	/// Returns the stable graph identity.
	pub const fn id(&self) -> GraphId {
		self.reference.id()
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the case-sensitive catalog name.
	pub const fn name(&self) -> &Text {
		self.reference.name()
	}

	/// Returns the graph reference owned by this datastore.
	pub const fn reference(&self) -> &GraphRef {
		&self.reference
	}

	/// Returns catalog-owned user properties in canonical key order.
	pub const fn properties(&self) -> &PropertyMap {
		&self.properties
	}

	/// Returns the durable topology validators attached to this graph.
	pub const fn topology_constraints(&self) -> GraphTopologyConstraints {
		self.topology_constraints
	}

	/// Returns the authoritative root of a rooted-tree graph.
	pub const fn root(&self) -> Option<&RecordId> {
		self.root.as_ref()
	}

	/// Returns the owner root result.
	pub(crate) const fn owner_root(&self) -> Option<GraphId> {
		self.owner_root
	}

	/// Returns whether this value is internal.
	pub(crate) const fn is_internal(&self) -> bool {
		self.owner_root.is_some()
	}

	/// Replaces properties.
	pub(crate) fn replace_properties(&self, properties: PropertyMap) -> FerrosGrynnResult<Self> {
		let mut replacement = self.clone();
		replacement.version = replacement.version.next()?;
		replacement.properties = properties;
		Ok(replacement)
	}

	/// Replaces definition.
	pub(crate) fn replace_definition(
		&self,
		properties: Option<PropertyMap>,
		topology_constraints: GraphTopologyConstraints,
		root: Option<RecordId>,
	) -> FerrosGrynnResult<Self> {
		let mut replacement = self.clone();
		replacement.version = replacement.version.next()?;
		if let Some(properties) = properties {
			replacement.properties = properties;
		}
		replacement.topology_constraints = topology_constraints;
		replacement.root = root;
		Ok(replacement)
	}

	/// Returns the label, adjacency, and optional endpoint engine capabilities.
	pub fn system_indexes(&self) -> impl ExactSizeIterator<Item = &SystemIndex> {
		self.system_indexes.iter()
	}

	/// Returns the synchronous endpoint capability after its first creation.
	pub fn edge_endpoint_index(&self) -> Option<SystemIndex> {
		self.system_indexes
			.iter()
			.copied()
			.find(|index| index.kind() == SystemIndexKind::EdgeEndpoint)
	}

	/// Installs a previously absent endpoint capability without changing core
	/// identities.
	pub(crate) fn with_edge_endpoint_index(
		mut self,
		index: SystemIndex,
	) -> FerrosGrynnResult<Self> {
		if index.kind() != SystemIndexKind::EdgeEndpoint || self.edge_endpoint_index().is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"edge endpoint capability is invalid or already present",
			));
		}
		self.system_indexes.push(index);
		Ok(self)
	}

	/// Returns graph-owned user index definitions in stable identity order.
	pub fn indexes(&self) -> impl ExactSizeIterator<Item = &GraphIndex> {
		self.indexes.iter()
	}

	/// Replaces graph-owned definitions without changing engine capabilities.
	pub(crate) fn replace_indexes(&self, indexes: Vec<GraphIndex>) -> FerrosGrynnResult<Self> {
		let mut replacement = self.clone();
		replacement.version = replacement.version.next()?;
		replacement.indexes = indexes;
		Ok(replacement)
	}
}

/// Durable global database parameter definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaParameter {
	/// Identifier used to locate this value within its owning subsystem.
	pub(crate) id: Option<SchemaId>,
	/// Version associated with this schema parameter.
	pub(crate) version: SchemaVersion,
	/// Name associated with this schema parameter.
	pub(crate) name: Text,
	/// Value associated with this schema parameter.
	pub(crate) value: Value,
	/// Comment associated with this schema parameter.
	pub(crate) comment: Option<Text>,
	/// Published at associated with this schema parameter.
	pub(crate) published_at: u64,
}

/// Declared side-effect class of one named function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum FunctionEffect {
	/// The function depends only on its arguments.
	Pure,
	/// The function may read the statement snapshot.
	Stable,
	/// The function may perform data mutations through a transaction savepoint.
	Impure,
}

/// One named parameter in a durable function signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaFunctionParameter {
	/// Name associated with this schema function parameter.
	pub(crate) name: Text,
	/// Value type associated with this schema function parameter.
	pub(crate) value_type: crate::values::Type,
}

impl SchemaFunctionParameter {
	/// Creates one parameter after validating its catalog name.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn new<IntoText: Into<Text>>(
		name: IntoText,
		value_type: crate::values::Type,
	) -> FerrosGrynnResult<Self> {
		let name = name.into();
		validate_catalog_name(name.as_str(), "function parameter")?;
		Ok(Self {
			name,
			value_type,
		})
	}

	/// Returns the source-level parameter name without a `$` prefix.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the resolved parameter type.
	pub const fn value_type(&self) -> &crate::values::Type {
		&self.value_type
	}
}

/// Immutable durable definition of one named function overload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaFunction {
	/// Identifier used to locate this value within its owning subsystem.
	pub(crate) id: Option<FunctionId>,
	/// Version associated with this schema function.
	pub(crate) version: SchemaVersion,
	/// Namespace associated with this schema function.
	pub(crate) namespace: Text,
	/// Name associated with this schema function.
	pub(crate) name: Text,
	/// Parameters associated with this schema function.
	pub(crate) parameters: Vec<SchemaFunctionParameter>,
	/// Result associated with this schema function.
	pub(crate) result: crate::values::Type,
	/// Effect associated with this schema function.
	pub(crate) effect: FunctionEffect,
	/// Cost associated with this schema function.
	pub(crate) cost: bool,
	/// Max depth associated with this schema function.
	pub(crate) max_depth: Option<u8>,
	/// Body associated with this schema function.
	pub(crate) body: Text,
	/// Comment associated with this schema function.
	pub(crate) comment: Option<Text>,
	/// Published at associated with this schema function.
	pub(crate) published_at: u64,
}

impl SchemaFunction {
	/// Creates an unpublished named function definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for an invalid name, duplicate
	/// parameter, empty body, invalid recursion bound, or non-pure cost
	/// function.
	#[allow(
		clippy::too_many_arguments,
		reason = "a durable function definition owns these fixed catalog fields"
	)]
	pub fn new<IntoNamespace: Into<Text>, IntoName: Into<Text>, IntoBody: Into<Text>>(
		namespace: IntoNamespace,
		name: IntoName,
		parameters: Vec<SchemaFunctionParameter>,
		result: crate::values::Type,
		effect: FunctionEffect,
		cost: bool,
		max_depth: Option<u8>,
		body: IntoBody,
	) -> FerrosGrynnResult<Self> {
		let namespace = namespace.into();
		let name = name.into();
		let body = body.into();
		validate_catalog_name(namespace.as_str(), "function namespace")?;
		validate_catalog_name(name.as_str(), "function name")?;
		if namespace.as_str() != "fn" {
			return Err(FerrosGrynnError::invalid_argument(
				"user-defined functions must use the 'fn' namespace",
			));
		}
		let mut names = std::collections::BTreeSet::new();
		if parameters.iter().any(|parameter| !names.insert(parameter.name.as_str())) {
			return Err(FerrosGrynnError::invalid_argument(
				"function parameter names must be unique",
			));
		}
		if body.is_empty() || body.as_str().contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"function body must be non-empty UTF-8 without NUL",
			));
		}
		if max_depth.is_some_and(|depth| !(1..=64).contains(&depth)) {
			return Err(FerrosGrynnError::invalid_argument("function MAXDEPTH must be in 1..=64"));
		}
		if cost && effect != FunctionEffect::Pure {
			return Err(FerrosGrynnError::invalid_argument("COST functions must be PURE"));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			namespace,
			name,
			parameters,
			result,
			effect,
			cost,
			max_depth,
			body,
			comment: None,
			published_at: 0,
		})
	}

	/// Attaches an optional durable catalog comment.
	#[must_use]
	pub fn with_comment<IntoText: Into<Text>>(mut self, comment: IntoText) -> Self {
		self.comment = Some(comment.into());
		self
	}

	/// Returns the stable identity after publication.
	pub const fn id(&self) -> Option<FunctionId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the namespace. User definitions always use `fn`.
	pub const fn namespace(&self) -> &Text {
		&self.namespace
	}

	/// Returns the unqualified function name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the ordered exact overload signature.
	pub fn parameters(&self) -> impl ExactSizeIterator<Item = &SchemaFunctionParameter> {
		self.parameters.iter()
	}

	/// Returns the resolved return type.
	pub const fn result(&self) -> &crate::values::Type {
		&self.result
	}

	/// Returns the declared and validated effect class.
	pub const fn effect(&self) -> FunctionEffect {
		self.effect
	}

	/// Returns whether the function is eligible as a custom cost function.
	pub const fn is_cost(&self) -> bool {
		self.cost
	}

	/// Returns the explicit recursion bound, if any.
	pub const fn max_depth(&self) -> Option<u8> {
		self.max_depth
	}

	/// Returns the persisted UTF-8 source body.
	pub const fn body(&self) -> &Text {
		&self.body
	}

	/// Returns the optional durable comment.
	pub const fn comment(&self) -> Option<&Text> {
		self.comment.as_ref()
	}

	/// Returns the signature result.
	pub(crate) fn signature(&self) -> (String, String, Vec<crate::values::Type>) {
		(
			self.namespace.to_string(),
			self.name.to_string(),
			self.parameters.iter().map(|parameter| parameter.value_type.clone()).collect(),
		)
	}
}

impl SchemaParameter {
	/// Creates an unpublished parameter definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for an empty name or a name containing
	/// NUL.
	pub fn new<IntoText: Into<Text>>(name: IntoText, value: Value) -> FerrosGrynnResult<Self> {
		let name = name.into();
		if name.is_empty() || name.as_str().contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"schema parameter name must be non-empty and must not contain NUL",
			));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name,
			value,
			comment: None,
			published_at: 0,
		})
	}

	/// Attaches an optional durable catalog comment.
	#[must_use]
	pub fn with_comment<IntoText: Into<Text>>(mut self, comment: IntoText) -> Self {
		self.comment = Some(comment.into());
		self
	}

	/// Returns the stable identity after the definition has been published.
	pub const fn id(&self) -> Option<SchemaId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the parameter name without the query-language `$` prefix.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the owned runtime value.
	pub const fn value(&self) -> &Value {
		&self.value
	}

	/// Returns the optional durable catalog comment.
	pub const fn comment(&self) -> Option<&Text> {
		self.comment.as_ref()
	}
}

/// Immutable MVCC snapshot of currently visible schema definitions.
#[derive(Clone)]
pub struct SchemaCatalog {
	/// Next schema id associated with this schema catalog.
	pub(crate) next_schema_id: u64,
	/// Next graph id associated with this schema catalog.
	pub(crate) next_graph_id: u64,
	/// Graphs associated with this schema catalog.
	pub(crate) graphs: BTreeMap<GraphId, Arc<GraphSchema>>,
	/// Graph names associated with this schema catalog.
	pub(crate) graph_names: BTreeMap<String, GraphId>,
	/// Graph versions associated with this schema catalog.
	pub(crate) graph_versions: BTreeMap<GraphId, u64>,
	/// Classes associated with this schema catalog.
	pub(crate) classes: BTreeMap<ClassId, Arc<ClassSchema>>,
	/// Class names associated with this schema catalog.
	pub(crate) class_names: BTreeMap<String, ClassId>,
	/// Class versions associated with this schema catalog.
	pub(crate) class_versions: BTreeMap<ClassId, u64>,
	/// Parameters associated with this schema catalog.
	pub(crate) parameters: BTreeMap<String, Arc<SchemaParameter>>,
	/// Parameter versions associated with this schema catalog.
	pub(crate) parameter_versions: BTreeMap<String, u64>,
	/// Functions associated with this schema catalog.
	pub(crate) functions: BTreeMap<FunctionId, Arc<SchemaFunction>>,
	/// Function signatures associated with this schema catalog.
	pub(crate) function_signatures:
		BTreeMap<(String, String, Vec<crate::values::Type>), FunctionId>,
	/// Function versions associated with this schema catalog.
	pub(crate) function_versions: BTreeMap<FunctionId, u64>,
	/// Analyzers associated with this schema catalog.
	pub(crate) analyzers: BTreeMap<AnalyzerId, Arc<SchemaAnalyzer>>,
	/// Analyzer names associated with this schema catalog.
	pub(crate) analyzer_names: BTreeMap<String, AnalyzerId>,
	/// Analyzer versions associated with this schema catalog.
	pub(crate) analyzer_versions: BTreeMap<AnalyzerId, u64>,
	/// Source-owned event definitions in stable dispatch order.
	pub(crate) events: BTreeMap<EventId, Arc<EventDefinition>>,
	/// Event names are unique within one stable source owner.
	pub(crate) event_names: BTreeMap<(EventTarget, Text), EventId>,
}

impl Default for SchemaCatalog {
	/// Constructs the default initial state of `SchemaCatalog`.
	fn default() -> Self {
		Self {
			next_schema_id: 1,
			next_graph_id: 1,
			graphs: BTreeMap::new(),
			graph_names: BTreeMap::new(),
			graph_versions: BTreeMap::new(),
			classes: BTreeMap::new(),
			class_names: BTreeMap::new(),
			class_versions: BTreeMap::new(),
			parameters: BTreeMap::new(),
			parameter_versions: BTreeMap::new(),
			functions: BTreeMap::new(),
			function_signatures: BTreeMap::new(),
			function_versions: BTreeMap::new(),
			analyzers: BTreeMap::new(),
			analyzer_names: BTreeMap::new(),
			analyzer_versions: BTreeMap::new(),
			events: BTreeMap::new(),
			event_names: BTreeMap::new(),
		}
	}
}

impl SchemaCatalog {
	/// Resolves one transaction-visible event by its stable identity.
	pub fn event(&self, id: EventId) -> Option<Arc<EventDefinition>> {
		self.events.get(&id).cloned()
	}

	/// Resolves an event name within its stable class or graph owner.
	pub fn named_event(&self, target: EventTarget, name: &str) -> Option<Arc<EventDefinition>> {
		self.events
			.values()
			.find(|event| event.target == target && event.name.as_str() == name)
			.cloned()
	}

	/// Iterates events in stable dispatch order without copying definitions.
	pub fn events(&self) -> impl ExactSizeIterator<Item = &Arc<EventDefinition>> {
		self.events.values()
	}

	/// Resolves a graph definition by its stable identity.
	pub fn graph(&self, id: GraphId) -> Option<Arc<GraphSchema>> {
		self.graphs.get(&id).cloned()
	}

	/// Resolves a graph definition by its case-sensitive catalog name.
	pub fn named_graph(&self, name: &str) -> Option<Arc<GraphSchema>> {
		self.graph_names.get(name).and_then(|id| self.graph(*id))
	}

	/// Resolves one class definition by its immutable `RecordId` namespace.
	pub fn class(&self, name: &str) -> Option<Arc<ClassSchema>> {
		self.class_names.get(name).and_then(|id| self.class_by_id(*id))
	}

	/// Resolves one class definition by its stable identity.
	pub fn class_by_id(&self, id: ClassId) -> Option<Arc<ClassSchema>> {
		self.classes.get(&id).cloned()
	}

	/// Resolves one global database parameter definition by name.
	pub fn parameter(&self, name: &str) -> Option<Arc<SchemaParameter>> {
		self.parameters.get(name).cloned()
	}

	/// Iterates public graph definitions in stable identity order.
	pub fn graphs(&self) -> impl Iterator<Item = &Arc<GraphSchema>> {
		self.graphs.values().filter(|graph| !graph.is_internal())
	}

	/// Returns the all graphs result.
	pub(crate) fn all_graphs(&self) -> impl ExactSizeIterator<Item = &Arc<GraphSchema>> {
		self.graphs.values()
	}

	/// Reports indexes whose immutable physical generations must be pinned at
	/// begin.
	pub(crate) fn has_snapshot_indexes(&self) -> bool {
		self.classes().any(|class| {
			class.indexes().any(|index| index.kind() == super::ClassIndexKind::Geometry)
		}) || self.all_graphs().any(|graph| graph.edge_endpoint_index().is_some())
	}

	/// Iterates class definitions in stable identity order.
	pub fn classes(&self) -> impl ExactSizeIterator<Item = &Arc<ClassSchema>> {
		self.classes.values()
	}

	/// Iterates global parameter definitions in name order.
	pub fn parameters(&self) -> impl ExactSizeIterator<Item = &Arc<SchemaParameter>> {
		self.parameters.values()
	}

	/// Resolves one exact named function overload.
	pub fn function(
		&self,
		namespace: &str,
		name: &str,
		parameters: &[crate::values::Type],
	) -> Option<Arc<SchemaFunction>> {
		self.function_signatures
			.get(&(namespace.to_owned(), name.to_owned(), parameters.to_vec()))
			.and_then(|id| self.functions.get(id))
			.cloned()
	}

	/// Iterates named function definitions in stable identity order.
	pub fn functions(&self) -> impl ExactSizeIterator<Item = &Arc<SchemaFunction>> {
		self.functions.values()
	}

	/// Resolves one durable analyzer by its case-sensitive catalog name.
	pub fn analyzer(&self, name: &str) -> Option<Arc<SchemaAnalyzer>> {
		self.analyzer_names.get(name).and_then(|id| self.analyzers.get(id)).cloned()
	}

	/// Resolves one durable analyzer by stable identity.
	pub fn analyzer_by_id(&self, id: AnalyzerId) -> Option<Arc<SchemaAnalyzer>> {
		self.analyzers.get(&id).cloned()
	}

	/// Iterates analyzer definitions in stable identity order.
	pub fn analyzers(&self) -> impl ExactSizeIterator<Item = &Arc<SchemaAnalyzer>> {
		self.analyzers.values()
	}
}

/// Validates catalog name.
pub(super) fn validate_catalog_name(name: &str, subject: &str) -> FerrosGrynnResult<()> {
	if name.is_empty() || name.contains('\0') {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"{subject} must be non-empty and must not contain NUL"
		)));
	}
	Ok(())
}

#[cfg(test)]
/// Regression tests for catalog behavior.
mod tests {
	use std::sync::Arc;

	use super::{ClassId, SchemaCatalog, SchemaId};
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		datastore::ClassSchema,
		values::runtime::{AtomKind, Text},
	};

	/// Verifies class identity is not an untyped schema identity.
	#[test]
	fn class_identity_is_not_an_untyped_schema_identity() {
		let schema_id = SchemaId::new(7).assert_some();
		let class_id = ClassId::from_schema_id(schema_id);
		let mut class = ClassSchema::new("person", AtomKind::Node, []).assert_ok();
		class.id = Some(class_id);
		let mut catalog = SchemaCatalog::default();
		catalog.classes.insert(class_id, Arc::new(class));
		catalog.class_names.insert(Text::from("person").to_string(), class_id);

		assert_eq!(catalog.class("person").assert_some().id(), Some(class_id));
		assert_eq!(SchemaId::from(class_id), schema_id);
	}
}
