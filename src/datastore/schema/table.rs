use std::collections::BTreeSet;

use super::{SchemaId, SchemaVersion, SystemIndex, TableField, TableIndex, TableIndexKind};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		runtime::{Text, graph::GraphRef},
		storage::validate_index_type,
	},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableKind {
	Any,
	Normal,
	Relation,
}

/// Validation policy applied to records owned by one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableSchemaMode {
	/// Records may contain properties not declared as fields.
	Schemaless,
	/// Records may contain only properties declared as fields.
	Schemafull,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
	pub(crate) id: Option<SchemaId>,
	pub(crate) version: SchemaVersion,
	pub(crate) name: Text,
	/// `None` places the table in the database system graph (`GraphId = 0`).
	pub(crate) graph: Option<GraphRef>,
	pub(crate) kind: TableKind,
	pub(crate) mode: TableSchemaMode,
	pub(crate) fields: Vec<TableField>,
	/// User-declared indexes in declaration order.
	pub(crate) indexes: Vec<TableIndex>,
	pub(crate) primary_index: Option<SystemIndex>,
	pub(crate) published_at: u64,
}

impl TableSchema {
	pub fn new<IntoText, Fields>(
		name: IntoText,
		graph: GraphRef,
		kind: TableKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator<Item = TableField>,
	{
		Self::new_with_graph(name.into(), Some(graph), kind, fields)
	}

	fn new_with_graph<Fields>(
		name: Text,
		graph: Option<GraphRef>,
		kind: TableKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		Fields: IntoIterator<Item = TableField>,
	{
		if name.as_str().is_empty() || name.as_str().contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"table name must be non-empty and must not contain NUL",
			));
		}
		let fields = fields.into_iter().collect::<Vec<_>>();
		let mut names = BTreeSet::new();
		if fields.iter().any(|field| {
			field.name.as_str().is_empty()
				|| field.name.as_str().contains('\0')
				|| field.name.as_str() == "id"
				|| !names.insert(field.name.as_str().to_owned())
		}) {
			return Err(FerrosGrynnError::invalid_argument(
				"table fields must have unique non-empty names and must not redefine implicit 'id'",
			));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name,
			graph,
			kind,
			mode: TableSchemaMode::Schemaless,
			fields,
			indexes: Vec::new(),
			primary_index: None,
			published_at: 0,
		})
	}

	/// Creates a table in the database system graph.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the table or field definitions
	/// violate the same contract as [`Self::new`].
	pub fn new_system<IntoText, Fields>(
		name: IntoText,
		kind: TableKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator<Item = TableField>,
	{
		Self::new_with_graph(name.into(), None, kind, fields)
	}

	/// Returns the stable identity after this definition is published.
	pub const fn id(&self) -> Option<SchemaId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Adds one validated table-index definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is duplicated, a scalar
	/// field is absent, or its value type has no stable format-v1 order.
	pub fn with_index(mut self, index: TableIndex) -> FerrosGrynnResult<Self> {
		if self.indexes.iter().any(|existing| existing.name == index.name) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"table '{}' contains duplicate index '{}'",
				self.name, index.name
			)));
		}
		for name in &index.fields {
			let field = self.fields.iter().find(|field| field.name == *name).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"index '{}' references unknown field '{}'",
					index.name, name
				))
			})?;
			validate_index_type(&field.value_type)?;
		}
		if index.kind == TableIndexKind::Count
			&& self.indexes.iter().any(|existing| existing.kind == TableIndexKind::Count)
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"table '{}' already contains its table-wide Count index",
				self.name
			)));
		}
		self.indexes.push(index);
		Ok(self)
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the owning named graph, or `None` for the database system graph.
	pub const fn graph(&self) -> Option<&GraphRef> {
		self.graph.as_ref()
	}

	pub const fn kind(&self) -> TableKind {
		self.kind
	}

	/// Returns the record-schema validation policy.
	pub const fn schema_mode(&self) -> TableSchemaMode {
		self.mode
	}

	/// Sets the record-schema validation policy.
	pub fn with_schema_mode(mut self, mode: TableSchemaMode) -> Self {
		self.mode = mode;
		self
	}

	pub fn fields(&self) -> impl ExactSizeIterator<Item = &TableField> {
		self.fields.iter()
	}

	/// Returns table indexes in definition order.
	pub fn indexes(&self) -> impl ExactSizeIterator<Item = &TableIndex> {
		self.indexes.iter()
	}

	/// Returns the engine-owned primary-index definition after publication.
	pub const fn primary_index(&self) -> Option<&SystemIndex> {
		self.primary_index.as_ref()
	}
}
