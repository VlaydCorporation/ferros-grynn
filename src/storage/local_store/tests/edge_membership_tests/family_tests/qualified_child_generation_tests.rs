//! Generation ordering revisits an already prepared retained EDGE Hot page.

use super::*;
use crate::{
	datastore::GraphOccurrence,
	graph::{EdgeEndpoint, QualifiedAtomId},
};

/// Sync coalesces page0 again after generation-zero page1 was prepared.
#[test]
fn qualified_child_generation_sync_reuses_hot_page_after_later_page() {
	remap(IoMode::Sync);
}

/// NativeAsync keeps both remapped roles through detach and reattach reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_generation_native_async_reuses_hot_page_after_later_page() {
	remap(IoMode::NativeAsync);
}

/// The observed committed identities, rather than allocation order alone,
/// establish the physical preparation order page0, page1, page0.
fn remap(mode: IoMode) {
	let (mut fixture, records, children) =
		child_scope_remap_tests::fixture(mode, AtomKind::Node, false);
	let root = fixture.store.inner.current_root();
	let portal = root
		.membership(fixture.graph.id(), root.resolve_atom(&records[1]).assert_some())
		.assert_some();
	let terminals = root
		.graph(children[1])
		.assert_some()
		.occurrences()
		.filter(|owner| owner.local_id().is_node())
		.map(GraphOccurrence::local_id)
		.collect::<Vec<_>>();
	assert_eq!(terminals.len(), 1);
	let terminal = terminals[0];
	let topology = EdgeTopology::directed(
		[EdgeEndpoint::Nested(QualifiedAtomId::new([portal], terminal).assert_ok())],
		[EdgeEndpoint::Local(fixture.nodes[1])],
	);
	drop(root);
	let spare = fixture.records[3].clone();
	let mut setup = fixture.store.begin();
	let original = setup
		.attach_edge(
			fixture.graph.id(),
			&spare,
			None,
			EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
			None,
			None,
		)
		.assert_ok();
	assert_eq!((original.slot(), original.generation()), (3, 0));
	let mut high = None;
	for slot in 4_u64..=102 {
		let record = format!("edge:qualified_generation_{slot}").parse::<RecordId>().assert_ok();
		setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		let local = setup
			.attach_edge(
				fixture.graph.id(),
				&record,
				None,
				if slot == 102 {
					topology.clone()
				} else {
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]])
				},
				None,
				None,
			)
			.assert_ok();
		assert_eq!((local.slot(), local.generation()), (slot, 0));
		if slot == 102 {
			high = Some((record, local));
		}
	}
	let (high_record, high_id) = high.assert_some();
	setup.commit().assert_ok();
	let mut reuse = fixture.store.begin();
	// Only this slot is freed, so the existing LIFO allocator must reuse it.
	reuse.detach_occurrence(fixture.graph.id(), &spare, false).assert_ok();
	let reused =
		reuse.attach_edge(fixture.graph.id(), &spare, None, topology, None, None).assert_ok();
	assert_eq!((reused.slot(), reused.generation()), (original.slot(), 1));
	reuse.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	let root = fixture.store.inner.current_root();
	let owner = |record: &RecordId| {
		let global = root.resolve_atom(record).assert_some();
		let local = root.membership(fixture.graph.id(), global).assert_some();
		root.occurrence(fixture.graph.id(), local).assert_some().clone()
	};
	let connectors = [owner(&high_record), owner(&spare)];
	assert_eq!(connectors[0].local_id(), high_id);
	assert_eq!(connectors[1].local_id(), reused);
	assert_eq!(root.occurrence(fixture.graph.id(), original), None);
	let expected = [portal, high_id, reused];
	let observed = root
		.graph(fixture.graph.id())
		.assert_some()
		.occurrences()
		.map(GraphOccurrence::local_id)
		.filter(|local| expected.contains(local))
		.collect::<Vec<_>>();
	assert_eq!(observed, expected);
	assert_eq!(
		observed.iter().map(|local| (local.slot(), local.generation())).collect::<Vec<_>>(),
		[(2, 0), (102, 0), (3, 1)]
	);
	assert_eq!(
		observed
			.iter()
			.map(|local| hot_slot_address(local.slot()).assert_ok().0.get())
			.collect::<Vec<_>>(),
		[0, 1, 0]
	);
	assert!(portal.raw() < high_id.raw() && high_id.raw() < reused.raw());
	drop(root);
	for connector in &connectors {
		qualified_child_scope_tests::assert_inline(&fixture, connector, terminal, 2);
	}
	eprintln!("QUALIFIED_CHILD_GENERATION_SOURCE_PASS {mode:?}");
	fixture = child_scope_remap_tests::publish(fixture, records.clone(), children, false);
	for connector in &connectors {
		qualified_child_scope_tests::assert_inline(&fixture, connector, terminal, 1);
	}
	eprintln!("QUALIFIED_CHILD_GENERATION_DETACH_COLD_PASS {mode:?}");
	// Cold open backfills the previous tail, and the shared property already
	// exists; establish a fresh boundary before retaining the next source pin.
	let mut warm = fixture.store.begin();
	warm.intern_property("qualified_child_generation_reattach_pin_source").assert_ok();
	warm.commit().assert_ok();
	fixture = child_scope_remap_tests::publish(fixture, records, children, true);
	for connector in &connectors {
		qualified_child_scope_tests::assert_inline(&fixture, connector, terminal, 2);
	}
	assert_bulk_image(&fixture);
	eprintln!("QUALIFIED_CHILD_GENERATION_ATTACH_COLD_PASS {mode:?}");
}
