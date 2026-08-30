use std::{
	collections::BTreeMap,
	num::NonZeroUsize,
	sync::{Arc, atomic::AtomicU64},
};

use parking_lot::{Mutex, RwLock};

use super::{
	options::{DatastoreOptions, InMemoryOptions, IoMode, LocalOptions},
	root::{ActiveSnapshot, DatabaseRoot},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		LocalCommitGraph,
		LocalCommitResult,
		LocalCommitTable,
		LocalCommitTableField,
		LocalCommitTableIndex,
		LocalStore,
		PageIoMode,
	},
	values::runtime::{PropertyKeyId, RecordId, graph::GraphStorageMode},
};

pub(crate) struct StoreInner {
	pub(crate) options: DatastoreOptions,
	pub(crate) backend: StoreBackend,
	pub(crate) next_transaction: AtomicU64,
	pub(crate) current: RwLock<Arc<DatabaseRoot>>,
	pub(crate) commit: Mutex<()>,
	pub(crate) active: Mutex<BTreeMap<u64, ActiveSnapshot>>,
}

/// Storage-specific durability owner behind the shared MVCC root.
pub(crate) enum StoreBackend {
	/// Volatile root with no durability owner.
	InMemory,
	/// Windows-first synchronous local durability owner.
	Local(Box<LocalStore>),
}

impl StoreBackend {
	pub(crate) fn local(&self) -> Option<&LocalStore> {
		match self {
			Self::InMemory => None,
			Self::Local(store) => Some(store),
		}
	}
}

impl StoreInner {
	/// Returns MVCC limits independent of the selected persistence backend.
	pub(crate) const fn transaction_limits(&self) -> InMemoryOptions {
		match self.options {
			DatastoreOptions::InMemory(options) => options,
			DatastoreOptions::Local(options) => InMemoryOptions {
				max_snapshot_age: options.max_snapshot_age,
				max_write_set_bytes: options.max_write_set_bytes,
			},
		}
	}

	/// Returns the graph-reference mode owned by this datastore.
	pub(crate) const fn graph_storage_mode(&self) -> GraphStorageMode {
		match self.backend {
			StoreBackend::InMemory => GraphStorageMode::InMemory,
			StoreBackend::Local(_) => GraphStorageMode::DiskBacked,
		}
	}

	/// Makes one already validated MVCC root durable before publication.
	pub(crate) fn persist_root(
		&self,
		expected_commit_ts: u64,
		root: &DatabaseRoot,
		changed_graphs: &[RecordId],
	) -> FerrosGrynnResult<Option<LocalCommitResult>> {
		let StoreBackend::Local(local) = &self.backend else {
			return Ok(None);
		};
		let graphs = changed_graphs
			.iter()
			.filter_map(|record_id| root.graphs.get(record_id).map(|stored| (record_id, stored)))
			.map(|(record_id, stored)| {
				stored
					.graph
					.materialized()
					.cloned()
					.map(|graph| LocalCommitGraph {
						record_id: record_id.clone(),
						name: stored.name.clone(),
						graph,
					})
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"changed local graph has no materialized commit image",
						)
					})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let tables = root
			.tables
			.values()
			.map(|schema| {
				let fields = schema
					.fields
					.iter()
					.map(|field| {
						Ok(LocalCommitTableField {
							name: field.name.as_str().to_owned(),
							value_type: field.value_type.clone(),
							property_key: field
								.key
								.ok_or_else(|| {
									FerrosGrynnError::invalid_storage_file(
										"table field has no interned property key",
									)
								})?
								.get(),
						})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				Ok(LocalCommitTable {
					name: schema.name.as_str().to_owned(),
					graph: schema.graph.id().clone(),
					kind: schema.kind,
					fields,
					indexes: schema
						.indexes
						.iter()
						.map(|index| {
							let fields = index
								.fields
								.iter()
								.map(|name| {
									schema
										.fields
										.iter()
										.find(|field| field.name == *name)
										.and_then(|field| field.key)
										.map(PropertyKeyId::get)
										.ok_or_else(|| {
											FerrosGrynnError::invalid_storage_file(
												"table index field has no interned property key",
											)
										})
								})
								.collect::<FerrosGrynnResult<Vec<_>>>()?;
							Ok(LocalCommitTableIndex {
								name: index.name.as_str().to_owned(),
								kind: index.kind,
								fields,
							})
						})
						.collect::<FerrosGrynnResult<Vec<_>>>()?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		local
			.commit(expected_commit_ts, &root.dictionary, &graphs, &tables, changed_graphs)
			.map(Some)
	}
}

#[derive(Debug)]
pub(crate) struct ValidatedLocalOptions {
	pub(crate) pool_pages: NonZeroUsize,
	pub(crate) checkpoint_page_threshold: usize,
	pub(crate) io_mode: PageIoMode,
}

/// Validates local resource limits and compile-time I/O availability.
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
					"Windows IoRing support requires a Windows build with feature native-async-io",
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
