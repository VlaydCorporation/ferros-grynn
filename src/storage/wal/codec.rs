#![allow(
	clippy::wildcard_imports,
	reason = "move-only WAL split keeps the existing imports during stage 1"
)]

use super::*;

/// Typed marker at the start of every frame-WAL record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(in crate::storage::wal) struct WalMagic([u8; 4]);

impl WalMagic {
	/// Unreleased stage-3a marker whose payload identity is incompatible.
	pub(in crate::storage::wal) const EXPERIMENTAL_3A: Self = Self(*b"MVCE");
	/// Assigned marker for the format-v1 database-level frame-WAL stream.
	pub(in crate::storage::wal) const FRAME_V1: Self = Self(*b"FGW1");

	/// Returns the exact persisted bytes.
	pub(in crate::storage::wal) const fn get(self) -> [u8; 4] {
		self.0
	}
}

/// Byte offset of the record LSN after the magic.
pub(in crate::storage::wal) const WAL_LSN_OFFSET: usize = size_of::<WalMagic>();
/// Byte offset of the transaction identity.
pub(in crate::storage::wal) const WAL_TX_ID_OFFSET: usize = WAL_LSN_OFFSET + size_of::<u64>();
/// Byte offset of the record-kind discriminant.
pub(in crate::storage::wal) const WAL_KIND_OFFSET: usize = WAL_TX_ID_OFFSET + size_of::<u64>();
/// Byte offset of the encoded payload length.
pub(in crate::storage::wal) const WAL_PAYLOAD_LEN_OFFSET: usize = WAL_KIND_OFFSET + size_of::<u8>();
/// Byte offset of the record checksum.
pub(in crate::storage::wal) const WAL_CHECKSUM_OFFSET: usize =
	WAL_PAYLOAD_LEN_OFFSET + size_of::<u32>();
/// Exact format-v1 WAL record-header size.
pub(in crate::storage::wal) const WAL_HEADER_SIZE: usize = WAL_CHECKSUM_OFFSET + size_of::<u64>();
/// Transaction identity offset inside the checksum prefix.
pub(in crate::storage::wal) const CHECKSUM_TX_ID_OFFSET: usize = size_of::<u64>();
/// Record-kind offset inside the checksum prefix.
pub(in crate::storage::wal) const CHECKSUM_KIND_OFFSET: usize =
	CHECKSUM_TX_ID_OFFSET + size_of::<u64>();
/// Payload-length offset inside the checksum prefix.
pub(in crate::storage::wal) const CHECKSUM_PAYLOAD_LEN_OFFSET: usize =
	CHECKSUM_KIND_OFFSET + size_of::<u8>();
/// Header bytes covered by the checksum before the payload.
pub(in crate::storage::wal) const WAL_CHECKSUM_PREFIX_SIZE: usize =
	CHECKSUM_PAYLOAD_LEN_OFFSET + size_of::<u32>();
/// Default segment target; an oversized transaction owns one larger segment.
pub(in crate::storage::wal) const DEFAULT_SEGMENT_TARGET_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum unique staged page bytes in one synchronous transaction.
pub(in crate::storage::wal) const MAX_TX_WRITE_BYTES: usize = 256 * 1024 * 1024;
/// Graph identity offset inside a PageFrame key.
pub(in crate::storage::wal) const PAGE_FRAME_GRAPH_ID_OFFSET: usize = 0;
/// Data-file identity offset inside a PageFrame key.
pub(in crate::storage::wal) const PAGE_FRAME_FILE_ID_OFFSET: usize =
	PAGE_FRAME_GRAPH_ID_OFFSET + size_of::<u64>();
/// Page-number offset inside a PageFrame key.
pub(in crate::storage::wal) const PAGE_FRAME_PAGE_NO_OFFSET: usize =
	PAGE_FRAME_FILE_ID_OFFSET + size_of::<u64>();
/// Fixed PageFrame fields before the complete page image.
pub(in crate::storage::wal) const PAGE_FRAME_PREFIX_SIZE: usize =
	PAGE_FRAME_PAGE_NO_OFFSET + size_of::<u64>();
/// Exact PageFrame payload size.
pub(in crate::storage::wal) const PAGE_FRAME_PAYLOAD_SIZE: usize =
	PAGE_FRAME_PREFIX_SIZE + PAGE_SIZE;
/// Largest assigned format-v1 record payload, checked before allocation.
pub(in crate::storage::wal) const MAX_WAL_PAYLOAD_BYTES: usize = PAGE_FRAME_PAYLOAD_SIZE;
/// Commit timestamp offset inside a TxCommit payload.
pub(in crate::storage::wal) const TX_COMMIT_TS_OFFSET: usize = 0;
/// Frame-count offset inside a TxCommit payload.
pub(in crate::storage::wal) const TX_COMMIT_FRAME_COUNT_OFFSET: usize =
	TX_COMMIT_TS_OFFSET + size_of::<u64>();
/// Reserved-padding offset inside a TxCommit payload.
pub(in crate::storage::wal) const TX_COMMIT_RESERVED_OFFSET: usize =
	TX_COMMIT_FRAME_COUNT_OFFSET + size_of::<u32>();
/// Exact TxCommit payload size.
pub(in crate::storage::wal) const TX_COMMIT_PAYLOAD_SIZE: usize =
	TX_COMMIT_RESERVED_OFFSET + size_of::<u32>();
/// Page-type offset inside a PageInit payload.
pub(in crate::storage::wal) const PAGE_INIT_PAGE_TYPE_OFFSET: usize =
	PAGE_FRAME_FILE_ID_OFFSET + size_of::<u64>();
/// Reserved bytes inside a PageInit payload.
pub(in crate::storage::wal) const PAGE_INIT_RESERVED_OFFSET: usize =
	PAGE_INIT_PAGE_TYPE_OFFSET + size_of::<u16>();
/// Canonical zero padding between PageInit type and page number.
pub(in crate::storage::wal) const PAGE_INIT_RESERVED_BYTES: usize = 6;
/// Page-number offset inside a PageInit payload.
pub(in crate::storage::wal) const PAGE_INIT_PAGE_NO_OFFSET: usize =
	PAGE_INIT_RESERVED_OFFSET + PAGE_INIT_RESERVED_BYTES;
/// Exact PageInit payload size.
pub(in crate::storage::wal) const PAGE_INIT_PAYLOAD_SIZE: usize =
	PAGE_INIT_PAGE_NO_OFFSET + size_of::<u64>();
/// Checkpoint generation offset inside CheckpointEnd.
pub(in crate::storage::wal) const CHECKPOINT_GENERATION_OFFSET: usize = 0;
/// Backfill watermark offset inside CheckpointEnd.
pub(in crate::storage::wal) const CHECKPOINT_BACKFILL_LSN_OFFSET: usize =
	CHECKPOINT_GENERATION_OFFSET + size_of::<u64>();
/// Exact CheckpointEnd payload size.
pub(in crate::storage::wal) const CHECKPOINT_END_PAYLOAD_SIZE: usize =
	CHECKPOINT_BACKFILL_LSN_OFFSET + size_of::<u64>();
/// Graph-name length offset inside GraphCreate.
pub(in crate::storage::wal) const GRAPH_CREATE_NAME_LEN_OFFSET: usize = size_of::<u64>();
/// Graph-name bytes offset inside GraphCreate.
pub(in crate::storage::wal) const GRAPH_CREATE_NAME_OFFSET: usize =
	GRAPH_CREATE_NAME_LEN_OFFSET + size_of::<u16>();
/// File suffix used by ordered WAL segments.
pub(in crate::storage::wal) const WAL_SEGMENT_SUFFIX: &str = ".wal";
/// Hexadecimal digits in one zero-padded segment stem.
pub(in crate::storage::wal) const WAL_SEGMENT_STEM_LEN: usize = 16;
/// Numeric radix used by segment stems.
pub(in crate::storage::wal) const WAL_SEGMENT_RADIX: u32 = 16;
/// First optional logical-annotation kind retained for forward consumers.
pub(in crate::storage::wal) const LOGICAL_ANNOTATION_KIND_START: u8 = 0x10;
/// Last optional logical-annotation kind retained for forward consumers.
pub(in crate::storage::wal) const LOGICAL_ANNOTATION_KIND_END: u8 = 0x22;

/// Recovery-relevant record kinds assigned by the disk format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::storage::wal) enum WalKind {
	/// Transaction batch begins.
	TxBegin = 0x30,
	/// Durable transaction marker.
	TxCommit = 0x31,
	/// Explicit abort marker used by future spilled transactions.
	TxAbort = 0x32,
	/// Complete page image.
	PageFrame = 0x33,
	/// Empty page allocation marker.
	PageInit = 0x34,
	/// Checkpoint backfill begins.
	CheckpointBegin = 0x35,
	/// Durable checkpoint marker.
	CheckpointEnd = 0x36,
	/// Graph catalog creation marker.
	GraphCreate = 0x37,
	/// Graph catalog removal marker.
	GraphDrop = 0x38,
}

impl WalKind {
	/// Decodes an assigned recovery record kind.
	pub(in crate::storage::wal) const fn from_raw(value: u8) -> Option<Self> {
		if value == Self::TxBegin.get() {
			Some(Self::TxBegin)
		} else if value == Self::TxCommit.get() {
			Some(Self::TxCommit)
		} else if value == Self::TxAbort.get() {
			Some(Self::TxAbort)
		} else if value == Self::PageFrame.get() {
			Some(Self::PageFrame)
		} else if value == Self::PageInit.get() {
			Some(Self::PageInit)
		} else if value == Self::CheckpointBegin.get() {
			Some(Self::CheckpointBegin)
		} else if value == Self::CheckpointEnd.get() {
			Some(Self::CheckpointEnd)
		} else if value == Self::GraphCreate.get() {
			Some(Self::GraphCreate)
		} else if value == Self::GraphDrop.get() {
			Some(Self::GraphDrop)
		} else {
			None
		}
	}

	/// Returns the assigned format byte.
	pub(in crate::storage::wal) const fn get(self) -> u8 {
		self as u8
	}
}

/// Returns whether a checksummed record is an optional logical annotation.
pub(in crate::storage::wal) const fn is_logical_annotation_kind(kind: u8) -> bool {
	kind >= LOGICAL_ANNOTATION_KIND_START && kind <= LOGICAL_ANNOTATION_KIND_END
}

/// Encodes the fixed PageFrame identity.
pub(in crate::storage::wal) fn encode_page_key(key: PageKey, output: &mut Vec<u8>) {
	output.extend_from_slice(&key.graph_id().get().to_le_bytes());
	output.extend_from_slice(&key.file_id().get().to_le_bytes());
	output.extend_from_slice(&key.page_no().get().to_le_bytes());
}

/// Decodes and validates the fixed PageFrame identity.
pub(in crate::storage::wal) fn decode_page_key(payload: &[u8]) -> FerrosGrynnResult<PageKey> {
	if payload.len() != PAGE_FRAME_PREFIX_SIZE {
		return Err(invalid_wal("invalid PageFrame key length"));
	}
	let file_id = DataFileId::from_raw(read_u64(payload, PAGE_FRAME_FILE_ID_OFFSET))
		.ok_or_else(|| invalid_wal("PageFrame contains an unknown data-file identity"))?;
	Ok(PageKey::new(
		GraphId::new(read_u64(payload, PAGE_FRAME_GRAPH_ID_OFFSET)),
		file_id,
		PageNo::new(read_u64(payload, PAGE_FRAME_PAGE_NO_OFFSET)),
	))
}

/// Encodes one fixed PageInit allocation marker.
pub(in crate::storage::wal) fn encode_page_init(
	key: PageKey,
	page_type: PageType,
) -> [u8; PAGE_INIT_PAYLOAD_SIZE] {
	let mut payload = [0_u8; PAGE_INIT_PAYLOAD_SIZE];
	payload[PAGE_FRAME_GRAPH_ID_OFFSET..PAGE_FRAME_FILE_ID_OFFSET]
		.copy_from_slice(&key.graph_id().get().to_le_bytes());
	payload[PAGE_FRAME_FILE_ID_OFFSET..PAGE_INIT_PAGE_TYPE_OFFSET]
		.copy_from_slice(&key.file_id().get().to_le_bytes());
	payload[PAGE_INIT_PAGE_TYPE_OFFSET..PAGE_INIT_RESERVED_OFFSET]
		.copy_from_slice(&page_type.get().to_le_bytes());
	payload[PAGE_INIT_PAGE_NO_OFFSET..].copy_from_slice(&key.page_no().get().to_le_bytes());
	payload
}

/// Decodes and validates one fixed PageInit allocation marker.
pub(in crate::storage::wal) fn decode_page_init(
	payload: &[u8],
) -> FerrosGrynnResult<(PageKey, PageType)> {
	if payload.len() != PAGE_INIT_PAYLOAD_SIZE {
		return Err(invalid_wal(format!(
			"PageInit payload has {} bytes, expected {PAGE_INIT_PAYLOAD_SIZE}",
			payload.len()
		)));
	}
	if payload[PAGE_INIT_RESERVED_OFFSET..PAGE_INIT_PAGE_NO_OFFSET].iter().any(|byte| *byte != 0) {
		return Err(invalid_wal("PageInit reserved padding is non-zero"));
	}
	let file_id = DataFileId::from_raw(read_u64(payload, PAGE_FRAME_FILE_ID_OFFSET))
		.ok_or_else(|| invalid_wal("PageInit contains an unknown data-file identity"))?;
	let page_type = PageType::from_raw(read_u16(payload, PAGE_INIT_PAGE_TYPE_OFFSET))
		.ok_or_else(|| invalid_wal("PageInit contains an unknown page type"))?;
	Ok((
		PageKey::new(
			GraphId::new(read_u64(payload, PAGE_FRAME_GRAPH_ID_OFFSET)),
			file_id,
			PageNo::new(read_u64(payload, PAGE_INIT_PAGE_NO_OFFSET)),
		),
		page_type,
	))
}

/// Decodes and validates a TxCommit payload.
pub(in crate::storage::wal) fn decode_tx_commit(payload: &[u8]) -> FerrosGrynnResult<(u64, u32)> {
	if payload.len() != TX_COMMIT_PAYLOAD_SIZE {
		return Err(invalid_wal(format!(
			"TxCommit payload has {} bytes, expected {TX_COMMIT_PAYLOAD_SIZE}",
			payload.len()
		)));
	}
	if payload[TX_COMMIT_RESERVED_OFFSET..].iter().any(|byte| *byte != 0) {
		return Err(invalid_wal("TxCommit reserved padding is non-zero"));
	}
	Ok((read_u64(payload, TX_COMMIT_TS_OFFSET), read_u32(payload, TX_COMMIT_FRAME_COUNT_OFFSET)))
}

/// Validates a system record whose payload must be empty.
pub(in crate::storage::wal) fn validate_system_empty(
	tx_id: TxId,
	payload: &[u8],
	kind: &str,
) -> FerrosGrynnResult<()> {
	if tx_id != TxId::SYSTEM || !payload.is_empty() {
		return Err(invalid_wal(format!("{kind} must use system tx_id and empty payload")));
	}
	Ok(())
}

/// Fixed durable checkpoint marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage::wal) struct CheckpointEnd {
	/// Monotonic checkpoint generation.
	pub(in crate::storage::wal) generation: CheckpointGeneration,
	/// Highest commit boundary copied to home files.
	pub(in crate::storage::wal) backfill_lsn: Lsn,
}

impl CheckpointEnd {
	/// Encodes the fixed format-v1 payload.
	pub(in crate::storage::wal) fn encode(self) -> [u8; CHECKPOINT_END_PAYLOAD_SIZE] {
		let mut payload = [0_u8; CHECKPOINT_END_PAYLOAD_SIZE];
		payload[CHECKPOINT_GENERATION_OFFSET..CHECKPOINT_BACKFILL_LSN_OFFSET]
			.copy_from_slice(&self.generation.get().to_le_bytes());
		payload[CHECKPOINT_BACKFILL_LSN_OFFSET..]
			.copy_from_slice(&self.backfill_lsn.get().to_le_bytes());
		payload
	}
}

/// Decodes a fixed CheckpointEnd payload.
pub(in crate::storage::wal) fn decode_checkpoint_end(
	tx_id: TxId,
	payload: &[u8],
) -> FerrosGrynnResult<CheckpointEnd> {
	if tx_id != TxId::SYSTEM || payload.len() != CHECKPOINT_END_PAYLOAD_SIZE {
		return Err(invalid_wal(format!(
			"CheckpointEnd payload has {} bytes, expected {CHECKPOINT_END_PAYLOAD_SIZE}",
			payload.len()
		)));
	}
	let generation = CheckpointGeneration::new(read_u64(payload, CHECKPOINT_GENERATION_OFFSET));
	if generation == CheckpointGeneration::ZERO {
		return Err(invalid_wal("CheckpointEnd generation zero is reserved"));
	}
	Ok(CheckpointEnd {
		generation,
		backfill_lsn: Lsn::new(read_u64(payload, CHECKPOINT_BACKFILL_LSN_OFFSET)),
	})
}

/// Validates the variable GraphCreate payload.
pub(in crate::storage::wal) fn validate_graph_create(payload: &[u8]) -> FerrosGrynnResult<()> {
	if payload.len() < GRAPH_CREATE_NAME_OFFSET {
		return Err(invalid_wal("GraphCreate payload is shorter than its fixed fields"));
	}
	let name_len = read_u16(payload, GRAPH_CREATE_NAME_LEN_OFFSET) as usize;
	let expected = GRAPH_CREATE_NAME_OFFSET
		.checked_add(name_len)
		.ok_or_else(|| invalid_wal("GraphCreate name length overflow"))?;
	if payload.len() != expected {
		return Err(invalid_wal("GraphCreate name length does not match payload"));
	}
	std::str::from_utf8(&payload[GRAPH_CREATE_NAME_OFFSET..])
		.map_err(|source| invalid_wal(format!("GraphCreate name is not UTF-8: {source}")))?;
	Ok(())
}

/// Validates the fixed GraphDrop payload.
pub(in crate::storage::wal) fn validate_graph_drop(payload: &[u8]) -> FerrosGrynnResult<()> {
	if payload.len() != size_of::<u64>() {
		return Err(invalid_wal(format!(
			"GraphDrop payload has {} bytes, expected {}",
			payload.len(),
			size_of::<u64>()
		)));
	}
	Ok(())
}

/// Computes the normative RapidHash V3 checksum over prefix and payload.
pub(in crate::storage::wal) fn wal_checksum(
	prefix: &[u8; WAL_CHECKSUM_PREFIX_SIZE],
	payload: &[u8],
) -> u64 {
	let mut hasher = RapidStreamHasherV3::new(&DEFAULT_RAPID_SECRETS);
	hasher.write(prefix);
	hasher.write(payload);
	hasher.finish()
}

/// Returns the encoded byte count for one complete transaction batch.
pub(in crate::storage::wal) fn transaction_encoded_size(
	frame_count: usize,
) -> FerrosGrynnResult<u64> {
	let frames = u64::try_from(frame_count)
		.map_err(|source| invalid_wal(format!("WAL frame count does not fit u64: {source}")))?;
	let frame_record = (WAL_HEADER_SIZE + PAGE_FRAME_PAYLOAD_SIZE) as u64;
	let frames_bytes = frames
		.checked_mul(frame_record)
		.ok_or_else(|| invalid_wal("WAL transaction frame size overflow"))?;
	(WAL_HEADER_SIZE as u64)
		.checked_add(frames_bytes)
		.and_then(|size| size.checked_add((WAL_HEADER_SIZE + TX_COMMIT_PAYLOAD_SIZE) as u64))
		.ok_or_else(|| invalid_wal("WAL transaction size overflow"))
}

/// Returns the path for one strict hexadecimal segment name.
pub(in crate::storage::wal) fn segment_path(directory: &Path, start_lsn: Lsn) -> PathBuf {
	directory.join(format!(
		"{:0width$x}{WAL_SEGMENT_SUFFIX}",
		start_lsn.get(),
		width = WAL_SEGMENT_STEM_LEN
	))
}

/// Reads one little-endian u16 from a validated fixed-size payload.
pub(in crate::storage::wal) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0_u8; size_of::<u16>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u16>()]);
	u16::from_le_bytes(value)
}

/// Reads one little-endian u32 from a validated fixed-size payload.
pub(in crate::storage::wal) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0_u8; size_of::<u32>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian u64 from a validated fixed-size payload.
pub(in crate::storage::wal) fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0_u8; size_of::<u64>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(value)
}

/// Creates a structured storage-format error.
pub(in crate::storage::wal) fn invalid_wal(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

/// Preserves the native I/O source behind the project status boundary.
pub(in crate::storage::wal) fn io_failure(
	operation: impl Into<String>,
	source: io::Error,
) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}
