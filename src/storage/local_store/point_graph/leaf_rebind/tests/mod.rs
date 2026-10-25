//! Closed-journal fixture cases share one family parameter without changing
//! their assertions.

use std::collections::BTreeSet;

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore, OccurrenceState},
	graph::{EdgeTopology, QualifiedAtomId},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{PropertyMap, RecordId},
};

/// A parent, two leaf candidates and a foreign graph share Base identities.
#[derive(Clone)]
struct Fixture {
	/// Logical source is owned independently of the seed datastore.
	before: DatabaseRoot,
	/// Selected root has no ancestors in the default case.
	parent: GraphId,
	/// Old/replacement leaves and three possible ancestors are public
	/// metagraphs.
	graphs: [GraphId; 5],
	/// NODE owner, spare NODE and EDGE connector can occur in different
	/// roots.
	globals: [GlobalAtomId; 3],
	/// Full selected NODE handle deliberately collides with foreign slot
	/// zero.
	owner: AtomId,
}

/// Real root operations establish the same schema and membership
/// invariants.
fn fixture(family: GraphKind) -> Fixture {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let parent = seed.define_graph("root", family).assert_ok().id();
	let graphs = ["old", "replacement", "foreign", "above", "highest"]
		.map(|name| seed.define_graph(name, GraphKind::Metagraph).assert_ok().id());
	let records = ["item:owner", "item:spare", "link:connector"]
		.map(|name| name.parse::<RecordId>().assert_ok());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let owner = seed.attach_node(parent, &records[0], None, Some(graphs[0])).assert_ok();
	seed.commit().assert_ok();
	let before = store.inner.current_root();
	let globals = records.map(|record| before.resolve_atom(&record).assert_some());
	Fixture {
		before: before.as_ref().clone(),
		parent,
		graphs,
		globals,
		owner,
	}
}

/// The only journal key is a generation-aware incidence target.
fn delta(fixture: &Fixture) -> PublicationDelta {
	PublicationDelta {
		records: BTreeSet::new(),
		global_slots: BTreeSet::new(),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::from([(fixture.parent, fixture.owner)]),
		memberships: BTreeSet::new(),
		point_graph_only: true,
	}
}

/// Replaces state through the real private root API, preserving other
/// owners.
fn rebound(fixture: &Fixture) -> DatabaseRoot {
	let mut after = fixture.before.clone();
	after
		.replace_occurrence_state(
			fixture.parent,
			fixture.owner,
			OccurrenceState::node(Some(fixture.graphs[1])),
		)
		.assert_ok();
	after
}

/// An exclusive child delta cannot silently absorb unrelated journal
/// targets.
#[test]
fn node_leaf_rebind_requires_closed_delta_and_full_owner_generation() {
	check_node_leaf_rebind_requires_closed_delta_and_full_owner_generation(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_node_leaf_rebind_requires_closed_delta_and_full_owner_generation(family: GraphKind) {
	let fixture = fixture(family);
	let after = rebound(&fixture);
	let selected = candidate(&fixture.before, &after, &delta(&fixture)).assert_some();
	assert_eq!(
		(selected.graph, selected.local, selected.global),
		(fixture.parent, fixture.owner, fixture.globals[0])
	);
	assert_eq!(
		(selected.before_child, selected.after_child),
		(Some(fixture.graphs[0]), Some(fixture.graphs[1]))
	);
	for case in 0..6 {
		let mut delta = delta(&fixture);
		match case {
			0 => {
				delta.point_graph_only = false;
			}
			1 => {
				delta.records.insert("item:owner".parse().assert_ok());
			}
			2 => {
				delta.global_slots.insert(fixture.globals[0].slot());
			}
			3 => {
				delta.overlays.insert((fixture.parent, fixture.globals[0]));
			}
			4 => {
				delta.memberships.insert((fixture.parent, fixture.globals[0]));
			}
			_ => {
				delta.incidence = BTreeSet::from([(
					fixture.parent,
					AtomId::from_parts(
						AtomKind::Node,
						fixture.owner.slot(),
						fixture.owner.generation() + 1,
					)
					.assert_ok(),
				)]);
			}
		}
		assert!(candidate(&fixture.before, &after, &delta).is_none(), "case {case}");
	}
}

/// Shared direct ancestors are admitted only when dense scope numbers stay.
#[test]
fn leaf_rebind_accepts_shared_direct_ancestors_only_for_present_replacement() {
	check_leaf_rebind_accepts_shared_direct_ancestors_only_for_present_replacement(
		GraphKind::Metagraph,
	);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_leaf_rebind_accepts_shared_direct_ancestors_only_for_present_replacement(
	family: GraphKind,
) {
	let mut fixture = fixture(family);
	for graph in [fixture.graphs[2], fixture.graphs[3]] {
		let node = fixture
			.before
			.attach_node(graph, fixture.globals[0], None, Some(fixture.parent))
			.assert_ok();
		fixture
			.before
			.attach_edge(
				graph,
				fixture.globals[2],
				None,
				EdgeTopology::directed([node], [node]),
				None,
				Some(fixture.parent),
			)
			.assert_ok();
	}
	assert!(candidate(&fixture.before, &rebound(&fixture), &delta(&fixture)).is_some());
	assert!(remap_candidate(&fixture.before, &rebound(&fixture), &delta(&fixture)).is_none());
	let mut absent = fixture.before.clone();
	absent
		.replace_occurrence_state(fixture.parent, fixture.owner, OccurrenceState::node(None))
		.assert_ok();
	for (before, after) in [(&fixture.before, &absent), (&absent, &fixture.before)] {
		assert!(candidate(before, after, &delta(&fixture)).is_none());
		assert!(remap_candidate(before, after, &delta(&fixture)).is_none());
	}
}

/// Paths beyond two ancestor levels and unjournaled changes keep fallback.
#[test]
fn leaf_rebind_rejects_deeper_ancestors_and_other_graph_mutations() {
	check_leaf_rebind_rejects_deeper_ancestors_and_other_graph_mutations(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_leaf_rebind_rejects_deeper_ancestors_and_other_graph_mutations(family: GraphKind) {
	let mut fixture = fixture(family);
	fixture
		.before
		.attach_node(fixture.graphs[2], fixture.globals[0], None, Some(fixture.parent))
		.assert_ok();
	let mut unrelated = rebound(&fixture);
	unrelated.attach_node(fixture.graphs[3], fixture.globals[1], None, None).assert_ok();
	assert!(candidate(&fixture.before, &unrelated, &delta(&fixture)).is_none());
	fixture
		.before
		.attach_node(fixture.graphs[3], fixture.globals[0], None, Some(fixture.graphs[2]))
		.assert_ok();
	assert!(candidate(&fixture.before, &rebound(&fixture), &delta(&fixture)).is_some());
	assert!(remap_candidate(&fixture.before, &rebound(&fixture), &delta(&fixture)).is_none());
	for diamond in [false, true] {
		let mut deeper = fixture.clone();
		deeper
			.before
			.attach_node(deeper.graphs[4], deeper.globals[0], None, Some(deeper.graphs[3]))
			.assert_ok();
		if diamond {
			// A shorter route must not hide the same owner's three-hop
			// route.
			deeper
				.before
				.attach_node(deeper.graphs[3], deeper.globals[1], None, Some(deeper.parent))
				.assert_ok();
		}
		assert!(candidate(&deeper.before, &rebound(&deeper), &delta(&deeper)).is_none());
		assert!(remap_candidate(&deeper.before, &rebound(&deeper), &delta(&deeper)).is_none());
	}
}

/// Lifecycle and ancestor cases retain the complete-generation publisher.
#[test]
fn node_leaf_rebind_rejects_attach_detach_descendants_and_ancestors() {
	check_node_leaf_rebind_rejects_attach_detach_descendants_and_ancestors(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_node_leaf_rebind_rejects_attach_detach_descendants_and_ancestors(family: GraphKind) {
	let mut fixture = fixture(family);
	// A surviving direct container would shift dense scopes on removal.
	fixture
		.before
		.attach_node(fixture.parent, fixture.globals[1], None, Some(fixture.graphs[2]))
		.assert_ok();
	let mut detached = fixture.before.clone();
	detached
		.replace_occurrence_state(fixture.parent, fixture.owner, OccurrenceState::node(None))
		.assert_ok();
	assert!(candidate(&fixture.before, &detached, &delta(&fixture)).is_none());
	assert!(candidate(&detached, &rebound(&fixture), &delta(&fixture)).is_none());
	for child in [fixture.graphs[0], fixture.graphs[1], fixture.graphs[2]] {
		let mut nested = fixture.clone();
		let target = if child == nested.graphs[2] {
			nested.parent
		} else {
			nested.graphs[2]
		};
		nested.before.attach_node(child, nested.globals[1], None, Some(target)).assert_ok();
		assert!(candidate(&nested.before, &rebound(&nested), &delta(&nested)).is_none());
	}
}

/// Surviving children use the remap plan while the fast-path boundary
/// stays.
#[test]
fn child_presence_with_survivors_selects_remap_without_widening_fast_path() {
	check_child_presence_with_survivors_selects_remap_without_widening_fast_path(
		GraphKind::Metagraph,
	);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_child_presence_with_survivors_selects_remap_without_widening_fast_path(family: GraphKind) {
	let mut fixture = fixture(family);
	fixture
		.before
		.attach_node(fixture.parent, fixture.globals[1], None, Some(fixture.graphs[2]))
		.assert_ok();
	let mut detached = fixture.before.clone();
	detached
		.replace_occurrence_state(fixture.parent, fixture.owner, OccurrenceState::node(None))
		.assert_ok();
	for (before, after) in [(&fixture.before, &detached), (&detached, &fixture.before)] {
		assert!(candidate(before, after, &delta(&fixture)).is_none());
		let remap = remap_candidate(before, after, &delta(&fixture)).assert_some();
		assert_eq!(
			(remap.graph, remap.local, remap.global),
			(fixture.parent, fixture.owner, fixture.globals[0])
		);
	}
}

/// Qualified NODE roles follow a surviving scope in both directions.
#[test]
fn child_presence_remap_accepts_qualified_node_roles_through_survivors() {
	check_child_presence_remap_accepts_qualified_node_roles_through_survivors(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_child_presence_remap_accepts_qualified_node_roles_through_survivors(family: GraphKind) {
	let mut fixture = fixture(family);
	let survivor = fixture
		.before
		.attach_node(fixture.parent, fixture.globals[1], None, Some(fixture.graphs[2]))
		.assert_ok();
	let terminal =
		fixture.before.attach_node(fixture.graphs[2], fixture.globals[0], None, None).assert_ok();
	let route = QualifiedAtomId::new([survivor], terminal).assert_ok();
	fixture
		.before
		.attach_edge(
			fixture.parent,
			fixture.globals[2],
			None,
			EdgeTopology::directed([route], [fixture.owner]),
			None,
			None,
		)
		.assert_ok();
	let mut detached = fixture.before.clone();
	detached
		.replace_occurrence_state(fixture.parent, fixture.owner, OccurrenceState::node(None))
		.assert_ok();
	for (before, after) in [(&fixture.before, &detached), (&detached, &fixture.before)] {
		assert!(candidate_core(before, after, &delta(&fixture)).is_some());
		assert!(candidate(before, after, &delta(&fixture)).is_none());
		assert!(remap_candidate(before, after, &delta(&fixture)).is_some());
	}
}

/// Presence changes preserve dense scope numbering only without survivors.
#[test]
fn leaf_child_presence_accepts_node_and_edge_without_surviving_children() {
	check_leaf_child_presence_accepts_node_and_edge_without_surviving_children(
		GraphKind::Metagraph,
	);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_leaf_child_presence_accepts_node_and_edge_without_surviving_children(family: GraphKind) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		let mut fixture = fixture(family);
		let node = fixture.owner;
		let absent = if kind == AtomKind::Node {
			OccurrenceState::node(None)
		} else {
			fixture
				.before
				.replace_occurrence_state(fixture.parent, node, OccurrenceState::node(None))
				.assert_ok();
			let roles = EdgeTopology::directed([node], [node]);
			fixture.owner = fixture
				.before
				.attach_edge(
					fixture.parent,
					fixture.globals[2],
					None,
					roles.clone(),
					Some(7_i64.into()),
					Some(fixture.graphs[0]),
				)
				.assert_ok();
			OccurrenceState::edge(roles, Some(7_i64.into()), None)
		};
		let mut detached = fixture.before.clone();
		detached.replace_occurrence_state(fixture.parent, fixture.owner, absent).assert_ok();
		let remove = candidate(&fixture.before, &detached, &delta(&fixture)).assert_some();
		assert_eq!((remove.before_child, remove.after_child), (Some(fixture.graphs[0]), None));
		let attach = candidate(&detached, &fixture.before, &delta(&fixture)).assert_some();
		assert_eq!((attach.before_child, attach.after_child), (None, Some(fixture.graphs[0])));
	}
}

/// Present leaves retain the same scope; selected-owner presence stays
/// closed.
#[test]
fn node_leaf_rebind_preserves_scope_with_selected_owner_routes() {
	check_node_leaf_rebind_preserves_scope_with_selected_owner_routes(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_node_leaf_rebind_preserves_scope_with_selected_owner_routes(family: GraphKind) {
	let mut fixture = fixture(family);
	let terminal =
		fixture.before.attach_node(fixture.graphs[0], fixture.globals[1], None, None).assert_ok();
	fixture.before.attach_node(fixture.graphs[1], fixture.globals[1], None, None).assert_ok();
	let route = QualifiedAtomId::new([fixture.owner], terminal).assert_ok();
	fixture
		.before
		.attach_edge(
			fixture.parent,
			fixture.globals[2],
			None,
			EdgeTopology::directed([route], [fixture.owner]),
			None,
			None,
		)
		.assert_ok();
	let after = rebound(&fixture);
	for (before, after) in [(&fixture.before, &after), (&after, &fixture.before)] {
		assert!(candidate(before, after, &delta(&fixture)).is_some());
	}
	let mut detached = fixture.before.clone();
	detached
		.replace_occurrence_state(fixture.parent, fixture.owner, OccurrenceState::node(None))
		.assert_ok();
	for (before, after) in [(&fixture.before, &detached), (&detached, &fixture.before)] {
		assert!(candidate(before, after, &delta(&fixture)).is_none());
		assert!(remap_candidate(before, after, &delta(&fixture)).is_none());
	}
}

/// Equal AtomId bits in a foreign materialization do not refer to this
/// owner.
#[test]
fn node_leaf_rebind_keeps_foreign_qualified_handle_collisions_independent() {
	check_node_leaf_rebind_keeps_foreign_qualified_handle_collisions_independent(
		GraphKind::Metagraph,
	);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_node_leaf_rebind_keeps_foreign_qualified_handle_collisions_independent(family: GraphKind) {
	let mut fixture = fixture(family);
	let terminal =
		fixture.before.attach_node(fixture.graphs[0], fixture.globals[1], None, None).assert_ok();
	let foreign = fixture
		.before
		.attach_node(fixture.graphs[2], fixture.globals[0], None, Some(fixture.graphs[0]))
		.assert_ok();
	assert_eq!(foreign, fixture.owner);
	let route = QualifiedAtomId::new([foreign], terminal).assert_ok();
	fixture
		.before
		.attach_edge(
			fixture.graphs[2],
			fixture.globals[2],
			None,
			EdgeTopology::directed([route], [foreign]),
			None,
			None,
		)
		.assert_ok();
	assert!(candidate(&fixture.before, &rebound(&fixture), &delta(&fixture)).is_some());
}

/// A child-only EDGE preserves both ordered roles and its exact weight.
#[test]
fn edge_leaf_rebind_requires_unchanged_roles_and_weight() {
	check_edge_leaf_rebind_requires_unchanged_roles_and_weight(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_edge_leaf_rebind_requires_unchanged_roles_and_weight(family: GraphKind) {
	let mut fixture = fixture(family);
	let node = fixture.owner;
	let roles = EdgeTopology::directed([node], [node]);
	fixture.owner = fixture
		.before
		.attach_edge(
			fixture.parent,
			fixture.globals[2],
			None,
			roles.clone(),
			Some(7_i64.into()),
			Some(fixture.graphs[0]),
		)
		.assert_ok();
	let mut after = fixture.before.clone();
	after
		.replace_occurrence_state(
			fixture.parent,
			fixture.owner,
			OccurrenceState::edge(roles.clone(), Some(7_i64.into()), Some(fixture.graphs[1])),
		)
		.assert_ok();
	let selected = candidate(&fixture.before, &after, &delta(&fixture)).assert_some();
	assert_eq!((selected.local, selected.global), (fixture.owner, fixture.globals[2]));
	for state in [
		OccurrenceState::edge(roles.clone(), Some(8_i64.into()), Some(fixture.graphs[1])),
		OccurrenceState::edge(
			EdgeTopology::undirected([node]),
			Some(7_i64.into()),
			Some(fixture.graphs[1]),
		),
		OccurrenceState::edge(
			EdgeTopology::directed([node, node], [node]),
			Some(7_i64.into()),
			Some(fixture.graphs[1]),
		),
		OccurrenceState::edge(roles.clone(), Some(7_i64.into()), None),
		OccurrenceState::edge(roles, Some(7_i64.into()), Some(fixture.graphs[0])),
	] {
		let mut changed = fixture.before.clone();
		changed.replace_occurrence_state(fixture.parent, fixture.owner, state).assert_ok();
		assert!(candidate(&fixture.before, &changed, &delta(&fixture)).is_none());
	}
	let mut stale = delta(&fixture);
	stale.incidence = BTreeSet::from([(
		fixture.parent,
		AtomId::from_parts(AtomKind::Edge, fixture.owner.slot(), fixture.owner.generation() + 1)
			.assert_ok(),
	)]);
	assert!(candidate(&fixture.before, &after, &stale).is_none());
}

/// Stable leaf routes may select either container; presence stays closed.
#[test]
fn edge_leaf_rebind_preserves_scope_with_qualified_container_routes() {
	check_edge_leaf_rebind_preserves_scope_with_qualified_container_routes(GraphKind::Metagraph);
}

/// Runs the same closed-journal assertions for the selected parent family.
fn check_edge_leaf_rebind_preserves_scope_with_qualified_container_routes(family: GraphKind) {
	let mut fixture = fixture(family);
	let node = fixture.owner;
	let terminal =
		fixture.before.attach_node(fixture.graphs[0], fixture.globals[1], None, None).assert_ok();
	fixture.before.attach_node(fixture.graphs[1], fixture.globals[1], None, None).assert_ok();
	let route = QualifiedAtomId::new([node], terminal).assert_ok();
	fixture.owner = fixture
		.before
		.attach_edge(
			fixture.parent,
			fixture.globals[2],
			None,
			EdgeTopology::directed([route], [node]),
			None,
			Some(fixture.graphs[0]),
		)
		.assert_ok();
	for selected_route in [false, true] {
		let owner = if selected_route {
			fixture.owner
		} else {
			node
		};
		let route = QualifiedAtomId::new([owner], terminal).assert_ok();
		let roles = EdgeTopology::directed([route], [node]);
		fixture
			.before
			.replace_occurrence_state(
				fixture.parent,
				fixture.owner,
				OccurrenceState::edge(roles.clone(), None, Some(fixture.graphs[0])),
			)
			.assert_ok();
		let mut after = fixture.before.clone();
		after
			.replace_occurrence_state(
				fixture.parent,
				fixture.owner,
				OccurrenceState::edge(roles.clone(), None, Some(fixture.graphs[1])),
			)
			.assert_ok();
		assert!(candidate(&fixture.before, &after, &delta(&fixture)).is_some());
		if selected_route {
			let mut detached = fixture.before.clone();
			detached
				.replace_occurrence_state(
					fixture.parent,
					fixture.owner,
					OccurrenceState::edge(roles, None, None),
				)
				.assert_ok();
			for (before, after) in [(&fixture.before, &detached), (&detached, &fixture.before)] {
				assert!(candidate(before, after, &delta(&fixture)).is_none());
				assert!(remap_candidate(before, after, &delta(&fixture)).is_none());
			}
		}
	}
}

/// Graph owners reuse the same source and publication contracts.
mod graph_owner_tests;

/// Detached Base writes require an exact journal alongside the leaf owner.
mod mixed_base_tests;
