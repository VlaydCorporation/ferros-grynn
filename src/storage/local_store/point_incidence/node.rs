//! NODE lifecycle and adjacency share one final image per physical Hot page.

use super::{
	AdjacencyChange,
	IncidencePlan,
	NodeChanges,
	NodeSide,
	NodeTarget,
	invalid,
	structural_target,
};
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, OccurrenceStateRef},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, EdgeTopology, GraphId},
	storage::{
		graph_pages::{
			HOT_SLOTS_PER_PAGE,
			NodeHotSlot,
			NodeSlotFlags,
			hot_slot_address,
			initialize_node_hot_page,
			read_node_hot_slot,
			write_node_hot_slot,
		},
		local_store::point_occurrence::{SlotState, SlotTransition},
		page::{AlignedPage, PageLsn, PageNo},
		types::PageKey,
	},
	values::storage::DiskAtomRef,
};

impl IncidencePlan<'_, '_> {
	/// Explicit NODE lifecycles and EDGE endpoints share one slot target.
	///
	/// # Errors
	/// Rejects contradictory transitions and resource admission before
	/// insertion.
	pub(super) fn include_nodes(
		&mut self,
		nodes: &[SlotTransition],
		memberships: &[(GraphId, GlobalAtomId)],
		changes: &mut NodeChanges,
	) -> FerrosGrynnResult<()> {
		let roots = (self.before, self.after);
		for node in nodes.iter().filter(|target| structural_target(target, memberships, roots)) {
			if node.kind != AtomKind::Node || node.graph != self.files.graph_id {
				return Err(invalid("NODE lifecycle target belongs to another allocator"));
			}
			let count = changes.len();
			match changes.entry(node.slot) {
				std::collections::btree_map::Entry::Vacant(entry) => {
					self.budget.tree_entry::<(u64, NodeTarget)>(count)?;
					entry.insert(NodeTarget {
						transition: *node,
						explicit: true,
						entries: Vec::new(),
					});
				}
				std::collections::btree_map::Entry::Occupied(mut entry) => {
					if entry.get().transition != *node {
						return Err(invalid(
							"NODE lifecycle and endpoint allocator states disagree",
						));
					}
					entry.get_mut().explicit = true;
				}
			}
		}
		Ok(())
	}

	/// Applies lifecycle while preserving stable implicit endpoint metadata.
	///
	/// # Errors
	/// Rejects source identity, adjacency, admission, I/O and codec failures.
	pub(super) fn patch_nodes(&mut self, changes: NodeChanges) -> FerrosGrynnResult<()> {
		let mut targets = changes.into_iter().peekable();
		while let Some((first, _)) = targets.peek() {
			let number = hot_slot_address(*first)?.0;
			self.reserve_page()?;
			let mut writes = self.hot_writes::<NodeHotSlot>(HOT_SLOTS_PER_PAGE)?;
			let key = PageKey::new(self.files.space(), self.files.nodes, number);
			let mut page = if number.get() < self.header.node_hot_pages {
				self.store.read_update_page(self.state, key)?
			} else {
				let mut page = Box::new(AlignedPage::zeroed());
				initialize_node_hot_page(&mut page, number, PageLsn::ZERO)?;
				page
			};

			while targets.peek().is_some_and(|(slot, _)| {
				hot_slot_address(*slot).is_ok_and(|(page, _)| page == number)
			}) {
				let (address, target) =
					targets.next().ok_or_else(|| invalid("NODE target disappeared"))?;
				let previous = target.transition.before.unwrap_or(SlotState {
					generation: 0,
					occurrence: None,
				});
				let accepted =
					target.transition.after.ok_or_else(|| invalid("NODE allocator shrank"))?;
				let nodes = (
					previous.occurrence.map(|(local, _)| local),
					accepted.occurrence.map(|(local, _)| local),
				);
				let entries = target.entries;
				let (_, within) = hot_slot_address(address)?;
				let source = read_node_hot_slot(&page, number, within)?;
				let source_label = if target.explicit {
					self.source_label(previous)?
				} else {
					None
				};
				if target.explicit {
					self.validate_source_overlay(previous, source.overlay)?;
				}
				validate_source(
					&source,
					previous,
					target.explicit,
					source_label,
					source.overlay,
					number,
				)?;
				let mut slot = match accepted.occurrence {
					None => NodeHotSlot::tombstone(accepted.generation),
					Some((_, global)) if previous.occurrence != accepted.occurrence => {
						NodeHotSlot::isolated(accepted.generation, global)
					}
					Some(_) => source.clone(),
				};
				if target.explicit && accepted.occurrence.is_some() {
					slot.overlay = self.target_overlay(previous, accepted, source.overlay)?;
					slot.flags.set(
						NodeSlotFlags::HAS_OVERLAY,
						!matches!(slot.overlay, crate::storage::atom_page::PropertyRef::None),
					);
					let old_label = if previous.occurrence == accepted.occurrence {
						source_label
					} else {
						None
					};
					let new_label = self.target_label(accepted)?;
					if old_label != new_label {
						slot.replace_label(number, old_label, new_label)?;
					}
				}
				(slot.out_count, slot.out_inline, slot.out_overflow) = self.merge_side(
					nodes,
					NodeSide::Outgoing,
					&entries,
					&source.out_inline,
					source.out_count,
					source.out_overflow,
				)?;
				(slot.in_count, slot.in_inline, slot.in_overflow) = self.merge_side(
					nodes,
					NodeSide::Incoming,
					&entries,
					&source.in_inline,
					source.in_count,
					source.in_overflow,
				)?;
				(slot.undirected_count, slot.undirected_inline, slot.undirected_overflow) = self
					.merge_side(
						nodes,
						NodeSide::Undirected,
						&entries,
						&source.undirected_inline,
						source.undirected_count,
						source.undirected_overflow,
					)?;
				writes.push((within, slot));
			}
			// Each source checksum is consumed before any slot changes that
			// page.
			for (within, slot) in writes {
				write_node_hot_slot(&mut page, within, &slot)?;
			}
			page.seal()?;
			self.pages.push((key, page));
		}
		Ok(())
	}

	/// Removes exact old generations, then orders final neighbors as bulk does.
	fn merge_side<const N: usize>(
		&mut self,
		nodes: (Option<AtomId>, Option<AtomId>),
		side: NodeSide,
		changes: &[AdjacencyChange],
		inline: &[Option<DiskAtomRef>; N],
		count: u32,
		overflow: Option<PageNo>,
	) -> FerrosGrynnResult<(u32, [Option<DiskAtomRef>; N], Option<PageNo>)> {
		if nodes.0 == nodes.1 && !changes.iter().any(|change| change.side == side) {
			return Ok((count, *inline, overflow));
		}
		let source = self.read_side(inline, count, overflow)?;
		let additions =
			changes.iter().filter(|change| change.side == side && change.inserted).count();
		let capacity = source
			.len()
			.checked_add(additions)
			.ok_or_else(|| invalid("NODE neighbor capacity overflows"))?;
		self.budget.retain(
			capacity
				.checked_mul(size_of::<AtomId>())
				.ok_or_else(|| invalid("NODE neighbor bytes overflow"))?,
		)?;
		let mut current = Vec::new();
		current
			.try_reserve_exact(capacity)
			.map_err(|_error| invalid("NODE neighbor allocation failed"))?;
		for reference in source {
			current.push(resolve_edge(self.before, self.files.graph_id, reference)?);
		}
		if let Some(node) = nodes.0 {
			validate_neighbors(self.before, self.files.graph_id, node, side, &current)?;
		}
		for change in changes.iter().filter(|change| change.side == side && !change.inserted) {
			let index = current
				.binary_search(&change.edge)
				.map_err(|_missing| invalid("removed EDGE is absent from source NODE adjacency"))?;
			current.remove(index);
		}
		// A reused slot must never inherit adjacency from the old generation.
		if nodes.0 != nodes.1 && !current.is_empty() {
			return Err(invalid("removed NODE retains adjacency after explicit EDGE removals"));
		}
		for change in changes.iter().filter(|change| change.side == side && change.inserted) {
			current.push(change.edge);
		}
		current.sort_unstable();
		if let Some(node) = nodes.1 {
			validate_neighbors(self.after, self.files.graph_id, node, side, &current)?;
		} else if !current.is_empty() {
			return Err(invalid("freed NODE retains final adjacency"));
		}
		let count =
			u32::try_from(current.len()).map_err(|_error| invalid("NODE degree exceeds u32"))?;
		self.budget.retain(
			current
				.len()
				.checked_mul(size_of::<DiskAtomRef>())
				.ok_or_else(|| invalid("NODE output references overflow"))?,
		)?;
		let mut references = Vec::new();
		references
			.try_reserve_exact(current.len())
			.map_err(|_error| invalid("NODE output reference allocation failed"))?;
		for edge in current {
			references.push(DiskAtomRef::from_parts(AtomKind::Edge, edge.slot())?);
		}
		let head = self.append_side(references.get(N..).unwrap_or(&[]))?;
		Ok((count, std::array::from_fn(|index| references.get(index).copied()), head))
	}
}

/// Checks canonical free/plain sources while retaining implicit endpoint
/// metadata.
fn validate_source(
	slot: &NodeHotSlot,
	state: SlotState,
	explicit: bool,
	label: Option<u32>,
	overlay: crate::storage::atom_page::PropertyRef,
	number: PageNo,
) -> FerrosGrynnResult<()> {
	let Some((local, global)) = state.occurrence else {
		return if *slot == NodeHotSlot::tombstone(state.generation) {
			Ok(())
		} else {
			Err(invalid("free NODE Hot source disagrees with allocator state"))
		};
	};
	if local.generation() != state.generation
		|| slot.generation != state.generation
		|| slot.global_atom != Some(global)
		|| slot.flags.contains(NodeSlotFlags::TOMBSTONE)
	{
		return Err(invalid("NODE adjacency source identity disagrees with replay"));
	}
	if explicit {
		let mut expected = NodeHotSlot::isolated(state.generation, global);
		expected.overlay = overlay;
		expected.flags.set(
			NodeSlotFlags::HAS_OVERLAY,
			!matches!(overlay, crate::storage::atom_page::PropertyRef::None),
		);
		if let Some(label) = label {
			expected.replace_label(number, None, Some(label))?;
		}
		expected.out_count = slot.out_count;
		expected.out_inline = slot.out_inline;
		expected.out_overflow = slot.out_overflow;
		expected.in_count = slot.in_count;
		expected.in_inline = slot.in_inline;
		expected.in_overflow = slot.in_overflow;
		expected.undirected_count = slot.undirected_count;
		expected.undirected_inline = slot.undirected_inline;
		expected.undirected_overflow = slot.undirected_overflow;
		if *slot != expected {
			return Err(invalid("explicit NODE source has nonplain metadata"));
		}
	}
	Ok(())
}

/// A generation-free source reference is meaningful only in its exact old root.
fn resolve_edge(
	root: &DatabaseRoot,
	graph: crate::graph::GraphId,
	reference: DiskAtomRef,
) -> FerrosGrynnResult<AtomId> {
	if reference.kind() != AtomKind::Edge {
		return Err(invalid("NODE adjacency references a non-EDGE atom"));
	}
	let owner = root.graph(graph).ok_or_else(|| invalid("adjacency graph is absent"))?;
	let index = usize::try_from(reference.slot())
		.map_err(|_error| invalid("adjacency EDGE slot exceeds usize"))?;
	let generation = owner
		.slot_generation(AtomKind::Edge, index)
		.ok_or_else(|| invalid("adjacency EDGE slot exceeds allocator"))?;
	let local = AtomId::from_parts(AtomKind::Edge, reference.slot(), generation)?;
	let occurrence = root
		.occurrence(graph, local)
		.ok_or_else(|| invalid("adjacency EDGE occurrence is absent"))?;
	if root.membership(graph, occurrence.global_id()) != Some(local) {
		return Err(invalid("adjacency EDGE membership disagrees"));
	}
	Ok(local)
}

/// Checks full AtomId order and exact per-role multiplicity without a graph
/// scan.
fn validate_neighbors(
	root: &DatabaseRoot,
	graph: crate::graph::GraphId,
	node: AtomId,
	side: NodeSide,
	edges: &[AtomId],
) -> FerrosGrynnResult<()> {
	if edges.windows(2).any(|pair| pair[0] > pair[1]) {
		return Err(invalid("NODE adjacency is not ordered by generation-aware identity"));
	}
	let mut start = 0;
	while start < edges.len() {
		let edge = edges[start];
		let mut end = start + 1;
		while end < edges.len() && edges[end] == edge {
			end += 1;
		}
		let occurrence = root
			.occurrence(graph, edge)
			.ok_or_else(|| invalid("NODE neighbor is absent from accepted root"))?;
		let OccurrenceStateRef::Edge {
			topology,
			..
		} = occurrence.state().view()
		else {
			return Err(invalid("NODE neighbor is not an EDGE"));
		};
		let count = |entries: &[crate::graph::EdgeEndpoint]| {
			entries.iter().filter(|endpoint| endpoint.local_id() == Some(node)).count()
		};
		let expected = match (topology, side) {
			(
				EdgeTopology::Directed {
					source,
					..
				},
				NodeSide::Outgoing,
			) => count(source),
			(
				EdgeTopology::Directed {
					target,
					..
				},
				NodeSide::Incoming,
			) => count(target),
			(
				EdgeTopology::Bidirectional {
					source,
					target,
				},
				NodeSide::Outgoing | NodeSide::Incoming,
			) => count(source) + count(target),
			(
				EdgeTopology::Undirected {
					endpoints,
				},
				NodeSide::Undirected,
			) => count(endpoints),
			_ => 0,
		};
		if expected != end - start {
			return Err(invalid("NODE neighbor multiplicity disagrees with EDGE topology"));
		}
		start = end;
	}
	Ok(())
}
