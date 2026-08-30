use std::collections::BTreeSet;

use super::{TableField, TableIndex};
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
	pub(crate) name: Text,
	pub(crate) graph: GraphRef,
	pub(crate) kind: TableKind,
	pub(crate) fields: Vec<TableField>,
	/// Scalar indexes in declaration order.
	pub(crate) indexes: Vec<TableIndex>,
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
		let name = name.into();
		if name.as_str().is_empty() {
			return Err(FerrosGrynnError::invalid_argument("table name must not be empty"));
		}
		let fields = fields.into_iter().collect::<Vec<_>>();
		let mut names = BTreeSet::new();
		if fields.iter().any(|field| {
			field.name.as_str().is_empty()
				|| field.name.as_str() == "id"
				|| !names.insert(field.name.as_str().to_owned())
		}) {
			return Err(FerrosGrynnError::invalid_argument(
				"table fields must have unique non-empty names and must not redefine implicit 'id'",
			));
		}
		Ok(Self {
			name,
			graph,
			kind,
			fields,
			indexes: Vec::new(),
		})
	}

	/// Adds one validated scalar B+tree definition.
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is duplicated, a field
	/// is absent, or its value type has no stable format-v1 scalar order.
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
		self.indexes.push(index);
		Ok(self)
	}

	pub const fn name(&self) -> &Text {
		&self.name
	}

	pub const fn graph(&self) -> &GraphRef {
		&self.graph
	}

	pub const fn kind(&self) -> TableKind {
		self.kind
	}

	pub fn fields(&self) -> impl ExactSizeIterator<Item = &TableField> {
		self.fields.iter()
	}

	/// Returns scalar indexes in definition order.
	pub fn indexes(&self) -> impl ExactSizeIterator<Item = &TableIndex> {
		self.indexes.iter()
	}
}
