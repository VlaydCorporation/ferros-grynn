//! Database-level frame-WAL transport, commit publication, and recovery.
//!
//! Transactions stage complete page images in memory. The single WAL writer
//! assigns frame LSNs under its commit lock, reseals every page, appends one
//! contiguous transaction, performs one durability barrier, and only then
//! publishes versions to readers.

use std::{
	collections::BTreeMap,
	fs::{self, File, OpenOptions},
	io::{self, BufWriter, Read, Seek, SeekFrom, Write},
	path::{Path, PathBuf},
	sync::{
		Arc,
		Weak,
		atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
	},
	time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock, RwLockReadGuard};
use rapidhash::{
	RapidHashMap,
	RapidHashSet,
	v3::{DEFAULT_RAPID_SECRETS, RapidStreamHasherV3},
};

use super::{
	page::{AlignedPage, PAGE_SIZE, PageHeader, PageLsn, PageNo, PageType, validate_page_bytes},
	types::{
		CheckpointGeneration,
		CommitTs,
		DataFileId,
		DataFileKind,
		GraphId,
		Lsn,
		PageKey,
		StorageSnapshot,
		TxId,
	},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Typed marker at the start of every frame-WAL record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct WalMagic([u8; 4]);

impl WalMagic {
	/// Unreleased stage-3a marker whose payload identity is incompatible.
	const EXPERIMENTAL_3A: Self = Self(*b"MVCE");
	/// Assigned marker for the format-v1 database-level frame-WAL stream.
	const FRAME_V1: Self = Self(*b"FGW1");

	/// Returns the exact persisted bytes.
	const fn get(self) -> [u8; 4] {
		self.0
	}
}

/// Byte offset of the record LSN after the magic.
const WAL_LSN_OFFSET: usize = size_of::<WalMagic>();
/// Byte offset of the transaction identity.
const WAL_TX_ID_OFFSET: usize = WAL_LSN_OFFSET + size_of::<u64>();
/// Byte offset of the record-kind discriminant.
const WAL_KIND_OFFSET: usize = WAL_TX_ID_OFFSET + size_of::<u64>();
/// Byte offset of the encoded payload length.
const WAL_PAYLOAD_LEN_OFFSET: usize = WAL_KIND_OFFSET + size_of::<u8>();
/// Byte offset of the record checksum.
const WAL_CHECKSUM_OFFSET: usize = WAL_PAYLOAD_LEN_OFFSET + size_of::<u32>();
/// Exact format-v1 WAL record-header size.
const WAL_HEADER_SIZE: usize = WAL_CHECKSUM_OFFSET + size_of::<u64>();
/// Transaction identity offset inside the checksum prefix.
const CHECKSUM_TX_ID_OFFSET: usize = size_of::<u64>();
/// Record-kind offset inside the checksum prefix.
const CHECKSUM_KIND_OFFSET: usize = CHECKSUM_TX_ID_OFFSET + size_of::<u64>();
/// Payload-length offset inside the checksum prefix.
const CHECKSUM_PAYLOAD_LEN_OFFSET: usize = CHECKSUM_KIND_OFFSET + size_of::<u8>();
/// Header bytes covered by the checksum before the payload.
const WAL_CHECKSUM_PREFIX_SIZE: usize = CHECKSUM_PAYLOAD_LEN_OFFSET + size_of::<u32>();
/// Default segment target; an oversized transaction owns one larger segment.
const DEFAULT_SEGMENT_TARGET_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum unique staged page bytes in one synchronous transaction.
const MAX_TX_WRITE_BYTES: usize = 256 * 1024 * 1024;
/// Graph identity offset inside a PageFrame key.
const PAGE_FRAME_GRAPH_ID_OFFSET: usize = 0;
/// Data-file identity offset inside a PageFrame key.
const PAGE_FRAME_FILE_ID_OFFSET: usize = PAGE_FRAME_GRAPH_ID_OFFSET + size_of::<u64>();
/// Page-number offset inside a PageFrame key.
const PAGE_FRAME_PAGE_NO_OFFSET: usize = PAGE_FRAME_FILE_ID_OFFSET + size_of::<u64>();
/// Fixed PageFrame fields before the complete page image.
const PAGE_FRAME_PREFIX_SIZE: usize = PAGE_FRAME_PAGE_NO_OFFSET + size_of::<u64>();
/// Exact PageFrame payload size.
const PAGE_FRAME_PAYLOAD_SIZE: usize = PAGE_FRAME_PREFIX_SIZE + PAGE_SIZE;
/// Largest assigned format-v1 record payload, checked before allocation.
const MAX_WAL_PAYLOAD_BYTES: usize = PAGE_FRAME_PAYLOAD_SIZE;
/// Commit timestamp offset inside a TxCommit payload.
const TX_COMMIT_TS_OFFSET: usize = 0;
/// Frame-count offset inside a TxCommit payload.
const TX_COMMIT_FRAME_COUNT_OFFSET: usize = TX_COMMIT_TS_OFFSET + size_of::<u64>();
/// Reserved-padding offset inside a TxCommit payload.
const TX_COMMIT_RESERVED_OFFSET: usize = TX_COMMIT_FRAME_COUNT_OFFSET + size_of::<u32>();
/// Exact TxCommit payload size.
const TX_COMMIT_PAYLOAD_SIZE: usize = TX_COMMIT_RESERVED_OFFSET + size_of::<u32>();
/// Page-type offset inside a PageInit payload.
const PAGE_INIT_PAGE_TYPE_OFFSET: usize = PAGE_FRAME_FILE_ID_OFFSET + size_of::<u64>();
/// Reserved bytes inside a PageInit payload.
const PAGE_INIT_RESERVED_OFFSET: usize = PAGE_INIT_PAGE_TYPE_OFFSET + size_of::<u16>();
/// Canonical zero padding between PageInit type and page number.
const PAGE_INIT_RESERVED_BYTES: usize = 6;
/// Page-number offset inside a PageInit payload.
const PAGE_INIT_PAGE_NO_OFFSET: usize = PAGE_INIT_RESERVED_OFFSET + PAGE_INIT_RESERVED_BYTES;
/// Exact PageInit payload size.
const PAGE_INIT_PAYLOAD_SIZE: usize = PAGE_INIT_PAGE_NO_OFFSET + size_of::<u64>();
/// Checkpoint generation offset inside CheckpointEnd.
const CHECKPOINT_GENERATION_OFFSET: usize = 0;
/// Backfill watermark offset inside CheckpointEnd.
const CHECKPOINT_BACKFILL_LSN_OFFSET: usize = CHECKPOINT_GENERATION_OFFSET + size_of::<u64>();
/// Exact CheckpointEnd payload size.
const CHECKPOINT_END_PAYLOAD_SIZE: usize = CHECKPOINT_BACKFILL_LSN_OFFSET + size_of::<u64>();
/// Graph-name length offset inside GraphCreate.
const GRAPH_CREATE_NAME_LEN_OFFSET: usize = size_of::<u64>();
/// Graph-name bytes offset inside GraphCreate.
const GRAPH_CREATE_NAME_OFFSET: usize = GRAPH_CREATE_NAME_LEN_OFFSET + size_of::<u16>();
/// File suffix used by ordered WAL segments.
const WAL_SEGMENT_SUFFIX: &str = ".wal";
/// Hexadecimal digits in one zero-padded segment stem.
const WAL_SEGMENT_STEM_LEN: usize = 16;
/// Numeric radix used by segment stems.
const WAL_SEGMENT_RADIX: u32 = 16;
/// First optional logical-annotation kind retained for forward consumers.
const LOGICAL_ANNOTATION_KIND_START: u8 = 0x10;
/// Last optional logical-annotation kind retained for forward consumers.
const LOGICAL_ANNOTATION_KIND_END: u8 = 0x22;

/// Result published after a transaction's single durability barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommitOutcome {
	/// Database-wide transaction identity.
	tx_id: TxId,
	/// Logical commit clock after this operation.
	commit_ts: CommitTs,
	/// Durable WAL boundary after this operation.
	commit_lsn: Lsn,
}

/// Persisted database clocks that bound WAL recovery after prefix pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RecoverySeed {
	/// Database-authoritative checkpoint generation.
	generation: CheckpointGeneration,
	/// Highest commit boundary known to be present in home files.
	checkpoint_lsn: Lsn,
	/// Record LSN of the matching durable CheckpointEnd.
	checkpoint_record_lsn: Lsn,
	/// Highest transaction commit published before the superblock replacement.
	durable_visible_lsn: Lsn,
	/// First WAL record LSN not assigned by the persisted checkpoint.
	next_lsn: Lsn,
	/// Highest assigned logical commit timestamp.
	commit_ts: CommitTs,
	/// First transaction identity not assigned by the persisted checkpoint.
	next_tx_id: TxId,
	/// First data-file instance identity not assigned by persisted state.
	next_file_instance_id: u64,
}

impl RecoverySeed {
	/// Empty database seed used by creation and legacy component tests.
	const EMPTY: Self = Self {
		generation: CheckpointGeneration::ZERO,
		checkpoint_lsn: Lsn::ZERO,
		checkpoint_record_lsn: Lsn::ZERO,
		durable_visible_lsn: Lsn::ZERO,
		next_lsn: Lsn::FIRST,
		commit_ts: CommitTs::ZERO,
		next_tx_id: TxId::new(1),
		next_file_instance_id: 0,
	};

	/// Creates a complete persisted recovery boundary.
	pub(super) const fn new(
		generation: CheckpointGeneration,
		checkpoint_lsn: Lsn,
		checkpoint_record_lsn: Lsn,
		durable_visible_lsn: Lsn,
		next_lsn: Lsn,
		commit_ts: CommitTs,
		next_tx_id: TxId,
		next_file_instance_id: u64,
	) -> Self {
		Self {
			generation,
			checkpoint_lsn,
			checkpoint_record_lsn,
			durable_visible_lsn,
			next_lsn,
			commit_ts,
			next_tx_id,
			next_file_instance_id,
		}
	}
}

/// Consistent WAL clocks captured under the single writer lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WalClocks {
	/// First unassigned WAL record LSN.
	pub(super) next_lsn: Lsn,
	/// Highest durable transaction commit marker.
	pub(super) durable_visible_lsn: Lsn,
	/// Highest assigned logical commit timestamp.
	pub(super) commit_ts: CommitTs,
	/// First unassigned transaction identity.
	pub(super) next_tx_id: TxId,
	/// First unassigned data-file instance identity.
	pub(super) next_file_instance_id: u64,
}

/// Immutable passive-checkpoint work selected under the WAL writer lock.
pub(super) struct CheckpointTicket {
	/// Generation assigned to this checkpoint attempt.
	generation: CheckpointGeneration,
	/// Reader-safe home-file backfill boundary.
	target: Lsn,
	/// Latest complete frame for every page at or before `target`.
	frames: Vec<ResolvedFrame>,
}

impl CheckpointTicket {
	/// Returns the assigned generation.
	pub(super) const fn generation(&self) -> CheckpointGeneration {
		self.generation
	}

	/// Returns the reader-safe backfill boundary.
	pub(super) const fn target(&self) -> Lsn {
		self.target
	}

	/// Returns the immutable page-frame plan.
	pub(super) fn frames(&self) -> &[ResolvedFrame] {
		&self.frames
	}
}

/// Durable CheckpointEnd and clocks awaiting the database commit point.
pub(super) struct CheckpointCommit {
	/// Generation assigned to the durable CheckpointEnd.
	generation: CheckpointGeneration,
	/// Reader-safe backfill boundary.
	target: Lsn,
	/// LSN of the durable CheckpointEnd record.
	checkpoint_record_lsn: Lsn,
	/// Consistent clocks captured with that record.
	clocks: WalClocks,
}

impl CheckpointCommit {
	/// Returns the assigned checkpoint generation.
	pub(super) const fn generation(&self) -> CheckpointGeneration {
		self.generation
	}

	/// Returns the home-file backfill boundary.
	pub(super) const fn target(&self) -> Lsn {
		self.target
	}

	/// Returns the matching durable CheckpointEnd record LSN.
	pub(super) const fn checkpoint_record_lsn(&self) -> Lsn {
		self.checkpoint_record_lsn
	}

	/// Returns the writer-consistent persisted clocks.
	pub(super) const fn clocks(&self) -> WalClocks {
		self.clocks
	}
}

impl CommitOutcome {
	/// Returns the committed transaction identity.
	pub(super) const fn tx_id(self) -> TxId {
		self.tx_id
	}

	/// Returns the published commit timestamp.
	pub(super) const fn commit_ts(self) -> CommitTs {
		self.commit_ts
	}

	/// Returns the published commit-marker LSN.
	pub(super) const fn commit_lsn(self) -> Lsn {
		self.commit_lsn
	}
}

/// Recovery-relevant record kinds assigned by the disk format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum WalKind {
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
	const fn from_raw(value: u8) -> Option<Self> {
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
	const fn get(self) -> u8 {
		self as u8
	}
}

/// Returns whether a checksummed record is an optional logical annotation.
const fn is_logical_annotation_kind(kind: u8) -> bool {
	kind >= LOGICAL_ANNOTATION_KIND_START && kind <= LOGICAL_ANNOTATION_KIND_END
}

/// Segment-qualified location of complete page bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameLocation {
	/// First record LSN encoded in the segment filename.
	segment_start_lsn: Lsn,
	/// Byte offset of the complete page image inside that segment.
	page_bytes_offset: u64,
}

/// One committed page version retained by the in-memory index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameVersion {
	/// LSN written into the page image.
	frame_lsn: Lsn,
	/// Commit marker controlling snapshot visibility.
	commit_lsn: Lsn,
	/// Segment-qualified physical WAL location.
	location: FrameLocation,
}

/// Resolved immutable frame that can be loaded without another index lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResolvedFrame {
	/// Physical page identity validated during recovery or commit.
	key: PageKey,
	/// Selected version metadata.
	version: FrameVersion,
}

/// Snapshot-visible frame resolutions protected from concurrent pruning.
pub(super) struct VisibleFrames<'wal> {
	/// WAL used to load the resolved physical locations.
	wal: &'wal FrameWal,
	/// Shared gate retained until every selected frame has been loaded.
	_prune_gate: RwLockReadGuard<'wal, ()>,
	/// Input-ordered frame choices; `None` means read the home file.
	frames: Vec<Option<ResolvedFrame>>,
}

impl VisibleFrames<'_> {
	/// Returns the input-ordered frame choices.
	pub(super) fn frames(&self) -> &[Option<ResolvedFrame>] {
		&self.frames
	}

	/// Loads one frame while this set prevents index and segment pruning.
	///
	/// # Errors
	///
	/// Returns an I/O, page-validation, or version-index mismatch error.
	pub(super) fn read_frame(
		&self,
		frame: ResolvedFrame,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		self.wal.read_frame_unlocked(frame, output)
	}
}

impl ResolvedFrame {
	/// Returns the selected page key.
	pub(super) const fn key(self) -> PageKey {
		self.key
	}

	/// Returns the selected frame LSN.
	pub(super) const fn frame_lsn(self) -> Lsn {
		self.version.frame_lsn
	}

	/// Returns the commit marker controlling visibility.
	pub(super) const fn commit_lsn(self) -> Lsn {
		self.version.commit_lsn
	}
}

/// One page image staged privately by a transaction.
struct StagedPage {
	/// Aligned complete page copied from the higher storage layer.
	page: Box<AlignedPage>,
}

/// Snapshot registration shared with checkpoint cancellation.
struct ActiveSnapshot {
	/// Durable visibility captured at transaction begin.
	snapshot: StorageSnapshot,
	/// Shared liveness flag observed by every use of the handle.
	active: Arc<AtomicBool>,
	/// Cooperative expiry used by passive checkpoint maintenance.
	expires_at: Option<Instant>,
}

/// Private frame write-set and snapshot registration.
pub(super) struct TxHandle {
	/// Database-wide transaction identity.
	id: TxId,
	/// Snapshot captured before any staging.
	snapshot: StorageSnapshot,
	/// Final page image per physical key.
	staged: BTreeMap<PageKey, StagedPage>,
	/// Unique staged page bytes charged to the write-set limit.
	staged_bytes: usize,
	/// WAL that owns the active snapshot registration.
	owner: Weak<FrameWalInner>,
	/// Shared liveness changed by drop, timeout, or restart checkpoint.
	active: Arc<AtomicBool>,
	/// Cooperative expiry checked before every storage operation.
	expires_at: Option<Instant>,
	/// Configured age reported by the typed timeout status.
	max_age_seconds: Option<u64>,
}

impl TxHandle {
	/// Returns this transaction's stable identity.
	pub(super) const fn id(&self) -> TxId {
		self.id
	}

	/// Returns the snapshot captured at begin.
	pub(super) const fn snapshot(&self) -> StorageSnapshot {
		self.snapshot
	}

	/// Returns whether this snapshot remains registered and unexpired.
	pub(super) fn is_active(&self) -> bool {
		self.active.load(AtomicOrdering::Acquire)
			&& !self.expires_at.is_some_and(|deadline| Instant::now() >= deadline)
	}

	/// Cancels this snapshot and releases its checkpoint watermark.
	pub(super) fn cancel(&self) {
		self.release_snapshot();
	}

	/// Stages the final complete image for one physical page.
	///
	/// Replacing the same key does not increase the bounded write-set charge.
	/// The frame LSN is deliberately left unchanged until commit owns the WAL
	/// writer lock.
	///
	/// # Errors
	///
	/// Returns an invalid-page error or a typed write-set limit error.
	pub(super) fn stage_page(&mut self, key: PageKey, page: &AlignedPage) -> FerrosGrynnResult<()> {
		if key.file_id().kind() == Some(DataFileKind::GraphSuperblock) {
			return Err(invalid_wal(
				"GraphSuperblock is checkpoint-owned and cannot be staged as a transaction frame",
			));
		}
		page.validate(key.page_no())?;
		if !self.staged.contains_key(&key) {
			let next_bytes = self.staged_bytes.checked_add(PAGE_SIZE).ok_or_else(|| {
				FerrosGrynnError::transaction_write_set_limit_exceeded(
					self.id.get(),
					u64::MAX,
					MAX_TX_WRITE_BYTES as u64,
				)
			})?;
			if next_bytes > MAX_TX_WRITE_BYTES {
				return Err(FerrosGrynnError::transaction_write_set_limit_exceeded(
					self.id.get(),
					next_bytes as u64,
					MAX_TX_WRITE_BYTES as u64,
				));
			}
			self.staged_bytes = next_bytes;
		}
		let mut staged = Box::new(AlignedPage::zeroed());
		staged.as_bytes_mut().copy_from_slice(page.as_bytes());
		self.staged.insert(key, StagedPage {
			page: staged,
		});
		Ok(())
	}

	/// Returns a staged image for read-your-writes without copying it.
	pub(super) fn staged_page(&self, key: PageKey) -> Option<&AlignedPage> {
		self.staged.get(&key).map(|staged| staged.page.as_ref())
	}

	/// Releases the active snapshot registration exactly once.
	fn release_snapshot(&self) {
		if !self.active.swap(false, AtomicOrdering::AcqRel) {
			return;
		}
		if let Some(owner) = self.owner.upgrade() {
			owner.active_snapshots.lock().remove(&self.id);
		}
	}
}

impl Drop for TxHandle {
	/// Aborting an in-memory write-set only releases its snapshot registration.
	fn drop(&mut self) {
		self.release_snapshot();
	}
}

/// Cloneable database-level frame-WAL handle.
#[derive(Clone)]
pub(super) struct FrameWal {
	/// Shared single-writer and read-mostly publication state.
	inner: Arc<FrameWalInner>,
}

/// Shared WAL state retained by transactions and handles.
struct FrameWalInner {
	/// Directory containing ordered hexadecimal WAL segments.
	directory: PathBuf,
	/// Rotation target applied only before a transaction batch.
	segment_target_bytes: u64,
	/// Single commit writer and current append segment.
	writer: Mutex<WriterState>,
	/// Durable versions visible to snapshots.
	published: RwLock<PublishedState>,
	/// Active snapshots used by the future checkpoint watermark.
	active_snapshots: Mutex<RapidHashMap<TxId, ActiveSnapshot>>,
	/// Serializes snapshot capture+registration against watermark capture.
	snapshot_gate: RwLock<()>,
	/// Protects resolved frame loading against index and segment pruning.
	prune_gate: RwLock<()>,
	/// Next transaction identity; relaxed ordering only allocates numbers.
	next_tx_id: AtomicU64,
}

/// Mutable state protected by the database commit lock.
struct WriterState {
	/// Segment currently accepting complete transaction batches.
	segment: ActiveSegment,
	/// Next record LSN reserved by append.
	next_lsn: Lsn,
	/// Whether the current append stream may accept another transaction.
	health: WriterHealth,
}

/// Reuse state of one in-process WAL writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterHealth {
	/// No failed append or durability barrier has occurred.
	Ready,
	/// Recovery and reopen are required before another commit.
	Poisoned,
}

/// One open buffered append segment.
struct ActiveSegment {
	/// First LSN represented by the segment filename.
	start_lsn: Lsn,
	/// Buffered sequential writer over the native file handle.
	writer: BufWriter<File>,
	/// Complete bytes currently in the segment.
	length: u64,
}

/// Read-mostly state changed only after a successful durability barrier.
#[derive(Default)]
struct PublishedState {
	/// Highest published logical commit timestamp.
	commit_ts: u64,
	/// Highest durable published commit marker.
	durable_visible_lsn: u64,
	/// Latest valid checkpoint marker observed during recovery.
	checkpoint_lsn: u64,
	/// Latest complete checkpoint generation observed in WAL.
	checkpoint_generation: u64,
	/// Record LSN of the latest complete checkpoint marker.
	checkpoint_record_lsn: u64,
	/// Highest durable CheckpointEnd generation, including an uncommitted
	/// superblock attempt.
	max_checkpoint_generation: u64,
	/// Highest backfill target encoded by any durable CheckpointEnd.
	max_checkpoint_backfill_lsn: u64,
	/// Committed page versions ordered by frame LSN.
	versions: RapidHashMap<PageKey, Vec<FrameVersion>>,
	/// Latest commit marker per page for conservative page-level FCW.
	latest_commit: RapidHashMap<PageKey, Lsn>,
	/// First data-file instance identity not observed in persisted WAL.
	next_file_instance_id: u64,
}

impl FrameWal {
	/// Creates a new WAL directory and its first empty segment.
	///
	/// # Errors
	///
	/// Returns an I/O error if the directory or segment already exists or
	/// cannot be created.
	pub(super) fn create_new(directory: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::create_new_with_target(directory, DEFAULT_SEGMENT_TARGET_BYTES)
	}

	/// Creates a WAL with an explicit segment target used by focused tests.
	///
	/// # Errors
	///
	/// Returns the same creation errors as [`Self::create_new`].
	fn create_new_with_target(
		directory: impl AsRef<Path>,
		segment_target_bytes: u64,
	) -> FerrosGrynnResult<Self> {
		if segment_target_bytes == 0 {
			return Err(invalid_wal("the WAL segment target must be non-zero"));
		}
		let directory = directory.as_ref().to_path_buf();
		fs::create_dir(&directory).map_err(|source| io_failure("create WAL directory", source))?;
		let segment = ActiveSegment::create(&directory, Lsn::FIRST)?;
		Ok(Self::from_recovery(
			directory,
			segment_target_bytes,
			segment,
			Lsn::FIRST,
			1,
			PublishedState::default(),
		))
	}

	/// Opens an existing WAL, repairs only a corrupt final tail, and rebuilds
	/// the committed version index in one streaming pass.
	///
	/// # Errors
	///
	/// Returns an error for I/O failure, corruption before the final segment,
	/// an invalid committed record, an LSN gap, or clock overflow.
	pub(super) fn open(directory: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open_with_seed(directory, RecoverySeed::EMPTY)
	}

	/// Opens an existing retained WAL using authoritative persisted clocks.
	///
	/// # Errors
	///
	/// Returns an error for an illegal pruned prefix, a gap after the persisted
	/// checkpoint, a missing matching CheckpointEnd, corruption, or clock
	/// exhaustion.
	pub(super) fn open_with_seed(
		directory: impl AsRef<Path>,
		seed: RecoverySeed,
	) -> FerrosGrynnResult<Self> {
		validate_recovery_seed(seed)?;
		let directory = directory.as_ref().to_path_buf();
		let recovery = recover_directory(&directory, seed)?;
		let scanned_next_lsn = if recovery.last_lsn == Lsn::ZERO {
			Lsn::FIRST
		} else {
			recovery.last_lsn.checked_next()?
		};
		let next_lsn = Lsn::new(scanned_next_lsn.get().max(seed.next_lsn.get()));
		let scanned_next_tx_id = recovery
			.max_tx_id
			.checked_add(1)
			.ok_or_else(|| invalid_wal("the WAL transaction clock is exhausted"))?;
		let next_tx_id = scanned_next_tx_id.max(seed.next_tx_id.get());
		let segment = if let Some(last_segment) = recovery.last_segment {
			ActiveSegment::open(last_segment)?
		} else {
			ActiveSegment::create(&directory, next_lsn)?
		};
		Ok(Self::from_recovery(
			directory,
			DEFAULT_SEGMENT_TARGET_BYTES,
			segment,
			next_lsn,
			next_tx_id,
			recovery.published,
		))
	}

	/// Builds shared state from either an empty or recovered WAL.
	fn from_recovery(
		directory: PathBuf,
		segment_target_bytes: u64,
		segment: ActiveSegment,
		next_lsn: Lsn,
		next_tx_id: u64,
		published: PublishedState,
	) -> Self {
		Self {
			inner: Arc::new(FrameWalInner {
				directory,
				segment_target_bytes,
				writer: Mutex::new(WriterState {
					segment,
					next_lsn,
					health: WriterHealth::Ready,
				}),
				published: RwLock::new(published),
				active_snapshots: Mutex::new(RapidHashMap::default()),
				snapshot_gate: RwLock::new(()),
				prune_gate: RwLock::new(()),
				next_tx_id: AtomicU64::new(next_tx_id),
			}),
		}
	}

	/// Captures the currently durable storage snapshot.
	pub(super) fn snapshot(&self) -> StorageSnapshot {
		let published = self.inner.published.read();
		StorageSnapshot::new(
			CommitTs::new(published.commit_ts),
			Lsn::new(published.durable_visible_lsn),
		)
	}

	/// Starts a private write-set and registers its snapshot.
	///
	/// # Errors
	///
	/// Returns an error when the transaction identity clock is exhausted.
	pub(super) fn begin_tx(&self) -> FerrosGrynnResult<TxHandle> {
		self.begin_tx_with_max_age(None)
	}

	/// Starts a private write-set whose snapshot expires after `max_age`.
	pub(super) fn begin_tx_with_max_age(
		&self,
		max_age: Option<Duration>,
	) -> FerrosGrynnResult<TxHandle> {
		let _snapshot_gate = self.inner.snapshot_gate.read();
		let raw_tx_id = self
			.inner
			.next_tx_id
			.fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |current| {
				current.checked_add(1)
			})
			.map_err(|current| {
				invalid_wal(format!("the WAL transaction clock is exhausted at {current}"))
			})?;
		let id = TxId::new(raw_tx_id);
		let snapshot = self.snapshot();
		let expires_at = max_age
			.map(|age| {
				Instant::now().checked_add(age).ok_or_else(|| {
					invalid_wal("the configured snapshot age exceeds the monotonic clock range")
				})
			})
			.transpose()?;
		let active = Arc::new(AtomicBool::new(true));
		self.inner.active_snapshots.lock().insert(id, ActiveSnapshot {
			snapshot,
			active: active.clone(),
			expires_at,
		});
		Ok(TxHandle {
			id,
			snapshot,
			staged: BTreeMap::new(),
			staged_bytes: 0,
			owner: Arc::downgrade(&self.inner),
			active,
			expires_at,
			max_age_seconds: max_age.map(|age| age.as_secs()),
		})
	}

	/// Verifies that a transaction belongs to this WAL instance.
	///
	/// # Errors
	///
	/// Returns a storage error for a handle created by another manager.
	pub(super) fn validate_tx(&self, tx: &TxHandle) -> FerrosGrynnResult<()> {
		if !Weak::ptr_eq(&tx.owner, &Arc::downgrade(&self.inner)) {
			return Err(invalid_wal("transaction belongs to another WAL"));
		}
		if tx.expires_at.is_some_and(|deadline| Instant::now() >= deadline) {
			tx.cancel();
			return Err(FerrosGrynnError::snapshot_expired(
				tx.id.get(),
				tx.max_age_seconds.unwrap_or(0),
			));
		}
		if !tx.active.load(AtomicOrdering::Acquire) {
			return Err(invalid_wal("storage snapshot was cancelled"));
		}
		Ok(())
	}

	/// Discards a private in-memory write-set without writing WAL records.
	pub(super) fn abort_tx(_tx: TxHandle) {}

	/// Commits all staged pages as one contiguous and durable WAL batch.
	///
	/// This transport enforces conservative page-level first-committer-wins.
	/// The datastore performs atom-level merge and SSI validation before
	/// opening the fresh serialized storage commit that reaches this layer.
	///
	/// # Errors
	///
	/// Returns a serialization error for a page changed after the transaction
	/// snapshot, a codec or clock error, or an I/O error. Any append/flush/sync
	/// failure poisons this handle until the WAL is reopened and recovered.
	pub(super) fn commit_tx(&self, mut tx: TxHandle) -> FerrosGrynnResult<CommitOutcome> {
		self.validate_tx(&tx)?;
		if tx.staged.is_empty() {
			let snapshot = self.snapshot();
			return Ok(CommitOutcome {
				tx_id: tx.id,
				commit_ts: snapshot.commit_ts(),
				commit_lsn: snapshot.visible_lsn(),
			});
		}

		let mut writer = self.inner.writer.lock();
		if writer.health == WriterHealth::Poisoned {
			return Err(io_failure(
				"append to poisoned WAL; reopen it to recover the tail",
				io::Error::other("previous WAL append or durability barrier failed"),
			));
		}

		let commit_ts = {
			let published = self.inner.published.read();
			for key in tx.staged.keys() {
				if published
					.latest_commit
					.get(key)
					.is_some_and(|latest| *latest > tx.snapshot.visible_lsn())
				{
					// The higher datastore layer rebases atom deltas before opening this
					// serialized storage transaction; this guard still protects direct
					// PageManager callers.
					return Err(FerrosGrynnError::serialization_failure(
						tx.id.get(),
						format!("page {:?} changed after the transaction snapshot", key),
					));
				}
			}
			published
				.commit_ts
				.checked_add(1)
				.map(CommitTs::new)
				.ok_or_else(|| invalid_wal("the WAL commit clock is exhausted"))?
		};

		let transaction_bytes = transaction_encoded_size(tx.staged.len())?;
		if writer.segment.length > 0
			&& writer
				.segment
				.length
				.checked_add(transaction_bytes)
				.is_none_or(|end| end > self.inner.segment_target_bytes)
		{
			writer.rotate(&self.inner.directory)?;
		}

		let append_result = writer.append_transaction(tx.id, commit_ts, &mut tx.staged);
		let (commit_lsn, committed_frames) = match append_result {
			Ok(result) => result,
			Err(error) => {
				writer.health = WriterHealth::Poisoned;
				return Err(error);
			}
		};

		if let Err(source) = writer.segment.writer.flush() {
			writer.health = WriterHealth::Poisoned;
			return Err(io_failure("flush WAL transaction", source));
		}
		if let Err(source) = writer.segment.writer.get_ref().sync_data() {
			writer.health = WriterHealth::Poisoned;
			return Err(io_failure("synchronize WAL transaction", source));
		}

		{
			let mut published = self.inner.published.write();
			for (key, version) in committed_frames {
				published.versions.entry(key).or_default().push(version);
				published.latest_commit.insert(key, commit_lsn);
				published.next_file_instance_id =
					published.next_file_instance_id.max(file_instance_successor(key.file_id()));
			}
			published.commit_ts = commit_ts.get();
			published.durable_visible_lsn = commit_lsn.get();
		}

		Ok(CommitOutcome {
			tx_id: tx.id,
			commit_ts,
			commit_lsn,
		})
	}

	/// Resolves an input-ordered set of page keys and retains the shared prune
	/// gate until the returned set is dropped.
	pub(super) fn visible_frames(
		&self,
		keys: &[PageKey],
		snapshot: StorageSnapshot,
	) -> VisibleFrames<'_> {
		let prune_gate = self.inner.prune_gate.read();
		let published = self.inner.published.read();
		let frames = keys
			.iter()
			.map(|key| {
				published.versions.get(key).and_then(|versions| {
					versions
						.iter()
						.rev()
						.find(|version| version.commit_lsn <= snapshot.visible_lsn())
						.copied()
						.map(|version| ResolvedFrame {
							key: *key,
							version,
						})
				})
			})
			.collect();
		drop(published);
		VisibleFrames {
			wal: self,
			_prune_gate: prune_gate,
			frames,
		}
	}

	/// Reads and validates one previously resolved complete frame.
	///
	/// # Errors
	///
	/// Returns an I/O error, invalid page error, or a mismatch between the
	/// version index and the persisted page LSN.
	pub(super) fn read_frame(
		&self,
		frame: ResolvedFrame,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		let _prune_gate = self.inner.prune_gate.read();
		self.read_frame_unlocked(frame, output)
	}

	/// Loads one resolved frame while the caller holds the shared prune gate.
	fn read_frame_unlocked(
		&self,
		frame: ResolvedFrame,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		let path = segment_path(&self.inner.directory, frame.version.location.segment_start_lsn);
		// ponytail: open per cache miss; add a bounded segment-handle cache only
		// if profiles show this open in the read-path budget.
		let mut file =
			File::open(path).map_err(|source| io_failure("open WAL frame segment", source))?;
		file.seek(SeekFrom::Start(frame.version.location.page_bytes_offset))
			.map_err(|source| io_failure("seek to WAL page frame", source))?;
		read_exact_required(&mut file, output.as_bytes_mut(), "read WAL page frame")?;
		let header = output.validate_mut(frame.key.page_no())?;
		if header.page_lsn().get() != frame.version.frame_lsn.get() {
			return Err(invalid_wal(format!(
				"frame {} contains page LSN {}",
				frame.version.frame_lsn.get(),
				header.page_lsn().get()
			)));
		}
		Ok(header)
	}

	/// Captures a passive-checkpoint target atomically with reader
	/// registration.
	///
	/// Returns `None` when no reader-safe progress beyond the committed home
	/// watermark exists.
	pub(super) fn capture_checkpoint_target(&self) -> Option<Lsn> {
		self.capture_checkpoint_target_with_cancellation(false)
	}

	/// Captures the durable tail after cancelling every active reader.
	pub(super) fn capture_restart_checkpoint_target(&self) -> Option<Lsn> {
		self.capture_checkpoint_target_with_cancellation(true)
	}

	fn capture_checkpoint_target_with_cancellation(&self, restart: bool) -> Option<Lsn> {
		let _snapshot_gate = self.inner.snapshot_gate.write();
		let published = self.inner.published.read();
		let durable = Lsn::new(published.durable_visible_lsn);
		if durable <= Lsn::new(published.checkpoint_lsn) {
			return None;
		}
		let now = Instant::now();
		let mut active = self.inner.active_snapshots.lock();
		active.retain(|_tx_id, registration| {
			let keep = !registration.expires_at.is_some_and(|deadline| now >= deadline);
			if !keep {
				registration.active.store(false, AtomicOrdering::Release);
			}
			keep
		});
		if restart {
			for registration in active.values() {
				registration.active.store(false, AtomicOrdering::Release);
			}
			active.clear();
		}
		let oldest = active
			.values()
			.map(|registration| registration.snapshot.visible_lsn())
			.min()
			.unwrap_or(durable);
		let target = durable.min(oldest);
		(target > Lsn::new(published.checkpoint_lsn)).then_some(target)
	}

	/// Counts unique pages whose latest durable version is not yet in home.
	pub(super) fn unbackfilled_page_count(&self) -> usize {
		let published = self.inner.published.read();
		let checkpoint = Lsn::new(published.checkpoint_lsn);
		published.latest_commit.values().filter(|commit| **commit > checkpoint).count()
	}

	/// Appends CheckpointBegin and snapshots the latest eligible frame per
	/// page.
	///
	/// # Errors
	///
	/// Returns an error for an invalid target, exhausted generation, poisoned
	/// writer, or WAL append failure.
	pub(super) fn begin_checkpoint(
		&self,
		target: Lsn,
		generation_floor: CheckpointGeneration,
	) -> FerrosGrynnResult<CheckpointTicket> {
		let mut writer = self.inner.writer.lock();
		if writer.health == WriterHealth::Poisoned {
			return Err(io_failure(
				"append checkpoint to poisoned WAL; reopen it to recover the tail",
				io::Error::other("previous WAL append or durability barrier failed"),
			));
		}
		let published = self.inner.published.read();
		if target <= Lsn::new(published.checkpoint_lsn)
			|| target > Lsn::new(published.durable_visible_lsn)
		{
			return Err(invalid_wal("passive checkpoint target is outside durable progress"));
		}
		let generation = CheckpointGeneration::new(
			published.max_checkpoint_generation.max(generation_floor.get()),
		)
		.checked_next()?;
		let mut frames = Vec::with_capacity(published.versions.len());
		for (key, versions) in &published.versions {
			if let Some(version) =
				versions.iter().rev().find(|version| version.commit_lsn <= target).copied()
			{
				frames.push(ResolvedFrame {
					key: *key,
					version,
				});
			}
		}
		frames.sort_unstable_by_key(|frame| frame.key);
		drop(published);
		if let Err(error) = writer.append_record(TxId::SYSTEM, WalKind::CheckpointBegin, &[]) {
			writer.health = WriterHealth::Poisoned;
			return Err(error);
		}
		Ok(CheckpointTicket {
			generation,
			target,
			frames,
		})
	}

	/// Appends and synchronizes the fixed CheckpointEnd marker.
	///
	/// The returned state is durable in WAL but does not become authoritative
	/// until the caller atomically replaces the database superblock.
	///
	/// # Errors
	///
	/// Returns an append, flush, sync, or clock error and poisons the writer on
	/// an uncertain WAL durability state.
	pub(super) fn finish_checkpoint(
		&self,
		ticket: CheckpointTicket,
	) -> FerrosGrynnResult<CheckpointCommit> {
		let mut writer = self.inner.writer.lock();
		if writer.health == WriterHealth::Poisoned {
			return Err(io_failure(
				"finish checkpoint on poisoned WAL; reopen it to recover the tail",
				io::Error::other("previous WAL append or durability barrier failed"),
			));
		}
		let payload = CheckpointEnd {
			generation: ticket.generation,
			backfill_lsn: ticket.target,
		}
		.encode();
		let checkpoint_record_lsn = writer.next_lsn;
		if let Err(error) = writer.append_record(TxId::SYSTEM, WalKind::CheckpointEnd, &payload) {
			writer.health = WriterHealth::Poisoned;
			return Err(error);
		}
		if let Err(source) = writer.segment.writer.flush() {
			writer.health = WriterHealth::Poisoned;
			return Err(io_failure("flush WAL checkpoint", source));
		}
		if let Err(source) = writer.segment.writer.get_ref().sync_data() {
			writer.health = WriterHealth::Poisoned;
			return Err(io_failure("synchronize WAL checkpoint", source));
		}
		let clocks = {
			let mut published = self.inner.published.write();
			published.max_checkpoint_generation = ticket.generation.get();
			published.max_checkpoint_backfill_lsn = ticket.target.get();
			WalClocks {
				next_lsn: writer.next_lsn,
				durable_visible_lsn: Lsn::new(published.durable_visible_lsn),
				commit_ts: CommitTs::new(published.commit_ts),
				next_tx_id: TxId::new(self.inner.next_tx_id.load(AtomicOrdering::Relaxed)),
				next_file_instance_id: published.next_file_instance_id,
			}
		};
		Ok(CheckpointCommit {
			generation: ticket.generation,
			target: ticket.target,
			checkpoint_record_lsn,
			clocks,
		})
	}

	/// Publishes a committed home generation and prunes only safe WAL state.
	///
	/// Call this only after the database superblock replacement succeeds.
	/// Latest-commit metadata is deliberately retained for page-level FCW.
	///
	/// # Errors
	///
	/// Returns an error when the commit does not match the durable WAL marker
	/// or a closed segment cannot be removed. Publication remains complete
	/// when only segment deletion fails.
	pub(super) fn publish_checkpoint(
		&self,
		checkpoint: &CheckpointCommit,
	) -> FerrosGrynnResult<()> {
		let _prune_gate = self.inner.prune_gate.write();
		{
			let mut published = self.inner.published.write();
			if checkpoint.generation.get() > published.max_checkpoint_generation
				|| checkpoint.target.get() > published.max_checkpoint_backfill_lsn
			{
				return Err(invalid_wal("checkpoint publication has no matching durable marker"));
			}
			published.checkpoint_generation = checkpoint.generation.get();
			published.checkpoint_lsn = checkpoint.target.get();
			published.checkpoint_record_lsn = checkpoint.checkpoint_record_lsn.get();
			published.versions.retain(|_key, versions| {
				versions.retain(|version| version.commit_lsn > checkpoint.target);
				!versions.is_empty()
			});
		}
		self.prune_closed_segments(checkpoint.target)
	}

	/// Returns committed frames that recovery must replay, ordered by frame
	/// LSN.
	pub(super) fn recovery_frames(&self) -> Vec<ResolvedFrame> {
		let _prune_gate = self.inner.prune_gate.read();
		let published = self.inner.published.read();
		let checkpoint_lsn = Lsn::new(published.checkpoint_lsn);
		let mut frames = published
			.versions
			.iter()
			.flat_map(|(key, versions)| {
				versions.iter().filter_map(move |version| {
					(version.frame_lsn > checkpoint_lsn).then_some(ResolvedFrame {
						key: *key,
						version: *version,
					})
				})
			})
			.collect::<Vec<_>>();
		frames.sort_unstable_by_key(|frame| frame.version.frame_lsn);
		frames
	}

	/// Removes closed segments whose final record is at or before `target`.
	fn prune_closed_segments(&self, target: Lsn) -> FerrosGrynnResult<()> {
		let active_start = self.inner.writer.lock().segment.start_lsn;
		let segments = list_segments(&self.inner.directory)?;
		for pair in segments.windows(2) {
			let segment = &pair[0];
			let last_lsn = pair[1].start_lsn.get().saturating_sub(1);
			if segment.start_lsn != active_start && last_lsn <= target.get() {
				fs::remove_file(&segment.path)
					.map_err(|source| io_failure("prune closed WAL segment", source))?;
			}
		}
		Ok(())
	}

	/// Returns the oldest active reader boundary for checkpoint planning.
	fn oldest_reader_lsn(&self) -> Option<Lsn> {
		self.inner
			.active_snapshots
			.lock()
			.values()
			.map(|registration| registration.snapshot.visible_lsn())
			.min()
	}
}

impl WriterState {
	/// Rotates before a transaction without splitting its records.
	fn rotate(&mut self, directory: &Path) -> FerrosGrynnResult<()> {
		let next = ActiveSegment::create(directory, self.next_lsn)?;
		self.segment = next;
		Ok(())
	}

	/// Appends one transaction and advances record clocks without publishing
	/// it.
	fn append_transaction(
		&mut self,
		tx_id: TxId,
		commit_ts: CommitTs,
		staged: &mut BTreeMap<PageKey, StagedPage>,
	) -> FerrosGrynnResult<(Lsn, Vec<(PageKey, FrameVersion)>)> {
		self.append_record(tx_id, WalKind::TxBegin, &[])?;
		let mut committed_frames = Vec::with_capacity(staged.len());
		let mut payload = Vec::with_capacity(PAGE_FRAME_PAYLOAD_SIZE);
		for (key, staged_page) in staged.iter_mut() {
			let frame_lsn = self.next_lsn;
			staged_page.page.assign_lsn(key.page_no(), PageLsn::new(frame_lsn.get()))?;
			payload.clear();
			encode_page_key(*key, &mut payload);
			payload.extend_from_slice(staged_page.page.as_bytes());
			let record = self.append_record(tx_id, WalKind::PageFrame, &payload)?;
			let page_bytes_offset = record
				.payload_offset
				.checked_add(PAGE_FRAME_PREFIX_SIZE as u64)
				.ok_or_else(|| invalid_wal("WAL page-frame offset overflow"))?;
			committed_frames.push((*key, FrameVersion {
				frame_lsn,
				commit_lsn: Lsn::ZERO,
				location: FrameLocation {
					segment_start_lsn: self.segment.start_lsn,
					page_bytes_offset,
				},
			}));
		}

		let frame_count = u32::try_from(staged.len())
			.map_err(|source| invalid_wal(format!("WAL frame count does not fit u32: {source}")))?;
		let mut commit_payload = [0_u8; TX_COMMIT_PAYLOAD_SIZE];
		commit_payload[TX_COMMIT_TS_OFFSET..TX_COMMIT_FRAME_COUNT_OFFSET]
			.copy_from_slice(&commit_ts.get().to_le_bytes());
		commit_payload[TX_COMMIT_FRAME_COUNT_OFFSET..TX_COMMIT_RESERVED_OFFSET]
			.copy_from_slice(&frame_count.to_le_bytes());
		let commit_lsn = self.next_lsn;
		self.append_record(tx_id, WalKind::TxCommit, &commit_payload)?;
		for (_, version) in &mut committed_frames {
			version.commit_lsn = commit_lsn;
		}
		Ok((commit_lsn, committed_frames))
	}

	/// Appends one checksummed record and reserves its successor LSN.
	fn append_record(
		&mut self,
		tx_id: TxId,
		kind: WalKind,
		payload: &[u8],
	) -> FerrosGrynnResult<RecordLocation> {
		let lsn = self.next_lsn;
		let location = self.segment.append(lsn, tx_id, kind.get(), payload)?;
		self.next_lsn = lsn.checked_next()?;
		Ok(location)
	}
}

impl ActiveSegment {
	/// Creates one empty segment named by its first record LSN.
	fn create(directory: &Path, start_lsn: Lsn) -> FerrosGrynnResult<Self> {
		let path = segment_path(directory, start_lsn);
		let file = OpenOptions::new()
			.read(true)
			.append(true)
			.create_new(true)
			.open(path)
			.map_err(|source| io_failure("create WAL segment", source))?;
		Ok(Self {
			start_lsn,
			writer: BufWriter::new(file),
			length: 0,
		})
	}

	/// Opens the recovered final segment for append.
	fn open(segment: SegmentDescriptor) -> FerrosGrynnResult<Self> {
		let file = OpenOptions::new()
			.read(true)
			.append(true)
			.open(&segment.path)
			.map_err(|source| io_failure("open WAL append segment", source))?;
		let length = file
			.metadata()
			.map_err(|source| io_failure("inspect WAL append segment", source))?
			.len();
		Ok(Self {
			start_lsn: segment.start_lsn,
			writer: BufWriter::new(file),
			length,
		})
	}

	/// Appends one exact format-v1 record without flushing it.
	fn append(
		&mut self,
		lsn: Lsn,
		tx_id: TxId,
		kind: u8,
		payload: &[u8],
	) -> FerrosGrynnResult<RecordLocation> {
		let payload_len = u32::try_from(payload.len()).map_err(|source| {
			invalid_wal(format!("WAL payload length does not fit u32: {source}"))
		})?;
		let record_length = (WAL_HEADER_SIZE as u64)
			.checked_add(u64::from(payload_len))
			.ok_or_else(|| invalid_wal("WAL record length overflow"))?;
		let end = self
			.length
			.checked_add(record_length)
			.ok_or_else(|| invalid_wal("WAL segment length overflow"))?;
		let mut prefix = [0_u8; WAL_CHECKSUM_PREFIX_SIZE];
		prefix[..CHECKSUM_TX_ID_OFFSET].copy_from_slice(&lsn.get().to_le_bytes());
		prefix[CHECKSUM_TX_ID_OFFSET..CHECKSUM_KIND_OFFSET]
			.copy_from_slice(&tx_id.get().to_le_bytes());
		prefix[CHECKSUM_KIND_OFFSET] = kind;
		prefix[CHECKSUM_PAYLOAD_LEN_OFFSET..].copy_from_slice(&payload_len.to_le_bytes());
		let checksum = wal_checksum(&prefix, payload);
		let payload_offset = self
			.length
			.checked_add(WAL_HEADER_SIZE as u64)
			.ok_or_else(|| invalid_wal("WAL payload offset overflow"))?;

		self.writer
			.write_all(&WalMagic::FRAME_V1.get())
			.and_then(|()| self.writer.write_all(&prefix))
			.and_then(|()| self.writer.write_all(&checksum.to_le_bytes()))
			.and_then(|()| self.writer.write_all(payload))
			.map_err(|source| io_failure("append WAL record", source))?;
		self.length = end;
		Ok(RecordLocation {
			payload_offset,
		})
	}
}

/// Byte locations returned after appending one record.
#[derive(Debug, Clone, Copy)]
struct RecordLocation {
	/// Start of the record payload.
	payload_offset: u64,
}

/// One ordered WAL segment discovered on disk.
#[derive(Debug, Clone)]
struct SegmentDescriptor {
	/// First record LSN encoded in the filename.
	start_lsn: Lsn,
	/// Complete native path.
	path: PathBuf,
}

/// Result of the single streaming recovery pass.
struct RecoveryResult {
	/// Rebuilt committed publication state.
	published: PublishedState,
	/// Highest transaction identity in any complete record.
	max_tx_id: u64,
	/// Highest complete and semantically valid record LSN.
	last_lsn: Lsn,
	/// Final segment after optional tail repair.
	last_segment: Option<SegmentDescriptor>,
}

/// Transaction frames retained as locations until a valid commit marker.
#[derive(Default)]
struct PendingTransaction {
	/// Frame metadata in strict record order.
	frames: Vec<(PageKey, Lsn, FrameLocation)>,
	/// Duplicate protection for the one-final-frame-per-key invariant.
	keys: RapidHashSet<PageKey>,
}

/// Mutable semantic recovery state.
#[derive(Default)]
struct RecoveryBuilder {
	/// Durable state reconstructed from committed records.
	published: PublishedState,
	/// Transactions without a validated commit marker.
	pending: RapidHashMap<TxId, PendingTransaction>,
	/// Transaction identities already started anywhere in the retained WAL.
	seen_transactions: RapidHashSet<TxId>,
	/// Highest complete transaction identity, including incomplete batches.
	max_tx_id: u64,
	/// Highest accepted record LSN.
	last_lsn: Lsn,
	/// Complete CheckpointEnd records retained for persisted-seed validation.
	checkpoint_ends: Vec<(Lsn, CheckpointEnd)>,
	/// Whether at least one unmatched CheckpointBegin precedes the next end.
	checkpoint_started: bool,
}

/// One checksummed raw record produced by the streaming codec.
struct RawRecord {
	/// Record position in the global stream.
	lsn: Lsn,
	/// Owning transaction or system identity.
	tx_id: TxId,
	/// Assigned or forward-compatible kind byte.
	kind: u8,
	/// Untrusted payload after bounded allocation and checksum validation.
	payload: Vec<u8>,
	/// Byte locations in the current segment.
	location: RecordLocation,
}

/// Result of reading at a record boundary.
enum RecordRead {
	/// Clean end exactly between records.
	Eof,
	/// Physical EOF inside an otherwise bounded record.
	Truncated(String),
	/// Complete checksummed record.
	Record(RawRecord),
	/// Complete bytes that violate a format or checksum invariant.
	Corrupt(String),
}

/// Exact-read outcome that distinguishes a clean boundary from truncation.
enum ExactRead {
	/// All requested bytes were read.
	Complete,
	/// No byte existed at the requested boundary.
	Eof,
	/// Some but not all requested bytes existed.
	Truncated,
}

/// Validates persisted clocks before they can influence scanner boundaries.
fn validate_recovery_seed(seed: RecoverySeed) -> FerrosGrynnResult<()> {
	if seed.next_lsn == Lsn::ZERO || seed.next_tx_id == TxId::SYSTEM {
		return Err(invalid_wal("persisted next WAL or transaction clock is reserved zero"));
	}
	if seed.next_file_instance_id > DataFileId::EXHAUSTED_INSTANCE_ID {
		return Err(invalid_wal("persisted next file-instance clock is invalid"));
	}
	if seed.checkpoint_lsn > seed.durable_visible_lsn {
		return Err(invalid_wal("persisted checkpoint exceeds durable visibility"));
	}
	if seed.checkpoint_lsn > seed.checkpoint_record_lsn {
		return Err(invalid_wal("persisted checkpoint exceeds its CheckpointEnd record"));
	}
	if seed.generation == CheckpointGeneration::ZERO {
		if seed.checkpoint_lsn != Lsn::ZERO || seed.checkpoint_record_lsn != Lsn::ZERO {
			return Err(invalid_wal("generation zero has non-zero checkpoint clocks"));
		}
	} else if seed.checkpoint_record_lsn == Lsn::ZERO {
		return Err(invalid_wal("persisted checkpoint generation has no record LSN"));
	}
	if seed.next_lsn <= seed.checkpoint_record_lsn || seed.next_lsn <= seed.durable_visible_lsn {
		return Err(invalid_wal("persisted next_lsn does not exceed durable clocks"));
	}
	Ok(())
}

/// Merges scanned clocks with authoritative persisted floors.
fn published_from_seed(seed: RecoverySeed, mut scanned: PublishedState) -> PublishedState {
	scanned.commit_ts = scanned.commit_ts.max(seed.commit_ts.get());
	scanned.durable_visible_lsn = scanned.durable_visible_lsn.max(seed.durable_visible_lsn.get());
	scanned.checkpoint_generation = seed.generation.get();
	scanned.checkpoint_lsn = seed.checkpoint_lsn.get();
	scanned.checkpoint_record_lsn = seed.checkpoint_record_lsn.get();
	scanned.max_checkpoint_generation =
		scanned.max_checkpoint_generation.max(seed.generation.get());
	scanned.max_checkpoint_backfill_lsn =
		scanned.max_checkpoint_backfill_lsn.max(seed.checkpoint_lsn.get());
	scanned.next_file_instance_id = scanned.next_file_instance_id.max(seed.next_file_instance_id);
	scanned
}

/// Recovers ordered segments and truncates only the corrupt tail of the last.
fn recover_directory(directory: &Path, seed: RecoverySeed) -> FerrosGrynnResult<RecoveryResult> {
	let segments = list_segments(directory)?;
	if segments.is_empty() {
		if seed.generation != CheckpointGeneration::ZERO {
			return Err(invalid_wal("persisted checkpoint has no retained WAL CheckpointEnd"));
		}
		return Ok(RecoveryResult {
			published: published_from_seed(seed, PublishedState::default()),
			max_tx_id: 0,
			last_lsn: Lsn::ZERO,
			last_segment: None,
		});
	}
	let checkpoint_successor = seed.checkpoint_lsn.checked_next()?;
	if segments[0].start_lsn > checkpoint_successor {
		return Err(invalid_wal(format!(
			"first retained WAL segment starts at LSN {}, leaving a gap after checkpoint {}",
			segments[0].start_lsn.get(),
			seed.checkpoint_lsn.get()
		)));
	}

	let mut builder = RecoveryBuilder::default();
	let mut expected_lsn = segments[0].start_lsn;
	'recovery: for (index, segment) in segments.iter().enumerate() {
		let is_last = index + 1 == segments.len();
		if segment.start_lsn != expected_lsn {
			return Err(invalid_wal(format!(
				"WAL segment {} starts at LSN {}, expected {}",
				segment.path.display(),
				segment.start_lsn.get(),
				expected_lsn.get()
			)));
		}
		let mut file = OpenOptions::new()
			.read(true)
			.write(is_last)
			.open(&segment.path)
			.map_err(|source| io_failure("open WAL segment for recovery", source))?;
		let mut offset = 0_u64;
		loop {
			let record_offset = offset;
			match read_record(&mut file, record_offset, expected_lsn)? {
				RecordRead::Eof => break,
				RecordRead::Truncated(reason) => {
					if !is_last {
						return Err(invalid_wal(format!(
							"truncated completed WAL segment {} at byte {record_offset}: {reason}",
							segment.path.display()
						)));
					}
					file.set_len(record_offset)
						.map_err(|source| io_failure("truncate corrupt WAL tail", source))?;
					break 'recovery;
				}
				RecordRead::Corrupt(reason) => {
					return Err(invalid_wal(format!(
						"corrupt WAL segment {} at byte {record_offset}: {reason}",
						segment.path.display()
					)));
				}
				RecordRead::Record(record) => {
					if WalKind::from_raw(record.kind).is_none()
						&& !is_logical_annotation_kind(record.kind)
					{
						return Err(invalid_wal(format!(
							"unassigned WAL kind {:#04x}",
							record.kind
						)));
					}
					let payload_len = u64::try_from(record.payload.len()).map_err(|source| {
						invalid_wal(format!("WAL payload length does not fit u64: {source}"))
					})?;
					let next_offset = record
						.location
						.payload_offset
						.checked_add(payload_len)
						.ok_or_else(|| invalid_wal("WAL recovery offset overflow"))?;
					builder.apply(record, segment.start_lsn)?;
					offset = next_offset;
					expected_lsn = expected_lsn.checked_next()?;
				}
			}
		}
		if !is_last && offset == 0 {
			return Err(invalid_wal(format!(
				"completed WAL segment {} is empty",
				segment.path.display()
			)));
		}
		if !is_last && !builder.pending.is_empty() {
			return Err(invalid_wal(format!(
				"WAL segment {} splits an unfinished transaction batch",
				segment.path.display()
			)));
		}
	}

	if seed.generation != CheckpointGeneration::ZERO {
		let matches_seed = builder.checkpoint_ends.iter().any(|(record_lsn, checkpoint)| {
			*record_lsn == seed.checkpoint_record_lsn
				&& checkpoint.generation == seed.generation
				&& checkpoint.backfill_lsn == seed.checkpoint_lsn
		});
		if !matches_seed {
			return Err(invalid_wal(
				"persisted checkpoint does not match its retained CheckpointEnd",
			));
		}
	}
	let scanned_successor = if builder.last_lsn == Lsn::ZERO {
		Lsn::FIRST
	} else {
		builder.last_lsn.checked_next()?
	};
	if scanned_successor < seed.next_lsn {
		return Err(invalid_wal("retained WAL tail ends before persisted next_lsn"));
	}
	for versions in builder.published.versions.values_mut() {
		versions.retain(|version| version.frame_lsn > seed.checkpoint_lsn);
		versions.sort_unstable_by_key(|version| version.frame_lsn);
	}
	builder.published.versions.retain(|_key, versions| !versions.is_empty());
	let published = published_from_seed(seed, builder.published);
	Ok(RecoveryResult {
		published,
		max_tx_id: builder.max_tx_id,
		last_lsn: builder.last_lsn,
		last_segment: segments.last().cloned(),
	})
}

impl RecoveryBuilder {
	/// Validates and applies one complete record without retaining page bytes.
	fn apply(&mut self, record: RawRecord, segment_start_lsn: Lsn) -> FerrosGrynnResult<()> {
		let kind = WalKind::from_raw(record.kind);
		match kind {
			Some(WalKind::TxBegin) => self.apply_tx_begin(record.tx_id, &record.payload)?,
			Some(WalKind::TxCommit) => {
				self.apply_tx_commit(record.tx_id, record.lsn, &record.payload)?;
			}
			Some(WalKind::TxAbort) => self.apply_tx_abort(record.tx_id, &record.payload)?,
			Some(WalKind::PageFrame) => {
				self.apply_page_frame(&record, segment_start_lsn)?;
			}
			Some(WalKind::PageInit) => self.validate_page_init(record.tx_id, &record.payload)?,
			Some(WalKind::CheckpointBegin) => {
				validate_system_empty(record.tx_id, &record.payload, "CheckpointBegin")?;
				self.checkpoint_started = true;
			}
			Some(WalKind::CheckpointEnd) => {
				let checkpoint = decode_checkpoint_end(record.tx_id, &record.payload)?;
				if !self.checkpoint_started {
					return Err(invalid_wal("CheckpointEnd has no preceding CheckpointBegin"));
				}
				if checkpoint.backfill_lsn == Lsn::ZERO
					|| checkpoint.backfill_lsn.get() > self.published.durable_visible_lsn
				{
					return Err(invalid_wal(
						"checkpoint backfill LSN is outside durable transaction progress",
					));
				}
				if checkpoint.generation.get() <= self.published.max_checkpoint_generation {
					return Err(invalid_wal("checkpoint generation is not strictly increasing"));
				}
				if checkpoint.backfill_lsn.get() < self.published.max_checkpoint_backfill_lsn {
					return Err(invalid_wal("checkpoint backfill LSN moved backwards"));
				}
				self.published.max_checkpoint_generation = checkpoint.generation.get();
				self.published.max_checkpoint_backfill_lsn = checkpoint.backfill_lsn.get();
				self.checkpoint_ends.push((record.lsn, checkpoint));
				self.checkpoint_started = false;
			}
			Some(WalKind::GraphCreate) => validate_graph_create(&record.payload)?,
			Some(WalKind::GraphDrop) => validate_graph_drop(&record.payload)?,
			None if is_logical_annotation_kind(record.kind) => {
				// Valid unknown records are forward-compatible annotations.
				// They do not participate in physical recovery.
			}
			None => {
				return Err(invalid_wal(format!("unassigned WAL kind {:#04x}", record.kind)));
			}
		}
		self.max_tx_id = self.max_tx_id.max(record.tx_id.get());
		self.last_lsn = record.lsn;
		Ok(())
	}

	/// Starts one pending transaction.
	fn apply_tx_begin(&mut self, tx_id: TxId, payload: &[u8]) -> FerrosGrynnResult<()> {
		if tx_id == TxId::SYSTEM || !payload.is_empty() {
			return Err(invalid_wal("TxBegin must have a non-zero tx_id and empty payload"));
		}
		if !self.seen_transactions.insert(tx_id) {
			return Err(invalid_wal(format!(
				"reused TxBegin identity for transaction {}",
				tx_id.get()
			)));
		}
		self.pending.insert(tx_id, PendingTransaction::default());
		Ok(())
	}

	/// Publishes one transaction only after validating its durable marker.
	fn apply_tx_commit(
		&mut self,
		tx_id: TxId,
		commit_lsn: Lsn,
		payload: &[u8],
	) -> FerrosGrynnResult<()> {
		let (commit_ts, frame_count) = decode_tx_commit(payload)?;
		let pending = self.pending.get(&tx_id).ok_or_else(|| {
			invalid_wal(format!("TxCommit for transaction {} has no TxBegin", tx_id.get()))
		})?;
		if pending.frames.len() != frame_count as usize {
			return Err(invalid_wal(format!(
				"TxCommit for transaction {} declares {frame_count} frames, found {}",
				tx_id.get(),
				pending.frames.len()
			)));
		}
		if commit_ts <= self.published.commit_ts {
			return Err(invalid_wal(format!(
				"commit timestamp {commit_ts} is not greater than {}",
				self.published.commit_ts
			)));
		}
		let pending = self
			.pending
			.remove(&tx_id)
			.ok_or_else(|| invalid_wal("pending transaction disappeared during recovery"))?;
		for (key, frame_lsn, location) in pending.frames {
			self.published.versions.entry(key).or_default().push(FrameVersion {
				frame_lsn,
				commit_lsn,
				location,
			});
			self.published.latest_commit.insert(key, commit_lsn);
		}
		self.published.commit_ts = commit_ts;
		self.published.durable_visible_lsn = commit_lsn.get();
		Ok(())
	}

	/// Removes an explicitly aborted pending transaction.
	fn apply_tx_abort(&mut self, tx_id: TxId, payload: &[u8]) -> FerrosGrynnResult<()> {
		if !payload.is_empty() {
			return Err(invalid_wal("TxAbort payload must be empty"));
		}
		if self.pending.remove(&tx_id).is_none() {
			return Err(invalid_wal(format!(
				"TxAbort for transaction {} has no TxBegin",
				tx_id.get()
			)));
		}
		Ok(())
	}

	/// Validates and indexes a complete page frame under its pending
	/// transaction.
	fn apply_page_frame(
		&mut self,
		record: &RawRecord,
		segment_start_lsn: Lsn,
	) -> FerrosGrynnResult<()> {
		if record.payload.len() != PAGE_FRAME_PAYLOAD_SIZE {
			return Err(invalid_wal(format!(
				"PageFrame payload has {} bytes, expected {PAGE_FRAME_PAYLOAD_SIZE}",
				record.payload.len()
			)));
		}
		let key = decode_page_key(&record.payload[..PAGE_FRAME_PREFIX_SIZE])?;
		self.published.next_file_instance_id =
			self.published.next_file_instance_id.max(file_instance_successor(key.file_id()));
		let page_bytes: &[u8; PAGE_SIZE] = record.payload[PAGE_FRAME_PREFIX_SIZE..]
			.try_into()
			.map_err(|source| invalid_wal(format!("PageFrame image length mismatch: {source}")))?;
		let header = validate_page_bytes(page_bytes, key.page_no())?;
		if header.page_lsn().get() != record.lsn.get() {
			return Err(invalid_wal(format!(
				"PageFrame record LSN {} contains page LSN {}",
				record.lsn.get(),
				header.page_lsn().get()
			)));
		}
		let pending = self.pending.get_mut(&record.tx_id).ok_or_else(|| {
			invalid_wal(format!("PageFrame for transaction {} has no TxBegin", record.tx_id.get()))
		})?;
		if !pending.keys.insert(key) {
			return Err(invalid_wal(format!(
				"transaction {} contains duplicate PageFrame for {:?}",
				record.tx_id.get(),
				key
			)));
		}
		let page_bytes_offset = record
			.location
			.payload_offset
			.checked_add(PAGE_FRAME_PREFIX_SIZE as u64)
			.ok_or_else(|| invalid_wal("recovered PageFrame offset overflow"))?;
		pending.frames.push((key, record.lsn, FrameLocation {
			segment_start_lsn,
			page_bytes_offset,
		}));
		Ok(())
	}

	/// Validates an allocation marker without publishing page contents.
	fn validate_page_init(&mut self, tx_id: TxId, payload: &[u8]) -> FerrosGrynnResult<()> {
		if !self.pending.contains_key(&tx_id) {
			return Err(invalid_wal(format!(
				"PageInit for transaction {} has no TxBegin",
				tx_id.get()
			)));
		}
		let (key, _page_type) = decode_page_init(payload)?;
		self.published.next_file_instance_id =
			self.published.next_file_instance_id.max(file_instance_successor(key.file_id()));
		Ok(())
	}
}

/// Returns the next file-instance clock after one validated 56-bit identity.
const fn file_instance_successor(file_id: DataFileId) -> u64 {
	file_id.instance_id() + 1
}

/// Reads and validates one raw record without interpreting its kind.
fn read_record(
	file: &mut File,
	record_offset: u64,
	expected_lsn: Lsn,
) -> FerrosGrynnResult<RecordRead> {
	let mut header = [0_u8; WAL_HEADER_SIZE];
	match read_exact_boundary(file, &mut header)
		.map_err(|source| io_failure("read WAL header", source))?
	{
		ExactRead::Eof => return Ok(RecordRead::Eof),
		ExactRead::Truncated => return Ok(RecordRead::Truncated("record header".into())),
		ExactRead::Complete => {}
	}
	if header[..WAL_LSN_OFFSET] == WalMagic::EXPERIMENTAL_3A.get() {
		return Ok(RecordRead::Corrupt("experimental stage-3a WAL format is unsupported".into()));
	}
	if header[..WAL_LSN_OFFSET] != WalMagic::FRAME_V1.get() {
		return Ok(RecordRead::Corrupt("invalid record magic".into()));
	}
	let lsn = Lsn::new(read_u64(&header, WAL_LSN_OFFSET));
	if lsn != expected_lsn {
		return Ok(RecordRead::Corrupt(format!(
			"record LSN {} does not match expected {}",
			lsn.get(),
			expected_lsn.get()
		)));
	}
	let tx_id = TxId::new(read_u64(&header, WAL_TX_ID_OFFSET));
	let kind = header[WAL_KIND_OFFSET];
	let payload_len = read_u32(&header, WAL_PAYLOAD_LEN_OFFSET) as usize;
	if payload_len > MAX_WAL_PAYLOAD_BYTES {
		return Ok(RecordRead::Corrupt(format!(
			"payload length {payload_len} exceeds {MAX_WAL_PAYLOAD_BYTES}"
		)));
	}
	let stored_checksum = read_u64(&header, WAL_CHECKSUM_OFFSET);
	let mut payload = vec![0_u8; payload_len];
	if matches!(
		read_exact_boundary(file, &mut payload)
			.map_err(|source| io_failure("read WAL payload", source))?,
		ExactRead::Eof | ExactRead::Truncated
	) {
		return Ok(RecordRead::Truncated("record payload".into()));
	}
	let mut prefix = [0_u8; WAL_CHECKSUM_PREFIX_SIZE];
	prefix.copy_from_slice(&header[WAL_LSN_OFFSET..WAL_CHECKSUM_OFFSET]);
	let computed_checksum = wal_checksum(&prefix, &payload);
	if stored_checksum != computed_checksum {
		return Ok(RecordRead::Corrupt(format!(
			"checksum mismatch: stored {stored_checksum:#018x}, computed {computed_checksum:#018x}"
		)));
	}
	let payload_offset = record_offset
		.checked_add(WAL_HEADER_SIZE as u64)
		.ok_or_else(|| invalid_wal("recovered WAL payload offset overflow"))?;
	Ok(RecordRead::Record(RawRecord {
		lsn,
		tx_id,
		kind,
		payload,
		location: RecordLocation {
			payload_offset,
		},
	}))
}

/// Reads exactly one buffer while preserving clean-boundary information.
fn read_exact_boundary(reader: &mut File, output: &mut [u8]) -> io::Result<ExactRead> {
	if output.is_empty() {
		return Ok(ExactRead::Complete);
	}
	let mut filled = 0;
	while filled < output.len() {
		match reader.read(&mut output[filled..]) {
			Ok(0) if filled == 0 => return Ok(ExactRead::Eof),
			Ok(0) => return Ok(ExactRead::Truncated),
			Ok(count) => filled += count,
			Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
			Err(source) => return Err(source),
		}
	}
	Ok(ExactRead::Complete)
}

/// Reads a required fixed extent and maps early EOF to a storage error.
fn read_exact_required(
	reader: &mut File,
	output: &mut [u8],
	operation: &str,
) -> FerrosGrynnResult<()> {
	match read_exact_boundary(reader, output).map_err(|source| io_failure(operation, source))? {
		ExactRead::Complete => Ok(()),
		ExactRead::Eof | ExactRead::Truncated => Err(io_failure(
			operation,
			io::Error::new(io::ErrorKind::UnexpectedEof, "short WAL frame read"),
		)),
	}
}

/// Lists and orders strict hexadecimal WAL segment filenames.
fn list_segments(directory: &Path) -> FerrosGrynnResult<Vec<SegmentDescriptor>> {
	let entries =
		fs::read_dir(directory).map_err(|source| io_failure("read WAL directory", source))?;
	let mut segments = Vec::new();
	for entry in entries {
		let entry = entry.map_err(|source| io_failure("read WAL directory entry", source))?;
		let file_type = entry
			.file_type()
			.map_err(|source| io_failure("inspect WAL directory entry", source))?;
		if !file_type.is_file() {
			continue;
		}
		let name = entry.file_name();
		let Some(name) = name.to_str() else {
			continue;
		};
		if !name.ends_with(WAL_SEGMENT_SUFFIX) {
			continue;
		}
		let stem = &name[..name.len() - WAL_SEGMENT_SUFFIX.len()];
		if stem.len() != WAL_SEGMENT_STEM_LEN {
			return Err(invalid_wal(format!("invalid WAL segment filename {name}")));
		}
		let start = u64::from_str_radix(stem, WAL_SEGMENT_RADIX).map_err(|source| {
			invalid_wal(format!("invalid WAL segment filename {name}: {source}"))
		})?;
		if start == 0 {
			return Err(invalid_wal(format!("WAL segment {name} starts at reserved LSN zero")));
		}
		segments.push(SegmentDescriptor {
			start_lsn: Lsn::new(start),
			path: entry.path(),
		});
	}
	segments.sort_unstable_by_key(|segment| segment.start_lsn);
	for pair in segments.windows(2) {
		if pair[0].start_lsn == pair[1].start_lsn {
			return Err(invalid_wal(format!(
				"duplicate WAL segment start LSN {}",
				pair[0].start_lsn.get()
			)));
		}
	}
	Ok(segments)
}

/// Encodes the fixed PageFrame identity.
fn encode_page_key(key: PageKey, output: &mut Vec<u8>) {
	output.extend_from_slice(&key.graph_id().get().to_le_bytes());
	output.extend_from_slice(&key.file_id().get().to_le_bytes());
	output.extend_from_slice(&key.page_no().get().to_le_bytes());
}

/// Decodes and validates the fixed PageFrame identity.
fn decode_page_key(payload: &[u8]) -> FerrosGrynnResult<PageKey> {
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
fn encode_page_init(key: PageKey, page_type: PageType) -> [u8; PAGE_INIT_PAYLOAD_SIZE] {
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
fn decode_page_init(payload: &[u8]) -> FerrosGrynnResult<(PageKey, PageType)> {
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
fn decode_tx_commit(payload: &[u8]) -> FerrosGrynnResult<(u64, u32)> {
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
fn validate_system_empty(tx_id: TxId, payload: &[u8], kind: &str) -> FerrosGrynnResult<()> {
	if tx_id != TxId::SYSTEM || !payload.is_empty() {
		return Err(invalid_wal(format!("{kind} must use system tx_id and empty payload")));
	}
	Ok(())
}

/// Fixed durable checkpoint marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckpointEnd {
	/// Monotonic checkpoint generation.
	generation: CheckpointGeneration,
	/// Highest commit boundary copied to home files.
	backfill_lsn: Lsn,
}

impl CheckpointEnd {
	/// Encodes the fixed format-v1 payload.
	fn encode(self) -> [u8; CHECKPOINT_END_PAYLOAD_SIZE] {
		let mut payload = [0_u8; CHECKPOINT_END_PAYLOAD_SIZE];
		payload[CHECKPOINT_GENERATION_OFFSET..CHECKPOINT_BACKFILL_LSN_OFFSET]
			.copy_from_slice(&self.generation.get().to_le_bytes());
		payload[CHECKPOINT_BACKFILL_LSN_OFFSET..]
			.copy_from_slice(&self.backfill_lsn.get().to_le_bytes());
		payload
	}
}

/// Decodes a fixed CheckpointEnd payload.
fn decode_checkpoint_end(tx_id: TxId, payload: &[u8]) -> FerrosGrynnResult<CheckpointEnd> {
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
fn validate_graph_create(payload: &[u8]) -> FerrosGrynnResult<()> {
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
fn validate_graph_drop(payload: &[u8]) -> FerrosGrynnResult<()> {
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
fn wal_checksum(prefix: &[u8; WAL_CHECKSUM_PREFIX_SIZE], payload: &[u8]) -> u64 {
	let mut hasher = RapidStreamHasherV3::new(&DEFAULT_RAPID_SECRETS);
	hasher.write(prefix);
	hasher.write(payload);
	hasher.finish()
}

/// Returns the encoded byte count for one complete transaction batch.
fn transaction_encoded_size(frame_count: usize) -> FerrosGrynnResult<u64> {
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
fn segment_path(directory: &Path, start_lsn: Lsn) -> PathBuf {
	directory.join(format!(
		"{:0width$x}{WAL_SEGMENT_SUFFIX}",
		start_lsn.get(),
		width = WAL_SEGMENT_STEM_LEN
	))
}

/// Reads one little-endian u16 from a validated fixed-size payload.
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0_u8; size_of::<u16>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u16>()]);
	u16::from_le_bytes(value)
}

/// Reads one little-endian u32 from a validated fixed-size payload.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0_u8; size_of::<u32>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u32>()]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian u64 from a validated fixed-size payload.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0_u8; size_of::<u64>()];
	value.copy_from_slice(&bytes[offset..offset + size_of::<u64>()]);
	u64::from_le_bytes(value)
}

/// Creates a structured storage-format error.
fn invalid_wal(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

/// Preserves the native I/O source behind the project status boundary.
fn io_failure(operation: impl Into<String>, source: io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
#[allow(
	clippy::panic_in_result_fn,
	reason = "fallible WAL fixtures return Result while assertions report contract failures"
)]
mod tests {
	use std::{
		error::Error,
		fs::OpenOptions,
		io::{Read, Seek, SeekFrom, Write},
		path::{Path, PathBuf},
		sync::{
			Arc,
			Barrier,
			atomic::{AtomicU64, Ordering},
		},
		thread,
	};

	use rapidhash::v3::rapidhash_v3;

	use super::*;
	use crate::storage::page::{PAGE_HEADER_SIZE, PageFlags, PageType};

	/// Stable singleton system file used by WAL fixtures.
	const TEST_FILE_ID: DataFileId = DataFileId::for_kind(DataFileKind::SystemCatalog);
	/// Assigned logical-annotation range representative skipped by recovery.
	const TEST_ANNOTATION_KIND: u8 = 0x10;
	/// First currently unassigned recovery-kind byte.
	const TEST_UNASSIGNED_KIND: u8 = WalKind::GraphDrop.get() + 1;
	/// First reserved format-v1 data-file kind encoded as a complete identity.
	const TEST_RESERVED_FILE_ID: u64 = DataFileKind::TensorCache.get() as u64 + 1;
	/// Monotonic suffix for collision-resistant WAL test directories.
	static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	/// Temporary directory removed on success and unwind.
	struct TestDirectory(PathBuf);

	impl TestDirectory {
		/// Allocates one absent path in the current workspace.
		fn new() -> io::Result<Self> {
			let suffix = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
			let name = format!(".ferros-grynn-wal-{}-{suffix}", std::process::id());
			Ok(Self(std::env::current_dir()?.join(name)))
		}

		/// Returns the temporary native path.
		fn as_path(&self) -> &Path {
			&self.0
		}
	}

	impl Drop for TestDirectory {
		/// Removes only the uniquely named test directory.
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.0));
		}
	}

	/// Creates one deterministic physical test key.
	fn page_key(page_no: u64) -> PageKey {
		PageKey::new(GraphId::SYSTEM, TEST_FILE_ID, PageNo::new(page_no))
	}

	/// Creates one sealed page carrying a marker byte.
	fn marked_page(page_no: u64, marker: u8) -> FerrosGrynnResult<AlignedPage> {
		let page_no = PageNo::new(page_no);
		let mut page = AlignedPage::new(
			PageType::StorageSuperblock,
			PageFlags::empty(),
			PageLsn::ZERO,
			page_no,
		);
		page.as_bytes_mut()[PAGE_HEADER_SIZE] = marker;
		page.seal()?;
		Ok(page)
	}

	/// Reads the marker from the newest frame visible to a snapshot.
	fn visible_marker(
		wal: &FrameWal,
		key: PageKey,
		snapshot: StorageSnapshot,
	) -> FerrosGrynnResult<Option<u8>> {
		let visible = wal.visible_frames(&[key], snapshot);
		let Some(frame) = visible.frames()[0] else {
			return Ok(None);
		};
		let mut page = AlignedPage::zeroed();
		visible.read_frame(frame, &mut page)?;
		Ok(Some(page.as_bytes()[PAGE_HEADER_SIZE]))
	}

	/// Reads a complete segment into memory for exact codec assertions.
	fn read_segment(path: &Path) -> io::Result<Vec<u8>> {
		let mut file = File::open(path)?;
		let mut bytes = Vec::new();
		file.read_to_end(&mut bytes)?;
		Ok(bytes)
	}

	/// Appends one valid raw frame at the writer's next LSN.
	fn append_raw_frame(
		writer: &mut WriterState,
		tx_id: TxId,
		key: PageKey,
		marker: u8,
	) -> FerrosGrynnResult<()> {
		let frame_lsn = writer.next_lsn;
		let page = marked_page(key.page_no().get(), marker)?
			.copy_with_lsn(key.page_no(), PageLsn::new(frame_lsn.get()))?;
		let mut payload = Vec::with_capacity(PAGE_FRAME_PAYLOAD_SIZE);
		encode_page_key(key, &mut payload);
		payload.extend_from_slice(page.as_bytes());
		writer.append_record(tx_id, WalKind::PageFrame, &payload)?;
		Ok(())
	}

	/// Appends one valid raw commit marker with a single frame.
	fn append_raw_commit(
		writer: &mut WriterState,
		tx_id: TxId,
		commit_ts: u64,
	) -> FerrosGrynnResult<()> {
		let mut payload = [0_u8; TX_COMMIT_PAYLOAD_SIZE];
		payload[TX_COMMIT_TS_OFFSET..TX_COMMIT_FRAME_COUNT_OFFSET]
			.copy_from_slice(&commit_ts.to_le_bytes());
		payload[TX_COMMIT_FRAME_COUNT_OFFSET..TX_COMMIT_RESERVED_OFFSET]
			.copy_from_slice(&1_u32.to_le_bytes());
		writer.append_record(tx_id, WalKind::TxCommit, &payload)?;
		Ok(())
	}

	/// Format-v1 records use exact offsets and RapidHash V3 over
	/// prefix+payload.
	#[test]
	fn record_codec_matches_normative_header_and_checksum() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		fs::create_dir(directory.as_path())?;
		let mut segment = ActiveSegment::create(directory.as_path(), Lsn::FIRST)?;
		let payload = [0xaa, 0xbb, 0xcc];
		segment.append(Lsn::new(0x0102), TxId::new(0x0304), TEST_ANNOTATION_KIND, &payload)?;
		segment.writer.flush()?;
		drop(segment);

		let bytes = read_segment(&segment_path(directory.as_path(), Lsn::FIRST))?;
		assert_eq!(bytes.len(), WAL_HEADER_SIZE + payload.len());
		assert_eq!(&bytes[..WAL_LSN_OFFSET], b"FGW1");
		assert_eq!(read_u64(&bytes, WAL_LSN_OFFSET), 0x0102);
		assert_eq!(read_u64(&bytes, WAL_TX_ID_OFFSET), 0x0304);
		assert_eq!(bytes[WAL_KIND_OFFSET], TEST_ANNOTATION_KIND);
		assert_eq!(read_u32(&bytes, WAL_PAYLOAD_LEN_OFFSET), 3);
		assert_eq!(&bytes[WAL_HEADER_SIZE..], &payload);
		let mut checksum_input = Vec::from(&bytes[WAL_LSN_OFFSET..WAL_CHECKSUM_OFFSET]);
		checksum_input.extend_from_slice(&payload);
		assert_eq!(read_u64(&bytes, WAL_CHECKSUM_OFFSET), rapidhash_v3(&checksum_input));
		Ok(())
	}

	/// The unreleased stage-3a magic is rejected rather than decoded under the
	/// incompatible DataFileId payload contract.
	#[test]
	fn recovery_rejects_experimental_stage3a_magic() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		drop(wal);
		let path = segment_path(directory.as_path(), Lsn::FIRST);
		let mut header = [0_u8; WAL_HEADER_SIZE];
		header[..WAL_LSN_OFFSET].copy_from_slice(&WalMagic::EXPERIMENTAL_3A.get());
		header[WAL_LSN_OFFSET..WAL_TX_ID_OFFSET].copy_from_slice(&Lsn::FIRST.get().to_le_bytes());
		header[WAL_TX_ID_OFFSET..WAL_KIND_OFFSET]
			.copy_from_slice(&TxId::SYSTEM.get().to_le_bytes());
		header[WAL_KIND_OFFSET] = TEST_ANNOTATION_KIND;
		let mut prefix = [0_u8; WAL_CHECKSUM_PREFIX_SIZE];
		prefix.copy_from_slice(&header[WAL_LSN_OFFSET..WAL_CHECKSUM_OFFSET]);
		header[WAL_CHECKSUM_OFFSET..].copy_from_slice(&wal_checksum(&prefix, &[]).to_le_bytes());
		let mut file = OpenOptions::new().append(true).open(&path)?;
		file.write_all(&header)?;
		file.sync_data()?;
		drop(file);

		let length = fs::metadata(&path)?.len();
		assert!(FrameWal::open(directory.as_path()).is_err());
		assert_eq!(fs::metadata(path)?.len(), length);
		Ok(())
	}

	/// PageFrame keys use the complete DataFileId without reserved padding.
	#[test]
	fn page_frame_key_layout_distinguishes_file_instances() -> Result<(), Box<dyn Error>> {
		let first_id = DataFileId::new(1, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid first test file identity"))?;
		let second_id = DataFileId::new(2, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid second test file identity"))?;
		let first = PageKey::new(GraphId::new(9), first_id, PageNo::new(17));
		let second = PageKey::new(GraphId::new(9), second_id, PageNo::new(17));
		let mut encoded = Vec::new();
		encode_page_key(first, &mut encoded);
		assert_eq!(encoded.len(), PAGE_FRAME_PREFIX_SIZE);
		assert_eq!(read_u64(&encoded, PAGE_FRAME_GRAPH_ID_OFFSET), 9);
		assert_eq!(read_u64(&encoded, PAGE_FRAME_FILE_ID_OFFSET), 0x010c);
		assert_eq!(read_u64(&encoded, PAGE_FRAME_PAGE_NO_OFFSET), 17);
		assert_eq!(decode_page_key(&encoded)?, first);
		assert_ne!(first, second);
		Ok(())
	}

	/// PageInit keeps its assigned type and six canonical reserved bytes.
	#[test]
	fn page_init_layout_is_fixed_and_rejects_padding() -> Result<(), Box<dyn Error>> {
		let key = page_key(23);
		let payload = encode_page_init(key, PageType::NodeHot);
		assert_eq!(payload.len(), 32);
		assert_eq!(read_u64(&payload, PAGE_FRAME_FILE_ID_OFFSET), TEST_FILE_ID.get());
		assert_eq!(read_u16(&payload, PAGE_INIT_PAGE_TYPE_OFFSET), PageType::NodeHot.get());
		assert!(
			payload[PAGE_INIT_RESERVED_OFFSET..PAGE_INIT_PAGE_NO_OFFSET]
				.iter()
				.all(|byte| *byte == 0)
		);
		assert_eq!(decode_page_init(&payload)?, (key, PageType::NodeHot));

		let mut malformed = payload;
		malformed[PAGE_INIT_RESERVED_OFFSET] = 1;
		assert!(decode_page_init(&malformed).is_err());
		Ok(())
	}

	/// CheckpointEnd is exactly generation plus backfill LSN; old DPT bytes
	/// fail.
	#[test]
	fn checkpoint_end_layout_is_fixed_without_dirty_page_table() -> Result<(), Box<dyn Error>> {
		let checkpoint = CheckpointEnd {
			generation: CheckpointGeneration::new(5),
			backfill_lsn: Lsn::new(99),
		};
		let payload = checkpoint.encode();
		assert_eq!(payload.len(), 16);
		assert_eq!(read_u64(&payload, CHECKPOINT_GENERATION_OFFSET), 5);
		assert_eq!(read_u64(&payload, CHECKPOINT_BACKFILL_LSN_OFFSET), 99);
		assert_eq!(decode_checkpoint_end(TxId::SYSTEM, &payload)?, checkpoint);

		let mut old_dpt_fixture = payload.to_vec();
		old_dpt_fixture.extend_from_slice(&0_u32.to_le_bytes());
		assert!(decode_checkpoint_end(TxId::SYSTEM, &old_dpt_fixture).is_err());
		assert!(decode_checkpoint_end(TxId::SYSTEM, &[0_u8; 16]).is_err());
		Ok(())
	}

	/// Recovery accepts a checkpoint marker only after its begin record and
	/// within already durable transaction progress.
	#[test]
	fn checkpoint_end_requires_begin_and_durable_target() -> Result<(), Box<dyn Error>> {
		let raw = |lsn: u64, kind: WalKind, payload: Vec<u8>| RawRecord {
			lsn: Lsn::new(lsn),
			tx_id: TxId::SYSTEM,
			kind: kind.get(),
			payload,
			location: RecordLocation {
				payload_offset: 0,
			},
		};
		let payload = CheckpointEnd {
			generation: CheckpointGeneration::new(1),
			backfill_lsn: Lsn::new(3),
		}
		.encode()
		.to_vec();
		let mut unpaired = RecoveryBuilder::default();
		unpaired.published.durable_visible_lsn = 3;
		assert!(unpaired.apply(raw(5, WalKind::CheckpointEnd, payload), Lsn::FIRST).is_err());

		let mut future = RecoveryBuilder::default();
		future.published.durable_visible_lsn = 3;
		future.apply(raw(4, WalKind::CheckpointBegin, Vec::new()), Lsn::FIRST)?;
		let future_payload = CheckpointEnd {
			generation: CheckpointGeneration::new(1),
			backfill_lsn: Lsn::new(4),
		}
		.encode()
		.to_vec();
		assert!(future.apply(raw(5, WalKind::CheckpointEnd, future_payload), Lsn::FIRST).is_err());
		Ok(())
	}

	/// Transaction staging cannot mutate checkpoint-owned graph metadata.
	#[test]
	fn graph_superblock_is_not_a_transaction_frame() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let file_id = DataFileId::for_kind(DataFileKind::GraphSuperblock);
		let key = PageKey::new(GraphId::new(1), file_id, PageNo::new(0));
		let page = AlignedPage::new(
			PageType::GraphSuperblock,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(0),
		);
		let mut tx = wal.begin_tx()?;
		assert!(tx.stage_page(key, &page).is_err());
		Ok(())
	}

	/// Commits publish after durability, preserve old snapshots, and recover
	/// clocks.
	#[test]
	fn committed_frames_preserve_snapshot_versions_across_reopen() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let key = page_key(0);
		let before_first = wal.snapshot();
		let mut first = wal.begin_tx()?;
		first.stage_page(key, &marked_page(0, 0)?)?;
		first.stage_page(key, &marked_page(0, 1)?)?;
		assert_eq!(first.staged_page(key).map(|page| page.as_bytes()[PAGE_HEADER_SIZE]), Some(1));
		let first_outcome = wal.commit_tx(first)?;
		assert_eq!(first_outcome.tx_id(), TxId::new(1));
		assert_eq!(first_outcome.commit_ts(), CommitTs::new(1));
		assert_eq!(first_outcome.commit_lsn(), Lsn::new(3));
		assert_eq!(visible_marker(&wal, key, before_first)?, None);
		assert_eq!(visible_marker(&wal, key, wal.snapshot())?, Some(1));

		let before_second = wal.snapshot();
		let mut second = wal.begin_tx()?;
		second.stage_page(key, &marked_page(0, 2)?)?;
		let second_outcome = wal.commit_tx(second)?;
		assert_eq!(second_outcome.commit_lsn(), Lsn::new(6));
		assert_eq!(visible_marker(&wal, key, before_second)?, Some(1));
		assert_eq!(visible_marker(&wal, key, wal.snapshot())?, Some(2));
		drop(wal);

		let reopened = FrameWal::open(directory.as_path())?;
		assert_eq!(reopened.snapshot().commit_ts(), CommitTs::new(2));
		assert_eq!(reopened.snapshot().visible_lsn(), Lsn::new(6));
		assert_eq!(visible_marker(&reopened, key, reopened.snapshot())?, Some(2));
		let mut third = reopened.begin_tx()?;
		assert_eq!(third.id(), TxId::new(3));
		third.stage_page(page_key(1), &marked_page(1, 3)?)?;
		assert_eq!(reopened.commit_tx(third)?.commit_lsn(), Lsn::new(9));
		Ok(())
	}

	/// The interim page-level first-committer-wins rule rejects a stale writer.
	#[test]
	fn stale_same_page_writer_conflicts_while_disjoint_page_commits() -> Result<(), Box<dyn Error>>
	{
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let shared_key = page_key(0);
		let mut first = wal.begin_tx()?;
		let mut stale = wal.begin_tx()?;
		first.stage_page(shared_key, &marked_page(0, 1)?)?;
		stale.stage_page(shared_key, &marked_page(0, 2)?)?;
		wal.commit_tx(first)?;
		assert!(wal.commit_tx(stale).is_err());

		let mut left = wal.begin_tx()?;
		let mut right = wal.begin_tx()?;
		left.stage_page(page_key(1), &marked_page(1, 3)?)?;
		right.stage_page(page_key(2), &marked_page(2, 4)?)?;
		wal.commit_tx(left)?;
		wal.commit_tx(right)?;
		assert_eq!(visible_marker(&wal, page_key(1), wal.snapshot())?, Some(3));
		assert_eq!(visible_marker(&wal, page_key(2), wal.snapshot())?, Some(4));
		Ok(())
	}

	/// Concurrent disjoint writers serialize through one global commit lock.
	#[test]
	fn concurrent_commits_publish_monotonic_clocks() -> Result<(), Box<dyn Error>> {
		const WRITER_COUNT: usize = 4;
		let directory = TestDirectory::new()?;
		let wal = Arc::new(FrameWal::create_new(directory.as_path())?);
		let barrier = Arc::new(Barrier::new(WRITER_COUNT));
		let mut handles = Vec::new();
		for page_index in 0..WRITER_COUNT {
			let page_no = u64::try_from(page_index)?;
			let marker = u8::try_from(page_index)?;
			let wal = Arc::clone(&wal);
			let barrier = Arc::clone(&barrier);
			handles.push(thread::spawn(move || -> FerrosGrynnResult<Lsn> {
				let mut tx = wal.begin_tx()?;
				tx.stage_page(page_key(page_no), &marked_page(page_no, marker)?)?;
				barrier.wait();
				Ok(wal.commit_tx(tx)?.commit_lsn())
			}));
		}

		let mut commit_lsns = Vec::new();
		for handle in handles {
			let outcome = handle
				.join()
				.map_err(|_panic_payload| io::Error::other("concurrent WAL writer panicked"))??;
			commit_lsns.push(outcome.get());
		}
		commit_lsns.sort_unstable();
		assert_eq!(commit_lsns, [3, 6, 9, 12]);
		assert_eq!(wal.snapshot().commit_ts(), CommitTs::new(u64::try_from(WRITER_COUNT)?));
		assert_eq!(wal.snapshot().visible_lsn(), Lsn::new(12));
		for page_index in 0..WRITER_COUNT {
			let page_no = u64::try_from(page_index)?;
			let marker = u8::try_from(page_index)?;
			assert_eq!(visible_marker(&wal, page_key(page_no), wal.snapshot())?, Some(marker));
		}
		Ok(())
	}

	/// Active snapshot registration retains and releases the oldest WAL
	/// boundary.
	#[test]
	fn transaction_drop_releases_oldest_reader_watermark() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let reader = wal.begin_tx()?;
		assert_eq!(wal.oldest_reader_lsn(), Some(Lsn::ZERO));
		let mut writer = wal.begin_tx()?;
		writer.stage_page(page_key(0), &marked_page(0, 1)?)?;
		wal.commit_tx(writer)?;
		assert_eq!(wal.oldest_reader_lsn(), Some(Lsn::ZERO));
		drop(reader);
		assert_eq!(wal.oldest_reader_lsn(), None);
		Ok(())
	}

	/// Segment rotation happens before a batch and never resets the global LSN.
	#[test]
	fn rotation_keeps_each_transaction_in_one_segment_and_lsn_global() -> Result<(), Box<dyn Error>>
	{
		let directory = TestDirectory::new()?;
		let target = transaction_encoded_size(1)?;
		let wal = FrameWal::create_new_with_target(directory.as_path(), target)?;
		for page_no in 0_u64..2 {
			let mut tx = wal.begin_tx()?;
			tx.stage_page(page_key(page_no), &marked_page(page_no, page_no as u8)?)?;
			wal.commit_tx(tx)?;
		}
		let segments = list_segments(directory.as_path())?;
		assert_eq!(segments.len(), 2);
		assert_eq!(segments[0].start_lsn, Lsn::new(1));
		assert_eq!(segments[1].start_lsn, Lsn::new(4));
		assert_eq!(fs::metadata(&segments[0].path)?.len(), target);
		assert_eq!(fs::metadata(&segments[1].path)?.len(), target);
		drop(wal);

		let reopened = FrameWal::open(directory.as_path())?;
		let mut tx = reopened.begin_tx()?;
		tx.stage_page(page_key(2), &marked_page(2, 2)?)?;
		assert_eq!(reopened.commit_tx(tx)?.commit_lsn(), Lsn::new(9));
		Ok(())
	}

	/// Recovery truncates only an incomplete final record and reuses its LSN.
	#[test]
	fn recovery_repairs_truncated_last_tail_without_losing_commit() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let mut tx = wal.begin_tx()?;
		tx.stage_page(page_key(0), &marked_page(0, 7)?)?;
		wal.commit_tx(tx)?;
		drop(wal);

		let path = segment_path(directory.as_path(), Lsn::FIRST);
		let valid_length = fs::metadata(&path)?.len();
		let mut file = OpenOptions::new().append(true).open(&path)?;
		file.write_all(&WalMagic::FRAME_V1.get())?;
		file.sync_data()?;
		drop(file);

		let reopened = FrameWal::open(directory.as_path())?;
		assert_eq!(fs::metadata(&path)?.len(), valid_length);
		assert_eq!(visible_marker(&reopened, page_key(0), reopened.snapshot())?, Some(7));
		let mut next = reopened.begin_tx()?;
		next.stage_page(page_key(1), &marked_page(1, 8)?)?;
		assert_eq!(reopened.commit_tx(next)?.commit_lsn(), Lsn::new(6));
		Ok(())
	}

	/// Oversized untrusted payload lengths are fatal without allocation or
	/// truncation, even in the final segment.
	#[test]
	fn recovery_rejects_oversized_last_payload_header() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		drop(wal);

		let path = segment_path(directory.as_path(), Lsn::FIRST);
		let oversized = u32::try_from(MAX_WAL_PAYLOAD_BYTES + 1)?;
		let mut header = [0_u8; WAL_HEADER_SIZE];
		header[..WAL_LSN_OFFSET].copy_from_slice(&WalMagic::FRAME_V1.get());
		header[WAL_LSN_OFFSET..WAL_TX_ID_OFFSET].copy_from_slice(&Lsn::FIRST.get().to_le_bytes());
		header[WAL_TX_ID_OFFSET..WAL_KIND_OFFSET]
			.copy_from_slice(&TxId::SYSTEM.get().to_le_bytes());
		header[WAL_KIND_OFFSET] = TEST_ANNOTATION_KIND;
		header[WAL_PAYLOAD_LEN_OFFSET..WAL_CHECKSUM_OFFSET]
			.copy_from_slice(&oversized.to_le_bytes());
		let mut file = OpenOptions::new().append(true).open(&path)?;
		file.write_all(&header)?;
		file.sync_data()?;
		drop(file);

		let length = fs::metadata(&path)?.len();
		assert!(FrameWal::open(directory.as_path()).is_err());
		assert_eq!(fs::metadata(path)?.len(), length);
		Ok(())
	}

	/// Complete frames without TxCommit remain invisible after recovery.
	#[test]
	fn recovery_discards_complete_uncommitted_transaction() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		{
			let mut writer = wal.inner.writer.lock();
			let tx_id = TxId::new(41);
			writer.append_record(tx_id, WalKind::TxBegin, &[])?;
			let frame_lsn = writer.next_lsn;
			let page =
				marked_page(0, 9)?.copy_with_lsn(PageNo::new(0), PageLsn::new(frame_lsn.get()))?;
			let mut payload = Vec::new();
			encode_page_key(page_key(0), &mut payload);
			payload.extend_from_slice(page.as_bytes());
			writer.append_record(tx_id, WalKind::PageFrame, &payload)?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
		}
		drop(wal);

		let reopened = FrameWal::open(directory.as_path())?;
		assert_eq!(visible_marker(&reopened, page_key(0), reopened.snapshot())?, None);
		let tx = reopened.begin_tx()?;
		assert_eq!(tx.id(), TxId::new(42));
		assert_eq!(tx.snapshot().visible_lsn(), Lsn::ZERO);
		Ok(())
	}

	/// Recovery orders versions by frame LSN even when commit markers are
	/// reversed.
	#[test]
	fn recovery_resolves_interleaved_transactions_by_frame_lsn() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let key = page_key(0);
		{
			let mut writer = wal.inner.writer.lock();
			let first = TxId::new(1);
			let second = TxId::new(2);
			writer.append_record(first, WalKind::TxBegin, &[])?;
			append_raw_frame(&mut writer, first, key, 1)?;
			writer.append_record(second, WalKind::TxBegin, &[])?;
			append_raw_frame(&mut writer, second, key, 2)?;
			append_raw_commit(&mut writer, second, 1)?;
			append_raw_commit(&mut writer, first, 2)?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
		}
		drop(wal);

		let reopened = FrameWal::open(directory.as_path())?;
		assert_eq!(reopened.snapshot().visible_lsn(), Lsn::new(6));
		assert_eq!(visible_marker(&reopened, key, reopened.snapshot())?, Some(2));
		assert_eq!(
			visible_marker(&reopened, key, StorageSnapshot::new(CommitTs::new(1), Lsn::new(5)),)?,
			Some(2)
		);
		Ok(())
	}

	/// Corruption in a completed non-final segment is fatal, not an EOF signal.
	#[test]
	fn recovery_rejects_corruption_before_final_segment() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal =
			FrameWal::create_new_with_target(directory.as_path(), transaction_encoded_size(1)?)?;
		for page_no in 0_u64..2 {
			let mut tx = wal.begin_tx()?;
			tx.stage_page(page_key(page_no), &marked_page(page_no, 1)?)?;
			wal.commit_tx(tx)?;
		}
		drop(wal);

		let first = segment_path(directory.as_path(), Lsn::FIRST);
		let mut file = OpenOptions::new().read(true).write(true).open(first)?;
		file.seek(SeekFrom::Start(
			(WAL_HEADER_SIZE + WAL_HEADER_SIZE + PAGE_FRAME_PREFIX_SIZE) as u64,
		))?;
		let mut byte = [0_u8; 1];
		file.read_exact(&mut byte)?;
		byte[0] ^= 0x80;
		file.seek(SeekFrom::Current(-1))?;
		file.write_all(&byte)?;
		file.sync_data()?;
		drop(file);
		assert!(FrameWal::open(directory.as_path()).is_err());
		Ok(())
	}

	/// A fully present final record with a checksum mismatch is durable
	/// corruption, not an incomplete append that recovery may discard.
	#[test]
	fn recovery_rejects_checksum_corruption_in_final_segment() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let mut tx = wal.begin_tx()?;
		tx.stage_page(page_key(0), &marked_page(0, 1)?)?;
		wal.commit_tx(tx)?;
		drop(wal);

		let path = segment_path(directory.as_path(), Lsn::FIRST);
		let length = fs::metadata(&path)?.len();
		let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
		file.seek(SeekFrom::Start(
			(WAL_HEADER_SIZE + WAL_HEADER_SIZE + PAGE_FRAME_PREFIX_SIZE) as u64,
		))?;
		let mut byte = [0_u8; 1];
		file.read_exact(&mut byte)?;
		byte[0] ^= 0x80;
		file.seek(SeekFrom::Current(-1))?;
		file.write_all(&byte)?;
		file.sync_data()?;
		drop(file);

		assert!(FrameWal::open(directory.as_path()).is_err());
		assert_eq!(fs::metadata(path)?.len(), length);
		Ok(())
	}

	/// Checksummed unassigned records are skipped without resetting LSN state.
	#[test]
	fn recovery_skips_forward_compatible_annotation_kind() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		{
			let mut writer = wal.inner.writer.lock();
			let lsn = writer.next_lsn;
			writer.segment.append(lsn, TxId::SYSTEM, TEST_ANNOTATION_KIND, b"annotation")?;
			writer.next_lsn = lsn.checked_next()?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
		}
		drop(wal);

		let reopened = FrameWal::open(directory.as_path())?;
		let mut tx = reopened.begin_tx()?;
		tx.stage_page(page_key(0), &marked_page(0, 1)?)?;
		assert_eq!(reopened.commit_tx(tx)?.commit_lsn(), Lsn::new(4));
		Ok(())
	}

	/// An unassigned kind is fatal and is never mistaken for a damaged tail.
	#[test]
	fn recovery_rejects_unassigned_kind_without_truncation() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		{
			let mut writer = wal.inner.writer.lock();
			let lsn = writer.next_lsn;
			writer.segment.append(lsn, TxId::SYSTEM, TEST_UNASSIGNED_KIND, &[])?;
			writer.next_lsn = lsn.checked_next()?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
		}
		drop(wal);

		let path = segment_path(directory.as_path(), Lsn::FIRST);
		let length = fs::metadata(&path)?.len();
		assert!(FrameWal::open(directory.as_path()).is_err());
		assert_eq!(fs::metadata(path)?.len(), length);
		Ok(())
	}

	/// A complete checksummed record with a reserved data-file kind is semantic
	/// corruption, not a repairable torn final tail.
	#[test]
	fn recovery_rejects_unknown_file_id_without_truncation() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		{
			let mut writer = wal.inner.writer.lock();
			let tx_id = TxId::new(1);
			writer.append_record(tx_id, WalKind::TxBegin, &[])?;
			let frame_lsn = writer.next_lsn;
			let page =
				marked_page(0, 1)?.copy_with_lsn(PageNo::new(0), PageLsn::new(frame_lsn.get()))?;
			let mut payload = Vec::with_capacity(PAGE_FRAME_PAYLOAD_SIZE);
			payload.extend_from_slice(&GraphId::SYSTEM.get().to_le_bytes());
			payload.extend_from_slice(&TEST_RESERVED_FILE_ID.to_le_bytes());
			payload.extend_from_slice(&0_u64.to_le_bytes());
			payload.extend_from_slice(page.as_bytes());
			writer.append_record(tx_id, WalKind::PageFrame, &payload)?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
		}
		drop(wal);

		let path = segment_path(directory.as_path(), Lsn::FIRST);
		let length = fs::metadata(&path)?.len();
		assert!(FrameWal::open(directory.as_path()).is_err());
		assert_eq!(fs::metadata(path)?.len(), length);
		Ok(())
	}

	/// Recovery rejects a segment boundary that splits one transaction batch.
	#[test]
	fn recovery_rejects_transaction_split_across_segments() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		{
			let mut writer = wal.inner.writer.lock();
			let tx_id = TxId::new(1);
			writer.append_record(tx_id, WalKind::TxBegin, &[])?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
			writer.rotate(directory.as_path())?;
			append_raw_frame(&mut writer, tx_id, page_key(0), 1)?;
			append_raw_commit(&mut writer, tx_id, 1)?;
			writer.segment.writer.flush()?;
			writer.segment.writer.get_ref().sync_data()?;
		}
		drop(wal);

		assert!(FrameWal::open(directory.as_path()).is_err());
		Ok(())
	}

	/// A committed checkpoint may prune a closed prefix and reopen from its
	/// persisted clock seed without reusing identities.
	#[test]
	fn checkpoint_prunes_closed_prefix_and_seeded_recovery_continues_clocks()
	-> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal =
			FrameWal::create_new_with_target(directory.as_path(), transaction_encoded_size(1)?)?;
		for page_no in 0_u64..2 {
			let mut tx = wal.begin_tx()?;
			tx.stage_page(page_key(page_no), &marked_page(page_no, page_no as u8)?)?;
			wal.commit_tx(tx)?;
		}
		let target = wal
			.capture_checkpoint_target()
			.ok_or_else(|| io::Error::other("checkpoint target was not captured"))?;
		assert_eq!(target, Lsn::new(6));
		let ticket = wal.begin_checkpoint(target, CheckpointGeneration::ZERO)?;
		assert_eq!(ticket.frames().len(), 2);
		let checkpoint = wal.finish_checkpoint(ticket)?;
		let seed = RecoverySeed::new(
			checkpoint.generation(),
			checkpoint.target(),
			checkpoint.checkpoint_record_lsn(),
			checkpoint.clocks().durable_visible_lsn,
			checkpoint.clocks().next_lsn,
			checkpoint.clocks().commit_ts,
			checkpoint.clocks().next_tx_id,
			checkpoint.clocks().next_file_instance_id,
		);
		wal.publish_checkpoint(&checkpoint)?;
		let segments = list_segments(directory.as_path())?;
		assert_eq!(segments.len(), 1);
		assert_eq!(segments[0].start_lsn, Lsn::new(4));
		drop(wal);

		let reopened = FrameWal::open_with_seed(directory.as_path(), seed)?;
		assert!(reopened.recovery_frames().is_empty());
		let mut tx = reopened.begin_tx()?;
		assert_eq!(tx.id(), TxId::new(3));
		tx.stage_page(page_key(2), &marked_page(2, 2)?)?;
		assert_eq!(reopened.commit_tx(tx)?.commit_lsn(), Lsn::new(11));
		Ok(())
	}

	/// Persisted file identities advance the allocator clock beyond every
	/// instance observed in committed PageFrame keys.
	#[test]
	fn committed_frame_advances_file_instance_clock() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let file_id = DataFileId::new(42, DataFileKind::Index)
			.ok_or_else(|| io::Error::other("invalid test file identity"))?;
		let key = PageKey::new(GraphId::new(1), file_id, PageNo::new(0));
		let mut tx = wal.begin_tx()?;
		tx.stage_page(key, &marked_page(0, 1)?)?;
		let commit = wal.commit_tx(tx)?;
		let ticket = wal.begin_checkpoint(commit.commit_lsn(), CheckpointGeneration::ZERO)?;
		let checkpoint = wal.finish_checkpoint(ticket)?;
		assert_eq!(checkpoint.clocks().next_file_instance_id, 43);
		Ok(())
	}

	/// A commit that lands during backfill remains above the captured target
	/// and is selected by the next passive checkpoint.
	#[test]
	fn commit_during_backfill_remains_for_next_checkpoint() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let mut first = wal.begin_tx()?;
		first.stage_page(page_key(0), &marked_page(0, 1)?)?;
		let first_commit = wal.commit_tx(first)?;
		let ticket = wal.begin_checkpoint(first_commit.commit_lsn(), CheckpointGeneration::ZERO)?;

		let mut concurrent = wal.begin_tx()?;
		concurrent.stage_page(page_key(1), &marked_page(1, 2)?)?;
		let concurrent_commit = wal.commit_tx(concurrent)?;
		let checkpoint = wal.finish_checkpoint(ticket)?;
		wal.publish_checkpoint(&checkpoint)?;
		assert_eq!(visible_marker(&wal, page_key(1), wal.snapshot())?, Some(2));
		assert_eq!(wal.capture_checkpoint_target(), Some(concurrent_commit.commit_lsn()));
		let next = wal.begin_checkpoint(concurrent_commit.commit_lsn(), checkpoint.generation())?;
		assert_eq!(next.generation(), CheckpointGeneration::new(2));
		assert_eq!(next.frames().iter().map(|frame| frame.key()).collect::<Vec<_>>(), [page_key(
			1
		)]);
		Ok(())
	}

	/// Snapshot resolution retains the shared prune gate through physical frame
	/// loading, so exclusive prune cannot invalidate a selected location.
	#[test]
	fn visible_frame_set_holds_prune_gate_until_drop() -> Result<(), Box<dyn Error>> {
		let directory = TestDirectory::new()?;
		let wal = FrameWal::create_new(directory.as_path())?;
		let mut tx = wal.begin_tx()?;
		tx.stage_page(page_key(0), &marked_page(0, 9)?)?;
		wal.commit_tx(tx)?;
		let visible = wal.visible_frames(&[page_key(0)], wal.snapshot());
		let frame = visible.frames()[0]
			.ok_or_else(|| io::Error::other("visible frame was not resolved"))?;
		assert!(wal.inner.prune_gate.try_write().is_none());
		let mut page = AlignedPage::zeroed();
		visible.read_frame(frame, &mut page)?;
		assert_eq!(page.as_bytes()[PAGE_HEADER_SIZE], 9);
		drop(visible);
		assert!(wal.inner.prune_gate.try_write().is_some());
		Ok(())
	}
}
