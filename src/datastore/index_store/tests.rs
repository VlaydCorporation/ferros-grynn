use std::sync::Arc;

use crate::{
	datastore::{
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassSchema,
		Datastore,
		IndexScope,
		IndexStoreRoot,
	},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, RecordIdPart, Type, Value},
};

fn record(name: &str) -> FerrosGrynnResult<RecordId> {
	RecordId::new("person", RecordIdPart::String(name.into()))
}

#[test]
fn base_and_effective_indexes_track_normalized_mutations() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut define = store.begin();
	let graph = define.define_graph("work", GraphKind::Graph)?;
	let class = ClassSchema::new("person", AtomKind::Node, [
		ClassField::new("email", Type::String),
		ClassField::new("age", Type::Int),
	])?
	.with_index(ClassIndex::new("by_age", ClassIndexKind::Standard, IndexScope::Base, ["age"])?)?
	.with_index(ClassIndex::new("unique_email", ClassIndexKind::Unique, IndexScope::Base, [
		"email",
	])?)?
	.with_index(ClassIndex::new(
		"base_count",
		ClassIndexKind::Count,
		IndexScope::Base,
		std::iter::empty::<&str>(),
	)?)?
	.with_index(ClassIndex::new(
		"effective_email",
		ClassIndexKind::Unique,
		IndexScope::Effective {
			graph: graph.id(),
		},
		["email"],
	)?)?
	.with_index(ClassIndex::new(
		"effective_count",
		ClassIndexKind::Count,
		IndexScope::Effective {
			graph: graph.id(),
		},
		std::iter::empty::<&str>(),
	)?)?;
	define.define_class(class)?;
	define.commit()?;

	let schema = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))?;
	let email = schema
		.fields()
		.find(|field| field.name().as_str() == "email")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("email key absent"))?;
	let age = schema
		.fields()
		.find(|field| field.name().as_str() == "age")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("age key absent"))?;

	let first = record("first")?;
	let second = record("second")?;
	let mut mutate = store.begin();
	let mut first_values = PropertyMap::new();
	first_values.insert(email, "a@example.test");
	first_values.insert(age, 30_i64);
	mutate.create_atom(first.clone(), first_values)?;
	let mut second_values = PropertyMap::new();
	second_values.insert(email, "b@example.test");
	second_values.insert(age, 30_i64);
	mutate.create_atom(second.clone(), second_values)?;
	mutate.attach_node(graph.id(), &first, Some("Engineer".into()), None)?;
	mutate.attach_node(graph.id(), &second, Some("Engineer".into()), None)?;
	mutate.commit()?;

	let root = store.inner.current_root();
	let class_id = schema.id().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("published class has no id")
	})?;
	assert_eq!(root.indexes.base_class(class_id).len(), 2);
	assert_eq!(root.indexes.graph_class(graph.id(), class_id).len(), 2);
	assert_eq!(root.indexes.graph_label(graph.id(), AtomKind::Node, &"Engineer".into()).len(), 2);
	let base_count = schema
		.indexes()
		.find(|index| index.name().as_str() == "base_count")
		.and_then(|index| index.id())
		.and_then(|id| root.indexes.base_index(id))
		.and_then(|index| index.entries.count());
	assert_eq!(base_count, Some(2));
	let effective_count = schema
		.indexes()
		.find(|index| index.name().as_str() == "effective_count")
		.and_then(|index| index.id())
		.and_then(|id| root.indexes.graph_index(graph.id(), id))
		.and_then(|index| index.entries.count());
	assert_eq!(effective_count, Some(2));
	drop(root);

	let mut overlay = store.begin();
	overlay.set_overlay(graph.id(), &second, email, Value::from("shared@example.test"))?;
	overlay.commit()?;
	let mut conflicting = store.begin();
	let error =
		match conflicting.set_base_property(&first, email, Value::from("shared@example.test")) {
			Ok(_previous) => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"effective unique conflict was not detected",
				));
			}
			Err(error) => error,
		};
	assert_eq!(error.status().code().to_string(), "FG-G2F04");
	assert_eq!(
		conflicting.atom(&first)?.and_then(|atom| atom.properties().get(email).cloned()),
		Some(Value::from("a@example.test"))
	);
	Ok(())
}

#[test]
fn reopen_rebuild_restores_effective_overlay_keys_and_zero_class_counts() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut define = store.begin();
	let graph = define.define_graph("social", GraphKind::Graph)?;
	let class =
		ClassSchema::new("person", AtomKind::Node, [ClassField::new("email", Type::String)])?
			.with_index(ClassIndex::new(
				"effective_email",
				ClassIndexKind::Unique,
				IndexScope::Effective {
					graph: graph.id(),
				},
				["email"],
			)?)?;
	define.define_class(class)?;
	define.commit()?;

	let schema = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))?;
	let email = schema
		.fields()
		.find(|field| field.name().as_str() == "email")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("email key absent"))?;
	let first = record("first")?;
	let second = record("second")?;
	let mut write = store.begin();
	let mut first_values = PropertyMap::new();
	first_values.insert(email, "base-a@example.test");
	write.create_atom(first.clone(), first_values)?;
	let mut second_values = PropertyMap::new();
	second_values.insert(email, "base-b@example.test");
	write.create_atom(second.clone(), second_values)?;
	write.attach_node(graph.id(), &first, None, None)?;
	write.attach_node(graph.id(), &second, None, None)?;
	write.set_overlay(graph.id(), &first, email, Value::from("effective@example.test"))?;
	write.commit()?;

	let mut reopened = (*store.inner.current_root()).clone();
	reopened.indexes = Arc::new(IndexStoreRoot::default());
	reopened.statistics = Arc::new(crate::stats::StatisticsStore::default());
	reopened.rebuild_derived_state()?;
	let class_id = schema.id().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("published class has no id")
	})?;
	assert_eq!(reopened.indexes.graph_class(graph.id(), class_id).len(), 2);
	assert_eq!(reopened.statistics.graph(graph.id()).unwrap().classes[&class_id], 2);

	let reopened = Datastore::from_root_for_test(Default::default(), reopened);
	let mut conflict = reopened.begin();
	assert!(
		conflict
			.set_overlay(graph.id(), &second, email, Value::from("effective@example.test"))
			.is_err()
	);
	Ok(())
}
