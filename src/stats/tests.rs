use std::num::NonZeroU32;

use super::{
	AnalyzeMethod,
	HistogramMethod,
	PropertyHistogramKey,
	StatisticsScope,
	analyze_database,
	analyze_database_with_method_at,
};
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::{AtomKind, GraphKind},
	values::{PropertyMap, RecordId, RecordIdPart, Type},
};

/// Verifies last label member removal and renaming remove empty counters.
#[test]
fn last_label_member_removal_and_renaming_remove_empty_counters() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("n", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = seed.define_graph("labels", GraphKind::Graph).assert_ok();
	let record: RecordId = "n:one".parse().assert_ok();
	seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	seed.attach_node(graph.id(), &record, Some("old".into()), None).assert_ok();
	seed.commit().assert_ok();
	let mut rename = store.begin();
	rename.set_occurrence_label(graph.id(), &record, Some("new".into())).assert_ok();
	rename.commit().assert_ok();
	let root = store.inner.current_root();
	let counters = &root
		.statistics
		.graph(graph.id())
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_argument("missing graph statistics")
		})
		.assert_ok()
		.labels;
	assert_eq!(counters.len(), 1);
	assert_eq!(counters.keys().next().map(|key| key.label.as_str()), Some("new"));
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &record, false).assert_ok();
	remove.commit().assert_ok();
	assert!(
		store
			.inner
			.current_root()
			.statistics
			.graph(graph.id())
			.ok_or_else(|| crate::gql_status::FerrosGrynnError::invalid_argument(
				"missing graph statistics"
			))
			.assert_ok()
			.labels
			.is_empty()
	);
}

/// Verifies full scan separates base and effective populations.
#[test]
fn full_scan_separates_base_and_effective_populations() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut define = store.begin();
	let graph = define.define_graph("social", GraphKind::Graph).assert_ok();
	define
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("age", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
	define.commit().assert_ok();
	let class = store
		.class("person")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("person"))
		.assert_ok();
	let class_id = class
		.id()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("class identity absent")
		})
		.assert_ok();
	let field = class
		.fields()
		.next()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("class field absent")
		})
		.assert_ok();
	let key = field
		.key
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("field key absent")
		})
		.assert_ok();
	let field_id = field
		.id()
		.ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::invalid_storage_file("field identity absent")
		})
		.assert_ok();
	let record = RecordId::new("person", RecordIdPart::String("alice".into())).assert_ok();
	let mut mutate = store.begin();
	let mut properties = PropertyMap::new();
	properties.insert(key, 30_i64);
	mutate.create_atom(record.clone(), properties).assert_ok();
	mutate.attach_node(graph.id(), &record, Some("Engineer".into()), None).assert_ok();
	mutate.set_overlay(graph.id(), &record, key, 31_i64.into()).assert_ok();
	mutate.commit().assert_ok();

	let root = store.inner.current_root();
	let statistics = analyze_database(&root).assert_ok();
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
		})
		.assert_ok();
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
		})
		.assert_ok();
	assert_ne!(base.buckets[0].lower, effective.buckets[0].lower);

	let scratch = std::env::temp_dir().join(format!("fg-stats-{}", crate::values::Uuid::now_v7()));
	let sampled = analyze_database_with_method_at(&root, &scratch, AnalyzeMethod::Approximate {
		sample_count: NonZeroU32::new(1_024)
			.ok_or_else(|| {
				crate::gql_status::FerrosGrynnError::invalid_argument("test sample is zero")
			})
			.assert_ok(),
	})
	.assert_ok();
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
		})
		.assert_ok();
	assert_eq!(sampled_base.method, HistogramMethod::Approximate);
	assert_eq!(sampled_base.sample_count, 1);
	assert_eq!(sampled_base.distinct_count, 1);
	drop(std::fs::remove_dir_all(scratch));
}

/// Verifies geometry analyze samples nonempty mbrs and keeps population.
#[test]
fn geometry_analyze_samples_nonempty_mbrs_and_keeps_population() {
	use crate::{
		test_assertions::AssertResult as _,
		values::{Geometry, Position, Value},
	};
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	let graph = transaction.define_graph("places", GraphKind::Graph).assert_ok();
	transaction
		.define_class(
			ClassSchema::new("place", AtomKind::Node, [ClassField::new(
				"shape",
				Type::Option(Box::new(Type::Geometry)),
			)])
			.assert_ok(),
		)
		.assert_ok();
	transaction.commit().assert_ok();
	let class = store.class("place").assert_some();
	let field = class.fields().next().assert_some();
	let key = field.key.assert_some();
	let mut transaction = store.begin();
	for ordinal in 0..1032_i64 {
		let record = RecordId::new("place", RecordIdPart::Int(ordinal)).assert_ok();
		let value = match ordinal {
			1030 => Value::Geometry(Geometry::Collection(vec![])),
			1031 => Value::Null,
			_ => Value::Geometry(Geometry::Point(Position::new([1.0, 2.0]).assert_ok())),
		};
		transaction.create_atom(record.clone(), PropertyMap::from_iter([(key, value)])).assert_ok();
		if ordinal == 0 {
			transaction.attach_node(graph.id(), &record, None, None).assert_ok();
			transaction
				.set_overlay(
					graph.id(),
					&record,
					key,
					Value::Geometry(Geometry::Point(Position::new([8.0, 9.0]).assert_ok())),
				)
				.assert_ok();
		}
	}
	transaction.commit().assert_ok();
	let root = store.inner.current_root();
	let scratch = std::env::temp_dir().join(format!("fg-stats-{}", crate::values::Uuid::now_v7()));
	for method in [AnalyzeMethod::Full, AnalyzeMethod::Approximate {
		sample_count: NonZeroU32::new(1024).assert_some(),
	}] {
		let stats = analyze_database_with_method_at(&root, &scratch, method).assert_ok();
		let base = &stats.base().property_histograms[&PropertyHistogramKey {
			class_id: class.id().assert_some(),
			field_id: field.id().assert_some(),
			scope: StatisticsScope::Base,
		}];
		assert_eq!(base.method, HistogramMethod::Exact);
		assert_eq!(base.sample_count, 1032);
		assert_eq!(base.mbr_population, 1030);
		assert_eq!(base.mbr_sample, vec![[1.0, 2.0, 1.0, 2.0]; 1024]);
		assert!((base.null_fraction - 1.0 / 1032.0).abs() < f32::EPSILON);
		let effective = &stats.graph(graph.id()).assert_some().property_histograms
			[&PropertyHistogramKey {
				class_id: class.id().assert_some(),
				field_id: field.id().assert_some(),
				scope: StatisticsScope::Effective {
					graph: graph.id(),
				},
			}];
		assert_eq!(effective.mbr_sample, vec![[8.0, 9.0, 8.0, 9.0]]);
	}
	drop(std::fs::remove_dir_all(scratch));
}
