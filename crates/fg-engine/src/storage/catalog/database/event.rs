//! Explicit little-endian event descriptors; executable IR is never encoded.

use super::codec::{Decoder, MAX_BYTES, invalid};
use crate::{
	datastore::{
		ClassId,
		EventAsyncOptions,
		EventDefinition,
		EventExecution,
		EventId,
		EventTarget,
		SchemaVersion,
	},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::history_codec::CodecBudget,
	values::Text,
};

/// Smallest valid descriptor, including nonempty name and both source fields.
pub(super) const MIN_BYTES: usize = 44;

/// Writes a bounded published definition with exact source owner identity.
pub(in crate::storage) fn encode(
	output: &mut Vec<u8>,
	event: &EventDefinition,
) -> FerrosGrynnResult<()> {
	let id = event.id().ok_or_else(|| invalid("published EVENT has no identity"))?;
	output.extend_from_slice(&id.get().to_le_bytes());
	output.extend_from_slice(&event.version().get().to_le_bytes());
	output.extend_from_slice(&event.published_at.to_le_bytes());
	let (kind, owner) = match event.target() {
		EventTarget::Class(class) => (0, u64::from(class.get())),
		EventTarget::Graph(graph) => (1, graph.get()),
	};
	let (asynchronous, retry, depth) = match event.execution() {
		EventExecution::Sync => (0, 0, 0),
		EventExecution::Async(options) => (1, options.retries(), options.max_depth()),
	};
	output.extend_from_slice(&[kind, asynchronous, retry, depth]);
	output.extend_from_slice(&owner.to_le_bytes());
	write_text(output, event.name(), 4096)?;
	output.push(u8::from(event.comment().is_some()));
	if let Some(comment) = event.comment() {
		write_text(output, comment, 4096)?;
	}
	write_text(output, event.when(), EventDefinition::source_byte_limit())?;
	write_text(output, event.then(), EventDefinition::source_byte_limit())?;
	Ok(())
}

/// Decodes source and policy after checked byte and allocation admission.
pub(in crate::storage) fn decode(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<EventDefinition> {
	let id = EventId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let kind = decoder.read_u8()?;
	let asynchronous = decoder.read_u8()?;
	let retry = decoder.read_u8()?;
	let depth = decoder.read_u8()?;
	let owner = decoder.read_u64()?;
	let target = match kind {
		0 => EventTarget::Class(ClassId::from_storage(u32::try_from(owner).map_err(|error| {
			invalid("EVENT class identity overflows").with_technical_source(error)
		})?)?),
		1 => EventTarget::Graph(
			GraphId::new(owner).ok_or_else(|| invalid("EVENT graph identity is zero"))?,
		),
		_ => return Err(invalid("EVENT source kind is unknown")),
	};
	let execution = match asynchronous {
		0 if retry == 0 && depth == 0 => EventExecution::Sync,
		1 => EventExecution::Async(
			EventAsyncOptions::new(retry, depth)
				.map_err(|error| invalid("EVENT async policy is invalid").with_cause(error))?,
		),
		_ => return Err(invalid("EVENT execution flags are invalid")),
	};
	let name = read_text(decoder, budget, 4096)?;
	let comment = match decoder.read_u8()? {
		0 => None,
		1 => Some(read_text(decoder, budget, 4096)?),
		_ => return Err(invalid("EVENT comment flag is invalid")),
	};
	let when = read_text(decoder, budget, EventDefinition::source_byte_limit())?;
	let then = read_text(decoder, budget, EventDefinition::source_byte_limit())?;
	let mut event = EventDefinition::new(name, target, when, then)
		.map_err(|error| invalid("EVENT descriptor fields are invalid").with_cause(error))?
		.with_execution(execution);
	event.id = Some(id);
	event.version = version;
	event.published_at = published_at;
	event.comment = comment;
	Ok(event)
}

/// Checks the complete text growth before extending the output buffer.
fn write_text(output: &mut Vec<u8>, value: &Text, maximum: usize) -> FerrosGrynnResult<()> {
	if value.len() > maximum || value.as_str().contains('\0') {
		return Err(invalid("EVENT text exceeds its UTF-8 limit"));
	}
	let needed = output
		.len()
		.checked_add(4)
		.and_then(|bytes| bytes.checked_add(value.len()))
		.ok_or_else(|| invalid("EVENT descriptor length overflows"))?;
	if needed > MAX_BYTES {
		return Err(invalid("EVENT descriptor exceeds catalog byte limit"));
	}
	output.extend_from_slice(
		&u32::try_from(value.len())
			.map_err(|error| invalid("EVENT text length overflows").with_technical_source(error))?
			.to_le_bytes(),
	);
	output.extend_from_slice(value.as_str().as_bytes());
	Ok(())
}

/// Validates framing and UTF-8 before allocating an owned text value.
fn read_text(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	maximum: usize,
) -> FerrosGrynnResult<Text> {
	let length = usize::try_from(decoder.read_u32()?)
		.map_err(|error| invalid("EVENT text length overflows").with_technical_source(error))?;
	if length > maximum {
		return Err(invalid("EVENT text exceeds its byte limit"));
	}
	let text = std::str::from_utf8(decoder.read_exact(length)?)
		.map_err(|error| invalid("EVENT text is not UTF-8").with_technical_source(error))?;
	if text.contains('\0') {
		return Err(invalid("EVENT text contains NUL"));
	}
	budget.admit(
		length
			.checked_add(std::mem::size_of::<Text>())
			.ok_or_else(|| invalid("EVENT text admission overflows"))?,
	)?;
	Ok(text.into())
}

#[cfg(test)]
/// Regression tests for event behavior.
mod tests {
	use super::*;

	/// Creates an explicit descriptor independent of catalog/page framing.
	fn fixture() -> FerrosGrynnResult<EventDefinition> {
		let target = EventTarget::Class(ClassId::from_storage(2)?);
		let mut event = EventDefinition::new("a", target, "true", "1")?
			.with_execution(EventExecution::Async(EventAsyncOptions::new(255, 16)?));
		event.id = Some(EventId::from_storage(3)?);
		event.published_at = 1;
		Ok(event)
	}

	/// Verifies event descriptor golden and policy round trip.
	#[test]
	fn event_descriptor_golden_and_policy_round_trip() {
		use crate::test_assertions::AssertResult as _;

		let event = fixture().assert_ok();
		let mut bytes = Vec::new();
		encode(&mut bytes, &event).assert_ok();
		let expected = [
			3, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 255, 16, 2, 0, 0, 0, 0, 0, 0, 0,
			1, 0, 0, 0, b'a', 0, 4, 0, 0, 0, b't', b'r', b'u', b'e', 1, 0, 0, 0, b'1',
		];
		assert_eq!(bytes.as_slice(), expected);
		let mut decoder = Decoder::new(&bytes);
		let mut budget = CodecBudget::new(1024 * 1024, bytes.len()).assert_ok();
		assert_eq!(decode(&mut decoder, &mut budget).assert_ok(), event);
		decoder.finish().assert_ok();
	}

	/// Verifies event descriptor rejects unknown flags depth and lengths.
	#[test]
	fn event_descriptor_rejects_unknown_flags_depth_and_lengths() {
		use crate::test_assertions::AssertResult as _;

		let mut bytes = Vec::new();
		encode(&mut bytes, &fixture().assert_ok()).assert_ok();
		for (offset, value) in [(16, 2), (17, 2), (19, 17)] {
			let mut invalid = bytes.clone();
			invalid[offset] = value;
			let mut decoder = Decoder::new(&invalid);
			let mut budget = CodecBudget::new(1024 * 1024, invalid.len()).assert_ok();
			assert!(decode(&mut decoder, &mut budget).is_err());
		}
		let mut oversized = bytes.clone();
		oversized[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
		let mut decoder = Decoder::new(&oversized);
		let mut budget = CodecBudget::new(1024 * 1024, oversized.len()).assert_ok();
		assert!(decode(&mut decoder, &mut budget).is_err());
		for length in 0..bytes.len() {
			let mut decoder = Decoder::new(&bytes[..length]);
			let mut budget = CodecBudget::new(1024 * 1024, length).assert_ok();
			assert!(decode(&mut decoder, &mut budget).is_err());
		}
	}
}
