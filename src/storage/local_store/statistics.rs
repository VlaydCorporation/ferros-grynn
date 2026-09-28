//! Eager statistics roots and lazy snapshot-exact auxiliary parts.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;

use super::{
	LocalStore,
	layout::invalid,
	physical_traversal::{LocalQuerySnapshot, LocalQuerySnapshotInner, read_snapshot_pages},
};
use crate::{
	datastore::{ClassId, DataSpaceId, DatabaseRoot, QueryStatisticsDemand},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphId},
	stats::{
		DegreeHistogram,
		DegreeHistogramKey,
		LabelStatistic,
		LabelStatisticKey,
		PropertyHistogram,
		PropertyHistogramKey,
		StatisticsStore,
	},
	storage::{
		base_statistics::{
			BaseStatisticsLayout,
			decode_base_class_statistics,
			decode_base_property_statistics,
			decode_base_statistics_root,
		},
		catalog::FileCatalog,
		database_superblock::DatabaseSuperblock,
		file_registry::{FileRegistry, RegisteredDataFile},
		graph_index::{LabelDictionary, LabelKind},
		graph_pages::{
			StatisticsLayout,
			decode_class_statistics,
			decode_degree_statistics,
			decode_label_statistics,
			decode_property_statistics,
			decode_statistics_root,
		},
		hybrid_engine::HybridReadEngine,
		image_io::{read_page_cached, visible_file_page_count},
		page::{AlignedPage, PageNo},
		types::{DataFileId, DataFileKind, PageKey, StorageSnapshot},
		wal::FrameWal,
	},
};

type ClassCounts = BTreeMap<ClassId, u64>;
type LabelCounts = BTreeMap<LabelStatisticKey, LabelStatistic>;
type DegreeHistograms = BTreeMap<DegreeHistogramKey, DegreeHistogram>;
type PropertyHistograms = BTreeMap<PropertyHistogramKey, PropertyHistogram>;

/// One immutable decoded auxiliary part shared by snapshots of one generation.
struct CachedPart<T> {
	value: Mutex<Option<Arc<T>>>,
}

impl<T> Default for CachedPart<T> {
	fn default() -> Self {
		Self {
			value: Mutex::new(None),
		}
	}
}

impl<T> CachedPart<T> {
	/// Decodes this part once. Failed reads remain retryable.
	fn get_or_load(
		&self,
		load: impl FnOnce() -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<Arc<T>> {
		let mut value = self.value.lock();
		if let Some(value) = value.as_ref() {
			return Ok(Arc::clone(value));
		}
		let loaded = Arc::new(load()?);
		*value = Some(Arc::clone(&loaded));
		Ok(loaded)
	}
}

/// Exact immutable file selected by a statistics root.
struct SnapshotFile {
	id: DataFileId,
	file: Arc<RegisteredDataFile>,
	visible_pages: u64,
}

impl SnapshotFile {
	/// Reads one page through the exact WAL/home boundary of the query
	/// snapshot.
	fn read_page(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		space: DataSpaceId,
		page: PageNo,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<()> {
		let mut loaded = read_snapshot_pages(
			store,
			snapshot,
			space,
			self.id,
			&self.file,
			self.visible_pages,
			&[page],
		)?;
		let page =
			loaded.pages.pop().ok_or_else(|| invalid("statistics page read returned no page"))?;
		output.as_bytes_mut().copy_from_slice(page.as_bytes());
		Ok(())
	}
}

/// Database-global statistics auxiliary bindings and caches.
struct PhysicalBaseStatistics {
	layout: BaseStatisticsLayout,
	classes: SnapshotFile,
	properties: SnapshotFile,
	class_cache: CachedPart<ClassCounts>,
	property_cache: CachedPart<PropertyHistograms>,
}

/// Graph-local statistics auxiliary bindings and caches.
struct PhysicalGraphStatistics {
	layout: StatisticsLayout,
	graph: SnapshotFile,
	labels: SnapshotFile,
	properties: SnapshotFile,
	class_cache: CachedPart<ClassCounts>,
	label_cache: CachedPart<LabelCounts>,
	degree_cache: CachedPart<DegreeHistograms>,
	property_cache: CachedPart<PropertyHistograms>,
}

/// Lazy statistics bindings retained with one durable catalog generation.
pub(super) struct PhysicalStatisticsFiles {
	base: PhysicalBaseStatistics,
	graphs: BTreeMap<GraphId, PhysicalGraphStatistics>,
}

/// Reads and validates only eager statistics roots. Auxiliary files remain
/// unopened until a planner or writer requests their exact snapshot version.
pub(super) fn load_statistics_roots(
	wal: &FrameWal,
	registry: &FileRegistry,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	authority: &DatabaseSuperblock,
	files: &FileCatalog,
) -> FerrosGrynnResult<(StatisticsStore, Arc<PhysicalStatisticsFiles>)> {
	let base_class_id = singleton(files, DataSpaceId::Database, DataFileKind::ClassStatistics)?;
	let base_property_id =
		singleton(files, DataSpaceId::Database, DataFileKind::PropertyStatistics)?;
	let base_class_pages =
		visible_file_page_count(wal, registry, snapshot, DataSpaceId::Database, base_class_id)?;
	let base_property_pages =
		visible_file_page_count(wal, registry, snapshot, DataSpaceId::Database, base_property_id)?;
	let mut root = AlignedPage::zeroed();
	read_page_cached(
		wal,
		registry,
		engine,
		snapshot,
		authority.generation,
		authority.system_backfilled_lsn,
		PageKey::new(DataSpaceId::Database, base_class_id, PageNo::new(0)),
		&mut root,
	)?;
	let (base_statistics, base_layout) =
		decode_base_statistics_root(&root, base_class_pages, base_property_pages)?;
	let mut statistics = StatisticsStore::default();
	*statistics.base_mut() = base_statistics;
	let base = PhysicalBaseStatistics {
		layout: base_layout,
		classes: snapshot_file(registry, DataSpaceId::Database, base_class_id, base_class_pages)?,
		properties: snapshot_file(
			registry,
			DataSpaceId::Database,
			base_property_id,
			base_property_pages,
		)?,
		class_cache: CachedPart::default(),
		property_cache: CachedPart::default(),
	};

	let mut graph_files = BTreeMap::new();
	let mut deferred = base_class_pages > 1 || base_property_pages != 0;
	for space in files
		.files
		.iter()
		.map(|entry| entry.space_id)
		.filter(|space| matches!(space, DataSpaceId::Graph(_)))
		.collect::<std::collections::BTreeSet<_>>()
	{
		let graph_file_id = singleton(files, space, DataFileKind::Statistics)?;
		let label_file_id = singleton(files, space, DataFileKind::LabelStatistics)?;
		let property_file_id = singleton(files, space, DataFileKind::PropertyStatistics)?;
		let graph_pages = visible_file_page_count(wal, registry, snapshot, space, graph_file_id)?;
		let label_pages = visible_file_page_count(wal, registry, snapshot, space, label_file_id)?;
		let property_pages =
			visible_file_page_count(wal, registry, snapshot, space, property_file_id)?;
		read_page_cached(
			wal,
			registry,
			engine,
			snapshot,
			authority.generation,
			authority.checkpoint_lsn,
			PageKey::new(space, graph_file_id, PageNo::new(0)),
			&mut root,
		)?;
		let (graph_statistics, layout) =
			decode_statistics_root(&root, graph_pages, label_pages, property_pages)?;
		let DataSpaceId::Graph(graph) = space else {
			unreachable!();
		};
		statistics.replace_graph(graph, graph_statistics);
		deferred |= graph_pages > 1 || label_pages != 0 || property_pages != 0;
		graph_files.insert(graph, PhysicalGraphStatistics {
			layout,
			graph: snapshot_file(registry, space, graph_file_id, graph_pages)?,
			labels: snapshot_file(registry, space, label_file_id, label_pages)?,
			properties: snapshot_file(registry, space, property_file_id, property_pages)?,
			class_cache: CachedPart::default(),
			label_cache: CachedPart::default(),
			degree_cache: CachedPart::default(),
			property_cache: CachedPart::default(),
		});
	}
	if deferred {
		statistics.mark_auxiliary_deferred();
	}
	Ok((
		statistics,
		Arc::new(PhysicalStatisticsFiles {
			base,
			graphs: graph_files,
		}),
	))
}

impl LocalStore {
	/// Adds only planner-demanded auxiliary parts. A corrupt optional part
	/// stays absent so the optimizer chooses its conservative fallback.
	pub(crate) fn load_planner_statistics(
		&self,
		snapshot: &LocalQuerySnapshot,
		database: &DatabaseRoot,
		demand: &QueryStatisticsDemand,
		statistics: &mut StatisticsStore,
	) {
		let Some(files) = snapshot.0.statistics.as_ref() else {
			return;
		};
		files.load_planner(self, &snapshot.0, database, demand, statistics);
	}

	/// Strictly loads every auxiliary part before the first mutation of a root
	/// recovered with root-only statistics.
	pub(crate) fn hydrate_statistics(&self, database: &mut DatabaseRoot) -> FerrosGrynnResult<()> {
		if !database.statistics.auxiliary_deferred() {
			return Ok(());
		}
		let Some(snapshot) = self.capture_query_snapshot(database.commit_ts)? else {
			return database.rebuild_statistics_at(&self.root.join("tmp").join("statistics"));
		};
		let files = snapshot
			.0
			.statistics
			.as_ref()
			.ok_or_else(|| invalid("deferred statistics have no physical snapshot binding"))?;
		let mut complete = database.statistics.as_ref().clone();
		files.load_all(self, &snapshot.0, database, &mut complete)?;
		database.statistics = Arc::new(complete);
		Ok(())
	}
}

impl PhysicalStatisticsFiles {
	/// Loads requested parts independently and ignores only the failed optional
	/// part, leaving planner fallback observable as missing statistics.
	fn load_planner(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		database: &DatabaseRoot,
		demand: &QueryStatisticsDemand,
		output: &mut StatisticsStore,
	) {
		if demand.base_classes
			&& let Ok(values) = self.base_classes(store, snapshot)
		{
			output.base_mut().classes = values.as_ref().clone();
		}
		if demand.base_properties
			&& let Ok(values) = self.base_properties(store, snapshot)
		{
			output.base_mut().property_histograms = values.as_ref().clone();
		}
		for graph in &demand.graph_classes {
			if let (Some(target), Ok(values)) =
				(output.graph_mut(*graph), self.graph_classes(store, snapshot, *graph))
			{
				target.classes = values.as_ref().clone();
			}
		}
		for graph in &demand.graph_properties {
			if let (Some(target), Ok(values)) =
				(output.graph_mut(*graph), self.graph_properties(store, snapshot, *graph))
			{
				target.property_histograms = values.as_ref().clone();
			}
		}
		for graph in demand.graph_labels.union(&demand.graph_degrees) {
			let Ok(dictionary) = label_dictionary(database, *graph) else {
				continue;
			};
			if demand.graph_labels.contains(graph)
				&& let (Some(target), Ok(values)) = (
					output.graph_mut(*graph),
					self.graph_labels(store, snapshot, *graph, &dictionary),
				) {
				target.labels = values.as_ref().clone();
			}
			if demand.graph_degrees.contains(graph)
				&& let (Some(target), Ok(values)) = (
					output.graph_mut(*graph),
					self.graph_degrees(store, snapshot, *graph, &dictionary),
				) {
				target.degree_histograms = values.as_ref().clone();
			}
		}
	}

	/// Strictly decodes all parts into an atomic temporary projection.
	fn load_all(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		database: &DatabaseRoot,
		output: &mut StatisticsStore,
	) -> FerrosGrynnResult<()> {
		output.base_mut().classes = self.base_classes(store, snapshot)?.as_ref().clone();
		output.base_mut().property_histograms =
			self.base_properties(store, snapshot)?.as_ref().clone();
		for graph in self.graphs.keys().copied() {
			let dictionary = label_dictionary(database, graph)?;
			let classes = self.graph_classes(store, snapshot, graph)?;
			let labels = self.graph_labels(store, snapshot, graph, &dictionary)?;
			let degrees = self.graph_degrees(store, snapshot, graph, &dictionary)?;
			let properties = self.graph_properties(store, snapshot, graph)?;
			let target = output
				.graph_mut(graph)
				.ok_or_else(|| invalid("statistics binding references an absent graph root"))?;
			target.classes = classes.as_ref().clone();
			target.labels = labels.as_ref().clone();
			target.degree_histograms = degrees.as_ref().clone();
			target.property_histograms = properties.as_ref().clone();
		}
		output.mark_auxiliary_complete();
		Ok(())
	}

	fn base_classes(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
	) -> FerrosGrynnResult<Arc<ClassCounts>> {
		self.base.class_cache.get_or_load(|| {
			decode_base_class_statistics(
				|page, output| {
					self.base.classes.read_page(
						store,
						snapshot,
						DataSpaceId::Database,
						page,
						output,
					)
				},
				self.base.layout,
			)
		})
	}

	fn base_properties(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
	) -> FerrosGrynnResult<Arc<PropertyHistograms>> {
		self.base.property_cache.get_or_load(|| {
			decode_base_property_statistics(
				|page, output| {
					self.base.properties.read_page(
						store,
						snapshot,
						DataSpaceId::Database,
						page,
						output,
					)
				},
				self.base.layout,
			)
		})
	}

	fn graph_classes(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		graph: GraphId,
	) -> FerrosGrynnResult<Arc<ClassCounts>> {
		let files = self.graph(graph)?;
		files.class_cache.get_or_load(|| {
			decode_class_statistics(
				|page, output| {
					files.graph.read_page(store, snapshot, DataSpaceId::Graph(graph), page, output)
				},
				files.layout,
			)
		})
	}

	fn graph_labels(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		graph: GraphId,
		dictionary: &LabelDictionary,
	) -> FerrosGrynnResult<Arc<LabelCounts>> {
		let files = self.graph(graph)?;
		files.label_cache.get_or_load(|| {
			decode_label_statistics(
				|page, output| {
					files.labels.read_page(store, snapshot, DataSpaceId::Graph(graph), page, output)
				},
				files.layout,
				dictionary,
			)
		})
	}

	fn graph_degrees(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		graph: GraphId,
		dictionary: &LabelDictionary,
	) -> FerrosGrynnResult<Arc<DegreeHistograms>> {
		let files = self.graph(graph)?;
		files.degree_cache.get_or_load(|| {
			decode_degree_statistics(
				|page, output| {
					files.graph.read_page(store, snapshot, DataSpaceId::Graph(graph), page, output)
				},
				files.layout,
				dictionary,
			)
		})
	}

	fn graph_properties(
		&self,
		store: &LocalStore,
		snapshot: &LocalQuerySnapshotInner,
		graph: GraphId,
	) -> FerrosGrynnResult<Arc<PropertyHistograms>> {
		let files = self.graph(graph)?;
		files.property_cache.get_or_load(|| {
			decode_property_statistics(
				|page, output| {
					files.properties.read_page(
						store,
						snapshot,
						DataSpaceId::Graph(graph),
						page,
						output,
					)
				},
				files.layout,
			)
		})
	}

	fn graph(&self, graph: GraphId) -> FerrosGrynnResult<&PhysicalGraphStatistics> {
		self.graphs
			.get(&graph)
			.ok_or_else(|| invalid("statistics demand references an absent graph binding"))
	}
}

fn label_dictionary(database: &DatabaseRoot, graph: GraphId) -> FerrosGrynnResult<LabelDictionary> {
	let graph = database
		.graph(graph)
		.ok_or_else(|| invalid("statistics data space references an absent graph"))?;
	LabelDictionary::from_labels(graph.occurrences().filter_map(|occurrence| {
		let label = occurrence.label()?;
		let kind = match occurrence.local_id().kind()? {
			AtomKind::Node => LabelKind::Node,
			AtomKind::Edge => LabelKind::Edge,
		};
		Some((kind, label.as_str()))
	}))
}

fn snapshot_file(
	registry: &FileRegistry,
	space: DataSpaceId,
	id: DataFileId,
	visible_pages: u64,
) -> FerrosGrynnResult<SnapshotFile> {
	Ok(SnapshotFile {
		id,
		file: registry.resolve_file(space, id)?,
		visible_pages,
	})
}

fn singleton(
	files: &FileCatalog,
	space: DataSpaceId,
	kind: DataFileKind,
) -> FerrosGrynnResult<DataFileId> {
	let mut matches = files
		.files
		.iter()
		.filter(|entry| entry.space_id == space && entry.file_id.kind() == Some(kind));
	let file = matches
		.next()
		.ok_or_else(|| invalid(format!("{space:?} has no {kind:?} statistics file")))?;
	if matches.next().is_some() {
		return Err(invalid(format!("{space:?} has duplicate {kind:?} statistics files")));
	}
	Ok(file.file_id)
}
