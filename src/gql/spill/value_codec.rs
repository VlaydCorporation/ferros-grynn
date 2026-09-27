//! Versioned transient value codec for query-run records.

use std::{ops::Bound, sync::Arc};

use super::invalid_run;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomOccurrenceRef, EndpointRole, PathDirection, PathStep, SnapshotId},
	values::{
		Array,
		Object,
		Path,
		Range,
		Set,
		Value,
		storage::{
			EncodeContext,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

const MAGIC: &[u8; 5] = b"FGQV\x01";
const TAG_CANONICAL: u8 = 0;
const TAG_OCCURRENCE: u8 = 1;
const TAG_PATH: u8 = 2;
const TAG_ARRAY: u8 = 3;
const TAG_SET: u8 = 4;
const TAG_OBJECT: u8 = 5;
const TAG_RANGE: u8 = 6;
const MAX_DEPTH: usize = 64;

pub(super) fn encode_query_value(
	value: Value,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut output = Vec::new();
	output.extend_from_slice(MAGIC);
	encode_value(&mut output, value, context, 0)?;
	Ok(output)
}

pub(super) fn decode_query_value(
	bytes: &[u8],
	context: &EncodeContext,
) -> FerrosGrynnResult<Value> {
	let Some(payload) = bytes.strip_prefix(MAGIC) else {
		return Err(invalid_run("query-run value codec magic/version is invalid"));
	};
	let mut decoder = Decoder {
		bytes: payload,
		position: 0,
		allocation: 0,
		limits: DecodeLimits::default(),
	};
	let value = decoder.decode_value(context, 0)?;
	if decoder.position != payload.len() {
		return Err(invalid_run("query-run value contains trailing bytes"));
	}
	Ok(value)
}

fn encode_value(
	output: &mut Vec<u8>,
	value: Value,
	context: &mut EncodeContext,
	depth: usize,
) -> FerrosGrynnResult<()> {
	if depth > MAX_DEPTH {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"query value depth",
			MAX_DEPTH as u64,
			depth as u64,
		));
	}
	if value.is_storable() {
		output.push(TAG_CANONICAL);
		let storage = StorageValue::from_runtime(value, context)?;
		let bytes = CanonicalCodec::encode(storage.as_ref())?;
		push_bytes(output, &bytes)?;
		return Ok(());
	}
	match value {
		Value::AtomOccurrence(occurrence) => {
			output.push(TAG_OCCURRENCE);
			encode_occurrence(output, *occurrence, context, depth + 1)
		}
		Value::Path(path) => {
			output.push(TAG_PATH);
			encode_path(output, &path, context, depth + 1)
		}
		Value::Array(values) => {
			output.push(TAG_ARRAY);
			push_len(output, values.len())?;
			for value in values {
				encode_value(output, value, context, depth + 1)?;
			}
			Ok(())
		}
		Value::Set(values) => {
			output.push(TAG_SET);
			push_len(output, values.len())?;
			for value in values {
				encode_value(output, value, context, depth + 1)?;
			}
			Ok(())
		}
		Value::Object(values) => {
			output.push(TAG_OBJECT);
			push_len(output, values.len())?;
			for (name, value) in values {
				push_bytes(output, name.as_str().as_bytes())?;
				encode_value(output, value, context, depth + 1)?;
			}
			Ok(())
		}
		Value::Range(range) => {
			output.push(TAG_RANGE);
			encode_bound(output, range.start().cloned(), context, depth + 1)?;
			encode_bound(output, range.end().cloned(), context, depth + 1)
		}
		value => Err(FerrosGrynnError::query_value_not_spillable(value.kind().to_string())),
	}
}

fn encode_occurrence(
	output: &mut Vec<u8>,
	occurrence: AtomOccurrenceRef,
	context: &mut EncodeContext,
	depth: usize,
) -> FerrosGrynnResult<()> {
	let (atom, graph) = occurrence.into_parts();
	encode_value(output, Value::Atom(Box::new(atom)), context, depth)?;
	encode_value(output, Value::GraphRef(Box::new(graph.as_ref().clone())), context, depth)
}

fn encode_path(
	output: &mut Vec<u8>,
	path: &Path,
	context: &mut EncodeContext,
	depth: usize,
) -> FerrosGrynnResult<()> {
	encode_value(output, Value::Uuid(path.origin()), context, depth)?;
	output.extend_from_slice(&path.snapshot().timestamp().to_le_bytes());
	output.extend_from_slice(&path.snapshot().transaction().to_le_bytes());
	match path.cost() {
		Some(cost) => {
			output.push(1);
			encode_value(output, Value::Number(cost), context, depth)?;
		}
		None => output.push(0),
	}
	push_len(output, path.steps().len())?;
	for step in path.steps() {
		encode_value(output, Value::AtomOccurrence(Box::new(step.atom().clone())), context, depth)?;
		match step.via_edge() {
			Some(edge) => {
				output.push(1);
				encode_value(
					output,
					Value::AtomOccurrence(Box::new(edge.clone())),
					context,
					depth,
				)?;
			}
			None => output.push(0),
		}
		output.push(direction_tag(step.direction()));
		output.push(role_tag(step.role()));
	}
	Ok(())
}

fn encode_bound(
	output: &mut Vec<u8>,
	bound: Bound<Value>,
	context: &mut EncodeContext,
	depth: usize,
) -> FerrosGrynnResult<()> {
	match bound {
		Bound::Unbounded => output.push(0),
		Bound::Included(value) => {
			output.push(1);
			encode_value(output, value, context, depth)?;
		}
		Bound::Excluded(value) => {
			output.push(2);
			encode_value(output, value, context, depth)?;
		}
	}
	Ok(())
}

fn push_len(output: &mut Vec<u8>, length: usize) -> FerrosGrynnResult<()> {
	let length =
		u32::try_from(length).map_err(|_error| invalid_run("query-run collection exceeds u32"))?;
	output.extend_from_slice(&length.to_le_bytes());
	Ok(())
}

fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> FerrosGrynnResult<()> {
	push_len(output, bytes.len())?;
	output.extend_from_slice(bytes);
	Ok(())
}

const fn direction_tag(direction: PathDirection) -> u8 {
	match direction {
		PathDirection::Forward => 0,
		PathDirection::Reverse => 1,
		PathDirection::Undirected => 2,
	}
}

const fn role_tag(role: EndpointRole) -> u8 {
	match role {
		EndpointRole::Source => 0,
		EndpointRole::Target => 1,
		EndpointRole::Undirected => 2,
	}
}

struct Decoder<'a> {
	bytes: &'a [u8],
	position: usize,
	allocation: usize,
	limits: DecodeLimits,
}

impl Decoder<'_> {
	fn decode_value(&mut self, context: &EncodeContext, depth: usize) -> FerrosGrynnResult<Value> {
		if depth > self.limits.max_depth {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"query value depth",
				self.limits.max_depth as u64,
				depth as u64,
			));
		}
		match self.byte()? {
			TAG_CANONICAL => self.decode_canonical(context),
			TAG_OCCURRENCE => self.decode_occurrence(context, depth + 1).map(Value::from),
			TAG_PATH => self.decode_path(context, depth + 1).map(Value::from),
			TAG_ARRAY => {
				let length = self.collection_len()?;
				let mut values = Vec::with_capacity(length);
				for _ in 0..length {
					values.push(self.decode_value(context, depth + 1)?);
				}
				Ok(Value::Array(Array::new(values)))
			}
			TAG_SET => {
				let length = self.collection_len()?;
				let mut values = Set::new();
				for _ in 0..length {
					if !values.insert(self.decode_value(context, depth + 1)?) {
						return Err(invalid_run("query-run set contains a duplicate value"));
					}
				}
				Ok(Value::Set(values))
			}
			TAG_OBJECT => {
				let length = self.collection_len()?;
				let mut values = Object::new();
				for _ in 0..length {
					let name = self.string()?;
					let value = self.decode_value(context, depth + 1)?;
					if values.insert(name.as_str(), value).is_some() {
						return Err(invalid_run("query-run object contains a duplicate field"));
					}
				}
				Ok(Value::Object(values))
			}
			TAG_RANGE => {
				let start = self.decode_bound(context, depth + 1)?;
				let end = self.decode_bound(context, depth + 1)?;
				Range::new(start, end).map(Value::Range)
			}
			_tag => Err(invalid_run("query-run value tag is invalid")),
		}
	}

	fn decode_canonical(&mut self, context: &EncodeContext) -> FerrosGrynnResult<Value> {
		let limits = self.limits;
		let bytes = self.bytes()?;
		CanonicalCodec::decode(bytes, limits)?.into_runtime(&context.decoder())
	}

	fn decode_occurrence(
		&mut self,
		context: &EncodeContext,
		depth: usize,
	) -> FerrosGrynnResult<AtomOccurrenceRef> {
		let Value::Atom(atom) = self.decode_value(context, depth)? else {
			return Err(invalid_run("query-run occurrence atom is invalid"));
		};
		let Value::GraphRef(graph) = self.decode_value(context, depth)? else {
			return Err(invalid_run("query-run occurrence graph is invalid"));
		};
		Ok(AtomOccurrenceRef::new(*atom, Arc::new(*graph)))
	}

	fn decode_path(&mut self, context: &EncodeContext, depth: usize) -> FerrosGrynnResult<Path> {
		let Value::Uuid(origin) = self.decode_value(context, depth)? else {
			return Err(invalid_run("query-run path origin is invalid"));
		};
		let snapshot = SnapshotId::new(self.u64()?, self.u64()?);
		let cost = match self.byte()? {
			0 => None,
			1 => {
				let Value::Number(value) = self.decode_value(context, depth)? else {
					return Err(invalid_run("query-run path cost is invalid"));
				};
				Some(value)
			}
			_ => return Err(invalid_run("query-run path cost flag is invalid")),
		};
		let length = self.collection_len()?;
		let mut steps = Vec::with_capacity(length);
		for _ in 0..length {
			let Value::AtomOccurrence(atom) = self.decode_value(context, depth)? else {
				return Err(invalid_run("query-run path atom is invalid"));
			};
			let via_edge = match self.byte()? {
				0 => None,
				1 => {
					let Value::AtomOccurrence(edge) = self.decode_value(context, depth)? else {
						return Err(invalid_run("query-run path edge is invalid"));
					};
					Some(*edge)
				}
				_ => return Err(invalid_run("query-run path edge flag is invalid")),
			};
			let direction = match self.byte()? {
				0 => PathDirection::Forward,
				1 => PathDirection::Reverse,
				2 => PathDirection::Undirected,
				_ => return Err(invalid_run("query-run path direction is invalid")),
			};
			let role = match self.byte()? {
				0 => EndpointRole::Source,
				1 => EndpointRole::Target,
				2 => EndpointRole::Undirected,
				_ => return Err(invalid_run("query-run path role is invalid")),
			};
			steps.push(PathStep::new(*atom, via_edge, direction, role));
		}
		Ok(Path::new(origin, snapshot, steps, cost))
	}

	fn decode_bound(
		&mut self,
		context: &EncodeContext,
		depth: usize,
	) -> FerrosGrynnResult<Bound<Value>> {
		match self.byte()? {
			0 => Ok(Bound::Unbounded),
			1 => self.decode_value(context, depth).map(Bound::Included),
			2 => self.decode_value(context, depth).map(Bound::Excluded),
			_ => Err(invalid_run("query-run range bound is invalid")),
		}
	}

	fn collection_len(&mut self) -> FerrosGrynnResult<usize> {
		let length = self.u32()? as usize;
		if length > self.limits.max_collection_len {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"query collection length",
				self.limits.max_collection_len as u64,
				length as u64,
			));
		}
		self.account(length.saturating_mul(std::mem::size_of::<Value>()))?;
		Ok(length)
	}

	fn string(&mut self) -> FerrosGrynnResult<String> {
		let length = self.u32()? as usize;
		if length > self.limits.max_string_bytes {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"query string bytes",
				self.limits.max_string_bytes as u64,
				length as u64,
			));
		}
		self.account(length)?;
		std::str::from_utf8(self.take(length)?)
			.map(str::to_owned)
			.map_err(|_error| invalid_run("query-run object field is not UTF-8"))
	}

	fn bytes(&mut self) -> FerrosGrynnResult<&[u8]> {
		let length = self.u32()? as usize;
		if length > self.limits.max_blob_bytes {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"query value bytes",
				self.limits.max_blob_bytes as u64,
				length as u64,
			));
		}
		self.take(length)
	}

	fn account(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		self.allocation = self
			.allocation
			.checked_add(bytes)
			.ok_or_else(|| invalid_run("query-run allocation accounting overflow"))?;
		if self.allocation > self.limits.max_total_allocation {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"query value allocation",
				self.limits.max_total_allocation as u64,
				self.allocation as u64,
			));
		}
		Ok(())
	}

	fn byte(&mut self) -> FerrosGrynnResult<u8> {
		Ok(self.take(1)?[0])
	}

	fn u32(&mut self) -> FerrosGrynnResult<u32> {
		let bytes = self.take(4)?;
		Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
	}

	fn u64(&mut self) -> FerrosGrynnResult<u64> {
		let bytes = self.take(8)?;
		Ok(u64::from_le_bytes([
			bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
		]))
	}

	fn take(&mut self, length: usize) -> FerrosGrynnResult<&[u8]> {
		let end = self
			.position
			.checked_add(length)
			.filter(|end| *end <= self.bytes.len())
			.ok_or_else(|| invalid_run("query-run value is truncated"))?;
		let bytes = &self.bytes[self.position..end];
		self.position = end;
		Ok(bytes)
	}
}
