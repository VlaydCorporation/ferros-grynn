use super::{ClassId, SchemaId, SchemaVersion};
use crate::values::runtime::{PropertyKeyId, Text, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassField {
	pub(crate) id: Option<SchemaId>,
	pub(crate) version: SchemaVersion,
	pub(crate) class_id: Option<ClassId>,
	pub(crate) name: Text,
	pub(crate) value_type: Type,
	pub(crate) key: Option<PropertyKeyId>,
}

impl ClassField {
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

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn value_type(&self) -> &Type {
		&self.value_type
	}
}
