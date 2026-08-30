#![allow(
	clippy::wildcard_imports,
	reason = "move-only module split keeps the existing imports during stage 1"
)]

use super::*;

/// Transaction-bound graph view backed by page-manager guards.
pub struct LocalGraphView<'tx> {
	pub(super) store: &'tx LocalStore,
	pub(super) transaction: Arc<LocalReadTransaction>,
	pub(super) graph: Arc<LocalGraph>,
}

/// Node projection retaining the hot page pin while cold properties remain
/// loadable through the same storage snapshot.
pub struct LocalNodeRead<'tx> {
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
	id: AtomId,
	record_id: RecordId,
	label: Option<Text>,
	slot: NodeHotSlot,
	_hot_page: PageRead<'tx>,
}

impl NodeRead for LocalNodeRead<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		self.transaction.ensure_live()?;
		Ok(Cow::Borrowed(&self.record_id))
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		self.transaction.ensure_live()?;
		Ok(self.label.as_ref().map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		read_local_properties(self.store, self.transaction, self.graph, self.slot.properties)
			.map(Cow::Owned)
	}

	fn inner_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		self.transaction.ensure_live()?;
		self.slot
			.subgraph_slot
			.map(|slot| {
				self.graph
					.children
					.get(&slot)
					.map(|child| Cow::Borrowed(child.reference()))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"node references an unknown nested graph slot",
						)
					})
			})
			.transpose()
	}

	fn inner_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		let Some(slot) = self.slot.subgraph_slot else {
			return Ok(None);
		};
		let child = self.graph.children.get(&slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node references an unknown nested graph slot")
		})?;
		let transaction = self.transaction_owner.clone().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"nested graph contents require an owning transaction view",
			)
		})?;
		let graph = self.store.materialize_graph(transaction, child)?.into_general()?;
		Ok(Some(Cow::Owned(graph)))
	}
}

/// Edge projection retaining the hot page pin and resolving topology lazily.
pub struct LocalEdgeRead<'tx> {
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
	id: AtomId,
	record_id: RecordId,
	label: Option<Text>,
	slot: EdgeHotSlot,
	_hot_page: PageRead<'tx>,
}

impl EdgeRead for LocalEdgeRead<'_> {
	fn id(&self) -> AtomId {
		self.id
	}

	fn record_id(&self) -> FerrosGrynnResult<Cow<'_, RecordId>> {
		self.transaction.ensure_live()?;
		Ok(Cow::Borrowed(&self.record_id))
	}

	fn topology(&self) -> FerrosGrynnResult<Cow<'_, EdgeTopology>> {
		decode_local_topology(self.store, self.transaction, self.graph, &self.slot).map(Cow::Owned)
	}

	fn label_text(&self) -> FerrosGrynnResult<Option<Cow<'_, Text>>> {
		self.transaction.ensure_live()?;
		Ok(self.label.as_ref().map(Cow::Borrowed))
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		read_local_properties(self.store, self.transaction, self.graph, self.slot.properties)
			.map(Cow::Owned)
	}

	fn weight(&self) -> Option<crate::values::runtime::Number> {
		self.slot.weight.map(Into::into)
	}

	fn transition_graph_ref(&self) -> FerrosGrynnResult<Option<Cow<'_, GraphRef>>> {
		self.transaction.ensure_live()?;
		self.slot
			.transition_graph
			.map(|slot| {
				self.graph
					.children
					.get(&slot)
					.map(|child| Cow::Borrowed(child.reference()))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"edge references an unknown transition graph slot",
						)
					})
			})
			.transpose()
	}

	fn transition_graph(&self) -> FerrosGrynnResult<Option<Cow<'_, Graph<Frozen>>>> {
		let Some(slot) = self.slot.transition_graph else {
			return Ok(None);
		};
		let child = self.graph.children.get(&slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"edge references an unknown transition graph slot",
			)
		})?;
		let transaction = self.transaction_owner.clone().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"transition graph contents require an owning transaction view",
			)
		})?;
		let graph = self.store.materialize_graph(transaction, child)?.into_general()?;
		Ok(Some(Cow::Owned(graph)))
	}
}

impl GraphRead for LocalGraphView<'_> {
	type Edge<'a>
		= LocalEdgeRead<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= std::vec::IntoIter<FerrosGrynnResult<AtomId>>
	where
		Self: 'a;
	type Node<'a>
		= LocalNodeRead<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= std::vec::IntoIter<FerrosGrynnResult<AtomId>>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.graph.identity
	}

	fn revision(&self) -> u64 {
		self.graph.revision
	}

	fn kind(&self) -> GraphKind {
		self.graph.kind()
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		read_local_properties(
			self.store,
			&self.transaction,
			self.graph.as_ref(),
			self.graph.properties,
		)
		.map(Cow::Owned)
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		collect_local_ids(self.store, &self.transaction, self.graph.as_ref(), AtomKind::Node)
			.map(|ids| ids.into_iter().map(Ok).collect::<Vec<_>>().into_iter())
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		collect_local_ids(self.store, &self.transaction, self.graph.as_ref(), AtomKind::Edge)
			.map(|ids| ids.into_iter().map(Ok).collect::<Vec<_>>().into_iter())
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		if id.kind() != Some(AtomKind::Node) {
			return Ok(None);
		}
		let Some(record_id) =
			record_for_local_atom(self.store, &self.transaction, self.graph.as_ref(), id)?
		else {
			return Ok(None);
		};
		read_local_node(
			self.store,
			&self.transaction,
			self.graph.as_ref(),
			id,
			record_id,
			Some(self.transaction.clone()),
		)
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		if id.kind() != Some(AtomKind::Edge) {
			return Ok(None);
		}
		let Some(record_id) =
			record_for_local_atom(self.store, &self.transaction, self.graph.as_ref(), id)?
		else {
			return Ok(None);
		};
		read_local_edge(
			self.store,
			&self.transaction,
			self.graph.as_ref(),
			id,
			record_id,
			Some(self.transaction.clone()),
		)
	}

	fn storage_mode(&self) -> GraphStorageMode {
		GraphStorageMode::DiskBacked
	}

	fn snapshot_id(&self) -> SnapshotId {
		self.transaction.snapshot_id()
	}

	fn graph_ref(&self) -> Option<&GraphRef> {
		Some(&self.graph.reference)
	}

	fn node_count(&self) -> FerrosGrynnResult<usize> {
		self.transaction.ensure_live()?;
		usize::try_from(self.graph.superblock.node_count)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds usize"))
	}

	fn edge_count(&self) -> FerrosGrynnResult<usize> {
		self.transaction.ensure_live()?;
		usize::try_from(self.graph.superblock.edge_count)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds usize"))
	}

	fn atom_id(&self, reference: &AtomRef) -> FerrosGrynnResult<Option<AtomId>> {
		lookup_local_atom(self.store, &self.transaction, self.graph.as_ref(), reference)
	}
}

pub(super) fn read_local_page<'tx>(
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &LocalGraph,
	kind: DataFileKind,
	page_no: PageNo,
) -> FerrosGrynnResult<PageRead<'tx>> {
	transaction.ensure_live()?;
	store.manager.read_page(&transaction.inner, graph.page_key(kind, page_no)?)
}

pub(super) fn copy_local_page(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	kind: DataFileKind,
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	let page = read_local_page(store, transaction, graph, kind, page_no)?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

pub(super) fn copy_local_index_page(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	file_id: DataFileId,
	page_no: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	transaction.ensure_live()?;
	let page =
		store.manager.read_page(&transaction.inner, graph.index_page_key(file_id, page_no))?;
	output.as_bytes_mut().copy_from_slice(page.as_bytes());
	Ok(())
}

pub(super) fn visit_local_primary(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	visit: impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	visit_primary_index(
		|page_no, output| {
			copy_local_page(store, transaction, graph, DataFileKind::Index, page_no, output)
		},
		graph.primary_index_root,
		visit,
	)
}

pub(super) fn decode_record_key(
	key: &[u8],
	context: &EncodeContext,
) -> FerrosGrynnResult<RecordId> {
	let stored = CanonicalCodec::decode(key, DecodeLimits::default())?;
	let Value::Record(record) = stored.into_runtime(&context.decoder())? else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"primary index key is not a canonical RecordId",
		));
	};
	Ok(*record)
}

pub(super) fn encode_record_key(
	record: &RecordId,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	CanonicalCodec::encode(
		StorageValue::from_runtime(Value::Record(Box::new(record.clone())), context)?.as_ref(),
	)
}

pub(super) fn local_query_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	record_id: RecordId,
	disk: DiskAtomRef,
) -> FerrosGrynnResult<LocalQueryAtom> {
	let atom_id = local_atom_id(store, transaction, graph, disk)?;
	let properties = match disk.kind() {
		AtomKind::Node => {
			read_local_node(store, transaction, graph, atom_id, record_id.clone(), None)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing node",
					)
				})?
				.properties()?
				.into_owned()
		}
		AtomKind::Edge => {
			read_local_edge(store, transaction, graph, atom_id, record_id.clone(), None)?
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"primary index references a missing edge",
					)
				})?
				.properties()?
				.into_owned()
		}
	};
	Ok(LocalQueryAtom {
		atom_id,
		record_id,
		properties,
	})
}

pub(super) fn lookup_local_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	reference: &AtomRef,
) -> FerrosGrynnResult<Option<AtomId>> {
	let mut context = graph.base_context()?;
	let key = encode_record_key(reference.id(), &mut context)?;
	let atom = lookup_primary_index(
		|page_no, output| {
			copy_local_page(store, transaction, graph, DataFileKind::Index, page_no, output)
		},
		graph.primary_index_root,
		&key,
	)?;
	let Some(atom) = atom else {
		return Ok(None);
	};
	if atom.kind() != reference.kind() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"primary index atom kind disagrees with its RecordId lookup",
		));
	}
	local_atom_id(store, transaction, graph, atom).map(Some)
}

pub(super) fn record_for_local_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	id: AtomId,
) -> FerrosGrynnResult<Option<RecordId>> {
	let disk = DiskAtomRef::from_parts(
		id.kind().ok_or_else(|| FerrosGrynnError::invalid_argument("invalid AtomId kind"))?,
		id.slot(),
	)?;
	let context = graph.decode_context(store, transaction)?;
	Ok(context.registered_atom(disk).map(|reference| reference.id().clone()))
}

pub(super) fn local_atom_id(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	atom: DiskAtomRef,
) -> FerrosGrynnResult<AtomId> {
	let (page_no, slot) = hot_slot_address(atom.slot())?;
	match atom.kind() {
		AtomKind::Node => {
			let page = read_local_page(store, transaction, graph, DataFileKind::NodesHot, page_no)?;
			let value = read_node_hot_slot(page.as_page(), page_no, slot)?;
			if value.flags.contains(NodeSlotFlags::TOMBSTONE) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"primary index references a node tombstone",
				));
			}
			AtomId::from_parts(AtomKind::Node, atom.slot(), value.generation)
		}
		AtomKind::Edge => {
			let page = read_local_page(store, transaction, graph, DataFileKind::EdgesHot, page_no)?;
			let value = read_edge_hot_slot(page.as_page(), page_no, slot)?;
			if value.flags.contains(EdgeSlotFlags::TOMBSTONE) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"primary index references an edge tombstone",
				));
			}
			AtomId::from_parts(AtomKind::Edge, atom.slot(), value.generation)
		}
	}
}

pub(super) fn read_local_node<'tx>(
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	id: AtomId,
	record_id: RecordId,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
) -> FerrosGrynnResult<Option<LocalNodeRead<'tx>>> {
	let (page_no, slot_index) = hot_slot_address(id.slot())?;
	let page = read_local_page(store, transaction, graph, DataFileKind::NodesHot, page_no)?;
	let slot = read_node_hot_slot(page.as_page(), page_no, slot_index)?;
	if slot.flags.contains(NodeSlotFlags::TOMBSTONE) || slot.generation != id.generation() {
		return Ok(None);
	}
	validate_table_id(slot.table_id, &record_id, &graph.labels.table_names)?;
	let label = decode_label(slot.label_id, &graph.labels.node_labels)?;
	Ok(Some(LocalNodeRead {
		store,
		transaction,
		graph,
		transaction_owner,
		id,
		record_id,
		label,
		slot,
		_hot_page: page,
	}))
}

pub(super) fn read_local_edge<'tx>(
	store: &'tx LocalStore,
	transaction: &'tx LocalReadTransaction,
	graph: &'tx LocalGraph,
	id: AtomId,
	record_id: RecordId,
	transaction_owner: Option<Arc<LocalReadTransaction>>,
) -> FerrosGrynnResult<Option<LocalEdgeRead<'tx>>> {
	let (page_no, slot_index) = hot_slot_address(id.slot())?;
	let page = read_local_page(store, transaction, graph, DataFileKind::EdgesHot, page_no)?;
	let slot = read_edge_hot_slot(page.as_page(), page_no, slot_index)?;
	if slot.flags.contains(EdgeSlotFlags::TOMBSTONE) || slot.generation != id.generation() {
		return Ok(None);
	}
	validate_table_id(slot.table_id, &record_id, &graph.labels.table_names)?;
	let label = decode_label(slot.label_id, &graph.labels.edge_labels)?;
	Ok(Some(LocalEdgeRead {
		store,
		transaction,
		graph,
		transaction_owner,
		id,
		record_id,
		label,
		slot,
		_hot_page: page,
	}))
}

pub(super) fn collect_local_ids(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	kind: AtomKind,
) -> FerrosGrynnResult<Vec<AtomId>> {
	let mut ids = Vec::new();
	visit_local_slots(store, transaction, graph, kind, |slot, generation, tombstone| {
		if !tombstone {
			ids.push(AtomId::from_parts(kind, slot, generation)?);
		}
		Ok(())
	})?;
	Ok(ids)
}

pub(super) fn visit_local_slots(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	kind: AtomKind,
	mut visit: impl FnMut(u64, u32, bool) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let (capacity, pages, file_kind) = match kind {
		AtomKind::Node => (
			graph.superblock.node_capacity,
			graph.superblock.node_hot_pages,
			DataFileKind::NodesHot,
		),
		AtomKind::Edge => (
			graph.superblock.edge_capacity,
			graph.superblock.edge_hot_pages,
			DataFileKind::EdgesHot,
		),
	};
	for page_index in 0..pages {
		let page_no = PageNo::new(page_index);
		let page = read_local_page(store, transaction, graph, file_kind, page_no)?;
		for within in 0..HOT_SLOTS_PER_PAGE {
			let slot = page_index
				.checked_mul(u64::try_from(HOT_SLOTS_PER_PAGE).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("hot slots per page exceed u64")
				})?)
				.and_then(|slot| slot.checked_add(within as u64))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("hot slot address overflows")
				})?;
			if slot >= capacity {
				break;
			}
			let (generation, tombstone) = match kind {
				AtomKind::Node => {
					let value = read_node_hot_slot(page.as_page(), page_no, within)?;
					(value.generation, value.flags.contains(NodeSlotFlags::TOMBSTONE))
				}
				AtomKind::Edge => {
					let value = read_edge_hot_slot(page.as_page(), page_no, within)?;
					(value.generation, value.flags.contains(EdgeSlotFlags::TOMBSTONE))
				}
			};
			visit(slot, generation, tombstone)?;
		}
	}
	Ok(())
}

pub(super) fn read_local_properties(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	location: Option<PropertyLocation>,
) -> FerrosGrynnResult<PropertyMap> {
	let Some(location) = location else {
		return Ok(PropertyMap::new());
	};
	let record = match location {
		PropertyLocation::Inline {
			page_no,
			slot,
			length,
		} => {
			let page =
				read_local_page(store, transaction, graph, DataFileKind::Properties, page_no)?;
			read_property_record(page.as_page(), page_no, slot, length)?.to_vec()
		}
		PropertyLocation::Large {
			first_page,
			length,
		} => {
			let limits = DecodeLimits::default();
			let length_usize = usize::try_from(length).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("large property length exceeds usize")
			})?;
			if length_usize > limits.max_total_allocation {
				return Err(FerrosGrynnError::storage_decode_limit_exceeded(
					"large property bytes",
					limits.max_total_allocation as u64,
					length_usize as u64,
				));
			}
			let page_count = length_usize.div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
			let mut pages = Vec::with_capacity(page_count);
			for offset in 0..page_count {
				let page_no = first_page
					.get()
					.checked_add(u64::try_from(offset).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("large property page exceeds u64")
					})?)
					.map(PageNo::new)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("large property page overflows")
					})?;
				let page =
					read_local_page(store, transaction, graph, DataFileKind::Properties, page_no)?;
				let mut copy = AlignedPage::zeroed();
				copy.as_bytes_mut().copy_from_slice(page.as_bytes());
				pages.push(copy);
			}
			decode_large_property(first_page, &pages, length, limits.max_total_allocation)?
		}
	};
	let context = graph.decode_context(store, transaction)?;
	decode_property_map(&record, &context.decoder(), DecodeLimits::default())
}

pub(super) fn read_local_refs(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	inline: &[Option<DiskAtomRef>],
	count: u32,
	overflow: Option<PageNo>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let count = usize::try_from(count).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("adjacency count exceeds usize")
	})?;
	let mut entries = inline.iter().flatten().copied().take(count).collect::<Vec<_>>();
	let mut next = overflow;
	let mut visited = BTreeSet::new();
	while entries.len() < count {
		let page_no = next.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("adjacency overflow ended too early")
		})?;
		if !visited.insert(page_no) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow contains a cycle",
			));
		}
		let page =
			read_local_page(store, transaction, graph, DataFileKind::AdjacencyOverflow, page_no)?;
		let (following, chunk) = decode_adjacency_overflow(page.as_page(), page_no)?;
		entries.extend(chunk);
		if entries.len() > count {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow exceeds its declared count",
			));
		}
		next = following;
	}
	if next.is_some() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"adjacency overflow contains entries beyond its declared count",
		));
	}
	Ok(entries)
}

pub(super) fn decode_local_topology(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	edge: &EdgeHotSlot,
) -> FerrosGrynnResult<EdgeTopology> {
	if edge.flags.contains(EdgeSlotFlags::CROSS_LEVEL) {
		let source = decode_cross_level_side(
			store,
			transaction,
			graph,
			edge.source_count,
			&edge.source_inline,
			edge.source_graph,
			edge.source_graph_overflow,
			EndpointSide::Source,
		)?;
		let target = decode_cross_level_side(
			store,
			transaction,
			graph,
			edge.target_count,
			&edge.target_inline,
			edge.target_graph,
			edge.target_graph_overflow,
			EndpointSide::Target,
		)?;
		return topology_from_sides(edge.topology, source, target);
	}
	let source = read_local_refs(
		store,
		transaction,
		graph,
		&edge.source_inline,
		edge.source_count,
		edge.source_overflow,
	)?
	.into_iter()
	.map(|atom| local_atom_id(store, transaction, graph, atom).map(EdgeEndpoint::Local))
	.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let target = read_local_refs(
		store,
		transaction,
		graph,
		&edge.target_inline,
		edge.target_count,
		edge.target_overflow,
	)?
	.into_iter()
	.map(|atom| local_atom_id(store, transaction, graph, atom).map(EdgeEndpoint::Local))
	.collect::<FerrosGrynnResult<Vec<_>>>()?;
	topology_from_sides(edge.topology, source, target)
}

pub(super) fn topology_from_sides(
	topology: EdgeTopologyCode,
	source: Vec<EdgeEndpoint>,
	target: Vec<EdgeEndpoint>,
) -> FerrosGrynnResult<EdgeTopology> {
	Ok(match topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::HyperDirected => {
			EdgeTopology::directed(source, target)
		}
		EdgeTopologyCode::Bidirectional => EdgeTopology::bidirectional(source, target),
		EdgeTopologyCode::Undirected | EdgeTopologyCode::HyperUndirected => {
			if !target.is_empty() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"undirected edge contains target endpoints",
				));
			}
			EdgeTopology::undirected(source)
		}
		EdgeTopologyCode::Loop => {
			if source.len() != 1 || target.len() != 1 || source != target {
				return Err(FerrosGrynnError::invalid_storage_file(
					"loop edge endpoints are inconsistent",
				));
			}
			EdgeTopology::directed(source, target)
		}
	})
}

pub(super) fn decode_cross_level_side(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	count: u32,
	inline: &[Option<DiskAtomRef>; 2],
	graph_slot: Option<u32>,
	graph_overflow: Option<PageNo>,
	side: EndpointSide,
) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
	let count = usize::try_from(count)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("endpoint count exceeds usize"))?;
	let encoded = if let Some(first) = graph_overflow {
		let mut entries = Vec::with_capacity(count);
		let mut next = Some(first);
		let mut visited = BTreeSet::new();
		while entries.len() < count {
			let page_no = next.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("cross-level overflow ended too early")
			})?;
			if !visited.insert(page_no) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"cross-level overflow contains a cycle",
				));
			}
			let page = read_local_page(
				store,
				transaction,
				graph,
				DataFileKind::CrossLevelAdjacencyOverflow,
				page_no,
			)?;
			let (following, chunk) = decode_cross_level_overflow(page.as_page(), page_no)?;
			if chunk.iter().any(|entry| entry.side != side) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"cross-level overflow contains the wrong endpoint side",
				));
			}
			entries.extend(chunk);
			if entries.len() > count {
				return Err(FerrosGrynnError::invalid_storage_file(
					"cross-level overflow exceeds its declared count",
				));
			}
			next = following;
		}
		if next.is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"cross-level overflow contains entries beyond its declared count",
			));
		}
		entries
	} else {
		let slot = if count == 0 {
			return Ok(Vec::new());
		} else {
			graph_slot.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"inline cross-level endpoints have no graph slot",
				)
			})?
		};
		inline
			.iter()
			.take(count)
			.map(|atom| {
				atom.map(|atom| QualifiedDiskAtomRef {
					graph_slot: slot,
					side,
					atom,
				})
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("inline cross-level endpoint is absent")
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?
	};
	encoded
		.into_iter()
		.map(|endpoint| {
			if endpoint.graph_slot == 0 {
				local_atom_id(store, transaction, graph, endpoint.atom).map(EdgeEndpoint::Local)
			} else {
				resolve_qualified_atom(
					store,
					transaction,
					graph,
					endpoint.graph_slot,
					endpoint.atom,
				)
				.map(EdgeEndpoint::Nested)
			}
		})
		.collect()
}

pub(super) fn resolve_qualified_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	graph_slot: u32,
	atom: DiskAtomRef,
) -> FerrosGrynnResult<QualifiedAtomId> {
	let mut scope = Vec::new();
	let target = find_qualified_atom(store, transaction, graph, graph_slot, atom, &mut scope)?
		.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"qualified endpoint references an unknown subgraph slot",
			)
		})?;
	QualifiedAtomId::new(scope, target)
}

pub(super) fn find_qualified_atom(
	store: &LocalStore,
	transaction: &LocalReadTransaction,
	graph: &LocalGraph,
	graph_slot: u32,
	atom: DiskAtomRef,
	scope: &mut Vec<AtomId>,
) -> FerrosGrynnResult<Option<AtomId>> {
	for child in graph.children.values() {
		let container = local_atom_id(
			store,
			transaction,
			graph,
			child.parent_atom.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("nested graph has no parent atom")
			})?,
		)?;
		scope.push(container);
		if child.subgraph_slot == Some(graph_slot) {
			return local_atom_id(store, transaction, child, atom).map(Some);
		}
		if let Some(target) =
			find_qualified_atom(store, transaction, child, graph_slot, atom, scope)?
		{
			return Ok(Some(target));
		}
		scope.pop();
	}
	Ok(None)
}

pub(super) const fn table_kind_accepts(table: TableKind, atom: AtomKind) -> bool {
	matches!(
		(table, atom),
		(TableKind::Any, _)
			| (TableKind::Normal, AtomKind::Node)
			| (TableKind::Relation, AtomKind::Edge)
	)
}
