//! Same-child META preparation must reject inconsistent accepted authorities.

use super::{transition_rewire_tests::portal, *};
use crate::{
	datastore::{OccurrenceState, PublicationDelta},
	storage::{
		graph_pages::{
			GraphArenaBindings,
			hot_slot_address,
			read_edge_hot_slot,
			write_edge_hot_slot,
		},
		local_store::{point_graph::GraphUpdatePlan, point_incidence, point_namespace::LabelPlan},
	},
};

/// Sync rejects false arena and source Hot identities before publication.
#[test]
fn transition_rewire_sync_bad_sources_publish_nothing() {
	bad_sources(IoMode::Sync);
}

/// NativeAsync reaches a valid control preparation before every injected fault.
#[cfg(feature = "native-async-io")]
#[test]
fn transition_rewire_native_async_bad_sources_publish_nothing() {
	bad_sources(IoMode::NativeAsync);
}

/// Binding faults stay private; valid checksummed Hot faults use a separate
/// WAL.
fn bad_sources(mode: IoMode) {
	for fault in 0..8 {
		let (fixture, _) = family_fixture(mode, GraphKind::Metagraph);
		let (global, edge, child) = portal(&fixture);
		let store = local(&fixture.store);
		let source = fixture.store.inner.current_root();
		let mut before = source.as_ref().clone();
		store.hydrate_statistics(&mut before).assert_ok();
		let mut after = before.clone();
		after
			.replace_occurrence_state(
				fixture.graph.id(),
				edge,
				OccurrenceState::edge(
					EdgeTopology::directed([fixture.nodes[1]], [fixture.nodes[0]]),
					None,
					Some(child),
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
		assert!(point_incidence::eligible(&before, &after, &delta));
		let mut state = store.state.lock();
		let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
		let mut context = crate::storage::atom_store_image::value_context(
			&after.schema,
			after.dictionary.as_ref().clone(),
		)
		.assert_ok();
		GraphUpdatePlan::build(store, &state, &before, &after, &delta, &mut labels, &mut context)
			.assert_ok();
		let index = state
			.bindings
			.graphs
			.iter()
			.position(|binding| binding.graph_id == fixture.graph.id())
			.assert_some();
		let accepted = state.bindings.graphs[index].arena.clone();
		let files = state.bindings.graphs[index].files;
		if fault < 5 {
			let mut graphs = accepted.graph_by_slot().clone();
			let mut scopes = accepted.scope_by_slot().clone();
			let slot = *scopes
				.iter()
				.find(|(_, scope)| scope.as_ref() == [edge].as_slice())
				.assert_some()
				.0;
			match fault {
				0 => {
					graphs.insert(0, child);
				}
				1 => {
					scopes.remove(&slot);
				}
				2 => {
					let other = u32::try_from(graphs.len()).assert_ok();
					graphs.insert(other, child);
					scopes.insert(other, vec![edge].into_boxed_slice());
				}
				3 => {
					graphs.insert(slot, fixture.graph.id());
				}
				_ => {
					graphs.remove(&slot);
				}
			}
			state.bindings.graphs[index].arena = GraphArenaBindings::new(graphs, scopes);
		} else {
			let (number, within) = hot_slot_address(edge.slot()).assert_ok();
			let key = PageKey::new(files.space(), files.edges, number);
			let mut page = store.read_update_page(&state, key).assert_ok();
			let mut hot = read_edge_hot_slot(&page, number, within).assert_ok();
			match fault {
				5 => {
					hot.generation += 1;
				}
				6 => {
					let other = before.resolve_atom(&fixture.records[0]).assert_some();
					assert_ne!(global, other);
					hot.global_atom = Some(other);
				}
				_ => {
					let wrong = *accepted
						.graph_by_slot()
						.iter()
						.find(|(slot, graph)| **slot != 0 && **graph != child)
						.assert_some()
						.0;
					hot.transition_subgraph_slot = Some(wrong);
				}
			}
			write_edge_hot_slot(&mut page, within, &hot).assert_ok();
			page.seal().assert_ok();
			let mut injected = store.wal.begin_tx().assert_ok();
			injected.stage_page(key, &page).assert_ok();
			store.wal.commit_tx(injected).assert_ok();
			// Ensure the decoder accepted the deliberate semantic fault.
			let decoded = store.read_update_page(&state, key).assert_ok();
			assert_eq!(read_edge_hot_slot(&decoded, number, within).assert_ok(), hot);
		}
		let boundary = store.wal.snapshot().visible_lsn();
		let catalog = state.file_catalog.files.clone();
		let directories = super::super::super::generation_directory_paths(&fixture.directory.0);
		let mut labels = LabelPlan::build(store, &state, &before, &after, &delta).assert_ok();
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
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::InvalidStorageFile { .. }
		));
		assert!(std::sync::Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(state.file_catalog.files, catalog);
		assert_eq!(
			super::super::super::generation_directory_paths(&fixture.directory.0),
			directories
		);
		state.bindings.graphs[index].arena = accepted;
	}
}
