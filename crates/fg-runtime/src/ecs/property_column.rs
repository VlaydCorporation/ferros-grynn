use std::any::Any;

use super::{AnyPool, SparseSet};
use crate::{
	gql_status::FerrosGrynnResult,
	values::runtime::{PropertyKeyId, graph::AtomId},
};

/// Distinction between an absent property, explicit null and a present typed
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PropertyState<T> {
	/// The property is absent from the materialized column.
	None,
	/// The property is present with an explicit null value.
	Null,
	/// The property is present with a typed value.
	Value(
		/// The property is present with a typed value.
		T,
	),
}

/// Typed ECS materialization of one graph property.
///
/// Presence and validity are independent: absence in `present` is `none`, a
/// cleared validity bit is `null`, and a set bit has a value in `values`.
#[derive(Debug, Clone)]
pub struct PropertyColumn<T> {
	/// Key associated with this property column.
	key: PropertyKeyId,
	/// Present associated with this property column.
	present: SparseSet<()>,
	/// Values associated with this property column.
	values: SparseSet<T>,
	/// Validity associated with this property column.
	validity: DenseBitmap,
}

impl<T> PropertyColumn<T> {
	/// Constructs `PropertyColumn<T>` from the supplied initial state.
	pub fn new(key: PropertyKeyId) -> Self {
		Self {
			key,
			present: SparseSet::new(),
			values: SparseSet::new(),
			validity: DenseBitmap::default(),
		}
	}

	/// Returns the stored key of this `PropertyColumn<T>`.
	pub const fn key(&self) -> PropertyKeyId {
		self.key
	}

	/// Returns the logical element count of this `PropertyColumn<T>`.
	pub const fn len(&self) -> usize {
		self.present.len()
	}

	/// Returns value len for `PropertyColumn<T>`.
	pub const fn value_len(&self) -> usize {
		self.values.len()
	}

	/// Reports whether this `PropertyColumn<T>` contains no logical elements.
	pub const fn is_empty(&self) -> bool {
		self.present.is_empty()
	}

	/// Reports whether the contains condition holds for `PropertyColumn<T>`.
	pub fn contains(&self, id: AtomId) -> bool {
		self.present.contains(id)
	}

	/// Reports whether the value is null for `PropertyColumn<T>`.
	pub fn is_null(&self, id: AtomId) -> bool {
		self.present.dense_index(id).is_some_and(|index| !self.validity.get(index))
	}

	/// Looks up the requested entry without modifying the collection.
	pub fn get(&self, id: AtomId) -> Option<&T> {
		self.values.get(id)
	}

	/// Returns state for `PropertyColumn<T>`.
	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "a valid property slot must have a dense value"
	)]
	pub fn state(&self, id: AtomId) -> PropertyState<&T> {
		let Some(index) = self.present.dense_index(id) else {
			return PropertyState::None;
		};
		if !self.validity.get(index) {
			return PropertyState::Null;
		}
		PropertyState::Value(
			self.values.get(id).expect("a valid property slot must have a dense value"),
		)
	}

	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "inserted property must be present"
	)]
	/// Returns set for `PropertyColumn<T>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn set(&mut self, id: AtomId, value: T) -> FerrosGrynnResult<Option<T>> {
		self.ensure_present(id)?;
		let previous = self.values.insert(id, value)?;
		let index = self.present.dense_index(id).expect("inserted property must be present");
		self.validity.set(index, true);
		Ok(previous)
	}

	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "inserted property must be present"
	)]
	/// Sets null for `PropertyColumn<T>`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn set_null(&mut self, id: AtomId) -> FerrosGrynnResult<Option<T>> {
		self.ensure_present(id)?;
		let previous = self.values.remove(id);
		let index = self.present.dense_index(id).expect("inserted property must be present");
		self.validity.set(index, false);
		Ok(previous)
	}

	/// Removes the requested entry and returns it when present.
	pub fn remove(&mut self, id: AtomId) -> Option<PropertyState<T>> {
		let index = self.present.dense_index(id)?;
		let value = self.values.remove(id);
		self.present.remove(id)?;
		// Validity follows the presence pool, whose dense order changes
		// independently of values.
		self.validity.swap_remove(index);
		Some(value.map_or(PropertyState::Null, PropertyState::Value))
	}

	/// Borrows the elements of this `PropertyColumn<T>` for iteration.
	pub fn iter(&self) -> impl ExactSizeIterator<Item = (AtomId, PropertyState<&T>)> {
		self.present.atoms().iter().copied().map(|id| (id, self.state(id)))
	}

	/// Returns values for `PropertyColumn<T>`.
	pub fn values(&self) -> impl ExactSizeIterator<Item = (AtomId, &T)> {
		self.values.iter()
	}

	/// Borrows the stored validity.words of this `PropertyColumn<T>`.
	pub fn validity_words(&self) -> &[u64] {
		&self.validity.words
	}

	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "replaced property must be present"
	)]
	/// Ensures present.
	fn ensure_present(&mut self, id: AtomId) -> FerrosGrynnResult<()> {
		let prior = self.present.occupant(id);
		let prior_len = self.present.len();
		self.present.insert(id, ())?;

		if self.present.len() > prior_len {
			self.validity.push(false);
		} else {
			let index = self.present.dense_index(id).expect("replaced property must be present");
			if let Some(prior) = prior.filter(|prior| *prior != id) {
				self.values.remove(prior);
			}
			self.validity.set(index, false);
		}
		Ok(())
	}
}

impl<T> Default for PropertyColumn<T> {
	/// Constructs the default initial state of `PropertyColumn<T>`.
	fn default() -> Self {
		Self::new(PropertyKeyId::new(0))
	}
}

impl<T: 'static + Send + Sync> AnyPool for PropertyColumn<T> {
	/// Removes atom for `PropertyColumn<T>`.
	fn remove_atom(&mut self, id: AtomId) {
		self.remove(id);
	}

	/// Tests membership of atom for `PropertyColumn<T>`.
	fn contains_atom(&self, id: AtomId) -> bool {
		self.contains(id)
	}

	/// Returns the logical element count of this `PropertyColumn<T>`.
	fn len(&self) -> usize {
		self.len()
	}

	/// Returns atoms for `PropertyColumn<T>`.
	fn atoms(&self) -> &[AtomId] {
		self.present.atoms()
	}

	/// Retains atoms for `PropertyColumn<T>`.
	fn retain_atoms(&mut self, predicate: &mut dyn FnMut(AtomId) -> bool) {
		let removed =
			self.present.atoms().iter().copied().filter(|id| !predicate(*id)).collect::<Vec<_>>();
		for id in removed {
			self.remove(id);
		}
	}

	/// Borrows any for `PropertyColumn<T>`.
	fn as_any(&self) -> &dyn Any {
		self
	}

	/// Borrows any mut for `PropertyColumn<T>`.
	fn as_any_mut(&mut self) -> &mut dyn Any {
		self
	}
}

#[derive(Debug, Clone, Default)]
/// Dense bitmap state.
struct DenseBitmap {
	/// Words associated with this dense bitmap.
	words: Vec<u64>,
	/// Number of logical elements currently stored.
	len: usize,
}

impl DenseBitmap {
	/// Pushes dense bitmap.
	fn push(&mut self, value: bool) {
		if self.len.is_multiple_of(u64::BITS as usize) {
			self.words.push(0);
		}
		let index = self.len;
		self.len += 1;
		self.set(index, value);
	}

	/// Returns dense bitmap.
	fn get(&self, index: usize) -> bool {
		assert!(index < self.len, "bitmap index out of bounds");
		self.words[index / 64] & (1_u64 << (index % 64)) != 0
	}

	/// Sets dense bitmap.
	fn set(&mut self, index: usize, value: bool) {
		assert!(index < self.len, "bitmap index out of bounds");
		let mask = 1_u64 << (index % 64);
		let word = &mut self.words[index / 64];
		if value {
			*word |= mask;
		} else {
			*word &= !mask;
		}
	}

	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "non-empty bitmap must have a word"
	)]
	/// Swaps remove.
	fn swap_remove(&mut self, index: usize) {
		assert!(index < self.len, "bitmap index out of bounds");
		let last = self.len - 1;
		if index != last {
			let value = self.get(last);
			self.set(index, value);
		}
		self.len -= 1;
		// includes self.len == 0
		if self.len.is_multiple_of(64) {
			self.words.pop();
		} else {
			let mask = 1_u64 << (self.len % 64);
			let last_word = self.words.last_mut().expect("non-empty bitmap must have a word");
			*last_word &= !mask;
		}
	}
}

#[cfg(test)]
/// Regression tests for property column behavior.
mod tests {
	use super::{PropertyColumn, PropertyState};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::runtime::{PropertyKeyId, graph::AtomId};

	/// Returns atom for this module.
	fn atom(slot: u64, generation: u32) -> AtomId {
		AtomId::from_raw((u64::from(generation) << 41) | slot)
	}

	/// Verifies distinguishes none null and value.
	#[test]
	fn distinguishes_none_null_and_value() {
		let mut column = PropertyColumn::<i32>::new(PropertyKeyId::new(4));
		let none = atom(0, 0);
		let null = atom(1, 0);
		let value = atom(2, 0);

		column.set_null(null).assert_ok();
		column.set(value, 42).assert_ok();
		assert_eq!(column.state(none), PropertyState::None);
		assert_eq!(column.state(null), PropertyState::Null);
		assert_eq!(column.state(value), PropertyState::Value(&42));
		assert_eq!(column.len(), 2);
		assert_eq!(column.value_len(), 1);
	}

	/// Verifies replacement removal and generation keep bitmaps aligned.
	#[test]
	fn replacement_removal_and_generation_keep_bitmaps_aligned() {
		let mut column = PropertyColumn::<usize>::new(PropertyKeyId::new(7));
		let ids = (0..130).map(|slot| atom(slot, 0)).collect::<Vec<_>>();
		for (index, id) in ids.iter().copied().enumerate() {
			if index % 3 == 0 {
				column.set_null(id).assert_ok();
			} else {
				column.set(id, index).assert_ok();
			}
		}

		for id in ids.iter().step_by(5).copied() {
			column.remove(id);
		}
		for (id, state) in column.iter() {
			assert_eq!(column.is_null(id), matches!(state, PropertyState::Null));
		}

		let old = atom(129, 0);
		let current = atom(129, 1);
		column.set(current, 999).assert_ok();
		assert_eq!(column.state(old), PropertyState::None);
		assert_eq!(column.state(current), PropertyState::Value(&999));
	}

	/// Verifies removing null reports null instead of none.
	#[test]
	fn removing_null_reports_null_instead_of_none() {
		let id = atom(9, 0);
		let mut column = PropertyColumn::<u8>::new(PropertyKeyId::new(1));
		column.set_null(id).assert_ok();
		assert_eq!(column.remove(id), Some(PropertyState::Null));
		assert_eq!(column.remove(id), None);
	}
}
