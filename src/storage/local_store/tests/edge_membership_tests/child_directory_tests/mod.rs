//! Direct/shared containment directories retain page identity across child DML.

use super::{support::local, *};

/// Valid noncanonical source page-chain layout remains stable.
mod chain_tests;
/// Coupled NODE/EDGE count changes, CASCADE and transient allocator ceilings.
mod lifecycle_tests;
/// Fixed logical admission and unchanged publication boundary on rejection.
mod resource_tests;
/// Old WAL-only directory versions survive subsequent writes and checkpoint.
mod wal_pin_tests;

/// Concrete source graph and independent bulk directory oracle.
mod support;

/// Malformed physical sources and atomic WAL failure outcomes.
mod recovery_tests;

use support::{assert_directories, candidate, directory, files, fixture, prepared, reopen};

/// Two A entries share page zero; B occupies the 511th EDGE-owned entry.
#[test]
fn bounded_child_directory_shared_parents_and_510_boundary() {
	let mut fixture = fixture(IoMode::Sync, 511);
	assert_directories(&fixture);
	let parent = fixture.parents[0].id();
	let other = fixture.parents[1].id();
	let intermediary = fixture.intermediary.id();
	let ancestor = fixture.ancestor.id();
	let parent_files = files(&fixture, parent);
	let other_files = files(&fixture, other);
	let intermediary_files = files(&fixture, intermediary);
	let ancestor_files = files(&fixture, ancestor);
	let ancestor_source = directory(&fixture, ancestor);
	let source = directory(&fixture, parent);
	assert_eq!(source.len(), 2);
	assert_eq!((source[0].1.len(), source[1].1.len()), (510, 1));
	let first_expected = [
		PageKey::new(parent_files.space(), parent_files.subgraphs, PageNo::new(0)),
		PageKey::new(other_files.space(), other_files.subgraphs, PageNo::new(0)),
		PageKey::new(intermediary_files.space(), intermediary_files.subgraphs, PageNo::new(0)),
	]
	.into_iter()
	.collect::<BTreeSet<_>>();
	let mut second_expected = first_expected.clone();
	second_expected.insert(PageKey::new(
		parent_files.space(),
		parent_files.subgraphs,
		PageNo::new(1),
	));
	for (targets, expected) in
		[(&[(0, 0)][..], &first_expected), (&[(0, 1), (1, 1)][..], &second_expected)]
	{
		let plan = prepared(&fixture, targets);
		let unique = plan.pages.iter().map(|(key, _)| *key).collect::<BTreeSet<_>>();
		assert_eq!(unique.len(), plan.pages.len(), "duplicate prepared PageKey");
		let prepared_directories = unique
			.iter()
			.filter(|key| key.file_id().kind() == Some(DataFileKind::Subgraphs))
			.copied()
			.collect::<BTreeSet<_>>();
		assert_eq!(&prepared_directories, expected);
		drop(plan);
		let store = local(&fixture.store);
		let watched = first_expected
			.iter()
			.copied()
			.chain([
				PageKey::new(parent_files.space(), parent_files.subgraphs, PageNo::new(1)),
				PageKey::new(ancestor_files.space(), ancestor_files.subgraphs, PageNo::new(0)),
			])
			.collect::<Vec<_>>();
		let headers = {
			let state = store.state.lock();
			[parent, other, intermediary, ancestor]
				.map(|graph| *state.graph_superblocks.get(&graph).assert_some())
		};
		let boundary = store.wal.snapshot().visible_lsn();
		candidate(&fixture, targets).commit().assert_ok();
		let changed = {
			let visible = store.wal.visible_frames(&watched, store.wal.snapshot());
			visible
				.frames()
				.iter()
				.flatten()
				.filter(|frame| frame.frame_lsn() > boundary)
				.map(|frame| frame.key())
				.collect::<BTreeSet<_>>()
		};
		assert_eq!(&changed, expected);
		let state = store.state.lock();
		for (graph, header) in [parent, other, intermediary, ancestor].into_iter().zip(headers) {
			assert_eq!(state.graph_superblocks.get(&graph), Some(&header));
		}
		for original in [parent_files, other_files, intermediary_files, ancestor_files] {
			for file in original.regular_files().into_iter().chain([original.superblock]) {
				assert!(
					state.registry.contains(original.space(), file),
					"CHILD_DIRECTORY_REBUILT_DATA_FILES"
				);
			}
		}
		drop(state);
		assert_directories(&fixture);
		assert_eq!(directory(&fixture, ancestor), ancestor_source);
		if targets.len() == 1 {
			assert_eq!(directory(&fixture, parent)[1], source[1]);
		}
	}
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = reopen(fixture);
	assert_directories(&fixture);
	assert_eq!(directory(&fixture, ancestor), ancestor_source);
	assert_eq!(files(&fixture, parent).subgraphs, parent_files.subgraphs);
}
