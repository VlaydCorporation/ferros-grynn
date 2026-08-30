#![allow(
	clippy::wildcard_imports,
	reason = "move-only WAL split keeps the existing imports during stage 1"
)]

use super::*;

impl FrameWal {
	/// Captures a passive-checkpoint target atomically with reader
	/// registration.
	///
	/// Returns `None` when no reader-safe progress beyond the committed home
	/// watermark exists.
	pub(in crate::storage) fn capture_checkpoint_target(&self) -> Option<Lsn> {
		self.capture_checkpoint_target_with_cancellation(false)
	}

	/// Captures the durable tail after cancelling every active reader.
	pub(in crate::storage) fn capture_restart_checkpoint_target(&self) -> Option<Lsn> {
		self.capture_checkpoint_target_with_cancellation(true)
	}

	pub(in crate::storage::wal) fn capture_checkpoint_target_with_cancellation(
		&self,
		restart: bool,
	) -> Option<Lsn> {
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
	pub(in crate::storage) fn unbackfilled_page_count(&self) -> usize {
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
	pub(in crate::storage) fn begin_checkpoint(
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
	pub(in crate::storage) fn finish_checkpoint(
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
	pub(in crate::storage) fn publish_checkpoint(
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
	pub(in crate::storage) fn recovery_frames(&self) -> Vec<ResolvedFrame> {
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
	pub(in crate::storage::wal) fn prune_closed_segments(
		&self,
		target: Lsn,
	) -> FerrosGrynnResult<()> {
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
	pub(in crate::storage::wal) fn oldest_reader_lsn(&self) -> Option<Lsn> {
		self.inner
			.active_snapshots
			.lock()
			.values()
			.map(|registration| registration.snapshot.visible_lsn())
			.min()
	}
}
