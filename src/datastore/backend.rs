//! Backend ownership and resource-policy conversion for one MVCC store.

use std::{
	num::NonZeroUsize,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use parking_lot::{Mutex, RwLock};

use super::{
	CheckpointMode,
	DatabaseRoot,
	DatastoreOptions,
	InMemoryOptions,
	IoMode,
	LocalOptions,
	SchemaId,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{GraphId, GraphStorageMode},
	storage::{LocalStore, PageIoMode},
};

pub(crate) struct StoreInner {
	pub(crate) options: DatastoreOptions,
	pub(crate) backend: StoreBackend,
	pub(crate) next_transaction: AtomicU64,
	pub(crate) next_schema_id: AtomicU64,
	pub(crate) next_graph_id: AtomicU64,
	pub(crate) current: RwLock<Arc<DatabaseRoot>>,
	pub(crate) commit: Mutex<()>,
}

pub(crate) enum StoreBackend {
	InMemory,
	Local(Box<LocalStore>),
}

impl StoreInner {
	pub(crate) fn allocate_schema_id(&self) -> FerrosGrynnResult<SchemaId> {
		let raw = self
			.next_schema_id
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
				(current != 0 && current < u64::from(u32::MAX)).then_some(current + 1)
			})
			.map_err(|_current| {
				FerrosGrynnError::invalid_argument("schema identity clock is exhausted")
			})?;
		let raw = u32::try_from(raw).map_err(|_error| {
			FerrosGrynnError::invalid_argument("schema identity clock is exhausted")
		})?;
		SchemaId::new(raw)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("schema identity is reserved"))
	}

	pub(crate) fn allocate_graph_id(&self) -> FerrosGrynnResult<GraphId> {
		let raw = self
			.next_graph_id
			.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| current.checked_add(1))
			.map_err(|_current| {
				FerrosGrynnError::invalid_argument("graph identity clock is exhausted")
			})?;
		GraphId::new(raw)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph identity zero is reserved"))
	}

	pub(crate) fn current_root(&self) -> Arc<DatabaseRoot> {
		self.current.read().clone()
	}

	pub(crate) const fn transaction_limits(&self) -> InMemoryOptions {
		match self.options {
			DatastoreOptions::InMemory(options) => options,
			DatastoreOptions::Local(options) => InMemoryOptions {
				max_snapshot_age: options.max_snapshot_age,
				max_write_set_bytes: options.max_write_set_bytes,
			},
		}
	}

	pub(crate) const fn graph_storage_mode(&self) -> GraphStorageMode {
		match &self.backend {
			StoreBackend::InMemory => GraphStorageMode::InMemory,
			StoreBackend::Local(_) => GraphStorageMode::DiskBacked,
		}
	}

	pub(crate) fn persist_root(&self, root: &DatabaseRoot) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => Ok(()),
			StoreBackend::Local(local) => local.persist_root(root),
		}
	}

	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		match &self.backend {
			StoreBackend::InMemory => Ok(false),
			StoreBackend::Local(local) => local.checkpoint(mode),
		}
	}
}

pub(crate) struct ValidatedLocalOptions {
	pub(crate) pool_pages: NonZeroUsize,
	pub(crate) checkpoint_page_threshold: usize,
	pub(crate) io_mode: PageIoMode,
}

pub(crate) fn validate_local_options(
	options: LocalOptions,
) -> FerrosGrynnResult<ValidatedLocalOptions> {
	let io_mode = match options.io_mode {
		IoMode::Sync => PageIoMode::Sync,
		IoMode::NativeAsync => {
			#[cfg(all(windows, feature = "native-async-io"))]
			{
				PageIoMode::NativeAsync
			}
			#[cfg(not(all(windows, feature = "native-async-io")))]
			{
				return Err(FerrosGrynnError::io_mode_unavailable(
					"NativeAsync",
					"Windows IoRing requires feature native-async-io",
				));
			}
		}
	};
	let pool_pages = NonZeroUsize::new(options.buffer_pool_pages).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("local buffer_pool_pages must be greater than zero")
	})?;
	if !(1..=100).contains(&options.checkpoint.unbackfilled_page_percent) {
		return Err(FerrosGrynnError::invalid_argument(
			"checkpoint unbackfilled_page_percent must be in 1..=100",
		));
	}
	let checkpoint_page_threshold = options
		.buffer_pool_pages
		.checked_mul(usize::from(options.checkpoint.unbackfilled_page_percent))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("checkpoint page threshold overflows"))?
		.div_ceil(100)
		.max(1);
	Ok(ValidatedLocalOptions {
		pool_pages,
		checkpoint_page_threshold,
		io_mode,
	})
}
