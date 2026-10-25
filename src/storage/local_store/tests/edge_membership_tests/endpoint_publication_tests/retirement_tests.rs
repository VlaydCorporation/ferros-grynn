//! A retired endpoint tree remains owned by snapshots until pins are released.

use super::*;

/// Runtime sweeping and cold recovery unlink only the old unreferenced tree.
#[test]
fn endpoint_selected_sync_retirement_respects_pins_and_recovery() {
	retirement(IoMode::Sync);
}

/// NativeAsync applies the same file ownership rule during reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn endpoint_selected_native_async_retirement_respects_pins_and_recovery() {
	retirement(IoMode::NativeAsync);
}

/// Reads the canonical endpoint path from the current accepted file catalog.
fn endpoint_path(fixture: &Fixture) -> std::path::PathBuf {
	let store = local(&fixture.store);
	let state = store.state.lock();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|binding| binding.graph_id == fixture.graph.id())
		.assert_some()
		.edge_endpoint
		.assert_some();
	let entry = state
		.file_catalog
		.files
		.iter()
		.find(|entry| {
			entry.space_id == DataSpaceId::Graph(fixture.graph.id())
				&& entry.file_id == binding.file_id
		})
		.assert_some();
	fixture.directory.0.join(entry.path.as_str())
}

/// A reachable generation directory must survive retirement of its old tree.
fn retirement(io_mode: IoMode) {
	for recover in [false, true] {
		let (mut fixture, endpoint, connector) = endpoint_fixture(io_mode);
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let old_path = endpoint_path(&fixture);
		let pin = local(&fixture.store)
			.capture_query_snapshot(fixture.store.inner.current_root().commit_ts)
			.assert_ok()
			.assert_some();
		let mut write = fixture.store.begin();
		let added = write
			.attach_edge(
				fixture.graph.id(),
				&fixture.records[2],
				None,
				EdgeTopology::directed([endpoint], [fixture.nodes[0]]),
				None,
				None,
			)
			.assert_ok();
		write.commit().assert_ok();
		let current_path = endpoint_path(&fixture);
		assert_ne!(old_path, current_path);
		assert!(old_path.is_file());
		assert!(current_path.is_file());
		assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
		assert!(old_path.is_file());
		if recover {
			let Fixture {
				store,
				options,
				graph,
				nodes,
				records,
				directory,
			} = fixture;
			drop(store);
			drop(pin);
			assert!(old_path.is_file());
			let store = Datastore::open_local(&directory.0, options).assert_ok();
			assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
			store.inner.maintenance.shutdown();
			fixture = Fixture {
				store,
				options,
				graph,
				nodes,
				records,
				directory,
			};
		} else {
			drop(pin);
			crate::storage::local_store::sweep_retired_generations(
				&mut local(&fixture.store).state.lock(),
			);
		}
		assert!(!old_path.exists());
		assert!(old_path.parent().assert_some().is_dir());
		assert!(current_path.is_file());
		assert_data_files(&fixture, &files);
		let raw =
			|edge: AtomId| DiskAtomRef::from_parts(AtomKind::Edge, edge.slot()).assert_ok().raw();
		assert_postings(&fixture, &[
			(0, raw(endpoint), raw(connector), 0),
			(0, raw(endpoint), raw(added), 0),
		]);
		assert_walk(&fixture, endpoint, &[
			(connector, fixture.nodes[1]),
			(added, fixture.nodes[0]),
		]);
		assert_bulk_image(&fixture);
	}
}
