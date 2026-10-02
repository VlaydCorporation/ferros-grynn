use std::num::NonZeroU32;

use super::{
	AnalyzeMethod,
	HistogramMethod,
	PropertyHistogramKey,
	StatisticsScope,
	analyze_database,
	analyze_database_with_method_at,
};
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

	let scratch = std::env::temp_dir().join(format!("fg-stats-{}", crate::values::Uuid::now_v7()));
	let sampled = analyze_database_with_method_at(&root, &scratch, AnalyzeMethod::Approximate {
		sample_count: NonZeroU32::new(1_024).ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("test sample is zero")
		})?,
	})?;
	let sampled_base = sampled
		.base()
		.property_histograms
		.get(&PropertyHistogramKey {
			class_id,
			field_id,
			scope: StatisticsScope::Base,
		})
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file(
				"sampled base histogram absent",
			)
		})?;
	assert_eq!(sampled_base.method, HistogramMethod::Approximate);
	assert_eq!(sampled_base.sample_count, 1);
	assert_eq!(sampled_base.distinct_count, 1);
	drop(std::fs::remove_dir_all(scratch));
	Ok(())
}

#[test]
fn geometry_analyze_samples_nonempty_mbrs_and_keeps_population() -> FerrosGrynnResult<()> {
	use crate::values::{Geometry, Position, Value};
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let graph = transaction.define_graph("places", GraphKind::Graph)?;
	transaction.define_class(ClassSchema::new("place", AtomKind::Node, [ClassField::new(
		"shape",
		Type::Option(Box::new(Type::Geometry)),
	)])?)?;
	transaction.commit()?;
	let class = store.class("place").unwrap();
	let field = class.fields().next().unwrap();
	let key = field.key.unwrap();
	let mut transaction = store.begin();
	for ordinal in 0..1032_i64 {
		let record = RecordId::new("place", RecordIdPart::Int(ordinal))?;
		let value = match ordinal {
			1030 => Value::Geometry(Geometry::Collection(vec![])),
			1031 => Value::Null,
			_ => Value::Geometry(Geometry::Point(Position::new([1.0, 2.0])?)),
		};
		transaction.create_atom(record.clone(), PropertyMap::from_iter([(key, value)]))?;
		if ordinal == 0 {
			transaction.attach_node(graph.id(), &record, None, None)?;
			transaction.set_overlay(
				graph.id(),
				&record,
				key,
				Value::Geometry(Geometry::Point(Position::new([8.0, 9.0])?)),
			)?;
		}
	}
	transaction.commit()?;
	let root = store.inner.current_root();
	let scratch = std::env::temp_dir().join(format!("fg-stats-{}", crate::values::Uuid::now_v7()));
	for method in [AnalyzeMethod::Full, AnalyzeMethod::Approximate {
		sample_count: NonZeroU32::new(1024).unwrap(),
	}] {
		let stats = analyze_database_with_method_at(&root, &scratch, method)?;
		let base = &stats.base().property_histograms[&PropertyHistogramKey {
			class_id: class.id().unwrap(),
			field_id: field.id().unwrap(),
			scope: StatisticsScope::Base,
		}];
		assert_eq!(base.method, HistogramMethod::Exact);
		assert_eq!(base.sample_count, 1032);
		assert_eq!(base.mbr_population, 1030);
		assert_eq!(base.mbr_sample, vec![[1.0, 2.0, 1.0, 2.0]; 1024]);
		assert!((base.null_fraction - 1.0 / 1032.0).abs() < f32::EPSILON);
		let effective = &stats.graph(graph.id()).unwrap().property_histograms
			[&PropertyHistogramKey {
				class_id: class.id().unwrap(),
				field_id: field.id().unwrap(),
				scope: StatisticsScope::Effective {
					graph: graph.id(),
				},
			}];
		assert_eq!(effective.mbr_sample, vec![[8.0, 9.0, 8.0, 9.0]]);
	}
	drop(std::fs::remove_dir_all(scratch));
	Ok(())
}
