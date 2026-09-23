//! Exact format-v1 codec for database-global AtomStore metadata.

use super::{
	atom_page::ATOM_SLOTS_PER_PAGE,
	page::{
		AlignedPage,
		PAGE_HEADER_SIZE,
		PageFlags,
		PageLsn,
		PageNo,
		PageType,
		STORAGE_FORMAT_VERSION,
	},
	types::{CheckpointGeneration, Lsn},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Exact AtomStore superblock format version.
const FORMAT_VERSION: u32 = STORAGE_FORMAT_VERSION as u32;
/// Oldest format version readable by this implementation.
const LAST_BACK_COMPAT_VERSION: u32 = FORMAT_VERSION;
/// Fixed physical page number of the AtomStore superblock.
const SUPERBLOCK_PAGE_NO: PageNo = PageNo::new(0);
/// Byte offset of the database-global data-space identity.
const DATA_SPACE_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of the format version.
const FORMAT_VERSION_OFFSET: usize = DATA_SPACE_OFFSET + 8;
/// Byte offset of the oldest compatible format version.
const BACK_COMPAT_VERSION_OFFSET: usize = FORMAT_VERSION_OFFSET + 4;
/// Byte offset of the checkpoint generation.
const CHECKPOINT_GENERATION_OFFSET: usize = BACK_COMPAT_VERSION_OFFSET + 4;
/// Byte offset of the backfilled WAL boundary.
const BACKFILLED_LSN_OFFSET: usize = CHECKPOINT_GENERATION_OFFSET + 8;
/// Byte offset of the matching checkpoint record LSN.
const CHECKPOINT_RECORD_LSN_OFFSET: usize = BACKFILLED_LSN_OFFSET + 8;
/// Byte offset of the allocated atom-slot capacity.
const ATOM_CAPACITY_OFFSET: usize = CHECKPOINT_RECORD_LSN_OFFSET + 8;
/// Byte offset of the live node count.
const NODE_COUNT_OFFSET: usize = ATOM_CAPACITY_OFFSET + 8;
/// Byte offset of the live edge count.
const EDGE_COUNT_OFFSET: usize = NODE_COUNT_OFFSET + 8;
/// Byte offset of the `atoms_hot.fgb` page ceiling.
const ATOMS_HOT_PAGES_OFFSET: usize = EDGE_COUNT_OFFSET + 8;
/// Byte offset of the `properties.fgb` page ceiling.
const PROPERTIES_PAGES_OFFSET: usize = ATOMS_HOT_PAGES_OFFSET + 8;
/// Byte offset of the `property_keys.fgb` page ceiling.
const PROPERTY_DICTIONARY_PAGES_OFFSET: usize = PROPERTIES_PAGES_OFFSET + 8;
/// First byte that must remain zero in format v1.
const RESERVED_OFFSET: usize = PROPERTY_DICTIONARY_PAGES_OFFSET + 8;
/// Reserved all-ones slot value that no live global atom may use.
const NULL_ATOM_SLOT: u64 = (1_u64 << 40) - 1;

/// Snapshot-visible metadata for the database-global AtomStore files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AtomStoreSuperblock {
	/// LSN of the last logical superblock page frame.
	pub(super) page_lsn: PageLsn,
	/// Checkpoint generation whose home files satisfy the published ceilings.
	pub(super) checkpoint_generation: CheckpointGeneration,
	/// Highest commit boundary copied to AtomStore home files.
	pub(super) backfilled_lsn: Lsn,
	/// Matching durable `CheckpointEnd` record.
	pub(super) checkpoint_record_lsn: Lsn,
	/// Allocated global atom slots.
	pub(super) atom_capacity: u64,
	/// Live node atoms.
	pub(super) node_count: u64,
	/// Live edge atoms.
	pub(super) edge_count: u64,
	/// Snapshot-visible `atoms_hot.fgb` page ceiling.
	pub(super) atoms_hot_pages: u64,
	/// Snapshot-visible `properties.fgb` page ceiling.
	pub(super) properties_pages: u64,
	/// Snapshot-visible `property_keys.fgb` page ceiling.
	pub(super) property_dictionary_pages: u64,
}

impl AtomStoreSuperblock {
	/// Encodes one complete checksummed logical superblock page.
	///
	/// # Errors
	///
	/// Returns an error when counts, capacities, page ceilings, or checkpoint
	/// clocks violate format-v1 invariants.
	pub(super) fn encode(self) -> FerrosGrynnResult<AlignedPage> {
		self.validate_semantics()?;
		let mut page = AlignedPage::new(
			PageType::AtomStoreSuperblock,
			PageFlags::empty(),
			self.page_lsn,
			SUPERBLOCK_PAGE_NO,
		);
		let bytes = page.as_bytes_mut();
		write_u64(bytes, DATA_SPACE_OFFSET, 0);
		write_u32(bytes, FORMAT_VERSION_OFFSET, FORMAT_VERSION);
		write_u32(bytes, BACK_COMPAT_VERSION_OFFSET, LAST_BACK_COMPAT_VERSION);
		write_u64(bytes, CHECKPOINT_GENERATION_OFFSET, self.checkpoint_generation.get());
		write_u64(bytes, BACKFILLED_LSN_OFFSET, self.backfilled_lsn.get());
		write_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET, self.checkpoint_record_lsn.get());
		write_u64(bytes, ATOM_CAPACITY_OFFSET, self.atom_capacity);
		write_u64(bytes, NODE_COUNT_OFFSET, self.node_count);
		write_u64(bytes, EDGE_COUNT_OFFSET, self.edge_count);
		write_u64(bytes, ATOMS_HOT_PAGES_OFFSET, self.atoms_hot_pages);
		write_u64(bytes, PROPERTIES_PAGES_OFFSET, self.properties_pages);
		write_u64(bytes, PROPERTY_DICTIONARY_PAGES_OFFSET, self.property_dictionary_pages);
		page.seal()?;
		Ok(page)
	}

	/// Decodes and validates one exact AtomStore superblock page.
	///
	/// # Errors
	///
	/// Returns an error for a malformed page header, incompatible version,
	/// non-zero reserved byte, or invalid metadata relation.
	pub(super) fn decode(page: &AlignedPage) -> FerrosGrynnResult<Self> {
		let header = page.validate(SUPERBLOCK_PAGE_NO)?;
		if header.page_type() != PageType::AtomStoreSuperblock || !header.flags().is_empty() {
			return Err(invalid("invalid AtomStoreSuperblock page type or flags"));
		}
		let bytes = page.as_bytes();
		if read_u64(bytes, DATA_SPACE_OFFSET) != 0 {
			return Err(invalid("AtomStoreSuperblock data_space_id is not database-global"));
		}
		let format = read_u32(bytes, FORMAT_VERSION_OFFSET);
		let back_compat = read_u32(bytes, BACK_COMPAT_VERSION_OFFSET);
		if format != FORMAT_VERSION || back_compat != LAST_BACK_COMPAT_VERSION {
			return Err(FerrosGrynnError::unsupported_storage_format(
				u64::from(format),
				u64::from(FORMAT_VERSION),
			));
		}
		if bytes[RESERVED_OFFSET..].iter().any(|byte| *byte != 0) {
			return Err(invalid("AtomStoreSuperblock reserved bytes are non-zero"));
		}
		let value = Self {
			page_lsn: header.page_lsn(),
			checkpoint_generation: CheckpointGeneration::new(read_u64(
				bytes,
				CHECKPOINT_GENERATION_OFFSET,
			)),
			backfilled_lsn: Lsn::new(read_u64(bytes, BACKFILLED_LSN_OFFSET)),
			checkpoint_record_lsn: Lsn::new(read_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET)),
			atom_capacity: read_u64(bytes, ATOM_CAPACITY_OFFSET),
			node_count: read_u64(bytes, NODE_COUNT_OFFSET),
			edge_count: read_u64(bytes, EDGE_COUNT_OFFSET),
			atoms_hot_pages: read_u64(bytes, ATOMS_HOT_PAGES_OFFSET),
			properties_pages: read_u64(bytes, PROPERTIES_PAGES_OFFSET),
			property_dictionary_pages: read_u64(bytes, PROPERTY_DICTIONARY_PAGES_OFFSET),
		};
		value.validate_semantics()?;
		Ok(value)
	}

	/// Returns the same logical version with a newly completed checkpoint
	/// tuple.
	///
	/// The page-frame LSN is deliberately preserved.
	pub(super) fn at_checkpoint(
		mut self,
		generation: CheckpointGeneration,
		backfilled_lsn: Lsn,
		checkpoint_record_lsn: Lsn,
	) -> FerrosGrynnResult<Self> {
		self.checkpoint_generation = generation;
		self.backfilled_lsn = backfilled_lsn;
		self.checkpoint_record_lsn = checkpoint_record_lsn;
		self.validate_semantics()?;
		if self.page_lsn.get() > self.backfilled_lsn.get() {
			return Err(invalid(
				"checkpointed AtomStore logical page LSN exceeds its backfill boundary",
			));
		}
		Ok(self)
	}

	/// Returns the number of live atoms declared by this metadata version.
	pub(super) fn live_count(self) -> FerrosGrynnResult<u64> {
		self.node_count
			.checked_add(self.edge_count)
			.ok_or_else(|| invalid("AtomStore live count overflows u64"))
	}

	/// Compares transaction-visible metadata while ignoring the WAL-assigned
	/// page-frame LSN.
	pub(super) fn same_logical_metadata(self, other: Self) -> bool {
		Self {
			page_lsn: PageLsn::ZERO,
			..self
		} == Self {
			page_lsn: PageLsn::ZERO,
			..other
		}
	}

	/// Validates relations between counts, page ceilings, and checkpoint
	/// clocks.
	fn validate_semantics(self) -> FerrosGrynnResult<()> {
		let live_count = self.live_count()?;
		if live_count > self.atom_capacity || self.atom_capacity > NULL_ATOM_SLOT {
			return Err(invalid("AtomStore counts or capacity exceed their format domain"));
		}
		let expected_hot_pages = self.atom_capacity.div_ceil(ATOM_SLOTS_PER_PAGE);
		if self.atoms_hot_pages != expected_hot_pages {
			return Err(invalid("AtomStore hot-page ceiling disagrees with atom capacity"));
		}
		if self.property_dictionary_pages == 0 {
			return Err(invalid("AtomStore property dictionary must contain its root page"));
		}
		if self.checkpoint_generation == CheckpointGeneration::ZERO {
			if self.backfilled_lsn != Lsn::ZERO || self.checkpoint_record_lsn != Lsn::ZERO {
				return Err(invalid("generation zero requires zero checkpoint clocks"));
			}
		} else if self.backfilled_lsn == Lsn::ZERO
			|| self.checkpoint_record_lsn == Lsn::ZERO
			|| self.backfilled_lsn > self.checkpoint_record_lsn
		{
			return Err(invalid("AtomStore checkpoint tuple is inconsistent"));
		}
		Ok(())
	}
}

/// Writes one little-endian `u32` at an already validated format offset.
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u64` at an already validated format offset.
fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Reads one little-endian `u32` from an exact superblock field.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64` from an exact superblock field.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

/// Creates the typed corruption error used by this codec.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn fixture() -> AtomStoreSuperblock {
		AtomStoreSuperblock {
			page_lsn: PageLsn::new(11),
			checkpoint_generation: CheckpointGeneration::new(3),
			backfilled_lsn: Lsn::new(17),
			checkpoint_record_lsn: Lsn::new(19),
			atom_capacity: 341,
			node_count: 7,
			edge_count: 5,
			atoms_hot_pages: 2,
			properties_pages: 4,
			property_dictionary_pages: 3,
		}
	}

	#[test]
	fn exact_offsets_round_trip_and_preserve_logical_lsn() -> FerrosGrynnResult<()> {
		let value = fixture();
		let page = value.encode()?;
		let bytes = page.as_bytes();
		assert_eq!(read_u64(bytes, DATA_SPACE_OFFSET), 0);
		assert_eq!(read_u32(bytes, FORMAT_VERSION_OFFSET), 1);
		assert_eq!(read_u32(bytes, BACK_COMPAT_VERSION_OFFSET), 1);
		assert_eq!(read_u64(bytes, CHECKPOINT_GENERATION_OFFSET), 3);
		assert_eq!(read_u64(bytes, BACKFILLED_LSN_OFFSET), 17);
		assert_eq!(read_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET), 19);
		assert_eq!(read_u64(bytes, ATOM_CAPACITY_OFFSET), 341);
		assert_eq!(read_u64(bytes, NODE_COUNT_OFFSET), 7);
		assert_eq!(read_u64(bytes, EDGE_COUNT_OFFSET), 5);
		assert_eq!(read_u64(bytes, ATOMS_HOT_PAGES_OFFSET), 2);
		assert_eq!(read_u64(bytes, PROPERTIES_PAGES_OFFSET), 4);
		assert_eq!(read_u64(bytes, PROPERTY_DICTIONARY_PAGES_OFFSET), 3);
		assert_eq!(AtomStoreSuperblock::decode(&page)?, value);
		let checkpointed =
			value.at_checkpoint(CheckpointGeneration::new(4), Lsn::new(23), Lsn::new(25))?;
		assert_eq!(checkpointed.page_lsn, value.page_lsn);
		Ok(())
	}

	#[test]
	fn rejects_wrong_header_version_reserved_checksum_and_legacy_graph_page()
	-> FerrosGrynnResult<()> {
		let value = fixture();
		let mut wrong_flags = value.encode()?;
		wrong_flags.reset(
			PageType::AtomStoreSuperblock,
			PageFlags::DIRTY,
			value.page_lsn,
			SUPERBLOCK_PAGE_NO,
		);
		assert!(AtomStoreSuperblock::decode(&wrong_flags).is_err());

		let mut wrong_version = value.encode()?;
		write_u32(wrong_version.as_bytes_mut(), FORMAT_VERSION_OFFSET, 2);
		wrong_version.seal()?;
		assert!(AtomStoreSuperblock::decode(&wrong_version).is_err());

		let mut reserved = value.encode()?;
		reserved.as_bytes_mut()[RESERVED_OFFSET] = 1;
		reserved.seal()?;
		assert!(AtomStoreSuperblock::decode(&reserved).is_err());

		let mut checksum = value.encode()?;
		checksum.as_bytes_mut()[NODE_COUNT_OFFSET] ^= 1;
		assert!(AtomStoreSuperblock::decode(&checksum).is_err());

		let graph = AlignedPage::new(
			PageType::GraphSuperblock,
			PageFlags::empty(),
			PageLsn::ZERO,
			SUPERBLOCK_PAGE_NO,
		);
		assert!(AtomStoreSuperblock::decode(&graph).is_err());
		Ok(())
	}

	#[test]
	fn rejects_invalid_counts_ceilings_and_checkpoint_tuple() {
		let mut value = fixture();
		value.node_count = value.atom_capacity;
		value.edge_count = 1;
		assert!(value.encode().is_err());
		value = fixture();
		value.atoms_hot_pages = 1;
		assert!(value.encode().is_err());
		value = fixture();
		value.property_dictionary_pages = 0;
		assert!(value.encode().is_err());
		value = fixture();
		value.backfilled_lsn = Lsn::new(20);
		value.checkpoint_record_lsn = Lsn::new(19);
		assert!(value.encode().is_err());
		value = fixture();
		assert!(
			value
				.at_checkpoint(
					CheckpointGeneration::new(4),
					Lsn::new(value.page_lsn.get() - 1),
					Lsn::new(value.page_lsn.get()),
				)
				.is_err()
		);
	}
}
