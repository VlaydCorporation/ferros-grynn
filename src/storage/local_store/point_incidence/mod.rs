//! One graph owns its explicit incidence slots and touched adjacency pages.

use std::{collections::BTreeMap, sync::Arc};

/// Strict overflow chains and generation-aware NODE adjacency merges.
mod adjacency;
/// Retained EDGE roles share final child scopes and one append allocator.
mod child_remap;
/// Shared side layout, strict source chains and qualified append output.
mod endpoints;
/// Source validation and final EDGE Hot slots under unchanged containment.
mod hot;
/// Source and final label metadata are independent of topology rendering.
mod metadata;
/// One NODE page pass combines explicit lifecycle and derived adjacency.
mod node;
#[cfg(test)]
/// Exhaustive final replay targets are required for attached Base lifecycle.
mod tests;
#[cfg(test)]
/// Ownerless kinds require distinct consumed allocator witnesses.
mod transient_tests;

pub(super) use child_remap::build_retained_edge_remap;
pub(super) use hot::validate_retained_edge_source;

pub(super) use super::point_occurrence::incidence_state;
use super::{
	LocalState,
	LocalStore,
	layout::invalid,
	point_graph::{GraphOverlays, GraphUpdatePlan},
	point_namespace::{LabelPlan, NamespaceGraph, PublicationBudget},
	point_occurrence::{SlotTransition, generation_consumed, structural_target},
};
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, GraphRoot, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphId},
	storage::{
		graph_pages::{GraphArenaBindings, HOT_SLOTS_PER_PAGE},
		graph_store::GraphFileSet,
		graph_superblock::GraphSuperblock,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		types::PageKey,
	},
};

/// The three persisted NODE sides have distinct incidence semantics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeSide {
	/// Directed source or either role of a bidirectional edge.
	Outgoing,
	/// Directed target or either role of a bidirectional edge.
	Incoming,
	/// Endpoint of an undirected edge.
	Undirected,
}

/// One multiplicity-preserving old removal or final insertion.
struct AdjacencyChange {
	/// Full generation-aware identity, before encoding to a DiskAtomRef.
	edge: AtomId,
	/// Persisted NODE side affected by this incidence.
	side: NodeSide,
	/// Removals precede insertions when a local slot is reused.
	inserted: bool,
}

/// Both accepted allocator states belong to one physical NODE slot.
struct NodeTarget {
	/// Source and final identities remain separate when a slot is reused.
	transition: SlotTransition,
	/// Explicit membership sources require canonical source NODE metadata.
	explicit: bool,
	/// Multiplicity-preserving removals and insertions for the same page slot.
	entries: Vec<AdjacencyChange>,
}

/// Slot ordering groups NODE writes by physical page without a graph scan.
type NodeChanges = BTreeMap<u64, NodeTarget>;
/// Prepared final frames share the existing source/history WAL barrier.
pub(super) type PointPages = Vec<(PageKey, Box<AlignedPage>)>;

/// Borrowed final replay roots and one graph's bounded unpublished output.
struct IncidencePlan<'a, 'context> {
	/// Existing durability owner supplies WAL-visible reads and append bounds.
	store: &'a LocalStore,
	/// The publication lock and checkpoint fence remain held by the caller.
	state: &'a LocalState,
	/// Exact old root resolves generation-free physical neighbors.
	before: &'a DatabaseRoot,
	/// Final replay root includes synchronous effects and structural
	/// validation.
	after: &'a DatabaseRoot,
	/// No graph file identity changes in this plan.
	files: GraphFileSet,
	/// Accepted paths authorize old physical roles and child metadata.
	source_arena: &'a GraphArenaBindings,
	/// Final paths encode private output independently of source validation.
	final_arena: &'a GraphArenaBindings,
	/// The source header bounds reads until final metadata is prepared.
	header: GraphSuperblock,
	/// Appends begin after the complete WAL-visible prefix, including old
	/// chains.
	adjacency_end: u64,
	/// Initialized only by a qualified append; local points perform no extra
	/// file lookup.
	qualified_end: Option<u64>,
	/// Prepared source/final namespaces are shared without dictionary clones.
	namespace: Option<&'a NamespaceGraph>,
	/// Only a changed dictionary overrides the source header's final ceiling.
	labels_pages: Option<u64>,
	/// Lifecycle and stable metadata share one graph-local append allocator.
	overlays: Option<&'a mut GraphOverlays<'context>>,
	/// Output frames never move after they have been populated.
	pages: PointPages,
	/// Shares admission with membership, indexes, statistics and WAL copies.
	budget: &'a mut PublicationBudget,
}

/// Requires a closed incidence journal and supported Base owner transitions.
pub(super) fn eligible(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> bool {
	// shortcut: structural operations outside the closed membership journal
	// retain the full publisher until their sparse plans have recovery
	// evidence.
	if !delta.point_graph_only {
		return false;
	}
	for &(graph, global) in &delta.overlays {
		if !delta.memberships.contains(&(graph, global))
			&& !super::point_graph::stable_metadata_target(before, after, delta, graph, global)
			&& !super::point_graph::supported_rewire(before, after, graph, global)
		{
			return false;
		}
	}
	let mut ownerless_graph = None;
	let mut ownerless_node = false;
	let mut ownerless_edge = false;
	for &(graph, global) in &delta.memberships {
		if !eligible_membership(before, after, delta, graph, global) {
			return false;
		}
		if before.atom(global).is_none() && after.atom(global).is_none() {
			if ownerless_graph != Some(graph) {
				if !eligible_ownerless_metadata(after, graph) {
					return false;
				}
				ownerless_graph = Some(graph);
				ownerless_node = false;
				ownerless_edge = false;
			}
			let Some(kind) = global.kind() else {
				return false;
			};
			// A consumed NODE must never authorize a missing EDGE target.
			let witnessed = match kind {
				AtomKind::Node => &mut ownerless_node,
				AtomKind::Edge => &mut ownerless_edge,
			};
			if !*witnessed {
				let Some((old, new)) = before.graph(graph).zip(after.graph(graph)) else {
					return false;
				};
				if !consumed_incidence(old, new, delta, graph, kind) {
					return false;
				}
				*witnessed = true;
			}
		}
	}
	for &(graph, local) in &delta.incidence {
		if before.occurrence(graph, local).is_some_and(|occurrence| {
			super::point_graph::stable_metadata_target(
				before,
				after,
				delta,
				graph,
				occurrence.global_id(),
			) || super::point_graph::supported_rewire(before, after, graph, occurrence.global_id())
		}) {
			continue;
		}
		let Some((old, new)) = before.graph(graph).zip(after.graph(graph)) else {
			return false;
		};
		let Some(kind) = local.kind() else {
			return false;
		};
		if !plain_slot(old, kind, local.slot(), delta)
			|| !plain_slot(new, kind, local.slot(), delta)
		{
			return false;
		}
	}
	before.graphs.iter().all(|(graph, old)| {
		after.graphs.get(graph).is_some_and(|new| {
			Arc::ptr_eq(old, new)
				|| (old.reference() == new.reference()
					&& delta.memberships.iter().any(|(target, _)| target == graph)
					&& old.slot_capacity(AtomKind::Node) <= new.slot_capacity(AtomKind::Node)
					&& old.slot_capacity(AtomKind::Edge) <= new.slot_capacity(AtomKind::Edge))
				|| super::point_graph::stable_graph(*graph, old, new, delta)
		})
	})
}

/// Keeps Base ownership and graph-family admission separate from slot checks.
fn eligible_membership(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	graph: GraphId,
	global: GlobalAtomId,
) -> bool {
	let owner = match (before.atom(global), after.atom(global)) {
		(Some(old), Some(new)) if old.class_id() == new.class_id() => Some(new),
		(Some(atom), None) | (None, Some(atom))
			if matches!(global.kind(), Some(AtomKind::Node | AtomKind::Edge))
				&& delta.global_slots.contains(&global.slot()) =>
		{
			Some(atom)
		}
		(None, None)
			if matches!(global.kind(), Some(AtomKind::Node | AtomKind::Edge))
				&& delta.global_slots.contains(&global.slot())
				&& before.membership(graph, global).is_none()
				&& after.membership(graph, global).is_none() =>
		{
			let Ok(slot) = usize::try_from(global.slot()) else {
				return false;
			};
			if !generation_consumed(
				before.atoms.slot_generation(slot),
				after.atoms.slot_generation(slot),
				global.generation(),
			) {
				return false;
			}
			None
		}
		_ => return false,
	};
	matches!(global.kind(), Some(AtomKind::Node | AtomKind::Edge))
		&& owner.is_none_or(|owner| {
			super::point_update::supports_point_indexes(after, owner.class_id())
		})
		&& super::point_graph::plain_membership_graph(after, graph)
}

/// A missing live owner cannot identify which specialized class was consumed.
fn eligible_ownerless_metadata(after: &DatabaseRoot, graph: GraphId) -> bool {
	// shortcut: without a live class owner, specialized class buckets keep the
	// full publisher until their transient sparse plans have recovery evidence.
	after.indexes.graphs.get(&graph).is_some_and(|indexes| {
		indexes
			.classes
			.keys()
			.all(|class| super::point_update::supports_point_indexes(after, *class))
	})
}

/// Only an explicit consumed local target witnesses its allocator transition.
fn consumed_incidence(
	old: &GraphRoot,
	new: &GraphRoot,
	delta: &PublicationDelta,
	graph: GraphId,
	kind: AtomKind,
) -> bool {
	delta.incidence.range((graph, AtomId::from_raw(0))..=(graph, AtomId::NULL)).any(|(_, local)| {
		local.kind() == Some(kind)
			&& usize::try_from(local.slot()).is_ok_and(|slot| {
				generation_consumed(
					old.slot_generation(kind, slot),
					new.slot_generation(kind, slot),
					local.generation(),
				)
			})
	})
}

/// Both live generations must be explicit membership targets with plain
/// topology.
fn plain_slot(graph: &GraphRoot, kind: AtomKind, slot: u64, delta: &PublicationDelta) -> bool {
	let Ok(index) = usize::try_from(slot) else {
		return false;
	};
	let Some(generation) = graph.slot_generation(kind, index) else {
		return true;
	};
	let Ok(local) = AtomId::from_parts(kind, slot, generation) else {
		return false;
	};
	graph.occurrence(local).is_none_or(|occurrence| {
		delta.memberships.contains(&(graph.reference().id(), occurrence.global_id()))
			&& incidence_state(occurrence.state())
	})
}

/// Only explicit live source/final payloads need a shared append allocator.
pub(super) fn has_overlays(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	labels: &LabelPlan,
	range: std::ops::Range<usize>,
) -> bool {
	labels.incidence[range]
		.iter()
		.filter(|target| structural_target(target, &labels.memberships, (before, after)))
		.any(|target| {
			[(before, target.before), (after, target.after)].into_iter().any(|(root, state)| {
				state.and_then(|state| state.occurrence).is_some_and(|(local, _)| {
					root.occurrence(target.graph, local)
						.is_some_and(|occurrence| occurrence.overlay().is_some())
				})
			})
		})
}

/// Prepares one graph before its stable metadata patches share the same frames.
///
/// # Errors
/// Preserves source validation, admission, codec and I/O failures.
pub(super) fn build_graph(
	store: &LocalStore,
	state: &LocalState,
	roots: (&DatabaseRoot, &DatabaseRoot),
	range: std::ops::Range<usize>,
	labels: &mut LabelPlan,
	result: &mut GraphUpdatePlan,
	overlays: Option<&mut GraphOverlays<'_>>,
) -> FerrosGrynnResult<()> {
	let (before, after) = roots;
	let graph = labels.incidence[range.start].graph;
	let targets = &labels.incidence[range];
	if !targets.iter().any(|target| structural_target(target, &labels.memberships, (before, after)))
	{
		return Ok(());
	}
	let node_count = targets.partition_point(|target| target.kind == AtomKind::Node);
	let (nodes, edges) = targets.split_at(node_count);
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == graph)
		.ok_or_else(|| invalid("EDGE membership graph binding is absent"))?;
	let files = binding.files;
	let header = *state
		.graph_superblocks
		.get(&graph)
		.ok_or_else(|| invalid("EDGE membership graph header is absent"))?;
	let adjacency_end = if !edges
		.iter()
		.any(|target| structural_target(target, &labels.memberships, (before, after)))
	{
		header.adjacency_overflow_pages
	} else {
		store.visible_pages(state, files.space(), files.adjacency)?
	};
	if adjacency_end < header.adjacency_overflow_pages {
		return Err(invalid("EDGE adjacency source is shorter than its header"));
	}
	let labels_pages = labels.dictionary_pages(graph)?;
	let mut plan = IncidencePlan::new(
		store,
		state,
		(before, after),
		(files, &binding.arena, header),
		&binding.arena,
		overlays,
		&mut labels.budget,
	);
	plan.adjacency_end = adjacency_end;
	plan.namespace = labels.graphs.get(&graph);
	plan.labels_pages = labels_pages;
	let mut changes = plan.patch_edges(edges, &labels.memberships)?;
	plan.include_nodes(nodes, &labels.memberships, &mut changes)?;
	plan.patch_nodes(changes)?;
	let (pages, header) = plan.finish()?;
	labels
		.budget
		.tree_entry::<(crate::graph::GraphId, GraphSuperblock)>(result.superblocks.len())?;
	result
		.pages
		.try_reserve_exact(pages.len())
		.map_err(|_error| invalid("EDGE publication page list allocation failed"))?;
	result.pages.extend(pages);
	result.superblocks.insert(graph, header);
	Ok(())
}

impl<'a, 'context> IncidencePlan<'a, 'context> {
	/// Separates accepted and final scope authority in one graph-local plan.
	fn new(
		store: &'a LocalStore,
		state: &'a LocalState,
		roots: (&'a DatabaseRoot, &'a DatabaseRoot),
		source: (GraphFileSet, &'a GraphArenaBindings, GraphSuperblock),
		final_arena: &'a GraphArenaBindings,
		overlays: Option<&'a mut GraphOverlays<'context>>,
		budget: &'a mut PublicationBudget,
	) -> Self {
		let (files, source_arena, header) = source;
		Self {
			store,
			state,
			before: roots.0,
			after: roots.1,
			files,
			source_arena,
			final_arena,
			header,
			adjacency_end: header.adjacency_overflow_pages,
			qualified_end: None,
			namespace: None,
			labels_pages: None,
			overlays,
			pages: Vec::new(),
			budget,
		}
	}

	/// Accounts final frames, vector growth and private WAL staging copies.
	fn reserve_page(&mut self) -> FerrosGrynnResult<()> {
		self.budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
		self.budget.stage_pages(1)?;
		self.pages
			.try_reserve_exact(1)
			.map_err(|_error| invalid("EDGE prepared page list allocation failed"))
	}

	/// Retains Hot write backing across nested adjacency and overlay
	/// preparation.
	///
	/// # Errors
	/// Rejects overflow, shared admission and allocation before filling the
	/// buffer.
	fn hot_writes<Slot>(&mut self, count: usize) -> FerrosGrynnResult<Vec<(usize, Slot)>> {
		let bytes = count
			.checked_mul(size_of::<(usize, Slot)>())
			.ok_or_else(|| invalid("Hot write scratch bytes overflow usize"))?;
		// Nested codecs must see the backing that remains live in the page
		// loop.
		self.budget.retain(bytes)?;
		let mut writes = Vec::new();
		writes
			.try_reserve_exact(count)
			.map_err(|_error| invalid("Hot write scratch allocation failed"))?;
		let excess = writes
			.capacity()
			.checked_sub(count)
			.and_then(|slots| slots.checked_mul(size_of::<(usize, Slot)>()))
			.ok_or_else(|| invalid("Hot write capacity bytes overflow usize"))?;
		self.budget.retain(excess)?;
		Ok(writes)
	}

	/// Seals the exact final ceilings with the database checkpoint authority.
	fn finish(mut self) -> FerrosGrynnResult<(PointPages, GraphSuperblock)> {
		let graph = self
			.after
			.graph(self.files.graph_id)
			.ok_or_else(|| invalid("EDGE target graph is absent"))?;
		self.header.edge_capacity = u64::try_from(graph.slot_capacity(AtomKind::Edge))
			.map_err(|_error| invalid("EDGE capacity exceeds u64"))?;
		self.header.edge_hot_pages = self.header.edge_capacity.div_ceil(HOT_SLOTS_PER_PAGE as u64);
		self.header.node_capacity = u64::try_from(graph.slot_capacity(AtomKind::Node))
			.map_err(|_error| invalid("NODE capacity exceeds u64"))?;
		self.header.node_hot_pages = self.header.node_capacity.div_ceil(HOT_SLOTS_PER_PAGE as u64);
		let statistics = self
			.after
			.statistics
			.graph(self.files.graph_id)
			.ok_or_else(|| invalid("incidence graph statistics are absent"))?;
		self.header.edge_count = statistics.edge_count;
		self.header.node_count = statistics.node_count;
		self.header.adjacency_overflow_pages = self.adjacency_end;
		if let Some(end) = self.qualified_end {
			self.header.cross_level_adjacency_overflow_pages = end;
		}
		if let Some(pages) = self.labels_pages {
			self.header.labels_pages = pages;
		}
		self.header.page_lsn = PageLsn::ZERO;
		self.header.checkpoint_generation = self.state.database_superblock.generation;
		self.header.backfilled_lsn = self.state.database_superblock.checkpoint_lsn;
		self.header.checkpoint_record_lsn = self.state.database_superblock.checkpoint_record_lsn;
		self.reserve_page()?;
		self.pages.push((
			PageKey::new(self.files.space(), self.files.superblock, PageNo::new(0)),
			Box::new(self.header.encode()?),
		));
		Ok((self.pages, self.header))
	}
}
