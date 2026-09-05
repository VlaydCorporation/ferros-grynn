#![allow(
	clippy::wildcard_imports,
	reason = "move-only module split keeps the existing imports during stage 1"
)]

use super::*;

pub(super) struct PropertyStoreBuilder {
	next_page: u64,
	current: Option<PropertyPageBuilder>,
	current_page: Option<PageNo>,
	pages: Vec<(PageNo, AlignedPage)>,
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
				self.pages.push((page_no, page));
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
			self.pages.push((page_no, builder.finish()?));
		}
		Ok(())
	}

	fn finish_numbered(mut self) -> FerrosGrynnResult<Vec<(PageNo, AlignedPage)>> {
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
		Ok(pages.into_iter().map(|(_page_no, page)| page).collect())
	}
}

#[derive(Default)]
pub(super) struct OverflowStoreBuilder {
	pages: Vec<AlignedPage>,
}

impl OverflowStoreBuilder {
	fn append(&mut self, entries: &[DiskAtomRef]) -> FerrosGrynnResult<Option<PageNo>> {
		if entries.is_empty() {
			return Ok(None);
		}
		let first = PageNo::new(u64::try_from(self.pages.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("adjacency page count exceeds u64")
		})?);
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
			self.pages.push(encode_adjacency_overflow(page_no, PageLsn::ZERO, next, chunk)?);
		}
		Ok(Some(first))
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
			self.pages.push(encode_cross_level_overflow(page_no, PageLsn::ZERO, next, chunk)?);
		}
		Ok(Some(first))
	}
}

pub(super) fn build_graph_image(
	plan: &PlannedGraph,
	dictionary: &mut PropertyDictionary,
	tables: &[LocalCommitTable],
) -> FerrosGrynnResult<BuiltGraphImage> {
	let graph_id = plan.graph_id;
	let record_id = &plan.record_id;
	let graph = &plan.graph;
	let node_ids = GraphRead::node_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let edge_ids = GraphRead::edge_ids(graph)?.collect::<FerrosGrynnResult<Vec<_>>>()?;
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

	let mut index_entries = Vec::with_capacity(node_ids.len() + edge_ids.len());
	for id in node_ids.iter().chain(&edge_ids) {
		let atom = graph
			.atom_ref(*id)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("graph atom disappeared"))?;
		let key = CanonicalCodec::encode(
			StorageValue::from_runtime(Value::Record(Box::new(atom.id().clone())), &mut context)?
				.as_ref(),
		)?;
		index_entries.push(PrimaryIndexEntry {
			key,
			atom: disk_refs.get(id).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("atom has no assigned disk reference")
			})?,
		});
	}
	index_entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	let primary = build_primary_index(&index_entries, PageLsn::ZERO)?;

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
	let indexes = build_scalar_indexes(record_id, graph, tables, &disk_refs, &mut context)?;

	let node_label_ids = dense_ids(node_labels)?;
	let edge_label_ids = dense_ids(edge_labels)?;
	let table_ids = context
		.table_names()
		.enumerate()
		.map(|(id, name)| {
			u32::try_from(id)
				.map(|id| (name.as_str().to_owned(), id))
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("table id exceeds u32"))
		})
		.collect::<FerrosGrynnResult<BTreeMap<_, _>>>()?;
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
			table_id: Some(table_ids.get(node.record_id().table()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("node table was not interned")
			})?),
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
			table_id: Some(table_ids.get(edge.record_id().table()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("edge table was not interned")
			})?),
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

	let statistics = graph_statistics(&node_adjacency, edge_ids.len())?;
	let statistic_pages = vec![encode_graph_statistics(statistics, PageLsn::ZERO)?];
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
	let freelist_pages = vec![encode_freelist(PageNo::new(0), PageLsn::ZERO, 0, None, &[])?];
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
	payloads.insert(DataFileKind::AdjacencyOverflow, overflow.pages);
	payloads.insert(DataFileKind::CrossLevelAdjacencyOverflow, cross_level_overflow.pages);
	payloads.insert(DataFileKind::Properties, property_pages);
	payloads.insert(DataFileKind::Subgraphs, subgraph_pages);
	payloads.insert(DataFileKind::Labels, label_pages);
	payloads.insert(DataFileKind::Freelist, freelist_pages);
	payloads.insert(DataFileKind::Statistics, statistic_pages);
	payloads.insert(DataFileKind::Index, primary.pages);

	let identity = graph.identity();
	Ok(BuiltGraphImage {
		catalog_entry: GraphCatalogEntry {
			graph_id,
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
		|| !base.supports_node_append_delta()
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
	let table_ids = context
		.table_names()
		.enumerate()
		.map(|(index, table)| {
			u32::try_from(index)
				.map(|index| (table.as_str().to_owned(), index))
				.map_err(|_error| FerrosGrynnError::invalid_storage_file("table id exceeds u32"))
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
			table_id: Some(table_ids.get(node.record_id.table()).copied().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("node table was not interned")
			})?),
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

	let mut statistics_page = AlignedPage::zeroed();
	let statistics =
		manager.read_page(transaction, base.page_key(DataFileKind::Statistics, PageNo::new(0))?)?;
	statistics_page.as_bytes_mut().copy_from_slice(statistics.as_bytes());
	let previous_statistics = decode_graph_statistics(&statistics_page)?;
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
	let divisor = if next_node_count == 0 {
		1.0
	} else {
		next_node_count as f64
	};
	let statistics = encode_graph_statistics(
		GraphStatistics {
			node_count: next_node_count,
			edge_count: previous_statistics.edge_count,
			average_out_degree: previous_statistics.average_out_degree
				* base.superblock.node_count as f64
				/ divisor,
			average_in_degree: previous_statistics.average_in_degree
				* base.superblock.node_count as f64
				/ divisor,
			maximum_out_degree: previous_statistics.maximum_out_degree,
			maximum_in_degree: previous_statistics.maximum_in_degree,
		},
		PageLsn::ZERO,
	)?;

	let mut superblock = base.superblock.clone();
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
		payloads.insert(DataFileKind::Properties, box_numbered_pages(property_pages)?);
	}
	payloads.insert(DataFileKind::Labels, number_pages(label_pages)?);
	payloads.insert(DataFileKind::Statistics, vec![(
		PageNo::new(0),
		boxed_page(PageNo::new(0), &statistics)?,
	)]);
	payloads.insert(DataFileKind::Index, number_pages(primary.pages)?);
	Ok(BuiltGraphDelta {
		catalog_entry,
		superblock,
		payloads,
	})
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

pub(super) fn build_scalar_indexes(
	record_id: &RecordId,
	graph: &Graph<Frozen>,
	tables: &[LocalCommitTable],
	disk_refs: &BTreeMap<AtomId, DiskAtomRef>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<BuiltScalarIndexImage>> {
	let mut built = Vec::new();
	for table in tables.iter().filter(|table| table.graph == *record_id) {
		for index in &table.indexes {
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
			let mut grouped = BTreeMap::<Vec<u8>, Vec<DiskAtomRef>>::new();
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
					let (atom_record, properties) = match kind {
						AtomKind::Node => {
							let node = graph.node(id).ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"indexed node disappeared during graph encoding",
								)
							})?;
							(node.record_id(), node.properties())
						}
						AtomKind::Edge => {
							let edge = graph.edge(id).ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"indexed edge disappeared during graph encoding",
								)
							})?;
							(edge.record_id(), edge.properties())
						}
					};
					if atom_record.table() != table.name {
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
					grouped.entry(key).or_default().push(atom);
				}
			}
			let entries = grouped
				.into_iter()
				.map(|(key, mut atoms)| {
					atoms.sort_unstable();
					atoms.dedup();
					ScalarIndexEntry {
						key,
						atoms,
					}
				})
				.collect::<Vec<_>>();
			let mode = match index.kind {
				TableIndexKind::Standard => ScalarIndexMode::Standard,
				TableIndexKind::Unique => ScalarIndexMode::Unique,
			};
			let tree = build_scalar_index(&entries, mode, PageLsn::ZERO)?;
			built.push(BuiltScalarIndexImage {
				table: table.name.clone(),
				name: index.name.clone(),
				kind: index.kind,
				fields: index.fields.clone(),
				root: tree.root,
				pages: tree.pages,
			});
		}
	}
	built.sort_unstable_by(|left, right| {
		(left.table.as_str(), left.name.as_str()).cmp(&(right.table.as_str(), right.name.as_str()))
	});
	Ok(built)
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
	chunks
		.iter()
		.enumerate()
		.map(|(index, entries)| {
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
			encode_subgraph_directory(page_no, PageLsn::ZERO, next, entries)
		})
		.collect()
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

pub(super) fn graph_statistics(
	nodes: &BTreeMap<AtomId, NodeAdjacency>,
	edge_count: usize,
) -> FerrosGrynnResult<GraphStatistics> {
	let total_out = nodes.values().try_fold(0_u64, |total, adjacency| {
		total
			.checked_add(u64::try_from(adjacency.outgoing.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("out-degree exceeds u64")
			})?)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("out-degree total overflows"))
	})?;
	let total_in = nodes.values().try_fold(0_u64, |total, adjacency| {
		total
			.checked_add(u64::try_from(adjacency.incoming.len()).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("in-degree exceeds u64")
			})?)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("in-degree total overflows"))
	})?;
	let node_count = u64::try_from(nodes.len())
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("node count exceeds u64"))?;
	let divisor = if node_count == 0 {
		1.0
	} else {
		node_count as f64
	};
	Ok(GraphStatistics {
		node_count,
		edge_count: u64::try_from(edge_count)
			.map_err(|_error| FerrosGrynnError::invalid_storage_file("edge count exceeds u64"))?,
		average_out_degree: total_out as f64 / divisor,
		average_in_degree: total_in as f64 / divisor,
		maximum_out_degree: nodes
			.values()
			.map(|adjacency| adjacency.outgoing.len() as u64)
			.max()
			.unwrap_or(0),
		maximum_in_degree: nodes
			.values()
			.map(|adjacency| adjacency.incoming.len() as u64)
			.max()
			.unwrap_or(0),
	})
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
		validate_table_id(value.table_id, &record_id, &labels.table_names)?;
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
		validate_table_id(value.table_id, &record_id, &labels.table_names)?;
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
	validate_auxiliary_files(&paths, &superblock)?;

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
		if kind == DataFileKind::Index && entry.file_id != graph.primary_index_file_id {
			continue;
		}
		if paths.insert(kind, entry.path.resolve(root)).is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"graph catalog contains duplicate file kinds",
			));
		}
	}
	for (kind, _name) in GRAPH_FILE_KINDS {
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

pub(super) fn validate_table_id(
	table_id: Option<u32>,
	record_id: &RecordId,
	tables: &[String],
) -> FerrosGrynnResult<()> {
	let id = table_id.ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("live atom has no table dictionary id")
	})?;
	let index = usize::try_from(id)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("table id exceeds usize"))?;
	if tables.get(index).map(String::as_str) != Some(record_id.table()) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"atom table id disagrees with its primary RecordId",
		));
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
) -> FerrosGrynnResult<()> {
	let subgraphs = open_graph_file(paths, DataFileKind::Subgraphs)?;
	let freelist = open_graph_file(paths, DataFileKind::Freelist)?;
	let statistics = open_graph_file(paths, DataFileKind::Statistics)?;
	if subgraphs.page_count() != 1 || freelist.page_count() != 1 || statistics.page_count() != 1 {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stage-4 auxiliary files must contain one root page",
		));
	}
	let mut page = AlignedPage::zeroed();
	subgraphs.read_page(PageNo::new(0), &mut page)?;
	if decode_subgraph_directory(&page, PageNo::new(0))? != (None, Vec::new()) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"stage-4 subgraph directory is not empty",
		));
	}
	freelist.read_page(PageNo::new(0), &mut page)?;
	let (_first, next, bitmap) = decode_freelist(&page, PageNo::new(0))?;
	if next.is_some() || bitmap.iter().any(|byte| *byte != 0) {
		return Err(FerrosGrynnError::invalid_storage_file(
			"immutable generation freelist is not empty",
		));
	}
	statistics.read_page(PageNo::new(0), &mut page)?;
	let stats = decode_graph_statistics(&page)?;
	if stats.node_count != superblock.node_count || stats.edge_count != superblock.edge_count {
		return Err(FerrosGrynnError::invalid_storage_file(
			"statistics counts disagree with GraphSuperblock",
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
