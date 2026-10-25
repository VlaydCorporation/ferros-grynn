//! Stable owner and state checks shared by admission and Hot metadata encoding.

use super::supports_point_indexes;
use crate::{
	datastore::{DatabaseRoot, GlobalAtomId, OccurrenceState, OccurrenceStateRef},
	graph::GraphId,
	values::Number,
};

/// Checks both runtime owners before accepting a graph-local Hot patch.
pub(super) fn stable_target(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	graph: GraphId,
	global: GlobalAtomId,
) -> bool {
	let Some((old, new)) = before.atom(global).zip(after.atom(global)) else {
		return false;
	};
	let Some((old_local, new_local)) =
		before.membership(graph, global).zip(after.membership(graph, global))
	else {
		return false;
	};
	let Some((old_occurrence, new_occurrence)) =
		before.occurrence(graph, old_local).zip(after.occurrence(graph, new_local))
	else {
		return false;
	};
	old.id() == new.id()
		&& old.class_id() == new.class_id()
		&& old_local == new_local
		&& old_occurrence.global_id() == global
		&& new_occurrence.global_id() == global
		&& stable_layout(old_occurrence.state(), new_occurrence.state())
		&& supports_point_indexes(after, new.class_id())
}

/// Full local handles and child bindings remain equal; weight is metadata.
pub(super) fn stable_layout(old: &OccurrenceState, new: &OccurrenceState) -> bool {
	old == new || stable_weights(old, new).is_some()
}

/// Borrows topology so weight admission never clones endpoint arrays.
pub(super) fn stable_weights(
	old: &OccurrenceState,
	new: &OccurrenceState,
) -> Option<(Option<Number>, Option<Number>)> {
	match (old.view(), new.view()) {
		(
			OccurrenceStateRef::Edge {
				topology: old_topology,
				weight: old_weight,
				transition_graph: old_transition,
			},
			OccurrenceStateRef::Edge {
				topology,
				weight,
				transition_graph,
			},
		) if old_topology == topology && old_transition == transition_graph => {
			Some((*old_weight, *weight))
		}
		_ => None,
	}
}

#[cfg(test)]
/// Paired layout proof and mixed dispatch preserve full accepted identities.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, PublicationDelta},
		graph::{AtomId, AtomKind, EdgeTopology, GraphKind, QualifiedAtomId},
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyMap, RecordId},
	};

	/// Paired layout proof preserves complete endpoint identities across
	/// metadata edits.
	#[test]
	fn stable_layout_preserves_topology_kind_and_child_binding() {
		let node = AtomId::from_parts(AtomKind::Node, 0, 0).assert_ok();
		let stale = AtomId::from_parts(AtomKind::Node, 0, 1).assert_ok();
		let edge = AtomId::from_parts(AtomKind::Edge, 0, 0).assert_ok();
		let child = GraphId::new(1).assert_some();
		let other = GraphId::new(2).assert_some();
		for child in [None, Some(child)] {
			let source = OccurrenceState::node(child);
			assert!(stable_layout(&source, &source));
			assert!(!stable_layout(&source, &OccurrenceState::node(Some(other))));
			for topology in [
				EdgeTopology::directed([node], [node]),
				EdgeTopology::directed([edge], [node]),
				EdgeTopology::directed([QualifiedAtomId::new([node], node).assert_ok()], [node]),
			] {
				let source = OccurrenceState::edge(topology.clone(), None, child);
				assert!(stable_layout(&source, &source));
				assert!(stable_layout(
					&source,
					&OccurrenceState::edge(topology.clone(), Some(7_i64.into()), child)
				));
				assert!(!stable_layout(
					&source,
					&OccurrenceState::edge(topology, None, Some(other))
				));
				assert!(!stable_layout(&source, &OccurrenceState::node(child)));
				assert!(!stable_layout(
					&source,
					&OccurrenceState::edge(EdgeTopology::directed([stale], [node]), None, child)
				));
			}
		}
		let original = OccurrenceState::edge(
			EdgeTopology::directed([QualifiedAtomId::new([node], node).assert_ok()], [node]),
			None,
			Some(child),
		);
		for endpoint in [
			QualifiedAtomId::new([stale], node).assert_ok(),
			QualifiedAtomId::new([node], stale).assert_ok(),
		] {
			assert!(!stable_layout(
				&original,
				&OccurrenceState::edge(
					EdgeTopology::directed([endpoint], [node]),
					None,
					Some(child)
				)
			));
		}
		assert!(!stable_layout(&OccurrenceState::node(Some(child)), &OccurrenceState::node(None)));
		assert!(!stable_layout(
			&original,
			&OccurrenceState::edge(
				EdgeTopology::directed([QualifiedAtomId::new([node], node).assert_ok()], [node]),
				None,
				None
			)
		));
	}

	/// Real mixed dispatch accepts stable qualified META metadata and rejects
	/// layout drift.
	#[test]
	fn mixed_metadata_target_requires_stable_layout_and_nonmembership() {
		use std::collections::BTreeSet;

		use super::super::{
			GraphOverlays,
			PublicationBudget,
			prepare_hot_change,
			stable_metadata_target,
		};
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("root", GraphKind::Metagraph).assert_ok().id();
		let child = seed.define_graph("child", GraphKind::Graph).assert_ok().id();
		let records = ["item:container", "item:other", "item:terminal", "link:meta", "link:spare"]
			.map(|name| name.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let container = seed.attach_node(graph, &records[0], None, Some(child)).assert_ok();
		let other = seed.attach_node(graph, &records[1], None, None).assert_ok();
		let terminal = seed.attach_node(child, &records[2], None, None).assert_ok();
		let topology =
			EdgeTopology::directed([QualifiedAtomId::new([container], terminal).assert_ok()], [
				other,
			]);
		seed.attach_edge(graph, &records[3], None, topology.clone(), None, Some(child)).assert_ok();
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		let global = before.resolve_atom(&records[3]).assert_some();
		let meta = before.membership(graph, global).assert_some();
		let spare_global = before.resolve_atom(&records[4]).assert_some();
		let state = OccurrenceState::edge(topology.clone(), Some(7_i64.into()), Some(child));
		let mut write = store.begin();
		write.replace_occurrence_state(graph, &records[3], state).assert_ok();
		let spare = write
			.attach_edge(
				graph,
				&records[4],
				None,
				EdgeTopology::directed([other], [container]),
				None,
				None,
			)
			.assert_ok();
		write.commit().assert_ok();
		let after = store.inner.current_root();
		assert_eq!(after.membership(graph, spare_global), Some(spare));
		let mut delta = PublicationDelta {
			records: BTreeSet::from([records[4].clone()]),
			global_slots: BTreeSet::new(),
			overlays: BTreeSet::new(),
			incidence: BTreeSet::from([(graph, meta), (graph, spare)]),
			memberships: BTreeSet::from([(graph, spare_global)]),
			point_graph_only: true,
		};
		assert!(stable_metadata_target(&before, &after, &delta, graph, global));
		assert!(super::super::eligible(&before, &after, &delta));
		delta.memberships.insert((graph, global));
		assert!(!stable_metadata_target(&before, &after, &delta, graph, global));
		delta.memberships.remove(&(graph, global));
		for state in [
			OccurrenceState::edge(
				EdgeTopology::directed([other], [container]),
				Some(7_i64.into()),
				Some(child),
			),
			OccurrenceState::edge(topology, Some(7_i64.into()), None),
		] {
			let mut changed = after.as_ref().clone();
			changed.replace_occurrence_state(graph, meta, state).assert_ok();
			// A changed owner may qualify for structural publication, never
			// this metadata route.
			assert!(!stable_metadata_target(&before, &changed, &delta, graph, global));
			let mut context =
				crate::values::storage::EncodeContext::new(changed.dictionary.as_ref().clone());
			let mut overlays = GraphOverlays::new(0, &mut context);
			let error = prepare_hot_change(
				&before,
				&changed,
				graph,
				global,
				None,
				&mut overlays,
				&mut PublicationBudget::new(),
			)
			.err()
			.assert_some();
			assert!(
				matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason } if reason.as_str() == "point graph metadata layout changed during preparation")
			);
		}
	}
}
