use super::{
	DiskAtomRef,
	EdgeHotSlot,
	EdgeSlotFlags,
	EdgeTopologyCode,
	NodeHotSlot,
	NodeSlotFlags,
	PageNo,
	invalid_page,
};
use crate::{graph::AtomKind, storage::atom_page::PropertyRef};

const MAX_GENERATION: u32 = (1 << 23) - 1;

pub(super) fn validate_node_slot(value: &NodeHotSlot) -> crate::gql_status::FerrosGrynnResult<()> {
	validate_generation(value.generation)?;
	if value.flags.contains(NodeSlotFlags::TOMBSTONE) {
		if value.flags != NodeSlotFlags::TOMBSTONE
			|| value.subgraph_slot.is_some()
			|| value.label_id.is_some()
			|| value.out_count != 0
			|| value.in_count != 0
			|| value.undirected_count != 0
			|| value.out_overflow.is_some()
			|| value.in_overflow.is_some()
			|| value.undirected_overflow.is_some()
			|| value.overlay != PropertyRef::None
			|| value.global_atom.is_some()
			|| value.out_inline.iter().any(Option::is_some)
			|| value.in_inline.iter().any(Option::is_some)
			|| value.undirected_inline.iter().any(Option::is_some)
		{
			return Err(invalid_page(0, "node tombstone is not canonical"));
		}
		return Ok(());
	}
	if value.global_atom.and_then(|atom| atom.kind()) != Some(AtomKind::Node) {
		return Err(invalid_page(0, "live node occurrence requires a global node identity"));
	}
	if value.flags.contains(NodeSlotFlags::META) != value.subgraph_slot.is_some() {
		return Err(invalid_page(0, "node META flag disagrees with subgraph slot"));
	}
	validate_overlay(value.flags.contains(NodeSlotFlags::HAS_OVERLAY), value.overlay)?;
	validate_inline(value.out_count, &value.out_inline, value.out_overflow, "node out")?;
	validate_inline(value.in_count, &value.in_inline, value.in_overflow, "node in")?;
	validate_inline(
		value.undirected_count,
		&value.undirected_inline,
		value.undirected_overflow,
		"node undirected",
	)
}

pub(super) fn validate_edge_slot(value: &EdgeHotSlot) -> crate::gql_status::FerrosGrynnResult<()> {
	validate_generation(value.generation)?;
	if value.flags.contains(EdgeSlotFlags::TOMBSTONE) {
		if value.topology != EdgeTopologyCode::Directed
			|| value.flags != EdgeSlotFlags::TOMBSTONE
			|| value.has_transition
			|| value.source_count != 0
			|| value.target_count != 0
			|| value.label_id.is_some()
			|| value.weight.is_some()
			|| value.transition_subgraph_slot.is_some()
			|| value.source_subgraph_slot.is_some()
			|| value.target_subgraph_slot.is_some()
			|| value.overlay != PropertyRef::None
			|| value.source_overflow.is_some()
			|| value.target_overflow.is_some()
			|| value.global_atom.is_some()
			|| value.source_subgraph_overflow.is_some()
			|| value.target_subgraph_overflow.is_some()
			|| value.source_inline.iter().any(Option::is_some)
			|| value.target_inline.iter().any(Option::is_some)
		{
			return Err(invalid_page(0, "edge tombstone is not canonical"));
		}
		return Ok(());
	}
	if value.global_atom.and_then(|atom| atom.kind()) != Some(AtomKind::Edge) {
		return Err(invalid_page(0, "live edge occurrence requires a global edge identity"));
	}
	if value.weight.is_some_and(|weight| !weight.is_finite()) {
		return Err(invalid_page(0, "edge weight must be finite"));
	}
	let total = value
		.source_count
		.checked_add(value.target_count)
		.ok_or_else(|| invalid_page(0, "edge endpoint count overflows u32"))?;
	let topology_matches = match value.topology {
		EdgeTopologyCode::Directed | EdgeTopologyCode::Bidirectional | EdgeTopologyCode::Loop => {
			total == 2
		}
		EdgeTopologyCode::Undirected => total == 2 && value.target_count == 0,
		EdgeTopologyCode::HyperDirected => total >= 2,
		EdgeTopologyCode::HyperUndirected => total >= 2 && value.target_count == 0,
	};
	if !topology_matches {
		return Err(invalid_page(0, "edge endpoint counts disagree with topology"));
	}
	let meta = value.flags.contains(EdgeSlotFlags::META);
	if meta != value.has_transition || meta != value.transition_subgraph_slot.is_some() {
		return Err(invalid_page(0, "edge META fields disagree"));
	}
	validate_overlay(value.flags.contains(EdgeSlotFlags::HAS_OVERLAY), value.overlay)?;
	if value.flags.contains(EdgeSlotFlags::CROSS_LEVEL) {
		validate_cross_level(
			value.source_count,
			&value.source_inline,
			value.source_overflow,
			value.source_subgraph_slot,
			value.source_subgraph_overflow,
			"edge source",
		)?;
		validate_cross_level(
			value.target_count,
			&value.target_inline,
			value.target_overflow,
			value.target_subgraph_slot,
			value.target_subgraph_overflow,
			"edge target",
		)?;
	} else {
		if value.source_subgraph_slot.is_some()
			|| value.target_subgraph_slot.is_some()
			|| value.source_subgraph_overflow.is_some()
			|| value.target_subgraph_overflow.is_some()
		{
			return Err(invalid_page(0, "local edge contains cross-level addresses"));
		}
		validate_inline(
			value.source_count,
			&value.source_inline,
			value.source_overflow,
			"edge source",
		)?;
		validate_inline(
			value.target_count,
			&value.target_inline,
			value.target_overflow,
			"edge target",
		)?;
	}
	if value.topology == EdgeTopologyCode::Loop && value.source_inline[0] != value.target_inline[0]
	{
		return Err(invalid_page(0, "loop endpoints must be identical"));
	}
	Ok(())
}

fn validate_generation(generation: u32) -> crate::gql_status::FerrosGrynnResult<()> {
	if generation > MAX_GENERATION {
		return Err(invalid_page(0, "hot-slot generation exceeds 23 bits"));
	}
	Ok(())
}

fn validate_overlay(flag: bool, overlay: PropertyRef) -> crate::gql_status::FerrosGrynnResult<()> {
	if flag != (overlay != PropertyRef::None) {
		return Err(invalid_page(0, "HAS_OVERLAY disagrees with overlay reference"));
	}
	Ok(())
}

fn validate_inline<const N: usize>(
	count: u32,
	inline: &[Option<DiskAtomRef>; N],
	overflow: Option<PageNo>,
	field: &str,
) -> crate::gql_status::FerrosGrynnResult<()> {
	let count = usize::try_from(count)
		.map_err(|_error| invalid_page(0, format!("{field} count exceeds usize")))?;
	let inline_count = count.min(N);
	if inline[..inline_count].iter().any(Option::is_none)
		|| inline[inline_count..].iter().any(Option::is_some)
		|| (count > N) != overflow.is_some()
	{
		return Err(invalid_page(0, format!("{field} inline and overflow fields disagree")));
	}
	Ok(())
}

fn validate_cross_level<const N: usize>(
	count: u32,
	inline: &[Option<DiskAtomRef>; N],
	local_overflow: Option<PageNo>,
	graph_slot: Option<u32>,
	graph_overflow: Option<PageNo>,
	field: &str,
) -> crate::gql_status::FerrosGrynnResult<()> {
	if local_overflow.is_some() {
		return Err(invalid_page(0, format!("{field} uses a local overflow")));
	}
	if graph_overflow.is_some() {
		if graph_slot.is_some() || inline.iter().any(Option::is_some) || count == 0 {
			return Err(invalid_page(0, format!("{field} qualified overflow is inconsistent")));
		}
		return Ok(());
	}
	if count == 0 {
		if graph_slot.is_some() || inline.iter().any(Option::is_some) {
			return Err(invalid_page(0, format!("{field} empty side has an address")));
		}
		return Ok(());
	}
	let count = usize::try_from(count)
		.map_err(|_error| invalid_page(0, format!("{field} count exceeds usize")))?;
	if graph_slot.is_none() || count > N {
		return Err(invalid_page(0, format!("{field} inline qualified side is invalid")));
	}
	validate_inline(count as u32, inline, None, field)
}
