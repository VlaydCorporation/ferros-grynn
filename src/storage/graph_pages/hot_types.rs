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
		/// Node occurrence has a non-null property-overlay reference.
		const HAS_OVERLAY = 0x10;
		/// Node slot is logically deleted and retains only its generation.
		const TOMBSTONE = 0x40;
		/// Node occurrence owns the nested graph named by its subgraph slot.
		const META = 0x80;
	}

	/// Assigned edge-occurrence flags.
	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
	pub(in crate::storage) struct EdgeSlotFlags: u8 {
		/// Edge slot is logically deleted and has no live topology or property references.
		const TOMBSTONE = 0x01;
		/// Edge occurrence represents a metaedge with an internal graph.
		const META = 0x04;
		/// Endpoint references carry graph-scope paths instead of only local atom references.
		const CROSS_LEVEL = 0x40;
		/// Edge occurrence has a non-null property-overlay reference.
		const HAS_OVERLAY = 0x80;
	}
}

/// Closed format-v1 edge-topology code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::storage) enum EdgeTopologyCode {
	/// Format discriminator for a directed edge.
	Directed = 0,
	/// Format discriminator for an undirected edge.
	Undirected = 1,
	/// Format discriminator for a bidirectional edge.
	Bidirectional = 2,
	/// Format discriminator for a directed hyperedge.
	HyperDirected = 3,
	/// Format discriminator for an undirected hyperedge.
	HyperUndirected = 4,
	/// Format discriminator for a loop edge.
	Loop = 5,
}

impl EdgeTopologyCode {
	/// Creates from raw.
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
	/// Generation identifying this version of the data.
	pub(in crate::storage) generation: u32,
	/// Subgraph slot associated with this node hot slot.
	pub(in crate::storage) subgraph_slot: Option<u32>,
	/// Label id associated with this node hot slot.
	pub(super) label_id: Option<u32>,
	/// Out count associated with this node hot slot.
	pub(in crate::storage) out_count: u32,
	/// In count associated with this node hot slot.
	pub(in crate::storage) in_count: u32,
	/// Undirected count associated with this node hot slot.
	pub(in crate::storage) undirected_count: u32,
	/// Flags associated with this node hot slot.
	pub(in crate::storage) flags: NodeSlotFlags,
	/// Out overflow associated with this node hot slot.
	pub(in crate::storage) out_overflow: Option<PageNo>,
	/// In overflow associated with this node hot slot.
	pub(in crate::storage) in_overflow: Option<PageNo>,
	/// Undirected overflow associated with this node hot slot.
	pub(in crate::storage) undirected_overflow: Option<PageNo>,
	/// Overlay associated with this node hot slot.
	pub(in crate::storage) overlay: PropertyRef,
	/// Global atom associated with this node hot slot.
	pub(in crate::storage) global_atom: Option<GlobalAtomId>,
	/// Out inline associated with this node hot slot.
	pub(in crate::storage) out_inline: [Option<DiskAtomRef>; 4],
	/// In inline associated with this node hot slot.
	pub(in crate::storage) in_inline: [Option<DiskAtomRef>; 4],
	/// Undirected inline associated with this node hot slot.
	pub(in crate::storage) undirected_inline: [Option<DiskAtomRef>; 2],
}

impl NodeHotSlot {
	/// Returns the tombstone result.
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
	/// Topology associated with this edge hot slot.
	pub(in crate::storage) topology: EdgeTopologyCode,
	/// Flags associated with this edge hot slot.
	pub(in crate::storage) flags: EdgeSlotFlags,
	/// Has transition associated with this edge hot slot.
	pub(super) has_transition: bool,
	/// Generation identifying this version of the data.
	pub(in crate::storage) generation: u32,
	/// Source count associated with this edge hot slot.
	pub(in crate::storage) source_count: u32,
	/// Target count associated with this edge hot slot.
	pub(in crate::storage) target_count: u32,
	/// Label id associated with this edge hot slot.
	pub(super) label_id: Option<u32>,
	/// Weight associated with this edge hot slot.
	pub(super) weight: Option<f64>,
	/// Transition subgraph slot associated with this edge hot slot.
	pub(in crate::storage) transition_subgraph_slot: Option<u32>,
	/// Source subgraph slot associated with this edge hot slot.
	pub(super) source_subgraph_slot: Option<u32>,
	/// Target subgraph slot associated with this edge hot slot.
	pub(super) target_subgraph_slot: Option<u32>,
	/// Overlay associated with this edge hot slot.
	pub(in crate::storage) overlay: PropertyRef,
	/// Source overflow associated with this edge hot slot.
	pub(in crate::storage) source_overflow: Option<PageNo>,
	/// Target overflow associated with this edge hot slot.
	pub(in crate::storage) target_overflow: Option<PageNo>,
	/// Global atom associated with this edge hot slot.
	pub(in crate::storage) global_atom: Option<GlobalAtomId>,
	/// Source subgraph overflow associated with this edge hot slot.
	pub(in crate::storage) source_subgraph_overflow: Option<PageNo>,
	/// Target subgraph overflow associated with this edge hot slot.
	pub(in crate::storage) target_subgraph_overflow: Option<PageNo>,
	/// Source inline associated with this edge hot slot.
	pub(in crate::storage) source_inline: [Option<DiskAtomRef>; 2],
	/// Target inline associated with this edge hot slot.
	pub(in crate::storage) target_inline: [Option<DiskAtomRef>; 2],
}

impl EdgeHotSlot {
	/// Returns the tombstone result.
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
