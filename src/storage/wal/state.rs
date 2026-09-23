#![allow(
	clippy::wildcard_imports,
	reason = "move-only WAL split keeps the existing imports during stage 1"
)]

use super::*;

/// Result published after a transaction's single durability barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct CommitOutcome {
	/// Database-wide transaction identity.
	pub(in crate::storage::wal) tx_id: TxId,
	/// Logical commit clock after this operation.
	pub(in crate::storage::wal) commit_ts: CommitTs,
	/// Durable WAL boundary after this operation.
	pub(in crate::storage::wal) commit_lsn: Lsn,
}

/// Persisted database clocks that bound WAL recovery after prefix pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct RecoverySeed {
	/// Database-authoritative checkpoint generation.
	pub(in crate::storage::wal) generation: CheckpointGeneration,
	/// Highest commit boundary known to be present in home files.
	pub(in crate::storage::wal) checkpoint_lsn: Lsn,
	/// Record LSN of the matching durable CheckpointEnd.
	pub(in crate::storage::wal) checkpoint_record_lsn: Lsn,
	/// Highest transaction commit published before the superblock replacement.
	pub(in crate::storage::wal) durable_visible_lsn: Lsn,
	/// First WAL record LSN not assigned by the persisted checkpoint.
	pub(in crate::storage::wal) next_lsn: Lsn,
	/// Highest assigned logical commit timestamp.
	pub(in crate::storage::wal) commit_ts: CommitTs,
	/// First transaction identity not assigned by the persisted checkpoint.
	pub(in crate::storage::wal) next_tx_id: TxId,
	/// First data-file instance identity not assigned by persisted state.
	pub(in crate::storage::wal) next_file_instance_id: u64,
}

impl RecoverySeed {
	/// Empty database seed used by creation and legacy component tests.
	pub(in crate::storage::wal) const EMPTY: Self = Self {
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
	pub(in crate::storage) const fn new(
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
pub(in crate::storage) struct WalClocks {
	/// First unassigned WAL record LSN.
	pub(in crate::storage) next_lsn: Lsn,
	/// Highest durable transaction commit marker.
	pub(in crate::storage) durable_visible_lsn: Lsn,
	/// Highest assigned logical commit timestamp.
	pub(in crate::storage) commit_ts: CommitTs,
	/// First unassigned transaction identity.
	pub(in crate::storage) next_tx_id: TxId,
	/// First unassigned data-file instance identity.
	pub(in crate::storage) next_file_instance_id: u64,
}

/// Immutable passive-checkpoint work selected under the WAL writer lock.
pub(in crate::storage) struct CheckpointTicket {
	/// Generation assigned to this checkpoint attempt.
	pub(in crate::storage::wal) generation: CheckpointGeneration,
	/// Reader-safe home-file backfill boundary.
	pub(in crate::storage::wal) target: Lsn,
	/// Latest complete frame for every page at or before `target`.
	pub(in crate::storage::wal) frames: Vec<ResolvedFrame>,
}

impl CheckpointTicket {
	/// Returns the assigned generation.
	pub(in crate::storage) const fn generation(&self) -> CheckpointGeneration {
		self.generation
	}

	/// Returns the reader-safe backfill boundary.
	pub(in crate::storage) const fn target(&self) -> Lsn {
		self.target
	}

	/// Returns the immutable page-frame plan.
	pub(in crate::storage) fn frames(&self) -> &[ResolvedFrame] {
		&self.frames
	}
}

/// Durable CheckpointEnd and clocks awaiting the database commit point.
pub(in crate::storage) struct CheckpointCommit {
	/// Generation assigned to the durable CheckpointEnd.
	pub(in crate::storage::wal) generation: CheckpointGeneration,
	/// Reader-safe backfill boundary.
	pub(in crate::storage::wal) target: Lsn,
	/// LSN of the durable CheckpointEnd record.
	pub(in crate::storage::wal) checkpoint_record_lsn: Lsn,
	/// Consistent clocks captured with that record.
	pub(in crate::storage::wal) clocks: WalClocks,
}

impl CheckpointCommit {
	/// Returns the assigned checkpoint generation.
	pub(in crate::storage) const fn generation(&self) -> CheckpointGeneration {
		self.generation
	}

	/// Returns the home-file backfill boundary.
	pub(in crate::storage) const fn target(&self) -> Lsn {
		self.target
	}

	/// Returns the matching durable CheckpointEnd record LSN.
	pub(in crate::storage) const fn checkpoint_record_lsn(&self) -> Lsn {
		self.checkpoint_record_lsn
	}

	/// Returns the writer-consistent persisted clocks.
	pub(in crate::storage) const fn clocks(&self) -> WalClocks {
		self.clocks
	}
}

impl CommitOutcome {
	/// Returns the committed transaction identity.
	pub(in crate::storage) const fn tx_id(self) -> TxId {
		self.tx_id
	}

	/// Returns the published commit timestamp.
	pub(in crate::storage) const fn commit_ts(self) -> CommitTs {
		self.commit_ts
	}

	/// Returns the published commit-marker LSN.
	pub(in crate::storage) const fn commit_lsn(self) -> Lsn {
		self.commit_lsn
	}
}

/// Segment-qualified location of complete page bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage::wal) struct FrameLocation {
	/// First record LSN encoded in the segment filename.
	pub(in crate::storage::wal) segment_start_lsn: Lsn,
	/// Byte offset of the complete page image inside that segment.
	pub(in crate::storage::wal) page_bytes_offset: u64,
}

/// One committed page version retained by the in-memory index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage::wal) struct FrameVersion {
	/// LSN written into the page image.
	pub(in crate::storage::wal) frame_lsn: Lsn,
	/// Commit marker controlling snapshot visibility.
	pub(in crate::storage::wal) commit_lsn: Lsn,
	/// Segment-qualified physical WAL location.
	pub(in crate::storage::wal) location: FrameLocation,
}

/// Resolved immutable frame that can be loaded without another index lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct ResolvedFrame {
	/// Physical page identity validated during recovery or commit.
	pub(in crate::storage::wal) key: PageKey,
	/// Selected version metadata.
	pub(in crate::storage::wal) version: FrameVersion,
}

/// Snapshot-visible frame resolutions protected from concurrent pruning.
pub(in crate::storage) struct VisibleFrames<'wal> {
	/// WAL used to load the resolved physical locations.
	pub(in crate::storage::wal) wal: &'wal FrameWal,
	/// Shared gate retained until every selected frame has been loaded.
	pub(in crate::storage::wal) _prune_gate: RwLockReadGuard<'wal, ()>,
	/// Input-ordered frame choices; `None` means read the home file.
	pub(in crate::storage::wal) frames: Vec<Option<ResolvedFrame>>,
}

impl VisibleFrames<'_> {
	/// Returns the input-ordered frame choices.
	pub(in crate::storage) fn frames(&self) -> &[Option<ResolvedFrame>] {
		&self.frames
	}

	/// Loads one frame while this set prevents index and segment pruning.
	///
	/// # Errors
	///
	/// Returns an I/O, page-validation, or version-index mismatch error.
	pub(in crate::storage) fn read_frame(
		&self,
		frame: ResolvedFrame,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		self.wal.read_frame_unlocked(frame, output)
	}
}

impl ResolvedFrame {
	/// Returns the selected page key.
	pub(in crate::storage) const fn key(self) -> PageKey {
		self.key
	}

	/// Returns the selected frame LSN.
	pub(in crate::storage) const fn frame_lsn(self) -> Lsn {
		self.version.frame_lsn
	}

	/// Returns the commit marker controlling visibility.
	pub(in crate::storage) const fn commit_lsn(self) -> Lsn {
		self.version.commit_lsn
	}
}

/// One page image staged privately by a transaction.
pub(in crate::storage::wal) struct StagedPage {
	/// Aligned complete page copied from the higher storage layer.
	pub(in crate::storage::wal) page: Box<AlignedPage>,
}

/// Snapshot registration shared with checkpoint cancellation.
pub(in crate::storage::wal) struct ActiveSnapshot {
	/// Durable visibility captured at transaction begin.
	pub(in crate::storage::wal) snapshot: StorageSnapshot,
	/// Shared liveness flag observed by every use of the handle.
	pub(in crate::storage::wal) active: Arc<AtomicBool>,
	/// Cooperative expiry used by passive checkpoint maintenance.
	pub(in crate::storage::wal) expires_at: Option<Instant>,
}

/// Private frame write-set and snapshot registration.
pub(in crate::storage) struct TxHandle {
	/// Database-wide transaction identity.
	pub(in crate::storage::wal) id: TxId,
	/// Snapshot captured before any staging.
	pub(in crate::storage::wal) snapshot: StorageSnapshot,
	/// Final page image per physical key.
	pub(in crate::storage::wal) staged: BTreeMap<PageKey, StagedPage>,
	/// Unique staged page bytes charged to the write-set limit.
	pub(in crate::storage::wal) staged_bytes: usize,
	/// WAL that owns the active snapshot registration.
	pub(in crate::storage::wal) owner: Weak<FrameWalInner>,
	/// Shared liveness changed by drop, timeout, or restart checkpoint.
	pub(in crate::storage::wal) active: Arc<AtomicBool>,
	/// Cooperative expiry checked before every storage operation.
	pub(in crate::storage::wal) expires_at: Option<Instant>,
	/// Configured age reported by the typed timeout status.
	pub(in crate::storage::wal) max_age_seconds: Option<u64>,
}

impl TxHandle {
	/// Returns this transaction's stable identity.
	pub(in crate::storage) const fn id(&self) -> TxId {
		self.id
	}

	/// Returns the snapshot captured at begin.
	pub(in crate::storage) const fn snapshot(&self) -> StorageSnapshot {
		self.snapshot
	}

	/// Returns whether this snapshot remains registered and unexpired.
	pub(in crate::storage) fn is_active(&self) -> bool {
		self.active.load(AtomicOrdering::Acquire)
			&& !self.expires_at.is_some_and(|deadline| Instant::now() >= deadline)
	}

	/// Cancels this snapshot and releases its checkpoint watermark.
	pub(in crate::storage) fn cancel(&self) {
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
	pub(in crate::storage) fn stage_page(
		&mut self,
		key: PageKey,
		page: &AlignedPage,
	) -> FerrosGrynnResult<()> {
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
	pub(in crate::storage) fn staged_page(&self, key: PageKey) -> Option<&AlignedPage> {
		self.staged.get(&key).map(|staged| staged.page.as_ref())
	}

	/// Returns the exclusive high-water page staged for one physical file.
	pub(in crate::storage) fn staged_file_page_count(
		&self,
		space_id: DataSpaceId,
		file_id: DataFileId,
	) -> Option<u64> {
		self.staged
			.keys()
			.filter(|key| key.space_id() == space_id && key.file_id() == file_id)
			.filter_map(|key| key.page_no().get().checked_add(1))
			.max()
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
pub(in crate::storage) struct FrameWal {
	/// Shared single-writer and read-mostly publication state.
	pub(in crate::storage::wal) inner: Arc<FrameWalInner>,
}

/// Shared WAL state retained by transactions and handles.
pub(in crate::storage::wal) struct FrameWalInner {
	/// Directory containing ordered hexadecimal WAL segments.
	pub(in crate::storage::wal) directory: PathBuf,
	/// Rotation target applied only before a transaction batch.
	pub(in crate::storage::wal) segment_target_bytes: u64,
	/// Single commit writer and current append segment.
	pub(in crate::storage::wal) writer: Mutex<WriterState>,
	/// Durable versions visible to snapshots.
	pub(in crate::storage::wal) published: RwLock<PublishedState>,
	/// Active snapshots used by the future checkpoint watermark.
	pub(in crate::storage::wal) active_snapshots: Mutex<RapidHashMap<TxId, ActiveSnapshot>>,
	/// Serializes snapshot capture+registration against watermark capture.
	pub(in crate::storage::wal) snapshot_gate: RwLock<()>,
	/// Protects resolved frame loading against index and segment pruning.
	pub(in crate::storage::wal) prune_gate: RwLock<()>,
	/// Next transaction identity; relaxed ordering only allocates numbers.
	pub(in crate::storage::wal) next_tx_id: AtomicU64,
}

/// Mutable state protected by the database commit lock.
pub(in crate::storage::wal) struct WriterState {
	/// Segment currently accepting complete transaction batches.
	pub(in crate::storage::wal) segment: ActiveSegment,
	/// Next record LSN reserved by append.
	pub(in crate::storage::wal) next_lsn: Lsn,
	/// Whether the current append stream may accept another transaction.
	pub(in crate::storage::wal) health: WriterHealth,
}

/// Reuse state of one in-process WAL writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage::wal) enum WriterHealth {
	/// No failed append or durability barrier has occurred.
	Ready,
	/// Recovery and reopen are required before another commit.
	Poisoned,
}

/// One open buffered append segment.
pub(in crate::storage::wal) struct ActiveSegment {
	/// First LSN represented by the segment filename.
	pub(in crate::storage::wal) start_lsn: Lsn,
	/// Buffered sequential writer over the native file handle.
	pub(in crate::storage::wal) writer: BufWriter<File>,
	/// Complete bytes currently in the segment.
	pub(in crate::storage::wal) length: u64,
}

/// Read-mostly state changed only after a successful durability barrier.
#[derive(Default)]
pub(in crate::storage::wal) struct PublishedState {
	/// Highest published logical commit timestamp.
	pub(in crate::storage::wal) commit_ts: u64,
	/// Highest durable published commit marker.
	pub(in crate::storage::wal) durable_visible_lsn: u64,
	/// Latest valid checkpoint marker observed during recovery.
	pub(in crate::storage::wal) checkpoint_lsn: u64,
	/// Latest complete checkpoint generation observed in WAL.
	pub(in crate::storage::wal) checkpoint_generation: u64,
	/// Record LSN of the latest complete checkpoint marker.
	pub(in crate::storage::wal) checkpoint_record_lsn: u64,
	/// Highest durable CheckpointEnd generation, including an uncommitted
	/// superblock attempt.
	pub(in crate::storage::wal) max_checkpoint_generation: u64,
	/// Highest backfill target encoded by any durable CheckpointEnd.
	pub(in crate::storage::wal) max_checkpoint_backfill_lsn: u64,
	/// Committed page versions ordered by frame LSN.
	pub(in crate::storage::wal) versions: RapidHashMap<PageKey, Vec<FrameVersion>>,
	/// Latest commit marker per page for conservative page-level FCW.
	pub(in crate::storage::wal) latest_commit: RapidHashMap<PageKey, Lsn>,
	/// First data-file instance identity not observed in persisted WAL.
	pub(in crate::storage::wal) next_file_instance_id: u64,
}
