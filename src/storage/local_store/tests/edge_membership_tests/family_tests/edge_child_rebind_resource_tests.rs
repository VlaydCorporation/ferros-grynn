//! Source role decoding shares the fixed leaf publication envelope.

use std::sync::Arc;

use super::{
	edge_child_rebind_matrix_tests::shape_fixture_with_family,
	edge_child_rebind_support::{Source, edge_change, edge_state},
	metagraph::{leaf_roots, prepare_leaf},
	transition_rewire_tests::{portal, rewrite},
	*,
};
use crate::{
	datastore::OccurrenceState,
	storage::{graph_index::with_external_sort_budget, graph_pages::EncodedEndpoint},
};

/// Sync admits a small source and rejects the growing encoded role before copy.
#[test]
fn edge_child_rebind_sync_fixed_role_budget_preserves_source() {
	role_budget(IoMode::Sync);
}

/// NativeAsync restores the unchanged default policy for the passing control.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_child_rebind_native_async_fixed_role_budget_preserves_source() {
	role_budget(IoMode::NativeAsync);
}

/// The wide source's one encoded role alone exceeds the fixed logical ceiling.
fn role_budget(mode: IoMode) {
	role_budget_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn role_budget_with_family(mode: IoMode, family: GraphKind) {
	let maximum = 128 * 1024;
	for wide in [false, true] {
		let (mut fixture, _, _, topology) = shape_fixture_with_family(mode, 4, family);
		// Equal directory counters isolate source role growth at the same
		// fixed envelope, rather than adding unrelated directory output.
		let (_, _, replacement) = super::node_child_rebind_tests::node_owner(&fixture);
		let (_, _, child) = portal(&fixture);
		let topology = if wide {
			let topology = EdgeTopology::directed(std::iter::repeat_n(fixture.nodes[0], 16384), [
				fixture.nodes[1],
			]);
			rewrite(
				&fixture,
				OccurrenceState::edge(topology.clone(), Some(11_i64.into()), Some(child)),
			)
			.commit()
			.assert_ok();
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = reopen(fixture);
			topology
		} else {
			topology
		};
		assert_eq!(
			topology.endpoint_count().checked_mul(size_of::<EncodedEndpoint>()).assert_some()
				> maximum,
			wide
		);
		let source = Source::capture(&fixture);
		let root = fixture.store.inner.current_root();
		let (before, after, delta) =
			leaf_roots(&fixture, source.owner.1, edge_state(&fixture, replacement));
		let store = local(&fixture.store);
		let boundary = store.wal.snapshot().visible_lsn();
		let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
		let result =
			with_external_sort_budget(maximum, || prepare_leaf(store, (&before, &after), &delta));
		if wide {
			let error = result.err().assert_some();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
		} else {
			assert_eq!(result.assert_ok().arenas.len(), 1);
		}
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		source.assert_current(&fixture, source.child, (1, 0, 1, 0));
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		let catalog = state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == fixture.graph.id())
			.assert_some();
		assert!(binding.arena.same_owner(&source.arena));
		assert!(catalog.arena.same_owner(&source.arena));
		drop(state);
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
		// Restore the scoped logical policy; production defaults stay
		// unchanged.
		prepare_leaf(store, (&before, &after), &delta).assert_ok();
		edge_change(&fixture, replacement).commit().assert_ok();
		source.assert_current(&fixture, replacement, (1, 0, 1, 0));
		assert_bulk_image_with_overlays(&fixture, &[source.owner.1]);
	}
}
