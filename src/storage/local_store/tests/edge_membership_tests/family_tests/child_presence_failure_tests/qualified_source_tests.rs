//! Malformed accepted roles cannot become an implicit bulk-remap fallback.

use super::{source_tests::stage, survivor_source_tests::rejected, *};
use crate::{
	storage::{
		graph_pages::{
			EndpointSide,
			decode_cross_level_overflow,
			encode_cross_level_overflow,
			read_edge_hot_slot,
		},
		page::PageLsn,
	},
	values::storage::DiskAtomRef,
};

/// Sync rejects each qualified-chain fault between passing remap controls.
#[test]
fn qualified_child_source_sync_malformed_chains_preserve_boundary() {
	sources(IoMode::Sync, Case::qualified);
}

/// NativeAsync validates the same source before preparing unpublished output.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_child_source_native_async_malformed_chains_preserve_boundary() {
	sources(IoMode::NativeAsync, Case::qualified);
}

/// Raw WAL injection deliberately keeps MVCC unchanged, so this exercises
/// preparation only; real commit/recovery belongs to the separate fault matrix.
pub(super) fn sources(mode: IoMode, make_case: fn(IoMode, AtomKind, bool) -> Case) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let case = make_case(mode, kind, attach);
			let published = case.fixture.store.inner.current_root();
			let arena = case.arena();
			let store = local(&case.fixture.store);
			let (before, after, delta) =
				leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
			let global = before.resolve_atom(&case.fixture.records[4]).assert_some();
			let connector = before.membership(case.fixture.graph.id(), global).assert_some();
			let state = store.state.lock();
			let files = state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == case.fixture.graph.id())
				.assert_some()
				.files;
			let ceiling = state
				.graph_superblocks
				.get(&case.fixture.graph.id())
				.assert_some()
				.cross_level_adjacency_overflow_pages;
			let (number, within) = hot_slot_address(connector.slot()).assert_ok();
			let hot = read_edge_hot_slot(
				&store
					.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
					.assert_ok(),
				number,
				within,
			)
			.assert_ok();
			assert_eq!((hot.source_count, hot.target_count), (2050, 3));
			let key = PageKey::new(
				files.space(),
				files.cross_level_adjacency,
				hot.source_subgraph_overflow.assert_some(),
			);
			let original = store.read_update_page(&state, key).assert_ok();
			drop(state);
			for fault in 0..8 {
				prepare_leaf(store, (&before, &after), &delta).assert_ok();
				let (old_next, mut entries) =
					decode_cross_level_overflow(&original, key.page_no()).assert_ok();
				assert!(old_next.is_some());
				assert_ne!(entries[0].graph_slot, entries[1].graph_slot);
				let next = match fault {
					0 => Some(key.page_no()),
					1 => Some(PageNo::new(ceiling)),
					2 => {
						entries.truncate(1);
						None
					}
					3 => {
						entries[0].side = EndpointSide::Target;
						old_next
					}
					4 => {
						entries[0].graph_slot = u32::MAX;
						old_next
					}
					5 => {
						entries[0].atom =
							DiskAtomRef::from_parts(AtomKind::Edge, connector.slot()).assert_ok();
						old_next
					}
					6 => {
						entries.swap(0, 1);
						old_next
					}
					_ => {
						assert_eq!(fault, 7);
						entries.clear();
						old_next
					}
				};
				let page =
					encode_cross_level_overflow(key.page_no(), PageLsn::ZERO, next, &entries)
						.assert_ok();
				stage(store, key, &page);
				if fault == 1 {
					// A readable, valid WAL tail still lies outside the old
					// header.
					let number = PageNo::new(ceiling);
					let tail = encode_cross_level_overflow(number, PageLsn::ZERO, None, &entries)
						.assert_ok();
					stage(
						store,
						PageKey::new(files.space(), files.cross_level_adjacency, number),
						&tail,
					);
					assert!(
						store
							.visible_pages(
								&store.state.lock(),
								files.space(),
								files.cross_level_adjacency
							)
							.assert_ok()
							> ceiling
					);
				}
				let reason = match fault {
					0 | 1 => {
						"qualified EDGE chain exceeds its source ceiling/count or contains a cycle"
					}
					2 => "qualified EDGE chain is shorter than its declared count",
					_ => "qualified EDGE chain disagrees with accepted topology",
				};
				rejected(&case, (&before, &after), &delta, reason);
				stage(store, key, &original);
				prepare_leaf(store, (&before, &after), &delta).assert_ok();
				case.assert_owners(&arena);
				assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
				let marker = if case.qualified_edge.is_some() {
					"QUALIFIED_EDGE_CHILD_SOURCE_FAULT_PASS"
				} else {
					"QUALIFIED_CHILD_SOURCE_FAULT_PASS"
				};
				eprintln!("{marker} {mode:?} {kind:?} {attach} {fault}");
			}
		}
	}
}

/// Sync EDGE roles reject malformed chains without claiming index preparation.
#[test]
fn qualified_edge_child_source_sync_malformed_chains_preserve_boundary() {
	sources(IoMode::Sync, Case::qualified_edge);
}

/// NativeAsync uses the same graph-only raw-WAL source controls.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_edge_child_source_native_async_malformed_chains_preserve_boundary() {
	sources(IoMode::NativeAsync, Case::qualified_edge);
}
