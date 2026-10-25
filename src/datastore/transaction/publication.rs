//! Exact physical publication targets from the final accepted replay.

use std::collections::BTreeSet;

use super::{Mutation, Transaction, commit::ReplayRemap};
use crate::datastore::{ConflictKey, DatabaseRoot, PublicationDelta};

impl Transaction {
	/// Keeps physical slots even when create/delete leaves no logical record.
	pub(super) fn publication_delta(
		&self,
		current: &DatabaseRoot,
		next: &DatabaseRoot,
		remap: &ReplayRemap,
	) -> PublicationDelta {
		let mut delta = PublicationDelta {
			records: BTreeSet::new(),
			global_slots: BTreeSet::new(),
			overlays: BTreeSet::new(),
			incidence: BTreeSet::new(),
			memberships: BTreeSet::new(),
			point_graph_only: self.mutations.iter().all(|mutation| {
				if let Mutation::ReplaceGraph(replacement) = mutation {
					return current
						.schema
						.graphs
						.get(&replacement.id())
						.is_some_and(|before| replacement.is_endpoint_extension_of(before));
				}
				matches!(
					mutation,
					Mutation::SetBaseProperty { .. }
						| Mutation::InternProperty { .. }
						| Mutation::RemoveBaseProperty { .. }
						| Mutation::CreateAtom { .. }
						| Mutation::DeleteAtom { .. }
						| Mutation::SetOverlay { .. }
						| Mutation::HideProperty { .. }
						| Mutation::ResetProperty { .. }
						| Mutation::ReplaceOccurrenceState { .. }
						| Mutation::SetOccurrenceLabel { .. }
						| Mutation::AttachNode { .. }
						| Mutation::AttachEdge { .. }
						| Mutation::Detach { .. }
				)
			}),
		};
		for key in self.writes.keys() {
			let global = match remap.conflict(*key) {
				ConflictKey::BaseAtom(global) => {
					delta.global_slots.insert(global.slot());
					global
				}
				ConflictKey::GraphOverlay(graph, global) => {
					delta.overlays.insert((graph, global));
					global
				}
				ConflictKey::GraphMembership(graph, global) => {
					delta.memberships.insert((graph, global));
					global
				}
				ConflictKey::GraphIncidence(graph, local) => {
					delta.incidence.insert((graph, local));
					continue;
				}
				_ => continue,
			};
			// Reused slots can retain distinct source and final RecordIds.
			if let Some(atom) = current.atom(global).or_else(|| next.atom(global))
				&& !delta.records.contains(atom.record_id())
			{
				delta.records.insert(atom.record_id().clone());
			}
		}
		delta
	}
}

#[cfg(test)]
/// Characterizes exact identities before extending graph-local target kinds.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore},
		graph::{AtomKind, GraphKind, GraphRef},
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyKeyId, PropertyMap, RecordId},
	};

	/// Seeds schema without reserving global or graph-local atom slots.
	fn fixture() -> (Datastore, GraphRef, PropertyKeyId) {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
		let key = seed.intern_property("value").assert_ok();
		seed.commit().assert_ok();
		(store, graph, key)
	}

	/// Executes the production replay to avoid constructing synthetic remap
	/// maps.
	fn replay(write: &Transaction, current: &DatabaseRoot) -> (DatabaseRoot, ReplayRemap) {
		let mut next = current.clone();
		let remap =
			Transaction::apply_mutations(&mut next, &write.mutations, current.commit_ts + 1, true)
				.assert_ok();
		(next, remap)
	}

	/// Removed logical records and transient allocator slots retain distinct
	/// roles.
	#[test]
	fn publication_targets_preserve_reused_and_transient_base_slots() {
		let (store, _graph, _key) = fixture();
		let old = "item:old".parse::<RecordId>().assert_ok();
		let replacement = "item:replacement".parse::<RecordId>().assert_ok();
		let transient = "item:transient".parse::<RecordId>().assert_ok();
		let mut seed = store.begin();
		seed.create_atom(old.clone(), PropertyMap::new()).assert_ok();
		seed.commit().assert_ok();
		let current = store.inner.current_root();
		let old_id = current.resolve_atom(&old).assert_some();
		let mut write = store.begin();
		write.delete_atom(&old).assert_ok();
		write.create_atom(replacement.clone(), PropertyMap::new()).assert_ok();
		let replacement_id = write.working.resolve_atom(&replacement).assert_some();
		assert_eq!(replacement_id.slot(), old_id.slot());
		assert_ne!(replacement_id, old_id);
		write.create_atom(transient.clone(), PropertyMap::new()).assert_ok();
		let transient_id = write.working.resolve_atom(&transient).assert_some();
		write.delete_atom(&transient).assert_ok();
		let (next, remap) = replay(&write, &current);
		let delta = write.publication_delta(&current, &next, &remap);
		assert_eq!(delta.records, BTreeSet::from([old, replacement]));
		assert_eq!(
			delta.global_slots,
			BTreeSet::from([
				remap.global(replacement_id).slot(),
				remap.global(transient_id).slot(),
			])
		);
		assert!(next.atom(remap.global(transient_id)).is_none());
		assert!(delta.overlays.is_empty());
		assert!(delta.incidence.is_empty());
		assert!(delta.memberships.is_empty());
		assert!(delta.point_graph_only);
	}

	/// Concurrent allocations must not publish the private source slot
	/// addresses.
	#[test]
	fn publication_targets_use_final_replay_allocation_addresses() {
		let (store, graph, key) = fixture();
		let pending = "item:pending".parse::<RecordId>().assert_ok();
		let other = "item:other".parse::<RecordId>().assert_ok();
		let mut write = store.begin();
		write.create_atom(pending.clone(), PropertyMap::new()).assert_ok();
		let private_global = write.working.resolve_atom(&pending).assert_some();
		let private_local = write.attach_node(graph.id(), &pending, None, None).assert_ok();
		write.set_overlay(graph.id(), &pending, key, 10.into()).assert_ok();
		let mut concurrent = store.begin();
		concurrent.create_atom(other.clone(), PropertyMap::new()).assert_ok();
		concurrent.attach_node(graph.id(), &other, None, None).assert_ok();
		concurrent.commit().assert_ok();
		let current = store.inner.current_root();
		let (next, remap) = replay(&write, &current);
		let actual_global = next.resolve_atom(&pending).assert_some();
		let actual_local = next.membership(graph.id(), actual_global).assert_some();
		assert_ne!(actual_global, private_global);
		assert_ne!(actual_local, private_local);
		assert_eq!(remap.global(private_global), actual_global);
		assert_eq!(remap.local(graph.id(), private_local), actual_local);
		let delta = write.publication_delta(&current, &next, &remap);
		assert_eq!(delta.records, BTreeSet::from([pending]));
		assert_eq!(delta.global_slots, BTreeSet::from([actual_global.slot()]));
		assert_eq!(delta.overlays, BTreeSet::from([(graph.id(), actual_global)]));
		assert_eq!(delta.incidence, BTreeSet::from([(graph.id(), actual_local)]));
		assert_eq!(delta.memberships, BTreeSet::from([(graph.id(), actual_global)]));
		assert!(delta.point_graph_only);
	}

	/// Replayed transient generations must not overwrite an independent writer.
	#[test]
	fn publication_targets_retain_rebased_transient_and_reused_occurrence_slots() {
		let (store, graph, _) = fixture();
		let records = ["item:transient", "item:replacement", "item:concurrent"]
			.map(|record| record.parse::<RecordId>().assert_ok());
		let mut seed = store.begin();
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		seed.commit().assert_ok();
		let mut write = store.begin();
		let transient = write.attach_node(graph.id(), &records[0], None, None).assert_ok();
		write.detach_occurrence(graph.id(), &records[0], false).assert_ok();
		let reused = write.attach_node(graph.id(), &records[1], None, None).assert_ok();
		assert_eq!(transient.slot(), reused.slot());
		assert_eq!(reused.generation(), transient.generation() + 1);
		let mut concurrent = store.begin();
		let independent = concurrent.attach_node(graph.id(), &records[2], None, None).assert_ok();
		concurrent.commit().assert_ok();
		let current = store.inner.current_root();
		let (next, remap) = replay(&write, &current);
		let actual_transient = remap.local(graph.id(), transient);
		let actual_reused = remap.local(graph.id(), reused);
		assert_ne!(actual_transient.slot(), independent.slot());
		assert_eq!(actual_transient.slot(), actual_reused.slot());
		assert_eq!(actual_reused.generation(), actual_transient.generation() + 1);
		let globals = records.map(|record| current.resolve_atom(&record).assert_some());
		assert_eq!(next.membership(graph.id(), globals[0]), None);
		assert_eq!(next.membership(graph.id(), globals[1]), Some(actual_reused));
		assert_eq!(next.membership(graph.id(), globals[2]), Some(independent));
		let delta = write.publication_delta(&current, &next, &remap);
		assert!(delta.global_slots.is_empty());
		assert_eq!(
			delta.incidence,
			BTreeSet::from([(graph.id(), actual_transient), (graph.id(), actual_reused)])
		);
		assert_eq!(
			delta.memberships,
			BTreeSet::from([(graph.id(), globals[0]), (graph.id(), globals[1])])
		);
	}

	/// EDGE-only publication remains closed after replay remaps its local slot.
	#[test]
	fn publication_targets_include_rebased_plain_edge_attachment() {
		use crate::graph::EdgeTopology;
		let (store, graph, _) = fixture();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let nodes = ["item:a", "item:b"].map(|record| {
			let record = record.parse::<RecordId>().assert_ok();
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
			seed.attach_node(graph.id(), &record, None, None).assert_ok()
		});
		let records =
			["link:pending", "link:other"].map(|record| record.parse::<RecordId>().assert_ok());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		seed.commit().assert_ok();
		let mut write = store.begin();
		let private = write
			.attach_edge(
				graph.id(),
				&records[0],
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		let mut concurrent = store.begin();
		concurrent
			.attach_edge(
				graph.id(),
				&records[1],
				None,
				EdgeTopology::directed([nodes[0]], [nodes[1]]),
				None,
				None,
			)
			.assert_ok();
		concurrent.commit().assert_ok();
		let current = store.inner.current_root();
		let (next, remap) = replay(&write, &current);
		let global = current.resolve_atom(&records[0]).assert_some();
		let accepted = next.membership(graph.id(), global).assert_some();
		assert_eq!((private.slot(), accepted.slot()), (0, 1));
		let delta = write.publication_delta(&current, &next, &remap);
		assert!(delta.point_graph_only);
		assert!(delta.global_slots.is_empty());
		assert!(delta.overlays.is_empty());
		assert_eq!(delta.incidence, BTreeSet::from([(graph.id(), accepted)]));
		assert_eq!(delta.memberships, BTreeSet::from([(graph.id(), global)]));
	}
	/// Final replay coalesces Base writes without adding the separate leaf
	/// owner.
	#[test]
	fn publication_targets_pair_leaf_replacement_and_detached_base() {
		let (store, graph, key) = fixture();
		let records = ["item:container", "item:detached"]
			.map(|record| record.parse::<RecordId>().assert_ok());
		let mut seed = store.begin();
		let children = ["old_child", "new_child"]
			.map(|name| seed.define_graph(name, GraphKind::Graph).assert_ok().id());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let owner = seed.attach_node(graph.id(), &records[0], None, Some(children[0])).assert_ok();
		seed.commit().assert_ok();
		let mut write = store.begin();
		write
			.replace_occurrence_state(
				graph.id(),
				&records[0],
				crate::datastore::OccurrenceState::node(Some(children[1])),
			)
			.assert_ok();
		write.set_base_property(&records[1], key, crate::values::Value::from(7_i64)).assert_ok();
		write.remove_base_property(&records[1], key).assert_ok();
		write.set_base_property(&records[1], key, crate::values::Value::from(9_i64)).assert_ok();
		let current = store.inner.current_root();
		let (next, remap) = replay(&write, &current);
		let globals = records.each_ref().map(|record| current.resolve_atom(record).assert_some());
		assert_eq!(next.resolve_atom(&records[1]), Some(globals[1]));
		assert_eq!(current.atoms.slot_capacity(), next.atoms.slot_capacity());
		assert!(current.memberships(globals[1]).is_none_or(|members| members.is_empty()));
		assert!(next.memberships(globals[1]).is_none_or(|members| members.is_empty()));
		assert_eq!(current.atom(globals[1]).assert_some().properties().get(key), None);
		assert_eq!(
			next.atom(globals[1]).assert_some().properties().get(key),
			Some(&crate::values::Value::from(9_i64))
		);
		assert_eq!(
			current.occurrence(graph.id(), owner).assert_some().child_graph(),
			Some(children[0])
		);
		assert_eq!(
			next.occurrence(graph.id(), owner).assert_some().child_graph(),
			Some(children[1])
		);
		assert!(std::sync::Arc::ptr_eq(&current.dictionary, &next.dictionary));
		let delta = write.publication_delta(&current, &next, &remap);
		assert_eq!(delta.records, BTreeSet::from([records[1].clone()]));
		assert_eq!(delta.global_slots, BTreeSet::from([globals[1].slot()]));
		assert_eq!(delta.incidence, BTreeSet::from([(graph.id(), owner)]));
		assert!(delta.overlays.is_empty());
		assert!(delta.memberships.is_empty());
		assert!(delta.point_graph_only);
	}

	/// Replay retains both Base targets separately from the selected leaf
	/// owner.
	#[test]
	fn publication_targets_pair_leaf_replacement_and_detached_base_batch() {
		let (store, graph, key) = fixture();
		let records = ["item:container", "item:z", "item:a"]
			.map(|record| record.parse::<RecordId>().assert_ok());
		let mut seed = store.begin();
		let children = ["old_batch_child", "new_batch_child"]
			.map(|name| seed.define_graph(name, GraphKind::Graph).assert_ok().id());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		}
		let owner = seed.attach_node(graph.id(), &records[0], None, Some(children[0])).assert_ok();
		seed.commit().assert_ok();
		let mut write = store.begin();
		write
			.replace_occurrence_state(
				graph.id(),
				&records[0],
				crate::datastore::OccurrenceState::node(Some(children[1])),
			)
			.assert_ok();
		for (record, value) in [(&records[1], 9_i64), (&records[2], 11)] {
			write.set_base_property(record, key, crate::values::Value::from(7_i64)).assert_ok();
			write.remove_base_property(record, key).assert_ok();
			write.set_base_property(record, key, crate::values::Value::from(value)).assert_ok();
		}
		let current = store.inner.current_root();
		let (next, remap) = replay(&write, &current);
		let globals = records.each_ref().map(|record| current.resolve_atom(record).assert_some());
		for (index, value) in [(1, 9_i64), (2, 11)] {
			assert_eq!(next.resolve_atom(&records[index]), Some(globals[index]));
			assert!(current.memberships(globals[index]).is_none_or(|members| members.is_empty()));
			assert!(next.memberships(globals[index]).is_none_or(|members| members.is_empty()));
			assert_eq!(current.atom(globals[index]).assert_some().properties().get(key), None);
			assert_eq!(
				next.atom(globals[index]).assert_some().properties().get(key),
				Some(&crate::values::Value::from(value))
			);
		}
		assert_eq!(current.atoms.slot_capacity(), next.atoms.slot_capacity());
		assert_eq!(
			current.occurrence(graph.id(), owner).assert_some().child_graph(),
			Some(children[0])
		);
		assert_eq!(
			next.occurrence(graph.id(), owner).assert_some().child_graph(),
			Some(children[1])
		);
		assert!(std::sync::Arc::ptr_eq(&current.dictionary, &next.dictionary));
		let delta = write.publication_delta(&current, &next, &remap);
		assert_eq!(delta.records, BTreeSet::from([records[1].clone(), records[2].clone()]));
		assert_eq!(delta.global_slots, BTreeSet::from([globals[1].slot(), globals[2].slot()]));
		assert_eq!(delta.incidence, BTreeSet::from([(graph.id(), owner)]));
		assert!(delta.overlays.is_empty());
		assert!(delta.memberships.is_empty());
		assert!(delta.point_graph_only);
	}
}
