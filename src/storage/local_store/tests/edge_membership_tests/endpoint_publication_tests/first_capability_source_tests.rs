//! Accepted first-capability sources reject inconsistent metadata before
//! output.

use super::{
	first_capability_tests::{Change, first_fixture},
	*,
};
use crate::{
	datastore::{IndexId, SystemIndexKind},
	storage::local_store::{
		layout::LayoutBuilder,
		point_endpoint::append_selected_indexes,
		point_namespace::PublicationBudget,
	},
};

/// Four independent source metadata faults reject before creating any file.
#[test]
fn endpoint_first_sync_inconsistent_source_rejects_before_output() {
	check_source(IoMode::Sync);
}

/// NativeAsync uses the same accepted catalog and runtime binding authority.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_first_native_async_inconsistent_source_rejects_before_output() {
	check_source(IoMode::NativeAsync);
}

/// A real positive prepare distinguishes rejection from a broken empty fixture.
fn check_source(io_mode: IoMode) {
	let (fixture, _, _) = first_fixture(io_mode, Change::Attach);
	let store = local(&fixture.store);
	let source = fixture.store.inner.current_root();
	let boundary = store.wal.snapshot().visible_lsn();
	let stamp = store.wal.snapshot().commit_ts();
	let paths = generation_directory_paths(&fixture.directory.0);
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let clock = state.file_catalog.next_file_instance_id;
	let binding_position = state
		.bindings
		.graphs
		.iter()
		.position(|graph| graph.graph_id == fixture.graph.id())
		.assert_some();
	let catalog_position = state
		._database_catalog
		.graphs
		.iter()
		.position(|graph| graph.id == fixture.graph.id())
		.assert_some();
	let accepted = state._database_catalog.graphs[catalog_position].clone();
	let fake = state.bindings.graphs[binding_position].membership;
	assert!(accepted.edge_endpoint_index.is_none());
	assert!(accepted.edge_endpoint.is_none());
	assert!(state.bindings.graphs[binding_position].edge_endpoint.is_none());
	drop(state);
	let assert_source = || {
		assert!(Arc::ptr_eq(&source, &fixture.store.inner.current_root()));
		assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(store.wal.snapshot().commit_ts(), stamp);
		let state = store.state.lock();
		assert_eq!(state.file_catalog.files, files);
		assert_eq!(state.file_catalog.next_file_instance_id, clock);
		assert_eq!(state._database_catalog.graphs[catalog_position], accepted);
		assert!(state.bindings.graphs[binding_position].edge_endpoint.is_none());
		drop(state);
		assert_eq!(generation_directory_paths(&fixture.directory.0), paths);
	};
	let mut after = source.as_ref().clone();
	after.commit_ts = stamp.get() + 1;
	let schema = after.schema.graph(fixture.graph.id()).assert_some();
	let next_id = after.schema.next_schema_id;
	let capability = SystemIndex::new(
		IndexId::new(u32::try_from(next_id).assert_ok()).assert_some(),
		SystemIndexKind::EdgeEndpoint,
	);
	let replacement = schema
		.replace_indexes(schema.indexes().cloned().collect())
		.assert_ok()
		.with_edge_endpoint_index(capability)
		.assert_ok();
	after.replace_graph_schema(replacement, after.commit_ts).assert_ok();
	after.advance_identity_clocks(next_id + 1, after.schema.next_graph_id);
	let selected = BTreeSet::from([fixture.graph.id()]);
	{
		let state = store.state.lock();
		let mut budget = PublicationBudget::new();
		let mut layout = LayoutBuilder::new(clock);
		let mut cleanup = None;
		let mut retired = BTreeSet::new();
		let generation = crate::values::Uuid::now_v7().to_string();
		let arenas = std::collections::BTreeMap::from([(
			fixture.graph.id(),
			state.bindings.graphs[binding_position].arena.clone(),
		)]);
		let replacements = append_selected_indexes(
			store,
			&state,
			(&after, Some(&arenas)),
			&selected,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
			&mut budget,
		)
		.assert_ok();
		assert_eq!(replacements.len(), 1);
		assert!(replacements.contains_key(&fixture.graph.id()));
		assert!(layout.has_new_files());
		assert!(cleanup.is_some());
		assert!(retired.is_empty());
	}
	assert_source();
	for fault in 0..4 {
		let mut state = store.state.lock();
		match fault {
			0 => state.bindings.graphs[binding_position].edge_endpoint = Some(fake),
			1 => state._database_catalog.graphs[catalog_position].edge_endpoint = Some(fake),
			2 => {
				state._database_catalog.graphs[catalog_position].edge_endpoint_index =
					Some((capability.id(), capability.version()));
			}
			_ => {
				state._database_catalog.graphs[catalog_position].edge_endpoint = Some(fake);
				state.bindings.graphs[binding_position].edge_endpoint = Some(fake);
			}
		}
		let mut budget = PublicationBudget::new();
		let mut layout = LayoutBuilder::new(clock);
		let mut cleanup = None;
		let mut retired = BTreeSet::new();
		let generation = crate::values::Uuid::now_v7().to_string();
		let result = append_selected_indexes(
			store,
			&state,
			(&after, None),
			&selected,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
			&mut budget,
		);
		state._database_catalog.graphs[catalog_position].edge_endpoint = None;
		state._database_catalog.graphs[catalog_position].edge_endpoint_index = None;
		state.bindings.graphs[binding_position].edge_endpoint = None;
		let error = result.assert_err();
		let expected = if fault < 2 {
			"selected endpoint catalog and runtime bindings disagree"
		} else {
			"selected endpoint source capability and binding disagree"
		};
		assert!(
			matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason } if reason.as_str() == expected)
		);
		// Cleanup cannot hide output created before the metadata rejection.
		assert!(cleanup.is_none());
		assert!(!layout.has_new_files());
		assert!(retired.is_empty());
		drop(state);
		assert_source();
	}
}
