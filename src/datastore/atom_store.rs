use std::{
	collections::BTreeMap,
	fmt::{Debug, Display, Formatter},
	sync::Arc,
};

use crate::{
	datastore::{ClassId, ClassSchema},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId},
	values::{PropertyMap, RecordId},
};

/// Largest generation representable by the shared `AtomId` layout.
const MAX_GENERATION: u32 = (1 << 23) - 1;

/// One slot namespace shared by database-global nodes and edges.
#[derive(Debug, Clone, Default)]
struct GlobalSlotAllocator {
	generations: Vec<u32>,
	occupied: Vec<bool>,
	free: Vec<u64>,
}

impl GlobalSlotAllocator {
	fn alloc(&mut self, kind: AtomKind) -> FerrosGrynnResult<GlobalAtomId> {
		let slot = match self.free.pop() {
			Some(slot) => slot,
			None => {
				let slot = u64::try_from(self.generations.len()).map_err(|_error| {
					FerrosGrynnError::graph_capacity_exceeded("global atom store")
				})?;
				self.generations.push(0);
				self.occupied.push(false);
				slot
			}
		};
		let index = usize::try_from(slot)
			.map_err(|_error| FerrosGrynnError::graph_capacity_exceeded("global atom store"))?;
		self.occupied[index] = true;
		AtomId::from_parts(kind, slot, self.generations[index]).map(GlobalAtomId)
	}

	fn restore(&mut self, id: GlobalAtomId) -> FerrosGrynnResult<()> {
		let index = usize::try_from(id.slot())
			.map_err(|_error| FerrosGrynnError::graph_capacity_exceeded("global atom store"))?;
		if id.generation() > MAX_GENERATION {
			return Err(FerrosGrynnError::graph_capacity_exceeded("global atom store"));
		}
		self.extend_through(index)?;
		if self.occupied[index] {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				"restored global slot is already occupied",
			));
		}
		self.generations[index] = id.generation();
		self.occupied[index] = true;
		self.free.retain(|slot| *slot != id.slot());
		Ok(())
	}

	fn restore_free(&mut self, slot: u64, generation: u32) -> FerrosGrynnResult<()> {
		let index = usize::try_from(slot)
			.map_err(|_error| FerrosGrynnError::graph_capacity_exceeded("global atom store"))?;
		if generation > MAX_GENERATION {
			return Err(FerrosGrynnError::graph_capacity_exceeded("global atom store"));
		}
		self.extend_through(index)?;
		if self.occupied[index] {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				"restored global free slot is occupied",
			));
		}
		self.generations[index] = generation;
		self.free.retain(|free| *free != slot);
		if generation < MAX_GENERATION {
			self.free.push(slot);
		}
		Ok(())
	}

	fn free(&mut self, id: GlobalAtomId) -> FerrosGrynnResult<()> {
		let index = usize::try_from(id.slot())
			.map_err(|_error| FerrosGrynnError::graph_capacity_exceeded("global atom store"))?;
		if self.occupied.get(index) != Some(&true)
			|| self.generations.get(index).copied() != Some(id.generation())
		{
			return Err(FerrosGrynnError::atom_does_not_exist("global", id.to_string()));
		}
		self.occupied[index] = false;
		if self.generations[index] < MAX_GENERATION {
			self.generations[index] += 1;
			self.free.push(id.slot());
		}
		Ok(())
	}

	fn extend_through(&mut self, index: usize) -> FerrosGrynnResult<()> {
		while self.generations.len() <= index {
			let slot = u64::try_from(self.generations.len())
				.map_err(|_error| FerrosGrynnError::graph_capacity_exceeded("global atom store"))?;
			self.generations.push(0);
			self.occupied.push(false);
			self.free.push(slot);
		}
		Ok(())
	}

	const fn capacity(&self) -> usize {
		self.generations.len()
	}

	fn generation(&self, slot: usize) -> Option<u32> {
		self.generations.get(slot).copied()
	}
}

/// Generation-aware address of one database-global atom.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct GlobalAtomId(AtomId);

impl GlobalAtomId {
	/// Restores an address from its durable packed representation.
	pub(crate) const fn from_raw(raw: u64) -> Self {
		Self(AtomId::from_raw(raw))
	}

	/// Restores a non-null identity from an untrusted durable integer.
	pub(crate) fn from_storage(raw: u64) -> FerrosGrynnResult<Self> {
		let atom = AtomId::from_raw(raw);
		if atom.is_null() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"global atom identity uses the reserved null value",
			));
		}
		Ok(Self(atom))
	}

	/// Returns the packed durable representation.
	pub(crate) const fn raw(self) -> u64 {
		self.0.raw()
	}

	/// Returns the immutable atom kind encoded in the address.
	pub(crate) const fn kind(self) -> Option<AtomKind> {
		self.0.kind()
	}

	/// Returns the allocator slot.
	pub(crate) const fn slot(self) -> u64 {
		self.0.slot()
	}

	/// Returns the slot generation.
	pub(crate) const fn generation(self) -> u32 {
		self.0.generation()
	}
}

impl Debug for GlobalAtomId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
		formatter.debug_tuple("GlobalAtomId").field(&self.0).finish()
	}
}

impl Display for GlobalAtomId {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
		Display::fmt(&self.0, formatter)
	}
}

/// Database-global identity, class, and base properties of one atom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaseAtom {
	id: GlobalAtomId,
	record_id: RecordId,
	class_id: ClassId,
	properties: PropertyMap,
}

impl BaseAtom {
	pub(crate) const fn id(&self) -> GlobalAtomId {
		self.id
	}

	pub(crate) const fn record_id(&self) -> &RecordId {
		&self.record_id
	}

	pub(crate) const fn class_id(&self) -> ClassId {
		self.class_id
	}

	pub(crate) fn kind(&self) -> AtomKind {
		match self.id.kind() {
			Some(kind) => kind,
			None => unreachable!(),
		}
	}

	pub(crate) const fn properties(&self) -> &PropertyMap {
		&self.properties
	}

	pub(crate) fn properties_mut(&mut self) -> &mut PropertyMap {
		&mut self.properties
	}
}

/// Database-global atom registry and exact reverse graph memberships.
#[derive(Debug, Clone)]
pub(crate) struct AtomStoreRoot {
	allocator: GlobalSlotAllocator,
	primary: BTreeMap<RecordId, GlobalAtomId>,
	atoms: BTreeMap<GlobalAtomId, Arc<BaseAtom>>,
	memberships: BTreeMap<GlobalAtomId, BTreeMap<GraphId, AtomId>>,
}

impl Default for AtomStoreRoot {
	fn default() -> Self {
		Self {
			allocator: GlobalSlotAllocator::default(),
			primary: BTreeMap::new(),
			atoms: BTreeMap::new(),
			memberships: BTreeMap::new(),
		}
	}
}

impl AtomStoreRoot {
	pub(crate) fn insert(
		&mut self,
		class: &ClassSchema,
		record_id: RecordId,
		properties: PropertyMap,
	) -> FerrosGrynnResult<GlobalAtomId> {
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("an atom requires a published class")
		})?;
		if record_id.class() != class.name().as_str() {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"RecordId class '{}' does not match class '{}'",
				record_id.class(),
				class.name()
			)));
		}
		if self.primary.contains_key(&record_id) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				format!("atom '{record_id}' already exists"),
			));
		}

		let id = self.allocator.alloc(class.kind())?;
		self.insert_record(id, class_id, record_id, properties);
		Ok(id)
	}

	pub(crate) fn insert_at(
		&mut self,
		id: GlobalAtomId,
		class: &ClassSchema,
		record_id: RecordId,
		properties: PropertyMap,
	) -> FerrosGrynnResult<()> {
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("an atom requires a published class")
		})?;
		if id.kind() != Some(class.kind()) || record_id.class() != class.name().as_str() {
			return Err(FerrosGrynnError::invalid_argument(
				"restored atom identity, class, and RecordId disagree",
			));
		}
		if self.primary.contains_key(&record_id) || self.atoms.contains_key(&id) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				"restored atom identity is already occupied",
			));
		}
		self.allocator.restore(id)?;
		self.insert_record(id, class_id, record_id, properties);
		Ok(())
	}

	fn insert_record(
		&mut self,
		id: GlobalAtomId,
		class_id: ClassId,
		record_id: RecordId,
		properties: PropertyMap,
	) {
		self.primary.insert(record_id.clone(), id);
		self.atoms.insert(
			id,
			Arc::new(BaseAtom {
				id,
				record_id,
				class_id,
				properties,
			}),
		);
	}

	pub(crate) fn resolve(&self, record_id: &RecordId) -> Option<GlobalAtomId> {
		self.primary.get(record_id).copied()
	}

	pub(crate) fn atom(&self, id: GlobalAtomId) -> Option<&BaseAtom> {
		self.atoms.get(&id).filter(|atom| atom.id == id).map(Arc::as_ref)
	}

	pub(crate) fn atom_arc(&self, id: GlobalAtomId) -> Option<Arc<BaseAtom>> {
		self.atoms.get(&id).filter(|atom| atom.id == id).cloned()
	}

	pub(crate) fn atom_mut(&mut self, id: GlobalAtomId) -> Option<&mut BaseAtom> {
		let atom = self.atoms.get_mut(&id)?;
		if atom.id != id {
			return None;
		}
		Some(Arc::make_mut(atom))
	}

	pub(crate) fn remove(&mut self, id: GlobalAtomId) -> FerrosGrynnResult<BaseAtom> {
		if self.memberships.get(&id).is_some_and(|memberships| !memberships.is_empty()) {
			return Err(FerrosGrynnError::graph_constraint_violation(
				"atom store",
				"a global atom with graph memberships cannot be deleted without CASCADE",
			));
		}
		let atom = self
			.atoms
			.get(&id)
			.cloned()
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", id.to_string()))?;
		if self.primary.get(atom.record_id()).copied() != Some(id) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"atom primary index disagrees with the global slot",
			));
		}
		let atom = self
			.atoms
			.remove(&id)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", id.to_string()))?;
		self.primary.remove(atom.record_id());
		self.memberships.remove(&id);
		self.allocator.free(id)?;
		Ok(Arc::try_unwrap(atom).unwrap_or_else(|atom| atom.as_ref().clone()))
	}

	pub(crate) fn atoms(&self) -> impl ExactSizeIterator<Item = &BaseAtom> {
		self.atoms.values().map(Arc::as_ref)
	}

	pub(crate) const fn slot_capacity(&self) -> usize {
		self.allocator.capacity()
	}

	pub(crate) fn slot_generation(&self, slot: usize) -> Option<u32> {
		self.allocator.generation(slot)
	}

	pub(crate) fn restore_free_slot(
		&mut self,
		slot: u64,
		generation: u32,
	) -> FerrosGrynnResult<()> {
		self.allocator.restore_free(slot, generation)
	}

	pub(crate) fn memberships(&self, id: GlobalAtomId) -> Option<&BTreeMap<GraphId, AtomId>> {
		self.memberships.get(&id)
	}

	pub(crate) fn add_membership(&mut self, global: GlobalAtomId, graph: GraphId, local: AtomId) {
		self.memberships.entry(global).or_default().insert(graph, local);
	}

	/// Restores one durable reverse membership without overwriting an existing
	/// binding.
	pub(crate) fn restore_membership(
		&mut self,
		global: GlobalAtomId,
		graph: GraphId,
		local: AtomId,
	) -> FerrosGrynnResult<()> {
		if self.atom(global).is_none() || global.kind() != local.kind() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"restored membership references an absent atom or mismatched kind",
			));
		}
		if self.memberships.get(&global).is_some_and(|memberships| memberships.contains_key(&graph))
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"restored membership duplicates a graph binding",
			));
		}
		self.memberships.entry(global).or_default().insert(graph, local);
		Ok(())
	}

	pub(crate) fn remove_membership(&mut self, global: GlobalAtomId, graph: GraphId) {
		let Some(memberships) = self.memberships.get_mut(&global) else {
			return;
		};
		memberships.remove(&graph);
		if memberships.is_empty() {
			self.memberships.remove(&global);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::RecordIdPart;

	#[test]
	fn reused_slot_rejects_the_stale_global_atom_id() {
		let mut class = ClassSchema::new("person", AtomKind::Node, []).unwrap();
		class.id = ClassId::new(1);
		let mut store = AtomStoreRoot::default();
		let first = store
			.insert(
				&class,
				RecordId::new("person", RecordIdPart::String("first".into())).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();
		store.remove(first).unwrap();
		let second = store
			.insert(
				&class,
				RecordId::new("person", RecordIdPart::String("second".into())).unwrap(),
				PropertyMap::new(),
			)
			.unwrap();

		assert_eq!(first.slot(), second.slot());
		assert_ne!(first.generation(), second.generation());
		assert!(store.atom(first).is_none());
		assert!(store.atom(second).is_some());
	}
}
