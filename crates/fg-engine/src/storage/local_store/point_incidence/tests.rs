//! Eligibility cannot accept an incomplete attached global lifecycle journal.

use std::{collections::BTreeSet, sync::Arc};

use crate::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassSchema,
		DatabaseRoot,
		Datastore,
		GlobalAtomId,
		IndexScope,
		PublicationDelta,
	},
	graph::{AtomId, AtomKind, EdgeTopology, GraphKind},
	storage::local_store::point_update::PointUpdatePlan,
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{PropertyMap, RecordId, Type},
};

/// Only the same live identity without an explicit membership is metadata-only.
#[test]
fn mixed_metadata_filter_preserves_structural_and_invalid_transitions() {
	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	use super::super::point_occurrence::{SlotState, SlotTransition};
	let graph = crate::graph::GraphId::new(1).assert_some();
	let local = AtomId::from_parts(AtomKind::Node, 0, 2).assert_ok();
	let global = GlobalAtomId::from_raw(AtomId::from_parts(AtomKind::Node, 7, 3).assert_ok().raw());
	let live = SlotState {
		generation: local.generation(),
		occurrence: Some((local, global)),
	};
	let stable = SlotTransition {
		graph,
		kind: AtomKind::Node,
		slot: local.slot(),
		before: Some(live),
		after: Some(live),
	};
	assert!(!super::structural_target(&stable, &[], (&root, &root)));
	assert!(super::structural_target(&stable, &[(graph, global)], (&root, &root)));
	let free = SlotState {
		generation: 3,
		occurrence: None,
	};
	let next = SlotState {
		generation: 3,
		..live
	};
	for (before, after) in [
		(None, None),
		(None, Some(live)),
		(Some(live), None),
		(Some(free), Some(free)),
		(Some(live), Some(free)),
		(Some(live), Some(next)),
	] {
		assert!(super::structural_target(
			&SlotTransition {
				before,
				after,
				..stable
			},
			&[],
			(&root, &root)
		));
	}
}

/// Builds the replay candidate before backend-specific publication work.
fn transition() -> (Arc<DatabaseRoot>, Arc<DatabaseRoot>, PublicationDelta) {
	transition_with_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok())
}

/// The same replay shape isolates admission from specialized class ownership.
fn transition_with_class(
	class: ClassSchema,
) -> (Arc<DatabaseRoot>, Arc<DatabaseRoot>, PublicationDelta) {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(class).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	seed.commit().assert_ok();
	let before = store.inner.current_root();
	let record = "item:new".parse::<RecordId>().assert_ok();
	// RAM commits rebuild resident geometry even without a spatial index;
	// the local sparse publisher instead borrows the accepted replay root.
	let mut after = before.as_ref().clone();
	let class = before.schema.class("item").assert_some();
	let global = after.insert_atom(&class, record.clone(), PropertyMap::new()).assert_ok();
	let node = after.attach_node(graph.id(), global, None, None).assert_ok();
	after.commit_ts += 1;
	let delta = PublicationDelta {
		records: BTreeSet::from([record]),
		global_slots: BTreeSet::from([global.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::from([(graph.id(), node)]),
		memberships: BTreeSet::from([(graph.id(), global)]),
		point_graph_only: true,
	};
	(before, Arc::new(after), delta)
}

/// A membership key without its Hot target cannot publish a Base insertion.
#[test]
fn global_node_lifecycle_eligibility_requires_membership_and_incidence_closure() {
	let (before, after, mut delta) = transition();
	assert!(Arc::ptr_eq(&before.resident_geometry, &after.resident_geometry));
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	let incidence = std::mem::take(&mut delta.incidence);
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	delta.incidence = incidence;
	let memberships = std::mem::take(&mut delta.memberships);
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	delta.memberships = memberships;
	delta.global_slots.clear();
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
}

/// Both consumed allocators, rather than a missing owner alone, admit a no-op.
#[test]
fn global_node_transient_eligibility_requires_consumed_replay_targets() {
	let (before, live, mut delta) = transition();
	let (graph, global) = *delta.memberships.first().assert_some();
	let mut after = live.as_ref().clone();
	after.detach(graph, global).assert_ok().assert_some();
	after.remove_atom(global).assert_ok();
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	let incidence = std::mem::take(&mut delta.incidence);
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	delta.incidence = incidence;
	delta.global_slots.clear();
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
}

/// Malformed journal identities must fail even when both final slots are free.
#[test]
fn global_node_transient_eligibility_rejects_stale_and_unconsumed_identities() {
	let (_, live, delta) = transition();
	let (graph, first) = *delta.memberships.first().assert_some();
	let mut source = live.as_ref().clone();
	source.detach(graph, first).assert_ok().assert_some();
	source.remove_atom(first).assert_ok();
	let class = source.schema.class("item").assert_some();
	let mut target = source.clone();
	let global = target
		.insert_atom(&class, "item:again".parse::<RecordId>().assert_ok(), PropertyMap::new())
		.assert_ok();
	let node = target.attach_node(graph, global, None, None).assert_ok();
	target.detach(graph, global).assert_ok().assert_some();
	target.remove_atom(global).assert_ok();
	let mut journal = delta;
	journal.memberships = BTreeSet::from([(graph, global)]);
	journal.incidence = BTreeSet::from([(graph, node)]);
	assert!(PointUpdatePlan::eligible(&source, &target, &journal).assert_ok());
	for generation in [0, 2] {
		let identity = AtomId::from_parts(AtomKind::Node, global.slot(), generation).assert_ok();
		journal.memberships = BTreeSet::from([(graph, GlobalAtomId::from_raw(identity.raw()))]);
		assert!(!PointUpdatePlan::eligible(&source, &target, &journal).assert_ok());
		journal.memberships = BTreeSet::from([(graph, global)]);
		journal.incidence = BTreeSet::from([(
			graph,
			AtomId::from_parts(AtomKind::Node, node.slot(), generation).assert_ok(),
		)]);
		assert!(!PointUpdatePlan::eligible(&source, &target, &journal).assert_ok());
		journal.incidence = BTreeSet::from([(graph, node)]);
	}
}

/// A transient without a live class owner conservatively retains
/// specialization.
#[test]
fn global_node_transient_specialized_class_bucket_keeps_full_publisher() {
	use crate::datastore::GeometryIndexOptions;
	let class = ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"shape",
		Type::Option(Box::new(Type::Geometry)),
	)])
	.assert_ok()
	.with_index(
		ClassIndex::new_geometry(
			"shape",
			IndexScope::Base,
			"shape",
			GeometryIndexOptions::default(),
		)
		.assert_ok(),
	)
	.assert_ok();
	let (before, live, delta) = transition_with_class(class);
	let (graph, global) = *delta.memberships.first().assert_some();
	let mut after = live.as_ref().clone();
	after.detach(graph, global).assert_ok().assert_some();
	after.remove_atom(global).assert_ok();
	assert!(super::eligible_membership(&before, &after, &delta, graph, global));
	assert!(!super::eligible_ownerless_metadata(&after, graph));
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
}

/// A live EDGE requires both membership and incidence closure after replay.
#[test]
fn global_edge_lifecycle_eligibility_requires_complete_final_targets() {
	let (before, after, mut delta) = edge_transition();
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	let incidence = std::mem::take(&mut delta.incidence);
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	delta.incidence = incidence;
	let memberships = std::mem::take(&mut delta.memberships);
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	delta.memberships = memberships;
	delta.global_slots.clear();
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
}

/// Both live and transient cases start from the same accepted EDGE replay.
pub(super) fn edge_transition() -> (Arc<DatabaseRoot>, Arc<DatabaseRoot>, PublicationDelta) {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("node", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("edge", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let nodes = ["node:a", "node:b"].map(|text| {
		let record = text.parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record, None, None).assert_ok()
	});
	seed.commit().assert_ok();
	let before = store.inner.current_root();
	let mut after = before.as_ref().clone();
	let class = before.schema.class("edge").assert_some();
	let record = "edge:new".parse::<RecordId>().assert_ok();
	let global = after.insert_atom(&class, record.clone(), PropertyMap::new()).assert_ok();
	let edge = after
		.attach_edge(
			graph.id(),
			global,
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	let delta = PublicationDelta {
		records: BTreeSet::from([record]),
		global_slots: BTreeSet::from([global.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::from([(graph.id(), edge)]),
		memberships: BTreeSet::from([(graph.id(), global)]),
		point_graph_only: true,
	};
	(before, Arc::new(after), delta)
}
