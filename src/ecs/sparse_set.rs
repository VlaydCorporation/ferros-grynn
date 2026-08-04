use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::graph::{AtomId, AtomKind},
};

pub const SPARSE_PAGE_LEN: usize = 4096;
const VACANT: u32 = u32::MAX;

#[derive(Debug, Clone, Default)]
struct SparsePages {
	pages: Vec<Option<Box<[u32]>>>,
}

impl SparsePages {
	fn get(&self, slot: u64) -> Option<u32> {
		let page = usize::try_from(slot / SPARSE_PAGE_LEN as u64).ok()?;
		let offset = slot as usize % SPARSE_PAGE_LEN;
		let value = *self.pages.get(page)?.as_ref()?.get(offset)?;
		(value != VACANT).then_some(value)
	}

	fn set(&mut self, slot: u64, dense: Option<u32>) -> FerrosGrynnResult<()> {
		let page = usize::try_from(slot / SPARSE_PAGE_LEN as u64)
			.map_err(|_| FerrosGrynnError::graph_capacity_exceeded("ECS sparse index"))?;
		let offset = slot as usize % SPARSE_PAGE_LEN;
		if page >= self.pages.len() {
			self.pages.resize_with(page + 1, || None);
		}
		let page = self.pages[page]
			.get_or_insert_with(|| vec![VACANT; SPARSE_PAGE_LEN].into_boxed_slice());
		page[offset] = dense.unwrap_or(VACANT);
		Ok(())
	}

	fn allocated_pages(&self) -> usize {
		self.pages.iter().filter(|page| page.is_some()).count()
	}
}

/// Generation-aware sparse set keyed by the complete graph [`AtomId`].
///
/// Node and edge slot spaces use different paged sparse indexes. A sparse hit
/// is accepted only if the dense atom equals the requested ID, including its
/// generation.
#[derive(Debug, Clone)]
pub struct SparseSet<T> {
	node_sparse: SparsePages,
	edge_sparse: SparsePages,
	dense_atoms: Vec<AtomId>,
	dense_components: Vec<T>,
}

impl<T> SparseSet<T> {
	pub fn new() -> Self {
		Self {
			node_sparse: SparsePages::default(),
			edge_sparse: SparsePages::default(),
			dense_atoms: Vec::new(),
			dense_components: Vec::new(),
		}
	}

	pub fn with_capacity(dense_capacity: usize) -> Self {
		let mut result = Self::new();
		result.dense_atoms.reserve(dense_capacity);
		result.dense_components.reserve(dense_capacity);
		result
	}

	pub fn len(&self) -> usize {
		self.dense_components.len()
	}

	pub fn is_empty(&self) -> bool {
		self.dense_components.is_empty()
	}

	pub fn insert(&mut self, id: AtomId, component: T) -> FerrosGrynnResult<Option<T>> {
		let pages = self.pages(id)?;
		if let Some(index) = pages.get(id.slot()).map(|index| index as usize) {
			debug_assert!(index < self.dense_atoms.len());
			self.dense_atoms[index] = id;
			return Ok(Some(std::mem::replace(&mut self.dense_components[index], component)));
		}

		let index = u32::try_from(self.dense_components.len())
			.map_err(|_| FerrosGrynnError::graph_capacity_exceeded("ECS component pool"))?;
		self.pages_mut(id)?.set(id.slot(), Some(index))?;
		self.dense_atoms.push(id);
		self.dense_components.push(component);
		Ok(None)
	}

	pub fn remove(&mut self, id: AtomId) -> Option<T> {
		let index = self.dense_index(id)?;
		let last = self.dense_components.len() - 1;
		self.pages_mut(id).ok()?.set(id.slot(), None).ok()?;

		self.dense_atoms.swap_remove(index);
		let removed = self.dense_components.swap_remove(index);
		if index != last {
			let moved = self.dense_atoms[index];
			self.pages_mut(moved).ok()?.set(moved.slot(), Some(index as u32)).ok()?;
		}
		Some(removed)
	}

	pub fn get(&self, id: AtomId) -> Option<&T> {
		self.dense_index(id).map(|index| &self.dense_components[index])
	}

	pub fn get_mut(&mut self, id: AtomId) -> Option<&mut T> {
		let index = self.dense_index(id)?;
		Some(&mut self.dense_components[index])
	}

	pub fn contains(&self, id: AtomId) -> bool {
		self.dense_index(id).is_some()
	}

	pub fn atoms(&self) -> &[AtomId] {
		&self.dense_atoms
	}

	pub fn components(&self) -> &[T] {
		&self.dense_components
	}

	pub fn components_mut(&mut self) -> &mut [T] {
		&mut self.dense_components
	}

	pub fn iter(&self) -> impl ExactSizeIterator<Item = (AtomId, &T)> {
		self.dense_atoms.iter().copied().zip(&self.dense_components)
	}

	pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (AtomId, &mut T)> {
		self.dense_atoms.iter().copied().zip(&mut self.dense_components)
	}

	pub fn clear(&mut self) {
		self.node_sparse = SparsePages::default();
		self.edge_sparse = SparsePages::default();
		self.dense_atoms.clear();
		self.dense_components.clear();
	}

	pub fn retain(&mut self, mut predicate: impl FnMut(AtomId, &mut T) -> bool) {
		let mut index = 0;
		while index < self.len() {
			let id = self.dense_atoms[index];
			if predicate(id, &mut self.dense_components[index]) {
				index += 1;
			} else {
				self.remove(id);
			}
		}
	}

	pub fn allocated_sparse_pages(&self) -> usize {
		self.node_sparse.allocated_pages() + self.edge_sparse.allocated_pages()
	}

	pub(crate) fn dense_index(&self, id: AtomId) -> Option<usize> {
		let index = self.pages(id).ok()?.get(id.slot())? as usize;
		(self.dense_atoms.get(index) == Some(&id)).then_some(index)
	}

	pub(crate) fn occupant(&self, id: AtomId) -> Option<AtomId> {
		let index = self.pages(id).ok()?.get(id.slot())? as usize;
		self.dense_atoms.get(index).copied()
	}

	fn pages(&self, id: AtomId) -> FerrosGrynnResult<&SparsePages> {
		match id.kind() {
			Some(AtomKind::Node) => Ok(&self.node_sparse),
			Some(AtomKind::Edge) => Ok(&self.edge_sparse),
			None => Err(invalid_atom(id)),
		}
	}

	fn pages_mut(&mut self, id: AtomId) -> FerrosGrynnResult<&mut SparsePages> {
		match id.kind() {
			Some(AtomKind::Node) => Ok(&mut self.node_sparse),
			Some(AtomKind::Edge) => Ok(&mut self.edge_sparse),
			None => Err(invalid_atom(id)),
		}
	}
}

impl<T> Default for SparseSet<T> {
	fn default() -> Self {
		Self::new()
	}
}

fn invalid_atom(id: AtomId) -> FerrosGrynnError {
	FerrosGrynnError::atom_does_not_exist("atom", id.to_string())
}

#[cfg(test)]
mod tests {
	use super::{SPARSE_PAGE_LEN, SparseSet};
	use crate::values::runtime::graph::AtomId;

	fn atom(slot: u64, generation: u32, edge: bool) -> AtomId {
		let kind = if edge {
			1_u64 << 40
		} else {
			0
		};
		AtomId::from_raw((u64::from(generation) << 41) | kind | slot)
	}

	#[test]
	fn uses_separate_lazy_pages_for_node_and_edge_slots() {
		let mut set = SparseSet::new();
		let node = atom(SPARSE_PAGE_LEN as u64 + 7, 0, false);
		let edge = atom(SPARSE_PAGE_LEN as u64 + 7, 0, true);

		set.insert(node, "node").unwrap();
		set.insert(edge, "edge").unwrap();
		assert_eq!(set.get(node), Some(&"node"));
		assert_eq!(set.get(edge), Some(&"edge"));
		assert_eq!(set.allocated_sparse_pages(), 2);
	}

	#[test]
	fn stale_generation_cannot_observe_or_remove_current_component() {
		let old = atom(17, 2, false);
		let current = atom(17, 3, false);
		let mut set = SparseSet::new();

		set.insert(old, 10).unwrap();
		assert_eq!(set.insert(current, 20).unwrap(), Some(10));
		assert_eq!(set.get(old), None);
		assert_eq!(set.get(current), Some(&20));
		assert_eq!(set.remove(old), None);
		assert_eq!(set.remove(current), Some(20));
	}

	#[test]
	fn swap_remove_repairs_sparse_index_and_retain_visits_moved_values() {
		let mut set = SparseSet::new();
		for slot in 0..8 {
			set.insert(atom(slot, 0, false), slot).unwrap();
		}
		assert_eq!(set.remove(atom(2, 0, false)), Some(2));
		assert_eq!(set.get(atom(7, 0, false)), Some(&7));

		set.retain(|_, value| *value % 2 == 0);
		let mut values = set.components().to_vec();
		values.sort_unstable();
		assert_eq!(values, [0, 4, 6]);
	}

	#[test]
	fn rejects_null_atom() {
		let error = SparseSet::<u8>::new().insert(AtomId::NULL, 1).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-G1F01");
	}
}
