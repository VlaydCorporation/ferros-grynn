//! Independent exclusive/shared readset authority regressions.

use super::{
	ClassSchema,
	ConflictKey,
	Datastore,
	GlobalAtomId,
	GraphKind,
	GraphRef,
	IsolationLevel,
	Transaction,
	TransactionAccess,
	TransactionOptions,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphRead},
	values::{PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Seeds two independent records and one snapshot-borrowed graph reader.
fn fixture() -> FerrosGrynnResult<(Datastore, [RecordId; 2], GraphRef, PropertyKeyId)> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("reader", AtomKind::Node, [])?)?;
	let key = seed.intern_property("value")?;
	let records = ["reader:a".parse::<RecordId>()?, "reader:b".parse::<RecordId>()?];
	for record in &records {
		let mut properties = PropertyMap::new();
		properties.insert(key, 1);
		seed.create_atom(record.clone(), properties)?;
	}
	let graph = seed.define_graph("read_tracking", GraphKind::Graph)?;
	seed.attach_node(graph.id(), &records[0], None, None)?;
	seed.commit()?;
	Ok((store, records, graph, key))
}

/// Projects owned values without relying on GQL or tracker implementation.
fn value(
	transaction: &mut Transaction,
	record: &RecordId,
	key: PropertyKeyId,
) -> FerrosGrynnResult<Value> {
	transaction
		.atom(record)?
		.and_then(|atom| atom.properties().get(key).cloned())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("read fixture value missing"))
}

/// Both access modes retain the first revision, including private writes and
/// savepoint restoration, and reject an independent conflicting writer.
#[test]
fn exclusive_shared_reads_preserve_savepoint_and_ssi() {
	use crate::test_assertions::AssertResult as _;

	let (store, records, graph, property) = fixture().assert_ok();
	let mut reader = store.begin();
	assert_eq!(value(&mut reader, &records[0], property).assert_ok(), 1.into());
	let global = reader
		.working
		.resolve_atom(&records[0])
		.ok_or_else(|| FerrosGrynnError::invalid_argument("read fixture global identity missing"))
		.assert_ok();
	let key = ConflictKey::BaseAtom(global);
	let first = reader.snapshot.version(key);
	assert_eq!(reader.reads.lock().get(&key), Some(&first));
	{
		let local = reader
			.working
			.membership(graph.id(), global)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("read fixture membership missing"))
			.assert_ok();
		let view = graph.read(&reader).assert_ok();
		assert!(view.node(local).assert_ok().is_some());
	}
	reader.record_read(key);
	let saved_reads = reader.reads.lock().clone();
	let saved_cache = reader.last_base_read;
	let savepoint = reader.gql_savepoint();
	let absent = ConflictKey::BaseAtom(GlobalAtomId::from_storage(999).assert_ok());
	reader.record_read_exclusive(absent);
	assert_eq!(reader.reads.lock().get(&absent), Some(&0));
	assert_eq!(value(&mut reader, &records[1], property).assert_ok(), 1.into());
	reader.restore_gql_savepoint(savepoint);
	assert_eq!(*reader.reads.lock(), saved_reads);
	assert_eq!(reader.last_base_read, saved_cache);
	let mut writer = store.begin();
	writer.set_base_property(&records[0], property, 2.into()).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&records[0], property, 3.into()).assert_ok();
	reader.record_read_exclusive(key);
	reader.record_read(key);
	assert_eq!(value(&mut reader, &records[0], property).assert_ok(), 3.into());
	assert_eq!(reader.reads.lock().get(&key), Some(&first));
	let Err(error) = reader.commit() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("stale read unexpectedly committed"));
			test_result.assert_ok();
		};
	};
	assert_eq!(error.status().code().to_string(), "FG-40001");
}

/// Refresh replaces both the recorded revision and the repeated-read cache.
#[test]
fn exclusive_read_committed_refreshes_revision_and_cache() {
	use crate::test_assertions::AssertResult as _;

	let (store, records, _, property) = fixture().assert_ok();
	let mut reader = store.begin_with(TransactionOptions::new(
		IsolationLevel::ReadCommitted,
		TransactionAccess::ReadOnly,
	));
	assert_eq!(value(&mut reader, &records[0], property).assert_ok(), 1.into());
	let global = reader
		.working
		.resolve_atom(&records[0])
		.ok_or_else(|| FerrosGrynnError::invalid_argument("read fixture global identity missing"))
		.assert_ok();
	let key = ConflictKey::BaseAtom(global);
	let first = reader.snapshot.version(key);
	let mut writer = store.begin();
	writer.set_base_property(&records[0], property, 2.into()).assert_ok();
	let stamp = writer.commit().assert_ok().commit_ts();
	assert_eq!(value(&mut reader, &records[0], property).assert_ok(), 2.into());
	assert!(reader.snapshot.version(key) > first);
	assert_eq!(reader.reads.lock().get(&key), Some(&stamp));
	assert_eq!(reader.last_base_read, Some(global));
	reader.commit().assert_ok();
}

/// A disjoint write proves SSI validation independently of write-write FCW.
#[test]
fn exclusive_atom_read_rejects_disjoint_write_antidependency() {
	use crate::test_assertions::AssertResult as _;

	let (store, records, _, property) = fixture().assert_ok();
	let mut reader = store.begin();
	assert_eq!(value(&mut reader, &records[0], property).assert_ok(), 1.into());
	let mut writer = store.begin();
	writer.set_base_property(&records[0], property, 2.into()).assert_ok();
	writer.commit().assert_ok();
	reader.set_base_property(&records[1], property, 3.into()).assert_ok();
	let Err(error) = reader.commit() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("stale read disjoint write committed"));
			test_result.assert_ok();
		};
	};
	assert_eq!(error.status().code().to_string(), "FG-40001");
}
