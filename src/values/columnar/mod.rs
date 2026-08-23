//! Arrow/DataFusion materialization boundary for graph properties.
//!
//! Runtime values remain independent of this module. A batch keeps presence
//! separate from Arrow validity so `none` and `null` remain distinguishable.

mod arrow;
#[cfg(feature = "datafusion")]
mod datafusion;

use std::collections::BTreeMap;

use ::arrow::{buffer::BooleanBuffer, record_batch::RecordBatch};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::runtime::{PropertyKeyId, PropertyMap},
};

pub const PROPERTY_KEY_ID_METADATA: &str = "ferros_grynn.property_key_id";
pub const VALUE_KIND_METADATA: &str = "ferros_grynn.value_kind";
pub const ENCODING_METADATA: &str = "ferros_grynn.encoding";

/// An Arrow batch plus the information Arrow validity cannot represent alone.
#[derive(Debug, Clone)]
pub struct PropertyBatch {
	pub(crate) record_batch: RecordBatch,
	pub(crate) presence: Vec<BooleanBuffer>,
	pub(crate) key_to_field: BTreeMap<PropertyKeyId, usize>,
	pub(crate) field_to_key: Vec<PropertyKeyId>,
}

impl PropertyBatch {
	#[allow(
		clippy::expect_used,
		clippy::missing_panics_doc,
		reason = "validated field mapping must be complete"
	)]
	pub fn try_new(
		record_batch: RecordBatch,
		presence: Vec<BooleanBuffer>,
		key_to_field: BTreeMap<PropertyKeyId, usize>,
	) -> FerrosGrynnResult<Self> {
		let field_count = record_batch.num_columns();
		if presence.len() != field_count {
			return Err(invalid_batch(format!(
				"expected {field_count} presence bitmaps, found {}",
				presence.len()
			)));
		}
		if key_to_field.len() != field_count {
			return Err(invalid_batch(format!(
				"expected {field_count} property mappings, found {}",
				key_to_field.len()
			)));
		}

		let mut field_to_key = vec![None; field_count];
		for (&key, &field) in &key_to_field {
			let Some(slot) = field_to_key.get_mut(field) else {
				return Err(invalid_batch(format!(
					"property {key} refers to missing Arrow field {field}"
				)));
			};
			if slot.replace(key).is_some() {
				return Err(invalid_batch(format!(
					"multiple properties refer to Arrow field {field}"
				)));
			}
		}

		for (field, bitmap) in presence.iter().enumerate() {
			if bitmap.len() != record_batch.num_rows() {
				return Err(invalid_batch(format!(
					"presence bitmap {field} has {} rows, expected {}",
					bitmap.len(),
					record_batch.num_rows()
				)));
			}
		}

		Ok(Self {
			record_batch,
			presence,
			key_to_field,
			field_to_key: field_to_key
				.into_iter()
				.map(|key| key.expect("validated field mapping must be complete"))
				.collect(),
		})
	}

	pub fn try_from_arrow(
		record_batch: RecordBatch,
		presence: Vec<BooleanBuffer>,
	) -> FerrosGrynnResult<Self> {
		let mut key_to_field = BTreeMap::new();
		for (field_index, field) in record_batch.schema().fields().iter().enumerate() {
			let raw = field
				.metadata()
				.get(PROPERTY_KEY_ID_METADATA)
				.ok_or_else(|| {
					invalid_batch(format!(
						"Arrow field {field_index} has no {PROPERTY_KEY_ID_METADATA} metadata"
					))
				})?
				.parse::<u32>()
				.map_err(|_err| {
					invalid_batch(format!(
						"Arrow field {field_index} has an invalid property key ID"
					))
				})?;
			if key_to_field.insert(PropertyKeyId::new(raw), field_index).is_some() {
				return Err(invalid_batch(format!("property key {raw} is mapped more than once")));
			}
		}
		Self::try_new(record_batch, presence, key_to_field)
	}

	pub fn record_batch(&self) -> &RecordBatch {
		&self.record_batch
	}

	pub fn presence(&self, key: PropertyKeyId) -> Option<&BooleanBuffer> {
		let field = *self.key_to_field.get(&key)?;
		self.presence.get(field)
	}

	pub fn field_index(&self, key: PropertyKeyId) -> Option<usize> {
		self.key_to_field.get(&key).copied()
	}

	pub fn property_key(&self, field_index: usize) -> Option<PropertyKeyId> {
		self.field_to_key.get(field_index).copied()
	}

	pub fn num_rows(&self) -> usize {
		self.record_batch.num_rows()
	}

	pub fn num_columns(&self) -> usize {
		self.record_batch.num_columns()
	}

	pub fn to_rows(&self) -> FerrosGrynnResult<Vec<PropertyMap>> {
		arrow::decode_rows(self)
	}
}

fn invalid_batch(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_property_batch(reason)
}

#[cfg(test)]
mod tests {
	use std::{collections::BTreeMap, sync::Arc};

	use ::arrow::{
		array::Int64Array,
		buffer::BooleanBuffer,
		datatypes::{DataType, Field, Schema},
		record_batch::RecordBatch,
	};

	use super::PropertyBatch;
	use crate::values::runtime::PropertyKeyId;

	#[test]
	fn validates_mapping_and_presence_lengths() {
		let schema = Arc::new(Schema::new(vec![Field::new("value", DataType::Int64, true)]));
		let batch =
			RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![Some(1), None]))])
				.unwrap();
		let mapping = BTreeMap::from([(PropertyKeyId::new(4), 0)]);

		let error =
			PropertyBatch::try_new(batch.clone(), vec![BooleanBuffer::new_set(1)], mapping.clone())
				.unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22F18");

		let error = PropertyBatch::try_new(batch, vec![BooleanBuffer::new_set(2)], BTreeMap::new())
			.unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22F18");
	}

	#[test]
	fn imported_arrow_batch_requires_property_metadata() {
		let schema = Arc::new(Schema::new(vec![Field::new("value", DataType::Int64, true)]));
		let batch =
			RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(vec![1]))]).unwrap();
		let error =
			PropertyBatch::try_from_arrow(batch, vec![BooleanBuffer::new_set(1)]).unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-22F18");
	}
}
