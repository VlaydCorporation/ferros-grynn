use bitflags::bitflags;

use super::{PageNo, invalid_page};
use crate::{
	datastore::GlobalAtomId,
	gql_status::FerrosGrynnResult,
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
	/// Replaces only the expected source label; the publisher owns its
	/// namespace.
	///
	/// # Errors
	/// Rejects a mismatched source or the reserved null sentinel as a live ID.
	pub(in crate::storage) fn replace_label(
		&mut self,
		page: PageNo,
		expected: Option<u32>,
		label: Option<u32>,
	) -> FerrosGrynnResult<()> {
		replace_label(&mut self.label_id, page, expected, label)
	}

	/// Constructs a live node before optional adjacency/metadata are assigned.
	pub(in crate::storage) fn isolated(generation: u32, global: GlobalAtomId) -> Self {
		Self {
			flags: NodeSlotFlags::empty(),
			global_atom: Some(global),
			..Self::tombstone(generation)
		}
	}

	/// Retains the allocator generation after a node is freed.
	pub(in crate::storage) fn tombstone(generation: u32) -> Self {
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
	/// Constructs a local plain edge without exposing optional codec fields.
	///
	/// # Errors
	/// Rejects qualified endpoints, invalid counts, weight or format-v1
	/// topology.
	pub(in crate::storage) fn plain(
		generation: u32,
		global: GlobalAtomId,
		topology: &super::EncodedTopology,
		overflow: (Option<PageNo>, Option<PageNo>),
		weight: Option<f64>,
	) -> FerrosGrynnResult<Self> {
		if topology.source.iter().chain(&topology.target).any(|endpoint| endpoint.graph_slot != 0) {
			return Err(invalid_page(0, "plain edge cannot contain qualified endpoints"));
		}
		let side = |entries: &[super::EncodedEndpoint], overflow| super::EncodedHotSide {
			inline: std::array::from_fn(|index| entries.get(index).map(|endpoint| endpoint.atom)),
			local_overflow: overflow,
			graph_slot: None,
			graph_overflow: None,
		};
		Self::live(
			generation,
			global,
			topology,
			(&side(&topology.source, overflow.0), &side(&topology.target, overflow.1)),
			false,
			None,
			weight,
			PropertyRef::None,
			None,
		)
	}

	/// Combines rendered sides and metadata under one format-v1 validator.
	///
	/// Callers retain ownership of endpoint resolution and overflow admission.
	///
	/// # Errors
	/// Rejects invalid counts, identities, weights or conflicting optional
	/// fields.
	pub(in crate::storage) fn live(
		generation: u32,
		global: GlobalAtomId,
		topology: &super::EncodedTopology,
		sides: (&super::EncodedHotSide, &super::EncodedHotSide),
		cross_level: bool,
		label: Option<u32>,
		weight: Option<f64>,
		overlay: PropertyRef,
		transition: Option<u32>,
	) -> FerrosGrynnResult<Self> {
		let (source, target) = sides;
		let mut flags = EdgeSlotFlags::empty();
		flags.set(EdgeSlotFlags::META, transition.is_some());
		flags.set(EdgeSlotFlags::CROSS_LEVEL, cross_level);
		flags.set(EdgeSlotFlags::HAS_OVERLAY, overlay != PropertyRef::None);
		let result = Self {
			topology: topology.code,
			flags,
			has_transition: transition.is_some(),
			generation,
			source_count: u32::try_from(topology.source.len())
				.map_err(|_error| invalid_page(0, "edge source count exceeds u32"))?,
			target_count: u32::try_from(topology.target.len())
				.map_err(|_error| invalid_page(0, "edge target count exceeds u32"))?,
			label_id: label,
			weight,
			transition_subgraph_slot: transition,
			source_subgraph_slot: source.graph_slot,
			target_subgraph_slot: target.graph_slot,
			overlay,
			source_overflow: source.local_overflow,
			target_overflow: target.local_overflow,
			global_atom: Some(global),
			source_subgraph_overflow: source.graph_overflow,
			target_subgraph_overflow: target.graph_overflow,
			source_inline: source.inline,
			target_inline: target.inline,
		};
		super::hot_validation::validate_edge_slot(&result)?;
		Ok(result)
	}

	/// Replaces only the expected source label; the publisher owns its
	/// namespace.
	///
	/// # Errors
	/// Rejects a mismatched source or the reserved null sentinel as a live ID.
	pub(in crate::storage) fn replace_label(
		&mut self,
		page: PageNo,
		expected: Option<u32>,
		label: Option<u32>,
	) -> FerrosGrynnResult<()> {
		replace_label(&mut self.label_id, page, expected, label)
	}

	/// Changes only a validated live EDGE's child binding and its two flags.
	///
	/// # Errors
	/// Rejects an invalid source, wrong expected child or reserved child slot
	/// before changing any field.
	pub(in crate::storage) fn replace_transition(
		&mut self,
		page: PageNo,
		expected: Option<u32>,
		transition: Option<u32>,
	) -> FerrosGrynnResult<()> {
		super::hot_validation::validate_edge_slot(self)?;
		if self.flags.contains(EdgeSlotFlags::TOMBSTONE)
			|| self.transition_subgraph_slot != expected
			|| transition.is_some_and(|slot| slot == 0 || slot == u32::MAX)
		{
			return Err(invalid_page(page.get(), "edge child source or replacement disagrees"));
		}
		self.transition_subgraph_slot = transition;
		self.has_transition = transition.is_some();
		self.flags.set(EdgeSlotFlags::META, transition.is_some());
		Ok(())
	}

	/// Reads an inline role scope without exposing mutable codec fields.
	pub(in crate::storage) const fn endpoint_graph_slot(
		&self,
		side: super::EndpointSide,
	) -> Option<u32> {
		match side {
			super::EndpointSide::Source => self.source_subgraph_slot,
			super::EndpointSide::Target => self.target_subgraph_slot,
		}
	}

	/// Returns the validated physical weight without exposing mutable codec
	/// fields.
	pub(in crate::storage) const fn weight(&self) -> Option<f64> {
		self.weight
	}

	/// Replaces a weight only when its decoded source matches the accepted
	/// root.
	///
	/// # Errors
	/// Rejects a mismatched source or a non-finite replacement before encoding.
	pub(in crate::storage) fn replace_weight(
		&mut self,
		page: PageNo,
		expected: Option<f64>,
		weight: Option<f64>,
	) -> FerrosGrynnResult<()> {
		if self.weight() != expected || weight.is_some_and(|value| !value.is_finite()) {
			return Err(invalid_page(page.get(), "edge weight source or replacement disagrees"));
		}
		self.weight = weight;
		Ok(())
	}

	/// Returns the tombstone result.
	pub(in crate::storage) fn tombstone(generation: u32) -> Self {
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

/// Keeps the two Hot slot kinds on one checked dense-label replacement
/// contract.
fn replace_label(
	current: &mut Option<u32>,
	page: PageNo,
	expected: Option<u32>,
	label: Option<u32>,
) -> FerrosGrynnResult<()> {
	if *current != expected || label == Some(u32::MAX) {
		return Err(invalid_page(page.get(), "label source or replacement disagrees"));
	}
	*current = label;
	Ok(())
}

#[cfg(test)]
/// Checked scalar updates reject bad inputs without modifying their source.
mod tests {
	use super::*;
	use crate::test_assertions::AssertResult as _;

	/// One qualified role and one root-local role keep their distinct scopes.
	fn scoped_sides() -> (
		GlobalAtomId,
		crate::storage::graph_pages::EncodedTopology,
		crate::storage::graph_pages::EncodedHotSide,
		crate::storage::graph_pages::EncodedHotSide,
	) {
		use crate::{
			graph::{AtomId, AtomKind},
			storage::graph_pages::{EncodedEndpoint, EncodedHotSide, EncodedTopology},
		};
		let global =
			GlobalAtomId::from_raw(AtomId::from_parts(AtomKind::Edge, 5, 2).assert_ok().raw());
		let left = DiskAtomRef::from_parts(AtomKind::Node, 3).assert_ok();
		let right = DiskAtomRef::from_parts(AtomKind::Edge, 9).assert_ok();
		let topology = EncodedTopology {
			code: EdgeTopologyCode::Directed,
			source: vec![EncodedEndpoint {
				graph_slot: 7,
				atom: left,
			}],
			target: vec![EncodedEndpoint {
				graph_slot: 0,
				atom: right,
			}],
		};
		let side = |atom, scope| EncodedHotSide {
			inline: [Some(atom), None],
			local_overflow: None,
			graph_slot: Some(scope),
			graph_overflow: None,
		};
		(global, topology, side(left, 7), side(right, 0))
	}

	/// Literal format bytes protect optional flags independently of round
	/// trips.
	#[test]
	fn live_edge_constructor_preserves_scoped_optional_fields() {
		use crate::storage::{
			graph_pages::{initialize_edge_hot_page, read_edge_hot_slot, write_edge_hot_slot},
			page::{AlignedPage, PAGE_HEADER_SIZE, PageLsn},
		};
		let (global, topology, source, target) = scoped_sides();
		let overlay = PropertyRef::inline(PageNo::new(4), 1, 80).assert_ok();
		let slot = EdgeHotSlot::live(
			9,
			global,
			&topology,
			(&source, &target),
			true,
			Some(2),
			Some(1.25),
			overlay,
			Some(11),
		)
		.assert_ok();
		assert_eq!(
			slot.endpoint_graph_slot(crate::storage::graph_pages::EndpointSide::Source),
			Some(7)
		);
		assert_eq!(
			slot.endpoint_graph_slot(crate::storage::graph_pages::EndpointSide::Target),
			Some(0)
		);
		let mut page = Box::new(AlignedPage::zeroed());
		initialize_edge_hot_page(&mut page, PageNo::new(0), PageLsn::new(1)).assert_ok();
		write_edge_hot_slot(&mut page, 0, &slot).assert_ok();
		page.seal().assert_ok();
		let bytes = &page.as_bytes()[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 160];
		assert_eq!(&bytes[..4], &[0, 0xc4, 1, 0]);
		assert_eq!(&bytes[4..8], &9_u32.to_le_bytes());
		assert_eq!(&bytes[16..20], &2_u32.to_le_bytes());
		assert_eq!(&bytes[24..32], &1.25_f64.to_bits().to_le_bytes());
		assert_eq!(&bytes[32..36], &11_u32.to_le_bytes());
		assert_eq!(&bytes[36..40], &7_u32.to_le_bytes());
		assert_eq!(&bytes[40..44], &0_u32.to_le_bytes());
		assert_eq!(&bytes[44..48], &1_u32.to_le_bytes());
		assert_eq!(&bytes[48..52], &80_u32.to_le_bytes());
		assert_eq!(&bytes[56..64], &4_u64.to_le_bytes());
		assert_eq!(&bytes[80..88], &global.raw().to_le_bytes());
		assert_eq!(read_edge_hot_slot(&page, PageNo::new(0), 0).assert_ok(), slot);
	}

	/// Qualified sides cannot mix ordinary, inline and whole-side addresses.
	#[test]
	fn live_edge_constructor_rejects_conflicting_side_addressing() {
		for (cross, local_overflow, graph_slot, graph_overflow) in [
			(false, None, Some(7), None),
			(true, Some(PageNo::new(1)), Some(7), None),
			(true, None, Some(7), Some(PageNo::new(1))),
			(true, None, None, None),
		] {
			let (global, topology, mut source, target) = scoped_sides();
			source.local_overflow = local_overflow;
			source.graph_slot = graph_slot;
			source.graph_overflow = graph_overflow;
			EdgeHotSlot::live(
				9,
				global,
				&topology,
				(&source, &target),
				cross,
				None,
				None,
				PropertyRef::None,
				None,
			)
			.assert_err();
			EdgeHotSlot::plain(9, global, &topology, (None, None), None).assert_err();
		}
	}

	/// A failed label comparison or reserved sentinel preserves either slot.
	#[test]
	fn hot_label_replacement_preserves_source_on_error() {
		let page = PageNo::new(0);
		let mut node = NodeHotSlot::tombstone(1);
		let mut edge = EdgeHotSlot::tombstone(1);
		for (expected, replacement) in [(Some(1), Some(2)), (None, Some(u32::MAX))] {
			let source = node.clone();
			node.replace_label(page, expected, replacement).assert_err();
			assert_eq!(node, source);
			let source = edge.clone();
			edge.replace_label(page, expected, replacement).assert_err();
			assert_eq!(edge, source);
		}
		node.replace_label(page, None, Some(0)).assert_ok();
		edge.replace_label(page, None, Some(0)).assert_ok();
		node.replace_label(page, Some(0), None).assert_ok();
		edge.replace_label(page, Some(0), None).assert_ok();
		assert_eq!(node.label_id, None);
		assert_eq!(edge.label_id, None);
	}

	/// Transition mutation preserves roles and synchronizes both META fields.
	#[test]
	fn edge_child_replacement_preserves_source_on_error_and_other_metadata() {
		let (global, topology, source, target) = scoped_sides();
		let mut slot = EdgeHotSlot::live(
			9,
			global,
			&topology,
			(&source, &target),
			true,
			Some(2),
			Some(1.25),
			PropertyRef::None,
			None,
		)
		.assert_ok();
		let original = slot.clone();
		for (expected, replacement) in [(Some(1), Some(2)), (None, Some(0)), (None, Some(u32::MAX))]
		{
			slot.replace_transition(PageNo::new(0), expected, replacement).assert_err();
			assert_eq!(slot, original);
		}
		slot.replace_transition(PageNo::new(0), None, Some(1)).assert_ok();
		let mut expected = original.clone();
		expected.transition_subgraph_slot = Some(1);
		expected.has_transition = true;
		expected.flags.set(EdgeSlotFlags::META, true);
		assert_eq!(slot, expected);
		slot.replace_transition(PageNo::new(0), Some(1), None).assert_ok();
		assert_eq!(slot, original);
		let mut tombstone = EdgeHotSlot::tombstone(9);
		let old = tombstone.clone();
		tombstone.replace_transition(PageNo::new(0), None, Some(1)).assert_err();
		assert_eq!(tombstone, old);
	}

	/// Both a wrong expected value and every non-finite weight are rejected.
	#[test]
	fn edge_weight_replacement_preserves_source_on_error() {
		let mut slot = EdgeHotSlot::tombstone(1);
		for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
			let source = slot.clone();
			slot.replace_weight(PageNo::new(0), None, Some(value)).assert_err();
			assert_eq!(slot, source);
		}
		let source = slot.clone();
		slot.replace_weight(PageNo::new(0), Some(1.0), Some(2.0)).assert_err();
		assert_eq!(slot, source);
		slot.replace_weight(PageNo::new(0), None, Some(-2.0)).assert_ok();
		assert_eq!(slot.weight(), Some(-2.0));
		slot.replace_weight(PageNo::new(0), Some(-2.0), None).assert_ok();
		assert_eq!(slot.weight(), None);
	}
}
