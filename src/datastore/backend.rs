//! Backend ownership and resource-policy conversion for one MVCC store.

use std::{
	collections::BTreeMap,
	fs,
	num::NonZeroUsize,
	path::PathBuf,
	sync::{
		Arc,
		atomic::{AtomicU64, AtomicUsize, Ordering},
	},
};

use parking_lot::{Mutex, RwLock};

use super::{
	CheckpointMode,
	DatabaseRoot,
	DatastoreOptions,
	FunctionId,
	InMemoryOptions,
	IoMode,
	LocalOptions,
	LocalStorageStatus,
	MaintenanceCoordinator,
	MaintenanceJobInfo,
	MaintenanceJobKind,
	MaintenanceRunOutcome,
	MaintenanceTarget,
	QueryStatisticsDemand,
	SchemaAnalyzer,
	SchemaId,
	SchemaVersion,
};
use crate::{
	gql::PureFunctionEvaluator,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId, GraphStorageMode},
	stats::StatisticsStore,
	storage::{
		LocalQuerySnapshot,
		LocalStore,
		PageIoMode,
		PhysicalIndexRead,
		PhysicalTraversalFieldsRead,
		PhysicalTraversalInnerRead,
		PhysicalTraversalRead,
		QuarantineArchiveReader,
		QuarantineArtifact,
	},
	values::{RecordId, Uuid, Vector, storage::DecodeContext},
};

pub(crate) struct StoreInner {
	pub(crate) options: DatastoreOptions,
	pub(crate) backend: StoreBackend,
	pub(crate) next_transaction: AtomicU64,
	pub(crate) next_schema_id: AtomicU64,
	pub(crate) next_graph_id: AtomicU64,
	pub(crate) import_owner: AtomicU64,
	pub(crate) datastore_handles: AtomicUsize,
	pub(crate) current: RwLock<Arc<DatabaseRoot>>,
	pub(crate) commit: Mutex<()>,
	pub(crate) query_scratch_root: PathBuf,
	pub(crate) quarantines: Mutex<BTreeMap<Uuid, PathBuf>>,
	pub(crate) maintenance: MaintenanceCoordinator,
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
				maintenance: options.maintenance,
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
		root: &mut DatabaseRoot,
		previous: Option<&DatabaseRoot>,
		changed_records: Option<&std::collections::BTreeSet<RecordId>>,
		graph_catalog_only: bool,
		dictionary_changed: bool,
	) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => {
				root.resident_geometry = Arc::new(crate::storage::build_resident_geometry_indexes(
					previous,
					root,
					changed_records,
				)?);
				Ok(())
			}
			StoreBackend::Local(local) => {
				let evaluator = PureFunctionEvaluator::new(root);
				let mut analyze =
					|analyzer: &SchemaAnalyzer, input: &str| evaluator.analyze(analyzer, input);
				let mut distance = |function: FunctionId,
				                    version: SchemaVersion,
				                    left: &Vector,
				                    right: &Vector| {
					evaluator.distance(function, version, left, right)
				};
				local.persist_root(
					root,
					previous,
					changed_records,
					graph_catalog_only,
					dictionary_changed,
					&mut analyze,
					&mut distance,
				)
			}
		}
	}

	pub(crate) fn persist_import_root(
		&self,
		root: &mut DatabaseRoot,
		quarantine: Option<&mut QuarantineArtifact>,
	) -> FerrosGrynnResult<BTreeMap<super::IndexId, std::time::Duration>> {
		match &self.backend {
			StoreBackend::InMemory => {
				root.resident_geometry =
					Arc::new(crate::storage::build_resident_geometry_indexes(None, root, None)?);
				if let Some(artifact) = quarantine {
					let id = artifact.id();
					let path = artifact.install(&self.query_scratch_root)?;
					artifact.publish();
					self.quarantines.lock().insert(id, path);
				}
				Ok(BTreeMap::new())
			}
			StoreBackend::Local(local) => {
				let evaluator = PureFunctionEvaluator::new(root);
				let mut analyze =
					|analyzer: &SchemaAnalyzer, input: &str| evaluator.analyze(analyzer, input);
				let mut distance = |function: FunctionId,
				                    version: SchemaVersion,
				                    left: &Vector,
				                    right: &Vector| {
					evaluator.distance(function, version, left, right)
				};
				local.persist_import_root(root, quarantine, &mut analyze, &mut distance)
			}
		}
	}

	pub(crate) fn open_quarantine(&self, id: Uuid) -> FerrosGrynnResult<QuarantineArchiveReader> {
		match &self.backend {
			StoreBackend::InMemory => {
				let path = self.quarantines.lock().get(&id).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("quarantine identity does not exist")
				})?;
				QuarantineArchiveReader::open(&path)
			}
			StoreBackend::Local(local) => local.open_quarantine(id),
		}
	}

	pub(crate) fn remove_quarantine(&self, id: Uuid) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => {
				let path = self.quarantines.lock().remove(&id).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("quarantine identity does not exist")
				})?;
				match fs::remove_file(path) {
					Ok(()) => Ok(()),
					Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
					Err(source) => {
						Err(FerrosGrynnError::storage_io_failure("remove import quarantine")
							.with_technical_source(source))
					}
				}
			}
			StoreBackend::Local(local) => {
				let _commit = self.commit.lock();
				let snapshot = self.current_root();
				let mut next = (*snapshot).clone();
				next.commit_ts = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"quarantine removal commit clock is exhausted",
					)
				})?;
				let evaluator = PureFunctionEvaluator::new(&next);
				let mut analyze =
					|analyzer: &SchemaAnalyzer, input: &str| evaluator.analyze(analyzer, input);
				let mut distance = |function: FunctionId,
				                    version: SchemaVersion,
				                    left: &Vector,
				                    right: &Vector| {
					evaluator.distance(function, version, left, right)
				};
				local.remove_quarantine(&next, id, &mut analyze, &mut distance)?;
				*self.current.write() = Arc::new(next);
				Ok(())
			}
		}
	}

	pub(crate) fn import_scratch_root(&self) -> std::path::PathBuf {
		match &self.backend {
			StoreBackend::InMemory => std::env::temp_dir().join("ferros-grynn-import"),
			StoreBackend::Local(local) => local.import_scratch_root().to_path_buf(),
		}
	}

	pub(crate) fn persist_maintenance_jobs(
		&self,
		jobs: &[MaintenanceJobInfo],
	) -> FerrosGrynnResult<()> {
		match &self.backend {
			StoreBackend::InMemory => Ok(()),
			StoreBackend::Local(local) => local.persist_maintenance_jobs(jobs),
		}
	}

	pub(crate) fn maintenance_publication_matches(&self, job: &MaintenanceJobInfo) -> bool {
		let Some(fingerprint) = job.candidate_fingerprint else {
			return false;
		};
		let root = self.current_root();
		match job.target {
			MaintenanceTarget::GraphIndex {
				graph,
				index,
				version,
			} => root.schema.graph(graph).is_some_and(|definition| {
				definition.edge_endpoint_index().is_some_and(|candidate| {
					candidate.id() == index
						&& candidate.version() == version
						&& root.commit_ts == fingerprint
				}) || definition.indexes().any(|candidate| {
					candidate.id() == Some(index)
						&& candidate.version() == version
						&& candidate.state() == super::ClassIndexState::Ready
						&& candidate.build_revision() == fingerprint
				})
			}),
			MaintenanceTarget::Index {
				class,
				index,
				version,
			} => root.schema.class_by_id(class).is_some_and(|definition| {
				definition.indexes().any(|candidate| {
					candidate.id() == Some(index)
						&& candidate.version() == version
						&& candidate.state() == super::ClassIndexState::Ready
						&& candidate.build_revision() == fingerprint
				})
			}),
			MaintenanceTarget::Graph(graph) => {
				root.commit_ts == fingerprint && root.graph(graph).is_some()
			}
			MaintenanceTarget::BaseClass(class) => {
				root.commit_ts == fingerprint && root.schema.class_by_id(class).is_some()
			}
			MaintenanceTarget::EffectiveClass {
				class,
				graph,
			} => {
				root.commit_ts == fingerprint
					&& root.schema.class_by_id(class).is_some()
					&& root.graph(graph).is_some()
			}
			MaintenanceTarget::Database => root.commit_ts == fingerprint,
		}
	}

	pub(crate) fn maintenance_target_exists(&self, target: MaintenanceTarget) -> bool {
		let root = self.current_root();
		match target {
			MaintenanceTarget::GraphIndex {
				graph,
				index,
				version,
			} => root.schema.graph(graph).is_some_and(|definition| {
				definition.edge_endpoint_index().is_some_and(|candidate| {
					candidate.id() == index && candidate.version() == version
				}) || definition.indexes().any(|candidate| {
					candidate.id() == Some(index) && candidate.version() == version
				})
			}),
			MaintenanceTarget::Database => true,
			MaintenanceTarget::Graph(graph) => root.graph(graph).is_some(),
			MaintenanceTarget::BaseClass(class) => root.schema.class_by_id(class).is_some(),
			MaintenanceTarget::EffectiveClass {
				class,
				graph,
			} => root.schema.class_by_id(class).is_some() && root.graph(graph).is_some(),
			MaintenanceTarget::Index {
				class,
				index,
				version,
			} => root.schema.class_by_id(class).is_some_and(|definition| {
				definition.indexes().any(|candidate| {
					candidate.id() == Some(index) && candidate.version() == version
				})
			}),
		}
	}

	pub(crate) fn resolve_analyze_mode(
		&self,
		target: MaintenanceTarget,
		mode: super::MaintenanceAnalyzeMode,
	) -> FerrosGrynnResult<super::MaintenanceAnalyzeMode> {
		if mode != super::MaintenanceAnalyzeMode::Automatic {
			return Ok(mode);
		}
		let root = self.current_root();
		let visible_hot_pages = match target {
			MaintenanceTarget::Database => root.graphs.values().fold(
				crate::storage::atom_hot_page_count(root.atoms.slot_capacity()),
				|pages, graph| {
					pages.saturating_add(crate::storage::graph_hot_page_count(
						graph.slot_capacity(AtomKind::Node),
						graph.slot_capacity(AtomKind::Edge),
					))
				},
			),
			MaintenanceTarget::BaseClass(class) => {
				if root.schema.class_by_id(class).is_none() {
					return Err(FerrosGrynnError::class_does_not_exist(class.to_string()));
				}
				crate::storage::atom_hot_page_count(root.atoms.slot_capacity())
			}
			MaintenanceTarget::Graph(graph)
			| MaintenanceTarget::EffectiveClass {
				graph,
				..
			} => {
				let graph = root
					.graph(graph)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
				crate::storage::graph_hot_page_count(
					graph.slot_capacity(AtomKind::Node),
					graph.slot_capacity(AtomKind::Edge),
				)
			}
			MaintenanceTarget::Index {
				..
			}
			| MaintenanceTarget::GraphIndex {
				..
			} => {
				return Err(FerrosGrynnError::invalid_argument(
					"ANALYZE does not accept an index maintenance target",
				));
			}
		};
		Ok(if visible_hot_pages <= 32 {
			super::MaintenanceAnalyzeMode::Full
		} else {
			super::MaintenanceAnalyzeMode::Approximate {
				sample_count: 30_000,
			}
		})
	}

	pub(crate) fn run_maintenance_job(
		&self,
		id: super::JobId,
	) -> FerrosGrynnResult<MaintenanceRunOutcome> {
		self.run_maintenance_job_inner(id, || Ok(()))
	}

	#[cfg(test)]
	pub(crate) fn run_maintenance_job_interleaved(
		&self,
		id: super::JobId,
		interleave: impl FnOnce() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<MaintenanceRunOutcome> {
		self.run_maintenance_job_inner(id, interleave)
	}

	fn run_maintenance_job_inner(
		&self,
		id: super::JobId,
		interleave: impl FnOnce() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<MaintenanceRunOutcome> {
		if self.maintenance.is_cancelled(id) {
			return Ok(MaintenanceRunOutcome::Cancelled);
		}
		let job = self
			.maintenance
			.job(id)
			.ok_or_else(|| FerrosGrynnError::maintenance_job_not_found(id.to_string()))?;
		let cancellation = self.maintenance.cancellation_token(id);
		let result = crate::storage::with_external_sort_control(
			job.max_memory_bytes,
			job.max_parallelism,
			cancellation.clone(),
			|| {
				let snapshot = self.current_root();
				let publication = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("maintenance commit clock is exhausted")
				})?;
				let mut next = self.build_maintenance_candidate(
					&snapshot,
					job.kind,
					job.target,
					publication,
					job.analyze_mode,
				)?;
				let metrics = crate::storage::external_sort_metrics();
				self.maintenance.stage_progress(
					id,
					maintenance_source_items(&next, job.target).max(metrics.processed_items),
					metrics.written_bytes,
					metrics.runs,
				)?;
				if self.maintenance.is_cancelled(id) {
					return Ok(MaintenanceRunOutcome::Cancelled);
				}
				interleave()?;
				if self.maintenance.has_pending_index_change_after(job.target, snapshot.commit_ts) {
					return Ok(MaintenanceRunOutcome::Retry);
				}
				if !self.maintenance.mark_publishing(self, id, publication)? {
					return Ok(MaintenanceRunOutcome::Cancelled);
				}
				let _commit = self.commit.lock();
				if self.current_root().commit_ts != snapshot.commit_ts {
					return Ok(MaintenanceRunOutcome::Retry);
				}
				if self.maintenance.is_cancelled(id) {
					return Ok(MaintenanceRunOutcome::Cancelled);
				}
				next.commit_ts = publication;
				self.persist_root(&mut next, None, None, false, false)?;
				*self.current.write() = Arc::new(next);
				self.maintenance.acknowledge_index_changes(job.target, snapshot.commit_ts);
				Ok(MaintenanceRunOutcome::Completed)
			},
		);
		if cancellation.load(std::sync::atomic::Ordering::Acquire) {
			Ok(MaintenanceRunOutcome::Cancelled)
		} else {
			result
		}
	}

	/// Runs the same concrete builder in the caller thread without creating a
	/// durable background job.
	pub(crate) fn run_foreground_maintenance(
		&self,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		options: super::MaintenanceOptions,
		analyze_mode: Option<super::MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<()> {
		crate::storage::with_external_sort_options(
			options.max_memory_bytes,
			options.max_parallelism,
			|| {
				let snapshot = self.current_root();
				let publication = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("maintenance commit clock is exhausted")
				})?;
				let mut next = self.build_maintenance_candidate(
					&snapshot,
					kind,
					target,
					publication,
					analyze_mode,
				)?;
				let _commit = self.commit.lock();
				if self.current_root().commit_ts != snapshot.commit_ts {
					return Err(FerrosGrynnError::maintenance_revision_race(target.display_name()));
				}
				next.commit_ts = publication;
				self.persist_root(&mut next, None, None, false, false)?;
				*self.current.write() = Arc::new(next);
				Ok(())
			},
		)
	}

	fn build_maintenance_candidate(
		&self,
		snapshot: &DatabaseRoot,
		kind: MaintenanceJobKind,
		target: MaintenanceTarget,
		build_revision: u64,
		analyze_mode: Option<super::MaintenanceAnalyzeMode>,
	) -> FerrosGrynnResult<DatabaseRoot> {
		let mut next = snapshot.clone();
		match kind {
			MaintenanceJobKind::BuildIndex
			| MaintenanceJobKind::RebuildIndex
			| MaintenanceJobKind::DeferredIndexCatchUp
			| MaintenanceJobKind::CompactIndex => {
				prepare_index_maintenance(&mut next, target, build_revision)?;
				next.rebuild_indexes()?;
			}
			MaintenanceJobKind::Analyze => {
				self.hydrate_statistics(&mut next)?;
				next.rebuild_statistics_with_mode_at(
					&self.import_scratch_root().join("maintenance"),
					analyze_mode.ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"ANALYZE maintenance is missing its resolved build mode",
						)
					})?,
					target,
				)?;
			}
			MaintenanceJobKind::CompactGraph | MaintenanceJobKind::CompactDatabase => {}
		}
		next.validate_full_import_state()?;
		Ok(next)
	}

	pub(crate) fn query_scratch_root(&self) -> std::path::PathBuf {
		self.query_scratch_root.clone()
	}

	pub(crate) fn capture_physical_query_snapshot(
		&self,
		commit_ts: u64,
	) -> FerrosGrynnResult<Option<LocalQuerySnapshot>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.capture_query_snapshot(commit_ts),
		}
	}

	pub(crate) fn has_physical_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
	) -> bool {
		match &self.backend {
			StoreBackend::InMemory => false,
			StoreBackend::Local(local) => local.has_physical_index(snapshot, index),
		}
	}

	pub(crate) fn physical_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
	) -> FerrosGrynnResult<Option<Arc<PhysicalIndexRead>>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.physical_index(snapshot, index),
		}
	}

	pub(crate) fn search_diskann_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
		query: &crate::values::Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<crate::storage::VectorSearchHit>>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.search_diskann_index(snapshot, index, query, k, breadth)
			}
		}
	}

	pub(crate) fn search_geometry_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
		mbr: [f64; 4],
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Option<crate::storage::GeometryCandidates>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local.search_geometry_index(snapshot, index, mbr, admit),
		}
	}

	pub(crate) fn search_hnsw_index(
		&self,
		snapshot: &LocalQuerySnapshot,
		index: super::IndexId,
		query: &crate::values::Vector,
		k: usize,
		breadth: usize,
		distance_function: &mut dyn FnMut(
			super::FunctionId,
			super::SchemaVersion,
			&crate::values::Vector,
			&crate::values::Vector,
		) -> FerrosGrynnResult<f64>,
	) -> FerrosGrynnResult<Option<Vec<crate::storage::VectorSearchHit>>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.search_hnsw_index(snapshot, index, query, k, breadth, distance_function)
			}
		}
	}

	pub(crate) fn load_planner_statistics(
		&self,
		snapshot: &LocalQuerySnapshot,
		database: &DatabaseRoot,
		demand: &QueryStatisticsDemand,
		statistics: &mut StatisticsStore,
	) {
		if let StoreBackend::Local(local) = &self.backend {
			local.load_planner_statistics(snapshot, database, demand, statistics);
		}
	}

	pub(crate) fn hydrate_statistics(&self, database: &mut DatabaseRoot) -> FerrosGrynnResult<()> {
		if !database.statistics.auxiliary_deferred() {
			return Ok(());
		}
		match &self.backend {
			StoreBackend::InMemory => Err(FerrosGrynnError::invalid_storage_file(
				"in-memory root unexpectedly contains deferred statistics",
			)),
			StoreBackend::Local(local) => local.hydrate_statistics(database),
		}
	}

	#[allow(
		clippy::too_many_arguments,
		reason = "the local frontier contract keeps graph, source, and direction axes explicit"
	)]
	pub(crate) fn physical_traversal_expand(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		source: crate::graph::AtomId,
		forward: bool,
		backward: bool,
		undirected: bool,
	) -> FerrosGrynnResult<Option<PhysicalTraversalRead>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => local
				.physical_traversal_expand(snapshot, graph, source, forward, backward, undirected),
		}
	}

	pub(crate) fn physical_traversal_inner_graph(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		owner: crate::graph::AtomId,
	) -> FerrosGrynnResult<Option<PhysicalTraversalInnerRead>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.physical_traversal_inner_graph(snapshot, graph, owner)
			}
		}
	}

	pub(crate) fn physical_traversal_fields(
		&self,
		snapshot: &LocalQuerySnapshot,
		graph: GraphId,
		owner: crate::graph::AtomId,
		context: &DecodeContext<'_>,
	) -> FerrosGrynnResult<Option<PhysicalTraversalFieldsRead>> {
		match &self.backend {
			StoreBackend::InMemory => Ok(None),
			StoreBackend::Local(local) => {
				local.physical_traversal_fields(snapshot, graph, owner, context)
			}
		}
	}

	pub(crate) fn checkpoint(&self, mode: CheckpointMode) -> FerrosGrynnResult<bool> {
		match &self.backend {
			StoreBackend::InMemory => Ok(false),
			StoreBackend::Local(local) => local.checkpoint(mode),
		}
	}

	pub(crate) fn local_storage_status(&self) -> Option<LocalStorageStatus> {
		match (&self.backend, self.options) {
			(StoreBackend::Local(local), DatastoreOptions::Local(options)) => {
				Some(local.runtime_status(options))
			}
			_ => None,
		}
	}

	#[cfg(feature = "storage-engine-prototype-api")]
	pub(crate) fn clear_resident_for_prototype(&self) -> FerrosGrynnResult<usize> {
		match &self.backend {
			StoreBackend::InMemory => Ok(0),
			StoreBackend::Local(local) => local.clear_resident_for_prototype(),
		}
	}
}

impl Drop for StoreInner {
	fn drop(&mut self) {
		self.maintenance.shutdown();
		if matches!(self.backend, StoreBackend::InMemory) {
			drop(fs::remove_dir_all(&self.query_scratch_root));
		}
	}
}

fn prepare_index_maintenance(
	database: &mut DatabaseRoot,
	target: MaintenanceTarget,
	build_revision: u64,
) -> FerrosGrynnResult<()> {
	if let MaintenanceTarget::GraphIndex {
		graph,
		index,
		version,
	} = target
	{
		let definition = database
			.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		if definition
			.edge_endpoint_index()
			.is_some_and(|candidate| candidate.id() == index && candidate.version() == version)
		{
			return Ok(());
		}
		let mut indexes = definition.indexes().cloned().collect::<Vec<_>>();
		let candidate =
			indexes.iter_mut().find(|candidate| candidate.id() == Some(index)).ok_or_else(
				|| FerrosGrynnError::invalid_argument("graph maintenance index is absent"),
			)?;
		if candidate.version() != version {
			return Err(FerrosGrynnError::invalid_argument(
				"graph maintenance definition changed before build",
			));
		}
		if matches!(candidate.definition(), super::GraphIndexDefinition::Reachability { .. })
			&& !candidate.is_on_demand()
		{
			let artifact = super::ReachabilityArtifact::build(
				database,
				candidate,
				build_revision,
				crate::storage::external_sort_run_budget(),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			let artifacts = Arc::make_mut(&mut database.reachability_artifacts);
			artifacts.remove(&index);
			if let Some(artifact) = artifact {
				artifacts.insert(index, Arc::new(artifact));
			}
		} else if matches!(
			candidate.definition(),
			super::GraphIndexDefinition::Neighbourhood { .. }
		) && !candidate.is_on_demand()
		{
			let artifact = super::NeighbourhoodArtifact::build(
				database,
				candidate,
				build_revision,
				crate::storage::external_sort_run_budget(),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			let artifacts = Arc::make_mut(&mut database.neighbourhood_artifacts);
			artifacts.remove(&index);
			if let Some(artifact) = artifact {
				artifacts.insert(index, Arc::new(artifact));
			}
		} else if matches!(candidate.definition(), super::GraphIndexDefinition::Path {
			algorithm: super::PathIndexAlgorithm::LandmarkSssp(_),
			..
		}) {
			if let super::GraphIndexDefinition::Path {
				cost: super::GraphIndexCost::Function {
					function,
					version,
				},
				..
			} = candidate.definition()
			{
				candidate.dependencies = PureFunctionEvaluator::for_builder(
					database,
					crate::storage::external_sort_run_budget(),
					crate::storage::check_external_sort_cancelled,
				)?
				.dependencies(*function, *version)?;
				candidate.validate()?;
			}
			let artifact = super::LandmarkPathArtifact::build(
				database,
				candidate,
				build_revision,
				crate::storage::external_sort_run_budget(),
				&mut crate::storage::check_external_sort_cancelled,
			)?;
			Arc::make_mut(&mut database.path_artifacts).insert(index, Arc::new(artifact));
		} else if !candidate.is_on_demand() {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"materialized graph index build",
			));
		}
		candidate.state = super::ClassIndexState::Ready;
		candidate.build_revision = build_revision;
		let replacement = definition.replace_indexes(indexes)?;
		database.replace_graph_schema(replacement, build_revision)?;
		return Ok(());
	}
	let MaintenanceTarget::Index {
		class,
		index,
		version,
	} = target
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"index maintenance job has a non-index target",
		));
	};
	let catalog = Arc::make_mut(&mut database.schema);
	let definition = catalog
		.classes
		.get_mut(&class)
		.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class.to_string()))?;
	let definition = Arc::make_mut(definition);
	let candidate =
		definition.indexes.iter_mut().find(|candidate| candidate.id() == Some(index)).ok_or_else(
			|| FerrosGrynnError::invalid_argument("maintenance index target no longer exists"),
		)?;
	if candidate.version() != version {
		return Err(FerrosGrynnError::invalid_argument(
			"maintenance index definition changed before build",
		));
	}
	candidate.state = super::ClassIndexState::Ready;
	candidate.build_revision = build_revision;
	Ok(())
}

fn maintenance_source_items(database: &DatabaseRoot, target: MaintenanceTarget) -> u64 {
	match target {
		MaintenanceTarget::Index {
			class,
			..
		} => database
			.atoms
			.atoms()
			.filter(|atom| atom.class_id() == class)
			.count()
			.try_into()
			.unwrap_or(u64::MAX),
		MaintenanceTarget::Graph(graph)
		| MaintenanceTarget::GraphIndex {
			graph,
			..
		} => database
			.graphs
			.get(&graph)
			.map_or(0, |graph| graph.occurrences().count().try_into().unwrap_or(u64::MAX)),
		MaintenanceTarget::BaseClass(class) => database
			.atoms
			.atoms()
			.filter(|atom| atom.class_id() == class)
			.count()
			.try_into()
			.unwrap_or(u64::MAX),
		MaintenanceTarget::EffectiveClass {
			class,
			graph,
		} => database.graphs.get(&graph).map_or(0, |graph| {
			graph
				.occurrences()
				.filter(|occurrence| {
					database
						.atom(occurrence.global_id())
						.is_some_and(|atom| atom.class_id() == class)
				})
				.count()
				.try_into()
				.unwrap_or(u64::MAX)
		}),
		MaintenanceTarget::Database => {
			let atoms = database.atoms.atoms().count().try_into().unwrap_or(u64::MAX);
			let occurrences = database.graphs.values().fold(0_u64, |total, graph| {
				total.saturating_add(graph.occurrences().count().try_into().unwrap_or(u64::MAX))
			});
			atoms.saturating_add(occurrences)
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

	#[test]
	fn in_memory_has_no_local_storage_status() {
		assert!(crate::datastore::Datastore::in_memory().local_storage_status().is_none());
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
