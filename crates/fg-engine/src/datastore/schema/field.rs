use super::{ClassId, SchemaId, SchemaVersion};
use crate::values::runtime::{PropertyKeyId, Text, Type};

/// Versioned field declaration linking a class, value type and interned
/// property key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassField {
	/// Identifier used to locate this value within its owning subsystem.
	pub(crate) id: Option<SchemaId>,
	/// Version associated with this class field.
	pub(crate) version: SchemaVersion,
	/// Identifier of the schema class.
	pub(crate) class_id: Option<ClassId>,
	/// Name associated with this class field.
	pub(crate) name: Text,
	/// Value type associated with this class field.
	pub(crate) value_type: Type,
	/// Key associated with this class field.
	pub(crate) key: Option<PropertyKeyId>,
}

impl ClassField {
	/// Constructs `ClassField` from the supplied initial state.
	pub fn new<IntoText: Into<Text>>(name: IntoText, value_type: Type) -> Self {
		Self {
			id: None,
			version: SchemaVersion::INITIAL,
			class_id: None,
			name: name.into(),
			value_type,
			key: None,
		}
	}

	/// Returns the stable identity after the containing class is published.
	pub const fn id(&self) -> Option<SchemaId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the owning class after the definition has been published.
	pub const fn class_id(&self) -> Option<ClassId> {
		self.class_id
	}

	/// Borrows the stored name of this `ClassField`.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Borrows the stored value type of this `ClassField`.
	pub const fn value_type(&self) -> &Type {
		&self.value_type
	}
}
