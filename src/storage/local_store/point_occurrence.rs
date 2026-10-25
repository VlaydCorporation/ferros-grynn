//! Final allocator and occurrence states for explicit replay slots.

use super::{layout::invalid, point_namespace::PublicationBudget};
use crate::{
	datastore::{
		DatabaseRoot,
		GlobalAtomId,
		OccurrenceState,
		OccurrenceStateRef,
		PublicationDelta,
	},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphId},
};

/// An allocated slot can be free while retaining its next generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlotState {
	/// The allocator generation is authoritative even without an occurrence.
	pub(super) generation: u32,
	/// Only live occurrences have a generation-aware local and global owner.
	pub(super) occurrence: Option<(AtomId, GlobalAtomId)>,
}

/// Several journal generations can name one final physical slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlotTransition {
	/// Existing graph whose allocator owns both states.
	pub(super) graph: GraphId,
	/// Node and edge slots have separate allocators and Hot files.
	pub(super) kind: AtomKind,
	/// Generation-independent address within that allocator.
	pub(super) slot: u64,
	/// None means the source allocator had not allocated this slot.
	pub(super) before: Option<SlotState>,
	/// A freed final slot still has Some state with its next generation.
	pub(super) after: Option<SlotState>,
}

/// A replay identity must lie within the generations consumed by this commit.
pub(super) fn generation_consumed(before: Option<u32>, after: Option<u32>, used: u32) -> bool {
	before.is_none_or(|generation| generation <= used)
		&& after.is_some_and(|generation| used < generation)
}

/// Structural slots have no child of their own; endpoint scopes are validated
/// by their renderer.
pub(super) fn incidence_state(state: &OccurrenceState) -> bool {
	match state.view() {
		OccurrenceStateRef::Node {
			nested_graph: None,
		} => true,
		OccurrenceStateRef::Edge {
			topology,
			transition_graph: None,
			..
		} => incidence_topology(topology),
		_ => false,
	}
}

/// Keeps endpoint handle admission independent from an occurrence's own child.
fn incidence_topology(topology: &crate::graph::EdgeTopology) -> bool {
	topology.endpoints().all(|endpoint| match endpoint {
		crate::graph::EdgeEndpoint::Local(id) => id.kind().is_some(),
		crate::graph::EdgeEndpoint::Nested(qualified) => {
			qualified.atom().kind().is_some()
				&& qualified.scope().iter().all(|owner| owner.kind().is_some())
		}
	})
}

/// Recognizes topology changes of the same EDGE and unchanged child binding.
pub(super) fn incidence_rewire(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	graph: GraphId,
	local: AtomId,
) -> bool {
	if local.kind() != Some(AtomKind::Edge) {
		return false;
	}
	let Some((old, new)) = before.occurrence(graph, local).zip(after.occurrence(graph, local))
	else {
		return false;
	};
	let global = old.global_id();
	if new.global_id() != global
		|| before.membership(graph, global) != Some(local)
		|| after.membership(graph, global) != Some(local)
		|| !before
			.atom(global)
			.zip(after.atom(global))
			.is_some_and(|(old, new)| old.class_id() == new.class_id())
	{
		return false;
	}
	match (old.state().view(), new.state().view()) {
		(
			OccurrenceStateRef::Edge {
				topology: old,
				transition_graph: old_child,
				..
			},
			OccurrenceStateRef::Edge {
				topology: new,
				transition_graph: new_child,
				..
			},
		) => {
			old_child == new_child
				&& old != new
				&& incidence_topology(old)
				&& incidence_topology(new)
		}
		_ => false,
	}
}

/// Leaves stable metadata to its patcher without changing shared index targets.
pub(super) fn structural_target(
	target: &SlotTransition,
	memberships: &[(GraphId, GlobalAtomId)],
	roots: (&DatabaseRoot, &DatabaseRoot),
) -> bool {
	let Some((old, new)) = target.before.zip(target.after) else {
		return true;
	};
	let Some((local, global)) = old.occurrence else {
		return true;
	};
	old != new
		|| memberships.binary_search(&(target.graph, global)).is_ok()
		|| incidence_rewire(roots.0, roots.1, target.graph, local)
}

/// Resolves only explicit replay slots, including transient allocations.
///
/// # Errors
/// Rejects missing graphs, invalid addresses, inconsistent owners and resource
/// admission before allocating the result.
pub(super) fn incidence_slots(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<Vec<SlotTransition>> {
	budget.retain(
		delta
			.incidence
			.len()
			.checked_mul(size_of::<SlotTransition>())
			.ok_or_else(|| invalid("point incidence targets overflow usize"))?,
	)?;
	let mut targets = Vec::new();
	targets
		.try_reserve_exact(delta.incidence.len())
		.map_err(|_error| invalid("point incidence target allocation failed"))?;
	for &(graph, local) in &delta.incidence {
		let kind = local.kind().ok_or_else(|| invalid("point incidence kind is null"))?;
		targets.push(SlotTransition {
			graph,
			kind,
			slot: local.slot(),
			before: slot_state(before, graph, kind, local.slot())?,
			after: slot_state(after, graph, kind, local.slot())?,
		});
	}
	targets.sort_unstable_by_key(|target| (target.graph, target.kind, target.slot));
	targets.dedup_by_key(|target| (target.graph, target.kind, target.slot));
	Ok(targets)
}

/// Looks up the allocator generation without scanning its capacity.
pub(super) fn slot_state(
	root: &DatabaseRoot,
	graph: GraphId,
	kind: AtomKind,
	slot: u64,
) -> FerrosGrynnResult<Option<SlotState>> {
	let owner = root.graph(graph).ok_or_else(|| invalid("point incidence graph is absent"))?;
	let index =
		usize::try_from(slot).map_err(|_error| invalid("point incidence slot exceeds usize"))?;
	let Some(generation) = owner.slot_generation(kind, index) else {
		return Ok(None);
	};
	let local = AtomId::from_parts(kind, slot, generation)?;
	let occurrence = owner.occurrence(local).map(|occurrence| (local, occurrence.global_id()));
	if let Some((local, global)) = occurrence
		&& (global.kind() != Some(kind) || root.membership(graph, global) != Some(local))
	{
		return Err(invalid("point incidence allocator and membership disagree"));
	}
	Ok(Some(SlotState {
		generation,
		occurrence,
	}))
}

#[cfg(test)]
/// Allocator transitions remain observable without a final live occurrence.
mod tests {
	use std::{collections::BTreeSet, sync::Arc};

	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::GraphKind,
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyMap, RecordId},
	};

	/// Missing, stale and unconsumed generations cannot prove a transient slot.
	#[test]
	fn transient_generation_requires_both_replay_bounds() {
		for (before, after, used, expected) in [
			(None, Some(1), 0, true),
			(Some(2), Some(4), 2, true),
			(Some(2), Some(4), 3, true),
			(Some(2), Some(4), 1, false),
			(Some(2), Some(4), 4, false),
			(None, None, 0, false),
			(None, Some(0), 0, false),
			(Some(2), Some(1), 2, false),
		] {
			assert_eq!(generation_consumed(before, after, used), expected);
		}
	}

	/// Leaves a second Base node available for reuse or transient attachment.
	fn fixture() -> (Arc<DatabaseRoot>, GraphId, [GlobalAtomId; 2], AtomId) {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("g", GraphKind::Graph).assert_ok().id();
		let records = ["item:a", "item:b"].map(|record| record.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let local = seed.attach_node(graph, &records[0], None, None).assert_ok();
		seed.commit().assert_ok();
		let root = store.inner.current_root();
		let globals = records.map(|record| root.resolve_atom(&record).assert_some());
		(root, graph, globals, local)
	}

	/// Keeps the physical-target fixture independent of transaction internals.
	fn delta(incidence: BTreeSet<(GraphId, AtomId)>) -> PublicationDelta {
		PublicationDelta {
			records: BTreeSet::new(),
			global_slots: BTreeSet::new(),
			overlays: BTreeSet::new(),
			incidence,
			memberships: BTreeSet::new(),
			point_graph_only: false,
		}
	}

	/// Both journal generations resolve to one physical replacement slot.
	#[test]
	fn occurrence_targets_combine_reused_generations_and_owners() {
		let (before, graph, globals, old) = fixture();
		let mut after = before.as_ref().clone();
		after.detach(graph, globals[0]).assert_ok().assert_some();
		let new = after.attach_node(graph, globals[1], None, None).assert_ok();
		assert_eq!(old.slot(), new.slot());
		assert_eq!(new.generation(), 1);
		let delta = delta(BTreeSet::from([(graph, old), (graph, new)]));
		let targets =
			incidence_slots(&before, &after, &delta, &mut PublicationBudget::new()).assert_ok();
		assert_eq!(targets.len(), 1);
		assert_eq!(targets[0].slot, 0);
		assert_eq!(targets[0].before.assert_some().occurrence, Some((old, globals[0])));
		assert_eq!(targets[0].after.assert_some().occurrence, Some((new, globals[1])));
	}

	/// A transient attachment must not vanish from the physical publication.
	#[test]
	fn occurrence_targets_retain_transient_allocator_generation() {
		let (before, graph, globals, _) = fixture();
		let mut after = before.as_ref().clone();
		let local = after.attach_node(graph, globals[1], None, None).assert_ok();
		after.detach(graph, globals[1]).assert_ok().assert_some();
		let delta = delta(BTreeSet::from([(graph, local)]));
		let targets =
			incidence_slots(&before, &after, &delta, &mut PublicationBudget::new()).assert_ok();
		assert_eq!(targets.len(), 1);
		assert_eq!(targets[0].slot, 1);
		assert_eq!(targets[0].before, None);
		assert_eq!(
			targets[0].after,
			Some(SlotState {
				generation: 1,
				occurrence: None,
			})
		);
	}
	/// A topology rewire never grants admission to changing a container
	/// binding.
	#[test]
	fn incidence_rewire_requires_same_owner_and_child_binding() {
		use crate::graph::EdgeTopology;
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("root", GraphKind::Metagraph).assert_ok().id();
		let children = ["first", "second"]
			.map(|name| seed.define_graph(name, GraphKind::Graph).assert_ok().id());
		let records =
			["item:a", "item:b", "link:owner"].map(|name| name.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let nodes =
			[0, 1].map(|index| seed.attach_node(graph, &records[index], None, None).assert_ok());
		let old = EdgeTopology::directed([nodes[0]], [nodes[1]]);
		let new = EdgeTopology::directed([nodes[1]], [nodes[0]]);
		let edge = seed
			.attach_edge(graph, &records[2], None, old.clone(), None, Some(children[0]))
			.assert_ok();
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		for child in [Some(children[0]), None, Some(children[1])] {
			let mut after = before.as_ref().clone();
			after
				.replace_occurrence_state(
					graph,
					edge,
					OccurrenceState::edge(new.clone(), None, child),
				)
				.assert_ok();
			assert_eq!(incidence_rewire(&before, &after, graph, edge), child == Some(children[0]));
			if child.is_some() {
				assert!(!incidence_state(after.occurrence(graph, edge).assert_some().state()));
			}
			let stale =
				AtomId::from_parts(AtomKind::Edge, edge.slot(), edge.generation() + 1).assert_ok();
			assert!(!incidence_rewire(&before, &after, graph, stale));
		}
		let mut plain = before.as_ref().clone();
		plain
			.replace_occurrence_state(graph, edge, OccurrenceState::edge(old.clone(), None, None))
			.assert_ok();
		let mut after = plain.clone();
		after
			.replace_occurrence_state(
				graph,
				edge,
				OccurrenceState::edge(new.clone(), None, Some(children[0])),
			)
			.assert_ok();
		assert!(!incidence_rewire(&plain, &after, graph, edge));
		after
			.replace_occurrence_state(graph, edge, OccurrenceState::edge(new, None, None))
			.assert_ok();
		assert!(incidence_rewire(&plain, &after, graph, edge));
		let mut weight = before.as_ref().clone();
		weight
			.replace_occurrence_state(
				graph,
				edge,
				OccurrenceState::edge(old, Some(7_i64.into()), Some(children[0])),
			)
			.assert_ok();
		assert!(!incidence_rewire(&before, &weight, graph, edge));
	}
}
