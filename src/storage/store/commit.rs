#![allow(
	clippy::wildcard_imports,
	reason = "move-only module split keeps the existing imports during stage 1"
)]

use super::*;

/// One graph generation ready to become reachable from the durable catalog.
pub(super) struct BuiltGraphGeneration {
	pub(super) catalog_entry: GraphCatalogEntry,
	pub(super) file_entries: Vec<FileCatalogEntry>,
	pub(super) registrations: Vec<DataFileRegistration>,
	pub(super) superblock: GraphSuperblock,
	pub(super) indexes: Vec<BuiltTableIndexBinding>,
}

pub(super) struct BuiltGraphImage {
	pub(super) catalog_entry: GraphCatalogEntry,
	pub(super) superblock: GraphSuperblock,
	pub(super) payloads: BTreeMap<DataFileKind, Vec<AlignedPage>>,
	pub(super) indexes: Vec<BuiltScalarIndexImage>,
}

pub(super) struct PlannedGraph {
	pub(super) graph_id: GraphId,
	pub(super) record_id: RecordId,
	/// Name written into the existing graph catalog entry.
	pub(super) name: String,
	pub(super) kind: GraphKind,
	pub(super) graph: Graph<Frozen>,
	pub(super) parent_graph_id: Option<GraphId>,
	pub(super) subgraph_slot: Option<u32>,
	pub(super) parent_atom: Option<DiskAtomRef>,
	pub(super) children: Vec<PlannedChild>,
}

pub(super) struct PlannedChild {
	pub(super) container: AtomId,
	pub(super) slot: u32,
	pub(super) graph: Box<PlannedGraph>,
}

pub(super) struct BuiltScalarIndexImage {
	pub(super) table: String,
	pub(super) name: String,
	pub(super) kind: TableIndexKind,
	pub(super) fields: Vec<u32>,
	pub(super) root: PageNo,
	pub(super) pages: Vec<AlignedPage>,
}

pub(super) struct BuiltTableIndexBinding {
	pub(super) table: String,
	pub(super) index: TableIndexCatalogEntry,
}

pub(super) fn database_property_dictionary(
	database: &DatabaseCatalog,
) -> FerrosGrynnResult<PropertyDictionary> {
	let mut dictionary = PropertyDictionary::with_capacity(database.property_keys.len());
	for (expected, name) in database.property_keys.iter().enumerate() {
		let actual = dictionary.intern(name.clone())?;
		if usize::try_from(actual.get()).ok() != Some(expected) {
			return Err(FerrosGrynnError::invalid_storage_file("property dictionary is not dense"));
		}
	}
	Ok(dictionary)
}

pub(super) fn allocate_graph_id(database: &mut DatabaseCatalog) -> FerrosGrynnResult<GraphId> {
	let graph_id = GraphId::new(database.next_graph_id);
	if graph_id == GraphId::SYSTEM {
		return Err(FerrosGrynnError::invalid_storage_file(
			"the durable graph identity clock is exhausted",
		));
	}
	database.next_graph_id = database.next_graph_id.checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("the durable graph identity clock is exhausted")
	})?;
	Ok(graph_id)
}

pub(super) fn plan_graph_tree(
	database: &mut DatabaseCatalog,
	graph_id: GraphId,
	record_id: &RecordId,
	name: &str,
	graph: &MaterializedGraph,
) -> FerrosGrynnResult<PlannedGraph> {
	let mut next_subgraph_slot = 1_u32;
	plan_graph(
		database,
		graph_id,
		(record_id.clone(), name.to_owned()),
		graph.kind(),
		graph.clone().into_general()?,
		None,
		None,
		None,
		&mut next_subgraph_slot,
	)
}

pub(super) fn plan_graph(
	database: &mut DatabaseCatalog,
	graph_id: GraphId,
	catalog_identity: (RecordId, String),
	kind: GraphKind,
	graph: Graph<Frozen>,
	parent_graph_id: Option<GraphId>,
	subgraph_slot: Option<u32>,
	parent_atom: Option<DiskAtomRef>,
	next_subgraph_slot: &mut u32,
) -> FerrosGrynnResult<PlannedGraph> {
	let (record_id, name) = catalog_identity;
	let mut nested = Vec::new();
	for id in GraphRead::node_ids(&graph)? {
		let id = id?;
		let node = graph.node(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested container disappeared")
		})?;
		if let Some(inner) = node.inner_graph() {
			nested.push((id, inner.clone()));
		}
	}
	for id in GraphRead::edge_ids(&graph)? {
		let id = id?;
		let edge = graph.edge(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("transition container disappeared")
		})?;
		if let Some(transition) = edge.transition_graph() {
			nested.push((id, transition.clone()));
		}
	}
	nested.sort_unstable_by_key(|(id, _graph)| *id);
	let mut children = Vec::with_capacity(nested.len());
	for (container, nested_graph) in nested {
		if *next_subgraph_slot == u32::MAX {
			return Err(FerrosGrynnError::graph_capacity_exceeded("subgraph"));
		}
		let slot = *next_subgraph_slot;
		*next_subgraph_slot = next_subgraph_slot
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("subgraph"))?;
		let child_id = allocate_graph_id(database)?;
		let child_record: RecordId =
			format!("{INTERNAL_NESTED_GRAPH_TABLE}:{}", child_id.get()).parse()?;
		let container_disk = DiskAtomRef::from_parts(
			container.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("nested container has an invalid kind")
			})?,
			container.slot(),
		)?;
		let child_name = child_record.id().to_string();
		let child = plan_graph(
			database,
			child_id,
			(child_record, child_name),
			GraphKind::Graph,
			nested_graph,
			Some(graph_id),
			Some(slot),
			Some(container_disk),
			next_subgraph_slot,
		)?;
		children.push(PlannedChild {
			container,
			slot,
			graph: Box::new(child),
		});
	}
	Ok(PlannedGraph {
		graph_id,
		record_id,
		name,
		kind,
		graph,
		parent_graph_id,
		subgraph_slot,
		parent_atom,
		children,
	})
}

pub(super) fn graph_tree_ids(database: &DatabaseCatalog, root: GraphId) -> BTreeSet<GraphId> {
	let mut ids = BTreeSet::from([root]);
	loop {
		let before = ids.len();
		for graph in &database.graphs {
			if graph.parent_graph_id.is_some_and(|parent| ids.contains(&parent)) {
				ids.insert(graph.graph_id);
			}
		}
		if ids.len() == before {
			return ids;
		}
	}
}

pub(super) fn build_graph_images(
	plan: &PlannedGraph,
	dictionary: &mut PropertyDictionary,
	tables: &[LocalCommitTable],
) -> FerrosGrynnResult<Vec<BuiltGraphImage>> {
	let mut images = vec![build_graph_image(plan, dictionary, tables)?];
	for child in &plan.children {
		images.extend(build_graph_images(&child.graph, dictionary, tables)?);
	}
	Ok(images)
}

pub(super) fn encode_tables(
	tables: &[LocalCommitTable],
	database: &DatabaseCatalog,
	previous: &DatabaseCatalog,
	new_indexes: &[BuiltTableIndexBinding],
) -> FerrosGrynnResult<Vec<TableCatalogEntry>> {
	let graph_ids = database
		.graphs
		.iter()
		.map(|entry| (entry.record_id.as_str(), entry.graph_id))
		.collect::<BTreeMap<_, _>>();
	let mut encoded = Vec::with_capacity(tables.len());
	for table in tables {
		let graph_id =
			graph_ids.get(table.graph.to_string().as_str()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(format!(
					"table '{}' references a graph absent from the durable catalog",
					table.name
				))
			})?;
		let mut indexes = Vec::with_capacity(table.indexes.len());
		for definition in &table.indexes {
			let candidate = new_indexes
				.iter()
				.find(|candidate| {
					candidate.table == table.name && candidate.index.name == definition.name
				})
				.map(|candidate| &candidate.index)
				.or_else(|| {
					previous.tables.iter().find(|existing| existing.name == table.name).and_then(
						|existing| {
							existing.indexes.iter().find(|index| index.name == definition.name)
						},
					)
				})
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(format!(
						"scalar index '{}.{}' has no built physical generation",
						table.name, definition.name
					))
				})?;
			if candidate.kind != definition.kind || candidate.fields != definition.fields {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' physical definition is stale",
					table.name, definition.name
				)));
			}
			indexes.push(candidate.clone());
		}
		indexes.sort_unstable_by(|left, right| left.name.cmp(&right.name));
		encoded.push(TableCatalogEntry {
			name: table.name.clone(),
			graph_id,
			kind: table.kind,
			fields: table
				.fields
				.iter()
				.map(|field| TableFieldCatalogEntry {
					name: field.name.clone(),
					value_type: field.value_type.clone(),
					property_key: field.property_key,
				})
				.collect(),
			indexes,
		});
	}
	encoded.sort_unstable_by(|left, right| left.name.cmp(&right.name));
	Ok(encoded)
}

pub(super) fn load_database(
	root: &Path,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	commit_ts: u64,
	database: &DatabaseCatalog,
	files: &FileCatalog,
) -> FerrosGrynnResult<LocalDatabaseSnapshot> {
	let dictionary = Arc::new(database_property_dictionary(database)?);
	let mut graphs = Vec::with_capacity(
		database.graphs.iter().filter(|entry| entry.parent_graph_id.is_none()).count(),
	);
	for entry in database.graphs.iter().filter(|entry| entry.parent_graph_id.is_none()) {
		let record_id: RecordId = entry.record_id.parse()?;
		graphs.push(LocalSnapshotGraph {
			record_id: record_id.clone(),
			name: entry.name.clone(),
			graph: Arc::new(open_local_graph(
				root,
				manager,
				transaction,
				entry,
				database,
				files,
				&database.tables,
				record_id,
				dictionary.clone(),
				None,
			)?),
		});
	}
	let graph_records = database
		.graphs
		.iter()
		.map(|entry| (entry.graph_id, entry.record_id.as_str()))
		.collect::<BTreeMap<_, _>>();
	let mut tables = Vec::with_capacity(database.tables.len());
	for table in &database.tables {
		let graph = graph_records.get(&table.graph_id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("table references an unknown graph identity")
		})?;
		tables.push(LocalCommitTable {
			name: table.name.clone(),
			graph: graph.parse()?,
			kind: table.kind,
			fields: table
				.fields
				.iter()
				.map(|field| LocalCommitTableField {
					name: field.name.clone(),
					value_type: field.value_type.clone(),
					property_key: field.property_key,
				})
				.collect(),
			indexes: table
				.indexes
				.iter()
				.map(|index| LocalCommitTableIndex {
					name: index.name.clone(),
					kind: index.kind,
					fields: index.fields.clone(),
				})
				.collect(),
		});
	}
	Ok(LocalDatabaseSnapshot {
		commit_ts,
		property_dictionary: dictionary,
		graphs,
		tables,
	})
}

pub(super) fn load_changed_graphs(
	root: &Path,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	database: &DatabaseCatalog,
	files: &FileCatalog,
	changed_graphs: &[RecordId],
	property_dictionary: Arc<PropertyDictionary>,
	superblocks: &BTreeMap<GraphId, GraphSuperblock>,
) -> FerrosGrynnResult<Vec<LocalSnapshotGraph>> {
	let mut graphs = Vec::with_capacity(changed_graphs.len());
	for record_id in changed_graphs {
		let record_text = record_id.to_string();
		let Some(entry) = database
			.graphs
			.iter()
			.find(|entry| entry.parent_graph_id.is_none() && entry.record_id == record_text)
		else {
			continue;
		};
		graphs.push(LocalSnapshotGraph {
			record_id: record_id.clone(),
			name: entry.name.clone(),
			graph: Arc::new(open_local_graph(
				root,
				manager,
				transaction,
				entry,
				database,
				files,
				&database.tables,
				record_id.clone(),
				property_dictionary.clone(),
				Some(superblocks),
			)?),
		});
	}
	Ok(graphs)
}

#[allow(
	clippy::too_many_arguments,
	reason = "recursive graph reopening carries one immutable catalog and storage context"
)]
pub(super) fn open_local_graph(
	root: &Path,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	entry: &GraphCatalogEntry,
	database: &DatabaseCatalog,
	files: &FileCatalog,
	tables: &[TableCatalogEntry],
	record_id: RecordId,
	property_dictionary: Arc<PropertyDictionary>,
	superblocks: Option<&BTreeMap<GraphId, GraphSuperblock>>,
) -> FerrosGrynnResult<LocalGraph> {
	let mut children = BTreeMap::new();
	for child in
		database.graphs.iter().filter(|child| child.parent_graph_id == Some(entry.graph_id))
	{
		let slot = child.subgraph_slot.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested graph has no subgraph slot")
		})?;
		let child_record: RecordId = child.record_id.parse()?;
		let graph = Arc::new(open_local_graph(
			root,
			manager,
			transaction,
			child,
			database,
			files,
			tables,
			child_record,
			property_dictionary.clone(),
			superblocks,
		)?);
		if children.insert(slot, graph).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"nested graph slots are not unique",
			));
		}
	}
	let paths = graph_file_paths(root, entry, files)?;
	let file_ids = graph_file_ids(entry, files)?;
	paths.get(&DataFileKind::GraphSuperblock).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph superblock file is missing")
	})?;
	let superblock = superblocks
		.and_then(|superblocks| superblocks.get(&entry.graph_id).cloned())
		.map_or_else(|| manager.graph_superblock(entry.graph_id), Ok)?;
	if superblock.graph_id != entry.graph_id
		|| superblock.node_count > superblock.node_capacity
		|| superblock.edge_count > superblock.edge_capacity
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph superblock disagrees with its catalog or capacities",
		));
	}
	let labels_file = open_graph_file(&paths, DataFileKind::Labels)?;
	if labels_file.page_count().checked_mul(PAGE_SIZE as u64) != Some(superblock.labels_size_bytes)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"label file length disagrees with GraphSuperblock",
		));
	}
	let labels_id = file_ids
		.get(&DataFileKind::Labels)
		.copied()
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label file identity is missing"))?;
	let labels = decode_label_dictionary(|page_no, output| {
		let page = manager.read_page(
			transaction,
			crate::storage::types::PageKey::new(entry.graph_id, labels_id, page_no),
		)?;
		output.as_bytes_mut().copy_from_slice(page.as_bytes());
		Ok(())
	})?;
	let mut scalar_indexes = BTreeMap::<String, Vec<LocalScalarIndex>>::new();
	for table in tables.iter().filter(|table| table.graph_id == entry.graph_id) {
		for index in &table.indexes {
			if !files
				.files
				.iter()
				.any(|file| file.graph_id == entry.graph_id && file.file_id == index.file_id)
			{
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"scalar index '{}.{}' references an unregistered file",
					table.name, index.name
				)));
			}
			let fields = index
				.fields
				.iter()
				.map(|property_key| {
					table
						.fields
						.iter()
						.find(|field| field.property_key == *property_key)
						.map(|field| (PropertyKeyId::new(*property_key), field.value_type.clone()))
						.ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"scalar index references an unknown table field",
							)
						})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			scalar_indexes.entry(table.name.clone()).or_default().push(LocalScalarIndex {
				name: index.name.clone(),
				kind: index.kind,
				fields,
				file_id: index.file_id,
				root: index.root,
			});
		}
	}
	let graph = LocalGraph {
		graph_id: entry.graph_id,
		subgraph_slot: entry.subgraph_slot,
		parent_atom: entry.parent_atom,
		reference: GraphRef::new(record_id, entry.kind, GraphStorageMode::DiskBacked),
		identity: uuid::Uuid::from_bytes(entry.identity).into(),
		revision: entry.revision,
		primary_index_root: entry.primary_index_root,
		properties: entry.properties,
		superblock,
		files: file_ids,
		labels,
		property_dictionary,
		decode_context: Mutex::new(None),
		scalar_indexes,
		children,
	};
	validate_local_subgraphs(manager, transaction, &graph)?;
	Ok(graph)
}

pub(super) fn validate_local_subgraphs(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	graph: &LocalGraph,
) -> FerrosGrynnResult<()> {
	let file_id = graph.file_id(DataFileKind::Subgraphs)?;
	let mut page_no = PageNo::new(0);
	let mut visited = BTreeSet::new();
	let mut entries = BTreeMap::new();
	loop {
		if !visited.insert(page_no) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"subgraph directory contains a page cycle",
			));
		}
		let page = manager.read_page(
			transaction,
			crate::storage::types::PageKey::new(graph.graph_id, file_id, page_no),
		)?;
		let (next, chunk) = decode_subgraph_directory(page.as_page(), page_no)?;
		for entry in chunk {
			if entry.slot == 0 || entries.insert(entry.slot, entry).is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"subgraph directory contains a reserved or duplicate slot",
				));
			}
		}
		let Some(next) = next else {
			break;
		};
		page_no = next;
	}
	if entries.len() != graph.children.len()
		|| u64::try_from(entries.len()).ok() != Some(graph.superblock.subgraph_count)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"subgraph directory disagrees with the catalog or superblock",
		));
	}
	for (slot, child) in &graph.children {
		let entry = entries.get(slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested graph is absent from its directory")
		})?;
		if entry.node_capacity != child.superblock.node_capacity
			|| entry.edge_capacity != child.superblock.edge_capacity
			|| u64::from(entry.node_count) != child.superblock.node_count
			|| u64::from(entry.edge_count) != child.superblock.edge_count
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"subgraph directory counts disagree with the nested graph",
			));
		}
		let parent = child.parent_atom.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("nested graph has no parent atom")
		})?;
		let (hot_page, hot_slot) = hot_slot_address(parent.slot())?;
		match parent.kind() {
			AtomKind::Node => {
				let page = manager
					.read_page(transaction, graph.page_key(DataFileKind::NodesHot, hot_page)?)?;
				let value = read_node_hot_slot(page.as_page(), hot_page, hot_slot)?;
				if value.generation != entry.generation || value.subgraph_slot != Some(*slot) {
					return Err(FerrosGrynnError::invalid_storage_file(
						"node nested-graph binding disagrees with SubgraphDir",
					));
				}
			}
			AtomKind::Edge => {
				let page = manager
					.read_page(transaction, graph.page_key(DataFileKind::EdgesHot, hot_page)?)?;
				let value = read_edge_hot_slot(page.as_page(), hot_page, hot_slot)?;
				if value.generation != entry.generation || value.transition_graph != Some(*slot) {
					return Err(FerrosGrynnError::invalid_storage_file(
						"edge transition binding disagrees with SubgraphDir",
					));
				}
			}
		}
	}
	Ok(())
}

pub(super) fn graph_file_ids(
	graph: &GraphCatalogEntry,
	files: &FileCatalog,
) -> FerrosGrynnResult<BTreeMap<DataFileKind, DataFileId>> {
	let mut ids = BTreeMap::new();
	for entry in files.files.iter().filter(|entry| entry.graph_id == graph.graph_id) {
		let kind = entry.file_id.kind().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph file has an unknown kind")
		})?;
		if kind == DataFileKind::Index && entry.file_id != graph.primary_index_file_id {
			continue;
		}
		if ids.insert(kind, entry.file_id).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"graph catalog contains duplicate file kinds",
			));
		}
	}
	for (kind, _name) in GRAPH_FILE_KINDS {
		if !ids.contains_key(&kind) {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"graph catalog is missing {kind:?}"
			)));
		}
	}
	Ok(ids)
}

pub(super) fn write_new_graph_generation(
	root: &Path,
	image: BuiltGraphImage,
	next_file_instance_id: &mut u64,
) -> FerrosGrynnResult<BuiltGraphGeneration> {
	let graph_id = image.catalog_entry.graph_id;
	let generation_name = Uuid::now_v7().to_string();
	let relative_directory = format!("graphs/{:016x}/{generation_name}", graph_id.get());
	let absolute_directory =
		root.join(relative_directory.replace('/', std::path::MAIN_SEPARATOR_STR));
	fs::create_dir_all(&absolute_directory)
		.map_err(|source| storage_io("create graph generation directory", source))?;
	let mut catalog_entry = image.catalog_entry;
	let mut payloads = image.payloads;
	let mut file_entries = Vec::with_capacity(GRAPH_FILE_KINDS.len() + image.indexes.len());
	let mut registrations = Vec::with_capacity(GRAPH_FILE_KINDS.len() + image.indexes.len());
	for (kind, filename) in GRAPH_FILE_KINDS {
		let file_id = allocate_file_id(next_file_instance_id, kind)?;
		if kind == DataFileKind::Index {
			catalog_entry.primary_index_file_id = file_id;
		}
		let relative = format!("{relative_directory}/{filename}");
		let catalog_path = CatalogPath::parse(&relative)?;
		let absolute = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
		let pages = payloads.remove(&kind).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph generation payload is missing")
		})?;
		write_pages(&absolute, &pages)?;
		let role = if kind == DataFileKind::GraphSuperblock {
			DataFileRole::GraphSuperblock
		} else {
			DataFileRole::WalBackfilled
		};
		file_entries.push(FileCatalogEntry {
			graph_id,
			file_id,
			role,
			path: catalog_path,
		});
		registrations.push(DataFileRegistration {
			graph_id,
			file_id,
			role,
			path: absolute,
		});
	}
	if !payloads.is_empty() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph generation contains an unregistered base payload",
		));
	}
	let mut indexes = Vec::with_capacity(image.indexes.len());
	for (ordinal, index) in image.indexes.into_iter().enumerate() {
		let file_id = allocate_file_id(next_file_instance_id, DataFileKind::Index)?;
		let filename = format!("scalar_{ordinal:04}.fgidx");
		let relative = format!("{relative_directory}/{filename}");
		let catalog_path = CatalogPath::parse(&relative)?;
		let absolute = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
		write_pages(&absolute, &index.pages)?;
		file_entries.push(FileCatalogEntry {
			graph_id,
			file_id,
			role: DataFileRole::WalBackfilled,
			path: catalog_path,
		});
		registrations.push(DataFileRegistration {
			graph_id,
			file_id,
			role: DataFileRole::WalBackfilled,
			path: absolute,
		});
		indexes.push(BuiltTableIndexBinding {
			table: index.table,
			index: TableIndexCatalogEntry {
				name: index.name,
				kind: index.kind,
				fields: index.fields,
				file_id,
				root: index.root,
			},
		});
	}
	Ok(BuiltGraphGeneration {
		catalog_entry,
		file_entries,
		registrations,
		superblock: image.superblock,
		indexes,
	})
}

pub(super) fn prepare_in_place_image(
	root: &Path,
	current: &GraphCatalogEntry,
	files: &FileCatalog,
	image: &mut BuiltGraphImage,
) -> FerrosGrynnResult<bool> {
	// ponytail: scalar indexes use immutable bulk generations; add incremental
	// WAL page maintenance only after indexed-write profiles justify it.
	if !image.indexes.is_empty() {
		return Ok(false);
	}
	let paths = graph_file_paths(root, current, files)?;
	let path = paths.get(&DataFileKind::GraphSuperblock).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph superblock file is missing")
	})?;
	let previous = select_graph_superblock(path)?;
	let next = &image.superblock;
	let stable_layout = previous.node_capacity == next.node_capacity
		&& previous.edge_capacity == next.edge_capacity
		&& previous.node_count == next.node_count
		&& previous.edge_count == next.edge_count
		&& previous.subgraph_capacity == next.subgraph_capacity
		&& previous.subgraph_count == next.subgraph_count
		&& previous.node_hot_pages == next.node_hot_pages
		&& previous.edge_hot_pages == next.edge_hot_pages
		&& previous.properties_pages == next.properties_pages
		&& previous.labels_size_bytes == next.labels_size_bytes
		&& previous.flags == next.flags
		&& previous.schema_root_page == next.schema_root_page
		&& previous.stats_root_page == next.stats_root_page;
	if !stable_layout {
		return Ok(false);
	}
	image.catalog_entry.created_lsn = current.created_lsn;
	image.catalog_entry.primary_index_file_id = current.primary_index_file_id;
	image.superblock.generation = previous.generation;
	image.superblock.backfilled_lsn = previous.backfilled_lsn;
	image.superblock.checkpoint_record_lsn = previous.checkpoint_record_lsn;
	image.superblock.created_at_ms = previous.created_at_ms;
	Ok(true)
}

pub(super) fn stage_graph_image(
	manager: &SyncPageManager,
	transaction: &mut TxHandle,
	files: &FileCatalog,
	image: &BuiltGraphImage,
) -> FerrosGrynnResult<()> {
	let graph_id = image.catalog_entry.graph_id;
	let file_ids = graph_file_ids(&image.catalog_entry, files)?;
	for (kind, pages) in &image.payloads {
		if *kind == DataFileKind::GraphSuperblock {
			continue;
		}
		let file_id = file_ids.get(kind).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!("graph file {kind:?} is missing"))
		})?;
		let physical_pages = manager.file_page_count(graph_id, file_id)?;
		let required = u64::try_from(pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("graph page count exceeds u64")
		})?;
		manager.ensure_file_pages(graph_id, file_id, required)?;
		for (index, page) in pages.iter().enumerate() {
			let page_no = PageNo::new(u64::try_from(index).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("graph page index exceeds u64")
			})?);
			let key = crate::storage::types::PageKey::new(graph_id, file_id, page_no);
			if page_no.get() < physical_pages {
				let current = manager.read_page(transaction, key)?;
				let mut canonical = AlignedPage::zeroed();
				canonical.as_bytes_mut().copy_from_slice(current.as_bytes());
				canonical.assign_lsn(page_no, PageLsn::ZERO)?;
				if canonical.as_bytes() == page.as_bytes() {
					continue;
				}
			}
			manager.stage_frame(transaction, key, page)?;
		}
	}
	Ok(())
}

pub(super) fn allocate_file_id(
	next_file_instance_id: &mut u64,
	kind: DataFileKind,
) -> FerrosGrynnResult<DataFileId> {
	let instance_id = *next_file_instance_id;
	let file_id = DataFileId::new(instance_id, kind).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("the physical-file identity clock is exhausted")
	})?;
	*next_file_instance_id = instance_id.checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("the physical-file identity clock is exhausted")
	})?;
	Ok(file_id)
}

pub(super) fn unix_time_ms() -> FerrosGrynnResult<u64> {
	let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|source| {
		FerrosGrynnError::invalid_storage_file(format!(
			"system clock precedes Unix epoch: {source}"
		))
	})?;
	u64::try_from(duration.as_millis())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("Unix timestamp exceeds u64"))
}

pub(super) fn write_pages(path: &Path, pages: &[AlignedPage]) -> FerrosGrynnResult<()> {
	let file = DirectFile::create_new(path)?;
	file.write_pages(PageNo::new(0), pages)?;
	file.sync_all()
}

pub(super) fn storage_io(operation: &str, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
