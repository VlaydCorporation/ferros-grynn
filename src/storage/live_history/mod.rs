//! Private computed LIVE envelopes over the database's shared history
//! transport.

#![allow(
	clippy::redundant_pub_crate,
	reason = "Computed history crosses private storage/datastore owners"
)]

/// Exact declared statuses, metadata and cause chains for replayed faults.
mod diagnostic;
/// Separate owned graph checkpoints over the existing history transport.
mod graph;
#[cfg(test)]
mod resource_tests;
#[cfg(test)]
mod tests;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveBatch,
		LiveBindingId,
		LiveBindingRevision,
		LiveEvent,
		LiveGap,
		LiveGapReason,
		LiveId,
		LiveIdentity,
		LiveIdentityPart,
		LivePayload,
		LiveVersion,
	},
	storage::history_codec::{
		CodecBudget,
		Decoder,
		append,
		append_blob,
		checked_payload,
		invalid,
		logical_values::{self as values, Work},
		qualified,
		values as canonical,
	},
	values::{DateTime, Value, storage::EncodeContext},
};

/// Fixed FGLB header with independent version, counts and section lengths.
const HEADER_BYTES: usize = 96;
/// Aggregate logical work cap, independent of bytes and individual counts.
const MAX_NODES: usize = 1_000_000;
/// Issued and pending control payloads are prepaid by consumer attachments.
const CONTROL_BYTES: usize = 64 * 1024;

/// LIVE references never enter the user-visible CHANGEFEED segment table.
pub type LiveHistorySegments = std::sync::Arc<
	std::collections::BTreeMap<LiveId, std::sync::Arc<Vec<crate::storage::HistorySegment>>>,
>;

/// Metadata sufficient for whole-commit seeking before image allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveGroupInfo {
	/// Exact subscription incarnation, never a ChangefeedId.
	pub(crate) id: LiveId,
	/// Captured logical definition version.
	pub(crate) version: LiveVersion,
	/// Existing durable commit clock.
	pub(crate) stamp: u64,
	/// Captured UTC publication time.
	pub(crate) time: i128,
	/// Complete result events, zero for controls.
	pub(crate) events: u32,
	/// Batch, gap or fault; validated by the header parser.
	kind: u8,
}

impl LiveGroupInfo {
	/// Separates graph data from ordinary rows and prepaid controls.
	pub(crate) const fn is_graph(self) -> bool {
		self.kind == 3
	}

	/// Returns whether this envelope is an explicit GAP or fault control.
	pub(crate) const fn is_control(self) -> bool {
		matches!(self.kind, 1 | 2)
	}

	/// Rejects oversized controls before continuation pages are allocated.
	pub(in crate::storage) fn admit_delivery(&self, bytes: u64) -> FerrosGrynnResult<()> {
		if self.is_control() && bytes > CONTROL_BYTES as u64 {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"LIVE control delivery",
				bytes,
				CONTROL_BYTES as u64,
			));
		}
		Ok(())
	}
}

/// Header lengths stay private to the trusted parser.
struct Header {
	/// Validated seeking metadata.
	info: LiveGroupInfo,
	/// Captured binding revision count.
	bindings: usize,
	/// Self-contained historical dictionaries.
	dictionary: usize,
	/// Complete logical body.
	body: usize,
}

/// Reads fixed framing without allocating dictionaries or results.
pub(crate) fn inspect(prefix: &[u8], length: u64) -> FerrosGrynnResult<LiveGroupInfo> {
	Ok(header(prefix, length)?.info)
}

/// Checks all fixed lengths, clocks, reserved bytes and independent count caps.
fn header(prefix: &[u8], length: u64) -> FerrosGrynnResult<Header> {
	let length = checked_payload(length)?;
	let mut decoder = Decoder::new(
		prefix.get(..HEADER_BYTES).ok_or_else(|| invalid("LIVE result header is truncated"))?,
	);
	if decoder.take(4)? != b"FGLB" || decoder.take(2)? != 2_u16.to_le_bytes() {
		return Err(invalid("LIVE result marker/version is invalid"));
	}
	let kind = decoder.u8()?;
	decoder.zero(1)?;
	let id = LiveId::from_uuid(
		uuid::Uuid::from_slice(decoder.take(16)?)
			.map_err(|_error| invalid("LIVE result identity is malformed"))?
			.into(),
	)?;
	let version = LiveVersion::new(decoder.u64()?)?;
	let stamp = decoder.u64()?;
	let time = i128::from_le_bytes(
		decoder.take(16)?.try_into().map_err(|_error| invalid("LIVE result time is truncated"))?,
	);
	let bindings = decoder.u32()? as usize;
	let events = decoder.u32()?;
	let dictionary = checked_payload(decoder.u64()?)?;
	let body = checked_payload(decoder.u64()?)?;
	decoder.zero(16)?;
	decoder.finish()?;
	if stamp == 0
		|| kind > 3
		|| bindings > MAX_NODES
		|| events as usize > MAX_NODES
		|| matches!(kind, 1 | 2) && (events != 0 || bindings != 0)
		|| matches!(kind, 0 | 3) && events == 0
		|| dictionary < 16
		|| dictionary.checked_add(body).and_then(|bytes| bytes.checked_add(HEADER_BYTES))
			!= Some(length)
		|| bindings
			.checked_mul(32)
			.and_then(|bytes| bytes.checked_add((events as usize).checked_mul(8)?))
			.is_none_or(|minimum| minimum > body)
	{
		return Err(invalid("LIVE result counts, clocks or lengths are inconsistent"));
	}
	jiff::Timestamp::from_nanosecond(time)
		.map_err(|_error| invalid("LIVE result time is outside the UTC range"))?;
	Ok(Header {
		info: LiveGroupInfo {
			id,
			version,
			stamp,
			time,
			events,
			kind,
		},
		bindings,
		dictionary,
		body,
	})
}

/// Checks initial results without publishing an initial batch. This uses the
/// same identity/value encoding as subsequent deliveries, so registration
/// cannot activate a descriptor whose current results cannot be restored.
pub(crate) fn validate_rows<'row>(
	rows: impl Iterator<Item = (&'row LiveIdentity, &'row Value)>,
	maximum: usize,
) -> FerrosGrynnResult<(usize, u64)> {
	let mut budget = CodecBudget::new(maximum, 0)?;
	let mut context = EncodeContext::default();
	let mut output = Vec::new();
	let mut work = Work {
		remaining: MAX_NODES,
	};
	for (identity, value) in rows {
		work.take(0, 1)?;
		output.clear();
		encode_identity(&mut output, identity, &mut context, &mut budget, &mut work)?;
		values::encode(&mut output, value, &mut context, &mut budget, &mut work, 0)?;
	}
	canonical::encode_dictionary(&context, &mut budget)?;
	Ok((budget.admitted_bytes(), (MAX_NODES - work.remaining) as u64))
}

/// Shares exact qualified and aggregate identity bytes across validation and
/// durable result encoding.
fn encode_identity(
	output: &mut Vec<u8>,
	identity: &LiveIdentity,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<()> {
	match identity {
		LiveIdentity::Record(record) => encode_record_identity(output, record, context, budget)?,
		LiveIdentity::Occurrence(occurrence) => {
			append(output, &[1], budget)?;
			qualified::encode_occurrence(output, occurrence, context, budget)?;
		}
		LiveIdentity::Aggregate(keys) => {
			append(output, &[2], budget)?;
			canonical::write_count(output, keys.len(), budget)?;
			for key in keys {
				values::encode(output, key, context, budget, work, 0)?;
			}
		}
		LiveIdentity::Composite(parts) => {
			work.take(0, parts.len())?;
			if !crate::live::valid_composite_parts(parts) {
				return Err(invalid("LIVE composite identity has a noncanonical shape"));
			}
			append(output, &[3], budget)?;
			canonical::write_count(output, parts.len(), budget)?;
			for part in parts {
				match part {
					LiveIdentityPart::Record(record) => {
						encode_record_identity(output, record, context, budget)?;
					}
					LiveIdentityPart::Occurrence(occurrence) => {
						append(output, &[1], budget)?;
						qualified::encode_occurrence(output, occurrence, context, budget)?;
					}
					LiveIdentityPart::Unmatched => append(output, &[2], budget)?,
					LiveIdentityPart::Split(position) => {
						append(output, &[3], budget)?;
						append(output, &position.to_le_bytes(), budget)?;
					}
				}
			}
		}
	}
	Ok(())
}

/// Record identities and composite record participants share their exact
/// bounded representation and admit the owned clone before constructing it.
fn encode_record_identity(
	output: &mut Vec<u8>,
	record: &crate::values::RecordId,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	append(output, &[0], budget)?;
	budget.admit(
		record
			.estimated_heap_bytes()
			.checked_add(128)
			.ok_or_else(|| invalid("LIVE record identity reservation overflows"))?,
	)?;
	let bytes = canonical::encode(&Value::from(record.clone()), context, budget)?;
	append_blob(output, &bytes, budget)
}

/// Restores a source record without consulting current primary records.
fn decode_record_identity(
	data: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<crate::values::RecordId> {
	match canonical::decode(data.blob()?, context, budget)? {
		Value::Record(record) => Ok(*record),
		_ => Err(invalid("LIVE record identity contains another value kind")),
	}
}

/// Decodes a bounded flat identity independently of projected images.
fn decode_identity(
	data: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
) -> FerrosGrynnResult<LiveIdentity> {
	Ok(match data.u8()? {
		0 => LiveIdentity::Record(decode_record_identity(data, context, budget)?),
		1 => LiveIdentity::Occurrence(qualified::decode_occurrence(data, context, budget)?),
		2 => {
			let count = canonical::count(data, 1)?;
			work.check_count(count)?;
			let mut keys = values::reserve::<Value>(count, budget)?;
			for _ in 0..count {
				keys.push(values::decode(data, context, budget, work, 0)?);
			}
			LiveIdentity::Aggregate(keys)
		}
		3 => {
			let count = canonical::count(data, 1)?;
			if count < 2 {
				return Err(invalid("LIVE composite identity requires at least two parts"));
			}
			work.take(0, count)?;
			let mut parts = values::reserve::<LiveIdentityPart>(count, budget)?;
			for _ in 0..count {
				parts.push(match data.u8()? {
					0 => LiveIdentityPart::Record(decode_record_identity(data, context, budget)?),
					1 => LiveIdentityPart::Occurrence(qualified::decode_occurrence(
						data, context, budget,
					)?),
					2 => LiveIdentityPart::Unmatched,
					3 => LiveIdentityPart::Split(data.u64()?),
					_ => return Err(invalid("LIVE composite identity part tag is unknown")),
				});
			}
			if !crate::live::valid_composite_parts(&parts) {
				return Err(invalid("LIVE composite identity has a noncanonical shape"));
			}
			LiveIdentity::Composite(parts)
		}
		_ => return Err(invalid("LIVE result identity tag is unknown")),
	})
}

/// Encodes exact already-evaluated results under the caller's aggregate cap.
pub(crate) fn encode(
	payload: &LivePayload,
	id: LiveId,
	version: LiveVersion,
	stamp: u64,
	time: i128,
	maximum: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut budget = CodecBudget::new(maximum, 0)?;
	let mut context = EncodeContext::default();
	let mut body = Vec::new();
	let mut work = Work {
		remaining: MAX_NODES,
	};
	let (kind, binding_count, event_count) = match payload {
		LivePayload::Batch(batch) => {
			if batch.id() != id
				|| batch.version() != version
				|| batch.versionstamp().get() != stamp
				|| batch.committed_at().as_inner().timestamp().as_nanosecond() != time
			{
				return Err(invalid("LIVE batch differs from its publication envelope"));
			}
			validate_batch(batch)?;
			work.take(0, batch.bindings().len())?;
			for binding in batch.bindings() {
				append(&mut body, binding.id.as_uuid().as_inner().as_bytes(), &mut budget)?;
				append(&mut body, &binding.definition.get().to_le_bytes(), &mut budget)?;
				append(&mut body, &binding.value.get().to_le_bytes(), &mut budget)?;
			}
			for event in batch.events() {
				work.take(0, 1)?;
				append(&mut body, &event.ordinal().to_le_bytes(), &mut budget)?;
				encode_identity(&mut body, event.identity(), &mut context, &mut budget, &mut work)?;
				let flags =
					u8::from(event.before().is_some()) | (u8::from(event.after().is_some()) << 1);
				append(&mut body, &[flags, 0, 0], &mut budget)?;
				for value in [event.before(), event.after()].into_iter().flatten() {
					values::encode(&mut body, value, &mut context, &mut budget, &mut work, 0)?;
				}
			}
			(0, batch.bindings().len(), batch.events().len())
		}
		LivePayload::Graph(batch) => {
			graph::encode(&mut body, batch, id, version, stamp, time, &mut budget, &mut work)?;
			(3, batch.bindings().len(), batch.delta().changes().len().max(1))
		}
		LivePayload::Gap(gap) => {
			encode_gap(&mut body, gap, id, stamp, &mut budget)?;
			(1, 0, 0)
		}
		LivePayload::Fault {
			diagnostic,
			gap,
		} => {
			encode_gap(&mut body, gap, id, stamp, &mut budget)?;
			if gap.reason() != LiveGapReason::EvaluationFault
				|| diagnostic.status().code().to_string() != "FG-G1F09"
			{
				return Err(invalid("LIVE fault lacks its evaluation fault status"));
			}
			diagnostic::encode(&mut body, diagnostic, &mut budget, &mut work, 0)?;
			(2, 0, 0)
		}
	};
	let dictionary = canonical::encode_dictionary(&context, &mut budget)?;
	let mut output = Vec::new();
	append(&mut output, b"FGLB", &mut budget)?;
	append(&mut output, &2_u16.to_le_bytes(), &mut budget)?;
	append(&mut output, &[kind, 0], &mut budget)?;
	append(&mut output, id.as_uuid().as_inner().as_bytes(), &mut budget)?;
	append(&mut output, &version.get().to_le_bytes(), &mut budget)?;
	append(&mut output, &stamp.to_le_bytes(), &mut budget)?;
	append(&mut output, &time.to_le_bytes(), &mut budget)?;
	canonical::write_count(&mut output, binding_count, &mut budget)?;
	canonical::write_count(&mut output, event_count, &mut budget)?;
	append(&mut output, &(dictionary.len() as u64).to_le_bytes(), &mut budget)?;
	append(&mut output, &(body.len() as u64).to_le_bytes(), &mut budget)?;
	append(&mut output, &[0; 16], &mut budget)?;
	append(&mut output, &dictionary, &mut budget)?;
	append(&mut output, &body, &mut budget)?;
	header(&output, output.len() as u64)?;
	Ok(output)
}

/// Restores the complete admitted result without executing SELECT or FETCH.
pub(crate) fn decode(bytes: &[u8], maximum: usize) -> FerrosGrynnResult<LivePayload> {
	let mut budget = CodecBudget::new(maximum, bytes.len())?;
	decode_bounded(bytes, &mut budget)
}

/// Consumer restoration applies the capture path's prepaid control ceiling.
pub(crate) fn decode_delivery(bytes: &[u8], maximum: usize) -> FerrosGrynnResult<LivePayload> {
	let info = inspect(bytes, bytes.len() as u64)?;
	info.admit_delivery(bytes.len() as u64)?;
	let mut budget = CodecBudget::new(maximum, bytes.len())?;
	decode_delivery_bounded(bytes, &mut budget)
}

/// Preserves aggregate page admission while separately capping owned controls.
pub(in crate::storage) fn decode_delivery_bounded(
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<LivePayload> {
	let info = inspect(bytes, bytes.len() as u64)?;
	info.admit_delivery(bytes.len() as u64)?;
	let maximum = budget.maximum;
	if info.is_control() {
		budget.maximum = maximum.min(
			budget
				.used
				.checked_add(CONTROL_BYTES - bytes.len())
				.ok_or_else(|| invalid("LIVE control admission overflows"))?,
		);
	}
	let result = decode_bounded(bytes, budget);
	budget.maximum = maximum;
	result
}

/// Uses the caller's complete page/payload admission, without resetting caps.
pub(in crate::storage) fn decode_bounded(
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<LivePayload> {
	let Header {
		info,
		bindings,
		dictionary,
		body,
	} = header(bytes, bytes.len() as u64)?;
	let mut decoder = Decoder::new(bytes);
	decoder.take(HEADER_BYTES)?;
	let context = canonical::decode_dictionary(decoder.take(dictionary)?, budget)?;
	let mut data = Decoder::new(decoder.take(body)?);
	let mut work = Work {
		remaining: MAX_NODES,
	};
	let payload = if info.kind == 0 {
		work.take(0, bindings)?;
		let mut revisions = values::reserve::<LiveBindingRevision>(bindings, budget)?;
		for _ in 0..bindings {
			let id = LiveBindingId::from_uuid(
				uuid::Uuid::from_slice(data.take(16)?)
					.map_err(|_error| invalid("LIVE historical binding identity is malformed"))?
					.into(),
			)?;
			revisions.push(LiveBindingRevision {
				id,
				definition: LiveVersion::new(data.u64()?)?,
				value: data.u64()?.into(),
			});
		}
		work.check_count(info.events as usize)?;
		let mut events = values::reserve::<LiveEvent>(info.events as usize, budget)?;
		for _ in 0..info.events {
			work.take(0, 1)?;
			let ordinal = data.u32()?;
			let identity = decode_identity(&mut data, &context, budget, &mut work)?;
			let flags = data.u8()?;
			data.zero(2)?;
			if flags == 0 || flags > 3 {
				return Err(invalid("LIVE result image presence is invalid"));
			}
			let before = if flags & 1 != 0 {
				Some(values::decode(&mut data, &context, budget, &mut work, 0)?)
			} else {
				None
			};
			let after = if flags & 2 != 0 {
				Some(values::decode(&mut data, &context, budget, &mut work, 0)?)
			} else {
				None
			};
			events.push(LiveEvent {
				output: None,
				ordinal,
				identity,
				before,
				after,
			});
		}
		let batch = LiveBatch {
			id: info.id,
			version: info.version,
			stamp: info.stamp.into(),
			committed_at: DateTime::from(
				jiff::Timestamp::from_nanosecond(info.time)?.to_zoned(jiff::tz::TimeZone::UTC),
			),
			events,
			bindings: revisions,
		};
		validate_batch(&batch)?;
		LivePayload::Batch(batch)
	} else if info.kind == 3 {
		LivePayload::Graph(std::sync::Arc::new(graph::decode(
			&mut data, info, bindings, budget, &mut work,
		)?))
	} else {
		let gap = decode_gap(&mut data, info.id, info.stamp)?;
		if info.kind == 1 {
			LivePayload::Gap(gap)
		} else {
			if gap.reason() != LiveGapReason::EvaluationFault {
				return Err(invalid("LIVE fault has another gap reason"));
			}
			let diagnostic = diagnostic::decode(&mut data, budget, &mut work, 0)?;
			if diagnostic.status().code().body() != "G1F09" {
				return Err(invalid("LIVE fault has a different declared status"));
			}
			LivePayload::Fault {
				diagnostic,
				gap,
			}
		}
	};
	data.finish()?;
	decoder.finish()?;
	Ok(payload)
}

/// Validates canonical net identity/ordinal order and historical parameter
/// clocks.
fn validate_batch(batch: &LiveBatch) -> FerrosGrynnResult<()> {
	if batch.events().is_empty()
		|| batch.events().len() > MAX_NODES
		|| batch.bindings().len() > MAX_NODES
		|| batch.events().windows(2).any(|pair| pair[0].identity() >= pair[1].identity())
		|| batch.bindings().windows(2).any(|pair| pair[0].id >= pair[1].id)
		|| batch.bindings().iter().any(|revision| {
			revision.value.get() == 0 || revision.value.get() > batch.versionstamp().get()
		}) {
		return Err(invalid("LIVE net result order or binding revisions are invalid"));
	}
	for (ordinal, event) in batch.events().iter().enumerate() {
		if event.ordinal() as usize != ordinal
			|| event.before().is_none() && event.after().is_none()
		{
			return Err(invalid("LIVE result ordinal or image presence is invalid"));
		}
	}
	Ok(())
}

/// Encodes an explicit whole-commit gap with independently optional bounds.
fn encode_gap(
	output: &mut Vec<u8>,
	gap: &LiveGap,
	id: LiveId,
	stamp: u64,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let reason = match gap.reason() {
		LiveGapReason::QueueOverflow => 0,
		LiveGapReason::VolatileHistoryLost => 1,
		LiveGapReason::InsufficientCoverage => 2,
		LiveGapReason::Retention => 3,
		LiveGapReason::SegmentCorrupt => 4,
		LiveGapReason::ImportSuppressed => 5,
		LiveGapReason::EvaluationFault => 6,
		LiveGapReason::SourceRetired => 7,
	};
	let from = gap.from().map_or(0, crate::changefeed::Versionstamp::get);
	let through = gap.through().map_or(0, crate::changefeed::Versionstamp::get);
	if gap.id() != id
		|| from > stamp
		|| through > stamp
		|| from != 0 && through != 0 && from > through
	{
		return Err(invalid("LIVE gap differs from its trusted publication bounds"));
	}
	append(
		output,
		&[
			reason,
			u8::from(gap.from().is_some()) | (u8::from(gap.through().is_some()) << 1),
			0,
			0,
			0,
			0,
			0,
			0,
		],
		budget,
	)?;
	append(output, &from.to_le_bytes(), budget)?;
	append(output, &through.to_le_bytes(), budget)
}

/// Rejects unknown reasons and noncanonical optional bound bytes.
fn decode_gap(data: &mut Decoder<'_>, id: LiveId, stamp: u64) -> FerrosGrynnResult<LiveGap> {
	let reason = match data.u8()? {
		0 => LiveGapReason::QueueOverflow,
		1 => LiveGapReason::VolatileHistoryLost,
		2 => LiveGapReason::InsufficientCoverage,
		3 => LiveGapReason::Retention,
		4 => LiveGapReason::SegmentCorrupt,
		5 => LiveGapReason::ImportSuppressed,
		6 => LiveGapReason::EvaluationFault,
		7 => LiveGapReason::SourceRetired,
		_ => return Err(invalid("LIVE gap reason is unknown")),
	};
	let flags = data.u8()?;
	data.zero(6)?;
	let from = data.u64()?;
	let through = data.u64()?;
	if flags > 3
		|| flags & 1 == 0 && from != 0
		|| flags & 2 == 0 && through != 0
		|| from > stamp
		|| through > stamp
		|| flags == 3 && from > through
	{
		return Err(invalid("LIVE gap optional bounds are inconsistent"));
	}
	Ok(LiveGap {
		id,
		reason,
		from: (flags & 1 != 0).then_some(from.into()),
		through: (flags & 2 != 0).then_some(through.into()),
	})
}
