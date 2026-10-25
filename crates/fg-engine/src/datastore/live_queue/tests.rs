//! Independent queue state transitions with whole-commit synthetic deliveries.

use super::*;
use crate::{
	live::{LiveBatch, LiveEvent, LiveId, LiveIdentity, LiveOverflowPolicy, LiveVersion},
	values::{DateTime, Uuid},
};

/// Uses an owned scalar result without datastore or storage reads.
fn batch(cursor: &LiveCursor, stamp: u64, count: usize) -> FerrosGrynnResult<LiveDelivery> {
	let token =
		LiveCursor::new(cursor.database_id(), cursor.live_id(), cursor.version(), stamp.into())?;
	let events = (0..count)
		.map(|ordinal| {
			Ok(LiveEvent {
				output: None,
				ordinal: u32::try_from(ordinal).map_err(|_error| overflow())?,
				identity: LiveIdentity::Aggregate(vec![Value::from(ordinal as u64)]),
				before: None,
				after: Some(Value::from(stamp)),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	Ok(LiveDelivery {
		token,
		payload: Arc::new(LivePayload::Batch(LiveBatch {
			id: cursor.live_id(),
			version: cursor.version(),
			stamp: stamp.into(),
			committed_at: DateTime::from(
				jiff::Timestamp::from_nanosecond(0)?.to_zoned(jiff::tz::TimeZone::UTC),
			),
			events,
			bindings: Vec::new(),
		})),
	})
}

/// Creates a bounded fault control independently of evaluation.
fn fault(cursor: &LiveCursor, stamp: u64, from: Option<u64>) -> FerrosGrynnResult<LiveDelivery> {
	Ok(LiveDelivery {
		token: LiveCursor::new(
			cursor.database_id(),
			cursor.live_id(),
			cursor.version(),
			stamp.into(),
		)?,
		payload: Arc::new(LivePayload::Fault {
			diagnostic: FerrosGrynnError::live_evaluation_fault(
				cursor.live_id().to_string(),
				"FG-G1F09",
				"fixture",
			),
			gap: LiveGap {
				id: cursor.live_id(),
				reason: LiveGapReason::EvaluationFault,
				from: from.map(Into::into),
				through: Some(stamp.into()),
			},
		}),
	})
}

/// Fixture starts after registration rather than emitting initial data.
fn queue() -> FerrosGrynnResult<Queue> {
	Ok(Queue::new(LiveCursor::new(
		Uuid::now_v7(),
		LiveId::from_uuid(Uuid::now_v7())?,
		LiveVersion::default(),
		1.into(),
	)?))
}

/// An owned replay head cannot consume the separately admitted live tail
/// budget.
#[test]
fn live_queue_historical_head_preserves_bounded_tail() {
	use crate::test_assertions::AssertResult as _;

	let mut queue = queue().assert_ok();
	let historical = batch(queue.cursor(), 2, 1).assert_ok();
	queue.inflight = Some(Entry {
		cost: Footprint::delivery(&historical).assert_ok(),
		delivery: historical,
	});
	let tail = batch(queue.cursor(), 3, 1).assert_ok();
	let options = LiveOptions::default()
		.with_queue(1, 1 << 20)
		.assert_ok()
		.with_overflow(LiveOverflowPolicy::Block);
	assert!(matches!(
		queue.prepare(&tail, &options, Footprint::default(), 1 << 20).assert_ok(),
		PreparedQueue::Blocked
	));
	let admitted = ready(
		queue
			.prepare_with_head(&tail, &options, Footprint::default(), 1 << 20, Some(2))
			.assert_ok(),
	)
	.assert_ok();
	assert_eq!(admitted.pending.len(), 1);
	assert_eq!(
		admitted.inflight.as_ref().map(|head| head.delivery.token().versionstamp().get()),
		Some(2)
	);
}

/// Tight byte admission includes every data slot, even beside a replay head.
#[test]
fn live_queue_tail_byte_boundary_includes_incoming_slot() {
	use crate::test_assertions::AssertResult as _;

	for historical in [false, true] {
		let mut queue = queue().assert_ok();
		let first = batch(queue.cursor(), 3, 1).assert_ok();
		let second = batch(queue.cursor(), 4, 1).assert_ok();
		let bytes = Footprint::delivery(&first).assert_ok().bytes
			+ Footprint::delivery(&second).assert_ok().bytes
			+ size_of::<Entry>();
		let options = LiveOptions::default()
			.with_queue(2, bytes)
			.assert_ok()
			.with_overflow(LiveOverflowPolicy::Block);
		let cutoff = historical.then_some(2);
		if historical {
			let head = batch(queue.cursor(), 2, 1).assert_ok();
			queue.inflight = Some(Entry {
				cost: Footprint::delivery(&head).assert_ok(),
				delivery: head,
			});
		}
		let first = ready(
			queue
				.prepare_with_head(&first, &options, Footprint::default(), 1 << 20, cutoff)
				.assert_ok(),
		)
		.assert_ok();
		assert!(matches!(
			first
				.prepare_with_head(&second, &options, Footprint::default(), 1 << 20, cutoff)
				.assert_ok(),
			PreparedQueue::Blocked
		));
	}
}

/// A blocked/oversized admission is not an installed queue.
fn ready(prepared: PreparedQueue) -> FerrosGrynnResult<Queue> {
	match prepared {
		PreparedQueue::Ready(queue) => Ok(queue),
		_ => Err(FerrosGrynnError::invalid_argument("fixture expected admitted queue")),
	}
}

/// Verifies live queue repeats issued batch and rejects forged ack.
#[test]
fn live_queue_repeats_issued_batch_and_rejects_forged_ack() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	assert!(matches!(state.next(), LiveNext::Idle));
	let first = batch(state.cursor(), 2, 2).assert_ok();
	state = ready(
		state
			.prepare(&first, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.assert_ok(),
	)
	.assert_ok();
	assert!(state.validate_ack(first.token()).is_err());
	for _ in 0..3 {
		let LiveNext::Delivery(issued) = state.next() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("missing delivery"));
				test_result.assert_ok();
			};
		};
		assert_eq!(issued.token(), first.token());
		assert!(Arc::ptr_eq(&issued.payload, &first.payload));
		assert_eq!(state.cursor().versionstamp().get(), 1);
	}
	let forged = batch(state.cursor(), 3, 1).assert_ok();
	assert!(state.validate_ack(forged.token()).is_err());
	let foreign =
		LiveCursor::new(Uuid::now_v7(), first.token.live_id(), first.token.version(), 2.into())
			.assert_ok();
	assert!(state.validate_ack(&foreign).is_err());
	let wrong_query = LiveCursor::new(
		first.token.database_id(),
		LiveId::from_uuid(Uuid::now_v7()).assert_ok(),
		first.token.version(),
		2.into(),
	)
	.assert_ok();
	assert!(state.validate_ack(&wrong_query).is_err());
	let wrong_version = LiveCursor::new(
		first.token.database_id(),
		first.token.live_id(),
		LiveVersion::new(2).assert_ok(),
		2.into(),
	)
	.assert_ok();
	assert!(state.validate_ack(&wrong_version).is_err());
	let before_ack = state.footprint().assert_ok();
	assert!(state.validate_ack(first.token()).assert_ok());
	assert_eq!(state.footprint().assert_ok(), before_ack); // Failed durable ACK leaves capacity retained.
	state.confirm(first.token()).assert_ok();
	state.confirm(first.token()).assert_ok();
	assert_eq!(state.cursor(), first.token());
	assert_eq!(state.footprint().assert_ok().entries, 0);
	assert!(matches!(state.next(), LiveNext::Idle));
}

/// Verifies live queue control merge accounts intervening tail and unknown
/// bounds.
#[test]
fn live_queue_control_merge_accounts_intervening_tail_and_unknown_bounds() {
	use crate::test_assertions::AssertResult as _;

	for unknown in [false, true] {
		let mut state = queue().assert_ok();
		let options = LiveOptions::default().with_queue(1, 256 * 1024).assert_ok();
		let first = batch(state.cursor(), 2, 1).assert_ok();
		state =
			ready(state.prepare(&first, &options, Footprint::default(), SESSION_BYTES).assert_ok())
				.assert_ok();
		let second = batch(state.cursor(), 3, 1).assert_ok();
		state = ready(
			state.prepare(&second, &options, Footprint::default(), SESSION_BYTES).assert_ok(),
		)
		.assert_ok();
		if unknown {
			let entry = state
				.pending
				.front_mut()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("missing fixture control"))
				.assert_ok();
			let LivePayload::Gap(gap) = Arc::make_mut(&mut entry.delivery.payload) else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("expected fixture GAP"));
					test_result.assert_ok();
				};
			};
			gap.from = None;
		}
		let third = batch(state.cursor(), 4, 1).assert_ok();
		state =
			ready(state.prepare(&third, &options, Footprint::default(), SESSION_BYTES).assert_ok())
				.assert_ok();
		let control = fault(state.cursor(), 5, Some(5)).assert_ok();
		state = ready(
			state.prepare(&control, &options, Footprint::default(), SESSION_BYTES).assert_ok(),
		)
		.assert_ok();
		let LiveNext::Delivery(delivery) = state.next() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("missing merged control"));
				test_result.assert_ok();
			};
		};
		let LivePayload::Fault {
			gap,
			..
		} = delivery.payload()
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("lost fault diagnostic"));
				test_result.assert_ok();
			};
		};
		assert_eq!(
			gap.from().map(|stamp| stamp.get()),
			if unknown {
				None
			} else {
				Some(2)
			}
		);
		assert_eq!(gap.through().map(|stamp| stamp.get()), Some(5));
		state.confirm(delivery.token()).assert_ok();
		assert!(
			matches!(state.next(), LiveNext::Idle),
			"batch4 was covered by the explicit merged interval"
		);
	}
}

/// Verifies live queue counts reserved vectors and ring capacity after drain.
#[test]
fn live_queue_counts_reserved_vectors_and_ring_capacity_after_drain() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	let mut delivery = batch(state.cursor(), 2, 1).assert_ok();
	let LivePayload::Batch(value) = Arc::make_mut(&mut delivery.payload) else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("fixture batch"));
			test_result.assert_ok();
		};
	};
	value
		.events
		.try_reserve_exact(10_000)
		.map_err(|error| resource("fixture", SESSION_BYTES).with_technical_source(error))
		.assert_ok();
	let event_bytes = value.events.capacity() * size_of::<LiveEvent>();
	assert!(Footprint::delivery(&delivery).assert_ok().bytes >= event_bytes);
	assert!(matches!(
		state
			.prepare(
				&delivery,
				&LiveOptions::default().with_queue(10, 1024).assert_ok(),
				Footprint::default(),
				SESSION_BYTES
			)
			.assert_ok(),
		PreparedQueue::Oversized
	));
	let options = LiveOptions::default();
	for stamp in 2..100 {
		let delivery = batch(state.cursor(), stamp, 1).assert_ok();
		state = ready(
			state.prepare(&delivery, &options, Footprint::default(), SESSION_BYTES).assert_ok(),
		)
		.assert_ok();
	}
	let allocation = state.pending.capacity().saturating_sub(1) * size_of::<Entry>();
	while let LiveNext::Delivery(delivery) = state.next() {
		state.confirm(delivery.token()).assert_ok();
	}
	assert_eq!(state.footprint().assert_ok().entries, 0);
	assert_eq!(state.footprint().assert_ok().bytes, QUEUE_BYTES + allocation);
}

/// Verifies live queue rejects duplicate and regressing pending tokens.
#[test]
fn live_queue_rejects_duplicate_and_regressing_pending_tokens() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	let delivery = batch(state.cursor(), 3, 1).assert_ok();
	state = ready(
		state
			.prepare(&delivery, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.assert_ok(),
	)
	.assert_ok();
	assert!(
		state
			.prepare(&delivery, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.is_err()
	);
	let regressing = batch(state.cursor(), 2, 1).assert_ok();
	assert!(
		state
			.prepare(&regressing, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.is_err()
	);
	let _issued = state.next();
	assert!(
		state
			.prepare(&delivery, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.is_err()
	);
}

/// Verifies live queue later overflow preserves unknown history start.
#[test]
fn live_queue_later_overflow_preserves_unknown_history_start() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	let options = LiveOptions::default().with_queue(1, 16 * 1024).assert_ok();
	let token = LiveCursor::new(
		state.cursor().database_id(),
		state.cursor().live_id(),
		state.cursor().version(),
		2.into(),
	)
	.assert_ok();
	let unknown = LiveDelivery {
		token,
		payload: Arc::new(LivePayload::Gap(crate::live::LiveGap {
			id: state.cursor().live_id(),
			reason: crate::live::LiveGapReason::InsufficientCoverage,
			from: None,
			through: Some(2.into()),
		})),
	};
	state =
		ready(state.prepare(&unknown, &options, Footprint::default(), SESSION_BYTES).assert_ok())
			.assert_ok();
	for stamp in 3..=4 {
		let data = batch(state.cursor(), stamp, 1).assert_ok();
		state =
			ready(state.prepare(&data, &options, Footprint::default(), SESSION_BYTES).assert_ok())
				.assert_ok();
	}
	let LiveNext::Delivery(delivery) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing unknown GAP"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Gap(gap) = delivery.payload() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("unknown GAP was lost"));
			test_result.assert_ok();
		};
	};
	assert_eq!(gap.from(), None);
	assert_eq!(gap.through().map(|stamp| stamp.get()), Some(4));
}

/// Verifies live queue session overflow preserves prepaid fault diagnostic.
#[test]
fn live_queue_session_overflow_preserves_prepaid_fault_diagnostic() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	let data = batch(state.cursor(), 2, 1).assert_ok();
	state = ready(
		state
			.prepare(&data, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.assert_ok(),
	)
	.assert_ok();
	let fault = fault(state.cursor(), 3, Some(3)).assert_ok();
	state = ready(
		state
			.prepare(&fault, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.assert_ok(),
	)
	.assert_ok();
	state = state.with_overflow(fault.token(), LiveOverflowPolicy::Gap).assert_ok();
	let LiveNext::Delivery(delivery) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing reserved fault"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Fault {
		gap,
		diagnostic,
	} = delivery.payload()
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("session overflow replaced fault"));
			test_result.assert_ok();
		};
	};
	assert_eq!(gap.from().map(|stamp| stamp.get()), Some(2));
	assert_eq!(gap.through().map(|stamp| stamp.get()), Some(3));
	assert_eq!(diagnostic.status().code().to_string(), "FG-G1F09");
	assert_eq!(state.footprint().assert_ok().entries, 0);
}

/// Verifies live queue prepaid control slot is not consumed by data.
#[test]
fn live_queue_prepaid_control_slot_is_not_consumed_by_data() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	for stamp in 2..=3 {
		let data = batch(state.cursor(), stamp, 1).assert_ok();
		state = ready(
			state
				.prepare(&data, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
				.assert_ok(),
		)
		.assert_ok();
	}
	let before = state.footprint().assert_ok().bytes;
	let control = fault(state.cursor(), 4, Some(4)).assert_ok();
	state = ready(
		state
			.prepare(&control, &LiveOptions::default(), Footprint::default(), SESSION_BYTES)
			.assert_ok(),
	)
	.assert_ok();
	assert_eq!(
		state.footprint().assert_ok().bytes,
		before,
		"first fault consumes prepaid capacity"
	);
}

/// Verifies live queue gap preserves issued control and covers lost tail.
#[test]
fn live_queue_gap_preserves_issued_control_and_covers_lost_tail() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	let options = LiveOptions::default().with_queue(1, 16 * 1024).assert_ok();
	let first = batch(state.cursor(), 2, 1).assert_ok();
	state = ready(state.prepare(&first, &options, Footprint::default(), SESSION_BYTES).assert_ok())
		.assert_ok();
	let second = batch(state.cursor(), 3, 1).assert_ok();
	state =
		ready(state.prepare(&second, &options, Footprint::default(), SESSION_BYTES).assert_ok())
			.assert_ok();
	let LiveNext::Delivery(gap_one) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing GAP"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Gap(gap) = gap_one.payload() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("expected GAP"));
			test_result.assert_ok();
		};
	};
	assert_eq!(gap.from().map(|stamp| stamp.get()), Some(2));
	assert_eq!(gap.through().map(|stamp| stamp.get()), Some(3));
	let third = batch(state.cursor(), 4, 1).assert_ok();
	state = ready(state.prepare(&third, &options, Footprint::default(), SESSION_BYTES).assert_ok())
		.assert_ok();
	let fourth = batch(state.cursor(), 5, 1).assert_ok();
	state =
		ready(state.prepare(&fourth, &options, Footprint::default(), SESSION_BYTES).assert_ok())
			.assert_ok();
	let LiveNext::Delivery(repeated) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("lost issued control"));
			test_result.assert_ok();
		};
	};
	assert_eq!(repeated.token(), gap_one.token());
	assert!(Arc::ptr_eq(&repeated.payload, &gap_one.payload));
	state.confirm(gap_one.token()).assert_ok();
	let LiveNext::Delivery(gap_two) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing second GAP"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Gap(gap) = gap_two.payload() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("expected second GAP"));
			test_result.assert_ok();
		};
	};
	assert_eq!(gap.from().map(|stamp| stamp.get()), Some(4));
	assert_eq!(gap.through().map(|stamp| stamp.get()), Some(5));
	state.confirm(gap_two.token()).assert_ok();
	let tail = batch(state.cursor(), 6, 1).assert_ok();
	state = ready(state.prepare(&tail, &options, Footprint::default(), SESSION_BYTES).assert_ok())
		.assert_ok();
	let LiveNext::Delivery(delivery) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing tail"));
			test_result.assert_ok();
		};
	};
	assert_eq!(delivery.token().versionstamp().get(), 6);
}

/// Verifies live queue block disconnect and budget failures leave original
/// intact.
#[test]
fn live_queue_block_disconnect_and_budget_failures_leave_original_intact() {
	use crate::test_assertions::AssertResult as _;

	let mut state = queue().assert_ok();
	let options = LiveOptions::default()
		.with_queue(1, 16 * 1024)
		.assert_ok()
		.with_overflow(LiveOverflowPolicy::Block);
	let first = batch(state.cursor(), 2, 1).assert_ok();
	state = ready(state.prepare(&first, &options, Footprint::default(), SESSION_BYTES).assert_ok())
		.assert_ok();
	let second = batch(state.cursor(), 3, 1).assert_ok();
	let before = state.footprint().assert_ok();
	assert!(matches!(
		state.prepare(&second, &options, Footprint::default(), SESSION_BYTES).assert_ok(),
		PreparedQueue::Blocked
	));
	assert_eq!(state.footprint().assert_ok(), before);
	assert!(state.prepare(&second, &options, Footprint::default(), 1).is_err());
	assert_eq!(state.footprint().assert_ok(), before);
	let oversized = batch(state.cursor(), 4, 2).assert_ok();
	assert!(matches!(
		state.prepare(&oversized, &options, Footprint::default(), SESSION_BYTES).assert_ok(),
		PreparedQueue::Oversized
	));
	assert_eq!(state.footprint().assert_ok(), before);
	let disconnect = options.with_overflow(LiveOverflowPolicy::Disconnect);
	state =
		ready(state.prepare(&second, &disconnect, Footprint::default(), SESSION_BYTES).assert_ok())
			.assert_ok();
	let LiveNext::Delivery(gap) = state.next() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing disconnect GAP"));
			test_result.assert_ok();
		};
	};
	state.confirm(gap.token()).assert_ok();
	assert!(matches!(state.next(), LiveNext::Closed));
}
