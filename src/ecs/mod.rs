#![allow(
	clippy::expect_used,
	clippy::missing_panics_doc,
	reason = "pool key and concrete type must agree"
)]

//! Runtime ECS sidecar for graph atoms.
//!
//! Component identity comes exclusively from graph [`AtomId`] values. This
//! module deliberately has no entity allocator of its own.

/// Component joins driven by the smallest participating pool.
mod join;
/// Materialized property columns with distinct absent, null and value states.
mod property_column;
/// Generation-aware sparse component storage.
mod sparse_set;

use std::any::{Any, TypeId};

pub use join::{JoinIter2, JoinIter3, JoinIter4, JoinIter5, JoinIter6, JoinIter7, JoinIter8};
pub use property_column::{PropertyColumn, PropertyState};
use rapidhash::RapidHashMap;
pub use sparse_set::{SPARSE_PAGE_LEN, SparseSet};

use crate::{
	gql_status::FerrosGrynnResult,
	values::runtime::{PropertyKeyId, graph::AtomId},
};

/// Type-erased operations required for registry-wide lifecycle handling.
pub trait AnyPool: Any + Send + Sync {
	/// Removes atom for `AnyPool`.
	fn remove_atom(&mut self, id: AtomId);
	/// Tests membership of atom for `AnyPool`.
	fn contains_atom(&self, id: AtomId) -> bool;
	/// Returns the logical element count of this `AnyPool`.
	fn len(&self) -> usize;
	/// Reports whether this `AnyPool` contains no logical elements.
	fn is_empty(&self) -> bool {
		self.len() == 0
	}
	/// Returns atoms for `AnyPool`.
	fn atoms(&self) -> &[AtomId];
	/// Retains atoms for `AnyPool`.
	fn retain_atoms(&mut self, predicate: &mut dyn FnMut(AtomId) -> bool);
	/// Borrows any for `AnyPool`.
	fn as_any(&self) -> &dyn Any;
	/// Borrows any mut for `AnyPool`.
	fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: 'static + Send + Sync> AnyPool for SparseSet<T> {
	/// Removes atom for `SparseSet<T>`.
	fn remove_atom(&mut self, id: AtomId) {
		self.remove(id);
	}

	/// Tests membership of atom for `SparseSet<T>`.
	fn contains_atom(&self, id: AtomId) -> bool {
		self.contains(id)
	}

	/// Returns the logical element count of this `SparseSet<T>`.
	fn len(&self) -> usize {
		self.len()
	}

	/// Returns atoms for `SparseSet<T>`.
	fn atoms(&self) -> &[AtomId] {
		self.atoms()
	}

	/// Retains atoms for `SparseSet<T>`.
	fn retain_atoms(&mut self, predicate: &mut dyn FnMut(AtomId) -> bool) {
		self.retain(|id, _| predicate(id));
	}

	/// Borrows any for `SparseSet<T>`.
	fn as_any(&self) -> &dyn Any {
		self
	}

	/// Borrows any mut for `SparseSet<T>`.
	fn as_any_mut(&mut self) -> &mut dyn Any {
		self
	}
}

/// Component registry analogous to an EnTT registry, keyed by graph atoms.
///
/// Ordinary components are keyed by `TypeId`. Materialized properties use a
/// separate `(PropertyKeyId, TypeId)` namespace so two properties with the same
/// Rust type never alias one another.
#[derive(Default)]
pub struct WorldComponents {
	/// Components associated with this world components.
	components: RapidHashMap<TypeId, Box<dyn AnyPool>>,
	/// Property columns associated with this world components.
	property_columns: RapidHashMap<(PropertyKeyId, TypeId), Box<dyn AnyPool>>,
}

impl WorldComponents {
	/// Constructs `WorldComponents` from the supplied initial state.
	pub fn new() -> Self {
		Self::default()
	}

	/// Returns component pool count for `WorldComponents`.
	pub fn component_pool_count(&self) -> usize {
		self.components.len()
	}

	/// Returns property pool count for `WorldComponents`.
	pub fn property_pool_count(&self) -> usize {
		self.property_columns.len()
	}

	/// Inserts a key and value using the collection's replacement rules.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn insert<T>(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.assure_component::<T>().insert(id, component)
	}

	/// Removes the requested entry and returns it when present.
	pub fn remove<T>(&mut self, id: AtomId) -> Option<T>
	where
		T: 'static + Send + Sync,
	{
		self.pool_mut::<T>()?.remove(id)
	}

	/// Looks up the requested entry without modifying the collection.
	pub fn get<T>(&self, id: AtomId) -> Option<&T>
	where
		T: 'static + Send + Sync,
	{
		self.pool::<T>()?.get(id)
	}

	/// Borrows the requested entry mutably when it exists.
	pub fn get_mut<T>(&mut self, id: AtomId) -> Option<&mut T>
	where
		T: 'static + Send + Sync,
	{
		self.pool_mut::<T>()?.get_mut(id)
	}

	/// Reports whether the contains condition holds for `WorldComponents`.
	pub fn contains<T>(&self, id: AtomId) -> bool
	where
		T: 'static + Send + Sync,
	{
		self.pool::<T>().is_some_and(|pool| pool.contains(id))
	}

	/// Returns pool for `WorldComponents`.
	pub fn pool<T>(&self) -> Option<&SparseSet<T>>
	where
		T: 'static + Send + Sync,
	{
		self.components
			.get(&TypeId::of::<T>())
			.and_then(|pool| pool.as_any().downcast_ref::<SparseSet<T>>())
	}

	/// Returns pool mut for `WorldComponents`.
	pub fn pool_mut<T>(&mut self) -> Option<&mut SparseSet<T>>
	where
		T: 'static + Send + Sync,
	{
		self.components
			.get_mut(&TypeId::of::<T>())
			.and_then(|pool| pool.as_any_mut().downcast_mut::<SparseSet<T>>())
	}

	/// Sets property for `WorldComponents`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn set_property<T>(
		&mut self,
		key: PropertyKeyId,
		id: AtomId,
		value: T,
	) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.assure_property_column::<T>(key).set(id, value)
	}

	/// Sets property null for `WorldComponents`.
	///
	/// # Errors
	///
	/// Propagates validation or execution failures from the underlying
	/// operation.
	pub fn set_property_null<T>(
		&mut self,
		key: PropertyKeyId,
		id: AtomId,
	) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.assure_property_column::<T>(key).set_null(id)
	}

	/// Removes property for `WorldComponents`.
	pub fn remove_property<T>(&mut self, key: PropertyKeyId, id: AtomId) -> Option<PropertyState<T>>
	where
		T: 'static + Send + Sync,
	{
		self.property_column_mut::<T>(key)?.remove(id)
	}

	/// Returns property for `WorldComponents`.
	pub fn property<T>(&self, key: PropertyKeyId, id: AtomId) -> PropertyState<&T>
	where
		T: 'static + Send + Sync,
	{
		self.property_column::<T>(key).map_or(PropertyState::None, |column| column.state(id))
	}

	/// Returns property column for `WorldComponents`.
	pub fn property_column<T>(&self, key: PropertyKeyId) -> Option<&PropertyColumn<T>>
	where
		T: 'static + Send + Sync,
	{
		self.property_columns
			.get(&(key, TypeId::of::<T>()))
			.and_then(|pool| pool.as_any().downcast_ref::<PropertyColumn<T>>())
	}

	/// Returns property column mut for `WorldComponents`.
	pub fn property_column_mut<T>(&mut self, key: PropertyKeyId) -> Option<&mut PropertyColumn<T>>
	where
		T: 'static + Send + Sync,
	{
		self.property_columns
			.get_mut(&(key, TypeId::of::<T>()))
			.and_then(|pool| pool.as_any_mut().downcast_mut::<PropertyColumn<T>>())
	}

	/// Removes an atom from every ordinary and materialized-property pool.
	pub fn remove_atom(&mut self, id: AtomId) {
		for pool in self.components.values_mut() {
			pool.remove_atom(id);
		}
		for pool in self.property_columns.values_mut() {
			pool.remove_atom(id);
		}
	}

	/// Removes all logical elements from the collection.
	pub fn clear(&mut self) {
		self.components.clear();
		self.property_columns.clear();
	}

	/// Drops components whose graph atom no longer exists.
	pub fn retain_atoms<Pred: FnMut(AtomId) -> bool>(&mut self, mut predicate: Pred) {
		for pool in self.components.values_mut() {
			pool.retain_atoms(&mut predicate);
		}
		for pool in self.property_columns.values_mut() {
			pool.retain_atoms(&mut predicate);
		}
	}

	/// Returns the assure component result.
	fn assure_component<T>(&mut self) -> &mut SparseSet<T>
	where
		T: 'static + Send + Sync,
	{
		self.components
			.entry(TypeId::of::<T>())
			.or_insert_with(|| Box::<SparseSet<T>>::default())
			.as_any_mut()
			.downcast_mut::<SparseSet<T>>()
			.expect("component pool key and concrete type must agree")
	}

	/// Returns the assure property column result.
	fn assure_property_column<T>(&mut self, key: PropertyKeyId) -> &mut PropertyColumn<T>
	where
		T: 'static + Send + Sync,
	{
		self.property_columns
			.entry((key, TypeId::of::<T>()))
			.or_insert_with(|| Box::new(PropertyColumn::<T>::new(key)))
			.as_any_mut()
			.downcast_mut::<PropertyColumn<T>>()
			.expect("property pool key and concrete type must agree")
	}
}

#[cfg(test)]
/// Regression tests for ecs behavior.
mod tests {
	use super::{PropertyState, WorldComponents};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::runtime::{PropertyKeyId, graph::AtomId};

	/// Returns atom for this module.
	fn atom(slot: u64, generation: u32, edge: bool) -> AtomId {
		let kind = if edge {
			1_u64 << 40
		} else {
			0
		};
		AtomId::from_raw((u64::from(generation) << 41) | kind | slot)
	}

	/// Verifies separates component and property namespaces.
	#[test]
	fn separates_component_and_property_namespaces() {
		let id = atom(7, 0, false);
		let first = PropertyKeyId::new(1);
		let second = PropertyKeyId::new(2);
		let mut world = WorldComponents::new();

		world.insert(id, 11_i64).assert_ok();
		world.set_property(first, id, 21_i64).assert_ok();
		world.set_property(second, id, 31_i64).assert_ok();

		assert_eq!(world.get::<i64>(id), Some(&11));
		assert_eq!(world.property::<i64>(first, id), PropertyState::Value(&21));
		assert_eq!(world.property::<i64>(second, id), PropertyState::Value(&31));
		assert_eq!(world.component_pool_count(), 1);
		assert_eq!(world.property_pool_count(), 2);
	}

	/// Verifies remove atom cleans every pool and respects generation.
	#[test]
	fn remove_atom_cleans_every_pool_and_respects_generation() {
		let old = atom(3, 4, true);
		let current = atom(3, 5, true);
		let key = PropertyKeyId::new(9);
		let mut world = WorldComponents::new();

		world.insert(old, 1_u32).assert_ok();
		world.set_property_null::<u64>(key, old).assert_ok();
		world.insert(current, 2_u32).assert_ok();
		world.set_property(key, current, 3_u64).assert_ok();

		assert_eq!(world.get::<u32>(old), None);
		assert_eq!(world.property::<u64>(key, old), PropertyState::None);
		world.remove_atom(old);
		assert_eq!(world.get::<u32>(current), Some(&2));
		assert_eq!(world.property::<u64>(key, current), PropertyState::Value(&3));

		world.remove_atom(current);
		assert_eq!(world.get::<u32>(current), None);
		assert_eq!(world.property::<u64>(key, current), PropertyState::None);
	}
}
