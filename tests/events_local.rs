//! Public event definition persistence and owner retirement on local backends.

#![cfg(windows)]

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
	let directory = std::env::temp_dir().join(format!(
		"fg-s29-events-{:016x}",
		Uuid::now_v7().as_inner().as_u128() & u128::from(u64::MAX),
	));
	let mut options = LocalOptions::default();
	options.io_mode = mode;
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory, options.clone())?;
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
	let store = Datastore::open_local(&directory, options.clone())?;
	let mut update = store.begin();
	assert_eq!(
		update.named_event(class_target, "audit")?.and_then(|event| event.id()),
		Some(class_event)
	);
	assert_eq!(
		update.named_event(graph_target, "audit")?.and_then(|event| event.id()),
		Some(graph_event)
	);
	update.create_atom("item:cold".parse()?, PropertyMap::from_iter([(value, Value::from(7))]))?;
	assert_eq!(
		update.atom(&"audit:cold".parse()?)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(7))
	);
	update.replace_event(class_event, EventDefinition::new("audit", class_target, "true", "2")?)?;
	update.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options.clone())?;
	let mut remove = store.begin();
	assert_eq!(remove.event(class_event)?.map(|event| event.version().get()), Some(2));
	assert_eq!(
		remove.atom(&"audit:cold".parse()?)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(7))
	);
	remove.delete_atom(&"item:cold".parse()?)?;
	remove.remove_class("item")?;
	remove.remove_graph("scope")?;
	assert!(remove.event(class_event)?.is_none());
	assert!(remove.event(graph_event)?.is_none());
	remove.commit()?;
	drop(store);
	let store = Datastore::open_local(&directory, options)?;
	let mut read = store.begin();
	assert!(read.event(class_event)?.is_none());
	assert!(read.event(graph_event)?.is_none());
	drop(read);
	drop(store);
	std::fs::remove_dir_all(directory).map_err(|source| {
		FerrosGrynnError::storage_io_failure("remove event test database")
			.with_technical_source(source)
	})?;
	Ok(())
}

#[test]
fn event_definitions_reopen_and_retire_sync() -> FerrosGrynnResult<()> {
	event_catalog_reopen(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn event_definitions_reopen_and_retire_native_async() -> FerrosGrynnResult<()> {
	event_catalog_reopen(IoMode::NativeAsync)
}
