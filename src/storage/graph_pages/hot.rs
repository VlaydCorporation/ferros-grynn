#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;

/// Decoded node hot-slot value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct NodeHotSlot {
	pub(in crate::storage) generation: u32,
	pub(in crate::storage) subgraph_slot: Option<u32>,
	pub(in crate::storage) label_id: Option<u32>,
	pub(in crate::storage) table_id: Option<u32>,
	pub(in crate::storage) out_count: u32,
	pub(in crate::storage) in_count: u32,
	pub(in crate::storage) undirected_count: u32,
	pub(in crate::storage) flags: NodeSlotFlags,
	pub(in crate::storage) out_overflow: Option<PageNo>,
	pub(in crate::storage) in_overflow: Option<PageNo>,
	pub(in crate::storage) undirected_overflow: Option<PageNo>,
	pub(in crate::storage) properties: Option<PropertyLocation>,
	pub(in crate::storage) out_inline: [Option<crate::values::storage::DiskAtomRef>; 4],
	pub(in crate::storage) in_inline: [Option<crate::values::storage::DiskAtomRef>; 4],
	pub(in crate::storage) undirected_inline: [Option<crate::values::storage::DiskAtomRef>; 2],
}

impl NodeHotSlot {
	/// Canonical tombstone used for every uninitialized hot-page slot.
	pub(in crate::storage) fn tombstone(generation: u32) -> Self {
		Self {
			generation,
			subgraph_slot: None,
			label_id: None,
			table_id: None,
			out_count: 0,
			in_count: 0,
			undirected_count: 0,
			flags: NodeSlotFlags::TOMBSTONE,
			out_overflow: None,
			in_overflow: None,
			undirected_overflow: None,
			properties: None,
			out_inline: [None; 4],
			in_inline: [None; 4],
			undirected_inline: [None; 2],
		}
	}
}

/// Decoded edge hot-slot value.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::storage) struct EdgeHotSlot {
	pub(in crate::storage) topology: EdgeTopologyCode,
	pub(in crate::storage) flags: EdgeSlotFlags,
	pub(in crate::storage) has_transition: bool,
	pub(in crate::storage) generation: u32,
	pub(in crate::storage) source_count: u32,
	pub(in crate::storage) target_count: u32,
	pub(in crate::storage) label_id: Option<u32>,
	pub(in crate::storage) table_id: Option<u32>,
	pub(in crate::storage) weight: Option<f64>,
	pub(in crate::storage) transition_graph: Option<u32>,
	pub(in crate::storage) source_graph: Option<u32>,
	pub(in crate::storage) target_graph: Option<u32>,
	pub(in crate::storage) properties: Option<PropertyLocation>,
	pub(in crate::storage) source_overflow: Option<PageNo>,
	pub(in crate::storage) target_overflow: Option<PageNo>,
	pub(in crate::storage) source_graph_overflow: Option<PageNo>,
	pub(in crate::storage) target_graph_overflow: Option<PageNo>,
	pub(in crate::storage) source_inline: [Option<crate::values::storage::DiskAtomRef>; 2],
	pub(in crate::storage) target_inline: [Option<crate::values::storage::DiskAtomRef>; 2],
}

impl EdgeHotSlot {
	/// Canonical tombstone used for every uninitialized hot-page slot.
	pub(in crate::storage) fn tombstone(generation: u32) -> Self {
		Self {
			topology: EdgeTopologyCode::Directed,
			flags: EdgeSlotFlags::TOMBSTONE,
			has_transition: false,
			generation,
			source_count: 0,
			target_count: 0,
			label_id: None,
			table_id: None,
			weight: None,
			transition_graph: None,
			source_graph: None,
			target_graph: None,
			properties: None,
			source_overflow: None,
			target_overflow: None,
			source_graph_overflow: None,
			target_graph_overflow: None,
			source_inline: [None; 2],
			target_inline: [None; 2],
		}
	}
}

/// Returns the page and within-page slot for a stable disk slot index.
pub(in crate::storage) fn hot_slot_address(slot: u64) -> FerrosGrynnResult<(PageNo, usize)> {
	let slots_per_page = u64::try_from(HOT_SLOTS_PER_PAGE)
		.map_err(|_error| invalid_page(0, "hot slots per page do not fit u64"))?;
	let page_no = PageNo::new(slot / slots_per_page);
	let within = usize::try_from(slot % slots_per_page)
		.map_err(|_error| invalid_page(page_no.get(), "hot slot index does not fit usize"))?;
	Ok((page_no, within))
}

/// Creates a node-hot page with every slot marked tombstone.
pub(in crate::storage) fn new_node_hot_page(
	page_no: PageNo,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	let mut page = AlignedPage::new(PageType::NodeHot, PageFlags::empty(), page_lsn, page_no);
	for slot in 0..HOT_SLOTS_PER_PAGE {
		write_node_hot_slot(&mut page, slot, &NodeHotSlot::tombstone(0))?;
	}
	page.seal()?;
	Ok(page)
}

/// Writes one node slot and leaves sealing to the page-level caller.
pub(in crate::storage) fn write_node_hot_slot(
	page: &mut AlignedPage,
	slot: usize,
	value: &NodeHotSlot,
) -> FerrosGrynnResult<()> {
	validate_node_slot(value)?;
	let range = hot_slot_range(page, PageType::NodeHot, slot)?;
	let bytes = &mut page.as_bytes_mut()[range];
	bytes.fill(0);
	write_u32(bytes, NODE_GENERATION_OFFSET, value.generation);
	write_optional_u32(bytes, NODE_SUBGRAPH_OFFSET, value.subgraph_slot);
	write_optional_u32(bytes, NODE_LABEL_OFFSET, value.label_id);
	write_optional_u32(bytes, NODE_TABLE_OFFSET, value.table_id);
	write_u32(bytes, NODE_OUT_COUNT_OFFSET, value.out_count);
	write_u32(bytes, NODE_IN_COUNT_OFFSET, value.in_count);
	write_u32(bytes, NODE_UNDIRECTED_COUNT_OFFSET, value.undirected_count);
	bytes[NODE_FLAGS_OFFSET] = value.flags.bits();
	write_optional_page(bytes, NODE_OUT_OVERFLOW_OFFSET, value.out_overflow);
	write_optional_page(bytes, NODE_IN_OVERFLOW_OFFSET, value.in_overflow);
	write_optional_page(bytes, NODE_UNDIRECTED_OVERFLOW_OFFSET, value.undirected_overflow);
	write_property_location(
		bytes,
		NODE_PROPERTIES_PAGE_OFFSET,
		NODE_PROPERTIES_SLOT_OFFSET,
		NODE_PROPERTIES_LEN_OFFSET,
		value.properties,
	)?;
	write_disk_refs(bytes, NODE_OUT_INLINE_OFFSET, &value.out_inline);
	write_disk_refs(bytes, NODE_IN_INLINE_OFFSET, &value.in_inline);
	write_disk_refs(bytes, NODE_UNDIRECTED_INLINE_OFFSET, &value.undirected_inline);
	Ok(())
}

/// Reads and validates one node hot slot.
pub(in crate::storage) fn read_node_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<NodeHotSlot> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::NodeHot || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "expected an unflagged NodeHot page"));
	}
	decode_node_hot_slot(page, page_no, slot)
}

/// Reads one node slot from an exclusively owned page whose header was
/// validated before in-memory mutations began.
pub(in crate::storage) fn read_unsealed_node_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<NodeHotSlot> {
	decode_node_hot_slot(page, page_no, slot)
}

fn decode_node_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<NodeHotSlot> {
	let range = hot_slot_range(page, PageType::NodeHot, slot)?;
	let bytes = &page.as_bytes()[range];
	validate_zero(&bytes[NODE_PADDING_OFFSET..NODE_OUT_OVERFLOW_OFFSET], page_no, "node padding")?;
	validate_zero(&bytes[NODE_RESERVED_OFFSET..NODE_OUT_INLINE_OFFSET], page_no, "node reserved")?;
	let value = NodeHotSlot {
		generation: read_u32(bytes, NODE_GENERATION_OFFSET),
		subgraph_slot: read_optional_u32(bytes, NODE_SUBGRAPH_OFFSET),
		label_id: read_optional_u32(bytes, NODE_LABEL_OFFSET),
		table_id: read_optional_u32(bytes, NODE_TABLE_OFFSET),
		out_count: read_u32(bytes, NODE_OUT_COUNT_OFFSET),
		in_count: read_u32(bytes, NODE_IN_COUNT_OFFSET),
		undirected_count: read_u32(bytes, NODE_UNDIRECTED_COUNT_OFFSET),
		flags: NodeSlotFlags::from_bits(bytes[NODE_FLAGS_OFFSET])
			.ok_or_else(|| invalid_page(page_no.get(), "node slot contains unknown flags"))?,
		out_overflow: read_optional_page(bytes, NODE_OUT_OVERFLOW_OFFSET),
		in_overflow: read_optional_page(bytes, NODE_IN_OVERFLOW_OFFSET),
		undirected_overflow: read_optional_page(bytes, NODE_UNDIRECTED_OVERFLOW_OFFSET),
		properties: read_property_location(
			bytes,
			NODE_PROPERTIES_PAGE_OFFSET,
			NODE_PROPERTIES_SLOT_OFFSET,
			NODE_PROPERTIES_LEN_OFFSET,
			page_no,
		)?,
		out_inline: read_disk_refs(bytes, NODE_OUT_INLINE_OFFSET)?,
		in_inline: read_disk_refs(bytes, NODE_IN_INLINE_OFFSET)?,
		undirected_inline: read_disk_refs(bytes, NODE_UNDIRECTED_INLINE_OFFSET)?,
	};
	validate_node_slot(&value)?;
	Ok(value)
}

/// Creates an edge-hot page with every slot marked tombstone.
pub(in crate::storage) fn new_edge_hot_page(
	page_no: PageNo,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	let mut page = AlignedPage::new(PageType::EdgeHot, PageFlags::empty(), page_lsn, page_no);
	for slot in 0..HOT_SLOTS_PER_PAGE {
		write_edge_hot_slot(&mut page, slot, &EdgeHotSlot::tombstone(0))?;
	}
	page.seal()?;
	Ok(page)
}

/// Writes one edge slot and leaves sealing to the page-level caller.
pub(in crate::storage) fn write_edge_hot_slot(
	page: &mut AlignedPage,
	slot: usize,
	value: &EdgeHotSlot,
) -> FerrosGrynnResult<()> {
	validate_edge_slot(value)?;
	let range = hot_slot_range(page, PageType::EdgeHot, slot)?;
	let bytes = &mut page.as_bytes_mut()[range];
	bytes.fill(0);
	bytes[EDGE_TOPOLOGY_OFFSET] = value.topology.get();
	bytes[EDGE_FLAGS_OFFSET] = value.flags.bits();
	bytes[EDGE_KIND_EXT_OFFSET] = u8::from(value.has_transition);
	write_u32(bytes, EDGE_GENERATION_OFFSET, value.generation);
	write_u32(bytes, EDGE_SOURCE_COUNT_OFFSET, value.source_count);
	write_u32(bytes, EDGE_TARGET_COUNT_OFFSET, value.target_count);
	write_optional_u32(bytes, EDGE_LABEL_OFFSET, value.label_id);
	write_optional_u32(bytes, EDGE_TABLE_OFFSET, value.table_id);
	write_f64(bytes, EDGE_WEIGHT_OFFSET, value.weight.unwrap_or(f64::NAN));
	write_optional_u32(bytes, EDGE_TRANSITION_GRAPH_OFFSET, value.transition_graph);
	write_optional_u32(bytes, EDGE_SOURCE_GRAPH_OFFSET, value.source_graph);
	write_optional_u32(bytes, EDGE_TARGET_GRAPH_OFFSET, value.target_graph);
	write_property_location(
		bytes,
		EDGE_PROPERTIES_PAGE_OFFSET,
		EDGE_PROPERTIES_SLOT_OFFSET,
		EDGE_PROPERTIES_LEN_OFFSET,
		value.properties,
	)?;
	write_optional_page(bytes, EDGE_SOURCE_OVERFLOW_OFFSET, value.source_overflow);
	write_optional_page(bytes, EDGE_TARGET_OVERFLOW_OFFSET, value.target_overflow);
	write_optional_page(bytes, EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET, value.source_graph_overflow);
	write_optional_page(bytes, EDGE_TARGET_GRAPH_OVERFLOW_OFFSET, value.target_graph_overflow);
	write_disk_refs(bytes, EDGE_SOURCE_INLINE_OFFSET, &value.source_inline);
	write_disk_refs(bytes, EDGE_TARGET_INLINE_OFFSET, &value.target_inline);
	Ok(())
}

/// Reads and validates one edge hot slot.
pub(in crate::storage) fn read_edge_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<EdgeHotSlot> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::EdgeHot || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "expected an unflagged EdgeHot page"));
	}
	decode_edge_hot_slot(page, page_no, slot)
}

/// Reads one edge slot from an exclusively owned page whose header was
/// validated before in-memory mutations began.
pub(in crate::storage) fn read_unsealed_edge_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<EdgeHotSlot> {
	decode_edge_hot_slot(page, page_no, slot)
}

fn decode_edge_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<EdgeHotSlot> {
	let range = hot_slot_range(page, PageType::EdgeHot, slot)?;
	let bytes = &page.as_bytes()[range];
	validate_zero(&bytes[EDGE_PADDING_A_OFFSET..EDGE_GENERATION_OFFSET], page_no, "edge padding")?;
	validate_zero(
		&bytes[EDGE_PADDING_B_OFFSET..EDGE_PROPERTIES_PAGE_OFFSET],
		page_no,
		"edge padding",
	)?;
	validate_zero(
		&bytes[EDGE_RESERVED_A_OFFSET..EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET],
		page_no,
		"edge reserved",
	)?;
	validate_zero(
		&bytes[EDGE_RESERVED_B_OFFSET..EDGE_SOURCE_INLINE_OFFSET],
		page_no,
		"edge reserved",
	)?;
	validate_zero(&bytes[EDGE_RESERVED_C_OFFSET..], page_no, "edge reserved")?;
	let kind_ext = bytes[EDGE_KIND_EXT_OFFSET];
	if kind_ext & !0x01 != 0 {
		return Err(invalid_page(page_no.get(), "edge kind extension contains reserved bits"));
	}
	let raw_weight = read_f64(bytes, EDGE_WEIGHT_OFFSET);
	let value = EdgeHotSlot {
		topology: EdgeTopologyCode::from_raw(bytes[EDGE_TOPOLOGY_OFFSET])
			.ok_or_else(|| invalid_page(page_no.get(), "edge slot contains unknown topology"))?,
		flags: EdgeSlotFlags::from_bits(bytes[EDGE_FLAGS_OFFSET])
			.ok_or_else(|| invalid_page(page_no.get(), "edge slot contains unknown flags"))?,
		has_transition: kind_ext & 0x01 != 0,
		generation: read_u32(bytes, EDGE_GENERATION_OFFSET),
		source_count: read_u32(bytes, EDGE_SOURCE_COUNT_OFFSET),
		target_count: read_u32(bytes, EDGE_TARGET_COUNT_OFFSET),
		label_id: read_optional_u32(bytes, EDGE_LABEL_OFFSET),
		table_id: read_optional_u32(bytes, EDGE_TABLE_OFFSET),
		weight: (!raw_weight.is_nan()).then_some(raw_weight),
		transition_graph: read_optional_u32(bytes, EDGE_TRANSITION_GRAPH_OFFSET),
		source_graph: read_optional_u32(bytes, EDGE_SOURCE_GRAPH_OFFSET),
		target_graph: read_optional_u32(bytes, EDGE_TARGET_GRAPH_OFFSET),
		properties: read_property_location(
			bytes,
			EDGE_PROPERTIES_PAGE_OFFSET,
			EDGE_PROPERTIES_SLOT_OFFSET,
			EDGE_PROPERTIES_LEN_OFFSET,
			page_no,
		)?,
		source_overflow: read_optional_page(bytes, EDGE_SOURCE_OVERFLOW_OFFSET),
		target_overflow: read_optional_page(bytes, EDGE_TARGET_OVERFLOW_OFFSET),
		source_graph_overflow: read_optional_page(bytes, EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET),
		target_graph_overflow: read_optional_page(bytes, EDGE_TARGET_GRAPH_OVERFLOW_OFFSET),
		source_inline: read_disk_refs(bytes, EDGE_SOURCE_INLINE_OFFSET)?,
		target_inline: read_disk_refs(bytes, EDGE_TARGET_INLINE_OFFSET)?,
	};
	validate_edge_slot(&value)?;
	Ok(value)
}
