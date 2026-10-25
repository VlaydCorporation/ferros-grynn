//! Source payload scratch must see the Hot writes that remain live.

use std::collections::BTreeSet;

use super::*;
use crate::{
	datastore::PublicationDelta,
	gql_status::Status,
	storage::{
		atom_store_image::value_context,
		graph_index::with_external_sort_budget,
		local_store::{point_graph::GraphUpdatePlan, point_incidence, point_namespace::LabelPlan},
	},
};

/// Frozen from the same-fixture retained/scratch-only source-plan contrast.
const SOURCE_DETACH_NEAR_BUDGET: usize = 196 * 1024;

/// Small and high-budget controls isolate the retained Hot/source codec
/// overlap.
#[test]
fn overlay_lifecycle_source_detach_accounts_live_hot_writes() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for bytes in [64, 40_000] {
			let (mut fixture, pending) = membership_fixture(io_mode);
			let graph = fixture.graph.id();
			let record = &pending[0];
			let mut seed = fixture.store.begin();
			seed.attach_node(graph, record, None, None).assert_ok();
			seed.set_overlay(graph, record, fixture.keys[1], Value::Bytes(vec![7; bytes].into()))
				.assert_ok();
			seed.commit().assert_ok();
			fixture = reopen_checkpointed(fixture, io_mode);
			let local = local_store(&fixture.store);
			let published = fixture.store.inner.current_root();
			let mut before = published.as_ref().clone();
			local.hydrate_statistics(&mut before).assert_ok();
			before.rebuild_indexes().assert_ok();
			let global = before.resolve_atom(&pending[0]).assert_some();
			let atom = before.membership(graph, global).assert_some();
			let mut after = before.clone();
			after.detach(graph, global).assert_ok().assert_some();
			let delta = PublicationDelta {
				records: [pending[0].clone()].into_iter().collect(),
				global_slots: BTreeSet::new(),
				overlays: BTreeSet::new(),
				incidence: [(graph, atom)].into_iter().collect(),
				memberships: [(graph, global)].into_iter().collect(),
				point_graph_only: true,
			};
			assert!(point_incidence::eligible(&before, &after, &delta));
			let state = local.state.lock();
			let boundary = local.wal.snapshot().visible_lsn();
			let files = state.file_catalog.files.clone();
			let directories = generation_directories(&fixture.directory.0);
			for budget in [512 * 1024, SOURCE_DETACH_NEAR_BUDGET] {
				let result = with_external_sort_budget(budget, || {
					let mut labels = LabelPlan::build(local, &state, &before, &after, &delta)?;
					let mut context =
						value_context(&after.schema, after.dictionary.as_ref().clone())?;
					GraphUpdatePlan::build(
						local,
						&state,
						&before,
						&after,
						&delta,
						&mut labels,
						&mut context,
					)
				});
				if bytes == 40_000 && budget == SOURCE_DETACH_NEAR_BUDGET {
					let error = result.err().assert_some();
					assert!(matches!(
						error.status().status(),
						Status::StorageDecodeLimitExceeded { .. }
					));
				} else {
					result.assert_ok();
				}
			}
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(state.file_catalog.files, files);
			assert_eq!(generation_directories(&fixture.directory.0), directories);
			assert!(Arc::ptr_eq(&fixture.store.inner.current_root(), &published));
		}
	}
}
