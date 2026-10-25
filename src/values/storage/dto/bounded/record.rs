//! Restricted record keys borrow class names and use their exact reader frames.

use super::{
	Converter,
	FerrosGrynnError,
	FerrosGrynnResult,
	RecordId,
	StorageRecordId,
	Text,
	Value,
};
use crate::values::{
	Array,
	Number,
	Object,
	RecordIdPart,
	storage::{StorageRecordIdPart, StorageRecordValue},
};

impl Converter<'_> {
	/// Enters the record frame even when its key is scalar or an empty array.
	pub(super) fn record(
		&mut self,
		value: &RecordId,
		depth: usize,
	) -> FerrosGrynnResult<StorageRecordId> {
		self.policy.check_depth(depth)?;
		let class_id = self.context.class_id(value.class_text()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"prepared class dictionary is missing a name",
			)
		})?;
		let part = match value.id() {
			RecordIdPart::Int(value) => StorageRecordIdPart::Int(*value),
			RecordIdPart::String(value) => StorageRecordIdPart::String(self.text(value)?),
			RecordIdPart::Uuid(value) => StorageRecordIdPart::Uuid(*value),
			RecordIdPart::Ulid(value) => StorageRecordIdPart::Ulid(*value),
			RecordIdPart::Array(values) => {
				StorageRecordIdPart::Array(self.record_values(values, depth + 1)?)
			}
			RecordIdPart::Object(values) => {
				StorageRecordIdPart::Object(self.record_object(values, depth + 1)?)
			}
		};
		Ok(StorageRecordId {
			class_id,
			part,
		})
	}

	/// Empty arrays do not enter an unused item frame.
	fn record_values(
		&mut self,
		values: &Array,
		depth: usize,
	) -> FerrosGrynnResult<Vec<StorageRecordValue>> {
		self.policy.check_collection_len(values.len())?;
		let mut result = self.budget.vector::<StorageRecordValue>(values.len())?;
		for value in values {
			result.push(self.record_value(value, depth)?);
		}
		Ok(result)
	}

	/// The object helper enters its frame even for an empty record map.
	fn record_object(
		&mut self,
		values: &Object,
		depth: usize,
	) -> FerrosGrynnResult<Vec<(Text, StorageRecordValue)>> {
		self.policy.check_depth(depth)?;
		self.policy.check_collection_len(values.len())?;
		let mut result = self.budget.vector::<(Text, StorageRecordValue)>(values.len())?;
		for (key, value) in values {
			result.push((self.text(key)?, self.record_value(value, depth + 1)?));
		}
		Ok(result)
	}

	/// Converts only the scalar/container kinds admitted by runtime RecordId.
	fn record_value(
		&mut self,
		value: &Value,
		depth: usize,
	) -> FerrosGrynnResult<StorageRecordValue> {
		self.policy.check_depth(depth)?;
		match value {
			Value::Number(Number::Int(value)) => Ok(StorageRecordValue::Int(*value)),
			Value::String(value) => self.text(value).map(StorageRecordValue::String),
			Value::Uuid(value) => Ok(StorageRecordValue::Uuid(*value)),
			Value::Ulid(value) => Ok(StorageRecordValue::Ulid(*value)),
			Value::Array(values) => {
				self.record_values(values, depth + 1).map(StorageRecordValue::Array)
			}
			Value::Object(values) => {
				self.record_object(values, depth + 1).map(StorageRecordValue::Object)
			}
			_ => Err(FerrosGrynnError::invalid_storage_value_encoding(
				0,
				"unsupported RecordId component",
			)),
		}
	}
}
