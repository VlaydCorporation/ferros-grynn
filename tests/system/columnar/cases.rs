use std::collections::BTreeMap;

use arrow::{
	array::{Array, Int64Array},
	buffer::BooleanBuffer,
	datatypes::{DataType, UnionMode},
};
use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::AtomKind,
	values::{
		Number,
		Object,
		PropertyKeyId,
		PropertyMap,
		Type,
		Value,
		columnar::{ENCODING_METADATA, PROPERTY_KEY_ID_METADATA, PropertyBatch},
	},
};

use crate::support::record;

/// Publishes scalar rows with distinct value/null/absent states through the
/// SDK.
fn seed(store: &Datastore) -> (PropertyKeyId, PropertyKeyId, Vec<PropertyMap>) {
	let mut schema = store.begin();
	schema
		.define_class(
			ClassSchema::new("columnar_row", AtomKind::Node, [
				ClassField::new("ordinal", Type::Int),
				ClassField::new("metric", Type::Any),
			])
			.unwrap(),
		)
		.unwrap();
	schema.commit().unwrap();
	let dictionary = store.property_dictionary();
	let ordinal = dictionary.get("ordinal").unwrap();
	let metric = dictionary.get("metric").unwrap();
	let mut rows = vec![PropertyMap::new(); 4];
	for (index, row) in rows.iter_mut().enumerate() {
		row.insert(ordinal, i64::try_from(index).unwrap());
	}
	rows[0].insert(metric, 42_i64);
	rows[1].insert(metric, Value::Null);
	rows[3].insert(metric, -5_i64);
	let mut write = store.begin();
	for (index, row) in rows.iter().enumerate() {
		write.create_atom(record(&format!("columnar_row:r{index}")), row.clone()).unwrap();
	}
	write.commit().unwrap();
	(ordinal, metric, rows)
}

/// Collects owned public snapshots in a stable identity order.
fn rows(store: &Datastore) -> Vec<PropertyMap> {
	let mut read = store.begin();
	(0..4)
		.map(|index| {
			read.atom(&record(&format!("columnar_row:r{index}")))
				.unwrap()
				.unwrap()
				.properties()
				.clone()
		})
		.collect()
}

#[test]
fn sdk_arrow_round_trip_preserves_presence_validity_metadata_and_recovered_rows() {
	let db = database();
	let (ordinal, metric, expected) = seed(&db);
	db.assert_committed(|store| {
		let batch = PropertyBatch::from_rows(&rows(store), &store.property_dictionary()).unwrap();
		assert_eq!(batch.num_rows(), 4);
		assert_eq!(batch.num_columns(), 2);
		let column = batch.field_index(metric).unwrap();
		assert_eq!(batch.property_key(column), Some(metric));
		assert_eq!(batch.field_index(PropertyKeyId::new(u32::MAX)), None);
		assert_eq!(batch.presence(metric).unwrap().iter().collect::<Vec<_>>(), vec![
			true, true, false, true
		]);
		let array =
			batch.record_batch().column(column).as_any().downcast_ref::<Int64Array>().unwrap();
		assert_eq!(array.value(0), 42);
		assert_eq!(array.value(3), -5);
		assert!(array.is_null(1));
		assert!(array.is_null(2));
		let schema = batch.record_batch().schema();
		assert_eq!(
			schema.field(column).metadata().get(PROPERTY_KEY_ID_METADATA),
			Some(&metric.get().to_string())
		);
		assert_eq!(
			schema.field(column).metadata().get(ENCODING_METADATA).map(String::as_str),
			Some("native")
		);
		let presence = (0..batch.num_columns())
			.map(|field| batch.presence(batch.property_key(field).unwrap()).unwrap().clone())
			.collect();
		let imported =
			PropertyBatch::try_from_arrow(batch.record_batch().clone(), presence).unwrap();
		assert_eq!(imported.field_index(ordinal), batch.field_index(ordinal));
		assert_eq!(imported.to_rows().unwrap(), expected);
	});
}

#[test]
fn sdk_dense_union_keeps_scalar_variants_without_coercion() {
	let db = database();
	let (_, metric, mut expected) = seed(&db);
	expected[0].insert(metric, 9_i64);
	expected[1].insert(metric, "nine");
	expected[2].insert(metric, 1.5_f64);
	expected[3].insert(metric, true);
	let mut write = db.begin();
	for (index, row) in expected.iter().enumerate() {
		write
			.set_base_property(
				&record(&format!("columnar_row:r{index}")),
				metric,
				row.get(metric).unwrap().clone(),
			)
			.unwrap();
	}
	write.commit().unwrap();
	db.assert_committed(|store| {
		let batch = PropertyBatch::from_rows(&rows(store), &store.property_dictionary()).unwrap();
		let field = batch.field_index(metric).unwrap();
		assert!(matches!(
			batch.record_batch().column(field).data_type(),
			DataType::Union(_, UnionMode::Dense)
		));
		let decoded = batch.to_rows().unwrap();
		assert_eq!(decoded, expected);
		assert!(matches!(decoded[0].get(metric), Some(Value::Number(Number::Int(9)))));
		assert!(matches!(decoded[2].get(metric), Some(Value::Number(Number::Float(_)))));
	});
}

#[test]
fn columnar_batch_owns_its_data_after_the_datastore_is_dropped() {
	let db = database();
	let (_, _, expected) = seed(&db);
	let batch = PropertyBatch::from_rows(&rows(&db), &db.property_dictionary()).unwrap();
	drop(db);
	assert_eq!(batch.to_rows().unwrap(), expected);
}

#[test]
fn arrow_import_rejects_missing_metadata_bad_presence_and_duplicate_field_mappings() {
	let db = database();
	let (ordinal, metric, _) = seed(&db);
	let batch = PropertyBatch::from_rows(&rows(&db), &db.property_dictionary()).unwrap();
	let mapping = BTreeMap::from([
		(ordinal, batch.field_index(ordinal).unwrap()),
		(metric, batch.field_index(metric).unwrap()),
	]);
	let error = PropertyBatch::try_new(
		batch.record_batch().clone(),
		vec![BooleanBuffer::new_set(3); 2],
		mapping,
	)
	.unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-22F18");
	let error = PropertyBatch::try_new(
		batch.record_batch().clone(),
		vec![BooleanBuffer::new_set(4); 2],
		BTreeMap::from([(ordinal, 0), (metric, 0)]),
	)
	.unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-22F18");
	let mut fields = batch
		.record_batch()
		.schema()
		.fields()
		.iter()
		.map(|field| field.as_ref().clone())
		.collect::<Vec<_>>();
	fields[0] = fields[0].clone().with_metadata(std::collections::HashMap::new());
	let raw = arrow::record_batch::RecordBatch::try_new(
		std::sync::Arc::new(arrow::datatypes::Schema::new(fields)),
		batch.record_batch().columns().to_vec(),
	)
	.unwrap();
	let error = PropertyBatch::try_from_arrow(raw, vec![BooleanBuffer::new_set(4); 2]).unwrap_err();
	assert_eq!(error.status().code().to_string(), "FG-22F18");
}

#[test]
fn none_only_rows_preserve_row_count_and_unsupported_values_return_domain_status() {
	let db = database();
	let (_, metric, _) = seed(&db);
	let dictionary = db.property_dictionary();
	let mut absent = vec![PropertyMap::new(); 3];
	absent[1].insert(metric, Value::None);
	let batch = PropertyBatch::from_rows(&absent, &dictionary).unwrap();
	assert_eq!(batch.num_rows(), 3);
	assert_eq!(batch.num_columns(), 0);
	assert_eq!(batch.to_rows().unwrap(), vec![PropertyMap::new(); 3]);
	let empty = PropertyBatch::from_rows(&[], &dictionary).unwrap();
	assert_eq!((empty.num_rows(), empty.num_columns()), (0, 0));
	for value in [Value::Array(vec![Value::from(1_i64)].into()), Value::Object(Object::new())] {
		let mut row = PropertyMap::new();
		row.insert(metric, value);
		assert_eq!(
			PropertyBatch::from_rows(&[row], &dictionary).unwrap_err().status().code().to_string(),
			"FG-22F17"
		);
	}
}

#[cfg(feature = "datafusion")]
#[test]
fn datafusion_sdk_filters_none_separately_from_null_and_aggregates_numeric_rows() {
	use datafusion::{
		functions_aggregate::expr_fn::sum,
		prelude::{SessionContext, col},
	};
	let db = database();
	let (_, metric, _) = seed(&db);
	let runtime = tokio::runtime::Runtime::new().unwrap();
	db.assert_committed(|store| {
		let batch = PropertyBatch::from_rows(&rows(store), &store.property_dictionary()).unwrap();
		let context = SessionContext::new();
		assert!(batch.register_datafusion_table(&context, "properties").unwrap().is_none());
		assert!(context.table_exist("properties").unwrap());
		assert_eq!(
			batch
				.register_datafusion_table(&context, "properties")
				.unwrap_err()
				.status()
				.code()
				.to_string(),
			"FG-50F02"
		);
		let hidden = PropertyBatch::presence_field_name(metric);
		let record_batch = batch.datafusion_record_batch().unwrap();
		assert_eq!(record_batch.num_columns(), 4);
		let presence = record_batch
			.column_by_name(&hidden)
			.unwrap()
			.as_any()
			.downcast_ref::<arrow::array::BooleanArray>()
			.unwrap();
		assert_eq!(presence.values().iter().collect::<Vec<_>>(), vec![true, true, false, true]);
		let frame = runtime.block_on(context.table("properties")).unwrap();
		let absent = frame
			.clone()
			.filter(!col(hidden.as_str()))
			.unwrap()
			.select_columns(&["ordinal"])
			.unwrap();
		let null = frame
			.clone()
			.filter(col(hidden.as_str()).and(col("metric").is_null()))
			.unwrap()
			.select_columns(&["ordinal"])
			.unwrap();
		let total = frame.aggregate(vec![], vec![sum(col("metric")).alias("total")]).unwrap();
		for (label, plan, expected) in [
			("none", absent, vec![2_i64]),
			("null", null, vec![1_i64]),
			("sum", total, vec![37_i64]),
		] {
			let batches = runtime.block_on(plan.collect()).unwrap();
			let actual = batches
				.iter()
				.flat_map(|result| {
					let column = result.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
					assert_eq!(column.null_count(), 0);
					column.values().iter().copied()
				})
				.collect::<Vec<_>>();
			assert_eq!(actual, expected, "{label}");
		}
	});
}
