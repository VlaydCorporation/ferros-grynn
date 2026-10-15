//! Exact format-v1 codecs for graph-local occurrence files.

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::storage::DiskAtomRef,
};

/// Persistent graph incidence and adjacency records.
mod adjacency;
/// Hot-page encoding and decoding.
mod hot;
/// Typed hot-page record representations.
mod hot_types;
/// Validation of hot-page records and references.
mod hot_validation;
mod image;
/// Graph-local property overlays.
mod overlay;
mod statistics;
/// Persistent nested-graph references.
mod subgraphs;

pub(super) use adjacency::{
	EndpointSide,
	QualifiedDiskAtomRef,
	decode_adjacency_overflow,
	decode_cross_level_overflow,
	encode_adjacency_overflow,
	encode_cross_level_overflow,
};
pub(super) use hot::{
	hot_slot_address,
	initialize_edge_hot_page,
	initialize_node_hot_page,
	read_edge_hot_slot,
	read_node_hot_slot,
	write_edge_hot_slot,
	write_node_hot_slot,
};
pub(super) use hot_types::{
	EdgeHotSlot,
	EdgeSlotFlags,
	EdgeTopologyCode,
	NodeHotSlot,
	NodeSlotFlags,
};
pub(super) use image::{
	BuiltGraphImage,
	GraphArenaBindings,
	build_graph_image,
	build_import_graph_image,
	open_graph_image,
};
pub(super) use overlay::{GraphOverlayStore, decode_overlay, encode_overlay, read_overlay_record};
pub(super) use statistics::{
	StatisticsLayout,
	decode_class_statistics,
	decode_class_statistics_range,
	decode_degree_statistics,
	decode_label_statistics,
	decode_property_statistics,
	decode_property_statistics_range,
	decode_statistics_root,
	encode_class_statistics,
	encode_property_statistics,
	encode_statistics,
};
pub(super) use subgraphs::{SubgraphEntry, decode_subgraph_directory, encode_subgraph_directory};

/// Configured hot slot bytes value.
pub(super) const HOT_SLOT_BYTES: usize = 160;
/// Configured hot slots per page value.
pub(super) const HOT_SLOTS_PER_PAGE: usize = 102;
/// Configured null u32 value.
pub(super) const NULL_U32: u32 = u32::MAX;
/// Byte offset of hot reserved in its encoded layout.
const HOT_RESERVED_OFFSET: usize = PAGE_HEADER_SIZE + HOT_SLOTS_PER_PAGE * HOT_SLOT_BYTES;

/// Byte offset of node generation in its encoded layout.
const NODE_GENERATION_OFFSET: usize = 0;
/// Byte offset of node subgraph in its encoded layout.
const NODE_SUBGRAPH_OFFSET: usize = 4;
/// Byte offset of node label in its encoded layout.
const NODE_LABEL_OFFSET: usize = 8;
/// Byte offset of node reserved in its encoded layout.
const NODE_RESERVED_OFFSET: usize = 12;
/// Byte offset of node out count in its encoded layout.
const NODE_OUT_COUNT_OFFSET: usize = 16;
/// Byte offset of node in count in its encoded layout.
const NODE_IN_COUNT_OFFSET: usize = 20;
/// Byte offset of node undirected count in its encoded layout.
const NODE_UNDIRECTED_COUNT_OFFSET: usize = 24;
/// Byte offset of node flags in its encoded layout.
const NODE_FLAGS_OFFSET: usize = 28;
/// Byte offset of node padding in its encoded layout.
const NODE_PADDING_OFFSET: usize = 29;
/// Byte offset of node out overflow in its encoded layout.
const NODE_OUT_OVERFLOW_OFFSET: usize = 32;
/// Byte offset of node in overflow in its encoded layout.
const NODE_IN_OVERFLOW_OFFSET: usize = 40;
/// Byte offset of node undirected overflow in its encoded layout.
const NODE_UNDIRECTED_OVERFLOW_OFFSET: usize = 48;
/// Byte offset of node overlay page in its encoded layout.
const NODE_OVERLAY_PAGE_OFFSET: usize = 56;
/// Byte offset of node overlay slot in its encoded layout.
const NODE_OVERLAY_SLOT_OFFSET: usize = 64;
/// Byte offset of node overlay len in its encoded layout.
const NODE_OVERLAY_LEN_OFFSET: usize = 68;
/// Byte offset of node global atom in its encoded layout.
const NODE_GLOBAL_ATOM_OFFSET: usize = 72;
/// Byte offset of node out inline in its encoded layout.
const NODE_OUT_INLINE_OFFSET: usize = 80;
/// Byte offset of node in inline in its encoded layout.
const NODE_IN_INLINE_OFFSET: usize = 112;
/// Byte offset of node undirected inline in its encoded layout.
const NODE_UNDIRECTED_INLINE_OFFSET: usize = 144;

/// Byte offset of edge topology in its encoded layout.
const EDGE_TOPOLOGY_OFFSET: usize = 0;
/// Byte offset of edge flags in its encoded layout.
const EDGE_FLAGS_OFFSET: usize = 1;
/// Byte offset of edge kind ext in its encoded layout.
const EDGE_KIND_EXT_OFFSET: usize = 2;
/// Byte offset of edge padding a in its encoded layout.
const EDGE_PADDING_A_OFFSET: usize = 3;
/// Byte offset of edge generation in its encoded layout.
const EDGE_GENERATION_OFFSET: usize = 4;
/// Byte offset of edge source count in its encoded layout.
const EDGE_SOURCE_COUNT_OFFSET: usize = 8;
/// Byte offset of edge target count in its encoded layout.
const EDGE_TARGET_COUNT_OFFSET: usize = 12;
/// Byte offset of edge label in its encoded layout.
const EDGE_LABEL_OFFSET: usize = 16;
/// Byte offset of edge reserved a in its encoded layout.
const EDGE_RESERVED_A_OFFSET: usize = 20;
/// Byte offset of edge weight in its encoded layout.
const EDGE_WEIGHT_OFFSET: usize = 24;
/// Byte offset of edge transition graph in its encoded layout.
const EDGE_TRANSITION_GRAPH_OFFSET: usize = 32;
/// Byte offset of edge source graph in its encoded layout.
const EDGE_SOURCE_GRAPH_OFFSET: usize = 36;
/// Byte offset of edge target graph in its encoded layout.
const EDGE_TARGET_GRAPH_OFFSET: usize = 40;
/// Byte offset of edge overlay slot in its encoded layout.
const EDGE_OVERLAY_SLOT_OFFSET: usize = 44;
/// Byte offset of edge overlay len in its encoded layout.
const EDGE_OVERLAY_LEN_OFFSET: usize = 48;
/// Byte offset of edge padding b in its encoded layout.
const EDGE_PADDING_B_OFFSET: usize = 52;
/// Byte offset of edge overlay page in its encoded layout.
const EDGE_OVERLAY_PAGE_OFFSET: usize = 56;
/// Byte offset of edge source overflow in its encoded layout.
const EDGE_SOURCE_OVERFLOW_OFFSET: usize = 64;
/// Byte offset of edge target overflow in its encoded layout.
const EDGE_TARGET_OVERFLOW_OFFSET: usize = 72;
/// Byte offset of edge global atom in its encoded layout.
const EDGE_GLOBAL_ATOM_OFFSET: usize = 80;
/// Byte offset of edge source graph overflow in its encoded layout.
const EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET: usize = 88;
/// Byte offset of edge target graph overflow in its encoded layout.
const EDGE_TARGET_GRAPH_OVERFLOW_OFFSET: usize = 96;
/// Byte offset of edge reserved b in its encoded layout.
const EDGE_RESERVED_B_OFFSET: usize = 104;
/// Byte offset of edge source inline in its encoded layout.
const EDGE_SOURCE_INLINE_OFFSET: usize = 112;
/// Byte offset of edge target inline in its encoded layout.
const EDGE_TARGET_INLINE_OFFSET: usize = 128;
/// Byte offset of edge reserved c in its encoded layout.
const EDGE_RESERVED_C_OFFSET: usize = 144;

/// Byte offset of overflow next page in its encoded layout.
const OVERFLOW_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of overflow count in its encoded layout.
const OVERFLOW_COUNT_OFFSET: usize = OVERFLOW_NEXT_PAGE_OFFSET + 8;
/// Byte offset of overflow reserved in its encoded layout.
const OVERFLOW_RESERVED_OFFSET: usize = OVERFLOW_COUNT_OFFSET + 4;
/// Byte offset of overflow entries in its encoded layout.
const OVERFLOW_ENTRIES_OFFSET: usize = OVERFLOW_RESERVED_OFFSET + 4;
/// Configured overflow refs per page value.
const OVERFLOW_REFS_PER_PAGE: usize = (PAGE_SIZE - OVERFLOW_ENTRIES_OFFSET) / 8;
/// Configured cross level refs per page value.
const CROSS_LEVEL_REFS_PER_PAGE: usize =
	(PAGE_SIZE - OVERFLOW_ENTRIES_OFFSET) / QualifiedDiskAtomRef::ENCODED_BYTES;

/// Byte offset of subgraph next page in its encoded layout.
const SUBGRAPH_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of subgraph entry count in its encoded layout.
const SUBGRAPH_ENTRY_COUNT_OFFSET: usize = SUBGRAPH_NEXT_PAGE_OFFSET + 8;
/// Byte offset of subgraph live count in its encoded layout.
const SUBGRAPH_LIVE_COUNT_OFFSET: usize = SUBGRAPH_ENTRY_COUNT_OFFSET + 4;
/// Byte offset of subgraph reserved in its encoded layout.
const SUBGRAPH_RESERVED_OFFSET: usize = SUBGRAPH_LIVE_COUNT_OFFSET + 4;
/// Byte offset of subgraph entries in its encoded layout.
const SUBGRAPH_ENTRIES_OFFSET: usize = SUBGRAPH_RESERVED_OFFSET + 16;
/// Configured subgraph entry bytes value.
const SUBGRAPH_ENTRY_BYTES: usize = 32;
/// Configured subgraphs per page value.
const SUBGRAPHS_PER_PAGE: usize = (PAGE_SIZE - SUBGRAPH_ENTRIES_OFFSET) / SUBGRAPH_ENTRY_BYTES;

/// Validates overflow header.
fn validate_overflow_header(
	page: &AlignedPage,
	page_no: PageNo,
	expected: PageType,
) -> FerrosGrynnResult<()> {
	let header = page.validate(page_no)?;
	if header.page_type() != expected || header.flags() != PageFlags::OVERFLOW {
		return Err(invalid_page(page_no.get(), "invalid overflow page header"));
	}
	Ok(())
}

/// Reads required disk refs.
fn read_required_disk_refs(
	bytes: &[u8],
	offset: usize,
	count: usize,
) -> FerrosGrynnResult<Vec<DiskAtomRef>> {
	let mut result = Vec::with_capacity(count);
	for index in 0..count {
		let raw = read_u64(bytes, offset + index * 8);
		if raw == u64::MAX {
			return Err(invalid_page(0, "live adjacency contains a null atom reference"));
		}
		result.push(DiskAtomRef::from_raw(raw)?);
	}
	Ok(result)
}

/// Writes optional page.
fn write_optional_page(bytes: &mut [u8], offset: usize, value: Option<PageNo>) {
	write_u64(bytes, offset, value.unwrap_or(PageNo::NULL).get());
}

/// Reads optional page.
fn read_optional_page(bytes: &[u8], offset: usize) -> Option<PageNo> {
	let page = PageNo::new(read_u64(bytes, offset));
	(page != PageNo::NULL).then_some(page)
}

/// Validates zero.
fn validate_zero(bytes: &[u8], page_no: PageNo, field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_page(page_no.get(), format!("{field} are non-zero")));
	}
	Ok(())
}

/// Returns the invalid page result.
fn invalid_page(page_no: u64, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(page_no, reason)
}

/// Writes u32.
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes u64.
fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Reads u16.
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0; 2];
	value.copy_from_slice(&bytes[offset..offset + 2]);
	u16::from_le_bytes(value)
}

/// Reads u32.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

/// Reads u64.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

#[cfg(test)]
/// Regression tests for graph pages behavior.
mod tests {
	use super::*;

	/// Verifies fixed layouts end at their normative boundaries.
	#[test]
	fn fixed_layouts_end_at_their_normative_boundaries() {
		assert_eq!(HOT_RESERVED_OFFSET, 16_352);
		assert_eq!(OVERFLOW_REFS_PER_PAGE, 2_042);
		assert_eq!(CROSS_LEVEL_REFS_PER_PAGE, 1_021);
		assert_eq!(SUBGRAPHS_PER_PAGE, 510);
	}
}
