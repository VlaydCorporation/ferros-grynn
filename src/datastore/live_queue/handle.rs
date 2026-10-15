//! Owned SDK handle with a weak datastore owner and explicit control commits.

use std::{
	future::Future,
	pin::Pin,
	sync::{Arc, Weak},
	task::{Context, Poll},
};

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
	/// Activation checkpoint is captured before the registration WAL barrier.
	graph_checkpoint: Option<Arc<crate::live::LiveGraphBatch>>,
	/// Bootstrap may inspect one replay item without issuing or acknowledging
	/// it.
	buffered_replay: Option<crate::live::LiveDelivery>,
}

/// One cancel-safe borrowed future; no task, runtime or second consumer exists.
pub struct Readiness<'a> {
	/// Borrows the single consumer for the entire cancel-safe wait.
	handle: &'a mut SubscriptionHandle,
	/// Identifies only this future's waiter slot for cancellation.
	registration: Option<u64>,
	/// Suspends data readiness while an unresolved conflict awaits a user
	/// decision.
	lifecycle_only: bool,
}

impl Future for Readiness<'_> {
	type Output = FerrosGrynnResult<()>;

	/// Polls this `Readiness<'_>` with the current task context.
	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		if this.handle.fused
			|| !this.lifecycle_only
				&& (this.handle.replay.is_some() || this.handle.buffered_replay.is_some())
		{
			return Poll::Ready(Ok(()));
		}
		let Some(store) = this.handle.store.upgrade() else {
			return Poll::Ready(Ok(()));
		};
		match store.live_consumers.poll_ready(
			this.handle.key,
			&mut this.registration,
			this.lifecycle_only,
			cx.waker(),
		) {
			Ok(true) => Poll::Ready(Ok(())),
			Ok(false) => Poll::Pending,
			Err(error) => Poll::Ready(Err(error)),
		}
	}
}

impl Drop for Readiness<'_> {
	/// Runs cleanup for this `Readiness<'_>` before its fields are released.
	fn drop(&mut self) {
		if let Some(registration) = self.registration
			&& let Some(store) = self.handle.store.upgrade()
		{
			store.live_consumers.cancel_ready(self.handle.key, registration);
		}
	}
}

impl SubscriptionHandle {
	/// A write-back transaction must belong to this exact live datastore owner.
	pub(crate) fn validate_store(&self, store: &Datastore) -> FerrosGrynnResult<()> {
		if self.store.upgrade().is_some_and(|owner| Arc::ptr_eq(&owner, &store.inner)) {
			Ok(())
		} else {
			Err(FerrosGrynnError::graph_write_back_unavailable())
		}
	}

	/// Waits on the existing queue predicate; Drop releases its fixed slot.
	pub(crate) fn ready(&mut self, lifecycle_only: bool) -> Readiness<'_> {
		Readiness {
			handle: self,
			registration: None,
			lifecycle_only,
		}
	}

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
			graph_checkpoint: None,
			buffered_replay: None,
		}
	}

	/// Takes already-checked history references after atomic tail attachment.
	pub(in crate::datastore) fn with_replay(mut self, replay: Replay) -> Self {
		self.replay = Some(replay);
		self
	}

	/// Resume captures this image under the same guard as its ACK and tail.
	pub(in crate::datastore) fn with_graph_checkpoint(
		mut self,
		checkpoint: Option<Arc<crate::live::LiveGraphBatch>>,
	) -> Self {
		self.graph_checkpoint = checkpoint;
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
		let (cursor, replay, graph) = {
			let _commit = store.commit.lock();
			store.live_consumers.registration(self.key)?
		};
		self.registered(cursor);
		self.replay = replay;
		self.graph_checkpoint = graph;
		Ok(())
	}

	/// Transfers the accepted activation image without looking at newer roots.
	pub(crate) fn take_graph_checkpoint(&mut self) -> Option<Arc<crate::live::LiveGraphBatch>> {
		let graph = self.graph_checkpoint.take();
		if graph.is_some()
			&& let Some(store) = self.store.upgrade()
		{
			let _commit = store.commit.lock();
			store.live_consumers.release_registration_graph(self.key);
		}
		graph
	}

	/// Restores an image at or before confirmed ACK; newer images need replay.
	pub(crate) fn graph_bootstrap(
		&mut self,
	) -> FerrosGrynnResult<
		Option<(Arc<crate::live::LiveGraphImage>, crate::changefeed::Versionstamp)>,
	> {
		let checkpoint = self.take_graph_checkpoint();
		if let Some(checkpoint) = &checkpoint {
			if checkpoint.versionstamp() <= self.cursor.versionstamp() {
				return Ok(Some((Arc::clone(&checkpoint.delta.after), checkpoint.versionstamp())));
			}
			if checkpoint.delta().base() <= self.cursor.versionstamp()
				&& let Some(before) = &checkpoint.delta.before
			{
				return Ok(Some((Arc::clone(before), checkpoint.delta().base())));
			}
		}
		let Some(replay) = &mut self.replay else {
			return Ok(None);
		};
		let store = self
			.store
			.upgrade()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE datastore is closed"))?;
		let Some(delivery) = replay.next(&store, &self.cursor)? else {
			return Ok(None);
		};
		let image = match delivery.payload() {
			crate::live::LivePayload::Graph(batch)
				if batch.delta().base() <= self.cursor.versionstamp() =>
			{
				batch.delta.before.as_ref().map(|before| (Arc::clone(before), batch.delta().base()))
			}
			crate::live::LivePayload::Batch(_)
			| crate::live::LivePayload::Graph(_)
			| crate::live::LivePayload::Gap(_)
			| crate::live::LivePayload::Fault {
				..
			} => None,
		};
		self.buffered_replay = Some(delivery);
		Ok(image)
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
			self.clear_private_buffers();
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
			self.clear_private_buffers();
			self.fused = true;
			return Ok(LiveNext::Closed);
		};
		let pending = {
			let _commit = store.commit.lock();
			store.live_consumers.next(self.key)?
		};
		if !matches!(pending, LiveNext::Idle)
			|| self.replay.is_none() && self.buffered_replay.is_none()
		{
			if matches!(pending, LiveNext::Closed) {
				self.clear_private_buffers();
				self.fused = true;
				let _commit = store.commit.lock();
				store.live_consumers.detach(self.key);
			}
			return Ok(pending);
		}
		// Direct/WAL reads and materialization occur outside both mutexes.
		let historical = if let Some(delivery) = self.buffered_replay.take() {
			Some(delivery)
		} else if let Some(replay) = &mut self.replay {
			replay.next(&store, &self.cursor)?
		} else {
			None
		};
		let _commit = store.commit.lock();
		if let Some(delivery) = historical {
			let next = store.live_consumers.issue_replay(self.key, delivery)?;
			if matches!(next, LiveNext::Closed) {
				self.clear_private_buffers();
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
		self.clear_private_buffers();
		Ok(())
	}

	/// Terminal cleanup releases unused saved payloads, not SDK agreed graphs.
	pub(crate) fn detach_closed(&mut self) {
		if let Some(store) = self.store.upgrade() {
			let _commit = store.commit.lock();
			store.live_consumers.detach(self.key);
		}
		self.capture_stopped = true;
		self.fused = true;
		self.clear_private_buffers();
	}

	/// Terminal cleanup releases unused saved payloads, not SDK agreed graphs.
	fn clear_private_buffers(&mut self) {
		self.replay = None;
		self.buffered_replay = None;
		self.graph_checkpoint = None;
	}
}

impl Drop for SubscriptionHandle {
	/// Runs cleanup for this `SubscriptionHandle` before its fields are
	/// released.
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

#[cfg(test)]
/// Regression tests for handle behavior.
mod tests {
	use super::*;
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		datastore::ClassSchema,
		graph::{AtomKind, GraphKind},
		live::{LiveExpr, LiveGraphResult, LiveOptions, LiveQuery, LiveSource},
		values::{PropertyMap, Value},
	};

	/// Three successors force bootstrap to inspect and buffer retained history.
	fn buffered_graph_replay() -> FerrosGrynnResult<(Datastore, crate::live::LiveSubscription)> {
		use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

		let store = Datastore::in_memory();
		let mut seed = store.begin();
		let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let key = seed.intern_property("value")?;
		let record = "item:a".parse()?;
		seed.create_atom(record, PropertyMap::from_iter([(key, Value::from(1))]))?;
		seed.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::from_nanoseconds(86_400_000_000_000)?,
		)?;
		seed.commit()?;
		let query = LiveQuery::new(LiveSource::Class(class)).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(Value::None))
				.with_family(GraphKind::Graph),
		);
		let subscription = store.subscribe_live(query, LiveOptions::default())?;
		let saved = subscription.cursor().clone();
		drop(subscription);
		for value in 2..=4 {
			let mut write = store.begin();
			write.set_base_property(&"item:a".parse()?, key, Value::from(value))?;
			write.commit()?;
		}
		let mut resumed = store.resume_live(&saved)?;
		let initial = resumed.inner.graph_bootstrap()?.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("retained graph predecessor absent")
		})?;
		(&(initial.1), &(saved.versionstamp())).assert_equal();
		(resumed.inner.buffered_replay.is_some()).assert_true();
		(&(resumed.cursor()), &(&saved)).assert_equal();
		Ok((store, resumed))
	}

	/// Verifies graph bootstrap terminal paths release buffered history.
	#[test]
	fn graph_bootstrap_terminal_paths_release_buffered_history() {
		use crate::test_assertions::AssertResult as _;

		let (_store, mut subscription) = buffered_graph_replay().assert_ok();
		subscription.close().assert_ok();
		assert!(subscription.inner.buffered_replay.is_none());
		assert!(subscription.inner.replay.is_none());
		assert!(subscription.inner.graph_checkpoint.is_none());
		let (store, mut subscription) = buffered_graph_replay().assert_ok();
		drop(store);
		assert!(matches!(subscription.next().assert_ok(), LiveNext::Closed));
		assert!(subscription.inner.buffered_replay.is_none());
		assert!(subscription.inner.replay.is_none());
	}

	/// A losing future releases its slot; publication wakes only outside locks.
	#[test]
	fn live_readiness_cancellation_publication_and_shutdown_are_race_safe() {
		use std::{
			sync::atomic::{AtomicUsize, Ordering},
			task::{Wake, Waker},
		};

		use crate::test_assertions::AssertResult as _;
		/// Waker test fixture observing notifications without owning the
		/// datastore.
		struct Probe {
			/// Backing blob store used to resolve unloaded tree nodes.
			store: Weak<StoreInner>,
			/// Shared counter recording wakes observed by the test fixture.
			wakes: AtomicUsize,
		}
		impl Wake for Probe {
			/// Returns wake for `Probe`.
			fn wake(self: Arc<Self>) {
				if let Some(store) = self.store.upgrade() {
					assert!(store.commit.try_lock().is_some(), "wake called under commit mutex");
				}
				self.wakes.fetch_add(1, Ordering::Relaxed);
			}
		}
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		let class =
			seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.commit().assert_ok();
		let mut subscription = store
			.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		let probe = Arc::new(Probe {
			store: Arc::downgrade(&store.inner),
			wakes: AtomicUsize::new(0),
		});
		let waker = Waker::from(Arc::clone(&probe));
		let mut context = Context::from_waker(&waker);
		{
			let mut readiness = subscription.inner.ready(false);
			assert!(Pin::new(&mut readiness).poll(&mut context).is_pending());
		}
		let mut write = store.begin();
		write.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
		write.commit().assert_ok();
		assert_eq!(probe.wakes.load(Ordering::Relaxed), 0);
		let LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("ready delivery absent"));
				test_result.assert_ok();
			};
		};
		subscription.ack(delivery.token()).assert_ok();
		{
			let mut readiness = subscription.inner.ready(false);
			assert!(Pin::new(&mut readiness).poll(&mut context).is_pending());
			let mut write = store.begin();
			write.create_atom("item:b".parse().assert_ok(), PropertyMap::new()).assert_ok();
			write.commit().assert_ok();
			assert_eq!(probe.wakes.load(Ordering::Relaxed), 1);
			assert!(matches!(Pin::new(&mut readiness).poll(&mut context), Poll::Ready(Ok(()))));
		}
		let LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("second delivery absent"));
				test_result.assert_ok();
			};
		};
		subscription.ack(delivery.token()).assert_ok();
		{
			let mut readiness = subscription.inner.ready(false);
			assert!(Pin::new(&mut readiness).poll(&mut context).is_pending());
			drop(store);
			assert_eq!(probe.wakes.load(Ordering::Relaxed), 2);
			assert!(matches!(Pin::new(&mut readiness).poll(&mut context), Poll::Ready(Ok(()))));
		}
		assert!(matches!(subscription.next().assert_ok(), LiveNext::Closed));
	}
}
