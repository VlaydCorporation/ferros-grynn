use std::collections::BTreeMap;

use crate::{
	datastore::{ClassId, ClassIndexKind, IndexId, IndexScope, SchemaVersion},
	graph::{AtomKind, GraphId},
	storage::page::{AlignedPage, PageNo},
};

/// Logical identity carried beside one physical index generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) enum ScopedIndexKind {
	/// Engine-owned graph-local class membership array.
	Class {
		class: ClassId,
		kind: AtomKind,
	},
	/// Engine-owned graph-local label membership array.
	Label {
		kind: AtomKind,
		label_id: u32,
	},
	/// Explicit user-declared index.
	Explicit {
		index: IndexId,
		version: SchemaVersion,
		class: ClassId,
		kind: ClassIndexKind,
	},
}

/// One immutable index side-file before catalog publication.
pub(in crate::storage) struct ScopedIndexFile {
	pub(in crate::storage) scope: IndexScope,
	pub(in crate::storage) relative_path: String,
	pub(in crate::storage) kind: ScopedIndexKind,
	pub(in crate::storage) root: PageNo,
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

/// Every index generation built from one immutable database root.
#[derive(Default)]
pub(in crate::storage) struct ScopedIndexImages {
	/// Database-space Base indexes.
	pub(in crate::storage) base: Vec<ScopedIndexFile>,
	/// Graph-space class, label, and Effective indexes.
	pub(in crate::storage) graphs: BTreeMap<GraphId, Vec<ScopedIndexFile>>,
}
