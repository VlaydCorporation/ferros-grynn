//! Retained EDGE roles follow final child scopes without changing incidence.

use super::{IncidencePlan, PointPages, invalid};
use crate::{
	datastore::{DatabaseRoot, GraphOccurrence, OccurrenceStateRef},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, EdgeEndpoint},
	storage::{
		atom_page::PropertyRef,
		graph_pages::{
			EdgeSlotFlags,
			GraphArenaBindings,
			hot_slot_address,
			read_edge_hot_slot,
			write_edge_hot_slot,
		},
		graph_store::GraphFileSet,
		graph_superblock::GraphSuperblock,
		local_store::{
			LocalState,
			LocalStore,
			point_graph::GraphOverlays,
			point_namespace::PublicationBudget,
			point_occurrence::SlotState,
		},
		types::PageKey,
	},
};

/// Shares one append ceiling and Hot frame per key across retained EDGE owners.
///
/// # Errors
/// Rejects source identity, topology, metadata, admission, codec and I/O
/// failures.
pub(in crate::storage::local_store) fn build_retained_edge_remap(
	store: &LocalStore,
	state: &LocalState,
	roots: (&DatabaseRoot, &DatabaseRoot),
	source: (GraphFileSet, &GraphArenaBindings, GraphSuperblock),
	final_arena: &GraphArenaBindings,
	overlays: &mut GraphOverlays<'_>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<(PointPages, GraphSuperblock)> {
	let graph = roots
		.0
		.graph(source.0.graph_id)
		.ok_or_else(|| invalid("child remap source graph is absent"))?;
	let mut plan =
		IncidencePlan::new(store, state, roots, source, final_arena, Some(overlays), budget);
	for owner in graph.occurrences().filter(|owner| owner.local_id().is_edge()) {
		plan.remap_retained_edge(owner)?;
	}
	plan.finish()
}

impl IncidencePlan<'_, '_> {
	/// Numeric role changes do not remove or add logical NODE adjacency.
	///
	/// # Errors
	/// Rejects replay/source mismatches and unpublished output failures.
	fn remap_retained_edge(&mut self, owner: &GraphOccurrence) -> FerrosGrynnResult<()> {
		let local = owner.local_id();
		let graph = self.files.graph_id;
		let next = self
			.after
			.occurrence(graph, local)
			.ok_or_else(|| invalid("child remap final EDGE is absent"))?;
		if next.global_id() != owner.global_id()
			|| self.before.membership(graph, owner.global_id()) != Some(local)
			|| self.after.membership(graph, owner.global_id()) != Some(local)
		{
			return Err(invalid("child remap full EDGE identity disagrees"));
		}
		let old_slot = owner
			.child_graph()
			.map(|_child| self.source_arena.direct_child_slot(self.before, graph, local))
			.transpose()?;
		let new_slot = next
			.child_graph()
			.map(|_child| self.final_arena.direct_child_slot(self.after, graph, local))
			.transpose()?;
		let OccurrenceStateRef::Edge {
			topology,
			..
		} = owner.state().view()
		else {
			return Err(invalid("child remap EDGE has NODE state"));
		};
		let mut roles_changed = false;
		for endpoint in topology.endpoints() {
			if let EdgeEndpoint::Nested(route) = endpoint {
				let old = self.source_arena.qualified(self.before, graph, route)?;
				let new = self.final_arena.qualified(self.after, graph, route)?;
				roles_changed |= old != new;
			}
		}
		if old_slot == new_slot && !roles_changed {
			return Ok(());
		}
		let (number, within) = hot_slot_address(local.slot())?;
		if owner.global_id().kind() != Some(AtomKind::Edge)
			|| local.slot() >= self.header.edge_capacity
			|| number.get() >= self.header.edge_hot_pages
		{
			return Err(invalid("child remap source EDGE lies outside its old identity/ceiling"));
		}
		let key = PageKey::new(self.files.space(), self.files.edges, number);
		// Generation order differs from physical slot order after reuse.
		let index =
			if let Some(index) = self.pages.iter().position(|(prepared, _)| *prepared == key) {
				index
			} else {
				self.reserve_page()?;
				let page = self.store.read_update_page(self.state, key)?;
				self.pages.push((key, page));
				self.pages.len() - 1
			};
		let previous = read_edge_hot_slot(&self.pages[index].1, number, within)?;
		let state = SlotState {
			generation: local.generation(),
			occurrence: Some((local, owner.global_id())),
		};
		self.validate_edge_source(state, &previous)?;
		let overlay = previous.overlay;
		let mut final_slot = if roles_changed {
			self.edge_slot(self.after, state, None, self.final_arena)?
		} else {
			let mut retained = previous;
			// A transition-only owner retains both already validated role
			// chains.
			retained.replace_transition(number, old_slot, new_slot)?;
			retained
		};
		final_slot.overlay = self.target_overlay(state, state, overlay)?;
		final_slot
			.flags
			.set(EdgeSlotFlags::HAS_OVERLAY, !matches!(final_slot.overlay, PropertyRef::None));
		write_edge_hot_slot(&mut self.pages[index].1, within, &final_slot)?;
		self.pages[index].1.seal()?;
		Ok(())
	}
}
