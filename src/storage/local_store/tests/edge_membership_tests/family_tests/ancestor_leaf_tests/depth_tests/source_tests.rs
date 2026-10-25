//! T containers and cache owners are validated before the four-owner
//! publication.

use super::*;
use crate::{
	datastore::{DatabaseRoot, PublicationDelta},
	storage::{
		graph_pages::{
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		local_store::{LocalState, LocalStore},
		page::AlignedPage,
	},
};

/// Sync reaches T cache guards at commit and T physical guards at preparation.
#[test]
fn ancestor_depth_sync_false_top_sources_publish_nothing() {
	bad_top_sources(IoMode::Sync);
}

/// NativeAsync uses the same full owner/scope source checks.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_depth_native_async_false_top_sources_publish_nothing() {
	bad_top_sources(IoMode::NativeAsync);
}

/// A successful control must actually prepare T alongside M/R/S.
fn assert_prepared_owners(
	case: &Depth,
	store: &LocalStore,
	roots: (&DatabaseRoot, &DatabaseRoot),
	delta: &PublicationDelta,
) {
	let plan = metagraph::prepare_leaf(store, roots, delta).assert_ok();
	let mut expected = [
		case.routes.fixture.graph.id(),
		case.routes.outer[0].id(),
		case.routes.outer[1].id(),
		case.top.id(),
	];
	expected.sort();
	assert_eq!(plan.arenas.keys().copied().collect::<Vec<_>>(), expected);
}

/// Both wrong slots still name R; only a full container/scope check can reject.
fn top_page_fault(
	case: &Depth,
	state: &LocalState,
	before: &DatabaseRoot,
	edge: bool,
) -> (PageKey, Box<AlignedPage>) {
	let store = local(&case.routes.fixture.store);
	let binding =
		state.bindings.graphs.iter().find(|entry| entry.graph_id == case.top.id()).assert_some();
	let index = usize::from(edge);
	let owner = case.owners[index];
	let slot = binding.arena.direct_child_slot(before, case.top.id(), owner).assert_ok();
	let wrong =
		binding.arena.direct_child_slot(before, case.top.id(), case.owners[index ^ 1]).assert_ok();
	assert_ne!(slot, wrong);
	assert_eq!(binding.arena.graph_by_slot().get(&slot), Some(&case.routes.outer[0].id()));
	assert_eq!(binding.arena.graph_by_slot().get(&slot), binding.arena.graph_by_slot().get(&wrong));
	let (number, within) = hot_slot_address(owner.slot()).assert_ok();
	let file = if edge {
		binding.files.edges
	} else {
		binding.files.nodes
	};
	let key = PageKey::new(binding.files.space(), file, number);
	let original = store.read_update_page(state, key).assert_ok();
	let mut page = store.read_update_page(state, key).assert_ok();
	if edge {
		let mut hot = read_edge_hot_slot(&page, number, within).assert_ok();
		hot.replace_transition(number, Some(slot), Some(wrong)).assert_ok();
		write_edge_hot_slot(&mut page, within, &hot).assert_ok();
	} else {
		let mut hot = read_node_hot_slot(&page, number, within).assert_ok();
		hot.subgraph_slot = Some(wrong);
		write_node_hot_slot(&mut page, within, &hot).assert_ok();
	}
	page.seal().assert_ok();
	super::super::source_tests::stage_source(store, key, &page);
	assert_ne!(store.read_update_page(state, key).assert_ok().as_bytes(), original.as_bytes());
	(key, original)
}

/// Cache faults keep clocks aligned; raw physical injection is
/// preparation-only.
fn bad_top_sources(mode: IoMode) {
	bad_top_sources_with_families(mode, [GraphKind::Metagraph; 3]);
}

/// Graph ancestors use exactly the same four-owner rejection and control gates.
pub(super) fn bad_top_sources_with_families(mode: IoMode, families: [GraphKind; 3]) {
	bad_top_sources_with_parent(mode, families, GraphKind::Metagraph);
}

/// Runs the unchanged ancestor gates for the selected public owner family.
pub(super) fn bad_top_sources_with_parent(
	mode: IoMode,
	families: [GraphKind; 3],
	parent_family: GraphKind,
) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for fault in 0..4 {
			let mut case = depth_with_parent(mode, kind, false, families, parent_family);
			let (before, after, delta) = roots(&case.routes);
			let source = case.routes.fixture.store.inner.current_root();
			let store = local(&case.routes.fixture.store);
			assert_prepared_owners(&case, store, (&before, &after), &delta);
			let mut state = store.state.lock();
			let original_sources = depth_sources(&case, &state);
			let index = state
				.bindings
				.graphs
				.iter()
				.position(|entry| entry.graph_id == case.top.id())
				.assert_some();
			let catalog_index = state
				._database_catalog
				.graphs
				.iter()
				.position(|entry| entry.id == case.top.id())
				.assert_some();
			let accepted = state.bindings.graphs[index].arena.clone();
			let catalog = state._database_catalog.graphs[catalog_index].arena.clone();
			assert!(accepted.same_owner(&catalog));
			let original_page =
				(fault >= 2).then(|| top_page_fault(&case, &state, &before, fault == 3));
			if fault == 0 {
				state.bindings.graphs[index].arena = GraphArenaBindings::new(
					accepted.graph_by_slot().clone(),
					accepted.scope_by_slot().clone(),
				);
			} else if fault == 1 {
				let mut graphs = accepted.graph_by_slot().clone();
				let path = [case.owners[0], case.routes.r_owners[0], case.routes.selected];
				let slot = *accepted
					.scope_by_slot()
					.iter()
					.find(|(_, stored)| stored.as_ref() == path.as_slice())
					.assert_some()
					.0;
				assert_eq!(
					graphs.insert(slot, case.routes.children[1].id()),
					Some(case.routes.children[0].id())
				);
				let forged = GraphArenaBindings::new(graphs, accepted.scope_by_slot().clone());
				state.bindings.graphs[index].arena = forged.clone();
				state._database_catalog.graphs[catalog_index].arena = forged;
			}
			let injected = depth_sources(&case, &state);
			let files = state.file_catalog.files.clone();
			let paths = generation_directory_paths(&case.routes.fixture.directory.0);
			let boundary = store.wal.snapshot().visible_lsn();
			drop(state);
			let plan_error =
				metagraph::prepare_leaf(store, (&before, &after), &delta).err().assert_some();
			let expected = [
				"ancestor leaf catalog arena owner disagrees",
				"ancestor leaf source arena disagrees with canonical scopes",
				"leaf rebind source NODE identity or child slot disagrees",
				"EDGE Hot source disagrees with accepted root",
			][fault];
			assert!(
				matches!(plan_error.status().status(),
					crate::gql_status::Status::InvalidStorageFile { reason } if reason.as_str() == expected),
				"fault {fault}: {plan_error}"
			);
			if original_page.is_none() {
				assert_eq!(store.wal.snapshot().commit_ts().get(), source.commit_ts);
				let commit_error = change(&case.routes).commit().assert_err();
				assert_eq!(plan_error.status().status(), commit_error.status().status());
			}
			assert!(Arc::ptr_eq(&source, &case.routes.fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			let mut state = store.state.lock();
			assert_depth_sources(&state, &injected);
			assert_eq!(state.file_catalog.files, files);
			assert_eq!(generation_directory_paths(&case.routes.fixture.directory.0), paths);
			state.bindings.graphs[index].arena = accepted;
			state._database_catalog.graphs[catalog_index].arena = catalog;
			let preparation_only = original_page.is_some();
			if let Some((key, page)) = original_page {
				super::super::source_tests::stage_source(store, key, &page);
			}
			assert_depth_sources(&state, &original_sources);
			drop(state);
			assert_prepared_owners(&case, store, (&before, &after), &delta);
			if preparation_only {
				eprintln!("ANCESTOR_DEPTH_TOP_PREP_REJECT_PASS {mode:?} {kind:?} fault={fault}");
			} else {
				change(&case.routes).commit().assert_ok();
				final_depth(&case, &original_sources);
				assert_retained(&case.routes.fixture, &files, false);
				drop(source);
				drop(before);
				drop(after);
				case.routes.fixture = reopen(case.routes.fixture);
				final_depth(&case, &original_sources);
				assert_retained(&case.routes.fixture, &files, false);
				eprintln!("ANCESTOR_DEPTH_TOP_COMMIT_REJECT_PASS {mode:?} {kind:?} fault={fault}");
			}
		}
	}
}
