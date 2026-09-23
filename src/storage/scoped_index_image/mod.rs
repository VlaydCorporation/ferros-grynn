//! Complete physical index images for normalized base atoms and graph
//! occurrences.

mod build;
mod model;
mod validate;

pub(in crate::storage) use build::build_scoped_index_images;
pub(in crate::storage) use model::{ScopedIndexFile, ScopedIndexImages, ScopedIndexKind};
pub(in crate::storage) use validate::validate_scoped_index_images;

#[cfg(test)]
mod tests;
