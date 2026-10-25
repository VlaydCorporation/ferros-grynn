//! Logical containment selects every immutable arena affected by a leaf rebind.

use std::sync::Arc;

use super::{
	GraphOverlays,
	GraphUpdatePlan,
	LocalState,
	LocalStore,
	admitted_arena,
	invalid,
	leaf_rebind,
};
use crate::{
	datastore::{DatabaseRoot, GraphRoot, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{GraphId, GraphKind},
	storage::{graph_pages::GraphArenaBindings, local_store::point_namespace::LabelPlan},
	values::storage::EncodeContext,
};

/// Containment comes from the logical source, never a possibly false cache map.
fn contains(parent: &GraphRoot, child: GraphId) -> bool {
	parent.occurrences().any(|owner| owner.child_graph() == Some(child))
}

/// Checks two hops even when a shorter route reaches the same graph.
fn contains_via_parent(root: &DatabaseRoot, owner: &GraphRoot, graph: GraphId) -> bool {
	owner
		.occurrences()
		.filter_map(|container| container.child_graph().and_then(|child| root.graph(child)))
		.any(|parent| contains(parent, graph))
}

/// Selects a physical container whose child arena includes the changed root.
fn affected_child(root: &DatabaseRoot, child: GraphId, graph: GraphId) -> bool {
	child == graph || root.graph(child).is_some_and(|parent| contains(parent, graph))
}

/// Logical paths select owners independently of possibly false cached arenas.
fn affected(root: &DatabaseRoot, owner: &GraphRoot, graph: GraphId) -> bool {
	contains(owner, graph) || contains_via_parent(root, owner, graph)
}

/// Present leaf replacement supports public Graph/Metagraph ancestors through
/// two levels.
pub(super) fn eligible(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	graph: GraphId,
) -> bool {
	if !leaf_rebind::stable_parent(before, after, delta, graph) {
		return false;
	}
	before.graphs.iter().all(|(id, source)| {
		(*id == graph || after.graphs.get(id).is_some_and(|next| Arc::ptr_eq(source, next)))
			&& (!affected(before, source, graph)
				|| before.schema.graphs.get(id).is_some_and(|schema| {
					!schema.is_internal()
                        && matches!(schema.reference().kind(), GraphKind::Graph | GraphKind::Metagraph)
				}))
			// shortcut: paths beyond two ancestor levels retain full publication
			// until their source, resource and recovery matrix is covered.
			&& source.occurrences().all(|owner| {
				owner.child_graph().is_none_or(|child| {
					!before
						.graph(child)
						.is_some_and(|parent| contains_via_parent(before, parent, graph))
				})
			})
	})
}

impl GraphUpdatePlan {
	/// Adds canonical final owners to the existing unpublished catalog plan.
	///
	/// # Errors
	/// Rejects absent/foreign authorities, noncanonical source maps, moved
	/// scopes and cumulative admission before any WAL publication or
	/// runtime install.
	pub(super) fn prepare_ancestor_arenas(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		graph: GraphId,
		labels: &mut LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<()> {
		if !roots.0.graphs.values().any(|parent| contains(parent, graph)) {
			return Ok(());
		}
		for (id, source) in &roots.0.graphs {
			if *id != graph && !affected(roots.0, source, graph) {
				continue;
			}
			self.prepare_ancestor_arena(store, state, roots, (id, source, graph), labels, context)?;
		}
		Ok(())
	}

	/// Validates one selected owner independently of ancestor discovery.
	///
	/// # Errors
	/// Preserves source authority, canonical scope and shared admission errors
	/// before WAL publication or runtime installation.
	fn prepare_ancestor_arena(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		owner: (&GraphId, &GraphRoot, GraphId),
		labels: &mut LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<()> {
		let (id, source, graph) = owner;
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == *id)
			.ok_or_else(|| invalid("ancestor leaf source binding is absent"))?;
		let catalog = state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == *id)
			.ok_or_else(|| invalid("ancestor leaf source catalog graph is absent"))?;
		if !binding.arena.same_owner(&catalog.arena) {
			return Err(invalid("ancestor leaf catalog arena owner disagrees"));
		}
		// Every temporary and retained root uses the caller's same
		// envelope.
		let accepted = admitted_arena(roots.0, *id, &mut labels.budget)?;
		if accepted != binding.arena {
			return Err(invalid("ancestor leaf source arena disagrees with canonical scopes"));
		}
		drop(accepted);
		if *id != graph {
			let header = *state
				.graph_superblocks
				.get(id)
				.ok_or_else(|| invalid("ancestor leaf source superblock is absent"))?;
			let end = store.visible_pages(state, binding.files.space(), binding.files.overlays)?;
			let mut overlays = GraphOverlays::new(end, context);
			for owner in source.occurrences().filter(|owner| {
				owner.child_graph().is_some_and(|child| affected_child(roots.0, child, graph))
			}) {
				let local = owner.local_id();
				let slot = binding.arena.direct_child_slot(roots.0, *id, local)?;
				if local.is_node() {
					let overlay = leaf_rebind::source_node_overlay(
						store,
						state,
						roots.0,
						(binding, header),
						(local, owner.global_id()),
						Some(slot),
						|bytes| labels.budget.scratch(bytes),
					)?;
					overlays.validate_source(
						store,
						state,
						(binding.files, header.overlays_pages),
						overlay,
						owner.overlay(),
						&mut labels.budget,
					)?;
				} else {
					super::super::point_incidence::validate_retained_edge_source(
						store,
						state,
						roots.0,
						(binding.files, &binding.arena, header),
						(local, owner.global_id()),
						&mut overlays,
						&mut labels.budget,
					)?;
				}
			}
		}
		let final_arena = admitted_arena(roots.1, *id, &mut labels.budget)?;
		if final_arena.scope_by_slot() != binding.arena.scope_by_slot() {
			return Err(invalid("ancestor leaf replacement moved numeric scopes"));
		}
		if let Some(prepared) = self.arenas.get(id) {
			// The existing leaf constructor must equal the full canonical
			// result, including unchanged aliases of the selected old
			// GraphId.
			if prepared != &final_arena {
				return Err(invalid("ancestor leaf prepared arena is not canonical"));
			}
		} else {
			labels.budget.tree_entry::<(GraphId, GraphArenaBindings)>(self.arenas.len())?;
			self.arenas.insert(*id, final_arena);
		}
		Ok(())
	}
}
