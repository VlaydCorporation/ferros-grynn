//! Validly encoded source faults cannot become a child rebind fallback.

use std::sync::Arc;

use super::{
	metagraph::with_replacement,
	node_child_rebind_tests::{node_owner, node_roots},
	*,
};
use crate::storage::{
	graph_pages::{
		GraphArenaBindings,
		decode_subgraph_directory,
		encode_subgraph_directory,
		read_node_hot_slot,
		write_node_hot_slot,
	},
	local_store::{point_graph::GraphUpdatePlan, point_namespace::LabelPlan},
	page::{AlignedPage, PageLsn},
};

/// Sync checks source authorities before any candidate can publish.
#[test]
fn node_child_rebind_sync_malformed_sources_preserve_publication_boundary() {
	bad_sources(IoMode::Sync);
}

/// NativeAsync reaches a valid control before and after each independent fault.
#[cfg(feature = "native-async-io")]
#[test]
fn node_child_rebind_native_async_malformed_sources_preserve_publication_boundary() {
	bad_sources(IoMode::NativeAsync);
}

/// Separate source frames carry a deliberate semantic fault with a valid
/// checksum.
fn stage_source(store: &crate::storage::local_store::LocalStore, key: PageKey, page: &AlignedPage) {
	let mut tx = store.wal.begin_tx().assert_ok();
	tx.stage_page(key, page).assert_ok();
	store.wal.commit_tx(tx).assert_ok();
}

/// Both catalog and binding share forged owners to exercise deeper validators.
fn forged_arena(
	arena: &GraphArenaBindings,
	owner: AtomId,
	root: crate::graph::GraphId,
	fault: usize,
) -> GraphArenaBindings {
	let mut graphs = arena.graph_by_slot().clone();
	let mut scopes = arena.scope_by_slot().clone();
	let slot = *scopes.iter().find(|(_, scope)| scope.as_ref() == [owner]).assert_some().0;
	match fault {
		1 => {
			let stale = AtomId::from_parts(AtomKind::Node, owner.slot(), owner.generation() + 1)
				.assert_ok();
			scopes.insert(slot, vec![stale].into_boxed_slice());
		}
		2 => {
			graphs.insert(slot, root);
		}
		3 => {
			let next = u32::try_from(graphs.len()).assert_ok();
			let child = *graphs.get(&slot).assert_some();
			graphs.insert(next, child);
			scopes.insert(next, vec![owner, owner].into_boxed_slice());
		}
		_ => {
			let child = graphs.remove(&slot).assert_some();
			let scope = scopes.remove(&slot).assert_some();
			graphs.insert(slot + 100, child);
			scopes.insert(slot + 100, scope);
		}
	}
	GraphArenaBindings::new(graphs, scopes)
}

/// Malformed Hot payloads remain legal page encodings to reach source
/// validation.
fn hot_fault(
	store: &crate::storage::local_store::LocalStore,
	state: &crate::storage::local_store::LocalState,
	files: crate::storage::graph_store::GraphFileSet,
	owner: AtomId,
	other: crate::datastore::GlobalAtomId,
	wrong_slot: u32,
	fault: usize,
) -> (PageKey, Box<AlignedPage>) {
	let (number, within) = hot_slot_address(owner.slot()).assert_ok();
	let key = PageKey::new(files.space(), files.nodes, number);
	let original = store.read_update_page(state, key).assert_ok();
	let mut page = store.read_update_page(state, key).assert_ok();
	let mut hot = read_node_hot_slot(&page, number, within).assert_ok();
	match fault {
		5 => {
			hot.generation += 1;
		}
		6 => {
			hot.global_atom = Some(other);
		}
		7 => {
			hot.subgraph_slot = Some(wrong_slot);
		}
		_ => {
			hot.replace_label(number, None, Some(123)).assert_ok();
		}
	}
	write_node_hot_slot(&mut page, within, &hot).assert_ok();
	page.seal().assert_ok();
	stage_source(store, key, &page);
	assert_eq!(
		read_node_hot_slot(&store.read_update_page(state, key).assert_ok(), number, within)
			.assert_ok(),
		hot
	);
	(key, original)
}

/// Source counters, links and completeness are checked for all direct entries.
fn directory_fault(
	store: &crate::storage::local_store::LocalStore,
	state: &crate::storage::local_store::LocalState,
	files: crate::storage::graph_store::GraphFileSet,
	slot: u32,
	fault: usize,
) -> (PageKey, Box<AlignedPage>) {
	let number = PageNo::new(0);
	let key = PageKey::new(files.space(), files.subgraphs, number);
	let original = store.read_update_page(state, key).assert_ok();
	let (mut next, mut entries) = decode_subgraph_directory(&original, number).assert_ok();
	match fault {
		9 => {
			entries.iter_mut().find(|entry| entry.slot == slot).assert_some().node_capacity += 1;
		}
		10 => {
			entries.iter_mut().find(|entry| entry.slot != slot).assert_some().node_capacity += 1;
		}
		11 => {
			next = Some(PageNo::new(1));
		}
		_ => {
			entries.retain(|entry| entry.slot != slot);
		}
	}
	let page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries).assert_ok();
	stage_source(store, key, &page);
	assert_eq!(
		decode_subgraph_directory(&store.read_update_page(state, key).assert_ok(), number)
			.assert_ok(),
		(next, entries)
	);
	(key, original)
}

/// Each fault owns an isolated fixture and restores a passing direct-plan
/// control.
fn bad_sources(mode: IoMode) {
	bad_sources_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn bad_sources_with_family(mode: IoMode, family: GraphKind) {
	for fault in 0..14 {
		let (fixture, _) = super::metagraph::container_fixture(mode, family);
		let (fixture, replacement) = with_replacement(fixture);
		let (_, owner, _) = node_owner(&fixture);
		let source = fixture.store.inner.current_root();
		let (before, after, delta) = node_roots(&fixture, replacement);
		let store = local(&fixture.store);
		super::metagraph::prepare_leaf(store, (&before, &after), &delta).assert_ok();
		let mut state = store.state.lock();
		let index = state
			.bindings
			.graphs
			.iter()
			.position(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		let catalog_index = state
			._database_catalog
			.graphs
			.iter()
			.position(|entry| entry.id == fixture.graph.id())
			.assert_some();
		let accepted = state.bindings.graphs[index].arena.clone();
		let catalog_arena = state._database_catalog.graphs[catalog_index].arena.clone();
		assert!(accepted.same_owner(&catalog_arena));
		let files = state.bindings.graphs[index].files;
		let slot = accepted.direct_child_slot(&before, fixture.graph.id(), owner).assert_ok();
		let mut original_page = None;
		let replacement_header = *state.graph_superblocks.get(&replacement).assert_some();
		match fault {
			0 => {
				state.bindings.graphs[index].arena = GraphArenaBindings::new(
					accepted.graph_by_slot().clone(),
					accepted.scope_by_slot().clone(),
				);
			}
			1..=4 => {
				let forged = forged_arena(&accepted, owner, fixture.graph.id(), fault);
				state.bindings.graphs[index].arena = forged.clone();
				state._database_catalog.graphs[catalog_index].arena = forged;
			}
			5..=8 => {
				let wrong = *accepted
					.graph_by_slot()
					.keys()
					.find(|candidate| **candidate != 0 && **candidate != slot)
					.assert_some();
				let other = before.resolve_atom(&"node:a".parse().assert_ok()).assert_some();
				assert_eq!(other.kind(), Some(AtomKind::Node));
				assert_ne!(
					other,
					before.occurrence(fixture.graph.id(), owner).assert_some().global_id()
				);
				original_page = Some(hot_fault(store, &state, files, owner, other, wrong, fault));
			}
			9..=12 => {
				original_page = Some(directory_fault(store, &state, files, slot, fault));
			}
			_ => {
				state.graph_superblocks.get_mut(&replacement).assert_some().node_count += 1;
			}
		}
		let injected_binding = state.bindings.graphs[index].arena.clone();
		let injected_catalog = state._database_catalog.graphs[catalog_index].arena.clone();
		let boundary = store.wal.snapshot().visible_lsn();
		let catalog_files = state.file_catalog.files.clone();
		let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
		let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)
		.assert_ok();
		let error = GraphUpdatePlan::build(
			store,
			&state,
			&before,
			&after,
			&delta,
			&mut labels,
			&mut context,
		)
		.err()
		.assert_some();
		assert!(
			matches!(
				error.status().status(),
				crate::gql_status::Status::InvalidStorageFile { .. }
					| crate::gql_status::Status::InvalidStoragePage { .. }
			),
			"fault {fault}: {error}"
		);
		assert!(Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(state.file_catalog.files, catalog_files);
		assert!(state.bindings.graphs[index].arena.same_owner(&injected_binding));
		assert!(state._database_catalog.graphs[catalog_index].arena.same_owner(&injected_catalog));
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
		state.bindings.graphs[index].arena = accepted;
		state._database_catalog.graphs[catalog_index].arena = catalog_arena;
		state.graph_superblocks.insert(replacement, replacement_header);
		if let Some((key, page)) = original_page {
			stage_source(store, key, &page);
		}
		drop(state);
		super::metagraph::prepare_leaf(store, (&before, &after), &delta).assert_ok();
	}
}
