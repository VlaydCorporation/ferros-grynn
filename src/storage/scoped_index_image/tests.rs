use super::{build_scoped_index_images, validate_scoped_index_images};
use crate::{
	datastore::{ClassField, ClassIndex, ClassIndexKind, ClassSchema, Datastore, IndexScope},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, RecordIdPart, Type},
};

#[test]
fn scoped_images_round_trip_base_effective_class_label_and_count() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let graph = transaction.define_graph("social", GraphKind::Graph)?;
	let class = ClassSchema::new("person", AtomKind::Node, [ClassField::new("age", Type::Int)])?
		.with_index(ClassIndex::new("by_age", ClassIndexKind::Standard, IndexScope::Base, [
			"age",
		])?)?
		.with_index(ClassIndex::new(
			"base_count",
			ClassIndexKind::Count,
			IndexScope::Base,
			std::iter::empty::<&str>(),
		)?)?
		.with_index(ClassIndex::new(
			"effective_age",
			ClassIndexKind::Unique,
			IndexScope::Effective {
				graph: graph.id(),
			},
			["age"],
		)?)?
		.with_index(ClassIndex::new(
			"effective_count",
			ClassIndexKind::Count,
			IndexScope::Effective {
				graph: graph.id(),
			},
			std::iter::empty::<&str>(),
		)?)?;
	transaction.define_class(class)?;
	transaction.commit()?;
	let schema = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))?;
	let key = schema
		.fields()
		.find(|field| field.name().as_str() == "age")
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument("age key absent"))?;
	let record = RecordId::new("person", RecordIdPart::String("alice".into()))?;
	let mut transaction = store.begin();
	let mut properties = PropertyMap::new();
	properties.insert(key, 42_i64);
	transaction.create_atom(record.clone(), properties)?;
	transaction.attach_node(graph.id(), &record, Some("Employee".into()), None)?;
	transaction.commit()?;

	let root = store.inner.current_root();
	let images = build_scoped_index_images(&root)?;
	validate_scoped_index_images(&images, &root)?;
	assert!(images.base.iter().any(|file| file.relative_path == "idx/std_0000.fgidx"));
	assert!(images.base.iter().any(|file| file.relative_path.starts_with("idx/cnt_")));
	let graph_files = images.graphs.get(&graph.id()).ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_storage_file("graph image absent")
	})?;
	assert!(graph_files.iter().any(|file| file.relative_path.starts_with("idx/cls_")));
	assert!(graph_files.iter().any(|file| file.relative_path == "idx/lbl_node_0.fgidx"));
	assert!(graph_files.iter().any(|file| file.relative_path == "idx/uniq_0000.fgidx"));
	Ok(())
}
