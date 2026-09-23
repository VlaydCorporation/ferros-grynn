use std::collections::BTreeSet;

use super::{IndexId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{GraphId, Text},
};

/// Logical scope of one user-declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum IndexScope {
	/// Indexes base values of every global atom in the class.
	Base,
	/// Indexes effective values of memberships in one graph.
	Effective {
		/// Graph whose inherited and overridden values are indexed.
		graph: GraphId,
	},
}

/// Physical contract for one user-declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassIndexKind {
	/// One key may reference multiple atoms.
	Standard,
	/// One key may reference at most one atom.
	Unique,
	/// Exact number of live members; no fields participate in the key.
	Count,
}

/// Transaction-visible usability state of one declared class index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassIndexState {
	/// The physical index is available to query planning.
	Ready,
	/// The definition is retained but queries must fall back to a scan.
	Stale,
	/// The index is deliberately excluded before a later `REMOVE INDEX`.
	PreparedForRemoval,
}

/// Declarative scalar B+tree attached to one class schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassIndex {
	/// Stable identity assigned when the containing class is published.
	pub(crate) id: Option<IndexId>,
	/// Semantic definition version.
	pub(crate) version: SchemaVersion,
	/// Class-local index name.
	pub(crate) name: Text,
	/// Physical index contract.
	pub(crate) kind: ClassIndexKind,
	/// Observable planner-availability contract.
	pub(crate) state: ClassIndexState,
	/// Explicit base or graph-effective scope.
	pub(crate) scope: IndexScope,
	/// Indexed fields in composite-key order.
	pub(crate) fields: Vec<Text>,
}

impl ClassIndex {
	/// Creates an index definition; field existence is checked by
	/// [`super::ClassSchema::with_index`].
	///
	/// # Errors
	///
	/// Returns an invalid-argument error when the name is empty, a Count index
	/// names fields, or a scalar index has no fields, names the synthetic `id`
	/// column, or repeats a field.
	pub fn new<IntoText, Fields>(
		name: IntoText,
		kind: ClassIndexKind,
		scope: IndexScope,
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
			ClassIndexKind::Count => !fields.is_empty(),
			ClassIndexKind::Standard | ClassIndexKind::Unique => fields.is_empty(),
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
			state: ClassIndexState::Ready,
			scope,
			fields,
		})
	}

	/// Returns the stable identity after the containing class is published.
	pub const fn id(&self) -> Option<IndexId> {
		self.id
	}

	/// Returns the semantic definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the class-local index name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the physical index contract.
	pub const fn kind(&self) -> ClassIndexKind {
		self.kind
	}

	/// Returns whether the index is usable, stale, or prepared for removal.
	pub const fn state(&self) -> ClassIndexState {
		self.state
	}

	/// Returns the explicit base or graph-effective scope.
	pub const fn scope(&self) -> IndexScope {
		self.scope
	}

	/// Returns fields in composite-key order.
	pub fn fields(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.fields.iter()
	}
}
