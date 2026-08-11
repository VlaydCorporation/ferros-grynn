#![allow(clippy::expect_used, clippy::missing_panics_doc, reason = "pool key and concrete type must agree")]

//! Runtime ECS sidecar for graph atoms.
//!
//! Component identity comes exclusively from graph [`AtomId`] values. This
//! module deliberately has no entity allocator of its own.

mod join;
mod property_column;
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
	fn remove_atom(&mut self, id: AtomId);
	fn contains_atom(&self, id: AtomId) -> bool;
	fn len(&self) -> usize;
	fn is_empty(&self) -> bool {
		self.len() == 0
	}
	fn atoms(&self) -> &[AtomId];
	fn retain_atoms(&mut self, predicate: &mut dyn FnMut(AtomId) -> bool);
	fn as_any(&self) -> &dyn Any;
	fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: 'static + Send + Sync> AnyPool for SparseSet<T> {
	fn remove_atom(&mut self, id: AtomId) {
		self.remove(id);
	}

	fn contains_atom(&self, id: AtomId) -> bool {
		self.contains(id)
	}

	fn len(&self) -> usize {
		self.len()
	}

	fn atoms(&self) -> &[AtomId] {
		self.atoms()
	}

	fn retain_atoms(&mut self, predicate: &mut dyn FnMut(AtomId) -> bool) {
		self.retain(|id, _| predicate(id));
	}

	fn as_any(&self) -> &dyn Any {
		self
	}

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
	components: RapidHashMap<TypeId, Box<dyn AnyPool>>,
	property_columns: RapidHashMap<(PropertyKeyId, TypeId), Box<dyn AnyPool>>,
}

impl WorldComponents {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn component_pool_count(&self) -> usize {
		self.components.len()
	}

	pub fn property_pool_count(&self) -> usize {
		self.property_columns.len()
	}

	pub fn insert<T>(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>>
	where
		T: 'static + Send + Sync,
	{
		self.assure_component::<T>().insert(id, component)
	}

	pub fn remove<T>(&mut self, id: AtomId) -> Option<T>
	where
		T: 'static + Send + Sync,
	{
		self.pool_mut::<T>()?.remove(id)
	}

	pub fn get<T>(&self, id: AtomId) -> Option<&T>
	where
		T: 'static + Send + Sync,
	{
		self.pool::<T>()?.get(id)
	}

	pub fn get_mut<T>(&mut self, id: AtomId) -> Option<&mut T>
	where
		T: 'static + Send + Sync,
	{
		self.pool_mut::<T>()?.get_mut(id)
	}

	pub fn contains<T>(&self, id: AtomId) -> bool
	where
		T: 'static + Send + Sync,
	{
		self.pool::<T>().is_some_and(|pool| pool.contains(id))
	}

	pub fn pool<T>(&self) -> Option<&SparseSet<T>>
	where
		T: 'static + Send + Sync,
	{
		self.components
			.get(&TypeId::of::<T>())
			.and_then(|pool| pool.as_any().downcast_ref::<SparseSet<T>>())
	}

	pub fn pool_mut<T>(&mut self) -> Option<&mut SparseSet<T>>
	where
		T: 'static + Send + Sync,
	{
		self.components
			.get_mut(&TypeId::of::<T>())
			.and_then(|pool| pool.as_any_mut().downcast_mut::<SparseSet<T>>())
	}

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

	pub fn remove_property<T>(&mut self, key: PropertyKeyId, id: AtomId) -> Option<PropertyState<T>>
	where
		T: 'static + Send + Sync,
	{
		self.property_column_mut::<T>(key)?.remove(id)
	}

	pub fn property<T>(&self, key: PropertyKeyId, id: AtomId) -> PropertyState<&T>
	where
		T: 'static + Send + Sync,
	{
		self.property_column::<T>(key).map_or(PropertyState::None, |column| column.state(id))
	}

	pub fn property_column<T>(&self, key: PropertyKeyId) -> Option<&PropertyColumn<T>>
	where
		T: 'static + Send + Sync,
	{
		self.property_columns
			.get(&(key, TypeId::of::<T>()))
			.and_then(|pool| pool.as_any().downcast_ref::<PropertyColumn<T>>())
	}

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
mod tests {
	use super::{PropertyState, WorldComponents};
	use crate::values::runtime::{PropertyKeyId, graph::AtomId};

	fn atom(slot: u64, generation: u32, edge: bool) -> AtomId {
		let kind = if edge {
			1_u64 << 40
		} else {
			0
		};
		AtomId::from_raw((u64::from(generation) << 41) | kind | slot)
	}

	#[test]
	fn separates_component_and_property_namespaces() {
		let id = atom(7, 0, false);
		let first = PropertyKeyId::new(1);
		let second = PropertyKeyId::new(2);
		let mut world = WorldComponents::new();

		world.insert(id, 11_i64).unwrap();
		world.set_property(first, id, 21_i64).unwrap();
		world.set_property(second, id, 31_i64).unwrap();

		assert_eq!(world.get::<i64>(id), Some(&11));
		assert_eq!(world.property::<i64>(first, id), PropertyState::Value(&21));
		assert_eq!(world.property::<i64>(second, id), PropertyState::Value(&31));
		assert_eq!(world.component_pool_count(), 1);
		assert_eq!(world.property_pool_count(), 2);
	}

	#[test]
	fn remove_atom_cleans_every_pool_and_respects_generation() {
		let old = atom(3, 4, true);
		let current = atom(3, 5, true);
		let key = PropertyKeyId::new(9);
		let mut world = WorldComponents::new();

		world.insert(old, 1_u32).unwrap();
		world.set_property_null::<u64>(key, old).unwrap();
		world.insert(current, 2_u32).unwrap();
		world.set_property(key, current, 3_u64).unwrap();

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
