//! Canonical T arenas share the same logical envelope as the six M/R/S maps.

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore, OccurrenceState},
	storage::graph_index::with_external_sort_budget,
	test_assertions::{AssertOption as _, AssertResult as _},
	values::{PropertyMap, RecordId},
};

/// Records the builder's logical growth and peak without allocator
/// measurements.
fn admission_receipt(root: &DatabaseRoot, graph: GraphId) -> (GraphArenaBindings, usize, usize) {
	let mut retained = 0_usize;
	let mut peak = 0_usize;
	let arena = GraphArenaBindings::build_admitted(root, graph, |growth, scratch| {
		let next = retained
			.checked_add(growth)
			.ok_or_else(|| invalid("depth admission retained bytes overflow usize"))?;
		peak = peak.max(
			next.checked_add(scratch)
				.ok_or_else(|| invalid("depth admission peak bytes overflow usize"))?,
		);
		retained = next;
		Ok(())
	})
	.assert_ok();
	(arena, retained, peak)
}

/// Six earlier maps fit, but an individually admitted T map exhausts their
/// account.
#[test]
fn canonical_depth_arenas_reject_top_only_cumulative_overrun() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let graphs = ["middle", "outer_r", "outer_s", "top"]
		.map(|name| seed.define_graph(name, GraphKind::Metagraph).assert_ok().id());
	let leaves =
		["old", "new"].map(|name| seed.define_graph(name, GraphKind::Graph).assert_ok().id());
	let records = ["item:a", "item:b", "item:c"].map(|name| name.parse::<RecordId>().assert_ok());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let selected = seed.attach_node(graphs[0], &records[0], None, Some(leaves[0])).assert_ok();
	seed.attach_node(graphs[0], &records[1], None, Some(leaves[0])).assert_ok();
	for record in &records[..2] {
		seed.attach_node(graphs[1], record, None, Some(graphs[0])).assert_ok();
		seed.attach_node(graphs[3], record, None, Some(graphs[1])).assert_ok();
	}
	seed.attach_node(graphs[1], &records[2], None, Some(leaves[0])).assert_ok();
	seed.attach_node(graphs[2], &records[0], None, Some(graphs[0])).assert_ok();
	seed.commit().assert_ok();
	let before = store.inner.current_root();
	let mut after = before.as_ref().clone();
	after
		.replace_occurrence_state(graphs[0], selected, OccurrenceState::node(Some(leaves[1])))
		.assert_ok();
	let inputs = [
		(before.as_ref(), graphs[0]),
		(&after, graphs[0]),
		(before.as_ref(), graphs[1]),
		(&after, graphs[1]),
		(before.as_ref(), graphs[2]),
		(&after, graphs[2]),
		(before.as_ref(), graphs[3]),
		(&after, graphs[3]),
	];
	let receipts = inputs.map(|(root, graph)| admission_receipt(root, graph));
	assert_eq!(receipts.each_ref().map(|(arena, _, _)| arena.graph_by_slot().len()), [
		3, 3, 8, 8, 4, 4, 17, 17
	]);
	let mut prefix_retained = 0_usize;
	let mut prefix_peak = 0_usize;
	for (_, retained, peak) in &receipts[..6] {
		prefix_peak = prefix_peak.max(prefix_retained.checked_add(*peak).assert_some());
		prefix_retained = prefix_retained.checked_add(*retained).assert_some();
	}
	// One fixed fixture derives its logical limit; no timing or RSS
	// calibration.
	let maximum = prefix_peak.max(receipts[6].2).max(receipts[7].2);
	assert!(maximum < PublicationBudget::new().remaining().assert_ok());
	assert!(prefix_retained.checked_add(receipts[6].2).assert_some() > maximum);
	with_external_sort_budget(maximum, || {
		let mut budget = PublicationBudget::new();
		for (index, (root, graph)) in inputs[..6].iter().enumerate() {
			assert_eq!(admitted_arena(root, *graph, &mut budget)?, receipts[index].0);
		}
		for index in 6..8 {
			let mut own_budget = PublicationBudget::new();
			assert_eq!(
				admitted_arena(inputs[index].0, inputs[index].1, &mut own_budget)?,
				receipts[index].0
			);
		}
		Ok(())
	})
	.assert_ok();
	let mut completed = 0;
	let error = with_external_sort_budget(maximum, || {
		let mut budget = PublicationBudget::new();
		for (root, graph) in inputs {
			let _arena = admitted_arena(root, graph, &mut budget)?;
			completed += 1;
		}
		Ok(())
	})
	.assert_err();
	assert_eq!(completed, 6, "only the first top map must exceed the shared envelope");
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	let mut normal = PublicationBudget::new();
	for index in 6..8 {
		assert_eq!(
			admitted_arena(inputs[index].0, inputs[index].1, &mut normal).assert_ok(),
			receipts[index].0
		);
	}
	assert!(Arc::ptr_eq(&before, &store.inner.current_root()));
	eprintln!("ANCESTOR_DEPTH_CUMULATIVE_REJECT_PASS completed={completed} limit={maximum}");
}
