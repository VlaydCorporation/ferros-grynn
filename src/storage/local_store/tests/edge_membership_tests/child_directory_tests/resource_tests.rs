//! Fixed logical admission for source directory validation and retained pages.

use super::{support::*, *};
use crate::storage::graph_index::with_external_sort_budget;

/// Identical child writes separate small and large parent validation at one
/// limit.
#[test]
fn bounded_child_directory_fixed_budget_preserves_publication_boundary() {
	// The preserved discovery series fixes this ceiling before the regression
	// runs.
	let maximum = 266240;
	for (entries, admitted) in [(3, true), (511, false)] {
		let fixture = fixture(IoMode::Sync, entries);
		let store = local(&fixture.store);
		let root = fixture.store.inner.current_root();
		let (before, after, delta) = roots(&fixture, &[(0, 0)]);
		let boundary = store.wal.snapshot().visible_lsn();
		let catalog = store.state.lock().file_catalog.files.clone();
		let generations = generation_directories(&fixture.directory.0);
		let result = with_external_sort_budget(maximum, || prepare(store, &before, &after, &delta));
		if admitted {
			let plan = result.assert_ok();
			assert!(!plan.pages.is_empty());
		} else {
			let error = result.err().assert_some();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
		}
		assert!(Arc::ptr_eq(&root, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.state.lock().file_catalog.files, catalog);
		assert_eq!(generation_directories(&fixture.directory.0), generations);
		// A larger ceiling admits the same source; rejection cannot hide
		// malformed data.
		with_external_sort_budget(512 * 1024, || prepare(store, &before, &after, &delta))
			.assert_ok();
		assert_directories(&fixture);
		// Direct-plan admission is the bounded contract above; ordinary full
		// commit is independently exercised under unchanged production
		// defaults.
		candidate(&fixture, &[(0, 0)]).commit().assert_ok();
		assert_directories(&fixture);
	}
}
