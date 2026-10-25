//! Replay merges independent Base edits before preparing one batch publication.

use super::*;

/// A pending batch uses the accepted root after an independent commit.
fn replay(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let mut case = case_fixture(mode, family, kind);
			let other = companion(&case);
			let independent = "loose:n18".parse::<RecordId>().assert_ok();
			let identity =
				case.fixture.store.inner.current_root().resolve_atom(&independent).assert_some();
			let pending = batch_change(&case, &other, 0);
			let mut concurrent = case.fixture.store.begin();
			concurrent.set_base_property(&independent, case.key, 1002_i64.into()).assert_ok();
			concurrent.commit().assert_ok();
			let store = local(&case.fixture.store);
			let source = case.fixture.store.inner.current_root();
			let files = store.state.lock().file_catalog.files.clone();
			let prefix = property_prefix(&case);
			let mut old = case.fixture.store.begin();
			let query = old.query_snapshot().assert_ok();
			pending.commit().assert_ok();
			assert_batch_current(&case, &other, 0, case.children[1]);
			assert_eq!(case.fixture.store.inner.current_root().commit_ts, source.commit_ts + 1);
			assert_eq!(store.wal.snapshot().commit_ts().get(), source.commit_ts + 1);
			assert_files(&case, &files);
			assert_prefix(&case, &prefix);
			assert_eq!(
				old.query_physical_inner_graph(&query, &case.fixture.graph, case.selected)
					.assert_ok()
					.assert_some()
					.graph,
				Some(case.children[0])
			);
			for (record, value) in [(&case.record, 16_i64), (&other.0, 17), (&independent, 1002)] {
				assert_eq!(
					old.atom(record).assert_ok().assert_some().properties().get(case.key),
					Some(&Value::from(value))
				);
			}
			drop((query, old, source));
			case.fixture = reopen(case.fixture);
			assert_batch_current(&case, &other, 0, case.children[1]);
			let root = case.fixture.store.inner.current_root();
			assert_eq!(root.resolve_atom(&independent), Some(identity));
			assert_eq!(
				root.atom(identity).assert_some().properties().get(case.key),
				Some(&Value::from(1002_i64))
			);
			drop(root);
			assert_files(&case, &files);
			case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			case.fixture = reopen(case.fixture);
			assert_batch_current(&case, &other, 0, case.children[1]);
			let root = case.fixture.store.inner.current_root();
			assert_eq!(root.resolve_atom(&independent), Some(identity));
			assert_eq!(
				root.atom(identity).assert_some().properties().get(case.key),
				Some(&Value::from(1002_i64))
			);
			assert_files(&case, &files);
			assert_prefix(&case, &prefix);
			println!("MIXED_BASE_BATCH_REPLAY_PASS {mode:?} {family:?} {kind:?}");
		}
	}
}

/// Sync merges both batch rows without overwriting an independently changed
/// row.
#[test]
fn mixed_base_batch_sync_replays_after_independent_writer() {
	replay(IoMode::Sync);
}

/// NativeAsync uses final accepted roots and preserves the independent row.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_base_batch_native_async_replays_after_independent_writer() {
	replay(IoMode::NativeAsync);
}
