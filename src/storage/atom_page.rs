//! Database-global fixed atom-slot page codec.

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	datastore::{ClassId, GlobalAtomId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind},
};

/// Bytes occupied by one persisted global atom slot.
const ATOM_SLOT_BYTES: usize = 48;
/// Number of global atom slots in one page.
pub(super) const ATOM_SLOTS_PER_PAGE: u64 = 340;
/// First byte after the fixed slot array.
const ATOM_PADDING_OFFSET: usize = PAGE_HEADER_SIZE + ATOM_SLOT_BYTES * 340;
/// Null property-page sentinel.
const NULL_PAGE: u64 = u64::MAX;
/// Null property-slot sentinel.
const NULL_SLOT: u32 = u32::MAX;
/// Large-object property-slot sentinel.
const LARGE_SLOT: u32 = u32::MAX - 1;
/// Largest inline slot-directory index.
const MAX_INLINE_SLOT: u32 = u16::MAX as u32;
/// Null class sentinel.
const NULL_CLASS: u32 = u32::MAX;
/// Tombstone bit in `AtomHotSlot.flags`.
const TOMBSTONE: u8 = 1 << 0;
/// Edge-kind bit in `AtomHotSlot.flags`.
const EDGE: u8 = 1 << 1;
/// Complete assigned atom-slot flag set.
const ALLOWED_FLAGS: u8 = TOMBSTONE | EDGE;

/// Canonical reference to one inline or large record in `properties.fgb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PropertyRef {
	/// No record exists.
	None,
	/// Record lives in one slotted heap page.
	Inline {
		/// Owning heap page.
		page: PageNo,
		/// Slot-directory index.
		slot: u16,
		/// Exact canonical byte length.
		length: u32,
	},
	/// Record occupies a large-object page chain.
	Large {
		/// First page of the chain.
		first_page: PageNo,
		/// Exact canonical byte length.
		length: u32,
	},
}

impl PropertyRef {
	/// Builds an inline non-empty record reference.
	pub(super) fn inline(page: PageNo, slot: u16, length: u32) -> FerrosGrynnResult<Self> {
		if page.get() == NULL_PAGE || length == 0 {
			return Err(invalid_atom_page(page, "inline record reference is empty or null"));
		}
		Ok(Self::Inline {
			page,
			slot,
			length,
		})
	}

	/// Builds a non-empty large-object reference.
	pub(super) fn large(first_page: PageNo, length: u32) -> FerrosGrynnResult<Self> {
		if first_page.get() == NULL_PAGE || length == 0 {
			return Err(invalid_atom_page(first_page, "large record reference is empty or null"));
		}
		Ok(Self::Large {
			first_page,
			length,
		})
	}

	/// Converts a typed property reference to its three fixed slot fields.
	pub(super) fn fields(self) -> (u64, u32, u32) {
		match self {
			Self::None => (NULL_PAGE, NULL_SLOT, 0),
			Self::Inline {
				page,
				slot,
				length,
			} => (page.get(), u32::from(slot), length),
			Self::Large {
				first_page,
				length,
			} => (first_page.get(), LARGE_SLOT, length),
		}
	}

	/// Decodes one property reference while enforcing null and large sentinels.
	pub(super) fn decode(
		page: u64,
		slot: u32,
		length: u32,
		field: &str,
	) -> FerrosGrynnResult<Self> {
		match (page, slot, length) {
			(NULL_PAGE, NULL_SLOT, 0) => Ok(Self::None),
			(NULL_PAGE, _, _) | (_, NULL_SLOT, _) | (_, _, 0) => Err(invalid_atom_page(
				PageNo::new(page),
				format!("{field} record reference has inconsistent sentinels"),
			)),
			(_, LARGE_SLOT, length) => Ok(Self::Large {
				first_page: PageNo::new(page),
				length,
			}),
			(_, slot @ 0..=MAX_INLINE_SLOT, length) => Ok(Self::Inline {
				page: PageNo::new(page),
				slot: slot as u16,
				length,
			}),
			_ => Err(invalid_atom_page(
				PageNo::new(page),
				format!("{field} record slot uses a reserved value"),
			)),
		}
	}
}

/// Decoded state of one 48-byte global atom slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AtomHotSlot {
	/// Live global atom base record.
	Live {
		/// Immutable node or edge kind.
		kind: AtomKind,
		/// Stable owning class.
		class_id: ClassId,
		/// Required canonical `RecordId` bytes.
		record_id: PropertyRef,
		/// Optional base `PropertyMap` bytes.
		properties: PropertyRef,
	},
	/// Reusable or retired address carrying only its next generation.
	Tombstone,
}

impl AtomHotSlot {
	/// Creates a live slot after validating its mandatory record reference.
	pub(super) fn live(
		kind: AtomKind,
		class_id: ClassId,
		record_id: PropertyRef,
		properties: PropertyRef,
	) -> FerrosGrynnResult<Self> {
		if record_id == PropertyRef::None {
			return Err(invalid_atom_page(
				PageNo::new(0),
				"live atom slot requires canonical RecordId bytes",
			));
		}
		Ok(Self::Live {
			kind,
			class_id,
			record_id,
			properties,
		})
	}
}

/// Mutable builder and reader for one canonical `AtomHotPage`.
pub(super) struct AtomHotPage {
	/// Owned aligned bytes for this page.
	page: Box<AlignedPage>,
}

impl AtomHotPage {
	/// Creates a page whose unused slots are canonical generation-zero
	/// tombstones.
	pub(super) fn new(page_no: PageNo, page_lsn: PageLsn) -> FerrosGrynnResult<Self> {
		let mut result = Self {
			page: Box::new(AlignedPage::new(
				PageType::AtomHot,
				PageFlags::empty(),
				page_lsn,
				page_no,
			)),
		};
		for slot in 0..ATOM_SLOTS_PER_PAGE {
			result.encode_slot(slot, 0, AtomHotSlot::Tombstone)?;
		}
		result.page.seal()?;
		Ok(result)
	}

	/// Opens and validates one complete atom page without copying it.
	pub(super) fn from_page(page: Box<AlignedPage>, page_no: PageNo) -> FerrosGrynnResult<Self> {
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::AtomHot || !header.flags().is_empty() {
			return Err(invalid_atom_page(page_no, "expected an unflagged AtomHot page"));
		}
		if page.as_bytes()[ATOM_PADDING_OFFSET..].iter().any(|byte| *byte != 0) {
			return Err(invalid_atom_page(page_no, "atom hot-page padding is non-zero"));
		}
		let result = Self {
			page,
		};
		for slot in 0..ATOM_SLOTS_PER_PAGE {
			result.decode_slot(slot)?;
		}
		Ok(result)
	}

	/// Writes the slot addressed by an exact generation-aware global identity.
	pub(super) fn write(&mut self, id: GlobalAtomId, slot: AtomHotSlot) -> FerrosGrynnResult<()> {
		let (page_no, slot_index) = atom_location(id.slot());
		let actual_page = self.page.validate(page_no)?.page_no();
		if actual_page != page_no {
			return Err(invalid_atom_page(
				actual_page,
				"global atom belongs to a different hot page",
			));
		}
		if let AtomHotSlot::Live {
			kind,
			..
		} = slot && id.kind() != Some(kind)
		{
			return Err(invalid_atom_page(page_no, "global identity and slot kind disagree"));
		}
		self.encode_slot(slot_index, id.generation(), slot)?;
		self.page.seal()
	}

	/// Decodes the selected slot and reconstructs its generation-aware
	/// identity.
	pub(super) fn read(
		&self,
		slot_within_page: u64,
	) -> FerrosGrynnResult<(GlobalAtomId, AtomHotSlot)> {
		let (generation, slot) = self.decode_slot(slot_within_page)?;
		let page_no =
			self.page.validate(PageNo::new(read_u64(self.page.as_bytes(), 24)))?.page_no();
		let global_slot = page_no
			.get()
			.checked_mul(ATOM_SLOTS_PER_PAGE)
			.and_then(|base| base.checked_add(slot_within_page))
			.ok_or_else(|| invalid_atom_page(page_no, "global atom slot overflows u64"))?;
		let kind = match slot {
			AtomHotSlot::Live {
				kind,
				..
			} => kind,
			AtomHotSlot::Tombstone => AtomKind::Node,
		};
		let atom = AtomId::from_parts(kind, global_slot, generation)?;
		Ok((GlobalAtomId::from_raw(atom.raw()), slot))
	}

	/// Returns the complete sealed page.
	pub(super) fn finish(self) -> Box<AlignedPage> {
		self.page
	}

	/// Encodes one checked slot without sealing the containing page.
	fn encode_slot(
		&mut self,
		slot: u64,
		generation: u32,
		value: AtomHotSlot,
	) -> FerrosGrynnResult<()> {
		let offset = slot_offset(slot)?;
		let bytes = self.page.as_bytes_mut();
		bytes[offset..offset + ATOM_SLOT_BYTES].fill(0);
		write_u32(bytes, offset, generation);
		match value {
			AtomHotSlot::Tombstone => {
				write_u32(bytes, offset + 4, NULL_CLASS);
				bytes[offset + 8] = TOMBSTONE;
				write_ref(bytes, offset + 16, PropertyRef::None);
				write_ref(bytes, offset + 32, PropertyRef::None);
			}
			AtomHotSlot::Live {
				kind,
				class_id,
				record_id,
				properties,
			} => {
				write_u32(bytes, offset + 4, class_id.get());
				bytes[offset + 8] = if kind == AtomKind::Edge {
					EDGE
				} else {
					0
				};
				write_ref(bytes, offset + 16, record_id);
				write_ref(bytes, offset + 32, properties);
			}
		}
		Ok(())
	}

	/// Decodes and validates one fixed slot.
	fn decode_slot(&self, slot: u64) -> FerrosGrynnResult<(u32, AtomHotSlot)> {
		let offset = slot_offset(slot)?;
		let bytes = self.page.as_bytes();
		let generation = read_u32(bytes, offset);
		let class_id = read_u32(bytes, offset + 4);
		let flags = bytes[offset + 8];
		if flags & !ALLOWED_FLAGS != 0
			|| bytes[offset + 9..offset + 16].iter().any(|byte| *byte != 0)
		{
			return Err(invalid_atom_page(
				self.page_no(),
				"atom slot contains unknown flags or non-zero reserved bytes",
			));
		}
		let record = read_ref(bytes, offset + 16, "RecordId")?;
		let properties = read_ref(bytes, offset + 32, "properties")?;
		if flags == TOMBSTONE {
			if class_id != NULL_CLASS
				|| record != PropertyRef::None
				|| properties != PropertyRef::None
			{
				return Err(invalid_atom_page(
					self.page_no(),
					"atom tombstone contains live fields",
				));
			}
			return Ok((generation, AtomHotSlot::Tombstone));
		}
		if flags & TOMBSTONE != 0 || record == PropertyRef::None {
			return Err(invalid_atom_page(
				self.page_no(),
				"live atom slot has invalid flags or no RecordId",
			));
		}
		let class_id = ClassId::from_storage(class_id)?;
		let kind = if flags & EDGE == 0 {
			AtomKind::Node
		} else {
			AtomKind::Edge
		};
		Ok((generation, AtomHotSlot::Live {
			kind,
			class_id,
			record_id: record,
			properties,
		}))
	}

	/// Returns the already validated page number stored in the header.
	fn page_no(&self) -> PageNo {
		PageNo::new(read_u64(self.page.as_bytes(), 24))
	}
}

/// Computes the owning page and within-page slot for one global slot index.
pub(super) const fn atom_location(global_slot: u64) -> (PageNo, u64) {
	(PageNo::new(global_slot / ATOM_SLOTS_PER_PAGE), global_slot % ATOM_SLOTS_PER_PAGE)
}

/// Converts a checked within-page slot to its byte offset.
fn slot_offset(slot: u64) -> FerrosGrynnResult<usize> {
	if slot >= ATOM_SLOTS_PER_PAGE {
		return Err(invalid_atom_page(PageNo::new(0), "atom slot is outside its page"));
	}
	let slot = usize::try_from(slot)
		.map_err(|_error| invalid_atom_page(PageNo::new(0), "atom slot exceeds usize"))?;
	Ok(PAGE_HEADER_SIZE + slot * ATOM_SLOT_BYTES)
}

/// Writes one fixed property reference at a format-defined offset.
fn write_ref(bytes: &mut [u8; PAGE_SIZE], offset: usize, reference: PropertyRef) {
	let (page, slot, length) = reference.fields();
	write_u64(bytes, offset, page);
	write_u32(bytes, offset + 8, slot);
	write_u32(bytes, offset + 12, length);
}

/// Reads one fixed property reference from a format-defined offset.
fn read_ref(bytes: &[u8; PAGE_SIZE], offset: usize, field: &str) -> FerrosGrynnResult<PropertyRef> {
	PropertyRef::decode(
		read_u64(bytes, offset),
		read_u32(bytes, offset + 8),
		read_u32(bytes, offset + 12),
		field,
	)
}

/// Writes one little-endian `u32` at an exact slot offset.
fn write_u32(bytes: &mut [u8; PAGE_SIZE], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u64` at an exact slot offset.
fn write_u64(bytes: &mut [u8; PAGE_SIZE], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Reads one little-endian `u32` from an exact slot field.
fn read_u32(bytes: &[u8; PAGE_SIZE], offset: usize) -> u32 {
	let mut encoded = [0; 4];
	encoded.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(encoded)
}

/// Reads one little-endian `u64` from an exact slot field.
fn read_u64(bytes: &[u8; PAGE_SIZE], offset: usize) -> u64 {
	let mut encoded = [0; 8];
	encoded.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(encoded)
}

/// Creates a typed page-corruption error for this codec.
fn invalid_atom_page(page: PageNo, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(page.get(), reason)
}

const _: () = {
	assert!(ATOM_PADDING_OFFSET == 16_352);
	assert!(ATOM_PADDING_OFFSET + 32 == PAGE_SIZE);
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn atom_hot_page_round_trips_live_slots_and_canonical_tombstones() -> FerrosGrynnResult<()> {
		let id = AtomId::from_parts(AtomKind::Edge, 341, 7)?;
		let id = GlobalAtomId::from_raw(id.raw());
		let mut page = AtomHotPage::new(PageNo::new(1), PageLsn::new(9))?;
		page.write(
			id,
			AtomHotSlot::live(
				AtomKind::Edge,
				ClassId::new(11).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("test ClassId is invalid")
				})?,
				PropertyRef::inline(PageNo::new(3), 4, 21)?,
				PropertyRef::large(PageNo::new(5), 9_000)?,
			)?,
		)?;

		let page = AtomHotPage::from_page(page.finish(), PageNo::new(1))?;
		let (decoded_id, decoded) = page.read(1)?;
		assert_eq!(decoded_id, id);
		assert!(matches!(decoded, AtomHotSlot::Live {
			kind: AtomKind::Edge,
			..
		}));
		assert!(matches!(page.read(0)?.1, AtomHotSlot::Tombstone));
		Ok(())
	}

	#[test]
	fn atom_hot_page_rejects_noncanonical_tombstone() -> FerrosGrynnResult<()> {
		let page = AtomHotPage::new(PageNo::new(0), PageLsn::new(0))?.finish();
		let mut bytes = *page.as_bytes();
		bytes[PAGE_HEADER_SIZE + 4..PAGE_HEADER_SIZE + 8].copy_from_slice(&1_u32.to_le_bytes());
		let mut corrupt = Box::new(AlignedPage::zeroed());
		corrupt.as_bytes_mut().copy_from_slice(&bytes);
		corrupt.seal()?;

		assert!(AtomHotPage::from_page(corrupt, PageNo::new(0)).is_err());
		Ok(())
	}
}
