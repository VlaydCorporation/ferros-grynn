//! Candidate-v1 database and physical-file catalogs.

mod database;
mod file;
mod page_chain;

pub(super) use database::{
	AnalyzerCatalogEntry,
	AnalyzerFilterCatalogEntry,
	AtomStoreCatalogBinding,
	ChangefeedCatalogEntry,
	ClassCatalogEntry,
	ClassFieldCatalogEntry,
	ClassIndexCatalogEntry,
	DatabaseCatalog,
	FunctionCatalogEntry,
	FunctionParameterCatalogEntry,
	GraphCatalogEntry,
	GraphPropertyCatalogEntry,
	LiveBindingCatalogEntry,
	LiveCatalogEntry,
	LiveHistoryCatalogEntry,
	ParameterCatalogEntry,
	PhysicalIndexBinding,
};
pub(in crate::storage) use database::{
	SourceDecoder,
	decode_event_source,
	decode_function_source,
	encode_event_source,
	encode_function_source,
};
pub(super) use file::{CatalogPath, FileCatalog, FileCatalogEntry};
pub(super) use page_chain::{CatalogKind, decode_page_chain, encode_page_chain};

/// Trusted relative path of the database catalog page file.
pub(super) const SYSTEM_CATALOG_PATH: &str = "schema/catalog.fgb";
