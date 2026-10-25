//! Borrowed qualified endpoint validation, separate from arena allocation.

use crate::{
	datastore::{DatabaseRoot, GraphOccurrence},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, GraphId, QualifiedAtomId},
	values::storage::DiskAtomRef,
};

/// Resolves a borrowed route using the caller's existing child-slot authority.
///
/// Bulk planning supplies its children table; no arena is built by this kernel.
///
/// # Errors
/// Preserves binding errors and rejects stale owners, mismatched children,
/// reserved child slots and inconsistent terminal or owner memberships.
pub(super) fn resolve_qualified(
	database: &DatabaseRoot,
	root: GraphId,
	qualified: &QualifiedAtomId,
	next_binding: impl FnMut(u32, AtomId, &[AtomId]) -> FerrosGrynnResult<(u32, GraphId)>,
) -> FerrosGrynnResult<(u32, DiskAtomRef)> {
	let (current, current_slot) = resolve_scope(database, root, qualified.scope(), next_binding)?;
	let graph =
		database.graph(current).ok_or_else(|| invalid("qualified endpoint graph is absent"))?;
	let atom = qualified.atom();
	let occurrence = graph
		.occurrence(atom)
		.ok_or_else(|| invalid("qualified endpoint atom is absent from its graph"))?;
	validate_membership(database, current, occurrence)?;
	Ok((
		current_slot,
		DiskAtomRef::from_parts(
			atom.kind().ok_or_else(|| invalid("null atom cannot be a qualified endpoint"))?,
			atom.slot(),
		)?,
	))
}

/// Resolves only the complete owner path without interpreting a terminal atom.
///
/// # Errors
/// Preserves binding lookup order and rejects stale owners, wrong children,
/// reserved slots and inconsistent owner memberships.
pub(super) fn resolve_scope(
	database: &DatabaseRoot,
	root: GraphId,
	scope: &[AtomId],
	mut next_binding: impl FnMut(u32, AtomId, &[AtomId]) -> FerrosGrynnResult<(u32, GraphId)>,
) -> FerrosGrynnResult<(GraphId, u32)> {
	let mut current = root;
	let mut current_slot = 0;
	for (depth, container) in scope.iter().enumerate() {
		let (slot, graph) = next_binding(current_slot, *container, &scope[..=depth])?;
		// A generation-free disk address is safe only after checking its full
		// owner chain.
		let owner = database
			.occurrence(current, *container)
			.ok_or_else(|| invalid("qualified endpoint container is absent"))?;
		validate_membership(database, current, owner)?;
		if slot == 0 || owner.child_graph() != Some(graph) {
			return Err(invalid("qualified endpoint child binding disagrees with its container"));
		}
		current = graph;
		current_slot = slot;
	}
	Ok((current, current_slot))
}

/// Rejects disagreement between an occurrence and either membership authority.
///
/// # Errors
/// Returns a storage-file error for an absent Base owner, wrong kind or
/// mapping.
fn validate_membership(
	database: &DatabaseRoot,
	graph: GraphId,
	occurrence: &GraphOccurrence,
) -> FerrosGrynnResult<()> {
	let global = occurrence.global_id();
	let local = occurrence.local_id();
	if global.kind() != local.kind()
		|| database.atom(global).is_none()
		|| database.membership(graph, global) != Some(local)
		|| database.memberships(global).and_then(|members| members.get(&graph)) != Some(&local)
	{
		return Err(invalid("qualified endpoint membership disagrees with its occurrence"));
	}
	Ok(())
}

/// Keeps route metadata errors on the existing storage-file diagnostic
/// boundary.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Child lookup cannot substitute for shared generation-aware validation.
mod tests;
