//! Exact net result preparation; delivery never re-evaluates primary records.

use std::{cell::RefCell, collections::BTreeMap, path::PathBuf, sync::Arc};

use crate::{
	datastore::{BindingAdmission, ConflictKey, DatabaseRoot, LiveDefinition},
	gql::live_lowering::{QueryEvaluation, evaluate_query_controlled},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveBatch,
		LiveBindingOptions,
		LiveBindingRevision,
		LiveEvent,
		LiveGap,
		LiveGapReason,
		LiveIdentity,
		LivePayload,
		LiveState,
	},
	values::{DateTime, Value},
};

/// Never relabel an explicitly suppressed import as durable computed backfill.
/// The transient current result remains independent of historical coverage.
pub(super) fn suppress_import_history(next: &mut DatabaseRoot) -> FerrosGrynnResult<()> {
	let mut budget = BindingAdmission::new(&LiveBindingOptions::default());
	let count = next.live_catalog().prepared.len();
	budget.admit(
		count
			.checked_mul(64)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE import controls"))?,
		count as u64,
	)?;
	let mut encoded = Vec::new();
	encoded.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded("LIVE import controls", count as u64)
			.with_technical_source(error)
	})?;
	for (id, payload) in next.live_catalog().prepared.iter() {
		let captured = match payload.as_ref() {
			LivePayload::Batch(batch) => Some((batch.version(), batch.versionstamp())),
			LivePayload::Graph(batch) => Some((batch.version(), batch.versionstamp())),
			LivePayload::Gap(_)
			| LivePayload::Fault {
				..
			} => None,
		};
		if let Some((version, stamp)) = captured {
			let gap = LivePayload::Gap(LiveGap {
				id: *id,
				reason: LiveGapReason::ImportSuppressed,
				from: Some(stamp),
				through: Some(stamp),
			});
			let bytes = crate::storage::live_history::encode(
				&gap,
				*id,
				version,
				next.commit_ts,
				next.committed_at_ns,
				budget.remaining_options()?.scratch_bytes(),
			)?;
			budget.admit(bytes.len(), 1)?;
			encoded.push((*id, Arc::from(bytes)));
		} else {
			let original = &next.live_catalog().prepared_encoded;
			if let Ok(at) = original.binary_search_by_key(id, |(owner, _)| *owner) {
				encoded.push((*id, Arc::clone(&original[at].1)));
			}
		}
	}
	Arc::make_mut(next.live_catalog_mut()).prepared_encoded = encoded.into();
	Ok(())
}

/// Prepares immutable batches against the complete final parameter/data image.
pub(super) fn evaluate_queries(
	before: &Arc<DatabaseRoot>,
	next: &mut DatabaseRoot,
	scratch_root: PathBuf,
	targets: super::LiveQueryTargets,
	observations: &crate::datastore::live_observation::Counters,
) -> FerrosGrynnResult<()> {
	if next.live_catalog().queries.is_empty() {
		return Ok(());
	}
	let mut budget = BindingAdmission::new(&LiveBindingOptions::default());
	let count = next.live_catalog().queries.len();
	budget.admit(
		count
			.checked_mul(512)
			.and_then(|bytes| {
				bytes.checked_add(next.live_catalog().bindings.len().checked_mul(128)?)
			})
			.and_then(|bytes| {
				bytes.checked_add(next.live_catalog().histories.len().checked_mul(128)?)
			})
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE candidate queries"))?,
		count as u64,
	)?;
	for entry in next.live_catalog().queries.values() {
		budget.admit(
			entry
				.functions
				.len()
				.checked_mul(8)
				.and_then(|bytes| bytes.checked_add(entry.record_sources.len().checked_mul(64)?))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE candidate captures"))?,
			0,
		)?;
		for (name, _) in &entry.record_sources {
			budget.admit(name.len(), 1)?;
		}
		if let Some(name) = &entry.capture_scope_name {
			budget.admit(name.len(), 1)?;
		}
		budget.admit(
			crate::datastore::LiveRhsScopeAnchor::retained_bytes(&entry.rhs_scope_anchors)?,
			entry.rhs_scope_anchors.len() as u64,
		)?;
	}
	let mut root = Arc::new(next.clone());
	let mut prepared = Vec::new();
	let mut encoded = Vec::new();
	let mut graph_encoded = Vec::new();
	graph_encoded.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE graph checkpoint envelopes",
			count as u64,
		)
		.with_technical_source(error)
	})?;
	encoded.try_reserve_exact(count).map_err(|_error| {
		FerrosGrynnError::traversal_resource_limit_exceeded("LIVE encoded envelopes", count as u64)
	})?;
	prepared.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded("LIVE prepared batches", count as u64)
			.with_technical_source(error)
	})?;
	let ids = root.live_catalog().queries.keys().copied().collect::<Vec<_>>();
	for id in ids {
		let definition = root.live_catalog().queries.get(&id).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("LIVE candidate lost its query")
		})?;
		if matches!(definition.state, LiveState::Closed | LiveState::Paused) {
			continue;
		}
		let pending = definition.defined_at == root.commit_ts;
		if matches!(targets, super::LiveQueryTargets::Pending) && !pending {
			continue;
		}
		if !pending
			&& matches!(definition.options.since(), crate::live::LiveSince::Timestamp(time)
			if time.as_inner().timestamp().as_nanosecond() > root.committed_at_ns)
		{
			continue;
		}
		let available = budget.remaining_options()?.scratch_bytes();
		observations.query_attempt();
		let result = evaluate_one(
			before,
			&root,
			&definition,
			pending,
			scratch_root.clone(),
			available,
			observations,
		);
		match result {
			Ok(PreparedEvaluation {
				payload,
				envelope,
				graph_checkpoint,
				capture_source: source,
				capture_scope_name,
				rhs_scope_anchors,
			}) => {
				if definition.capture_source != source
					|| definition.capture_scope_name != capture_scope_name
					|| definition.rhs_scope_anchors != rhs_scope_anchors
				{
					let stamp = root.commit_ts;
					let candidate = Arc::make_mut(&mut root);
					let entry = Arc::make_mut(candidate.live_catalog_mut())
						.queries
						.get_mut(&id)
						.ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"LIVE source update lost its query",
							)
						})?;
					let entry = Arc::make_mut(entry);
					entry.capture_source = source;
					entry.capture_scope_name = capture_scope_name;
					entry.rhs_scope_anchors = rhs_scope_anchors;
					entry.published_at = stamp;
					candidate.mark_version(ConflictKey::LiveDefinition(id), stamp);
				}
				let envelope = bytes_for_batch(envelope, &mut budget)?;
				if let Some(checkpoint) = graph_checkpoint {
					let bytes = envelope.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("graph checkpoint has no envelope")
					})?;
					budget.admit(512, 1)?;
					Arc::make_mut(Arc::make_mut(&mut root).live_catalog_mut())
						.replace_graph_checkpoint(id, checkpoint)?;
					graph_encoded.push((id, Arc::clone(bytes)));
				}
				if let Some(payload) = payload {
					let cost = crate::datastore::live_queue::Footprint::payload(&payload)?;
					budget.admit(cost.bytes, cost.entries as u64)?;
					if let Some(bytes) = envelope {
						encoded.push((id, bytes));
					}
					prepared.push((id, Arc::new(payload)));
				}
			}
			Err(error) if pending => return Err(error),
			Err(error) => {
				let stamp = root.commit_ts;
				let (diagnostic, payload, bytes) =
					prepare_fault(id, definition.version, stamp, root.committed_at_ns, error)?;
				let candidate = Arc::make_mut(&mut root);
				let entry =
					Arc::make_mut(candidate.live_catalog_mut()).queries.get_mut(&id).ok_or_else(
						|| FerrosGrynnError::invalid_storage_file("LIVE fault lost its query"),
					)?;
				let entry = Arc::make_mut(entry);
				entry.state = LiveState::Paused;
				entry.published_at = stamp;
				entry.diagnostic = Some(diagnostic.clone());
				candidate.mark_version(ConflictKey::LiveDefinition(id), stamp);
				budget.admit(4096, 1)?;
				if let Some(bytes) = bytes_for_batch(Some(bytes), &mut budget)? {
					encoded.push((id, bytes));
				}
				prepared.push((id, Arc::new(payload)));
			}
		}
	}
	Arc::make_mut(Arc::make_mut(&mut root).live_catalog_mut()).prepared = prepared.into();
	Arc::make_mut(Arc::make_mut(&mut root).live_catalog_mut()).prepared_encoded = encoded.into();
	Arc::make_mut(Arc::make_mut(&mut root).live_catalog_mut()).prepared_graphs =
		graph_encoded.into();
	*next = Arc::unwrap_or_clone(root);
	Ok(())
}

/// A diagnostic-capture failure is itself a bounded evaluation fault, never a
/// veto on source DML.
fn prepare_fault(
	id: crate::live::LiveId,
	version: crate::live::LiveVersion,
	stamp: u64,
	time: i128,
	error: FerrosGrynnError,
) -> FerrosGrynnResult<(FerrosGrynnError, LivePayload, Vec<u8>)> {
	let origin = error.status().code().to_string();
	let mut diagnostic = FerrosGrynnError::live_evaluation_fault(
		id.to_string(),
		origin.clone(),
		error.status().definition().subcondition.unwrap_or("evaluation failed"),
	)
	.with_cause(error);
	let gap = LiveGap {
		id,
		reason: LiveGapReason::EvaluationFault,
		from: Some(stamp.into()),
		through: Some(stamp.into()),
	};
	let mut payload = LivePayload::Fault {
		diagnostic: diagnostic.clone(),
		gap: gap.clone(),
	};
	let encode = |payload: &LivePayload| {
		crate::storage::live_history::encode(payload, id, version, stamp, time, 64 * 1024)
	};
	let bytes = match encode(&payload) {
		Ok(bytes) => bytes,
		Err(capture_error) => {
			diagnostic = FerrosGrynnError::live_evaluation_fault(
				id.to_string(),
				origin,
				"original diagnostic unavailable: LIVE diagnostic capture failed",
			)
			.with_cause(capture_error);
			payload = LivePayload::Fault {
				diagnostic,
				gap,
			};
			encode(&payload)?
		}
	};
	// Technical sources may own arbitrary runtime data. Only the bounded
	// declared diagnostic is retained by roots and delivery queues.
	let payload = match crate::storage::live_history::decode(&bytes, 64 * 1024) {
		Ok(payload) => payload,
		Err(_decode_error) => {
			let diagnostic = FerrosGrynnError::live_evaluation_fault(
				id.to_string(),
				"FG-G1F09",
				"original diagnostic unavailable: LIVE diagnostic restoration exceeded its cap",
			);
			let payload = LivePayload::Fault {
				diagnostic,
				gap: LiveGap {
					id,
					reason: LiveGapReason::EvaluationFault,
					from: Some(stamp.into()),
					through: Some(stamp.into()),
				},
			};
			let bytes = encode(&payload)?;
			let payload = crate::storage::live_history::decode(&bytes, 64 * 1024)?;
			let LivePayload::Fault {
				diagnostic,
				..
			} = &payload
			else {
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE diagnostic lost its fault envelope",
				));
			};
			return Ok((diagnostic.clone(), payload, bytes));
		}
	};
	let LivePayload::Fault {
		diagnostic,
		..
	} = &payload
	else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"LIVE diagnostic lost its fault envelope",
		));
	};
	Ok((diagnostic.clone(), payload, bytes))
}

/// Registration produces no initial batch; old and new bindings stay
/// independent.
struct PreparedEvaluation {
	/// Whole-commit result, possibly empty.
	payload: Option<LivePayload>,
	/// Exact durable bytes when the batch has events.
	envelope: Option<Vec<u8>>,
	/// Activation also persists its accepted image without issuing a delivery.
	graph_checkpoint: Option<Arc<crate::live::LiveGraphBatch>>,
	/// Successful root source owner.
	capture_source: crate::changefeed::ChangefeedSource,
	/// Successful string scope operand paired with that owner.
	capture_scope_name: Option<crate::values::Text>,
	/// Successful root JOIN name/identity pairs; failed evaluation retains
	/// prior values.
	rhs_scope_anchors: Vec<crate::datastore::LiveRhsScopeAnchor>,
}

/// Registration produces no initial batch; old and new bindings stay
/// independent.
fn evaluate_one(
	before: &Arc<DatabaseRoot>,
	after: &Arc<DatabaseRoot>,
	definition: &LiveDefinition,
	pending: bool,
	scratch_root: PathBuf,
	available: usize,
	observations: &crate::datastore::live_observation::Counters,
) -> FerrosGrynnResult<PreparedEvaluation> {
	let options = LiveBindingOptions::default().with_budgets(
		definition.options.scratch_bytes().min(available),
		definition.options.work(),
	)?;
	let mut budget = BindingAdmission::new(&options);
	let result =
		evaluate_one_admitted(before, after, definition, pending, scratch_root, &mut budget);
	observations.admitted(&budget);
	result
}

/// Evaluates one complete candidate with caller-owned admission.
fn evaluate_one_admitted(
	before: &Arc<DatabaseRoot>,
	after: &Arc<DatabaseRoot>,
	definition: &LiveDefinition,
	pending: bool,
	scratch_root: PathBuf,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<PreparedEvaluation> {
	for source in definition.query.sources() {
		budget.admit(0, 1)?;
		super::live::validate_source(after, source)?;
	}
	if super::live::source_class(after, definition.query.source())? != definition.source_class {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE source owner changed before evaluation",
		));
	}
	super::live_evaluation::validate_captures(
		after,
		&definition.record_sources,
		&definition.functions,
		budget,
	)?;
	let anchor = match definition.capture_source {
		crate::changefeed::ChangefeedSource::Graph(graph) => {
			definition.capture_scope_name.as_ref().map(|name| (graph, name.as_str()))
		}
		crate::changefeed::ChangefeedSource::Class(_) => None,
	};
	let evaluate = |root: &Arc<DatabaseRoot>, budget: &mut BindingAdmission| {
		let limits = budget.remaining_options()?;
		let options =
			definition.options.clone().with_budgets(limits.scratch_bytes(), limits.work())?;
		let bindings = |id| root.live_catalog().binding_value(id);
		let evaluation = evaluate_query_controlled(
			root,
			&definition.query,
			&bindings,
			&options,
			scratch_root.clone(),
			&|_key| {},
			anchor,
			&definition.rhs_scope_anchors,
			None,
		)?;
		admit_evaluation(&evaluation, budget)?;
		if evaluation.functions != definition.functions
			|| evaluation.record_sources != definition.record_sources
		{
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE overload or record owner resolution changed",
			));
		}
		Ok(evaluation)
	};
	let historical_rows = pending
		&& !matches!(definition.options.since(), crate::live::LiveSince::Now)
		&& !matches!(definition.options.since(), crate::live::LiveSince::Timestamp(time)
			if time.as_inner().timestamp().as_nanosecond() > after.committed_at_ns)
		&& definition
			.source_class
			.and_then(|id| before.schema.class_by_id(id))
			.map(|class| -> FerrosGrynnResult<bool> {
				Ok(super::live_backfill::rows::eligible(&definition.query, &class, budget)?
					|| super::live_backfill::domain::eligible(&definition.query, &class, budget)?)
			})
			.transpose()?
			.unwrap_or(false);
	let old = if definition.query.graph_result().is_some() || pending && !historical_rows {
		None
	} else {
		Some(evaluate(before, budget)?)
	};
	let mut new = evaluate(after, budget)?;
	let capture_source = new.capture_source;
	if pending {
		validate_evaluation(&new, budget)?;
	}
	let dependencies = crate::datastore::query_dependencies(&definition.query, budget)?;
	budget.admit(
		dependencies.len().checked_mul(std::mem::size_of::<LiveBindingRevision>()).ok_or_else(
			|| FerrosGrynnError::operation_overflow("LIVE batch parameter revisions"),
		)?,
		dependencies.len() as u64,
	)?;
	let bindings = dependencies
		.into_iter()
		.map(|id| {
			let entry = after.live_catalog().bindings.get(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE batch lost its parameter")
			})?;
			Ok(LiveBindingRevision {
				id,
				definition: entry.version,
				value: entry.value_at.into(),
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let capture_scope_name = new.capture_scope_name;
	let rhs_scope_anchors = new.rhs_scope_anchors;
	if let Some(image) = new.graph_image.take() {
		let prior = before.live_catalog().graph_checkpoints.get(&definition.id);
		if !pending && prior.is_none() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE graph lost its accepted checkpoint",
			));
		}
		let base = prior.map_or(0.into(), |batch| batch.versionstamp());
		let previous = prior.map(|batch| Arc::clone(&batch.delta.after));
		let admission = RefCell::new(&mut *budget);
		let delta = crate::live::LiveGraphDelta::between(
			previous,
			Arc::new(image),
			base,
			after.commit_ts.into(),
			&|bytes, work| admission.borrow_mut().admit(bytes, work),
		)?;
		let changed = pending || !delta.changes().is_empty();
		if !changed {
			return Ok(PreparedEvaluation {
				payload: None,
				envelope: None,
				graph_checkpoint: None,
				capture_source,
				capture_scope_name,
				rhs_scope_anchors,
			});
		}
		let checkpoint = Arc::new(crate::live::LiveGraphBatch {
			id: definition.id,
			version: definition.version,
			committed_at: DateTime::from(
				jiff::Timestamp::from_nanosecond(after.committed_at_ns)?
					.to_zoned(jiff::tz::TimeZone::UTC),
			),
			delta,
			bindings,
		});
		after.live_catalog().graph_checkpoint_total(definition.id, &checkpoint)?;
		let payload = LivePayload::Graph(Arc::clone(&checkpoint));
		if !crate::datastore::live_queue::Footprint::fits_empty(&payload, &definition.options)? {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE graph whole commit exceeds an empty queue",
				definition.options.queue_bytes() as u64,
			));
		}
		let maximum = budget.remaining_options()?.scratch_bytes();
		let envelope = Some(crate::storage::live_history::encode(
			&payload,
			definition.id,
			definition.version,
			after.commit_ts,
			after.committed_at_ns,
			maximum,
		)?);
		return Ok(PreparedEvaluation {
			payload: (!pending).then_some(payload),
			envelope,
			graph_checkpoint: Some(checkpoint),
			capture_source,
			capture_scope_name,
			rhs_scope_anchors,
		});
	}
	let events = match old {
		None => Vec::new(),
		Some(old) => net_changes(old.rows, new.rows, budget)?,
	};
	let mut batch = LiveBatch {
		id: definition.id,
		version: definition.version,
		stamp: after.commit_ts.into(),
		committed_at: DateTime::from(
			jiff::Timestamp::from_nanosecond(after.committed_at_ns)?
				.to_zoned(jiff::tz::TimeZone::UTC),
		),
		events,
		bindings,
	};
	if definition.query.output() != crate::live::LiveOutputMode::Full && !batch.events.is_empty() {
		let limits = budget.remaining_options()?;
		let (bytes, work) = crate::live::prepare_batch_presentation(
			&mut batch,
			definition.query.output(),
			&limits,
		)?;
		budget.admit(bytes, work)?;
	}
	let has_events = !batch.events.is_empty();
	let payload = LivePayload::Batch(batch);
	if has_events
		&& !crate::datastore::live_queue::Footprint::fits_empty(&payload, &definition.options)?
	{
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"LIVE whole commit exceeds an empty queue",
			definition.options.queue_bytes() as u64,
		));
	}
	let bytes = if !has_events {
		None
	} else {
		let maximum = budget.remaining_options()?.scratch_bytes();
		Some(crate::storage::live_history::encode(
			&payload,
			definition.id,
			definition.version,
			after.commit_ts,
			after.committed_at_ns,
			maximum,
		)?)
	};
	Ok(PreparedEvaluation {
		payload: has_events.then_some(payload),
		envelope: bytes,
		graph_checkpoint: None,
		capture_source,
		capture_scope_name,
		rhs_scope_anchors,
	})
}

/// Admits the final shared bytes and their temporary Vec-to-Arc copy before
/// allocation.
fn bytes_for_batch(
	bytes: Option<Vec<u8>>,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Option<Arc<[u8]>>> {
	bytes
		.map(|bytes| {
			budget.admit(
				bytes.len().checked_mul(2).and_then(|bytes| bytes.checked_add(64)).ok_or_else(
					|| FerrosGrynnError::operation_overflow("LIVE encoded envelope bytes"),
				)?,
				0,
			)?;
			Ok(Arc::from(bytes))
		})
		.transpose()
}

/// Accounts every retained result and its source/group identity before copying.
pub(super) fn admit_evaluation(
	evaluation: &QueryEvaluation,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	let bytes = evaluation
		.rows
		.iter()
		.try_fold(512_usize, |bytes, (identity, value)| {
			bytes
				.checked_add(value.estimated_heap_bytes())?
				.checked_add(identity.estimated_heap_bytes())?
				.checked_add(192)
		})
		.and_then(|bytes| bytes.checked_add(evaluation.reads.len().checked_mul(64)?))
		.and_then(|bytes| bytes.checked_add(evaluation.functions.len().checked_mul(8)?))
		.and_then(|bytes| {
			bytes.checked_add(evaluation.capture_scope_name.as_ref().map_or(0, |name| name.len()))
		})
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE retained result scratch"))?;
	let bytes = bytes
		.checked_add(evaluation.graph.as_ref().map_or(0, |graph| graph.estimated_heap_bytes()))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE graph retained scratch"))?;
	let bytes = bytes
		.checked_add(
			evaluation
				.graph_image
				.as_ref()
				.map(crate::live::LiveGraphImage::estimated_heap_bytes)
				.transpose()?
				.unwrap_or(0),
		)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE graph image retained scratch"))?;
	budget.admit(bytes, evaluation.work_used).and_then(|()| {
		budget.admit(
			crate::datastore::LiveRhsScopeAnchor::retained_bytes(&evaluation.rhs_scope_anchors)?,
			evaluation.rhs_scope_anchors.len() as u64,
		)
	})
}

/// Rejects nonrestorable initial results without emitting an initial delivery.
pub(super) fn validate_evaluation(
	evaluation: &QueryEvaluation,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	let maximum = budget.remaining_options()?.scratch_bytes();
	if let Some(image) = &evaluation.graph_image {
		let bytes = crate::storage::live_graph_image::encode(image, maximum)?;
		return budget.admit(bytes.len(), 1);
	}
	let (bytes, work) =
		crate::storage::live_history::validate_rows(evaluation.rows.iter(), maximum)?;
	budget.admit(bytes, work)
}

/// Consumes both owned maps without cloning payloads or losing scalar None.
#[allow(
	clippy::mutable_key_type,
	reason = "LIVE keys are immutable qualified identities and checked aggregate values"
)]
pub(super) fn net_changes(
	mut before: BTreeMap<LiveIdentity, Value>,
	after: BTreeMap<LiveIdentity, Value>,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<Vec<LiveEvent>> {
	let maximum = before
		.len()
		.checked_add(after.len())
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE net identities"))?;
	budget.admit(
		maximum
			.checked_mul(std::mem::size_of::<LiveEvent>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE net events"))?,
		maximum as u64,
	)?;
	let mut entries = Vec::new();
	entries.try_reserve_exact(maximum).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded("LIVE net allocation", maximum as u64)
			.with_technical_source(error)
	})?;
	for (identity, value) in after {
		let old = before.remove(&identity);
		if old.as_ref().is_some_and(|old| crate::values::runtime::same_value(old, &value)) {
			continue;
		}
		entries.push(LiveEvent {
			output: None,
			ordinal: 0,
			identity,
			before: old,
			after: Some(value),
		});
	}
	for (identity, value) in before {
		entries.push(LiveEvent {
			output: None,
			ordinal: 0,
			identity,
			before: Some(value),
			after: None,
		});
	}
	entries.sort_unstable_by(|left, right| left.identity.cmp(&right.identity));
	for (ordinal, entry) in entries.iter_mut().enumerate() {
		entry.ordinal = u32::try_from(ordinal)
			.map_err(|_error| FerrosGrynnError::operation_overflow("LIVE ordinal"))?;
	}
	Ok(entries)
}

#[cfg(test)]
mod tests;
