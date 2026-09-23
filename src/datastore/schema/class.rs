use std::collections::BTreeSet;

use super::{ClassField, ClassId, ClassIndex, ClassIndexKind, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		runtime::{AtomKind, Text},
		storage::validate_index_type,
	},
};

/// Validation policy applied to global atoms of one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassSchemaMode {
	/// Atoms may contain properties not declared as fields.
	Schemaless,
	/// Atoms may contain only properties declared as fields.
	Schemafull,
}

/// Versioned schema and index definitions for one `RecordId` namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassSchema {
	pub(crate) id: Option<ClassId>,
	pub(crate) version: SchemaVersion,
	pub(crate) name: Text,
	pub(crate) kind: AtomKind,
	pub(crate) mode: ClassSchemaMode,
	pub(crate) fields: Vec<ClassField>,
	pub(crate) indexes: Vec<ClassIndex>,
	pub(crate) published_at: u64,
}

impl ClassSchema {
	/// Creates an unpublished class definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the class or field names violate
	/// their identity contract.
	pub fn new<IntoText, Fields>(
		name: IntoText,
		kind: AtomKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator<Item = ClassField>,
	{
		let name = name.into();
		if name.is_empty() || name.as_str().contains('\0') {
			return Err(FerrosGrynnError::invalid_argument(
				"class name must be non-empty and must not contain NUL",
			));
		}
		let fields = fields.into_iter().collect::<Vec<_>>();
		let mut names = BTreeSet::new();
		if fields.iter().any(|field| {
			field.name.is_empty()
				|| field.name.as_str().contains('\0')
				|| field.name.as_str() == "id"
				|| !names.insert(field.name.clone())
		}) {
			return Err(FerrosGrynnError::invalid_argument(
				"class fields must have unique non-empty names and must not redefine implicit 'id'",
			));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name,
			kind,
			mode: ClassSchemaMode::Schemaless,
			fields,
			indexes: Vec::new(),
			published_at: 0,
		})
	}

	/// Returns the stable identity after publication.
	pub const fn id(&self) -> Option<ClassId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the immutable `RecordId` namespace.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the immutable node or edge kind.
	pub const fn kind(&self) -> AtomKind {
		self.kind
	}

	/// Returns the property-schema validation policy.
	pub const fn schema_mode(&self) -> ClassSchemaMode {
		self.mode
	}

	/// Sets the property-schema validation policy.
	#[must_use]
	pub fn with_schema_mode(mut self, mode: ClassSchemaMode) -> Self {
		self.mode = mode;
		self
	}

	/// Adds one field definition after validating its class-local name.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error for an empty, reserved, or duplicate
	/// field name.
	pub fn with_field(mut self, field: ClassField) -> FerrosGrynnResult<Self> {
		if field.name.is_empty()
			|| field.name.as_str().contains('\0')
			|| field.name.as_str() == "id"
			|| self.fields.iter().any(|existing| existing.name == field.name)
		{
			return Err(FerrosGrynnError::invalid_argument(
				"class fields require unique non-empty names and must not redefine implicit 'id'",
			));
		}
		self.fields.push(field);
		Ok(self)
	}

	/// Adds one validated class-index definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is duplicated, a scalar
	/// field is absent, or its value type has no stable ordering.
	pub fn with_index(mut self, index: ClassIndex) -> FerrosGrynnResult<Self> {
		if self.indexes.iter().any(|existing| existing.name == index.name) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"class '{}' contains duplicate index '{}'",
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
		if index.kind == ClassIndexKind::Count
			&& self.indexes.iter().any(|existing| {
				existing.kind == ClassIndexKind::Count && existing.scope == index.scope
			}) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"class '{}' already contains its Count index in this scope",
				self.name
			)));
		}
		self.indexes.push(index);
		Ok(self)
	}

	/// Iterates fields in declaration order.
	pub fn fields(&self) -> impl ExactSizeIterator<Item = &ClassField> {
		self.fields.iter()
	}

	/// Iterates indexes in declaration order.
	pub fn indexes(&self) -> impl ExactSizeIterator<Item = &ClassIndex> {
		self.indexes.iter()
	}
}

#[cfg(test)]
mod tests {
	use super::{ClassSchema, ClassSchemaMode};
	use crate::{
		datastore::{ClassField, ClassIndex, ClassIndexKind, IndexScope},
		values::{AtomKind, Type},
	};

	#[test]
	fn class_requires_kind_and_explicit_index_scope() {
		let index =
			ClassIndex::new("by_email", ClassIndexKind::Unique, IndexScope::Base, ["email"])
				.unwrap();
		let class =
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("email", Type::String)])
				.unwrap()
				.with_schema_mode(ClassSchemaMode::Schemafull)
				.with_index(index)
				.unwrap();

		assert_eq!(class.name(), "person");
		assert_eq!(class.kind(), AtomKind::Node);
		assert_eq!(class.schema_mode(), ClassSchemaMode::Schemafull);
		assert_eq!(class.indexes().next().unwrap().scope(), IndexScope::Base);
	}
}
