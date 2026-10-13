//! Snapshot-owned logical conflict revisions.

use std::{collections::BTreeMap, sync::Arc};

use super::ConflictKey;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Exact revisions keep the ordinary root-clone cost at two inner Arc handles.
#[derive(Clone, Default)]
pub(super) struct VersionStore {
	/// Data, schema and source-capture revisions retained by old snapshots.
	primary: Arc<BTreeMap<ConflictKey, u64>>,
	/// Independent controls share one shell; each active namespace copies its
	/// own map.
	controls: Arc<ControlVersions>,
}

/// LIVE and invocation revisions have independent lifetimes and retention
/// policies.
#[derive(Clone, Default, PartialEq, Eq)]
struct ControlVersions {
	/// Existing subscription/binding controls preserve their snapshot
	/// semantics.
	live: Arc<BTreeMap<ConflictKey, u64>>,
	/// Invocation keys are bounded by the reachable outbox identities.
	events: Arc<BTreeMap<ConflictKey, u64>>,
}

/// Invocation lifecycle never copies data or subscription revision maps.
fn is_event(key: &ConflictKey) -> bool {
	matches!(key, ConflictKey::EventInvocation(_) | ConflictKey::EventOutboxRange)
}

/// Classifies independently writable subscription and invocation namespaces.
fn is_control(key: &ConflictKey) -> bool {
	matches!(
		key,
		ConflictKey::EventInvocation(_)
			| ConflictKey::EventOutboxRange
			| ConflictKey::LiveDefinition(_)
			| ConflictKey::LiveBinding(_)
			| ConflictKey::LiveCatalogRange
	)
}

impl VersionStore {
	/// Borrows the last publication revision of one logical key.
	pub(super) fn get(&self, key: &ConflictKey) -> Option<&u64> {
		if is_control(key) {
			if is_event(key) {
				self.controls.events.get(key)
			} else {
				self.controls.live.get(key)
			}
		} else {
			self.primary.get(key)
		}
	}

	/// Records a revision, copying only the affected domain.
	pub(super) fn insert(&mut self, key: ConflictKey, revision: u64) {
		let domain = if is_control(&key) {
			let controls = Arc::make_mut(&mut self.controls);
			if is_event(&key) {
				&mut controls.events
			} else {
				&mut controls.live
			}
		} else {
			&mut self.primary
		};
		Arc::make_mut(domain).insert(key, revision);
	}

	/// Charges the small COW shells and exact invocation map before a lifecycle
	/// write.
	pub(super) fn event_copy_bytes(&self, additional: usize) -> FerrosGrynnResult<usize> {
		let count = self
			.controls
			.events
			.len()
			.checked_add(additional)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT revision count"))?;
		crate::values::storage::tree_allocation_bytes(
			count,
			std::mem::size_of::<(ConflictKey, u64)>(),
		)?
		.checked_add(std::mem::size_of::<Self>() + std::mem::size_of::<ControlVersions>() + 64)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT revision bytes"))
	}

	/// Retired job IDs cannot be reused; stale lifecycle writes also require a
	/// live marker.
	pub(super) fn retain_event_invocations(&mut self, outbox: &super::EventOutbox) {
		Arc::make_mut(&mut Arc::make_mut(&mut self.controls).events).retain(|key, _| match key {
			ConflictKey::EventInvocation(id) => outbox.invocations.contains_key(id),
			_ => true,
		});
	}

	/// Compares leased data inputs, including independently allocated equal
	/// maps.
	pub(super) fn same_primary(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.primary, &other.primary) || self.primary == other.primary
	}

	/// Compares control revisions without scanning the data domain.
	pub(super) fn same_controls(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.controls, &other.controls) || self.controls == other.controls
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
		self.controls = Arc::clone(&current.controls);
	}

	/// Reserves both B-tree roots for conservative retention admission.
	///
	/// # Errors
	///
	/// Returns typed overflow before copying revision metadata.
	pub(super) fn allocation_bytes(&self) -> FerrosGrynnResult<usize> {
		let width = std::mem::size_of::<(ConflictKey, u64)>();
		let primary = crate::values::storage::tree_allocation_bytes(self.primary.len(), width)?;
		let live = crate::values::storage::tree_allocation_bytes(self.controls.live.len(), width)?;
		let events =
			crate::values::storage::tree_allocation_bytes(self.controls.events.len(), width)?;
		primary
			.checked_add(live)
			.and_then(|bytes| bytes.checked_add(events))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("revision metadata"))
	}
}

#[cfg(test)]
mod tests;
