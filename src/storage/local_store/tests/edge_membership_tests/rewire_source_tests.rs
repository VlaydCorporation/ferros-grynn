//! Checksummed malformed old EDGE roles fail before sparse publication.

use super::{rewire_tests::write_topology, support::*, *};
use crate::{
	datastore::PublicationDelta,
	storage::{
		graph_pages::{
			decode_adjacency_overflow,
			encode_adjacency_overflow_into,
			hot_slot_address,
			read_edge_hot_slot,
		},
		local_store::{point_graph::GraphUpdatePlan, point_incidence, point_namespace::LabelPlan},
		page::{PageLsn, PageNo},
		types::PageKey,
	},
	values::storage::DiskAtomRef,
};

/// Old EDGE endpoint pages are distinct from NODE incident-edge chains.
#[test]
fn edge_rewire_sync_invalid_old_endpoint_chains_publish_nothing() {
	invalid_old_endpoints(IoMode::Sync);
}

/// NativeAsync preparation checks identical source structure and ordering.
#[cfg(feature = "native-async-io")]
#[test]
fn edge_rewire_native_async_invalid_old_endpoint_chains_publish_nothing() {
	invalid_old_endpoints(IoMode::NativeAsync);
}

/// The valid control reaches preparation before each test-only corrupt frame.
fn invalid_old_endpoints(io_mode: IoMode) {
	for fault in 0..5 {
		let mut fixture = fixture(io_mode, 1, 1);
		let sources = (0..2050).map(|index| fixture.nodes[index % 2]).collect::<Vec<_>>();
		write_topology(&fixture, EdgeTopology::directed(sources, [fixture.nodes[1]]))
			.commit()
			.assert_ok();
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = reopen(fixture);
		let store = local(&fixture.store);
		let source = fixture.store.inner.current_root();
		let mut before = source.as_ref().clone();
		store.hydrate_statistics(&mut before).assert_ok();
		let mut after = before.clone();
		let global = before.resolve_atom(&fixture.records[0]).assert_some();
		let edge = before.membership(fixture.graph.id(), global).assert_some();
		after
			.replace_occurrence_state(
				fixture.graph.id(),
				edge,
				crate::datastore::OccurrenceState::edge(
					EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
					None,
					None,
				),
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
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == fixture.graph.id())
			.assert_some()
			.files;
		let (number, within) = hot_slot_address(edge.slot()).assert_ok();
		let hot_page = store
			.read_update_page(&state, PageKey::new(files.space(), files.edges, number))
			.assert_ok();
		let hot = read_edge_hot_slot(&hot_page, number, within).assert_ok();
		assert_eq!(hot.source_count, 2050);
		let key = PageKey::new(files.space(), files.adjacency, hot.source_overflow.assert_some());
		let mut page = store.read_update_page(&state, key).assert_ok();
		let (old_next, mut entries) = decode_adjacency_overflow(&page, key.page_no()).assert_ok();
		assert!(old_next.is_some());
		assert_ne!(entries[0], entries[1]);
		let next = match fault {
			0 => Some(key.page_no()),
			1 => Some(PageNo::new(
				state
					.graph_superblocks
					.get(&fixture.graph.id())
					.assert_some()
					.adjacency_overflow_pages,
			)),
			2 => {
				entries.truncate(1);
				None
			}
			3 => {
				entries[0] = DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok();
				old_next
			}
			_ => {
				entries.swap(0, 1);
				old_next
			}
		};
		encode_adjacency_overflow_into(&mut page, key.page_no(), PageLsn::ZERO, next, &entries)
			.assert_ok();
		// Valid checksums isolate chain validation from ordinary page
		// corruption.
		let mut write = store.wal.begin_tx().assert_ok();
		write.stage_page(key, &page).assert_ok();
		store.wal.commit_tx(write).assert_ok();
		let boundary = store.wal.snapshot().visible_lsn();
		let files_before = state.file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)
		.assert_ok();
		// Direct preparation avoids the deliberate logical/WAL clock mismatch.
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
			0 | 1 => "adjacency source exceeds its ceiling or contains a cycle",
			2 => "adjacency chain is shorter than its declared count",
			_ => "EDGE physical endpoints disagree with accepted topology",
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason }
			if reason.as_str() == expected)
		);
		assert!(std::sync::Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(state.file_catalog.files, files_before);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
	}
}
