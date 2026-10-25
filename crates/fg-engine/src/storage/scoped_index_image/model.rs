use std::{collections::BTreeMap, time::Duration};

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
		/// Class associated with this scoped index kind.
		class: ClassId,
		/// Kind associated with this scoped index kind.
		kind: AtomKind,
	},
	/// Engine-owned graph-local label membership array.
	Label {
		/// Kind associated with this scoped index kind.
		kind: AtomKind,
		/// Label id associated with this scoped index kind.
		label_id: u32,
	},
	/// Explicit user-declared index.
	Explicit {
		/// Index associated with this scoped index kind.
		index: IndexId,
		/// Version associated with this scoped index kind.
		version: SchemaVersion,
		/// Class associated with this scoped index kind.
		class: ClassId,
		/// Kind associated with this scoped index kind.
		kind: ClassIndexKind,
	},
}

/// One immutable index side-file before catalog publication.
pub(in crate::storage) struct ScopedIndexFile {
	/// Scope associated with this scoped index file.
	pub(in crate::storage) scope: IndexScope,
	/// Relative path associated with this scoped index file.
	pub(in crate::storage) relative_path: String,
	/// Kind associated with this scoped index file.
	pub(in crate::storage) kind: ScopedIndexKind,
	/// Root associated with this scoped index file.
	pub(in crate::storage) root: PageNo,
	/// Pages associated with this scoped index file.
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

/// Metadata for one immutable side-file written directly by the import
/// streaming builder.
pub(in crate::storage) struct StreamedScopedIndexFile {
	/// Scope associated with this streamed scoped index file.
	pub(in crate::storage) scope: IndexScope,
	/// Path associated with this streamed scoped index file.
	pub(in crate::storage) path: String,
	/// Kind associated with this streamed scoped index file.
	pub(in crate::storage) kind: ScopedIndexKind,
	/// Root associated with this streamed scoped index file.
	pub(in crate::storage) root: PageNo,
	/// Page count associated with this streamed scoped index file.
	pub(in crate::storage) page_count: u64,
}

/// Every index generation built from one immutable database root.
#[derive(Default)]
pub(in crate::storage) struct ScopedIndexImages {
	/// Database-space Base indexes.
	pub(in crate::storage) base: Vec<ScopedIndexFile>,
	/// Graph-space class, label, and Effective indexes.
	pub(in crate::storage) graphs: BTreeMap<GraphId, Vec<ScopedIndexFile>>,
}

/// Every directly written immutable index file of one import generation.
#[derive(Default)]
pub(in crate::storage) struct StreamedScopedIndexImages {
	/// Base associated with this streamed scoped index images.
	pub(in crate::storage) base: Vec<StreamedScopedIndexFile>,
	/// Graphs associated with this streamed scoped index images.
	pub(in crate::storage) graphs: BTreeMap<GraphId, Vec<StreamedScopedIndexFile>>,
	/// Index timings associated with this streamed scoped index images.
	pub(in crate::storage) index_timings: BTreeMap<IndexId, Duration>,
}
