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

/// Verifies event sync sdk context noop and delete.
#[test]
fn event_sync_sdk_context_noop_and_delete() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, value) = setup().assert_ok();
	let mut tx = store.begin();
	for (name, kind) in [("created", "CREATE"), ("updated", "UPDATE"), ("deleted", "DELETE")] {
		tx.define_event(
			EventDefinition::new(
				name,
				target,
				format!("$event = '{kind}'"),
				format!(
					"{{ CREATE audit:{name} CONTENT {{ old: $before?.value, new: $after?.value, \
					 value: $value.value, input: $input }}; }}"
				),
			)
			.assert_ok(),
		)
		.assert_ok();
	}
	tx.commit().assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	let mut write = store.begin();
	write
		.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))
		.assert_ok();
	assert!(write.atom(&"audit:created".parse().assert_ok()).assert_ok().is_some());
	write.set_base_property(&record, value, Value::from(1)).assert_ok();
	assert!(write.atom(&"audit:updated".parse().assert_ok()).assert_ok().is_none());
	write.set_base_property(&record, value, Value::from(2)).assert_ok();
	write.delete_atom(&record).assert_ok();
	write.commit().assert_ok();
	let mut read = store.begin();
	let dictionary = store.property_dictionary();
	let old = dictionary
		.get("old")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("audit old key"))
		.assert_ok();
	let new = dictionary
		.get("new")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("audit new key"))
		.assert_ok();
	let update = read
		.atom(&"audit:updated".parse().assert_ok())
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("audit update missing"))
		.assert_ok();
	assert_eq!(update.properties().get(old), Some(&Value::from(1)));
	assert_eq!(update.properties().get(new), Some(&Value::from(2)));
	assert!(read.atom(&"audit:deleted".parse().assert_ok()).assert_ok().is_some());
	assert!(read.atom(&record).assert_ok().is_none());
}

/// Verifies event sync error restores original and handler effects.
#[test]
fn event_sync_error_restores_original_and_handler_effects() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, value) = setup().assert_ok();
	let mut tx = store.begin();
	tx.define_event(
		EventDefinition::new(
			"reject",
			target,
			"$after.value = 2",
			"{ CREATE audit:partial; THROW 'blocked'; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	let mut write = store.begin();
	write
		.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))]))
		.assert_ok();
	assert!(write.set_base_property(&record, value, Value::from(2)).is_err());
	assert_eq!(
		write.atom(&record).assert_ok().and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(1))
	);
	assert!(write.atom(&"audit:partial".parse().assert_ok()).assert_ok().is_none());
	write.set_base_property(&record, value, Value::from(3)).assert_ok();
	write.commit().assert_ok();
	assert_eq!(
		store
			.begin()
			.atom(&record)
			.assert_ok()
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(3))
	);
}

/// Verifies event sync nested mutations reenter dispatch.
#[test]
fn event_sync_nested_mutations_reenter_dispatch() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, value) = setup().assert_ok();
	let mut tx = store.begin();
	tx.define_event(
		EventDefinition::new(
			"increment",
			target,
			"$after.value < 3",
			"{ UPDATE item:root SET value = $after.value + 1; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let mut write = store.begin();
	let record: RecordId = "item:root".parse().assert_ok();
	write
		.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(0))]))
		.assert_ok();
	assert_eq!(
		write.atom(&record).assert_ok().and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(3))
	);
	write.commit().assert_ok();
}

/// Verifies event sync inherited graph update and detach.
#[test]
fn event_sync_inherited_graph_update_and_detach() {
	use crate::test_assertions::AssertResult as _;

	let (store, _target, value) = setup().assert_ok();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	tx.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))])).assert_ok();
	tx.attach_node(graph.id(), &record, None, None).assert_ok();
	tx.define_event(
		EventDefinition::new(
			"changed",
			EventTarget::Graph(graph.id()),
			"$event = 'UPDATE'",
			"{ CREATE audit:inherited CONTENT { old: $before.value, new: $after.value }; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.define_event(
		EventDefinition::new(
			"detached",
			EventTarget::Graph(graph.id()),
			"$event = 'DELETE'",
			"{ CREATE audit:detached; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let mut write = store.begin();
	write.set_base_property(&record, value, Value::from(2)).assert_ok();
	write.detach_occurrence(graph.id(), &record, false).assert_ok();
	assert!(write.atom(&"audit:inherited".parse().assert_ok()).assert_ok().is_some());
	assert!(write.atom(&"audit:detached".parse().assert_ok()).assert_ok().is_some());
	write.commit().assert_ok();
}

/// Verifies event sync handlers share loop gas.
#[test]
fn event_sync_handlers_share_loop_gas() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, _value) = setup().assert_ok();
	let mut tx = store.begin();
	for name in ["a", "b"] {
		tx.define_event(
			EventDefinition::new(name, target, "true", "{ FOR $n IN 0..400 { LET $x = 1; }; }")
				.assert_ok(),
		)
		.assert_ok();
	}
	tx.commit().assert_ok();
	let options = QueryExecutionOptions {
		max_loop_iterations: 600,
		..Default::default()
	};
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	let record: RecordId = "item:limited".parse().assert_ok();
	assert!(
		write
			.with_event_control(&control, |tx| tx.create_atom(record.clone(), PropertyMap::new()))
			.is_err()
	);
	assert!(write.atom(&record).assert_ok().is_none());
}

/// Verifies event sync recursion failure restores depth and data.
#[test]
fn event_sync_recursion_failure_restores_depth_and_data() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, value) = setup().assert_ok();
	let mut tx = store.begin();
	tx.define_event(
		EventDefinition::new(
			"cycle",
			target,
			"true",
			"{ UPDATE item:root SET value = $after.value + 1; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let options = QueryExecutionOptions {
		max_control_depth: 4,
		..Default::default()
	};
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	let record: RecordId = "item:root".parse().assert_ok();
	assert!(
		write
			.with_event_control(&control, |tx| tx
				.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(0))])))
			.is_err()
	);
	assert!(write.atom(&record).assert_ok().is_none());
	assert_eq!(write.event_depth, 0);
}

/// Verifies event sync gql handler keeps lexical frame private.
#[test]
fn event_sync_gql_handler_keeps_lexical_frame_private() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, _value) = setup().assert_ok();
	let mut tx = store.begin();
	tx.define_event(
		EventDefinition::new(
			"created",
			target,
			"$event = 'CREATE'",
			"{ LET $private = $after.value; CREATE audit:gql CONTENT { value: $private }; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	tx.commit().assert_ok();
	let mut write = store.begin();
	for result in
		write.execute("CREATE item:gql CONTENT { value: 7 }; RETURN 1;", &Object::new()).assert_ok()
	{
		let _value = result.result.assert_ok();
	}
	assert!(write.atom(&"audit:gql".parse().assert_ok()).assert_ok().is_some());
	assert!(!write.gql_bindings().contains_key("private"));
	write.commit().assert_ok();
}

/// Verifies event sync empty snapshot observes concurrent registration.
#[test]
fn event_sync_empty_snapshot_observes_concurrent_registration() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, _value) = setup().assert_ok();
	let mut write = store.begin();
	write.create_atom("item:stale".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let mut definition = store.begin();
	definition
		.define_event(EventDefinition::new("new", target, "true", "1").assert_ok())
		.assert_ok();
	definition.commit().assert_ok();
	assert!(write.commit().is_err());
	assert!(store.begin().atom(&"item:stale".parse().assert_ok()).assert_ok().is_none());
}

/// Verifies event sync context reads detect serializable cycle.
#[test]
fn event_sync_context_reads_detect_serializable_cycle() {
	use crate::test_assertions::AssertResult as _;

	let (store, _target, value) = setup().assert_ok();
	let mut seed = store.begin();
	let graph = seed.define_graph("scope", GraphKind::Graph).assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	seed.create_atom(record.clone(), PropertyMap::from_iter([(value, Value::from(1))])).assert_ok();
	seed.attach_node(graph.id(), &record, None, None).assert_ok();
	seed.define_event(
		EventDefinition::new(
			"read_context",
			EventTarget::Graph(graph.id()),
			"$input?.x != NONE",
			"{ CREATE audit:receipt CONTENT { value: $after.value }; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	let x = seed.intern_property("x").assert_ok();
	seed.commit().assert_ok();
	let mut first = store.begin();
	first.set_overlay(graph.id(), &record, x, Value::from(1)).assert_ok();
	let mut second = store.begin();
	assert!(second.atom(&"audit:receipt".parse().assert_ok()).assert_ok().is_none());
	second.set_base_property(&record, value, Value::from(2)).assert_ok();
	second.commit().assert_ok();
	assert!(first.commit().is_err());
	assert!(store.begin().atom(&"audit:receipt".parse().assert_ok()).assert_ok().is_none());
}

/// Verifies event sync raw input preserves equal fields and patch removals.
#[test]
fn event_sync_raw_input_preserves_equal_fields_and_patch_removals() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, _value) = setup().assert_ok();
	let mut seed = store.begin();
	for result in seed
		.execute("CREATE item:a CONTENT { kept: 1, changed: 1, removed: 9 };", &Object::new())
		.assert_ok()
	{
		let _value = result.result.assert_ok();
	}
	let id = seed
		.define_event(
			EventDefinition::new(
				"input",
				target,
				"$event = 'UPDATE'",
				"{ CREATE audit:merge CONTENT { input: $input }; }",
			)
			.assert_ok(),
		)
		.assert_ok();
	seed.commit().assert_ok();
	let mut write = store.begin();
	for result in
		write.execute("UPDATE item:a MERGE { kept: 1, changed: 2 };", &Object::new()).assert_ok()
	{
		let _value = result.result.assert_ok();
	}
	write
		.replace_event(
			id,
			EventDefinition::new(
				"input",
				target,
				"$event = 'UPDATE'",
				"{ CREATE audit:patch CONTENT { input: $input }; }",
			)
			.assert_ok(),
		)
		.assert_ok();
	for result in write
		.execute("UPDATE item:a PATCH { removed: NONE, changed: 3 };", &Object::new())
		.assert_ok()
	{
		let _value = result.result.assert_ok();
	}
	write.commit().assert_ok();
	let input = store
		.property_dictionary()
		.get("input")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("input key"))
		.assert_ok();
	let mut read = store.begin();
	let merge = read
		.atom(&"audit:merge".parse().assert_ok())
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("merge receipt"))
		.assert_ok();
	let patch = read
		.atom(&"audit:patch".parse().assert_ok())
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("patch receipt"))
		.assert_ok();
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
}

/// Verifies event sync context is frozen before ordered handlers.
#[test]
fn event_sync_context_is_frozen_before_ordered_handlers() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, value) = setup().assert_ok();
	let mut seed = store.begin();
	let first = seed
		.define_event(
			EventDefinition::new(
				"z_first",
				target,
				"$event = 'CREATE'",
				"{ UPDATE item:a SET value = 99; CREATE audit:order CONTENT { value: 1 }; }",
			)
			.assert_ok(),
		)
		.assert_ok();
	let second = seed
		.define_event(
			EventDefinition::new(
				"a_second",
				target,
				"$event = 'CREATE'",
				"{ UPDATE audit:order SET value = value + 1; CREATE audit:frozen CONTENT { value: \
				 $after.value }; }",
			)
			.assert_ok(),
		)
		.assert_ok();
	assert!(first < second);
	seed.commit().assert_ok();
	let mut write = store.begin();
	write
		.create_atom(
			"item:a".parse().assert_ok(),
			PropertyMap::from_iter([(value, Value::from(1))]),
		)
		.assert_ok();
	assert_eq!(
		write
			.atom(&"item:a".parse().assert_ok())
			.assert_ok()
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(99))
	);
	assert_eq!(
		write
			.atom(&"audit:order".parse().assert_ok())
			.assert_ok()
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(2))
	);
	assert_eq!(
		write
			.atom(&"audit:frozen".parse().assert_ok())
			.assert_ok()
			.and_then(|atom| atom.properties().get(value).cloned()),
		Some(Value::from(1))
	);
	write.commit().assert_ok();
}

/// Verifies event sync try rollback preserves subsequent dispatch.
#[test]
fn event_sync_try_rollback_preserves_subsequent_dispatch() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, _value) = setup().assert_ok();
	let mut seed = store.begin();
	seed.define_event(
		EventDefinition::new(
			"reject",
			target,
			"$after.value = 2",
			"{ CREATE audit:partial; THROW 'blocked'; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let mut write = store.begin();
	for result in write
		.execute(
			"TRY { CREATE item:bad CONTENT { value: 2 }; } CATCH |$e| { $e.message; }; CREATE \
			 item:good CONTENT { value: 3 };",
			&Object::new(),
		)
		.assert_ok()
	{
		let _value = result.result.assert_ok();
	}
	assert!(write.atom(&"item:bad".parse().assert_ok()).assert_ok().is_none());
	assert!(write.atom(&"audit:partial".parse().assert_ok()).assert_ok().is_none());
	assert!(write.atom(&"item:good".parse().assert_ok()).assert_ok().is_some());
	assert_eq!(write.event_depth, 0);
	write.commit().assert_ok();
}

/// Verifies event sync context copy limit rolls back before handler.
#[test]
fn event_sync_context_copy_limit_rolls_back_before_handler() {
	use crate::test_assertions::AssertResult as _;

	let (store, target, value) = setup().assert_ok();
	let mut seed = store.begin();
	seed.define_event(
		EventDefinition::new("large", target, "true", "{ CREATE audit:unexpected; }").assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(1024 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scratch cap"))
			.assert_ok(),
		..Default::default()
	};
	let control = ExecutionControl::new(&options, None);
	let mut write = store.begin();
	let record: RecordId = "item:large".parse().assert_ok();
	let result = write.with_event_control(&control, |tx| {
		tx.create_atom(
			record.clone(),
			PropertyMap::from_iter([(value, Value::from("x".repeat(400 * 1024)))]),
		)
	});
	assert!(result.is_err());
	assert!(write.atom(&record).assert_ok().is_none());
	assert!(write.atom(&"audit:unexpected".parse().assert_ok()).assert_ok().is_none());
}

/// Verifies event sync gql relate context preserves class and graph contracts.
#[test]
fn event_sync_gql_relate_context_preserves_class_and_graph_contracts() {
	use crate::test_assertions::AssertResult as _;

	let (store, _target, _value) = setup().assert_ok();
	let mut seed = store.begin();
	let edge_class =
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("scope", GraphKind::Graph).assert_ok();
	for record in ["item:a", "item:b"] {
		seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	seed.define_event(
		EventDefinition::new(
			"class_relate",
			EventTarget::Class(edge_class),
			"$event = 'RELATE'",
			"{ CREATE audit:class_relate CONTENT { event: $event, value: $value }; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.define_event(
		EventDefinition::new(
			"graph_relate",
			EventTarget::Graph(graph.id()),
			"$event = 'RELATE'",
			"{ CREATE audit:graph_relate CONTENT { before: $before, after: $after, value: $value \
			 }; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let mut write = store.begin();
	for result in write
		.execute("RELATE (item:a)-[link:e]->(item:b) IN GRAPH scope;", &Object::new())
		.assert_ok()
	{
		let _value = result.result.assert_ok();
	}
	assert!(write.atom(&"audit:class_relate".parse().assert_ok()).assert_ok().is_some());
	let dictionary = store.property_dictionary();
	let before = write.intern_property("before").assert_ok();
	let after = write.intern_property("after").assert_ok();
	let value = dictionary
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("value key"))
		.assert_ok();
	let audit = write
		.atom(&"audit:graph_relate".parse().assert_ok())
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("graph relate receipt"))
		.assert_ok();
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
	write.commit().assert_ok();
}

/// Verifies event sync qualified relate preserves distinct owner routes.
#[test]
fn event_sync_qualified_relate_preserves_distinct_owner_routes() {
	use crate::test_assertions::AssertResult as _;

	let (store, _target, value) = setup().assert_ok();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let parent = seed.define_graph("parent", GraphKind::Metagraph).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	let records = ["item:a", "item:b", "item:first", "item:second", "link:base", "link:connector"]
		.map(str::parse::<RecordId>)
		.into_iter()
		.collect::<FerrosGrynnResult<Vec<_>>>()
		.assert_ok();
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let a = seed.attach_node(child.id(), &records[0], None, None).assert_ok();
	let b = seed.attach_node(child.id(), &records[1], None, None).assert_ok();
	let endpoint = seed
		.attach_edge(child.id(), &records[4], None, EdgeTopology::directed([a], [b]), None, None)
		.assert_ok();
	let first = seed.attach_node(parent.id(), &records[2], None, Some(child.id())).assert_ok();
	let second = seed.attach_node(parent.id(), &records[3], None, Some(child.id())).assert_ok();
	seed.define_event(
		EventDefinition::new(
			"route",
			EventTarget::Graph(parent.id()),
			"$event = 'RELATE' AND meta::id($value.edge) = 'connector'",
			"{ CREATE audit:route CONTENT { value: $value }; RELATE ATOM <[{ occurrence: \
			 $value.endpoints.source[0], owners: $value.endpoints.source_paths[0] }]> \
			 ~[link:generated]~> <[{ occurrence: $value.endpoints.target[0], owners: \
			 $value.endpoints.target_paths[0] }]> IN GRAPH parent; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let mut write = store.begin();
	write
		.attach_edge(
			parent.id(),
			&records[5],
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![first], endpoint).assert_ok())],
				[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![second], endpoint).assert_ok())],
			),
			None,
			None,
		)
		.assert_ok();
	let audit = write
		.atom(&"audit:route".parse().assert_ok())
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("route receipt"))
		.assert_ok();
	let endpoints = audit
		.properties()
		.get(value)
		.and_then(Value::as_object)
		.and_then(|value| value.get("endpoints"))
		.and_then(Value::as_object)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("route endpoints"))
		.assert_ok();
	assert_eq!(endpoints.get("source"), endpoints.get("target"));
	assert_ne!(endpoints.get("source_paths"), endpoints.get("target_paths"));
	for (field, record) in [("source_paths", &records[2]), ("target_paths", &records[3])] {
		let path = endpoints
			.get(field)
			.and_then(Value::as_array)
			.and_then(|paths| paths.get(0))
			.and_then(Value::as_array)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("owner path"))
			.assert_ok();
		assert_eq!(path.len(), 1);
		assert!(
			matches!(path.get(0), Some(Value::AtomOccurrence(owner)) if owner.atom().id() == record)
		);
	}
	let generated = write
		.membership(parent.id(), &"link:generated".parse().assert_ok())
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing generated connector"))
		.assert_ok();
	let view = parent.read(&write).assert_ok();
	{
		let edge = view
			.edge(generated)
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing generated edge"))
			.assert_ok();
		let topology = edge.topology().assert_ok();
		for (side, owner) in [(topology.source(), first), (topology.target(), second)] {
			assert!(
				matches!(&side[0], EdgeEndpoint::Nested(qualified) if qualified.scope() == [owner] && qualified.atom() == endpoint)
			);
		}
		drop(topology);
	}
	drop(view);
	write.commit().assert_ok();
}

/// Verifies event sync atomwalk autogeneration uses common relate for all
/// incidence.
#[test]
fn event_sync_atomwalk_autogeneration_uses_common_relate_for_all_incidence() {
	use crate::test_assertions::AssertResult as _;

	let (store, _target, _value) = setup().assert_ok();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("scope", GraphKind::Graph).assert_ok();
	for record in ["item:a", "item:b"] {
		seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record.parse().assert_ok(), None, None).assert_ok();
	}
	seed.define_event(
		EventDefinition::new(
			"generate",
			EventTarget::Graph(graph.id()),
			"$event = 'RELATE' AND meta::id($value.edge) = 'base'",
			"{ RELATE ATOM \
			 <($value.endpoints.source[0])>~[link:vv]~><($value.endpoints.target[0])> IN GRAPH \
			 scope; RELATE ATOM <($value.endpoints.source[0])>~[link:ve]~><[$value.edge]> IN \
			 GRAPH scope; RELATE ATOM <[$value.edge]>~[link:ev]~><($value.endpoints.target[0])> \
			 IN GRAPH scope; RELATE ATOM <[$value.edge]>~[link:ee]~><[$value.edge]> IN GRAPH \
			 scope; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	for result in store
		.execute("RELATE (item:a)-[link:base]->(item:b) IN GRAPH scope;", &Object::new())
		.assert_ok()
	{
		result.result.assert_ok();
	}
	let mut read = store.begin();
	for record in ["link:base", "link:vv", "link:ve", "link:ev", "link:ee"] {
		assert!(
			read.membership(graph.id(), &record.parse().assert_ok()).assert_ok().is_some(),
			"{record}"
		);
	}
}

/// Verifies event sync large label copy is admitted before handler.
#[test]
fn event_sync_large_label_copy_is_admitted_before_handler() {
	use crate::test_assertions::AssertResult as _;

	let (store, _target, _value) = setup().assert_ok();
	let mut seed = store.begin();
	let graph = seed.define_graph("scope", GraphKind::Graph).assert_ok();
	let record: RecordId = "item:label".parse().assert_ok();
	seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	let old_label = crate::values::Text::from("a".repeat(230 * 1024));
	seed.attach_node(graph.id(), &record, Some(old_label.clone()), None).assert_ok();
	seed.define_event(
		EventDefinition::new(
			"label",
			EventTarget::Graph(graph.id()),
			"true",
			"{ CREATE audit:unexpected; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(1024 * 1024)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scratch cap"))
			.assert_ok(),
		..Default::default()
	};
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
		.ok_or_else(|| FerrosGrynnError::invalid_argument("label atom"))
		.assert_ok();
	let local = write
		.working
		.membership(graph.id(), global)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("label occurrence"))
		.assert_ok();
	assert_eq!(
		write
			.working
			.occurrence(graph.id(), local)
			.and_then(crate::datastore::GraphOccurrence::label),
		Some(&old_label)
	);
	assert!(write.atom(&"audit:unexpected".parse().assert_ok()).assert_ok().is_none());
}
