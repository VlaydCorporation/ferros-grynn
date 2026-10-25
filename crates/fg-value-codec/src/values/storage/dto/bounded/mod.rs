//! Borrowed conversion keeps runtime maps and identities out of the temporary
//! tree.

use std::ops::Bound;

use fg_runtime::__internal::{GeometryInternal as _, TextInternal as _};

use super::{
	StorageAtomRef,
	StorageBound,
	StorageDateTime,
	StorageDecimal,
	StorageDuration,
	StorageRange,
	StorageRecordId,
	StorageValue,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		Bytes,
		Number,
		PropertyKeyId,
		PropertyMap,
		RecordId,
		Text,
		Value,
		Vector,
		storage::{
			EncodeContext,
			canonical_f64,
			codec::{self, CanonicalCodec, DecodeAdmission, DecodeLimits},
			encode_budget::EncodeBudget,
		},
	},
};

/// Bounded scalar keys preserve format framing without recursive buffers.
mod order_key;

/// Restricted RecordId helpers preserve the decoder's extra container frames.
mod record;

/// One writer owns the temporary tree; only context growth survives its return.
struct Converter<'a> {
	/// Prepared property/class/graph authorities; new time-zone names may
	/// append.
	context: &'a mut EncodeContext,
	/// One operation's retained backing and sequential scratch.
	budget: EncodeBudget,
	/// The normal cold reader must be able to materialize the resulting value.
	limits: DecodeLimits,
	/// Immutable length/depth policy; allocation is checked in final DTO order.
	policy: DecodeAdmission,
}

impl StorageValue {
	/// Primes resolvers from a borrowed value without a final encoded payload.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved prepared keys and reader-policy
	/// excess. Copies and resolver growth are admitted before allocation; the
	/// caller must discard its private context after any failure.
	pub fn prime_context_bounded(
		value: &Value,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<usize> {
		let mut converter = Converter::new(context, maximum, DecodeLimits::default());
		let value = converter.value(value, 0)?;
		CanonicalCodec::check_decode_limits(value.as_ref(), converter.limits)?;
		drop(value);
		Ok(converter.budget.context_bytes())
	}

	/// Encodes one borrowed root value without cloning its runtime payload.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved prepared keys and resource excess
	/// before their copies; callers abandon the private context after failure.
	pub fn encode_value_bounded(
		value: &Value,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<u8>, usize)> {
		let mut converter = Converter::new(context, maximum, DecodeLimits::default());
		let value = converter.value(value, 0)?;
		converter.finish(value)
	}

	/// Encodes borrowed properties within an operation envelope, retaining no
	/// DTO.
	///
	/// # Errors
	/// Rejects unsupported values, unresolved prepared keys and resource excess
	/// before their copies; callers abandon the private context after failure.
	pub fn encode_properties_bounded(
		properties: &PropertyMap,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<u8>, usize)> {
		let mut converter = Converter::new(context, maximum, DecodeLimits::default());
		converter.policy.check_collection_len(properties.len())?;
		let mut values = converter.budget.vector::<(PropertyKeyId, Self)>(properties.len())?;
		for (key, value) in properties {
			values.push((key, converter.value(value, 1)?));
		}
		converter.finish(Self::Object(values))
	}

	/// Encodes an identity without cloning its runtime class or composite tree.
	///
	/// # Errors
	/// Returns class-resolution, canonical codec and preallocation limit
	/// errors.
	pub fn encode_record_bounded(
		record: &RecordId,
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<u8>, usize)> {
		let mut converter = Converter::new(context, maximum, DecodeLimits::default());
		let record = converter.record(record, 1)?;
		converter.finish(Self::Record(record))
	}
}

impl<'a> Converter<'a> {
	/// Borrows input authorities and starts an independent record envelope.
	fn new(context: &'a mut EncodeContext, maximum: usize, limits: DecodeLimits) -> Self {
		Self {
			context,
			budget: EncodeBudget::new(maximum),
			limits,
			policy: DecodeAdmission::new(limits),
		}
	}

	/// Checks the final canonical traversal, then reserves output beside the
	/// DTO.
	fn finish(self, value: StorageValue) -> FerrosGrynnResult<(Vec<u8>, usize)> {
		CanonicalCodec::check_decode_limits(value.as_ref(), self.limits)?;
		let bytes = CanonicalCodec::encode_bounded(value.as_ref(), self.budget.remaining())?;
		drop(value);
		Ok((bytes, self.budget.context_bytes()))
	}

	/// Preserves zero-copy static Text clones and bounds heap copies by their
	/// owner.
	fn text(&mut self, value: &Text) -> FerrosGrynnResult<Text> {
		DecodeAdmission::check_length("string bytes", self.limits.max_string_bytes, value.len())?;
		if value.estimated_clone_heap_bytes() != 0 {
			self.budget.retain(Text::estimated_copy_heap_bytes(value.len()))?;
		}
		Ok(value.clone())
	}

	/// Creates only exact typed DTO backing after checking each entered reader
	/// frame.
	fn value(&mut self, value: &Value, depth: usize) -> FerrosGrynnResult<StorageValue> {
		self.policy.check_depth(depth)?;
		Ok(match value {
			Value::None => StorageValue::None,
			Value::Null => StorageValue::Null,
			Value::Bool(value) => StorageValue::Bool(*value),
			Value::Number(Number::Int(value)) => StorageValue::Int(*value),
			Value::Number(Number::Float(value)) => StorageValue::Float(canonical_f64(*value)),
			Value::Number(Number::Decimal(value)) => {
				StorageValue::Decimal(StorageDecimal::from_runtime(*value))
			}
			Value::String(value) => StorageValue::String(self.text(value)?),
			Value::Bytes(value) => {
				DecodeAdmission::check_length(
					"blob bytes",
					self.limits.max_blob_bytes,
					value.len(),
				)?;
				self.budget.retain(value.len())?;
				StorageValue::Bytes(Bytes::from(value.as_slice()))
			}
			Value::DateTime(value) => {
				let zoned = value.as_inner();
				let zone = self.context.intern_time_zone_bounded(zoned, &mut self.budget)?;
				StorageValue::DateTime(StorageDateTime::from_timestamp(zoned.timestamp(), zone))
			}
			Value::Duration(value) => StorageValue::Duration(StorageDuration::from_runtime(*value)),
			Value::Uuid(value) => StorageValue::Uuid(*value),
			Value::Ulid(value) => StorageValue::Ulid(*value),
			Value::Record(value) => StorageValue::Record(self.record(value, depth + 1)?),
			Value::Atom(value) => StorageValue::AtomRef(StorageAtomRef {
				kind: value.kind(),
				record_id: self.record(value.id(), depth + 1)?,
			}),
			Value::GraphRef(value) => StorageValue::GraphRef(self.context.graph_to_disk(value)?),
			Value::Array(values) => {
				self.policy.check_collection_len(values.len())?;
				let mut result = self.budget.vector::<StorageValue>(values.len())?;
				for value in values {
					result.push(self.value(value, depth + 1)?);
				}
				StorageValue::Array(result)
			}
			Value::Set(values) => {
				self.policy.check_collection_len(values.len())?;
				let mut result = self.budget.vector::<StorageValue>(values.len())?;
				for value in values {
					result.push(self.value(value, depth + 1)?);
				}
				self.canonicalize_set(&mut result)?;
				StorageValue::Set(result)
			}
			Value::Object(values) => {
				self.policy.check_collection_len(values.len())?;
				let mut result =
					self.budget.vector::<(PropertyKeyId, StorageValue)>(values.len())?;
				for (name, value) in values {
					let key = self.context.properties.get(name.as_str()).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_value_encoding(
							0,
							"prepared property dictionary is missing a key",
						)
					})?;
					result.push((key, self.value(value, depth + 1)?));
				}
				result.sort_unstable_by_key(|(key, _)| *key);
				StorageValue::Object(result)
			}
			Value::Range(value) => StorageValue::Range(StorageRange {
				start: self.bound(value.start(), depth + 1)?,
				end: self.bound(value.end(), depth + 1)?,
			}),
			Value::Geometry(value) => {
				self.budget.retain(codec::geometry_allocation(value, depth, self.limits)?)?;
				// Source spare capacity is excluded; validate the admitted
				// clone.
				let value = value.clone();
				self.budget.scratch(value.validation_scratch_bytes()?)?;
				value.validate()?;
				StorageValue::Geometry(value)
			}
			Value::Vector(value) => {
				self.policy.check_collection_len(value.len())?;
				u16::try_from(value.len()).map_err(|_error| {
					FerrosGrynnError::invalid_storage_value_encoding(
						0,
						"vector dimension exceeds u16",
					)
				})?;
				let bytes = match value {
					Vector::F64(v) => EncodeBudget::array_bytes::<f64>(v.len())?,
					Vector::F32(v) => EncodeBudget::array_bytes::<f32>(v.len())?,
					Vector::F16(v) => EncodeBudget::array_bytes::<half::f16>(v.len())?,
					Vector::I64(v) => EncodeBudget::array_bytes::<i64>(v.len())?,
					Vector::I32(v) => EncodeBudget::array_bytes::<i32>(v.len())?,
					Vector::I16(v) => EncodeBudget::array_bytes::<i16>(v.len())?,
					Vector::I8(v) => v.len(),
					Vector::U8(v) => v.len(),

					_ => {
						return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"Unsupported runtime variant at the workspace boundary.",
						));
					}
				};
				self.budget.retain(bytes)?;
				StorageValue::Vector(value.clone())
			}
			Value::Regex(_)
			| Value::Matcher(_)
			| Value::AtomOccurrence(_)
			| Value::Path(_)
			| Value::Graph(_)
			| Value::Closure(_)
			| Value::Error(_)
			| Value::Cursor(_) => {
				return Err(FerrosGrynnError::unsupported_storage_value(value.kind().to_string()));
			}

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported runtime variant at the workspace boundary.",
				));
			}
		})
	}

	/// Unbounded endpoints do not enter an otherwise unused child frame.
	fn bound(&mut self, value: Bound<&Value>, depth: usize) -> FerrosGrynnResult<StorageBound> {
		let included = matches!(value, Bound::Included(_));
		match value {
			Bound::Unbounded => Ok(StorageBound::Unbounded),
			Bound::Included(value) | Bound::Excluded(value) => {
				self.budget.retain(size_of::<StorageValue>())?;
				let child = Box::new(self.value(value, depth)?);
				Ok(if included {
					StorageBound::Included(child)
				} else {
					StorageBound::Excluded(child)
				})
			}
		}
	}

	/// Keys, typed tuples and child encoder scratch overlap with the source DTO
	/// Vec.
	fn canonicalize_set(&self, values: &mut Vec<StorageValue>) -> FerrosGrynnResult<()> {
		let mut bytes = EncodeBudget::array_bytes::<(Vec<u8>, StorageValue)>(values.len())?;
		let mut scratch = 0;
		for value in values.iter() {
			let layout = CanonicalCodec::encoding_layout(value.as_ref())?;
			bytes = EncodeBudget::add(bytes, layout.encoded_len)?;
			scratch = scratch.max(layout.scratch_bytes);
		}
		self.budget.scratch(EncodeBudget::add(bytes, scratch)?)?;
		let mut keyed = Vec::new();
		keyed.try_reserve_exact(values.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_value_encoding(0, "Set key allocation failed")
		})?;
		for value in values.drain(..) {
			let key =
				CanonicalCodec::encode_bounded(value.as_ref(), EncodeBudget::add(bytes, scratch)?)?;
			keyed.push((key, value));
		}
		codec::finish_set(values, keyed)
	}
}

#[cfg(test)]
/// Differential bytes and operation resource boundaries.
mod tests;
