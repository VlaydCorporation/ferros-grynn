//! Complete physical index images for normalized base atoms and graph
//! occurrences.

mod build;
mod fulltext;
mod geometry;
mod model;
mod stream;
mod validate;
mod vector;

pub(in crate::storage) use build::{build_scoped_index_images, local_address};
pub(in crate::storage) use fulltext::{
	build_fulltext_index_images,
	write_streamed_fulltext_index_images,
};
pub(in crate::storage) use geometry::{
	build_geometry_index_images,
	write_streamed_geometry_index_images,
};
pub(in crate::storage) use model::{
	ScopedIndexFile,
	ScopedIndexImages,
	ScopedIndexKind,
	StreamedScopedIndexFile,
	StreamedScopedIndexImages,
};
pub(in crate::storage) use stream::{
	write_streamed_file,
	write_streamed_scoped_indexes,
	write_streamed_unique_file,
};
pub(in crate::storage) use validate::validate_scoped_index_images;
pub(in crate::storage) use vector::{
	build_vector_index_images,
	write_streamed_vector_index_images,
};

#[cfg(test)]
mod tests;
