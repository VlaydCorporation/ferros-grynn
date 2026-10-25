//! A fixed logical envelope admits a small arena and rejects its large copy.

use std::sync::Arc;

use super::{
	node_child_rebind_tests::{assert_node_state, node_change, node_owner, node_roots},
	*,
};
use crate::storage::graph_index::with_external_sort_budget;

/// Sync admission accounts for the complete retained canonical arena copy.
#[test]
fn node_child_rebind_sync_fixed_copy_budget_preserves_source() {
	copy_budget(IoMode::Sync);
}

/// NativeAsync uses the same fixed logical ceiling and unchanged defaults.
#[cfg(feature = "native-async-io")]
#[test]
fn node_child_rebind_native_async_fixed_copy_budget_preserves_source() {
	copy_budget(IoMode::NativeAsync);
}

/// The large copy alone exceeds this ceiling before either canonical map is
/// cloned.
fn copy_budget(mode: IoMode) {
	copy_budget_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn copy_budget_with_family(mode: IoMode, family: GraphKind) {
	let maximum = 128 * 1024;
	for additional in [0, 1024] {
		let (fixture, key) = super::metagraph::container_fixture(mode, family);
		let (_, _, old_child) = node_owner(&fixture);
		let (_, _, replacement) = super::transition_rewire_tests::portal(&fixture);
		if additional != 0 {
			let mut setup = fixture.store.begin();
			for index in 0..additional {
				let record = format!("node:copy{index}").parse::<RecordId>().assert_ok();
				let mut properties = PropertyMap::new();
				properties.insert(key, 0_i64);
				setup.create_atom(record.clone(), properties).assert_ok();
				setup.attach_node(fixture.graph.id(), &record, None, Some(old_child)).assert_ok();
			}
			setup.commit().assert_ok();
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		}
		let store = local(&fixture.store);
		let root = fixture.store.inner.current_root();
		let (before, after, delta) = node_roots(&fixture, replacement);
		let (files, arena) = {
			let state = store.state.lock();
			(
				state.file_catalog.files.clone(),
				state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == fixture.graph.id())
					.assert_some()
					.arena
					.clone(),
			)
		};
		assert_eq!(arena.retained_copy_bytes().assert_ok() > maximum, additional != 0);
		let boundary = store.wal.snapshot().visible_lsn();
		let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
		let result = with_external_sort_budget(maximum, || {
			super::metagraph::prepare_leaf(store, (&before, &after), &delta)
		});
		if additional == 0 {
			let plan = result.assert_ok();
			assert_eq!(plan.arenas.len(), 1);
		} else {
			let error = result.err().assert_some();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
		}
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		let state = store.state.lock();
		assert_eq!(state.file_catalog.files, files);
		assert!(
			state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(&arena)
		);
		assert!(
			state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == fixture.graph.id())
				.assert_some()
				.arena
				.same_owner(&arena)
		);
		drop(state);
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
		// The passing control restores the existing scoped policy; production
		// defaults are never raised to admit this test.
		super::metagraph::prepare_leaf(store, (&before, &after), &delta).assert_ok();
		let (global, owner, _) = node_owner(&fixture);
		node_change(&fixture, replacement).commit().assert_ok();
		assert_node_state(&fixture, global, owner, replacement);
		assert_retained(&fixture, &files, false);
		assert_bulk_image(&fixture);
	}
}
