use std::{
	num::NonZeroUsize,
	time::{Duration, Instant},
};

use super::consts::{
	DEFAULT_BUFFER_POOL_PAGES,
	DEFAULT_CHECKPOINT_DIRTY_PERCENT,
	DEFAULT_CHECKPOINT_INTERVAL,
	DEFAULT_MAX_SNAPSHOT_AGE,
	DEFAULT_MAX_WRITE_SET_BYTES,
};

/// Resource limits shared by foreground and background maintenance builders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MaintenanceOptions {
	/// Maximum aggregate memory used by all sort runs and worker lanes.
	pub max_memory_bytes: NonZeroUsize,
	/// Maximum number of scoped worker threads used by one build.
	pub max_parallelism: NonZeroUsize,
}

impl Default for MaintenanceOptions {
	/// Constructs the default initial state of `MaintenanceOptions`.
	fn default() -> Self {
		Self {
			max_memory_bytes: NonZeroUsize::new(256 * 1024 * 1024).unwrap_or(NonZeroUsize::MIN),
			max_parallelism: std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
		}
	}
}

/// Physical I/O strategy selected for one local datastore.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IoMode {
	/// Synchronous positional Direct I/O.
	#[default]
	Sync,
	/// Windows IoRing Direct I/O when the `native-async-io` feature is enabled.
	NativeAsync,
}

/// Checkpoint behavior requested by an explicit maintenance call.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointMode {
	/// Advance only to the oldest active storage snapshot.
	#[default]
	Passive,
	/// Cancel active storage snapshots and advance to the captured durable
	/// tail.
	Restart,
}

/// Automatic passive-checkpoint thresholds for a local datastore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckpointPolicy {
	/// Percentage of the buffer-pool page count allowed to remain WAL-only.
	pub unbackfilled_page_percent: u8,
	/// Maximum time between checkpoint attempts while WAL-only pages exist.
	pub interval: Duration,
}

impl Default for CheckpointPolicy {
	/// Constructs the default initial state of `CheckpointPolicy`.
	fn default() -> Self {
		Self {
			unbackfilled_page_percent: DEFAULT_CHECKPOINT_DIRTY_PERCENT,
			interval: DEFAULT_CHECKPOINT_INTERVAL,
		}
	}
}

/// Resource limits and I/O selection for a local datastore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LocalOptions {
	/// Physical I/O implementation selected at open time.
	pub io_mode: IoMode,
	/// Exact number of 16-KiB frames in the bounded page cache.
	pub buffer_pool_pages: usize,
	/// Automatic passive-checkpoint thresholds.
	pub checkpoint: CheckpointPolicy,
	/// Maximum lifetime of one transaction snapshot.
	pub max_snapshot_age: Duration,
	/// Maximum estimated bytes in one transaction-private write set.
	pub max_write_set_bytes: usize,
	/// Resource limits for maintenance and import builders.
	pub maintenance: MaintenanceOptions,
}

impl Default for LocalOptions {
	/// Constructs the default initial state of `LocalOptions`.
	fn default() -> Self {
		Self {
			io_mode: IoMode::Sync,
			buffer_pool_pages: DEFAULT_BUFFER_POOL_PAGES,
			checkpoint: CheckpointPolicy::default(),
			max_snapshot_age: DEFAULT_MAX_SNAPSHOT_AGE,
			max_write_set_bytes: DEFAULT_MAX_WRITE_SET_BYTES,
			maintenance: MaintenanceOptions::default(),
		}
	}
}

/// Snapshot-lifetime, write-set and maintenance limits for an in-memory
/// datastore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InMemoryOptions {
	/// Maximum lifetime accepted for a retained transaction snapshot.
	pub max_snapshot_age: Duration,
	/// Maximum admitted memory for one transaction write set.
	pub max_write_set_bytes: usize,
	/// Resource limits for maintenance and import builders.
	pub maintenance: MaintenanceOptions,
}

impl Default for InMemoryOptions {
	/// Constructs the default initial state of `InMemoryOptions`.
	fn default() -> Self {
		Self {
			max_snapshot_age: DEFAULT_MAX_SNAPSHOT_AGE,
			max_write_set_bytes: DEFAULT_MAX_WRITE_SET_BYTES,
			maintenance: MaintenanceOptions::default(),
		}
	}
}

/// Backend and resource limits owned by one datastore instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DatastoreOptions {
	/// Volatile in-process storage.
	InMemory(
		/// Volatile in-process storage.
		InMemoryOptions,
	),
	/// Exclusively owned local disk storage.
	Local(
		/// Exclusively owned local disk storage.
		LocalOptions,
	),
}

impl Default for DatastoreOptions {
	/// Constructs the default initial state of `DatastoreOptions`.
	fn default() -> Self {
		Self::InMemory(InMemoryOptions::default())
	}
}

/// Transaction isolation contract requested by a datastore client.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IsolationLevel {
	/// Read data committed at each permitted transaction observation point.
	ReadCommitted,
	/// Read from one retained snapshot with write-conflict validation.
	Snapshot,
	/// Apply serializable transaction validation in addition to snapshot reads.
	#[default]
	Serializable,
}

/// Whether a transaction may modify the database or only read it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransactionAccess {
	/// Permit writes in addition to reads.
	#[default]
	ReadWrite,
	/// Reject operations that would mutate the database.
	ReadOnly,
}

/// Isolation and access settings selected when beginning a transaction.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransactionOptions {
	/// Isolation contract applied when the transaction begins.
	pub isolation: IsolationLevel,
	/// Requested access mode or explicit query access-path selection.
	pub access: TransactionAccess,
}

impl TransactionOptions {
	/// Constructs `TransactionOptions` from the supplied initial state.
	pub fn new(isolation: IsolationLevel, access: TransactionAccess) -> Self {
		Self {
			isolation,
			access,
		}
	}
}

/// Cooperative controls for commit preparation and LIVE capacity waits.
/// Cancellation uses the existing query token so query execution and its
/// publication share one boundary. Controls stop before the durable barrier.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CommitOptions {
	/// Absolute deadline, also bounding LIVE BLOCK waits.
	pub deadline: Option<Instant>,
	/// Shared cooperative cancellation request.
	pub cancellation: Option<crate::gql::QueryCancellation>,
}

impl CommitOptions {
	/// Checks cancellation and the absolute deadline before publication.
	pub(crate) fn check(&self) -> crate::gql_status::FerrosGrynnResult<()> {
		if self.cancellation.as_ref().is_some_and(crate::gql::QueryCancellation::is_cancelled) {
			return Err(crate::gql_status::FerrosGrynnError::query_cancelled());
		}
		if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
			return Err(crate::gql_status::FerrosGrynnError::query_timed_out(0));
		}
		Ok(())
	}
}
