//! Exact format-v1 codecs for graph-local occurrence files.

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::storage::DiskAtomRef,
};

mod adjacency;
mod freelist;
mod hot;
mod hot_types;
mod hot_validation;
mod image;
mod overlay;
mod statistics;
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
	EncodedStatisticsFiles,
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
	validate_statistics_file_lengths,
};
pub(super) use subgraphs::{SubgraphEntry, decode_subgraph_directory, encode_subgraph_directory};

pub(super) const HOT_SLOT_BYTES: usize = 160;
pub(super) const HOT_SLOTS_PER_PAGE: usize = 102;
pub(super) const NULL_U32: u32 = u32::MAX;
const HOT_RESERVED_OFFSET: usize = PAGE_HEADER_SIZE + HOT_SLOTS_PER_PAGE * HOT_SLOT_BYTES;

const NODE_GENERATION_OFFSET: usize = 0;
const NODE_SUBGRAPH_OFFSET: usize = 4;
const NODE_LABEL_OFFSET: usize = 8;
const NODE_RESERVED_OFFSET: usize = 12;
const NODE_OUT_COUNT_OFFSET: usize = 16;
const NODE_IN_COUNT_OFFSET: usize = 20;
const NODE_UNDIRECTED_COUNT_OFFSET: usize = 24;
const NODE_FLAGS_OFFSET: usize = 28;
const NODE_PADDING_OFFSET: usize = 29;
const NODE_OUT_OVERFLOW_OFFSET: usize = 32;
const NODE_IN_OVERFLOW_OFFSET: usize = 40;
const NODE_UNDIRECTED_OVERFLOW_OFFSET: usize = 48;
const NODE_OVERLAY_PAGE_OFFSET: usize = 56;
const NODE_OVERLAY_SLOT_OFFSET: usize = 64;
const NODE_OVERLAY_LEN_OFFSET: usize = 68;
const NODE_GLOBAL_ATOM_OFFSET: usize = 72;
const NODE_OUT_INLINE_OFFSET: usize = 80;
const NODE_IN_INLINE_OFFSET: usize = 112;
const NODE_UNDIRECTED_INLINE_OFFSET: usize = 144;

const EDGE_TOPOLOGY_OFFSET: usize = 0;
const EDGE_FLAGS_OFFSET: usize = 1;
const EDGE_KIND_EXT_OFFSET: usize = 2;
const EDGE_PADDING_A_OFFSET: usize = 3;
const EDGE_GENERATION_OFFSET: usize = 4;
const EDGE_SOURCE_COUNT_OFFSET: usize = 8;
const EDGE_TARGET_COUNT_OFFSET: usize = 12;
const EDGE_LABEL_OFFSET: usize = 16;
const EDGE_RESERVED_A_OFFSET: usize = 20;
const EDGE_WEIGHT_OFFSET: usize = 24;
const EDGE_TRANSITION_GRAPH_OFFSET: usize = 32;
const EDGE_SOURCE_GRAPH_OFFSET: usize = 36;
const EDGE_TARGET_GRAPH_OFFSET: usize = 40;
const EDGE_OVERLAY_SLOT_OFFSET: usize = 44;
const EDGE_OVERLAY_LEN_OFFSET: usize = 48;
const EDGE_PADDING_B_OFFSET: usize = 52;
const EDGE_OVERLAY_PAGE_OFFSET: usize = 56;
const EDGE_SOURCE_OVERFLOW_OFFSET: usize = 64;
const EDGE_TARGET_OVERFLOW_OFFSET: usize = 72;
const EDGE_GLOBAL_ATOM_OFFSET: usize = 80;
const EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET: usize = 88;
const EDGE_TARGET_GRAPH_OVERFLOW_OFFSET: usize = 96;
const EDGE_RESERVED_B_OFFSET: usize = 104;
const EDGE_SOURCE_INLINE_OFFSET: usize = 112;
const EDGE_TARGET_INLINE_OFFSET: usize = 128;
const EDGE_RESERVED_C_OFFSET: usize = 144;

const OVERFLOW_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const OVERFLOW_COUNT_OFFSET: usize = OVERFLOW_NEXT_PAGE_OFFSET + 8;
const OVERFLOW_RESERVED_OFFSET: usize = OVERFLOW_COUNT_OFFSET + 4;
const OVERFLOW_ENTRIES_OFFSET: usize = OVERFLOW_RESERVED_OFFSET + 4;
const OVERFLOW_REFS_PER_PAGE: usize = (PAGE_SIZE - OVERFLOW_ENTRIES_OFFSET) / 8;
const CROSS_LEVEL_REFS_PER_PAGE: usize =
	(PAGE_SIZE - OVERFLOW_ENTRIES_OFFSET) / QualifiedDiskAtomRef::ENCODED_BYTES;

const SUBGRAPH_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const SUBGRAPH_ENTRY_COUNT_OFFSET: usize = SUBGRAPH_NEXT_PAGE_OFFSET + 8;
const SUBGRAPH_LIVE_COUNT_OFFSET: usize = SUBGRAPH_ENTRY_COUNT_OFFSET + 4;
const SUBGRAPH_RESERVED_OFFSET: usize = SUBGRAPH_LIVE_COUNT_OFFSET + 4;
const SUBGRAPH_ENTRIES_OFFSET: usize = SUBGRAPH_RESERVED_OFFSET + 16;
const SUBGRAPH_ENTRY_BYTES: usize = 32;
const SUBGRAPHS_PER_PAGE: usize = (PAGE_SIZE - SUBGRAPH_ENTRIES_OFFSET) / SUBGRAPH_ENTRY_BYTES;

const FREELIST_FIRST_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
const FREELIST_NEXT_PAGE_OFFSET: usize = FREELIST_FIRST_PAGE_OFFSET + 8;
const FREELIST_BITMAP_OFFSET: usize = FREELIST_NEXT_PAGE_OFFSET + 8;

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

fn write_optional_page(bytes: &mut [u8], offset: usize, value: Option<PageNo>) {
	write_u64(bytes, offset, value.unwrap_or(PageNo::NULL).get());
}

fn read_optional_page(bytes: &[u8], offset: usize) -> Option<PageNo> {
	let page = PageNo::new(read_u64(bytes, offset));
	(page != PageNo::NULL).then_some(page)
}

fn validate_zero(bytes: &[u8], page_no: PageNo, field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid_page(page_no.get(), format!("{field} are non-zero")));
	}
	Ok(())
}

fn invalid_page(page_no: u64, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(page_no, reason)
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0; 2];
	value.copy_from_slice(&bytes[offset..offset + 2]);
	u16::from_le_bytes(value)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fixed_layouts_end_at_their_normative_boundaries() {
		assert_eq!(HOT_RESERVED_OFFSET, 16_352);
		assert_eq!(OVERFLOW_REFS_PER_PAGE, 2_042);
		assert_eq!(CROSS_LEVEL_REFS_PER_PAGE, 1_021);
		assert_eq!(SUBGRAPHS_PER_PAGE, 510);
	}
}
