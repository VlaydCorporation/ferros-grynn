//! Root-arena graph-slot planning from explicit container occurrences.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{super::SubgraphEntry, GraphArenaBindings};
use crate::{
	datastore::{DatabaseRoot, OccurrenceState, OccurrenceStateRef},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, GraphId, QualifiedAtomId},
	values::storage::DiskAtomRef,
};

#[derive(Debug, Clone, Copy)]
/// Child binding state.
struct ChildBinding {
	/// Graph associated with this child binding.
	graph: GraphId,
	/// Slot associated with this child binding.
	slot: u32,
	/// Container associated with this child binding.
	container: AtomId,
}

/// Pending graph state.
struct PendingGraph {
	/// Graph associated with this pending graph.
	graph: GraphId,
	/// Slot associated with this pending graph.
	slot: u32,
	/// Ancestry associated with this pending graph.
	ancestry: Vec<GraphId>,
}

/// Owned scope layout is independent of the database borrow used by bulk
/// rendering.
struct GraphArenaLayout {
	/// Root defining every scope and the direct directory owner.
	root: GraphId,
	/// Direct children retain the existing deterministic breadth-first slot
	/// order.
	children: BTreeMap<u32, Vec<ChildBinding>>,
	/// Canonical immutable maps remain owned after the source root is released.
	bindings: GraphArenaBindings,
}

/// Bulk rendering combines the owned layout with borrowed occurrence data.
pub(super) struct GraphArena<'database> {
	/// Source occurrences supply directory counters and qualified terminals.
	database: &'database DatabaseRoot,
	/// The same layout supplies Hot references, directories and catalog maps.
	layout: GraphArenaLayout,
}

impl GraphArenaLayout {
	/// Plans the existing dense BFS layout without constructing any physical
	/// pages.
	///
	/// # Errors
	/// Rejects missing roots/children, containment cycles and exhausted slots.
	fn build(database: &DatabaseRoot, root: GraphId) -> FerrosGrynnResult<Self> {
		Self::build_admitted(database, root, |_retained, _scratch| Ok(()))
	}

	/// Uses one canonical BFS while admitting retained growth and split
	/// scratch. Temporary backing stays conservatively charged for the
	/// preparation.
	///
	/// # Errors
	/// Preserves source validation and rejects admission, overflow and
	/// allocation failures before the corresponding collection can grow.
	fn build_admitted(
		database: &DatabaseRoot,
		root: GraphId,
		mut admit: impl FnMut(usize, usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		if database.graph(root).is_none() {
			return Err(invalid("root arena graph is absent"));
		}
		let mut graph_by_slot = BTreeMap::new();
		admit_tree::<(u32, GraphId)>(0, &mut admit)?;
		graph_by_slot.insert(0, root);
		let mut scope_by_slot = BTreeMap::new();
		admit_tree::<(u32, Box<[AtomId]>)>(0, &mut admit)?;
		scope_by_slot.insert(0, Box::<[AtomId]>::default());
		let mut children = BTreeMap::new();
		let mut queue = VecDeque::new();
		reserve_queue(&mut queue, &mut admit)?;
		let mut ancestry = reserved_vec(1, &mut admit)?;
		ancestry.push(root);
		queue.push_back(PendingGraph {
			graph: root,
			slot: 0,
			ancestry,
		});
		let mut next_slot = 1_u32;
		while let Some(parent) = queue.pop_front() {
			let graph = database
				.graph(parent.graph)
				.ok_or_else(|| invalid("root arena references an absent graph"))?;
			let direct_count = graph
				.occurrences()
				.filter(|occurrence| child_graph(occurrence.state()).is_some())
				.count();
			let mut direct = reserved_vec(direct_count, &mut admit)?;
			for child in graph.occurrences().filter_map(|occurrence| {
				child_graph(occurrence.state()).map(|child| (child, occurrence.local_id()))
			}) {
				direct.push(child);
			}
			direct.sort_unstable_by_key(|(child, container)| (container.raw(), child.get()));
			let mut seen = BTreeSet::new();
			let source_scope = scope_by_slot
				.get(&parent.slot)
				.ok_or_else(|| invalid("root arena parent has no scope"))?;
			let mut parent_scope = reserved_vec(source_scope.len(), &mut admit)?;
			parent_scope.extend_from_slice(source_scope);
			let mut planned = reserved_vec(direct.len(), &mut admit)?;
			for (child, container) in direct {
				if seen.contains(&container) {
					return Err(invalid("one container occurrence has multiple child graphs"));
				}
				admit_tree::<AtomId>(seen.len(), &mut admit)?;
				seen.insert(container);
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
				admit_tree::<(u32, GraphId)>(graph_by_slot.len(), &mut admit)?;
				graph_by_slot.insert(slot, child);
				let length = parent_scope
					.len()
					.checked_add(1)
					.ok_or_else(|| invalid("root arena scope length overflows usize"))?;
				let mut scope = reserved_vec(length, &mut admit)?;
				scope.extend_from_slice(&parent_scope);
				scope.push(container);
				admit_tree::<(u32, Box<[AtomId]>)>(scope_by_slot.len(), &mut admit)?;
				admit(0, array_bytes::<AtomId>(scope.len())?)?;
				scope_by_slot.insert(slot, scope.into_boxed_slice());
				planned.push(ChildBinding {
					graph: child,
					slot,
					container,
				});
				let length = parent
					.ancestry
					.len()
					.checked_add(1)
					.ok_or_else(|| invalid("root arena ancestry length overflows usize"))?;
				let mut ancestry = reserved_vec(length, &mut admit)?;
				ancestry.extend_from_slice(&parent.ancestry);
				ancestry.push(child);
				reserve_queue(&mut queue, &mut admit)?;
				queue.push_back(PendingGraph {
					graph: child,
					slot,
					ancestry,
				});
			}
			admit_tree::<(u32, Vec<ChildBinding>)>(children.len(), &mut admit)?;
			children.insert(parent.slot, planned);
		}
		admit(GraphArenaBindings::owner_bytes(), 0)?;
		Ok(Self {
			root,
			children,
			bindings: GraphArenaBindings::new(graph_by_slot, scope_by_slot),
		})
	}
}

impl GraphArenaBindings {
	/// Builds owned canonical maps under the caller's shared logical envelope.
	///
	/// # Errors
	/// Rejects invalid containment, allocation and admission before returning
	/// any owner. The estimate includes conservative builder temporaries,
	/// not RSS.
	pub(in crate::storage) fn build_admitted(
		database: &DatabaseRoot,
		root: GraphId,
		admit: impl FnMut(usize, usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		GraphArenaLayout::build_admitted(database, root, admit).map(|layout| layout.bindings)
	}
}

/// Checks one array's logical backing before any reservation or copy.
///
/// # Errors
/// Rejects element counts whose byte estimate overflows usize.
fn array_bytes<Value>(count: usize) -> FerrosGrynnResult<usize> {
	count
		.checked_mul(size_of::<Value>())
		.ok_or_else(|| invalid("root arena array bytes overflow usize"))
}

/// Admits one vacant tree entry and the same split scratch as publication.
///
/// # Errors
/// Preserves estimate overflow and the caller's typed admission failure.
fn admit_tree<Entry>(
	count: usize,
	admit: &mut impl FnMut(usize, usize) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let old = if count == 0 {
		0
	} else {
		crate::memory::tree_retained_bytes::<Entry>(count)?
	};
	let next =
		count.checked_add(1).ok_or_else(|| invalid("root arena tree count overflows usize"))?;
	let growth = crate::memory::tree_retained_bytes::<Entry>(next)?
		.checked_sub(old)
		.ok_or_else(|| invalid("root arena tree reserve decreases"))?;
	admit(growth, 4096)
}

/// Reserves the complete final length before filling an owned vector.
///
/// # Errors
/// Rejects byte overflow, shared admission and allocation failures.
fn reserved_vec<Value>(
	count: usize,
	admit: &mut impl FnMut(usize, usize) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<Value>> {
	let mut values = Vec::new();
	if count != 0 {
		admit(array_bytes::<Value>(count)?, 0)?;
		values
			.try_reserve_exact(count)
			.map_err(|_error| invalid("root arena vector allocation failed"))?;
	}
	Ok(values)
}

/// Charges a growing queue's whole new backing while old backing is retained.
///
/// # Errors
/// Rejects capacity overflow, admission and allocation before pushing.
fn reserve_queue(
	queue: &mut VecDeque<PendingGraph>,
	admit: &mut impl FnMut(usize, usize) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	if queue.len() < queue.capacity() {
		return Ok(());
	}
	let next = queue
		.capacity()
		.checked_add(queue.capacity().max(1))
		.ok_or_else(|| invalid("root arena queue capacity overflows usize"))?;
	admit(array_bytes::<PendingGraph>(next)?, 0)?;
	queue
		.try_reserve_exact(next - queue.len())
		.map_err(|_error| invalid("root arena queue allocation failed"))
}

impl<'database> GraphArena<'database> {
	/// Borrows source data while planning the unchanged bulk-renderer layout.
	///
	/// # Errors
	/// Preserves every layout validation error before bulk page construction.
	pub(super) fn build(
		database: &'database DatabaseRoot,
		root: GraphId,
	) -> FerrosGrynnResult<Self> {
		Ok(Self {
			database,
			layout: GraphArenaLayout::build(database, root)?,
		})
	}

	/// Returns the bindings result.
	pub(super) fn bindings(&self) -> GraphArenaBindings {
		self.layout.bindings.clone()
	}

	/// Returns the direct slot result.
	pub(super) fn direct_slot(
		&self,
		parent_slot: u32,
		container: AtomId,
		child: GraphId,
	) -> Option<u32> {
		self.layout
			.children
			.get(&parent_slot)?
			.iter()
			.find(|binding| binding.graph == child && binding.container == container)
			.map(|binding| binding.slot)
	}

	/// Returns the entries result.
	pub(super) fn entries(&self, parent: GraphId) -> FerrosGrynnResult<Vec<SubgraphEntry>> {
		if parent != self.layout.root {
			return Err(invalid("subgraph directory can only be built for the arena root"));
		}
		self.layout
			.children
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

	/// Resolves through planned child slots without rebuilding an arena.
	///
	/// # Errors
	/// Preserves missing child binding, graph and terminal occurrence errors.
	pub(super) fn qualified(
		&self,
		qualified: &QualifiedAtomId,
	) -> FerrosGrynnResult<(u32, DiskAtomRef)> {
		super::qualified::resolve_qualified(
			self.database,
			self.layout.root,
			qualified,
			|parent, container, _prefix| {
				self.layout
					.children
					.get(&parent)
					.into_iter()
					.flatten()
					.find(|binding| binding.container == container)
					.map(|binding| (binding.slot, binding.graph))
					.ok_or_else(|| invalid("qualified endpoint scope has no child binding"))
			},
		)
	}
}

/// Returns the child graph result.
fn child_graph(state: &OccurrenceState) -> Option<GraphId> {
	match state.view() {
		OccurrenceStateRef::Node {
			nested_graph,
		} => *nested_graph,
		OccurrenceStateRef::Edge {
			transition_graph,
			..
		} => *transition_graph,
	}
}

/// Returns the usize u32 result.
fn usize_u32(value: usize) -> FerrosGrynnResult<u32> {
	u32::try_from(value).map_err(|_error| invalid("subgraph count exceeds u32"))
}

/// Returns the usize u64 result.
fn usize_u64(value: usize) -> FerrosGrynnResult<u64> {
	u64::try_from(value).map_err(|_error| invalid("subgraph capacity exceeds u64"))
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Owned planning preserves the exact existing breadth-first scope contract.
mod tests {
	use std::sync::Arc;

	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::{EdgeTopology, GraphKind},
		test_assertions::{AssertOption as _, AssertResult as _},
		values::PropertyMap,
	};

	/// Shared and deep children distinguish scopes from graph identities.
	struct Fixture {
		/// The runtime root can be released without releasing the owned plan.
		database: Arc<DatabaseRoot>,
		/// Arena root has two NODE containers and one EDGE transition.
		root: GraphId,
		/// The first root child contains a deeper NODE container.
		branch: GraphId,
		/// Two root paths deliberately select the same leaf graph.
		leaf: GraphId,
		/// The branch's descendant must follow all direct children in BFS
		/// order.
		deep: GraphId,
		/// A precreated leaf provides a real rebind target.
		replacement: GraphId,
		/// Full NODE handles determine the first two direct scope positions.
		owners: [AtomId; 2],
		/// The EDGE owner retains a distinct scope for the shared child.
		portal: AtomId,
		/// This plain root NODE can become a new direct container.
		spare: AtomId,
		/// The full descendant handle must remain in the deep scope path.
		deep_owner: AtomId,
	}

	/// Valid transactions establish every graph and occurrence used by the
	/// planner.
	fn fixture() -> Fixture {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let root = seed.define_graph("root", GraphKind::Metagraph).assert_ok().id();
		let branch = seed.define_graph("branch", GraphKind::Metagraph).assert_ok().id();
		let leaf = seed.define_graph("leaf", GraphKind::Graph).assert_ok().id();
		let deep = seed.define_graph("deep", GraphKind::Graph).assert_ok().id();
		let replacement = seed.define_graph("replacement", GraphKind::Graph).assert_ok().id();
		for record in
			["item:a", "item:b", "item:spare", "item:deep", "item:terminal", "link:portal"]
		{
			seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
		}
		let owners = [("item:a", branch), ("item:b", leaf)].map(|(record, child)| {
			seed.attach_node(root, &record.parse().assert_ok(), None, Some(child)).assert_ok()
		});
		let spare =
			seed.attach_node(root, &"item:spare".parse().assert_ok(), None, None).assert_ok();
		let portal = seed
			.attach_edge(
				root,
				&"link:portal".parse().assert_ok(),
				None,
				EdgeTopology::directed([owners[0]], [owners[1]]),
				None,
				Some(leaf),
			)
			.assert_ok();
		let deep_owner = seed
			.attach_node(branch, &"item:deep".parse().assert_ok(), None, Some(deep))
			.assert_ok();
		for graph in [leaf, deep, replacement] {
			seed.attach_node(graph, &"item:terminal".parse().assert_ok(), None, None).assert_ok();
		}
		seed.commit().assert_ok();
		Fixture {
			database: store.inner.current_root(),
			root,
			branch,
			leaf,
			deep,
			replacement,
			owners,
			portal,
			spare,
			deep_owner,
		}
	}

	/// Literal maps prove BFS order, full paths, shared-child multiplicity and
	/// ownership.
	#[test]
	fn owned_arena_layout_keeps_literal_bfs_after_source_release() {
		let fixture = fixture();
		let layout = GraphArenaLayout::build(&fixture.database, fixture.root).assert_ok();
		let admitted = GraphArenaBindings::build_admitted(
			&fixture.database,
			fixture.root,
			|_retained, _scratch| Ok(()),
		)
		.assert_ok();
		let graphs = BTreeMap::from([
			(0, fixture.root),
			(1, fixture.branch),
			(2, fixture.leaf),
			(3, fixture.leaf),
			(4, fixture.deep),
		]);
		let scopes = BTreeMap::from([
			(0, Box::<[AtomId]>::default()),
			(1, vec![fixture.owners[0]].into_boxed_slice()),
			(2, vec![fixture.owners[1]].into_boxed_slice()),
			(3, vec![fixture.portal].into_boxed_slice()),
			(4, vec![fixture.owners[0], fixture.deep_owner].into_boxed_slice()),
		]);
		assert_eq!(
			GraphArena::build(&fixture.database, fixture.root).assert_ok().bindings(),
			layout.bindings
		);
		drop(fixture);
		assert_eq!(layout.bindings.graph_by_slot(), &graphs);
		assert_eq!(layout.bindings.scope_by_slot(), &scopes);
		assert_eq!(admitted.graph_by_slot(), &graphs);
		assert_eq!(admitted.scope_by_slot(), &scopes);
	}

	/// Leaf rebind keeps slots; adding a direct owner shifts later direct and
	/// deep slots.
	#[test]
	fn owned_arena_layout_distinguishes_leaf_rebind_from_direct_insert() {
		let fixture = fixture();
		let original = GraphArenaLayout::build(&fixture.database, fixture.root).assert_ok();
		let mut rebound = fixture.database.as_ref().clone();
		rebound
			.replace_occurrence_state(
				fixture.root,
				fixture.owners[1],
				OccurrenceState::node(Some(fixture.replacement)),
			)
			.assert_ok();
		let rebound = GraphArenaLayout::build(&rebound, fixture.root).assert_ok();
		let mut graphs = original.bindings.graph_by_slot().clone();
		graphs.insert(2, fixture.replacement);
		assert_eq!(rebound.bindings.graph_by_slot(), &graphs);
		assert_eq!(rebound.bindings.scope_by_slot(), original.bindings.scope_by_slot());
		assert_eq!(original.bindings.graph_by_slot().get(&2), Some(&fixture.leaf));
		let mut inserted = fixture.database.as_ref().clone();
		inserted
			.replace_occurrence_state(
				fixture.root,
				fixture.spare,
				OccurrenceState::node(Some(fixture.replacement)),
			)
			.assert_ok();
		let inserted = GraphArenaLayout::build(&inserted, fixture.root).assert_ok();
		assert_eq!(
			inserted.bindings.graph_by_slot(),
			&BTreeMap::from([
				(0, fixture.root),
				(1, fixture.branch),
				(2, fixture.leaf),
				(3, fixture.replacement),
				(4, fixture.leaf),
				(5, fixture.deep)
			])
		);
		assert_eq!(
			inserted.bindings.scope_by_slot(),
			&BTreeMap::from([
				(0, Box::<[AtomId]>::default()),
				(1, vec![fixture.owners[0]].into_boxed_slice()),
				(2, vec![fixture.owners[1]].into_boxed_slice()),
				(3, vec![fixture.spare].into_boxed_slice()),
				(4, vec![fixture.portal].into_boxed_slice()),
				(5, vec![fixture.owners[0], fixture.deep_owner].into_boxed_slice())
			])
		);
	}

	/// A small source DAG can have a wide or exponential number of full paths.
	fn expanding_fixture(width: usize, depth: usize) -> (Arc<DatabaseRoot>, GraphId, GraphId) {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		let records = (0..width)
			.map(|index| {
				format!("item:owner_{index}").parse::<crate::values::RecordId>().assert_ok()
			})
			.collect::<Vec<_>>();
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let graphs = (0..=depth)
			.map(|index| {
				seed.define_graph(format!("level_{index}"), GraphKind::Metagraph).assert_ok().id()
			})
			.collect::<Vec<_>>();
		for pair in graphs.windows(2) {
			for record in &records {
				seed.attach_node(pair[0], record, None, Some(pair[1])).assert_ok();
			}
		}
		seed.commit().assert_ok();
		(store.inner.current_root(), graphs[0], *graphs.last().assert_some())
	}

	/// The test caller supplies a fixed logical quota through the same
	/// callback.
	fn bounded_layout(
		database: &DatabaseRoot,
		graph: GraphId,
		maximum: usize,
	) -> FerrosGrynnResult<GraphArenaBindings> {
		let mut retained = 0_usize;
		GraphArenaBindings::build_admitted(database, graph, |growth, scratch| {
			let next = retained
				.checked_add(growth)
				.ok_or_else(|| invalid("test arena retained bytes overflow"))?;
			let total = next
				.checked_add(scratch)
				.ok_or_else(|| invalid("test arena scratch bytes overflow"))?;
			if total > maximum {
				return Err(FerrosGrynnError::storage_decode_limit_exceeded(
					"arena layout",
					total as u64,
					maximum as u64,
				));
			}
			retained = next;
			Ok(())
		})
	}

	/// The same64KiB admits an empty arena and rejects wide/deep path
	/// expansion.
	#[test]
	fn admitted_arena_rejects_wide_and_deep_aliases_without_changing_source() {
		for (width, depth) in [(512, 1), (2, 20)] {
			let (database, root, leaf) = expanding_fixture(width, depth);
			let source = database.clone();
			let small = bounded_layout(&database, leaf, 64 * 1024).assert_ok();
			assert_eq!(small.graph_by_slot(), &BTreeMap::from([(0, leaf)]));
			let error = bounded_layout(&database, root, 64 * 1024).err().assert_some();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			assert!(Arc::ptr_eq(&database, &source));
			assert_eq!(bounded_layout(&database, leaf, 64 * 1024).assert_ok(), small);
		}
	}

	/// Missing roots precede admission; a late callback failure returns no
	/// owner.
	#[test]
	fn admitted_arena_preserves_source_errors_and_discards_late_partial_work() {
		let fixture = fixture();
		let mut calls = 0_usize;
		let absent = GraphId::new(u64::MAX).assert_some();
		let error =
			GraphArenaBindings::build_admitted(&fixture.database, absent, |_growth, _scratch| {
				calls += 1;
				Ok(())
			})
			.err()
			.assert_some();
		assert_eq!(calls, 0);
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
			if reason.as_str() == "root arena graph is absent")
		);
		let original = GraphArenaBindings::build_admitted(
			&fixture.database,
			fixture.root,
			|_growth, _scratch| {
				calls += 1;
				Ok(())
			},
		)
		.assert_ok();
		let reject_at = calls / 2;
		assert!(reject_at > 0);
		let accepted_owner = original.clone();
		calls = 0;
		let error = GraphArenaBindings::build_admitted(
			&fixture.database,
			fixture.root,
			|_growth, _scratch| {
				calls += 1;
				if calls == reject_at {
					return Err(invalid("injected late arena admission failure"));
				}
				Ok(())
			},
		)
		.err()
		.assert_some();
		assert_eq!(calls, reject_at);
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
			if reason.as_str() == "injected late arena admission failure")
		);
		assert!(original.same_owner(&accepted_owner));
		assert_eq!(
			bounded_layout(&fixture.database, fixture.root, 64 * 1024).assert_ok(),
			original
		);
	}

	/// Invalid roots preserve the existing typed error and validation order.
	#[test]
	fn owned_arena_layout_rejects_missing_graphs_and_containment_cycles() {
		let fixture = fixture();
		let absent = GraphId::new(u64::MAX).assert_some();
		assert_invalid(&fixture.database, absent, "root arena graph is absent");
		let mut missing = fixture.database.as_ref().clone();
		missing
			.replace_occurrence_state(
				fixture.root,
				fixture.owners[0],
				OccurrenceState::node(Some(absent)),
			)
			.assert_ok();
		assert_invalid(&missing, fixture.root, "container references an absent child graph");
		let mut cycle = fixture.database.as_ref().clone();
		cycle
			.replace_occurrence_state(
				fixture.root,
				fixture.owners[0],
				OccurrenceState::node(Some(fixture.root)),
			)
			.assert_ok();
		assert_invalid(&cycle, fixture.root, "root arena contains a containment cycle");
	}

	/// A generic error cannot substitute for the precise storage-boundary
	/// rejection.
	fn assert_invalid(database: &DatabaseRoot, graph: GraphId, expected: &str) {
		for error in [
			GraphArenaLayout::build(database, graph).err().assert_some(),
			GraphArenaBindings::build_admitted(database, graph, |_growth, _scratch| Ok(()))
				.err()
				.assert_some(),
		] {
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
				if reason.as_str() == expected)
			);
		}
	}
}
