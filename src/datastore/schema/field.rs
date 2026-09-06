use super::{SchemaId, SchemaVersion};
use crate::values::runtime::{PropertyKeyId, Text, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableField {
	pub(crate) id: Option<SchemaId>,
	pub(crate) version: SchemaVersion,
	pub(crate) name: Text,
	pub(crate) value_type: Type,
	pub(crate) key: Option<PropertyKeyId>,
}

impl TableField {
	pub fn new<IntoText: Into<Text>>(name: IntoText, value_type: Type) -> Self {
		Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name: name.into(),
			value_type,
			key: None,
		}
	}

	/// Returns the stable identity after the containing table is published.
	pub const fn id(&self) -> Option<SchemaId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn value_type(&self) -> &Type {
		&self.value_type
	}
}
