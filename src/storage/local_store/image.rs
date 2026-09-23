//! Full normalized generation builder used by create, commit, and recovery
//! tests.

use std::collections::BTreeMap;

use super::{
	GraphBinding,
	catalog::encode_database_catalog,
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
	datastore::{DataSpaceId, DatabaseRoot, IndexId},
	gql_status::FerrosGrynnResult,
	storage::{
		atom_store_image::{
			AtomStoreFileSet,
			build_durable_atom_store_image_with_context,
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
		graph_index::{LabelDictionary, LabelKind},
		graph_pages::{BuiltGraphImage, encode_statistics},
		graph_store::{
			GraphFileSet,
			ReverseMembershipFile,
			build_durable_graph_image,
			build_reverse_membership_image,
		},
		graph_superblock::GraphSuperblock,
		page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
		scoped_index_image::{
			ScopedIndexFile,
			ScopedIndexKind,
			build_scoped_index_images,
			validate_scoped_index_images,
		},
		types::{DataFileId, DataFileKind},
	},
	values::{
		Uuid,
		storage::{EncodeContext, StorageValue},
	},
};

/// All pages and exact roots of one not-yet-published generation.
pub(super) struct BuiltGeneration {
	pub(super) file_catalog: FileCatalog,
	pub(super) database_catalog: DatabaseCatalog,
	pub(super) bindings: GenerationBindings,
	pub(super) files: Vec<FileImage>,
	pub(super) atom_superblock: AtomStoreSuperblock,
	pub(super) graph_superblocks: BTreeMap<crate::graph::GraphId, GraphSuperblock>,
}

/// Builds every source image before any filesystem path becomes reachable.
pub(super) fn build_generation(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
) -> FerrosGrynnResult<BuiltGeneration> {
	let generation = Uuid::now_v7().to_string();
	let mut context = value_context(&database.schema, database.dictionary.as_ref().clone())?;
	prime_value_context(database, &mut context)?;

	let atom = build_durable_atom_store_image_with_context(
		&database.atoms,
		&database.schema,
		&mut context,
	)?;
	let reverse = build_reverse_membership_image(database)?;
	let mut graph_images = BTreeMap::new();
	for graph in database.schema.graphs() {
		graph_images
			.insert(graph.id(), build_durable_graph_image(database, graph.id(), &mut context)?);
	}
	let scoped = build_scoped_index_images(database)?;
	validate_scoped_index_images(&scoped, database)?;

	let mut layout = LayoutBuilder::new(next_file_instance_id);
	let atom_prefix = atom_prefix(&generation);
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
	let primary_root = atom.atoms.primary.root;
	let primary = layout.add(
		DataSpaceId::Database,
		DataFileKind::Index,
		DataFileRole::WalBackfilled,
		format!("{atom_prefix}/primary.fgidx"),
		None,
		atom.atoms.primary.pages,
	)?;
	let reverse_root = reverse.root;
	let reverse_file = layout.add(
		DataSpaceId::Database,
		DataFileKind::Index,
		DataFileRole::WalBackfilled,
		format!("{atom_prefix}/memberships.fgidx"),
		None,
		reverse.pages,
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
	add_scoped_indexes(
		&mut layout,
		DataSpaceId::Database,
		&atom_prefix,
		scoped.base,
		&mut physical_indexes,
	)?;

	let mut raw_graphs = Vec::new();
	let mut graph_superblocks = BTreeMap::new();
	for (graph_id, image) in graph_images {
		let prefix = graph_prefix(graph_id, &generation);
		let raw = add_graph(&mut layout, &prefix, image, database)?;
		graph_superblocks.insert(graph_id, raw.superblock);
		if let Some(indexes) = scoped.graphs.get(&graph_id) {
			add_scoped_indexes(
				&mut layout,
				DataSpaceId::Graph(graph_id),
				&prefix,
				indexes.iter().map(clone_scoped_file).collect::<FerrosGrynnResult<Vec<_>>>()?,
				&mut physical_indexes,
			)?;
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

	Ok(BuiltGeneration {
		file_catalog,
		database_catalog,
		bindings,
		files,
		atom_superblock,
		graph_superblocks,
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
	image: BuiltGraphImage,
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
	let membership_root = image.membership.root;
	let membership_file = add_graph_file(
		layout,
		space,
		DataFileKind::Index,
		prefix,
		"membership.fgidx",
		image.membership.pages,
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
		} = kind && physical
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

fn clone_scoped_file(index: &ScopedIndexFile) -> FerrosGrynnResult<ScopedIndexFile> {
	let mut pages = Vec::with_capacity(index.pages.len());
	for source in &index.pages {
		let mut page = AlignedPage::zeroed();
		page.as_bytes_mut().copy_from_slice(source.as_bytes());
		pages.push(page);
	}
	Ok(ScopedIndexFile {
		scope: index.scope,
		relative_path: index.relative_path.clone(),
		kind: index.kind,
		root: index.root,
		pages,
	})
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
	});
	Ok(())
}

fn prime_value_context(
	database: &DatabaseRoot,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
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
