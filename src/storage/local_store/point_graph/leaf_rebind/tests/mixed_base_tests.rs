//! A mixed leaf replacement composes with detached stable Base owners.

use std::{collections::BTreeMap, sync::Arc};

use super::*;
use crate::values::Value;

/// Real private-root mutations retain the exact record/slot journal pairing.
fn mixed_roots(family: GraphKind) -> (Fixture, DatabaseRoot, PublicationDelta) {
	let mut fixture = fixture(family);
	let key = Arc::make_mut(&mut fixture.before.dictionary).intern("mixed_value").assert_ok();
	let mut after = rebound(&fixture);
	after.set_base_property(fixture.globals[1], key, Value::from(7_i64)).assert_ok();
	let mut delta = delta(&fixture);
	delta.records.insert("item:spare".parse().assert_ok());
	delta.global_slots.insert(fixture.globals[1].slot());
	assert!(fixture.before.memberships(fixture.globals[1]).is_none_or(BTreeMap::is_empty));
	assert!(after.memberships(fixture.globals[1]).is_none_or(BTreeMap::is_empty));
	(fixture, after, delta)
}

/// Metagraph replacement composes with an independent generation-stable Base.
#[test]
fn mixed_leaf_metagraph_accepts_one_unattached_base_update() {
	accepts_mixed(GraphKind::Metagraph);
}

/// Genuine Graph replacement uses the same independent Base contract.
#[test]
fn mixed_leaf_graph_accepts_one_unattached_base_update() {
	accepts_mixed(GraphKind::Graph);
}

/// A positive candidate must remain replacement-only, without scope remapping.
fn accepts_mixed(family: GraphKind) {
	let (fixture, after, delta) = mixed_roots(family);
	let selected = candidate(&fixture.before, &after, &delta);
	assert!(selected.is_some(), "MIXED_LEAF_BASE_CANDIDATE_NOT_ADMITTED");
	let selected = selected.assert_some();
	assert_eq!(
		(selected.graph, selected.local, selected.global),
		(fixture.parent, fixture.owner, fixture.globals[0])
	);
	assert_eq!(
		(selected.before_child, selected.after_child),
		(Some(fixture.graphs[0]), Some(fixture.graphs[1]))
	);
	assert!(remap_candidate(&fixture.before, &after, &delta).is_none());
}

/// A record without its exact replay slot cannot authorize another Base owner.
#[test]
fn mixed_leaf_rejects_incomplete_and_mismatched_base_journal() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for case in 0..5 {
			let (fixture, after, mut delta) = mixed_roots(family);
			match case {
				0 => delta.records.clear(),
				1 => delta.global_slots.clear(),
				2 => {
					delta.global_slots = BTreeSet::from([fixture.globals[0].slot()]);
				}
				3 => {
					delta.records.insert("item:owner".parse().assert_ok());
				}
				_ => {
					delta.global_slots.insert(fixture.globals[2].slot());
				}
			}
			assert!(candidate(&fixture.before, &after, &delta).is_none(), "case {case}");
			assert!(remap_candidate(&fixture.before, &after, &delta).is_none(), "case {case}");
		}
	}
}

/// Either accepted membership side would require an Effective fanout plan.
#[test]
fn mixed_leaf_rejects_base_memberships_on_either_side() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for side in 0..3 {
			let (mut fixture, mut after, delta) = mixed_roots(family);
			assert!(candidate(&fixture.before, &after, &delta).is_some());
			if side != 1 {
				fixture
					.before
					.attach_node(fixture.graphs[2], fixture.globals[1], None, None)
					.assert_ok();
			}
			if side != 0 {
				after.attach_node(fixture.graphs[2], fixture.globals[1], None, None).assert_ok();
			}
			assert!(!closed_delta(&fixture.before, &after, &delta), "{family:?} side={side}");
			assert!(candidate(&fixture.before, &after, &delta).is_none());
		}
	}
}

/// Stable record names cannot hide a deleted, reused or transient allocator.
#[test]
fn mixed_leaf_rejects_base_lifecycle_and_capacity_changes() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for fault in 0..4 {
			let (fixture, mut after, delta) = mixed_roots(family);
			assert!(candidate(&fixture.before, &after, &delta).is_some());
			let class = fixture.before.schema.class("item").assert_some().clone();
			match fault {
				0 => {
					after.remove_atom(fixture.globals[1]).assert_ok();
				}
				1 => {
					after.remove_atom(fixture.globals[1]).assert_ok();
					let reused = after
						.insert_atom(&class, "item:spare".parse().assert_ok(), PropertyMap::new())
						.assert_ok();
					assert_eq!(reused.slot(), fixture.globals[1].slot());
					assert_ne!(reused, fixture.globals[1]);
				}
				2 | 3 => {
					let added = after
						.insert_atom(&class, "item:added".parse().assert_ok(), PropertyMap::new())
						.assert_ok();
					if fault == 3 {
						after.remove_atom(added).assert_ok();
					}
					assert_ne!(fixture.before.atoms.slot_capacity(), after.atoms.slot_capacity());
				}
				_ => unreachable!("bounded lifecycle case"),
			}
			assert!(!closed_delta(&fixture.before, &after, &delta), "{family:?} fault={fault}");
			assert!(candidate(&fixture.before, &after, &delta).is_none());
		}
	}
}

/// A detached Base does not widen presence, dictionary or unrelated journals.
#[test]
fn mixed_leaf_keeps_presence_and_other_journals_on_fallback() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for fault in 0..7 {
			let (mut fixture, mut after, mut delta) = mixed_roots(family);
			assert!(candidate(&fixture.before, &after, &delta).is_some());
			match fault {
				0 => fixture
					.before
					.replace_occurrence_state(
						fixture.parent,
						fixture.owner,
						OccurrenceState::node(None),
					)
					.assert_ok(),
				1 => after
					.replace_occurrence_state(
						fixture.parent,
						fixture.owner,
						OccurrenceState::node(None),
					)
					.assert_ok(),
				2 => {
					delta.incidence.insert((fixture.graphs[2], fixture.owner));
				}
				3 => {
					Arc::make_mut(&mut after.dictionary).intern("new_mixed_key").assert_ok();
				}
				4 => {
					delta.overlays.insert((fixture.parent, fixture.globals[0]));
				}
				5 => {
					delta.memberships.insert((fixture.parent, fixture.globals[0]));
				}
				_ => delta.point_graph_only = false,
			}
			assert!(!closed_delta(&fixture.before, &after, &delta), "{family:?} fault={fault}");
			assert!(candidate(&fixture.before, &after, &delta).is_none());
			assert!(remap_candidate(&fixture.before, &after, &delta).is_none());
		}
	}
}

/// A detached Base preserves the existing two-level ancestor boundary.
#[test]
fn mixed_leaf_preserves_the_ancestor_depth_boundary() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		let (mut fixture, _, _) = mixed_roots(family);
		for depth in 0..4 {
			if depth != 0 {
				let child = if depth == 1 {
					fixture.parent
				} else {
					fixture.graphs[depth]
				};
				fixture
					.before
					.attach_node(fixture.graphs[depth + 1], fixture.globals[0], None, Some(child))
					.assert_ok();
			}
			let mut after = rebound(&fixture);
			let key = after.dictionary.get("mixed_value").assert_some();
			after.set_base_property(fixture.globals[1], key, Value::from(7_i64)).assert_ok();
			let mut delta = delta(&fixture);
			delta.records.insert("item:spare".parse().assert_ok());
			delta.global_slots.insert(fixture.globals[1].slot());
			assert_eq!(
				candidate(&fixture.before, &after, &delta).is_some(),
				depth <= 2,
				"{family:?} depth={depth}"
			);
			assert!(remap_candidate(&fixture.before, &after, &delta).is_none());
		}
	}
}

/// Record order differs from slot order; admission must validate each lookup.
#[test]
fn mixed_leaf_batch_matches_records_to_slots_without_zip() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		let (mut fixture, _, _) = mixed_roots(family);
		let key = fixture.before.dictionary.get("mixed_value").assert_some();
		let class = fixture.before.schema.class("item").assert_some().clone();
		let extra_record = "item:a".parse::<RecordId>().assert_ok();
		let extra = fixture
			.before
			.insert_atom(&class, extra_record.clone(), PropertyMap::new())
			.assert_ok();
		let mut after = rebound(&fixture);
		after.set_base_property(fixture.globals[1], key, 7_i64.into()).assert_ok();
		after.set_base_property(extra, key, 9_i64.into()).assert_ok();
		let mut delta = delta(&fixture);
		delta.records = BTreeSet::from(["item:spare".parse().assert_ok(), extra_record]);
		delta.global_slots = BTreeSet::from([fixture.globals[1].slot(), extra.slot()]);
		assert_eq!(fixture.before.atoms.slot_capacity(), after.atoms.slot_capacity());
		let resolved = delta
			.records
			.iter()
			.map(|record| {
				let old = fixture.before.resolve_atom(record).assert_some();
				assert_eq!(after.resolve_atom(record), Some(old));
				for root in [&fixture.before, &after] {
					assert!(root.memberships(old).is_none_or(BTreeMap::is_empty));
				}
				old.slot()
			})
			.collect::<Vec<_>>();
		assert_ne!(resolved, delta.global_slots.iter().copied().collect::<Vec<_>>());
		let selected = candidate(&fixture.before, &after, &delta);
		assert!(selected.is_some(), "MIXED_BASE_BATCH_NOT_ADMITTED {family:?}");
		let selected = selected.assert_some();
		assert_eq!(
			(selected.graph, selected.local, selected.global),
			(fixture.parent, fixture.owner, fixture.globals[0])
		);
		assert!(remap_candidate(&fixture.before, &after, &delta).is_none());
	}
}

/// An invalid second record must not inherit admission from the valid first.
#[test]
fn mixed_leaf_batch_rejects_invalid_second_owner_and_slot() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for fault in 0..7 {
			let (mut fixture, _, _) = mixed_roots(family);
			let key = fixture.before.dictionary.get("mixed_value").assert_some();
			let class = fixture.before.schema.class("item").assert_some().clone();
			let record = "item:z".parse::<RecordId>().assert_ok();
			let extra =
				fixture.before.insert_atom(&class, record.clone(), PropertyMap::new()).assert_ok();
			let mut after = rebound(&fixture);
			after.set_base_property(fixture.globals[1], key, 7_i64.into()).assert_ok();
			after.set_base_property(extra, key, 9_i64.into()).assert_ok();
			let mut delta = delta(&fixture);
			delta.records = BTreeSet::from(["item:spare".parse().assert_ok(), record.clone()]);
			delta.global_slots = BTreeSet::from([fixture.globals[1].slot(), extra.slot()]);
			assert_eq!(delta.records.last(), Some(&record));
			assert!(candidate(&fixture.before, &after, &delta).is_some());
			match fault {
				0 => {
					fixture.before.attach_node(fixture.graphs[2], extra, None, None).assert_ok();
				}
				1 => {
					after.attach_node(fixture.graphs[2], extra, None, None).assert_ok();
				}
				2 => {
					after.remove_atom(extra).assert_ok();
				}
				3 => {
					after.remove_atom(extra).assert_ok();
					let reused =
						after.insert_atom(&class, record.clone(), PropertyMap::new()).assert_ok();
					assert_eq!(reused.slot(), extra.slot());
					assert_ne!(reused, extra);
				}
				4 => {
					delta.global_slots.remove(&extra.slot());
					delta.global_slots.insert(fixture.globals[0].slot());
				}
				5 => {
					delta.records.remove(&record);
					delta.records.insert("item:zz_missing".parse().assert_ok());
				}
				_ => {
					delta.records.remove(&record);
					delta.records.insert("item:owner".parse().assert_ok());
					delta.global_slots.remove(&extra.slot());
					delta.global_slots.insert(fixture.globals[0].slot());
				}
			}
			assert!(
				candidate(&fixture.before, &after, &delta).is_none(),
				"{family:?} fault={fault}"
			);
			assert!(remap_candidate(&fixture.before, &after, &delta).is_none());
		}
	}
}

/// Adding Base records does not widen the supported ancestor depth boundary.
#[test]
fn mixed_leaf_batch_preserves_the_ancestor_depth_boundary() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		let (mut fixture, _, _) = mixed_roots(family);
		let class = fixture.before.schema.class("item").assert_some().clone();
		let record = "item:batch_extra".parse::<RecordId>().assert_ok();
		let extra =
			fixture.before.insert_atom(&class, record.clone(), PropertyMap::new()).assert_ok();
		for depth in 0..4 {
			if depth != 0 {
				let child = if depth == 1 {
					fixture.parent
				} else {
					fixture.graphs[depth]
				};
				fixture
					.before
					.attach_node(fixture.graphs[depth + 1], fixture.globals[0], None, Some(child))
					.assert_ok();
			}
			let mut after = rebound(&fixture);
			let key = after.dictionary.get("mixed_value").assert_some();
			after.set_base_property(fixture.globals[1], key, 7_i64.into()).assert_ok();
			after.set_base_property(extra, key, 9_i64.into()).assert_ok();
			let mut delta = delta(&fixture);
			delta.records = BTreeSet::from(["item:spare".parse().assert_ok(), record.clone()]);
			delta.global_slots = BTreeSet::from([fixture.globals[1].slot(), extra.slot()]);
			assert_eq!(
				candidate(&fixture.before, &after, &delta).is_some(),
				depth <= 2,
				"{family:?} depth={depth}"
			);
			assert!(remap_candidate(&fixture.before, &after, &delta).is_none());
		}
	}
}
