use std::{
	collections::BTreeMap,
	fmt::{Display, Formatter},
	num::NonZeroU32,
	sync::Arc,
};

use super::{TableKind, TableSchema};
use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
		RecordId,
		Text,
		Value,
		graph::{AtomId, AtomKind, GraphRef},
	},
};

/// Stable identity of one schema-catalog object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaId(NonZeroU32);

impl SchemaId {
	/// Creates an identity from its durable representation.
	///
	/// Zero is reserved by the in-memory membership table and [`u32::MAX`] is
	/// the on-disk hot-slot sentinel for no table.
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

/// Stable identity of one index definition.
///
/// Index identities share the schema-catalog allocation clock with graphs,
/// tables, fields, and parameters, but this type prevents an index identity
/// from being confused with another catalog object's identity.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

	/// Re-types an identity already allocated by the shared schema clock.
	pub(crate) const fn from_schema_id(value: SchemaId) -> Self {
		Self(value)
	}

	/// Decodes and validates the durable integer representation.
	///
	/// # Errors
	///
	/// Returns an invalid-storage error for a reserved schema identity.
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
	fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.get(), f)
	}
}

/// Monotonic semantic version of one schema-catalog object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// Engine-owned index capabilities visible through the schema catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemIndexKind {
	/// Stable record identity for one table.
	Primary,
	/// Enumeration of graph atoms by label.
	Label,
	/// Role-separated graph adjacency stored with atom pages.
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
	id: SchemaId,
	version: SchemaVersion,
	name: Text,
	reference: GraphRef,
	system_indexes: [SystemIndex; 2],
	pub(crate) published_at: u64,
}

impl GraphSchema {
	pub(crate) const fn new(
		id: SchemaId,
		version: SchemaVersion,
		name: Text,
		reference: GraphRef,
		label: SystemIndex,
		adjacency: SystemIndex,
		published_at: u64,
	) -> Self {
		Self {
			id,
			version,
			name,
			reference,
			system_indexes: [label, adjacency],
			published_at,
		}
	}

	/// Returns the stable catalog identity.
	pub const fn id(&self) -> SchemaId {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the case-sensitive catalog name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the graph reference owned by this datastore.
	pub const fn reference(&self) -> &GraphRef {
		&self.reference
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

#[derive(Clone, Copy)]
struct MembershipSlot {
	generation: u32,
	table_id: u32,
}

impl Default for MembershipSlot {
	fn default() -> Self {
		Self {
			generation: u32::MAX,
			table_id: 0,
		}
	}
}

/// Generation-aware table membership for one graph address space.
#[derive(Clone, Default)]
pub(crate) struct AtomTableMemberships {
	nodes: CowVec<MembershipSlot>,
	edges: CowVec<MembershipSlot>,
}

impl AtomTableMemberships {
	pub(crate) fn get(&self, atom: AtomId) -> Option<SchemaId> {
		let slot = usize::try_from(atom.slot()).ok()?;
		let entry = match atom.kind()? {
			AtomKind::Node => self.nodes.get(slot),
			AtomKind::Edge => self.edges.get(slot),
		}?;
		(entry.generation == atom.generation()).then(|| SchemaId::new(entry.table_id)).flatten()
	}

	pub(crate) fn set(&mut self, atom: AtomId, table: Option<SchemaId>) -> FerrosGrynnResult<()> {
		let slot = usize::try_from(atom.slot())
			.map_err(|_error| FerrosGrynnError::graph_capacity_exceeded("table membership slot"))?;
		let required = slot
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("table membership slot"))?;
		let slots = match atom.kind() {
			Some(AtomKind::Node) => &mut self.nodes,
			Some(AtomKind::Edge) => &mut self.edges,
			None => {
				return Err(FerrosGrynnError::invalid_argument(
					"table membership requires a valid atom kind",
				));
			}
		};
		if slots.len() < required {
			slots.resize_with(required, MembershipSlot::default);
		}
		slots[slot] = MembershipSlot {
			generation: atom.generation(),
			table_id: table.map_or(0, SchemaId::get),
		};
		Ok(())
	}

	pub(crate) fn assigned(&self) -> impl Iterator<Item = (AtomId, SchemaId)> + '_ {
		let nodes = self.nodes.iter().enumerate().filter_map(|(slot, entry)| {
			let slot = u64::try_from(slot).ok()?;
			let table = SchemaId::new(entry.table_id)?;
			AtomId::from_parts(AtomKind::Node, slot, entry.generation)
				.ok()
				.map(|atom| (atom, table))
		});
		let edges = self.edges.iter().enumerate().filter_map(|(slot, entry)| {
			let slot = u64::try_from(slot).ok()?;
			let table = SchemaId::new(entry.table_id)?;
			AtomId::from_parts(AtomKind::Edge, slot, entry.generation)
				.ok()
				.map(|atom| (atom, table))
		});
		nodes.chain(edges)
	}
}

/// Immutable MVCC snapshot of all currently visible schema definitions.
#[derive(Clone)]
pub struct SchemaCatalog {
	pub(crate) next_id: u64,
	pub(crate) graphs: BTreeMap<RecordId, Arc<GraphSchema>>,
	pub(crate) graph_names: BTreeMap<String, RecordId>,
	pub(crate) graph_versions: BTreeMap<RecordId, u64>,
	pub(crate) tables: BTreeMap<String, Arc<TableSchema>>,
	pub(crate) table_versions: BTreeMap<String, u64>,
	pub(crate) parameters: BTreeMap<String, Arc<SchemaParameter>>,
	pub(crate) parameter_versions: BTreeMap<String, u64>,
	pub(crate) memberships: BTreeMap<RecordId, AtomTableMemberships>,
}

impl Default for SchemaCatalog {
	fn default() -> Self {
		Self {
			next_id: 1,
			graphs: BTreeMap::new(),
			graph_names: BTreeMap::new(),
			graph_versions: BTreeMap::new(),
			tables: BTreeMap::new(),
			table_versions: BTreeMap::new(),
			parameters: BTreeMap::new(),
			parameter_versions: BTreeMap::new(),
			memberships: BTreeMap::new(),
		}
	}
}

impl SchemaCatalog {
	/// Resolves a named graph definition by its stable record identity.
	pub fn graph(&self, id: &RecordId) -> Option<Arc<GraphSchema>> {
		self.graphs.get(id).cloned()
	}

	/// Resolves a named graph definition by its case-sensitive catalog name.
	pub fn named_graph(&self, name: &str) -> Option<Arc<GraphSchema>> {
		self.graph_names.get(name).and_then(|id| self.graph(id))
	}

	/// Resolves one table definition by name.
	pub fn table(&self, name: &str) -> Option<Arc<TableSchema>> {
		self.tables.get(name).cloned()
	}

	/// Resolves one table definition by its stable catalog identity.
	pub fn table_by_id(&self, id: SchemaId) -> Option<Arc<TableSchema>> {
		self.tables.values().find(|table| table.id() == Some(id)).cloned()
	}

	/// Resolves one global parameter definition by name.
	pub fn parameter(&self, name: &str) -> Option<Arc<SchemaParameter>> {
		self.parameters.get(name).cloned()
	}

	/// Iterates named graph definitions in stable record-id order.
	pub fn graphs(&self) -> impl ExactSizeIterator<Item = &Arc<GraphSchema>> {
		self.graphs.values()
	}

	/// Iterates table definitions in name order.
	pub fn tables(&self) -> impl ExactSizeIterator<Item = &Arc<TableSchema>> {
		self.tables.values()
	}

	/// Iterates global parameter definitions in name order.
	pub fn parameters(&self) -> impl ExactSizeIterator<Item = &Arc<SchemaParameter>> {
		self.parameters.values()
	}

	pub(crate) fn membership(&self, graph: &RecordId, atom: AtomId) -> Option<SchemaId> {
		self.memberships.get(graph).and_then(|memberships| memberships.get(atom))
	}

	pub(crate) fn memberships(&self, graph: &RecordId) -> AtomTableMemberships {
		self.memberships.get(graph).cloned().unwrap_or_default()
	}

	pub(crate) fn set_membership(
		&mut self,
		graph: &RecordId,
		atom: AtomId,
		table: Option<SchemaId>,
	) -> FerrosGrynnResult<()> {
		self.memberships.entry(graph.clone()).or_default().set(atom, table)
	}

	pub(crate) fn clear_table_memberships(&mut self, table: SchemaId) -> FerrosGrynnResult<()> {
		for memberships in self.memberships.values_mut() {
			let atoms = memberships
				.assigned()
				.filter_map(|(atom, assigned)| (assigned == table).then_some(atom))
				.collect::<Vec<_>>();
			for atom in atoms {
				memberships.set(atom, None)?;
			}
		}
		Ok(())
	}

	pub(crate) fn validate_memberships(&self) -> FerrosGrynnResult<()> {
		for (graph, memberships) in &self.memberships {
			for (atom, table_id) in memberships.assigned() {
				let table = self.table_by_id(table_id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"table membership references an absent schema definition",
					)
				})?;
				if table.graph.id() != graph {
					return Err(FerrosGrynnError::invalid_storage_file(
						"table membership references a table from another graph",
					));
				}
				let kind_matches = matches!(
					(table.kind, atom.kind()),
					(TableKind::Any, Some(_))
						| (TableKind::Normal, Some(AtomKind::Node))
						| (TableKind::Relation, Some(AtomKind::Edge))
				);
				if !kind_matches {
					return Err(FerrosGrynnError::invalid_storage_file(
						"table membership atom kind disagrees with its schema",
					));
				}
			}
		}
		Ok(())
	}
}
