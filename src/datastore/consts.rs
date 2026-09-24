use std::time::Duration;

/// Default lifetime of one MVCC snapshot before cooperative cancellation.
pub(super) const DEFAULT_MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(300);
/// Default logical transaction write-set ceiling.
pub(super) const DEFAULT_MAX_WRITE_SET_BYTES: usize = 256 * 1024 * 1024;
/// Default count of applied logical items retained in one private import
/// segment.
pub(super) const DEFAULT_IMPORT_SEGMENT_ITEMS: usize = 1_000_000;
/// Maximum count of detailed violations retained in an import report.
pub(super) const IMPORT_MAX_REPORTED_VIOLATIONS: usize = 1_000;
/// Default 64-MiB pool measured in format-v1 pages.
pub(super) const DEFAULT_BUFFER_POOL_PAGES: usize = 4_096;
/// Default WAL-only page ratio that triggers a passive checkpoint.
pub(super) const DEFAULT_CHECKPOINT_DIRTY_PERCENT: u8 = 25;
/// Default maximum interval between passive checkpoint attempts.
pub(super) const DEFAULT_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(300);

/// Conservative logical charge for changing one database-global atom.
pub(super) const BASE_ATOM_WRITE_BYTES: usize = 128;
/// Logical charge for creating one reverse and graph-local membership entry.
pub(super) const GRAPH_MEMBERSHIP_WRITE_BYTES: usize = 64;
/// Logical charge for one graph-local occurrence slot and its structural state.
pub(super) const GRAPH_INCIDENCE_WRITE_BYTES: usize = 128;
/// Logical charge for one sparse graph-local property overlay.
pub(super) const GRAPH_OVERLAY_WRITE_BYTES: usize = 64;
/// Per-membership cost reserved by a base-property fanout preflight.
pub(super) const EFFECTIVE_FANOUT_WRITE_BYTES: usize = 64;
/// Logical charge for publishing one class or graph schema definition.
pub(super) const SCHEMA_WRITE_BYTES: usize = 256;
/// Logical charge for one append-only property dictionary update.
pub(super) const PROPERTY_DICTIONARY_WRITE_BYTES: usize = 64;
