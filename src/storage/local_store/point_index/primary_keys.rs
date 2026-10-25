//! Canonical Primary mutations prepared from final live identities.

use super::{
	BaseAtom,
	DatabaseRoot,
	EncodeContext,
	FerrosGrynnResult,
	GlobalAtomId,
	PrimaryIndexMutation,
	PublicationDelta,
	invalid,
};
use crate::{
	storage::local_store::point_namespace::PublicationBudget,
	values::storage::StorageValue,
};

/// Primary keys change only when final live identities change.
///
/// # Errors
/// Propagates slot lookup, class resolution, canonical encoding, allocation
/// and shared publication-budget errors before physical index preparation.
pub(super) fn primary_changes(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	context: &mut EncodeContext,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<Vec<PrimaryIndexMutation<GlobalAtomId>>> {
	let mut mutations = Vec::new();
	for slot in &delta.global_slots {
		let old = before.atoms.atom_by_slot(*slot)?;
		let new = after.atoms.atom_by_slot(*slot)?;
		if old.map(BaseAtom::id) == new.map(BaseAtom::id) {
			continue;
		}
		for (atom, insert) in [(old, false), (new, true)] {
			if let Some(atom) = atom {
				reserve_mutation(&mut mutations, budget)?;
				// The preceding key remains owned while the next DTO and output
				// overlap.
				let (key, context_bytes) = StorageValue::encode_record_bounded(
					atom.record_id(),
					context,
					budget.remaining()?,
				)?;
				budget.retain(
					context_bytes
						.checked_add(key.capacity())
						.ok_or_else(|| invalid("Primary key retained bytes overflow usize"))?,
				)?;
				mutations.push(PrimaryIndexMutation {
					key,
					atom: atom.id(),
					insert,
				});
			}
		}
	}
	Ok(mutations)
}

/// Admits old/new backing overlap before growing the ordered mutation vector.
///
/// # Errors
/// Rejects size overflow or the shared resource ceiling before allocation;
/// preserves allocation failures with their technical cause.
fn reserve_mutation(
	mutations: &mut Vec<PrimaryIndexMutation<GlobalAtomId>>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	let capacity = mutations.capacity();
	if mutations.len() < capacity {
		return Ok(());
	}
	let next = capacity
		.checked_mul(2)
		.ok_or_else(|| invalid("Primary mutation capacity overflows usize"))?
		.max(1);
	let entry_bytes = size_of::<PrimaryIndexMutation<GlobalAtomId>>();
	let peak = next
		.checked_mul(entry_bytes)
		.ok_or_else(|| invalid("Primary mutation backing overflows usize"))?;
	let additional = (next - capacity)
		.checked_mul(entry_bytes)
		.ok_or_else(|| invalid("Primary mutation growth overflows usize"))?;
	budget.scratch(peak)?;
	budget.retain(additional)?;
	mutations.try_reserve_exact(next - mutations.len()).map_err(|source| {
		invalid("Primary mutation allocation failed").with_technical_source(source)
	})
}
