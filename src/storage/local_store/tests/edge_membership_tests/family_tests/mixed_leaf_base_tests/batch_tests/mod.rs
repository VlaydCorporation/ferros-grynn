//! Two detached Base owners and one leaf replacement share a retained slice.

use super::*;

/// The second record is independent of the selected container and first Base.
fn companion(case: &Case) -> (RecordId, GlobalAtomId) {
	let record = "loose:n17".parse::<RecordId>().assert_ok();
	let root = case.fixture.store.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	assert_ne!(global, case.global);
	assert!(root.memberships(global).is_none_or(|members| members.is_empty()));
	assert_eq!(
		root.atom(global).assert_some().properties().get(case.key),
		Some(&Value::from(17_i64))
	);
	(record, global)
}

/// Distinct new keys avoid turning the physical regression into Unique denial.
fn batch_change(case: &Case, other: &(RecordId, GlobalAtomId), operation: u8) -> Transaction {
	let mut write = change(case, operation);
	write.set_base_property(&other.0, case.key, 1001_i64.into()).assert_ok();
	write
}

/// Check both full identities and values before relying on file oracles.
fn assert_batch_current(
	case: &Case,
	other: &(RecordId, GlobalAtomId),
	operation: u8,
	child: GraphId,
) {
	let root = case.fixture.store.inner.current_root();
	for (record, global, value) in [
		(&case.record, case.global, (operation != 1).then(|| Value::from(1000_i64))),
		(&other.0, other.1, Some(Value::from(1001_i64))),
	] {
		assert_eq!(root.resolve_atom(record), Some(global));
		assert!(root.memberships(global).is_none_or(|members| members.is_empty()));
		assert_eq!(root.atom(global).assert_some().properties().get(case.key), value.as_ref());
	}
	assert_eq!(
		root.occurrence(case.fixture.graph.id(), case.selected).assert_some().child_graph(),
		Some(child)
	);
	drop(root);
	assert_bulk_image(&case.fixture);
	crate::storage::local_store::tests::base_lifecycle_tests::assert_base_raw_postings(
		&case.fixture.store,
	);
}

/// The independent second posting must retain its old version as well.
fn assert_companion_postings(
	case: &Case,
	other: &(RecordId, GlobalAtomId),
	pin: &LocalQuerySnapshot,
	old: bool,
) {
	let root = case.fixture.store.inner.current_root();
	let storage = local(&case.fixture.store);
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary).assert_ok();
	for index in case.indexes {
		let decoded = storage.physical_index(pin, index).assert_ok().assert_some();
		if let PhysicalIndexEntries::Count(count) = &decoded.entries {
			assert_eq!(*count, 32);
			continue;
		}
		let PhysicalIndexEntries::Scalar(groups) = &decoded.entries else {
			unreachable!("batch fixture decoded another index kind");
		};
		for value in [17_i64, 1001] {
			let key = crate::values::storage::encode_index_key(
				&[(&Value::from(value), &Type::Int)],
				&mut context,
			)
			.assert_ok();
			let actual = groups
				.iter()
				.filter(|group| group.key == key)
				.flat_map(|group| group.atoms.iter().map(|atom| (atom.kind, atom.slot)))
				.collect::<Vec<_>>();
			let expected = ((old && value == 17) || (!old && value == 1001))
				.then_some((other.1.kind().assert_some(), other.1.slot()))
				.into_iter()
				.collect::<Vec<_>>();
			assert_eq!(actual, expected);
		}
	}
}

/// Logical fallback success must not conceal replacement of stable files.
fn functional_batch(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for operation in 0..3 {
				let mut case = case_fixture(mode, family, kind);
				let other = companion(&case);
				let storage = local(&case.fixture.store);
				let source = case.fixture.store.inner.current_root();
				let prefix = property_prefix(&case);
				let mut old_read = case.fixture.store.begin();
				let old_query = old_read.query_snapshot().assert_ok();
				let old_physical =
					storage.capture_query_snapshot(source.commit_ts).assert_ok().assert_some();
				let (files, home) = {
					let state = storage.state.lock();
					(state.file_catalog.files.clone(), state.database_superblock.checkpoint_lsn)
				};
				let boundary = storage.wal.snapshot().visible_lsn();
				let clock = storage.wal.snapshot().commit_ts().get();
				assert_eq!(clock, source.commit_ts);
				assert!(boundary > home);
				batch_change(&case, &other, operation).commit().assert_ok();
				assert_batch_current(&case, &other, operation, case.children[1]);
				assert_eq!(case.fixture.store.inner.current_root().commit_ts, source.commit_ts + 1);
				assert_eq!(storage.wal.snapshot().commit_ts().get(), clock + 1);
				let observed = storage.state.lock().file_catalog.files.clone();
				let retained = files.len() == observed.len()
					&& files.iter().all(|file| observed.contains(file));
				println!(
					"MIXED_BASE_BATCH_LOGICAL {mode:?} {family:?} {kind:?} operation={operation} \
					 records=2 retained={retained}"
				);
				assert!(retained, "MIXED_BASE_BATCH_REBUILT_DATA_FILES");
				assert_prefix(&case, &prefix);
				let current = storage
					.capture_query_snapshot(case.fixture.store.inner.current_root().commit_ts)
					.assert_ok()
					.assert_some();
				assert!(case.fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
				let advanced = storage.state.lock().database_superblock.checkpoint_lsn;
				assert!(advanced > home && advanced <= boundary);
				assert_eq!(
					old_read
						.query_physical_inner_graph(&old_query, &case.fixture.graph, case.selected)
						.assert_ok()
						.assert_some()
						.graph,
					Some(case.children[0])
				);
				for (record, value) in [(&case.record, 16_i64), (&other.0, 17)] {
					assert_eq!(
						old_read.atom(record).assert_ok().assert_some().properties().get(case.key),
						Some(&Value::from(value))
					);
				}
				assert_postings(&case, &old_physical, true, operation);
				assert_postings(&case, &current, false, operation);
				assert_companion_postings(&case, &other, &old_physical, true);
				assert_companion_postings(&case, &other, &current, false);
				drop((current, old_physical, old_query, old_read, source));
				case.fixture = reopen(case.fixture);
				assert_batch_current(&case, &other, operation, case.children[1]);
				assert_files(&case, &files);
				assert_prefix(&case, &prefix);
				case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				case.fixture = reopen(case.fixture);
				assert_batch_current(&case, &other, operation, case.children[1]);
				assert_files(&case, &files);
				assert_prefix(&case, &prefix);
				println!(
					"MIXED_BASE_BATCH_FUNCTIONAL_PASS {mode:?} {family:?} {kind:?} \
					 operation={operation}"
				);
			}
		}
	}
}

/// Two Base setters without incidence already use the production point writer.
fn base_only(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			let mut case = case_fixture(mode, family, kind);
			let other = companion(&case);
			let files = local(&case.fixture.store).state.lock().file_catalog.files.clone();
			let mut write = case.fixture.store.begin();
			write.set_base_property(&case.record, case.key, 1000_i64.into()).assert_ok();
			write.set_base_property(&other.0, case.key, 1001_i64.into()).assert_ok();
			write.commit().assert_ok();
			assert_batch_current(&case, &other, 0, case.children[0]);
			assert_files(&case, &files);
			case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
			case.fixture = reopen(case.fixture);
			assert_batch_current(&case, &other, 0, case.children[0]);
			assert_files(&case, &files);
			println!("MIXED_BASE_BATCH_BASE_ONLY_PASS {mode:?} {family:?} {kind:?}");
		}
	}
}

/// Sync must retain the complete file set for all batch owner families.
#[test]
fn mixed_base_batch_sync_retains_files_and_pins() {
	functional_batch(IoMode::Sync);
}

/// Actual NativeAsync exercises identical publication and recovery semantics.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_base_batch_native_async_retains_files_and_pins() {
	functional_batch(IoMode::NativeAsync);
}

/// The existing Base-only batch is an independent positive control.
#[test]
fn mixed_base_batch_sync_base_only_control() {
	base_only(IoMode::Sync);
}

/// The Base-only control also preserves requested native mode on cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_base_batch_native_async_base_only_control() {
	base_only(IoMode::NativeAsync);
}

/// Both Base images and their exact replay targets share the chosen incidence.
fn batch_roots(
	case: &Case,
	other: &(RecordId, GlobalAtomId),
	leaf: bool,
) -> (DatabaseRoot, DatabaseRoot, PublicationDelta) {
	let (before, mut after, mut delta) = publication_roots(case, true, leaf);
	after.set_base_property(other.1, case.key, 1001_i64.into()).assert_ok();
	delta.records.insert(other.0.clone());
	delta.global_slots.insert(other.1.slot());
	(before, after, delta)
}

/// Recovery must restore both Base sides, never just the selected first row.
fn assert_batch_old(case: &Case, other: &(RecordId, GlobalAtomId)) {
	assert_old(case);
	let root = case.fixture.store.inner.current_root();
	assert_eq!(root.resolve_atom(&other.0), Some(other.1));
	assert!(root.memberships(other.1).is_none_or(|members| members.is_empty()));
	assert_eq!(
		root.atom(other.1).assert_some().properties().get(case.key),
		Some(&Value::from(17_i64))
	);
}

/// Batch-specific fault points validate both records in one WAL decision.
mod recovery_tests;

/// The complete two-Base and graph plans share the opened resource policy.
mod resource_tests;

/// A corrupt second Base source cannot publish the first Base or leaf.
mod source_tests;

/// Replay preserves an independently committed Base row.
mod replay_tests;
