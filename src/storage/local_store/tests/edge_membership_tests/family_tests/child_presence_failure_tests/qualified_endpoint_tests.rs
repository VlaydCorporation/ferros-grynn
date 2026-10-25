//! Coupled preparation remains separate from injected-WAL commit/recovery.

use std::path::{Path, PathBuf};

use super::*;
use crate::{
	datastore::{DatabaseRoot, PublicationDelta},
	gql_status::FerrosGrynnResult,
	storage::local_store::{
		point_endpoint::EndpointRevisionPlan,
		point_index::PointIndexPlan,
		point_namespace::LabelPlan,
		point_update::PointUpdatePlan,
	},
};

/// Sync validates every old header before selected output can exist.
#[test]
fn qualified_edge_endpoint_sync_malformed_headers_preserve_complete_source() {
	headers(IoMode::Sync);
}

/// NativeAsync retains source identity and the same preparation boundary.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_endpoint_native_async_malformed_headers_preserve_complete_source() {
	headers(IoMode::NativeAsync);
}

/// Uses production graph-to-index arena handoff but never stages or commits.
/// Returns the exact graph/endpoint selection for the caller's assertion.
///
/// # Errors
/// Preserves source, admission, codec and I/O failures before publication.
fn prepare(
	case: &Case,
	roots: (&DatabaseRoot, &DatabaseRoot),
	delta: &PublicationDelta,
) -> FerrosGrynnResult<bool> {
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	let mut labels = LabelPlan::build(store, &state, roots.0, roots.1, delta)?;
	let mut context = crate::storage::atom_store_image::value_context(
		&roots.1.schema,
		roots.1.dictionary.as_ref().clone(),
	)?;
	let point =
		PointUpdatePlan::build(store, &state, roots.0, roots.1, delta, &mut labels, &mut context)?;
	let indexes = PointIndexPlan::build(
		store,
		&state,
		roots.0,
		roots.1,
		delta,
		Some(&point.graph_arenas),
		&mut labels,
		&mut context,
	)?
	.assert_some();
	EndpointRevisionPlan::build(
		store,
		&state,
		roots.1,
		&mut labels.budget,
		Some(&indexes.endpoints),
	)?;
	// Dropping the armed cleanup guard removes only private selected output.
	Ok(point.graph_arenas.contains_key(&case.fixture.graph.id())
		&& indexes.endpoints.len() == 1
		&& indexes.endpoints.contains_key(&case.fixture.graph.id()))
}

/// The current accepted catalog supplies the exact canonical endpoint path.
pub(super) fn endpoint_path(case: &Case) -> PathBuf {
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == case.fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let entry = state
		.file_catalog
		.files
		.iter()
		.find(|entry| {
			entry.space_id == crate::datastore::DataSpaceId::Graph(case.fixture.graph.id())
				&& entry.file_id == binding.file_id
		})
		.assert_some();
	case.fixture.directory.0.join(entry.path.as_str())
}

/// Recovery with no physical pins must reclaim even a retired file in a live
/// directory.
pub(super) fn recovered(case: &Case, old: &Path, accepted: bool) {
	let current = endpoint_path(case);
	assert!(current.is_file());
	assert_eq!(current == old, !accepted);
	assert_eq!(old.is_file(), !accepted);
	assert_eq!(
		super::super::super::super::generation_directory_paths(&case.fixture.directory.0),
		super::super::super::super::catalog_generation_directory_paths(
			&case.fixture.directory.0,
			&case.fixture.store
		)
	);
}

/// A measured positive graph control distinguishes index admission from
/// whole-operation rejection.
pub(super) fn resources(mode: IoMode) {
	resources_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn resources_with_family(mode: IoMode, family: GraphKind) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let mut case = Case::qualified_edge_with_family(mode, kind, attach, family);
			let normal = case.fixture.options.maintenance.max_memory_bytes;
			case.fixture.options.maintenance.max_memory_bytes =
				std::num::NonZeroUsize::new(1024 * 1024).assert_some();
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
				1024 * 1024
			);
			case.check(false);
			let root = case.fixture.store.inner.current_root();
			let store = local(&case.fixture.store);
			let files = store.state.lock().file_catalog.files.clone();
			let directories =
				super::super::super::super::generation_directory_paths(&case.fixture.directory.0);
			let boundary = store.wal.snapshot().visible_lsn();
			let arena = case.arena();
			let pages = case.all_pages();
			let (before, after, delta) =
				leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
			with_external_sort_budget(1024 * 1024, || {
				prepare_leaf(store, (&before, &after), &delta)?;
				Ok(())
			})
			.assert_ok();
			let error = with_external_sort_budget(1024 * 1024, || {
				prepare(&case, (&before, &after), &delta)
			})
			.assert_err();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			let error = case.change().commit().assert_err();
			assert!(matches!(
				error.status().status(),
				crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
			));
			assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
			assert_eq!(store.state.lock().file_catalog.files, files);
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(
				super::super::super::super::generation_directory_paths(&case.fixture.directory.0),
				directories
			);
			case.assert_owners(&arena);
			case.assert_pages(&pages);
			case.check(false);
			drop(root);
			drop(before);
			drop(after);
			case.fixture.options.maintenance.max_memory_bytes = normal;
			case.fixture = reopen(case.fixture);
			assert_eq!(
				case.fixture
					.store
					.local_storage_status()
					.assert_some()
					.options()
					.maintenance
					.max_memory_bytes,
				normal
			);
			case.change().commit().assert_ok();
			case.check(true);
			assert_retained(&case.fixture, &files, false);
			case.fixture = reopen(case.fixture);
			case.check(true);
			eprintln!("QUALIFIED_EDGE_ENDPOINT_BUDGET_PASS {mode:?} {kind:?} {attach}");
		}
	}
}

/// Each fresh fixture isolates raw WAL clock skew from genuine commit/recovery
/// tests.
fn headers(mode: IoMode) {
	headers_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn headers_with_family(mode: IoMode, family: GraphKind) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			for fault in 0..6 {
				let case = Case::qualified_edge_with_family(mode, kind, attach, family);
				let published = case.fixture.store.inner.current_root();
				let arena = case.arena();
				let store = local(&case.fixture.store);
				let (before, mut after, delta) =
					leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
				after.commit_ts = store.wal.snapshot().commit_ts().get() + 1;
				let files = store.state.lock().file_catalog.files.clone();
				let directories = super::super::super::super::generation_directory_paths(
					&case.fixture.directory.0,
				);
				let boundary = store.wal.snapshot().visible_lsn();
				let pages = case.all_pages();
				assert!(prepare(&case, (&before, &after), &delta).assert_ok());
				case.assert_pages(&pages);
				case.assert_owners(&arena);
				assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
				assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
				assert_eq!(store.state.lock().file_catalog.files, files);
				assert_eq!(
					super::super::super::super::generation_directory_paths(
						&case.fixture.directory.0
					),
					directories
				);
				let state = store.state.lock();
				let binding = state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == case.fixture.graph.id())
					.assert_some()
					.edge_endpoint
					.assert_some();
				let key = PageKey::new(
					crate::datastore::DataSpaceId::Graph(case.fixture.graph.id()),
					binding.file_id,
					binding.root,
				);
				let mut page = store.read_update_page(&state, key).assert_ok();
				let next_revision = store.wal.snapshot().commit_ts().get() + 1;
				page.as_bytes_mut()[48..56].copy_from_slice(&next_revision.to_le_bytes());
				match fault {
					0 => page.as_bytes_mut()[32..36].copy_from_slice(&0_u32.to_le_bytes()),
					1 => page.as_bytes_mut()[36..40].copy_from_slice(&0_u32.to_le_bytes()),
					2 => page.as_bytes_mut()[40..48].copy_from_slice(&0_u64.to_le_bytes()),
					3 => page.as_bytes_mut()[48..56].copy_from_slice(&0_u64.to_le_bytes()),
					4 => page.as_bytes_mut()[64..72].copy_from_slice(&1_u64.to_le_bytes()),
					_ => {
						assert_eq!(fault, 5);
						page.as_bytes_mut()[80] = 1;
					}
				}
				page.seal().assert_ok();
				drop(state);
				source_tests::stage(store, key, &page);
				assert_eq!(store.wal.snapshot().commit_ts().get(), next_revision);
				let boundary = store.wal.snapshot().visible_lsn();
				let forged = case.all_pages();
				after.commit_ts = next_revision + 1;
				let error = prepare(&case, (&before, &after), &delta).assert_err();
				assert!(
					matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
					if reason.as_str() == "edge endpoint header disagrees with its exact snapshot")
				);
				assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
				assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
				assert_eq!(store.state.lock().file_catalog.files, files);
				assert_eq!(
					super::super::super::super::generation_directory_paths(
						&case.fixture.directory.0
					),
					directories
				);
				case.assert_owners(&arena);
				case.assert_pages(&forged);
				eprintln!(
					"QUALIFIED_EDGE_ENDPOINT_HEADER_FAULT_PASS {mode:?} {kind:?} {attach} {fault}"
				);
			}
		}
	}
}
