//! Root-arena graph-slot planning from explicit container occurrences.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{super::SubgraphEntry, GraphArenaBindings};
use crate::{
	datastore::{DatabaseRoot, OccurrenceState},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId, QualifiedAtomId},
	values::storage::DiskAtomRef,
};

#[derive(Debug, Clone, Copy)]
struct ChildBinding {
	graph: GraphId,
	slot: u32,
	container: AtomId,
}

struct PendingGraph {
	graph: GraphId,
	slot: u32,
	ancestry: Vec<GraphId>,
}

/// Stable root-arena slots and direct parent bindings for one graph tree.
pub(super) struct GraphArena<'database> {
	database: &'database DatabaseRoot,
	root: GraphId,
	children: BTreeMap<u32, Vec<ChildBinding>>,
	bindings: GraphArenaBindings,
}

impl<'database> GraphArena<'database> {
	pub(super) fn build(
		database: &'database DatabaseRoot,
		root: GraphId,
	) -> FerrosGrynnResult<Self> {
		if database.graph(root).is_none() {
			return Err(invalid("root arena graph is absent"));
		}
		let mut graph_by_slot = BTreeMap::from([(0, root)]);
		let mut scope_by_slot = BTreeMap::from([(0, Box::<[AtomId]>::default())]);
		let mut children = BTreeMap::new();
		let mut queue = VecDeque::from([PendingGraph {
			graph: root,
			slot: 0,
			ancestry: vec![root],
		}]);
		let mut next_slot = 1_u32;
		while let Some(parent) = queue.pop_front() {
			let graph = database
				.graph(parent.graph)
				.ok_or_else(|| invalid("root arena references an absent graph"))?;
			let mut direct = graph
				.occurrences()
				.filter_map(|occurrence| {
					child_graph(occurrence.state()).map(|child| (child, occurrence.local_id()))
				})
				.collect::<Vec<_>>();
			direct.sort_unstable_by_key(|(child, container)| (container.raw(), child.get()));
			let mut seen = BTreeSet::new();
			let parent_scope = scope_by_slot
				.get(&parent.slot)
				.ok_or_else(|| invalid("root arena parent has no scope"))?
				.to_vec();
			let mut planned = Vec::with_capacity(direct.len());
			for (child, container) in direct {
				if !seen.insert(container) {
					return Err(invalid("one container occurrence has multiple child graphs"));
				}
				if parent.ancestry.contains(&child) {
					return Err(invalid("root arena contains a containment cycle"));
				}
				if database.graph(child).is_none() {
					return Err(invalid("container references an absent child graph"));
				}
				let slot = next_slot;
				next_slot = next_slot
					.checked_add(1)
					.ok_or_else(|| invalid("root arena slot clock is exhausted"))?;
				graph_by_slot.insert(slot, child);
				let mut scope = parent_scope.clone();
				scope.push(container);
				scope_by_slot.insert(slot, scope.into_boxed_slice());
				planned.push(ChildBinding {
					graph: child,
					slot,
					container,
				});
				let mut ancestry = parent.ancestry.clone();
				ancestry.push(child);
				queue.push_back(PendingGraph {
					graph: child,
					slot,
					ancestry,
				});
			}
			children.insert(parent.slot, planned);
		}
		Ok(Self {
			database,
			root,
			children,
			bindings: GraphArenaBindings {
				graph_by_slot,
				scope_by_slot,
			},
		})
	}

	pub(super) fn bindings(&self) -> GraphArenaBindings {
		self.bindings.clone()
	}

	pub(super) fn direct_slot(
		&self,
		parent_slot: u32,
		container: AtomId,
		child: GraphId,
	) -> Option<u32> {
		self.children
			.get(&parent_slot)?
			.iter()
			.find(|binding| binding.graph == child && binding.container == container)
			.map(|binding| binding.slot)
	}

	pub(super) fn entries(&self, parent: GraphId) -> FerrosGrynnResult<Vec<SubgraphEntry>> {
		if parent != self.root {
			return Err(invalid("subgraph directory can only be built for the arena root"));
		}
		self.children
			.get(&0)
			.into_iter()
			.flatten()
			.map(|binding| {
				let graph = self
					.database
					.graph(binding.graph)
					.ok_or_else(|| invalid("subgraph binding references an absent graph"))?;
				let node_count = graph
					.occurrences()
					.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Node))
					.count();
				let edge_count = graph
					.occurrences()
					.filter(|occurrence| occurrence.local_id().kind() == Some(AtomKind::Edge))
					.count();
				Ok(SubgraphEntry {
					slot: binding.slot,
					generation: binding.container.generation(),
					node_capacity: usize_u64(graph.slot_capacity(AtomKind::Node))?,
					edge_capacity: usize_u64(graph.slot_capacity(AtomKind::Edge))?,
					node_count: usize_u32(node_count)?,
					edge_count: usize_u32(edge_count)?,
				})
			})
			.collect()
	}

	pub(super) fn qualified(
		&self,
		qualified: &QualifiedAtomId,
	) -> FerrosGrynnResult<(u32, DiskAtomRef)> {
		let mut current = self.root;
		let mut current_slot = 0;
		for container in qualified.scope() {
			let binding = self
				.children
				.get(&current_slot)
				.into_iter()
				.flatten()
				.find(|binding| binding.container == *container)
				.ok_or_else(|| invalid("qualified endpoint scope has no child binding"))?;
			current = binding.graph;
			current_slot = binding.slot;
		}
		let graph = self
			.database
			.graph(current)
			.ok_or_else(|| invalid("qualified endpoint graph is absent"))?;
		let atom = qualified.atom();
		if graph.occurrence(atom).is_none() {
			return Err(invalid("qualified endpoint atom is absent from its graph"));
		}
		Ok((current_slot, disk_ref(atom)?))
	}
}

fn child_graph(state: &OccurrenceState) -> Option<GraphId> {
	match state {
		OccurrenceState::Node {
			nested_graph,
		} => *nested_graph,
		OccurrenceState::Edge {
			transition_graph,
			..
		} => *transition_graph,
	}
}

fn disk_ref(id: AtomId) -> FerrosGrynnResult<DiskAtomRef> {
	DiskAtomRef::from_parts(
		id.kind().ok_or_else(|| invalid("null atom cannot be a qualified endpoint"))?,
		id.slot(),
	)
}

fn usize_u32(value: usize) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid("subgraph count exceeds u32"))
}

fn usize_u64(value: usize) -> FerrosGrynnResult<u64> {
	u64::try_from(value).map_err(|_error| invalid("subgraph capacity exceeds u64"))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
