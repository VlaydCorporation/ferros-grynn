//! Scalar bytes stay canonical; qualified captures have an explicit owned
//! envelope.

use std::ops::Bound;

use super::{
	CodecBudget,
	Decoder,
	EncodeContext,
	FerrosGrynnResult,
	MAX_NODES,
	Value,
	Work,
	append,
	invalid,
	values,
};
use crate::storage::history_codec::logical_values;

/// Detects qualified compounds within the same descriptor depth/work limits.
fn qualified(value: &Value, depth: usize, work: &mut Work<'_>) -> FerrosGrynnResult<bool> {
	work.take(depth, 1)?;
	let mut found = matches!(value, Value::AtomOccurrence(_) | Value::Path(_));
	match value {
		Value::Array(values) => {
			for child in values {
				found |= qualified(child, depth + 1, work)?;
			}
		}
		Value::Set(values) => {
			for child in values {
				found |= qualified(child, depth + 1, work)?;
			}
		}
		Value::Object(values) => {
			for child in values.values() {
				found |= qualified(child, depth + 1, work)?;
			}
		}
		Value::Range(range) => {
			for child in [range.start(), range.end()] {
				if let Bound::Included(child) | Bound::Excluded(child) = child {
					found |= qualified(child, depth + 1, work)?;
				}
			}
		}
		_ => {}
	}
	Ok(found)
}

/// Encodes source identities without admitting runtime graph or closure
/// captures.
pub(super) fn encode(
	value: &Value,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	observe: Option<&dyn Fn(u64) -> FerrosGrynnResult<()>>,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut scan = Work {
		observe,
		remaining: MAX_NODES,
	};
	if !qualified(value, 0, &mut scan)? {
		return values::encode_observed(value, context, budget, observe);
	}
	let mut output = Vec::new();
	append(&mut output, b"FGLV\x01\x00\x00\x00", budget)?;
	let mut work = logical_values::Work {
		remaining: MAX_NODES,
	};
	logical_values::encode(&mut output, value, context, budget, &mut work, 0)?;
	if let Some(observe) = observe {
		observe((MAX_NODES - work.remaining) as u64)?;
	}
	Ok(output)
}

/// Resolves only this descriptor's self-contained historical dictionaries.
pub(super) fn decode(
	bytes: &[u8],
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Value> {
	if !bytes.starts_with(b"FGLV") {
		return values::decode(bytes, context, budget);
	}
	let mut input = Decoder::new(bytes);
	input.take(4)?;
	if input.take(2)? != 1_u16.to_le_bytes() {
		return Err(invalid("LIVE owned value version is invalid"));
	}
	input.zero(2)?;
	let mut work = logical_values::Work {
		remaining: MAX_NODES,
	};
	let value = logical_values::decode(&mut input, context, budget, &mut work, 0)?;
	if input.position != bytes.len() {
		return Err(invalid("LIVE owned value has trailing bytes"));
	}
	Ok(value)
}

#[cfg(test)]
mod tests {
	use super::{super::*, decode, encode, *};
	use crate::graph::{
		AtomKind,
		AtomOccurrenceRef,
		AtomRef,
		GraphId,
		GraphKind,
		GraphRef,
		GraphStorageMode,
	};

	#[test]
	fn graph_result_qualified_literals_and_frozen_parameters_roundtrip() -> FerrosGrynnResult<()> {
		let graph = GraphRef::new(
			GraphId::new(7).ok_or_else(|| invalid("fixture graph ID"))?,
			"source",
			GraphKind::Graph,
			GraphStorageMode::InMemory,
		)?;
		let value = Value::from(AtomOccurrenceRef::new(
			AtomRef::new(AtomKind::Node, "person:one".parse()?),
			std::sync::Arc::new(graph),
		));
		let query = LiveQuery::new(LiveSource::Class(
			ClassId::new(1).ok_or_else(|| invalid("fixture class ID"))?,
		))
		.with_parameters(Object::from_iter([("picked", Value::Array(vec![value.clone()].into()))]))
		.with_graph_result(crate::live::LiveGraphResult::new(
			LiveExpr::Literal(value),
			LiveExpr::Array(Vec::new()),
		));
		let descriptor = LiveDescriptor::Query(query);
		let bytes = super::super::encode(&descriptor, 1 << 20)?;
		assert_eq!(super::super::decode(&bytes, 1 << 20)?, descriptor);
		let mut context = EncodeContext::default();
		let mut budget = CodecBudget::new(1 << 20, 0)?;
		let LiveDescriptor::Query(query) = descriptor else {
			return Err(invalid("fixture query"));
		};
		let value = query.parameters().get("picked").ok_or_else(|| invalid("fixture parameter"))?;
		let good = encode(value, &mut context, &mut budget, None)?;
		assert_eq!(&good[..8], b"FGLV\x01\x00\x00\x00");
		for offset in [4, 6, 8] {
			let mut bad = good.clone();
			bad[offset] = 255;
			assert!(decode(&bad, &context, &mut CodecBudget::new(1 << 20, 0)?).is_err());
		}
		assert!(decode(&good, &context, &mut CodecBudget::new(1, 0)?).is_err());
		Ok(())
	}
}
