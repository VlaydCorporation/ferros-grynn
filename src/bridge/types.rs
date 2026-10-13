//! Checked identities and owned mapping metadata independent of adapters.

use std::num::NonZeroU32;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	values::{Object, RecordId, Text},
};

/// Compact durable database identity in one datastore's schema namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeDatabaseId(NonZeroU32);

impl BridgeDatabaseId {
	/// Creates a positive database identity; zero is reserved.
	pub const fn new(value: u32) -> Option<Self> {
		match NonZeroU32::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the stable positive namespace identity.
	pub const fn get(self) -> u32 {
		self.0.get()
	}
}

/// Durable reference qualified by the Ferros-Grynn datastore origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeDatabaseRef {
	/// Datastore origin qualifying this logical identity.
	pub(crate) datastore: uuid::Uuid,
	/// Stable logical registry identity.
	pub(crate) id: BridgeDatabaseId,
}

impl BridgeDatabaseRef {
	/// Returns the owning datastore identity.
	pub const fn datastore(self) -> uuid::Uuid {
		self.datastore
	}

	/// Returns the database's compact registry identity.
	pub const fn id(self) -> BridgeDatabaseId {
		self.id
	}
}

/// Non-reusable mapping identity, independent of transaction-local addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeMappingId(pub(crate) uuid::Uuid);

impl BridgeMappingId {
	/// Returns the stable mapping UUID.
	pub const fn get(self) -> uuid::Uuid {
		self.0
	}
}

/// Owned mapping reference; it never retains a transaction or physical page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeMappingRef {
	/// Datastore origin qualifying this logical identity.
	pub(crate) datastore: uuid::Uuid,
	/// Stable logical registry identity.
	pub(crate) id: BridgeMappingId,
}

impl BridgeMappingRef {
	/// Returns the owning datastore identity.
	pub const fn datastore(self) -> uuid::Uuid {
		self.datastore
	}

	/// Returns the non-reusable mapping identity.
	pub const fn id(self) -> BridgeMappingId {
		self.id
	}
}

impl From<BridgeMappingRef> for crate::values::Value {
	fn from(reference: BridgeMappingRef) -> Self {
		Self::Object(Object::from_iter([
			("datastore", Self::Uuid(reference.datastore.into())),
			("mapping", Self::Uuid(reference.id.0.into())),
		]))
	}
}
impl TryFrom<crate::values::Value> for BridgeMappingRef {
	type Error = FerrosGrynnError;

	fn try_from(value: crate::values::Value) -> FerrosGrynnResult<Self> {
		let crate::values::Value::Object(mut fields) = value else {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"mapping reference must be an Object",
			));
		};
		let Some(crate::values::Value::Uuid(datastore)) = fields.remove("datastore") else {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"mapping datastore identity is absent",
			));
		};
		let Some(crate::values::Value::Uuid(mapping)) = fields.remove("mapping") else {
			return Err(FerrosGrynnError::bridge_mapping_error("mapping identity is absent"));
		};
		if !fields.is_empty() || datastore.as_inner().is_nil() || mapping.as_inner().is_nil() {
			return Err(FerrosGrynnError::bridge_mapping_error("mapping reference is malformed"));
		}
		Ok(Self {
			datastore: datastore.into_inner(),
			id: BridgeMappingId(mapping.into_inner()),
		})
	}
}

/// Independent uniqueness domains for properties and graph-local relations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BridgeMappingRole {
	/// Properties attached to one global atom.
	Atom,
	/// Relation representing one graph-local occurrence.
	Relation,
}

/// Local logical subject; external records are never graph endpoints.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BridgeMappingSubject {
	/// Additional properties of one database-global atom.
	Atom(RecordId),
	/// Relation corresponding to one local edge occurrence.
	Relation {
		/// Stable occurrence owner graph.
		graph: GraphId,
		/// Database-global edge identity.
		edge: RecordId,
	},
}

impl BridgeMappingSubject {
	/// Returns the uniqueness role of this subject.
	pub const fn role(&self) -> BridgeMappingRole {
		match self {
			Self::Atom(_) => BridgeMappingRole::Atom,
			Self::Relation {
				..
			} => BridgeMappingRole::Relation,
		}
	}

	/// Returns the database-global atom identity.
	pub const fn record(&self) -> &RecordId {
		match self {
			Self::Atom(record) => record,
			Self::Relation {
				edge,
				..
			} => edge,
		}
	}
}

/// External identity qualified by its registered database, not a local atom.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeEntityRef {
	/// Registered external database identity.
	pub database: BridgeDatabaseRef,
	/// Adapter-native record namespace/key represented without driver types.
	pub record: RecordId,
}

/// Durable mapping lifecycle. Pending reserves both unique directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeMappingState {
	/// Both sides are reserved; the external outcome awaits confirmation.
	Pending,
	/// The correspondence or idempotent external operation is confirmed.
	Ready,
	/// The local subject was removed; the old reference remains diagnosable.
	Tombstoned,
}

/// Exact idempotent relation intent retained across process restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeMirrorRequest {
	/// Preallocated external relation identity.
	pub relation: RecordId,
	/// External reference corresponding to the local incoming endpoint.
	pub source: RecordId,
	/// External reference corresponding to the local outgoing endpoint.
	pub target: RecordId,
	/// Typed relation properties, independent of query source or driver state.
	pub properties: Object,
}

/// One authoritative mapping record shared by both lookup indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeMapping {
	/// Owned identity independent of physical storage addresses.
	pub(crate) reference: BridgeMappingRef,
	/// Local global-atom or graph-occurrence identity.
	pub(crate) subject: BridgeMappingSubject,
	/// External database and native record identity.
	pub(crate) external: BridgeEntityRef,
	/// Snapshot-visible mapping lifecycle state.
	pub(crate) state: BridgeMappingState,
	/// Immutable recoverable intent; state transitions share its payload.
	pub(crate) mirror: Option<std::sync::Arc<BridgeMirrorRequest>>,
}

impl BridgeMapping {
	/// Returns the stable owned reference.
	pub const fn reference(&self) -> BridgeMappingRef {
		self.reference
	}

	/// Returns the local logical subject.
	pub const fn subject(&self) -> &BridgeMappingSubject {
		&self.subject
	}

	/// Returns the qualified external identity.
	pub const fn external(&self) -> &BridgeEntityRef {
		&self.external
	}

	/// Returns the snapshot-visible lifecycle state.
	pub const fn state(&self) -> BridgeMappingState {
		self.state
	}

	/// Returns the immutable operation required for explicit recovery.
	pub fn mirror(&self) -> Option<&BridgeMirrorRequest> {
		self.mirror.as_deref()
	}
}

/// Durable non-secret connection identity and explicit table correspondences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeDatabase {
	/// Owned identity independent of physical storage addresses.
	pub(crate) reference: BridgeDatabaseRef,
	/// Operation-level name resolving the registered runtime adapter.
	pub(crate) alias: Text,
	/// Non-secret adapter provider name.
	pub(crate) provider: Text,
	/// Configured native namespace, distinct from datastore identity.
	pub(crate) namespace: Text,
	/// Registered database descriptor or identity for this operation.
	pub(crate) database: Text,
	/// Injective local-class to external-properties-table correspondences.
	pub(crate) classes: std::collections::BTreeMap<Text, Text>,
	/// Injective local-graph to external-relation-table correspondences.
	pub(crate) graphs: std::collections::BTreeMap<GraphId, Text>,
}

/// Non-secret registration settings; clients are attached independently.
#[derive(Debug, Clone)]
pub struct BridgeDatabaseOptions {
	/// Operation-level name resolving the registered runtime adapter.
	pub(crate) alias: Text,
	/// Non-secret adapter provider name.
	pub(crate) provider: Text,
	/// Configured native namespace, distinct from datastore identity.
	pub(crate) namespace: Text,
	/// Registered database descriptor or identity for this operation.
	pub(crate) database: Text,
	/// Injective local-class to external-properties-table correspondences.
	pub(crate) classes: std::collections::BTreeMap<Text, Text>,
	/// Injective local-graph to external-relation-table correspondences.
	pub(crate) graphs: std::collections::BTreeMap<GraphId, Text>,
}

impl BridgeDatabaseOptions {
	/// Creates checked identity metadata without credentials or a connection.
	/// # Errors
	/// Rejects empty alias/provider and unbounded or NUL-containing names.
	pub fn new(
		alias: impl Into<Text>,
		provider: impl Into<Text>,
		namespace: impl Into<Text>,
		database: impl Into<Text>,
	) -> FerrosGrynnResult<Self> {
		let value = Self {
			alias: alias.into(),
			provider: provider.into(),
			namespace: namespace.into(),
			database: database.into(),
			classes: std::collections::BTreeMap::new(),
			graphs: std::collections::BTreeMap::new(),
		};
		for name in [&value.alias, &value.provider, &value.namespace, &value.database] {
			validate_name(name, true)?;
		}
		if value.alias.is_empty() || value.provider.is_empty() {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"bridge alias/provider must not be empty",
			));
		}
		Ok(value)
	}

	/// Associates a local atom class with one external properties table.
	/// # Errors
	/// Rejects invalid names, duplicate classes and non-injective table
	/// mappings.
	#[must_use = "registration settings are returned by value"]
	pub fn map_class(
		mut self,
		class: impl Into<Text>,
		table: impl Into<Text>,
	) -> FerrosGrynnResult<Self> {
		let class = class.into();
		let table = table.into();
		validate_name(&class, false)?;
		validate_name(&table, false)?;
		if self.classes.contains_key(&class) || self.classes.values().any(|value| value == &table) {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"class/table correspondence must be injective",
			));
		}
		self.classes.insert(class, table);
		Ok(self)
	}

	/// Associates a local graph with one external relation table.
	/// # Errors
	/// Rejects invalid table names and ambiguous graph/table correspondences.
	#[must_use = "registration settings are returned by value"]
	pub fn map_graph(mut self, graph: GraphId, table: impl Into<Text>) -> FerrosGrynnResult<Self> {
		let table = table.into();
		validate_name(&table, false)?;
		if self.graphs.contains_key(&graph) || self.graphs.values().any(|value| value == &table) {
			return Err(FerrosGrynnError::bridge_mapping_error(
				"graph/table correspondence must be injective",
			));
		}
		self.graphs.insert(graph, table);
		Ok(self)
	}
}

/// Rejects unbounded, empty or NUL-containing native names as required.
fn validate_name(name: &Text, allow_empty: bool) -> FerrosGrynnResult<()> {
	if name.len() > 4096 || name.as_str().contains('\0') || !allow_empty && name.is_empty() {
		return Err(FerrosGrynnError::bridge_mapping_error("invalid bridge identity name"));
	}
	Ok(())
}

/// Applies the same native record namespace rules as persistence recovery.
pub fn validate_record(record: &RecordId) -> FerrosGrynnResult<()> {
	validate_name(record.class_text(), false)
}

impl BridgeDatabase {
	/// Returns the qualified database registry reference.
	pub const fn reference(&self) -> BridgeDatabaseRef {
		self.reference
	}

	/// Returns the operation-level GQL alias.
	pub const fn alias(&self) -> &Text {
		&self.alias
	}

	/// Returns the adapter provider name.
	pub const fn provider(&self) -> &Text {
		&self.provider
	}

	/// Returns the configured native namespace.
	pub const fn namespace(&self) -> &Text {
		&self.namespace
	}

	/// Returns the configured native database name.
	pub const fn database(&self) -> &Text {
		&self.database
	}

	/// Resolves the external properties table for a local atom class.
	/// # Errors
	/// Returns an invalid-argument status when no correspondence was
	/// registered.
	pub fn class_table(&self, class: &str) -> FerrosGrynnResult<&Text> {
		self.classes
			.get(class)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("bridge class mapping is absent"))
	}

	/// Resolves the external relation table for one local graph.
	/// # Errors
	/// Returns an invalid-argument status when no correspondence was
	/// registered.
	pub fn graph_table(&self, graph: GraphId) -> FerrosGrynnResult<&Text> {
		self.graphs
			.get(&graph)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("bridge graph mapping is absent"))
	}
}
