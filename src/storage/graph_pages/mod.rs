//! Format-v1 codecs for the graph pages used by the first local vertical slice.

use bitflags::bitflags;

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	stats::{
		DegreeBucket,
		DegreeDirection,
		DegreeHistogram,
		DegreeHistogramKey,
		GraphStatistics,
		LabelStatistic,
		LabelStatisticKey,
		PropertyBucket,
		PropertyHistogram,
		PropertyHistogramKey,
	},
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

/// Adjacency and cross-level overflow codecs.
mod adjacency;
/// Allocation bitmap codec.
mod freelist;
/// Fixed node and edge hot slots.
mod hot;
/// Canonical property page codecs.
mod props;
/// Planner statistics page codec.
mod statistics;
/// Nested-graph directory codec.
mod subgraphs;

pub(super) use adjacency::{
	EndpointSide,
	QualifiedDiskAtomRef,
	decode_adjacency_overflow,
	decode_cross_level_overflow,
	encode_adjacency_overflow,
	encode_cross_level_overflow,
};
pub(super) use freelist::{decode_freelist, encode_freelist};
pub(super) use hot::{
	EdgeHotSlot,
	NodeHotSlot,
	hot_slot_address,
	new_edge_hot_page,
	new_node_hot_page,
	read_edge_hot_slot,
	read_node_hot_slot,
	write_edge_hot_slot,
	write_node_hot_slot,
};
pub(super) use props::{
	PropertyPageBuilder,
	decode_large_property,
	decode_property_map,
	encode_large_property,
	encode_property_map,
	read_property_record,
};
pub(super) use statistics::{
	StatisticsLayout,
	decode_degree_statistics,
	decode_label_statistics_file,
	decode_property_statistics,
	decode_statistics_summary,
	encode_statistics,
	encode_statistics_delta,
	statistics_bulk_layout_matches,
	statistics_layout,
	validate_statistics_file_lengths,
};
#[cfg(test)]
use statistics::{decode_graph_statistics, encode_graph_statistics};
pub(super) use subgraphs::{SubgraphEntry, decode_subgraph_directory, encode_subgraph_directory};

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
const STATS_LABEL_FIRST_OFFSET: usize = STATS_DEGREE_HISTOGRAM_OFFSET + size_of::<u64>();
const STATS_PROPERTY_FIRST_OFFSET: usize = STATS_LABEL_FIRST_OFFSET + size_of::<u64>();
const STATS_LABEL_PAGE_COUNT_OFFSET: usize = STATS_PROPERTY_FIRST_OFFSET + size_of::<u64>();
const STATS_DEGREE_PAGE_COUNT_OFFSET: usize = STATS_LABEL_PAGE_COUNT_OFFSET + size_of::<u32>();
const STATS_PROPERTY_PAGE_COUNT_OFFSET: usize = STATS_DEGREE_PAGE_COUNT_OFFSET + size_of::<u32>();
const STATS_LAYOUT_VERSION_OFFSET: usize = STATS_PROPERTY_PAGE_COUNT_OFFSET + size_of::<u32>();
const STATS_ANALYZED_REVISION_OFFSET: usize = STATS_LAYOUT_VERSION_OFFSET + size_of::<u32>();
const STATS_DIRTY_ROWS_OFFSET: usize = STATS_ANALYZED_REVISION_OFFSET + size_of::<u64>();
const STATS_TOTAL_OUT_OFFSET: usize = STATS_DIRTY_ROWS_OFFSET + size_of::<u64>();
const STATS_TOTAL_IN_OFFSET: usize = STATS_TOTAL_OUT_OFFSET + size_of::<u64>();
const STATS_TOTAL_UNDIRECTED_OFFSET: usize = STATS_TOTAL_IN_OFFSET + size_of::<u64>();
const STATS_REVISION_OFFSET: usize = STATS_TOTAL_UNDIRECTED_OFFSET + size_of::<u64>();
const STATS_RESERVED_OFFSET: usize = STATS_REVISION_OFFSET + size_of::<u64>();

/// Per-label statistics fields.
const LABEL_STATS_NEXT_OFFSET: usize = PAGE_HEADER_SIZE;
const LABEL_STATS_COUNT_OFFSET: usize = LABEL_STATS_NEXT_OFFSET + size_of::<u64>();
const LABEL_STATS_PADDING_OFFSET: usize = LABEL_STATS_COUNT_OFFSET + size_of::<u32>();
const LABEL_STATS_ENTRIES_OFFSET: usize = LABEL_STATS_PADDING_OFFSET + size_of::<u32>();
const LABEL_STATS_ENTRY_BYTES: usize = size_of::<u32>() + size_of::<u64>() * 2;
const LABEL_STATS_PER_PAGE: usize =
	(PAGE_SIZE - LABEL_STATS_ENTRIES_OFFSET) / LABEL_STATS_ENTRY_BYTES;

/// Degree-histogram fields.
const DEGREE_STATS_LABEL_OFFSET: usize = PAGE_HEADER_SIZE;
const DEGREE_STATS_DIRECTION_OFFSET: usize = DEGREE_STATS_LABEL_OFFSET + size_of::<u32>();
const DEGREE_STATS_PADDING_OFFSET: usize = DEGREE_STATS_DIRECTION_OFFSET + size_of::<u8>();
const DEGREE_STATS_BUCKET_COUNT_OFFSET: usize = DEGREE_STATS_PADDING_OFFSET + size_of::<u8>() * 2;
const DEGREE_STATS_BUCKETS_OFFSET: usize = DEGREE_STATS_BUCKET_COUNT_OFFSET + size_of::<u16>();
const DEGREE_STATS_BUCKET_BYTES: usize = size_of::<u32>() * 2 + size_of::<u64>();

/// Property-histogram fields.
const PROPERTY_STATS_TABLE_OFFSET: usize = PAGE_HEADER_SIZE;
const PROPERTY_STATS_FIELD_OFFSET: usize = PROPERTY_STATS_TABLE_OFFSET + size_of::<u32>();
const PROPERTY_STATS_TYPE_OFFSET: usize = PROPERTY_STATS_FIELD_OFFSET + size_of::<u32>();
const PROPERTY_STATS_PADDING_OFFSET: usize = PROPERTY_STATS_TYPE_OFFSET + size_of::<u8>();
const PROPERTY_STATS_DISTINCT_OFFSET: usize = PROPERTY_STATS_PADDING_OFFSET + size_of::<u8>();
const PROPERTY_STATS_NULL_FRACTION_OFFSET: usize =
	PROPERTY_STATS_DISTINCT_OFFSET + size_of::<u64>();
const PROPERTY_STATS_BUCKET_COUNT_OFFSET: usize =
	PROPERTY_STATS_NULL_FRACTION_OFFSET + size_of::<f32>();
const PROPERTY_STATS_OFFSETS_OFFSET: usize = PROPERTY_STATS_BUCKET_COUNT_OFFSET + size_of::<u16>();

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
	use std::collections::BTreeMap;

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
			total_out_degree: 3,
			total_in_degree: 3,
			maximum_out_degree: 2,
			maximum_in_degree: 2,
			..GraphStatistics::default()
		};
		let page = encode_graph_statistics(&expected, PageLsn::ZERO)?;
		assert_eq!(decode_graph_statistics(&page)?, expected);
		Ok(())
	}

	/// A complete statistics image preserves exact counters and bulk
	/// distributions.
	#[test]
	fn statistics_file_round_trips_auxiliary_pages() -> FerrosGrynnResult<()> {
		let person = LabelStatisticKey {
			kind: AtomKind::Node,
			label: "Person".into(),
		};
		let retired = LabelStatisticKey {
			kind: AtomKind::Edge,
			label: "OLD".into(),
		};
		let expected = GraphStatistics {
			revision: 12,
			analyzed_revision: 10,
			dirty_rows: 2,
			node_count: 2,
			edge_count: 1,
			total_out_degree: 1,
			total_in_degree: 1,
			maximum_out_degree: 1,
			maximum_in_degree: 1,
			labels: BTreeMap::from([
				(person.clone(), LabelStatistic {
					atom_count: 2,
					property_count: 4,
				}),
				(retired, LabelStatistic::default()),
			]),
			degree_histograms: BTreeMap::from([(
				DegreeHistogramKey {
					label: None,
					direction: DegreeDirection::Out,
				},
				DegreeHistogram {
					buckets: vec![DegreeBucket {
						minimum: 0,
						maximum: 1,
						node_count: 2,
					}],
				},
			)]),
			property_histograms: BTreeMap::from([(
				PropertyHistogramKey {
					table_id: 7,
					field_id: 9,
				},
				PropertyHistogram {
					type_tag: TypeTag::Int.byte(),
					distinct_count: 2,
					null_fraction: 0.25,
					buckets: vec![PropertyBucket {
						lower: vec![1],
						upper: vec![9],
						frequency: 3,
						distinct_count: 2,
					}],
				},
			)]),
			..GraphStatistics::default()
		};
		let node_labels = vec!["Person".to_owned()];
		let edge_labels = vec!["OLD".to_owned()];
		let mut files = encode_statistics(&expected, &node_labels, &edge_labels, PageLsn::ZERO)?;
		assert!(files.graph.len() > 1);
		assert!(!files.labels.is_empty());
		assert!(!files.properties.is_empty());
		let graph_pages = u64::try_from(files.graph.len())
			.map_err(|_error| invalid_page(0, "test graph page count exceeds u64"))?;
		let label_pages = u64::try_from(files.labels.len())
			.map_err(|_error| invalid_page(0, "test label page count exceeds u64"))?;
		let property_pages = u64::try_from(files.properties.len())
			.map_err(|_error| invalid_page(0, "test property page count exceeds u64"))?;
		let (mut decoded, layout) =
			decode_statistics_summary(&files.graph[0], graph_pages, label_pages, property_pages)?;
		decoded.degree_histograms = decode_degree_statistics(
			|page_no, output| {
				let index = usize::try_from(page_no.get()).map_err(|_error| {
					invalid_page(page_no.get(), "test page number exceeds usize")
				})?;
				output.as_bytes_mut().copy_from_slice(files.graph[index].as_bytes());
				Ok(())
			},
			layout,
			&node_labels,
			&edge_labels,
		)?;
		decoded.labels = decode_label_statistics_file(
			|page_no, output| {
				let index = usize::try_from(page_no.get()).map_err(|_error| {
					invalid_page(page_no.get(), "test page number exceeds usize")
				})?;
				output.as_bytes_mut().copy_from_slice(files.labels[index].as_bytes());
				Ok(())
			},
			layout,
			&node_labels,
			&edge_labels,
		)?;
		decoded.property_histograms = decode_property_statistics(
			|page_no, output| {
				let index = usize::try_from(page_no.get()).map_err(|_error| {
					invalid_page(page_no.get(), "test page number exceeds usize")
				})?;
				output.as_bytes_mut().copy_from_slice(files.properties[index].as_bytes());
				Ok(())
			},
			layout,
		)?;
		assert_eq!(decoded, expected);

		let label_pages = read_u32(files.graph[0].as_bytes(), STATS_LABEL_PAGE_COUNT_OFFSET);
		write_u32(
			files.graph[0].as_bytes_mut(),
			STATS_LABEL_PAGE_COUNT_OFFSET,
			label_pages.checked_add(1).ok_or_else(|| invalid_page(0, "test count overflows"))?,
		);
		files.graph[0].seal()?;
		assert!(
			decode_statistics_summary(
				&files.graph[0],
				graph_pages,
				u64::from(label_pages),
				property_pages,
			)
			.is_err()
		);
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
