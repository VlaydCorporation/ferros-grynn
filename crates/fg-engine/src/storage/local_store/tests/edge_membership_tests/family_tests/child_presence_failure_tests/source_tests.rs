//! Codec-valid zero/one source faults must reject before WAL publication.

use super::*;
use crate::{
	datastore::GlobalAtomId,
	storage::{
		graph_pages::{
			NodeSlotFlags,
			SubgraphEntry,
			decode_subgraph_directory,
			encode_subgraph_directory,
			read_edge_hot_slot,
			read_node_hot_slot,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		page::PageLsn,
	},
};

/// Sync checks each injected authority between passing preparation controls.
#[test]
fn child_presence_sync_malformed_zero_one_sources_preserve_boundary() {
	sources(IoMode::Sync);
}

/// NativeAsync uses the same exact accepted owners and old page ceilings.
#[cfg(feature = "native-async-io")]
#[test]
fn child_presence_native_async_malformed_zero_one_sources_preserve_boundary() {
	sources(IoMode::NativeAsync);
}

/// Publishes a valid frame solely to exercise the real source-page reader.
pub(super) fn stage(
	store: &crate::storage::local_store::LocalStore,
	key: PageKey,
	page: &AlignedPage,
) {
	let mut tx = store.wal.begin_tx().assert_ok();
	tx.stage_page(key, page).assert_ok();
	store.wal.commit_tx(tx).assert_ok();
}

/// Changing all related META fields together reaches semantic source
/// validation.
fn hot_fault(case: &Case, key: PageKey, original: &AlignedPage, fault: usize) {
	hot_fault_at(case, case.owner, key, original, fault);
}

/// A survivor uses its own full identity even when the selected owner is a
/// NODE.
pub(super) fn hot_fault_at(
	case: &Case,
	owner: (GlobalAtomId, AtomId),
	key: PageKey,
	original: &AlignedPage,
	fault: usize,
) {
	let kind = owner.1.kind().assert_some();
	let store = local(&case.fixture.store);
	let within = hot_slot_address(owner.1.slot()).assert_ok().1;
	let other = if kind == AtomKind::Node {
		"node:a".parse().assert_ok()
	} else {
		case.fixture.records[0].clone()
	};
	let other = case.fixture.store.inner.current_root().resolve_atom(&other).assert_some();
	assert_eq!(other.kind(), Some(kind));
	assert_ne!(other, owner.0);
	let state = store.state.lock();
	let mut page = store.read_update_page(&state, key).assert_ok();
	if kind == AtomKind::Node {
		let mut hot = read_node_hot_slot(original, key.page_no(), within).assert_ok();
		match fault {
			0 => hot.generation += 1,
			1 => hot.global_atom = Some(other),
			_ => {
				hot.subgraph_slot = if fault == 3 {
					Some(if hot.subgraph_slot == Some(2) {
						3
					} else {
						2
					})
				} else if hot.subgraph_slot.is_some() {
					None
				} else {
					Some(1)
				};
				hot.flags.set(NodeSlotFlags::META, hot.subgraph_slot.is_some());
			}
		}
		write_node_hot_slot(&mut page, within, &hot).assert_ok();
		page.seal().assert_ok();
		drop(state);
		stage(store, key, &page);
		let state = store.state.lock();
		assert_eq!(
			read_node_hot_slot(
				&store.read_update_page(&state, key).assert_ok(),
				key.page_no(),
				within
			)
			.assert_ok(),
			hot
		);
	} else {
		let mut hot = read_edge_hot_slot(original, key.page_no(), within).assert_ok();
		match fault {
			0 => hot.generation += 1,
			1 => hot.global_atom = Some(other),
			_ => {
				let old = hot.transition_subgraph_slot;
				let new = if fault == 3 {
					Some(if old == Some(2) {
						3
					} else {
						2
					})
				} else if old.is_some() {
					None
				} else {
					Some(1)
				};
				hot.replace_transition(key.page_no(), old, new).assert_ok();
			}
		}
		write_edge_hot_slot(&mut page, within, &hot).assert_ok();
		page.seal().assert_ok();
		drop(state);
		stage(store, key, &page);
		let state = store.state.lock();
		assert_eq!(
			read_edge_hot_slot(
				&store.read_update_page(&state, key).assert_ok(),
				key.page_no(),
				within
			)
			.assert_ok(),
			hot
		);
	}
}

/// Exact reasons prevent an earlier malformed fixture from masking a deeper
/// guard.
fn sources(mode: IoMode) {
	sources_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn sources_with_family(mode: IoMode, family: GraphKind) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let case = Case::new_with_family(mode, kind, attach, family);
			case.check(false);
			let published = case.fixture.store.inner.current_root();
			let store = local(&case.fixture.store);
			let (before, after, delta) =
				leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
			let child = before
				.occurrence(case.fixture.graph.id(), case.owner.1)
				.assert_some()
				.child_graph()
				.or_else(|| {
					after
						.occurrence(case.fixture.graph.id(), case.owner.1)
						.assert_some()
						.child_graph()
				})
				.assert_some();
			let state = store.state.lock();
			let binding_index = state
				.bindings
				.graphs
				.iter()
				.position(|binding| binding.graph_id == case.fixture.graph.id())
				.assert_some();
			let catalog_index = state
				._database_catalog
				.graphs
				.iter()
				.position(|entry| entry.id == case.fixture.graph.id())
				.assert_some();
			let accepted = state.bindings.graphs[binding_index].arena.clone();
			assert!(accepted.same_owner(&state._database_catalog.graphs[catalog_index].arena));
			let header = *state.graph_superblocks.get(&case.fixture.graph.id()).assert_some();
			let child_header = *state.graph_superblocks.get(&child).assert_some();
			drop(state);
			let [(hot_key, hot), (directory_key, directory)] = case.pages();
			let (_, original_entries) =
				decode_subgraph_directory(&directory, PageNo::new(0)).assert_ok();
			assert_eq!(original_entries.len(), usize::from(!attach));
			for fault in 0..11 {
				prepare_leaf(store, (&before, &after), &delta).assert_ok();
				let reason = match fault {
					0..=3 => {
						hot_fault(&case, hot_key, &hot, fault);
						if kind == AtomKind::Node {
							"leaf rebind source NODE identity or child slot disagrees"
						} else {
							"EDGE Hot source disagrees with accepted root"
						}
					}
					4 => {
						store.state.lock().bindings.graphs[binding_index].arena =
							GraphArenaBindings::new(
								accepted.graph_by_slot().clone(),
								accepted.scope_by_slot().clone(),
							);
						"leaf rebind source catalog arena owner disagrees"
					}
					5..=7 => {
						let mut entries = original_entries.clone();
						let next = (fault == 5).then_some(PageNo::new(0));
						if fault == 6 {
							if attach {
								entries.push(SubgraphEntry {
									slot: 1,
									generation: case.owner.1.generation(),
									node_capacity: 1,
									edge_capacity: 0,
									node_count: 1,
									edge_count: 0,
								});
							} else {
								entries.clear();
							}
						} else if fault == 7 {
							if attach {
								let mut state = store.state.lock();
								let header = state
									.graph_superblocks
									.get_mut(&case.fixture.graph.id())
									.assert_some();
								header.subgraph_count = 1;
								header.subgraph_capacity = header.subgraph_capacity.max(1);
							} else {
								entries[0].generation += 1;
							}
						}
						let page = encode_subgraph_directory(
							PageNo::new(0),
							PageLsn::ZERO,
							next,
							&entries,
						)
						.assert_ok();
						stage(store, directory_key, &page);
						let state = store.state.lock();
						assert_eq!(
							decode_subgraph_directory(
								&store.read_update_page(&state, directory_key).assert_ok(),
								PageNo::new(0)
							)
							.assert_ok(),
							(next, entries)
						);
						if fault == 7 && !attach {
							"parent directory direct scope or generation disagrees"
						} else {
							"child presence directory source coverage disagrees"
						}
					}
					8 => {
						store
							.state
							.lock()
							.graph_superblocks
							.get_mut(&case.fixture.graph.id())
							.assert_some()
							.subgraphs_pages = 2;
						"child presence directory exceeds its zero/one layout"
					}
					9 => {
						store
							.state
							.lock()
							.graph_superblocks
							.get_mut(&child)
							.assert_some()
							.node_count = 0;
						if attach {
							"parent directory replacement child counters disagree"
						} else {
							"parent directory source child counters disagree"
						}
					}
					_ => {
						let mut graphs = accepted.graph_by_slot().clone();
						let mut scopes = accepted.scope_by_slot().clone();
						if attach {
							graphs.insert(1, child);
							scopes.insert(1, vec![case.owner.1].into_boxed_slice());
						} else {
							graphs.remove(&1);
							scopes.remove(&1);
						}
						let forged = GraphArenaBindings::new(graphs, scopes);
						let mut state = store.state.lock();
						state.bindings.graphs[binding_index].arena = forged.clone();
						state._database_catalog.graphs[catalog_index].arena = forged;
						if attach {
							"leaf rebind accepted direct scope disagrees"
						} else {
							"qualified point scope is absent from accepted arena"
						}
					}
				};
				let root = case.fixture.store.inner.current_root();
				let boundary = store.wal.snapshot().visible_lsn();
				let state = store.state.lock();
				let files = state.file_catalog.files.clone();
				let owners = (
					state.bindings.graphs[binding_index].arena.clone(),
					state._database_catalog.graphs[catalog_index].arena.clone(),
				);
				drop(state);
				let directories = super::super::super::super::generation_directory_paths(
					&case.fixture.directory.0,
				);
				let error = prepare_leaf(store, (&before, &after), &delta).err().assert_some();
				assert!(
					matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason: actual }
					if actual.as_str() == reason),
					"fault={fault}, kind={kind:?}, attach={attach}, error={error}"
				);
				assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
				assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
				let mut state = store.state.lock();
				assert_eq!(state.file_catalog.files, files);
				assert!(state.bindings.graphs[binding_index].arena.same_owner(&owners.0));
				assert!(state._database_catalog.graphs[catalog_index].arena.same_owner(&owners.1));
				assert_eq!(
					super::super::super::super::generation_directory_paths(
						&case.fixture.directory.0
					),
					directories
				);
				state.bindings.graphs[binding_index].arena = accepted.clone();
				state._database_catalog.graphs[catalog_index].arena = accepted.clone();
				*state.graph_superblocks.get_mut(&case.fixture.graph.id()).assert_some() = header;
				*state.graph_superblocks.get_mut(&child).assert_some() = child_header;
				drop(state);
				stage(store, hot_key, &hot);
				stage(store, directory_key, &directory);
				prepare_leaf(store, (&before, &after), &delta).assert_ok();
				case.assert_owners(&accepted);
			}
			assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
			let stamp = store.wal.snapshot().commit_ts().get();
			assert!(stamp > published.commit_ts);
			// Raw test frames advance WAL authority without publishing an MVCC
			// root.
			assert!(store.capture_query_snapshot(published.commit_ts).assert_ok().is_none());
			let physical = store.capture_query_snapshot(stamp).assert_ok().assert_some();
			let read = store
				.physical_traversal_inner_graph(&physical, case.fixture.graph.id(), case.owner.1)
				.assert_ok()
				.assert_some();
			assert_eq!(
				read.graph,
				before
					.occurrence(case.fixture.graph.id(), case.owner.1)
					.assert_some()
					.child_graph()
			);
			drop(physical);
			assert_bulk_image(&case.fixture);
		}
	}
}
