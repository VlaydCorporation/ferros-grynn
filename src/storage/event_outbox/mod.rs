//! Explicit accepted source/context bytes; primary property codecs stay
//! unchanged.

use std::sync::Arc;

/// Commit deltas and bounded authoritative checkpoints.
pub(in crate::storage) mod journal;

use crate::{
	datastore::{EventDefinition, EventExecution, SchemaFunction},
	gql_status::FerrosGrynnResult,
	storage::{
		catalog,
		history_codec::{
			CodecBudget,
			Decoder,
			append,
			append_blob,
			invalid,
			logical_values::{self, Work},
			values,
		},
	},
	values::{Value, storage::EncodeContext},
};

/// Independent pending payload ceiling shared with datastore admission.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Bounded descriptor/context node count, independent of encoded byte lengths.
const MAX_NODES: usize = 1_000_000;

/// Checked source payload materialized only for an admitted worker attempt.
pub struct EventPayload {
	/// Decoder admission stays held while the worker compiles and executes the
	/// payload.
	pub(crate) retained_bytes: usize,
	/// Accepted definition revision, retained after source retirement.
	pub(crate) definition: EventDefinition,
	/// Historical context; retries never recompute these arguments.
	pub(crate) arguments: [Value; 5],
	/// Exact transitive THEN sources, without executable typed IR.
	pub(crate) functions: Vec<Arc<SchemaFunction>>,
}

/// Encodes accepted descriptors and qualified values under one scratch ledger.
pub fn encode_payload(
	event: &EventDefinition,
	arguments: &[Value; 5],
	functions: &[Arc<SchemaFunction>],
	maximum: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	if event.id().is_none() || !matches!(event.execution(), EventExecution::Async(_)) {
		return Err(invalid("accepted EVENT payload requires a published ASYNC definition"));
	}
	let mut budget = CodecBudget::new(maximum, 0)?;
	let mut context = EncodeContext::default();
	let mut body = Vec::new();
	let mut work = Work {
		remaining: MAX_NODES,
	};
	for argument in arguments {
		logical_values::encode(&mut body, argument, &mut context, &mut budget, &mut work, 0)?;
	}
	let dictionary = values::encode_dictionary(&context, &mut budget)?;
	let mut definition = Vec::new();
	budget.admit(
		event
			.name()
			.len()
			.saturating_add(event.when().len())
			.saturating_add(event.then().len())
			.saturating_add(event.comment().map_or(0, crate::values::Text::len))
			.saturating_mul(4)
			.saturating_add(512),
	)?;
	catalog::encode_event_source(&mut definition, event)?;
	let mut output = Vec::new();
	append(&mut output, b"FGEI", &mut budget)?;
	append(&mut output, &[1, 0, 0, 0], &mut budget)?;
	append_blob(&mut output, &definition, &mut budget)?;
	values::write_count(&mut output, functions.len(), &mut budget)?;
	let mut previous = None;
	for function in functions {
		let id = function.id().ok_or_else(|| invalid("frozen function has no identity"))?;
		if previous.is_some_and(|previous| previous >= id) {
			return Err(invalid("frozen function sources are not sorted distinct"));
		}
		previous = Some(id);
		let descriptor = catalog::encode_function_source(function, &mut budget)?;
		append_blob(&mut output, &descriptor, &mut budget)?;
	}
	append_blob(&mut output, &dictionary, &mut budget)?;
	append_blob(&mut output, &body, &mut budget)?;
	if output.len() > MAX_PAYLOAD_BYTES {
		return Err(invalid("EVENT payload exceeds pending byte ceiling"));
	}
	Ok(output)
}

/// Restores the accepted context without consulting current functions or data.
pub fn decode_payload(bytes: &[u8], maximum: usize) -> FerrosGrynnResult<EventPayload> {
	if bytes.len() > MAX_PAYLOAD_BYTES {
		return Err(invalid("EVENT payload exceeds pending byte ceiling"));
	}
	let mut decoder = Decoder::new(bytes);
	if decoder.take(4)? != b"FGEI" || decoder.take(2)? != 1_u16.to_le_bytes() {
		return Err(invalid("EVENT payload marker/version is invalid"));
	}
	decoder.zero(2)?;
	let mut budget = CodecBudget::new(maximum, bytes.len())?;
	let mut source = catalog::SourceDecoder::new(decoder.blob()?);
	let definition = catalog::decode_event_source(&mut source, &mut budget)?;
	source.finish()?;
	if !matches!(definition.execution(), EventExecution::Async(_)) {
		return Err(invalid("accepted EVENT payload is not ASYNC"));
	}
	let count = values::count(&mut decoder, 36)?;
	let mut functions = logical_values::reserve::<Arc<SchemaFunction>>(count, &mut budget)?;
	let mut previous = None;
	for _ in 0..count {
		let function = catalog::decode_function_source(decoder.blob()?, &mut budget)?;
		let id = function.id().ok_or_else(|| invalid("frozen function has no identity"))?;
		if previous.is_some_and(|previous| previous >= id) {
			return Err(invalid("frozen function sources are not sorted distinct"));
		}
		previous = Some(id);
		budget.admit(std::mem::size_of::<SchemaFunction>() + 32)?;
		functions.push(Arc::new(function));
	}
	let dictionary = values::decode_dictionary(decoder.blob()?, &mut budget)?;
	let mut body = Decoder::new(decoder.blob()?);
	decoder.finish()?;
	let mut work = Work {
		remaining: MAX_NODES,
	};
	let mut arguments = [Value::None, Value::None, Value::None, Value::None, Value::None];
	for argument in &mut arguments {
		*argument = logical_values::decode(&mut body, &dictionary, &mut budget, &mut work, 0)?;
	}
	body.finish()?;
	if !matches!(arguments[0].as_str(), Some("CREATE" | "UPDATE" | "DELETE" | "RELATE")) {
		return Err(invalid("EVENT historical operation kind is invalid"));
	}
	Ok(EventPayload {
		retained_bytes: budget.admitted_bytes(),
		definition,
		arguments,
		functions,
	})
}

#[cfg(test)]
/// Regression tests for event outbox behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{EventAsyncOptions, EventId, EventTarget, FunctionEffect, FunctionId},
		graph::{
			AtomKind,
			AtomOccurrenceRef,
			AtomRef,
			GraphId,
			GraphKind,
			GraphRef,
			GraphStorageMode,
		},
		values::{Array, Object, Type},
	};

	/// Verifies event payload round trips frozen sources and qualified context.
	#[test]
	fn event_payload_round_trips_frozen_sources_and_qualified_context() {
		use crate::test_assertions::AssertResult as _;

		let graph = GraphRef::new(
			GraphId::new(1).ok_or_else(|| invalid("fixture graph")).assert_ok(),
			"scope",
			GraphKind::Metagraph,
			GraphStorageMode::InMemory,
		)
		.assert_ok();
		let occurrence = AtomOccurrenceRef::new(
			AtomRef::new(AtomKind::Edge, "link:e".parse().assert_ok()),
			Arc::new(graph.clone()),
		);
		let mut event =
			EventDefinition::new("audit", EventTarget::Graph(graph.id()), "true", "fn::read()")
				.assert_ok()
				.with_execution(EventExecution::Async(EventAsyncOptions::new(255, 16).assert_ok()));
		event.id = Some(EventId::from_storage(2).assert_ok());
		let mut function = SchemaFunction::new(
			"fn",
			"read",
			Vec::new(),
			Type::Int,
			FunctionEffect::Stable,
			false,
			None,
			"{ RETURN 1; }",
		)
		.assert_ok();
		function.id = Some(FunctionId::from_storage(3).assert_ok());
		let functions = vec![Arc::new(function)];
		let arguments = [
			Value::from("RELATE"),
			Value::Null,
			Value::Null,
			Value::from(Object::from_iter([
				("edge", Value::from(occurrence.clone())),
				("endpoints", Value::from(Array::new(vec![Value::from(occurrence)]))),
			])),
			Value::from(Object::from_iter([("explicit", Value::from(7))])),
		];
		let bytes = encode_payload(&event, &arguments, &functions, 16 * 1024 * 1024).assert_ok();
		assert_eq!(&bytes[..8], b"FGEI\x01\x00\x00\x00");
		let decoded = decode_payload(&bytes, 16 * 1024 * 1024).assert_ok();
		assert_eq!(decoded.definition, event);
		assert_eq!(decoded.arguments, arguments);
		assert_eq!(decoded.functions, functions);
		for length in 0..bytes.len() {
			assert!(decode_payload(&bytes[..length], 16 * 1024 * 1024).is_err());
		}
		for offset in [0, 4, 6] {
			let mut invalid = bytes.clone();
			invalid[offset] = 255;
			assert!(decode_payload(&invalid, 16 * 1024 * 1024).is_err());
		}
		assert!(decode_payload(&bytes, bytes.len()).is_err());
	}

	/// Verifies event payload function comments use owned decode bytes.
	#[test]
	fn event_payload_function_comments_use_owned_decode_bytes() {
		use crate::test_assertions::AssertResult as _;

		let mut event = EventDefinition::new(
			"commented",
			EventTarget::Class(crate::datastore::ClassId::from_storage(1).assert_ok()),
			"true",
			"1",
		)
		.assert_ok()
		.with_execution(EventExecution::Async(EventAsyncOptions::default()));
		event.id = Some(EventId::from_storage(1000).assert_ok());
		let mut functions = Vec::new();
		for index in 2..=31 {
			let mut function = SchemaFunction::new(
				"fn",
				format!("f{index}"),
				Vec::new(),
				Type::Int,
				FunctionEffect::Pure,
				false,
				None,
				"{ RETURN 1; }",
			)
			.assert_ok()
			.with_comment("c".repeat(4096));
			function.id = Some(FunctionId::from_storage(index).assert_ok());
			functions.push(Arc::new(function));
		}
		let arguments = [Value::from("CREATE"), Value::Null, Value::Null, Value::Null, Value::Null];
		let bytes = encode_payload(&event, &arguments, &functions, 16 * 1024 * 1024).assert_ok();
		let decoded = decode_payload(&bytes, 16 * 1024 * 1024).assert_ok();
		assert_eq!(decoded.functions, functions);
		assert!(decode_payload(&bytes, bytes.len()).is_err());
	}
}
