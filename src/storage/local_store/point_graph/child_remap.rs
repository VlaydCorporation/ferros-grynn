//! Direct leaf scope renumbering shares one admitted unpublished page plan.

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
	datastore::DatabaseRoot,
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::{
		graph_pages::{
			GraphArenaBindings,
			NodeSlotFlags,
			hot_slot_address,
			read_node_hot_slot,
			write_node_hot_slot,
		},
		graph_superblock::GraphSuperblock,
		local_store::point_namespace::{LabelPlan, PublicationBudget},
		page::{AlignedPage, PAGE_SIZE},
		types::PageKey,
	},
	values::storage::EncodeContext,
};

impl GraphUpdatePlan {
	/// Moves numeric child and role slots without changing logical occurrences.
	///
	/// # Errors
	/// Rejects source maps/owners/metadata, resource admission and I/O before
	/// publishing any final owner or page.
	pub(super) fn prepare_child_remap(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		rebind: leaf_rebind::LeafRebind,
		labels: &mut LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<()> {
		if !labels.targets.is_empty()
			|| !labels.graphs.is_empty()
			|| !labels.class_graphs.is_empty()
			|| !labels.memberships.is_empty()
		{
			return Err(invalid("child remap unexpectedly changes an index namespace"));
		}
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == rebind.graph)
			.ok_or_else(|| invalid("child remap source binding is absent"))?;
		let catalog = state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == rebind.graph)
			.ok_or_else(|| invalid("child remap source catalog graph is absent"))?;
		if !binding.arena.same_owner(&catalog.arena) {
			return Err(invalid("child remap source catalog arena owner disagrees"));
		}
		let accepted = admitted_arena(roots.0, rebind.graph, &mut labels.budget)?;
		if accepted != binding.arena {
			return Err(invalid("child remap source arena disagrees with canonical scopes"));
		}
		drop(accepted);
		let final_arena = admitted_arena(roots.1, rebind.graph, &mut labels.budget)?;
		let header = *state
			.graph_superblocks
			.get(&rebind.graph)
			.ok_or_else(|| invalid("child remap source graph superblock is absent"))?;
		let before = roots
			.0
			.graph(rebind.graph)
			.ok_or_else(|| invalid("child remap source graph is absent"))?;
		let end = store.visible_pages(state, binding.files.space(), binding.files.overlays)?;
		let mut overlays = GraphOverlays::new(end, context);
		for owner in before.occurrences() {
			let local = owner.local_id();
			let next = roots
				.1
				.occurrence(rebind.graph, local)
				.ok_or_else(|| invalid("child remap final owner is absent"))?;
			if next.global_id() != owner.global_id()
				|| roots.0.membership(rebind.graph, owner.global_id()) != Some(local)
				|| roots.1.membership(rebind.graph, owner.global_id()) != Some(local)
			{
				return Err(invalid("child remap full owner identity disagrees"));
			}
			if !local.is_node() {
				continue;
			}
			let old_slot = owner
				.child_graph()
				.map(|_child| binding.arena.direct_child_slot(roots.0, rebind.graph, local))
				.transpose()?;
			let new_slot = next
				.child_graph()
				.map(|_child| final_arena.direct_child_slot(roots.1, rebind.graph, local))
				.transpose()?;
			if old_slot == new_slot {
				continue;
			}
			let overlay = leaf_rebind::source_node_overlay(
				store,
				state,
				roots.0,
				(binding, header),
				(local, owner.global_id()),
				old_slot,
				// This source backing is released before overlay validation.
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
			let (number, within) = hot_slot_address(local.slot())?;
			let key = PageKey::new(binding.files.space(), binding.files.nodes, number);
			let index = self.remap_hot_page(store, state, key, &mut labels.budget)?;
			let page = &mut self.pages[index].1;
			let mut hot = read_node_hot_slot(page, number, within)?;
			hot.flags.set(NodeSlotFlags::META, new_slot.is_some());
			hot.subgraph_slot = new_slot;
			write_node_hot_slot(page, within, &hot)?;
			page.seal()?;
		}
		// EDGE and NODE keys are disjoint; each EDGE pass owns its append tail.
		let first_frame = self.pages.len();
		let (pages, header) = super::super::point_incidence::build_retained_edge_remap(
			store,
			state,
			roots,
			(binding.files, &binding.arena, header),
			&final_arena,
			&mut overlays,
			&mut labels.budget,
		)?;
		labels.budget.tree_entry::<(GraphId, GraphSuperblock)>(self.superblocks.len())?;
		self.pages
			.try_reserve_exact(pages.len())
			.map_err(|_error| invalid("child remap page list allocation failed"))?;
		self.pages.extend(pages);
		self.superblocks.insert(rebind.graph, header);
		let incidence_frames = first_frame..self.pages.len();
		labels.budget.tree_entry::<(GraphId, GraphArenaBindings)>(self.arenas.len())?;
		self.arenas.insert(rebind.graph, final_arena);
		self.finish_graph(state, (binding.files, end, incidence_frames, true), overlays, labels)
	}

	/// Coalesces all changed owners into one retained frame for a Hot page.
	///
	/// # Errors
	/// Rejects admission, vector allocation and source I/O before adding a
	/// page.
	fn remap_hot_page(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		key: PageKey,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<usize> {
		if let Some(index) = self.pages.iter().position(|(prepared, _)| *prepared == key) {
			return Ok(index);
		}
		budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
		budget.stage_pages(1)?;
		self.pages
			.try_reserve_exact(1)
			.map_err(|_error| invalid("child remap Hot page allocation failed"))?;
		let page = store.read_update_page(state, key)?;
		self.pages.push((key, page));
		Ok(self.pages.len() - 1)
	}
}
