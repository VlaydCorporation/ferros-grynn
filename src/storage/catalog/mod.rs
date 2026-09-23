//! Candidate-v1 database and physical-file catalogs.

mod database;
mod file;
mod page_chain;

pub(super) use database::{
	AtomStoreCatalogBinding,
	ClassCatalogEntry,
	ClassFieldCatalogEntry,
	ClassIndexCatalogEntry,
	DatabaseCatalog,
	GraphCatalogEntry,
	ParameterCatalogEntry,
	PhysicalIndexBinding,
};
pub(super) use file::{CatalogPath, FileCatalog, FileCatalogEntry};
pub(super) use page_chain::{CatalogKind, decode_page_chain, encode_page_chain};

/// Trusted relative path of the database catalog page file.
pub(super) const SYSTEM_CATALOG_PATH: &str = "schema/catalog.fgb";
