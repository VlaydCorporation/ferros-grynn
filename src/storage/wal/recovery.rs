#![allow(
	clippy::wildcard_imports,
	reason = "move-only WAL split keeps the existing imports during stage 1"
)]

use super::*;

/// Byte locations returned after appending one record.
#[derive(Debug, Clone, Copy)]
pub(in crate::storage::wal) struct RecordLocation {
	/// Start of the record payload.
	pub(in crate::storage::wal) payload_offset: u64,
}

/// One ordered WAL segment discovered on disk.
#[derive(Debug, Clone)]
pub(in crate::storage::wal) struct SegmentDescriptor {
	/// First record LSN encoded in the filename.
	pub(in crate::storage::wal) start_lsn: Lsn,
	/// Complete native path.
	pub(in crate::storage::wal) path: PathBuf,
}

/// Result of the single streaming recovery pass.
pub(in crate::storage::wal) struct RecoveryResult {
	/// Rebuilt committed publication state.
	pub(in crate::storage::wal) published: PublishedState,
	/// Highest transaction identity in any complete record.
	pub(in crate::storage::wal) max_tx_id: u64,
	/// Highest complete and semantically valid record LSN.
	pub(in crate::storage::wal) last_lsn: Lsn,
	/// Final segment after optional tail repair.
	pub(in crate::storage::wal) last_segment: Option<SegmentDescriptor>,
}

/// Transaction frames retained as locations until a valid commit marker.
#[derive(Default)]
pub(in crate::storage::wal) struct PendingTransaction {
	/// Frame metadata in strict record order.
	pub(in crate::storage::wal) frames: Vec<(PageKey, Lsn, FrameLocation)>,
	/// Duplicate protection for the one-final-frame-per-key invariant.
	pub(in crate::storage::wal) keys: RapidHashSet<PageKey>,
}

/// Mutable semantic recovery state.
#[derive(Default)]
pub(in crate::storage::wal) struct RecoveryBuilder {
	/// Durable state reconstructed from committed records.
	pub(in crate::storage::wal) published: PublishedState,
	/// Transactions without a validated commit marker.
	pub(in crate::storage::wal) pending: RapidHashMap<TxId, PendingTransaction>,
	/// Transaction identities already started anywhere in the retained WAL.
	pub(in crate::storage::wal) seen_transactions: RapidHashSet<TxId>,
	/// Highest complete transaction identity, including incomplete batches.
	pub(in crate::storage::wal) max_tx_id: u64,
	/// Highest accepted record LSN.
	pub(in crate::storage::wal) last_lsn: Lsn,
	/// Complete CheckpointEnd records retained for persisted-seed validation.
	pub(in crate::storage::wal) checkpoint_ends: Vec<(Lsn, CheckpointEnd)>,
	/// Whether at least one unmatched CheckpointBegin precedes the next end.
	pub(in crate::storage::wal) checkpoint_started: bool,
}

/// One checksummed raw record produced by the streaming codec.
pub(in crate::storage::wal) struct RawRecord {
	/// Record position in the global stream.
	pub(in crate::storage::wal) lsn: Lsn,
	/// Owning transaction or system identity.
	pub(in crate::storage::wal) tx_id: TxId,
	/// Assigned or forward-compatible kind byte.
	pub(in crate::storage::wal) kind: u8,
	/// Untrusted payload after bounded allocation and checksum validation.
	pub(in crate::storage::wal) payload: Vec<u8>,
	/// Byte locations in the current segment.
	pub(in crate::storage::wal) location: RecordLocation,
}

/// Result of reading at a record boundary.
pub(in crate::storage::wal) enum RecordRead {
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
pub(in crate::storage::wal) enum ExactRead {
	/// All requested bytes were read.
	Complete,
	/// No byte existed at the requested boundary.
	Eof,
	/// Some but not all requested bytes existed.
	Truncated,
}

/// Validates persisted clocks before they can influence scanner boundaries.
pub(in crate::storage::wal) fn validate_recovery_seed(seed: RecoverySeed) -> FerrosGrynnResult<()> {
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
pub(in crate::storage::wal) fn published_from_seed(
	seed: RecoverySeed,
	mut scanned: PublishedState,
) -> PublishedState {
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
pub(in crate::storage::wal) fn recover_directory(
	directory: &Path,
	seed: RecoverySeed,
) -> FerrosGrynnResult<RecoveryResult> {
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
	pub(super) fn apply(
		&mut self,
		record: RawRecord,
		segment_start_lsn: Lsn,
	) -> FerrosGrynnResult<()> {
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
pub(in crate::storage::wal) const fn file_instance_successor(file_id: DataFileId) -> u64 {
	file_id.instance_id() + 1
}

/// Reads and validates one raw record without interpreting its kind.
pub(in crate::storage::wal) fn read_record(
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
pub(in crate::storage::wal) fn read_exact_boundary(
	reader: &mut File,
	output: &mut [u8],
) -> io::Result<ExactRead> {
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
pub(in crate::storage::wal) fn read_exact_required(
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
pub(in crate::storage::wal) fn list_segments(
	directory: &Path,
) -> FerrosGrynnResult<Vec<SegmentDescriptor>> {
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
