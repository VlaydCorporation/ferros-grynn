//! Checksummed qualified chain faults must fail before any candidate
//! publication.

use super::*;
use crate::{
	datastore::PublicationDelta,
	storage::{
		graph_pages::{
			EndpointSide,
			decode_cross_level_overflow,
			encode_cross_level_overflow,
			hot_slot_address,
			read_edge_hot_slot,
		},
		local_store::{point_graph::GraphUpdatePlan, point_incidence, point_namespace::LabelPlan},
		page::PageLsn,
	},
	values::storage::DiskAtomRef,
};

/// Sync rejects cycle, old ceiling, short, empty, scope, address, side and
/// order faults.
#[test]
fn qualified_sync_bad_source_chains_publish_nothing() {
	bad_source_chains(IoMode::Sync);
}

/// NativeAsync validates the same bytes before changing the root, files or WAL.
#[cfg(feature = "native-async-io")]
#[test]
fn qualified_native_async_bad_source_chains_publish_nothing() {
	bad_source_chains(IoMode::NativeAsync);
}

/// Valid control preparation proves each test reaches the qualified point
/// renderer.
fn bad_source_chains(mode: IoMode) {
	for fault in 0..8 {
		let mut fixture = qualified_fixture(mode);
		let (write, _) = attachment(&fixture, wide_topology(&fixture));
		write.commit().assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		let store = local(&fixture.store);
		let source = fixture.store.inner.current_root();
		let mut before = source.as_ref().clone();
		store.hydrate_statistics(&mut before).assert_ok();
		let mut after = before.clone();
		let global = before.resolve_atom(&fixture.records[2]).assert_some();
		let edge = before.membership(fixture.graph.id(), global).assert_some();
		after
			.replace_occurrence_state(
				fixture.graph.id(),
				edge,
				OccurrenceState::edge(short_topology(&fixture), None, None),
			)
			.assert_ok();
		let delta = PublicationDelta {
			records: BTreeSet::new(),
			global_slots: BTreeSet::new(),
			overlays: BTreeSet::new(),
			incidence: BTreeSet::from([(fixture.graph.id(), edge)]),
			memberships: BTreeSet::new(),
			point_graph_only: true,
		};
		let state = store.state.lock();
		assert!(point_incidence::eligible(&before, &after, &delta));
		let mut control = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)
		.assert_ok();
		GraphUpdatePlan::build(store, &state, &before, &after, &delta, &mut control, &mut context)
			.assert_ok();
		let binding = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		let files = binding.files;
		let ceiling = state
			.graph_superblocks
			.get(&fixture.graph.id())
			.assert_some()
			.cross_level_adjacency_overflow_pages;
		let (number, within) = hot_slot_address(edge.slot()).assert_ok();
		let hot = store
			.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
			.assert_ok();
		let hot = read_edge_hot_slot(&hot, number, within).assert_ok();
		let key = PageKey::new(
			files.space(),
			files.cross_level_adjacency,
			hot.source_subgraph_overflow.assert_some(),
		);
		let page = store.read_update_page(&state, key).assert_ok();
		let (old_next, mut entries) = decode_cross_level_overflow(&page, key.page_no()).assert_ok();
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
				entries[0].atom = DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok();
				old_next
			}
			6 => {
				entries.swap(0, 1);
				old_next
			}
			_ => {
				entries.clear();
				old_next
			}
		};
		let page =
			encode_cross_level_overflow(key.page_no(), PageLsn::ZERO, next, &entries).assert_ok();
		let mut injected = store.wal.begin_tx().assert_ok();
		injected.stage_page(key, &page).assert_ok();
		if fault == 1 {
			// Available WAL tail is deliberately valid but lies outside the
			// accepted old header.
			let tail =
				encode_cross_level_overflow(PageNo::new(ceiling), PageLsn::ZERO, None, &entries)
					.assert_ok();
			injected
				.stage_page(
					PageKey::new(files.space(), files.cross_level_adjacency, PageNo::new(ceiling)),
					&tail,
				)
				.assert_ok();
		}
		store.wal.commit_tx(injected).assert_ok();
		let boundary = store.wal.snapshot().visible_lsn();
		let catalog = state.file_catalog.files.clone();
		let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
		let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)
		.assert_ok();
		// Direct preparation isolates source validation from the deliberately
		// injected WAL clock.
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
		let expected = match fault {
			0 | 1 => "qualified EDGE chain exceeds its source ceiling/count or contains a cycle",
			2 => "qualified EDGE chain is shorter than its declared count",
			_ => "qualified EDGE chain disagrees with accepted topology",
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason } if reason.as_str() == expected)
		);
		assert!(std::sync::Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(state.file_catalog.files, catalog);
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
	}
}
