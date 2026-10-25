//! IANA names must survive sparse publication and exact physical snapshots.

use super::*;
use crate::{
	datastore::{IoMode, SchemaParameter},
	graph::{AtomId, GraphRef},
	values::{DateTime, PropertyKeyId},
};

/// Real local files and two records isolate unchanged and changed payloads.
struct Fixture {
	/// Production datastore owning the files.
	store: Datastore,
	/// Requested mode is checked after create and reopen.
	options: LocalOptions,
	/// Graph containing the first record.
	graph: GraphRef,
	/// Physical occurrence addressed by snapshot reads.
	owner: AtomId,
	/// First record keeps the original IANA identifier.
	first: RecordId,
	/// Independent mutation target.
	second: RecordId,
	/// Shared property slot.
	key: PropertyKeyId,
	/// Fields drop in declaration order: release db.lock before directory
	/// cleanup.
	directory: TestDirectory,
}

/// Initial Base decoding must receive the table already stored in the catalog.
#[test]
fn timezone_initial_base_cold_open_preserves_name() {
	for &mode in modes() {
		let fixture = seed(mode, false);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		let reopened = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_mode(&reopened, mode);
		assert_base(&reopened, &fixture.first, fixture.key, "Europe/London");
	}
}

/// An unrelated text replacement must retain every immutable payload's IDs.
#[test]
fn timezone_sparse_text_commit_retains_published_prefix() {
	for &mode in modes() {
		let fixture = seed(mode, false);
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut write = fixture.store.begin();
		write.set_base_property(&fixture.second, fixture.key, Value::from("changed")).assert_ok();
		write.commit().assert_ok();
		assert_names(&fixture.store, &["Europe/London"]);
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		let reopened = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_mode(&reopened, mode);
		assert_base(&reopened, &fixture.first, fixture.key, "Europe/London");
	}
}

/// A new zone appends instead of reusing an unchanged record's identifier.
#[test]
fn timezone_sparse_new_zone_appends_without_reassignment() {
	for &mode in modes() {
		let fixture = seed(mode, false);
		let files = local(&fixture.store).state.lock().file_catalog.files.clone();
		let mut write = fixture.store.begin();
		write.set_base_property(&fixture.second, fixture.key, datetime("Asia/Tokyo")).assert_ok();
		write.commit().assert_ok();
		assert_names(&fixture.store, &["Europe/London", "Asia/Tokyo"]);
		assert_eq!(local(&fixture.store).state.lock().file_catalog.files, files);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		let reopened = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_mode(&reopened, mode);
		assert_base(&reopened, &fixture.first, fixture.key, "Europe/London");
		assert_base(&reopened, &fixture.second, fixture.key, "Asia/Tokyo");
	}
}

/// Recovery resolves a new payload with the catalog published in the same WAL.
#[test]
fn timezone_sparse_wal_recovery_preserves_both_names() {
	for &mode in modes() {
		let fixture = seed(mode, false);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		let local = local(&fixture.store);
		let boundary = local.wal.snapshot().visible_lsn();
		let properties = local.state.lock().bindings.atoms.properties;
		let mut write = fixture.store.begin();
		write.set_base_property(&fixture.second, fixture.key, datetime("Asia/Tokyo")).assert_ok();
		write.commit().assert_ok();
		assert!(
			local.wal.recovery_frames().iter().any(|frame| {
				frame.frame_lsn() > boundary && frame.key().file_id() == properties
			})
		);
		// No explicit checkpoint follows the new payload and catalog
		// publication.
		drop(fixture.store);
		let reopened = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_mode(&reopened, mode);
		assert_base(&reopened, &fixture.first, fixture.key, "Europe/London");
		assert_base(&reopened, &fixture.second, fixture.key, "Asia/Tokyo");
	}
}

/// Catalog-only publication uses the same prefix before priming new values.
#[test]
fn timezone_catalog_only_commit_retains_and_extends_prefix() {
	for &mode in modes() {
		let fixture = seed(mode, false);
		let mut write = fixture.store.begin();
		write.intern_property("unused").assert_ok();
		write.commit().assert_ok();
		assert_names(&fixture.store, &["Europe/London"]);
		let mut write = fixture.store.begin();
		write
			.define_parameter(SchemaParameter::new("zone", datetime("Asia/Tokyo")).assert_ok())
			.assert_ok();
		write.commit().assert_ok();
		assert_names(&fixture.store, &["Europe/London", "Asia/Tokyo"]);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		let reopened = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_mode(&reopened, mode);
		assert_base(&reopened, &fixture.first, fixture.key, "Europe/London");
	}
}

/// The physical Base decoder needs authority even before the first mutation.
#[test]
fn timezone_physical_base_reads_published_authority() {
	for &mode in modes() {
		let fixture = seed(mode, false);
		assert_physical(&fixture, "Europe/London");
	}
}

/// Old overlay pages keep their table when the new generation reuses ID one.
#[test]
fn timezone_physical_overlay_pins_authority_across_generation_replacement() {
	for &mode in modes() {
		let fixture = seed(mode, true);
		let mut old = fixture.store.begin();
		let snapshot = old.query_snapshot().assert_ok();
		let initial = old
			.query_physical_fields(&snapshot, &fixture.graph, fixture.owner)
			.assert_ok()
			.assert_some();
		assert_datetime(initial.properties.get(fixture.key).assert_some(), "Europe/London");
		let mut write = fixture.store.begin();
		write
			.set_overlay(fixture.graph.id(), &fixture.first, fixture.key, datetime("Asia/Tokyo"))
			.assert_ok();
		write.define_graph("force_generation", GraphKind::Graph).assert_ok();
		write.commit().assert_ok();
		// The full image contains no London value: current ID one is now Tokyo.
		assert_names(&fixture.store, &["Asia/Tokyo"]);
		fixture.store.checkpoint(CheckpointMode::Passive).assert_ok();
		let retained = old
			.query_physical_fields(&snapshot, &fixture.graph, fixture.owner)
			.assert_ok()
			.assert_some();
		assert_datetime(retained.properties.get(fixture.key).assert_some(), "Europe/London");
		assert_physical(&fixture, "Asia/Tokyo");
		drop(snapshot);
		drop(old);
		fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
		drop(fixture.store);
		let reopened = Datastore::open_local(&fixture.directory.0, fixture.options).assert_ok();
		assert_mode(&reopened, mode);
		let mut read = reopened.begin();
		let snapshot = read.query_snapshot().assert_ok();
		let fields = read
			.query_physical_fields(&snapshot, &fixture.graph, fixture.owner)
			.assert_ok()
			.assert_some();
		assert_datetime(fields.properties.get(fixture.key).assert_some(), "Asia/Tokyo");
	}
}

/// No implicit backend fallback may satisfy either local transport scenario.
fn modes() -> &'static [IoMode] {
	#[cfg(feature = "native-async-io")]
	return &[IoMode::Sync, IoMode::NativeAsync];
	#[cfg(not(feature = "native-async-io"))]
	&[IoMode::Sync]
}

/// Publishes the original zone in a full image, avoiding a sparse-path setup.
fn seed(mode: IoMode, overlay: bool) -> Fixture {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode: mode,
		buffer_pool_pages: 16,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_mode(&store, mode);
	store.inner.maintenance.shutdown();
	let mut write = store.begin();
	write.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = write.define_graph("g", GraphKind::Graph).assert_ok();
	let key = write.intern_property("value").assert_ok();
	let first = "item:a".parse::<RecordId>().assert_ok();
	let second = "item:b".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(
		key,
		if overlay {
			Value::from("base")
		} else {
			datetime("Europe/London")
		},
	);
	write.create_atom(first.clone(), properties).assert_ok();
	let owner = write.attach_node(graph.id(), &first, None, None).assert_ok();
	if overlay {
		write.set_overlay(graph.id(), &first, key, datetime("Europe/London")).assert_ok();
	}
	let mut properties = PropertyMap::new();
	properties.insert(key, Value::from("seed"));
	write.create_atom(second.clone(), properties).assert_ok();
	write.commit().assert_ok();
	assert_names(&store, &["Europe/London"]);
	Fixture {
		store,
		options,
		graph,
		owner,
		first,
		second,
		key,
		directory,
	}
}

/// Equal instants deliberately expose the insufficiency of ordinary value Eq.
fn datetime(zone: &str) -> Value {
	let text = match zone {
		"Europe/London" => "2025-01-01T00:00:00+00:00[Europe/London]",
		"Asia/Tokyo" => "2025-01-01T09:00:00+09:00[Asia/Tokyo]",
		_ => unreachable!("fixture has two IANA zones"),
	};
	Value::from(text.parse::<DateTime>().assert_ok())
}

/// Compare both the represented instant and its persisted IANA identity.
fn assert_datetime(value: &Value, zone: &str) {
	let actual = value.as_datetime().assert_some().as_inner();
	let expected = datetime(zone);
	let expected = expected.as_datetime().assert_some().as_inner();
	assert_eq!(actual.timestamp(), expected.timestamp());
	assert_eq!(actual.time_zone().iana_name(), Some(zone));
}

/// Logical Base reads are independent of physical traversal composition.
fn assert_base(store: &Datastore, record: &RecordId, key: PropertyKeyId, zone: &str) {
	let mut read = store.begin();
	let atom = read.atom(record).assert_ok().assert_some();
	assert_datetime(atom.properties().get(key).assert_some(), zone);
}

/// Exercise the datastore caller's decoder through the actual physical path.
fn assert_physical(fixture: &Fixture, zone: &str) {
	let mut read = fixture.store.begin();
	let snapshot = read.query_snapshot().assert_ok();
	let fields = read
		.query_physical_fields(&snapshot, &fixture.graph, fixture.owner)
		.assert_ok()
		.assert_some();
	assert_datetime(fields.properties.get(fixture.key).assert_some(), zone);
}

/// Prefix checks isolate publication failures from later Base open errors.
fn assert_names(store: &Datastore, names: &[&str]) {
	let state = local(store).state.lock();
	assert_eq!(
		state._database_catalog.time_zones.iter().map(|name| name.as_str()).collect::<Vec<_>>(),
		names
	);
}

/// Only the production local backend is admitted by this fixture.
fn local(store: &Datastore) -> &super::super::LocalStore {
	match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local timezone fixture uses RAM backend"),
	}
}

/// Effective mode is checked after every create and reopen.
fn assert_mode(store: &Datastore, mode: IoMode) {
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, mode);
}
