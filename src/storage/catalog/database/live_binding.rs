//! Explicit reactive definition/value sections in the existing catalog.

use std::{mem::size_of, sync::Arc};

use super::{
	LiveBindingCatalogEntry,
	codec::{Decoder, invalid},
};
use crate::{
	gql_status::FerrosGrynnResult,
	live::{LiveBindingId, LiveBindingOptions, LiveExpr, LiveState, LiveVersion},
	storage::{
		history_codec::CodecBudget,
		live_descriptor::{self, LiveDescriptor},
	},
};

/// Fixed header; variable canonical descriptors follow the identity vectors.
pub(super) const MIN_BYTES: usize = 104;
/// Aggregate catalog cap, including every preexisting section.
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Appends one complete group after checking every length and aggregate bound.
pub(super) fn encode(
	output: &mut Vec<u8>,
	entry: &LiveBindingCatalogEntry,
) -> FerrosGrynnResult<()> {
	let (code, reason) = entry
		.diagnostic
		.as_ref()
		.map_or(("", ""), |(code, reason)| (code.as_str(), reason.as_str()));
	let source_bytes = entry
		.record_sources
		.iter()
		.try_fold(0_usize, |bytes, (name, _)| bytes.checked_add(8)?.checked_add(name.len()))
		.ok_or_else(|| invalid("LIVE source owner length overflows"))?;
	let length = MIN_BYTES
		.checked_add(
			entry
				.dependencies
				.len()
				.checked_mul(16)
				.ok_or_else(|| invalid("LIVE dependency length overflows"))?,
		)
		.and_then(|length| length.checked_add(entry.functions.len().checked_mul(8)?))
		.and_then(|length| length.checked_add(source_bytes))
		.and_then(|length| length.checked_add(entry.descriptor.len()))
		.and_then(|length| length.checked_add(entry.value_descriptor.len()))
		.and_then(|length| length.checked_add(code.len()))
		.and_then(|length| length.checked_add(reason.len()))
		.ok_or_else(|| invalid("LIVE binding entry length overflows"))?;
	if output.len().checked_add(length).is_none_or(|length| length > MAX_BYTES) {
		return Err(invalid("LIVE binding exceeds the aggregate catalog byte cap"));
	}
	let dependencies = u32::try_from(entry.dependencies.len())
		.map_err(|_error| invalid("LIVE dependency count overflows"))?;
	let functions = u32::try_from(entry.functions.len())
		.map_err(|_error| invalid("LIVE function count overflows"))?;
	let source_count = u32::try_from(entry.record_sources.len())
		.map_err(|_error| invalid("LIVE source count overflows"))?;
	let code_len = u32::try_from(code.len())
		.map_err(|_error| invalid("LIVE diagnostic code length overflows"))?;
	let reason_len = u32::try_from(reason.len())
		.map_err(|_error| invalid("LIVE diagnostic reason length overflows"))?;
	output.try_reserve_exact(length).map_err(|_error| invalid("LIVE binding allocation failed"))?;
	output.extend_from_slice(entry.id.as_uuid().as_inner().as_bytes());
	for value in [entry.version.get(), entry.defined_at, entry.published_at, entry.value_at] {
		output.extend_from_slice(&value.to_le_bytes());
	}
	output.push(match entry.state {
		LiveState::Active => 0,
		LiveState::Paused => 1,
		LiveState::Closed => 2,
		LiveState::Pending => return Err(invalid("private LIVE binding cannot be encoded")),
	});
	output.extend_from_slice(&[0; 3]);
	output.extend_from_slice(&source_count.to_le_bytes());
	for value in [
		entry.options.scratch_bytes() as u64,
		entry.options.work(),
		entry.descriptor.len() as u64,
		entry.value_descriptor.len() as u64,
	] {
		output.extend_from_slice(&value.to_le_bytes());
	}
	for value in [dependencies, functions, code_len, reason_len] {
		output.extend_from_slice(&value.to_le_bytes());
	}
	for id in &entry.dependencies {
		output.extend_from_slice(id.as_uuid().as_inner().as_bytes());
	}
	super::live_capture::encode(output, &entry.functions, &entry.record_sources)?;
	output.extend_from_slice(&entry.descriptor);
	output.extend_from_slice(&entry.value_descriptor);
	output.extend_from_slice(code.as_bytes());
	output.extend_from_slice(reason.as_bytes());
	Ok(())
}

/// Validates fixed fields/counts before allocating canonical owned values.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<LiveBindingCatalogEntry> {
	budget.admit(size_of::<LiveBindingCatalogEntry>())?;
	let id = binding_id(decoder)?;
	let version = LiveVersion::new(decoder.read_u64()?)?;
	let defined_at = decoder.read_u64()?;
	let published_at = decoder.read_u64()?;
	let value_at = decoder.read_u64()?;
	let state = match decoder.read_u8()? {
		0 => LiveState::Active,
		1 => LiveState::Paused,
		2 => LiveState::Closed,
		_ => return Err(invalid("LIVE binding state is invalid")),
	};
	decoder.expect_zero(3, "LIVE binding reserved bytes")?;
	let source_count = decoder.read_count("LIVE record source count")?;
	let scratch = addressable(decoder.read_u64()?)?;
	let work = decoder.read_u64()?;
	let options = LiveBindingOptions::default()
		.with_budgets(scratch, work)
		.map_err(|cause| invalid("LIVE binding limits are invalid").with_cause(cause))?;
	let descriptor_len = addressable(decoder.read_u64()?)?;
	let value_len = addressable(decoder.read_u64()?)?;
	let dependency_count = decoder.read_count("LIVE dependency count")?;
	let function_count = decoder.read_count("LIVE function count")?;
	let code_len = decoder.read_count("LIVE diagnostic code length")?;
	let reason_len = decoder.read_count("LIVE diagnostic reason length")?;
	let length = dependency_count
		.checked_mul(16)
		.and_then(|length| length.checked_add(function_count.checked_mul(8)?))
		.and_then(|length| length.checked_add(source_count.checked_mul(8)?))
		.and_then(|length| length.checked_add(descriptor_len))
		.and_then(|length| length.checked_add(value_len))
		.and_then(|length| length.checked_add(code_len))
		.and_then(|length| length.checked_add(reason_len))
		.ok_or_else(|| invalid("LIVE binding section lengths overflow"))?;
	if length > decoder.remaining_bytes() || code_len > 8 || reason_len > 4096 {
		return Err(invalid(
			"LIVE binding sections exceed their available bytes or diagnostic cap",
		));
	}
	budget.admit(
		dependency_count
			.checked_mul(size_of::<LiveBindingId>())
			.ok_or_else(|| invalid("LIVE binding identity allocation overflows"))?,
	)?;
	// The recovered runtime map owns a second vector copy while this DTO is
	// still retained. Admit that copy and its map/definition/fault metadata
	// now.
	budget.admit(
		dependency_count
			.checked_mul(size_of::<LiveBindingId>())
			.and_then(|bytes| {
				bytes.checked_add(size_of::<crate::datastore::LiveBindingDefinition>() + 512)
			})
			.and_then(|bytes| bytes.checked_add(code_len.checked_add(reason_len)?.checked_mul(2)?))
			.ok_or_else(|| invalid("LIVE recovered binding metadata overflows"))?,
	)?;
	let mut dependencies = Vec::new();
	dependencies
		.try_reserve_exact(dependency_count)
		.map_err(|_error| invalid("LIVE dependency allocation failed"))?;
	for _ in 0..dependency_count {
		dependencies.push(binding_id(decoder)?);
	}
	let (functions, record_sources) =
		super::live_capture::decode(decoder, budget, function_count, source_count)?;
	let descriptor = decoder.read_exact(descriptor_len)?;
	let LiveDescriptor::Binding {
		expression,
		parameters,
	} = live_descriptor::decode_bounded(descriptor, budget)?
	else {
		return Err(invalid("LIVE binding does not contain a binding recipe"));
	};
	let value_descriptor = decoder.read_exact(value_len)?;
	let LiveDescriptor::Binding {
		expression: LiveExpr::Literal(value),
		parameters: value_parameters,
	} = live_descriptor::decode_bounded(value_descriptor, budget)?
	else {
		return Err(invalid("LIVE binding value is not a literal historical descriptor"));
	};
	if !value_parameters.is_empty() {
		return Err(invalid("LIVE binding value has unexpected captures"));
	}
	budget.admit(
		descriptor_len
			.checked_add(value_len)
			.and_then(|length| length.checked_add(code_len))
			.and_then(|length| length.checked_add(reason_len))
			.ok_or_else(|| invalid("LIVE binding retained payload overflows"))?,
	)?;
	let diagnostic = super::live_capture::diagnostic(decoder, budget, code_len, reason_len)?;
	Ok(LiveBindingCatalogEntry {
		id,
		version,
		state,
		defined_at,
		published_at,
		value_at,
		options,
		expression: Arc::new(expression),
		parameters: Arc::new(parameters),
		value: Arc::new(value),
		descriptor: Arc::from(descriptor),
		value_descriptor: Arc::from(value_descriptor),
		dependencies,
		functions,
		record_sources,
		diagnostic,
	})
}

/// Rejects a malformed non-nil identity before inserting it into any
/// dictionary.
fn binding_id(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<LiveBindingId> {
	LiveBindingId::from_uuid(
		uuid::Uuid::from_slice(decoder.read_exact(16)?)
			.map_err(|_error| invalid("LIVE binding UUID is malformed"))?
			.into(),
	)
}

/// Checks persisted lengths before converting them into allocation sizes.
fn addressable(value: u64) -> FerrosGrynnResult<usize> {
	let value =
		usize::try_from(value).map_err(|_error| invalid("LIVE binding length exceeds usize"))?;
	if value > isize::MAX as usize {
		return Err(invalid("LIVE binding length is not addressable"));
	}
	Ok(value)
}
