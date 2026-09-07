//! Database-level frame-WAL transport, commit publication, and recovery.
//!
//! Transactions stage complete page images in memory. The single WAL writer
//! assigns frame LSNs under its commit lock, reseals every page, appends one
//! contiguous transaction, performs one durability barrier, and only then
//! publishes versions to readers.

mod checkpoint;
mod codec;
mod recovery;
mod state;
mod writer;

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
use state::{
	ActiveSegment,
	ActiveSnapshot,
	FrameLocation,
	FrameVersion,
	FrameWalInner,
	PublishedState,
	StagedPage,
	WriterHealth,
	WriterState,
};
pub(in crate::storage) use state::{
	CheckpointCommit,
	CheckpointTicket,
	CommitOutcome,
	FrameWal,
	RecoverySeed,
	ResolvedFrame,
	TxHandle,
	VisibleFrames,
	WalClocks,
};
#[allow(
	clippy::wildcard_imports,
	reason = "move-only WAL split keeps the existing codec and recovery helpers during stage 1"
)]
use {codec::*, recovery::*};

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
	const TEST_RESERVED_FILE_ID: u64 = DataFileKind::PropertyStatistics.get() as u64 + 1;
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
