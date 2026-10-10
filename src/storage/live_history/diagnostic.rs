//! Bounded declared statuses and diagnostic records, without technical sources.

use std::{
	collections::BTreeMap,
	num::NonZeroU32,
	sync::Arc,
	time::{Duration, UNIX_EPOCH},
};

use super::Work;
use crate::{
	gql_status::{
		ExecutionPhase,
		FerrosGrynnError,
		FerrosGrynnResult,
		LabeledLocation,
		LineColumn,
		Location,
		Severity,
		Status,
		StatusCode,
		StatusObject,
		TextSpan,
		formatting::DiagnosticValue,
	},
	storage::history_codec::{CodecBudget, Decoder, append, append_blob, invalid, values},
};

/// Maximum diagnostic collection count, independent of the envelope work cap.
const MAX_COUNT: usize = 1024;
/// Maximum owned diagnostic string, checked before copying it.
const MAX_TEXT: usize = 64 * 1024;

/// Writes bounded UTF-8 without copying the borrowed status field.
fn text(output: &mut Vec<u8>, value: &str, budget: &mut CodecBudget) -> FerrosGrynnResult<()> {
	if value.len() > MAX_TEXT {
		return Err(invalid("LIVE diagnostic text exceeds its cap"));
	}
	append_blob(output, value.as_bytes(), budget)
}

/// Admits an owned UTF-8 copy after checking its independent length cap.
fn read_text(data: &mut Decoder<'_>, budget: &mut CodecBudget) -> FerrosGrynnResult<String> {
	let bytes = data.blob()?;
	if bytes.len() > MAX_TEXT {
		return Err(invalid("LIVE diagnostic text exceeds its cap"));
	}
	let value = std::str::from_utf8(bytes)
		.map_err(|_error| invalid("LIVE diagnostic text is not UTF-8"))?;
	budget.admit(
		bytes
			.len()
			.checked_mul(2)
			.and_then(|bytes| bytes.checked_add(64))
			.ok_or_else(|| invalid("LIVE diagnostic text reservation overflows"))?,
	)?;
	Ok(value.to_owned())
}

/// Applies aggregate work and the tighter diagnostic recursion bound.
fn check(work: &mut Work, depth: usize) -> FerrosGrynnResult<()> {
	if depth > 16 {
		return Err(invalid("LIVE diagnostic nesting exceeds 16"));
	}
	work.take(depth, 1)
}

/// Counts are bounded before decoding maps, lists or location vectors.
fn count(data: &mut Decoder<'_>, minimum: usize, work: &Work) -> FerrosGrynnResult<usize> {
	let count = values::count(data, minimum)?;
	if count > MAX_COUNT {
		return Err(invalid("LIVE diagnostic count exceeds its cap"));
	}
	work.check_count(count)?;
	Ok(count)
}

/// Writes exact diagnostic primitives rather than converting u64 or f64 to
/// JSON.
fn value(
	output: &mut Vec<u8>,
	item: &DiagnosticValue,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<()> {
	check(work, depth)?;
	match item {
		DiagnosticValue::Null => append(output, &[0], budget)?,
		DiagnosticValue::Bool(item) => append(output, &[1, u8::from(*item)], budget)?,
		DiagnosticValue::Integer(item) => {
			append(output, &[2], budget)?;
			append(output, &item.to_le_bytes(), budget)?;
		}
		DiagnosticValue::Unsigned(item) => {
			append(output, &[3], budget)?;
			append(output, &item.to_le_bytes(), budget)?;
		}
		DiagnosticValue::Float(item) => {
			append(output, &[4], budget)?;
			append(output, &item.to_le_bytes(), budget)?;
		}
		DiagnosticValue::String(item) => {
			append(output, &[5], budget)?;
			text(output, item, budget)?;
		}
		DiagnosticValue::List(items) => {
			if items.len() > MAX_COUNT {
				return Err(invalid("LIVE diagnostic list exceeds its cap"));
			}
			append(output, &[6], budget)?;
			values::write_count(output, items.len(), budget)?;
			for item in items {
				value(output, item, budget, work, depth + 1)?;
			}
		}
		DiagnosticValue::Map(items) => {
			append(output, &[7], budget)?;
			map(
				output,
				items.iter().map(|(key, value)| (key.as_str(), value)),
				items.len(),
				budget,
				work,
				depth + 1,
			)?;
		}
		_ => return Err(invalid("LIVE diagnostic value kind is unsupported")),
	}
	Ok(())
}

/// Appends canonical maps without allocating a second borrowed map.
fn map<'item>(
	output: &mut Vec<u8>,
	items: impl Iterator<Item = (&'item str, &'item DiagnosticValue)>,
	count: usize,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<()> {
	check(work, depth)?;
	if count > MAX_COUNT {
		return Err(invalid("LIVE diagnostic map exceeds its cap"));
	}
	values::write_count(output, count, budget)?;
	for (key, item) in items {
		text(output, key, budget)?;
		value(output, item, budget, work, depth + 1)?;
	}
	Ok(())
}

/// Restores exact typed numeric cells and checked compound collections.
fn read_value(
	data: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<DiagnosticValue> {
	check(work, depth)?;
	Ok(match data.u8()? {
		0 => DiagnosticValue::Null,
		1 => DiagnosticValue::Bool(match data.u8()? {
			0 => false,
			1 => true,
			_ => return Err(invalid("LIVE diagnostic bool is invalid")),
		}),
		2 => DiagnosticValue::Integer(i64::from_le_bytes(
			data.take(8)?
				.try_into()
				.map_err(|_error| invalid("LIVE diagnostic integer is truncated"))?,
		)),
		3 => DiagnosticValue::Unsigned(data.u64()?),
		4 => DiagnosticValue::Float(f64::from_le_bytes(
			data.take(8)?
				.try_into()
				.map_err(|_error| invalid("LIVE diagnostic float is truncated"))?,
		)),
		5 => DiagnosticValue::String(read_text(data, budget)?),
		6 => {
			let count = count(data, 1, work)?;
			let mut items = super::values::reserve::<DiagnosticValue>(count, budget)?;
			for _ in 0..count {
				items.push(read_value(data, budget, work, depth + 1)?);
			}
			DiagnosticValue::List(items)
		}
		7 => DiagnosticValue::Map(read_map(data, budget, work, depth + 1)?),
		_ => return Err(invalid("LIVE diagnostic value tag is unknown")),
	})
}

/// Admits full B-tree nodes and rejects duplicate/noncanonical parameter names.
fn read_map(
	data: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<BTreeMap<String, DiagnosticValue>> {
	check(work, depth)?;
	let count = count(data, 9, work)?;
	budget.admit(crate::values::storage::tree_allocation_bytes(
		count,
		std::mem::size_of::<(String, DiagnosticValue)>(),
	)?)?;
	let mut items = BTreeMap::new();
	for _ in 0..count {
		let key = read_text(data, budget)?;
		if items.last_key_value().is_some_and(|(previous, _)| previous >= &key) {
			return Err(invalid("LIVE diagnostic keys are not sorted distinct"));
		}
		items.insert(key, read_value(data, budget, work, depth + 1)?);
	}
	Ok(items)
}

/// Encodes complete logical source coordinates without retaining source text.
fn location(
	output: &mut Vec<u8>,
	location: &Location,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	if location.span().end().is_none()
		|| (location.start().line, location.start().column)
			> (location.end().line, location.end().column)
	{
		return Err(invalid("LIVE diagnostic location bounds are invalid"));
	}
	append(output, &[u8::from(location.source_id().is_some())], budget)?;
	if let Some(source) = location.source_id() {
		text(output, source, budget)?;
	}
	for item in [
		location.span().offset,
		location.span().len,
		location.start().line.get(),
		location.start().column.get(),
		location.end().line.get(),
		location.end().column.get(),
	] {
		append(output, &item.to_le_bytes(), budget)?;
	}
	Ok(())
}

/// Checks nonzero line coordinates and span overflow before runtime
/// construction.
fn read_location(data: &mut Decoder<'_>, budget: &mut CodecBudget) -> FerrosGrynnResult<Location> {
	let source = match data.u8()? {
		0 => None,
		1 => Some(Arc::<str>::from(read_text(data, budget)?)),
		_ => return Err(invalid("LIVE diagnostic source presence is invalid")),
	};
	let span = TextSpan::new(data.u32()?, data.u32()?);
	if span.end().is_none() {
		return Err(invalid("LIVE diagnostic span overflows"));
	}
	let mut nonzero = || {
		NonZeroU32::new(data.u32()?)
			.ok_or_else(|| invalid("LIVE diagnostic line coordinate is zero"))
	};
	let start = LineColumn::new(nonzero()?, nonzero()?);
	let end = LineColumn::new(nonzero()?, nonzero()?);
	if (start.line, start.column) > (end.line, end.column) {
		return Err(invalid("LIVE diagnostic coordinates are reversed"));
	}
	Ok(Location::from_parts(source, span, start, end))
}

/// Persists the exact declared status, logical metadata and bounded cause
/// chain.
pub(super) fn encode(
	output: &mut Vec<u8>,
	error: &FerrosGrynnError,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<()> {
	check(work, depth)?;
	let status = error.status();
	let bytes = status
		.status()
		.parameter_allocation_bytes()
		.ok_or_else(|| invalid("LIVE diagnostic parameter admission overflows"))?;
	budget.admit(
		bytes.checked_mul(4).ok_or_else(|| invalid("LIVE diagnostic parameter copies overflow"))?,
	)?;
	append(output, status.code().body().as_bytes(), budget)?;
	let parameters = status.parameters();
	map(
		output,
		parameters.iter().map(|(key, item)| (*key, item)),
		parameters.len(),
		budget,
		work,
		0,
	)?;
	let diagnostic = status.diagnostic();
	append(output, &[u8::from(diagnostic.primary_location().is_some())], budget)?;
	if let Some(primary) = diagnostic.primary_location() {
		location(output, primary, budget)?;
	}
	if diagnostic.related_locations().len() > MAX_COUNT {
		return Err(invalid("LIVE diagnostic related locations exceed cap"));
	}
	values::write_count(output, diagnostic.related_locations().len(), budget)?;
	for related in diagnostic.related_locations() {
		work.take(0, 1)?;
		location(output, related.location(), budget)?;
		append(output, &[u8::from(related.label().is_some())], budget)?;
		if let Some(label) = related.label() {
			text(output, label, budget)?;
		}
	}
	let phase = match diagnostic.phase() {
		None => 0,
		Some(ExecutionPhase::Parsing) => 1,
		Some(ExecutionPhase::SemanticAnalysis) => 2,
		Some(ExecutionPhase::Planning) => 3,
		Some(ExecutionPhase::Optimization) => 4,
		Some(ExecutionPhase::Execution) => 5,
		Some(ExecutionPhase::Commit) => 6,
		Some(ExecutionPhase::Persistence) => 7,
		Some(ExecutionPhase::Recovery) => 8,
		Some(_) => return Err(invalid("LIVE diagnostic phase is unsupported")),
	};
	let severity = match diagnostic.severity_override() {
		None => 0,
		Some(Severity::Information) => 1,
		Some(Severity::Warning) => 2,
		Some(Severity::Error) => 3,
		Some(Severity::Critical) => 4,
		Some(_) => return Err(invalid("LIVE diagnostic severity is unsupported")),
	};
	append(output, &[phase, severity, u8::from(diagnostic.timestamp().is_some()), 0], budget)?;
	if let Some(timestamp) = diagnostic.timestamp() {
		let (negative, elapsed) = match timestamp.duration_since(UNIX_EPOCH) {
			Ok(elapsed) => (0, elapsed),
			Err(error) => (1, error.duration()),
		};
		append(output, &[negative], budget)?;
		append(output, &elapsed.as_secs().to_le_bytes(), budget)?;
		append(output, &elapsed.subsec_nanos().to_le_bytes(), budget)?;
	}
	map(
		output,
		diagnostic.extensions().iter().map(|(key, item)| (key.as_str(), item)),
		diagnostic.extensions().len(),
		budget,
		work,
		0,
	)?;
	append(output, &[u8::from(error.cause().is_some())], budget)?;
	if let Some(cause) = error.cause() {
		encode(output, cause, budget, work, depth + 1)?;
	}
	Ok(())
}

/// Restores original status codes, parameters and causes through generated
/// validation.
pub(super) fn decode(
	data: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<FerrosGrynnError> {
	check(work, depth)?;
	budget.admit(1024)?;
	let body = std::str::from_utf8(data.take(5)?)
		.map_err(|_error| invalid("LIVE diagnostic code is not UTF-8"))?;
	let code = format!("FG-{body}")
		.parse::<StatusCode>()
		.map_err(|_error| invalid("LIVE diagnostic code is invalid"))?;
	let parameters = read_map(data, budget, work, 0)?;
	let status = Status::from_parameters(code, parameters)
		.map_err(|cause| invalid("LIVE captured status schema is invalid").with_cause(cause))?;
	if !status.definition().kind.is_error() {
		return Err(invalid("LIVE fault contains a non-error status"));
	}
	let mut status = StatusObject::new(status);
	match data.u8()? {
		0 => {}
		1 => status = status.with_location(read_location(data, budget)?),
		_ => return Err(invalid("LIVE diagnostic primary location presence is invalid")),
	}
	let related = count(data, 26, work)?;
	budget.admit(
		related
			.checked_mul(3)
			.and_then(|slots| slots.checked_add(4))
			.and_then(|slots| slots.checked_mul(std::mem::size_of::<LabeledLocation>()))
			.ok_or_else(|| invalid("LIVE diagnostic related allocation overflows"))?,
	)?;
	for _ in 0..related {
		work.take(0, 1)?;
		let location = read_location(data, budget)?;
		let label = match data.u8()? {
			0 => None,
			1 => Some(read_text(data, budget)?),
			_ => return Err(invalid("LIVE diagnostic label presence is invalid")),
		};
		status = status.with_related_location(LabeledLocation::new(location, label));
	}
	let phase = match data.u8()? {
		0 => None,
		1 => Some(ExecutionPhase::Parsing),
		2 => Some(ExecutionPhase::SemanticAnalysis),
		3 => Some(ExecutionPhase::Planning),
		4 => Some(ExecutionPhase::Optimization),
		5 => Some(ExecutionPhase::Execution),
		6 => Some(ExecutionPhase::Commit),
		7 => Some(ExecutionPhase::Persistence),
		8 => Some(ExecutionPhase::Recovery),
		_ => return Err(invalid("LIVE diagnostic phase is unknown")),
	};
	if let Some(phase) = phase {
		status = status.with_phase(phase);
	}
	let severity = match data.u8()? {
		0 => None,
		1 => Some(Severity::Information),
		2 => Some(Severity::Warning),
		3 => Some(Severity::Error),
		4 => Some(Severity::Critical),
		_ => return Err(invalid("LIVE diagnostic severity is unknown")),
	};
	if let Some(severity) = severity {
		status = status
			.with_severity(severity)
			.map_err(|_error| invalid("LIVE diagnostic severity is incompatible"))?;
	}
	let timestamp = data.u8()?;
	data.zero(1)?;
	match timestamp {
		0 => {}
		1 => {
			let sign = data.u8()?;
			let seconds = data.u64()?;
			let nanos = data.u32()?;
			if nanos >= 1_000_000_000 {
				return Err(invalid("LIVE diagnostic timestamp is not normalized"));
			}
			let elapsed = Duration::new(seconds, nanos);
			let timestamp = match sign {
				0 => UNIX_EPOCH.checked_add(elapsed),
				1 => UNIX_EPOCH.checked_sub(elapsed),
				_ => return Err(invalid("LIVE diagnostic timestamp sign is invalid")),
			}
			.ok_or_else(|| invalid("LIVE diagnostic timestamp is out of range"))?;
			status = status.with_timestamp(timestamp);
		}
		_ => return Err(invalid("LIVE diagnostic timestamp presence is invalid")),
	}
	let extensions = read_map(data, budget, work, 0)?;
	// Moving extensions into the status creates a second map while the first
	// drains.
	budget.admit(crate::values::storage::tree_allocation_bytes(
		extensions.len(),
		std::mem::size_of::<(String, DiagnosticValue)>(),
	)?)?;
	for (key, item) in extensions {
		status = status.with_extension(key, item);
	}
	let mut error = FerrosGrynnError::try_from(status)
		.map_err(|_error| invalid("LIVE fault contains a non-error status"))?;
	match data.u8()? {
		0 => {}
		1 => error = error.with_cause(decode(data, budget, work, depth + 1)?),
		_ => return Err(invalid("LIVE diagnostic cause presence is invalid")),
	}
	Ok(error)
}
