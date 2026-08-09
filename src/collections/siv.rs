use std::ops::{Deref, Index, IndexMut};

/// Alias the differentiate between IDs and index.
/// An ID allows to access the data through the index vector and is associated
/// with the same object until its erased. An index is simply the current
/// position of the object in the data vector and may change with deletions.
type Id = usize;

const INVALID_ID: Id = usize::MAX;

struct Metadata {
	rid: Id,
	validity_id: Id,
}

/// Stable Index Vector.
/// A vector that provide stable IDs when adding objects.
/// These IDs will still allow to access their associated objects even after
/// inserting of removing other objects. This comes at the cost of a small
/// overhead because of an additional indirection.
pub struct SIV<T> {
	data: Vec<T>,
	metadata: Vec<Metadata>,
	indices: Vec<Id>,
}

impl<T> SIV<T> {
	pub fn new() -> SIV<T> {
		Self::default()
	}

	pub fn with_capacity(capacity: usize) -> SIV<T> {
		Self {
			data: Vec::with_capacity(capacity),
			metadata: Vec::with_capacity(capacity),
			indices: Vec::with_capacity(capacity),
		}
	}

	/// Copies the provided object at the end of the vector.
	pub fn push(&mut self, value: T) -> Id {
		let id = self.get_free_slot();
		self.data.push(value);
		id
	}

	/// Removes the object from the vector.
	/// Param `id` - The ID of the object to remove.
	pub fn remove(&mut self, id: Id) -> T {
		let data_id = self.indices[id];
		let last_data_id = self.data.len() - 1;
		let last_id = self.metadata[last_data_id].rid;
		self.metadata[data_id].validity_id += 1;
		self.data.swap(data_id, last_data_id);
		self.metadata.swap(data_id, last_data_id);
		self.indices.swap(id, last_id);
		self.data.pop().unwrap()
	}

	/// Removes the object from the vector
	/// Param `idx` - The index in the data vector of the object to remove.
	pub fn remove_via_data(&mut self, idx: usize) {
		self.remove(self.metadata[idx].rid);
	}

	/// Removes the object referenced by the handle from the vector
	pub fn erase(&mut self, handle: &Handle<'_, T>) {
		// TODO: asset that `handle.vector` equals to `self`
		assert!(handle.is_valid());
		self.remove(handle.id());
	}

	/// Return the index in the data vector of the object referenced by the
	/// provided ID.
	pub fn data_index(&self, id: Id) -> usize {
		self.indices[id]
	}

	/// Returns the number of objects in the vector
	pub fn len(&self) -> usize {
		self.data.len()
	}

	/// Tells if the vector is currently empty
	pub fn is_empty(&self) -> bool {
		self.data.is_empty()
	}

	/// Returns the vector's capacity (i.e. the number of allocated slots in the
	/// vector)
	pub fn capacity(&self) -> usize {
		self.data.capacity()
	}

	/// Creates a handle pointing to the provided ID.
	pub fn create_handle(&self, id: Id) -> Handle<'_, T> {
		assert!(self.data_index(id) < self.len());
		Handle::new(id, self.metadata[self.indices[id]].validity_id, self)
	}

	pub fn create_handle_from_data(&self, idx: usize) -> Handle<'_, T> {
		assert!(idx < self.len());
		Handle::new(self.metadata[idx].rid, self.metadata[idx].validity_id, self)
	}

	/// Checks if the provided object is still valid considering its last known
	/// validity ID. Param `validity_id` - The last known validity ID.
	pub fn is_valid(&self, id: Id, validity_id: Id) -> bool {
		validity_id == self.metadata[self.indices[id]].validity_id
	}

	pub fn validity_id(&self, id: Id) -> Id {
		self.metadata[self.indices[id]].validity_id
	}

	pub fn data(&self) -> &Vec<T> {
		&self.data
	}

	pub fn next_id(&self) -> Id {
		if self.metadata.len() > self.data.len() {
			self.metadata[self.data().len()].rid
		} else {
			self.data.len()
		}
	}

	pub fn clear(&mut self) {
		self.data.clear();
		self.metadata.iter_mut().for_each(|m| m.validity_id += 1);
	}

	pub fn iv_valid_id(&self, id: Id) -> bool {
		id < self.indices.len()
	}

	fn get_free_id(&mut self) -> Id {
		if self.metadata.len() > self.data.len() {
			self.metadata[self.data.len()].validity_id += 1;
			self.metadata[self.data.len()].rid
		} else {
			let new_id = self.data.len();
			self.metadata.push(Metadata {
				rid: new_id,
				validity_id: 0,
			});
			self.indices.push(new_id);
			new_id
		}
	}

	fn get_free_slot(&mut self) -> Id {
		let id = self.get_free_id();
		self.indices[id] = self.data.len();
		id
	}
}

impl<T> Index<Id> for SIV<T> {
	type Output = T;

	fn index(&self, index: Id) -> &Self::Output {
		unsafe { self.data.get_unchecked(self.indices[index]) }
	}
}

impl<T> IndexMut<Id> for SIV<T> {
	fn index_mut(&mut self, index: Id) -> &mut Self::Output {
		unsafe { self.data.get_unchecked_mut(self.indices[index]) }
	}
}

impl<T> Default for SIV<T> {
	fn default() -> Self {
		Self {
			data: Vec::default(),
			metadata: Vec::default(),
			indices: Vec::default(),
		}
	}
}

/// A standalone struct allowing to access an object without the need to have a
/// reference to the containing Vector.
pub struct Handle<'a, T> {
	/// The ID of the object.
	id: Id,
	/// The validity ID of the object at the time of creation. Used to check the
	/// validity of the handle.
	validity_id: Id,
	/// A reference to the vector containing the object associated with this
	/// handle
	vector: &'a SIV<T>,
}

impl<'a, T> Handle<'a, T> {
	pub fn new(id: Id, validity_id: Id, vector: &'a SIV<T>) -> Self {
		Self {
			id,
			validity_id,
			vector,
		}
	}

	pub fn get(&self) -> &T {
		&self.vector[self.id]
	}

	/// Returns the ID of the associated object
	pub fn id(&self) -> Id {
		self.id
	}

	/// Check if the reference is associated with a vector and has a correct
	/// validity ID. Returns `false` if uninitialized or if the object has been
	/// erased from the vector, `true` otherwise
	pub fn is_valid(&self) -> bool {
		self.vector.is_valid(self.id, self.validity_id)
	}
}

impl<'a, T> Deref for Handle<'a, T> {
	type Target = T;

	fn deref(&self) -> &Self::Target {
		&self.vector[self.id]
	}
}
