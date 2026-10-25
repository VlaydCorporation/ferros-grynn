//! Stable payloads must share the physical publication memory envelope.

use super::*;

/// Sync must reject an oversized payload before publication.
#[test]
fn overlay_budget_sync_rejects_large_owned_payload() {
	overlay_budget(IoMode::Sync);
}

/// Actual NativeAsync uses the same shared resource policy after reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_budget_native_async_rejects_large_owned_payload() {
	overlay_budget(IoMode::NativeAsync);
}

/// An unindexed class separates payload admission from scalar sort budgets.
fn overlay_budget(io_mode: IoMode) {
	let (mut fixture, _) = membership_fixture(io_mode);
	let records = seed_payloads(&fixture, 1);
	let record = &records[0];
	fixture.options.maintenance.max_memory_bytes =
		std::num::NonZeroUsize::new(512 * 1024).assert_some();
	fixture = reopen_checkpointed(fixture, io_mode);
	let previous = bytes(16);
	let mut control = fixture.store.begin();
	control.set_overlay(fixture.graph.id(), record, fixture.keys[1], previous.clone()).assert_ok();
	control.commit().assert_ok();
	let local = local_store(&fixture.store);
	let stamp = fixture.store.inner.current_root().commit_ts;
	let boundary = local.wal.snapshot().visible_lsn();
	let files = local.state.lock().file_catalog.files.clone();
	let directories = generation_directories(&fixture.directory.0);
	let mut write = fixture.store.begin();
	write.set_overlay(fixture.graph.id(), record, fixture.keys[1], bytes(1024 * 1024)).assert_ok();
	let result = write.commit();
	assert!(result.is_err(), "oversized overlay escaped shared publication budget");
	let error = result.assert_err();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
	));
	assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
	assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
	assert_eq!(local.state.lock().file_catalog.files, files);
	assert_eq!(generation_directories(&fixture.directory.0), directories);
	assert_overlay_property(
		&fixture.store,
		fixture.graph.id(),
		record,
		fixture.keys[1],
		previous.clone(),
	);
	fixture = reopen_checkpointed(fixture, io_mode);
	assert_overlay_property(&fixture.store, fixture.graph.id(), record, fixture.keys[1], previous);
}

/// Separate small overlays still share their page backing and WAL reservations.
#[test]
fn overlay_budget_sync_rejects_aggregate_and_mixed_pages() {
	aggregate_overlays(IoMode::Sync);
}

/// NativeAsync has the same aggregate envelope with and without membership DML.
#[cfg(feature = "native-async-io")]
#[test]
fn overlay_budget_native_async_rejects_aggregate_and_mixed_pages() {
	aggregate_overlays(IoMode::NativeAsync);
}

/// Positive controls distinguish payload accumulation from constant setup
/// costs.
fn aggregate_overlays(io_mode: IoMode) {
	for mixed in [false, true] {
		let (mut fixture, pending) = membership_fixture(io_mode);
		let records = seed_payloads(&fixture, 8);
		fixture.options.maintenance.max_memory_bytes =
			std::num::NonZeroUsize::new(512 * 1024).assert_some();
		fixture = reopen_checkpointed(fixture, io_mode);
		let graph = fixture.graph.id();
		let previous = bytes(16 * 1024);
		let mut control = fixture.store.begin();
		control.set_overlay(graph, &records[0], fixture.keys[1], previous.clone()).assert_ok();
		if mixed {
			control.attach_node(graph, &pending[0], None, None).assert_ok();
		}
		control.commit().assert_ok();
		let local = local_store(&fixture.store);
		let stamp = fixture.store.inner.current_root().commit_ts;
		let boundary = local.wal.snapshot().visible_lsn();
		let files = local.state.lock().file_catalog.files.clone();
		let directories = generation_directories(&fixture.directory.0);
		let mut old = fixture.store.begin();
		let mut write = fixture.store.begin();
		for record in &records {
			write
				.set_overlay(
					graph,
					record,
					fixture.keys[1],
					Value::Bytes(vec![9; 16 * 1024].into()),
				)
				.assert_ok();
		}
		if mixed {
			write.detach_occurrence(graph, &pending[0], false).assert_ok();
		}
		let error = write.commit().assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
		));
		assert_eq!(fixture.store.inner.current_root().commit_ts, stamp);
		assert_eq!(local.wal.snapshot().visible_lsn(), boundary);
		assert_eq!(local.state.lock().file_catalog.files, files);
		assert_eq!(generation_directories(&fixture.directory.0), directories);
		assert_eq!(
			fixture.store.begin().membership(graph, &pending[0]).assert_ok().is_some(),
			mixed
		);
		for (index, record) in records.iter().enumerate() {
			let expected = if index == 0 {
				previous.clone()
			} else {
				bytes(1)
			};
			assert_eq!(
				old.effective_property(graph, record, fixture.keys[1]).assert_ok(),
				Some(expected.clone())
			);
			assert_overlay_property(&fixture.store, graph, record, fixture.keys[1], expected);
		}
		drop(old);
		fixture = reopen_checkpointed(fixture, io_mode);
		assert_eq!(
			fixture.store.begin().membership(graph, &pending[0]).assert_ok().is_some(),
			mixed
		);
		for (index, record) in records.iter().enumerate() {
			assert_overlay_property(
				&fixture.store,
				graph,
				record,
				fixture.keys[1],
				if index == 0 {
					previous.clone()
				} else {
					bytes(1)
				},
			);
		}
	}
}

/// Unindexed payload owners avoid involving scalar-sort resource policy.
fn seed_payloads(fixture: &LabelFixture, count: usize) -> Vec<RecordId> {
	let mut seed = fixture.store.begin();
	seed.define_class(
		ClassSchema::new("probe", AtomKind::Node, [ClassField::new("payload", Type::Any)])
			.assert_ok(),
	)
	.assert_ok();
	let mut records = Vec::new();
	for index in 0..count {
		let record = format!("probe:payload{index}").parse::<RecordId>().assert_ok();
		let mut properties = PropertyMap::new();
		properties.insert(fixture.keys[1], bytes(1));
		seed.create_atom(record.clone(), properties).assert_ok();
		seed.attach_node(fixture.graph.id(), &record, None, None).assert_ok();
		records.push(record);
	}
	seed.commit().assert_ok();
	records
}

/// Owned bytes exercise the payload-copy path without text interning.
fn bytes(length: usize) -> Value {
	Value::Bytes(crate::values::Bytes::from(vec![7; length]))
}
