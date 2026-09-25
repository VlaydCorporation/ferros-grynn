//! Full normalized generation builder used by create, commit, and recovery
//! tests.

use std::{collections::BTreeMap, path::Path, time::Duration};

use super::{
	GraphBinding,
	catalog::encode_database_catalog,
	cleanup::UnpublishedGeneration,
	layout::{
		DATABASE_CATALOG_CONTINUATION,
		DATABASE_CATALOG_ROOT,
		FILE_CATALOG_CONTINUATION,
		FILE_CATALOG_ROOT,
		FileImage,
		GenerationBindings,
		LayoutBuilder,
		atom_prefix,
		graph_prefix,
		invalid,
	},
};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot, GlobalAtomId, IndexId},
	gql_status::FerrosGrynnResult,
	storage::{
		atom_store_image::{
			AtomStoreFileSet,
			BuiltAtomStoreImage,
			DurableAtomStoreImage,
			build_durable_atom_store_image_with_context,
			build_import_atom_store_image_with_context,
			value_context,
		},
		atom_store_superblock::AtomStoreSuperblock,
		base_statistics::encode_base_statistics,
		catalog::{
			AtomStoreCatalogBinding,
			CatalogKind,
			DatabaseCatalog,
			FileCatalog,
			PhysicalIndexBinding,
			encode_page_chain,
		},
		file_registry::DataFileRole,
		graph_index::{BuiltPrimaryIndex, LabelDictionary, LabelKind, StreamedIndex},
		graph_pages::{BuiltGraphImage, encode_statistics},
		graph_store::{
			GraphFileSet,
			ReverseMembershipFile,
			build_durable_graph_image,
			build_import_durable_graph_image,
			build_import_reverse_membership_image,
			build_reverse_membership_image,
		},
		graph_superblock::GraphSuperblock,
		page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
		scoped_index_image::{
			ScopedIndexFile,
			ScopedIndexKind,
			StreamedScopedIndexFile,
			build_scoped_index_images,
			validate_scoped_index_images,
			write_streamed_scoped_indexes,
			write_streamed_unique_file,
		},
		types::{DataFileId, DataFileKind},
	},
	values::{
		Uuid,
		storage::{DiskAtomRef, EncodeContext, StorageValue},
	},
};

enum GenerationIndex {
	Buffered(BuiltPrimaryIndex),
	Prewritten(StreamedIndex),
}

impl GenerationIndex {
	const fn root(&self) -> PageNo {
		match self {
			Self::Buffered(index) => index.root,
			Self::Prewritten(index) => index.root,
		}
	}
}

/// All pages and exact roots of one not-yet-published generation.
pub(super) struct BuiltGeneration {
	pub(super) file_catalog: FileCatalog,
	pub(super) database_catalog: DatabaseCatalog,
	pub(super) bindings: GenerationBindings,
	pub(super) files: Vec<FileImage>,
	pub(super) atom_superblock: AtomStoreSuperblock,
	pub(super) graph_superblocks: BTreeMap<crate::graph::GraphId, GraphSuperblock>,
	pub(super) index_timings: BTreeMap<IndexId, Duration>,
}

fn map_atom_primary<Primary>(
	image: DurableAtomStoreImage<Primary>,
	map: impl FnOnce(Primary) -> FerrosGrynnResult<GenerationIndex>,
) -> FerrosGrynnResult<DurableAtomStoreImage<GenerationIndex>> {
	let DurableAtomStoreImage {
		superblock,
		atoms,
		dictionary,
	} = image;
	let BuiltAtomStoreImage {
		atoms_hot,
		properties,
		primary,
	} = atoms;
	Ok(DurableAtomStoreImage {
		superblock,
		atoms: BuiltAtomStoreImage {
			atoms_hot,
			properties,
			primary: map(primary)?,
		},
		dictionary,
	})
}

fn map_graph_membership<Membership>(
	image: BuiltGraphImage<Membership>,
	map: impl FnOnce(Membership) -> FerrosGrynnResult<GenerationIndex>,
) -> FerrosGrynnResult<BuiltGraphImage<GenerationIndex>> {
	let BuiltGraphImage {
		reference,
		superblock,
		nodes,
		edges,
		adjacency,
		cross_level_adjacency,
		overlays,
		subgraphs,
		labels,
		membership,
		arena,
	} = image;
	Ok(BuiltGraphImage {
		reference,
		superblock,
		nodes,
		edges,
		adjacency,
		cross_level_adjacency,
		overlays,
		subgraphs,
		labels,
		membership: map(membership)?,
		arena,
	})
}

/// Builds every source image before any filesystem path becomes reachable.
pub(super) fn build_generation(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
) -> FerrosGrynnResult<BuiltGeneration> {
	let generation = Uuid::now_v7().to_string();
	build_generation_inner(database, next_file_instance_id, &generation, None)
}

/// Builds one import generation while cleanup is armed before any streamed
/// side-file becomes visible in the filesystem.
pub(super) fn build_import_generation(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
	root: &Path,
) -> FerrosGrynnResult<(BuiltGeneration, UnpublishedGeneration)> {
	let generation = Uuid::now_v7().to_string();
	let unpublished = UnpublishedGeneration::for_database(root, database, &generation);
	let built = build_generation_inner(database, next_file_instance_id, &generation, Some(root))?;
	Ok((built, unpublished))
}

fn build_generation_inner(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
	generation: &str,
	stream_root: Option<&Path>,
) -> FerrosGrynnResult<BuiltGeneration> {
	let mut context = value_context(&database.schema, database.dictionary.as_ref().clone())?;
	prime_value_context(database, &mut context)?;

	let atom_prefix = atom_prefix(generation);
	let atom = match stream_root {
		Some(root) => {
			let image = build_import_atom_store_image_with_context(
				&database.atoms,
				&database.schema,
				&mut context,
				root,
			)?;
			map_atom_primary(image, |records| {
				write_streamed_unique_file::<GlobalAtomId>(
					&records,
					root,
					&format!("{atom_prefix}/primary.fgidx"),
				)
				.map(GenerationIndex::Prewritten)
			})?
		}
		None => map_atom_primary(
			build_durable_atom_store_image_with_context(
				&database.atoms,
				&database.schema,
				&mut context,
			)?,
			|index| Ok(GenerationIndex::Buffered(index)),
		)?,
	};
	let reverse = match stream_root {
		Some(root) => {
			let records = build_import_reverse_membership_image(database, root)?;
			GenerationIndex::Prewritten(write_streamed_unique_file::<DiskAtomRef>(
				&records,
				root,
				&format!("{atom_prefix}/memberships.fgidx"),
			)?)
		}
		None => GenerationIndex::Buffered(build_reverse_membership_image(database)?),
	};
	let mut graph_images = BTreeMap::new();
	for graph in database.schema.graphs() {
		let graph_id = graph.id();
		let image = match stream_root {
			Some(root) => {
				let image =
					build_import_durable_graph_image(database, graph_id, &mut context, root)?;
				let prefix = graph_prefix(graph_id, generation);
				map_graph_membership(image, |records| {
					write_streamed_unique_file::<DiskAtomRef>(
						&records,
						root,
						&format!("{prefix}/membership.fgidx"),
					)
					.map(GenerationIndex::Prewritten)
				})?
			}
			None => map_graph_membership(
				build_durable_graph_image(database, graph_id, &mut context)?,
				|index| Ok(GenerationIndex::Buffered(index)),
			)?,
		};
		graph_images.insert(graph_id, image);
	}
	let mut scoped = if stream_root.is_none() {
		let images = build_scoped_index_images(database)?;
		validate_scoped_index_images(&images, database)?;
		Some(images)
	} else {
		None
	};
	let mut streamed = stream_root
		.map(|root| write_streamed_scoped_indexes(database, root, generation))
		.transpose()?;

	let mut layout = LayoutBuilder::new(next_file_instance_id);
	let atom_superblock = atom.superblock;
	add_atom_superblock(&mut layout, &atom_prefix, atom.superblock)?;
	let atoms_hot = layout.add(
		DataSpaceId::Database,
		DataFileKind::AtomsHot,
		DataFileRole::WalBackfilled,
		format!("{atom_prefix}/atoms_hot.fgb"),
		None,
		atom.atoms.atoms_hot,
	)?;
	let properties = layout.add(
		DataSpaceId::Database,
		DataFileKind::Properties,
		DataFileRole::WalBackfilled,
		format!("{atom_prefix}/properties.fgb"),
		None,
		atom.atoms.properties,
	)?;
	layout.add(
		DataSpaceId::Database,
		DataFileKind::PropertyDictionary,
		DataFileRole::WalBackfilled,
		format!("{atom_prefix}/property_keys.fgb"),
		None,
		atom.dictionary.pages,
	)?;
	let primary_root = atom.atoms.primary.root();
	let primary = add_generation_index(
		&mut layout,
		DataSpaceId::Database,
		format!("{atom_prefix}/primary.fgidx"),
		atom.atoms.primary,
	)?;
	let reverse_root = reverse.root();
	let reverse_file = add_generation_index(
		&mut layout,
		DataSpaceId::Database,
		format!("{atom_prefix}/memberships.fgidx"),
		reverse,
	)?;
	add_freelist(
		&mut layout,
		DataSpaceId::Database,
		format!("{atom_prefix}/freelist_atoms.fgb"),
		atoms_hot,
	)?;
	add_freelist(
		&mut layout,
		DataSpaceId::Database,
		format!("{atom_prefix}/freelist_props.fgb"),
		properties,
	)?;
	add_base_statistics(&mut layout, &atom_prefix, database)?;

	let mut physical_indexes = BTreeMap::new();
	if let Some(images) = &mut scoped {
		add_scoped_indexes(
			&mut layout,
			DataSpaceId::Database,
			&atom_prefix,
			std::mem::take(&mut images.base),
			&mut physical_indexes,
		)?;
	}
	if let Some(images) = &mut streamed {
		add_streamed_scoped_indexes(
			&mut layout,
			std::mem::take(&mut images.base),
			&mut physical_indexes,
		)?;
	}

	let mut raw_graphs = Vec::new();
	let mut graph_superblocks = BTreeMap::new();
	for (graph_id, image) in graph_images {
		let prefix = graph_prefix(graph_id, generation);
		let raw = add_graph(&mut layout, &prefix, image, database)?;
		graph_superblocks.insert(graph_id, raw.superblock);
		if let Some(indexes) = scoped.as_mut().and_then(|images| images.graphs.remove(&graph_id)) {
			add_scoped_indexes(
				&mut layout,
				DataSpaceId::Graph(graph_id),
				&prefix,
				indexes,
				&mut physical_indexes,
			)?;
		}
		if let Some(indexes) = streamed.as_mut().and_then(|images| images.graphs.remove(&graph_id))
		{
			add_streamed_scoped_indexes(&mut layout, indexes, &mut physical_indexes)?;
		}
		raw_graphs.push(raw);
	}

	let (file_catalog, mut files) = layout.finish()?;
	let atoms = AtomStoreFileSet::from_catalog(&file_catalog, primary, primary_root)?;
	let reverse = ReverseMembershipFile::from_catalog(&file_catalog, reverse_file, reverse_root)?;
	let graphs = raw_graphs
		.into_iter()
		.map(|raw| {
			Ok(GraphBinding {
				graph_id: raw.graph_id,
				generation: database.commit_ts,
				files: GraphFileSet::from_catalog(
					&file_catalog,
					raw.graph_id,
					raw.membership.file_id,
					raw.membership.root,
				)?,
				membership: raw.membership,
				arena: raw.arena,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let bindings = GenerationBindings {
		atoms,
		reverse,
		graphs,
		physical_indexes,
	};
	let atom_binding = AtomStoreCatalogBinding {
		generation: database.commit_ts,
		primary: PhysicalIndexBinding {
			file_id: primary,
			root: primary_root,
		},
		reverse_membership: PhysicalIndexBinding {
			file_id: reverse_file,
			root: reverse_root,
		},
	};
	let database_catalog = encode_database_catalog(
		database,
		atom_binding,
		&bindings.graphs,
		&bindings.physical_indexes,
		&mut context,
	)?;
	append_catalog_pages(&mut files, &file_catalog, &database_catalog)?;
	let index_timings = streamed.map(|images| images.index_timings).unwrap_or_default();

	Ok(BuiltGeneration {
		file_catalog,
		database_catalog,
		bindings,
		files,
		atom_superblock,
		graph_superblocks,
		index_timings,
	})
}

struct RawGraphBinding {
	graph_id: crate::graph::GraphId,
	membership: PhysicalIndexBinding,
	arena: crate::storage::graph_pages::GraphArenaBindings,
	superblock: GraphSuperblock,
}

fn add_graph(
	layout: &mut LayoutBuilder,
	prefix: &str,
	image: BuiltGraphImage<GenerationIndex>,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<RawGraphBinding> {
	let space = DataSpaceId::Graph(image.reference.id());
	let graph_id = image.reference.id();
	let superblock = image.superblock;
	add_graph_superblock(layout, space, prefix, image.superblock)?;
	let nodes = add_graph_file(
		layout,
		space,
		DataFileKind::NodesHot,
		prefix,
		"nodes_hot.fgb",
		image.nodes,
	)?;
	let edges = add_graph_file(
		layout,
		space,
		DataFileKind::EdgesHot,
		prefix,
		"edges_hot.fgb",
		image.edges,
	)?;
	let adjacency = add_graph_file(
		layout,
		space,
		DataFileKind::AdjacencyOverflow,
		prefix,
		"adjacency_overflow.fgb",
		image.adjacency,
	)?;
	let cross_level = add_graph_file(
		layout,
		space,
		DataFileKind::CrossLevelAdjacencyOverflow,
		prefix,
		"adj_over_cl.fgb",
		image.cross_level_adjacency,
	)?;
	let overlays = add_graph_file(
		layout,
		space,
		DataFileKind::Overlays,
		prefix,
		"overlays.fgb",
		image.overlays,
	)?;
	let subgraphs = add_graph_file(
		layout,
		space,
		DataFileKind::Subgraphs,
		prefix,
		"subgraphs.fgb",
		image.subgraphs,
	)?;
	add_graph_file(layout, space, DataFileKind::Labels, prefix, "labels.fgb", image.labels)?;
	let membership_root = image.membership.root();
	let membership_file = add_generation_index(
		layout,
		space,
		format!("{prefix}/membership.fgidx"),
		image.membership,
	)?;
	for (target, name) in [
		(nodes, "freelist_nodes.fgb"),
		(edges, "freelist_edges.fgb"),
		(adjacency, "freelist_adj.fgb"),
		(cross_level, "freelist_adj_cl.fgb"),
		(overlays, "freelist_overlays.fgb"),
		(subgraphs, "freelist_subgraphs.fgb"),
	] {
		add_freelist(layout, space, format!("{prefix}/{name}"), target)?;
	}
	add_graph_statistics(layout, prefix, graph_id, database)?;
	Ok(RawGraphBinding {
		graph_id,
		membership: PhysicalIndexBinding {
			file_id: membership_file,
			root: membership_root,
		},
		arena: image.arena,
		superblock,
	})
}

fn add_generation_index(
	layout: &mut LayoutBuilder,
	space: DataSpaceId,
	path: String,
	index: GenerationIndex,
) -> FerrosGrynnResult<DataFileId> {
	match index {
		GenerationIndex::Buffered(index) => layout.add(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			path,
			None,
			index.pages,
		),
		GenerationIndex::Prewritten(index) => layout.add_prewritten(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			path,
			index.page_count,
		),
	}
}

fn add_atom_superblock(
	layout: &mut LayoutBuilder,
	prefix: &str,
	superblock: AtomStoreSuperblock,
) -> FerrosGrynnResult<()> {
	layout.add(
		DataSpaceId::Database,
		DataFileKind::AtomStoreSuperblock,
		DataFileRole::Superblock,
		format!("{prefix}/superblock.fgb"),
		None,
		vec![superblock.encode()?],
	)?;
	Ok(())
}

fn add_graph_superblock(
	layout: &mut LayoutBuilder,
	space: DataSpaceId,
	prefix: &str,
	superblock: GraphSuperblock,
) -> FerrosGrynnResult<()> {
	layout.add(
		space,
		DataFileKind::GraphSuperblock,
		DataFileRole::Superblock,
		format!("{prefix}/superblock.fgb"),
		None,
		vec![superblock.encode()?],
	)?;
	Ok(())
}

fn add_freelist(
	layout: &mut LayoutBuilder,
	space: DataSpaceId,
	path: String,
	target: DataFileId,
) -> FerrosGrynnResult<()> {
	layout.add_freelist(space, path, target, freelist_page()?)?;
	Ok(())
}

fn add_graph_file(
	layout: &mut LayoutBuilder,
	space: DataSpaceId,
	kind: DataFileKind,
	prefix: &str,
	name: &str,
	pages: Vec<AlignedPage>,
) -> FerrosGrynnResult<DataFileId> {
	layout.add(space, kind, DataFileRole::WalBackfilled, format!("{prefix}/{name}"), None, pages)
}

fn add_graph_statistics(
	layout: &mut LayoutBuilder,
	prefix: &str,
	graph_id: crate::graph::GraphId,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<()> {
	let graph = database.graph(graph_id).ok_or_else(|| invalid("statistics graph is absent"))?;
	let labels = LabelDictionary::from_labels(graph.occurrences().filter_map(|occurrence| {
		let label = occurrence.label()?;
		let kind = match occurrence.local_id().kind()? {
			crate::graph::AtomKind::Node => LabelKind::Node,
			crate::graph::AtomKind::Edge => LabelKind::Edge,
		};
		Some((kind, label.as_str()))
	}))?;
	let statistics = database
		.statistics
		.graph(graph_id)
		.ok_or_else(|| invalid("graph statistics are absent"))?;
	let encoded = encode_statistics(statistics, &labels, PageLsn::ZERO)?;
	let space = DataSpaceId::Graph(graph_id);
	add_graph_file(
		layout,
		space,
		DataFileKind::Statistics,
		prefix,
		"stats/graph_stats.fgb",
		encoded.graph,
	)?;
	add_graph_file(
		layout,
		space,
		DataFileKind::LabelStatistics,
		prefix,
		"stats/label_stats.fgb",
		encoded.labels,
	)?;
	add_graph_file(
		layout,
		space,
		DataFileKind::PropertyStatistics,
		prefix,
		"stats/prop_stats.fgb",
		encoded.properties,
	)?;
	Ok(())
}

fn add_base_statistics(
	layout: &mut LayoutBuilder,
	prefix: &str,
	database: &DatabaseRoot,
) -> FerrosGrynnResult<()> {
	let encoded = encode_base_statistics(database.statistics.base(), PageLsn::ZERO)?;
	layout.add(
		DataSpaceId::Database,
		DataFileKind::ClassStatistics,
		DataFileRole::WalBackfilled,
		format!("{prefix}/stats/class_stats.fgb"),
		None,
		encoded.classes,
	)?;
	layout.add(
		DataSpaceId::Database,
		DataFileKind::PropertyStatistics,
		DataFileRole::WalBackfilled,
		format!("{prefix}/stats/prop_stats.fgb"),
		None,
		encoded.properties,
	)?;
	Ok(())
}

fn add_scoped_indexes(
	layout: &mut LayoutBuilder,
	space: DataSpaceId,
	prefix: &str,
	indexes: Vec<ScopedIndexFile>,
	physical: &mut BTreeMap<IndexId, PhysicalIndexBinding>,
) -> FerrosGrynnResult<()> {
	for index in indexes {
		let root = index.root;
		let kind = index.kind;
		let file_id = layout.add(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			format!("{prefix}/{}", index.relative_path),
			None,
			index.pages,
		)?;
		if let ScopedIndexKind::Explicit {
			index,
			..
		} = kind
			&& physical
				.insert(index, PhysicalIndexBinding {
					file_id,
					root,
				})
				.is_some()
		{
			return Err(invalid("explicit index has multiple physical generations"));
		}
	}
	Ok(())
}

fn add_streamed_scoped_indexes(
	layout: &mut LayoutBuilder,
	indexes: Vec<StreamedScopedIndexFile>,
	physical: &mut BTreeMap<IndexId, PhysicalIndexBinding>,
) -> FerrosGrynnResult<()> {
	for index in indexes {
		let root = index.root;
		let kind = index.kind;
		let space = match index.scope {
			crate::datastore::IndexScope::Base => DataSpaceId::Database,
			crate::datastore::IndexScope::Effective {
				graph,
			} => DataSpaceId::Graph(graph),
		};
		let file_id = layout.add_prewritten(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			index.path,
			index.page_count,
		)?;
		if let ScopedIndexKind::Explicit {
			index,
			..
		} = kind
			&& physical
				.insert(index, PhysicalIndexBinding {
					file_id,
					root,
				})
				.is_some()
		{
			return Err(invalid("explicit index has multiple physical generations"));
		}
	}
	Ok(())
}

fn append_catalog_pages(
	files: &mut Vec<FileImage>,
	file_catalog: &FileCatalog,
	database_catalog: &DatabaseCatalog,
) -> FerrosGrynnResult<()> {
	let system = file_catalog
		.files
		.iter()
		.find(|entry| entry.file_id.kind() == Some(DataFileKind::SystemCatalog))
		.cloned()
		.ok_or_else(|| invalid("system catalog file is absent"))?;
	let (mut page_numbers, mut pages) = encode_page_chain(
		CatalogKind::Database,
		DATABASE_CATALOG_ROOT,
		DATABASE_CATALOG_CONTINUATION,
		&database_catalog.encode()?,
		PageLsn::ZERO,
	)?;
	let (mut file_page_numbers, mut file_pages) = encode_page_chain(
		CatalogKind::Files,
		FILE_CATALOG_ROOT,
		FILE_CATALOG_CONTINUATION,
		&file_catalog.encode()?,
		PageLsn::ZERO,
	)?;
	page_numbers.append(&mut file_page_numbers);
	pages.append(&mut file_pages);
	files.push(FileImage {
		entry: system,
		page_numbers,
		pages,
		prewritten_page_count: None,
	});
	Ok(())
}

pub(super) fn prime_value_context(
	database: &DatabaseRoot,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
	for graph in database.schema.graphs() {
		for (_key, value) in graph.properties() {
			drop(StorageValue::from_runtime(value.clone(), context)?);
		}
	}
	for parameter in database.schema.parameters() {
		drop(StorageValue::from_runtime(parameter.value().clone(), context)?);
	}
	Ok(())
}

fn freelist_page() -> FerrosGrynnResult<AlignedPage> {
	let mut page =
		AlignedPage::new(PageType::Freelist, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	page.seal()?;
	Ok(page)
}
