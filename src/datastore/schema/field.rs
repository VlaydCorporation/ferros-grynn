use crate::values::runtime::{PropertyKeyId, Text, Type};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableField {
	pub(crate) name: Text,
	pub(crate) value_type: Type,
	pub(crate) key: Option<PropertyKeyId>,
}

impl TableField {
	pub fn new<IntoText: Into<Text>>(name: IntoText, value_type: Type) -> Self {
		Self {
			name: name.into(),
			value_type,
			key: None,
		}
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn value_type(&self) -> &Type {
		&self.value_type
	}
}
