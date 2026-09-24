//! Creation and WAL staging of one immutable generation's physical files.

use std::{fs, path::Path};

use super::{image::BuiltGeneration, layout::invalid};
use crate::{
	datastore::DataSpaceId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		direct::DirectFile,
		types::{DataFileKind, PageKey},
		wal::TxHandle,
	},
};

/// Materializes file lengths before publication and optionally installs the
/// initial home image when no WAL history exists yet.
pub(super) fn create_generation_files(
	root: &Path,
	generation: &BuiltGeneration,
	install_home: bool,
) -> FerrosGrynnResult<()> {
	create_generation_files_with_mode(root, generation, install_home, false)
}

/// Writes an unreachable import generation to its final home files while
/// leaving the shared catalog file reachable only through WAL publication.
pub(super) fn create_import_generation_files(
	root: &Path,
	generation: &BuiltGeneration,
) -> FerrosGrynnResult<()> {
	create_generation_files_with_mode(root, generation, false, true)
}

fn create_generation_files_with_mode(
	root: &Path,
	generation: &BuiltGeneration,
	install_home: bool,
	write_unreachable_home: bool,
) -> FerrosGrynnResult<()> {
	for image in &generation.files {
		let path = image.entry.path.resolve(root);
		if let Some(expected) = image.prewritten_page_count {
			if !write_unreachable_home {
				return Err(invalid("prewritten file reached a non-import generation"));
			}
			let file = DirectFile::open_read_write(&path)?;
			if file.page_count() != expected {
				return Err(invalid("prewritten file length disagrees with its catalog image"));
			}
			file.sync_data()?;
			continue;
		}
		let system = image.entry.space_id == DataSpaceId::Database
			&& image.entry.file_id.kind() == Some(DataFileKind::SystemCatalog);
		if system && !install_home {
			let file = DirectFile::open_read_write(&path)?;
			let required = page_count(image)?;
			if file.page_count() < required {
				file.resize_pages(required)?;
				file.sync_data()?;
			}
			continue;
		}
		let parent = path.parent().ok_or_else(|| invalid("data file has no parent directory"))?;
		fs::create_dir_all(parent)
			.map_err(|source| io_failure("create generation directory", source))?;
		let file = DirectFile::create_new(&path)?;
		file.resize_pages(page_count(image)?)?;
		if install_home || write_unreachable_home {
			for (number, page) in image.page_numbers.iter().copied().zip(&image.pages) {
				file.write_page(number, page)?;
			}
		}
		file.sync_data()?;
	}
	Ok(())
}

/// Stages every data, index, metadata, and catalog page in one WAL handle.
pub(super) fn stage_generation(
	tx: &mut TxHandle,
	generation: &BuiltGeneration,
) -> FerrosGrynnResult<()> {
	for image in &generation.files {
		if image.prewritten_page_count.is_some() {
			return Err(invalid("prewritten file cannot be staged through the page WAL"));
		}
		page_count(image)?;
		for (number, page) in image.page_numbers.iter().copied().zip(&image.pages) {
			tx.stage_page(PageKey::new(image.entry.space_id, image.entry.file_id, number), page)?;
		}
	}
	Ok(())
}

/// Stages only the shared catalog pages that atomically make a pre-synced
/// import generation reachable.
pub(super) fn stage_import_catalog(
	tx: &mut TxHandle,
	generation: &BuiltGeneration,
) -> FerrosGrynnResult<()> {
	let image = generation
		.files
		.iter()
		.find(|image| {
			image.entry.space_id == DataSpaceId::Database
				&& image.entry.file_id.kind() == Some(DataFileKind::SystemCatalog)
		})
		.ok_or_else(|| invalid("import generation has no system catalog image"))?;
	page_count(image)?;
	for (number, page) in image.page_numbers.iter().copied().zip(&image.pages) {
		tx.stage_page(PageKey::new(image.entry.space_id, image.entry.file_id, number), page)?;
	}
	Ok(())
}

fn page_count(image: &super::layout::FileImage) -> FerrosGrynnResult<u64> {
	if let Some(page_count) = image.prewritten_page_count {
		if !image.page_numbers.is_empty() || !image.pages.is_empty() {
			return Err(invalid("prewritten data file also contains buffered pages"));
		}
		return Ok(page_count);
	}
	if image.page_numbers.len() != image.pages.len() {
		return Err(invalid("data-file page numbers and images have different lengths"));
	}
	image.page_numbers.iter().map(|page| page.get()).max().map_or(Ok(0), |last| {
		last.checked_add(1).ok_or_else(|| invalid("data-file page count overflows u64"))
	})
}

fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
