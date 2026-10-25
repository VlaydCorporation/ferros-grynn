use std::{
	collections::{BTreeMap, BTreeSet},
	ops::Bound,
};

use super::super::{ClassId, ClassIndexKind, GlobalAtomId, IndexId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId},
	values::{PropertyKeyId, Text, Type},
};

/// Resolved field metadata needed to build one scalar key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexField {
	/// Key associated with this index field.
	pub(crate) key: PropertyKeyId,
	/// Value type associated with this index field.
	pub(crate) value_type: Type,
}

/// Logical metadata and values of one explicit class index.
#[derive(Debug, Clone)]
pub struct IndexBinding<Address> {
	/// Identifier used to locate this value within its owning subsystem.
	pub(crate) id: IndexId,
	/// Version associated with this index binding.
	pub(crate) version: SchemaVersion,
	/// Identifier of the schema class.
	pub(crate) class_id: ClassId,
	/// Name associated with this index binding.
	pub(crate) name: Text,
	/// Kind associated with this index binding.
	pub(crate) kind: ClassIndexKind,
	/// Fields associated with this index binding.
	pub(crate) fields: Vec<IndexField>,
	/// Whether immutable durable entries remain the read authority until the
	/// first transaction-private mutation rebuilds mutable index state.
	pub(crate) deferred: bool,
	/// Entries associated with this index binding.
	pub(crate) entries: IndexEntries<Address>,
}

/// Exact in-memory representation selected by the declared index kind.
#[derive(Debug, Clone)]
pub enum IndexEntries<Address> {
	/// Standard case of index entries.
	Standard(
		/// Standard case of index entries.
		BTreeMap<Vec<u8>, BTreeSet<Address>>,
	),
	/// Unique case of index entries.
	Unique(
		/// Unique case of index entries.
		BTreeMap<Vec<u8>, Address>,
	),
	/// Count case of index entries.
	Count(
		/// Count case of index entries.
		u64,
	),
}

impl<Address: Copy + Ord> IndexEntries<Address> {
	/// Creates empty storage for the declared scalar index kind.
	pub(crate) fn empty(kind: ClassIndexKind) -> Self {
		match kind {
			ClassIndexKind::Standard => Self::Standard(BTreeMap::new()),
			ClassIndexKind::Unique => Self::Unique(BTreeMap::new()),
			ClassIndexKind::Count => Self::Count(0),
			ClassIndexKind::Fulltext
			| ClassIndexKind::Hnsw
			| ClassIndexKind::DiskAnn
			| ClassIndexKind::Geometry => {
				unreachable!("specialized indexes do not use the scalar index store")
			}
		}
	}

	/// Returns the count-index total, or `None` for scalar posting maps.
	pub(crate) fn count(&self) -> Option<u64> {
		match self {
			Self::Count(count) => Some(*count),
			Self::Standard(_) | Self::Unique(_) => None,
		}
	}

	/// Collects addresses matching one encoded scalar key.
	pub(crate) fn lookup(&self, key: &[u8]) -> Vec<Address> {
		match self {
			Self::Standard(entries) => {
				entries.get(key).into_iter().flat_map(BTreeSet::iter).copied().collect()
			}
			Self::Unique(entries) => entries.get(key).copied().into_iter().collect(),
			Self::Count(_) => Vec::new(),
		}
	}

	/// Visits scalar posting groups in encoded-key order and stops when the
	/// visitor has enough rows for its bounded window.
	pub(crate) fn visit_groups(
		&self,
		lower: Bound<Vec<u8>>,
		upper: Bound<Vec<u8>>,
		reverse: bool,
		mut visit: impl FnMut(&[u8], Vec<Address>) -> bool,
	) {
		match self {
			Self::Standard(entries) if reverse => {
				for (key, addresses) in entries.range((lower, upper)).rev() {
					if !visit(key, addresses.iter().copied().collect()) {
						break;
					}
				}
			}
			Self::Standard(entries) => {
				for (key, addresses) in entries.range((lower, upper)) {
					if !visit(key, addresses.iter().copied().collect()) {
						break;
					}
				}
			}
			Self::Unique(entries) if reverse => {
				for (key, address) in entries.range((lower, upper)).rev() {
					if !visit(key, vec![*address]) {
						break;
					}
				}
			}
			Self::Unique(entries) => {
				for (key, address) in entries.range((lower, upper)) {
					if !visit(key, vec![*address]) {
						break;
					}
				}
			}
			Self::Count(_) => {}
		}
	}

	/// Returns whether this value can insert.
	pub(crate) fn can_insert(
		&self,
		key: &[u8],
		address: Option<Address>,
		index_name: &Text,
	) -> FerrosGrynnResult<()> {
		if let Self::Unique(entries) = self
			&& entries.get(key).is_some_and(|existing| Some(*existing) != address)
		{
			return Err(FerrosGrynnError::graph_constraint_violation(
				index_name.to_string(),
				"unique index key is already owned by another atom",
			));
		}
		Ok(())
	}

	/// Returns the checked increment result.
	pub(crate) fn checked_increment(&self) -> FerrosGrynnResult<()> {
		if let Self::Count(count) = self {
			count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("class count index overflows u64")
			})?;
		}
		Ok(())
	}

	/// Adds a scalar key/address pair or increments a count index.
	pub(crate) fn insert(
		&mut self,
		key: Option<Vec<u8>>,
		address: Address,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Standard(entries) => {
				entries.entry(scalar_key(key)?).or_default().insert(address);
			}
			Self::Unique(entries) => {
				entries.insert(scalar_key(key)?, address);
			}
			Self::Count(count) => {
				*count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("class count index overflows u64")
				})?;
			}
		}
		Ok(())
	}

	/// Removes a matching scalar posting or decrements a count index.
	pub(crate) fn remove(&mut self, key: Option<&[u8]>, address: Address) -> FerrosGrynnResult<()> {
		match self {
			Self::Standard(entries) => {
				let key = scalar_key(key)?;
				if let Some(addresses) = entries.get_mut(key) {
					addresses.remove(&address);
					if addresses.is_empty() {
						entries.remove(key);
					}
				}
			}
			Self::Unique(entries) => {
				let key = scalar_key(key)?;
				if entries.get(key) == Some(&address) {
					entries.remove(key);
				}
			}
			Self::Count(count) => {
				*count = count.checked_sub(1).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("class count index underflows u64")
				})?;
			}
		}
		Ok(())
	}

	/// Returns the scalar entries result.
	#[cfg(test)]
	pub(crate) fn scalar_entries(&self) -> Option<&BTreeMap<Vec<u8>, BTreeSet<Address>>> {
		match self {
			Self::Standard(entries) => Some(entries),
			Self::Unique(_) | Self::Count(_) => None,
		}
	}
}

/// Returns the scalar key result.
fn scalar_key<T>(key: Option<T>) -> FerrosGrynnResult<T> {
	key.ok_or_else(|| FerrosGrynnError::invalid_storage_file("scalar index mutation has no key"))
}

/// Engine-owned and explicit indexes of one graph data space.
#[derive(Debug, Clone, Default)]
pub struct GraphIndexRoot {
	/// Classes associated with this graph index root.
	pub(crate) classes: BTreeMap<ClassId, BTreeSet<AtomId>>,
	/// Labels associated with this graph index root.
	pub(crate) labels: BTreeMap<(AtomKind, Text), BTreeSet<AtomId>>,
	/// Explicit associated with this graph index root.
	pub(crate) explicit: BTreeMap<IndexId, IndexBinding<AtomId>>,
}

impl GraphIndexRoot {
	/// Maintains engine-owned label postings independently of explicit indexes.
	///
	/// # Errors
	/// Rejects a null address or an absent source posting before insertion.
	pub(super) fn replace_label(
		&mut self,
		local: AtomId,
		before: Option<&Text>,
		after: Option<&Text>,
	) -> FerrosGrynnResult<()> {
		if before == after {
			return Ok(());
		}
		let kind = local.kind().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("live occurrence kind is null")
		})?;
		if let Some(label) = before {
			let key = (kind, label.clone());
			let members = self.labels.get_mut(&key).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("source label lookup is absent")
			})?;
			if !members.remove(&local) {
				return Err(FerrosGrynnError::invalid_storage_file(
					"source label lookup does not contain the occurrence",
				));
			}
			// Empty keys would shift dense durable label IDs away from source
			// labels.
			if members.is_empty() {
				self.labels.remove(&key);
			}
		}
		if let Some(label) = after {
			self.labels.entry((kind, label.clone())).or_default().insert(local);
		}
		Ok(())
	}
}

/// All transaction-visible indexes of one immutable database root.
#[derive(Debug, Clone, Default)]
pub struct IndexStoreRoot {
	/// Classes associated with this index store root.
	pub(crate) classes: BTreeMap<ClassId, BTreeSet<GlobalAtomId>>,
	/// Base associated with this index store root.
	pub(crate) base: BTreeMap<IndexId, IndexBinding<GlobalAtomId>>,
	/// Graphs associated with this index store root.
	pub(crate) graphs: BTreeMap<GraphId, GraphIndexRoot>,
}

impl IndexStoreRoot {
	/// Removes one schema-defined binding after its definition becomes stale.
	/// Engine-owned class and label lookups are unaffected.
	pub(crate) fn remove_explicit_index(&mut self, id: IndexId) {
		self.base.remove(&id);
		for graph in self.graphs.values_mut() {
			graph.explicit.remove(&id);
		}
	}

	/// Returns whether any explicit index still uses its immutable durable
	/// side-file instead of transaction-private mutable entries.
	pub(crate) fn has_deferred_indexes(&self) -> bool {
		self.base.values().any(|binding| binding.deferred)
			|| self
				.graphs
				.values()
				.flat_map(|graph| graph.explicit.values())
				.any(|binding| binding.deferred)
	}

	/// Removes only schema-defined indexes while preserving class and label
	/// lookup structures required by import DML.
	pub(crate) fn defer_explicit_indexes(&mut self, retain_unique: bool) {
		self.base.retain(|_id, binding| retain_unique && binding.kind == ClassIndexKind::Unique);
		for graph in self.graphs.values_mut() {
			graph
				.explicit
				.retain(|_id, binding| retain_unique && binding.kind == ClassIndexKind::Unique);
		}
	}

	/// Registers graph.
	pub(crate) fn register_graph(&mut self, graph: GraphId) -> FerrosGrynnResult<()> {
		if self.graphs.insert(graph, GraphIndexRoot::default()).is_some() {
			return Err(FerrosGrynnError::invalid_argument("graph indexes are already registered"));
		}
		Ok(())
	}

	/// Returns the base index result.
	pub(crate) fn base_index(&self, id: IndexId) -> Option<&IndexBinding<GlobalAtomId>> {
		self.base.get(&id)
	}

	/// Returns the graph index result.
	pub(crate) fn graph_index(&self, graph: GraphId, id: IndexId) -> Option<&IndexBinding<AtomId>> {
		self.graphs.get(&graph)?.explicit.get(&id)
	}

	/// Returns the base class result.
	#[cfg(test)]
	pub(crate) fn base_class(&self, class: ClassId) -> Vec<GlobalAtomId> {
		self.base_class_iter(class).collect()
	}

	/// Borrows class membership without allocating a complete address vector.
	pub(crate) fn base_class_iter(
		&self,
		class: ClassId,
	) -> impl Iterator<Item = GlobalAtomId> + '_ {
		self.classes.get(&class).into_iter().flat_map(BTreeSet::iter).copied()
	}

	/// Returns the graph class result.
	pub(crate) fn graph_class(&self, graph: GraphId, class: ClassId) -> Vec<AtomId> {
		self.graphs
			.get(&graph)
			.and_then(|indexes| indexes.classes.get(&class))
			.into_iter()
			.flat_map(BTreeSet::iter)
			.copied()
			.collect()
	}

	/// Returns the graph label result.
	pub(crate) fn graph_label(&self, graph: GraphId, kind: AtomKind, label: &Text) -> Vec<AtomId> {
		self.graphs
			.get(&graph)
			.and_then(|indexes| indexes.labels.get(&(kind, label.clone())))
			.into_iter()
			.flat_map(BTreeSet::iter)
			.copied()
			.collect()
	}
}
