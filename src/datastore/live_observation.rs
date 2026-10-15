//! Constant-size non-authoritative observations over existing LIVE owners.

use std::sync::atomic::{AtomicU64, Ordering};

use super::{DatabaseRoot, Datastore, changefeed_observation::add};
use crate::{
	gql_status::FerrosGrynnResult,
	live::{LivePayload, LiveStatistics},
};

/// Payload-free process counters shared by datastore handles.
#[derive(Default)]
pub(super) struct Counters {
	/// Query candidate evaluation attempts.
	queries: AtomicU64,
	/// Binding candidate evaluation attempts.
	bindings: AtomicU64,
	/// Known charged evaluation work, excluding unreported failed kernel work.
	work: AtomicU64,
	/// Conservative admission high-water mark.
	scratch: AtomicU64,
	/// Persisted current-stamp live batches.
	batches: AtomicU64,
	/// Events in those batches.
	events: AtomicU64,
	/// Explicit live GAPs.
	gaps: AtomicU64,
	/// Live fault notifications.
	faults: AtomicU64,
	/// Persisted computed groups.
	groups: AtomicU64,
	/// Persisted logical envelope bytes.
	bytes: AtomicU64,
	/// Persisted GAP/fault controls.
	controls: AtomicU64,
	/// Successful ACK watermark advances.
	acks: AtomicU64,
	/// Successfully charged historical replay work.
	replay: AtomicU64,
	/// Reader-local logical page requests.
	pages: AtomicU64,
}

/// Scalar observations captured before persistence clears local RAM history.
#[derive(Default)]
pub(super) struct Publication {
	/// Volatile and durable batches for this stamp.
	batches: u64,
	/// Complete batch event count.
	events: u64,
	/// Explicit live GAP count.
	gaps: u64,
	/// Evaluation fault count.
	faults: u64,
	/// Actually durable computed groups.
	groups: u64,
	/// Actually durable logical bytes.
	bytes: u64,
	/// Actually durable controls, including import suppression.
	controls: u64,
	/// Advances of existing same-version confirmed watermarks.
	acks: u64,
}

impl Publication {
	/// Reads catalog metadata and newly published immutable handles, including
	/// backfill.
	pub(super) fn capture(root: &DatabaseRoot, previous: Option<&DatabaseRoot>) -> Self {
		let mut result = Self::default();
		for (_, payload) in root.live_catalog().prepared.iter() {
			match payload.as_ref() {
				LivePayload::Batch(batch) if batch.versionstamp().get() == root.commit_ts => {
					result.batches = result.batches.saturating_add(1);
					result.events = result.events.saturating_add(batch.events().len() as u64);
				}
				LivePayload::Graph(batch) if batch.versionstamp().get() == root.commit_ts => {
					result.batches = result.batches.saturating_add(1);
					result.events =
						result.events.saturating_add(batch.delta().changes().len().max(1) as u64);
				}
				LivePayload::Gap(gap)
					if gap.through().is_some_and(|stamp| stamp.get() == root.commit_ts) =>
				{
					result.gaps = result.gaps.saturating_add(1);
				}
				LivePayload::Fault {
					gap,
					..
				} if gap.through().is_some_and(|stamp| stamp.get() == root.commit_ts) => {
					result.faults = result.faults.saturating_add(1);
				}
				_ => {}
			}
		}
		for (id, history) in &root.live_catalog().histories {
			let prior = previous
				.and_then(|root| root.live_catalog().histories.get(id))
				.map_or(0, |history| history.last_stamp);
			let floor = prior.max(history.purged_through);
			if history.last_stamp <= floor {
				continue;
			}
			let range = (
				std::ops::Bound::Excluded((*id, floor)),
				std::ops::Bound::Included((*id, history.last_stamp)),
			);
			for (_, bytes) in root.live_history().range(range) {
				result.groups = result.groups.saturating_add(1);
				result.bytes = result.bytes.saturating_add(bytes.len() as u64);
				if crate::storage::live_history::inspect(bytes, bytes.len() as u64)
					.is_ok_and(|info| info.is_control())
				{
					result.controls = result.controls.saturating_add(1);
				}
			}
		}
		if let Some(previous) = previous {
			for (id, next) in &root.live_catalog().queries {
				if previous.live_catalog().queries.get(id).is_some_and(|old| {
					old.version == next.version && old.acknowledged < next.acknowledged
				}) {
					result.acks = result.acks.saturating_add(1);
				}
			}
		}
		result
	}
}

impl Counters {
	/// Records a query preparation attempt before its outcome is known.
	pub(super) fn query_attempt(&self) {
		add(&self.queries, 1);
	}

	/// Records a binding preparation attempt before its outcome is known.
	pub(super) fn binding_attempt(&self) {
		add(&self.bindings, 1);
	}

	/// Records known admission even when later preparation fails.
	pub(super) fn admitted(&self, budget: &super::BindingAdmission) {
		let (bytes, work) = budget.observation();
		if work != 0 {
			add(&self.work, work);
		}
		self.scratch.fetch_max(bytes as u64, Ordering::Relaxed);
	}

	/// Records only complete successful persistence, never an uncertain
	/// failure.
	pub(super) fn published(&self, value: Publication) {
		for (counter, amount) in [
			(&self.batches, value.batches),
			(&self.events, value.events),
			(&self.gaps, value.gaps),
			(&self.faults, value.faults),
			(&self.groups, value.groups),
			(&self.bytes, value.bytes),
			(&self.controls, value.controls),
			(&self.acks, value.acks),
		] {
			if amount != 0 {
				add(counter, amount);
			}
		}
	}

	/// Records charged replay work once, including the first pull's metadata.
	pub(super) fn replay_work(&self, work: u64) {
		if work != 0 {
			add(&self.replay, work);
		}
	}

	/// Records physical-history page attempts, including failed optional reads.
	pub(super) fn page_reads(&self, pages: u64) {
		if pages != 0 {
			add(&self.pages, pages);
		}
	}
}

impl Datastore {
	/// Returns payload-free process diagnostics and current consumer gauges.
	///
	/// Counters reset on open; they do not replace snapshot-consistent
	/// `LiveInfo`.
	///
	/// ```
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// let store = ferros_grynn::datastore::Datastore::in_memory();
	/// let statistics = store.live_statistics()?;
	/// assert_eq!(statistics.query_evaluation_attempts, 0);
	/// assert_eq!(statistics.consumers, 0);
	/// # Ok(()) }
	/// ```
	///
	/// # Errors
	///
	/// Returns checked consumer footprint or bounded metadata errors.
	pub fn live_statistics(&self) -> FerrosGrynnResult<LiveStatistics> {
		let root = self.inner.current_root();
		let (consumers, events, retained, issued) = self.inner.live_consumers.observations()?;
		let counters = &self.inner.live_observation;
		Ok(LiveStatistics {
			query_evaluation_attempts: counters.queries.load(Ordering::Relaxed),
			binding_evaluation_attempts: counters.bindings.load(Ordering::Relaxed),
			admitted_evaluation_work: counters.work.load(Ordering::Relaxed),
			peak_admitted_scratch_bytes: counters.scratch.load(Ordering::Relaxed),
			published_batches: counters.batches.load(Ordering::Relaxed),
			published_events: counters.events.load(Ordering::Relaxed),
			published_gaps: counters.gaps.load(Ordering::Relaxed),
			published_faults: counters.faults.load(Ordering::Relaxed),
			durable_history_groups: counters.groups.load(Ordering::Relaxed),
			durable_history_bytes: counters.bytes.load(Ordering::Relaxed),
			durable_history_controls: counters.controls.load(Ordering::Relaxed),
			deliveries_issued: issued,
			acknowledgements: counters.acks.load(Ordering::Relaxed),
			admitted_replay_work: counters.replay.load(Ordering::Relaxed),
			history_page_reads: counters.pages.load(Ordering::Relaxed),
			consumers,
			queued_events: events,
			retained_consumer_bytes: retained,
			registered_queries: root.live_catalog().queries.len() as u64,
			registered_bindings: root.live_catalog().bindings.len() as u64,
		})
	}
}

#[cfg(test)]
/// Regression tests for live observation behavior.
mod tests {
	use super::*;
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		datastore::{ClassField, ClassSchema},
		graph::AtomKind,
		live::{LiveNext, LiveOptions, LiveQuery, LiveSource},
		values::{PropertyMap, RecordId, Type, Value},
	};

	/// Publication, first issue and ACK are counted once while gauges track
	/// retained data.
	#[test]
	fn live_statistics_publication_delivery_ack_and_disabled_paths() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		assert_eq!(store.live_statistics().assert_ok(), LiveStatistics::default());
		let mut setup = store.begin();
		let class = setup
			.define_class(
				ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)])
					.assert_ok(),
			)
			.assert_ok();
		let key = setup.intern_property("value").assert_ok();
		setup
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
			)
			.assert_ok();
		let record: RecordId = "item:a".parse().assert_ok();
		setup
			.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))
			.assert_ok();
		setup.commit().assert_ok();
		assert_eq!(store.live_statistics().assert_ok().query_evaluation_attempts, 0);
		assert_eq!(store.live_statistics().assert_ok().published_events, 0);
		let mut binding = store.begin();
		binding
			.define_live_binding(
				crate::live::LiveExpr::Literal(1.into()),
				crate::values::Object::new(),
				crate::live::LiveBindingOptions::default(),
			)
			.assert_ok();
		binding.commit().assert_ok();
		assert!(store.live_statistics().assert_ok().binding_evaluation_attempts > 0);
		assert_eq!(store.live_statistics().assert_ok().registered_bindings, 1);
		let mut handle = store
			.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
			.assert_ok();
		let initial = store.live_statistics().assert_ok();
		assert!(initial.query_evaluation_attempts > 0);
		assert_eq!(initial.acknowledgements, 0, "registration is not ACK");
		assert_eq!(initial.consumers, 1);
		assert!(matches!(handle.next().assert_ok(), LiveNext::Idle));
		let mut update = store.begin();
		update.set_base_property(&record, key, 2.into()).assert_ok();
		update.commit().assert_ok();
		let published = store.live_statistics().assert_ok();
		assert_eq!(published.published_batches - initial.published_batches, 1);
		assert_eq!(published.published_events - initial.published_events, 1);
		assert_eq!(published.durable_history_groups - initial.durable_history_groups, 1);
		assert!(published.durable_history_bytes > initial.durable_history_bytes);
		assert!(published.admitted_evaluation_work > initial.admitted_evaluation_work);
		assert!(published.peak_admitted_scratch_bytes > 0);
		assert_eq!(published.queued_events, 1);
		let LiveNext::Delivery(delivery) = handle.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"statistics delivery missing",
					));
				test_result.assert_ok();
			};
		};
		let issued = store.live_statistics().assert_ok();
		assert_eq!(issued.deliveries_issued - initial.deliveries_issued, 1);
		let LiveNext::Delivery(repeated) = handle.next().assert_ok() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"statistics repeated delivery missing",
					));
				test_result.assert_ok();
			};
		};
		assert_eq!(delivery.token(), repeated.token());
		assert_eq!(store.live_statistics().assert_ok().deliveries_issued, issued.deliveries_issued);
		handle.ack(delivery.token()).assert_ok();
		let confirmed = store.live_statistics().assert_ok();
		assert_eq!(confirmed.acknowledgements, initial.acknowledgements + 1);
		assert_eq!(confirmed.queued_events, 0);
		handle.ack(delivery.token()).assert_ok();
		assert_eq!(
			store.live_statistics().assert_ok().acknowledgements,
			confirmed.acknowledgements
		);
		handle.close().assert_ok();
		let closed = store.live_statistics().assert_ok();
		assert_eq!(closed.consumers, 0);
		assert_eq!(closed.retained_consumer_bytes, 0);
		assert_eq!(closed.published_batches, published.published_batches);
	}

	/// Import suppression preserves live batches while durable controls remain
	/// explicit.
	#[test]
	fn live_statistics_import_capture_and_suppression() {
		use crate::{
			datastore::{ImportChangefeedMode, ImportOptions},
			test_assertions::AssertResult as _,
		};
		for mode in [ImportChangefeedMode::On, ImportChangefeedMode::Off] {
			let store = Datastore::in_memory();
			let mut setup = store.begin();
			let class = setup
				.define_class(
					ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)])
						.assert_ok(),
				)
				.assert_ok();
			let key = setup.intern_property("value").assert_ok();
			let record: RecordId = "item:a".parse().assert_ok();
			setup
				.create_atom(record.clone(), PropertyMap::from_iter([(key, 1.into())]))
				.assert_ok();
			setup
				.configure_changefeed(
					ChangefeedSource::Class(class),
					ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
				)
				.assert_ok();
			setup.commit().assert_ok();
			let mut handle = store
				.subscribe_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())
				.assert_ok();
			let before = store.live_statistics().assert_ok();
			store
				.import(
					ImportOptions {
						changefeed: mode,
						segment_size: 1,
						..ImportOptions::default()
					},
					|session| {
						for value in [2_i64, 3] {
							session.apply_item(|tx| {
								tx.set_base_property(&record, key, value.into())
							})?;
						}
						Ok(())
					},
				)
				.assert_ok();
			let after = store.live_statistics().assert_ok();
			assert_eq!(after.published_batches - before.published_batches, 1);
			assert_eq!(after.published_events - before.published_events, 1);
			assert_eq!(after.durable_history_groups - before.durable_history_groups, 1);
			assert_eq!(
				after.durable_history_controls - before.durable_history_controls,
				u64::from(mode == ImportChangefeedMode::Off)
			);
			assert!(
				matches!(handle.next().assert_ok(), LiveNext::Delivery(delivery) if matches!(delivery.payload(), LivePayload::Batch(_)))
			);
			handle.close().assert_ok();
		}
	}

	/// Fresh historical registration counts all newly durable envelopes,
	/// without recounting ACKs.
	#[test]
	fn live_statistics_registration_backfill_and_replay() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(
				ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)])
					.assert_ok(),
			)
			.assert_ok();
		let key = setup.intern_property("value").assert_ok();
		setup
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::new("P1D".parse().assert_ok()).assert_ok(),
			)
			.assert_ok();
		let floor = setup.commit().assert_ok().versionstamp();
		let record: RecordId = "item:a".parse().assert_ok();
		let mut create = store.begin();
		create
			.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))
			.assert_ok();
		create.commit().assert_ok();
		let mut update = store.begin();
		update.set_base_property(&record, key, 2.into()).assert_ok();
		update.commit().assert_ok();
		let before = store.live_statistics().assert_ok();
		let mut handle = store
			.subscribe_live(
				LiveQuery::new(LiveSource::Class(class)),
				LiveOptions::default().with_since(crate::live::LiveSince::Versionstamp(floor)),
			)
			.assert_ok();
		let published = store.live_statistics().assert_ok();
		assert_eq!(published.durable_history_groups - before.durable_history_groups, 2);
		assert!(published.durable_history_bytes > before.durable_history_bytes);
		assert_eq!(
			published.published_batches, before.published_batches,
			"no current-stamp delta was synthesized"
		);
		for _ in 0..2 {
			let LiveNext::Delivery(delivery) = handle.next().assert_ok() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"statistics historical delivery missing",
						));
					test_result.assert_ok();
				};
			};
			let first = store.live_statistics().assert_ok();
			assert!(first.admitted_replay_work > 0);
			let LiveNext::Delivery(repeated) = handle.next().assert_ok() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"statistics repeated history missing",
						));
					test_result.assert_ok();
				};
			};
			assert_eq!(repeated.token(), delivery.token());
			let again = store.live_statistics().assert_ok();
			assert_eq!(again.admitted_replay_work, first.admitted_replay_work);
			assert_eq!(again.deliveries_issued, first.deliveries_issued);
			handle.ack(delivery.token()).assert_ok();
		}
		assert!(matches!(handle.next().assert_ok(), LiveNext::Idle));
		let after = store.live_statistics().assert_ok();
		assert_eq!(
			after.durable_history_groups, published.durable_history_groups,
			"ACK did not republish old envelopes"
		);
		assert_eq!(after.history_page_reads, 0, "RAM history has no physical page requests");
		handle.close().assert_ok();
	}
}
