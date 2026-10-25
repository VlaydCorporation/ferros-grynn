//! One bounded order-key output, without recursive child buffers.

use super::{Converter, EncodeBudget, FerrosGrynnError, FerrosGrynnResult, StorageValue};
use crate::values::{
	Type,
	Value,
	storage::{
		EncodeContext,
		StorageBound,
		StorageRecordIdPart,
		StorageRecordValue,
		codec::DecodeLimits,
		order_key::{
			encode_decimal,
			encode_duration,
			encode_float,
			ordered_i64,
			validate_index_type,
		},
	},
};

/// Count and write passes share the same framing and immutable prepared DTOs.
struct KeyWriter {
	/// Absent during preflight; the write pass owns one exact admitted buffer.
	output: Option<Vec<u8>>,
	/// Checked byte count, including every active level of zero escaping.
	length: usize,
	/// Applying zero escaping repeatedly adds one FF for each active level.
	escapes: usize,
}

impl StorageValue {
	/// Encodes scalar index components within the caller's remaining envelope.
	///
	/// # Errors
	/// Rejects unsupported types, unresolved identities, invalid values and
	/// resource excess before DTO/output allocation. Failed contexts are
	/// private.
	pub(crate) fn encode_index_key_bounded(
		components: &[(&Value, &Type)],
		context: &mut EncodeContext,
		maximum: usize,
	) -> FerrosGrynnResult<(Vec<u8>, usize)> {
		if components.is_empty() {
			return Err(FerrosGrynnError::invalid_argument(
				"a scalar index requires at least one field",
			));
		}
		let mut converter = Converter::new(context, maximum, DecodeLimits::default());
		let mut values = converter.budget.vector::<Self>(components.len())?;
		for (value, value_type) in components {
			validate_index_type(value_type)?;
			values.push(converter.value(value, 0)?);
		}
		let mut count = KeyWriter {
			output: None,
			length: 0,
			escapes: 0,
		};
		count.components(&values, components, converter.context)?;
		converter.budget.scratch(count.length)?;
		let mut output = Vec::new();
		output.try_reserve_exact(count.length).map_err(|source| {
			FerrosGrynnError::invalid_storage_value_encoding(0, "scalar key allocation failed")
				.with_technical_source(source)
		})?;
		let mut writer = KeyWriter {
			output: Some(output),
			length: 0,
			escapes: 0,
		};
		writer.components(&values, components, converter.context)?;
		if writer.length != count.length {
			return Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"scalar key count/write mismatch",
			));
		}
		let output = writer.output.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_value_encoding(0, "scalar writer has no output")
		})?;
		Ok((output, converter.budget.context_bytes()))
	}
}

impl KeyWriter {
	/// Only nonterminal composite components receive an outer separator.
	fn components(
		&mut self,
		values: &[StorageValue],
		types: &[(&Value, &Type)],
		context: &EncodeContext,
	) -> FerrosGrynnResult<()> {
		for (index, (value, (_, value_type))) in values.iter().zip(types).enumerate() {
			if index + 1 == values.len() {
				self.typed(value, value_type, context)?;
			} else {
				self.escaped(|writer| writer.typed(value, value_type, context))?;
				self.bytes(&[0, 0])?;
			}
		}
		Ok(())
	}

	/// Each zero becomes 00 followed by one FF per enclosing escape layer.
	fn bytes(&mut self, bytes: &[u8]) -> FerrosGrynnResult<()> {
		let escaped = bytes
			.iter()
			.filter(|byte| **byte == 0)
			.count()
			.checked_mul(self.escapes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("scalar key escaping"))?;
		let additional = EncodeBudget::add(bytes.len(), escaped)?;
		self.length = EncodeBudget::add(self.length, additional)?;
		if let Some(output) = &mut self.output {
			if self.length > output.capacity() {
				return Err(FerrosGrynnError::invalid_storage_value_encoding(
					0,
					"scalar key count/write mismatch",
				));
			}
			for byte in bytes {
				output.push(*byte);
				if *byte == 0 {
					output.extend(std::iter::repeat_n(0xff, self.escapes));
				}
			}
		}
		Ok(())
	}

	/// Child framing stays at its caller's level, including on error.
	fn escaped(
		&mut self,
		action: impl FnOnce(&mut Self) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		self.escapes = EncodeBudget::add(self.escapes, 1)?;
		let result = action(self);
		self.escapes -= 1;
		result
	}

	/// Presence precedes the same scalar body used by the format-v1 control.
	fn typed(
		&mut self,
		value: &StorageValue,
		value_type: &Type,
		context: &EncodeContext,
	) -> FerrosGrynnResult<()> {
		match value {
			StorageValue::None => return self.bytes(&[0]),
			StorageValue::Null => return self.bytes(&[1]),
			_ => {}
		}
		if let Type::Option(inner) = value_type {
			return self.typed(value, inner, context);
		}
		self.bytes(&[2])?;
		match (value, value_type) {
			(StorageValue::Bool(value), Type::Bool) => self.bytes(&[u8::from(*value)]),
			(StorageValue::Int(value), Type::Int) => self.bytes(&ordered_i64(*value)),
			(StorageValue::Float(bits), Type::Float) => self.bytes(&encode_float(*bits)?),
			(StorageValue::Decimal(value), Type::Decimal) => {
				self.bytes(&encode_decimal(value.mantissa, value.scale)?)
			}
			(StorageValue::String(value), Type::String) => {
				self.bytes(value.as_bytes())?;
				self.bytes(&[0])
			}
			(StorageValue::Bytes(value), Type::Bytes) => {
				self.bytes(value.as_slice())?;
				self.bytes(&[0])
			}
			(StorageValue::DateTime(value), Type::DateTime) => {
				if value.nanos >= 1_000_000_000 {
					return Err(FerrosGrynnError::invalid_argument(
						"datetime nanoseconds exceed one second",
					));
				}
				self.bytes(&ordered_i64(value.seconds))?;
				self.bytes(&value.nanos.to_be_bytes())
			}
			(StorageValue::Duration(value), Type::Duration) => self.bytes(&encode_duration(value)?),
			(StorageValue::Uuid(value), Type::Uuid) => self.bytes(value.as_inner().as_bytes()),
			(StorageValue::Ulid(value), Type::Ulid) => self.bytes(&value.as_inner().to_bytes()),
			(StorageValue::Record(value), Type::Record) => {
				self.record(value.class_id, &value.part, context)
			}
			(StorageValue::AtomRef(value), Type::Atom(expected)) if value.kind() == *expected => {
				self.record(value.record_id.class_id, &value.record_id.part, context)
			}
			(StorageValue::GraphRef(value), Type::GraphRef(expected)) => {
				let runtime = context.registered_graph(*value).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"indexed graph reference is absent from its conversion context",
					)
				})?;
				if runtime.kind() != *expected {
					return Err(FerrosGrynnError::invalid_argument(
						"graph reference does not match indexed field type",
					));
				}
				self.bytes(&value.raw().to_be_bytes())
			}
			(StorageValue::Array(values), Type::Array(element))
			| (StorageValue::Set(values), Type::Set(element)) => {
				for value in values {
					self.escaped(|writer| writer.typed(value, element, context))?;
					self.bytes(&[0, 1])?;
				}
				self.bytes(&[0, 0])
			}
			(StorageValue::Range(value), Type::Range(element)) => {
				self.bound(&value.start, element, context, true)?;
				self.bound(&value.end, element, context, false)
			}
			_ => Err(FerrosGrynnError::invalid_argument("value does not match indexed field type")),
		}
	}

	/// A range endpoint escapes its typed payload independently of inclusivity.
	fn bound(
		&mut self,
		bound: &StorageBound,
		element: &Type,
		context: &EncodeContext,
		lower: bool,
	) -> FerrosGrynnResult<()> {
		let (value, included) = match bound {
			StorageBound::Unbounded => return self.bytes(&[u8::from(!lower)]),
			StorageBound::Included(value) => (value.as_ref(), true),
			StorageBound::Excluded(value) => (value.as_ref(), false),
		};
		self.bytes(&[u8::from(lower)])?;
		self.escaped(|writer| writer.typed(value, element, context))?;
		self.bytes(&[0, u8::from(!included)])
	}

	/// Record ordering uses class names, never dictionary allocation order.
	fn record(
		&mut self,
		class: u32,
		part: &StorageRecordIdPart,
		context: &EncodeContext,
	) -> FerrosGrynnResult<()> {
		let name = context.class_name(class).ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"indexed RecordId class is absent from its dictionary",
			)
		})?;
		self.escaped(|writer| writer.bytes(name.as_bytes()))?;
		self.bytes(&[0, 0])?;
		match part {
			StorageRecordIdPart::Int(value) => self.record_scalar(0, &ordered_i64(*value)),
			StorageRecordIdPart::String(value) => self.record_text(value.as_bytes()),
			StorageRecordIdPart::Uuid(value) => self.record_scalar(2, value.as_inner().as_bytes()),
			StorageRecordIdPart::Ulid(value) => self.record_scalar(3, &value.as_inner().to_bytes()),
			StorageRecordIdPart::Array(values) => self.record_array(values),
			StorageRecordIdPart::Object(values) => self.record_object(values),
		}
	}

	/// Nested record values borrow their own tree instead of cloning a part.
	fn record_value(&mut self, value: &StorageRecordValue) -> FerrosGrynnResult<()> {
		match value {
			StorageRecordValue::Int(value) => self.record_scalar(0, &ordered_i64(*value)),
			StorageRecordValue::String(value) => self.record_text(value.as_bytes()),
			StorageRecordValue::Uuid(value) => self.record_scalar(2, value.as_inner().as_bytes()),
			StorageRecordValue::Ulid(value) => self.record_scalar(3, &value.as_inner().to_bytes()),
			StorageRecordValue::Array(values) => self.record_array(values),
			StorageRecordValue::Object(values) => self.record_object(values),
		}
	}

	/// Fixed-width record bodies share their kind prefix.
	fn record_scalar(&mut self, kind: u8, bytes: &[u8]) -> FerrosGrynnResult<()> {
		self.bytes(&[kind])?;
		self.bytes(bytes)
	}

	/// Strings have their own escaping inside the enclosing record value.
	fn record_text(&mut self, bytes: &[u8]) -> FerrosGrynnResult<()> {
		self.bytes(&[1])?;
		self.escaped(|writer| writer.bytes(bytes))?;
		self.bytes(&[0, 0])
	}

	/// Array delimiters are outside the escaped child payload.
	fn record_array(&mut self, values: &[StorageRecordValue]) -> FerrosGrynnResult<()> {
		self.bytes(&[4])?;
		for value in values {
			self.escaped(|writer| writer.record_value(value))?;
			self.bytes(&[0, 1])?;
		}
		self.bytes(&[0, 0])
	}

	/// Object names retain their storage lexical order and framing.
	fn record_object(
		&mut self,
		values: &[(crate::values::Text, StorageRecordValue)],
	) -> FerrosGrynnResult<()> {
		self.bytes(&[5])?;
		for (name, value) in values {
			self.escaped(|writer| writer.bytes(name.as_bytes()))?;
			self.bytes(&[0, 1])?;
			self.escaped(|writer| writer.record_value(value))?;
			self.bytes(&[0, 1])?;
		}
		self.bytes(&[0, 0])
	}
}

#[cfg(test)]
/// Differential format and operation-budget regression tests.
mod tests;
