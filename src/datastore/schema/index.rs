use std::collections::BTreeSet;

use super::{SchemaId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::Text,
};

/// Scalar B+tree uniqueness contract for one table index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableIndexKind {
	/// One key may reference multiple atoms.
	Standard,
	/// One key may reference at most one atom.
	Unique,
}

/// Declarative scalar B+tree attached to one table schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableIndex {
	/// Stable identity assigned when the containing table is published.
	pub(crate) id: Option<SchemaId>,
	/// Semantic definition version.
	pub(crate) version: SchemaVersion,
	/// Table-local index name.
	pub(crate) name: Text,
	/// Whether duplicate keys are permitted.
	pub(crate) kind: TableIndexKind,
	/// Indexed fields in composite-key order.
	pub(crate) fields: Vec<Text>,
}

impl TableIndex {
	/// Creates an index definition; field existence is checked by
	/// [`super::TableSchema::with_index`].
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the index name or field list is
	/// empty, names the synthetic `id` column, or repeats a field.
	pub fn new<IntoText, Fields>(
		name: IntoText,
		kind: TableIndexKind,
		fields: Fields,
	) -> FerrosGrynnResult<Self>
	where
		IntoText: Into<Text>,
		Fields: IntoIterator,
		Fields::Item: Into<Text>,
	{
		let name = name.into();
		let fields = fields.into_iter().map(Into::into).collect::<Vec<_>>();
		let mut unique = BTreeSet::new();
		if name.is_empty()
			|| name.as_str().contains('\0')
			|| fields.is_empty()
			|| fields.iter().any(|field| {
				field.is_empty()
					|| field.as_str().contains('\0')
					|| field.as_str() == "id"
					|| !unique.insert(field.clone())
			}) {
			return Err(FerrosGrynnError::invalid_argument(
				"an index requires a non-empty name and unique explicit table fields",
			));
		}
		Ok(Self {
			id: None,
			version: SchemaVersion::INITIAL,
			name,
			kind,
			fields,
		})
	}

	/// Returns the stable identity after the containing table is published.
	pub const fn id(&self) -> Option<SchemaId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the table-local index name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the standard or unique key contract.
	pub const fn kind(&self) -> TableIndexKind {
		self.kind
	}

	/// Returns fields in composite-key order.
	pub fn fields(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.fields.iter()
	}
}
