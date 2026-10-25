//! Cumulative two-Base/graph admission uses the complete production plan
//! sequence.

use super::*;
use crate::{
	datastore::{DatabaseRoot, PublicationDelta},
	gql_status::Status,
	storage::{
		graph_index::with_external_sort_budget,
		local_store::{point_update::PointUpdatePlan, tests::generation_directory_paths},
	},
};

/// Search measures admission, including temporary peaks, rather than guessing
/// it.
fn minimum(case: &Case, roots: (&DatabaseRoot, &DatabaseRoot), delta: &PublicationDelta) -> usize {
	let mut low = 1;
	let mut high = 1024 * 1024;
	assert!(
		with_external_sort_budget(high, || prepare_publication(case, roots, delta)).assert_ok()
	);
	while low < high {
		let middle = low + (high - low) / 2;
		match with_external_sort_budget(middle, || prepare_publication(case, roots, delta)) {
			Ok(selected) => {
				assert!(selected);
				high = middle;
			}
			Err(error) => {
				assert_limit(&error, middle);
				low = middle + 1;
			}
		}
	}
	low
}

/// Allocation or source failures cannot masquerade as budget rejection.
fn assert_limit(error: &crate::gql_status::FerrosGrynnError, limit: usize) {
	assert!(
		matches!(error.status().status(), Status::StorageDecodeLimitExceeded { actual, maximum, .. } if actual > maximum && *maximum == limit as u64)
	);
}

/// Each separate complete plan fits at the same ceiling that rejects their sum.
fn resources(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let mut case = case_fixture(mode, family, kind);
			let defaults = case.fixture.options.maintenance.max_memory_bytes;
			let other = companion(&case);
			let base = batch_roots(&case, &other, false);
			let leaf = publication_roots(&case, false, true);
			let both = batch_roots(&case, &other, true);
			for (before, after, delta) in [&base, &leaf, &both] {
				assert!(PointUpdatePlan::eligible(before, after, delta).assert_ok());
			}
			let limit = minimum(&case, (&both.0, &both.1), &both.2) - 1;
			for (before, after, delta) in [&base, &leaf] {
				assert!(
					with_external_sort_budget(limit, || prepare_publication(
						&case,
						(before, after),
						delta
					))
					.assert_ok()
				);
			}
			let error = with_external_sort_budget(limit, || {
				prepare_publication(&case, (&both.0, &both.1), &both.2)
			})
			.assert_err();
			assert_limit(&error, limit);
			drop((base, leaf, both));
			case.fixture.options.maintenance.max_memory_bytes =
				std::num::NonZeroUsize::new(limit).assert_some();
			case.fixture = reopen(case.fixture);
			assert_eq!(
				case.fixture
					.store
					.local_storage_status()
					.assert_some()
					.options()
					.maintenance
					.max_memory_bytes
					.get(),
				limit
			);
			assert_batch_old(&case, &other);
			let published = case.fixture.store.inner.current_root();
			let arena = source_arena(&case);
			let prefix = property_prefix(&case);
			let store = local(&case.fixture.store);
			let files = store.state.lock().file_catalog.files.clone();
			let paths = generation_directory_paths(&case.fixture.directory.0);
			let boundary = store.wal.snapshot().visible_lsn();
			let error = batch_change(&case, &other, 0).commit().assert_err();
			assert_limit(&error, limit);
			assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(generation_directory_paths(&case.fixture.directory.0), paths);
			assert_source_arena(&case, &arena);
			assert_prefix(&case, &prefix);
			assert_batch_old(&case, &other);
			drop(published);
			case.fixture = reopen(case.fixture);
			assert_batch_old(&case, &other);
			assert_files(&case, &files);
			case.fixture.options.maintenance.max_memory_bytes = defaults;
			case.fixture = reopen(case.fixture);
			batch_change(&case, &other, 0).commit().assert_ok();
			assert_batch_current(&case, &other, 0, case.children[1]);
			assert_files(&case, &files);
			assert_prefix(&case, &prefix);
			case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			case.fixture = reopen(case.fixture);
			assert_batch_current(&case, &other, 0, case.children[1]);
			assert_files(&case, &files);
			assert_prefix(&case, &prefix);
			// Stable scalar/Count output charges remain a separate D7 gate.
			eprintln!(
				"MIXED_BASE_BATCH_CUMULATIVE_RESOURCE_PASS {mode:?} {family:?} {kind:?} \
				 maximum={limit}"
			);
		}
	}
}

/// Sync observes the opened policy and restores ordinary defaults for success.
#[test]
fn mixed_base_batch_sync_cumulative_resource_rejection() {
	resources(IoMode::Sync);
}

/// NativeAsync checks the same complete plans and cold recovery boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_base_batch_native_async_cumulative_resource_rejection() {
	resources(IoMode::NativeAsync);
}
