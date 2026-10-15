//! Authoritative checkpoints and commit deltas over shared WAL-backed pages.

use std::sync::Arc;

use crate::{
	datastore::{
		ClassId,
		EventId,
		EventInvocation,
		EventInvocationState,
		EventOutbox,
		EventTarget,
		JobId,
		SchemaVersion,
	},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::history_codec::{CodecBudget, Decoder, append, append_blob, invalid, values},
};

/// Encodes complete checkpoints or only changed identities; payloads stay
/// immutable.
pub(in crate::storage) fn encode(
	previous: Option<&EventOutbox>,
	current: &EventOutbox,
	stamp: u64,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	let changed = |id: &JobId, invocation: &Arc<EventInvocation>| {
		previous.is_none_or(|before| before.invocations.get(id).is_none_or(|old| old != invocation))
	};
	let updates =
		current.invocations.iter().filter(|(id, invocation)| changed(id, invocation)).count();
	let removed = previous.map_or(0, |before| {
		before.invocations.keys().filter(|id| !current.invocations.contains_key(id)).count()
	});
	let mut output = Vec::new();
	append(&mut output, b"FGEO", budget)?;
	append(&mut output, &[1, 0, u8::from(previous.is_none()), 0], budget)?;
	append(&mut output, &stamp.to_le_bytes(), budget)?;
	values::write_count(
		&mut output,
		updates.checked_add(removed).ok_or_else(|| invalid("EVENT journal count overflows"))?,
		budget,
	)?;
	values::write_count(&mut output, current.pending_count, budget)?;
	append(&mut output, &(current.pending_bytes as u64).to_le_bytes(), budget)?;
	if let Some(before) = previous {
		for id in before.invocations.keys().filter(|id| !current.invocations.contains_key(id)) {
			append(&mut output, &[0], budget)?;
			append(&mut output, id.as_uuid().as_inner().as_bytes(), budget)?;
		}
	}
	for (id, invocation) in
		current.invocations.iter().filter(|(id, invocation)| changed(id, invocation))
	{
		append(&mut output, &[1], budget)?;
		append(&mut output, id.as_uuid().as_inner().as_bytes(), budget)?;
		for value in [invocation.event.get(), invocation.version.get()] {
			append(&mut output, &value.to_le_bytes(), budget)?;
		}
		let (kind, owner) = match invocation.target {
			EventTarget::Class(class) => (0, u64::from(class.get())),
			EventTarget::Graph(graph) => (1, graph.get()),
		};
		append(
			&mut output,
			&[
				kind,
				state_tag(invocation.state),
				invocation.retries,
				invocation.depth,
				invocation.ceiling,
				0,
			],
			budget,
		)?;
		append(&mut output, &invocation.attempts.to_le_bytes(), budget)?;
		append(&mut output, &owner.to_le_bytes(), budget)?;
		for value in [invocation.accepted_at, invocation.updated_at] {
			append(&mut output, &value.to_le_bytes(), budget)?;
		}
		append(&mut output, &invocation.due_at_ns.to_le_bytes(), budget)?;
		append(&mut output, &invocation.created_at_ns.to_le_bytes(), budget)?;
		append(&mut output, &invocation.updated_at_ns.to_le_bytes(), budget)?;
		append(&mut output, &[u8::from(invocation.diagnostic.is_some())], budget)?;
		if let Some(message) = &invocation.diagnostic {
			values::append_text(&mut output, message, budget)?;
		}
		match &invocation.payload {
			None => append(&mut output, &[0], budget)?,
			Some(payload)
				if previous
					.and_then(|before| before.invocations.get(id))
					.and_then(|old| old.payload.as_ref())
					.is_some_and(|old| old == payload) =>
			{
				append(&mut output, &[1], budget)?;
			}
			Some(payload) => {
				append(&mut output, &[2], budget)?;
				append_blob(&mut output, payload, budget)?;
			}
		}
	}
	Ok(output)
}

/// Applies one complete checked publication; caller discards the candidate on
/// error.
pub(in crate::storage) fn decode(
	bytes: &[u8],
	root: &mut EventOutbox,
	expected_stamp: u64,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let mut decoder = Decoder::new(bytes);
	if decoder.take(4)? != b"FGEO" || decoder.take(2)? != 1_u16.to_le_bytes() {
		return Err(invalid("EVENT journal marker/version is invalid"));
	}
	let checkpoint = match decoder.u8()? {
		0 => false,
		1 => true,
		_ => return Err(invalid("EVENT journal checkpoint flag is invalid")),
	};
	decoder.zero(1)?;
	if decoder.u64()? != expected_stamp {
		return Err(invalid("EVENT journal publication stamp differs"));
	}
	let count = values::count(&mut decoder, 17)?;
	if count
		> if checkpoint {
			20_000
		} else {
			40_000
		} {
		return Err(invalid("EVENT journal identity count exceeds its ceiling"));
	}
	let expected_count = decoder.u32()? as usize;
	let expected_bytes = usize::try_from(decoder.u64()?).map_err(|error| {
		invalid("EVENT journal payload count overflows").with_technical_source(error)
	})?;
	if checkpoint {
		*root = EventOutbox::default();
	}
	budget.admit(crate::values::storage::tree_allocation_bytes(
		root.invocations.len().saturating_add(count),
		std::mem::size_of::<(JobId, Arc<EventInvocation>)>(),
	)?)?;
	let mut identities = std::collections::BTreeSet::new();
	for _ in 0..count {
		let operation = decoder.u8()?;
		let uuid = uuid::Uuid::from_slice(decoder.take(16)?).map_err(|error| {
			invalid("EVENT invocation UUID is malformed").with_technical_source(error)
		})?;
		if uuid.is_nil() {
			return Err(invalid("EVENT invocation UUID is nil"));
		}
		let id = JobId::from_uuid(uuid.into());
		if !identities.insert(id) {
			return Err(invalid("EVENT journal repeats an identity"));
		}
		if operation == 0 {
			if checkpoint
				|| !root.invocations.remove(&id).is_some_and(|old| !old.state.is_pending())
			{
				return Err(invalid("EVENT journal removed a pending or absent invocation"));
			}
			continue;
		}
		if operation != 1 {
			return Err(invalid("EVENT journal operation tag is unknown"));
		}
		let event = EventId::from_storage(decoder.u32()?)?;
		let version = SchemaVersion::from_storage(decoder.u32()?)?;
		let kind = decoder.u8()?;
		let state = decode_state(decoder.u8()?)?;
		let retries = decoder.u8()?;
		let depth = decoder.u8()?;
		let ceiling = decoder.u8()?;
		decoder.zero(1)?;
		let attempts = u16::from_le_bytes(decoder.take(2)?.try_into().map_err(|error| {
			invalid("EVENT attempts are truncated").with_technical_source(error)
		})?);
		let owner = decoder.u64()?;
		let target = match kind {
			0 => EventTarget::Class(ClassId::from_storage(u32::try_from(owner).map_err(
				|error| invalid("EVENT class identity overflows").with_technical_source(error),
			)?)?),
			1 => EventTarget::Graph(
				GraphId::new(owner).ok_or_else(|| invalid("EVENT graph identity is zero"))?,
			),
			_ => return Err(invalid("EVENT target kind is unknown")),
		};
		let accepted_at = decoder.u64()?;
		let updated_at = decoder.u64()?;
		let due_at_ns = i128::from_le_bytes(decoder.take(16)?.try_into().map_err(|error| {
			invalid("EVENT due time is truncated").with_technical_source(error)
		})?);
		let created_at_ns = i128::from_le_bytes(decoder.take(16)?.try_into().map_err(|error| {
			invalid("EVENT creation time is truncated").with_technical_source(error)
		})?);
		let updated_at_ns = i128::from_le_bytes(decoder.take(16)?.try_into().map_err(|error| {
			invalid("EVENT update time is truncated").with_technical_source(error)
		})?);
		for time in [due_at_ns, created_at_ns, updated_at_ns] {
			jiff::Timestamp::from_nanosecond(time).map_err(|error| {
				invalid("EVENT timestamp is outside UTC range").with_technical_source(error)
			})?;
		}
		let diagnostic = match decoder.u8()? {
			0 => None,
			1 => {
				let text = values::text(&mut decoder, budget)?;
				if text.len() > 4096 || text.as_str().contains('\0') {
					return Err(invalid("EVENT diagnostic exceeds its ceiling"));
				}
				Some(text)
			}
			_ => return Err(invalid("EVENT diagnostic flag is unknown")),
		};
		let payload = match decoder.u8()? {
			0 => None,
			1 if !checkpoint => root
				.invocations
				.get(&id)
				.and_then(|old| old.payload.as_ref())
				.cloned()
				.map(Some)
				.ok_or_else(|| invalid("EVENT reused payload is absent"))?,
			2 => {
				let bytes = decoder.blob()?;
				if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
					return Err(invalid("EVENT payload exceeds its ceiling"));
				}
				budget.admit(bytes.len() + 32)?;
				Some(Arc::from(bytes))
			}
			_ => return Err(invalid("EVENT payload flag is invalid")),
		};
		if depth > ceiling
			|| ceiling > 16
			|| attempts > u16::from(retries) + 1
			|| state.is_pending() != payload.is_some()
			|| accepted_at == 0
			|| accepted_at > updated_at
			|| updated_at > expected_stamp
			|| created_at_ns > updated_at_ns
			|| !checkpoint && updated_at != expected_stamp
			|| state == EventInvocationState::Queued && attempts > u16::from(retries)
			|| matches!(
				state,
				EventInvocationState::Running
					| EventInvocationState::Completed
					| EventInvocationState::Failed
			) && attempts == 0
		{
			return Err(invalid("EVENT invocation bounds or lifecycle are inconsistent"));
		}
		if let Some(old) = root.invocations.get(&id) {
			if old.event != event
				|| old.version != version
				|| old.target != target
				|| old.retries != retries
				|| old.depth != depth
				|| old.ceiling != ceiling
				|| old.accepted_at != accepted_at
				|| old.created_at_ns != created_at_ns
				|| old.updated_at_ns > updated_at_ns
				|| attempts < old.attempts
				|| !old.state.is_pending()
			{
				return Err(invalid("EVENT immutable identity or terminal marker changed"));
			}
			if let (Some(old), Some(new)) = (&old.payload, &payload)
				&& old != new
			{
				return Err(invalid("EVENT retry changed accepted payload"));
			}
			let valid_transition = match (old.state, state) {
				(EventInvocationState::Queued, EventInvocationState::Running) => {
					attempts == old.attempts + 1
				}
				(
					EventInvocationState::Running,
					EventInvocationState::Running
					| EventInvocationState::Completed
					| EventInvocationState::Failed,
				) => attempts == old.attempts,
				(EventInvocationState::Running, EventInvocationState::Queued) => {
					attempts == old.attempts && attempts <= u16::from(retries)
				}
				(_, EventInvocationState::Cancelled) => attempts == old.attempts,
				_ => false,
			};
			if !valid_transition {
				return Err(invalid("EVENT journal lifecycle transition is invalid"));
			}
		} else if !checkpoint
			&& (state != EventInvocationState::Queued
				|| attempts != 0
				|| accepted_at != expected_stamp)
		{
			return Err(invalid("EVENT journal introduced an unqueued invocation"));
		}
		budget.admit(std::mem::size_of::<EventInvocation>() + 32)?;
		root.invocations.insert(
			id,
			Arc::new(EventInvocation {
				id,
				event,
				version,
				target,
				state,
				attempts,
				retries,
				depth,
				ceiling,
				due_at_ns,
				accepted_at,
				updated_at,
				created_at_ns,
				updated_at_ns,
				diagnostic,
				payload,
			}),
		);
	}
	decoder.finish()?;
	root.pending_count =
		root.invocations.values().filter(|invocation| invocation.state.is_pending()).count();
	root.pending_bytes = root
		.invocations
		.values()
		.filter_map(|invocation| invocation.payload.as_ref())
		.try_fold(0usize, |total, payload| {
			total
				.checked_add(payload.len())
				.ok_or_else(|| invalid("EVENT pending byte count overflows"))
		})?;
	if root.pending_count != expected_count
		|| root.pending_bytes != expected_bytes
		|| expected_count > 10_000
		|| expected_bytes > 64 * 1024 * 1024
	{
		return Err(invalid("EVENT pending counts disagree with journal"));
	}
	root.validate_retained_bounds()?;
	Ok(())
}

/// Closed lifecycle tags are explicit development-format bytes.
const fn state_tag(state: EventInvocationState) -> u8 {
	match state {
		EventInvocationState::Queued => 0,
		EventInvocationState::Running => 1,
		EventInvocationState::Completed => 2,
		EventInvocationState::Cancelled => 3,
		EventInvocationState::Failed => 4,
	}
}

/// Unknown lifecycle tags never silently discard pending work.
fn decode_state(tag: u8) -> FerrosGrynnResult<EventInvocationState> {
	match tag {
		0 => Ok(EventInvocationState::Queued),
		1 => Ok(EventInvocationState::Running),
		2 => Ok(EventInvocationState::Completed),
		3 => Ok(EventInvocationState::Cancelled),
		4 => Ok(EventInvocationState::Failed),
		_ => Err(invalid("EVENT lifecycle tag is unknown")),
	}
}

#[cfg(test)]
/// Regression tests for journal behavior.
mod tests {
	use super::*;
	use crate::datastore::{EventAsyncOptions, EventCause, EventDefinition, EventExecution};

	/// Verifies event journal rejects unreachable queue and terminal overflow.
	#[test]
	fn event_journal_rejects_unreachable_queue_and_terminal_overflow() {
		use crate::test_assertions::AssertResult as _;

		let mut definition = EventDefinition::new(
			"a",
			EventTarget::Class(ClassId::from_storage(1).assert_ok()),
			"true",
			"1",
		)
		.assert_ok()
		.with_execution(EventExecution::Async(EventAsyncOptions::default()));
		definition.id = Some(EventId::from_storage(2).assert_ok());
		let invocation = EventInvocation::queued(
			&definition,
			EventCause::next(None, definition.execution()).assert_ok(),
			Arc::from([1_u8]),
			5,
		)
		.assert_ok();
		let mut root = EventOutbox::default();
		root.enqueue(Arc::new(invocation), 1).assert_ok();
		let id = *root
			.invocations
			.keys()
			.next()
			.ok_or_else(|| invalid("fixture identity absent"))
			.assert_ok();
		let mut impossible = root.clone();
		Arc::make_mut(
			impossible
				.invocations
				.get_mut(&id)
				.ok_or_else(|| invalid("fixture invocation absent"))
				.assert_ok(),
		)
		.attempts = 1;
		let bytes =
			encode(None, &impossible, 1, &mut CodecBudget::new(1 << 20, 0).assert_ok()).assert_ok();
		assert!(
			decode(
				&bytes,
				&mut EventOutbox::default(),
				1,
				&mut CodecBudget::new(1 << 20, bytes.len()).assert_ok()
			)
			.is_err()
		);
		root.transition(id, 0, EventInvocationState::Running, 5, None, 2).assert_ok();
		root.transition(id, 1, EventInvocationState::Completed, 5, None, 3).assert_ok();
		let terminal = root.invocations[&id].as_ref().clone();
		for index in 1..=10_001_u128 {
			let mut terminal = terminal.clone();
			terminal.id = JobId::from_uuid(uuid::Uuid::from_u128(index).into());
			root.invocations.insert(terminal.id, Arc::new(terminal));
		}
		let bytes =
			encode(None, &root, 3, &mut CodecBudget::new(64 << 20, 0).assert_ok()).assert_ok();
		assert!(
			decode(
				&bytes,
				&mut EventOutbox::default(),
				3,
				&mut CodecBudget::new(64 << 20, bytes.len()).assert_ok()
			)
			.is_err()
		);
		root.invocations.clear();
		for index in 1..=2000_u128 {
			let mut terminal = terminal.clone();
			terminal.id = JobId::from_uuid(uuid::Uuid::from_u128(index).into());
			terminal.diagnostic = Some("a".repeat(4096).into());
			root.invocations.insert(terminal.id, Arc::new(terminal));
		}
		let bytes =
			encode(None, &root, 3, &mut CodecBudget::new(128 << 20, 0).assert_ok()).assert_ok();
		assert!(
			decode(
				&bytes,
				&mut EventOutbox::default(),
				3,
				&mut CodecBudget::new(128 << 20, bytes.len()).assert_ok()
			)
			.is_err()
		);
	}

	/// Verifies event journal checkpoint attempt and completion are atomic.
	#[test]
	fn event_journal_checkpoint_attempt_and_completion_are_atomic() {
		use crate::test_assertions::AssertResult as _;

		let mut definition = EventDefinition::new(
			"a",
			EventTarget::Class(ClassId::from_storage(1).assert_ok()),
			"true",
			"1",
		)
		.assert_ok()
		.with_execution(EventExecution::Async(EventAsyncOptions::default()));
		definition.id = Some(EventId::from_storage(2).assert_ok());
		let mut invocation = EventInvocation::queued(
			&definition,
			EventCause::next(None, definition.execution()).assert_ok(),
			Arc::from([1_u8, 2, 3]),
			5,
		)
		.assert_ok();
		invocation.id = JobId::from_uuid(uuid::Uuid::from_u128(1).into());
		let id = invocation.id;
		let mut queued = EventOutbox::default();
		queued.enqueue(Arc::new(invocation), 1).assert_ok();
		let mut budget = CodecBudget::new(1 << 20, 0).assert_ok();
		let checkpoint = encode(None, &queued, 1, &mut budget).assert_ok();
		assert_eq!(&checkpoint[..8], b"FGEO\x01\x00\x01\x00");
		let mut recovered = EventOutbox::default();
		decode(
			&checkpoint,
			&mut recovered,
			1,
			&mut CodecBudget::new(1 << 20, checkpoint.len()).assert_ok(),
		)
		.assert_ok();
		assert_eq!(recovered, queued);
		let mut running = queued.clone();
		running.transition(id, 0, EventInvocationState::Running, 5, None, 2).assert_ok();
		let delta =
			encode(Some(&queued), &running, 2, &mut CodecBudget::new(1 << 20, 0).assert_ok())
				.assert_ok();
		assert!(delta.len() < checkpoint.len());
		decode(&delta, &mut recovered, 2, &mut CodecBudget::new(1 << 20, delta.len()).assert_ok())
			.assert_ok();
		assert_eq!(recovered, running);
		let mut completed = running.clone();
		completed.transition(id, 1, EventInvocationState::Completed, 0, None, 3).assert_ok();
		let marker =
			encode(Some(&running), &completed, 3, &mut CodecBudget::new(1 << 20, 0).assert_ok())
				.assert_ok();
		decode(
			&marker,
			&mut recovered,
			3,
			&mut CodecBudget::new(1 << 20, marker.len()).assert_ok(),
		)
		.assert_ok();
		assert_eq!(recovered, completed);
		assert_eq!(recovered.pending_count, 0);
		assert!(
			recovered.invocations.get(&id).is_some_and(|invocation| invocation.payload.is_none()
				&& invocation.state == EventInvocationState::Completed)
		);
		assert!(
			decode(
				&marker,
				&mut recovered.clone(),
				3,
				&mut CodecBudget::new(1 << 20, marker.len()).assert_ok()
			)
			.is_err()
		);
		for length in 0..checkpoint.len() {
			assert!(
				decode(
					&checkpoint[..length],
					&mut EventOutbox::default(),
					1,
					&mut CodecBudget::new(1 << 20, length).assert_ok()
				)
				.is_err()
			);
		}
	}
}
