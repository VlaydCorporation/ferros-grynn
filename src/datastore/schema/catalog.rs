use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
	num::NonZeroU32,
	sync::Arc,
};

use super::ClassSchema;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{GraphId, GraphRef, PropertyMap, Text, Value},
};

/// Stable identity of one schema-catalog object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SchemaId(NonZeroU32);

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

	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("schema identity is reserved"))
	}
}

impl Display for SchemaId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), formatter)
	}
}

/// Stable identity of one atom class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ClassId(SchemaId);

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

	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("class identity is reserved"))
	}
}

impl From<ClassId> for SchemaId {
	fn from(value: ClassId) -> Self {
		value.0
	}
}

impl Display for ClassId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), formatter)
	}
}

/// Stable identity of one index definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IndexId(SchemaId);

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

	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("index identity is reserved"))
	}
}

impl From<IndexId> for SchemaId {
	fn from(value: IndexId) -> Self {
		value.0
	}
}

impl Display for IndexId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), formatter)
	}
}

/// Monotonic semantic version of one schema-catalog object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SchemaVersion(NonZeroU32);

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

	pub(crate) fn next(self) -> FerrosGrynnResult<Self> {
		self.get()
			.checked_add(1)
			.and_then(Self::new)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("schema version is exhausted"))
	}

	pub(crate) fn from_storage(value: u32) -> FerrosGrynnResult<Self> {
		Self::new(value).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("schema version zero is reserved")
		})
	}
}

impl Display for SchemaVersion {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), formatter)
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
}

/// One engine-owned index definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemIndex {
	id: IndexId,
	version: SchemaVersion,
	kind: SystemIndexKind,
}

impl SystemIndex {
	pub(crate) const fn new(id: IndexId, kind: SystemIndexKind) -> Self {
		Self {
			id,
			version: SchemaVersion::INITIAL,
			kind,
		}
	}

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
	version: SchemaVersion,
	reference: GraphRef,
	system_indexes: [SystemIndex; 2],
	properties: PropertyMap,
	pub(crate) published_at: u64,
}

impl GraphSchema {
	pub(crate) fn new(
		version: SchemaVersion,
		reference: GraphRef,
		label: SystemIndex,
		adjacency: SystemIndex,
		properties: PropertyMap,
		published_at: u64,
	) -> Self {
		Self {
			version,
			reference,
			system_indexes: [label, adjacency],
			properties,
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

	pub(crate) fn replace_properties(&self, properties: PropertyMap) -> FerrosGrynnResult<Self> {
		let mut replacement = self.clone();
		replacement.version = replacement.version.next()?;
		replacement.properties = properties;
		Ok(replacement)
	}

	/// Returns the label and adjacency capabilities owned by the engine.
	pub fn system_indexes(&self) -> impl ExactSizeIterator<Item = &SystemIndex> {
		self.system_indexes.iter()
	}
}

/// Durable global database parameter definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaParameter {
	pub(crate) id: Option<SchemaId>,
	pub(crate) version: SchemaVersion,
	pub(crate) name: Text,
	pub(crate) value: Value,
	pub(crate) published_at: u64,
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
			published_at: 0,
		})
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
}

/// Immutable MVCC snapshot of currently visible schema definitions.
#[derive(Clone)]
pub struct SchemaCatalog {
	pub(crate) next_schema_id: u64,
	pub(crate) next_graph_id: u64,
	pub(crate) graphs: BTreeMap<GraphId, Arc<GraphSchema>>,
	pub(crate) graph_names: BTreeMap<String, GraphId>,
	pub(crate) graph_versions: BTreeMap<GraphId, u64>,
	pub(crate) classes: BTreeMap<ClassId, Arc<ClassSchema>>,
	pub(crate) class_names: BTreeMap<String, ClassId>,
	pub(crate) class_versions: BTreeMap<ClassId, u64>,
	pub(crate) parameters: BTreeMap<String, Arc<SchemaParameter>>,
	pub(crate) parameter_versions: BTreeMap<String, u64>,
}

impl Default for SchemaCatalog {
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
		}
	}
}

impl SchemaCatalog {
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

	/// Iterates graph definitions in stable identity order.
	pub fn graphs(&self) -> impl ExactSizeIterator<Item = &Arc<GraphSchema>> {
		self.graphs.values()
	}

	/// Iterates class definitions in stable identity order.
	pub fn classes(&self) -> impl ExactSizeIterator<Item = &Arc<ClassSchema>> {
		self.classes.values()
	}

	/// Iterates global parameter definitions in name order.
	pub fn parameters(&self) -> impl ExactSizeIterator<Item = &Arc<SchemaParameter>> {
		self.parameters.values()
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::{ClassId, SchemaCatalog, SchemaId};
	use crate::{
		datastore::ClassSchema,
		values::runtime::{AtomKind, Text},
	};

	#[test]
	fn class_identity_is_not_an_untyped_schema_identity() {
		let schema_id = SchemaId::new(7).unwrap();
		let class_id = ClassId::from_schema_id(schema_id);
		let mut class = ClassSchema::new("person", AtomKind::Node, []).unwrap();
		class.id = Some(class_id);
		let mut catalog = SchemaCatalog::default();
		catalog.classes.insert(class_id, Arc::new(class));
		catalog.class_names.insert(Text::from("person").to_string(), class_id);

		assert_eq!(catalog.class("person").unwrap().id(), Some(class_id));
		assert_eq!(SchemaId::from(class_id), schema_id);
	}
}
