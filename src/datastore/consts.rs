use std::time::Duration;

/// Default lifetime of one MVCC snapshot before cooperative cancellation.
pub(super) const DEFAULT_MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(300);
/// Default logical transaction write-set ceiling.
pub(super) const DEFAULT_MAX_WRITE_SET_BYTES: usize = 256 * 1024 * 1024;
/// Fixed metadata charge for one dirty graph or atom target.
pub(super) const DIRTY_TARGET_WRITE_BYTES: usize = 256;
/// Fixed metadata charge for a deleted catalog entry.
pub(super) const CATALOG_DELETE_WRITE_BYTES: usize = 64;
/// Fixed metadata charge for one changed table schema.
pub(super) const TABLE_SCHEMA_WRITE_BYTES: usize = 128;
/// Fixed metadata charge for one table field.
pub(super) const TABLE_FIELD_WRITE_BYTES: usize = 64;
/// Default 64-MiB pool measured in format-v1 pages.
pub(super) const DEFAULT_BUFFER_POOL_PAGES: usize = 4_096;
/// Default WAL-only page ratio that triggers a passive checkpoint.
pub(super) const DEFAULT_CHECKPOINT_DIRTY_PERCENT: u8 = 25;
/// Default maximum interval between passive checkpoint attempts.
pub(super) const DEFAULT_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(300);
/// Reserved table name used for hidden durable nested-graph identities.
pub(crate) const INTERNAL_NESTED_GRAPH_TABLE: &str = "__fg_nested";
/// Maximum UTF-8 byte length of a catalog graph name.
pub(super) const MAX_GRAPH_NAME_BYTES: usize = 128;
