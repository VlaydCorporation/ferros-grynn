//! Mixed preparation validates Base and graph authorities before publication.

use super::*;
use crate::storage::{
	atom_page::{AtomHotPage, atom_location},
	graph_pages::{GraphArenaBindings, decode_subgraph_directory, encode_subgraph_directory},
	local_store::{LocalState, LocalStore, tests::generation_directory_paths},
	page::PageLsn,
};

/// Deliberate source frames advance the WAL clock; these cases are
/// preparation-only.
fn stage_source(store: &LocalStore, key: PageKey, page: &AlignedPage) {
	let mut tx = store.wal.begin_tx().assert_ok();
	tx.stage_page(key, page).assert_ok();
	store.wal.commit_tx(tx).assert_ok();
}

/// A legal Hot encoding carries the wrong full generation for the Base owner.
fn false_base_source(case: &Case, state: &LocalState) -> (PageKey, Box<AlignedPage>) {
	let store = local(&case.fixture.store);
	let (number, within) = atom_location(case.global.slot());
	let key = PageKey::new(DataSpaceId::Database, state.bindings.atoms.atoms_hot, number);
	let original = store.read_update_page(state, key).assert_ok();
	let mut hot =
		AtomHotPage::from_page(store.read_update_page(state, key).assert_ok(), number).assert_ok();
	let (id, slot) = hot.read(within).assert_ok();
	assert_eq!(id, case.global);
	let stale = GlobalAtomId::from_raw(
		AtomId::from_parts(
			case.global.kind().assert_some(),
			case.global.slot(),
			case.global.generation() + 1,
		)
		.assert_ok()
		.raw(),
	);
	hot.write(stale, slot).assert_ok();
	stage_source(store, key, &hot.finish());
	(key, original)
}

/// A checksummed directory disagrees with the already published child ceiling.
fn false_directory_source(case: &Case, state: &LocalState) -> (PageKey, Box<AlignedPage>) {
	let store = local(&case.fixture.store);
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == case.fixture.graph.id())
		.assert_some();
	let root = case.fixture.store.inner.current_root();
	let slot =
		binding.arena.direct_child_slot(&root, case.fixture.graph.id(), case.selected).assert_ok();
	let number = PageNo::new(0);
	let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
	let original = store.read_update_page(state, key).assert_ok();
	let (next, mut entries) = decode_subgraph_directory(&original, number).assert_ok();
	entries.iter_mut().find(|entry| entry.slot == slot).assert_some().node_capacity += 1;
	let page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries).assert_ok();
	stage_source(store, key, &page);
	(key, original)
}

/// Failed preparation preserves injected authorities until the test restores
/// them.
fn malformed_sources(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for fault in 0..3 {
				let case = case_fixture(mode, family, kind);
				let (before, mut after, delta) = publication_roots(&case, true, true);
				assert!(prepare_publication(&case, (&before, &after), &delta).assert_ok());
				let published = case.fixture.store.inner.current_root();
				let accepted = source_arena(&case);
				let prefix = property_prefix(&case);
				let store = local(&case.fixture.store);
				let mut state = store.state.lock();
				let index = state
					.bindings
					.graphs
					.iter()
					.position(|entry| entry.graph_id == case.fixture.graph.id())
					.assert_some();
				let catalog_index = state
					._database_catalog
					.graphs
					.iter()
					.position(|entry| entry.id == case.fixture.graph.id())
					.assert_some();
				let original = match fault {
					0 => Some(false_base_source(&case, &state)),
					1 => Some(false_directory_source(&case, &state)),
					_ => {
						state.bindings.graphs[index].arena = GraphArenaBindings::new(
							accepted.graph_by_slot().clone(),
							accepted.scope_by_slot().clone(),
						);
						None
					}
				};
				let injected = state.bindings.graphs[index].arena.clone();
				let catalog = state._database_catalog.graphs[catalog_index].arena.clone();
				let files = state.file_catalog.files.clone();
				let boundary = store.wal.snapshot().visible_lsn();
				let paths = generation_directory_paths(&case.fixture.directory.0);
				drop(state);
				after.commit_ts = store.wal.snapshot().commit_ts().get() + 1;
				let error = prepare_publication(&case, (&before, &after), &delta).assert_err();
				assert!(
					matches!(
						error.status().status(),
						crate::gql_status::Status::InvalidStorageFile { .. }
							| crate::gql_status::Status::InvalidStoragePage { .. }
					),
					"fault={fault}: {error}"
				);
				if fault == 2 {
					let error = change(&case, 0).commit().assert_err();
					assert!(
						matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason } if reason.as_str() == "leaf rebind source catalog arena owner disagrees")
					);
				}
				assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
				assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
				assert_eq!(generation_directory_paths(&case.fixture.directory.0), paths);
				let mut state = store.state.lock();
				assert_eq!(state.file_catalog.files, files);
				assert!(state.bindings.graphs[index].arena.same_owner(&injected));
				assert!(state._database_catalog.graphs[catalog_index].arena.same_owner(&catalog));
				state.bindings.graphs[index].arena = accepted;
				if let Some((key, page)) = original {
					stage_source(store, key, &page);
				}
				drop(state);
				assert_prefix(&case, &prefix);
				after.commit_ts = store.wal.snapshot().commit_ts().get() + 1;
				assert!(prepare_publication(&case, (&before, &after), &delta).assert_ok());
				if fault == 2 {
					change(&case, 0).commit().assert_ok();
					assert_current(&case, 0);
					assert_files(&case, &files);
				}
				eprintln!(
					"MIXED_LEAF_BASE_SOURCE_REJECT_PASS {mode:?} {family:?} {kind:?} fault={fault}"
				);
			}
		}
	}
}

/// Sync rejects both physical sources and reaches the actual cache-owner guard.
#[test]
fn mixed_leaf_base_sync_invalid_sources_preserve_publication() {
	malformed_sources(IoMode::Sync);
}

/// NativeAsync restores a passing complete plan after each independent fault.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_invalid_sources_preserve_publication() {
	malformed_sources(IoMode::NativeAsync);
}
