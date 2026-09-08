#![allow(
	clippy::wildcard_imports,
	reason = "move-only module split keeps the existing imports during stage 1"
)]

use super::*;

pub(super) struct PropertyStoreBuilder {
	next_page: u64,
	current: Option<PropertyPageBuilder>,
	current_page: Option<PageNo>,
	pages: Vec<(PageNo, Box<AlignedPage>)>,
}

impl PropertyStoreBuilder {
	fn new() -> Self {
		Self::starting_at(0)
	}

	fn starting_at(next_page: u64) -> Self {
		Self {
			next_page,
			current: None,
			current_page: None,
			pages: Vec::new(),
		}
	}

	fn insert(&mut self, record: &[u8]) -> FerrosGrynnResult<PropertyLocation> {
		let length = u32::try_from(record.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("property record exceeds u32")
		})?;
		if record.len() > MAX_INLINE_PROPERTY_BYTES {
			let first_page = PageNo::new(self.next_page);
			let pages = encode_large_property(first_page, PageLsn::ZERO, record)?;
			self.next_page = self
				.next_page
				.checked_add(u64::try_from(pages.len()).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("large property page count exceeds u64")
				})?)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("property page clock is exhausted")
				})?;
			for (index, page) in pages.into_iter().enumerate() {
				let page_no = first_page
					.get()
					.checked_add(u64::try_from(index).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("property page index exceeds u64")
					})?)
					.map(PageNo::new)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("property page number overflows")
					})?;
				self.pages.push((page_no, Box::new(page)));
			}
			return Ok(PropertyLocation::Large {
				first_page,
				length,
			});
		}

		if self.current.is_none() {
			self.start_page()?;
		}
		if let Some(slot) = self
			.current
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("property page is absent"))?
			.try_insert(record)?
		{
			return Ok(PropertyLocation::Inline {
				page_no: self.current_page.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("property page number is absent")
				})?,
				slot,
				length,
			});
		}
		self.finish_current()?;
		self.start_page()?;
		let slot = self
			.current
			.as_mut()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("property page is absent"))?
			.try_insert(record)?
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("inline property does not fit an empty page")
			})?;
		Ok(PropertyLocation::Inline {
			page_no: self.current_page.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("property page number is absent")
			})?,
			slot,
			length,
		})
	}

	fn start_page(&mut self) -> FerrosGrynnResult<()> {
		let page_no = PageNo::new(self.next_page);
		self.next_page = self.next_page.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("property page clock is exhausted")
		})?;
		self.current = Some(PropertyPageBuilder::new(page_no, PageLsn::ZERO));
		self.current_page = Some(page_no);
		Ok(())
	}

	fn finish_current(&mut self) -> FerrosGrynnResult<()> {
		if let Some(builder) = self.current.take() {
			let page_no = self.current_page.take().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("property page number is absent")
			})?;
			let page = builder.finish()?;
			self.pages.push((page_no, page));
		}
		Ok(())
	}

	fn finish_numbered(mut self) -> FerrosGrynnResult<Vec<(PageNo, Box<AlignedPage>)>> {
		self.finish_current()?;
		self.pages.sort_unstable_by_key(|(page_no, _page)| *page_no);
		Ok(self.pages)
	}

	fn finish(self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		let pages = self.finish_numbered()?;
		for (index, (page_no, _page)) in pages.iter().enumerate() {
			if page_no.get()
				!= u64::try_from(index).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("property page index exceeds u64")
				})? {
				return Err(FerrosGrynnError::invalid_storage_file(
					"property pages are not physically contiguous",
				));
			}
		}
		let mut output = Vec::with_capacity(pages.len());
		for (_page_no, page) in pages {
			output.push(*page);
		}
		Ok(output)
	}
}

pub(super) struct OverflowStoreBuilder {
	next_page: u64,
	pages: Vec<(PageNo, AlignedPage)>,
}

impl Default for OverflowStoreBuilder {
	fn default() -> Self {
		Self::starting_at(0)
	}
}

impl OverflowStoreBuilder {
	fn starting_at(next_page: u64) -> Self {
		Self {
			next_page,
			pages: Vec::new(),
		}
	}

	fn append(&mut self, entries: &[DiskAtomRef]) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = PageNo::new(self.next_page);
		let chunks = entries.chunks(OVERFLOW_REFS_PER_PAGE).collect::<Vec<_>>();
		for (index, chunk) in chunks.iter().enumerate() {
			let page_no = first
				.get()
				.checked_add(u64::try_from(index).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("adjacency page index exceeds u64")
				})?)
				.map(PageNo::new)
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("adjacency page number overflows")
				})?;
			let next = if index + 1 == chunks.len() {
				None
			} else {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("adjacency link overflows")
				})?))
			};
			let page = encode_adjacency_overflow(page_no, PageLsn::ZERO, next, chunk)?;
			self.pages.push((page_no, page));
		}
		self.next_page = self
			.next_page
			.checked_add(u64::try_from(chunks.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("adjacency page count exceeds u64")
			})?)
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("adjacency page number overflows")
			})?;
		Ok(Some(first))
	}

	fn finish_numbered(self) -> Vec<(PageNo, AlignedPage)> {
		self.pages
	}

	fn finish(self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		for (index, (page_no, _page)) in self.pages.iter().enumerate() {
			if usize::try_from(page_no.get()).ok() != Some(index) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"adjacency pages are not zero-based",
				));
			}
		}
		Ok(self.pages.into_iter().map(|(_page_no, page)| page).collect())
	}
}

#[derive(Default)]
pub(super) struct NodeAdjacency {
	outgoing: Vec<DiskAtomRef>,
	incoming: Vec<DiskAtomRef>,
	undirected: Vec<DiskAtomRef>,
}

pub(super) struct EncodedTopology {
	code: EdgeTopologyCode,
	source: Vec<EncodedEndpoint>,
	target: Vec<EncodedEndpoint>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct EncodedEndpoint {
	graph_slot: u32,
	atom: DiskAtomRef,
}

#[derive(Default)]
pub(super) struct CrossLevelOverflowStoreBuilder {
	pages: Vec<AlignedPage>,
}

impl CrossLevelOverflowStoreBuilder {
	fn append(
		&mut self,
		entries: &[EncodedEndpoint],
		side: EndpointSide,
	) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = PageNo::new(u64::try_from(self.pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("cross-level page count exceeds u64")
		})?);
		let encoded = entries
			.iter()
			.map(|entry| QualifiedDiskAtomRef {
				graph_slot: entry.graph_slot,
				side,
				atom: entry.atom,
			})
			.collect::<Vec<_>>();
		let chunks = encoded.chunks(CROSS_LEVEL_REFS_PER_PAGE).collect::<Vec<_>>();
		for (index, chunk) in chunks.iter().enumerate() {
			let page_no = PageNo::new(
				first
					.get()
					.checked_add(u64::try_from(index).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("cross-level page index exceeds u64")
					})?)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("cross-level page number overflows")
					})?,
			);
			let next = if index + 1 < chunks.len() {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("cross-level page link overflows")
				})?))
			} else {
				None
			};
			let page = encode_cross_level_overflow(page_no, PageLsn::ZERO, next, chunk)?;
			self.pages.push(page);
		}
		Ok(Some(first))
	}
}

pub(super) fn build_graph_image(
	root: &Path,
	plan: &PlannedGraph,
	dictionary: &mut PropertyDictionary,
	tables: &[LocalCommitTable],
	memberships: &[LocalCommitTableMembership],
) -> FerrosGrynnResult<BuiltGraphImage> {
	let graph_id = plan.graph_id;
	let record_id = &plan.record_id;
	let graph = &plan.graph;
	let node_ids = GraphRead::node_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let edge_ids = GraphRead::edge_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let live_atoms = node_ids.iter().chain(&edge_ids).copied().collect::<BTreeSet<_>>();
	let mut table_memberships = BTreeMap::new();
	for membership in memberships {
		let table_id = membership.table_id.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"complete graph membership contains an explicit empty assignment",
			)
		})?;
		if !live_atoms.contains(&membership.id) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"table membership references an atom absent from the graph",
			));
		}
		let table = tables
			.iter()
			.find(|table| table.id == table_id && table.graph.as_ref() == Some(record_id))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"table membership references an absent or foreign table",
				)
			})?;
		if !table_kind_accepts(
			table.kind,
			membership.id.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("table membership has an invalid atom kind")
			})?,
		) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"table membership atom kind disagrees with its table",
			));
		}
		if table_memberships.insert(membership.id, table_id).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"complete graph membership contains a duplicate atom",
			));
		}
	}
	let mut disk_refs = BTreeMap::new();
	let mut context = EncodeContext::new(dictionary.clone());
	let mut table_names = BTreeSet::new();
	let mut node_labels = BTreeSet::new();
	let mut edge_labels = BTreeSet::new();
	for id in node_ids.iter().chain(&edge_ids) {
		let atom = graph
			.atom_ref(*id)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph atom disappeared"))?;
		let disk = DiskAtomRef::from_parts(atom.kind(), id.slot())?;
		context.register_atom(atom.clone(), disk)?;
		table_names.insert(atom.id().table().to_owned());
		disk_refs.insert(*id, disk);
	}
	for table in &table_names {
		context.register_table_name(table.clone().into())?;
	}
	for id in &node_ids {
		let node = graph
			.node(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph node disappeared"))?;
		if let Some(label) = node.label() {
			node_labels.insert(label.to_owned());
		}
	}
	for id in &edge_ids {
		let edge = graph
			.edge(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph edge disappeared"))?;
		if let Some(label) = edge.label() {
			edge_labels.insert(label.to_owned());
		}
	}

	let mut primary_sorter = ExternalIndexSorter::new(root);
	for id in node_ids.iter().chain(&edge_ids) {
		let atom = graph
			.atom_ref(*id)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph atom disappeared"))?;
		let key = CanonicalCodec::encode(
			StorageValue::from_runtime(Value::Record(Box::new(atom.id().clone())), &mut context)?
				.as_ref(),
		)?;
		let atom = disk_refs.get(id).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("atom has no assigned disk reference")
		})?;
		primary_sorter.push(key, atom)?;
	}
	let primary_records = primary_sorter.finish()?;
	let primary = build_primary_index_sorted(&primary_records, PageLsn::ZERO)?;

	let mut properties = PropertyStoreBuilder::new();
	let graph_properties =
		encode_optional_properties(graph.properties(), &mut context, &mut properties)?;
	let mut node_properties = BTreeMap::new();
	for id in &node_ids {
		let node = graph.node(*id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph node disappeared during property encoding",
			)
		})?;
		node_properties.insert(
			*id,
			encode_optional_properties(node.properties(), &mut context, &mut properties)?,
		);
	}
	let mut edge_properties = BTreeMap::new();
	for id in &edge_ids {
		let edge = graph.edge(*id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph edge disappeared during property encoding",
			)
		})?;
		edge_properties.insert(
			*id,
			encode_optional_properties(edge.properties(), &mut context, &mut properties)?,
		);
	}
	let property_pages = properties.finish()?;
	let indexes = build_table_indexes(
		root,
		record_id,
		graph,
		tables,
		&table_memberships,
		&disk_refs,
		&mut context,
	)?;

	let node_label_ids = dense_ids(node_labels)?;
	let edge_label_ids = dense_ids(edge_labels)?;
	let label_indexes = build_label_indexes(
		graph,
		&node_ids,
		&edge_ids,
		&disk_refs,
		&node_label_ids,
		&edge_label_ids,
	)?;
	let labels = LabelDictionary {
		node_labels: node_label_ids.keys().cloned().collect(),
		edge_labels: edge_label_ids.keys().cloned().collect(),
		property_keys: context
			.property_dictionary()
			.iter()
			.map(|(_id, name)| name.as_str().to_owned())
			.collect(),
		table_names: context.table_names().map(|name| name.as_str().to_owned()).collect(),
		time_zones: context.time_zone_names().map(|name| name.as_str().to_owned()).collect(),
	};
	let label_pages = encode_label_dictionary(&labels, PageLsn::ZERO)?;
	*dictionary = context.property_dictionary().clone();

	let mut node_adjacency =
		node_ids.iter().map(|id| (*id, NodeAdjacency::default())).collect::<BTreeMap<_, _>>();
	let mut encoded_topologies = BTreeMap::new();
	for id in &edge_ids {
		let edge = graph
			.edge(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph edge disappeared"))?;
		let topology = encode_topology(edge.topology(), &disk_refs, plan)?;
		record_node_adjacency(
			edge.topology(),
			disk_refs.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge has no assigned disk reference")
			})?,
			&mut node_adjacency,
		)?;
		encoded_topologies.insert(*id, topology);
	}

	let node_capacity = u64::try_from(graph.slot_capacity(AtomKind::Node)).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("node slot capacity exceeds u64")
	})?;
	let edge_capacity = u64::try_from(graph.slot_capacity(AtomKind::Edge)).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("edge slot capacity exceeds u64")
	})?;
	let node_page_count = page_count_for_slots(node_capacity)?;
	let edge_page_count = page_count_for_slots(edge_capacity)?;
	let mut node_pages = make_hot_pages(node_page_count, new_node_hot_page)?;
	let mut edge_pages = make_hot_pages(edge_page_count, new_edge_hot_page)?;
	for slot in 0..graph.slot_capacity(AtomKind::Node) {
		let generation = graph.slot_generation(AtomKind::Node, slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node slot generation is absent")
		})?;
		let slot = u64::try_from(slot)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("node slot exceeds u64"))?;
		let (page_no, within_page) = hot_slot_address(slot)?;
		let page_index = checked_page_index(page_no, node_pages.len())?;
		write_node_hot_slot(
			&mut node_pages[page_index],
			within_page,
			&NodeHotSlot::tombstone(generation),
		)?;
	}
	for slot in 0..graph.slot_capacity(AtomKind::Edge) {
		let generation = graph.slot_generation(AtomKind::Edge, slot).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge slot generation is absent")
		})?;
		let slot = u64::try_from(slot)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge slot exceeds u64"))?;
		let (page_no, within_page) = hot_slot_address(slot)?;
		let page_index = checked_page_index(page_no, edge_pages.len())?;
		write_edge_hot_slot(
			&mut edge_pages[page_index],
			within_page,
			&EdgeHotSlot::tombstone(generation),
		)?;
	}
	let mut overflow = OverflowStoreBuilder::default();
	let mut cross_level_overflow = CrossLevelOverflowStoreBuilder::default();
	for id in &node_ids {
		let node = graph
			.node(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph node disappeared"))?;
		let adjacency = node_adjacency.get(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("node adjacency was not initialized")
		})?;
		let subgraph_slot =
			plan.children.iter().find(|child| child.container == *id).map(|child| child.slot);
		let value = NodeHotSlot {
			generation: id.generation(),
			subgraph_slot,
			label_id: node
				.label()
				.map(|label| {
					node_label_ids.get(label).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("node label was not interned")
					})
				})
				.transpose()?,
			table_id: table_memberships.get(id).copied(),
			out_count: checked_u32(adjacency.outgoing.len(), "outgoing adjacency")?,
			in_count: checked_u32(adjacency.incoming.len(), "incoming adjacency")?,
			undirected_count: checked_u32(adjacency.undirected.len(), "undirected adjacency")?,
			flags: if subgraph_slot.is_some() {
				NodeSlotFlags::META
			} else {
				NodeSlotFlags::empty()
			},
			out_overflow: overflow.append(adjacency.outgoing.get(4..).unwrap_or(&[]))?,
			in_overflow: overflow.append(adjacency.incoming.get(4..).unwrap_or(&[]))?,
			undirected_overflow: overflow.append(adjacency.undirected.get(2..).unwrap_or(&[]))?,
			properties: node_properties.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("node property location is absent")
			})?,
			out_inline: inline_refs(&adjacency.outgoing),
			in_inline: inline_refs(&adjacency.incoming),
			undirected_inline: inline_refs(&adjacency.undirected),
		};
		let (page_no, slot) = hot_slot_address(id.slot())?;
		let page_index = checked_page_index(page_no, node_pages.len())?;
		write_node_hot_slot(&mut node_pages[page_index], slot, &value)?;
	}
	for (index, page) in node_pages.iter_mut().enumerate() {
		page.seal()?;
		page.validate(PageNo::new(u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("node page index exceeds u64")
		})?))?;
	}
	for id in &edge_ids {
		let edge = graph
			.edge(*id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph edge disappeared"))?;
		let topology = encoded_topologies.get(id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge topology was not encoded")
		})?;
		let weight = edge
			.weight()
			.map(|weight| {
				weight.as_f64().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("edge weight cannot be represented as f64")
				})
			})
			.transpose()?;
		let cross_level =
			topology.source.iter().chain(&topology.target).any(|endpoint| endpoint.graph_slot != 0);
		let source = encode_hot_endpoint_side(
			&topology.source,
			EndpointSide::Source,
			cross_level,
			&mut overflow,
			&mut cross_level_overflow,
		)?;
		let target = encode_hot_endpoint_side(
			&topology.target,
			EndpointSide::Target,
			cross_level,
			&mut overflow,
			&mut cross_level_overflow,
		)?;
		let transition_graph =
			plan.children.iter().find(|child| child.container == *id).map(|child| child.slot);
		let mut flags = EdgeSlotFlags::empty();
		if transition_graph.is_some() {
			flags |= EdgeSlotFlags::META;
		}
		if cross_level {
			flags |= EdgeSlotFlags::CROSS_LEVEL;
		}
		let value = EdgeHotSlot {
			topology: topology.code,
			flags,
			has_transition: transition_graph.is_some(),
			generation: id.generation(),
			source_count: checked_u32(topology.source.len(), "edge sources")?,
			target_count: checked_u32(topology.target.len(), "edge targets")?,
			label_id: edge
				.label()
				.map(|label| {
					edge_label_ids.get(label).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("edge label was not interned")
					})
				})
				.transpose()?,
			table_id: table_memberships.get(id).copied(),
			weight,
			transition_graph,
			source_graph: source.graph_slot,
			target_graph: target.graph_slot,
			properties: edge_properties.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge property location is absent")
			})?,
			source_overflow: source.local_overflow,
			target_overflow: target.local_overflow,
			source_graph_overflow: source.graph_overflow,
			target_graph_overflow: target.graph_overflow,
			source_inline: source.inline,
			target_inline: target.inline,
		};
		let (page_no, slot) = hot_slot_address(id.slot())?;
		let page_index = checked_page_index(page_no, edge_pages.len())?;
		write_edge_hot_slot(&mut edge_pages[page_index], slot, &value)?;
	}
	for (index, page) in edge_pages.iter_mut().enumerate() {
		page.seal()?;
		page.validate(PageNo::new(u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("edge page index exceeds u64")
		})?))?;
	}

	let statistic_fields = tables
		.iter()
		.filter(|table| table.graph.as_ref() == Some(record_id))
		.flat_map(|table| {
			table.fields.iter().map(|field| StatisticField {
				table_id: table.id,
				field_id: field.id,
				property_key: PropertyKeyId::new(field.property_key),
				value_type: &field.value_type,
			})
		})
		.collect::<Vec<_>>();
	let statistics = analyze_graph(
		graph,
		graph.revision(),
		&statistic_fields,
		|id| table_memberships.get(&id).copied(),
		&mut context,
	)?;
	let statistic_files =
		encode_statistics(&statistics, &labels.node_labels, &labels.edge_labels, PageLsn::ZERO)?;
	let subgraph_entries = plan
		.children
		.iter()
		.map(|child| {
			Ok(SubgraphEntry {
				slot: child.slot,
				generation: child.container.generation(),
				node_capacity: u64::try_from(child.graph.graph.slot_capacity(AtomKind::Node))
					.map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("nested node capacity exceeds u64")
					})?,
				edge_capacity: u64::try_from(child.graph.graph.slot_capacity(AtomKind::Edge))
					.map_err(|_error| {
						FerrosGrynnError::invalid_storage_file("nested edge capacity exceeds u64")
					})?,
				node_count: checked_u32(
					GraphRead::node_count(&child.graph.graph)?,
					"nested node count",
				)?,
				edge_count: checked_u32(
					GraphRead::edge_count(&child.graph.graph)?,
					"nested edge count",
				)?,
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let subgraph_pages = encode_subgraph_pages(&subgraph_entries)?;
	let graph_name = plan.name.clone();
	let now = unix_time_ms()?;
	let superblock = GraphSuperblock {
		graph_id,
		generation: CheckpointGeneration::ZERO,
		backfilled_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
		created_at_ms: now,
		updated_at_ms: now,
		node_capacity,
		edge_capacity,
		node_count: u64::try_from(node_ids.len())
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds u64"))?,
		edge_count: u64::try_from(edge_ids.len())
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds u64"))?,
		subgraph_capacity: subgraph_entries.last().map_or(0, |entry| u64::from(entry.slot) + 1),
		subgraph_count: u64::try_from(subgraph_entries.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("subgraph count exceeds u64")
		})?,
		node_hot_pages: u64::try_from(node_pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("node page count exceeds u64")
		})?,
		edge_hot_pages: u64::try_from(edge_pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("edge page count exceeds u64")
		})?,
		properties_pages: u64::try_from(property_pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("property page count exceeds u64")
		})?,
		labels_size_bytes: u64::try_from(label_pages.len())
			.ok()
			.and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label file size overflows"))?,
		flags: GraphSuperblockFlags::empty(),
		schema_root_page: PageNo::NULL,
		stats_root_page: PageNo::new(0),
		graph_name,
	};

	let mut payloads = BTreeMap::new();
	payloads.insert(DataFileKind::GraphSuperblock, vec![superblock.encode()?]);
	payloads.insert(DataFileKind::NodesHot, node_pages);
	payloads.insert(DataFileKind::EdgesHot, edge_pages);
	let overflow_pages = overflow.finish()?;
	payloads.insert(DataFileKind::AdjacencyOverflow, overflow_pages);
	payloads.insert(DataFileKind::CrossLevelAdjacencyOverflow, cross_level_overflow.pages);
	payloads.insert(DataFileKind::Properties, property_pages);
	payloads.insert(DataFileKind::Subgraphs, subgraph_pages);
	payloads.insert(DataFileKind::Labels, label_pages);
	payloads.insert(DataFileKind::Statistics, statistic_files.graph);
	payloads.insert(DataFileKind::LabelStatistics, statistic_files.labels);
	payloads.insert(DataFileKind::PropertyStatistics, statistic_files.properties);
	payloads.insert(DataFileKind::Index, primary.pages);

	let identity = graph.identity();
	let schema = plan.schema;
	Ok(BuiltGraphImage {
		catalog_entry: GraphCatalogEntry {
			graph_id,
			schema_id: schema.map_or(0, |schema| schema.id),
			schema_version: schema.map_or(0, |schema| schema.version),
			label_index: schema.map(|schema| SystemIndexCatalogEntry {
				index_id: schema.label_index.id,
				schema_version: schema.label_index.version,
			}),
			adjacency_index: schema.map(|schema| SystemIndexCatalogEntry {
				index_id: schema.adjacency_index.id,
				schema_version: schema.adjacency_index.version,
			}),
			parent_graph_id: plan.parent_graph_id,
			subgraph_slot: plan.subgraph_slot,
			parent_atom: plan.parent_atom,
			record_id: record_id.to_string(),
			name: plan.name.clone(),
			kind: plan.kind,
			created_lsn: Lsn::ZERO,
			identity: *identity.as_inner().as_bytes(),
			revision: graph.revision(),
			primary_index_root: primary.root,
			primary_index_file_id: DataFileId::for_kind(DataFileKind::Index),
			properties: graph_properties,
		},
		superblock,
		payloads,
		indexes,
		label_indexes,
	})
}

pub(super) fn build_node_append_delta(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	current: &GraphCatalogEntry,
	record_id: &RecordId,
	name: &str,
	base: &LocalGraph,
	nodes: &[LocalCommitNode],
	dictionary: &mut PropertyDictionary,
) -> FerrosGrynnResult<BuiltGraphDelta> {
	if nodes.is_empty()
		|| current.parent_graph_id.is_some()
		|| current.identity != *base.identity.as_inner().as_bytes()
		|| current.revision != base.revision
		|| current.graph_id != base.graph_id
		|| current.record_id != record_id.to_string()
		|| !base.supports_atom_delta()
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"node append delta does not match its durable base graph",
		));
	}

	let node_count = u64::try_from(nodes.len())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("node delta exceeds u64"))?;
	let next_capacity = base
		.superblock
		.node_capacity
		.checked_add(node_count)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("node capacity overflows u64"))?;
	let next_revision = base
		.revision
		.checked_add(node_count)
		.ok_or_else(|| FerrosGrynnError::graph_capacity_exceeded("revision".to_owned()))?;
	for (offset, node) in nodes.iter().enumerate() {
		let expected = base
			.superblock
			.node_capacity
			.checked_add(u64::try_from(offset).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("node delta offset exceeds u64")
			})?)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("node slot overflows u64"))?;
		if node.id.kind() != Some(AtomKind::Node)
			|| node.id.slot() != expected
			|| node.id.generation() != 0
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"node append delta contains a non-contiguous slot",
			));
		}
	}

	let mut context = EncodeContext::new(dictionary.clone());
	for (expected, table) in base.labels.table_names.iter().enumerate() {
		let actual = context.register_table_name(table.clone().into())?;
		if usize::try_from(actual).ok() != Some(expected) {
			return Err(FerrosGrynnError::invalid_storage_file("table dictionary is not dense"));
		}
	}
	for (index, zone) in base.labels.time_zones.iter().enumerate() {
		let actual = context.register_time_zone_name(zone.clone().into())?;
		let expected = u32::try_from(index)
			.ok()
			.and_then(|index| index.checked_add(1))
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("time-zone id overflows"))?;
		if actual != expected {
			return Err(FerrosGrynnError::invalid_storage_file(
				"time-zone dictionary is not dense",
			));
		}
	}
	for table in nodes.iter().map(|node| node.record_id.table()).collect::<BTreeSet<_>>() {
		context.register_table_name(table.to_owned().into())?;
	}

	// ponytail: the first physical delta still bulk-rebuilds the primary
	// B+tree. Replace this with right-edge incremental insertion only if this
	// measured ceiling dominates after full graph materialization is gone.
	let mut primary_entries = scan_primary_index(
		|page_no, output| {
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::Index, page_no)?)?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		},
		base.primary_index_root,
	)?;
	for entry in &primary_entries {
		let stored = CanonicalCodec::decode(&entry.key, DecodeLimits::default())?;
		let Value::Record(record) = stored.into_runtime(&context.decoder())? else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index key is not a canonical RecordId",
			));
		};
		context.register_atom(AtomRef::new(entry.atom.kind(), *record), entry.atom)?;
	}
	for node in nodes {
		let atom = DiskAtomRef::from_parts(AtomKind::Node, node.id.slot())?;
		context.register_atom(AtomRef::new(AtomKind::Node, node.record_id.clone()), atom)?;
		let key = CanonicalCodec::encode(
			StorageValue::from_runtime(
				Value::Record(Box::new(node.record_id.clone())),
				&mut context,
			)?
			.as_ref(),
		)?;
		primary_entries.push(PrimaryIndexEntry {
			key,
			atom,
		});
	}
	primary_entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	let primary = build_primary_index(&primary_entries, PageLsn::ZERO)?;
	for (index, page) in primary.pages.iter().enumerate() {
		page.validate(PageNo::new(u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("primary page index exceeds u64")
		})?))?;
	}

	let mut labels = base.labels.clone();
	for label in nodes
		.iter()
		.filter_map(|node| node.label.as_ref().map(|label| label.as_str()))
		.collect::<BTreeSet<_>>()
	{
		if !labels.node_labels.iter().any(|existing| existing == label) {
			labels.node_labels.push(label.to_owned());
		}
	}
	let node_label_ids = labels
		.node_labels
		.iter()
		.enumerate()
		.map(|(index, label)| {
			u32::try_from(index).map(|index| (label.as_str(), index)).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("node label id exceeds u32")
			})
		})
		.collect::<FerrosGrynnResult<BTreeMap<_, _>>>()?;
	let mut property_store = PropertyStoreBuilder::starting_at(base.superblock.properties_pages);
	let mut node_pages = BTreeMap::<PageNo, Box<AlignedPage>>::new();
	for node in nodes {
		let (page_no, within_page) = hot_slot_address(node.id.slot())?;
		if !node_pages.contains_key(&page_no) {
			let page = if page_no.get() < base.superblock.node_hot_pages {
				let current = manager
					.read_page(transaction, base.page_key(DataFileKind::NodesHot, page_no)?)?;
				let mut page = Box::new(AlignedPage::zeroed());
				page.as_bytes_mut().copy_from_slice(current.as_bytes());
				page.assign_lsn(page_no, PageLsn::ZERO)?;
				page
			} else {
				boxed_page(page_no, &new_node_hot_page(page_no, PageLsn::ZERO)?)?
			};
			node_pages.insert(page_no, page);
		}
		let page = node_pages
			.get(&page_no)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("node delta page is absent"))?;
		if !read_node_hot_slot(page, page_no, within_page)?.flags.contains(NodeSlotFlags::TOMBSTONE)
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"node append delta targets a live slot",
			));
		}
	}
	for node in nodes {
		let properties =
			encode_optional_properties(&node.properties, &mut context, &mut property_store)?;
		let (page_no, within_page) = hot_slot_address(node.id.slot())?;
		let page = node_pages
			.get_mut(&page_no)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("node delta page is absent"))?;
		write_node_hot_slot(page, within_page, &NodeHotSlot {
			generation: node.id.generation(),
			subgraph_slot: None,
			label_id: node
				.label
				.as_ref()
				.map(|label| {
					node_label_ids.get(label.as_str()).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("node label was not interned")
					})
				})
				.transpose()?,
			table_id: node.table_id,
			out_count: 0,
			in_count: 0,
			undirected_count: 0,
			flags: NodeSlotFlags::empty(),
			out_overflow: None,
			in_overflow: None,
			undirected_overflow: None,
			properties,
			out_inline: [None; 4],
			in_inline: [None; 4],
			undirected_inline: [None; 2],
		})?;
	}
	for (page_no, page) in &mut node_pages {
		page.seal()?;
		page.validate(*page_no)?;
	}
	let property_pages = property_store.finish_numbered()?;

	labels.property_keys = context
		.property_dictionary()
		.iter()
		.map(|(_id, property)| property.as_str().to_owned())
		.collect();
	labels.table_names = context.table_names().map(|table| table.as_str().to_owned()).collect();
	labels.time_zones = context.time_zone_names().map(|zone| zone.as_str().to_owned()).collect();
	let label_pages = encode_label_dictionary(&labels, PageLsn::ZERO)?;
	*dictionary = context.property_dictionary().clone();

	let mut previous_statistics = base.statistics_with_labels(manager, transaction)?;
	hydrate_legacy_label_statistics(
		&mut previous_statistics,
		manager,
		transaction,
		base,
		&context,
	)?;
	if previous_statistics.node_count != base.superblock.node_count
		|| previous_statistics.edge_count != base.superblock.edge_count
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics counts disagree with GraphSuperblock",
		));
	}
	let next_node_count = base
		.superblock
		.node_count
		.checked_add(node_count)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("node count overflows u64"))?;
	let mut next_statistics = previous_statistics;
	next_statistics.node_count = next_node_count;
	for node in nodes {
		if let Some(table_id) = node.table_id {
			let count = next_statistics.tables.entry(table_id).or_default();
			*count = count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("table statistics count overflows")
			})?;
		}
		if let Some(label) = &node.label {
			next_statistics.replace_label(
				AtomKind::Node,
				None,
				Some((label, node.properties.len())),
			)?;
		}
	}
	next_statistics.mark_dirty(node_count, next_revision)?;
	let (statistics_root, label_statistics) = encode_statistics_delta(
		&next_statistics,
		&labels.node_labels,
		&labels.edge_labels,
		base.statistics_layout,
		PageLsn::ZERO,
	)?;
	let index_payloads = build_count_index_delta(manager, transaction, base, &next_statistics)?;

	let mut superblock = base.superblock.clone();
	if nodes.iter().any(|node| node.label.is_some()) {
		superblock.flags.insert(GraphSuperblockFlags::LABEL_INDEX_STALE);
	}
	superblock.updated_at_ms = unix_time_ms()?;
	superblock.node_capacity = next_capacity;
	superblock.node_count = next_node_count;
	superblock.node_hot_pages = u64::try_from(page_count_for_slots(next_capacity)?)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("node page count exceeds u64"))?;
	superblock.properties_pages = property_pages
		.last()
		.map_or(Some(base.superblock.properties_pages), |(page_no, _page)| {
			page_no.get().checked_add(1)
		})
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("property page count overflows"))?;
	superblock.labels_size_bytes = u64::try_from(label_pages.len())
		.ok()
		.and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label file size overflows"))?;
	superblock.graph_name = name.to_owned();

	let mut catalog_entry = current.clone();
	catalog_entry.name = name.to_owned();
	catalog_entry.revision = next_revision;
	catalog_entry.primary_index_root = primary.root;

	let mut payloads = BTreeMap::new();
	payloads.insert(DataFileKind::NodesHot, node_pages.into_iter().collect());
	if !property_pages.is_empty() {
		payloads.insert(DataFileKind::Properties, property_pages);
	}
	payloads.insert(DataFileKind::Labels, number_pages(label_pages)?);
	payloads.insert(DataFileKind::Statistics, vec![(PageNo::new(0), statistics_root)]);
	payloads.insert(DataFileKind::LabelStatistics, number_pages(label_statistics)?);
	payloads.insert(DataFileKind::Index, number_pages(primary.pages)?);
	Ok(BuiltGraphDelta {
		catalog_entry,
		superblock,
		payloads,
		index_payloads,
		removed_subgraph_slots: Vec::new(),
	})
}

pub(super) fn build_atom_delta(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	current: &GraphCatalogEntry,
	record_id: &RecordId,
	name: &str,
	base: &LocalGraph,
	delta: &LocalCommitAtomDelta,
	dictionary: &mut PropertyDictionary,
) -> FerrosGrynnResult<BuiltGraphDelta> {
	if delta.nodes.is_empty() && delta.edges.is_empty() && delta.memberships.is_empty()
		|| current.parent_graph_id.is_some()
		|| current.identity != *base.identity.as_inner().as_bytes()
		|| current.revision != base.revision
		|| current.graph_id != base.graph_id
		|| current.record_id != record_id.to_string()
		|| !base.supports_atom_delta()
		|| delta.node_capacity < base.superblock.node_capacity
		|| delta.edge_capacity < base.superblock.edge_capacity
		|| delta.revision <= base.revision
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"atom delta does not match its durable base graph",
		));
	}
	validate_allocated_slots(
		AtomKind::Node,
		base.superblock.node_capacity,
		delta.node_capacity,
		delta.nodes.iter().filter_map(|change| match change {
			LocalCommitNodeChange::Upsert {
				id,
				created: true,
				..
			}
			| LocalCommitNodeChange::Remove {
				id,
				created: true,
				..
			} => Some(*id),
			_ => None,
		}),
	)?;
	validate_allocated_slots(
		AtomKind::Edge,
		base.superblock.edge_capacity,
		delta.edge_capacity,
		delta.edges.iter().filter_map(|change| match change {
			LocalCommitEdgeChange::Upsert {
				id,
				created: true,
				..
			}
			| LocalCommitEdgeChange::Remove {
				id,
				created: true,
				..
			} => Some(*id),
			_ => None,
		}),
	)?;

	let mut source_context = EncodeContext::new(dictionary.clone());
	let mut context = EncodeContext::new(dictionary.clone());
	for (expected, table) in base.labels.table_names.iter().enumerate() {
		let source = source_context.register_table_name(table.clone().into())?;
		let target = context.register_table_name(table.clone().into())?;
		if usize::try_from(source).ok() != Some(expected)
			|| usize::try_from(target).ok() != Some(expected)
		{
			return Err(FerrosGrynnError::invalid_storage_file("table dictionary is not dense"));
		}
	}
	for (index, zone) in base.labels.time_zones.iter().enumerate() {
		let source = source_context.register_time_zone_name(zone.clone().into())?;
		let target = context.register_time_zone_name(zone.clone().into())?;
		let expected = u32::try_from(index)
			.ok()
			.and_then(|index| index.checked_add(1))
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("time-zone id overflows"))?;
		if source != expected || target != expected {
			return Err(FerrosGrynnError::invalid_storage_file(
				"time-zone dictionary is not dense",
			));
		}
	}

	// ponytail: the format-v1 primary index is still replaced as one image.
	// Q-05 owns incremental insertion/removal after point-DML profiles exist.
	let mut primary_entries = scan_primary_index(
		|page_no, output| {
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::Index, page_no)?)?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		},
		base.primary_index_root,
	)?;
	let mut source_atoms = Vec::with_capacity(primary_entries.len());
	for entry in &primary_entries {
		let stored = CanonicalCodec::decode(&entry.key, DecodeLimits::default())?;
		let Value::Record(record) = stored.into_runtime(&source_context.decoder())? else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index key is not a canonical RecordId",
			));
		};
		let runtime = AtomRef::new(entry.atom.kind(), *record);
		source_context.register_atom(runtime.clone(), entry.atom)?;
		source_atoms.push((runtime, entry.atom));
	}
	let removed = delta
		.nodes
		.iter()
		.filter_map(|change| match change {
			LocalCommitNodeChange::Remove {
				id,
				created: false,
				..
			} => DiskAtomRef::from_parts(AtomKind::Node, id.slot()).ok(),
			_ => None,
		})
		.chain(delta.edges.iter().filter_map(|change| match change {
			LocalCommitEdgeChange::Remove {
				id,
				created: false,
				..
			} => DiskAtomRef::from_parts(AtomKind::Edge, id.slot()).ok(),
			_ => None,
		}))
		.collect::<BTreeSet<_>>();
	primary_entries.retain(|entry| !removed.contains(&entry.atom));
	for (runtime, atom) in source_atoms {
		if !removed.contains(&atom) {
			context.register_atom(runtime, atom)?;
		}
	}
	for (id, record) in delta_created_records(delta) {
		context.register_table_name(record.table().to_owned().into())?;
		let atom = DiskAtomRef::from_parts(
			id.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("created atom has no kind")
			})?,
			id.slot(),
		)?;
		context.register_atom(AtomRef::new(atom.kind(), record.clone()), atom)?;
		let key = CanonicalCodec::encode(
			StorageValue::from_runtime(Value::Record(Box::new(record.clone())), &mut context)?
				.as_ref(),
		)?;
		primary_entries.push(PrimaryIndexEntry {
			key,
			atom,
		});
	}
	primary_entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	if primary_entries.windows(2).any(|entries| entries[0].key == entries[1].key) {
		return Err(FerrosGrynnError::graph_constraint_violation(
			"record id",
			"atom delta contains a duplicate RecordId",
		));
	}
	let primary = build_primary_index(&primary_entries, PageLsn::ZERO)?;

	let mut labels = base.labels.clone();
	for label in delta
		.nodes
		.iter()
		.filter_map(|change| match change {
			LocalCommitNodeChange::Upsert {
				label: Some(label),
				..
			} => Some(label.as_str()),
			_ => None,
		})
		.collect::<BTreeSet<_>>()
	{
		if !labels.node_labels.iter().any(|existing| existing == label) {
			labels.node_labels.push(label.to_owned());
		}
	}
	for label in delta
		.edges
		.iter()
		.filter_map(|change| match change {
			LocalCommitEdgeChange::Upsert {
				label: Some(label),
				..
			} => Some(label.as_str()),
			_ => None,
		})
		.collect::<BTreeSet<_>>()
	{
		if !labels.edge_labels.iter().any(|existing| existing == label) {
			labels.edge_labels.push(label.to_owned());
		}
	}
	let node_label_ids = indexed_strings(&labels.node_labels, "node label")?;
	let edge_label_ids = indexed_strings(&labels.edge_labels, "edge label")?;
	let mut property_store = PropertyStoreBuilder::starting_at(base.superblock.properties_pages);
	let mut node_properties = BTreeMap::new();
	for change in &delta.nodes {
		if let LocalCommitNodeChange::Upsert {
			id,
			properties,
			..
		} = change
		{
			node_properties.insert(
				*id,
				encode_optional_properties(properties, &mut context, &mut property_store)?,
			);
		}
	}
	let mut edge_properties = BTreeMap::new();
	for change in &delta.edges {
		if let LocalCommitEdgeChange::Upsert {
			id,
			properties,
			..
		} = change
		{
			edge_properties.insert(
				*id,
				encode_optional_properties(properties, &mut context, &mut property_store)?,
			);
		}
	}
	let property_pages = property_store.finish_numbered()?;
	labels.property_keys = context
		.property_dictionary()
		.iter()
		.map(|(_id, property)| property.as_str().to_owned())
		.collect();
	labels.table_names = context.table_names().map(|table| table.as_str().to_owned()).collect();
	labels.time_zones = context.time_zone_names().map(|zone| zone.as_str().to_owned()).collect();
	let label_pages = encode_label_dictionary(&labels, PageLsn::ZERO)?;
	*dictionary = context.property_dictionary().clone();

	let node_page_count = u64::try_from(page_count_for_slots(delta.node_capacity)?)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("node page count exceeds u64"))?;
	let edge_page_count = u64::try_from(page_count_for_slots(delta.edge_capacity)?)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge page count exceeds u64"))?;
	let mut node_pages = BTreeMap::<PageNo, Box<AlignedPage>>::new();
	let mut edge_pages = BTreeMap::<PageNo, Box<AlignedPage>>::new();
	let mut removed_subgraph_slots = Vec::new();
	let mut label_index_changed = false;

	for change in &delta.nodes {
		let (id, created) = match change {
			LocalCommitNodeChange::Upsert {
				id,
				created,
				..
			}
			| LocalCommitNodeChange::Remove {
				id,
				created,
				..
			} => (*id, *created),
		};
		let (page_no, within) = hot_slot_address(id.slot())?;
		let page = delta_hot_page(
			&mut node_pages,
			manager,
			transaction,
			base,
			DataFileKind::NodesHot,
			page_no,
			base.superblock.node_hot_pages,
			new_node_hot_page,
		)?;
		let previous = read_node_hot_slot(page, page_no, within)?;
		let previous_label_id = previous.label_id;
		validate_delta_slot(
			id,
			created,
			previous.generation,
			previous.flags.contains(NodeSlotFlags::TOMBSTONE),
		)?;
		match change {
			LocalCommitNodeChange::Remove {
				..
			} => {
				label_index_changed |= previous.label_id.is_some();
				if let Some(slot) = previous.subgraph_slot {
					removed_subgraph_slots.push(slot);
				}
				write_node_hot_slot(page, within, &NodeHotSlot::tombstone(next_generation(id)?))?;
				page.seal()?;
			}
			LocalCommitNodeChange::Upsert {
				record_id: _,
				label,
				..
			} => {
				let mut next = if created {
					NodeHotSlot::tombstone(id.generation())
				} else {
					previous
				};
				next.generation = id.generation();
				next.flags.remove(NodeSlotFlags::TOMBSTONE);
				let next_label_id = label
					.as_ref()
					.map(|label| {
						node_label_ids.get(label.as_str()).copied().ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file("node label was not interned")
						})
					})
					.transpose()?;
				label_index_changed |= if created {
					next_label_id.is_some()
				} else {
					previous_label_id != next_label_id
				};
				next.label_id = next_label_id;
				next.properties = node_properties.get(&id).copied().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("node property location is absent")
				})?;
				write_node_hot_slot(page, within, &next)?;
				page.seal()?;
			}
		}
	}

	let overflow_file = base.file_id(DataFileKind::AdjacencyOverflow)?;
	let overflow_start =
		manager.visible_file_page_count(transaction, base.graph_id, overflow_file)?;
	let mut overflow = OverflowStoreBuilder::starting_at(overflow_start);
	let mut cross_level_overflow = CrossLevelOverflowStoreBuilder::default();
	for adjacency in &delta.adjacency {
		let (page_no, within) = hot_slot_address(adjacency.id.slot())?;
		let page = delta_hot_page(
			&mut node_pages,
			manager,
			transaction,
			base,
			DataFileKind::NodesHot,
			page_no,
			base.superblock.node_hot_pages,
			new_node_hot_page,
		)?;
		let mut slot = read_node_hot_slot(page, page_no, within)?;
		if slot.flags.contains(NodeSlotFlags::TOMBSTONE)
			|| slot.generation != adjacency.id.generation()
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"atom delta adjacency targets a stale node",
			));
		}
		let outgoing = delta_edge_refs(&adjacency.outgoing)?;
		let incoming = delta_edge_refs(&adjacency.incoming)?;
		let undirected = delta_edge_refs(&adjacency.undirected)?;
		slot.out_count = checked_u32(outgoing.len(), "outgoing adjacency")?;
		slot.in_count = checked_u32(incoming.len(), "incoming adjacency")?;
		slot.undirected_count = checked_u32(undirected.len(), "undirected adjacency")?;
		slot.out_inline = inline_refs(&outgoing);
		slot.in_inline = inline_refs(&incoming);
		slot.undirected_inline = inline_refs(&undirected);
		slot.out_overflow = overflow.append(outgoing.get(4..).unwrap_or(&[]))?;
		slot.in_overflow = overflow.append(incoming.get(4..).unwrap_or(&[]))?;
		slot.undirected_overflow = overflow.append(undirected.get(2..).unwrap_or(&[]))?;
		write_node_hot_slot(page, within, &slot)?;
		page.seal()?;
	}

	for change in &delta.edges {
		let (id, created) = match change {
			LocalCommitEdgeChange::Upsert {
				id,
				created,
				..
			}
			| LocalCommitEdgeChange::Remove {
				id,
				created,
				..
			} => (*id, *created),
		};
		let (page_no, within) = hot_slot_address(id.slot())?;
		let page = delta_hot_page(
			&mut edge_pages,
			manager,
			transaction,
			base,
			DataFileKind::EdgesHot,
			page_no,
			base.superblock.edge_hot_pages,
			new_edge_hot_page,
		)?;
		let previous = read_edge_hot_slot(page, page_no, within)?;
		let previous_label_id = previous.label_id;
		validate_delta_slot(
			id,
			created,
			previous.generation,
			previous.flags.contains(EdgeSlotFlags::TOMBSTONE),
		)?;
		match change {
			LocalCommitEdgeChange::Remove {
				..
			} => {
				label_index_changed |= previous.label_id.is_some();
				if let Some(slot) = previous.transition_graph {
					removed_subgraph_slots.push(slot);
				}
				write_edge_hot_slot(page, within, &EdgeHotSlot::tombstone(next_generation(id)?))?;
				page.seal()?;
			}
			LocalCommitEdgeChange::Upsert {
				record_id: _,
				topology,
				label,
				weight,
				topology_changed,
				..
			} => {
				let mut next = if created {
					EdgeHotSlot::tombstone(id.generation())
				} else {
					previous
				};
				next.generation = id.generation();
				next.flags.remove(EdgeSlotFlags::TOMBSTONE);
				let next_label_id = label
					.as_ref()
					.map(|label| {
						edge_label_ids.get(label.as_str()).copied().ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file("edge label was not interned")
						})
					})
					.transpose()?;
				label_index_changed |= if created {
					next_label_id.is_some()
				} else {
					previous_label_id != next_label_id
				};
				next.label_id = next_label_id;
				next.properties = edge_properties.get(&id).copied().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("edge property location is absent")
				})?;
				next.weight = weight
					.map(|weight| {
						weight.as_f64().ok_or_else(|| {
							FerrosGrynnError::invalid_argument(
								"edge weight cannot be represented as f64",
							)
						})
					})
					.transpose()?;
				if created || *topology_changed {
					let encoded = encode_delta_topology(topology)?;
					let source = encode_hot_endpoint_side(
						&encoded.source,
						EndpointSide::Source,
						false,
						&mut overflow,
						&mut cross_level_overflow,
					)?;
					let target = encode_hot_endpoint_side(
						&encoded.target,
						EndpointSide::Target,
						false,
						&mut overflow,
						&mut cross_level_overflow,
					)?;
					next.topology = encoded.code;
					next.source_count = checked_u32(encoded.source.len(), "edge sources")?;
					next.target_count = checked_u32(encoded.target.len(), "edge targets")?;
					next.source_inline = source.inline;
					next.target_inline = target.inline;
					next.source_overflow = source.local_overflow;
					next.target_overflow = target.local_overflow;
					next.source_graph = None;
					next.target_graph = None;
					next.source_graph_overflow = None;
					next.target_graph_overflow = None;
					next.flags.remove(EdgeSlotFlags::CROSS_LEVEL);
				}
				write_edge_hot_slot(page, within, &next)?;
				page.seal()?;
			}
		}
	}
	let mut membership_atoms = BTreeSet::new();
	for membership in &delta.memberships {
		if !membership_atoms.insert(membership.id) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"atom delta contains duplicate table memberships",
			));
		}
		if membership.table_id == Some(0) {
			return Err(FerrosGrynnError::invalid_storage_file("schema identity zero is reserved"));
		}
		let kind = membership.id.kind().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("table membership has an invalid atom kind")
		})?;
		let (file, page_count) = match kind {
			AtomKind::Node => (DataFileKind::NodesHot, base.superblock.node_hot_pages),
			AtomKind::Edge => (DataFileKind::EdgesHot, base.superblock.edge_hot_pages),
		};
		let pages = match kind {
			AtomKind::Node => &mut node_pages,
			AtomKind::Edge => &mut edge_pages,
		};
		let (page_no, within) = hot_slot_address(membership.id.slot())?;
		let page = match kind {
			AtomKind::Node => delta_hot_page(
				pages,
				manager,
				transaction,
				base,
				file,
				page_no,
				page_count,
				new_node_hot_page,
			)?,
			AtomKind::Edge => delta_hot_page(
				pages,
				manager,
				transaction,
				base,
				file,
				page_no,
				page_count,
				new_edge_hot_page,
			)?,
		};
		match kind {
			AtomKind::Node => {
				let mut slot = read_node_hot_slot(page, page_no, within)?;
				if slot.flags.contains(NodeSlotFlags::TOMBSTONE)
					|| slot.generation != membership.id.generation()
				{
					return Err(FerrosGrynnError::invalid_storage_file(
						"table membership targets a stale node",
					));
				}
				slot.table_id = membership.table_id;
				write_node_hot_slot(page, within, &slot)?;
			}
			AtomKind::Edge => {
				let mut slot = read_edge_hot_slot(page, page_no, within)?;
				if slot.flags.contains(EdgeSlotFlags::TOMBSTONE)
					|| slot.generation != membership.id.generation()
				{
					return Err(FerrosGrynnError::invalid_storage_file(
						"table membership targets a stale edge",
					));
				}
				slot.table_id = membership.table_id;
				write_edge_hot_slot(page, within, &slot)?;
			}
		}
	}
	for (page_no, page) in &mut node_pages {
		page.seal()?;
		page.validate(*page_no)?;
	}
	for (page_no, page) in &mut edge_pages {
		page.seal()?;
		page.validate(*page_no)?;
	}
	let overflow_pages = overflow.finish_numbered();
	if !cross_level_overflow.pages.is_empty() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"local atom delta unexpectedly produced cross-level overflow",
		));
	}

	let created_nodes = count_node_changes(&delta.nodes, true, false)?;
	let removed_nodes = count_node_changes(&delta.nodes, false, true)?;
	let created_edges = count_edge_changes(&delta.edges, true, false)?;
	let removed_edges = count_edge_changes(&delta.edges, false, true)?;
	let next_node_count = base
		.superblock
		.node_count
		.checked_add(created_nodes)
		.and_then(|count| count.checked_sub(removed_nodes))
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("node count delta is invalid"))?;
	let next_edge_count = base
		.superblock
		.edge_count
		.checked_add(created_edges)
		.and_then(|count| count.checked_sub(removed_edges))
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("edge count delta is invalid"))?;
	let statistics = delta_graph_statistics(
		manager,
		transaction,
		base,
		delta,
		next_node_count,
		next_edge_count,
		&source_context,
	)?;
	let (statistics_root, label_statistics) = encode_statistics_delta(
		&statistics,
		&labels.node_labels,
		&labels.edge_labels,
		base.statistics_layout,
		PageLsn::ZERO,
	)?;
	let index_payloads = build_count_index_delta(manager, transaction, base, &statistics)?;

	removed_subgraph_slots.sort_unstable();
	removed_subgraph_slots.dedup();
	let mut next_subgraph_count = None;
	let subgraph_pages = if removed_subgraph_slots.is_empty() {
		None
	} else {
		let mut entries = read_subgraph_directory_entries(manager, transaction, base)?
			.into_values()
			.collect::<Vec<_>>();
		entries.retain(|entry| !removed_subgraph_slots.contains(&entry.slot));
		next_subgraph_count = Some(u64::try_from(entries.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("subgraph count exceeds u64")
		})?);
		Some(encode_subgraph_pages(&entries)?)
	};

	let mut superblock = base.superblock.clone();
	if label_index_changed {
		superblock.flags.insert(GraphSuperblockFlags::LABEL_INDEX_STALE);
	}
	superblock.updated_at_ms = unix_time_ms()?;
	superblock.node_capacity = delta.node_capacity;
	superblock.edge_capacity = delta.edge_capacity;
	superblock.node_count = next_node_count;
	superblock.edge_count = next_edge_count;
	superblock.node_hot_pages = node_page_count;
	superblock.edge_hot_pages = edge_page_count;
	superblock.properties_pages = property_pages
		.last()
		.map_or(Some(base.superblock.properties_pages), |(page_no, _page)| {
			page_no.get().checked_add(1)
		})
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("property page count overflows"))?;
	superblock.labels_size_bytes = u64::try_from(label_pages.len())
		.ok()
		.and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label file size overflows"))?;
	if let Some(count) = next_subgraph_count {
		superblock.subgraph_count = count;
	}
	superblock.graph_name = name.to_owned();

	let mut catalog_entry = current.clone();
	catalog_entry.name = name.to_owned();
	catalog_entry.revision = delta.revision;
	catalog_entry.primary_index_root = primary.root;

	let mut payloads = BTreeMap::new();
	if !node_pages.is_empty() {
		payloads.insert(DataFileKind::NodesHot, node_pages.into_iter().collect());
	}
	if !edge_pages.is_empty() {
		payloads.insert(DataFileKind::EdgesHot, edge_pages.into_iter().collect());
	}
	if !property_pages.is_empty() {
		payloads.insert(DataFileKind::Properties, property_pages);
	}
	if !overflow_pages.is_empty() {
		payloads.insert(DataFileKind::AdjacencyOverflow, box_numbered_pages(overflow_pages)?);
	}
	if let Some(pages) = subgraph_pages {
		payloads.insert(DataFileKind::Subgraphs, number_pages(pages)?);
	}
	payloads.insert(DataFileKind::Labels, number_pages(label_pages)?);
	payloads.insert(DataFileKind::Statistics, vec![(PageNo::new(0), statistics_root)]);
	payloads.insert(DataFileKind::LabelStatistics, number_pages(label_statistics)?);
	payloads.insert(DataFileKind::Index, number_pages(primary.pages)?);
	Ok(BuiltGraphDelta {
		catalog_entry,
		superblock,
		payloads,
		index_payloads,
		removed_subgraph_slots,
	})
}

fn validate_allocated_slots(
	kind: AtomKind,
	base_capacity: u64,
	next_capacity: u64,
	created: impl Iterator<Item = AtomId>,
) -> FerrosGrynnResult<()> {
	let created = created.collect::<Vec<_>>();
	let expected = next_capacity.checked_sub(base_capacity).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("atom delta capacity moved backwards")
	})?;
	if u64::try_from(created.len()).ok() != Some(expected) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"atom delta does not account for every allocated slot",
		));
	}
	for (offset, id) in created.into_iter().enumerate() {
		let slot = base_capacity
			.checked_add(u64::try_from(offset).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("atom delta offset exceeds u64")
			})?)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("atom slot overflows"))?;
		if id.kind() != Some(kind) || id.slot() != slot || id.generation() != 0 {
			return Err(FerrosGrynnError::invalid_storage_file(
				"atom delta contains a non-contiguous allocated slot",
			));
		}
	}
	Ok(())
}

fn delta_created_records(
	delta: &LocalCommitAtomDelta,
) -> impl Iterator<Item = (AtomId, &RecordId)> {
	delta
		.nodes
		.iter()
		.filter_map(|change| match change {
			LocalCommitNodeChange::Upsert {
				id,
				record_id,
				created: true,
				..
			} => Some((*id, record_id)),
			_ => None,
		})
		.chain(delta.edges.iter().filter_map(|change| match change {
			LocalCommitEdgeChange::Upsert {
				id,
				record_id,
				created: true,
				..
			} => Some((*id, record_id)),
			_ => None,
		}))
}

fn indexed_strings<'value>(
	values: &'value [String],
	field: &str,
) -> FerrosGrynnResult<BTreeMap<&'value str, u32>> {
	values
		.iter()
		.enumerate()
		.map(|(index, value)| {
			u32::try_from(index).map(|index| (value.as_str(), index)).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file(format!("{field} id exceeds u32"))
			})
		})
		.collect()
}

#[allow(clippy::too_many_arguments, reason = "one hot-page load carries its exact file contract")]
fn delta_hot_page<'pages>(
	pages: &'pages mut BTreeMap<PageNo, Box<AlignedPage>>,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	kind: DataFileKind,
	page_no: PageNo,
	base_pages: u64,
	new_page: fn(PageNo, PageLsn) -> FerrosGrynnResult<AlignedPage>,
) -> FerrosGrynnResult<&'pages mut AlignedPage> {
	if !pages.contains_key(&page_no) {
		let page = if page_no.get() < base_pages {
			let source = manager.read_page(transaction, base.page_key(kind, page_no)?)?;
			let mut page = Box::new(AlignedPage::zeroed());
			page.as_bytes_mut().copy_from_slice(source.as_bytes());
			page.assign_lsn(page_no, PageLsn::ZERO)?;
			page
		} else {
			Box::new(new_page(page_no, PageLsn::ZERO)?)
		};
		pages.insert(page_no, page);
	}
	pages
		.get_mut(&page_no)
		.map(Box::as_mut)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("atom delta hot page is absent"))
}

fn validate_delta_slot(
	id: AtomId,
	created: bool,
	generation: u32,
	tombstone: bool,
) -> FerrosGrynnResult<()> {
	if created {
		if !tombstone {
			return Err(FerrosGrynnError::invalid_storage_file(
				"atom delta allocated an occupied slot",
			));
		}
	} else if tombstone || generation != id.generation() {
		return Err(FerrosGrynnError::invalid_storage_file("atom delta targets a stale slot"));
	}
	Ok(())
}

fn next_generation(id: AtomId) -> FerrosGrynnResult<u32> {
	let Some(next) = id.generation().checked_add(1) else {
		return Ok(id.generation());
	};
	match AtomId::from_parts(
		id.kind()
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("removed atom has no kind"))?,
		id.slot(),
		next,
	) {
		Ok(_id) => Ok(next),
		Err(_error) => Ok(id.generation()),
	}
}

fn delta_edge_refs(edges: &[AtomId]) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	edges
		.iter()
		.map(|edge| {
			if edge.kind() != Some(AtomKind::Edge) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"node adjacency contains a non-edge atom",
				));
			}
			DiskAtomRef::from_parts(AtomKind::Edge, edge.slot())
		})
		.collect()
}

fn encode_delta_topology(topology: &EdgeTopology) -> FerrosGrynnResult<EncodedTopology> {
	let encode = |endpoints: &[EdgeEndpoint]| {
		endpoints
			.iter()
			.map(|endpoint| {
				let EdgeEndpoint::Local(id) = endpoint else {
					return Err(FerrosGrynnError::invalid_argument(
						"point incidence replacement accepts only local AtomRef endpoints",
					));
				};
				Ok(EncodedEndpoint {
					graph_slot: 0,
					atom: DiskAtomRef::from_parts(
						id.kind().ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file("endpoint has no atom kind")
						})?,
						id.slot(),
					)?,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()
	};
	Ok(match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			let source = encode(source)?;
			let target = encode(target)?;
			let code = if source.len() == 1 && target.len() == 1 && source == target {
				EdgeTopologyCode::Loop
			} else if source.len() + target.len() == 2 {
				EdgeTopologyCode::Directed
			} else {
				EdgeTopologyCode::HyperDirected
			};
			EncodedTopology {
				code,
				source,
				target,
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => EncodedTopology {
			code: EdgeTopologyCode::Bidirectional,
			source: encode(source)?,
			target: encode(target)?,
		},
		EdgeTopology::Undirected {
			endpoints,
		} => {
			let source = encode(endpoints)?;
			let code = if source.len() == 2 {
				EdgeTopologyCode::Undirected
			} else {
				EdgeTopologyCode::HyperUndirected
			};
			EncodedTopology {
				code,
				source,
				target: Vec::new(),
			}
		}
	})
}

fn count_node_changes(
	changes: &[LocalCommitNodeChange],
	created: bool,
	removed: bool,
) -> FerrosGrynnResult<u64> {
	u64::try_from(
		changes
			.iter()
			.filter(|change| match change {
				LocalCommitNodeChange::Upsert {
					created: actual,
					..
				} => created && !removed && *actual,
				LocalCommitNodeChange::Remove {
					created: actual,
					..
				} => removed && !created && !*actual,
			})
			.count(),
	)
	.map_err(|_error| FerrosGrynnError::invalid_storage_file("node delta count exceeds u64"))
}

fn count_edge_changes(
	changes: &[LocalCommitEdgeChange],
	created: bool,
	removed: bool,
) -> FerrosGrynnResult<u64> {
	u64::try_from(
		changes
			.iter()
			.filter(|change| match change {
				LocalCommitEdgeChange::Upsert {
					created: actual,
					..
				} => created && !removed && *actual,
				LocalCommitEdgeChange::Remove {
					created: actual,
					..
				} => removed && !created && !*actual,
			})
			.count(),
	)
	.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge delta count exceeds u64"))
}

fn delta_graph_statistics(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	delta: &LocalCommitAtomDelta,
	node_count: u64,
	edge_count: u64,
	context: &EncodeContext,
) -> FerrosGrynnResult<GraphStatistics> {
	let mut previous = base.statistics_with_labels(manager, transaction)?;
	hydrate_legacy_label_statistics(&mut previous, manager, transaction, base, context)?;
	if previous.node_count != base.superblock.node_count
		|| previous.edge_count != base.superblock.edge_count
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics counts disagree with GraphSuperblock",
		));
	}
	let mut total_out = i128::from(previous.total_out_degree);
	let mut total_in = i128::from(previous.total_in_degree);
	let mut total_undirected = i128::from(previous.total_undirected_degree);
	let final_adjacency = delta
		.adjacency
		.iter()
		.map(|adjacency| (adjacency.id, adjacency))
		.collect::<BTreeMap<_, _>>();
	let mut affected = final_adjacency.keys().copied().collect::<BTreeSet<_>>();
	affected.extend(delta.nodes.iter().filter_map(|change| match change {
		LocalCommitNodeChange::Remove {
			id,
			..
		} => Some(*id),
		LocalCommitNodeChange::Upsert {
			..
		} => None,
	}));
	for id in affected {
		if id.slot() < base.superblock.node_capacity {
			let (page_no, within) = hot_slot_address(id.slot())?;
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::NodesHot, page_no)?)?;
			let slot = read_node_hot_slot(page.as_page(), page_no, within)?;
			if !slot.flags.contains(NodeSlotFlags::TOMBSTONE) && slot.generation == id.generation()
			{
				total_out -= i128::from(slot.out_count);
				total_in -= i128::from(slot.in_count);
				total_undirected -= i128::from(slot.undirected_count);
			}
		}
		if let Some(adjacency) = final_adjacency.get(&id) {
			total_out += i128::try_from(adjacency.outgoing.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("out-degree exceeds i128")
			})?;
			total_in += i128::try_from(adjacency.incoming.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("in-degree exceeds i128")
			})?;
			total_undirected += i128::try_from(adjacency.undirected.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("undirected degree exceeds i128")
			})?;
		}
	}
	if total_out < 0 || total_in < 0 || total_undirected < 0 {
		return Err(FerrosGrynnError::invalid_storage_file(
			"atom delta produced negative degree totals",
		));
	}
	let total_out = u64::try_from(total_out)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("out-degree total exceeds u64"))?;
	let total_in = u64::try_from(total_in)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("in-degree total exceeds u64"))?;
	let total_undirected = u64::try_from(total_undirected).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("undirected-degree total exceeds u64")
	})?;
	let largest_added_out = final_adjacency
		.values()
		.map(|adjacency| adjacency.outgoing.len())
		.max()
		.map_or(Ok(0), |degree| {
			u64::try_from(degree)
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("out-degree exceeds u64"))
		})?;
	let largest_added_in = final_adjacency
		.values()
		.map(|adjacency| adjacency.incoming.len())
		.max()
		.map_or(Ok(0), |degree| {
			u64::try_from(degree)
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("in-degree exceeds u64"))
		})?;
	let maximum_out_degree = if node_count == 0 {
		0
	} else {
		previous.maximum_out_degree.max(largest_added_out).min(total_out)
	};
	let maximum_in_degree = if node_count == 0 {
		0
	} else {
		previous.maximum_in_degree.max(largest_added_in).min(total_in)
	};
	// ponytail: maxima remain conservative after degree decreases. Stage 6's
	// StatisticsStore/ANALYZE owns an exact rescan if planner quality needs it.
	let changed_rows = delta
		.nodes
		.len()
		.checked_add(delta.edges.len())
		.and_then(|count| count.checked_add(delta.memberships.len()))
		.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("statistics changed-row count overflows")
		})?;
	let changed_rows = u64::try_from(changed_rows).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("statistics changed-row count exceeds u64")
	})?;
	let mut statistics = previous;
	statistics.node_count = node_count;
	statistics.edge_count = edge_count;
	statistics.total_out_degree = total_out;
	statistics.total_in_degree = total_in;
	statistics.total_undirected_degree = total_undirected;
	statistics.maximum_out_degree = maximum_out_degree;
	statistics.maximum_in_degree = maximum_in_degree;
	update_delta_label_statistics(&mut statistics, manager, transaction, base, delta, context)?;
	update_delta_table_statistics(&mut statistics, manager, transaction, base, delta)?;
	statistics.mark_dirty(changed_rows, delta.revision)?;
	Ok(statistics)
}

/// Applies exact before/after membership changes without scanning the graph.
fn update_delta_table_statistics(
	statistics: &mut GraphStatistics,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	delta: &LocalCommitAtomDelta,
) -> FerrosGrynnResult<()> {
	// Table totals are catalog-owned and therefore are not part of the
	// statistics-root page. Hydrate only the tables that need a physical Count
	// update; the datastore root reconciles all logical totals after commit.
	statistics.tables.clear();
	for (table_id, index) in &base.count_indexes {
		let page = manager.read_page(
			transaction,
			crate::storage::types::PageKey::new(base.graph_id, index.file_id, PageNo::new(0)),
		)?;
		let count = decode_count_index(page.as_page())?;
		if count.table_id != *table_id {
			return Err(FerrosGrynnError::invalid_storage_file(
				"Count index table identity disagrees with its catalog binding",
			));
		}
		statistics.tables.insert(*table_id, count.atom_count);
	}
	let node_changes = delta
		.nodes
		.iter()
		.map(|change| match change {
			LocalCommitNodeChange::Upsert {
				id,
				created,
				..
			}
			| LocalCommitNodeChange::Remove {
				id,
				created,
			} => (*id, (change, *created)),
		})
		.collect::<BTreeMap<_, _>>();
	let edge_changes = delta
		.edges
		.iter()
		.map(|change| match change {
			LocalCommitEdgeChange::Upsert {
				id,
				created,
				..
			}
			| LocalCommitEdgeChange::Remove {
				id,
				created,
			} => (*id, (change, *created)),
		})
		.collect::<BTreeMap<_, _>>();
	let memberships = delta
		.memberships
		.iter()
		.map(|membership| (membership.id, membership.table_id))
		.collect::<BTreeMap<_, _>>();
	let mut affected =
		node_changes.keys().chain(edge_changes.keys()).copied().collect::<BTreeSet<_>>();
	affected.extend(memberships.keys().copied());
	for id in affected {
		let created = node_changes
			.get(&id)
			.map(|(_change, created)| *created)
			.or_else(|| edge_changes.get(&id).map(|(_change, created)| *created))
			.unwrap_or(false);
		let before = if created {
			None
		} else {
			previous_table_membership(manager, transaction, base, id)?
		};
		let removed = node_changes.get(&id).is_some_and(|(change, _created)| {
			matches!(change, LocalCommitNodeChange::Remove { .. })
		}) || edge_changes.get(&id).is_some_and(|(change, _created)| {
			matches!(change, LocalCommitEdgeChange::Remove { .. })
		});
		let mut after = (!removed).then_some(before).flatten();
		if !removed && let Some(table_id) = memberships.get(&id) {
			after = *table_id;
		}
		if before == after {
			continue;
		}
		if let Some(table_id) = before {
			if let Some(count) = statistics.tables.get_mut(&table_id) {
				*count = count.checked_sub(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("Count index value underflows")
				})?;
			}
		}
		if let Some(table_id) = after {
			if let Some(count) = statistics.tables.get_mut(&table_id) {
				*count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("Count index value overflows")
				})?;
			}
		}
	}
	Ok(())
}

/// Reads one exact prior table membership from a live hot slot.
fn previous_table_membership(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	id: AtomId,
) -> FerrosGrynnResult<Option<u32>> {
	let (page_no, within) = hot_slot_address(id.slot())?;
	let (generation, tombstone, table_id) = match id.kind() {
		Some(AtomKind::Node) => {
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::NodesHot, page_no)?)?;
			let slot = read_node_hot_slot(page.as_page(), page_no, within)?;
			(slot.generation, slot.flags.contains(NodeSlotFlags::TOMBSTONE), slot.table_id)
		}
		Some(AtomKind::Edge) => {
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::EdgesHot, page_no)?)?;
			let slot = read_edge_hot_slot(page.as_page(), page_no, within)?;
			(slot.generation, slot.flags.contains(EdgeSlotFlags::TOMBSTONE), slot.table_id)
		}
		None => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"table statistics delta contains an invalid atom kind",
			));
		}
	};
	if tombstone || generation != id.generation() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"table statistics delta references a stale atom",
		));
	}
	Ok(table_id)
}

/// Reconstructs exact label counters before upgrading a legacy one-page root.
fn hydrate_legacy_label_statistics(
	statistics: &mut GraphStatistics,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	context: &EncodeContext,
) -> FerrosGrynnResult<()> {
	if statistics.revision != 0 || statistics.node_count == 0 && statistics.edge_count == 0 {
		return Ok(());
	}
	statistics.labels.clear();
	let slots_per_page = u64::try_from(HOT_SLOTS_PER_PAGE).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("hot slots per page exceeds u64")
	})?;
	let mut node_count = 0_u64;
	for raw_page in 0..base.superblock.node_hot_pages {
		let page_no = PageNo::new(raw_page);
		let page =
			manager.read_page(transaction, base.page_key(DataFileKind::NodesHot, page_no)?)?;
		for within in 0..HOT_SLOTS_PER_PAGE {
			let within_u64 = u64::try_from(within).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("node slot offset exceeds u64")
			})?;
			let slot_index = raw_page
				.checked_mul(slots_per_page)
				.and_then(|first| first.checked_add(within_u64))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("node statistics slot overflows")
				})?;
			if slot_index >= base.superblock.node_capacity {
				break;
			}
			let slot = read_node_hot_slot(page.as_page(), page_no, within)?;
			if slot.flags.contains(NodeSlotFlags::TOMBSTONE) {
				continue;
			}
			node_count = node_count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("node statistics count overflows")
			})?;
			if let Some(label) = decode_label(slot.label_id, &base.labels.node_labels)? {
				let properties = read_delta_property_count(
					manager,
					transaction,
					base,
					slot.properties,
					context,
				)?;
				statistics.replace_label(AtomKind::Node, None, Some((&label, properties)))?;
			}
		}
	}
	let mut edge_count = 0_u64;
	for raw_page in 0..base.superblock.edge_hot_pages {
		let page_no = PageNo::new(raw_page);
		let page =
			manager.read_page(transaction, base.page_key(DataFileKind::EdgesHot, page_no)?)?;
		for within in 0..HOT_SLOTS_PER_PAGE {
			let within_u64 = u64::try_from(within).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("edge slot offset exceeds u64")
			})?;
			let slot_index = raw_page
				.checked_mul(slots_per_page)
				.and_then(|first| first.checked_add(within_u64))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("edge statistics slot overflows")
				})?;
			if slot_index >= base.superblock.edge_capacity {
				break;
			}
			let slot = read_edge_hot_slot(page.as_page(), page_no, within)?;
			if slot.flags.contains(EdgeSlotFlags::TOMBSTONE) {
				continue;
			}
			edge_count = edge_count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge statistics count overflows")
			})?;
			if let Some(label) = decode_label(slot.label_id, &base.labels.edge_labels)? {
				let properties = read_delta_property_count(
					manager,
					transaction,
					base,
					slot.properties,
					context,
				)?;
				statistics.replace_label(AtomKind::Edge, None, Some((&label, properties)))?;
			}
		}
	}
	if node_count != statistics.node_count || edge_count != statistics.edge_count {
		return Err(FerrosGrynnError::invalid_storage_file(
			"legacy statistics counts disagree with live hot slots",
		));
	}
	let dirty_rows = node_count.checked_add(edge_count).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("legacy statistics dirty count overflows")
	})?;
	statistics.mark_dirty(dirty_rows, base.revision)?;
	Ok(())
}

/// Applies node and edge label changes to exact persisted counters.
fn update_delta_label_statistics(
	statistics: &mut GraphStatistics,
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	delta: &LocalCommitAtomDelta,
	context: &EncodeContext,
) -> FerrosGrynnResult<()> {
	for change in &delta.nodes {
		match change {
			LocalCommitNodeChange::Upsert {
				id,
				label,
				properties,
				created,
				..
			} => {
				let before = if *created {
					None
				} else {
					previous_label_observation(manager, transaction, base, *id, context)?
				};
				statistics.replace_label(
					AtomKind::Node,
					before.as_ref().map(|(label, count)| (label, *count)),
					label.as_ref().map(|label| (label, properties.len())),
				)?;
			}
			LocalCommitNodeChange::Remove {
				id,
				created,
			} => {
				let before = if *created {
					None
				} else {
					previous_label_observation(manager, transaction, base, *id, context)?
				};
				statistics.replace_label(
					AtomKind::Node,
					before.as_ref().map(|(label, count)| (label, *count)),
					None,
				)?;
			}
		}
	}
	for change in &delta.edges {
		match change {
			LocalCommitEdgeChange::Upsert {
				id,
				label,
				properties,
				created,
				..
			} => {
				let before = if *created {
					None
				} else {
					previous_label_observation(manager, transaction, base, *id, context)?
				};
				statistics.replace_label(
					AtomKind::Edge,
					before.as_ref().map(|(label, count)| (label, *count)),
					label.as_ref().map(|label| (label, properties.len())),
				)?;
			}
			LocalCommitEdgeChange::Remove {
				id,
				created,
			} => {
				let before = if *created {
					None
				} else {
					previous_label_observation(manager, transaction, base, *id, context)?
				};
				statistics.replace_label(
					AtomKind::Edge,
					before.as_ref().map(|(label, count)| (label, *count)),
					None,
				)?;
			}
		}
	}
	Ok(())
}

/// Reads the prior label and property count for one changed hot slot.
fn previous_label_observation(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	id: AtomId,
	context: &EncodeContext,
) -> FerrosGrynnResult<Option<(Text, usize)>> {
	let (page_no, within) = hot_slot_address(id.slot())?;
	let (generation, tombstone, label, properties) = match id.kind() {
		Some(AtomKind::Node) => {
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::NodesHot, page_no)?)?;
			let slot = read_node_hot_slot(page.as_page(), page_no, within)?;
			(
				slot.generation,
				slot.flags.contains(NodeSlotFlags::TOMBSTONE),
				decode_label(slot.label_id, &base.labels.node_labels)?,
				slot.properties,
			)
		}
		Some(AtomKind::Edge) => {
			let page =
				manager.read_page(transaction, base.page_key(DataFileKind::EdgesHot, page_no)?)?;
			let slot = read_edge_hot_slot(page.as_page(), page_no, within)?;
			(
				slot.generation,
				slot.flags.contains(EdgeSlotFlags::TOMBSTONE),
				decode_label(slot.label_id, &base.labels.edge_labels)?,
				slot.properties,
			)
		}
		None => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"statistics delta contains an invalid atom kind",
			));
		}
	};
	if tombstone || generation != id.generation() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics delta references a stale atom",
		));
	}
	let Some(label) = label else {
		return Ok(None);
	};
	let property_count =
		read_delta_property_count(manager, transaction, base, properties, context)?;
	Ok(Some((label, property_count)))
}

/// Decodes one prior property record and returns its exact entry count.
fn read_delta_property_count(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	location: Option<PropertyLocation>,
	context: &EncodeContext,
) -> FerrosGrynnResult<usize> {
	let Some(location) = location else {
		return Ok(0);
	};
	let limits = DecodeLimits::default();
	let record = match location {
		PropertyLocation::Inline {
			page_no,
			slot,
			length,
		} => {
			let page = manager
				.read_page(transaction, base.page_key(DataFileKind::Properties, page_no)?)?;
			read_property_record(page.as_page(), page_no, slot, length)?.to_vec()
		}
		PropertyLocation::Large {
			first_page,
			length,
		} => {
			let length_usize = usize::try_from(length).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("large property length exceeds usize")
			})?;
			if length_usize > limits.max_total_allocation {
				let limit = u64::try_from(limits.max_total_allocation).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("decode limit exceeds u64")
				})?;
				let actual = u64::try_from(length_usize).map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("property length exceeds u64")
				})?;
				return Err(FerrosGrynnError::storage_decode_limit_exceeded(
					"large property bytes",
					limit,
					actual,
				));
			}
			let page_count = length_usize.div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
			let mut pages = Vec::with_capacity(page_count);
			for offset in 0..page_count {
				let page_no = first_page
					.get()
					.checked_add(u64::try_from(offset).map_err(|_error| {
						FerrosGrynnError::invalid_storage_file(
							"large property page offset exceeds u64",
						)
					})?)
					.map(PageNo::new)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"large property page number overflows",
						)
					})?;
				let page = manager
					.read_page(transaction, base.page_key(DataFileKind::Properties, page_no)?)?;
				let mut copy = AlignedPage::zeroed();
				copy.as_bytes_mut().copy_from_slice(page.as_bytes());
				pages.push(copy);
			}
			decode_large_property(first_page, &pages, length, limits.max_total_allocation)?
		}
	};
	decode_property_map(&record, &context.decoder(), limits).map(|properties| properties.len())
}

fn number_pages(pages: Vec<AlignedPage>) -> FerrosGrynnResult<Vec<(PageNo, Box<AlignedPage>)>> {
	pages
		.iter()
		.enumerate()
		.map(|(index, page)| {
			let page_no = u64::try_from(index).map(PageNo::new).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("graph page index exceeds u64")
			})?;
			Ok((page_no, boxed_page(page_no, page)?))
		})
		.collect()
}

fn box_numbered_pages(
	pages: Vec<(PageNo, AlignedPage)>,
) -> FerrosGrynnResult<Vec<(PageNo, Box<AlignedPage>)>> {
	pages.iter().map(|(page_no, page)| Ok((*page_no, boxed_page(*page_no, page)?))).collect()
}

fn boxed_page(page_no: PageNo, page: &AlignedPage) -> FerrosGrynnResult<Box<AlignedPage>> {
	page.validate(page_no)?;
	let mut stored = Box::new(AlignedPage::zeroed());
	stored.as_bytes_mut().copy_from_slice(page.as_bytes());
	stored.validate(page_no)?;
	Ok(stored)
}

/// Builds only Count pages whose exact table total changed in this delta.
fn build_count_index_delta(
	manager: &SyncPageManager,
	transaction: &TxHandle,
	base: &LocalGraph,
	statistics: &GraphStatistics,
) -> FerrosGrynnResult<Vec<(DataFileId, Box<AlignedPage>)>> {
	let mut pages = Vec::new();
	for (table_id, index) in &base.count_indexes {
		let key = crate::storage::types::PageKey::new(base.graph_id, index.file_id, PageNo::new(0));
		let current = manager.read_page(transaction, key)?;
		let current = decode_count_index(current.as_page())?;
		if current.table_id != *table_id {
			return Err(FerrosGrynnError::invalid_storage_file(
				"Count index table identity disagrees with its catalog binding",
			));
		}
		let atom_count = statistics.tables.get(table_id).copied().unwrap_or(0);
		if current.atom_count == atom_count {
			continue;
		}
		let page = encode_count_index(
			CountIndexValue {
				atom_count,
				last_updated_lsn: current.last_updated_lsn,
				..current
			},
			PageLsn::ZERO,
		)?;
		pages.push((index.file_id, Box::new(page)));
	}
	Ok(pages)
}

#[cfg(test)]
#[test]
fn numbered_delta_pages_preserve_aligned_headers() -> FerrosGrynnResult<()> {
	let index = build_primary_index(
		&[
			PrimaryIndexEntry {
				key: vec![1],
				atom: DiskAtomRef::from_parts(AtomKind::Node, 0)?,
			},
			PrimaryIndexEntry {
				key: vec![2],
				atom: DiskAtomRef::from_parts(AtomKind::Node, 1)?,
			},
		],
		PageLsn::ZERO,
	)?;
	let pages = number_pages(index.pages)?;
	pages[0].1.validate(PageNo::new(0))?;
	Ok(())
}

pub(super) fn build_table_indexes(
	root: &Path,
	record_id: &RecordId,
	graph: &Graph<Frozen>,
	tables: &[LocalCommitTable],
	memberships: &BTreeMap<AtomId, u32>,
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<BuiltTableIndexImage>> {
	let mut built = Vec::new();
	for table in tables.iter().filter(|table| table.graph.as_ref() == Some(record_id)) {
		for index in &table.indexes {
			if index.kind == TableIndexKind::Count {
				let atom_count = u64::try_from(
					memberships.values().filter(|table_id| **table_id == table.id).count(),
				)
				.map_err(|_error| {
					FerrosGrynnError::invalid_storage_file("table count exceeds u64")
				})?;
				built.push(BuiltTableIndexImage {
					table: table.name.clone(),
					table_id: table.id,
					name: index.name.clone(),
					index_id: index.id,
					schema_version: index.version,
					kind: index.kind,
					fields: Vec::new(),
					root: PageNo::new(0),
					pages: vec![encode_count_index(
						CountIndexValue {
							table_id: table.id,
							atom_count,
							last_updated_lsn: 0,
							created_at_lsn: 0,
						},
						PageLsn::ZERO,
					)?],
				});
				continue;
			}
			let fields = index
				.fields
				.iter()
				.map(|property_key| {
					table
						.fields
						.iter()
						.find(|field| field.property_key == *property_key)
						.map(|field| (PropertyKeyId::new(*property_key), &field.value_type))
						.ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"scalar index references a field absent from its table",
							)
						})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let mut sorter = ExternalIndexSorter::new(root);
			for kind in [AtomKind::Node, AtomKind::Edge] {
				if !table_kind_accepts(table.kind, kind) {
					continue;
				}
				let ids = match kind {
					AtomKind::Node => {
						GraphRead::node_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?
					}
					AtomKind::Edge => {
						GraphRead::edge_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?
					}
				};
				for id in ids {
					let properties = match kind {
						AtomKind::Node => {
							let node = graph.node(id).ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"indexed node disappeared during graph encoding",
								)
							})?;
							node.properties()
						}
						AtomKind::Edge => {
							let edge = graph.edge(id).ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"indexed edge disappeared during graph encoding",
								)
							})?;
							edge.properties()
						}
					};
					if memberships.get(&id).copied() != Some(table.id) {
						continue;
					}
					let missing = Value::None;
					let components = fields
						.iter()
						.map(|(property_key, value_type)| {
							(properties.get(*property_key).unwrap_or(&missing), *value_type)
						})
						.collect::<Vec<_>>();
					let key = encode_index_key(&components, context)?;
					let atom = disk_refs.get(&id).copied().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"indexed atom has no assigned disk reference",
						)
					})?;
					sorter.push(key, atom)?;
				}
			}
			let records = sorter.finish()?;
			let mode = match index.kind {
				TableIndexKind::Standard => ScalarIndexMode::Standard,
				TableIndexKind::Unique => ScalarIndexMode::Unique,
				TableIndexKind::Count => {
					return Err(FerrosGrynnError::invalid_storage_file(
						"Count index reached scalar B+tree packing",
					));
				}
			};
			let tree = build_scalar_index_sorted(&records, mode, PageLsn::ZERO)?;
			built.push(BuiltTableIndexImage {
				table: table.name.clone(),
				table_id: table.id,
				name: index.name.clone(),
				index_id: index.id,
				schema_version: index.version,
				kind: index.kind,
				fields: index.fields.clone(),
				root: tree.root,
				pages: tree.pages,
			});
		}
	}
	built.sort_unstable_by_key(|index| index.index_id);
	Ok(built)
}

/// Builds dense immutable side-files for every label present in one graph
/// image.
fn build_label_indexes(
	graph: &Graph<Frozen>,
	node_ids: &[AtomId],
	edge_ids: &[AtomId],
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	node_labels: &BTreeMap<String, u32>,
	edge_labels: &BTreeMap<String, u32>,
) -> FerrosGrynnResult<Vec<BuiltLabelIndexImage>> {
	let mut grouped = BTreeMap::<(AtomKind, u32), Vec<DiskAtomRef>>::new();
	for (kind, ids, labels) in
		[(AtomKind::Node, node_ids, node_labels), (AtomKind::Edge, edge_ids, edge_labels)]
	{
		for id in ids {
			let label = match kind {
				AtomKind::Node => graph.node(*id).and_then(|node| node.label()),
				AtomKind::Edge => graph.edge(*id).and_then(|edge| edge.label()),
			};
			let Some(label) = label else {
				continue;
			};
			let label_id = labels.get(label).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("graph label was not interned")
			})?;
			let atom = disk_refs.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("labeled atom has no disk reference")
			})?;
			grouped.entry((kind, label_id)).or_default().push(atom);
		}
	}
	grouped
		.into_iter()
		.map(|((kind, label_id), mut atoms)| {
			atoms.sort_unstable_by_key(|atom| atom.slot());
			Ok(BuiltLabelIndexImage {
				kind,
				label_id,
				pages: build_label_index(&atoms, kind, PageLsn::ZERO)?,
			})
		})
		.collect()
}

pub(super) fn encode_optional_properties(
	properties: &PropertyMap,
	context: &mut EncodeContext,
	store: &mut PropertyStoreBuilder,
) -> FerrosGrynnResult<Option<PropertyLocation>> {
	if properties.is_empty() {
		return Ok(None);
	}
	let record = encode_property_map(properties, context)?;
	store.insert(&record).map(Some)
}

pub(super) fn dense_ids(values: BTreeSet<String>) -> FerrosGrynnResult<BTreeMap<String, u32>> {
	values
		.into_iter()
		.enumerate()
		.map(|(id, value)| {
			u32::try_from(id)
				.map(|id| (value, id))
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("label id exceeds u32"))
		})
		.collect()
}

pub(super) fn page_count_for_slots(capacity: u64) -> FerrosGrynnResult<usize> {
	if capacity == 0 {
		return Ok(0);
	}
	let slots = u64::try_from(HOT_SLOTS_PER_PAGE)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("hot slots exceed u64"))?;
	usize::try_from(capacity.div_ceil(slots))
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("hot page count exceeds usize"))
}

pub(super) fn make_hot_pages(
	count: usize,
	new_page: fn(PageNo, PageLsn) -> FerrosGrynnResult<AlignedPage>,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let mut pages = Vec::with_capacity(count);
	for index in 0..count {
		let page_no = u64::try_from(index).map(PageNo::new).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("hot page index exceeds u64")
		})?;
		let page = new_page(page_no, PageLsn::ZERO)?;
		pages.push(page);
	}
	Ok(pages)
}

/// Multiple over-aligned hot pages retain complete headers when collected.
#[cfg(test)]
#[test]
pub(super) fn hot_page_builder_preserves_aligned_page_headers() -> FerrosGrynnResult<()> {
	let pages = make_hot_pages(2, new_node_hot_page)?;
	pages[0].validate(PageNo::new(0))?;
	pages[1].validate(PageNo::new(1))?;
	Ok(())
}

pub(super) fn checked_page_index(page_no: PageNo, page_count: usize) -> FerrosGrynnResult<usize> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("page number exceeds usize"))?;
	if index >= page_count {
		return Err(FerrosGrynnError::invalid_storage_file("hot-slot address exceeds its file"));
	}
	Ok(index)
}

pub(super) fn checked_u32(value: usize, field: &str) -> FerrosGrynnResult<u32> {
	u32::try_from(value)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file(format!("{field} exceeds u32")))
}

pub(super) fn encode_subgraph_pages(
	entries: &[SubgraphEntry],
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let chunks = if entries.is_empty() {
		vec![&[][..]]
	} else {
		entries.chunks(SUBGRAPHS_PER_PAGE).collect::<Vec<_>>()
	};
	let mut pages = Vec::with_capacity(chunks.len());
	for (index, entries) in chunks.iter().enumerate() {
		let page_no = PageNo::new(u64::try_from(index).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("subgraph directory page exceeds u64")
		})?);
		let next = if index + 1 < chunks.len() {
			Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("subgraph directory page link overflows")
			})?))
		} else {
			None
		};
		let page = encode_subgraph_directory(page_no, PageLsn::ZERO, next, entries)?;
		pages.push(page);
	}
	Ok(pages)
}

#[cfg(test)]
#[test]
fn subgraph_page_builder_preserves_multi_page_links_and_headers() -> FerrosGrynnResult<()> {
	let mut entries = Vec::with_capacity(SUBGRAPHS_PER_PAGE + 1);
	for index in 0..=SUBGRAPHS_PER_PAGE {
		let slot = u32::try_from(index + 1).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("test subgraph slot exceeds u32")
		})?;
		entries.push(SubgraphEntry {
			slot,
			generation: 0,
			node_capacity: 1,
			edge_capacity: 0,
			node_count: 1,
			edge_count: 0,
		});
	}
	let pages = encode_subgraph_pages(&entries)?;
	assert_eq!(pages.len(), 2);
	let (next, first) = decode_subgraph_directory(&pages[0], PageNo::new(0))?;
	let (tail, second) = decode_subgraph_directory(&pages[1], PageNo::new(1))?;
	assert_eq!(next, Some(PageNo::new(1)));
	assert_eq!(tail, None);
	assert_eq!(first.len(), SUBGRAPHS_PER_PAGE);
	assert_eq!(second.len(), 1);
	assert_eq!(second[0], entries[SUBGRAPHS_PER_PAGE]);
	Ok(())
}

pub(super) fn inline_refs<const N: usize>(entries: &[DiskAtomRef]) -> [Option<DiskAtomRef>; N] {
	let mut inline = [None; N];
	for (target, value) in inline.iter_mut().zip(entries) {
		*target = Some(*value);
	}
	inline
}

pub(super) struct EncodedHotEndpointSide {
	inline: [Option<DiskAtomRef>; 2],
	local_overflow: Option<PageNo>,
	graph_slot: Option<u32>,
	graph_overflow: Option<PageNo>,
}

pub(super) fn encode_hot_endpoint_side(
	entries: &[EncodedEndpoint],
	side: EndpointSide,
	cross_level: bool,
	local: &mut OverflowStoreBuilder,
	qualified: &mut CrossLevelOverflowStoreBuilder,
) -> FerrosGrynnResult<EncodedHotEndpointSide> {
	if !cross_level {
		let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
		return Ok(EncodedHotEndpointSide {
			inline: inline_refs(&atoms),
			local_overflow: local.append(atoms.get(2..).unwrap_or(&[]))?,
			graph_slot: None,
			graph_overflow: None,
		});
	}
	let one_graph = entries
		.first()
		.map(|first| entries.iter().all(|entry| entry.graph_slot == first.graph_slot))
		.unwrap_or(true);
	if entries.len() <= 2 && one_graph {
		let atoms = entries.iter().map(|entry| entry.atom).collect::<Vec<_>>();
		return Ok(EncodedHotEndpointSide {
			inline: inline_refs(&atoms),
			local_overflow: None,
			graph_slot: entries.first().map(|entry| entry.graph_slot),
			graph_overflow: None,
		});
	}
	Ok(EncodedHotEndpointSide {
		inline: [None; 2],
		local_overflow: None,
		graph_slot: None,
		graph_overflow: qualified.append(entries, side)?,
	})
}

pub(super) fn encode_topology(
	topology: &EdgeTopology,
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	plan: &PlannedGraph,
) -> FerrosGrynnResult<EncodedTopology> {
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			let source = encode_endpoints(source, disk_refs, plan)?;
			let target = encode_endpoints(target, disk_refs, plan)?;
			let code = if source.len() == 1 && target.len() == 1 && source == target {
				EdgeTopologyCode::Loop
			} else if source.len() + target.len() == 2 {
				EdgeTopologyCode::Directed
			} else {
				EdgeTopologyCode::HyperDirected
			};
			Ok(EncodedTopology {
				code,
				source,
				target,
			})
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			let source = encode_endpoints(source, disk_refs, plan)?;
			let target = encode_endpoints(target, disk_refs, plan)?;
			if source.len() + target.len() != 2 {
				return Err(FerrosGrynnError::invalid_argument(
					"bidirectional hyperedges require a format extension",
				));
			}
			Ok(EncodedTopology {
				code: EdgeTopologyCode::Bidirectional,
				source,
				target,
			})
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			let source = encode_endpoints(endpoints, disk_refs, plan)?;
			let code = if source.len() == 2 {
				EdgeTopologyCode::Undirected
			} else {
				EdgeTopologyCode::HyperUndirected
			};
			Ok(EncodedTopology {
				code,
				source,
				target: Vec::new(),
			})
		}
	}
}

pub(super) fn encode_endpoints(
	endpoints: &[EdgeEndpoint],
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	plan: &PlannedGraph,
) -> FerrosGrynnResult<Vec<EncodedEndpoint>> {
	endpoints
		.iter()
		.map(|endpoint| match endpoint {
			EdgeEndpoint::Local(id) => disk_refs
				.get(id)
				.copied()
				.map(|atom| EncodedEndpoint {
					graph_slot: 0,
					atom,
				})
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"edge endpoint references an unknown local atom",
					)
				}),
			EdgeEndpoint::Nested(qualified) => encode_qualified_endpoint(plan, qualified),
		})
		.collect()
}

pub(super) fn encode_qualified_endpoint(
	plan: &PlannedGraph,
	qualified: &QualifiedAtomId,
) -> FerrosGrynnResult<EncodedEndpoint> {
	let mut current = plan;
	let mut graph_slot = None;
	for container in qualified.scope() {
		let child =
			current.children.iter().find(|child| child.container == *container).ok_or_else(
				|| {
					FerrosGrynnError::graph_constraint_violation(
						"metagraph",
						"qualified endpoint scope does not identify a nested graph",
					)
				},
			)?;
		graph_slot = Some(child.slot);
		current = &child.graph;
	}
	let atom = qualified.atom();
	let exists = match atom.kind() {
		Some(AtomKind::Node) => current.graph.node(atom).is_some(),
		Some(AtomKind::Edge) => current.graph.edge(atom).is_some(),
		None => false,
	};
	if !exists {
		return Err(FerrosGrynnError::graph_constraint_violation(
			"metagraph",
			"qualified endpoint atom is absent from its nested graph",
		));
	}
	Ok(EncodedEndpoint {
		graph_slot: graph_slot.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("qualified endpoint has no nested graph slot")
		})?,
		atom: DiskAtomRef::from_parts(
			atom.kind().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("qualified endpoint has an invalid kind")
			})?,
			atom.slot(),
		)?,
	})
}

pub(super) fn record_node_adjacency(
	topology: &EdgeTopology,
	edge: DiskAtomRef,
	nodes: &mut BTreeMap<AtomId, NodeAdjacency>,
) -> FerrosGrynnResult<()> {
	let mut append = |endpoint: &EdgeEndpoint,
	                  out: bool,
	                  input: bool,
	                  undirected: bool|
	 -> FerrosGrynnResult<()> {
		let Some(id) = endpoint.local_id() else {
			return Ok(());
		};
		if id.kind() != Some(AtomKind::Node) {
			return Ok(());
		}
		let adjacency = nodes.get_mut(&id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("edge references an unknown node")
		})?;
		if out {
			adjacency.outgoing.push(edge);
		}
		if input {
			adjacency.incoming.push(edge);
		}
		if undirected {
			adjacency.undirected.push(edge);
		}
		Ok(())
	};
	match topology {
		EdgeTopology::Directed {
			source,
			target,
		} => {
			for endpoint in source {
				append(endpoint, true, false, false)?;
			}
			for endpoint in target {
				append(endpoint, false, true, false)?;
			}
		}
		EdgeTopology::Bidirectional {
			source,
			target,
		} => {
			for endpoint in source.iter().chain(target) {
				append(endpoint, true, true, false)?;
			}
		}
		EdgeTopology::Undirected {
			endpoints,
		} => {
			for endpoint in endpoints {
				append(endpoint, false, false, true)?;
			}
		}
	}
	Ok(())
}

pub(super) struct LoadedNodeSlot {
	id: AtomId,
	record_id: RecordId,
	value: NodeHotSlot,
	outgoing: Vec<DiskAtomRef>,
	incoming: Vec<DiskAtomRef>,
	undirected: Vec<DiskAtomRef>,
}

pub(super) struct LoadedEdgeSlot {
	id: AtomId,
	record_id: RecordId,
	value: EdgeHotSlot,
	source: Vec<DiskAtomRef>,
	target: Vec<DiskAtomRef>,
}

pub(super) struct DecodedNode {
	record_id: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
}

pub(super) struct DecodedEdge {
	record_id: RecordId,
	topology: EdgeTopology,
	label: Option<Text>,
	properties: PropertyMap,
	weight: Option<crate::values::Number>,
}

pub(super) struct DecodedGraph {
	identity: Uuid,
	revision: u64,
	kind: GraphKind,
	properties: PropertyMap,
	nodes: BTreeMap<AtomId, DecodedNode>,
	edges: BTreeMap<AtomId, DecodedEdge>,
}

impl GraphRead for DecodedGraph {
	type Edge<'a>
		= EdgeView<'a>
	where
		Self: 'a;
	type EdgeIds<'a>
		= FallibleAtomIds<std::iter::Copied<std::collections::btree_map::Keys<'a, AtomId, DecodedEdge>>>
	where
		Self: 'a;
	type Node<'a>
		= NodeView<'a>
	where
		Self: 'a;
	type NodeIds<'a>
		= FallibleAtomIds<std::iter::Copied<std::collections::btree_map::Keys<'a, AtomId, DecodedNode>>>
	where
		Self: 'a;

	fn identity(&self) -> Uuid {
		self.identity
	}

	fn revision(&self) -> u64 {
		self.revision
	}

	fn kind(&self) -> GraphKind {
		self.kind
	}

	fn properties(&self) -> FerrosGrynnResult<Cow<'_, PropertyMap>> {
		Ok(Cow::Borrowed(&self.properties))
	}

	fn node_ids(&self) -> FerrosGrynnResult<Self::NodeIds<'_>> {
		Ok(FallibleAtomIds::new(self.nodes.keys().copied()))
	}

	fn edge_ids(&self) -> FerrosGrynnResult<Self::EdgeIds<'_>> {
		Ok(FallibleAtomIds::new(self.edges.keys().copied()))
	}

	fn node(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Node<'_>>> {
		Ok(self.nodes.get(&id).map(|node| {
			NodeView::new(id, &node.record_id, node.label.as_ref(), &node.properties, None)
		}))
	}

	fn edge(&self, id: AtomId) -> FerrosGrynnResult<Option<Self::Edge<'_>>> {
		Ok(self.edges.get(&id).map(|edge| {
			EdgeView::new(
				id,
				&edge.record_id,
				&edge.topology,
				edge.label.as_ref(),
				&edge.properties,
				edge.weight,
				None,
			)
		}))
	}

	fn snapshot_id(&self) -> SnapshotId {
		SnapshotId::in_memory(self.revision)
	}
}

pub(super) fn load_graph(
	root: &Path,
	entry: &GraphCatalogEntry,
	files: &FileCatalog,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<MaterializedGraph> {
	let paths = graph_file_paths(root, entry, files)?;
	let superblock_path = paths.get(&DataFileKind::GraphSuperblock).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph superblock file is missing")
	})?;
	let superblock = select_graph_superblock(superblock_path)?;
	if superblock.graph_id != entry.graph_id {
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph superblock identity disagrees with the database catalog",
		));
	}

	let labels_file = open_graph_file(&paths, DataFileKind::Labels)?;
	if labels_file.page_count().checked_mul(PAGE_SIZE as u64) != Some(superblock.labels_size_bytes)
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"label file length disagrees with GraphSuperblock",
		));
	}
	let labels = decode_label_dictionary(|page_no, output| {
		labels_file.read_page(page_no, output).map(|_header| ())
	})?;
	let mut context = EncodeContext::new(dictionary.clone());
	for (expected, table) in labels.table_names.iter().enumerate() {
		let actual = context.register_table_name(table.clone().into())?;
		if usize::try_from(actual).ok() != Some(expected) {
			return Err(FerrosGrynnError::invalid_storage_file("table dictionary is not dense"));
		}
	}
	for (index, zone) in labels.time_zones.iter().enumerate() {
		let actual = context.register_time_zone_name(zone.clone().into())?;
		let expected = u32::try_from(index)
			.ok()
			.and_then(|index| index.checked_add(1))
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("time-zone id overflows"))?;
		if actual != expected {
			return Err(FerrosGrynnError::invalid_storage_file(
				"time-zone dictionary is not dense",
			));
		}
	}

	let index_file = open_graph_file(&paths, DataFileKind::Index)?;
	let primary_entries = scan_primary_index(
		|page_no, output| index_file.read_page(page_no, output).map(|_header| ()),
		entry.primary_index_root,
	)?;
	let mut records = BTreeMap::new();
	for primary in primary_entries {
		let stored = CanonicalCodec::decode(&primary.key, DecodeLimits::default())?;
		let Value::Record(record) = stored.into_runtime(&context.decoder())? else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index key is not a canonical RecordId",
			));
		};
		if records.insert(primary.atom, *record).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index maps one atom more than once",
			));
		}
	}

	let node_file = open_graph_file(&paths, DataFileKind::NodesHot)?;
	let edge_file = open_graph_file(&paths, DataFileKind::EdgesHot)?;
	let adjacency_file = open_graph_file(&paths, DataFileKind::AdjacencyOverflow)?;
	if node_file.page_count() != superblock.node_hot_pages
		|| edge_file.page_count() != superblock.edge_hot_pages
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"hot-file length disagrees with GraphSuperblock",
		));
	}
	let node_pages = read_all_pages(&node_file)?;
	let edge_pages = read_all_pages(&edge_file)?;
	let mut live_atoms = BTreeMap::new();
	let mut loaded_nodes = Vec::new();
	for slot in 0..superblock.node_capacity {
		let (page_no, within) = hot_slot_address(slot)?;
		let value = read_node_hot_slot(
			&node_pages[checked_page_index(page_no, node_pages.len())?],
			page_no,
			within,
		)?;
		if value.flags.contains(NodeSlotFlags::TOMBSTONE) {
			continue;
		}
		if value.flags.contains(NodeSlotFlags::META) || value.subgraph_slot.is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"stage-4 graph generation contains a nested node",
			));
		}
		let disk = DiskAtomRef::from_parts(AtomKind::Node, slot)?;
		let record_id = records.get(&disk).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live node is absent from the primary index")
		})?;
		validate_table_id(value.table_id)?;
		let id = AtomId::from_parts(AtomKind::Node, slot, value.generation)?;
		live_atoms.insert(disk, id);
		loaded_nodes.push(LoadedNodeSlot {
			id,
			record_id,
			outgoing: read_ref_array(
				&adjacency_file,
				&value.out_inline,
				value.out_count,
				value.out_overflow,
			)?,
			incoming: read_ref_array(
				&adjacency_file,
				&value.in_inline,
				value.in_count,
				value.in_overflow,
			)?,
			undirected: read_ref_array(
				&adjacency_file,
				&value.undirected_inline,
				value.undirected_count,
				value.undirected_overflow,
			)?,
			value,
		});
	}
	let mut loaded_edges = Vec::new();
	for slot in 0..superblock.edge_capacity {
		let (page_no, within) = hot_slot_address(slot)?;
		let value = read_edge_hot_slot(
			&edge_pages[checked_page_index(page_no, edge_pages.len())?],
			page_no,
			within,
		)?;
		if value.flags.contains(EdgeSlotFlags::TOMBSTONE) {
			continue;
		}
		if value.flags.intersects(EdgeSlotFlags::META | EdgeSlotFlags::CROSS_LEVEL)
			|| value.has_transition
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"stage-4 graph generation contains meta or cross-level incidence",
			));
		}
		let disk = DiskAtomRef::from_parts(AtomKind::Edge, slot)?;
		let record_id = records.get(&disk).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live edge is absent from the primary index")
		})?;
		validate_table_id(value.table_id)?;
		let id = AtomId::from_parts(AtomKind::Edge, slot, value.generation)?;
		live_atoms.insert(disk, id);
		loaded_edges.push(LoadedEdgeSlot {
			id,
			record_id,
			source: read_ref_array(
				&adjacency_file,
				&value.source_inline,
				value.source_count,
				value.source_overflow,
			)?,
			target: read_ref_array(
				&adjacency_file,
				&value.target_inline,
				value.target_count,
				value.target_overflow,
			)?,
			value,
		});
	}
	if usize::try_from(superblock.node_count).ok() != Some(loaded_nodes.len())
		|| usize::try_from(superblock.edge_count).ok() != Some(loaded_edges.len())
		|| records.len() != live_atoms.len()
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph live counts disagree with hot pages or primary index",
		));
	}
	for (disk, record) in &records {
		let id = live_atoms.get(disk).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("primary index references a tombstone")
		})?;
		context.register_atom(AtomRef::new(disk.kind(), record.clone()), *disk)?;
		if id.kind() != Some(disk.kind()) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"primary index kind disagrees with the hot page",
			));
		}
	}

	let properties_file = open_graph_file(&paths, DataFileKind::Properties)?;
	if properties_file.page_count() != superblock.properties_pages {
		return Err(FerrosGrynnError::invalid_storage_file(
			"property-file length disagrees with GraphSuperblock",
		));
	}
	let graph_properties = read_properties(&properties_file, entry.properties, &context)?;
	let mut nodes = BTreeMap::new();
	for node in &loaded_nodes {
		nodes.insert(node.id, DecodedNode {
			record_id: node.record_id.clone(),
			label: decode_label(node.value.label_id, &labels.node_labels)?,
			properties: read_properties(&properties_file, node.value.properties, &context)?,
		});
	}
	let mut edges = BTreeMap::new();
	for edge in &loaded_edges {
		edges.insert(edge.id, DecodedEdge {
			record_id: edge.record_id.clone(),
			topology: decode_topology(edge, &live_atoms)?,
			label: decode_label(edge.value.label_id, &labels.edge_labels)?,
			properties: read_properties(&properties_file, edge.value.properties, &context)?,
			weight: edge.value.weight.map(Into::into),
		});
	}
	validate_node_adjacency(&loaded_nodes, &loaded_edges, &edges)?;
	validate_auxiliary_files(&paths, &superblock, &labels)?;
	validate_freelist_files(root, entry.graph_id, files)?;

	let decoded = DecodedGraph {
		identity: uuid::Uuid::from_bytes(entry.identity).into(),
		revision: entry.revision,
		kind: entry.kind,
		properties: graph_properties,
		nodes,
		edges,
	};
	materialize_graph(&decoded)
}

pub(super) fn graph_file_paths(
	root: &Path,
	graph: &GraphCatalogEntry,
	files: &FileCatalog,
) -> FerrosGrynnResult<BTreeMap<DataFileKind, PathBuf>> {
	let mut paths = BTreeMap::new();
	for entry in files.files.iter().filter(|entry| entry.graph_id == graph.graph_id) {
		let kind = entry.file_id.kind().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("graph file has an unknown kind")
		})?;
		if kind == DataFileKind::Freelist {
			continue;
		}
		if kind == DataFileKind::Index && entry.file_id != graph.primary_index_file_id {
			continue;
		}
		if paths.insert(kind, entry.path.resolve(root)).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"graph catalog contains duplicate file kinds",
			));
		}
	}
	for (kind, _name) in file_layout::GRAPH_FILES {
		if !paths.contains_key(&kind) {
			return Err(FerrosGrynnError::invalid_storage_file(format!(
				"graph catalog is missing {kind:?}"
			)));
		}
	}
	Ok(paths)
}

pub(super) fn open_graph_file(
	paths: &BTreeMap<DataFileKind, PathBuf>,
	kind: DataFileKind,
) -> FerrosGrynnResult<DirectFile> {
	let path = paths.get(&kind).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(format!("graph file {kind:?} is missing"))
	})?;
	DirectFile::open_read_only(path)
}

pub(super) fn read_all_pages(file: &DirectFile) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let count = usize::try_from(file.page_count()).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("file page count exceeds usize")
	})?;
	let mut pages = (0..count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
	file.read_pages(PageNo::new(0), &mut pages)?;
	Ok(pages)
}

pub(super) fn validate_table_id(table_id: Option<u32>) -> FerrosGrynnResult<()> {
	if table_id == Some(0) {
		return Err(FerrosGrynnError::invalid_storage_file("schema identity zero is reserved"));
	}
	Ok(())
}

pub(super) fn read_ref_array<const N: usize>(
	file: &DirectFile,
	inline: &[Option<DiskAtomRef>; N],
	count: u32,
	overflow: Option<PageNo>,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let count = usize::try_from(count).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("adjacency count exceeds usize")
	})?;
	let mut entries = inline.iter().take(count.min(N)).flatten().copied().collect::<Vec<_>>();
	let mut next = overflow;
	let mut visited = BTreeSet::new();
	while let Some(page_no) = next {
		if !visited.insert(page_no) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow contains a cycle",
			));
		}
		let mut page = AlignedPage::zeroed();
		file.read_page(page_no, &mut page)?;
		let (following, chunk) = decode_adjacency_overflow(&page, page_no)?;
		entries.extend(chunk);
		if entries.len() > count {
			return Err(FerrosGrynnError::invalid_storage_file(
				"adjacency overflow exceeds its declared count",
			));
		}
		next = following;
	}
	if entries.len() != count {
		return Err(FerrosGrynnError::invalid_storage_file(
			"adjacency overflow ended before its declared count",
		));
	}
	Ok(entries)
}

pub(super) fn decode_label(id: Option<u32>, labels: &[String]) -> FerrosGrynnResult<Option<Text>> {
	id.map(|id| {
		let index = usize::try_from(id)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("label id exceeds usize"))?;
		labels
			.get(index)
			.cloned()
			.map(Into::into)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("label id is out of range"))
	})
	.transpose()
}

pub(super) fn read_properties(
	file: &DirectFile,
	location: Option<PropertyLocation>,
	context: &EncodeContext,
) -> FerrosGrynnResult<PropertyMap> {
	let Some(location) = location else {
		return Ok(PropertyMap::new());
	};
	let limits = DecodeLimits::default();
	let record = match location {
		PropertyLocation::Inline {
			page_no,
			slot,
			length,
		} => {
			let mut page = AlignedPage::zeroed();
			file.read_page(page_no, &mut page)?;
			read_property_record(&page, page_no, slot, length)?.to_vec()
		}
		PropertyLocation::Large {
			first_page,
			length,
		} => {
			let length_usize = usize::try_from(length).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("large property length exceeds usize")
			})?;
			let page_count = length_usize.div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
			let mut pages = (0..page_count).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
			file.read_pages(first_page, &mut pages)?;
			decode_large_property(first_page, &pages, length, limits.max_total_allocation)?
		}
	};
	decode_property_map(&record, &context.decoder(), limits)
}

pub(super) fn decode_topology(
	edge: &LoadedEdgeSlot,
	atoms: &BTreeMap<DiskAtomRef, AtomId>,
) -> FerrosGrynnResult<EdgeTopology> {
	let source = runtime_endpoints(&edge.source, atoms)?;
	let target = runtime_endpoints(&edge.target, atoms)?;
	Ok(match edge.value.topology {
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

pub(super) fn runtime_endpoints(
	entries: &[DiskAtomRef],
	atoms: &BTreeMap<DiskAtomRef, AtomId>,
) -> FerrosGrynnResult<Vec<EdgeEndpoint>> {
	entries
		.iter()
		.map(|entry| {
			atoms.get(entry).copied().map(EdgeEndpoint::Local).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"edge endpoint references an unknown live atom",
				)
			})
		})
		.collect()
}

pub(super) fn validate_node_adjacency(
	nodes: &[LoadedNodeSlot],
	edges: &[LoadedEdgeSlot],
	decoded_edges: &BTreeMap<AtomId, DecodedEdge>,
) -> FerrosGrynnResult<()> {
	let mut expected =
		nodes.iter().map(|node| (node.id, NodeAdjacency::default())).collect::<BTreeMap<_, _>>();
	for edge in edges {
		let topology = decoded_edges
			.get(&edge.id)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("decoded edge is absent"))?;
		record_node_adjacency(
			&topology.topology,
			DiskAtomRef::from_parts(AtomKind::Edge, edge.id.slot())?,
			&mut expected,
		)?;
	}
	for node in nodes {
		let expected = expected.get(&node.id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("decoded node adjacency is absent")
		})?;
		if node.outgoing != expected.outgoing
			|| node.incoming != expected.incoming
			|| node.undirected != expected.undirected
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"node adjacency disagrees with edge topology",
			));
		}
	}
	Ok(())
}

pub(super) fn validate_auxiliary_files(
	paths: &BTreeMap<DataFileKind, PathBuf>,
	superblock: &GraphSuperblock,
	labels: &LabelDictionary,
) -> FerrosGrynnResult<()> {
	let subgraphs = open_graph_file(paths, DataFileKind::Subgraphs)?;
	let graph_statistics = open_graph_file(paths, DataFileKind::Statistics)?;
	let label_statistics = open_graph_file(paths, DataFileKind::LabelStatistics)?;
	let property_statistics = open_graph_file(paths, DataFileKind::PropertyStatistics)?;
	if subgraphs.page_count() != 1 || graph_statistics.page_count() == 0 {
		return Err(FerrosGrynnError::invalid_storage_file(
			"auxiliary files have an invalid root-page count",
		));
	}
	let mut page = AlignedPage::zeroed();
	subgraphs.read_page(PageNo::new(0), &mut page)?;
	if decode_subgraph_directory(&page, PageNo::new(0))? != (None, Vec::new()) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stage-4 subgraph directory is not empty",
		));
	}
	graph_statistics.read_page(PageNo::new(0), &mut page)?;
	let (mut stats, layout) = decode_statistics_summary(
		&page,
		graph_statistics.page_count(),
		label_statistics.page_count(),
		property_statistics.page_count(),
	)?;
	stats.degree_histograms = decode_degree_statistics(
		|page_no, output| graph_statistics.read_page(page_no, output).map(|_header| ()),
		layout,
		&labels.node_labels,
		&labels.edge_labels,
	)?;
	stats.labels = decode_label_statistics_file(
		|page_no, output| label_statistics.read_page(page_no, output).map(|_header| ()),
		layout,
		&labels.node_labels,
		&labels.edge_labels,
	)?;
	stats.property_histograms = decode_property_statistics(
		|page_no, output| property_statistics.read_page(page_no, output).map(|_header| ()),
		layout,
	)?;
	if stats.node_count != superblock.node_count || stats.edge_count != superblock.edge_count {
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics counts disagree with GraphSuperblock",
		));
	}
	Ok(())
}

/// Validates all empty freelist roots through the legacy direct loader.
fn validate_freelist_files(
	root: &Path,
	graph_id: GraphId,
	files: &FileCatalog,
) -> FerrosGrynnResult<()> {
	let mut count = 0;
	for entry in files.files.iter().filter(|entry| {
		entry.graph_id == graph_id && entry.file_id.kind() == Some(DataFileKind::Freelist)
	}) {
		let file = DirectFile::open_read_only(entry.path.resolve(root))?;
		if file.page_count() != 1 {
			return Err(FerrosGrynnError::invalid_storage_file(
				"freelist file must contain exactly one page",
			));
		}
		let mut page = AlignedPage::zeroed();
		file.read_page(PageNo::new(0), &mut page)?;
		let (_first, next, bitmap) = decode_freelist(&page, PageNo::new(0))?;
		if next.is_some() || bitmap.iter().any(|byte| *byte != 0) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"immutable generation freelist is not empty",
			));
		}
		count += 1;
	}
	if count != file_layout::FREELIST_FILES.len() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"graph generation does not contain six freelist files",
		));
	}
	Ok(())
}

pub(super) fn materialize_graph(decoded: &DecodedGraph) -> FerrosGrynnResult<MaterializedGraph> {
	let graph = Graph::<Mutable>::from_read_preserving(decoded)?.freeze();
	match decoded.kind {
		GraphKind::Graph => Ok(graph.into()),
		GraphKind::Tree => Ok(Tree::<Frozen>::try_from(graph)?.into()),
		GraphKind::RootedTree => Ok(RootedTree::<Frozen>::try_from(graph)?.into()),
		GraphKind::Forest => Ok(Forest::<Frozen>::try_from(graph)?.into()),
		GraphKind::Dag => Ok(Dag::<Frozen>::try_from(graph)?.into()),
		GraphKind::Hypergraph => Ok(Hypergraph::<Frozen>::try_from(graph)?.into()),
		GraphKind::Metagraph => Ok(Metagraph::<Frozen>::try_from(graph)?.into()),
		GraphKind::Subgraph => Err(FerrosGrynnError::invalid_storage_file(
			"a named graph catalog cannot contain a Subgraph family",
		)),
	}
}
