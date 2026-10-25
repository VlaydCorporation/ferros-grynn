//! Base EDGE lifecycle and its local topology publish through one WAL commit.

use super::*;
use crate::storage::{
	catalog::FileCatalogEntry,
	local_store::{PhysicalIndexEntries, tests::base_lifecycle_tests::assert_base_raw_postings},
};

/// Hot, role and adjacency boundaries include selected tree replacement.
mod boundary_tests;
/// Slot reuse, replay and multiple reverse memberships retain one authority.
mod lifecycle_tests;
/// New Base NODE/EDGE owners compose in one replayed incidence publication.
mod mixed_tests;
/// Exact Hot frame cuts and canonical checkpoint authority.
mod oracles;
/// Known/uncertain WAL failures and resource/source rejection remain atomic.
mod recovery_tests;
/// Consumed global and local EDGE slots remain durable without a live owner.
mod transient_tests;
use oracles::{assert_class_slots, assert_hot_frames};

/// The first Base EDGE must retain unrelated global and graph file identities.
#[test]
fn global_edge_create_attach_sync_retains_files_and_reopens() {
	create_attach(IoMode::Sync);
}

/// NativeAsync remains selected for ordinary create and every cold open.
#[cfg(feature = "native-async-io")]
#[test]
fn global_edge_create_attach_native_async_retains_files_and_reopens() {
	create_attach(IoMode::NativeAsync);
}

/// Deleting a Base EDGE removes its topology through the same publication.
#[test]
fn global_edge_delete_cascade_sync_retains_files_and_reopens() {
	delete_cascade(IoMode::Sync);
}

/// NativeAsync must retain old physical topology while publishing deletion.
#[cfg(feature = "native-async-io")]
#[test]
fn global_edge_delete_cascade_native_async_retains_files_and_reopens() {
	delete_cascade(IoMode::NativeAsync);
}

/// All schema and index work precedes the observed ordinary DML commit.
fn fixture(io_mode: IoMode) -> support::Fixture {
	let fixture = warm_fixture(io_mode);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	support::reopen(fixture)
}

/// Warm and cold cases share the same schema and zero-EDGE source.
fn warm_fixture(io_mode: IoMode) -> support::Fixture {
	let fixture = support::fixture(io_mode, 0, 0);
	let mut seed = fixture.store.begin();
	seed.define_graph("other", GraphKind::Graph).assert_ok();
	seed.define_field("edge", ClassField::new("value", Type::Int)).assert_ok();
	define_indexes(&mut seed, "edge", fixture.graph.id());
	seed.commit().assert_ok();
	fixture
}

/// Both replacement classes use the same four scalar publication contracts.
fn define_indexes(seed: &mut Transaction, class: &str, graph: GraphId) {
	for (name, kind, scope, field) in [
		("base", ClassIndexKind::Standard, IndexScope::Base, Some("value")),
		(
			"effective",
			ClassIndexKind::Standard,
			IndexScope::Effective {
				graph,
			},
			Some("value"),
		),
		(
			"unique",
			ClassIndexKind::Unique,
			IndexScope::Effective {
				graph,
			},
			Some("value"),
		),
		(
			"count",
			ClassIndexKind::Count,
			IndexScope::Effective {
				graph,
			},
			None,
		),
	] {
		seed.define_index(class, ClassIndex::new(name, kind, scope, field).assert_ok()).assert_ok();
	}
}

/// The shared production sequence retains its accepted local EDGE identity.
fn candidate(fixture: &support::Fixture, record: &RecordId, value: i64) -> (Transaction, AtomId) {
	let mut write = fixture.store.begin();
	let edge = insert_edge(
		fixture,
		&mut write,
		record,
		value,
		EdgeTopology::directed([fixture.nodes[0]], [fixture.nodes[1]]),
	);
	(write, edge)
}

/// Batch fixtures use the same public create/attach sequence as point DML.
fn insert_edge(
	fixture: &support::Fixture,
	write: &mut Transaction,
	record: &RecordId,
	value: i64,
	topology: EdgeTopology,
) -> AtomId {
	let key = fixture.store.inner.current_root().dictionary.get("value").assert_some();
	let mut properties = PropertyMap::new();
	properties.insert(key, value);
	write.create_atom(record.clone(), properties).assert_ok();
	write.attach_edge(fixture.graph.id(), record, None, topology, None, None).assert_ok()
}

/// Existing NODE snapshots must remain empty after the first EDGE is published.
fn create_attach(io_mode: IoMode) {
	let mut fixture = fixture(io_mode);
	let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
	let mut old = fixture.store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	let record = "edge:first".parse::<RecordId>().assert_ok();
	let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
	let (write, edge) = candidate(&fixture, &record, 10);
	write.commit().assert_ok();
	assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
	assert_hot_frames(&fixture, boundary, &[0], &[0]);
	assert_indexes(&fixture, "edge", &[(record.clone(), 10)]);
	assert_class_slots(&fixture, "edge", &[0]);
	support::assert_bulk_image(&fixture);
	assert_adjacency(&old, &snapshot, &fixture.graph, &fixture.nodes, None);
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_adjacency(&old, &snapshot, &fixture.graph, &fixture.nodes, None);
	drop(snapshot);
	drop(old);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = support::reopen(fixture);
	let root = fixture.store.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	assert_eq!(root.membership(fixture.graph.id(), global), Some(edge));
	assert_indexes(&fixture, "edge", &[(record, 10)]);
	support::assert_bulk_image(&fixture);
}

/// A pinned physical occurrence remains readable after Base CASCADE deletion.
fn delete_cascade(io_mode: IoMode) {
	let mut fixture = fixture(io_mode);
	let record = "edge:old".parse::<RecordId>().assert_ok();
	let (write, edge) = candidate(&fixture, &record, 10);
	write.commit().assert_ok();
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = support::reopen(fixture);
	let files = support::local(&fixture.store).state.lock().file_catalog.files.clone();
	let mut old = fixture.store.begin();
	let snapshot = old.query_snapshot().assert_ok();
	let boundary = support::local(&fixture.store).wal.snapshot().visible_lsn();
	let mut write = fixture.store.begin();
	write.delete_atom_cascade(&record, true).assert_ok();
	write.commit().assert_ok();
	assert_fixed_files(&fixture.store, &files, &[fixture.graph.id()]);
	assert_hot_frames(&fixture, boundary, &[0], &[0]);
	assert_indexes(&fixture, "edge", &[]);
	assert_class_slots(&fixture, "edge", &[]);
	support::assert_bulk_image(&fixture);
	assert_adjacency(&old, &snapshot, &fixture.graph, &fixture.nodes, Some(edge));
	fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
	assert_adjacency(&old, &snapshot, &fixture.graph, &fixture.nodes, Some(edge));
	drop(snapshot);
	drop(old);
	fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	fixture = support::reopen(fixture);
	assert!(fixture.store.inner.current_root().resolve_atom(&record).is_none());
	assert_indexes(&fixture, "edge", &[]);
	support::assert_bulk_image(&fixture);
}

/// Only selected indexes and target class-statistics metadata may be replaced.
fn assert_fixed_files(store: &Datastore, source: &[FileCatalogEntry], graphs: &[GraphId]) {
	let state = support::local(store).state.lock();
	for entry in source {
		let selected = entry.file_id.kind() == Some(DataFileKind::Index)
			&& (entry.space_id == DataSpaceId::Database
				|| graphs.iter().any(|graph| entry.space_id == DataSpaceId::Graph(*graph)))
			|| graphs.iter().any(|graph| entry.space_id == DataSpaceId::Graph(*graph))
				&& entry.file_id.kind() == Some(DataFileKind::Statistics);
		if !selected {
			assert!(
				state.file_catalog.files.contains(entry),
				"rebuilt unrelated file: {}",
				entry.path.as_str()
			);
		}
	}
}

/// Raw global identities and owned physical probes are independent of Hot bulk.
fn assert_indexes(fixture: &support::Fixture, class: &str, expected: &[(RecordId, i64)]) {
	assert_base_raw_postings(&fixture.store);
	let root = fixture.store.inner.current_root();
	let local = support::local(&fixture.store);
	let physical = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let mut checked = 0;
	for index in root.schema.class(class).assert_some().indexes() {
		checked += 1;
		let entries =
			local.physical_index(&physical, index.id().assert_some()).assert_ok().assert_some();
		match &entries.entries {
			PhysicalIndexEntries::Count(count) => {
				assert_eq!(*count, u64::try_from(expected.len()).assert_ok());
			}
			PhysicalIndexEntries::Scalar(groups) => {
				assert_eq!(
					groups.iter().map(|group| group.atoms.len()).sum::<usize>(),
					expected.len()
				);
				for (record, value) in expected {
					let scope = (index.scope() != IndexScope::Base).then_some(&fixture.graph);
					let mut cursor = read
						.query_index_probe_cursor(
							&snapshot,
							class,
							scope,
							index.id().assert_some(),
							&[Value::from(*value)],
						)
						.assert_ok();
					assert_eq!(cursor.next_row().assert_ok().assert_some().record_id, *record);
					assert!(cursor.next_row().assert_ok().is_none());
				}
			}
			_ => unreachable!("scalar EDGE fixture changed index kind"),
		}
	}
	assert_eq!(checked, 4);
}
