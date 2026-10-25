//! Canonical physical identities for full and selected file generations.

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
/// Configured file catalog root value.
pub(super) const FILE_CATALOG_ROOT: PageNo = PageNo::new(1);
/// Configured database catalog continuation value.
pub(super) const DATABASE_CATALOG_CONTINUATION: PageNo = PageNo::new(2);
/// Configured file catalog continuation value.
pub(super) const FILE_CATALOG_CONTINUATION: PageNo = PageNo::new(1030);

/// Complete page image of one newly allocated physical file.
pub(super) struct FileImage {
	/// Entry associated with this file image.
	pub(super) entry: FileCatalogEntry,
	/// Page numbers associated with this file image.
	pub(super) page_numbers: Vec<PageNo>,
	/// Pages associated with this file image.
	pub(super) pages: Vec<AlignedPage>,
	/// Exact page count for an immutable side-file already written and synced
	/// by its bounded streaming builder.
	pub(super) prewritten_page_count: Option<u64>,
}

/// Exact roots required to reopen one full normalized generation.
pub(super) struct GenerationBindings {
	/// Atoms associated with this generation bindings.
	pub(super) atoms: AtomStoreFileSet,
	/// Reverse associated with this generation bindings.
	pub(super) reverse: ReverseMembershipFile,
	/// Graphs associated with this generation bindings.
	pub(super) graphs: Vec<GraphBinding>,
	/// Physical indexes associated with this generation bindings.
	pub(super) physical_indexes: BTreeMap<IndexId, PhysicalIndexBinding>,
	/// Physical deltas associated with this generation bindings.
	pub(super) physical_deltas: BTreeMap<IndexId, PhysicalIndexBinding>,
}

/// Allocation helper shared by the AtomStore, graph, and index packers.
pub(super) struct LayoutBuilder {
	/// Next file instance id associated with this layout builder.
	next_file_instance_id: u64,
	/// Files associated with this layout builder.
	files: Vec<FileImage>,
	/// Retained associated with this layout builder.
	retained: Vec<FileCatalogEntry>,
}

impl LayoutBuilder {
	/// Creates a new `LayoutBuilder` value.
	pub(super) const fn new(next_file_instance_id: u64) -> Self {
		Self {
			next_file_instance_id,
			files: Vec::new(),
			retained: Vec::new(),
		}
	}

	/// Reports additive images even when no previous identity is retired.
	pub(super) fn has_new_files(&self) -> bool {
		!self.files.is_empty()
	}

	/// Keeps one immutable file reachable without copying it into the new
	/// generation. The retained identity must have been allocated by an older
	/// catalog generation.
	pub(super) fn retain(&mut self, entry: FileCatalogEntry) -> FerrosGrynnResult<()> {
		if entry.file_id.instance_id() >= self.next_file_instance_id {
			return Err(invalid("retained file identity is beyond the allocation clock"));
		}
		self.retained.push(entry);
		Ok(())
	}

	/// Returns the add result.
	pub(super) fn add(
		&mut self,
		space_id: DataSpaceId,
		kind: DataFileKind,
		role: DataFileRole,
		path: String,
		companion_of: Option<DataFileId>,
		pages: Vec<AlignedPage>,
	) -> FerrosGrynnResult<DataFileId> {
		let mut page_numbers = Vec::new();
		page_numbers
			.try_reserve_exact(pages.len())
			.map_err(|_error| invalid("file page directory allocation failed"))?;
		for index in 0..pages.len() {
			page_numbers.push(PageNo::new(
				u64::try_from(index).map_err(|_error| invalid("file page index exceeds u64"))?,
			));
		}
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
			prewritten_page_count: None,
		});
		Ok(file_id)
	}

	/// Registers one immutable side-file that a streaming builder already
	/// wrote and synchronized below the unpublished generation directory.
	pub(super) fn add_prewritten(
		&mut self,
		space_id: DataSpaceId,
		kind: DataFileKind,
		role: DataFileRole,
		path: String,
		page_count: u64,
	) -> FerrosGrynnResult<DataFileId> {
		if page_count == 0 {
			return Err(invalid("prewritten data file is empty"));
		}
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
				companion_of: None,
				role,
				path: CatalogPath::parse(&path)?,
			},
			page_numbers: Vec::new(),
			pages: Vec::new(),
			prewritten_page_count: Some(page_count),
		});
		Ok(file_id)
	}

	/// Returns the add freelist result.
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

	/// Finishes layout builder.
	pub(super) fn finish(mut self) -> FerrosGrynnResult<(FileCatalog, Vec<FileImage>)> {
		let mut catalog = FileCatalog::bootstrap()?;
		catalog.next_file_instance_id = self.next_file_instance_id;
		catalog.files.append(&mut self.retained);
		catalog.files.extend(self.files.iter().map(|file| file.entry.clone()));
		catalog.files.sort_unstable_by_key(|file| (file.space_id, file.file_id));
		self.files.sort_unstable_by_key(|file| (file.entry.space_id, file.entry.file_id));
		catalog.encode()?;
		Ok((catalog, self.files))
	}
}

/// Bounds selected-file vector growth and two simultaneously owned catalog
/// copies.
pub(super) fn selected_file_metadata_bytes(path: &str) -> FerrosGrynnResult<usize> {
	// Initial Vec capacity is four; later growth can retain old+new buffers.
	// Both catalogs clone the selected entry/path while FileImage stays alive.
	4_usize
		.checked_mul(size_of::<FileImage>())
		.and_then(|bytes| bytes.checked_add(6 * size_of::<FileCatalogEntry>()))
		.and_then(|bytes| bytes.checked_add(path.len().checked_mul(4)?))
		.and_then(|bytes| bytes.checked_add(3 * size_of::<String>()))
		.ok_or_else(|| invalid("selected file metadata bytes overflow usize"))
}

/// Returns the atom prefix result.
pub(super) fn atom_prefix(generation: &str) -> String {
	format!("atoms/{generation}")
}

/// Returns the graph prefix result.
pub(super) fn graph_prefix(graph: crate::graph::GraphId, generation: &str) -> String {
	format!("graphs/{:016x}/{generation}", graph.get())
}

/// Returns the invalid result.
pub(super) fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Regression tests for layout behavior.
mod tests {
	use super::*;
	use crate::storage::page::{PageFlags, PageLsn, PageType};

	/// Fallible directory construction retains only the admitted exact
	/// capacity.
	#[test]
	fn file_image_page_directory_reserves_exact_capacity() {
		use crate::test_assertions::AssertResult as _;
		for count in [0, 1, 3, 5, 17] {
			let mut layout = LayoutBuilder::new(1);
			let pages = (0..count).map(|_| AlignedPage::zeroed()).collect();
			layout
				.add(
					DataSpaceId::Database,
					DataFileKind::Index,
					DataFileRole::WalBackfilled,
					"atoms/test/primary.fgidx".to_owned(),
					None,
					pages,
				)
				.assert_ok();
			assert_eq!(layout.files[0].page_numbers.len(), count);
			assert_eq!(layout.files[0].page_numbers.capacity(), count);
		}
	}

	/// Selected additions reserve vector slack and both simultaneous catalog
	/// copies.
	#[test]
	fn selected_file_metadata_reserve_covers_multiple_graph_catalog_copies() {
		use crate::{
			graph::GraphId,
			test_assertions::{AssertOption as _, AssertResult as _},
			values::Uuid,
		};
		let graphs = [GraphId::new(1).assert_some(), GraphId::new(2).assert_some()];
		for count in [1_usize, 3, 5, 17, 33] {
			let mut layout = LayoutBuilder::new(1);
			let mut admitted = 4096_usize;
			for index in 0..count {
				let graph = graphs[index % 2];
				let path = format!(
					"{}/idx/lbl_node_{index}.fgidx",
					graph_prefix(graph, &Uuid::now_v7().to_string())
				);
				admitted += selected_file_metadata_bytes(&path).assert_ok();
				layout
					.add_prewritten(
						DataSpaceId::Graph(graph),
						DataFileKind::Index,
						DataFileRole::WalBackfilled,
						path,
						1,
					)
					.assert_ok();
			}
			let (catalog, images) = layout.finish().assert_ok();
			let second = catalog.clone();
			let vectors = images.capacity() * size_of::<FileImage>()
				+ (catalog.files.capacity() + second.files.capacity())
					* size_of::<FileCatalogEntry>();
			let paths = images.iter().map(|image| image.entry.path.as_str().len()).sum::<usize>()
				+ catalog
					.files
					.iter()
					.chain(&second.files)
					.map(|entry| entry.path.as_str().len())
					.sum::<usize>();
			assert!(admitted >= vectors + paths);
		}
	}

	/// Keeps over-aligned page storage separate from per-page numbers so moving
	/// a generated image cannot corrupt its first header byte.
	#[test]
	fn file_image_preserves_over_aligned_page_bytes() {
		use crate::test_assertions::AssertResult as _;

		let page = AlignedPage::new(
			PageType::IndexUniqueBtreeLeaf,
			PageFlags::LEAF,
			PageLsn::ZERO,
			PageNo::new(0),
		);
		let expected = page.as_bytes()[..8].to_vec();
		let mut layout = LayoutBuilder::new(1);
		layout
			.add(
				DataSpaceId::Database,
				DataFileKind::Index,
				DataFileRole::WalBackfilled,
				"atoms/test/primary.fgidx".to_owned(),
				None,
				vec![page],
			)
			.assert_ok();

		assert_eq!(layout.files[0].page_numbers, [PageNo::new(0)]);
		assert_eq!(&layout.files[0].pages[0].as_bytes()[..8], expected);
		layout.files[0].pages[0].validate(PageNo::new(0)).assert_ok();
	}
}
