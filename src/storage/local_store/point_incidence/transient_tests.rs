//! Ownerless NODE and EDGE journals require separate consumed local witnesses.

use std::{collections::BTreeSet, sync::Arc};

use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, PublicationDelta},
	graph::{AtomId, AtomKind, EdgeTopology},
	storage::local_store::point_update::PointUpdatePlan,
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{PropertyMap, RecordId},
};

/// Missing targets cannot publish either consumed allocator authority.
#[test]
fn global_edge_transient_eligibility_requires_consumed_replay_targets() {
	let (before, live, mut delta) = super::tests::edge_transition();
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

/// Old and next generations are not evidence for this transient replay.
#[test]
fn global_edge_transient_eligibility_rejects_stale_and_unconsumed_identities() {
	let (_, live, mut delta) = super::tests::edge_transition();
	let (graph, first) = *delta.memberships.first().assert_some();
	let mut before = live.as_ref().clone();
	before.detach(graph, first).assert_ok().assert_some();
	before.remove_atom(first).assert_ok();
	let mut after = before.clone();
	let record = "edge:again".parse::<RecordId>().assert_ok();
	let class = before.schema.class("edge").assert_some();
	let global = after.insert_atom(&class, record.clone(), PropertyMap::new()).assert_ok();
	let endpoints = [0, 1].map(|slot| AtomId::from_parts(AtomKind::Node, slot, 0).assert_ok());
	let edge = after
		.attach_edge(
			graph,
			global,
			None,
			EdgeTopology::directed([endpoints[0]], [endpoints[1]]),
			None,
			None,
		)
		.assert_ok();
	after.detach(graph, global).assert_ok().assert_some();
	after.remove_atom(global).assert_ok();
	delta.records = BTreeSet::from([record]);
	delta.global_slots = BTreeSet::from([global.slot()]);
	delta.memberships = BTreeSet::from([(graph, global)]);
	delta.incidence = BTreeSet::from([(graph, edge)]);
	assert_eq!((global.generation(), edge.generation()), (1, 1));
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	for generation in [0, 2] {
		let fake = AtomId::from_parts(AtomKind::Edge, global.slot(), generation).assert_ok();
		delta.memberships = BTreeSet::from([(graph, GlobalAtomId::from_raw(fake.raw()))]);
		assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
		delta.memberships = BTreeSet::from([(graph, global)]);
		delta.incidence = BTreeSet::from([(
			graph,
			AtomId::from_parts(AtomKind::Edge, edge.slot(), generation).assert_ok(),
		)]);
		assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
		delta.incidence = BTreeSet::from([(graph, edge)]);
	}
}

/// An earlier NODE witness cannot hide a missing or wrong-kind EDGE target.
#[test]
fn global_edge_transient_mixed_kinds_require_separate_incidence_witnesses() {
	let (before, after, mut delta) = mixed_transition();
	assert!(PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	let edge = delta
		.incidence
		.iter()
		.find(|(_, local)| local.kind() == Some(AtomKind::Edge))
		.copied()
		.assert_some();
	delta.incidence.remove(&edge);
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
	delta.incidence.insert((
		edge.0,
		AtomId::from_parts(AtomKind::Node, edge.1.slot(), edge.1.generation()).assert_ok(),
	));
	assert!(!PointUpdatePlan::eligible(&before, &after, &delta).assert_ok());
}

/// NODE sorts before EDGE here, and the EDGE owns a transient NODE endpoint.
fn mixed_transition() -> (Arc<DatabaseRoot>, Arc<DatabaseRoot>, PublicationDelta) {
	let (before, _, delta) = super::tests::edge_transition();
	let graph = delta.memberships.first().assert_some().0;
	let mut after = before.as_ref().clone();
	let node_record = "node:gone".parse::<RecordId>().assert_ok();
	let edge_record = "edge:gone".parse::<RecordId>().assert_ok();
	let node_class = before.schema.class("node").assert_some();
	let edge_class = before.schema.class("edge").assert_some();
	let global_node =
		after.insert_atom(&node_class, node_record.clone(), PropertyMap::new()).assert_ok();
	let node = after.attach_node(graph, global_node, None, None).assert_ok();
	let global_edge =
		after.insert_atom(&edge_class, edge_record.clone(), PropertyMap::new()).assert_ok();
	let endpoint = AtomId::from_parts(AtomKind::Node, 1, 0).assert_ok();
	let edge = after
		.attach_edge(
			graph,
			global_edge,
			None,
			EdgeTopology::directed([node], [endpoint]),
			None,
			None,
		)
		.assert_ok();
	after.detach(graph, global_edge).assert_ok().assert_some();
	after.detach(graph, global_node).assert_ok().assert_some();
	after.remove_atom(global_edge).assert_ok();
	after.remove_atom(global_node).assert_ok();
	let delta = PublicationDelta {
		records: BTreeSet::from([node_record, edge_record]),
		global_slots: BTreeSet::from([global_node.slot(), global_edge.slot()]),
		overlays: BTreeSet::new(),
		incidence: BTreeSet::from([(graph, node), (graph, edge)]),
		memberships: BTreeSet::from([(graph, global_node), (graph, global_edge)]),
		point_graph_only: true,
	};
	(before, Arc::new(after), delta)
}
