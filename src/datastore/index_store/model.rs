use std::collections::{BTreeMap, BTreeSet};

use super::super::{ClassId, ClassIndexKind, GlobalAtomId, IndexId, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId},
	values::{PropertyKeyId, Text, Type},
};

/// Resolved field metadata needed to build one scalar key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexField {
	pub(crate) key: PropertyKeyId,
	pub(crate) value_type: Type,
}

/// Logical metadata and values of one explicit class index.
#[derive(Debug, Clone)]
pub(crate) struct IndexBinding<Address> {
	pub(crate) id: IndexId,
	pub(crate) version: SchemaVersion,
	pub(crate) class_id: ClassId,
	pub(crate) name: Text,
	pub(crate) kind: ClassIndexKind,
	pub(crate) fields: Vec<IndexField>,
	pub(crate) entries: IndexEntries<Address>,
}

/// Exact in-memory representation selected by the declared index kind.
#[derive(Debug, Clone)]
pub(crate) enum IndexEntries<Address> {
	Standard(BTreeMap<Vec<u8>, BTreeSet<Address>>),
	Unique(BTreeMap<Vec<u8>, Address>),
	Count(u64),
}

impl<Address: Copy + Ord> IndexEntries<Address> {
	pub(crate) fn empty(kind: ClassIndexKind) -> Self {
		match kind {
			ClassIndexKind::Standard => Self::Standard(BTreeMap::new()),
			ClassIndexKind::Unique => Self::Unique(BTreeMap::new()),
			ClassIndexKind::Count => Self::Count(0),
		}
	}

	pub(crate) fn count(&self) -> Option<u64> {
		match self {
			Self::Count(count) => Some(*count),
			Self::Standard(_) | Self::Unique(_) => None,
		}
	}

	pub(crate) fn lookup(&self, key: &[u8]) -> Vec<Address> {
		match self {
			Self::Standard(entries) => {
				entries.get(key).into_iter().flat_map(BTreeSet::iter).copied().collect()
			}
			Self::Unique(entries) => entries.get(key).copied().into_iter().collect(),
			Self::Count(_) => Vec::new(),
		}
	}

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

	pub(crate) fn checked_increment(&self) -> FerrosGrynnResult<()> {
		if let Self::Count(count) = self {
			count.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("class count index overflows u64")
			})?;
		}
		Ok(())
	}

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

	pub(crate) fn scalar_entries(&self) -> Option<&BTreeMap<Vec<u8>, BTreeSet<Address>>> {
		match self {
			Self::Standard(entries) => Some(entries),
			Self::Unique(_) | Self::Count(_) => None,
		}
	}

	pub(crate) fn unique_entries(&self) -> Option<&BTreeMap<Vec<u8>, Address>> {
		match self {
			Self::Unique(entries) => Some(entries),
			Self::Standard(_) | Self::Count(_) => None,
		}
	}
}

fn scalar_key<T>(key: Option<T>) -> FerrosGrynnResult<T> {
	key.ok_or_else(|| FerrosGrynnError::invalid_storage_file("scalar index mutation has no key"))
}

/// Engine-owned and explicit indexes of one graph data space.
#[derive(Debug, Clone, Default)]
pub(crate) struct GraphIndexRoot {
	pub(crate) classes: BTreeMap<ClassId, BTreeSet<AtomId>>,
	pub(crate) labels: BTreeMap<(AtomKind, Text), BTreeSet<AtomId>>,
	pub(crate) explicit: BTreeMap<IndexId, IndexBinding<AtomId>>,
}

/// All transaction-visible indexes of one immutable database root.
#[derive(Debug, Clone, Default)]
pub(crate) struct IndexStoreRoot {
	pub(crate) classes: BTreeMap<ClassId, BTreeSet<GlobalAtomId>>,
	pub(crate) base: BTreeMap<IndexId, IndexBinding<GlobalAtomId>>,
	pub(crate) graphs: BTreeMap<GraphId, GraphIndexRoot>,
}

impl IndexStoreRoot {
	pub(crate) fn register_graph(&mut self, graph: GraphId) -> FerrosGrynnResult<()> {
		if self.graphs.insert(graph, GraphIndexRoot::default()).is_some() {
			return Err(FerrosGrynnError::invalid_argument("graph indexes are already registered"));
		}
		Ok(())
	}

	pub(crate) fn base_index(&self, id: IndexId) -> Option<&IndexBinding<GlobalAtomId>> {
		self.base.get(&id)
	}

	pub(crate) fn graph_index(&self, graph: GraphId, id: IndexId) -> Option<&IndexBinding<AtomId>> {
		self.graphs.get(&graph)?.explicit.get(&id)
	}

	pub(crate) fn base_class(&self, class: ClassId) -> Vec<GlobalAtomId> {
		self.classes.get(&class).into_iter().flat_map(BTreeSet::iter).copied().collect()
	}

	pub(crate) fn graph_class(&self, graph: GraphId, class: ClassId) -> Vec<AtomId> {
		self.graphs
			.get(&graph)
			.and_then(|indexes| indexes.classes.get(&class))
			.into_iter()
			.flat_map(BTreeSet::iter)
			.copied()
			.collect()
	}

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
