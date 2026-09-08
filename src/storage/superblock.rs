//! Safe format-v1 codecs for database and graph superblocks.

use bitflags::bitflags;

use super::{
	page::{
		AlignedPage,
		PAGE_HEADER_SIZE,
		PAGE_SIZE,
		PageFlags,
		PageLsn,
		PageNo,
		PageType,
		STORAGE_FORMAT_VERSION,
	},
	types::{CheckpointGeneration, CommitTs, GraphId, Lsn, TxId},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Format version encoded by both superblock layouts.
const FORMAT_VERSION: u32 = STORAGE_FORMAT_VERSION as u32;
/// Oldest format version that the current writer promises to preserve.
const LAST_BACK_COMPAT_VERSION: u32 = FORMAT_VERSION;
/// Physical page number reserved for every superblock.
const SUPERBLOCK_PAGE_NO: PageNo = PageNo::new(0);
/// Assigned database-superblock flags in format v1; no bits are assigned.
const DATABASE_FLAGS_V1: u32 = 0;

/// Database-name byte offset.
const DB_NAME_OFFSET: usize = PAGE_HEADER_SIZE;
/// Fixed database-name field size.
const DB_NAME_BYTES: usize = 32;
/// Database format-version byte offset.
const DB_FORMAT_VERSION_OFFSET: usize = DB_NAME_OFFSET + DB_NAME_BYTES;
/// Database backward-compatibility version byte offset.
const DB_BACK_COMPAT_VERSION_OFFSET: usize = DB_FORMAT_VERSION_OFFSET + size_of::<u32>();
/// Database checkpoint generation byte offset.
const DB_GENERATION_OFFSET: usize = DB_BACK_COMPAT_VERSION_OFFSET + size_of::<u32>();
/// Database home-file checkpoint watermark byte offset.
const DB_CHECKPOINT_LSN_OFFSET: usize = DB_GENERATION_OFFSET + size_of::<u64>();
/// Database CheckpointEnd record byte offset.
const DB_CHECKPOINT_RECORD_LSN_OFFSET: usize = DB_CHECKPOINT_LSN_OFFSET + size_of::<u64>();
/// Database durable visibility byte offset.
const DB_DURABLE_VISIBLE_LSN_OFFSET: usize = DB_CHECKPOINT_RECORD_LSN_OFFSET + size_of::<u64>();
/// Database next-WAL-LSN byte offset.
const DB_NEXT_LSN_OFFSET: usize = DB_DURABLE_VISIBLE_LSN_OFFSET + size_of::<u64>();
/// Database commit clock byte offset.
const DB_COMMIT_TS_OFFSET: usize = DB_NEXT_LSN_OFFSET + size_of::<u64>();
/// Database next-transaction clock byte offset.
const DB_NEXT_TX_ID_OFFSET: usize = DB_COMMIT_TS_OFFSET + size_of::<u64>();
/// Database next-file-instance clock byte offset.
const DB_NEXT_FILE_INSTANCE_ID_OFFSET: usize = DB_NEXT_TX_ID_OFFSET + size_of::<u64>();
/// System-file backfill watermark byte offset.
const DB_SYSTEM_BACKFILLED_LSN_OFFSET: usize = DB_NEXT_FILE_INSTANCE_ID_OFFSET + size_of::<u64>();
/// Graph-catalog root byte offset.
const DB_GRAPH_CATALOG_ROOT_OFFSET: usize = DB_SYSTEM_BACKFILLED_LSN_OFFSET + size_of::<u64>();
/// File-catalog root byte offset.
const DB_FILE_CATALOG_ROOT_OFFSET: usize = DB_GRAPH_CATALOG_ROOT_OFFSET + size_of::<u64>();
/// Persisted graph-count byte offset.
const DB_GRAPH_COUNT_OFFSET: usize = DB_FILE_CATALOG_ROOT_OFFSET + size_of::<u64>();
/// Database flag byte offset.
const DB_FLAGS_OFFSET: usize = DB_GRAPH_COUNT_OFFSET + size_of::<u32>();
/// First reserved database-superblock byte.
const DB_RESERVED_OFFSET: usize = DB_FLAGS_OFFSET + size_of::<u32>();

/// Graph identity byte offset.
const GRAPH_ID_OFFSET: usize = PAGE_HEADER_SIZE;
/// Graph format-version byte offset.
const GRAPH_FORMAT_VERSION_OFFSET: usize = GRAPH_ID_OFFSET + size_of::<u64>();
/// Graph backward-compatibility version byte offset.
const GRAPH_BACK_COMPAT_VERSION_OFFSET: usize = GRAPH_FORMAT_VERSION_OFFSET + size_of::<u32>();
/// Graph checkpoint generation byte offset.
const GRAPH_GENERATION_OFFSET: usize = GRAPH_BACK_COMPAT_VERSION_OFFSET + size_of::<u32>();
/// Graph home-file backfill watermark byte offset.
const GRAPH_BACKFILLED_LSN_OFFSET: usize = GRAPH_GENERATION_OFFSET + size_of::<u64>();
/// Graph creation-time byte offset.
const GRAPH_CREATED_AT_MS_OFFSET: usize = GRAPH_BACKFILLED_LSN_OFFSET + size_of::<u64>();
/// Graph update-time byte offset.
const GRAPH_UPDATED_AT_MS_OFFSET: usize = GRAPH_CREATED_AT_MS_OFFSET + size_of::<u64>();
/// Node-capacity byte offset.
const GRAPH_NODE_CAPACITY_OFFSET: usize = GRAPH_UPDATED_AT_MS_OFFSET + size_of::<u64>();
/// Edge-capacity byte offset.
const GRAPH_EDGE_CAPACITY_OFFSET: usize = GRAPH_NODE_CAPACITY_OFFSET + size_of::<u64>();
/// Node-count byte offset.
const GRAPH_NODE_COUNT_OFFSET: usize = GRAPH_EDGE_CAPACITY_OFFSET + size_of::<u64>();
/// Edge-count byte offset.
const GRAPH_EDGE_COUNT_OFFSET: usize = GRAPH_NODE_COUNT_OFFSET + size_of::<u64>();
/// Subgraph-capacity byte offset.
const GRAPH_SUBGRAPH_CAPACITY_OFFSET: usize = GRAPH_EDGE_COUNT_OFFSET + size_of::<u64>();
/// Subgraph-count byte offset.
const GRAPH_SUBGRAPH_COUNT_OFFSET: usize = GRAPH_SUBGRAPH_CAPACITY_OFFSET + size_of::<u64>();
/// Node-hot page-count byte offset.
const GRAPH_NODE_HOT_PAGES_OFFSET: usize = GRAPH_SUBGRAPH_COUNT_OFFSET + size_of::<u64>();
/// Edge-hot page-count byte offset.
const GRAPH_EDGE_HOT_PAGES_OFFSET: usize = GRAPH_NODE_HOT_PAGES_OFFSET + size_of::<u64>();
/// Property page-count byte offset.
const GRAPH_PROPERTIES_PAGES_OFFSET: usize = GRAPH_EDGE_HOT_PAGES_OFFSET + size_of::<u64>();
/// Label-file byte-length byte offset.
const GRAPH_LABELS_SIZE_BYTES_OFFSET: usize = GRAPH_PROPERTIES_PAGES_OFFSET + size_of::<u64>();
/// Graph flag byte offset.
const GRAPH_FLAGS_OFFSET: usize = GRAPH_LABELS_SIZE_BYTES_OFFSET + size_of::<u64>();
/// Schema root-page byte offset.
const GRAPH_SCHEMA_ROOT_PAGE_OFFSET: usize = GRAPH_FLAGS_OFFSET + size_of::<u64>();
/// Statistics root-page byte offset.
const GRAPH_STATS_ROOT_PAGE_OFFSET: usize = GRAPH_SCHEMA_ROOT_PAGE_OFFSET + size_of::<u64>();
/// Graph-name byte offset.
const GRAPH_NAME_OFFSET: usize = GRAPH_STATS_ROOT_PAGE_OFFSET + size_of::<u64>();
/// Fixed graph-name field size.
const GRAPH_NAME_BYTES: usize = 128;
/// First reserved graph-superblock byte.
const GRAPH_RESERVED_OFFSET: usize = GRAPH_NAME_OFFSET + GRAPH_NAME_BYTES;

bitflags! {
	/// Assigned format-v1 graph-superblock flags.
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub(super) struct GraphSuperblockFlags: u64 {
		/// Graph data files are encrypted.
		const ENCRYPTED = 0x01;
		/// Property payloads use LZ4 compression.
		const LZ4_PROPERTIES = 0x02;
		/// Graph is opened read-only.
		const READ_ONLY = 0x04;
		/// Per-label side-files do not represent the current graph revision.
		const LABEL_INDEX_STALE = 0x08;
	}
}

/// Durable database-wide clocks and checkpoint authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DatabaseSuperblock {
	/// User-visible local database name.
	pub(super) db_name: String,
	/// Monotonic committed checkpoint generation.
	pub(super) generation: CheckpointGeneration,
	/// Highest commit boundary present in every home file.
	pub(super) checkpoint_lsn: Lsn,
	/// WAL record LSN of the matching CheckpointEnd.
	pub(super) checkpoint_record_lsn: Lsn,
	/// Highest durable transaction commit visible before reopen.
	pub(super) durable_visible_lsn: Lsn,
	/// Next unassigned WAL record LSN.
	pub(super) next_lsn: Lsn,
	/// Highest assigned commit timestamp.
	pub(super) commit_ts: CommitTs,
	/// Next unassigned transaction identity.
	pub(super) next_tx_id: TxId,
	/// Next unassigned 56-bit data-file instance identity.
	pub(super) next_file_instance_id: u64,
	/// Highest commit boundary copied to system files.
	pub(super) system_backfilled_lsn: Lsn,
	/// Root page of the future durable graph catalog.
	pub(super) graph_catalog_root_page: PageNo,
	/// Root page of the future durable file catalog.
	pub(super) file_catalog_root_page: PageNo,
	/// Number of durable graph catalog entries.
	pub(super) graph_count: u32,
}

impl DatabaseSuperblock {
	/// Encodes one checksummed aligned database-superblock page.
	///
	/// # Errors
	///
	/// Returns an error for an invalid name, clock relation, or format-v1
	/// invariant.
	pub(super) fn encode(&self) -> FerrosGrynnResult<AlignedPage> {
		let mut page = AlignedPage::zeroed();
		self.encode_into(&mut page)?;
		Ok(page)
	}

	/// Encodes into caller-owned aligned storage so checkpoint can reuse its
	/// fixed extent without allocating another page buffer.
	///
	/// # Errors
	///
	/// Returns the same validation errors as [`Self::encode`].
	pub(super) fn encode_into(&self, page: &mut AlignedPage) -> FerrosGrynnResult<()> {
		self.validate_semantics()?;
		page.reset(
			PageType::DatabaseSuperblock,
			PageFlags::empty(),
			PageLsn::new(self.checkpoint_record_lsn.get()),
			SUPERBLOCK_PAGE_NO,
		);
		let bytes = page.as_bytes_mut();
		write_fixed_name(bytes, DB_NAME_OFFSET, DB_NAME_BYTES, &self.db_name)?;
		write_u32(bytes, DB_FORMAT_VERSION_OFFSET, FORMAT_VERSION);
		write_u32(bytes, DB_BACK_COMPAT_VERSION_OFFSET, LAST_BACK_COMPAT_VERSION);
		write_u64(bytes, DB_GENERATION_OFFSET, self.generation.get());
		write_u64(bytes, DB_CHECKPOINT_LSN_OFFSET, self.checkpoint_lsn.get());
		write_u64(bytes, DB_CHECKPOINT_RECORD_LSN_OFFSET, self.checkpoint_record_lsn.get());
		write_u64(bytes, DB_DURABLE_VISIBLE_LSN_OFFSET, self.durable_visible_lsn.get());
		write_u64(bytes, DB_NEXT_LSN_OFFSET, self.next_lsn.get());
		write_u64(bytes, DB_COMMIT_TS_OFFSET, self.commit_ts.get());
		write_u64(bytes, DB_NEXT_TX_ID_OFFSET, self.next_tx_id.get());
		write_u64(bytes, DB_NEXT_FILE_INSTANCE_ID_OFFSET, self.next_file_instance_id);
		write_u64(bytes, DB_SYSTEM_BACKFILLED_LSN_OFFSET, self.system_backfilled_lsn.get());
		write_u64(bytes, DB_GRAPH_CATALOG_ROOT_OFFSET, self.graph_catalog_root_page.get());
		write_u64(bytes, DB_FILE_CATALOG_ROOT_OFFSET, self.file_catalog_root_page.get());
		write_u32(bytes, DB_GRAPH_COUNT_OFFSET, self.graph_count);
		write_u32(bytes, DB_FLAGS_OFFSET, DATABASE_FLAGS_V1);
		page.seal()?;
		Ok(())
	}

	/// Decodes and validates one database-superblock page.
	///
	/// # Errors
	///
	/// Returns an error for page corruption, a mismatched page type, unknown
	/// flags, non-zero reserved bytes, an unsupported version, or invalid
	/// clocks.
	pub(super) fn decode(page: &AlignedPage) -> FerrosGrynnResult<Self> {
		let header = page.validate(SUPERBLOCK_PAGE_NO)?;
		if header.page_type() != PageType::DatabaseSuperblock || !header.flags().is_empty() {
			return Err(invalid_superblock("invalid DatabaseSuperblock page header"));
		}
		let bytes = page.as_bytes();
		validate_version(bytes, DB_FORMAT_VERSION_OFFSET, DB_BACK_COMPAT_VERSION_OFFSET)?;
		if read_u32(bytes, DB_FLAGS_OFFSET) != DATABASE_FLAGS_V1 {
			return Err(invalid_superblock("DatabaseSuperblock contains unknown flags"));
		}
		validate_zero(bytes, DB_RESERVED_OFFSET, "DatabaseSuperblock reserved bytes")?;
		let value = Self {
			db_name: read_fixed_name(bytes, DB_NAME_OFFSET, DB_NAME_BYTES)?,
			generation: CheckpointGeneration::new(read_u64(bytes, DB_GENERATION_OFFSET)),
			checkpoint_lsn: Lsn::new(read_u64(bytes, DB_CHECKPOINT_LSN_OFFSET)),
			checkpoint_record_lsn: Lsn::new(read_u64(bytes, DB_CHECKPOINT_RECORD_LSN_OFFSET)),
			durable_visible_lsn: Lsn::new(read_u64(bytes, DB_DURABLE_VISIBLE_LSN_OFFSET)),
			next_lsn: Lsn::new(read_u64(bytes, DB_NEXT_LSN_OFFSET)),
			commit_ts: CommitTs::new(read_u64(bytes, DB_COMMIT_TS_OFFSET)),
			next_tx_id: TxId::new(read_u64(bytes, DB_NEXT_TX_ID_OFFSET)),
			next_file_instance_id: read_u64(bytes, DB_NEXT_FILE_INSTANCE_ID_OFFSET),
			system_backfilled_lsn: Lsn::new(read_u64(bytes, DB_SYSTEM_BACKFILLED_LSN_OFFSET)),
			graph_catalog_root_page: PageNo::new(read_u64(bytes, DB_GRAPH_CATALOG_ROOT_OFFSET)),
			file_catalog_root_page: PageNo::new(read_u64(bytes, DB_FILE_CATALOG_ROOT_OFFSET)),
			graph_count: read_u32(bytes, DB_GRAPH_COUNT_OFFSET),
		};
		if header.page_lsn().get() != value.checkpoint_record_lsn.get() {
			return Err(invalid_superblock(
				"DatabaseSuperblock header LSN does not match checkpoint_record_lsn",
			));
		}
		value.validate_semantics()?;
		Ok(value)
	}

	/// Validates cross-field clock and sentinel invariants.
	fn validate_semantics(&self) -> FerrosGrynnResult<()> {
		validate_name(&self.db_name, DB_NAME_BYTES)?;
		if self.generation == CheckpointGeneration::ZERO {
			if self.checkpoint_lsn != Lsn::ZERO || self.checkpoint_record_lsn != Lsn::ZERO {
				return Err(invalid_superblock("generation zero requires zero checkpoint clocks"));
			}
		} else if self.checkpoint_record_lsn == Lsn::ZERO {
			return Err(invalid_superblock(
				"a completed checkpoint generation requires CheckpointEnd LSN",
			));
		}
		if self.checkpoint_lsn > self.durable_visible_lsn {
			return Err(invalid_superblock("checkpoint_lsn exceeds durable_visible_lsn"));
		}
		if self.checkpoint_lsn > self.checkpoint_record_lsn {
			return Err(invalid_superblock("checkpoint_lsn exceeds its CheckpointEnd record"));
		}
		if self.system_backfilled_lsn > self.checkpoint_lsn {
			return Err(invalid_superblock("system_backfilled_lsn exceeds checkpoint_lsn"));
		}
		if self.next_lsn <= self.durable_visible_lsn || self.next_lsn <= self.checkpoint_record_lsn
		{
			return Err(invalid_superblock("next_lsn does not exceed persisted WAL clocks"));
		}
		if self.next_tx_id == TxId::SYSTEM {
			return Err(invalid_superblock("next_tx_id zero is reserved"));
		}
		if self.next_file_instance_id > super::types::DataFileId::EXHAUSTED_INSTANCE_ID {
			return Err(invalid_superblock("next_file_instance_id exceeds its exhausted sentinel"));
		}
		Ok(())
	}
}

/// Checkpoint-owned graph metadata stored outside transactional PageFrame WAL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GraphSuperblock {
	/// Graph whose files this metadata describes.
	pub(super) graph_id: GraphId,
	/// Monotonic checkpoint generation copied from database authority.
	pub(super) generation: CheckpointGeneration,
	/// Highest commit boundary copied to this graph's home files.
	pub(super) backfilled_lsn: Lsn,
	/// Matching CheckpointEnd record LSN stored in the common page header.
	pub(super) checkpoint_record_lsn: Lsn,
	/// Graph creation time in Unix milliseconds.
	pub(super) created_at_ms: u64,
	/// Last graph metadata update time in Unix milliseconds.
	pub(super) updated_at_ms: u64,
	/// Allocated node slots.
	pub(super) node_capacity: u64,
	/// Allocated edge slots.
	pub(super) edge_capacity: u64,
	/// Live node count.
	pub(super) node_count: u64,
	/// Live edge count.
	pub(super) edge_count: u64,
	/// Allocated subgraph slots.
	pub(super) subgraph_capacity: u64,
	/// Live subgraph count.
	pub(super) subgraph_count: u64,
	/// Number of node-hot pages.
	pub(super) node_hot_pages: u64,
	/// Number of edge-hot pages.
	pub(super) edge_hot_pages: u64,
	/// Number of property pages.
	pub(super) properties_pages: u64,
	/// Logical label-file length in bytes.
	pub(super) labels_size_bytes: u64,
	/// Assigned graph feature flags.
	pub(super) flags: GraphSuperblockFlags,
	/// Schema catalog root or [`PageNo::NULL`].
	pub(super) schema_root_page: PageNo,
	/// Statistics catalog root or [`PageNo::NULL`].
	pub(super) stats_root_page: PageNo,
	/// User-visible graph name.
	pub(super) graph_name: String,
}

impl GraphSuperblock {
	/// Encodes one checksummed aligned graph-superblock page.
	///
	/// # Errors
	///
	/// Returns an error for an invalid name, clock, count, or flag invariant.
	pub(super) fn encode(&self) -> FerrosGrynnResult<AlignedPage> {
		let mut page = AlignedPage::zeroed();
		self.encode_into(&mut page)?;
		Ok(page)
	}

	/// Encodes into caller-owned aligned storage so checkpoint can reuse its
	/// fixed extent without allocating another page buffer.
	///
	/// # Errors
	///
	/// Returns the same validation errors as [`Self::encode`].
	pub(super) fn encode_into(&self, page: &mut AlignedPage) -> FerrosGrynnResult<()> {
		self.validate_semantics()?;
		page.reset(
			PageType::GraphSuperblock,
			PageFlags::empty(),
			PageLsn::new(self.checkpoint_record_lsn.get()),
			SUPERBLOCK_PAGE_NO,
		);
		let bytes = page.as_bytes_mut();
		write_u64(bytes, GRAPH_ID_OFFSET, self.graph_id.get());
		write_u32(bytes, GRAPH_FORMAT_VERSION_OFFSET, FORMAT_VERSION);
		write_u32(bytes, GRAPH_BACK_COMPAT_VERSION_OFFSET, LAST_BACK_COMPAT_VERSION);
		write_u64(bytes, GRAPH_GENERATION_OFFSET, self.generation.get());
		write_u64(bytes, GRAPH_BACKFILLED_LSN_OFFSET, self.backfilled_lsn.get());
		write_u64(bytes, GRAPH_CREATED_AT_MS_OFFSET, self.created_at_ms);
		write_u64(bytes, GRAPH_UPDATED_AT_MS_OFFSET, self.updated_at_ms);
		write_u64(bytes, GRAPH_NODE_CAPACITY_OFFSET, self.node_capacity);
		write_u64(bytes, GRAPH_EDGE_CAPACITY_OFFSET, self.edge_capacity);
		write_u64(bytes, GRAPH_NODE_COUNT_OFFSET, self.node_count);
		write_u64(bytes, GRAPH_EDGE_COUNT_OFFSET, self.edge_count);
		write_u64(bytes, GRAPH_SUBGRAPH_CAPACITY_OFFSET, self.subgraph_capacity);
		write_u64(bytes, GRAPH_SUBGRAPH_COUNT_OFFSET, self.subgraph_count);
		write_u64(bytes, GRAPH_NODE_HOT_PAGES_OFFSET, self.node_hot_pages);
		write_u64(bytes, GRAPH_EDGE_HOT_PAGES_OFFSET, self.edge_hot_pages);
		write_u64(bytes, GRAPH_PROPERTIES_PAGES_OFFSET, self.properties_pages);
		write_u64(bytes, GRAPH_LABELS_SIZE_BYTES_OFFSET, self.labels_size_bytes);
		write_u64(bytes, GRAPH_FLAGS_OFFSET, self.flags.bits());
		write_u64(bytes, GRAPH_SCHEMA_ROOT_PAGE_OFFSET, self.schema_root_page.get());
		write_u64(bytes, GRAPH_STATS_ROOT_PAGE_OFFSET, self.stats_root_page.get());
		write_fixed_name(bytes, GRAPH_NAME_OFFSET, GRAPH_NAME_BYTES, &self.graph_name)?;
		page.seal()?;
		Ok(())
	}

	/// Decodes and validates one graph-superblock page.
	///
	/// # Errors
	///
	/// Returns an error for page corruption, incompatible format, unknown
	/// flags, non-zero reserved bytes, or invalid clocks and counts.
	pub(super) fn decode(page: &AlignedPage) -> FerrosGrynnResult<Self> {
		let header = page.validate(SUPERBLOCK_PAGE_NO)?;
		if header.page_type() != PageType::GraphSuperblock || !header.flags().is_empty() {
			return Err(invalid_superblock("invalid GraphSuperblock page header"));
		}
		let bytes = page.as_bytes();
		validate_version(bytes, GRAPH_FORMAT_VERSION_OFFSET, GRAPH_BACK_COMPAT_VERSION_OFFSET)?;
		validate_zero(bytes, GRAPH_RESERVED_OFFSET, "GraphSuperblock reserved bytes")?;
		let raw_flags = read_u64(bytes, GRAPH_FLAGS_OFFSET);
		let flags = GraphSuperblockFlags::from_bits(raw_flags)
			.ok_or_else(|| invalid_superblock("GraphSuperblock contains unknown flags"))?;
		let value = Self {
			graph_id: GraphId::new(read_u64(bytes, GRAPH_ID_OFFSET)),
			generation: CheckpointGeneration::new(read_u64(bytes, GRAPH_GENERATION_OFFSET)),
			backfilled_lsn: Lsn::new(read_u64(bytes, GRAPH_BACKFILLED_LSN_OFFSET)),
			checkpoint_record_lsn: Lsn::new(header.page_lsn().get()),
			created_at_ms: read_u64(bytes, GRAPH_CREATED_AT_MS_OFFSET),
			updated_at_ms: read_u64(bytes, GRAPH_UPDATED_AT_MS_OFFSET),
			node_capacity: read_u64(bytes, GRAPH_NODE_CAPACITY_OFFSET),
			edge_capacity: read_u64(bytes, GRAPH_EDGE_CAPACITY_OFFSET),
			node_count: read_u64(bytes, GRAPH_NODE_COUNT_OFFSET),
			edge_count: read_u64(bytes, GRAPH_EDGE_COUNT_OFFSET),
			subgraph_capacity: read_u64(bytes, GRAPH_SUBGRAPH_CAPACITY_OFFSET),
			subgraph_count: read_u64(bytes, GRAPH_SUBGRAPH_COUNT_OFFSET),
			node_hot_pages: read_u64(bytes, GRAPH_NODE_HOT_PAGES_OFFSET),
			edge_hot_pages: read_u64(bytes, GRAPH_EDGE_HOT_PAGES_OFFSET),
			properties_pages: read_u64(bytes, GRAPH_PROPERTIES_PAGES_OFFSET),
			labels_size_bytes: read_u64(bytes, GRAPH_LABELS_SIZE_BYTES_OFFSET),
			flags,
			schema_root_page: PageNo::new(read_u64(bytes, GRAPH_SCHEMA_ROOT_PAGE_OFFSET)),
			stats_root_page: PageNo::new(read_u64(bytes, GRAPH_STATS_ROOT_PAGE_OFFSET)),
			graph_name: read_fixed_name(bytes, GRAPH_NAME_OFFSET, GRAPH_NAME_BYTES)?,
		};
		value.validate_semantics()?;
		Ok(value)
	}

	/// Validates graph-local clocks, counts, and canonical name encoding.
	fn validate_semantics(&self) -> FerrosGrynnResult<()> {
		validate_name(&self.graph_name, GRAPH_NAME_BYTES)?;
		if self.graph_id == GraphId::SYSTEM {
			return Err(invalid_superblock("graph_id zero is reserved for system files"));
		}
		if self.generation == CheckpointGeneration::ZERO {
			if self.backfilled_lsn != Lsn::ZERO || self.checkpoint_record_lsn != Lsn::ZERO {
				return Err(invalid_superblock(
					"graph generation zero requires zero checkpoint clocks",
				));
			}
		} else if self.checkpoint_record_lsn == Lsn::ZERO {
			return Err(invalid_superblock(
				"a completed graph generation requires CheckpointEnd LSN",
			));
		}
		if self.backfilled_lsn > self.checkpoint_record_lsn {
			return Err(invalid_superblock("graph backfilled_lsn exceeds CheckpointEnd LSN"));
		}
		if self.updated_at_ms < self.created_at_ms {
			return Err(invalid_superblock("graph update time precedes creation time"));
		}
		if self.node_count > self.node_capacity
			|| self.edge_count > self.edge_capacity
			|| self.subgraph_count > self.subgraph_capacity
		{
			return Err(invalid_superblock("graph live count exceeds its capacity"));
		}
		Ok(())
	}
}

/// Validates one version pair against the only supported format.
fn validate_version(
	bytes: &[u8; PAGE_SIZE],
	format_offset: usize,
	compat_offset: usize,
) -> FerrosGrynnResult<()> {
	let actual = read_u32(bytes, format_offset);
	if actual != FORMAT_VERSION {
		return Err(FerrosGrynnError::unsupported_storage_format(
			u64::from(actual),
			u64::from(FORMAT_VERSION),
		));
	}
	let compatible = read_u32(bytes, compat_offset);
	if compatible == 0 || compatible > actual {
		return Err(invalid_superblock("invalid last_back_compat_version"));
	}
	Ok(())
}

/// Validates one UTF-8 name before fixed-field encoding.
fn validate_name(name: &str, field_size: usize) -> FerrosGrynnResult<()> {
	if name.is_empty() || name.as_bytes().contains(&0) || name.len() >= field_size {
		return Err(invalid_superblock(format!(
			"name must contain 1..{} non-NUL UTF-8 bytes",
			field_size - 1
		)));
	}
	Ok(())
}

/// Writes one canonical NUL-terminated fixed UTF-8 field.
fn write_fixed_name(
	bytes: &mut [u8; PAGE_SIZE],
	offset: usize,
	field_size: usize,
	name: &str,
) -> FerrosGrynnResult<()> {
	validate_name(name, field_size)?;
	let end = offset + name.len();
	bytes[offset..end].copy_from_slice(name.as_bytes());
	Ok(())
}

/// Reads one canonical NUL-terminated fixed UTF-8 field.
fn read_fixed_name(
	bytes: &[u8; PAGE_SIZE],
	offset: usize,
	field_size: usize,
) -> FerrosGrynnResult<String> {
	let field = &bytes[offset..offset + field_size];
	let terminator = field
		.iter()
		.position(|byte| *byte == 0)
		.ok_or_else(|| invalid_superblock("fixed name lacks a NUL terminator"))?;
	if terminator == 0 || field[terminator..].iter().any(|byte| *byte != 0) {
		return Err(invalid_superblock("fixed name has non-canonical padding"));
	}
	let name = std::str::from_utf8(&field[..terminator])
		.map_err(|source| invalid_superblock(format!("fixed name is not UTF-8: {source}")))?;
	Ok(name.to_owned())
}

/// Requires every byte from `offset` to the end of the page to be zero.
fn validate_zero(bytes: &[u8; PAGE_SIZE], offset: usize, field: &str) -> FerrosGrynnResult<()> {
	if bytes[offset..].iter().any(|byte| *byte != 0) {
		return Err(invalid_superblock(format!("{field} are non-zero")));
	}
	Ok(())
}

/// Writes one little-endian `u32` at a named, compile-time layout offset.
fn write_u32(bytes: &mut [u8; PAGE_SIZE], offset: usize, value: u32) {
	bytes[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u64` at a named, compile-time layout offset.
fn write_u64(bytes: &mut [u8; PAGE_SIZE], offset: usize, value: u64) {
	bytes[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

/// Reads one little-endian `u32` at a validated fixed layout offset.
fn read_u32(bytes: &[u8; PAGE_SIZE], offset: usize) -> u32 {
	let mut value = [0_u8; size_of::<u32>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64` at a validated fixed layout offset.
fn read_u64(bytes: &[u8; PAGE_SIZE], offset: usize) -> u64 {
	let mut value = [0_u8; size_of::<u64>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(value)
}

/// Creates a structured invalid-superblock error.
fn invalid_superblock(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Creates one valid database fixture with non-trivial clocks.
	fn database_fixture() -> DatabaseSuperblock {
		DatabaseSuperblock {
			db_name: "ferros-grynn".into(),
			generation: CheckpointGeneration::new(3),
			checkpoint_lsn: Lsn::new(40),
			checkpoint_record_lsn: Lsn::new(44),
			durable_visible_lsn: Lsn::new(42),
			next_lsn: Lsn::new(45),
			commit_ts: CommitTs::new(9),
			next_tx_id: TxId::new(11),
			next_file_instance_id: 17,
			system_backfilled_lsn: Lsn::new(40),
			graph_catalog_root_page: PageNo::NULL,
			file_catalog_root_page: PageNo::NULL,
			graph_count: 2,
		}
	}

	/// Creates one valid graph fixture with assigned flags and roots.
	fn graph_fixture() -> GraphSuperblock {
		GraphSuperblock {
			graph_id: GraphId::new(7),
			generation: CheckpointGeneration::new(3),
			backfilled_lsn: Lsn::new(40),
			checkpoint_record_lsn: Lsn::new(44),
			created_at_ms: 100,
			updated_at_ms: 120,
			node_capacity: 128,
			edge_capacity: 256,
			node_count: 12,
			edge_count: 24,
			subgraph_capacity: 8,
			subgraph_count: 2,
			node_hot_pages: 2,
			edge_hot_pages: 3,
			properties_pages: 4,
			labels_size_bytes: 1024,
			flags: GraphSuperblockFlags::READ_ONLY,
			schema_root_page: PageNo::NULL,
			stats_root_page: PageNo::NULL,
			graph_name: "graph".into(),
		}
	}

	/// Database codec preserves exact offsets and semantic values.
	#[test]
	fn database_superblock_round_trip_matches_layout() -> FerrosGrynnResult<()> {
		let expected = database_fixture();
		let page = expected.encode()?;
		assert_eq!(read_u32(page.as_bytes(), DB_FORMAT_VERSION_OFFSET), FORMAT_VERSION);
		assert_eq!(read_u64(page.as_bytes(), DB_GENERATION_OFFSET), 3);
		assert_eq!(read_u64(page.as_bytes(), DB_CHECKPOINT_RECORD_LSN_OFFSET), 44);
		assert_eq!(read_u64(page.as_bytes(), DB_GRAPH_CATALOG_ROOT_OFFSET), u64::MAX);
		assert_eq!(DatabaseSuperblock::decode(&page)?, expected);
		Ok(())
	}

	/// Graph codec preserves exact offsets and all metadata fields.
	#[test]
	fn graph_superblock_round_trip_matches_layout() -> FerrosGrynnResult<()> {
		let expected = graph_fixture();
		let page = expected.encode()?;
		assert_eq!(read_u64(page.as_bytes(), GRAPH_ID_OFFSET), 7);
		assert_eq!(read_u64(page.as_bytes(), GRAPH_BACKFILLED_LSN_OFFSET), 40);
		assert_eq!(read_u64(page.as_bytes(), GRAPH_FLAGS_OFFSET), 4);
		assert_eq!(GraphSuperblock::decode(&page)?, expected);
		Ok(())
	}

	/// Semantic corruption remains detectable after a checksum is recomputed.
	#[test]
	fn superblock_rejects_reserved_flags_versions_and_clocks() -> FerrosGrynnResult<()> {
		let mut database = database_fixture().encode()?;
		write_u32(database.as_bytes_mut(), DB_FLAGS_OFFSET, 1);
		database.seal()?;
		assert!(DatabaseSuperblock::decode(&database).is_err());

		let mut graph = graph_fixture().encode()?;
		write_u64(graph.as_bytes_mut(), GRAPH_FLAGS_OFFSET, 0x10);
		graph.seal()?;
		assert!(GraphSuperblock::decode(&graph).is_err());

		let mut reserved = database_fixture().encode()?;
		reserved.as_bytes_mut()[DB_RESERVED_OFFSET] = 1;
		reserved.seal()?;
		assert!(DatabaseSuperblock::decode(&reserved).is_err());

		let mut version = database_fixture().encode()?;
		write_u32(version.as_bytes_mut(), DB_FORMAT_VERSION_OFFSET, FORMAT_VERSION + 1);
		version.seal()?;
		assert!(DatabaseSuperblock::decode(&version).is_err());

		let mut invalid_clocks = database_fixture();
		invalid_clocks.next_lsn = invalid_clocks.checkpoint_record_lsn;
		assert!(invalid_clocks.encode().is_err());
		let mut invalid_graph_clocks = graph_fixture();
		invalid_graph_clocks.backfilled_lsn = Lsn::new(45);
		assert!(invalid_graph_clocks.encode().is_err());

		let mut checksum = graph_fixture().encode()?;
		checksum.as_bytes_mut()[GRAPH_NAME_OFFSET] ^= 1;
		assert!(GraphSuperblock::decode(&checksum).is_err());
		Ok(())
	}
}
