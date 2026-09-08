use std::collections::BTreeSet;

use super::{IndexId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::Text,
};

/// Physical contract for one user-declared table index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableIndexKind {
	/// One key may reference multiple atoms.
	Standard,
	/// One key may reference at most one atom.
	Unique,
	/// Exact number of live members; no fields participate in the key.
	Count,
}

/// Declarative scalar B+tree attached to one table schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableIndex {
	/// Stable identity assigned when the containing table is published.
	pub(crate) id: Option<IndexId>,
	/// Semantic definition version.
	pub(crate) version: SchemaVersion,
	/// Table-local index name.
	pub(crate) name: Text,
	/// Physical index contract.
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
	/// Returns an invalid-argument error when the name is empty, a Count index
	/// names fields, or a scalar index has no fields, names the synthetic `id`
	/// column, or repeats a field.
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
		let invalid_fields = match kind {
			TableIndexKind::Count => !fields.is_empty(),
			TableIndexKind::Standard | TableIndexKind::Unique => fields.is_empty(),
		};
		if name.is_empty()
			|| name.as_str().contains('\0')
			|| invalid_fields
			|| fields.iter().any(|field| {
				field.is_empty()
					|| field.as_str().contains('\0')
					|| field.as_str() == "id"
					|| !unique.insert(field.clone())
			}) {
			return Err(FerrosGrynnError::invalid_argument(
				"an index requires a non-empty name and fields matching its physical kind",
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
	pub const fn id(&self) -> Option<IndexId> {
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

	/// Returns the physical index contract.
	pub const fn kind(&self) -> TableIndexKind {
		self.kind
	}

	/// Returns fields in composite-key order.
	pub fn fields(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.fields.iter()
	}
}
