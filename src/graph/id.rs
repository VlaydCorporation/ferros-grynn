use std::{
	fmt::{self, Debug, Display, Formatter},
	num::NonZeroU64,
};

use crate::{
	collections::cow_vec::CowVec,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Bit offset of the node/edge discriminator above the 40-bit slot.
const KIND_SHIFT: u32 = 40;
/// Bit offset of the 23-bit generation above the slot and kind fields.
const GENERATION_SHIFT: u32 = 41;
/// Mask selecting the node/edge discriminator in a packed atom ID.
const KIND_BIT: u64 = 1 << KIND_SHIFT;
/// Mask selecting the materialization-local 40-bit atom slot.
const SLOT_MASK: u64 = KIND_BIT - 1;
/// Largest generation representable by the packed atom ID.
const GENERATION_MASK: u32 = (1 << 23) - 1;

/// Stable identity of one cataloged graph.
///
/// Zero addresses the database data space and is never a graph identity.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct GraphId(
	/// Nonzero durable graph identity; zero is reserved for the database data
	/// space.
	NonZeroU64,
);

impl GraphId {
	/// Creates a graph identity from its durable representation.
	pub const fn new(value: u64) -> Option<Self> {
		match NonZeroU64::new(value) {
			Some(value) => Some(Self(value)),
			None => None,
		}
	}

	/// Returns the durable integer representation.
	pub const fn get(self) -> u64 {
		self.0.get()
	}
}

impl Display for GraphId {
	/// Formats `GraphId` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		Display::fmt(&self.get(), formatter)
	}
}

/// Whether an atom handle denotes a node or an edge.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum AtomKind {
	/// Node atom rather than an edge atom.
	Node,
	/// Edge atom rather than a node atom.
	Edge,
}

impl Display for AtomKind {
	/// Formats `AtomKind` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter.write_str(match self {
			Self::Node => "node",
			Self::Edge => "edge",
		})
	}
}

/// A generation-aware in-memory atom handle.
///
/// Layout: `generation(23) | kind(1) | slot(40)`.
#[derive(Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct AtomId(
	/// Packed generation, node-or-edge kind and materialization-local slot
	/// bits.
	u64,
);

impl AtomId {
	/// Sentinel atom handle that does not identify a live node or edge.
	pub const NULL: Self = Self(u64::MAX);

	/// Constructs from parts for `AtomId`.
	///
	/// # Errors
	///
	/// Returns `graph_capacity_exceeded` diagnostics.
	pub fn from_parts(kind: AtomKind, slot: u64, generation: u32) -> FerrosGrynnResult<Self> {
		if slot > SLOT_MASK || generation > GENERATION_MASK {
			return Err(FerrosGrynnError::graph_capacity_exceeded(kind.to_string()));
		}
		Ok(Self::new(kind, slot, generation))
	}

	/// Constructs from raw for `AtomId`.
	pub const fn from_raw(value: u64) -> Self {
		Self(value)
	}

	/// Returns the wrapped raw representation.
	pub const fn raw(self) -> u64 {
		self.0
	}

	/// Reports whether the value is null for `AtomId`.
	pub const fn is_null(self) -> bool {
		self.0 == Self::NULL.0
	}

	/// Returns kind for `AtomId`.
	pub const fn kind(self) -> Option<AtomKind> {
		if self.is_null() {
			None
		} else if self.0 & KIND_BIT == 0 {
			Some(AtomKind::Node)
		} else {
			Some(AtomKind::Edge)
		}
	}

	/// Reports whether the value is node for `AtomId`.
	pub const fn is_node(self) -> bool {
		matches!(self.kind(), Some(AtomKind::Node))
	}

	/// Reports whether the value is edge for `AtomId`.
	pub const fn is_edge(self) -> bool {
		matches!(self.kind(), Some(AtomKind::Edge))
	}

	/// Returns slot for `AtomId`.
	pub const fn slot(self) -> u64 {
		self.0 & SLOT_MASK
	}

	/// Returns generation for `AtomId`.
	pub const fn generation(self) -> u32 {
		(self.0 >> GENERATION_SHIFT) as u32
	}

	/// Creates a new `AtomId` value.
	pub(crate) const fn new(kind: AtomKind, slot: u64, generation: u32) -> Self {
		let kind = match kind {
			AtomKind::Node => 0,
			AtomKind::Edge => KIND_BIT,
		};
		Self(((generation as u64) << GENERATION_SHIFT) | kind | slot)
	}
}

impl Debug for AtomId {
	/// Formats `AtomId` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self.kind() {
			Some(kind) => formatter
				.debug_struct("AtomId")
				.field("kind", &kind)
				.field("slot", &self.slot())
				.field("generation", &self.generation())
				.finish(),
			None => formatter.write_str("AtomId::NULL"),
		}
	}
}

impl Display for AtomId {
	/// Formats `AtomId` using the requested formatter.
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		match self.kind() {
			Some(kind) => write!(formatter, "{kind}:{}@{}", self.slot(), self.generation()),
			None => formatter.write_str("null"),
		}
	}
}

#[derive(Debug, Clone)]
/// Slot allocator state.
pub struct SlotAllocator {
	/// Kind associated with this slot allocator.
	kind: AtomKind,
	/// Generations associated with this slot allocator.
	generations: CowVec<u32>,
	/// Occupied associated with this slot allocator.
	occupied: CowVec<bool>,
	/// Free associated with this slot allocator.
	free: CowVec<u64>,
	/// Live associated with this slot allocator.
	live: usize,
}

impl SlotAllocator {
	/// Creates a new `SlotAllocator` value.
	pub(crate) fn new(kind: AtomKind) -> Self {
		Self {
			kind,
			generations: CowVec::new(),
			occupied: CowVec::new(),
			free: CowVec::new(),
			live: 0,
		}
	}

	/// Configures capacity.
	pub(crate) fn with_capacity(kind: AtomKind, capacity: usize) -> Self {
		Self {
			kind,
			generations: CowVec::with_capacity(capacity),
			occupied: CowVec::with_capacity(capacity),
			free: CowVec::new(),
			live: 0,
		}
	}

	/// Returns the alloc result.
	pub(crate) fn alloc(&mut self) -> FerrosGrynnResult<AtomId> {
		let slot = if let Some(slot) = self.free.pop() {
			slot
		} else {
			let slot = u64::try_from(self.generations.len()).map_err(|_conversion_error| {
				FerrosGrynnError::graph_capacity_exceeded(self.kind.to_string())
			})?;
			if slot > SLOT_MASK {
				return Err(FerrosGrynnError::graph_capacity_exceeded(self.kind.to_string()));
			}
			self.generations.push(0);
			self.occupied.push(false);
			slot
		};

		let index = slot as usize;
		self.occupied[index] = true;
		self.live += 1;
		Ok(AtomId::new(self.kind, slot, self.generations[index]))
	}

	/// Restores slot allocator.
	pub(crate) fn restore(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		if id.kind() != Some(self.kind) || id.slot() > SLOT_MASK {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				id.to_string(),
				id.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
				self.kind.to_string(),
			));
		}
		let index = usize::try_from(id.slot()).map_err(|_conversion_error| {
			FerrosGrynnError::graph_capacity_exceeded(self.kind.to_string())
		})?;
		while self.generations.len() <= index {
			let slot = self.generations.len() as u64;
			self.generations.push(0);
			self.occupied.push(false);
			self.free.push(slot);
		}
		if self.occupied[index] {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"graph",
				format!("atom slot {} is already occupied", id.slot()),
			));
		}
		self.free.retain(|slot| *slot != id.slot());
		self.generations[index] = id.generation();
		self.occupied[index] = true;
		self.live += 1;
		Ok(())
	}

	/// Restores free.
	pub(crate) fn restore_free(&mut self, slot: u64, generation: u32) -> FerrosGrynnResult<()> {
		if slot > SLOT_MASK || generation > GENERATION_MASK {
			return Err(FerrosGrynnError::graph_capacity_exceeded(self.kind.to_string()));
		}
		let index = usize::try_from(slot).map_err(|_conversion_error| {
			FerrosGrynnError::graph_capacity_exceeded(self.kind.to_string())
		})?;
		while self.generations.len() <= index {
			let free_slot = self.generations.len() as u64;
			self.generations.push(0);
			self.occupied.push(false);
			self.free.push(free_slot);
		}
		if self.occupied[index] {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"graph",
				format!("atom slot {slot} is occupied"),
			));
		}
		self.generations[index] = generation;
		self.free.retain(|free_slot| *free_slot != slot);
		if generation < GENERATION_MASK {
			self.free.push(slot);
		}
		Ok(())
	}

	/// Returns whether the free condition holds.
	pub(crate) fn free(&mut self, id: AtomId) -> bool {
		if !self.contains(id) {
			return false;
		}
		let index = id.slot() as usize;
		self.occupied[index] = false;
		self.live -= 1;
		if self.generations[index] < GENERATION_MASK {
			self.generations[index] += 1;
			self.free.push(id.slot());
		}
		true
	}

	/// Checks for slot allocator.
	pub(crate) fn contains(&self, id: AtomId) -> bool {
		if id.kind() != Some(self.kind) {
			return false;
		}
		let Ok(index) = usize::try_from(id.slot()) else {
			return false;
		};
		self.occupied.get(index).copied().unwrap_or(false)
			&& self.generations.get(index).copied() == Some(id.generation())
	}

	/// Returns the capacity result.
	pub(crate) fn capacity(&self) -> usize {
		self.generations.len()
	}

	/// Returns the generation result.
	pub(crate) fn generation(&self, slot: usize) -> Option<u32> {
		self.generations.get(slot).copied()
	}

	/// Returns the live len result.
	pub(crate) const fn live_len(&self) -> usize {
		self.live
	}
}

#[cfg(test)]
/// Regression tests for id behavior.
mod tests {
	use super::{AtomId, AtomKind, SlotAllocator};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies atom id uses u40 slots and u23 generations.
	#[test]
	fn atom_id_uses_u40_slots_and_u23_generations() {
		let slot = (1_u64 << 39) + 42;
		let generation = 7_u32;
		let node = AtomId::from_parts(AtomKind::Node, slot, generation).assert_ok();
		let edge = AtomId::from_parts(AtomKind::Edge, slot, generation).assert_ok();

		assert_eq!(node.kind(), Some(AtomKind::Node));
		assert_eq!(edge.kind(), Some(AtomKind::Edge));
		assert_eq!(node.slot(), slot);
		assert_eq!(edge.slot(), slot);
		assert_eq!(node.generation(), generation);
		assert_eq!(edge.generation(), generation);
	}

	/// Verifies slot reuse invalidates stale handles.
	#[test]
	fn slot_reuse_invalidates_stale_handles() {
		let mut allocator = SlotAllocator::new(AtomKind::Node);
		let old = allocator.alloc().assert_ok();
		assert!(allocator.free(old));
		let current = allocator.alloc().assert_ok();

		assert_eq!(old.slot(), current.slot());
		assert_ne!(old.generation(), current.generation());
		assert!(!allocator.contains(old));
		assert!(allocator.contains(current));
	}
}
