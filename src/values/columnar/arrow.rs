use std::{
	collections::{BTreeMap, BTreeSet, HashMap},
	sync::Arc,
};

use ::arrow::{
	array::{
		Array, ArrayRef, BinaryArray, BooleanArray, Decimal128Array, FixedSizeBinaryArray,
		Float64Array, Int64Array, NullArray, StringArray, UnionArray,
		builder::FixedSizeBinaryBuilder,
	},
	buffer::{BooleanBuffer, ScalarBuffer},
	datatypes::{DataType, Field, Schema, UnionFields},
	error::ArrowError,
	record_batch::{RecordBatch, RecordBatchOptions},
};
use rust_decimal::Decimal;

use super::{
	ENCODING_METADATA, PROPERTY_KEY_ID_METADATA, PropertyBatch, VALUE_KIND_METADATA, invalid_batch,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{
		Bytes, Number, PropertyDictionary, PropertyKeyId, PropertyMap, Text, Ulid, Uuid, Value,
	},
};

const ENCODING_NATIVE: &str = "native";
const ENCODING_DENSE_UNION: &str = "dense_union";

impl PropertyBatch {
	/// Materializes row-oriented property maps as native Arrow arrays.
	///
	/// `Value::None` is normalized to an absent property. Incompatible
	/// supported scalar kinds use a dense union without coercion.
	/// StorageValue-backed fallback for nested values is introduced with the
	/// storage codec stage.
	pub fn from_rows(
		rows: &[PropertyMap],
		dictionary: &PropertyDictionary,
	) -> FerrosGrynnResult<Self> {
		let keys = rows
			.iter()
			.flat_map(PropertyMap::iter)
			.filter(|(_, value)| !value.is_none())
			.map(|(key, _)| key)
			.collect::<BTreeSet<_>>();

		let mut fields = Vec::with_capacity(keys.len());
		let mut arrays = Vec::with_capacity(keys.len());
		let mut presence = Vec::with_capacity(keys.len());
		let mut mapping = BTreeMap::new();

		for key in keys {
			let name = dictionary.resolve(key).ok_or_else(|| {
				invalid_batch(format!("property key {key} is missing from PropertyDictionary"))
			})?;
			let values = rows.iter().map(|row| row.get(key)).collect::<Vec<_>>();
			let present = values
				.iter()
				.map(|value| value.is_some_and(|value| !value.is_none()))
				.collect::<BooleanBuffer>();
			let (array, kind, encoding) = encode_column(key, &values)?;
			let field_index = fields.len();
			fields.push(property_field(key, name, array.data_type().clone(), kind, encoding));
			arrays.push(array);
			presence.push(present);
			mapping.insert(key, field_index);
		}

		let schema = Arc::new(Schema::new(fields));
		let options = RecordBatchOptions::new().with_row_count(Some(rows.len()));
		let record_batch = RecordBatch::try_new_with_options(schema, arrays, &options)
			.map_err(|source| arrow_failure("construct", source))?;
		Self::try_new(record_batch, presence, mapping)
	}
}

pub(super) fn decode_rows(batch: &PropertyBatch) -> FerrosGrynnResult<Vec<PropertyMap>> {
	let mut rows = vec![PropertyMap::new(); batch.num_rows()];
	let schema = batch.record_batch.schema();
	for field_index in 0..batch.num_columns() {
		let key = batch.field_to_key[field_index];
		let field = schema.field(field_index);
		let array = batch.record_batch.column(field_index);
		let logical_nulls = array.logical_nulls();
		for (row_index, row) in rows.iter_mut().enumerate() {
			if !batch.presence[field_index].value(row_index) {
				continue;
			}
			if logical_nulls.as_ref().is_some_and(|nulls| nulls.is_null(row_index)) {
				row.insert(key, Value::Null);
				continue;
			}
			let value = decode_value(array.as_ref(), field, row_index)?;
			row.insert(key, value);
		}
	}
	Ok(rows)
}

#[allow(
	clippy::expect_used,
	clippy::missing_panics_doc,
	reason = "one kind must have a first element"
)]
fn encode_column(
	key: PropertyKeyId,
	values: &[Option<&Value>],
) -> FerrosGrynnResult<(ArrayRef, &'static str, &'static str)> {
	let kinds = values
		.iter()
		.filter_map(|value| value.and_then(|value| ColumnKind::of_value(key, value).transpose()))
		.collect::<FerrosGrynnResult<BTreeSet<_>>>()?;

	match kinds.len() {
		0 => Ok((Arc::new(NullArray::new(values.len())), "null", ENCODING_NATIVE)),
		1 => {
			let kind = *kinds.first().expect("one kind must have a first element");
			Ok((build_array(kind, values)?, kind.metadata_name(), ENCODING_NATIVE))
		}
		_ => encode_union(key, values, kinds),
	}
}

#[allow(
	clippy::expect_used,
	clippy::missing_panics_doc,
	unwrap_in_result,
	reason = "non-sentinel has a kind & kind was collected from the same values"
)]
fn encode_union(
	key: PropertyKeyId,
	values: &[Option<&Value>],
	kinds: BTreeSet<ColumnKind>,
) -> FerrosGrynnResult<(ArrayRef, &'static str, &'static str)> {
	let kinds = kinds.into_iter().collect::<Vec<_>>();
	if kinds.len() > i8::MAX as usize {
		return Err(FerrosGrynnError::unsupported_columnar_value(
			u64::from(key.get()),
			"union with too many variants",
		));
	}

	let mut child_values = vec![Vec::new(); kinds.len()];
	let mut type_ids = Vec::with_capacity(values.len());
	let mut offsets = Vec::with_capacity(values.len());
	for value in values {
		let child = match value {
			Some(value) if !value.is_none() && !value.is_null() => {
				let kind = ColumnKind::of_value(key, value)?.expect("non-sentinel has a kind");
				kinds.binary_search(&kind).expect("kind was collected from the same values")
			}
			_ => 0,
		};
		type_ids.push(child as i8);
		offsets.push(i32::try_from(child_values[child].len()).map_err(|_err| {
			FerrosGrynnError::graph_capacity_exceeded("Arrow dense union child")
		})?);
		child_values[child].push(value.filter(|value| !value.is_none() && !value.is_null()));
	}

	let mut union_fields = Vec::with_capacity(kinds.len());
	let mut children = Vec::with_capacity(kinds.len());
	for (type_id, (kind, values)) in kinds.iter().copied().zip(child_values.iter()).enumerate() {
		let array = build_array(kind, values)?;
		let mut metadata = HashMap::new();
		metadata.insert(VALUE_KIND_METADATA.to_owned(), kind.metadata_name().to_owned());
		union_fields.push((
			type_id as i8,
			Arc::new(
				Field::new(kind.field_name(), array.data_type().clone(), true)
					.with_metadata(metadata),
			),
		));
		children.push(array);
	}

	let fields = union_fields.into_iter().collect::<UnionFields>();
	let array =
		UnionArray::try_new(fields, ScalarBuffer::from(type_ids), Some(offsets.into()), children)
			.map_err(|source| arrow_failure("construct a dense union for", source))?;
	Ok((Arc::new(array), "union", ENCODING_DENSE_UNION))
}

fn build_array(kind: ColumnKind, values: &[Option<&Value>]) -> FerrosGrynnResult<ArrayRef> {
	let array: ArrayRef = match kind {
		ColumnKind::Bool => Arc::new(BooleanArray::from(
			values
				.iter()
				.map(|value| match value {
					Some(Value::Bool(value)) => Some(*value),
					_ => None,
				})
				.collect::<Vec<_>>(),
		)),
		ColumnKind::Int => Arc::new(Int64Array::from(
			values
				.iter()
				.map(|value| match value {
					Some(Value::Number(Number::Int(value))) => Some(*value),
					_ => None,
				})
				.collect::<Vec<_>>(),
		)),
		ColumnKind::Float => Arc::new(Float64Array::from(
			values
				.iter()
				.map(|value| match value {
					Some(Value::Number(Number::Float(value))) => Some(*value),
					_ => None,
				})
				.collect::<Vec<_>>(),
		)),
		ColumnKind::Decimal(scale) => {
			let values = values
				.iter()
				.map(|value| match value {
					Some(Value::Number(Number::Decimal(value))) if value.scale() == scale => {
						Some(value.mantissa())
					}
					_ => None,
				})
				.collect::<Vec<_>>();
			let array = Decimal128Array::from(values)
				.with_precision_and_scale(38, scale as i8)
				.map_err(|source| arrow_failure("construct a decimal column for", source))?;
			Arc::new(array)
		}
		ColumnKind::Text => {
			Arc::new(StringArray::from_iter(values.iter().map(|value| match value {
				Some(Value::String(value)) => Some(value.as_str()),
				_ => None,
			})))
		}
		ColumnKind::Bytes => {
			Arc::new(BinaryArray::from_iter(values.iter().map(|value| match value {
				Some(Value::Bytes(value)) => Some(value.as_slice()),
				_ => None,
			})))
		}
		ColumnKind::Uuid | ColumnKind::Ulid => {
			let mut builder = FixedSizeBinaryBuilder::with_capacity(values.len(), 16);
			for value in values {
				match (kind, value) {
					(ColumnKind::Uuid, Some(Value::Uuid(value))) => builder
						.append_value(value.as_inner().as_bytes())
						.map_err(|source| arrow_failure("append a UUID to", source))?,
					(ColumnKind::Ulid, Some(Value::Ulid(value))) => builder
						.append_value(value.as_inner().to_bytes())
						.map_err(|source| arrow_failure("append a ULID to", source))?,
					_ => builder.append_null(),
				}
			}
			Arc::new(builder.finish())
		}
	};
	Ok(array)
}

fn decode_value(array: &dyn Array, field: &Field, index: usize) -> FerrosGrynnResult<Value> {
	if let Some(union) = array.as_any().downcast_ref::<UnionArray>() {
		let type_id = union.type_id(index);
		let offset = union.value_offset(index);
		let child_field = union
			.fields()
			.iter()
			.find_map(|(candidate, field)| (candidate == type_id).then_some(field))
			.ok_or_else(|| invalid_batch(format!("dense union type ID {type_id} has no field")))?;
		return decode_value(union.child(type_id).as_ref(), child_field, offset);
	}

	let kind = field
		.metadata()
		.get(VALUE_KIND_METADATA)
		.ok_or_else(|| invalid_batch(format!("Arrow field {} has no value kind", field.name())))?;
	match kind.as_str() {
		"bool" => Ok(Value::Bool(downcast::<BooleanArray>(array, field)?.value(index))),
		"int" => Ok(Value::from(downcast::<Int64Array>(array, field)?.value(index))),
		"float" => Ok(Value::from(downcast::<Float64Array>(array, field)?.value(index))),
		"string" => {
			Ok(Value::String(Text::from(downcast::<StringArray>(array, field)?.value(index))))
		}
		"bytes" => {
			Ok(Value::Bytes(Bytes::from(downcast::<BinaryArray>(array, field)?.value(index))))
		}
		"uuid" => {
			let bytes = fixed_16(array, field, index)?;
			Ok(Value::Uuid(Uuid::from(uuid::Uuid::from_bytes(bytes))))
		}
		"ulid" => {
			let bytes = fixed_16(array, field, index)?;
			Ok(Value::Ulid(Ulid::from(ulid::Ulid::from_bytes(bytes))))
		}
		value if value.starts_with("decimal:") => {
			let scale = value["decimal:".len()..].parse::<u32>().map_err(|_err| {
				invalid_batch(format!("Arrow field {} has invalid decimal scale", field.name()))
			})?;
			let mantissa = downcast::<Decimal128Array>(array, field)?.value(index);
			Ok(Value::from(Decimal::from_i128_with_scale(mantissa, scale)))
		}
		other => Err(invalid_batch(format!(
			"Arrow field {} has unknown value kind {other}",
			field.name()
		))),
	}
}

fn fixed_16(array: &dyn Array, field: &Field, index: usize) -> FerrosGrynnResult<[u8; 16]> {
	downcast::<FixedSizeBinaryArray>(array, field)?
		.value(index)
		.try_into()
		.map_err(|_err| invalid_batch(format!("Arrow field {} is not 16 bytes wide", field.name())))
}

fn downcast<'a, T: Array + 'static>(
	array: &'a dyn Array,
	field: &Field,
) -> FerrosGrynnResult<&'a T> {
	array.as_any().downcast_ref::<T>().ok_or_else(|| {
		invalid_batch(format!(
			"Arrow field {} has physical type {} inconsistent with its metadata",
			field.name(),
			array.data_type()
		))
	})
}

fn property_field(
	key: PropertyKeyId,
	name: &Text,
	data_type: DataType,
	kind: &str,
	encoding: &str,
) -> Field {
	let mut metadata = HashMap::new();
	metadata.insert(PROPERTY_KEY_ID_METADATA.to_owned(), key.get().to_string());
	metadata.insert(VALUE_KIND_METADATA.to_owned(), kind.to_owned());
	metadata.insert(ENCODING_METADATA.to_owned(), encoding.to_owned());
	Field::new(name.as_str(), data_type, true).with_metadata(metadata)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ColumnKind {
	Bool,
	Int,
	Float,
	Decimal(u32),
	Text,
	Bytes,
	Uuid,
	Ulid,
}

impl ColumnKind {
	fn of_value(key: PropertyKeyId, value: &Value) -> FerrosGrynnResult<Option<Self>> {
		let kind = match value {
			Value::None | Value::Null => return Ok(None),
			Value::Bool(_) => Self::Bool,
			Value::Number(Number::Int(_)) => Self::Int,
			Value::Number(Number::Float(_)) => Self::Float,
			Value::Number(Number::Decimal(value)) => Self::Decimal(value.scale()),
			Value::String(_) => Self::Text,
			Value::Bytes(_) => Self::Bytes,
			Value::Uuid(_) => Self::Uuid,
			Value::Ulid(_) => Self::Ulid,
			other => {
				return Err(FerrosGrynnError::unsupported_columnar_value(
					u64::from(key.get()),
					other.kind().to_string(),
				));
			}
		};
		Ok(Some(kind))
	}

	fn metadata_name(self) -> &'static str {
		match self {
			Self::Bool => "bool",
			Self::Int => "int",
			Self::Float => "float",
			Self::Decimal(scale) => match scale {
				0 => "decimal:0",
				1 => "decimal:1",
				2 => "decimal:2",
				3 => "decimal:3",
				4 => "decimal:4",
				5 => "decimal:5",
				6 => "decimal:6",
				7 => "decimal:7",
				8 => "decimal:8",
				9 => "decimal:9",
				10 => "decimal:10",
				11 => "decimal:11",
				12 => "decimal:12",
				13 => "decimal:13",
				14 => "decimal:14",
				15 => "decimal:15",
				16 => "decimal:16",
				17 => "decimal:17",
				18 => "decimal:18",
				19 => "decimal:19",
				20 => "decimal:20",
				21 => "decimal:21",
				22 => "decimal:22",
				23 => "decimal:23",
				24 => "decimal:24",
				25 => "decimal:25",
				26 => "decimal:26",
				27 => "decimal:27",
				28 => "decimal:28",
				_ => "decimal:invalid",
			},
			Self::Text => "string",
			Self::Bytes => "bytes",
			Self::Uuid => "uuid",
			Self::Ulid => "ulid",
		}
	}

	fn field_name(self) -> &'static str {
		self.metadata_name()
	}
}

fn arrow_failure(operation: impl Into<String>, source: ArrowError) -> FerrosGrynnError {
	FerrosGrynnError::arrow_property_batch_failure(operation).with_technical_source(source)
}

#[cfg(test)]
mod tests {
	use rust_decimal::Decimal;

	use super::PropertyBatch;
	use crate::values::runtime::{Bytes, PropertyDictionary, PropertyMap, Ulid, Uuid, Value};

	#[test]
	fn arrow_round_trip_preserves_none_null_and_native_scalars() {
		let mut dictionary = PropertyDictionary::new();
		let active = dictionary.intern("active").unwrap();
		let score = dictionary.intern("score").unwrap();
		let label = dictionary.intern("label").unwrap();
		let payload = dictionary.intern("payload").unwrap();
		let ratio = dictionary.intern("ratio").unwrap();
		let external = dictionary.intern("external").unwrap();

		let uuid = Uuid::new_v4();
		let mut rows = vec![PropertyMap::new(); 4];
		rows[0].insert(active, true);
		rows[0].insert(score, 10_i64);
		rows[0].insert(label, "first");
		rows[0].insert(payload, Bytes::from([1, 2, 3]));
		rows[0].insert(ratio, Decimal::new(125, 2));
		rows[0].insert(external, uuid);
		rows[1].insert(active, Value::Null);
		rows[1].insert(score, Value::None);
		rows[2].insert(score, 30_i64);
		rows[3].insert(active, false);

		let batch = PropertyBatch::from_rows(&rows, &dictionary).unwrap();
		let rebuilt =
			PropertyBatch::try_from_arrow(batch.record_batch().clone(), batch.presence.clone())
				.unwrap();
		let mut normalized = rows.clone();
		normalized[1].remove(score);
		assert_eq!(rebuilt.to_rows().unwrap(), normalized);
		assert!(!batch.presence(score).unwrap().value(1));
		assert!(batch.presence(active).unwrap().value(1));
	}

	#[test]
	fn incompatible_scalar_kinds_use_dense_union_without_coercion() {
		let mut dictionary = PropertyDictionary::new();
		let mixed = dictionary.intern("mixed").unwrap();
		let mut rows = vec![PropertyMap::new(); 5];
		rows[0].insert(mixed, 9_i64);
		rows[1].insert(mixed, "nine");
		rows[2].insert(mixed, Value::Null);
		rows[3].insert(mixed, true);

		let batch = PropertyBatch::from_rows(&rows, &dictionary).unwrap();
		assert!(matches!(
			batch.record_batch().column(0).data_type(),
			::arrow::datatypes::DataType::Union(..)
		));
		assert_eq!(batch.to_rows().unwrap(), rows);
	}

	#[test]
	fn unsupported_nested_value_has_domain_status() {
		let mut dictionary = PropertyDictionary::new();
		let key = dictionary.intern("nested").unwrap();
		let mut row = PropertyMap::new();
		row.insert(key, Value::Array(vec![Value::from(1_i64)].into()));

		let error = PropertyBatch::from_rows(&[row], &dictionary).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22F17");
	}

	#[test]
	fn empty_and_explicit_none_rows_keep_row_count_without_columns() {
		let mut dictionary = PropertyDictionary::new();
		let ignored = dictionary.intern("ignored").unwrap();
		let mut rows = vec![PropertyMap::new(); 3];
		rows[1].insert(ignored, Value::None);

		let batch = PropertyBatch::from_rows(&rows, &dictionary).unwrap();
		assert_eq!(batch.num_rows(), 3);
		assert_eq!(batch.num_columns(), 0);
		assert_eq!(batch.to_rows().unwrap(), vec![PropertyMap::new(); 3]);
	}

	#[test]
	fn union_preserves_float_decimal_scales_and_ulid() {
		let mut dictionary = PropertyDictionary::new();
		let key = dictionary.intern("variant").unwrap();
		let ulid = Ulid::new();
		let mut rows = vec![PropertyMap::new(); 4];
		rows[0].insert(key, Decimal::new(12, 1));
		rows[1].insert(key, Decimal::new(123, 2));
		rows[2].insert(key, 1.5_f64);
		rows[3].insert(key, ulid);

		let batch = PropertyBatch::from_rows(&rows, &dictionary).unwrap();
		assert_eq!(batch.to_rows().unwrap(), rows);
	}
}
