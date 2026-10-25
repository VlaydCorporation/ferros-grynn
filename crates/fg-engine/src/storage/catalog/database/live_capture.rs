//! Shared checked function/source identities for LIVE queries and bindings.

use std::mem::size_of;

use super::codec::{Decoder, invalid};
use crate::{
	datastore::{ClassId, FunctionId, SchemaVersion},
	gql_status::{FerrosGrynnResult, StatusCode},
	storage::history_codec::CodecBudget,
	values::Text,
};

/// Concrete function versions and historical source namespace owners.
pub(super) type Identities = (Vec<(FunctionId, SchemaVersion)>, Vec<(Text, ClassId)>);

/// Computes the exact variable identity bytes with checked arithmetic.
pub(super) fn size(
	functions: &[(FunctionId, SchemaVersion)],
	sources: &[(Text, ClassId)],
) -> FerrosGrynnResult<usize> {
	sources
		.iter()
		.try_fold(
			functions
				.len()
				.checked_mul(8)
				.ok_or_else(|| invalid("LIVE function bytes overflow"))?,
			|bytes, (name, _)| bytes.checked_add(8)?.checked_add(name.len()),
		)
		.ok_or_else(|| invalid("LIVE source bytes overflow"))
}

/// Appends pre-admitted identity vectors in canonical order.
pub(super) fn encode(
	output: &mut Vec<u8>,
	functions: &[(FunctionId, SchemaVersion)],
	sources: &[(Text, ClassId)],
) -> FerrosGrynnResult<()> {
	for (id, version) in functions {
		output.extend_from_slice(&id.get().to_le_bytes());
		output.extend_from_slice(&version.get().to_le_bytes());
	}
	for (name, id) in sources {
		output.extend_from_slice(&id.get().to_le_bytes());
		output.extend_from_slice(
			&u32::try_from(name.len())
				.map_err(|_error| invalid("LIVE source name is too long"))?
				.to_le_bytes(),
		);
		output.extend_from_slice(name.as_str().as_bytes());
	}
	Ok(())
}

/// Checks fixed minima and admits both DTO and runtime copies before growth.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	functions: usize,
	sources: usize,
) -> FerrosGrynnResult<Identities> {
	let minimum = functions
		.checked_mul(8)
		.and_then(|bytes| bytes.checked_add(sources.checked_mul(8)?))
		.ok_or_else(|| invalid("LIVE identity lengths overflow"))?;
	if minimum > decoder.remaining_bytes() {
		return Err(invalid("LIVE identities exceed available bytes"));
	}
	budget.admit(
		functions
			.checked_mul(2 * size_of::<(FunctionId, SchemaVersion)>())
			.and_then(|bytes| {
				bytes.checked_add(sources.checked_mul(2 * size_of::<(Text, ClassId)>())?)
			})
			.ok_or_else(|| invalid("LIVE identity allocation overflows"))?,
	)?;
	let mut function_ids = Vec::new();
	function_ids
		.try_reserve_exact(functions)
		.map_err(|_error| invalid("LIVE function allocation failed"))?;
	for _ in 0..functions {
		let id = FunctionId::new(decoder.read_u32()?)
			.ok_or_else(|| invalid("LIVE function identity is reserved"))?;
		let version = SchemaVersion::new(decoder.read_u32()?)
			.ok_or_else(|| invalid("LIVE function version is reserved"))?;
		function_ids.push((id, version));
	}
	let mut record_sources = Vec::new();
	record_sources
		.try_reserve_exact(sources)
		.map_err(|_error| invalid("LIVE source owner allocation failed"))?;
	for _ in 0..sources {
		let id = ClassId::new(decoder.read_u32()?)
			.ok_or_else(|| invalid("LIVE source owner identity is reserved"))?;
		let length = decoder.read_count("LIVE source name bytes")?;
		let name = std::str::from_utf8(decoder.read_exact(length)?)
			.map_err(|_error| invalid("LIVE source name is not UTF-8"))?;
		if name.is_empty() || name.contains('\0') {
			return Err(invalid("LIVE source name is invalid"));
		}
		budget.admit(
			length
				.checked_mul(2)
				.ok_or_else(|| invalid("LIVE source name allocation overflows"))?,
		)?;
		record_sources.push((name.into(), id));
	}
	Ok((function_ids, record_sources))
}

/// Fault metadata keeps a recognized origin code and bounded static reason.
pub(super) fn diagnostic(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
	code_len: usize,
	reason_len: usize,
) -> FerrosGrynnResult<Option<(Text, Text)>> {
	if code_len > 8 || reason_len > 4096 {
		return Err(invalid("LIVE fault metadata exceeds limits"));
	}
	budget.admit(
		code_len
			.checked_add(reason_len)
			.and_then(|bytes| bytes.checked_mul(3))
			.ok_or_else(|| invalid("LIVE diagnostic bytes overflow"))?,
	)?;
	let code = std::str::from_utf8(decoder.read_exact(code_len)?)
		.map_err(|_error| invalid("LIVE diagnostic code is not UTF-8"))?;
	let reason = std::str::from_utf8(decoder.read_exact(reason_len)?)
		.map_err(|_error| invalid("LIVE diagnostic reason is not UTF-8"))?;
	if code.is_empty() && reason.is_empty() {
		return Ok(None);
	}
	code.parse::<StatusCode>()
		.map_err(|_error| invalid("LIVE diagnostic status code is malformed"))?;
	if reason.is_empty() {
		return Err(invalid("LIVE diagnostic classification is empty"));
	}
	Ok(Some((code.into(), reason.into())))
}
