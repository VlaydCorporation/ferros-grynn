//! Canonical file names and physical identities for one full generation.

use std::collections::BTreeMap;

use super::GraphBinding;
use crate::{
	datastore::{DataSpaceId, IndexId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		atom_store_image::AtomStoreFileSet,
		catalog::{CatalogPath, FileCatalog, FileCatalogEntry, PhysicalIndexBinding},
		file_registry::DataFileRole,
		graph_store::ReverseMembershipFile,
		page::{AlignedPage, PageNo},
		types::{DataFileId, DataFileKind},
	},
};

/// Fixed root pages keep catalog discovery independent of a per-commit
/// database-superblock rewrite.
pub(super) const DATABASE_CATALOG_ROOT: PageNo = PageNo::new(0);
pub(super) const FILE_CATALOG_ROOT: PageNo = PageNo::new(1);
pub(super) const DATABASE_CATALOG_CONTINUATION: PageNo = PageNo::new(2);
pub(super) const FILE_CATALOG_CONTINUATION: PageNo = PageNo::new(1030);

/// Complete page image of one newly allocated physical file.
pub(super) struct FileImage {
	pub(super) entry: FileCatalogEntry,
	pub(super) page_numbers: Vec<PageNo>,
	pub(super) pages: Vec<AlignedPage>,
}

/// Exact roots required to reopen one full normalized generation.
pub(super) struct GenerationBindings {
	pub(super) atoms: AtomStoreFileSet,
	pub(super) reverse: ReverseMembershipFile,
	pub(super) graphs: Vec<GraphBinding>,
	pub(super) physical_indexes: BTreeMap<IndexId, PhysicalIndexBinding>,
}

/// Allocation helper shared by the AtomStore, graph, and index packers.
pub(super) struct LayoutBuilder {
	next_file_instance_id: u64,
	files: Vec<FileImage>,
}

impl LayoutBuilder {
	pub(super) const fn new(next_file_instance_id: u64) -> Self {
		Self {
			next_file_instance_id,
			files: Vec::new(),
		}
	}

	pub(super) fn add(
		&mut self,
		space_id: DataSpaceId,
		kind: DataFileKind,
		role: DataFileRole,
		path: String,
		companion_of: Option<DataFileId>,
		pages: Vec<AlignedPage>,
	) -> FerrosGrynnResult<DataFileId> {
		let page_numbers = (0..pages.len())
			.map(|index| {
				u64::try_from(index)
					.map(PageNo::new)
					.map_err(|_error| invalid("file page index exceeds u64"))
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let file_id = DataFileId::new(self.next_file_instance_id, kind)
			.ok_or_else(|| invalid("physical file identity clock is exhausted"))?;
		self.next_file_instance_id = self
			.next_file_instance_id
			.checked_add(1)
			.ok_or_else(|| invalid("physical file identity clock is exhausted"))?;
		self.files.push(FileImage {
			entry: FileCatalogEntry {
				space_id,
				file_id,
				companion_of,
				role,
				path: CatalogPath::parse(&path)?,
			},
			page_numbers,
			pages,
		});
		Ok(file_id)
	}

	pub(super) fn add_freelist(
		&mut self,
		space_id: DataSpaceId,
		path: String,
		target: DataFileId,
		page: AlignedPage,
	) -> FerrosGrynnResult<DataFileId> {
		self.add(
			space_id,
			DataFileKind::Freelist,
			DataFileRole::WalBackfilled,
			path,
			Some(target),
			vec![page],
		)
	}

	pub(super) fn finish(mut self) -> FerrosGrynnResult<(FileCatalog, Vec<FileImage>)> {
		let mut catalog = FileCatalog::bootstrap()?;
		catalog.next_file_instance_id = self.next_file_instance_id;
		catalog.files.extend(self.files.iter().map(|file| file.entry.clone()));
		catalog.files.sort_unstable_by_key(|file| (file.space_id, file.file_id));
		self.files.sort_unstable_by_key(|file| (file.entry.space_id, file.entry.file_id));
		catalog.encode()?;
		Ok((catalog, self.files))
	}
}

pub(super) fn atom_prefix(generation: &str) -> String {
	format!("atoms/{generation}")
}

pub(super) fn graph_prefix(graph: crate::graph::GraphId, generation: &str) -> String {
	format!("graphs/{:016x}/{generation}", graph.get())
}

pub(super) fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::storage::page::{PageFlags, PageLsn, PageType};

	/// Keeps over-aligned page storage separate from per-page numbers so moving
	/// a generated image cannot corrupt its first header byte.
	#[test]
	fn file_image_preserves_over_aligned_page_bytes() -> FerrosGrynnResult<()> {
		let page = AlignedPage::new(
			PageType::IndexUniqueBtreeLeaf,
			PageFlags::LEAF,
			PageLsn::ZERO,
			PageNo::new(0),
		);
		let expected = page.as_bytes()[..8].to_vec();
		let mut layout = LayoutBuilder::new(1);
		layout.add(
			DataSpaceId::Database,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			"atoms/test/primary.fgidx".to_owned(),
			None,
			vec![page],
		)?;

		assert_eq!(layout.files[0].page_numbers, [PageNo::new(0)]);
		assert_eq!(&layout.files[0].pages[0].as_bytes()[..8], expected);
		layout.files[0].pages[0].validate(PageNo::new(0))?;
		Ok(())
	}
}
