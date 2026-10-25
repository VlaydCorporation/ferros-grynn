//! Synthetic stable membership ownership must not apply one label shift twice.

use std::collections::BTreeSet;

use super::*;
use crate::{
	datastore::PublicationDelta,
	storage::{
		atom_store_image::value_context,
		graph_pages::{
			build_graph_image,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
		},
		local_store::{point_graph::GraphUpdatePlan, point_incidence, point_namespace::LabelPlan},
		page::PageLsn,
		types::PageKey,
	},
};

/// Incidence renders the explicit owner while the suffix shifts other postings.
#[test]
fn label_lifecycle_stable_explicit_owner_uses_final_namespace_once() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for offset in [1, 3] {
			let (fixture, _) = membership_fixture(io_mode);
			let graph = fixture.graph.id();
			let local = local_store(&fixture.store);
			let published = fixture.store.inner.current_root();
			let mut before = published.as_ref().clone();
			local.hydrate_statistics(&mut before).assert_ok();
			let record = &fixture.records[offset];
			let global = before.resolve_atom(record).assert_some();
			let atom = before.membership(graph, global).assert_some();
			assert!(before.occurrence(graph, atom).assert_some().overlay().is_none());
			let mut after = before.clone();
			after.set_occurrence_label(graph, atom, Some("A".into())).assert_ok();
			// Preserve the exact identity while explicitly assigning renderer
			// ownership.
			let delta = PublicationDelta {
				records: [record.clone()].into_iter().collect(),
				global_slots: BTreeSet::new(),
				overlays: BTreeSet::new(),
				incidence: [(graph, atom)].into_iter().collect(),
				memberships: [(graph, global)].into_iter().collect(),
				point_graph_only: true,
			};
			assert!(point_incidence::eligible(&before, &after, &delta));
			let state = local.state.lock();
			let files_before = state.file_catalog.files.clone();
			let boundary = local.wal.snapshot().visible_lsn();
			let files = state
				.bindings
				.graphs
				.iter()
				.find(|entry| entry.graph_id == graph)
				.assert_some()
				.files;
			let mut labels = LabelPlan::build(local, &state, &before, &after, &delta).assert_ok();
			let mut context =
				value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
			let plan = GraphUpdatePlan::build(
				local,
				&state,
				&before,
				&after,
				&delta,
				&mut labels,
				&mut context,
			)
			.assert_ok();
			let keys =
				plan.pages.iter().map(|(key, _)| *key).collect::<std::collections::BTreeSet<_>>();
			assert_eq!(keys.len(), plan.pages.len());
			let bulk = build_graph_image(&after, graph, &mut context, PageLsn::ZERO).assert_ok();
			for occurrence in after.graph(graph).assert_some().occurrences() {
				let id = occurrence.local_id();
				let (number, within) = hot_slot_address(id.slot()).assert_ok();
				let file = if id.is_node() {
					files.nodes
				} else {
					files.edges
				};
				let key = PageKey::new(files.space(), file, number);
				// Borrow prepared frames; only untouched source pages need an
				// owner.
				let fallback;
				let page = match plan.pages.iter().find(|(candidate, _)| *candidate == key) {
					Some((_, page)) => page.as_ref(),
					None => {
						fallback = local.read_update_page(&state, key).assert_ok();
						fallback.as_ref()
					}
				};
				let page_index = usize::try_from(number.get()).assert_ok();
				if id.is_node() {
					let actual = read_node_hot_slot(page, number, within).assert_ok();
					let mut expected =
						read_node_hot_slot(&bulk.nodes[page_index], number, within).assert_ok();
					expected.overlay = actual.overlay;
					assert_eq!(actual, expected);
				} else {
					let actual = read_edge_hot_slot(page, number, within).assert_ok();
					let mut expected =
						read_edge_hot_slot(&bulk.edges[page_index], number, within).assert_ok();
					expected.overlay = actual.overlay;
					assert_eq!(actual, expected);
				}
			}
			assert_eq!(state.file_catalog.files, files_before);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert!(Arc::ptr_eq(&fixture.store.inner.current_root(), &published));
		}
	}
}

/// A checksummed but false old label cannot authorize an explicit lifecycle
/// slot.
#[test]
fn label_lifecycle_checksum_valid_hot_label_mismatch_publishes_nothing() {
	use crate::{
		gql_status::Status,
		storage::graph_pages::{write_edge_hot_slot, write_node_hot_slot},
	};
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for offset in [1, 3] {
			let (fixture, _) = membership_fixture(io_mode);
			let fixture = reopen_checkpointed(fixture, io_mode);
			let graph = fixture.graph.id();
			let local = local_store(&fixture.store);
			let published = fixture.store.inner.current_root();
			let mut before = published.as_ref().clone();
			local.hydrate_statistics(&mut before).assert_ok();
			let record = &fixture.records[offset];
			let global = before.resolve_atom(record).assert_some();
			let atom = before.membership(graph, global).assert_some();
			let mut after = before.clone();
			after.set_occurrence_label(graph, atom, Some("A".into())).assert_ok();
			let delta = PublicationDelta {
				records: [record.clone()].into_iter().collect(),
				global_slots: BTreeSet::new(),
				overlays: BTreeSet::new(),
				incidence: [(graph, atom)].into_iter().collect(),
				memberships: [(graph, global)].into_iter().collect(),
				point_graph_only: true,
			};
			let state = local.state.lock();
			let mut labels = LabelPlan::build(local, &state, &before, &after, &delta).assert_ok();
			let mut context =
				value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
			GraphUpdatePlan::build(
				local,
				&state,
				&before,
				&after,
				&delta,
				&mut labels,
				&mut context,
			)
			.assert_ok();
			let files = state
				.bindings
				.graphs
				.iter()
				.find(|entry| entry.graph_id == graph)
				.assert_some()
				.files;
			let (number, within) = hot_slot_address(atom.slot()).assert_ok();
			let file = if atom.is_node() {
				files.nodes
			} else {
				files.edges
			};
			let key = PageKey::new(files.space(), file, number);
			let mut page = local.read_update_page(&state, key).assert_ok();
			if atom.is_node() {
				let mut slot = read_node_hot_slot(&page, number, within).assert_ok();
				slot.replace_label(number, Some(1), Some(0)).assert_ok();
				write_node_hot_slot(&mut page, within, &slot).assert_ok();
			} else {
				let mut slot = read_edge_hot_slot(&page, number, within).assert_ok();
				slot.replace_label(number, Some(1), Some(0)).assert_ok();
				write_edge_hot_slot(&mut page, within, &slot).assert_ok();
			}
			page.seal().assert_ok();
			// Direct preparation isolates a false physical source from the
			// logical clock.
			let mut injection = local.wal.begin_tx().assert_ok();
			injection.stage_page(key, &page).assert_ok();
			local.wal.commit_tx(injection).assert_ok();
			let boundary = local.wal.snapshot().visible_lsn();
			let catalog = state.file_catalog.files.clone();
			let mut labels = LabelPlan::build(local, &state, &before, &after, &delta).assert_ok();
			let mut context =
				value_context(&after.schema, after.dictionary.as_ref().clone()).assert_ok();
			let error = GraphUpdatePlan::build(
				local,
				&state,
				&before,
				&after,
				&delta,
				&mut labels,
				&mut context,
			)
			.err()
			.assert_some();
			let expected = if atom.is_node() {
				"explicit NODE source"
			} else {
				"EDGE Hot source"
			};
			assert!(
				matches!(error.status().status(), Status::InvalidStorageFile { reason } if reason.contains(expected))
			);
			assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
			assert_eq!(state.file_catalog.files, catalog);
			assert!(Arc::ptr_eq(&fixture.store.inner.current_root(), &published));
		}
	}
}
