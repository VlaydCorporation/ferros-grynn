//! Lazy decoding of immutable scalar and count index side-files.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;

use super::{
	LocalStore,
	physical_traversal::{LocalQuerySnapshot, read_snapshot_pages},
};
use crate::{
	datastore::{ClassId, ClassIndexKind, IndexId, IndexScope},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomKind,
	storage::{
		catalog::{DatabaseCatalog, PhysicalIndexBinding},
		file_registry::{FileRegistry, RegisteredDataFile},
		graph_index::{CountIndexScope, ScalarIndexMode, decode_count_index, scan_scalar_index},
		page::{AlignedPage, PageNo},
	},
	values::storage::DiskAtomRef,
};

/// Generation-free address returned by one immutable physical index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PhysicalIndexAtom {
	pub(crate) kind: AtomKind,
	pub(crate) slot: u64,
}

/// One scalar key and its canonically ordered postings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalScalarGroup {
	pub(crate) key: Vec<u8>,
	pub(crate) atoms: Vec<PhysicalIndexAtom>,
}

/// Decoded immutable payload selected by the catalog index kind.
#[derive(Debug)]
pub(crate) enum PhysicalIndexEntries {
	Scalar(Vec<PhysicalScalarGroup>),
	Count(u64),
}

/// Exact logical metadata and lazily decoded payload of one durable index.
#[derive(Debug)]
pub(crate) struct PhysicalIndexRead {
	pub(crate) class_id: ClassId,
	pub(crate) kind: ClassIndexKind,
	pub(crate) scope: IndexScope,
	pub(crate) entries: PhysicalIndexEntries,
}

/// One current-generation file with a coalesced first decode.
pub(super) struct PhysicalIndexFile {
	class_id: ClassId,
	kind: ClassIndexKind,
	scope: IndexScope,
	binding: PhysicalIndexBinding,
	file: Arc<RegisteredDataFile>,
	decoded: Mutex<Option<Arc<PhysicalIndexRead>>>,
}

impl LocalStore {
	/// Returns whether an exact immutable side-file belongs to this snapshot.
	pub(crate) fn has_physical_index(&self, snapshot: &LocalQuerySnapshot, index: IndexId) -> bool {
		snapshot.0.indexes.contains_key(&index)
	}

	/// Decodes one immutable side-file at first demand and shares it for every
	/// later statement retaining the same physical generation.
	pub(crate) fn physical_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: IndexId,
	) -> FerrosGrynnResult<Option<Arc<PhysicalIndexRead>>> {
		let Some(file) = snapshot.0.indexes.get(&index) else {
			return Ok(None);
		};
		file.read(self, snapshot).map(Some)
	}
}

impl PhysicalIndexFile {
	fn read(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshot,
	) -> FerrosGrynnResult<Arc<PhysicalIndexRead>> {
		let mut decoded = self.decoded.lock();
		if let Some(decoded) = decoded.as_ref() {
			return Ok(Arc::clone(decoded));
		}
		let space = match self.scope {
			IndexScope::Base => crate::datastore::DataSpaceId::Database,
			IndexScope::Effective {
				graph,
			} => crate::datastore::DataSpaceId::Graph(graph),
		};
		let mut read_page = |page: PageNo, output: &mut AlignedPage| {
			let mut loaded = read_snapshot_pages(
				store,
				&snapshot.0,
				space,
				self.binding.file_id,
				&self.file,
				self.file.page_count(),
				&[page],
			)?;
			let page = loaded
				.pages
				.pop()
				.ok_or_else(|| invalid("physical index page read returned no page"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		let entries = match self.kind {
			ClassIndexKind::Standard | ClassIndexKind::Unique => {
				let mode = if self.kind == ClassIndexKind::Standard {
					ScalarIndexMode::Standard
				} else {
					ScalarIndexMode::Unique
				};
				let entries =
					scan_scalar_index::<DiskAtomRef>(&mut read_page, self.binding.root, mode)?
						.into_iter()
						.map(|entry| PhysicalScalarGroup {
							key: entry.key,
							atoms: entry
								.atoms
								.into_iter()
								.map(|atom| PhysicalIndexAtom {
									kind: atom.kind(),
									slot: atom.slot(),
								})
								.collect(),
						})
						.collect();
				PhysicalIndexEntries::Scalar(entries)
			}
			ClassIndexKind::Count => {
				if self.binding.root != PageNo::new(0) {
					return Err(invalid("Count index root is not page zero"));
				}
				let mut page = AlignedPage::zeroed();
				read_page(self.binding.root, &mut page)?;
				let value = decode_count_index(&page)?;
				let expected_scope = match self.scope {
					IndexScope::Base => CountIndexScope::Base,
					IndexScope::Effective {
						graph,
					} => CountIndexScope::Effective(graph),
				};
				if value.class_id != self.class_id.get() || value.scope != expected_scope {
					return Err(invalid("Count index metadata disagrees with the catalog"));
				}
				PhysicalIndexEntries::Count(value.atom_count)
			}
		};
		let value = Arc::new(PhysicalIndexRead {
			class_id: self.class_id,
			kind: self.kind,
			scope: self.scope,
			entries,
		});
		*decoded = Some(Arc::clone(&value));
		Ok(value)
	}
}

/// Binds catalog identities to lazy registered handles without opening the
/// underlying native files.
pub(super) fn bind_physical_indexes(
	catalog: &DatabaseCatalog,
	registry: &FileRegistry,
) -> FerrosGrynnResult<BTreeMap<IndexId, Arc<PhysicalIndexFile>>> {
	let mut files = BTreeMap::new();
	for class in &catalog.classes {
		for index in &class.indexes {
			let Some(binding) = index.physical else {
				continue;
			};
			let space = match index.scope {
				IndexScope::Base => crate::datastore::DataSpaceId::Database,
				IndexScope::Effective {
					graph,
				} => crate::datastore::DataSpaceId::Graph(graph),
			};
			let file = Arc::new(PhysicalIndexFile {
				class_id: class.id,
				kind: index.kind,
				scope: index.scope,
				binding,
				file: registry.resolve_file(space, binding.file_id)?,
				decoded: Mutex::new(None),
			});
			if files.insert(index.id, file).is_some() {
				return Err(invalid("duplicate physical IndexId in database catalog"));
			}
		}
	}
	Ok(files)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
