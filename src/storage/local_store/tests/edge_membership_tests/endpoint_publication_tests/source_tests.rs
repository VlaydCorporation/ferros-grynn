//! Malformed retained endpoint headers cannot be hidden by selected
//! replacement.

use super::*;
use crate::storage::local_store::{
	layout::LayoutBuilder,
	point_endpoint::append_selected_indexes,
	point_namespace::PublicationBudget,
};

/// Valid-checksum identity/revision/extent faults reject before output
/// creation.
#[test]
fn endpoint_selected_sync_malformed_header_publishes_nothing() {
	malformed_header(IoMode::Sync);
}

/// NativeAsync observes the same checked old header at the accepted WAL
/// boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_malformed_header_publishes_nothing() {
	malformed_header(IoMode::NativeAsync);
}

/// A private selected build drops its cleanup guard without publishing
/// anything.
fn prepare_only(
	fixture: &Fixture,
	after: &crate::datastore::DatabaseRoot,
) -> FerrosGrynnResult<()> {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let mut budget = PublicationBudget::new();
	let mut layout = LayoutBuilder::new(state.file_catalog.next_file_instance_id);
	let mut cleanup = None;
	let mut retired = BTreeSet::new();
	let generation = crate::values::Uuid::now_v7().to_string();
	append_selected_indexes(
		store,
		&state,
		(after, None),
		&BTreeSet::from([fixture.graph.id()]),
		&generation,
		&mut layout,
		&mut cleanup,
		&mut retired,
		&mut budget,
	)?;
	Ok(())
}

/// Direct preparation isolates deliberately injected WAL/logical clock skew.
fn malformed_header(io_mode: IoMode) {
	for fault in 0..6 {
		let (fixture, _, _) = endpoint_fixture(io_mode);
		let source = fixture.store.inner.current_root();
		let store = local(&fixture.store);
		let mut after = source.as_ref().clone();
		after.commit_ts = store.wal.snapshot().commit_ts().get() + 1;
		let files = store.state.lock().file_catalog.files.clone();
		let directories = generation_directory_paths(&fixture.directory.0);
		let original_boundary = store.wal.snapshot().visible_lsn();
		prepare_only(&fixture, &after).assert_ok();
		assert!(Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), original_boundary);
		assert_eq!(store.state.lock().file_catalog.files, files);
		assert_eq!(generation_directory_paths(&fixture.directory.0), directories);
		let state = store.state.lock();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.edge_endpoint
			.assert_some();
		let key =
			PageKey::new(DataSpaceId::Graph(fixture.graph.id()), binding.file_id, binding.root);
		let mut page = store.read_update_page(&state, key).assert_ok();
		let next_revision = store.wal.snapshot().commit_ts().get() + 1;
		page.as_bytes_mut()[48..56].copy_from_slice(&next_revision.to_le_bytes());
		match fault {
			0 => page.as_bytes_mut()[32..36].copy_from_slice(&0_u32.to_le_bytes()),
			1 => page.as_bytes_mut()[36..40].copy_from_slice(&0_u32.to_le_bytes()),
			2 => page.as_bytes_mut()[40..48].copy_from_slice(&0_u64.to_le_bytes()),
			3 => page.as_bytes_mut()[48..56].copy_from_slice(&0_u64.to_le_bytes()),
			4 => page.as_bytes_mut()[64..72].copy_from_slice(&1_u64.to_le_bytes()),
			_ => page.as_bytes_mut()[80] = 1,
		}
		page.seal().assert_ok();
		let mut injection = store.wal.begin_tx().assert_ok();
		injection.stage_page(key, &page).assert_ok();
		store.wal.commit_tx(injection).assert_ok();
		assert_eq!(store.wal.snapshot().commit_ts().get(), next_revision);
		let boundary = store.wal.snapshot().visible_lsn();
		assert_eq!(state.file_catalog.files, files);
		drop(state);
		assert_eq!(generation_directory_paths(&fixture.directory.0), directories);
		after.commit_ts = next_revision + 1;
		let error = prepare_only(&fixture, &after).assert_err();
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
			if reason.as_str() == "edge endpoint header disagrees with its exact snapshot")
		);
		assert!(Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.state.lock().file_catalog.files, files);
		assert_eq!(generation_directory_paths(&fixture.directory.0), directories);
	}
}
