use super::{PropertyHistogramKey, StatisticsScope, analyze_database};
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, RecordIdPart, Type},
};

#[test]
fn full_scan_separates_base_and_effective_populations() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut define = store.begin();
	let graph = define.define_graph("social", GraphKind::Graph)?;
	define.define_class(ClassSchema::new("person", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	define.commit()?;
	let class = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))?;
	let class_id = class.id().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_storage_file("class identity absent")
	})?;
	let field = class.fields().next().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_storage_file("class field absent")
	})?;
	let key = field.key.ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_storage_file("field key absent")
	})?;
	let field_id = field.id().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_storage_file("field identity absent")
	})?;
	let record = RecordId::new("person", RecordIdPart::String("alice".into()))?;
	let mut mutate = store.begin();
	let mut properties = PropertyMap::new();
	properties.insert(key, 30_i64);
	mutate.create_atom(record.clone(), properties)?;
	mutate.attach_node(graph.id(), &record, Some("Engineer".into()), None)?;
	mutate.set_overlay(graph.id(), &record, key, 31_i64.into())?;
	mutate.commit()?;

	let root = store.inner.current_root();
	let statistics = analyze_database(&root)?;
	assert_eq!(statistics.base().classes.get(&class_id), Some(&1));
	let base = statistics
		.base()
		.property_histograms
		.get(&PropertyHistogramKey {
			class_id,
			field_id,
			scope: StatisticsScope::Base,
		})
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("base histogram absent")
		})?;
	let graph_id = graph.id();
	let effective = statistics
		.graph(graph_id)
		.and_then(|statistics| {
			statistics.property_histograms.get(&PropertyHistogramKey {
				class_id,
				field_id,
				scope: StatisticsScope::Effective {
					graph: graph_id,
				},
			})
		})
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("effective histogram absent")
		})?;
	assert_ne!(base.buckets[0].lower, effective.buckets[0].lower);
	Ok(())
}
