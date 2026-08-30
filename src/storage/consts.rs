//! Stable names shared by local storage components.

/// Fixed database-superblock filename inside one local database directory.
pub(in crate::storage) const DATABASE_SUPERBLOCK_FILE: &str = "db.fgb";
/// Fixed frame-WAL directory inside one local database directory.
pub(in crate::storage) const WAL_DIRECTORY: &str = "wal";
