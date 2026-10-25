//! Old directory readers retain a WAL-only source across the next publication.

use super::{support::*, *};
use crate::storage::{
	graph_pages::decode_subgraph_directory,
	image_io::read_pinned_page_cached,
	page::AlignedPage,
};

/// A Sync reader pins the first directory WAL version before another child
/// write.
#[test]
fn bounded_child_directory_sync_pins_wal_source_through_passive() {
	wal_source(IoMode::Sync);
}

/// NativeAsync retains the same old WAL directory version through checkpoint.
#[cfg(feature = "native-async-io")]
#[test]
fn bounded_child_directory_native_async_pins_wal_source_through_passive() {
	wal_source(IoMode::NativeAsync);
}

/// The retained source is newer than home and older than the next directory
/// frame.
fn wal_source(io_mode: IoMode) {
	let mut fixture = fixture(io_mode, 3);
	candidate(&fixture, &[(0, 0)]).commit().assert_ok();
	let root = fixture.store.inner.current_root();
	let store = local(&fixture.store);
	let pin = store.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let home = store.state.lock().database_superblock.checkpoint_lsn;
	assert!(pin.0.snapshot.visible_lsn() > home);
	let parents = [fixture.parents[0].id(), fixture.parents[1].id(), fixture.intermediary.id()];
	let sources = parents.map(|parent| {
		let files = files(&fixture, parent);
		let key = PageKey::new(files.space(), files.subgraphs, PageNo::new(0));
		let file = store.state.lock().registry.resolve(key).assert_ok();
		let expected = directory(&fixture, parent);
		assert_eq!(expected.len(), 1);
		(key, file, expected)
	});
	let keys = sources.iter().map(|(key, _, _)| *key).collect::<Vec<_>>();
	let old_versions = {
		let visible = store.wal.visible_frames(&keys, pin.0.snapshot);
		visible
			.frames()
			.iter()
			.map(|frame| {
				let frame = frame.assert_some();
				assert!(frame.frame_lsn() > home);
				frame.frame_lsn()
			})
			.collect::<Vec<_>>()
	};
	candidate(&fixture, &[(0, 1)]).commit().assert_ok();
	{
		let visible = store.wal.visible_frames(&keys, store.wal.snapshot());
		for (frame, old) in visible.frames().iter().zip(old_versions) {
			assert!(frame.assert_some().frame_lsn() > old);
		}
	}
	assert_directories(&fixture);
	for checkpoint in [false, true] {
		if checkpoint {
			assert!(fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
			let advanced = store.state.lock().database_superblock.checkpoint_lsn;
			assert!(advanced > home && advanced <= pin.0.snapshot.visible_lsn());
		}
		for (key, file, expected) in &sources {
			let mut page = AlignedPage::zeroed();
			read_pinned_page_cached(
				&store.wal,
				file,
				&store.engine,
				pin.0.snapshot,
				pin.0.home_generation,
				pin.0.home_ceiling,
				*key,
				&mut page,
			)
			.assert_ok();
			let actual = decode_subgraph_directory(&page, key.page_no()).assert_ok();
			assert_eq!(&actual, &expected[0]);
		}
		assert_directories(&fixture);
	}
	drop(sources);
	drop(pin);
	drop(root);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_directories(&fixture);
	let graph = fixture.children[0].id();
	let root = fixture.store.inner.current_root();
	for record in &fixture.pending {
		assert!(root.membership(graph, root.resolve_atom(record).assert_some()).is_some());
	}
}
