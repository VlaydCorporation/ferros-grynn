//! Explicit function source descriptors shared by schema and accepted
//! invocations.

use super::{
	FunctionCatalogEntry,
	FunctionParameterCatalogEntry,
	codec::{
		Decoder,
		invalid,
		read_optional_string,
		read_string,
		write_optional_string,
		write_string,
		write_u16,
		write_u32,
	},
	type_codec,
};
use crate::{
	datastore::{
		FunctionEffect,
		FunctionId,
		SchemaFunction,
		SchemaFunctionParameter,
		SchemaVersion,
	},
	gql_status::FerrosGrynnResult,
	storage::history_codec::CodecBudget,
	values::Type,
};

impl FunctionCatalogEntry {
	/// One authoritative logical-to-storage source projection.
	pub(in crate::storage) fn from_definition(
		function: &SchemaFunction,
	) -> FerrosGrynnResult<Self> {
		Ok(Self {
			id: function.id().ok_or_else(|| invalid("published function has no FunctionId"))?,
			version: function.version(),
			published_at: function.published_at,
			namespace: function.namespace().clone(),
			name: function.name().clone(),
			parameters: function
				.parameters()
				.map(|parameter| FunctionParameterCatalogEntry {
					name: parameter.name().clone(),
					value_type: parameter.value_type().clone(),
				})
				.collect(),
			result: function.result().clone(),
			effect: function.effect(),
			cost: function.is_cost(),
			max_depth: function.max_depth(),
			body: function.body().clone(),
			comment: function.comment().cloned(),
		})
	}

	/// Restores checked source metadata without compiling or executing it.
	pub(in crate::storage) fn into_definition(self) -> FerrosGrynnResult<SchemaFunction> {
		let parameters = self
			.parameters
			.into_iter()
			.map(|parameter| SchemaFunctionParameter::new(parameter.name, parameter.value_type))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut function = SchemaFunction::new(
			self.namespace,
			self.name,
			parameters,
			self.result,
			self.effect,
			self.cost,
			self.max_depth,
			self.body,
		)?;
		function.id = Some(self.id);
		function.version = self.version;
		function.published_at = self.published_at;
		function.comment = self.comment;
		Ok(function)
	}
}

/// Encodes one frozen descriptor after admitting all temporary owned copies.
pub(in crate::storage) fn encode_source(
	function: &SchemaFunction,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut bytes = function
		.body()
		.len()
		.saturating_mul(4)
		.saturating_add(function.name().len())
		.saturating_add(function.namespace().len())
		.saturating_add(function.comment().map_or(0, crate::values::Text::len))
		.saturating_add(1024);
	bytes = bytes
		.checked_add(type_copy_bytes(function.result(), 0)?)
		.ok_or_else(|| invalid("frozen function scratch overflows"))?;
	for parameter in function.parameters() {
		let value_type_bytes = type_copy_bytes(parameter.value_type(), 0)?;
		bytes = bytes
			.checked_add(256 + parameter.name().len())
			.and_then(|bytes| bytes.checked_add(value_type_bytes))
			.ok_or_else(|| invalid("frozen function scratch overflows"))?;
	}
	budget.admit(bytes)?;
	let descriptor = FunctionCatalogEntry::from_definition(function)?;
	let mut output = Vec::new();
	encode(&mut output, &descriptor)?;
	if output.len() > super::codec::MAX_BYTES {
		return Err(invalid("frozen function descriptor exceeds its byte ceiling"));
	}
	Ok(output)
}

/// Source framing bounds every type/name allocation before the existing
/// decoder.
pub(in crate::storage) fn decode_source(
	bytes: &[u8],
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<SchemaFunction> {
	if bytes.len() > super::codec::MAX_BYTES || bytes.len() < 28 {
		return Err(invalid("frozen function descriptor length is invalid"));
	}
	let available = budget
		.maximum
		.checked_sub(budget.used)
		.ok_or_else(|| invalid("frozen source budget is inconsistent"))?;
	let mut decoder = Decoder::bounded(bytes, available)?;
	let descriptor = decode(&mut decoder)?;
	let conversion = descriptor
		.parameters
		.len()
		.checked_mul(std::mem::size_of::<SchemaFunctionParameter>())
		.and_then(|bytes| bytes.checked_add(256))
		.ok_or_else(|| invalid("frozen source conversion bytes overflow"))?;
	decoder.admit_heap(
		conversion
			.checked_add(crate::values::storage::tree_allocation_bytes(
				descriptor.parameters.len(),
				std::mem::size_of::<&str>(),
			)?)
			.ok_or_else(|| invalid("frozen source conversion bytes overflow"))?,
	)?;
	let admitted = decoder.admitted_bytes();
	decoder.finish()?;
	budget.admit(admitted)?;
	descriptor
		.into_definition()
		.map_err(|error| invalid("frozen function descriptor is invalid").with_cause(error))
}

/// Bounds recursive type copies before creating the shared catalog projection.
fn type_copy_bytes(value: &Type, depth: usize) -> FerrosGrynnResult<usize> {
	if depth > 64 {
		return Err(invalid("frozen function type depth exceeds 64"));
	}
	let mut bytes = 256usize;
	match value {
		Type::Array(value) | Type::Set(value) | Type::Range(value) | Type::Option(value) => {
			bytes = bytes.saturating_add(type_copy_bytes(value, depth + 1)?);
		}
		Type::Union(values) => {
			if values.len() > 4096 {
				return Err(invalid("frozen function type count exceeds 4096"));
			}
			for value in values {
				bytes = bytes.saturating_add(type_copy_bytes(value, depth + 1)?);
			}
		}
		Type::RecordShape(values) => {
			if values.len() > 4096 {
				return Err(invalid("frozen function type count exceeds 4096"));
			}
			for (name, value) in values {
				bytes = bytes
					.saturating_add(name.len())
					.saturating_add(type_copy_bytes(value, depth + 1)?);
			}
		}
		Type::Closure {
			parameters,
			result,
		} => {
			if parameters.len() > 4096 {
				return Err(invalid("frozen function type count exceeds 4096"));
			}
			for value in parameters {
				bytes = bytes.saturating_add(type_copy_bytes(value, depth + 1)?);
			}
			bytes = bytes.saturating_add(type_copy_bytes(result, depth + 1)?);
		}
		_ => {}
	}
	Ok(bytes)
}

/// Writes the authoritative source descriptor without executable IR.
pub(super) fn encode(
	output: &mut Vec<u8>,
	function: &FunctionCatalogEntry,
) -> FerrosGrynnResult<()> {
	output.extend_from_slice(&function.id.get().to_le_bytes());
	output.extend_from_slice(&function.version.get().to_le_bytes());
	output.extend_from_slice(&function.published_at.to_le_bytes());
	output.push(function_effect(function.effect));
	output.push(u8::from(function.cost));
	output.push(function.max_depth.unwrap_or(0));
	output.push(0);
	write_u16(output, function.parameters.len(), "function parameter count")?;
	output.extend_from_slice(&0_u16.to_le_bytes());
	write_u32(output, function.body.len(), "function body length")?;
	write_string(output, function.namespace.as_str(), "function namespace")?;
	write_string(output, function.name.as_str(), "function name")?;
	type_codec::encode(output, &function.result, 0)?;
	for parameter in &function.parameters {
		write_string(output, parameter.name.as_str(), "function parameter name")?;
		type_codec::encode(output, &parameter.value_type, 0)?;
	}
	write_optional_string(
		output,
		function.comment.as_ref().map(|comment| comment.as_str()),
		"function comment",
	)?;
	output.extend_from_slice(function.body.as_bytes());
	Ok(())
}

/// Restores the exact descriptor using the catalog bounds.
pub(super) fn decode(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<FunctionCatalogEntry> {
	let id = FunctionId::from_storage(decoder.read_u32()?)?;
	let version = SchemaVersion::from_storage(decoder.read_u32()?)?;
	let published_at = decoder.read_u64()?;
	let effect = decode_function_effect(decoder.read_u8()?)?;
	let cost = match decoder.read_u8()? {
		0 => false,
		1 => true,
		_ => return Err(invalid("function cost flag is invalid")),
	};
	let max_depth = match decoder.read_u8()? {
		0 => None,
		value @ 1..=64 => Some(value),
		_ => return Err(invalid("function recursion bound is invalid")),
	};
	decoder.expect_zero(1, "function reserved byte")?;
	let parameter_count = usize::from(decoder.read_u16()?);
	decoder.expect_zero(2, "function reserved bytes")?;
	let body_len = decoder.read_count("function body length")?;
	let namespace = read_string(decoder, "function namespace")?.into();
	let name = read_string(decoder, "function name")?.into();
	let result = type_codec::decode(decoder, 0)?;
	if parameter_count > decoder.remaining_bytes().saturating_sub(body_len.saturating_add(2)) / 4 {
		return Err(invalid("function parameter count exceeds remaining bytes"));
	}
	decoder.admit_heap(
		parameter_count
			.checked_mul(std::mem::size_of::<FunctionParameterCatalogEntry>())
			.ok_or_else(|| invalid("function parameter allocation overflows"))?,
	)?;
	let mut parameters = Vec::with_capacity(parameter_count);
	for _ in 0..parameter_count {
		parameters.push(FunctionParameterCatalogEntry {
			name: read_string(decoder, "function parameter name")?.into(),
			value_type: type_codec::decode(decoder, 0)?,
		});
	}
	let comment = read_optional_string(decoder, "function comment")?.map(Into::into);
	decoder.admit_heap(body_len.saturating_mul(2).saturating_add(64))?;
	let body = std::str::from_utf8(decoder.read_exact(body_len)?)
		.map_err(|_error| invalid("function body is not UTF-8"))?
		.into();
	Ok(FunctionCatalogEntry {
		id,
		version,
		published_at,
		namespace,
		name,
		parameters,
		result,
		effect,
		cost,
		max_depth,
		body,
		comment,
	})
}

/// Stable explicit effect tags shared by catalog and frozen sources.
const fn function_effect(effect: FunctionEffect) -> u8 {
	match effect {
		FunctionEffect::Pure => 0,
		FunctionEffect::Stable => 1,
		FunctionEffect::Impure => 2,
	}
}

/// Rejects unassigned effect tags before constructing a descriptor.
fn decode_function_effect(value: u8) -> FerrosGrynnResult<FunctionEffect> {
	match value {
		0 => Ok(FunctionEffect::Pure),
		1 => Ok(FunctionEffect::Stable),
		2 => Ok(FunctionEffect::Impure),
		_ => Err(invalid("function effect is invalid")),
	}
}

#[cfg(test)]
/// Regression tests for function behavior.
mod tests {
	use super::*;

	/// Verifies event frozen function counts are checked before allocation.
	#[test]
	fn event_frozen_function_counts_are_checked_before_allocation() {
		use crate::test_assertions::AssertResult as _;

		let mut function = SchemaFunction::new(
			"fn",
			"a",
			Vec::new(),
			Type::Union(vec![Type::Int]),
			FunctionEffect::Pure,
			false,
			None,
			"{ RETURN 1; }",
		)
		.assert_ok();
		function.id = Some(FunctionId::from_storage(1).assert_ok());
		let bytes =
			encode_source(&function, &mut CodecBudget::new(1 << 20, 0).assert_ok()).assert_ok();
		let mut parameters = bytes.clone();
		parameters[20..22].copy_from_slice(&u16::MAX.to_le_bytes());
		let error = decode_source(&parameters, &mut CodecBudget::new(16 * 1024, 0).assert_ok())
			.err()
			.ok_or_else(|| invalid("malformed parameter count was accepted"))
			.assert_ok();
		assert!(error.to_string().contains("parameter count exceeds remaining bytes"));
		let mut members = bytes;
		members[36..38].copy_from_slice(&4096_u16.to_le_bytes());
		let error = decode_source(&members, &mut CodecBudget::new(16 * 1024, 0).assert_ok())
			.err()
			.ok_or_else(|| invalid("malformed type count was accepted"))
			.assert_ok();
		assert!(error.to_string().contains("member count exceeds remaining bytes"));
	}
}
