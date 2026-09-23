//! Exact checkpoint-owned database-superblock codec.

use super::{
	page::{
		AlignedPage,
		PAGE_HEADER_SIZE,
		PageFlags,
		PageLsn,
		PageNo,
		PageType,
		STORAGE_FORMAT_VERSION,
	},
	types::{CheckpointGeneration, CommitTs, Lsn, TxId},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

const FORMAT_VERSION: u32 = STORAGE_FORMAT_VERSION as u32;
const PAGE_NO: PageNo = PageNo::new(0);
const NAME_OFFSET: usize = PAGE_HEADER_SIZE;
const NAME_BYTES: usize = 32;
const FORMAT_OFFSET: usize = 64;
const COMPAT_OFFSET: usize = 68;
const GENERATION_OFFSET: usize = 72;
const CHECKPOINT_LSN_OFFSET: usize = 80;
const CHECKPOINT_RECORD_LSN_OFFSET: usize = 88;
const DURABLE_VISIBLE_LSN_OFFSET: usize = 96;
const NEXT_LSN_OFFSET: usize = 104;
const COMMIT_TS_OFFSET: usize = 112;
const NEXT_TX_ID_OFFSET: usize = 120;
const NEXT_FILE_ID_OFFSET: usize = 128;
const SYSTEM_BACKFILLED_LSN_OFFSET: usize = 136;
const DATABASE_CATALOG_ROOT_OFFSET: usize = 144;
const FILE_CATALOG_ROOT_OFFSET: usize = 152;
const GRAPH_COUNT_OFFSET: usize = 160;
const FLAGS_OFFSET: usize = 164;
const RESERVED_OFFSET: usize = 168;

/// Durable database-wide clocks and catalog roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DatabaseSuperblock {
	pub(super) name: String,
	pub(super) generation: CheckpointGeneration,
	pub(super) checkpoint_lsn: Lsn,
	pub(super) checkpoint_record_lsn: Lsn,
	pub(super) durable_visible_lsn: Lsn,
	pub(super) next_lsn: Lsn,
	pub(super) commit_ts: CommitTs,
	pub(super) next_tx_id: TxId,
	pub(super) next_file_instance_id: u64,
	pub(super) system_backfilled_lsn: Lsn,
	pub(super) database_catalog_root: PageNo,
	pub(super) file_catalog_root: PageNo,
	pub(super) graph_count: u32,
}

impl DatabaseSuperblock {
	pub(super) fn encode(&self) -> FerrosGrynnResult<AlignedPage> {
		self.validate()?;
		let mut page = AlignedPage::new(
			PageType::DatabaseSuperblock,
			PageFlags::empty(),
			PageLsn::new(self.checkpoint_record_lsn.get()),
			PAGE_NO,
		);
		let bytes = page.as_bytes_mut();
		bytes[NAME_OFFSET..NAME_OFFSET + self.name.len()].copy_from_slice(self.name.as_bytes());
		write_u32(bytes, FORMAT_OFFSET, FORMAT_VERSION);
		write_u32(bytes, COMPAT_OFFSET, FORMAT_VERSION);
		write_u64(bytes, GENERATION_OFFSET, self.generation.get());
		write_u64(bytes, CHECKPOINT_LSN_OFFSET, self.checkpoint_lsn.get());
		write_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET, self.checkpoint_record_lsn.get());
		write_u64(bytes, DURABLE_VISIBLE_LSN_OFFSET, self.durable_visible_lsn.get());
		write_u64(bytes, NEXT_LSN_OFFSET, self.next_lsn.get());
		write_u64(bytes, COMMIT_TS_OFFSET, self.commit_ts.get());
		write_u64(bytes, NEXT_TX_ID_OFFSET, self.next_tx_id.get());
		write_u64(bytes, NEXT_FILE_ID_OFFSET, self.next_file_instance_id);
		write_u64(bytes, SYSTEM_BACKFILLED_LSN_OFFSET, self.system_backfilled_lsn.get());
		write_u64(bytes, DATABASE_CATALOG_ROOT_OFFSET, self.database_catalog_root.get());
		write_u64(bytes, FILE_CATALOG_ROOT_OFFSET, self.file_catalog_root.get());
		write_u32(bytes, GRAPH_COUNT_OFFSET, self.graph_count);
		write_u32(bytes, FLAGS_OFFSET, 0);
		page.seal()?;
		Ok(page)
	}

	pub(super) fn decode(page: &AlignedPage) -> FerrosGrynnResult<Self> {
		let header = page.validate(PAGE_NO)?;
		if header.page_type() != PageType::DatabaseSuperblock || !header.flags().is_empty() {
			return Err(invalid("invalid DatabaseSuperblock page header"));
		}
		let bytes = page.as_bytes();
		let format = read_u32(bytes, FORMAT_OFFSET);
		if format != FORMAT_VERSION || read_u32(bytes, COMPAT_OFFSET) != FORMAT_VERSION {
			return Err(FerrosGrynnError::unsupported_storage_format(
				u64::from(format),
				u64::from(FORMAT_VERSION),
			));
		}
		if read_u32(bytes, FLAGS_OFFSET) != 0
			|| bytes[RESERVED_OFFSET..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("DatabaseSuperblock flags or reserved bytes are non-zero"));
		}
		let name_field = &bytes[NAME_OFFSET..NAME_OFFSET + NAME_BYTES];
		let end = name_field
			.iter()
			.position(|byte| *byte == 0)
			.ok_or_else(|| invalid("database name has no NUL terminator"))?;
		if end == 0 || name_field[end..].iter().any(|byte| *byte != 0) {
			return Err(invalid("database name has non-canonical padding"));
		}
		let name = std::str::from_utf8(&name_field[..end])
			.map_err(|_error| invalid("database name is not UTF-8"))?
			.to_owned();
		let value = Self {
			name,
			generation: CheckpointGeneration::new(read_u64(bytes, GENERATION_OFFSET)),
			checkpoint_lsn: Lsn::new(read_u64(bytes, CHECKPOINT_LSN_OFFSET)),
			checkpoint_record_lsn: Lsn::new(read_u64(bytes, CHECKPOINT_RECORD_LSN_OFFSET)),
			durable_visible_lsn: Lsn::new(read_u64(bytes, DURABLE_VISIBLE_LSN_OFFSET)),
			next_lsn: Lsn::new(read_u64(bytes, NEXT_LSN_OFFSET)),
			commit_ts: CommitTs::new(read_u64(bytes, COMMIT_TS_OFFSET)),
			next_tx_id: TxId::new(read_u64(bytes, NEXT_TX_ID_OFFSET)),
			next_file_instance_id: read_u64(bytes, NEXT_FILE_ID_OFFSET),
			system_backfilled_lsn: Lsn::new(read_u64(bytes, SYSTEM_BACKFILLED_LSN_OFFSET)),
			database_catalog_root: PageNo::new(read_u64(bytes, DATABASE_CATALOG_ROOT_OFFSET)),
			file_catalog_root: PageNo::new(read_u64(bytes, FILE_CATALOG_ROOT_OFFSET)),
			graph_count: read_u32(bytes, GRAPH_COUNT_OFFSET),
		};
		if header.page_lsn().get() != value.checkpoint_record_lsn.get() {
			return Err(invalid("DatabaseSuperblock page LSN disagrees with checkpoint record"));
		}
		value.validate()?;
		Ok(value)
	}

	fn validate(&self) -> FerrosGrynnResult<()> {
		if self.name.is_empty()
			|| self.name.len() >= NAME_BYTES
			|| self.name.as_bytes().contains(&0)
		{
			return Err(invalid("database name must contain 1..=31 non-NUL UTF-8 bytes"));
		}
		if self.generation == CheckpointGeneration::ZERO {
			if self.checkpoint_lsn != Lsn::ZERO || self.checkpoint_record_lsn != Lsn::ZERO {
				return Err(invalid("generation zero requires zero checkpoint clocks"));
			}
		} else if self.checkpoint_record_lsn == Lsn::ZERO {
			return Err(invalid("checkpoint generation requires a checkpoint record"));
		}
		if self.checkpoint_lsn > self.durable_visible_lsn
			|| self.checkpoint_lsn > self.checkpoint_record_lsn
			|| self.system_backfilled_lsn > self.checkpoint_lsn
		{
			return Err(invalid("DatabaseSuperblock checkpoint clocks are inconsistent"));
		}
		if self.next_lsn <= self.durable_visible_lsn || self.next_lsn <= self.checkpoint_record_lsn
		{
			return Err(invalid("DatabaseSuperblock next LSN does not exceed durable clocks"));
		}
		if self.next_tx_id == TxId::SYSTEM
			|| self.next_file_instance_id == 0
			|| self.next_file_instance_id > super::types::DataFileId::EXHAUSTED_INSTANCE_ID
		{
			return Err(invalid("DatabaseSuperblock allocation clock is invalid"));
		}
		if self.database_catalog_root == PageNo::NULL || self.file_catalog_root == PageNo::NULL {
			return Err(invalid("DatabaseSuperblock catalog roots are absent"));
		}
		Ok(())
	}
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exact_database_superblock_round_trip() -> FerrosGrynnResult<()> {
		let expected = DatabaseSuperblock {
			name: "test".into(),
			generation: CheckpointGeneration::ZERO,
			checkpoint_lsn: Lsn::ZERO,
			checkpoint_record_lsn: Lsn::ZERO,
			durable_visible_lsn: Lsn::ZERO,
			next_lsn: Lsn::FIRST,
			commit_ts: CommitTs::ZERO,
			next_tx_id: TxId::new(1),
			next_file_instance_id: 5,
			system_backfilled_lsn: Lsn::ZERO,
			database_catalog_root: PageNo::new(0),
			file_catalog_root: PageNo::new(1),
			graph_count: 0,
		};
		assert_eq!(DatabaseSuperblock::decode(&expected.encode()?)?, expected);
		Ok(())
	}
}
