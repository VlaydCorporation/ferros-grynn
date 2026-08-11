use std::any::Any;

use super::{AnyPool, SparseSet};
use crate::{
	gql_status::FerrosGrynnResult,
	values::runtime::{PropertyKeyId, graph::AtomId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PropertyState<T> {
	None,
	Null,
	Value(T),
}

/// Typed ECS materialization of one graph property.
///
/// Presence and validity are independent: absence in `present` is `none`, a
/// cleared validity bit is `null`, and a set bit has a value in `values`.
#[derive(Debug, Clone)]
pub struct PropertyColumn<T> {
	key: PropertyKeyId,
	present: SparseSet<()>,
	values: SparseSet<T>,
	validity: DenseBitmap,
}

impl<T> PropertyColumn<T> {
	pub fn new(key: PropertyKeyId) -> Self {
		Self {
			key,
			present: SparseSet::new(),
			values: SparseSet::new(),
			validity: DenseBitmap::default(),
		}
	}

	pub const fn key(&self) -> PropertyKeyId {
		self.key
	}

	pub const fn len(&self) -> usize {
		self.present.len()
	}

	pub const fn value_len(&self) -> usize {
		self.values.len()
	}

	pub const fn is_empty(&self) -> bool {
		self.present.is_empty()
	}

	pub fn contains(&self, id: AtomId) -> bool {
		self.present.contains(id)
	}

	pub fn is_null(&self, id: AtomId) -> bool {
		self.present.dense_index(id).is_some_and(|index| !self.validity.get(index))
	}

	pub fn get(&self, id: AtomId) -> Option<&T> {
		self.values.get(id)
	}

	#[allow(clippy::expect_used, clippy::missing_panics_doc, reason = "a valid property slot must have a dense value")]
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

	#[allow(clippy::expect_used, clippy::missing_panics_doc, reason = "inserted property must be present")]
	pub fn set(&mut self, id: AtomId, value: T) -> FerrosGrynnResult<Option<T>> {
		self.ensure_present(id)?;
		let previous = self.values.insert(id, value)?;
		let index = self.present.dense_index(id).expect("inserted property must be present");
		self.validity.set(index, true);
		Ok(previous)
	}

	#[allow(clippy::expect_used, clippy::missing_panics_doc, reason = "inserted property must be present")]
	pub fn set_null(&mut self, id: AtomId) -> FerrosGrynnResult<Option<T>> {
		self.ensure_present(id)?;
		let previous = self.values.remove(id);
		let index = self.present.dense_index(id).expect("inserted property must be present");
		self.validity.set(index, false);
		Ok(previous)
	}

	pub fn remove(&mut self, id: AtomId) -> Option<PropertyState<T>> {
		let index = self.present.dense_index(id)?;
		let value = self.values.remove(id);
		self.present.remove(id)?;
		self.validity.swap_remove(index);
		Some(value.map_or(PropertyState::Null, PropertyState::Value))
	}

	pub fn iter(&self) -> impl ExactSizeIterator<Item = (AtomId, PropertyState<&T>)> {
		self.present.atoms().iter().copied().map(|id| (id, self.state(id)))
	}

	pub fn values(&self) -> impl ExactSizeIterator<Item = (AtomId, &T)> {
		self.values.iter()
	}

	pub fn validity_words(&self) -> &[u64] {
		&self.validity.words
	}

	#[allow(clippy::expect_used, clippy::missing_panics_doc, reason = "replaced property must be present")]
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
	fn default() -> Self {
		Self::new(PropertyKeyId::new(0))
	}
}

impl<T: 'static + Send + Sync> AnyPool for PropertyColumn<T> {
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
		self.present.atoms()
	}

	fn retain_atoms(&mut self, predicate: &mut dyn FnMut(AtomId) -> bool) {
		let removed =
			self.present.atoms().iter().copied().filter(|id| !predicate(*id)).collect::<Vec<_>>();
		for id in removed {
			self.remove(id);
		}
	}

	fn as_any(&self) -> &dyn Any {
		self
	}

	fn as_any_mut(&mut self) -> &mut dyn Any {
		self
	}
}

#[derive(Debug, Clone, Default)]
struct DenseBitmap {
	words: Vec<u64>,
	len: usize,
}

impl DenseBitmap {
	fn push(&mut self, value: bool) {
		if self.len.is_multiple_of(u64::BITS as usize) {
			self.words.push(0);
		}
		let index = self.len;
		self.len += 1;
		self.set(index, value);
	}

	fn get(&self, index: usize) -> bool {
		assert!(index < self.len, "bitmap index out of bounds");
		self.words[index / 64] & (1_u64 << (index % 64)) != 0
	}

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

	#[allow(clippy::expect_used, clippy::missing_panics_doc, reason = "non-empty bitmap must have a word")]
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
mod tests {
	use super::{PropertyColumn, PropertyState};
	use crate::values::runtime::{PropertyKeyId, graph::AtomId};

	fn atom(slot: u64, generation: u32) -> AtomId {
		AtomId::from_raw((u64::from(generation) << 41) | slot)
	}

	#[test]
	fn distinguishes_none_null_and_value() {
		let mut column = PropertyColumn::<i32>::new(PropertyKeyId::new(4));
		let none = atom(0, 0);
		let null = atom(1, 0);
		let value = atom(2, 0);

		column.set_null(null).unwrap();
		column.set(value, 42).unwrap();
		assert_eq!(column.state(none), PropertyState::None);
		assert_eq!(column.state(null), PropertyState::Null);
		assert_eq!(column.state(value), PropertyState::Value(&42));
		assert_eq!(column.len(), 2);
		assert_eq!(column.value_len(), 1);
	}

	#[test]
	fn replacement_removal_and_generation_keep_bitmaps_aligned() {
		let mut column = PropertyColumn::<usize>::new(PropertyKeyId::new(7));
		let ids = (0..130).map(|slot| atom(slot, 0)).collect::<Vec<_>>();
		for (index, id) in ids.iter().copied().enumerate() {
			if index % 3 == 0 {
				column.set_null(id).unwrap();
			} else {
				column.set(id, index).unwrap();
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
		column.set(current, 999).unwrap();
		assert_eq!(column.state(old), PropertyState::None);
		assert_eq!(column.state(current), PropertyState::Value(&999));
	}

	#[test]
	fn removing_null_reports_null_instead_of_none() {
		let id = atom(9, 0);
		let mut column = PropertyColumn::<u8>::new(PropertyKeyId::new(1));
		column.set_null(id).unwrap();
		assert_eq!(column.remove(id), Some(PropertyState::Null));
		assert_eq!(column.remove(id), None);
	}
}
