//! Ordinary membership commits must retain unrelated physical generations.

use super::*;

/// Slot boundaries, reuse and transient state exercise the same durable path.
mod boundary_tests;
/// Selected tree replacements share the ordinary atomic publication barrier.
mod fallback_tests;
/// Global NODE lifecycle and its local membership share one publication.
mod global_lifecycle_tests;
/// Failure, uncertain recovery and builder-admission boundaries.
mod recovery_tests;
use crate::{
	datastore::{DataSpaceId, IoMode},
	storage::{local_store::PhysicalIndexEntries, types::DataFileKind},
};

/// The first attachment exercises new class metadata and existing index roots.
#[test]
fn isolated_node_membership_point_publication_retains_unrelated_files() {
	isolated_node_membership_publication(IoMode::Sync);
}

/// NativeAsync must remain selected through both create and cold reopen.
#[cfg(feature = "native-async-io")]
#[test]
fn isolated_node_membership_native_async_point_publication_retains_unrelated_files() {
	isolated_node_membership_publication(IoMode::NativeAsync);
}

/// The same durable oracle covers both supported Windows transports.
fn isolated_node_membership_publication(io_mode: IoMode) {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut seed = store.begin();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	let other = seed.define_graph("other", GraphKind::Graph).assert_ok();
	let value = define_membership_class(&mut seed, graph.id());
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut properties = PropertyMap::new();
	properties.insert(value, 10_i64);
	seed.create_atom(record.clone(), properties).assert_ok();
	seed.attach_node(other.id(), &record, None, None).assert_ok();
	seed.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	store.inner.maintenance.shutdown();
	let local = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let files = local.state.lock().file_catalog.files.clone();
	let mut write = store.begin();
	let occurrence = write.attach_node(graph.id(), &record, None, None).assert_ok();
	assert_eq!(occurrence.slot(), 0);
	assert_eq!(occurrence.generation(), 0);
	write.commit().assert_ok();
	for entry in &files {
		let selected = (entry.space_id == DataSpaceId::Graph(graph.id())
			&& matches!(
				entry.file_id.kind(),
				Some(DataFileKind::Index | DataFileKind::Statistics)
			))
			|| (entry.space_id == DataSpaceId::Database
				&& entry.file_id.kind() == Some(DataFileKind::Index)
				&& entry.path.as_str().ends_with("/memberships.fgidx"));
		if !selected {
			assert!(
				local.state.lock().file_catalog.files.contains(entry),
				"node membership rebuilt unrelated file: {}",
				entry.path.as_str()
			);
		}
	}
	let root = store.inner.current_root();
	let count = root
		.schema
		.class("item")
		.assert_some()
		.indexes()
		.find(|index| index.kind() == ClassIndexKind::Count)
		.assert_some()
		.id()
		.assert_some();
	let snapshot = local.capture_query_snapshot(root.commit_ts).assert_ok().assert_some();
	let physical = local.physical_index(&snapshot, count).assert_ok().assert_some();
	assert!(matches!(physical.entries, PhysicalIndexEntries::Count(1)));
	drop(snapshot);
	drop(physical);
	drop(root);
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).assert_ok();
	assert_eq!(reopened.local_storage_status().assert_some().options().io_mode, io_mode);
	let root = reopened.inner.current_root();
	let global = root.resolve_atom(&record).assert_some();
	assert_eq!(root.membership(graph.id(), global), Some(occurrence));
	assert!(root.membership(other.id(), global).is_some());
}

/// An empty persisted array can outlive its decoded lookup bucket after reopen.
#[test]
fn isolated_node_membership_empty_class_source_survives_sync_reopen() {
	for analyze in [false, true] {
		empty_class_reopen_cycle(IoMode::Sync, analyze);
	}
}

/// NativeAsync must preserve the same empty-source ownership across ANALYZE.
#[cfg(feature = "native-async-io")]
#[test]
fn isolated_node_membership_empty_class_source_survives_native_async_reopen() {
	for analyze in [false, true] {
		empty_class_reopen_cycle(IoMode::NativeAsync, analyze);
	}
}

/// Detaches the last member before rebuilding the lazy physical source owner.
fn empty_class_reopen_cycle(io_mode: IoMode, analyze: bool) {
	use crate::datastore::{MaintenanceAnalyzeMode, MaintenanceTarget};
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	let record = "item:a".parse::<RecordId>().assert_ok();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
	seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	seed.attach_node(graph.id(), &record, None, None).assert_ok();
	seed.commit().assert_ok();
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &record, false).assert_ok();
	remove.commit().assert_ok();
	if analyze {
		store
			.run_foreground_maintenance_with_analysis(
				MaintenanceJobKind::Analyze,
				MaintenanceTarget::Graph(graph.id()),
				options.maintenance,
				Some(MaintenanceAnalyzeMode::Full),
			)
			.assert_ok();
	}
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	store.inner.maintenance.shutdown();
	assert_eq!(store.local_storage_status().assert_some().options().io_mode, io_mode);
	let mut attach = store.begin();
	let local = attach.attach_node(graph.id(), &record, None, None).assert_ok();
	assert_eq!((local.slot(), local.generation()), (0, 1));
	attach.commit().assert_ok();
	let backend = match &store.inner.backend {
		StoreBackend::Local(local) => Some(local),
		StoreBackend::InMemory => None,
	}
	.assert_some();
	let class = store.inner.current_root().schema.class("item").assert_some().id().assert_some();
	let suffix = format!("/idx/cls_{}.fgidx", class.get());
	assert_eq!(
		backend
			.state
			.lock()
			.file_catalog
			.files
			.iter()
			.filter(|entry| {
				entry.space_id == DataSpaceId::Graph(graph.id())
					&& entry.path.as_str().ends_with(&suffix)
			})
			.count(),
		1,
		"cold source must not leave duplicate canonical class arrays"
	);
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &record, false).assert_ok();
	remove.commit().assert_ok();
	store.checkpoint(CheckpointMode::Restart).assert_ok();
	drop(store);
	let store = Datastore::open_local(&directory.0, options).assert_ok();
	let root = store.inner.current_root();
	assert!(root.membership(graph.id(), root.resolve_atom(&record).assert_some()).is_none());
	assert_eq!(root.graph(graph.id()).assert_some().slot_generation(AtomKind::Node, 0), Some(2));
}

/// Shared scalar scopes keep production membership fixtures consistent.
fn define_membership_class(
	seed: &mut crate::datastore::Transaction,
	graph: crate::graph::GraphId,
) -> crate::values::PropertyKeyId {
	let mut class =
		ClassSchema::new("item", AtomKind::Node, [ClassField::new("value", Type::Int)]).assert_ok();
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
		class = class.with_index(ClassIndex::new(name, kind, scope, field).assert_ok()).assert_ok();
	}
	seed.define_class(class).assert_ok();
	seed.intern_property("value").assert_ok()
}

/// Keeps backend extraction out of the independent physical oracles.
fn local_store(store: &Datastore) -> &crate::storage::local_store::LocalStore {
	match &store.inner.backend {
		StoreBackend::Local(local) => local,
		StoreBackend::InMemory => unreachable!("local membership fixture has a RAM backend"),
	}
}
