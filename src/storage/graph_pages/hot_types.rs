use bitflags::bitflags;

use super::PageNo;
use crate::{
	datastore::GlobalAtomId,
	storage::atom_page::PropertyRef,
	values::storage::DiskAtomRef,
};

bitflags! {
	/// Assigned node-occurrence flags.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub(in crate::storage) struct NodeSlotFlags: u8 {
		const HAS_OVERLAY = 0x10;
		const TOMBSTONE = 0x40;
		const META = 0x80;
	}

	/// Assigned edge-occurrence flags.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub(in crate::storage) struct EdgeSlotFlags: u8 {
		const TOMBSTONE = 0x01;
		const META = 0x04;
		const CROSS_LEVEL = 0x40;
		const HAS_OVERLAY = 0x80;
	}
}

/// Closed format-v1 edge-topology code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::storage) enum EdgeTopologyCode {
	Directed = 0,
	Undirected = 1,
	Bidirectional = 2,
	HyperDirected = 3,
	HyperUndirected = 4,
	Loop = 5,
}

impl EdgeTopologyCode {
	pub(in crate::storage) fn from_raw(raw: u8) -> Option<Self> {
		match raw {
			0 => Some(Self::Directed),
			1 => Some(Self::Undirected),
			2 => Some(Self::Bidirectional),
			3 => Some(Self::HyperDirected),
			4 => Some(Self::HyperUndirected),
			5 => Some(Self::Loop),
			_ => None,
		}
	}
}

/// Decoded graph-local node occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::storage) struct NodeHotSlot {
	pub(super) generation: u32,
	pub(super) subgraph_slot: Option<u32>,
	pub(super) label_id: Option<u32>,
	pub(super) out_count: u32,
	pub(super) in_count: u32,
	pub(super) undirected_count: u32,
	pub(super) flags: NodeSlotFlags,
	pub(super) out_overflow: Option<PageNo>,
	pub(super) in_overflow: Option<PageNo>,
	pub(super) undirected_overflow: Option<PageNo>,
	pub(super) overlay: PropertyRef,
	pub(super) global_atom: Option<GlobalAtomId>,
	pub(super) out_inline: [Option<DiskAtomRef>; 4],
	pub(super) in_inline: [Option<DiskAtomRef>; 4],
	pub(super) undirected_inline: [Option<DiskAtomRef>; 2],
}

impl NodeHotSlot {
	pub(super) fn tombstone(generation: u32) -> Self {
		Self {
			generation,
			subgraph_slot: None,
			label_id: None,
			out_count: 0,
			in_count: 0,
			undirected_count: 0,
			flags: NodeSlotFlags::TOMBSTONE,
			out_overflow: None,
			in_overflow: None,
			undirected_overflow: None,
			overlay: PropertyRef::None,
			global_atom: None,
			out_inline: [None; 4],
			in_inline: [None; 4],
			undirected_inline: [None; 2],
		}
	}
}

/// Decoded graph-local edge occurrence.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::storage) struct EdgeHotSlot {
	pub(super) topology: EdgeTopologyCode,
	pub(super) flags: EdgeSlotFlags,
	pub(super) has_transition: bool,
	pub(super) generation: u32,
	pub(super) source_count: u32,
	pub(super) target_count: u32,
	pub(super) label_id: Option<u32>,
	pub(super) weight: Option<f64>,
	pub(super) transition_subgraph_slot: Option<u32>,
	pub(super) source_subgraph_slot: Option<u32>,
	pub(super) target_subgraph_slot: Option<u32>,
	pub(super) overlay: PropertyRef,
	pub(super) source_overflow: Option<PageNo>,
	pub(super) target_overflow: Option<PageNo>,
	pub(super) global_atom: Option<GlobalAtomId>,
	pub(super) source_subgraph_overflow: Option<PageNo>,
	pub(super) target_subgraph_overflow: Option<PageNo>,
	pub(super) source_inline: [Option<DiskAtomRef>; 2],
	pub(super) target_inline: [Option<DiskAtomRef>; 2],
}

impl EdgeHotSlot {
	pub(super) fn tombstone(generation: u32) -> Self {
		Self {
			topology: EdgeTopologyCode::Directed,
			flags: EdgeSlotFlags::TOMBSTONE,
			has_transition: false,
			generation,
			source_count: 0,
			target_count: 0,
			label_id: None,
			weight: None,
			transition_subgraph_slot: None,
			source_subgraph_slot: None,
			target_subgraph_slot: None,
			overlay: PropertyRef::None,
			source_overflow: None,
			target_overflow: None,
			global_atom: None,
			source_subgraph_overflow: None,
			target_subgraph_overflow: None,
			source_inline: [None; 2],
			target_inline: [None; 2],
		}
	}
}
