#![allow(
	clippy::wildcard_imports,
	reason = "move-only WAL split keeps the existing imports during stage 1"
)]

use super::*;

/// Test-only durability boundaries; failures poison the writer like real I/O.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommitFailurePoint {
	/// Ordinary production behavior.
	None,
	/// No transaction bytes have been appended.
	BeforeAppend,
	/// Complete records reached the buffered writer.
	AfterAppend,
	/// Buffered records reached the file, before synchronization.
	AfterFlush,
	/// The barrier succeeded, but no in-process visibility was published.
	AfterSync,
}

#[cfg(test)]
thread_local! {
	/// Scoped test thread state, never shared by unrelated commits.
	static COMMIT_FAILURE_POINT: std::cell::Cell<CommitFailurePoint> = const { std::cell::Cell::new(CommitFailurePoint::None) };
}

/// Restores the previous failure boundary when the test leaves its scope.
#[cfg(test)]
pub struct CommitFailureGuard(
	/// Restores the previous failure boundary when the test leaves its scope.
	CommitFailurePoint,
);

#[cfg(test)]
impl CommitFailureGuard {
	/// Installs one test failure after ordinary seed commits have completed.
	pub(crate) fn set(point: CommitFailurePoint) -> Self {
		Self(COMMIT_FAILURE_POINT.replace(point))
	}
}

#[cfg(test)]
impl Drop for CommitFailureGuard {
	/// Runs cleanup for this `CommitFailureGuard` before its fields are
	/// released.
	fn drop(&mut self) {
		COMMIT_FAILURE_POINT.set(self.0);
	}
}

/// Models an uncertain transport result without exposing partial publication.
#[cfg(test)]
fn inject_commit_failure(
	writer: &mut WriterState,
	point: CommitFailurePoint,
) -> FerrosGrynnResult<()> {
	if COMMIT_FAILURE_POINT.get() == point {
		writer.health = WriterHealth::Poisoned;
		return Err(io_failure(
			"injected WAL durability failure",
			io::Error::other("test commit boundary"),
		));
	}
	Ok(())
}

impl FrameWal {
	/// Rejects publication preparation after an uncertain transport failure,
	/// before callers create more unreachable files. Read pins remain usable.
	pub(in crate::storage) fn ensure_writable(&self) -> FerrosGrynnResult<()> {
		if self.inner.writer.lock().health == WriterHealth::Poisoned {
			return Err(io_failure(
				"prepare publication with poisoned WAL; reopen to recover",
				io::Error::other("previous WAL durability barrier failed"),
			));
		}
		Ok(())
	}

	/// Creates a new WAL directory and its first empty segment.
	///
	/// # Errors
	///
	/// Returns an I/O error if the directory or segment already exists or
	/// cannot be created.
	pub(in crate::storage) fn create_new(directory: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::create_new_with_target(directory, DEFAULT_SEGMENT_TARGET_BYTES)
	}

	/// Creates a WAL with an explicit segment target used by focused tests.
	///
	/// # Errors
	///
	/// Returns the same creation errors as [`Self::create_new`].
	pub(in crate::storage::wal) fn create_new_with_target(
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
	#[cfg(test)]
	pub(in crate::storage) fn open(directory: impl AsRef<Path>) -> FerrosGrynnResult<Self> {
		Self::open_with_seed(directory, RecoverySeed::EMPTY)
	}

	/// Opens an existing retained WAL using authoritative persisted clocks.
	///
	/// # Errors
	///
	/// Returns an error for an illegal pruned prefix, a gap after the persisted
	/// checkpoint, a missing matching CheckpointEnd, corruption, or clock
	/// exhaustion.
	pub(in crate::storage) fn open_with_seed(
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
	pub(in crate::storage::wal) fn from_recovery(
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
	pub(in crate::storage) fn snapshot(&self) -> StorageSnapshot {
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
	pub(in crate::storage) fn begin_tx(&self) -> FerrosGrynnResult<TxHandle> {
		self.begin_tx_with_max_age(None)
	}

	/// Starts a private write-set whose snapshot expires after `max_age`.
	pub(in crate::storage) fn begin_tx_with_max_age(
		&self,
		max_age: Option<Duration>,
	) -> FerrosGrynnResult<TxHandle> {
		let _snapshot_gate = self.inner.snapshot_gate.read();
		let raw_tx_id = self
			.inner
			.next_tx_id
			.try_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |current| {
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
	pub(in crate::storage) fn validate_tx(&self, tx: &TxHandle) -> FerrosGrynnResult<()> {
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
	pub(in crate::storage) fn commit_tx(
		&self,
		mut tx: TxHandle,
	) -> FerrosGrynnResult<CommitOutcome> {
		self.validate_tx(&tx)?;
		if tx.staged.is_empty() {
			let snapshot = self.snapshot();
			return Ok(CommitOutcome {
				#[cfg(test)]
				tx_id: tx.id,
				commit_ts: snapshot.commit_ts(),
				#[cfg(test)]
				commit_lsn: snapshot.visible_lsn(),
			});
		}

		// Lock order matches checkpoint publication: prune gate → writer.
		// Reserved version vectors cannot disappear before visibility handoff.
		let _prune_gate = self.inner.prune_gate.read();
		let mut writer = self.inner.writer.lock();
		if writer.health == WriterHealth::Poisoned {
			return Err(io_failure(
				"append to poisoned WAL; reopen it to recover the tail",
				io::Error::other("previous WAL append or durability barrier failed"),
			));
		}

		// Checkpoint pruning must not remove version vectors whose next slot
		// is admitted here. Writer serialization prevents concurrent inserts.
		let mut new_versions = RapidHashMap::default();
		let commit_ts = {
			let mut published = self.inner.published.write();
			for key in tx.staged.keys() {
				if published
					.latest_commit
					.get(key)
					.is_some_and(|latest| *latest > tx.snapshot.visible_lsn())
				{
					// The higher datastore layer rebases atom deltas before
					// opening this serialized storage transaction; this
					// guard still protects direct frame-WAL callers.
					return Err(FerrosGrynnError::serialization_failure(
						tx.id.get(),
						format!("page {:?} changed after the transaction snapshot", key),
					));
				}
			}
			let missing_versions =
				tx.staged.keys().filter(|key| !published.versions.contains_key(*key)).count();
			let missing_latest =
				tx.staged.keys().filter(|key| !published.latest_commit.contains_key(*key)).count();
			published.versions.try_reserve(missing_versions).map_err(|error| {
				invalid_wal("WAL visibility allocation failed").with_technical_source(error)
			})?;
			published.latest_commit.try_reserve(missing_latest).map_err(|error| {
				invalid_wal("WAL commit map allocation failed").with_technical_source(error)
			})?;
			new_versions.try_reserve(missing_versions).map_err(|error| {
				invalid_wal("WAL new version admission failed").with_technical_source(error)
			})?;
			for key in tx.staged.keys() {
				if let Some(versions) = published.versions.get_mut(key) {
					versions.try_reserve(1).map_err(|error| {
						invalid_wal("WAL version slot allocation failed")
							.with_technical_source(error)
					})?;
				} else {
					let mut versions = Vec::<FrameVersion>::new();
					versions.try_reserve_exact(1).map_err(|error| {
						invalid_wal("WAL first version allocation failed")
							.with_technical_source(error)
					})?;
					new_versions.insert(*key, versions);
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

		let mut publication = Vec::new();
		publication.try_reserve_exact(tx.staged.len()).map_err(|error| {
			invalid_wal("WAL publication admission failed").with_technical_source(error)
		})?;
		#[cfg(test)]
		inject_commit_failure(&mut writer, CommitFailurePoint::BeforeAppend)?;
		let append_result = writer.append_transaction(tx.id, commit_ts, &mut tx.staged);
		let (commit_lsn, committed_frames) = match append_result {
			Ok(result) => result,
			Err(error) => {
				writer.health = WriterHealth::Poisoned;
				return Err(error);
			}
		};
		// Validate the private insertion plan before the durability barrier.
		for (key, version) in committed_frames {
			let mut new = new_versions.remove(&key);
			if let Some(versions) = &mut new {
				versions.push(version);
			}
			publication.push((key, version, new));
		}

		#[cfg(test)]
		inject_commit_failure(&mut writer, CommitFailurePoint::AfterAppend)?;
		if let Err(source) = writer.segment.writer.flush() {
			writer.health = WriterHealth::Poisoned;
			return Err(io_failure("flush WAL transaction", source));
		}
		#[cfg(test)]
		inject_commit_failure(&mut writer, CommitFailurePoint::AfterFlush)?;
		if let Err(source) = writer.segment.writer.get_ref().sync_data() {
			writer.health = WriterHealth::Poisoned;
			return Err(io_failure("synchronize WAL transaction", source));
		}

		#[cfg(test)]
		inject_commit_failure(&mut writer, CommitFailurePoint::AfterSync)?;
		{
			let mut published = self.inner.published.write();
			for (key, version, new) in publication {
				if let Some(versions) = new {
					published.versions.insert(key, versions);
				} else if let Some(versions) = published.versions.get_mut(&key) {
					versions.push(version);
				}
				published.latest_commit.insert(key, commit_lsn);
				published.next_file_instance_id =
					published.next_file_instance_id.max(file_instance_successor(key.file_id()));
			}
			published.commit_ts = commit_ts.get();
			published.durable_visible_lsn = commit_lsn.get();
		}

		Ok(CommitOutcome {
			#[cfg(test)]
			tx_id: tx.id,
			commit_ts,
			#[cfg(test)]
			commit_lsn,
		})
	}

	/// Resolves an input-ordered set of page keys and retains the shared prune
	/// gate until the returned set is dropped.
	pub(in crate::storage) fn visible_frames(
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

	/// Returns the exclusive high-water page visible in WAL for one file.
	pub(in crate::storage) fn visible_file_page_count(
		&self,
		snapshot: StorageSnapshot,
		space_id: DataSpaceId,
		file_id: DataFileId,
	) -> Option<u64> {
		let published = self.inner.published.read();
		published
			.versions
			.iter()
			.filter(|(key, versions)| {
				key.space_id() == space_id
					&& key.file_id() == file_id
					&& versions.iter().any(|version| version.commit_lsn <= snapshot.visible_lsn())
			})
			.filter_map(|(key, _versions)| key.page_no().get().checked_add(1))
			.max()
	}

	/// Pins segments and reuses one handle for a bounded physical read scope.
	pub(in crate::storage) fn frame_read_scope(&self) -> FrameReadScope<'_> {
		FrameReadScope {
			wal: self,
			segment: None,
			_prune_gate: self.inner.prune_gate.read(),
		}
	}

	/// Loads one resolved frame while the caller holds the shared prune gate.
	pub(in crate::storage::wal) fn read_frame_unlocked(
		&self,
		frame: ResolvedFrame,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		let path = segment_path(&self.inner.directory, frame.version.location.segment_start_lsn);
		// Singleton reads open once; checkpoint scopes reuse the segment
		// handle.
		let mut file =
			File::open(path).map_err(|source| io_failure("open WAL frame segment", source))?;
		read_frame_from(&mut file, frame, output)
	}
}

impl FrameReadScope<'_> {
	/// Reads a resolved frame without reopening an unchanged segment.
	///
	/// # Errors
	///
	/// Returns the existing open, seek, exact-read, page or LSN validation
	/// error.
	pub(in crate::storage) fn read_frame(
		&mut self,
		frame: ResolvedFrame,
		output: &mut AlignedPage,
	) -> FerrosGrynnResult<PageHeader> {
		let start = frame.version.location.segment_start_lsn;
		if let Some((cached, file)) = &mut self.segment
			&& *cached == start
		{
			return read_frame_from(file, frame, output);
		}
		// Close the previous segment first: at most one handle is live.
		self.segment = None;
		let path = segment_path(&self.wal.inner.directory, start);
		let mut file =
			File::open(path).map_err(|source| io_failure("open WAL frame segment", source))?;
		let result = read_frame_from(&mut file, frame, output);
		self.segment = Some((start, file));
		result
	}
}

/// Loads and validates one resolved frame from its already opened segment.
///
/// # Errors
///
/// Returns the existing seek, exact-read, page-validation or LSN mismatch
/// error.
fn read_frame_from(
	file: &mut File,
	frame: ResolvedFrame,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<PageHeader> {
	file.seek(SeekFrom::Start(frame.version.location.page_bytes_offset))
		.map_err(|source| io_failure("seek to WAL page frame", source))?;
	read_exact_required(file, output.as_bytes_mut(), "read WAL page frame")?;
	let header = output.validate(frame.key.page_no())?;
	if header.page_lsn().get() != frame.version.frame_lsn.get() {
		return Err(invalid_wal(format!(
			"frame {} contains page LSN {}",
			frame.version.frame_lsn.get(),
			header.page_lsn().get()
		)));
	}
	Ok(header)
}

impl WriterState {
	/// Rotates before a transaction without splitting its records.
	pub(in crate::storage::wal) fn rotate(&mut self, directory: &Path) -> FerrosGrynnResult<()> {
		let next = ActiveSegment::create(directory, self.next_lsn)?;
		self.segment = next;
		Ok(())
	}

	/// Appends one transaction and advances record clocks without publishing
	/// it.
	pub(in crate::storage::wal) fn append_transaction(
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
			if staged_page.page.validate(key.page_no())?.page_type() == PageType::IndexCountSingle {
				stamp_count_index_lsn(&mut staged_page.page, frame_lsn.get())?;
			}
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
	pub(in crate::storage::wal) fn append_record(
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
	pub(in crate::storage::wal) fn create(
		directory: &Path,
		start_lsn: Lsn,
	) -> FerrosGrynnResult<Self> {
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
	pub(in crate::storage::wal) fn open(segment: SegmentDescriptor) -> FerrosGrynnResult<Self> {
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
	pub(in crate::storage::wal) fn append(
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
