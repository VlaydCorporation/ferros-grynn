//! Immutable canonical arena ownership shared by catalog and physical
//! snapshots.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, GraphId, QualifiedAtomId},
	values::storage::DiskAtomRef,
};

/// Maps remain private so accepted owners cannot acquire a different scope
/// table.
#[derive(Debug, Default)]
struct GraphArenaData {
	/// Graph identity at each compact root-arena slot.
	graph_by_slot: BTreeMap<u32, GraphId>,
	/// Full generation-aware container path at the same slot.
	scope_by_slot: BTreeMap<u32, Box<[AtomId]>>,
}

/// Canonical maps use value equality; clones retain one immutable owner.
///
/// Only these maps belong to FGDC. Pointer identity is a runtime property.
#[derive(Debug, Clone, Default)]
pub(in crate::storage) struct GraphArenaBindings(
	/// Shared canonical maps remain alive through the last physical or catalog
	/// owner.
	Arc<GraphArenaData>,
);

impl GraphArenaBindings {
	/// Fixed shared-owner backing is charged before maps move into the Arc.
	pub(super) const fn owner_bytes() -> usize {
		size_of::<GraphArenaData>() + 2 * size_of::<usize>() + 2 * align_of::<GraphArenaData>()
	}

	/// Takes canonical input; existing build/open validators own its
	/// validation.
	pub(in crate::storage) fn new(
		graph_by_slot: BTreeMap<u32, GraphId>,
		scope_by_slot: BTreeMap<u32, Box<[AtomId]>>,
	) -> Self {
		Self(Arc::new(GraphArenaData {
			graph_by_slot,
			scope_by_slot,
		}))
	}

	/// Borrows graph identities without cloning the accepted arena.
	pub(in crate::storage) fn graph_by_slot(&self) -> &BTreeMap<u32, GraphId> {
		&self.0.graph_by_slot
	}

	/// Borrows generation-aware scopes without exposing a mutable alias.
	pub(in crate::storage) fn scope_by_slot(&self) -> &BTreeMap<u32, Box<[AtomId]>> {
		&self.0.scope_by_slot
	}

	/// Estimates an independent canonical owner before copying either map.
	///
	/// # Errors
	/// Rejects tree or boxed-scope byte counts that overflow the logical
	/// envelope.
	pub(in crate::storage) fn retained_copy_bytes(&self) -> FerrosGrynnResult<usize> {
		let graphs =
			crate::memory::tree_retained_bytes::<(u32, GraphId)>(self.graph_by_slot().len())?;
		let scopes =
			crate::memory::tree_retained_bytes::<(u32, Box<[AtomId]>)>(self.scope_by_slot().len())?;
		let metadata = Self::owner_bytes();
		let initial =
			graphs.checked_add(scopes).and_then(|bytes| bytes.checked_add(metadata)).ok_or_else(
				|| FerrosGrynnError::invalid_storage_file("arena copy tree bytes overflow usize"),
			)?;
		self.scope_by_slot().values().try_fold(initial, |retained, scope| {
			scope
				.len()
				.checked_mul(size_of::<AtomId>())
				.and_then(|bytes| retained.checked_add(bytes))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("arena copy scope bytes overflow usize")
				})
		})
	}

	/// Resolves against the accepted immutable arena without rebuilding its
	/// graph tree.
	///
	/// # Errors
	/// Rejects a foreign root, missing/duplicate prefix, wrong child or stale
	/// membership.
	pub(in crate::storage) fn qualified(
		&self,
		database: &DatabaseRoot,
		root: GraphId,
		qualified: &QualifiedAtomId,
	) -> FerrosGrynnResult<(u32, DiskAtomRef)> {
		self.validate_root(root)?;
		super::qualified::resolve_qualified(
			database,
			root,
			qualified,
			|_parent, _container, prefix| self.resolve_prefix(prefix),
		)
	}

	/// Resolves one accepted direct container without building an arena.
	///
	/// # Errors
	/// Rejects foreign roots, absent/duplicate scopes, stale owners and child
	/// bindings that disagree with their full-generation occurrence.
	pub(in crate::storage) fn direct_child_slot(
		&self,
		database: &DatabaseRoot,
		root: GraphId,
		container: AtomId,
	) -> FerrosGrynnResult<u32> {
		self.validate_root(root)?;
		super::qualified::resolve_scope(
			database,
			root,
			&[container],
			|_parent, _container, prefix| self.resolve_prefix(prefix),
		)
		.map(|(_graph, slot)| slot)
	}

	/// Validates the root once before traversing accepted scope prefixes.
	///
	/// # Errors
	/// Rejects a foreign root or a missing/nonempty reserved root scope.
	fn validate_root(&self, root: GraphId) -> FerrosGrynnResult<()> {
		if self.graph_by_slot().get(&0) != Some(&root)
			|| self.scope_by_slot().get(&0).is_none_or(|scope| !scope.is_empty())
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"qualified point arena has the wrong root",
			));
		}
		Ok(())
	}

	/// Borrows one exact full path without allocating or collapsing shared
	/// children.
	///
	/// # Errors
	/// Rejects an absent/duplicate scope or missing graph binding.
	fn resolve_prefix(&self, prefix: &[AtomId]) -> FerrosGrynnResult<(u32, GraphId)> {
		// shortcut: scan accepted scopes per prefix; add an admitted index only
		// after Q-12 attribution.
		let mut matches = self.scope_by_slot().iter().filter(|(_, scope)| scope.as_ref() == prefix);
		let (slot, _) = matches.next().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"qualified point scope is absent from accepted arena",
			)
		})?;
		if matches.next().is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"qualified point scope has duplicate arena slots",
			));
		}
		let graph = self.graph_by_slot().get(slot).copied().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("qualified point scope has no graph binding")
		})?;
		Ok((*slot, graph))
	}

	/// Distinguishes a shared accepted owner from an independently equal arena.
	pub(in crate::storage) fn same_owner(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.0, &other.0)
	}
}

impl PartialEq for GraphArenaBindings {
	/// Runtime identity can short-circuit unchanged canonical value equality.
	fn eq(&self, other: &Self) -> bool {
		self.same_owner(other)
			|| (self.graph_by_slot() == other.graph_by_slot()
				&& self.scope_by_slot() == other.scope_by_slot())
	}
}

impl Eq for GraphArenaBindings {}

#[cfg(test)]
/// Snapshot clones and independently decoded arenas preserve different
/// contracts.
mod tests {
	use super::*;
	use crate::{
		graph::AtomKind,
		test_assertions::{AssertOption as _, AssertResult as _},
	};

	/// Shared child paths retain their full container generations.
	fn fixture() -> GraphArenaBindings {
		let root = GraphId::new(1).assert_some();
		let child = GraphId::new(2).assert_some();
		let containers = [0, 1].map(|slot| AtomId::from_parts(AtomKind::Node, slot, 3).assert_ok());
		GraphArenaBindings::new(
			BTreeMap::from([(0, root), (1, child), (2, child)]),
			BTreeMap::from([
				(0, Box::<[AtomId]>::default()),
				(1, vec![containers[0]].into_boxed_slice()),
				(2, vec![containers[1]].into_boxed_slice()),
			]),
		)
	}

	/// Dropping one snapshot cannot drop maps retained by another snapshot.
	#[test]
	fn arena_clone_retains_one_immutable_canonical_owner() {
		let source = fixture();
		let retained = source.clone();
		assert!(source.same_owner(&retained));
		assert!(std::ptr::eq(source.graph_by_slot(), retained.graph_by_slot()));
		assert!(std::ptr::eq(source.scope_by_slot(), retained.scope_by_slot()));
		assert_eq!(Arc::strong_count(&source.0), 2);
		drop(source);
		assert_eq!(Arc::strong_count(&retained.0), 1);
		assert_eq!(retained.graph_by_slot().len(), 3);
		assert_ne!(retained.scope_by_slot()[&1], retained.scope_by_slot()[&2]);
	}

	/// A decoded value can equal an accepted arena without sharing its owner.
	#[test]
	fn arena_value_equality_preserves_independent_canonical_inputs() {
		let source = fixture();
		let independent = fixture();
		assert_eq!(source, independent);
		assert!(!source.same_owner(&independent));
		let mut scopes = independent.scope_by_slot().clone();
		scopes.remove(&2);
		let invalid = GraphArenaBindings::new(independent.graph_by_slot().clone(), scopes);
		assert_ne!(source, invalid);
		assert_eq!(
			GraphArenaBindings::default(),
			GraphArenaBindings::new(BTreeMap::new(), BTreeMap::new())
		);
	}
}
