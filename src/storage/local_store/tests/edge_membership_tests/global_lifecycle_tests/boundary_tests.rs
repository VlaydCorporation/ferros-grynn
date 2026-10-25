//! Global/EDGE Hot, endpoint roles and derived NODE chains cross together.

use super::*;

/// Separate global/local page boundaries retain untouched file identities.
#[test]
fn global_edge_hot_and_adjacency_boundaries_match_bulk() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		for count in [101_u64, 102, 337, 338] {
			let mut fixture = fixture(io_mode);
			let mut seed = fixture.store.begin();
			let mut expected = Vec::new();
			for index in 0..count {
				let record = format!("edge:s{index:04}").parse::<RecordId>().assert_ok();
				let value = i64::try_from(index).assert_ok();
				insert_edge(
					&fixture,
					&mut seed,
					&record,
					value,
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				);
				expected.push((record, value));
			}
			seed.commit().assert_ok();
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
			let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
			let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
			let mut write = fixture.store.begin();
			for index in count..count + 2 {
				let record = format!("edge:s{index:04}").parse::<RecordId>().assert_ok();
				let value = i64::try_from(index).assert_ok();
				let edge = insert_edge(
					&fixture,
					&mut write,
					&record,
					value,
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				);
				assert_eq!((edge.slot(), edge.generation()), (index, 0));
				expected.push((record, value));
			}
			write.commit().assert_ok();
			assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
			let (global, edges): (&[u64], &[u64]) = match count {
				101 => (&[0], &[0, 1]),
				102 => (&[0], &[1]),
				337 => (&[0, 1], &[3]),
				338 => (&[1], &[3]),
				_ => unreachable!("boundary fixture changed its domain"),
			};
			assert_hot_frames(&fixture, boundary, global, edges);
			assert_indexes(&fixture, "edge", &expected);
			support::assert_bulk_image(&fixture);
			fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			fixture = support::reopen(fixture);
			assert_indexes(&fixture, "edge", &expected);
			support::assert_bulk_image(&fixture);
		}
	}
}

/// Repeated roles, self loops and all plain directions retain multiplicity.
#[test]
fn global_edge_topology_roles_and_directions_match_bulk() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let mut write = fixture.store.begin();
		let [a, b] = fixture.nodes;
		let topologies = [
			EdgeTopology::directed([a, a, b], [b, a, b]),
			EdgeTopology::undirected([a, a, b, b]),
			EdgeTopology::bidirectional([a], [b]),
			EdgeTopology::directed([a], [a]),
			EdgeTopology::directed([b, b, b], [a, a, a]),
		];
		let mut expected = Vec::new();
		for (index, topology) in topologies.into_iter().enumerate() {
			let record = format!("edge:t{index}").parse::<RecordId>().assert_ok();
			let value = i64::try_from(index).assert_ok();
			insert_edge(&fixture, &mut write, &record, value, topology);
			expected.push((record, value));
		}
		let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
		let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
		write.commit().assert_ok();
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		assert_hot_frames(&fixture, boundary, &[0], &[0]);
		assert_indexes(&fixture, "edge", &expected);
		support::assert_bulk_image(&fixture);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		assert_indexes(&fixture, "edge", &expected);
		support::assert_bulk_image(&fixture);
	}
}

/// Real leaf overflow replaces selected trees atomically with Base/topology.
#[test]
fn global_edge_selected_tree_fallback_is_atomic() {
	for io_mode in [IoMode::Sync, IoMode::NativeAsync] {
		if io_mode == IoMode::NativeAsync && !cfg!(feature = "native-async-io") {
			continue;
		}
		let mut fixture = fixture(io_mode);
		let local = support::local(&fixture.store);
		let files = local.state.lock().file_catalog.files.clone();
		let primary = local.state.lock()._database_catalog.atom_store.primary.file_id;
		let make_write = || {
			let mut write = fixture.store.begin();
			for index in 0..1024 {
				insert_edge(
					&fixture,
					&mut write,
					&format!("edge:f{index:04}").parse::<RecordId>().assert_ok(),
					i64::from(index),
					EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
				);
			}
			write
		};
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let directories = generation_directories(&fixture.directory.0);
		{
			let write = make_write();
			let _failure = GenerationFailureGuard::set(GenerationFailurePoint::BeforeWalCommit);
			write.commit().assert_err();
		}
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
		make_write().commit().assert_ok();
		assert_ne!(local.state.lock()._database_catalog.atom_store.primary.file_id, primary);
		assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
		support::assert_bulk_image(&fixture);
		let expected = (0..1024)
			.map(|index| {
				(format!("edge:f{index:04}").parse::<RecordId>().assert_ok(), i64::from(index))
			})
			.collect::<Vec<_>>();
		assert_indexes(&fixture, "edge", &expected);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		fixture = support::reopen(fixture);
		assert_indexes(&fixture, "edge", &expected);
		support::assert_bulk_image(&fixture);
	}
}
