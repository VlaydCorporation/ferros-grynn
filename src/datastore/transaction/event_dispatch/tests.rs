//! Synchronous event behavior through the public SDK and common GQL kernel.

use std::num::NonZeroUsize;

use crate::{
	datastore::{ClassSchema, Datastore, EventDefinition, EventTarget},
	gql::{ExecutionControl, QueryExecutionOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomKind,
		EdgeEndpoint,
		EdgeRead,
		EdgeTopology,
		GraphKind,
		GraphRead,
		QualifiedAtomId,
	},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Value},
};

/// Creates a record class, audit sink and owned property-name key.
fn setup() -> FerrosGrynnResult<(Datastore, EventTarget, PropertyKeyId)> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("audit", AtomKind::Node, [])?)?;
	let value = tx.intern_property("value")?;
	tx.commit()?;
	Ok((store, EventTarget::Class(class), value))
}

#[test]
fn event_sync_sdk_context_noop_and_delete() -> FerrosGrynnResult<()> {
	let (store, target, value) = setup()?;
	let mut tx = store.begin();
	for (name, kind) in [("created", "CREATE"), ("updated", "UPDATE"), ("deleted", "DELETE")] {
		tx.define_event(EventDefinition::new(
			name,
			target,
			format!("$event = '{kind}'"),
			format!(
				"{{ CREATE audit:{name} CONTENT {{ old: $before?.value, new: $after?.value, \
				 value: $value.value, input: $input }}; }}"
			),
		)?)?;
	}
	tx.commit()?;
	let record: RecordId = "item:a".parse()?;
	let mut write = store.begin();
	write.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))?;
	assert!(write.atom(&"audit:created".parse()?)?.is_some());
	write.set_base_property(&record, value, Value::from(1))?;
	assert!(write.atom(&"audit:updated".parse()?)?.is_none());
	write.set_base_property(&record, value, Value::from(2))?;
	write.delete_atom(&record)?;
	write.commit()?;
	let mut read = store.begin();
	let dictionary = store.property_dictionary();
	let old =
		dictionary.get("old").ok_or_else(|| FerrosGrynnError::invalid_argument("audit old key"))?;
	let new =
		dictionary.get("new").ok_or_else(|| FerrosGrynnError::invalid_argument("audit new key"))?;
	let update = read
		.atom(&"audit:updated".parse()?)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("audit update missing"))?;
	assert_eq!(update.properties().get(old), Some(&Value::from(1)));
	assert_eq!(update.properties().get(new), Some(&Value::from(2)));
	assert!(read.atom(&"audit:deleted".parse()?)?.is_some());
	assert!(read.atom(&record)?.is_none());
	Ok(())
}

#[test]
fn event_sync_error_restores_original_and_handler_effects() -> FerrosGrynnResult<()> {
	let (store, target, value) = setup()?;
	let mut tx = store.begin();
	tx.define_event(EventDefinition::new(
		"reject",
		target,
		"$after.value = 2",
		"{ CREATE audit:partial; THROW 'blocked'; }",
	)?)?;
	tx.commit()?;
	let record: RecordId = "item:a".parse()?;
	let mut write = store.begin();
	write.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))?;
	assert!(write.set_base_property(&record, value, Value::from(2)).is_err());
	assert_eq!(
		write.atom(&record)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(1))
	);
	assert!(write.atom(&"audit:partial".parse()?)?.is_none());
	write.set_base_property(&record, value, Value::from(3))?;
	write.commit()?;
	assert_eq!(
		store.begin().atom(&record)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(3))
	);
	Ok(())
}

#[test]
fn event_sync_nested_mutations_reenter_dispatch() -> FerrosGrynnResult<()> {
	let (store, target, value) = setup()?;
	let mut tx = store.begin();
	tx.define_event(EventDefinition::new(
		"increment",
		target,
		"$after.value < 3",
		"{ UPDATE item:root SET value = $after.value + 1; }",
	)?)?;
	tx.commit()?;
	let mut write = store.begin();
	let record: RecordId = "item:root".parse()?;
	write.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(0))]))?;
	assert_eq!(
		write.atom(&record)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(3))
	);
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_inherited_graph_update_and_detach() -> FerrosGrynnResult<()> {
	let (store, _target, value) = setup()?;
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph)?;
	let record: RecordId = "item:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))?;
	tx.attach_node(graph.id(), &record, None, None)?;
	tx.define_event(EventDefinition::new(
		"changed",
		EventTarget::Graph(graph.id()),
		"$event = 'UPDATE'",
		"{ CREATE audit:inherited CONTENT { old: $before.value, new: $after.value }; }",
	)?)?;
	tx.define_event(EventDefinition::new(
		"detached",
		EventTarget::Graph(graph.id()),
		"$event = 'DELETE'",
		"{ CREATE audit:detached; }",
	)?)?;
	tx.commit()?;
	let mut write = store.begin();
	write.set_base_property(&record, value, Value::from(2))?;
	write.detach_occurrence(graph.id(), &record, false)?;
	assert!(write.atom(&"audit:inherited".parse()?)?.is_some());
	assert!(write.atom(&"audit:detached".parse()?)?.is_some());
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_handlers_share_loop_gas() -> FerrosGrynnResult<()> {
	let (store, target, _value) = setup()?;
	let mut tx = store.begin();
	for name in ["a", "b"] {
		tx.define_event(EventDefinition::new(
			name,
			target,
			"true",
			"{ FOR $n IN 0..400 { LET $x = 1; }; }",
		)?)?;
	}
	tx.commit()?;
	let mut options = QueryExecutionOptions::default();
	options.max_loop_iterations = 600;
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	let record: RecordId = "item:limited".parse()?;
	assert!(
		write
			.with_event_control(&control, |tx| tx.create_atom(record.clone(), PropertyMap::new()))
			.is_err()
	);
	assert!(write.atom(&record)?.is_none());
	Ok(())
}

#[test]
fn event_sync_recursion_failure_restores_depth_and_data() -> FerrosGrynnResult<()> {
	let (store, target, value) = setup()?;
	let mut tx = store.begin();
	tx.define_event(EventDefinition::new(
		"cycle",
		target,
		"true",
		"{ UPDATE item:root SET value = $after.value + 1; }",
	)?)?;
	tx.commit()?;
	let mut options = QueryExecutionOptions::default();
	options.max_control_depth = 4;
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	let record: RecordId = "item:root".parse()?;
	assert!(
		write
			.with_event_control(&control, |tx| tx
				.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(0))])))
			.is_err()
	);
	assert!(write.atom(&record)?.is_none());
	assert_eq!(write.event_depth, 0);
	Ok(())
}

#[test]
fn event_sync_gql_handler_keeps_lexical_frame_private() -> FerrosGrynnResult<()> {
	let (store, target, _value) = setup()?;
	let mut tx = store.begin();
	tx.define_event(EventDefinition::new(
		"created",
		target,
		"$event = 'CREATE'",
		"{ LET $private = $after.value; CREATE audit:gql CONTENT { value: $private }; }",
	)?)?;
	tx.commit()?;
	let mut write = store.begin();
	for result in
		write.execute("CREATE item:gql CONTENT { value: 7 }; RETURN 1;", &Object::new())?
	{
		let _value = result.result?;
	}
	assert!(write.atom(&"audit:gql".parse()?)?.is_some());
	assert!(!write.gql_bindings().contains_key("private"));
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_empty_snapshot_observes_concurrent_registration() -> FerrosGrynnResult<()> {
	let (store, target, _value) = setup()?;
	let mut write = store.begin();
	write.create_atom("item:stale".parse()?, PropertyMap::new())?;
	let mut definition = store.begin();
	definition.define_event(EventDefinition::new("new", target, "true", "1")?)?;
	definition.commit()?;
	assert!(write.commit().is_err());
	assert!(store.begin().atom(&"item:stale".parse()?)?.is_none());
	Ok(())
}

#[test]
fn event_sync_context_reads_detect_serializable_cycle() -> FerrosGrynnResult<()> {
	let (store, _target, value) = setup()?;
	let mut seed = store.begin();
	let graph = seed.define_graph("scope", GraphKind::Graph)?;
	let record: RecordId = "item:a".parse()?;
	seed.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))?;
	seed.attach_node(graph.id(), &record, None, None)?;
	seed.define_event(EventDefinition::new(
		"read_context",
		EventTarget::Graph(graph.id()),
		"$input?.x != NONE",
		"{ CREATE audit:receipt CONTENT { value: $after.value }; }",
	)?)?;
	let x = seed.intern_property("x")?;
	seed.commit()?;
	let mut first = store.begin();
	first.set_overlay(graph.id(), &record, x, Value::from(1))?;
	let mut second = store.begin();
	assert!(second.atom(&"audit:receipt".parse()?)?.is_none());
	second.set_base_property(&record, value, Value::from(2))?;
	second.commit()?;
	assert!(first.commit().is_err());
	assert!(store.begin().atom(&"audit:receipt".parse()?)?.is_none());
	Ok(())
}

#[test]
fn event_sync_raw_input_preserves_equal_fields_and_patch_removals() -> FerrosGrynnResult<()> {
	let (store, target, _value) = setup()?;
	let mut seed = store.begin();
	for result in
		seed.execute("CREATE item:a CONTENT { kept: 1, changed: 1, removed: 9 };", &Object::new())?
	{
		let _value = result.result?;
	}
	let id = seed.define_event(EventDefinition::new(
		"input",
		target,
		"$event = 'UPDATE'",
		"{ CREATE audit:merge CONTENT { input: $input }; }",
	)?)?;
	seed.commit()?;
	let mut write = store.begin();
	for result in write.execute("UPDATE item:a MERGE { kept: 1, changed: 2 };", &Object::new())? {
		let _value = result.result?;
	}
	write.replace_event(
		id,
		EventDefinition::new(
			"input",
			target,
			"$event = 'UPDATE'",
			"{ CREATE audit:patch CONTENT { input: $input }; }",
		)?,
	)?;
	for result in
		write.execute("UPDATE item:a PATCH { removed: NONE, changed: 3 };", &Object::new())?
	{
		let _value = result.result?;
	}
	write.commit()?;
	let input = store
		.property_dictionary()
		.get("input")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("input key"))?;
	let mut read = store.begin();
	let merge = read
		.atom(&"audit:merge".parse()?)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("merge receipt"))?;
	let patch = read
		.atom(&"audit:patch".parse()?)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("patch receipt"))?;
	assert_eq!(
		merge.properties().get(input),
		Some(&Value::from(Object::from_iter([("kept", 1), ("changed", 2)])))
	);
	assert_eq!(
		patch.properties().get(input),
		Some(&Value::from(Object::from_iter([
			("removed", Value::None),
			("changed", Value::from(3))
		])))
	);
	Ok(())
}

#[test]
fn event_sync_context_is_frozen_before_ordered_handlers() -> FerrosGrynnResult<()> {
	let (store, target, value) = setup()?;
	let mut seed = store.begin();
	let first = seed.define_event(EventDefinition::new(
		"z_first",
		target,
		"$event = 'CREATE'",
		"{ UPDATE item:a SET value = 99; CREATE audit:order CONTENT { value: 1 }; }",
	)?)?;
	let second = seed.define_event(EventDefinition::new(
		"a_second",
		target,
		"$event = 'CREATE'",
		"{ UPDATE audit:order SET value = value + 1; CREATE audit:frozen CONTENT { value: \
		 $after.value }; }",
	)?)?;
	assert!(first < second);
	seed.commit()?;
	let mut write = store.begin();
	write.create_atom("item:a".parse()?, PropertyMap::from_iter([(value, Value::from(1))]))?;
	assert_eq!(
		write.atom(&"item:a".parse()?)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(99))
	);
	assert_eq!(
		write.atom(&"audit:order".parse()?)?.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(2))
	);
	assert_eq!(
		write
			.atom(&"audit:frozen".parse()?)?
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(1))
	);
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_try_rollback_preserves_subsequent_dispatch() -> FerrosGrynnResult<()> {
	let (store, target, _value) = setup()?;
	let mut seed = store.begin();
	seed.define_event(EventDefinition::new(
		"reject",
		target,
		"$after.value = 2",
		"{ CREATE audit:partial; THROW 'blocked'; }",
	)?)?;
	seed.commit()?;
	let mut write = store.begin();
	for result in write.execute(
		"TRY { CREATE item:bad CONTENT { value: 2 }; } CATCH |$e| { $e.message; }; CREATE \
		 item:good CONTENT { value: 3 };",
		&Object::new(),
	)? {
		let _value = result.result?;
	}
	assert!(write.atom(&"item:bad".parse()?)?.is_none());
	assert!(write.atom(&"audit:partial".parse()?)?.is_none());
	assert!(write.atom(&"item:good".parse()?)?.is_some());
	assert_eq!(write.event_depth, 0);
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_context_copy_limit_rolls_back_before_handler() -> FerrosGrynnResult<()> {
	let (store, target, value) = setup()?;
	let mut seed = store.begin();
	seed.define_event(EventDefinition::new(
		"large",
		target,
		"true",
		"{ CREATE audit:unexpected; }",
	)?)?;
	seed.commit()?;
	let mut options = QueryExecutionOptions::default();
	options.barrier_memory_bytes = NonZeroUsize::new(1024 * 1024)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("scratch cap"))?;
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	let record: RecordId = "item:large".parse()?;
	let result = write.with_event_control(&control, |tx| {
		tx.create_atom(
			record.clone(),
			PropertyMap::from_iter([(value, Value::from("x".repeat(400 * 1024)))]),
		)
	});
	assert!(result.is_err());
	assert!(write.atom(&record)?.is_none());
	assert!(write.atom(&"audit:unexpected".parse()?)?.is_none());
	Ok(())
}

#[test]
fn event_sync_gql_relate_context_preserves_class_and_graph_contracts() -> FerrosGrynnResult<()> {
	let (store, _target, _value) = setup()?;
	let mut seed = store.begin();
	let edge_class = seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = seed.define_graph("scope", GraphKind::Graph)?;
	for record in ["item:a", "item:b"] {
		seed.create_atom(record.parse()?, PropertyMap::new())?;
	}
	seed.define_event(EventDefinition::new(
		"class_relate",
		EventTarget::Class(edge_class),
		"$event = 'RELATE'",
		"{ CREATE audit:class_relate CONTENT { event: $event, value: $value }; }",
	)?)?;
	seed.define_event(EventDefinition::new(
		"graph_relate",
		EventTarget::Graph(graph.id()),
		"$event = 'RELATE'",
		"{ CREATE audit:graph_relate CONTENT { before: $before, after: $after, value: $value }; }",
	)?)?;
	seed.commit()?;
	let mut write = store.begin();
	for result in
		write.execute("RELATE (item:a)-[link:e]->(item:b) IN GRAPH scope;", &Object::new())?
	{
		let _value = result.result?;
	}
	assert!(write.atom(&"audit:class_relate".parse()?)?.is_some());
	let dictionary = store.property_dictionary();
	let before = write.intern_property("before")?;
	let after = write.intern_property("after")?;
	let value =
		dictionary.get("value").ok_or_else(|| FerrosGrynnError::invalid_argument("value key"))?;
	let audit = write
		.atom(&"audit:graph_relate".parse()?)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("graph relate receipt"))?;
	assert_eq!(audit.properties().get(before), Some(&Value::Null));
	assert_eq!(audit.properties().get(after), Some(&Value::Null));
	assert!(
		audit
			.properties()
			.get(value)
			.and_then(Value::as_object)
			.and_then(|value| value.get("edge"))
			.is_some_and(|edge| matches!(edge, Value::AtomOccurrence(_)))
	);
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_qualified_relate_preserves_distinct_owner_routes() -> FerrosGrynnResult<()> {
	let (store, _target, value) = setup()?;
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let parent = seed.define_graph("parent", GraphKind::Metagraph)?;
	let child = seed.define_graph("child", GraphKind::Graph)?;
	let records = ["item:a", "item:b", "item:first", "item:second", "link:base", "link:connector"]
		.map(str::parse::<RecordId>)
		.into_iter()
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new())?;
	}
	let a = seed.attach_node(child.id(), &records[0], None, None)?;
	let b = seed.attach_node(child.id(), &records[1], None, None)?;
	let endpoint = seed.attach_edge(
		child.id(),
		&records[4],
		None,
		EdgeTopology::directed([a], [b]),
		None,
		None,
	)?;
	let first = seed.attach_node(parent.id(), &records[2], None, Some(child.id()))?;
	let second = seed.attach_node(parent.id(), &records[3], None, Some(child.id()))?;
	seed.define_event(EventDefinition::new(
		"route",
		EventTarget::Graph(parent.id()),
		"$event = 'RELATE' AND meta::id($value.edge) = 'connector'",
		"{ CREATE audit:route CONTENT { value: $value }; RELATE ATOM <[{ occurrence: \
		 $value.endpoints.source[0], owners: $value.endpoints.source_paths[0] }]> \
		 ~[link:generated]~> <[{ occurrence: $value.endpoints.target[0], owners: \
		 $value.endpoints.target_paths[0] }]> IN GRAPH parent; }",
	)?)?;
	seed.commit()?;
	let mut write = store.begin();
	write.attach_edge(
		parent.id(),
		&records[5],
		None,
		EdgeTopology::directed(
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![first], endpoint)?)],
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![second], endpoint)?)],
		),
		None,
		None,
	)?;
	let audit = write
		.atom(&"audit:route".parse()?)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("route receipt"))?;
	let endpoints = audit
		.properties()
		.get(value)
		.and_then(Value::as_object)
		.and_then(|value| value.get("endpoints"))
		.and_then(Value::as_object)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("route endpoints"))?;
	assert_eq!(endpoints.get("source"), endpoints.get("target"));
	assert_ne!(endpoints.get("source_paths"), endpoints.get("target_paths"));
	for (field, record) in [("source_paths", &records[2]), ("target_paths", &records[3])] {
		let path = endpoints
			.get(field)
			.and_then(Value::as_array)
			.and_then(|paths| paths.get(0))
			.and_then(Value::as_array)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("owner path"))?;
		assert_eq!(path.len(), 1);
		assert!(
			matches!(path.get(0), Some(Value::AtomOccurrence(owner)) if owner.atom().id() == record)
		);
	}
	let generated = write
		.membership(parent.id(), &"link:generated".parse()?)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing generated connector"))?;
	let view = parent.read(&write)?;
	let edge = view
		.edge(generated)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing generated edge"))?;
	let topology = edge.topology()?;
	for (side, owner) in [(topology.source(), first), (topology.target(), second)] {
		assert!(
			matches!(&side[0], EdgeEndpoint::Nested(qualified) if qualified.scope() == [owner] && qualified.atom() == endpoint)
		);
	}
	drop(topology);
	drop(edge);
	drop(view);
	write.commit()?;
	Ok(())
}

#[test]
fn event_sync_atomwalk_autogeneration_uses_common_relate_for_all_incidence() -> FerrosGrynnResult<()>
{
	let (store, _target, _value) = setup()?;
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let graph = seed.define_graph("scope", GraphKind::Graph)?;
	for record in ["item:a", "item:b"] {
		seed.create_atom(record.parse()?, PropertyMap::new())?;
		seed.attach_node(graph.id(), &record.parse()?, None, None)?;
	}
	seed.define_event(EventDefinition::new(
		"generate",
		EventTarget::Graph(graph.id()),
		"$event = 'RELATE' AND meta::id($value.edge) = 'base'",
		"{ RELATE ATOM <($value.endpoints.source[0])>~[link:vv]~><($value.endpoints.target[0])> \
		 IN GRAPH scope; RELATE ATOM <($value.endpoints.source[0])>~[link:ve]~><[$value.edge]> IN \
		 GRAPH scope; RELATE ATOM <[$value.edge]>~[link:ev]~><($value.endpoints.target[0])> IN \
		 GRAPH scope; RELATE ATOM <[$value.edge]>~[link:ee]~><[$value.edge]> IN GRAPH scope; }",
	)?)?;
	seed.commit()?;
	for result in
		store.execute("RELATE (item:a)-[link:base]->(item:b) IN GRAPH scope;", &Object::new())?
	{
		result.result?;
	}
	let mut read = store.begin();
	for record in ["link:base", "link:vv", "link:ve", "link:ev", "link:ee"] {
		assert!(read.membership(graph.id(), &record.parse()?)?.is_some(), "{record}");
	}
	Ok(())
}

#[test]
fn event_sync_large_label_copy_is_admitted_before_handler() -> FerrosGrynnResult<()> {
	let (store, _target, _value) = setup()?;
	let mut seed = store.begin();
	let graph = seed.define_graph("scope", GraphKind::Graph)?;
	let record: RecordId = "item:label".parse()?;
	seed.create_atom(record.clone(), PropertyMap::new())?;
	let old_label = crate::values::Text::from("a".repeat(230 * 1024));
	seed.attach_node(graph.id(), &record, Some(old_label.clone()), None)?;
	seed.define_event(EventDefinition::new(
		"label",
		EventTarget::Graph(graph.id()),
		"true",
		"{ CREATE audit:unexpected; }",
	)?)?;
	seed.commit()?;
	let mut options = QueryExecutionOptions::default();
	options.barrier_memory_bytes = NonZeroUsize::new(1024 * 1024)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("scratch cap"))?;
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	assert!(
		write
			.with_event_control(&control, |tx| tx.set_occurrence_label(
				graph.id(),
				&record,
				Some("b".repeat(230 * 1024).into())
			))
			.is_err()
	);
	let global = write
		.working
		.resolve_atom(&record)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("label atom"))?;
	let local = write
		.working
		.membership(graph.id(), global)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("label occurrence"))?;
	assert_eq!(
		write
			.working
			.occurrence(graph.id(), local)
			.and_then(crate::datastore::GraphOccurrence::label),
		Some(&old_label)
	);
	assert!(write.atom(&"audit:unexpected".parse()?)?.is_none());
	Ok(())
}
