//! Canonical caches and checksum-valid parent pages must reject before WAL.

use super::*;
use crate::{
	datastore::DatabaseRoot,
	storage::{
		graph_pages::{
			encode_subgraph_directory,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		local_store::{
			LocalState,
			LocalStore,
			point_graph::GraphUpdatePlan,
			point_namespace::LabelPlan,
			tests::generation_directory_paths,
		},
		page::AlignedPage,
	},
};

/// Sync rejects false cache owners at commit and false pages at preparation.
#[test]
fn ancestor_leaf_sync_false_sources_publish_nothing() {
	bad_sources(IoMode::Sync);
}

/// NativeAsync keeps the same cache-commit and physical-preparation boundaries.
#[cfg(feature = "native-async-io")]
#[test]
fn ancestor_leaf_native_async_false_sources_publish_nothing() {
	bad_sources(IoMode::NativeAsync);
}

/// Raw WAL injection exercises preparation only: it advances the WAL clock
/// without publishing a matching logical root, so an actual commit is invalid.
pub(super) fn stage_source(store: &LocalStore, key: PageKey, page: &AlignedPage) {
	let mut tx = store.wal.begin_tx().assert_ok();
	tx.stage_page(key, page).assert_ok();
	store.wal.commit_tx(tx).assert_ok();
}

/// Source faults preserve page framing and checksum to reach semantic checks.
fn page_fault(
	routes: &Fanout,
	state: &LocalState,
	before: &DatabaseRoot,
	fault: usize,
) -> (PageKey, Box<AlignedPage>) {
	let store = local(&routes.fixture.store);
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == routes.outer[0].id())
		.assert_some();
	let local = routes.r_owners[usize::from((6..8).contains(&fault))];
	let slot = binding.arena.direct_child_slot(before, binding.graph_id, local).assert_ok();
	let (number, within) = if fault < 8 {
		hot_slot_address(local.slot()).assert_ok()
	} else {
		(PageNo::new(0), 0)
	};
	let file = if fault < 6 {
		binding.files.nodes
	} else if fault < 8 {
		binding.files.edges
	} else {
		binding.files.subgraphs
	};
	let key = PageKey::new(binding.files.space(), file, number);
	let original = store.read_update_page(state, key).assert_ok();
	let mut page = store.read_update_page(state, key).assert_ok();
	if fault < 6 {
		let mut hot = read_node_hot_slot(&page, number, within).assert_ok();
		if fault == 4 {
			hot.generation += 1;
		} else {
			hot.subgraph_slot = Some(
				binding
					.arena
					.direct_child_slot(before, binding.graph_id, routes.r_owners[2])
					.assert_ok(),
			);
		}
		write_node_hot_slot(&mut page, within, &hot).assert_ok();
	} else if fault < 8 {
		let mut hot = read_edge_hot_slot(&page, number, within).assert_ok();
		if fault == 6 {
			hot.generation += 1;
		} else {
			let wrong = binding
				.arena
				.direct_child_slot(before, binding.graph_id, routes.r_owners[2])
				.assert_ok();
			hot.replace_transition(number, Some(slot), Some(wrong)).assert_ok();
		}
		write_edge_hot_slot(&mut page, within, &hot).assert_ok();
	} else {
		let (next, mut entries) = decode_subgraph_directory(&page, number).assert_ok();
		if fault == 8 {
			entries.iter_mut().find(|entry| entry.slot == slot).assert_some().node_capacity += 1;
		} else {
			entries.retain(|entry| entry.slot != slot);
		}
		*page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries).assert_ok();
	}
	page.seal().assert_ok();
	stage_source(store, key, &page);
	assert_ne!(store.read_update_page(state, key).assert_ok().as_bytes(), original.as_bytes());
	(key, original)
}

/// Value-equal foreign owners and false full paths exercise distinct guards.
fn bad_sources(mode: IoMode) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for fault in 0..12 {
			let routes = fanout(mode, kind, false);
			let (before, after, delta) = roots(&routes);
			let source = routes.fixture.store.inner.current_root();
			let store = local(&routes.fixture.store);
			assert_eq!(
				metagraph::prepare_leaf(store, (&before, &after), &delta).assert_ok().arenas.len(),
				3
			);
			let mut state = store.state.lock();
			let graph = if fault == 3 || fault == 10 {
				routes.outer[1].id()
			} else if fault == 11 {
				routes.fixture.graph.id()
			} else {
				routes.outer[0].id()
			};
			let index = state
				.bindings
				.graphs
				.iter()
				.position(|entry| entry.graph_id == graph)
				.assert_some();
			let catalog_index = state
				._database_catalog
				.graphs
				.iter()
				.position(|entry| entry.id == graph)
				.assert_some();
			let accepted = state.bindings.graphs[index].arena.clone();
			let catalog = state._database_catalog.graphs[catalog_index].arena.clone();
			assert!(accepted.same_owner(&catalog));
			let original_sources = arena_sources(&routes, &state);
			let original_page =
				(4..10).contains(&fault).then(|| page_fault(&routes, &state, &before, fault));
			if fault == 0 || fault == 10 {
				state.bindings.graphs[index].arena = GraphArenaBindings::new(
					accepted.graph_by_slot().clone(),
					accepted.scope_by_slot().clone(),
				);
			} else if fault < 4 || fault == 11 {
				let mut graphs = accepted.graph_by_slot().clone();
				let mut scopes = accepted.scope_by_slot().clone();
				let path = if fault == 3 {
					vec![routes.s_owner, routes.selected]
				} else {
					vec![routes.r_owners[0], routes.selected]
				};
				let slot = if fault == 11 {
					*scopes
						.iter()
						.find(|(_, scope)| scope.len() == 1 && scope[0] != routes.selected)
						.assert_some()
						.0
				} else {
					*scopes
						.iter()
						.find(|(_, scope)| scope.as_ref() == path.as_slice())
						.assert_some()
						.0
				};
				match fault {
					2 => {
						let mut scope = scopes.get(&slot).assert_some().to_vec();
						*scope.last_mut().assert_some() = AtomId::from_parts(
							kind,
							routes.selected.slot(),
							routes.selected.generation() + 1,
						)
						.assert_ok();
						scopes.insert(slot, scope.into_boxed_slice());
					}
					3 => {
						graphs.remove(&slot).assert_some();
						scopes.remove(&slot).assert_some();
					}
					_ => {
						graphs.insert(slot, routes.children[1].id());
					}
				}
				let forged = GraphArenaBindings::new(graphs, scopes);
				state.bindings.graphs[index].arena = forged.clone();
				state._database_catalog.graphs[catalog_index].arena = forged;
			}
			let injected = arena_sources(&routes, &state);
			let files = state.file_catalog.files.clone();
			let paths = generation_directory_paths(&routes.fixture.directory.0);
			let boundary = store.wal.snapshot().visible_lsn();
			let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
			let mut context = crate::storage::atom_store_image::value_context(
				&after.schema,
				after.dictionary.as_ref().clone(),
			)
			.assert_ok();
			let plan_error = GraphUpdatePlan::build(
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
			drop(state);
			assert!(
				matches!(
					plan_error.status().status(),
					crate::gql_status::Status::InvalidStorageFile { .. }
						| crate::gql_status::Status::InvalidStoragePage { .. }
				),
				"fault {fault}: {plan_error}"
			);
			if original_page.is_none() {
				// A clock mismatch would reject before the intended arena
				// guard.
				assert_eq!(store.wal.snapshot().commit_ts().get(), source.commit_ts);
				let commit_error = change(&routes).commit().assert_err();
				assert_eq!(
					plan_error.status().status(),
					commit_error.status().status(),
					"fault {fault}"
				);
			}
			assert!(Arc::ptr_eq(&source, &routes.fixture.store.inner.current_root()));
			assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
			let mut state = store.state.lock();
			assert_eq!(state.file_catalog.files, files);
			assert_sources(&state, &injected);
			assert_eq!(generation_directory_paths(&routes.fixture.directory.0), paths);
			state.bindings.graphs[index].arena = accepted;
			state._database_catalog.graphs[catalog_index].arena = catalog;
			let preparation_only = original_page.is_some();
			if let Some((key, page)) = original_page {
				stage_source(store, key, &page);
			}
			drop(state);
			assert_eq!(
				metagraph::prepare_leaf(store, (&before, &after), &delta).assert_ok().arenas.len(),
				3
			);
			if preparation_only {
				eprintln!("ANCESTOR_SOURCE_PREP_REJECT_PASS {mode:?} {kind:?} {fault}");
			} else {
				let old = original_sources
					.iter()
					.map(|(id, arena, _)| (*id, arena.clone()))
					.collect::<Vec<_>>();
				change(&routes).commit().assert_ok();
				current(&routes, &old);
				assert_retained(&routes.fixture, &files, false);
				eprintln!("ANCESTOR_SOURCE_COMMIT_REJECT_PASS {mode:?} {kind:?} {fault}");
			}
		}
	}
}
