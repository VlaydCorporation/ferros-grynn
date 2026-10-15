//! Full normalized generation builder used by create, commit, and recovery
//! tests.

use std::{collections::BTreeMap, path::Path, time::Duration};

use super::{
	GraphBinding,
	catalog::encode_database_catalog,
	cleanup::UnpublishedGeneration,
	index_delta::{IndexReusePlan, RetainedDelta},
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
	datastore::{
		AnalyzedToken,
		DataSpaceId,
		DatabaseRoot,
		FunctionId,
		GlobalAtomId,
		IndexId,
		SchemaAnalyzer,
		SchemaVersion,
	},
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
			CatalogPath,
			DatabaseCatalog,
			FileCatalog,
			FileCatalogEntry,
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
			StreamedScopedIndexImages,
			build_fulltext_index_images,
			build_geometry_index_images,
			build_scoped_index_images,
			build_vector_index_images,
			validate_scoped_index_images,
			write_streamed_fulltext_index_images,
			write_streamed_geometry_index_images,
			write_streamed_scoped_indexes,
			write_streamed_unique_file,
			write_streamed_vector_index_images,
		},
		types::{DataFileId, DataFileKind},
	},
	values::{
		Uuid,
		Vector,
		storage::{DiskAtomRef, EncodeContext, StorageValue},
	},
};

/// Generation index alternatives.
enum GenerationIndex {
	/// Buffered case of generation index.
	Buffered(
		/// Buffered case of generation index.
		BuiltPrimaryIndex,
	),
	/// Prewritten case of generation index.
	Prewritten(
		/// Prewritten case of generation index.
		StreamedIndex,
	),
}

impl GenerationIndex {
	/// Returns the root result.
	const fn root(&self) -> PageNo {
		match self {
			Self::Buffered(index) => index.root,
			Self::Prewritten(index) => index.root,
		}
	}
}

/// All pages and exact roots of one not-yet-published generation.
pub(super) struct BuiltGeneration {
	/// Removes unpublished mapping files unless publication retains them.
	pub(super) mapping_cleanup: Option<UnpublishedGeneration>,
	/// File catalog associated with this built generation.
	pub(super) file_catalog: FileCatalog,
	/// Database catalog associated with this built generation.
	pub(super) database_catalog: DatabaseCatalog,
	/// Bindings associated with this built generation.
	pub(super) bindings: GenerationBindings,
	/// Files associated with this built generation.
	pub(super) files: Vec<FileImage>,
	/// Atom superblock associated with this built generation.
	pub(super) atom_superblock: AtomStoreSuperblock,
	/// Graph superblocks associated with this built generation.
	pub(super) graph_superblocks: BTreeMap<crate::graph::GraphId, GraphSuperblock>,
	/// Index timings associated with this built generation.
	pub(super) index_timings: BTreeMap<IndexId, Duration>,
}

impl BuiltGeneration {
	/// Carries immutable quarantine registrations across a full-generation
	/// rewrite and optionally appends the file published by this import.
	pub(super) fn update_quarantines(
		&mut self,
		previous: &FileCatalog,
		addition: Option<Uuid>,
		removal: Option<Uuid>,
	) -> FerrosGrynnResult<()> {
		self.file_catalog.files.extend(
			previous
				.files
				.iter()
				.filter(|entry| {
					entry.file_id.kind() == Some(DataFileKind::ImportQuarantine)
						&& removal.is_none_or(|id| {
							entry.path.as_str() != format!("admin/quarantine/{id}.fgq")
						})
				})
				.cloned(),
		);
		if let Some(id) = addition {
			let file_id = DataFileId::new(
				self.file_catalog.next_file_instance_id,
				DataFileKind::ImportQuarantine,
			)
			.ok_or_else(|| invalid("quarantine file identity clock is exhausted"))?;
			self.file_catalog.next_file_instance_id = self
				.file_catalog
				.next_file_instance_id
				.checked_add(1)
				.ok_or_else(|| invalid("quarantine file identity clock is exhausted"))?;
			self.file_catalog.files.push(FileCatalogEntry {
				space_id: DataSpaceId::Database,
				file_id,
				companion_of: None,
				role: DataFileRole::AuxiliaryImmutable,
				path: CatalogPath::parse(&format!("admin/quarantine/{id}.fgq"))?,
			});
		}
		self.refresh_catalogs()
	}

	/// Replaces both staged catalog chains after adding retained side files.
	pub(super) fn refresh_catalogs(&mut self) -> FerrosGrynnResult<()> {
		self.file_catalog.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
		self.file_catalog.encode()?;
		self.files.retain(|image| image.entry.file_id.kind() != Some(DataFileKind::SystemCatalog));
		append_catalog_pages(&mut self.files, &self.file_catalog, &self.database_catalog)
	}
}

/// Returns the map atom primary result.
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

/// Returns the map graph membership result.
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
	let mut analyze_function = |_analyzer: &SchemaAnalyzer, _input: &str| {
		Err(invalid("function analyzer requires the datastore evaluator"))
	};
	let mut distance_function =
		|function: FunctionId, _version: SchemaVersion, _left: &Vector, _right: &Vector| {
			Err(invalid(format!(
				"custom vector distance function {function} requires the datastore evaluator"
			)))
		};
	build_generation_with_reuse(
		database,
		next_file_instance_id,
		IndexReusePlan::default(),
		&mut analyze_function,
		&mut distance_function,
	)
}

/// Builds one ordinary generation while retaining compatible immutable index
/// bases and publishing only their exact row deltas.
pub(super) fn build_generation_with_reuse(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
	reuse: IndexReusePlan,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	distance_function: &mut dyn FnMut(
		FunctionId,
		SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<BuiltGeneration> {
	let generation = Uuid::now_v7().to_string();
	build_generation_inner(
		database,
		next_file_instance_id,
		&generation,
		None,
		false,
		reuse,
		analyze_function,
		distance_function,
	)
}

/// Builds an ordinary WAL-backed generation while streaming immutable
/// specialized index bases directly into its unreachable directory.
pub(super) fn build_streamed_generation_with_reuse(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
	root: &Path,
	reuse: IndexReusePlan,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	distance_function: &mut dyn FnMut(
		FunctionId,
		SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<(BuiltGeneration, UnpublishedGeneration)> {
	let generation = Uuid::now_v7().to_string();
	let unpublished = UnpublishedGeneration::for_database(root, database, &generation);
	let built = build_generation_inner(
		database,
		next_file_instance_id,
		&generation,
		Some(root),
		false,
		reuse,
		analyze_function,
		distance_function,
	)?;
	Ok((built, unpublished))
}

/// Builds one import generation while cleanup is armed before any streamed
/// side-file becomes visible in the filesystem.
pub(super) fn build_import_generation(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
	root: &Path,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	distance_function: &mut dyn FnMut(
		FunctionId,
		SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<(BuiltGeneration, UnpublishedGeneration)> {
	let generation = Uuid::now_v7().to_string();
	let unpublished = UnpublishedGeneration::for_database(root, database, &generation);
	let built = build_generation_inner(
		database,
		next_file_instance_id,
		&generation,
		Some(root),
		true,
		IndexReusePlan::default(),
		analyze_function,
		distance_function,
	)?;
	Ok((built, unpublished))
}

/// Builds generation inner.
fn build_generation_inner(
	database: &DatabaseRoot,
	next_file_instance_id: u64,
	generation: &str,
	stream_root: Option<&Path>,
	import: bool,
	reuse: IndexReusePlan,
	analyze_function: &mut dyn FnMut(
		&SchemaAnalyzer,
		&str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>>,
	distance_function: &mut dyn FnMut(
		FunctionId,
		SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<BuiltGeneration> {
	if import && stream_root.is_none() {
		return Err(invalid("import generation has no streaming root"));
	}
	let import_root = if import {
		stream_root
	} else {
		None
	};
	let mut context = value_context(&database.schema, database.dictionary.as_ref().clone())?;
	prime_value_context(database, &mut context)?;

	let atom_prefix = atom_prefix(generation);
	let atom = match import_root {
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
	let reverse = match import_root {
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
	for graph in database.schema.all_graphs() {
		let graph_id = graph.id();
		let image = match import_root {
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
	let mut scoped = if import_root.is_none() {
		let images = build_scoped_index_images(database)?;
		validate_scoped_index_images(&images, database)?;
		Some(images)
	} else {
		None
	};
	let mut streamed = match import_root {
		Some(root) => Some(write_streamed_scoped_indexes(database, root, generation)?),
		None if stream_root.is_some() => Some(StreamedScopedIndexImages::default()),
		None => None,
	};
	let skipped = reuse.skipped_builds();
	let mut fulltext = match stream_root {
		Some(root) => {
			let images = write_streamed_fulltext_index_images(
				database,
				&skipped,
				root,
				generation,
				analyze_function,
			)?;
			merge_streamed_images(
				streamed.as_mut().ok_or_else(|| invalid("streamed index accumulator is absent"))?,
				images,
			)?;
			None
		}
		None => Some(build_fulltext_index_images(database, &skipped, analyze_function)?),
	};
	let mut vectors = match stream_root {
		Some(root) => {
			let images = write_streamed_vector_index_images(
				database,
				&skipped,
				root,
				generation,
				distance_function,
			)?;
			merge_streamed_images(
				streamed.as_mut().ok_or_else(|| invalid("streamed index accumulator is absent"))?,
				images,
			)?;
			None
		}
		None => Some(build_vector_index_images(database, &skipped, distance_function)?),
	};
	let mut geometry = match stream_root {
		Some(root) => {
			let images =
				write_streamed_geometry_index_images(database, &skipped, root, generation)?;
			merge_streamed_images(
				streamed.as_mut().ok_or_else(|| invalid("streamed index accumulator is absent"))?,
				images,
			)?;
			None
		}
		None => Some(build_geometry_index_images(database, &skipped)?),
	};

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
	let mut physical_deltas = BTreeMap::new();
	add_reused_indexes(
		&mut layout,
		generation,
		reuse,
		&mut physical_indexes,
		&mut physical_deltas,
	)?;
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
	if let Some(images) = &mut fulltext {
		add_scoped_indexes(
			&mut layout,
			DataSpaceId::Database,
			&atom_prefix,
			std::mem::take(&mut images.base),
			&mut physical_indexes,
		)?;
	}
	if let Some(images) = &mut vectors {
		add_scoped_indexes(
			&mut layout,
			DataSpaceId::Database,
			&atom_prefix,
			std::mem::take(&mut images.base),
			&mut physical_indexes,
		)?;
	}
	if let Some(images) = &mut geometry {
		add_scoped_indexes(
			&mut layout,
			DataSpaceId::Database,
			&atom_prefix,
			std::mem::take(&mut images.base),
			&mut physical_indexes,
		)?;
	}

	let mut raw_graphs = Vec::new();
	let mut graph_superblocks = BTreeMap::new();
	for (graph_id, image) in graph_images {
		let prefix = graph_prefix(graph_id, generation);
		let mut raw = add_graph(&mut layout, &prefix, image, database)?;
		raw.edge_endpoint =
			add_edge_endpoint(&mut layout, &prefix, database, graph_id, &raw.arena, stream_root)?;
		add_graph_artifacts(
			&mut layout,
			&prefix,
			database,
			graph_id,
			stream_root,
			&mut physical_indexes,
		)?;
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
		if let Some(indexes) = fulltext.as_mut().and_then(|images| images.graphs.remove(&graph_id))
		{
			add_scoped_indexes(
				&mut layout,
				DataSpaceId::Graph(graph_id),
				&prefix,
				indexes,
				&mut physical_indexes,
			)?;
		}
		if let Some(indexes) = vectors.as_mut().and_then(|images| images.graphs.remove(&graph_id)) {
			add_scoped_indexes(
				&mut layout,
				DataSpaceId::Graph(graph_id),
				&prefix,
				indexes,
				&mut physical_indexes,
			)?;
		}
		if let Some(indexes) = geometry.as_mut().and_then(|images| images.graphs.remove(&graph_id))
		{
			add_scoped_indexes(
				&mut layout,
				DataSpaceId::Graph(graph_id),
				&prefix,
				indexes,
				&mut physical_indexes,
			)?;
		}
		raw_graphs.push(raw);
	}

	let (bridge_binding, mapping_cleanup) =
		super::bridge::prepare_layout(database, &mut layout, stream_root)?;
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
				edge_endpoint: raw.edge_endpoint,
				arena: raw.arena,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let bindings = GenerationBindings {
		atoms,
		reverse,
		graphs,
		physical_indexes,
		physical_deltas,
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
	let mut database_catalog = encode_database_catalog(
		database,
		atom_binding,
		&bindings.graphs,
		&bindings.physical_indexes,
		&bindings.physical_deltas,
		&mut context,
	)?;
	database_catalog.bridges = bridge_binding;
	append_catalog_pages(&mut files, &file_catalog, &database_catalog)?;
	let index_timings = streamed.map(|images| images.index_timings).unwrap_or_default();

	Ok(BuiltGeneration {
		mapping_cleanup,
		file_catalog,
		database_catalog,
		bindings,
		files,
		atom_superblock,
		graph_superblocks,
		index_timings,
	})
}

/// Raw graph binding state.
struct RawGraphBinding {
	/// Identifier of the graph owning this state.
	graph_id: crate::graph::GraphId,
	/// Membership associated with this raw graph binding.
	membership: PhysicalIndexBinding,
	/// Edge endpoint associated with this raw graph binding.
	edge_endpoint: Option<PhysicalIndexBinding>,
	/// Arena associated with this raw graph binding.
	arena: crate::storage::graph_pages::GraphArenaBindings,
	/// Superblock associated with this raw graph binding.
	superblock: GraphSuperblock,
}

/// Returns the add graph result.
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
		edge_endpoint: None,
		arena: image.arena,
		superblock,
	})
}

/// Publishes synchronous endpoint postings in the same generation as topology.
fn add_edge_endpoint(
	layout: &mut LayoutBuilder,
	prefix: &str,
	database: &DatabaseRoot,
	graph: crate::graph::GraphId,
	arena: &crate::storage::graph_pages::GraphArenaBindings,
	stream_root: Option<&Path>,
) -> FerrosGrynnResult<Option<PhysicalIndexBinding>> {
	let Some(capability) =
		database.schema.graph(graph).and_then(|schema| schema.edge_endpoint_index())
	else {
		return Ok(None);
	};
	let temporary = std::env::temp_dir();
	let scratch = stream_root.unwrap_or(&temporary);
	let (records, posting_count) =
		crate::storage::edge_endpoint_index::prepare(database, graph, arena, scratch)?;
	let header = crate::storage::edge_endpoint_index::EndpointHeader {
		graph,
		capability,
		revision: database.commit_ts,
		root: PageNo::NULL,
		page_count: 0,
		posting_count,
	};
	let path = format!("{prefix}/idx/ee_endpoint.fgidx");
	let space = DataSpaceId::Graph(graph);
	let file_id = if let Some(root) = stream_root {
		let built = crate::storage::scoped_index_image::write_streamed_file(root, &path, |file| {
			crate::storage::edge_endpoint_index::stream(
				&records,
				header,
				scratch,
				|number, page| file.write_page(number, page),
			)
		})?;
		layout.add_prewritten(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			path,
			built.page_count,
		)?
	} else {
		let pages = crate::storage::edge_endpoint_index::buffered(&records, header, scratch)?;
		layout.add(space, DataFileKind::Index, DataFileRole::WalBackfilled, path, None, pages)?
	};
	Ok(Some(PhysicalIndexBinding {
		file_id,
		root: PageNo::new(0),
	}))
}

/// Publishes complete graph artifacts through the existing generation writer.
fn add_graph_artifacts(
	layout: &mut LayoutBuilder,
	prefix: &str,
	database: &DatabaseRoot,
	graph: crate::graph::GraphId,
	stream_root: Option<&Path>,
	physical: &mut BTreeMap<IndexId, PhysicalIndexBinding>,
) -> FerrosGrynnResult<()> {
	let schema =
		database.schema.graph(graph).ok_or_else(|| invalid("artifact graph schema is absent"))?;
	for (ordinal, index) in schema.indexes().enumerate() {
		if index.state() != crate::datastore::ClassIndexState::Ready {
			continue;
		}
		let id = index.id().ok_or_else(|| invalid("graph artifact definition has no identity"))?;
		let reachability = database.reachability_artifacts.get(&id);
		let neighbourhood = database.neighbourhood_artifacts.get(&id);
		let path_artifact = database.path_artifacts.get(&id);
		let tensor_artifact = database.tensor_artifacts.get(&id);
		let (identity, page_count, descriptor, family) =
			match (reachability, neighbourhood, path_artifact, tensor_artifact) {
				(Some(artifact), None, None, None) => (
					artifact.layout.header.identity,
					artifact.layout.page_count,
					&artifact.descriptor,
					"reach",
				),
				(None, Some(artifact), None, None) => (
					artifact.layout.header.identity,
					artifact.layout.page_count,
					&artifact.descriptor,
					"neighbour",
				),
				(None, None, Some(artifact), None) => (
					artifact.layout.header.identity,
					artifact.layout.page_count,
					&artifact.descriptor,
					"path",
				),
				(None, None, None, Some(artifact)) => (
					artifact.layout.header.identity,
					artifact.layout.page_count,
					&artifact.descriptor,
					"tensor",
				),
				(None, None, None, None) => continue,
				_ => {
					return Err(invalid("graph definition has multiple artifact families"));
				}
			};
		if identity.version != index.version().get()
			|| identity.revision != index.build_revision()
			|| *descriptor != index.descriptor()?
		{
			return Err(invalid("graph publication has an incompatible artifact"));
		}
		if physical.contains_key(&id) {
			continue;
		}
		let path = format!("{prefix}/idx/{family}_{ordinal:04}.fgidx");
		let file_kind = if tensor_artifact.is_some() {
			DataFileKind::TensorCache
		} else {
			DataFileKind::Index
		};
		let space = DataSpaceId::Graph(graph);
		let stream = |write: &mut dyn FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>| {
			if let Some(artifact) = reachability {
				artifact.layout.stream(
					|kind, ordinal, output| artifact.encode_element(kind, ordinal, output),
					write,
					&mut crate::storage::check_external_sort_cancelled,
				)
			} else if let Some(artifact) = neighbourhood {
				artifact.layout.stream(
					|kind, ordinal, output| artifact.encode_element(kind, ordinal, output),
					write,
					&mut crate::storage::check_external_sort_cancelled,
				)
			} else if let Some(artifact) = path_artifact {
				artifact.layout.stream(
					|kind, ordinal, output| artifact.encode_element(kind, ordinal, output),
					write,
					&mut crate::storage::check_external_sort_cancelled,
				)
			} else if let Some(artifact) = tensor_artifact {
				artifact.layout.stream(
					|kind, ordinal, output| artifact.encode_element(kind, ordinal, output),
					write,
					&mut crate::storage::check_external_sort_cancelled,
				)
			} else {
				Err(invalid("graph artifact disappeared before page streaming"))
			}
		};
		let file_id = if let Some(root) = stream_root {
			let built =
				crate::storage::scoped_index_image::write_streamed_file(root, &path, |file| {
					stream(&mut |at, page| file.write_page(at, page))?;
					Ok(StreamedIndex {
						root: PageNo::new(0),
						page_count,
					})
				})?;
			layout.add_prewritten(
				space,
				file_kind,
				DataFileRole::WalBackfilled,
				path,
				built.page_count,
			)?
		} else {
			if let Some(artifact) = reachability {
				artifact.layout.admit_decode(crate::storage::external_sort_run_budget())?;
			} else if let Some(artifact) = neighbourhood {
				artifact.layout.admit_decode(crate::storage::external_sort_run_budget())?;
			} else if let Some(artifact) = path_artifact {
				artifact.layout.admit_decode(crate::storage::external_sort_run_budget())?;
			} else if let Some(artifact) = tensor_artifact {
				crate::storage::derived_index::ArraySection::admit_decode(
					&artifact.layout.sections,
					artifact.layout.header.rows,
					artifact.layout.header.connectors,
					crate::storage::external_sort_run_budget(),
				)?;
			}
			let count = usize::try_from(page_count).map_err(|cause| {
				invalid("graph artifact image page extent exceeds platform")
					.with_technical_source(cause)
			})?;
			if count
				.checked_mul(crate::storage::page::PAGE_SIZE)
				.is_none_or(|bytes| bytes > crate::storage::external_sort_run_budget())
			{
				return Err(invalid("graph artifact buffered pages exceed scratch cap"));
			}
			let mut pages = Vec::new();
			pages.try_reserve_exact(count).map_err(|cause| {
				invalid("graph artifact page allocation failed").with_technical_source(cause)
			})?;
			pages.resize_with(count, AlignedPage::zeroed);
			stream(&mut |at, page| {
				pages[at.get() as usize].as_bytes_mut().copy_from_slice(page.as_bytes());
				Ok(())
			})?;
			layout.add(space, file_kind, DataFileRole::WalBackfilled, path, None, pages)?
		};
		physical.insert(id, PhysicalIndexBinding {
			file_id,
			root: PageNo::new(0),
		});
	}
	Ok(())
}

/// Returns the add generation index result.
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

/// Returns the add atom superblock result.
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

/// Returns the add graph superblock result.
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

/// Returns the add freelist result.
fn add_freelist(
	layout: &mut LayoutBuilder,
	space: DataSpaceId,
	path: String,
	target: DataFileId,
) -> FerrosGrynnResult<()> {
	layout.add_freelist(space, path, target, freelist_page()?)?;
	Ok(())
}

/// Returns the add graph file result.
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

/// Returns the add graph statistics result.
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

/// Returns the add base statistics result.
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

/// Returns the add scoped indexes result.
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

/// Returns the add reused indexes result.
fn add_reused_indexes(
	layout: &mut LayoutBuilder,
	generation: &str,
	reuse: IndexReusePlan,
	physical: &mut BTreeMap<IndexId, PhysicalIndexBinding>,
	deltas: &mut BTreeMap<IndexId, PhysicalIndexBinding>,
) -> FerrosGrynnResult<()> {
	let mut ordinals = BTreeMap::<(crate::datastore::IndexScope, u8), u32>::new();
	for (index, retained) in reuse.indexes {
		layout.retain(retained.base_entry)?;
		if physical.insert(index, retained.base).is_some() {
			return Err(invalid("retained index has multiple immutable bases"));
		}
		let binding = match retained.delta {
			RetainedDelta::None => continue,
			RetainedDelta::Existing {
				binding,
				entry,
			} => {
				layout.retain(entry)?;
				binding
			}
			RetainedDelta::Replacement(pages) => {
				let kind = match retained.kind {
					crate::datastore::ClassIndexKind::Fulltext => (0, "ft_delta"),
					crate::datastore::ClassIndexKind::Hnsw => (1, "hnsw_delta"),
					crate::datastore::ClassIndexKind::DiskAnn => (2, "diskann_delta"),
					crate::datastore::ClassIndexKind::Geometry => (3, "geo_delta"),
					_ => return Err(invalid("non-specialized index reached delta publication")),
				};
				let ordinal = ordinals.entry((retained.scope, kind.0)).or_default();
				let current = *ordinal;
				*ordinal = ordinal
					.checked_add(1)
					.ok_or_else(|| invalid("index delta ordinal exceeds u32"))?;
				let (space, prefix) = match retained.scope {
					crate::datastore::IndexScope::Base => {
						(DataSpaceId::Database, atom_prefix(generation))
					}
					crate::datastore::IndexScope::Effective {
						graph,
					} => (DataSpaceId::Graph(graph), graph_prefix(graph, generation)),
				};
				let file_id = layout.add(
					space,
					DataFileKind::Index,
					DataFileRole::WalBackfilled,
					format!("{prefix}/idx/{}_{current:04}.fgidx", kind.1),
					None,
					pages,
				)?;
				PhysicalIndexBinding {
					file_id,
					root: PageNo::new(0),
				}
			}
		};
		if deltas.insert(index, binding).is_some() {
			return Err(invalid("retained index has multiple exact deltas"));
		}
	}
	for (index, (binding, entry)) in reuse.graph_artifacts {
		layout.retain(entry)?;
		if physical.insert(index, binding).is_some() {
			return Err(invalid("graph artifact has duplicate retained bindings"));
		}
	}
	Ok(())
}

/// Returns the add streamed scoped indexes result.
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

/// Merges streamed images.
fn merge_streamed_images(
	target: &mut StreamedScopedIndexImages,
	mut source: StreamedScopedIndexImages,
) -> FerrosGrynnResult<()> {
	target.base.append(&mut source.base);
	for (graph, mut files) in source.graphs {
		target.graphs.entry(graph).or_default().append(&mut files);
	}
	for (index, elapsed) in source.index_timings {
		if target.index_timings.insert(index, elapsed).is_some() {
			return Err(invalid("streamed index timing contains a duplicate IndexId"));
		}
	}
	Ok(())
}

/// Appends catalog pages.
pub(super) fn append_catalog_pages(
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

/// Returns the prime value context result.
pub(super) fn prime_value_context(
	database: &DatabaseRoot,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
	for graph in database.schema.all_graphs() {
		for (_key, value) in graph.properties() {
			drop(StorageValue::from_runtime(value.clone(), context)?);
		}
	}
	for parameter in database.schema.parameters() {
		drop(StorageValue::from_runtime(parameter.value().clone(), context)?);
	}
	for analyzer in database.schema.analyzers() {
		for filter in analyzer.filters() {
			let crate::datastore::AnalyzerFilter::Mapper(entries) = filter else {
				continue;
			};
			let value = crate::values::Object::from_iter(entries.iter().map(|(source, target)| {
				(
					source.clone(),
					target.clone().map_or(crate::values::Value::Null, crate::values::Value::String),
				)
			}));
			drop(StorageValue::from_runtime(crate::values::Value::Object(value), context)?);
		}
	}
	Ok(())
}

/// Returns the freelist page result.
fn freelist_page() -> FerrosGrynnResult<AlignedPage> {
	let mut page =
		AlignedPage::new(PageType::Freelist, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	page.seal()?;
	Ok(page)
}
