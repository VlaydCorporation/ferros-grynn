//! Backend ownership and resource-policy conversion for one MVCC store.

use std::{
	collections::BTreeMap,
	fs,
	num::NonZeroUsize,
	path::PathBuf,
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
	pub(crate) import_owner: AtomicU64,
	pub(crate) current: RwLock<Arc<DatabaseRoot>>,
	pub(crate) commit: Mutex<()>,
	pub(crate) query_scratch_root: PathBuf,
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

	pub(crate) fn persist_root(
		&self,
		root: &DatabaseRoot,
		graph_catalog_only: bool,
		dictionary_changed: bool,
	) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => Ok(()),
			StoreBackend::Local(local) => {
				local.persist_root(root, graph_catalog_only, dictionary_changed)
			}
		}
	}

	pub(crate) fn persist_import_root(
		&self,
		root: &DatabaseRoot,
	) -> FerrosGrynnResult<BTreeMap<super::IndexId, std::time::Duration>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(BTreeMap::new()),
			StoreBackend::Local(local) => local.persist_import_root(root),
		}
	}

	pub(crate) fn import_scratch_root(&self) -> std::path::PathBuf {
		match &self.backend {
			StoreBackend::InMemory => std::env::temp_dir().join("ferros-grynn-import"),
			StoreBackend::Local(local) => local.import_scratch_root().to_path_buf(),
		}
	}

	pub(crate) fn query_scratch_root(&self) -> std::path::PathBuf {
		self.query_scratch_root.clone()
	}

	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		match &self.backend {
			StoreBackend::InMemory => Ok(false),
			StoreBackend::Local(local) => local.checkpoint(mode),
		}
	}
}

impl Drop for StoreInner {
	fn drop(&mut self) {
		if matches!(self.backend, StoreBackend::InMemory) {
			drop(fs::remove_dir_all(&self.query_scratch_root));
		}
	}
}

pub(crate) struct ValidatedLocalOptions {
	pub(crate) pool_pages: NonZeroUsize,
	pub(crate) checkpoint_page_threshold: usize,
	pub(crate) io_mode: PageIoMode,
}

#[cfg(test)]
mod query_scratch_tests {
	use super::*;

	#[test]
	fn in_memory_stores_own_distinct_query_scratch_roots() {
		let first = crate::datastore::Datastore::in_memory();
		let second = crate::datastore::Datastore::in_memory();
		let first_root = first.inner.query_scratch_root();
		let second_root = second.inner.query_scratch_root();
		assert_ne!(first_root, second_root);
		fs::create_dir_all(&first_root).unwrap();
		fs::create_dir_all(&second_root).unwrap();

		drop(first);
		assert!(!first_root.exists());
		assert!(second_root.exists());
		drop(second);
		assert!(!second_root.exists());
	}
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
