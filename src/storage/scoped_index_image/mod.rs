//! Complete physical index images for normalized base atoms and graph
//! occurrences.

mod build;
mod model;
mod stream;
mod validate;

pub(in crate::storage) use build::build_scoped_index_images;
pub(in crate::storage) use model::{
	ScopedIndexFile,
	ScopedIndexImages,
	ScopedIndexKind,
	StreamedScopedIndexFile,
	StreamedScopedIndexImages,
};
pub(in crate::storage) use stream::{write_streamed_scoped_indexes, write_streamed_unique_file};
pub(in crate::storage) use validate::validate_scoped_index_images;

#[cfg(test)]
mod tests;
