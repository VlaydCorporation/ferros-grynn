//! Format-v1 codecs for the graph pages used by the first local vertical slice.

use bitflags::bitflags;

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		PropertyMap,
		storage::{
			DecodeContext,
			EncodeContext,
			StorageValue,
			TypeTag,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

/// Fixed byte size of both hot-slot formats.
pub(super) const HOT_SLOT_BYTES: usize = 160;
/// Number of fixed hot slots stored in one page.
pub(super) const HOT_SLOTS_PER_PAGE: usize = 102;
/// Sentinel for an absent `u32` identifier.
pub(super) const NULL_U32: u32 = u32::MAX;
/// Sentinel for a large property chain in a hot-slot property slot field.
pub(super) const LARGE_PROPERTY_SLOT: u32 = u32::MAX - 1;
/// First byte after all hot slots; the remaining bytes are reserved.
const HOT_RESERVED_OFFSET: usize = PAGE_HEADER_SIZE + HOT_SLOTS_PER_PAGE * HOT_SLOT_BYTES;

/// Node-slot field offsets.
const NODE_GENERATION_OFFSET: usize = 0;
const NODE_SUBGRAPH_OFFSET: usize = 4;
const NODE_LABEL_OFFSET: usize = 8;
const NODE_TABLE_OFFSET: usize = 12;
const NODE_OUT_COUNT_OFFSET: usize = 16;
const NODE_IN_COUNT_OFFSET: usize = 20;
const NODE_UNDIRECTED_COUNT_OFFSET: usize = 24;
const NODE_FLAGS_OFFSET: usize = 28;
const NODE_PADDING_OFFSET: usize = 29;
const NODE_OUT_OVERFLOW_OFFSET: usize = 32;
const NODE_IN_OVERFLOW_OFFSET: usize = 40;
const NODE_UNDIRECTED_OVERFLOW_OFFSET: usize = 48;
const NODE_PROPERTIES_PAGE_OFFSET: usize = 56;
const NODE_PROPERTIES_SLOT_OFFSET: usize = 64;
const NODE_PROPERTIES_LEN_OFFSET: usize = 68;
const NODE_RESERVED_OFFSET: usize = 72;
const NODE_OUT_INLINE_OFFSET: usize = 80;
const NODE_IN_INLINE_OFFSET: usize = 112;
const NODE_UNDIRECTED_INLINE_OFFSET: usize = 144;

/// Edge-slot field offsets.
const EDGE_TOPOLOGY_OFFSET: usize = 0;
const EDGE_FLAGS_OFFSET: usize = 1;
const EDGE_KIND_EXT_OFFSET: usize = 2;
const EDGE_PADDING_A_OFFSET: usize = 3;
const EDGE_GENERATION_OFFSET: usize = 4;
const EDGE_SOURCE_COUNT_OFFSET: usize = 8;
const EDGE_TARGET_COUNT_OFFSET: usize = 12;
const EDGE_LABEL_OFFSET: usize = 16;
const EDGE_TABLE_OFFSET: usize = 20;
const EDGE_WEIGHT_OFFSET: usize = 24;
const EDGE_TRANSITION_GRAPH_OFFSET: usize = 32;
const EDGE_SOURCE_GRAPH_OFFSET: usize = 36;
const EDGE_TARGET_GRAPH_OFFSET: usize = 40;
const EDGE_PROPERTIES_SLOT_OFFSET: usize = 44;
const EDGE_PROPERTIES_LEN_OFFSET: usize = 48;
const EDGE_PADDING_B_OFFSET: usize = 52;
const EDGE_PROPERTIES_PAGE_OFFSET: usize = 56;
const EDGE_SOURCE_OVERFLOW_OFFSET: usize = 64;
const EDGE_TARGET_OVERFLOW_OFFSET: usize = 72;
const EDGE_RESERVED_A_OFFSET: usize = 80;
const EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET: usize = 88;
const EDGE_TARGET_GRAPH_OVERFLOW_OFFSET: usize = 96;
const EDGE_RESERVED_B_OFFSET: usize = 104;
const EDGE_SOURCE_INLINE_OFFSET: usize = 112;
const EDGE_TARGET_INLINE_OFFSET: usize = 128;
const EDGE_RESERVED_C_OFFSET: usize = 144;

/// Adjacency overflow fields.
const OVERFLOW_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const OVERFLOW_COUNT_OFFSET: usize = OVERFLOW_NEXT_PAGE_OFFSET + size_of::<u64>();
const OVERFLOW_RESERVED_OFFSET: usize = OVERFLOW_COUNT_OFFSET + size_of::<u32>();
const OVERFLOW_ENTRIES_OFFSET: usize = OVERFLOW_RESERVED_OFFSET + size_of::<u32>();
/// Number of plain disk references in one adjacency overflow page.
pub(super) const OVERFLOW_REFS_PER_PAGE: usize =
	(PAGE_SIZE - OVERFLOW_ENTRIES_OFFSET) / size_of::<u64>();
/// Number of qualified disk references in one cross-level overflow page.
pub(super) const CROSS_LEVEL_REFS_PER_PAGE: usize =
	(PAGE_SIZE - OVERFLOW_ENTRIES_OFFSET) / QualifiedDiskAtomRef::ENCODED_BYTES;

/// Property heap fixed fields and slot-directory origin.
const PROPERTY_SLOT_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
const PROPERTY_FREE_START_OFFSET: usize = PROPERTY_SLOT_COUNT_OFFSET + size_of::<u16>();
const PROPERTY_FREE_END_OFFSET: usize = PROPERTY_FREE_START_OFFSET + size_of::<u16>();
const PROPERTY_RESERVED_OFFSET: usize = PROPERTY_FREE_END_OFFSET + size_of::<u16>();
const PROPERTY_SLOT_DIRECTORY_OFFSET: usize = PROPERTY_RESERVED_OFFSET + size_of::<u16>();
const PROPERTY_SLOT_ENTRY_BYTES: usize = size_of::<u16>() * 2;
/// Largest inline property record; larger records use a chunk chain.
pub(super) const MAX_INLINE_PROPERTY_BYTES: usize = (PAGE_SIZE - PAGE_HEADER_SIZE) / 2;

/// Large-property chunk fields.
const LARGE_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const LARGE_DATA_LEN_OFFSET: usize = LARGE_NEXT_PAGE_OFFSET + size_of::<u64>();
const LARGE_RESERVED_OFFSET: usize = LARGE_DATA_LEN_OFFSET + size_of::<u32>();
const LARGE_DATA_OFFSET: usize = LARGE_RESERVED_OFFSET + size_of::<u32>();
/// Raw bytes held by one large-property chunk.
pub(super) const LARGE_PROPERTY_CHUNK_BYTES: usize = PAGE_SIZE - LARGE_DATA_OFFSET;

/// Subgraph-directory fields.
const SUBGRAPH_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const SUBGRAPH_ENTRY_COUNT_OFFSET: usize = SUBGRAPH_NEXT_PAGE_OFFSET + size_of::<u64>();
const SUBGRAPH_LIVE_COUNT_OFFSET: usize = SUBGRAPH_ENTRY_COUNT_OFFSET + size_of::<u32>();
const SUBGRAPH_RESERVED_OFFSET: usize = SUBGRAPH_LIVE_COUNT_OFFSET + size_of::<u32>();
const SUBGRAPH_ENTRIES_OFFSET: usize = SUBGRAPH_RESERVED_OFFSET + 16;
const SUBGRAPH_ENTRY_BYTES: usize = 32;
/// Entries available in one subgraph-directory page.
pub(super) const SUBGRAPHS_PER_PAGE: usize =
	(PAGE_SIZE - SUBGRAPH_ENTRIES_OFFSET) / SUBGRAPH_ENTRY_BYTES;

/// Freelist fixed fields and bitmap origin.
const FREELIST_FIRST_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const FREELIST_NEXT_PAGE_OFFSET: usize = FREELIST_FIRST_PAGE_OFFSET + size_of::<u64>();
const FREELIST_BITMAP_OFFSET: usize = FREELIST_NEXT_PAGE_OFFSET + size_of::<u64>();
/// Pages represented by one format-v1 freelist bitmap.
pub(super) const FREELIST_PAGES_COVERED: u64 =
	((PAGE_SIZE - FREELIST_BITMAP_OFFSET) * u8::BITS as usize) as u64;

/// Statistics root scalar fields.
const STATS_NODE_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
const STATS_EDGE_COUNT_OFFSET: usize = STATS_NODE_COUNT_OFFSET + size_of::<u64>();
const STATS_AVG_OUT_OFFSET: usize = STATS_EDGE_COUNT_OFFSET + size_of::<u64>();
const STATS_AVG_IN_OFFSET: usize = STATS_AVG_OUT_OFFSET + size_of::<f64>();
const STATS_MAX_OUT_OFFSET: usize = STATS_AVG_IN_OFFSET + size_of::<f64>();
const STATS_MAX_IN_OFFSET: usize = STATS_MAX_OUT_OFFSET + size_of::<u64>();
const STATS_DEGREE_HISTOGRAM_OFFSET: usize = STATS_MAX_IN_OFFSET + size_of::<u64>();
const STATS_RESERVED_OFFSET: usize = STATS_DEGREE_HISTOGRAM_OFFSET + size_of::<u64>();

bitflags! {
	/// Assigned node-hot slot flags.
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub(super) struct NodeSlotFlags: u8 {
		/// Node contains a nested graph.
		const META = 0x80;
		/// Slot is not live; only the generation is meaningful.
		const TOMBSTONE = 0x40;
		/// Traversal scratch marker persisted by explicit algorithms.
		const VISITED = 0x20;
		/// Query-result membership marker.
		const IN_RESULT = 0x08;
		/// Articulation-point marker.
		const CUT_VERTEX = 0x04;
	}

	/// Assigned edge-hot slot flags.
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub(super) struct EdgeSlotFlags: u8 {
		/// Slot is not live; only the generation is meaningful.
		const TOMBSTONE = 0x01;
		/// Traversal scratch marker persisted by explicit algorithms.
		const VISITED = 0x02;
		/// Edge has metagraph semantics.
		const META = 0x04;
		/// Edge permits multiple equivalent incidences.
		const MULTI = 0x08;
		/// Edge is a bridge in a stored analysis result.
		const BRIDGE = 0x10;
		/// Query-result membership marker.
		const IN_RESULT = 0x20;
		/// At least one endpoint belongs to a nested graph.
		const CROSS_LEVEL = 0x40;
	}
}

/// Closed topology discriminants persisted by `EdgeHotSlot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum EdgeTopologyCode {
	/// One local source and one local target.
	Directed = 0,
	/// Two unordered endpoints.
	Undirected = 1,
	/// One source and target traversable in both directions.
	Bidirectional = 2,
	/// Directed incidence with more than two endpoints.
	HyperDirected = 3,
	/// Unordered incidence with more than two endpoints.
	HyperUndirected = 4,
	/// Both binary endpoints reference the same atom.
	Loop = 5,
}

impl EdgeTopologyCode {
	/// Decodes only format-v1 assigned topology bytes.
	fn from_raw(value: u8) -> Option<Self> {
		match value {
			0 => Some(Self::Directed),
			1 => Some(Self::Undirected),
			2 => Some(Self::Bidirectional),
			3 => Some(Self::HyperDirected),
			4 => Some(Self::HyperUndirected),
			5 => Some(Self::Loop),
			_ => None,
		}
	}

	/// Returns the stable format byte.
	const fn get(self) -> u8 {
		self as u8
	}
}

/// Location of one canonical property map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PropertyLocation {
	/// Record in one property slotted page.
	Inline {
		/// Property heap page number.
		page_no: PageNo,
		/// Slot-directory index.
		slot: u16,
		/// Canonical record length.
		length: u32,
	},
	/// Raw canonical record split across large-property pages.
	Large {
		/// First chunk page.
		first_page: PageNo,
		/// Complete canonical record length.
		length: u32,
	},
}

/// Decoded node hot-slot value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NodeHotSlot {
	pub(super) generation: u32,
	pub(super) subgraph_slot: Option<u32>,
	pub(super) label_id: Option<u32>,
	pub(super) table_id: Option<u32>,
	pub(super) out_count: u32,
	pub(super) in_count: u32,
	pub(super) undirected_count: u32,
	pub(super) flags: NodeSlotFlags,
	pub(super) out_overflow: Option<PageNo>,
	pub(super) in_overflow: Option<PageNo>,
	pub(super) undirected_overflow: Option<PageNo>,
	pub(super) properties: Option<PropertyLocation>,
	pub(super) out_inline: [Option<crate::values::storage::DiskAtomRef>; 4],
	pub(super) in_inline: [Option<crate::values::storage::DiskAtomRef>; 4],
	pub(super) undirected_inline: [Option<crate::values::storage::DiskAtomRef>; 2],
}

impl NodeHotSlot {
	/// Canonical tombstone used for every uninitialized hot-page slot.
	pub(super) fn tombstone(generation: u32) -> Self {
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
pub(super) struct EdgeHotSlot {
	pub(super) topology: EdgeTopologyCode,
	pub(super) flags: EdgeSlotFlags,
	pub(super) has_transition: bool,
	pub(super) generation: u32,
	pub(super) source_count: u32,
	pub(super) target_count: u32,
	pub(super) label_id: Option<u32>,
	pub(super) table_id: Option<u32>,
	pub(super) weight: Option<f64>,
	pub(super) transition_graph: Option<u32>,
	pub(super) source_graph: Option<u32>,
	pub(super) target_graph: Option<u32>,
	pub(super) properties: Option<PropertyLocation>,
	pub(super) source_overflow: Option<PageNo>,
	pub(super) target_overflow: Option<PageNo>,
	pub(super) source_graph_overflow: Option<PageNo>,
	pub(super) target_graph_overflow: Option<PageNo>,
	pub(super) source_inline: [Option<crate::values::storage::DiskAtomRef>; 2],
	pub(super) target_inline: [Option<crate::values::storage::DiskAtomRef>; 2],
}

/// Minimal planner statistics persisted by the first local slice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GraphStatistics {
	pub(super) node_count: u64,
	pub(super) edge_count: u64,
	pub(super) average_out_degree: f64,
	pub(super) average_in_degree: f64,
	pub(super) maximum_out_degree: u64,
	pub(super) maximum_in_degree: u64,
}

/// Encodes one format-v1 statistics root without speculative histograms.
pub(super) fn encode_graph_statistics(
	statistics: GraphStatistics,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<AlignedPage> {
	if !statistics.average_out_degree.is_finite()
		|| !statistics.average_in_degree.is_finite()
		|| statistics.average_out_degree < 0.0
		|| statistics.average_in_degree < 0.0
	{
		return Err(invalid_page(0, "graph degree averages must be finite and non-negative"));
	}
	let mut page =
		AlignedPage::new(PageType::StatisticsRoot, PageFlags::empty(), page_lsn, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u64(bytes, STATS_NODE_COUNT_OFFSET, statistics.node_count);
	write_u64(bytes, STATS_EDGE_COUNT_OFFSET, statistics.edge_count);
	write_u64(bytes, STATS_AVG_OUT_OFFSET, statistics.average_out_degree.to_bits());
	write_u64(bytes, STATS_AVG_IN_OFFSET, statistics.average_in_degree.to_bits());
	write_u64(bytes, STATS_MAX_OUT_OFFSET, statistics.maximum_out_degree);
	write_u64(bytes, STATS_MAX_IN_OFFSET, statistics.maximum_in_degree);
	write_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET, None);
	page.seal()?;
	Ok(page)
}

/// Decodes and validates one minimal format-v1 statistics root.
pub(super) fn decode_graph_statistics(page: &AlignedPage) -> FerrosGrynnResult<GraphStatistics> {
	let page_no = PageNo::new(0);
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::StatisticsRoot || !header.flags().is_empty() {
		return Err(invalid_page(0, "invalid StatisticsRoot page header"));
	}
	let bytes = page.as_bytes();
	if read_optional_page(bytes, STATS_DEGREE_HISTOGRAM_OFFSET).is_some() {
		return Err(invalid_page(0, "unsupported degree histogram root"));
	}
	validate_zero(&bytes[STATS_RESERVED_OFFSET..], page_no, "statistics reserved bytes")?;
	let statistics = GraphStatistics {
		node_count: read_u64(bytes, STATS_NODE_COUNT_OFFSET),
		edge_count: read_u64(bytes, STATS_EDGE_COUNT_OFFSET),
		average_out_degree: f64::from_bits(read_u64(bytes, STATS_AVG_OUT_OFFSET)),
		average_in_degree: f64::from_bits(read_u64(bytes, STATS_AVG_IN_OFFSET)),
		maximum_out_degree: read_u64(bytes, STATS_MAX_OUT_OFFSET),
		maximum_in_degree: read_u64(bytes, STATS_MAX_IN_OFFSET),
	};
	if !statistics.average_out_degree.is_finite()
		|| !statistics.average_in_degree.is_finite()
		|| statistics.average_out_degree < 0.0
		|| statistics.average_in_degree < 0.0
	{
		return Err(invalid_page(0, "graph degree averages must be finite and non-negative"));
	}
	Ok(statistics)
}

impl EdgeHotSlot {
	/// Canonical tombstone used for every uninitialized hot-page slot.
	pub(super) fn tombstone(generation: u32) -> Self {
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
pub(super) fn hot_slot_address(slot: u64) -> FerrosGrynnResult<(PageNo, usize)> {
	let slots_per_page = u64::try_from(HOT_SLOTS_PER_PAGE)
		.map_err(|_error| invalid_page(0, "hot slots per page do not fit u64"))?;
	let page_no = PageNo::new(slot / slots_per_page);
	let within = usize::try_from(slot % slots_per_page)
		.map_err(|_error| invalid_page(page_no.get(), "hot slot index does not fit usize"))?;
	Ok((page_no, within))
}

/// Creates a node-hot page with every slot marked tombstone.
pub(super) fn new_node_hot_page(
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
pub(super) fn write_node_hot_slot(
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
pub(super) fn read_node_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<NodeHotSlot> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::NodeHot || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "expected an unflagged NodeHot page"));
	}
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
pub(super) fn new_edge_hot_page(
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
pub(super) fn write_edge_hot_slot(
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
pub(super) fn read_edge_hot_slot(
	page: &AlignedPage,
	page_no: PageNo,
	slot: usize,
) -> FerrosGrynnResult<EdgeHotSlot> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::EdgeHot || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "expected an unflagged EdgeHot page"));
	}
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

/// One side of a qualified edge endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum EndpointSide {
	/// Source endpoint.
	Source = 0,
	/// Target endpoint.
	Target = 1,
}

/// Cross-level endpoint stored in a qualified overflow page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct QualifiedDiskAtomRef {
	/// Nested graph slot.
	pub(super) graph_slot: u32,
	/// Endpoint side retained for a shared overflow chain.
	pub(super) side: EndpointSide,
	/// Atom address within the nested graph.
	pub(super) atom: crate::values::storage::DiskAtomRef,
}

impl QualifiedDiskAtomRef {
	/// Exact format-v1 entry width including canonical padding.
	const ENCODED_BYTES: usize = 16;
}

/// Encodes one plain adjacency-overflow page.
pub(super) fn encode_adjacency_overflow(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[crate::values::storage::DiskAtomRef],
) -> FerrosGrynnResult<AlignedPage> {
	if entries.len() > OVERFLOW_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "adjacency overflow page is over capacity"));
	}
	let mut page =
		AlignedPage::new(PageType::AdjacencyOverflow, PageFlags::OVERFLOW, page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET, next_page);
	write_u32(
		bytes,
		OVERFLOW_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "overflow count exceeds u32"))?,
	);
	for (index, entry) in entries.iter().enumerate() {
		write_u64(bytes, OVERFLOW_ENTRIES_OFFSET + index * size_of::<u64>(), entry.raw());
	}
	page.seal()?;
	Ok(page)
}

/// Decodes one plain adjacency-overflow page.
pub(super) fn decode_adjacency_overflow(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(Option<PageNo>, Vec<crate::values::storage::DiskAtomRef>)> {
	validate_overflow_header(page, page_no, PageType::AdjacencyOverflow)?;
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[OVERFLOW_RESERVED_OFFSET..OVERFLOW_ENTRIES_OFFSET],
		page_no,
		"overflow padding",
	)?;
	let count = usize::try_from(read_u32(bytes, OVERFLOW_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "overflow count does not fit usize"))?;
	if count > OVERFLOW_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "adjacency overflow count exceeds capacity"));
	}
	let entries = read_required_disk_refs(bytes, OVERFLOW_ENTRIES_OFFSET, count)?;
	let end = OVERFLOW_ENTRIES_OFFSET + count * size_of::<u64>();
	validate_zero(&bytes[end..], page_no, "adjacency overflow trailing bytes")?;
	Ok((read_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET), entries))
}

/// Encodes one cross-level adjacency-overflow page.
pub(super) fn encode_cross_level_overflow(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[QualifiedDiskAtomRef],
) -> FerrosGrynnResult<AlignedPage> {
	if entries.len() > CROSS_LEVEL_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "cross-level overflow page is over capacity"));
	}
	let mut page = AlignedPage::new(
		PageType::CrossLevelAdjacencyOverflow,
		PageFlags::OVERFLOW,
		page_lsn,
		page_no,
	);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET, next_page);
	write_u32(
		bytes,
		OVERFLOW_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "cross-level count exceeds u32"))?,
	);
	for (index, entry) in entries.iter().enumerate() {
		let offset = OVERFLOW_ENTRIES_OFFSET + index * QualifiedDiskAtomRef::ENCODED_BYTES;
		write_u32(bytes, offset, entry.graph_slot);
		bytes[offset + size_of::<u32>()] = entry.side as u8;
		write_u64(bytes, offset + 8, entry.atom.raw());
	}
	page.seal()?;
	Ok(page)
}

/// Decodes one cross-level adjacency-overflow page.
pub(super) fn decode_cross_level_overflow(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(Option<PageNo>, Vec<QualifiedDiskAtomRef>)> {
	validate_overflow_header(page, page_no, PageType::CrossLevelAdjacencyOverflow)?;
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[OVERFLOW_RESERVED_OFFSET..OVERFLOW_ENTRIES_OFFSET],
		page_no,
		"overflow padding",
	)?;
	let count = usize::try_from(read_u32(bytes, OVERFLOW_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "cross-level count does not fit usize"))?;
	if count > CROSS_LEVEL_REFS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "cross-level overflow count exceeds capacity"));
	}
	let mut entries = Vec::with_capacity(count);
	for index in 0..count {
		let offset = OVERFLOW_ENTRIES_OFFSET + index * QualifiedDiskAtomRef::ENCODED_BYTES;
		let side = match bytes[offset + size_of::<u32>()] {
			0 => EndpointSide::Source,
			1 => EndpointSide::Target,
			_ => return Err(invalid_page(page_no.get(), "cross-level endpoint side is invalid")),
		};
		validate_zero(&bytes[offset + 5..offset + 8], page_no, "cross-level entry padding")?;
		entries.push(QualifiedDiskAtomRef {
			graph_slot: read_u32(bytes, offset),
			side,
			atom: crate::values::storage::DiskAtomRef::from_raw(read_u64(bytes, offset + 8))?,
		});
	}
	let end = OVERFLOW_ENTRIES_OFFSET + count * QualifiedDiskAtomRef::ENCODED_BYTES;
	validate_zero(&bytes[end..], page_no, "cross-level overflow trailing bytes")?;
	Ok((read_optional_page(bytes, OVERFLOW_NEXT_PAGE_OFFSET), entries))
}

/// Mutable builder for one canonical property slotted page.
pub(super) struct PropertyPageBuilder {
	/// Complete aligned page being filled.
	page: AlignedPage,
	/// Next slot-directory entry.
	slot_count: u16,
	/// First unused byte after the slot directory.
	free_start: u16,
	/// First byte of the packed record zone.
	free_end: u16,
}

impl PropertyPageBuilder {
	/// Creates one empty property page.
	pub(super) fn new(page_no: PageNo, page_lsn: PageLsn) -> Self {
		Self {
			page: AlignedPage::new(PageType::PropHeap, PageFlags::empty(), page_lsn, page_no),
			slot_count: 0,
			free_start: PROPERTY_SLOT_DIRECTORY_OFFSET as u16,
			free_end: PAGE_SIZE as u16,
		}
	}

	/// Inserts one record when it fits, returning its stable directory slot.
	///
	/// Returns `Ok(None)` when a different page is required.
	pub(super) fn try_insert(&mut self, record: &[u8]) -> FerrosGrynnResult<Option<u16>> {
		if record.len() > MAX_INLINE_PROPERTY_BYTES {
			return Ok(None);
		}
		let required = record
			.len()
			.checked_add(PROPERTY_SLOT_ENTRY_BYTES)
			.ok_or_else(|| invalid_page(0, "property record size overflows usize"))?;
		if required > usize::from(self.free_end - self.free_start) {
			return Ok(None);
		}
		let record_len = u16::try_from(record.len())
			.map_err(|_error| invalid_page(0, "inline property record exceeds u16"))?;
		let record_offset = self
			.free_end
			.checked_sub(record_len)
			.ok_or_else(|| invalid_page(0, "property page free_end underflow"))?;
		let slot_offset = usize::from(self.free_start);
		let bytes = self.page.as_bytes_mut();
		write_u16(bytes, slot_offset, record_offset);
		write_u16(bytes, slot_offset + size_of::<u16>(), record_len);
		bytes[usize::from(record_offset)..usize::from(record_offset) + record.len()]
			.copy_from_slice(record);
		let slot = self.slot_count;
		self.slot_count = self
			.slot_count
			.checked_add(1)
			.ok_or_else(|| invalid_page(0, "property slot count overflows u16"))?;
		self.free_start = self
			.free_start
			.checked_add(PROPERTY_SLOT_ENTRY_BYTES as u16)
			.ok_or_else(|| invalid_page(0, "property slot directory overflows u16"))?;
		self.free_end = record_offset;
		Ok(Some(slot))
	}

	/// Seals metadata and returns the complete page.
	pub(super) fn finish(mut self) -> FerrosGrynnResult<AlignedPage> {
		let bytes = self.page.as_bytes_mut();
		write_u16(bytes, PROPERTY_SLOT_COUNT_OFFSET, self.slot_count);
		write_u16(bytes, PROPERTY_FREE_START_OFFSET, self.free_start);
		write_u16(bytes, PROPERTY_FREE_END_OFFSET, self.free_end);
		self.page.seal()?;
		Ok(self.page)
	}
}

/// Reads one property record after validating the complete slotted page.
pub(super) fn read_property_record(
	page: &AlignedPage,
	page_no: PageNo,
	slot: u16,
	expected_len: u32,
) -> FerrosGrynnResult<&[u8]> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::PropHeap || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "expected an unflagged PropHeap page"));
	}
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[PROPERTY_RESERVED_OFFSET..PROPERTY_SLOT_DIRECTORY_OFFSET],
		page_no,
		"property page padding",
	)?;
	let slot_count = read_u16(bytes, PROPERTY_SLOT_COUNT_OFFSET);
	let free_start = read_u16(bytes, PROPERTY_FREE_START_OFFSET);
	let free_end = read_u16(bytes, PROPERTY_FREE_END_OFFSET);
	let expected_free_start = PROPERTY_SLOT_DIRECTORY_OFFSET
		.checked_add(usize::from(slot_count) * PROPERTY_SLOT_ENTRY_BYTES)
		.ok_or_else(|| invalid_page(page_no.get(), "property slot directory overflows usize"))?;
	if usize::from(free_start) != expected_free_start
		|| free_start > free_end
		|| usize::from(free_end) > PAGE_SIZE
		|| slot >= slot_count
	{
		return Err(invalid_page(page_no.get(), "property page metadata is inconsistent"));
	}
	let slot_offset =
		PROPERTY_SLOT_DIRECTORY_OFFSET + usize::from(slot) * PROPERTY_SLOT_ENTRY_BYTES;
	let record_offset = usize::from(read_u16(bytes, slot_offset));
	let record_len = usize::from(read_u16(bytes, slot_offset + size_of::<u16>()));
	if record_offset == 0 {
		return Err(invalid_page(page_no.get(), "property slot is a tombstone"));
	}
	let record_end = record_offset
		.checked_add(record_len)
		.ok_or_else(|| invalid_page(page_no.get(), "property record range overflows usize"))?;
	if record_offset < usize::from(free_end)
		|| record_end > PAGE_SIZE
		|| u32::try_from(record_len).ok() != Some(expected_len)
	{
		return Err(invalid_page(page_no.get(), "property record range or length is invalid"));
	}
	Ok(&bytes[record_offset..record_end])
}

/// Encodes a runtime property map using the existing canonical value codec.
pub(super) fn encode_property_map(
	properties: &PropertyMap,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	let values = properties
		.iter()
		.map(|(key, value)| {
			StorageValue::from_runtime(value.clone(), context).map(|value| (key, value))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let encoded = CanonicalCodec::encode(StorageValue::Object(values).as_ref())?;
	if encoded.first().copied() != Some(TypeTag::Object.byte()) {
		return Err(FerrosGrynnError::invalid_storage_value_encoding(
			0,
			"property codec did not produce a canonical object",
		));
	}
	Ok(encoded[1..].to_vec())
}

/// Decodes one canonical property record with allocation limits applied by
/// the existing value codec before materialization.
pub(super) fn decode_property_map(
	record: &[u8],
	context: &DecodeContext<'_>,
	limits: DecodeLimits,
) -> FerrosGrynnResult<PropertyMap> {
	let total = record.len().checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_value_encoding(0, "property length overflow")
	})?;
	if total > limits.max_total_allocation {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"property bytes",
			limits.max_total_allocation as u64,
			total as u64,
		));
	}
	let mut encoded = Vec::with_capacity(total);
	encoded.push(TypeTag::Object.byte());
	encoded.extend_from_slice(record);
	let StorageValue::Object(values) = CanonicalCodec::decode(&encoded, limits)? else {
		return Err(FerrosGrynnError::invalid_storage_value_encoding(
			0,
			"property record is not a canonical object body",
		));
	};
	let mut properties = PropertyMap::with_capacity(values.len());
	for (key, value) in values {
		properties.insert(key, value.into_runtime(context)?);
	}
	Ok(properties)
}

/// Splits one large canonical property record into a contiguous chunk chain.
pub(super) fn encode_large_property(
	first_page: PageNo,
	page_lsn: PageLsn,
	record: &[u8],
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	if record.len() <= MAX_INLINE_PROPERTY_BYTES {
		return Err(invalid_page(
			first_page.get(),
			"small property record does not require a large-object chain",
		));
	}
	let page_count = record.len().div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let index_u64 = u64::try_from(index)
			.map_err(|_error| invalid_page(first_page.get(), "large chunk index exceeds u64"))?;
		let page_no =
			first_page.get().checked_add(index_u64).map(PageNo::new).ok_or_else(|| {
				invalid_page(first_page.get(), "large chunk page number overflows u64")
			})?;
		let next_page =
			if index + 1 == page_count {
				None
			} else {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					invalid_page(page_no.get(), "large chunk next page overflows")
				})?))
			};
		let start = index
			.checked_mul(LARGE_PROPERTY_CHUNK_BYTES)
			.ok_or_else(|| invalid_page(page_no.get(), "large chunk offset overflows usize"))?;
		let end = start.saturating_add(LARGE_PROPERTY_CHUNK_BYTES).min(record.len());
		let chunk = &record[start..end];
		let mut page =
			AlignedPage::new(PageType::PropLargeHeap, PageFlags::OVERFLOW, page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		write_optional_page(bytes, LARGE_NEXT_PAGE_OFFSET, next_page);
		write_u32(
			bytes,
			LARGE_DATA_LEN_OFFSET,
			u32::try_from(chunk.len())
				.map_err(|_error| invalid_page(page_no.get(), "large chunk exceeds u32"))?,
		);
		bytes[LARGE_DATA_OFFSET..LARGE_DATA_OFFSET + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
		pages.push(page);
	}
	Ok(pages)
}

/// Decodes one already loaded large-property chain in physical order.
pub(super) fn decode_large_property(
	first_page: PageNo,
	pages: &[AlignedPage],
	expected_len: u32,
	limit: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	let expected_len = usize::try_from(expected_len).map_err(|_error| {
		invalid_page(first_page.get(), "large property length does not fit usize")
	})?;
	if expected_len > limit {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"large property bytes",
			limit as u64,
			expected_len as u64,
		));
	}
	let mut output = Vec::with_capacity(expected_len);
	for (index, page) in pages.iter().enumerate() {
		let index_u64 = u64::try_from(index)
			.map_err(|_error| invalid_page(first_page.get(), "large chunk index exceeds u64"))?;
		let page_no = PageNo::new(
			first_page
				.get()
				.checked_add(index_u64)
				.ok_or_else(|| invalid_page(first_page.get(), "large chunk page overflows"))?,
		);
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::PropLargeHeap || header.flags() != PageFlags::OVERFLOW {
			return Err(invalid_page(page_no.get(), "invalid large-property page header"));
		}
		let bytes = page.as_bytes();
		validate_zero(
			&bytes[LARGE_RESERVED_OFFSET..LARGE_DATA_OFFSET],
			page_no,
			"large chunk padding",
		)?;
		let data_len =
			usize::try_from(read_u32(bytes, LARGE_DATA_LEN_OFFSET)).map_err(|_error| {
				invalid_page(page_no.get(), "large chunk length does not fit usize")
			})?;
		if data_len > LARGE_PROPERTY_CHUNK_BYTES
			|| output.len().saturating_add(data_len) > expected_len
		{
			return Err(invalid_page(page_no.get(), "large chunk length exceeds its bounds"));
		}
		let expected_next = if index + 1 == pages.len() {
			None
		} else {
			Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
				invalid_page(page_no.get(), "large chunk next-page number overflows")
			})?))
		};
		if read_optional_page(bytes, LARGE_NEXT_PAGE_OFFSET) != expected_next {
			return Err(invalid_page(page_no.get(), "large property chain link is inconsistent"));
		}
		output.extend_from_slice(&bytes[LARGE_DATA_OFFSET..LARGE_DATA_OFFSET + data_len]);
		validate_zero(
			&bytes[LARGE_DATA_OFFSET + data_len..],
			page_no,
			"large chunk trailing bytes",
		)?;
	}
	if output.len() != expected_len {
		return Err(invalid_page(first_page.get(), "large property chain length mismatch"));
	}
	Ok(output)
}

/// One nested-graph directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SubgraphEntry {
	pub(super) slot: u32,
	pub(super) generation: u32,
	pub(super) node_capacity: u64,
	pub(super) edge_capacity: u64,
	pub(super) node_count: u32,
	pub(super) edge_count: u32,
}

/// Encodes one subgraph-directory page.
pub(super) fn encode_subgraph_directory(
	page_no: PageNo,
	page_lsn: PageLsn,
	next_page: Option<PageNo>,
	entries: &[SubgraphEntry],
) -> FerrosGrynnResult<AlignedPage> {
	if entries.len() > SUBGRAPHS_PER_PAGE {
		return Err(invalid_page(page_no.get(), "subgraph directory page is over capacity"));
	}
	let mut page = AlignedPage::new(PageType::SubgraphDir, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_optional_page(bytes, SUBGRAPH_NEXT_PAGE_OFFSET, next_page);
	write_u32(
		bytes,
		SUBGRAPH_ENTRY_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "subgraph count exceeds u32"))?,
	);
	write_u32(
		bytes,
		SUBGRAPH_LIVE_COUNT_OFFSET,
		u32::try_from(entries.len())
			.map_err(|_error| invalid_page(page_no.get(), "live subgraph count exceeds u32"))?,
	);
	for (index, entry) in entries.iter().enumerate() {
		let offset = SUBGRAPH_ENTRIES_OFFSET + index * SUBGRAPH_ENTRY_BYTES;
		write_u32(bytes, offset, entry.slot);
		write_u32(bytes, offset + 4, entry.generation);
		write_u64(bytes, offset + 8, entry.node_capacity);
		write_u64(bytes, offset + 16, entry.edge_capacity);
		write_u32(bytes, offset + 24, entry.node_count);
		write_u32(bytes, offset + 28, entry.edge_count);
	}
	page.seal()?;
	Ok(page)
}

/// Decodes one subgraph-directory page.
pub(super) fn decode_subgraph_directory(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(Option<PageNo>, Vec<SubgraphEntry>)> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::SubgraphDir || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid SubgraphDir page header"));
	}
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[SUBGRAPH_RESERVED_OFFSET..SUBGRAPH_ENTRIES_OFFSET],
		page_no,
		"subgraph padding",
	)?;
	let count = usize::try_from(read_u32(bytes, SUBGRAPH_ENTRY_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "subgraph count does not fit usize"))?;
	let live = usize::try_from(read_u32(bytes, SUBGRAPH_LIVE_COUNT_OFFSET))
		.map_err(|_error| invalid_page(page_no.get(), "live subgraph count does not fit usize"))?;
	if count > SUBGRAPHS_PER_PAGE || live > count {
		return Err(invalid_page(page_no.get(), "subgraph directory counts are invalid"));
	}
	let mut entries = Vec::with_capacity(count);
	for index in 0..count {
		let offset = SUBGRAPH_ENTRIES_OFFSET + index * SUBGRAPH_ENTRY_BYTES;
		entries.push(SubgraphEntry {
			slot: read_u32(bytes, offset),
			generation: read_u32(bytes, offset + 4),
			node_capacity: read_u64(bytes, offset + 8),
			edge_capacity: read_u64(bytes, offset + 16),
			node_count: read_u32(bytes, offset + 24),
			edge_count: read_u32(bytes, offset + 28),
		});
	}
	let end = SUBGRAPH_ENTRIES_OFFSET + count * SUBGRAPH_ENTRY_BYTES;
	validate_zero(&bytes[end..], page_no, "subgraph directory trailing bytes")?;
	Ok((read_optional_page(bytes, SUBGRAPH_NEXT_PAGE_OFFSET), entries))
}

/// Creates one freelist page with an explicit covered range and bitmap.
pub(super) fn encode_freelist(
	page_no: PageNo,
	page_lsn: PageLsn,
	first_page_covered: u64,
	next_page: Option<PageNo>,
	bitmap: &[u8],
) -> FerrosGrynnResult<AlignedPage> {
	if bitmap.len() > PAGE_SIZE - FREELIST_BITMAP_OFFSET {
		return Err(invalid_page(page_no.get(), "freelist bitmap is over capacity"));
	}
	let mut page = AlignedPage::new(PageType::Freelist, PageFlags::empty(), page_lsn, page_no);
	let bytes = page.as_bytes_mut();
	write_u64(bytes, FREELIST_FIRST_PAGE_OFFSET, first_page_covered);
	write_optional_page(bytes, FREELIST_NEXT_PAGE_OFFSET, next_page);
	bytes[FREELIST_BITMAP_OFFSET..FREELIST_BITMAP_OFFSET + bitmap.len()].copy_from_slice(bitmap);
	page.seal()?;
	Ok(page)
}

/// Validates and returns the complete fixed-size freelist bitmap.
pub(super) fn decode_freelist(
	page: &AlignedPage,
	page_no: PageNo,
) -> FerrosGrynnResult<(u64, Option<PageNo>, &[u8])> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::Freelist || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "invalid Freelist page header"));
	}
	let bytes = page.as_bytes();
	Ok((
		read_u64(bytes, FREELIST_FIRST_PAGE_OFFSET),
		read_optional_page(bytes, FREELIST_NEXT_PAGE_OFFSET),
		&bytes[FREELIST_BITMAP_OFFSET..],
	))
}

/// Validates a node slot's count, overflow, and canonical tombstone state.
fn validate_node_slot(value: &NodeHotSlot) -> FerrosGrynnResult<()> {
	if value.flags.contains(NodeSlotFlags::TOMBSTONE) {
		return Ok(());
	}
	validate_inline_count(value.out_count, &value.out_inline, value.out_overflow, "node out")?;
	validate_inline_count(value.in_count, &value.in_inline, value.in_overflow, "node in")?;
	validate_inline_count(
		value.undirected_count,
		&value.undirected_inline,
		value.undirected_overflow,
		"node undirected",
	)?;
	if value.flags.contains(NodeSlotFlags::META) != value.subgraph_slot.is_some() {
		return Err(invalid_page(0, "node META flag disagrees with subgraph_slot"));
	}
	Ok(())
}

/// Validates edge topology, counts, overflow, transition, and cross-level
/// fields.
fn validate_edge_slot(value: &EdgeHotSlot) -> FerrosGrynnResult<()> {
	if value.flags.contains(EdgeSlotFlags::TOMBSTONE) {
		return Ok(());
	}
	let total = value
		.source_count
		.checked_add(value.target_count)
		.ok_or_else(|| invalid_page(0, "edge endpoint count overflows u32"))?;
	let topology_valid = match value.topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::Bidirectional | EdgeTopologyCode::Loop => {
			total == 2
		}
		EdgeTopologyCode::Undirected => total == 2 && value.target_count == 0,
		EdgeTopologyCode::HyperDirected => total >= 2,
		EdgeTopologyCode::HyperUndirected => total >= 2 && value.target_count == 0,
	};
	if !topology_valid {
		return Err(invalid_page(0, "edge endpoint counts do not match topology"));
	}
	if value.flags.contains(EdgeSlotFlags::CROSS_LEVEL) {
		validate_cross_level_side(
			value.source_count,
			&value.source_inline,
			value.source_overflow,
			value.source_graph,
			value.source_graph_overflow,
			"edge source",
		)?;
		validate_cross_level_side(
			value.target_count,
			&value.target_inline,
			value.target_overflow,
			value.target_graph,
			value.target_graph_overflow,
			"edge target",
		)?;
	} else {
		validate_inline_count(
			value.source_count,
			&value.source_inline,
			value.source_overflow,
			"edge source",
		)?;
		validate_inline_count(
			value.target_count,
			&value.target_inline,
			value.target_overflow,
			"edge target",
		)?;
	}
	if value.has_transition != value.transition_graph.is_some() {
		return Err(invalid_page(0, "edge transition bit disagrees with transition graph"));
	}
	if !value.flags.contains(EdgeSlotFlags::CROSS_LEVEL)
		&& (value.source_graph.is_some()
			|| value.target_graph.is_some()
			|| value.source_graph_overflow.is_some()
			|| value.target_graph_overflow.is_some())
	{
		return Err(invalid_page(0, "local edge contains cross-level graph addresses"));
	}
	Ok(())
}

fn validate_cross_level_side(
	count: u32,
	inline: &[Option<crate::values::storage::DiskAtomRef>; 2],
	local_overflow: Option<PageNo>,
	graph_slot: Option<u32>,
	graph_overflow: Option<PageNo>,
	field: &str,
) -> FerrosGrynnResult<()> {
	if local_overflow.is_some() {
		return Err(invalid_page(
			0,
			format!("{field} uses a plain overflow for qualified endpoints"),
		));
	}
	if graph_overflow.is_some() {
		if graph_slot.is_some() || inline.iter().any(Option::is_some) || count == 0 {
			return Err(invalid_page(
				0,
				format!("{field} qualified overflow fields are not canonical"),
			));
		}
		return Ok(());
	}
	if count > 2 || graph_slot.is_some() != (count != 0) {
		return Err(invalid_page(0, format!("{field} inline qualified endpoint count is invalid")));
	}
	validate_inline_count(count, inline, None, field)
}

/// Validates inline population and its required continuation pointer.
fn validate_inline_count(
	count: u32,
	inline: &[Option<crate::values::storage::DiskAtomRef>],
	overflow: Option<PageNo>,
	field: &str,
) -> FerrosGrynnResult<()> {
	let inline_capacity = u32::try_from(inline.len())
		.map_err(|_error| invalid_page(0, "inline capacity exceeds u32"))?;
	if (count > inline_capacity) != overflow.is_some() {
		return Err(invalid_page(0, format!("{field} count disagrees with overflow pointer")));
	}
	let populated = usize::try_from(count.min(inline_capacity))
		.map_err(|_error| invalid_page(0, "inline count does not fit usize"))?;
	if inline[..populated].iter().any(Option::is_none)
		|| inline[populated..].iter().any(Option::is_some)
	{
		return Err(invalid_page(0, format!("{field} inline entries are not dense")));
	}
	Ok(())
}

/// Validates the shared overflow page header.
fn validate_overflow_header(
	page: &AlignedPage,
	page_no: PageNo,
	page_type: PageType,
) -> FerrosGrynnResult<()> {
	let header = page.validate(page_no)?;
	if header.page_type() != page_type || header.flags() != PageFlags::OVERFLOW {
		return Err(invalid_page(page_no.get(), "invalid adjacency overflow page header"));
	}
	Ok(())
}

/// Returns one checked fixed hot-slot range.
fn hot_slot_range(
	page: &AlignedPage,
	page_type: PageType,
	slot: usize,
) -> FerrosGrynnResult<std::ops::Range<usize>> {
	if slot >= HOT_SLOTS_PER_PAGE {
		return Err(invalid_page(0, "hot slot index exceeds page capacity"));
	}
	let raw_type = read_u16(page.as_bytes(), 4);
	if raw_type != page_type.get() {
		return Err(invalid_page(0, "hot page type does not match slot codec"));
	}
	let start = PAGE_HEADER_SIZE + slot * HOT_SLOT_BYTES;
	Ok(start..start + HOT_SLOT_BYTES)
}

/// Encodes one optional property location into the three hot-slot fields.
fn write_property_location(
	bytes: &mut [u8],
	page_offset: usize,
	slot_offset: usize,
	len_offset: usize,
	location: Option<PropertyLocation>,
) -> FerrosGrynnResult<()> {
	match location {
		None => {
			write_u64(bytes, page_offset, PageNo::NULL.get());
			write_u32(bytes, slot_offset, NULL_U32);
			write_u32(bytes, len_offset, 0);
		}
		Some(PropertyLocation::Inline {
			page_no,
			slot,
			length,
		}) => {
			write_u64(bytes, page_offset, page_no.get());
			write_u32(bytes, slot_offset, u32::from(slot));
			write_u32(bytes, len_offset, length);
		}
		Some(PropertyLocation::Large {
			first_page,
			length,
		}) => {
			if length == 0 {
				return Err(invalid_page(
					first_page.get(),
					"large property length must be non-zero",
				));
			}
			write_u64(bytes, page_offset, first_page.get());
			write_u32(bytes, slot_offset, LARGE_PROPERTY_SLOT);
			write_u32(bytes, len_offset, length);
		}
	}
	Ok(())
}

/// Decodes one optional property location from hot-slot fields.
fn read_property_location(
	bytes: &[u8],
	page_offset: usize,
	slot_offset: usize,
	len_offset: usize,
	page_no: PageNo,
) -> FerrosGrynnResult<Option<PropertyLocation>> {
	let properties_page = PageNo::new(read_u64(bytes, page_offset));
	let slot = read_u32(bytes, slot_offset);
	let length = read_u32(bytes, len_offset);
	match (properties_page == PageNo::NULL, slot, length) {
		(true, NULL_U32, 0) => Ok(None),
		(false, LARGE_PROPERTY_SLOT, length) if length != 0 => Ok(Some(PropertyLocation::Large {
			first_page: properties_page,
			length,
		})),
		(false, slot, length) if slot < u32::from(u16::MAX) => Ok(Some(PropertyLocation::Inline {
			page_no: properties_page,
			slot: u16::try_from(slot)
				.map_err(|_error| invalid_page(page_no.get(), "property slot does not fit u16"))?,
			length,
		})),
		_ => Err(invalid_page(page_no.get(), "hot-slot property reference is inconsistent")),
	}
}

/// Writes optional disk references with the all-ones sentinel.
fn write_disk_refs(
	bytes: &mut [u8],
	offset: usize,
	values: &[Option<crate::values::storage::DiskAtomRef>],
) {
	for (index, value) in values.iter().enumerate() {
		write_u64(
			bytes,
			offset + index * size_of::<u64>(),
			value.map_or(u64::MAX, crate::values::storage::DiskAtomRef::raw),
		);
	}
}

/// Reads a fixed optional disk-reference array and enforces dense prefix use.
fn read_disk_refs<const N: usize>(
	bytes: &[u8],
	offset: usize,
) -> FerrosGrynnResult<[Option<crate::values::storage::DiskAtomRef>; N]> {
	let mut values = [None; N];
	let mut ended = false;
	for (index, value) in values.iter_mut().enumerate() {
		let raw = read_u64(bytes, offset + index * size_of::<u64>());
		if raw == u64::MAX {
			ended = true;
		} else {
			if ended {
				return Err(invalid_page(0, "inline disk references are not a dense prefix"));
			}
			*value = Some(crate::values::storage::DiskAtomRef::from_raw(raw)?);
		}
	}
	Ok(values)
}

/// Reads a required sequence of plain disk references.
fn read_required_disk_refs(
	bytes: &[u8],
	offset: usize,
	count: usize,
) -> FerrosGrynnResult<Vec<crate::values::storage::DiskAtomRef>> {
	(0..count)
		.map(|index| {
			crate::values::storage::DiskAtomRef::from_raw(read_u64(
				bytes,
				offset + index * size_of::<u64>(),
			))
		})
		.collect()
}

/// Encodes an optional page number.
fn write_optional_page(bytes: &mut [u8], offset: usize, value: Option<PageNo>) {
	write_u64(bytes, offset, value.unwrap_or(PageNo::NULL).get());
}

/// Decodes an optional page number.
fn read_optional_page(bytes: &[u8], offset: usize) -> Option<PageNo> {
	let value = PageNo::new(read_u64(bytes, offset));
	(value != PageNo::NULL).then_some(value)
}

/// Encodes an optional `u32` identifier.
fn write_optional_u32(bytes: &mut [u8], offset: usize, value: Option<u32>) {
	write_u32(bytes, offset, value.unwrap_or(NULL_U32));
}

/// Decodes an optional `u32` identifier.
fn read_optional_u32(bytes: &[u8], offset: usize) -> Option<u32> {
	let value = read_u32(bytes, offset);
	(value != NULL_U32).then_some(value)
}

/// Rejects non-zero bytes in one reserved page region.
fn validate_zero(bytes: &[u8], page_no: PageNo, field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_page(page_no.get(), format!("{field} are non-zero")));
	}
	Ok(())
}

/// Creates a typed invalid-page status.
fn invalid_page(page_no: u64, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(page_no, reason)
}

/// Writes one little-endian `u16`.
fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + size_of::<u16>()].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u32`.
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u64`.
fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `f64` bit pattern.
fn write_f64(bytes: &mut [u8], offset: usize, value: f64) {
	write_u64(bytes, offset, value.to_bits());
}

/// Reads one little-endian `u16`.
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0_u8; size_of::<u16>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u16>()]);
	u16::from_le_bytes(value)
}

/// Reads one little-endian `u32`.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0_u8; size_of::<u32>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64`.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0_u8; size_of::<u64>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(value)
}

/// Reads one little-endian `f64` bit pattern.
fn read_f64(bytes: &[u8], offset: usize) -> f64 {
	f64::from_bits(read_u64(bytes, offset))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::{PropertyDictionary, Value, graph::AtomKind, storage::DiskAtomRef};

	/// Representative node fields occupy their normative offsets and
	/// round-trip.
	#[test]
	fn node_hot_slot_round_trips_normative_layout() -> FerrosGrynnResult<()> {
		let edge = DiskAtomRef::from_parts(AtomKind::Edge, 17)?;
		let value = NodeHotSlot {
			generation: 9,
			subgraph_slot: None,
			label_id: Some(3),
			table_id: Some(7),
			out_count: 1,
			in_count: 0,
			undirected_count: 0,
			flags: NodeSlotFlags::IN_RESULT,
			out_overflow: None,
			in_overflow: None,
			undirected_overflow: None,
			properties: Some(PropertyLocation::Inline {
				page_no: PageNo::new(5),
				slot: 2,
				length: 19,
			}),
			out_inline: [Some(edge), None, None, None],
			in_inline: [None; 4],
			undirected_inline: [None; 2],
		};
		let mut page = new_node_hot_page(PageNo::new(2), PageLsn::ZERO)?;
		write_node_hot_slot(&mut page, 4, &value)?;
		page.seal()?;
		let offset = PAGE_HEADER_SIZE + 4 * HOT_SLOT_BYTES;
		assert_eq!(&page.as_bytes()[offset..offset + 4], &9_u32.to_le_bytes());
		assert_eq!(read_node_hot_slot(&page, PageNo::new(2), 4)?, value);
		assert!(
			read_node_hot_slot(&page, PageNo::new(2), 5)?.flags.contains(NodeSlotFlags::TOMBSTONE)
		);
		Ok(())
	}

	/// Representative edge fields and NaN weight sentinel round-trip.
	#[test]
	fn edge_hot_slot_round_trips_and_rejects_reserved_flags() -> FerrosGrynnResult<()> {
		let node = DiskAtomRef::from_parts(AtomKind::Node, 1)?;
		let other = DiskAtomRef::from_parts(AtomKind::Node, 2)?;
		let value = EdgeHotSlot {
			topology: EdgeTopologyCode::Directed,
			flags: EdgeSlotFlags::empty(),
			has_transition: false,
			generation: 4,
			source_count: 1,
			target_count: 1,
			label_id: None,
			table_id: Some(8),
			weight: None,
			transition_graph: None,
			source_graph: None,
			target_graph: None,
			properties: None,
			source_overflow: None,
			target_overflow: None,
			source_graph_overflow: None,
			target_graph_overflow: None,
			source_inline: [Some(node), None],
			target_inline: [Some(other), None],
		};
		let mut page = new_edge_hot_page(PageNo::new(0), PageLsn::ZERO)?;
		write_edge_hot_slot(&mut page, 0, &value)?;
		page.seal()?;
		assert_eq!(read_edge_hot_slot(&page, PageNo::new(0), 0)?, value);

		page.as_bytes_mut()[PAGE_HEADER_SIZE + EDGE_FLAGS_OFFSET] = 0x80;
		page.seal()?;
		assert!(read_edge_hot_slot(&page, PageNo::new(0), 0).is_err());
		Ok(())
	}

	/// Slotted property records reuse the canonical StorageValue codec and its
	/// allocation limits.
	#[test]
	fn property_page_round_trips_canonical_map() -> FerrosGrynnResult<()> {
		let mut dictionary = PropertyDictionary::new();
		let name = dictionary.intern("name")?;
		let age = dictionary.intern("age")?;
		let mut properties = PropertyMap::new();
		properties.insert(name, Value::from("Ada"));
		properties.insert(age, Value::from(37_i64));
		let mut context = EncodeContext::new(dictionary);
		let record = encode_property_map(&properties, &mut context)?;
		let mut builder = PropertyPageBuilder::new(PageNo::new(3), PageLsn::ZERO);
		let slot = builder
			.try_insert(&record)?
			.ok_or_else(|| invalid_page(3, "small test property did not fit"))?;
		let page = builder.finish()?;
		let stored = read_property_record(
			&page,
			PageNo::new(3),
			slot,
			u32::try_from(record.len()).map_err(|_error| invalid_page(3, "record too long"))?,
		)?;
		let decoded = decode_property_map(stored, &context.decoder(), DecodeLimits::default())?;
		assert_eq!(decoded, properties);
		Ok(())
	}

	/// Plain and qualified overflow pages preserve order and exact links.
	#[test]
	fn adjacency_overflow_codecs_preserve_order() -> FerrosGrynnResult<()> {
		let first = DiskAtomRef::from_parts(AtomKind::Edge, 3)?;
		let second = DiskAtomRef::from_parts(AtomKind::Edge, 4)?;
		let page =
			encode_adjacency_overflow(PageNo::new(7), PageLsn::ZERO, Some(PageNo::new(8)), &[
				first, second,
			])?;
		assert_eq!(
			decode_adjacency_overflow(&page, PageNo::new(7))?,
			(Some(PageNo::new(8)), vec![first, second])
		);

		let qualified = QualifiedDiskAtomRef {
			graph_slot: 11,
			side: EndpointSide::Target,
			atom: second,
		};
		let page = encode_cross_level_overflow(PageNo::new(9), PageLsn::ZERO, None, &[qualified])?;
		assert_eq!(decode_cross_level_overflow(&page, PageNo::new(9))?, (None, vec![qualified]));
		Ok(())
	}

	/// Large records are bounded before allocation and require an exact chain.
	#[test]
	fn large_property_chain_round_trips_exact_length() -> FerrosGrynnResult<()> {
		let record = vec![0x5a; LARGE_PROPERTY_CHUNK_BYTES + 17];
		let pages = encode_large_property(PageNo::new(20), PageLsn::ZERO, &record)?;
		let expected_len = u32::try_from(record.len())
			.map_err(|_error| invalid_page(20, "large test record exceeds u32"))?;
		assert_eq!(
			decode_large_property(PageNo::new(20), &pages, expected_len, record.len())?,
			record
		);
		assert!(
			decode_large_property(PageNo::new(20), &pages, expected_len, record.len() - 1).is_err()
		);
		Ok(())
	}

	/// Freelist and empty subgraph pages retain their typed headers.
	#[test]
	fn empty_allocator_and_subgraph_pages_are_valid() -> FerrosGrynnResult<()> {
		let freelist = encode_freelist(PageNo::new(0), PageLsn::ZERO, 0, None, &[])?;
		let (first, next, bitmap) = decode_freelist(&freelist, PageNo::new(0))?;
		assert_eq!((first, next), (0, None));
		assert!(bitmap.iter().all(|byte| *byte == 0));

		let subgraphs = encode_subgraph_directory(PageNo::new(0), PageLsn::ZERO, None, &[])?;
		assert_eq!(decode_subgraph_directory(&subgraphs, PageNo::new(0))?, (None, Vec::new()));
		Ok(())
	}

	/// Minimal count statistics retain exact integer and finite degree fields.
	#[test]
	fn graph_statistics_round_trip_without_histogram() -> FerrosGrynnResult<()> {
		let expected = GraphStatistics {
			node_count: 4,
			edge_count: 3,
			average_out_degree: 0.75,
			average_in_degree: 0.75,
			maximum_out_degree: 2,
			maximum_in_degree: 2,
		};
		let page = encode_graph_statistics(expected, PageLsn::ZERO)?;
		assert_eq!(decode_graph_statistics(&page)?, expected);
		Ok(())
	}

	/// Compile-time layout arithmetic matches the normative page table.
	#[test]
	fn format_constants_cover_exact_page_boundaries() {
		assert_eq!(HOT_RESERVED_OFFSET, 16_352);
		assert!(NodeSlotFlags::from_bits(0x10).is_none());
		assert_eq!(OVERFLOW_REFS_PER_PAGE, 2_042);
		assert_eq!(CROSS_LEVEL_REFS_PER_PAGE, 1_021);
		assert_eq!(SUBGRAPHS_PER_PAGE, 510);
		assert_eq!(FREELIST_PAGES_COVERED, 130_688);
	}
}
