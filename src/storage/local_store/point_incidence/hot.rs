//! Generation-aware EDGE sources are checked before any final Hot slot is
//! written.

use super::{IncidencePlan, NodeChanges, SlotTransition, invalid, structural_target};
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, OccurrenceStateRef},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, EdgeEndpoint, GraphId},
	storage::{
		graph_pages::{
			EdgeHotSlot,
			EdgeSlotFlags,
			EncodedEndpoint,
			EndpointSide,
			hot_slot_address,
			initialize_edge_hot_page,
			read_edge_hot_slot,
			resolve_topology,
			write_edge_hot_slot,
		},
		local_store::point_occurrence::SlotState,
		page::{AlignedPage, PageLsn},
		types::PageKey,
	},
	values::storage::DiskAtomRef,
};

/// Validates a retained EDGE without allocating final topology or Hot frames.
///
/// # Errors
/// Rejects old identity/ceiling, topology/role-chain, metadata, admission and
/// I/O failures before the leaf arena is copied.
pub(in crate::storage::local_store) fn validate_retained_edge_source(
	store: &super::LocalStore,
	state: &super::LocalState,
	before: &DatabaseRoot,
	source: (super::GraphFileSet, &super::GraphArenaBindings, super::GraphSuperblock),
	owner: (crate::graph::AtomId, GlobalAtomId),
	overlays: &mut super::GraphOverlays<'_>,
	budget: &mut super::PublicationBudget,
) -> FerrosGrynnResult<()> {
	let (files, arena, header) = source;
	let (local, global) = owner;
	let (number, within) = hot_slot_address(local.slot())?;
	if local.kind() != Some(AtomKind::Edge)
		|| global.kind() != Some(AtomKind::Edge)
		|| local.slot() >= header.edge_capacity
		|| number.get() >= header.edge_hot_pages
	{
		return Err(invalid("leaf rebind source EDGE lies outside its old identity/ceiling"));
	}
	budget.retain(crate::storage::page::PAGE_SIZE)?;
	let page = store.read_update_page(state, PageKey::new(files.space(), files.edges, number))?;
	let hot = read_edge_hot_slot(&page, number, within)?;
	// The source branch uses old ceilings and addresses; rendering a final
	// side here would append roles against the accepted, not final, arena.
	let mut plan = IncidencePlan::new(
		store,
		state,
		(before, before),
		(files, arena, header),
		arena,
		Some(overlays),
		budget,
	);
	plan.validate_edge_source(
		SlotState {
			generation: local.generation(),
			occurrence: Some((local, global)),
		},
		&hot,
	)
}

impl IncidencePlan<'_, '_> {
	/// Groups explicit slots by source page, including transient/free
	/// generations.
	///
	/// # Errors
	/// Rejects allocator/source/topology mismatches, admission and I/O
	/// failures.
	pub(super) fn patch_edges(
		&mut self,
		targets: &[SlotTransition],
		memberships: &[(GraphId, GlobalAtomId)],
	) -> FerrosGrynnResult<NodeChanges> {
		let roots = (self.before, self.after);
		let mut changes = NodeChanges::new();
		let mut start = 0;
		while start < targets.len() {
			let number = hot_slot_address(targets[start].slot)?.0;
			let mut end = start + 1;
			while end < targets.len() && hot_slot_address(targets[end].slot)?.0 == number {
				end += 1;
			}
			if !targets[start..end]
				.iter()
				.any(|target| structural_target(target, memberships, roots))
			{
				start = end;
				continue;
			}
			self.reserve_page()?;
			let mut writes = self.hot_writes::<EdgeHotSlot>(end - start)?;
			let key = PageKey::new(self.files.space(), self.files.edges, number);
			let mut page = if number.get() < self.header.edge_hot_pages {
				self.store.read_update_page(self.state, key)?
			} else {
				let mut page = Box::new(AlignedPage::zeroed());
				initialize_edge_hot_page(&mut page, number, PageLsn::ZERO)?;
				page
			};

			for transition in targets[start..end]
				.iter()
				.filter(|target| structural_target(target, memberships, roots))
			{
				if transition.kind != AtomKind::Edge {
					return Err(invalid("EDGE membership plan contains a NODE slot"));
				}
				let (_, within) = hot_slot_address(transition.slot)?;
				let previous = read_edge_hot_slot(&page, number, within)?;
				let old = transition.before.unwrap_or(SlotState {
					generation: 0,
					occurrence: None,
				});
				self.validate_edge_source(old, &previous)?;
				let accepted = transition.after.ok_or_else(|| invalid("EDGE allocator shrank"))?;
				let mut target = self.edge_slot(self.after, accepted, None, self.final_arena)?;
				target.overlay = self.target_overlay(old, accepted, previous.overlay)?;
				target.flags.set(
					EdgeSlotFlags::HAS_OVERLAY,
					!matches!(target.overlay, crate::storage::atom_page::PropertyRef::None),
				);
				writes.push((within, target));
				for (root, state, inserted) in
					[(self.before, old, false), (self.after, accepted, true)]
				{
					if let Some((local, _)) = state.occurrence {
						let occurrence = root
							.occurrence(self.files.graph_id, local)
							.ok_or_else(|| invalid("EDGE delta occurrence disappeared"))?;
						let OccurrenceStateRef::Edge {
							topology,
							..
						} = occurrence.state().view()
						else {
							return Err(invalid("EDGE delta has NODE state"));
						};
						self.record_changes(&mut changes, local, topology, inserted)?;
					}
				}
			}
			// Source decoding must finish before the first write changes its
			// checksum.
			for (within, slot) in writes {
				write_edge_hot_slot(&mut page, within, &slot)?;
			}
			page.seal()?;
			self.pages.push((key, page));
			start = end;
		}
		Ok(changes)
	}

	/// Checks the complete accepted source without rendering any final side.
	///
	/// # Errors
	/// Preserves source topology, role-chain, transition, metadata and
	/// admission errors.
	pub(super) fn validate_edge_source(
		&mut self,
		state: SlotState,
		source: &EdgeHotSlot,
	) -> FerrosGrynnResult<()> {
		let expected = self.edge_slot(self.before, state, Some(source), self.source_arena)?;
		if *source != expected {
			return Err(invalid("EDGE Hot source disagrees with accepted root"));
		}
		Ok(())
	}

	/// Uses old chain addresses only for comparison; final sides append fresh
	/// pages.
	pub(super) fn edge_slot(
		&mut self,
		root: &DatabaseRoot,
		state: SlotState,
		source: Option<&EdgeHotSlot>,
		arena: &super::GraphArenaBindings,
	) -> FerrosGrynnResult<EdgeHotSlot> {
		let Some((local, global)) = state.occurrence else {
			return Ok(EdgeHotSlot::tombstone(state.generation));
		};
		let occurrence = root
			.occurrence(self.files.graph_id, local)
			.ok_or_else(|| invalid("plain EDGE occurrence is absent"))?;
		let OccurrenceStateRef::Edge {
			topology,
			weight,
			transition_graph,
		} = occurrence.state().view()
		else {
			return Err(invalid("plain EDGE state changed during preparation"));
		};
		// Both role collectors can retain minimum and spare Vec capacity.
		let topology_bytes = topology
			.endpoint_count()
			.checked_add(8)
			.and_then(|count| count.checked_mul(2 * size_of::<EncodedEndpoint>()))
			.ok_or_else(|| invalid("EDGE topology bytes overflow"))?;
		self.budget.retain(topology_bytes)?;
		let encoded = resolve_topology(topology, |endpoint| {
			if let EdgeEndpoint::Nested(qualified) = endpoint {
				let (graph_slot, atom) = arena.qualified(root, self.files.graph_id, qualified)?;
				return Ok(EncodedEndpoint {
					graph_slot,
					atom,
				});
			}
			let node =
				endpoint.local_id().ok_or_else(|| invalid("plain EDGE endpoint is not local"))?;
			let kind = node.kind().ok_or_else(|| invalid("plain EDGE endpoint is null"))?;
			let target = root
				.occurrence(self.files.graph_id, node)
				.ok_or_else(|| invalid("plain EDGE endpoint occurrence is absent"))?;
			if root.membership(self.files.graph_id, target.global_id()) != Some(node) {
				return Err(invalid("plain EDGE endpoint membership disagrees"));
			}
			Ok(EncodedEndpoint {
				graph_slot: 0,
				atom: DiskAtomRef::from_parts(kind, node.slot())?,
			})
		})?;
		let actual_bytes = encoded
			.source
			.capacity()
			.checked_add(encoded.target.capacity())
			.and_then(|count| count.checked_mul(size_of::<EncodedEndpoint>()))
			.ok_or_else(|| invalid("EDGE topology capacity overflows"))?;
		if let Some(excess) = actual_bytes.checked_sub(topology_bytes) {
			self.budget.retain(excess)?;
		}
		let cross_level = topology.is_cross_level();
		let left =
			self.endpoint_side(&encoded.source, EndpointSide::Source, cross_level, source)?;
		let right =
			self.endpoint_side(&encoded.target, EndpointSide::Target, cross_level, source)?;
		let transition = transition_graph
			.map(|_child| arena.direct_child_slot(root, self.files.graph_id, local))
			.transpose()?;
		let weight = weight.map(f64::try_from).transpose()?;
		let mut slot = if cross_level || transition.is_some() {
			EdgeHotSlot::live(
				state.generation,
				global,
				&encoded,
				(&left, &right),
				cross_level,
				None,
				weight,
				crate::storage::atom_page::PropertyRef::None,
				transition,
			)?
		} else {
			EdgeHotSlot::plain(
				state.generation,
				global,
				&encoded,
				(left.local_overflow, right.local_overflow),
				weight,
			)?
		};
		let label = if source.is_some() {
			self.source_label(state)?
		} else {
			self.target_label(state)?
		};
		if let Some(label) = label {
			slot.replace_label(hot_slot_address(local.slot())?.0, None, Some(label))?;
		}
		if let Some(source) = source {
			self.validate_source_overlay(state, source.overlay)?;
			slot.overlay = source.overlay;
			slot.flags.set(
				EdgeSlotFlags::HAS_OVERLAY,
				!matches!(source.overlay, crate::storage::atom_page::PropertyRef::None),
			);
		}
		Ok(slot)
	}
}
