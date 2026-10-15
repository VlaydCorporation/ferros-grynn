#![cfg(test)]

//! Public event definition persistence and owner retirement on local backends.

#![cfg(windows)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn::{
	datastore::{
		ClassField,
		ClassSchema,
		Datastore,
		EventDefinition,
		EventTarget,
		IoMode,
		LocalOptions,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, Type, Uuid, Value},
};

/// Exercises metadata-only publication, cold reopening and source retirement.
fn event_catalog_reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

	let directory = std::env::temp_dir().join(format!(
		"fg-s29-events-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX),
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options)?;
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			Type::Int,
		)])?)?;
	setup.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
	let graph = setup.define_graph("scope", GraphKind::Graph)?;
	setup.commit()?;
	let value = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("published value field"))?;
	let class_target = EventTarget::Class(class);
	let graph_target = EventTarget::Graph(graph.id());
	let mut registration = store.begin();
	let class_event = registration.define_event(EventDefinition::new(
		"audit",
		class_target,
		"$event = 'CREATE'",
		"{ CREATE audit:cold CONTENT { value: $after.value }; }",
	)?)?;
	let graph_event =
		registration.define_event(EventDefinition::new("audit", graph_target, "true", "1")?)?;
	registration.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut update = store.begin();
	(
		&(update.named_event(class_target, "audit")?.and_then(|event| event.id())),
		&(Some(class_event)),
	)
		.assert_equal();
	(
		&(update.named_event(graph_target, "audit")?.and_then(|event| event.id())),
		&(Some(graph_event)),
	)
		.assert_equal();
	update.create_atom("item:cold".parse()?, PropertyMap::from_iter([(value, Value::from(7))]))?;
	(
		&(update
			.atom(&"audit:cold".parse()?)?
			.and_then(|atom| atom.properties().get(value).cloned())),
		&(Some(Value::from(7))),
	)
		.assert_equal();
	update.replace_event(class_event, EventDefinition::new("audit", class_target, "true", "2")?)?;
	update.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut remove = store.begin();
	(&(remove.event(class_event)?.map(|event| event.version().get())), &(Some(2))).assert_equal();
	(
		&(remove
			.atom(&"audit:cold".parse()?)?
			.and_then(|atom| atom.properties().get(value).cloned())),
		&(Some(Value::from(7))),
	)
		.assert_equal();
	remove.delete_atom(&"item:cold".parse()?)?;
	remove.remove_class("item")?;
	remove.remove_graph("scope")?;
	(remove.event(class_event)?.is_none()).assert_true();
	(remove.event(graph_event)?.is_none()).assert_true();
	remove.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut read = store.begin();
	(read.event(class_event)?.is_none()).assert_true();
	(read.event(graph_event)?.is_none()).assert_true();
	drop(read);
	drop(store);
	std::fs::remove_dir_all(directory).map_err(|source| {
		FerrosGrynnError::storage_io_failure("remove event test database")
			.with_technical_source(source)
	})?;
	Ok(())
}

/// Verifies event definitions reopen and retire sync.
#[test]
fn event_definitions_reopen_and_retire_sync() -> FerrosGrynnResult<()> {
	event_catalog_reopen(IoMode::Sync)
}

/// Verifies event definitions reopen and retire native async.
#[cfg(feature = "native-async-io")]
#[test]
fn event_definitions_reopen_and_retire_native_async() -> FerrosGrynnResult<()> {
	event_catalog_reopen(IoMode::NativeAsync)
}
