//! Snapshot-owned logical conflict revisions.

use std::{collections::BTreeMap, sync::Arc};

use super::ConflictKey;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Exact FCW/SSI revisions with independently copied data and LIVE domains.
#[derive(Clone, Default)]
pub(super) struct VersionStore {
	/// Data, schema and source-capture revisions retained by old snapshots.
	primary: Arc<BTreeMap<ConflictKey, u64>>,
	/// LIVE control revisions do not copy the per-atom data domain.
	live: Arc<BTreeMap<ConflictKey, u64>>,
}

/// Classifies only the independently writable LIVE control namespace.
fn is_live(key: &ConflictKey) -> bool {
	matches!(
		key,
		ConflictKey::LiveDefinition(_)
			| ConflictKey::LiveBinding(_)
			| ConflictKey::LiveCatalogRange
	)
}

impl VersionStore {
	/// Borrows the last publication revision of one logical key.
	pub(super) fn get(&self, key: &ConflictKey) -> Option<&u64> {
		if is_live(key) {
			self.live.get(key)
		} else {
			self.primary.get(key)
		}
	}

	/// Records a revision, copying only the affected domain.
	pub(super) fn insert(&mut self, key: ConflictKey, revision: u64) {
		let domain = if is_live(&key) {
			&mut self.live
		} else {
			&mut self.primary
		};
		Arc::make_mut(domain).insert(key, revision);
	}

	/// Compares leased data inputs, including independently allocated equal
	/// maps.
	pub(super) fn same_primary(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.primary, &other.primary) || self.primary == other.primary
	}

	/// Compares control revisions without scanning the data domain.
	pub(super) fn same_live(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.live, &other.live) || self.live == other.live
	}

	/// Checks whether an import has unpublished revisions at its provisional
	/// stamp.
	pub(super) fn primary_has_revision(&self, revision: u64) -> bool {
		self.primary.values().any(|value| *value == revision)
	}

	/// Reserves the small COW shell and any primary map copied by restamping.
	///
	/// # Errors
	///
	/// Returns typed overflow before allocating either part.
	pub(super) fn rebase_allocation_bytes(
		&self,
		restamp_primary: bool,
	) -> FerrosGrynnResult<usize> {
		let primary = if restamp_primary {
			crate::values::storage::tree_allocation_bytes(
				self.primary.len(),
				std::mem::size_of::<(ConflictKey, u64)>(),
			)?
		} else {
			0
		};
		primary
			.checked_add(std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("import revision metadata"))
	}

	/// Restamps unpublished data while sharing current committed controls
	/// unchanged.
	pub(super) fn rebase(&mut self, current: &Self, from: u64, stamp: u64, restamp_primary: bool) {
		if restamp_primary {
			for value in Arc::make_mut(&mut self.primary).values_mut() {
				if *value == from {
					*value = stamp;
				}
			}
		}
		self.live = Arc::clone(&current.live);
	}

	/// Reserves both B-tree roots for conservative retention admission.
	///
	/// # Errors
	///
	/// Returns typed overflow before copying revision metadata.
	pub(super) fn allocation_bytes(&self) -> FerrosGrynnResult<usize> {
		let width = std::mem::size_of::<(ConflictKey, u64)>();
		let primary = crate::values::storage::tree_allocation_bytes(self.primary.len(), width)?;
		let live = crate::values::storage::tree_allocation_bytes(self.live.len(), width)?;
		primary
			.checked_add(live)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("revision metadata"))
	}
}

#[cfg(test)]
mod tests;
