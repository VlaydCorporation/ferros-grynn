use std::{
	fs,
	io,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
};

use crate::{
	datastore::{
		ClassId,
		ClassSchema,
		DataSpaceId,
		DatabaseRoot,
		GraphSchema,
		IndexId,
		SchemaVersion,
		SystemIndex,
		SystemIndexKind,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, EdgeTopology, GraphId, GraphKind, GraphRef, GraphStorageMode},
	storage::{
		atom_store_image::{
			AtomStoreFileSet,
			DurableAtomStoreImage,
			build_durable_atom_store_image,
			value_context,
		},
		catalog::{CatalogPath, FileCatalog, FileCatalogEntry},
		direct::DirectFile,
		file_registry::DataFileRole,
		graph_index::BuiltPrimaryIndex,
		graph_pages::BuiltGraphImage,
		graph_store::{
			GraphFileSet,
			ReverseMembershipFile,
			build_durable_graph_image,
			build_reverse_membership_image,
		},
		page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
		types::{DataFileId, DataFileKind},
	},
	values::{Number, PropertyMap, RecordId, RecordIdPart, Value},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Self-cleaning directory for one Direct-I/O integration test.
pub(super) struct TestDirectory(PathBuf);

impl TestDirectory {
	pub(super) fn new() -> io::Result<Self> {
		let suffix = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
		Ok(Self(
			std::env::current_dir()?
				.join(format!(".ferros-grynn-graph-store-{}-{suffix}", std::process::id())),
		))
	}

	pub(super) fn as_path(&self) -> &Path {
		&self.0
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

/// Runtime state and identities used by durable round-trip checks.
pub(super) struct Fixture {
	pub(super) database: DatabaseRoot,
	pub(super) graph_id: GraphId,
	pub(super) reference: GraphRef,
	pub(super) left: crate::datastore::GlobalAtomId,
	pub(super) left_local: crate::graph::AtomId,
}

impl Fixture {
	pub(super) fn build() -> FerrosGrynnResult<Self> {
		let mut database = DatabaseRoot::default();
		let person = install_class(&mut database, 1, "person", AtomKind::Node)?;
		let relation = install_class(&mut database, 2, "knows", AtomKind::Edge)?;
		let graph_id = GraphId::new(7).ok_or_else(|| invalid("test GraphId is invalid"))?;
		let reference =
			GraphRef::new(graph_id, "social", GraphKind::Graph, GraphStorageMode::DiskBacked)?;
		let schema = GraphSchema::new(
			SchemaVersion::INITIAL,
			reference.clone(),
			SystemIndex::new(
				IndexId::new(3).ok_or_else(|| invalid("test label index ID is invalid"))?,
				SystemIndexKind::Label,
			),
			SystemIndex::new(
				IndexId::new(4).ok_or_else(|| invalid("test adjacency index ID is invalid"))?,
				SystemIndexKind::Adjacency,
			),
			1,
		);
		database.install_graph(schema, 1)?;
		let name = Arc::make_mut(&mut database.dictionary).intern("name")?;
		let mut properties = PropertyMap::new();
		properties.insert(name, "base");
		let left = database.insert_atom(
			&person,
			RecordId::new("person", RecordIdPart::String("left".into()))?,
			properties,
		)?;
		let right = database.insert_atom(
			&person,
			RecordId::new("person", RecordIdPart::String("right".into()))?,
			PropertyMap::new(),
		)?;
		let relation = database.insert_atom(
			&relation,
			RecordId::new("knows", RecordIdPart::String("edge".into()))?,
			PropertyMap::new(),
		)?;
		let left_local = database.attach_node(graph_id, left, Some("Person".into()), None)?;
		let right_local = database.attach_node(graph_id, right, Some("Person".into()), None)?;
		database.attach_edge(
			graph_id,
			relation,
			Some("Friend".into()),
			EdgeTopology::directed([left_local], [right_local]),
			Some(Number::from(2_i64)),
			None,
		)?;
		database.set_overlay(graph_id, left_local, name, Value::from("private"))?;
		Ok(Self {
			database,
			graph_id,
			reference,
			left,
			left_local,
		})
	}

	pub(super) fn empty_database(&self) -> FerrosGrynnResult<DatabaseRoot> {
		let mut empty = DatabaseRoot::default();
		empty.schema = self.database.schema.clone();
		empty.dictionary = self.database.dictionary.clone();
		empty.add_graph(self.reference.clone())?;
		Ok(empty)
	}

	pub(super) fn images(
		&self,
	) -> FerrosGrynnResult<(DurableAtomStoreImage, BuiltGraphImage, BuiltPrimaryIndex)> {
		let atom = build_durable_atom_store_image(
			&self.database.atoms,
			&self.database.schema,
			&self.database.dictionary,
		)?;
		let mut context =
			value_context(&self.database.schema, self.database.dictionary.as_ref().clone())?;
		let graph = build_durable_graph_image(&self.database, self.graph_id, &mut context)?;
		let reverse = build_reverse_membership_image(&self.database)?;
		Ok((atom, graph, reverse))
	}
}

/// Physical bindings and catalog for one combined AtomStore/graph generation.
pub(super) struct CombinedLayout {
	pub(super) catalog: FileCatalog,
	pub(super) atoms: AtomStoreFileSet,
	pub(super) graph: GraphFileSet,
	pub(super) reverse: ReverseMembershipFile,
}

impl CombinedLayout {
	pub(super) fn create(
		root: &Path,
		graph_id: GraphId,
		empty_atom: &DurableAtomStoreImage,
		empty_graph: &BuiltGraphImage,
		empty_reverse: &BuiltPrimaryIndex,
	) -> FerrosGrynnResult<Self> {
		let mut next = 1_u64;
		let atom_superblock = allocate(&mut next, DataFileKind::AtomStoreSuperblock)?;
		let atoms_hot = allocate(&mut next, DataFileKind::AtomsHot)?;
		let properties = allocate(&mut next, DataFileKind::Properties)?;
		let dictionary = allocate(&mut next, DataFileKind::PropertyDictionary)?;
		let primary = allocate(&mut next, DataFileKind::Index)?;
		let reverse = allocate(&mut next, DataFileKind::Index)?;
		let atoms_freelist = allocate(&mut next, DataFileKind::Freelist)?;
		let properties_freelist = allocate(&mut next, DataFileKind::Freelist)?;

		let graph_superblock = allocate(&mut next, DataFileKind::GraphSuperblock)?;
		let nodes = allocate(&mut next, DataFileKind::NodesHot)?;
		let edges = allocate(&mut next, DataFileKind::EdgesHot)?;
		let adjacency = allocate(&mut next, DataFileKind::AdjacencyOverflow)?;
		let cross_level = allocate(&mut next, DataFileKind::CrossLevelAdjacencyOverflow)?;
		let overlays = allocate(&mut next, DataFileKind::Overlays)?;
		let subgraphs = allocate(&mut next, DataFileKind::Subgraphs)?;
		let labels = allocate(&mut next, DataFileKind::Labels)?;
		let membership = allocate(&mut next, DataFileKind::Index)?;
		let graph_targets = [nodes, edges, adjacency, cross_level, overlays, subgraphs];
		let graph_freelists = graph_targets
			.iter()
			.map(|_target| allocate(&mut next, DataFileKind::Freelist))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;

		let mut catalog = FileCatalog::bootstrap()?;
		catalog.next_file_instance_id = next;
		catalog.files.extend([
			entry(
				DataSpaceId::Database,
				atom_superblock,
				DataFileRole::Superblock,
				"atoms/g/superblock.fgb",
				None,
			)?,
			entry(
				DataSpaceId::Database,
				atoms_hot,
				DataFileRole::WalBackfilled,
				"atoms/g/atoms_hot.fgb",
				None,
			)?,
			entry(
				DataSpaceId::Database,
				properties,
				DataFileRole::WalBackfilled,
				"atoms/g/properties.fgb",
				None,
			)?,
			entry(
				DataSpaceId::Database,
				dictionary,
				DataFileRole::WalBackfilled,
				"atoms/g/property_keys.fgb",
				None,
			)?,
			entry(
				DataSpaceId::Database,
				primary,
				DataFileRole::WalBackfilled,
				"atoms/g/primary.fgidx",
				None,
			)?,
			entry(
				DataSpaceId::Database,
				reverse,
				DataFileRole::WalBackfilled,
				"atoms/g/memberships.fgidx",
				None,
			)?,
			entry(
				DataSpaceId::Database,
				atoms_freelist,
				DataFileRole::WalBackfilled,
				"atoms/g/freelist_atoms.fgb",
				Some(atoms_hot),
			)?,
			entry(
				DataSpaceId::Database,
				properties_freelist,
				DataFileRole::WalBackfilled,
				"atoms/g/freelist_props.fgb",
				Some(properties),
			)?,
		]);
		let graph_space = DataSpaceId::Graph(graph_id);
		let graph_prefix = format!("graphs/{}/g", graph_id.get());
		catalog.files.extend([
			entry(
				graph_space,
				graph_superblock,
				DataFileRole::Superblock,
				&format!("{graph_prefix}/superblock.fgb"),
				None,
			)?,
			entry(
				graph_space,
				nodes,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/nodes_hot.fgb"),
				None,
			)?,
			entry(
				graph_space,
				edges,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/edges_hot.fgb"),
				None,
			)?,
			entry(
				graph_space,
				adjacency,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/adjacency_overflow.fgb"),
				None,
			)?,
			entry(
				graph_space,
				cross_level,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/adj_over_cl.fgb"),
				None,
			)?,
			entry(
				graph_space,
				overlays,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/overlays.fgb"),
				None,
			)?,
			entry(
				graph_space,
				subgraphs,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/subgraphs.fgb"),
				None,
			)?,
			entry(
				graph_space,
				labels,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/labels.fgb"),
				None,
			)?,
			entry(
				graph_space,
				membership,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/membership.fgidx"),
				None,
			)?,
		]);
		for ((file, target), name) in graph_freelists.into_iter().zip(graph_targets).zip([
			"nodes",
			"edges",
			"adj",
			"adj_cl",
			"overlays",
			"subgraphs",
		]) {
			catalog.files.push(entry(
				graph_space,
				file,
				DataFileRole::WalBackfilled,
				&format!("{graph_prefix}/freelist_{name}.fgb"),
				Some(target),
			)?);
		}
		catalog.files.sort_unstable_by_key(|item| (item.space_id, item.file_id));
		catalog.encode()?;

		// Keep these test-only metadata pages at stable addresses across the I/O loop. This is a
		// conservative Q-27 mitigation; the investigation did not prove that every populated
		// over-aligned stack value is unsafe. Production bulk builders initialize final frames in place.
		let atom_superblock_page = Box::new(empty_atom.superblock.encode()?);
		let graph_superblock_page = Box::new(empty_graph.superblock.encode()?);
		for item in &catalog.files {
			let pages: &[AlignedPage] = if item.file_id == atom_superblock {
				std::slice::from_ref(atom_superblock_page.as_ref())
			} else if item.file_id == dictionary {
				&empty_atom.dictionary.pages
			} else if item.file_id == primary {
				&empty_atom.atoms.primary.pages
			} else if item.file_id == reverse {
				&empty_reverse.pages
			} else if item.file_id == graph_superblock {
				std::slice::from_ref(graph_superblock_page.as_ref())
			} else if item.file_id == nodes {
				&empty_graph.nodes
			} else if item.file_id == edges {
				&empty_graph.edges
			} else if item.file_id == adjacency {
				&empty_graph.adjacency
			} else if item.file_id == cross_level {
				&empty_graph.cross_level_adjacency
			} else if item.file_id == overlays {
				&empty_graph.overlays
			} else if item.file_id == subgraphs {
				&empty_graph.subgraphs
			} else if item.file_id == labels {
				&empty_graph.labels
			} else if item.file_id == membership {
				&empty_graph.membership.pages
			} else if item.file_id.kind() == Some(DataFileKind::Freelist) {
				write_freelist(item.path.resolve(root).as_path())?;
				continue;
			} else {
				&[]
			};
			write_pages(item.path.resolve(root).as_path(), pages)?;
		}

		let atoms =
			AtomStoreFileSet::from_catalog(&catalog, primary, empty_atom.atoms.primary.root)?;
		let graph = GraphFileSet::from_catalog(
			&catalog,
			graph_id,
			membership,
			empty_graph.membership.root,
		)?;
		let reverse = ReverseMembershipFile::from_catalog(&catalog, reverse, empty_reverse.root)?;
		Ok(Self {
			catalog,
			atoms,
			graph,
			reverse,
		})
	}
}

fn install_class(
	database: &mut DatabaseRoot,
	id: u32,
	name: &str,
	kind: AtomKind,
) -> FerrosGrynnResult<Arc<ClassSchema>> {
	let mut class = ClassSchema::new(name, kind, [])?;
	class.id = ClassId::new(id);
	database.install_class(class, 1)?;
	database.schema.class(name).ok_or_else(|| invalid("installed test class is absent"))
}

fn allocate(next: &mut u64, kind: DataFileKind) -> FerrosGrynnResult<DataFileId> {
	let value = DataFileId::new(*next, kind)
		.ok_or_else(|| invalid("test data-file identity is invalid"))?;
	*next = next.checked_add(1).ok_or_else(|| invalid("test data-file clock is exhausted"))?;
	Ok(value)
}

fn entry(
	space_id: DataSpaceId,
	file_id: DataFileId,
	role: DataFileRole,
	path: &str,
	companion_of: Option<DataFileId>,
) -> FerrosGrynnResult<FileCatalogEntry> {
	Ok(FileCatalogEntry {
		space_id,
		file_id,
		companion_of,
		role,
		path: CatalogPath::parse(path)?,
	})
}

fn write_freelist(path: &Path) -> FerrosGrynnResult<()> {
	let mut page =
		AlignedPage::new(PageType::Freelist, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	page.seal()?;
	write_pages(path, &[page])
}

fn write_pages(path: &Path, pages: &[AlignedPage]) -> FerrosGrynnResult<()> {
	let parent = path.parent().ok_or_else(|| invalid("test data file has no parent"))?;
	fs::create_dir_all(parent).map_err(|source| {
		FerrosGrynnError::storage_io_failure("create durable graph test directory")
			.with_technical_source(source)
	})?;
	let file = DirectFile::create_new(path)?;
	if !pages.is_empty() {
		file.write_pages(PageNo::new(0), pages)?;
	}
	file.sync_data()
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
