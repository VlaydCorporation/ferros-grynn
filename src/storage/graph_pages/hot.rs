use super::hot_validation::{validate_edge_slot, validate_node_slot};
#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;
use crate::{datastore::GlobalAtomId, storage::atom_page::PropertyRef};

const ABSENT_WEIGHT_BITS: u64 = 0x7ff8_0000_0000_0000;

/// Returns the page and within-page slot for a stable disk slot index.
pub(in crate::storage) fn hot_slot_address(slot: u64) -> FerrosGrynnResult<(PageNo, usize)> {
	let slots_per_page = u64::try_from(HOT_SLOTS_PER_PAGE)
		.map_err(|_error| invalid_page(0, "hot slots per page do not fit u64"))?;
	let page_no = PageNo::new(slot / slots_per_page);
	let within = usize::try_from(slot % slots_per_page)
		.map_err(|_error| invalid_page(page_no.get(), "hot slot index does not fit usize"))?;
	Ok((page_no, within))
}

/// Initializes one node-hot page in its final aligned storage.
///
/// # Errors
///
/// Returns an error when the tombstone slot layout or final checksum cannot be
/// encoded.
pub(in crate::storage) fn initialize_node_hot_page(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	page.reset(PageType::NodeHot, PageFlags::empty(), page_lsn, page_no);
	for slot in 0..HOT_SLOTS_PER_PAGE {
		write_node_hot_slot(page, slot, &NodeHotSlot::tombstone(0))?;
	}
	page.seal()?;
	Ok(())
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
	write_u32(bytes, NODE_RESERVED_OFFSET, 0);
	write_u32(bytes, NODE_OUT_COUNT_OFFSET, value.out_count);
	write_u32(bytes, NODE_IN_COUNT_OFFSET, value.in_count);
	write_u32(bytes, NODE_UNDIRECTED_COUNT_OFFSET, value.undirected_count);
	bytes[NODE_FLAGS_OFFSET] = value.flags.bits();
	write_optional_page(bytes, NODE_OUT_OVERFLOW_OFFSET, value.out_overflow);
	write_optional_page(bytes, NODE_IN_OVERFLOW_OFFSET, value.in_overflow);
	write_optional_page(bytes, NODE_UNDIRECTED_OVERFLOW_OFFSET, value.undirected_overflow);
	write_record_ref(
		bytes,
		NODE_OVERLAY_PAGE_OFFSET,
		NODE_OVERLAY_SLOT_OFFSET,
		NODE_OVERLAY_LEN_OFFSET,
		value.overlay,
	);
	write_u64(
		bytes,
		NODE_GLOBAL_ATOM_OFFSET,
		value.global_atom.map_or(u64::MAX, GlobalAtomId::raw),
	);
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
	validate_zero(&page.as_bytes()[HOT_RESERVED_OFFSET..], page_no, "NodeHot tail")?;
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
	if read_u32(bytes, NODE_RESERVED_OFFSET) != 0 {
		return Err(invalid_page(page_no.get(), "node reserved field is non-zero"));
	}
	let raw_global = read_u64(bytes, NODE_GLOBAL_ATOM_OFFSET);
	let value = NodeHotSlot {
		generation: read_u32(bytes, NODE_GENERATION_OFFSET),
		subgraph_slot: read_optional_u32(bytes, NODE_SUBGRAPH_OFFSET),
		label_id: read_optional_u32(bytes, NODE_LABEL_OFFSET),
		out_count: read_u32(bytes, NODE_OUT_COUNT_OFFSET),
		in_count: read_u32(bytes, NODE_IN_COUNT_OFFSET),
		undirected_count: read_u32(bytes, NODE_UNDIRECTED_COUNT_OFFSET),
		flags: NodeSlotFlags::from_bits(bytes[NODE_FLAGS_OFFSET])
			.ok_or_else(|| invalid_page(page_no.get(), "node slot contains unknown flags"))?,
		out_overflow: read_optional_page(bytes, NODE_OUT_OVERFLOW_OFFSET),
		in_overflow: read_optional_page(bytes, NODE_IN_OVERFLOW_OFFSET),
		undirected_overflow: read_optional_page(bytes, NODE_UNDIRECTED_OVERFLOW_OFFSET),
		overlay: PropertyRef::decode(
			read_u64(bytes, NODE_OVERLAY_PAGE_OFFSET),
			read_u32(bytes, NODE_OVERLAY_SLOT_OFFSET),
			read_u32(bytes, NODE_OVERLAY_LEN_OFFSET),
			"node overlay",
		)?,
		global_atom: (raw_global != u64::MAX)
			.then(|| GlobalAtomId::from_storage(raw_global))
			.transpose()?,
		out_inline: read_disk_refs(bytes, NODE_OUT_INLINE_OFFSET)?,
		in_inline: read_disk_refs(bytes, NODE_IN_INLINE_OFFSET)?,
		undirected_inline: read_disk_refs(bytes, NODE_UNDIRECTED_INLINE_OFFSET)?,
	};
	validate_node_slot(&value)?;
	Ok(value)
}

/// Initializes one edge-hot page in its final aligned storage.
///
/// # Errors
///
/// Returns an error when the tombstone slot layout or final checksum cannot be
/// encoded.
pub(in crate::storage) fn initialize_edge_hot_page(
	page: &mut AlignedPage,
	page_no: PageNo,
	page_lsn: PageLsn,
) -> FerrosGrynnResult<()> {
	page.reset(PageType::EdgeHot, PageFlags::empty(), page_lsn, page_no);
	for slot in 0..HOT_SLOTS_PER_PAGE {
		write_edge_hot_slot(page, slot, &EdgeHotSlot::tombstone(0))?;
	}
	page.seal()?;
	Ok(())
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
	bytes[EDGE_TOPOLOGY_OFFSET] = value.topology as u8;
	bytes[EDGE_FLAGS_OFFSET] = value.flags.bits();
	bytes[EDGE_KIND_EXT_OFFSET] = u8::from(value.has_transition);
	write_u32(bytes, EDGE_GENERATION_OFFSET, value.generation);
	write_u32(bytes, EDGE_SOURCE_COUNT_OFFSET, value.source_count);
	write_u32(bytes, EDGE_TARGET_COUNT_OFFSET, value.target_count);
	write_optional_u32(bytes, EDGE_LABEL_OFFSET, value.label_id);
	write_u32(bytes, EDGE_RESERVED_A_OFFSET, 0);
	write_u64(bytes, EDGE_WEIGHT_OFFSET, value.weight.map_or(ABSENT_WEIGHT_BITS, f64::to_bits));
	write_optional_u32(bytes, EDGE_TRANSITION_GRAPH_OFFSET, value.transition_subgraph_slot);
	write_optional_u32(bytes, EDGE_SOURCE_GRAPH_OFFSET, value.source_subgraph_slot);
	write_optional_u32(bytes, EDGE_TARGET_GRAPH_OFFSET, value.target_subgraph_slot);
	write_record_ref(
		bytes,
		EDGE_OVERLAY_PAGE_OFFSET,
		EDGE_OVERLAY_SLOT_OFFSET,
		EDGE_OVERLAY_LEN_OFFSET,
		value.overlay,
	);
	write_optional_page(bytes, EDGE_SOURCE_OVERFLOW_OFFSET, value.source_overflow);
	write_optional_page(bytes, EDGE_TARGET_OVERFLOW_OFFSET, value.target_overflow);
	write_u64(
		bytes,
		EDGE_GLOBAL_ATOM_OFFSET,
		value.global_atom.map_or(u64::MAX, GlobalAtomId::raw),
	);
	write_optional_page(bytes, EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET, value.source_subgraph_overflow);
	write_optional_page(bytes, EDGE_TARGET_GRAPH_OVERFLOW_OFFSET, value.target_subgraph_overflow);
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
	validate_zero(&page.as_bytes()[HOT_RESERVED_OFFSET..], page_no, "EdgeHot tail")?;
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
		&bytes[EDGE_PADDING_B_OFFSET..EDGE_OVERLAY_PAGE_OFFSET],
		page_no,
		"edge padding",
	)?;
	validate_zero(
		&bytes[EDGE_RESERVED_B_OFFSET..EDGE_SOURCE_INLINE_OFFSET],
		page_no,
		"edge reserved",
	)?;
	validate_zero(&bytes[EDGE_RESERVED_C_OFFSET..], page_no, "edge reserved")?;
	if read_u32(bytes, EDGE_RESERVED_A_OFFSET) != 0 {
		return Err(invalid_page(page_no.get(), "edge reserved field is non-zero"));
	}
	let kind_ext = bytes[EDGE_KIND_EXT_OFFSET];
	if kind_ext & !0x01 != 0 {
		return Err(invalid_page(page_no.get(), "edge kind extension contains reserved bits"));
	}
	let raw_weight = read_u64(bytes, EDGE_WEIGHT_OFFSET);
	if f64::from_bits(raw_weight).is_nan() && raw_weight != ABSENT_WEIGHT_BITS {
		return Err(invalid_page(page_no.get(), "edge weight uses a non-canonical NaN"));
	}
	let raw_global = read_u64(bytes, EDGE_GLOBAL_ATOM_OFFSET);
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
		weight: (raw_weight != ABSENT_WEIGHT_BITS).then(|| f64::from_bits(raw_weight)),
		transition_subgraph_slot: read_optional_u32(bytes, EDGE_TRANSITION_GRAPH_OFFSET),
		source_subgraph_slot: read_optional_u32(bytes, EDGE_SOURCE_GRAPH_OFFSET),
		target_subgraph_slot: read_optional_u32(bytes, EDGE_TARGET_GRAPH_OFFSET),
		overlay: PropertyRef::decode(
			read_u64(bytes, EDGE_OVERLAY_PAGE_OFFSET),
			read_u32(bytes, EDGE_OVERLAY_SLOT_OFFSET),
			read_u32(bytes, EDGE_OVERLAY_LEN_OFFSET),
			"edge overlay",
		)?,
		source_overflow: read_optional_page(bytes, EDGE_SOURCE_OVERFLOW_OFFSET),
		target_overflow: read_optional_page(bytes, EDGE_TARGET_OVERFLOW_OFFSET),
		global_atom: (raw_global != u64::MAX)
			.then(|| GlobalAtomId::from_storage(raw_global))
			.transpose()?,
		source_subgraph_overflow: read_optional_page(bytes, EDGE_SOURCE_GRAPH_OVERFLOW_OFFSET),
		target_subgraph_overflow: read_optional_page(bytes, EDGE_TARGET_GRAPH_OVERFLOW_OFFSET),
		source_inline: read_disk_refs(bytes, EDGE_SOURCE_INLINE_OFFSET)?,
		target_inline: read_disk_refs(bytes, EDGE_TARGET_INLINE_OFFSET)?,
	};
	validate_edge_slot(&value)?;
	Ok(value)
}

fn hot_slot_range(
	page: &AlignedPage,
	page_type: PageType,
	slot: usize,
) -> FerrosGrynnResult<std::ops::Range<usize>> {
	if slot >= HOT_SLOTS_PER_PAGE || read_u16(page.as_bytes(), 4) != page_type.get() {
		return Err(invalid_page(0, "hot slot address or page type is invalid"));
	}
	let start = PAGE_HEADER_SIZE + slot * HOT_SLOT_BYTES;
	Ok(start..start + HOT_SLOT_BYTES)
}

fn write_record_ref(
	bytes: &mut [u8],
	page_offset: usize,
	slot_offset: usize,
	length_offset: usize,
	reference: PropertyRef,
) {
	let (page, slot, length) = reference.fields();
	write_u64(bytes, page_offset, page);
	write_u32(bytes, slot_offset, slot);
	write_u32(bytes, length_offset, length);
}

fn write_optional_u32(bytes: &mut [u8], offset: usize, value: Option<u32>) {
	write_u32(bytes, offset, value.unwrap_or(NULL_U32));
}

fn read_optional_u32(bytes: &[u8], offset: usize) -> Option<u32> {
	let value = read_u32(bytes, offset);
	(value != NULL_U32).then_some(value)
}

fn write_disk_refs<const N: usize>(
	bytes: &mut [u8],
	offset: usize,
	values: &[Option<DiskAtomRef>; N],
) {
	for (index, value) in values.iter().enumerate() {
		write_u64(bytes, offset + index * 8, value.map_or(u64::MAX, DiskAtomRef::raw));
	}
}

fn read_disk_refs<const N: usize>(
	bytes: &[u8],
	offset: usize,
) -> FerrosGrynnResult<[Option<DiskAtomRef>; N]> {
	let mut result = [None; N];
	for (index, value) in result.iter_mut().enumerate() {
		let raw = read_u64(bytes, offset + index * 8);
		*value = (raw != u64::MAX).then(|| DiskAtomRef::from_raw(raw)).transpose()?;
	}
	Ok(result)
}

#[cfg(test)]
mod tests {
	use std::mem::align_of;

	use super::*;
	use crate::{
		graph::{AtomId, AtomKind},
		storage::atom_page::PropertyRef,
	};

	#[inline(never)]
	fn verify_final_buffers() -> FerrosGrynnResult<[(usize, usize, usize); 2]> {
		let mut pages = (0..2).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		let before = pages.iter().map(|page| std::ptr::from_ref(page).addr()).collect::<Vec<_>>();
		assert!(before.iter().all(|address| address % align_of::<AlignedPage>() == 0));
		initialize_node_hot_page(&mut pages[0], PageNo::new(0), PageLsn::new(9))?;
		initialize_edge_hot_page(&mut pages[1], PageNo::new(1), PageLsn::new(9))?;
		let after = pages.iter().map(|page| std::ptr::from_ref(page).addr()).collect::<Vec<_>>();
		assert_eq!(before, after);

		let mut observations = [(0, 0, 0); 2];
		for (index, page) in pages.iter().enumerate() {
			let in_loop = std::ptr::from_ref(page).addr();
			assert_eq!(in_loop, before[index]);
			assert_eq!(in_loop % align_of::<AlignedPage>(), 0);
			observations[index] = (before[index], after[index], in_loop);
		}

		assert_eq!(&pages[0].as_bytes()[..4], b"FHN\x01");
		assert_eq!(&pages[1].as_bytes()[..4], b"FHE\x01");
		assert_eq!(pages[0].validate(PageNo::new(0))?.page_type(), PageType::NodeHot);
		assert_eq!(pages[1].validate(PageNo::new(1))?.page_type(), PageType::EdgeHot);
		write_node_hot_slot(&mut pages[0], 0, &NodeHotSlot::tombstone(1))?;
		pages[0].seal()?;
		assert_eq!(&pages[0].as_bytes()[..4], b"FHN\x01");
		assert_eq!(pages[0].validate(PageNo::new(0))?.page_type(), PageType::NodeHot);
		Ok(observations)
	}

	#[test]
	fn final_buffers_preserve_addresses_alignment_and_headers() -> FerrosGrynnResult<()> {
		for (index, (before, after, in_loop)) in verify_final_buffers()?.into_iter().enumerate() {
			eprintln!(
				"Hot buffer {index}: before={before:#x}, after={after:#x}, in_loop={in_loop:#x}, \
				 mod={}",
				in_loop % align_of::<AlignedPage>()
			);
		}
		Ok(())
	}

	#[test]
	fn node_occurrence_uses_normative_global_and_overlay_offsets() -> FerrosGrynnResult<()> {
		let global = GlobalAtomId::from_raw(AtomId::from_parts(AtomKind::Node, 8, 3)?.raw());
		let edge = DiskAtomRef::from_parts(AtomKind::Edge, 7)?;
		let expected = NodeHotSlot {
			generation: 4,
			subgraph_slot: None,
			label_id: Some(2),
			out_count: 1,
			in_count: 0,
			undirected_count: 0,
			flags: NodeSlotFlags::HAS_OVERLAY,
			out_overflow: None,
			in_overflow: None,
			undirected_overflow: None,
			overlay: PropertyRef::inline(PageNo::new(5), 2, 19)?,
			global_atom: Some(global),
			out_inline: [Some(edge), None, None, None],
			in_inline: [None; 4],
			undirected_inline: [None; 2],
		};
		let mut pages = vec![AlignedPage::zeroed()];
		initialize_node_hot_page(&mut pages[0], PageNo::new(0), PageLsn::new(9))?;
		write_node_hot_slot(&mut pages[0], 0, &expected)?;
		pages[0].seal()?;
		assert_eq!(read_node_hot_slot(&pages[0], PageNo::new(0), 0)?, expected);
		assert_eq!(read_u32(pages[0].as_bytes(), PAGE_HEADER_SIZE + 12), 0);
		assert_eq!(read_u64(pages[0].as_bytes(), PAGE_HEADER_SIZE + 72), global.raw());
		Ok(())
	}

	#[test]
	fn reserved_edge_flag_is_rejected() -> FerrosGrynnResult<()> {
		let mut pages = vec![AlignedPage::zeroed()];
		initialize_edge_hot_page(&mut pages[0], PageNo::new(0), PageLsn::ZERO)?;
		pages[0].as_bytes_mut()[PAGE_HEADER_SIZE + EDGE_FLAGS_OFFSET] = 0x20;
		pages[0].seal()?;
		assert!(read_edge_hot_slot(&pages[0], PageNo::new(0), 0).is_err());
		Ok(())
	}
}
