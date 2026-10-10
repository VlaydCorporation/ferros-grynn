//! Owned SDK handle with a weak datastore owner and explicit control commits.

use std::sync::{Arc, Weak};

use super::{ConsumerKey, Replay};
use crate::{
	datastore::{Datastore, StoreInner},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveCursor, LiveId, LiveNext},
};

/// Private concrete adapter; public LIVE types expose no datastore machinery.
pub struct SubscriptionHandle {
	/// Waiting does not retain a transaction or keep the datastore open.
	store: Weak<StoreInner>,
	/// Exact transient attachment, independent of the stable query identity.
	key: ConsumerKey,
	/// Owned confirmed watermark, still accessible after shutdown or detach.
	cursor: LiveCursor,
	/// Polling is fused after terminal delivery, materialization failure or
	/// detach.
	fused: bool,
	/// Idempotence belongs only to a successfully committed durable capture
	/// stop.
	capture_stopped: bool,
	/// Lazy saved-result replay releases history pins before idle tail polling.
	replay: Option<Replay>,
}

impl SubscriptionHandle {
	/// Exports only a retained handle's checked GQL attachment identity.
	pub(in crate::datastore) const fn gql_attachment(&self) -> crate::datastore::GqlLiveAttachment {
		crate::datastore::GqlLiveAttachment::new(self.key)
	}

	/// Updates fixed cursor metadata after its successful ordinary publication.
	pub(in crate::datastore) fn apply_gql_ack(&mut self, ack: &crate::datastore::GqlLiveAck) {
		if self.gql_attachment() == ack.attachment
			&& ack.cursor.versionstamp() > self.cursor.versionstamp()
		{
			self.cursor = ack.cursor.clone();
		}
	}

	/// Constructs an already-admitted pending or active attachment.
	pub(in crate::datastore) fn new(
		store: &Arc<StoreInner>,
		key: ConsumerKey,
		cursor: LiveCursor,
	) -> Self {
		Self {
			store: Arc::downgrade(store),
			key,
			cursor,
			fused: false,
			capture_stopped: false,
			replay: None,
		}
	}

	/// Takes already-checked history references after atomic tail attachment.
	pub(in crate::datastore) fn with_replay(mut self, replay: Replay) -> Self {
		self.replay = Some(replay);
		self
	}

	/// Finalizes the registration watermark only after its successful barrier.
	pub(in crate::datastore) fn registered(&mut self, cursor: LiveCursor) {
		self.cursor = cursor;
	}

	/// Moves the already-admitted registration cursor/replay after commit.
	pub(in crate::datastore) fn finish_registration(&mut self) -> FerrosGrynnResult<()> {
		let store = self
			.store
			.upgrade()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE datastore is closed"))?;
		let (cursor, replay) = {
			let _commit = store.commit.lock();
			store.live_consumers.registration(self.key)?
		};
		self.registered(cursor);
		self.replay = replay;
		Ok(())
	}

	/// Returns the stable query incarnation.
	pub(crate) const fn id(&self) -> LiveId {
		self.key.id
	}

	/// Returns the owned confirmed watermark, never the outstanding token.
	pub(crate) const fn cursor(&self) -> &LiveCursor {
		&self.cursor
	}

	/// Polls the complete next delivery without retaining a snapshot.
	pub(crate) fn next(&mut self) -> FerrosGrynnResult<LiveNext> {
		let result = self.poll();
		if result.is_err() {
			// A failed materialization cannot continue beyond a skipped group.
			// Detach releases head capacity; a new resume retries from durable
			// ACK.
			self.replay = None;
			self.fused = true;
			if let Some(store) = self.store.upgrade() {
				let _commit = store.commit.lock();
				store.live_consumers.detach(self.key);
			}
		}
		result
	}

	/// Fuses errors at the outer boundary after every lock has been released.
	fn poll(&mut self) -> FerrosGrynnResult<LiveNext> {
		if self.fused {
			return Ok(LiveNext::Closed);
		}
		let Some(store) = self.store.upgrade() else {
			self.replay = None;
			self.fused = true;
			return Ok(LiveNext::Closed);
		};
		let pending = {
			let _commit = store.commit.lock();
			store.live_consumers.next(self.key)?
		};
		if !matches!(pending, LiveNext::Idle) || self.replay.is_none() {
			if matches!(pending, LiveNext::Closed) {
				self.replay = None;
				self.fused = true;
				let _commit = store.commit.lock();
				store.live_consumers.detach(self.key);
			}
			return Ok(pending);
		}
		let Some(replay) = &mut self.replay else {
			return Ok(LiveNext::Idle);
		};
		// Direct/WAL reads and materialization occur outside both mutexes.
		let historical = replay.next(&store, &self.cursor)?;
		let _commit = store.commit.lock();
		if let Some(delivery) = historical {
			let next = store.live_consumers.issue_replay(self.key, delivery)?;
			if matches!(next, LiveNext::Closed) {
				self.replay = None;
				self.fused = true;
				store.live_consumers.detach(self.key);
			}
			Ok(next)
		} else {
			let next = store.live_consumers.finish_replay(self.key)?;
			self.replay = None;
			Ok(next)
		}
	}

	/// Confirms a previously issued token through the ordinary durable barrier.
	pub(crate) fn ack(&mut self, token: &LiveCursor) -> FerrosGrynnResult<()> {
		token.validate(self.cursor.database_id(), self.cursor.live_id(), self.cursor.version())?;
		if token.versionstamp() <= self.cursor.versionstamp() {
			return Ok(());
		}
		if self.fused {
			return Err(FerrosGrynnError::invalid_argument("LIVE consumer is closed"));
		}
		let inner = self
			.store
			.upgrade()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE datastore is closed"))?;
		{
			let _commit = inner.commit.lock();
			if !inner.live_consumers.validate_ack(self.key, token)? {
				return Ok(());
			}
		}
		let store = Datastore::from_inner_alive(inner)?;
		let mut tx = store.begin();
		tx.acknowledge_live(token)?;
		tx.commit()?;
		// Queue confirmation is part of the same preallocated publication;
		// failed persistence cannot release its retained payload or capacity.
		self.cursor = token.clone();
		Ok(())
	}

	/// Stops the durable incarnation before releasing this attachment.
	pub(crate) fn close(&mut self) -> FerrosGrynnResult<()> {
		if self.capture_stopped {
			return Ok(());
		}
		let inner = self
			.store
			.upgrade()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE datastore is closed"))?;
		let store = Datastore::from_inner_alive(inner)?;
		let mut tx = store.begin();
		tx.close_live(self.key.id)?;
		tx.commit()?;
		{
			let _commit = store.inner.commit.lock();
			store.inner.live_consumers.detach(self.key);
		}
		self.capture_stopped = true;
		self.fused = true;
		self.replay = None;
		Ok(())
	}
}

impl Drop for SubscriptionHandle {
	fn drop(&mut self) {
		if !self.fused
			&& let Some(store) = self.store.upgrade()
		{
			// This guard is owned by the caller, outside Transaction::commit;
			// permit cleanup inside commit never takes this mutex again.
			let _commit = store.commit.lock();
			store.live_consumers.detach(self.key);
		}
	}
}
