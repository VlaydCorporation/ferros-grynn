//! Closed leaf-child changes share logical eligibility and source metadata.

use std::sync::Arc;

use super::{
	GraphOverlays,
	GraphUpdatePlan,
	LocalState,
	LocalStore,
	ancestor_leaf,
	invalid,
	stable_graph,
};
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, OccurrenceStateRef, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, EdgeEndpoint, GraphId, GraphKind},
	storage::{
		atom_page::PropertyRef,
		graph_pages::{
			GraphArenaBindings,
			NodeSlotFlags,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		graph_superblock::GraphSuperblock,
		local_store::{
			GraphBinding,
			point_label,
			point_namespace::LabelPlan,
			point_update::supports_point_indexes,
		},
		page::{AlignedPage, PAGE_SIZE},
		types::PageKey,
	},
	values::{RecordId, storage::EncodeContext},
};

/// A closed replay delta changes the child of one unchanged NODE/EDGE owner.
pub(super) struct LeafRebind {
	/// Selected scope and every admitted ancestor publish in one catalog plan.
	pub(super) graph: GraphId,
	/// Full generation-aware container identity stays unchanged.
	pub(super) local: AtomId,
	/// Physical source and both membership maps must agree on this Base owner.
	pub(super) global: GlobalAtomId,
	/// Accepted source child supplies the old directory counters.
	pub(super) before_child: Option<GraphId>,
	/// An unchanged, already published child supplies the final counters.
	pub(super) after_child: Option<GraphId>,
}

/// Recognizes only a leaf rebind whose physical scope numbering cannot move.
pub(super) fn candidate(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> Option<LeafRebind> {
	let rebind = candidate_core(before, after, delta)?;
	(!presence_moves_survivors(before, &rebind)).then_some(rebind)
}

/// Direct leaf presence may move other slots without changing logical roles.
pub(super) fn remap_candidate(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> Option<LeafRebind> {
	let rebind = candidate_core(before, after, delta)?;
	if !presence_moves_survivors(before, &rebind) {
		return None;
	}
	// shortcut: selected-owner presence and shifted ancestor scopes retain
	// the full publisher until their complete physical fanout is supported.
	let closed = before.graph(rebind.graph)?.occurrences().all(|owner| {
		(owner.local_id() == rebind.local
			|| after.occurrence(rebind.graph, owner.local_id()) == Some(owner))
			&& owner.child_graph().is_none_or(|child| public_leaf(before, after, child))
	});
	closed.then_some(rebind)
}

/// Presence alone changes dense positions when another child survives.
fn presence_moves_survivors(before: &DatabaseRoot, rebind: &LeafRebind) -> bool {
	(rebind.before_child.is_none() || rebind.after_child.is_none())
		&& before.graph(rebind.graph).is_some_and(|graph| {
			graph
				.occurrences()
				.any(|owner| owner.local_id() != rebind.local && owner.child_graph().is_some())
		})
}

/// One incidence key permits independent stable Base edits alongside it.
fn closed_delta(before: &DatabaseRoot, after: &DatabaseRoot, delta: &PublicationDelta) -> bool {
	if !delta.point_graph_only
		|| delta.incidence.len() != 1
		|| !delta.overlays.is_empty()
		|| !delta.memberships.is_empty()
		|| !Arc::ptr_eq(&before.dictionary, &after.dictionary)
	{
		return false;
	}
	if delta.records.is_empty() && delta.global_slots.is_empty() {
		return Arc::ptr_eq(&before.atoms, &after.atoms);
	}
	detached_base_delta(before, after, delta)
}

/// Detached Bases have no Effective consumers skipped by leaf preparation.
fn detached_base_delta(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> bool {
	if delta.records.is_empty()
		|| delta.records.len() != delta.global_slots.len()
		|| before.atoms.slot_capacity() != after.atoms.slot_capacity()
	{
		return false;
	}
	let Some(&(graph, local)) = delta.incidence.first() else {
		return false;
	};
	let (Some(old), Some(new)) = (before.occurrence(graph, local), after.occurrence(graph, local))
	else {
		return false;
	};
	// Mixed presence retains the full publisher until its fanout is supported.
	old.child_graph().is_some()
		&& new.child_graph().is_some()
		&& delta.records.iter().all(|record| {
			detached_base_owner(before, after, record).is_some_and(|global| {
				// Record and slot orders differ; resolve each stable identity.
				delta.global_slots.contains(&global.slot())
					&& old.global_id() != global
					&& new.global_id() != global
			})
		})
}

/// Checks one stable detached identity independently of journal cardinality.
fn detached_base_owner(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	record: &RecordId,
) -> Option<GlobalAtomId> {
	let global = before.resolve_atom(record)?;
	if after.resolve_atom(record) != Some(global)
		|| [before, after]
			.into_iter()
			.any(|root| root.memberships(global).is_some_and(|members| !members.is_empty()))
	{
		return None;
	}
	let (old, new) = before.atom(global).zip(after.atom(global))?;
	(old.class_id() == new.class_id() && old.record_id() == record && new.record_id() == record)
		.then_some(global)
}

/// Both physical plans share the same complete logical owner contract.
fn candidate_core(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> Option<LeafRebind> {
	if !closed_delta(before, after, delta) {
		return None;
	}
	let &(graph, local) = delta.incidence.first()?;
	if !matches!(local.kind(), Some(AtomKind::Node | AtomKind::Edge)) {
		return None;
	}
	let old = before.occurrence(graph, local)?;
	let new = after.occurrence(graph, local)?;
	let (old_child, new_child) = match (old.state().view(), new.state().view()) {
		(
			OccurrenceStateRef::Node {
				nested_graph: old,
			},
			OccurrenceStateRef::Node {
				nested_graph: new,
			},
		) => (*old, *new),
		(
			OccurrenceStateRef::Edge {
				topology: old_roles,
				weight: old_weight,
				transition_graph: old,
			},
			OccurrenceStateRef::Edge {
				topology: new_roles,
				weight: new_weight,
				transition_graph: new,
			},
		) if old_roles == new_roles && old_weight == new_weight => (*old, *new),
		_ => return None,
	};
	let global = old.global_id();
	if old_child == new_child
		|| new.global_id() != global
		|| before.membership(graph, global) != Some(local)
		|| after.membership(graph, global) != Some(local)
		|| old.label() != new.label()
		|| old.overlay() != new.overlay()
		|| !supports_point_indexes(after, after.atom(global)?.class_id())
		|| old_child.into_iter().chain(new_child).any(|child| !public_leaf(before, after, child))
		|| (!isolated_parent(before, after, delta, graph)
			&& (old_child.is_none()
				|| new_child.is_none()
				|| !ancestor_leaf::eligible(before, after, delta, graph)))
		// Presence changes cannot strand selected-owner qualified routes. A
		// present leaf replacement keeps their physical slot; commit validation
		// separately requires an explicit anchor for the new terminal identity.
		|| ((old_child.is_none() || new_child.is_none())
			&& !unreferenced_owner(before, graph, local))
	{
		return None;
	}
	Some(LeafRebind {
		graph,
		local,
		global,
		before_child: old_child,
		after_child: new_child,
	})
}

/// Public leaf identity and contents are already durable in both snapshots.
fn public_leaf(before: &DatabaseRoot, after: &DatabaseRoot, child: GraphId) -> bool {
	before.schema.graphs.get(&child).is_some_and(|schema| !schema.is_internal())
		&& before.graphs.get(&child).zip(after.graphs.get(&child)).is_some_and(|(old, new)| {
			Arc::ptr_eq(old, new) && old.occurrences().all(|owner| owner.child_graph().is_none())
		})
}

/// Public containment families share stable physical parent metadata.
pub(super) fn stable_parent(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	graph: GraphId,
) -> bool {
	if !before.schema.graphs.get(&graph).is_some_and(|schema| {
		!schema.is_internal()
			&& matches!(schema.reference().kind(), GraphKind::Graph | GraphKind::Metagraph)
	}) || !before
		.graph(graph)
		.zip(after.graph(graph))
		.is_some_and(|(old, new)| stable_graph(graph, old, new, delta))
		|| before.graphs.len() != after.graphs.len()
		|| before.schema.graphs.iter().any(|(id, schema)| {
			after.schema.graphs.get(id).is_none_or(|next| !Arc::ptr_eq(schema, next))
		}) {
		return false;
	}
	true
}

/// Structural isolation keeps leaf changes in one published parent arena.
fn isolated_parent(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	graph: GraphId,
) -> bool {
	if !stable_parent(before, after, delta, graph) {
		return false;
	}
	// Ancestor arenas require the full publisher.
	before.graphs.iter().all(|(id, source)| {
		(*id == graph || after.graphs.get(id).is_some_and(|next| Arc::ptr_eq(source, next)))
			&& source.occurrences().all(|owner| owner.child_graph() != Some(graph))
	})
}

/// No route in this parent currently traverses the selected owner.
fn unreferenced_owner(before: &DatabaseRoot, graph: GraphId, local: AtomId) -> bool {
	before.graph(graph).is_some_and(|parent| {
		parent.occurrences().all(|owner| {
			match owner.state().view() {
			OccurrenceStateRef::Node { .. } => true,
			OccurrenceStateRef::Edge { topology, .. } => topology.endpoints().all(|endpoint| {
				!matches!(endpoint, EdgeEndpoint::Nested(route) if route.scope().first() == Some(&local))
			}),
		}
		})
	})
}

impl GraphUpdatePlan {
	/// Prepares the final owner and counters before the shared WAL barrier.
	///
	/// # Errors
	/// Rejects source identity, arena, payload, directory, admission and I/O
	/// failures.
	pub(super) fn prepare_leaf_rebind(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		rebind: LeafRebind,
		labels: &mut LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<()> {
		if !labels.targets.is_empty()
			|| !labels.graphs.is_empty()
			|| !labels.class_graphs.is_empty()
			|| !labels.memberships.is_empty()
		{
			return Err(invalid("leaf rebind unexpectedly changes an index namespace"));
		}
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == rebind.graph)
			.ok_or_else(|| invalid("leaf rebind source graph binding is absent"))?;
		let catalog = state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == rebind.graph)
			.ok_or_else(|| invalid("leaf rebind source catalog graph is absent"))?;
		if !binding.arena.same_owner(&catalog.arena) {
			return Err(invalid("leaf rebind source catalog arena owner disagrees"));
		}
		let slot = rebind
			.before_child
			.map(|_child| binding.arena.direct_child_slot(roots.0, rebind.graph, rebind.local))
			.transpose()?;
		validate_arena(&binding.arena, &rebind, slot)?;
		let header = state
			.graph_superblocks
			.get(&rebind.graph)
			.ok_or_else(|| invalid("leaf rebind source graph superblock is absent"))?;

		let owner = roots
			.0
			.occurrence(rebind.graph, rebind.local)
			.ok_or_else(|| invalid("leaf rebind source occurrence is absent"))?;
		let node_overlay = if rebind.local.kind() == Some(AtomKind::Node) {
			Some(source_node_overlay(
				store,
				state,
				roots.0,
				(binding, *header),
				(rebind.local, rebind.global),
				slot,
				|bytes| labels.budget.retain(bytes),
			)?)
		} else {
			None
		};
		let end = store.visible_pages(state, binding.files.space(), binding.files.overlays)?;
		let mut overlays = GraphOverlays::new(end, context);
		if let Some(reference) = node_overlay {
			overlays.validate_source(
				store,
				state,
				(binding.files, header.overlays_pages),
				reference,
				owner.overlay(),
				&mut labels.budget,
			)?;
		} else {
			super::super::point_incidence::validate_retained_edge_source(
				store,
				state,
				roots.0,
				(binding.files, &binding.arena, *header),
				(rebind.local, rebind.global),
				&mut overlays,
				&mut labels.budget,
			)?;
		}
		// Retain the private copy through catalog encoding and WAL; accepted
		// maps and old snapshots keep their original immutable owner.
		labels.budget.retain(binding.arena.retained_copy_bytes()?)?;
		labels.budget.scratch(4096)?;
		labels.budget.tree_entry::<(GraphId, GraphArenaBindings)>(self.arenas.len())?;
		if rebind.before_child.is_none() {
			labels.budget.tree_entry::<(u32, GraphId)>(binding.arena.graph_by_slot().len())?;
			labels
				.budget
				.tree_entry::<(u32, Box<[AtomId]>)>(binding.arena.scope_by_slot().len())?;
			labels.budget.retain(size_of::<AtomId>())?;
		}
		let mut graphs = binding.arena.graph_by_slot().clone();
		let mut scopes = binding.arena.scope_by_slot().clone();
		let final_slot = rebind.after_child.map(|_child| slot.unwrap_or(1));
		if let Some(child) = rebind.after_child {
			let selected = final_slot.ok_or_else(|| invalid("leaf rebind final slot is absent"))?;
			graphs.insert(selected, child);
			if slot.is_none() {
				scopes.insert(selected, vec![rebind.local].into_boxed_slice());
			}
		} else if let Some(slot) = slot {
			graphs.remove(&slot);
			scopes.remove(&slot);
		}
		let arena = GraphArenaBindings::new(graphs, scopes);
		if rebind.after_child.is_some()
			&& Some(arena.direct_child_slot(roots.1, rebind.graph, rebind.local)?) != final_slot
		{
			return Err(invalid("leaf rebind final slot changed"));
		}
		if slot != final_slot {
			// Only presence changes write Hot metadata; unchanged
			// role/adjacency bytes and every other owner remain at
			// their accepted addresses.
			labels.budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
			labels.budget.stage_pages(1)?;
			self.pages
				.try_reserve_exact(1)
				.map_err(|_error| invalid("leaf child Hot page allocation failed"))?;
			let (number, within) = hot_slot_address(rebind.local.slot())?;
			let file = if rebind.local.is_node() {
				binding.files.nodes
			} else {
				binding.files.edges
			};
			let key = PageKey::new(binding.files.space(), file, number);
			let mut page = store.read_update_page(state, key)?;
			if rebind.local.is_node() {
				let mut hot = read_node_hot_slot(&page, number, within)?;
				hot.flags.set(NodeSlotFlags::META, final_slot.is_some());
				hot.subgraph_slot = final_slot;
				write_node_hot_slot(&mut page, within, &hot)?;
			} else {
				let mut hot = read_edge_hot_slot(&page, number, within)?;
				hot.replace_transition(number, slot, final_slot)?;
				write_edge_hot_slot(&mut page, within, &hot)?;
			}
			page.seal()?;
			self.pages.push((key, page));
		}
		self.arenas.insert(rebind.graph, arena);
		self.finish_graph(state, (binding.files, end, 0..0, true), overlays, labels)?;
		Ok(())
	}
}

/// Stable and remapped children validate the same accepted NODE metadata.
///
/// # Errors
/// Preserves the old ceiling, identity, META/slot, label, admission and I/O
/// failures before returning the borrowed source overlay address.
pub(super) fn source_node_overlay(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	source: (&GraphBinding, GraphSuperblock),
	owner: (AtomId, GlobalAtomId),
	slot: Option<u32>,
	mut admit: impl FnMut(usize) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<PropertyRef> {
	let (binding, header) = source;
	let (local, global) = owner;
	let owner = before
		.occurrence(binding.graph_id, local)
		.ok_or_else(|| invalid("leaf rebind source occurrence is absent"))?;
	let (number, within) = hot_slot_address(local.slot())?;
	if local.slot() >= header.node_capacity || number.get() >= header.node_hot_pages {
		return Err(invalid("leaf rebind source NODE lies outside its old ceiling"));
	}
	admit(PAGE_SIZE)?;
	let page = store.read_update_page(
		state,
		PageKey::new(binding.files.space(), binding.files.nodes, number),
	)?;
	let mut hot = read_node_hot_slot(&page, number, within)?;
	if hot.generation != local.generation()
		|| hot.global_atom != Some(global)
		|| hot.flags.contains(NodeSlotFlags::META) != slot.is_some()
		|| hot.subgraph_slot != slot
	{
		return Err(invalid("leaf rebind source NODE identity or child slot disagrees"));
	}
	let label = point_label::label_id(before, binding.graph_id, AtomKind::Node, owner.label())?;
	hot.replace_label(number, label, label)?;
	Ok(hot.overlay)
}

/// Checks dense accepted scopes for replacement or zero/one-child presence.
/// Presence changes admit no surviving child whose numeric scope could move.
///
/// # Errors
/// Rejects a foreign root, mismatched direct owner/child or non-dense maps.
fn validate_arena(
	arena: &GraphArenaBindings,
	rebind: &LeafRebind,
	slot: Option<u32>,
) -> FerrosGrynnResult<()> {
	if arena.graph_by_slot().get(&0) != Some(&rebind.graph)
		|| arena.scope_by_slot().get(&0).is_none_or(|scope| !scope.is_empty())
		|| slot.is_some_and(|slot| {
			arena.graph_by_slot().get(&slot) != rebind.before_child.as_ref()
				|| arena
					.scope_by_slot()
					.get(&slot)
					.is_none_or(|scope| scope.as_ref() != [rebind.local])
		})
		|| arena.graph_by_slot().len() != arena.scope_by_slot().len()
		|| ((rebind.before_child.is_none() || rebind.after_child.is_none())
			&& arena.graph_by_slot().len() != 1 + usize::from(rebind.before_child.is_some()))
	{
		return Err(invalid("leaf rebind accepted direct scope disagrees"));
	}
	for (ordinal, ((graph_slot, _), (scope_slot, scope))) in
		arena.graph_by_slot().iter().zip(arena.scope_by_slot()).enumerate()
	{
		if u32::try_from(ordinal).ok() != Some(*graph_slot)
			|| graph_slot != scope_slot
			|| (scope.len() > 1 && scope.first() == Some(&rebind.local))
		{
			return Err(invalid("leaf rebind arena is sparse or contains selected descendants"));
		}
	}
	Ok(())
}

#[cfg(test)]
/// Closed replay and route identity determine the supported fallback boundary.
mod tests;
