//! Stable historical atoms and qualified occurrences shared by source and LIVE
//! images.

use std::sync::Arc;

use super::{CodecBudget, Decoder, append, append_blob, invalid, values};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomOccurrenceRef, AtomRef, GraphRef},
	values::{Value, storage::EncodeContext},
};

/// Encodes a stable atom through the existing canonical RecordId
/// representation.
pub(in crate::storage) fn encode_atom(
	output: &mut Vec<u8>,
	atom: &AtomRef,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	budget.admit(
		128usize
			.checked_add(atom.id().estimated_heap_bytes())
			.ok_or_else(|| invalid("history atom reservation overflows"))?,
	)?;
	let bytes = values::encode(&Value::from(atom.clone()), context, budget)?;
	append_blob(output, &bytes, budget)
}

/// Rejects canonical values that are not stable atom references.
pub(in crate::storage) fn decode_atom(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<AtomRef> {
	match values::decode(decoder.blob()?, context, budget)? {
		Value::Atom(atom) => Ok(*atom),
		_ => Err(invalid("history identity is not an atom reference")),
	}
}

/// Writes the historical graph slot and stable atom, never a physical handle.
pub(in crate::storage) fn encode_occurrence(
	output: &mut Vec<u8>,
	occurrence: &AtomOccurrenceRef,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let slot = values::graph_slot(context, occurrence.graph(), budget)?;
	append(output, &slot.to_le_bytes(), budget)?;
	encode_atom(output, occurrence.atom(), context, budget)
}

/// Resolves a qualified occurrence solely from this group's dictionary.
pub(in crate::storage) fn decode_occurrence(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<AtomOccurrenceRef> {
	let graph = values::graph(context, decoder.u64()?, budget)?;
	let atom = decode_atom(decoder, context, budget)?;
	budget.admit(std::mem::size_of::<GraphRef>() + 32)?;
	Ok(AtomOccurrenceRef::new(atom, Arc::new(graph)))
}
