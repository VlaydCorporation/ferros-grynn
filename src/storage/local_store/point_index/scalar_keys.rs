//! Canonical Base/Effective changes share the physical publication envelope.

use super::{
	BTreeMap,
	BTreeSet,
	BaseAtom,
	DatabaseRoot,
	EncodeContext,
	FerrosGrynnResult,
	GlobalAtomId,
	GraphId,
	GraphOccurrence,
	IndexId,
	PreparedIndexChange,
	PublicationDelta,
	invalid,
};
use crate::storage::local_store::point_namespace::PublicationBudget;

/// Canonical changes grouped by their independent physical binding.
pub(super) type ScopedChanges =
	BTreeMap<Option<GraphId>, BTreeMap<IndexId, Vec<PreparedIndexChange>>>;

/// Final sides retain generation-aware addresses and one shared resource cap.
///
/// # Errors
/// Propagates lookup, owner, encoding and preallocation admission failures.
pub(super) fn scalar_changes(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	context: &mut EncodeContext,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<ScopedChanges> {
	let mut affected = ScopedChanges::new();
	let mut effective = BTreeSet::new();
	for target in delta.overlays.iter().chain(&delta.memberships) {
		effective_target(&mut effective, *target, budget)?;
	}
	for slot in &delta.global_slots {
		let old = before.atoms.atom_by_slot(*slot)?;
		let new = after.atoms.atom_by_slot(*slot)?;
		if let Some((old, new)) = old.zip(new)
			&& old.id() == new.id()
			&& old.class_id() == new.class_id()
		{
			for (graph, _) in
				after.memberships(new.id()).into_iter().flat_map(|members| members.iter())
			{
				effective_target(&mut effective, (*graph, new.id()), budget)?;
			}
			let batch = after.indexes.prepare_base_replace_bounded(
				old,
				new,
				context,
				budget.available_bytes()?,
			)?;
			append_batch(&mut affected, batch, budget)?;
		} else {
			for (atom, insert) in [(old, false), (new, true)] {
				if let Some(atom) = atom {
					let batch = after.indexes.prepare_base_transition(
						atom,
						insert,
						context,
						budget.available_bytes()?,
					)?;
					append_batch(&mut affected, batch, budget)?;
				}
			}
		}
	}
	// Both entry routes share one final pair; no intermediate key is durable.
	for (graph, global) in effective {
		let old = effective_side(before, graph, global)?;
		let new = effective_side(after, graph, global)?;
		if let Some((old, new)) = old.zip(new)
			&& old.0.class_id() == new.0.class_id()
			&& old.1.local_id() == new.1.local_id()
		{
			let batch = after.indexes.prepare_effective_replace_bounded(
				graph,
				old,
				new,
				context,
				budget.available_bytes()?,
			)?;
			append_batch(&mut affected, batch, budget)?;
		} else {
			for (side, insert) in [(old, false), (new, true)] {
				if let Some((atom, occurrence)) = side {
					let batch = after.indexes.prepare_effective_transition(
						graph,
						atom,
						occurrence,
						insert,
						context,
						budget.available_bytes()?,
					)?;
					append_batch(&mut affected, batch, budget)?;
				}
			}
		}
	}
	Ok(affected)
}

/// Duplicate effective prerequisites do not allocate or retain another tree
/// slot.
fn effective_target(
	targets: &mut BTreeSet<(GraphId, GlobalAtomId)>,
	target: (GraphId, GlobalAtomId),
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	if !targets.contains(&target) {
		budget.tree_entry::<(GraphId, GlobalAtomId)>(targets.len())?;
		targets.insert(target);
	}
	Ok(())
}

/// Source backing stays live during moves; payload and context are charged
/// once.
fn append_batch(
	affected: &mut ScopedChanges,
	batch: (Vec<PreparedIndexChange>, usize),
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	let (changes, persistent) = batch;
	budget.retain(persistent)?;
	let source = changes
		.capacity()
		.checked_mul(size_of::<PreparedIndexChange>())
		.ok_or_else(|| invalid("scalar changes backing overflows usize"))?;
	budget.with_scratch(source, |budget| {
		for change in changes {
			if !affected.contains_key(&change.graph) {
				budget
					.tree_entry::<(Option<GraphId>, BTreeMap<IndexId, Vec<PreparedIndexChange>>)>(
						affected.len(),
					)?;
			}
			let indexes = affected.entry(change.graph).or_default();
			if !indexes.contains_key(&change.index) {
				budget.tree_entry::<(IndexId, Vec<PreparedIndexChange>)>(indexes.len())?;
			}
			let destination = indexes.entry(change.index).or_default();
			reserve_change(destination, budget)?;
			destination.push(change);
		}
		Ok(())
	})
}

/// Destination growth overlaps its prior backing and the source batch iterator.
fn reserve_change(
	changes: &mut Vec<PreparedIndexChange>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	if changes.len() < changes.capacity() {
		return Ok(());
	}
	let old = changes.capacity();
	let next = old
		.checked_mul(2)
		.ok_or_else(|| invalid("scalar changes capacity overflows usize"))?
		.max(1);
	let bytes = next
		.checked_mul(size_of::<PreparedIndexChange>())
		.ok_or_else(|| invalid("scalar changes growth overflows usize"))?;
	budget.scratch(bytes)?;
	changes.try_reserve_exact(next - changes.len()).map_err(|source| {
		invalid("scalar changes allocation failed").with_technical_source(source)
	})?;
	budget.retain(
		(changes.capacity() - old)
			.checked_mul(size_of::<PreparedIndexChange>())
			.ok_or_else(|| invalid("scalar changes retained bytes overflow usize"))?,
	)
}

/// An absent membership is valid; an incomplete live owner is corruption.
fn effective_side(
	root: &DatabaseRoot,
	graph: GraphId,
	global: GlobalAtomId,
) -> FerrosGrynnResult<Option<(&BaseAtom, &GraphOccurrence)>> {
	let Some(local) = root.membership(graph, global) else {
		return Ok(None);
	};
	Ok(Some((
		root.atom(global).ok_or_else(|| invalid("point effective atom is absent"))?,
		root.occurrence(graph, local)
			.ok_or_else(|| invalid("point effective occurrence is absent"))?,
	)))
}
