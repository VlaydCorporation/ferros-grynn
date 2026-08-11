use std::{collections::HashMap, sync::Arc};

use ::arrow::{
	array::{ArrayRef, BooleanArray},
	datatypes::{DataType, Field, Schema},
	record_batch::RecordBatch,
};
use ::datafusion::{catalog::TableProvider, datasource::MemTable, prelude::SessionContext};

use super::PropertyBatch;
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

const PRESENCE_METADATA: &str = "ferros_grynn.presence_for_property";

impl PropertyBatch {
	/// Builds the DataFusion-facing batch.
	///
	/// Hidden presence columns let the query planner lower `IS NONE` separately
	/// from Arrow's ordinary `IS NULL` semantics.
	pub fn datafusion_record_batch(&self) -> FerrosGrynnResult<RecordBatch> {
		let mut fields = self.record_batch.schema().fields().iter().cloned().collect::<Vec<_>>();
		let mut columns = self.record_batch.columns().to_vec();
		for (&key, &field_index) in &self.key_to_field {
			let mut metadata = HashMap::new();
			metadata.insert(PRESENCE_METADATA.to_owned(), key.get().to_string());
			fields.push(Arc::new(
				Field::new(Self::presence_field_name(key), DataType::Boolean, false)
					.with_metadata(metadata),
			));
			columns
				.push(Arc::new(BooleanArray::new(self.presence[field_index].clone(), None))
					as ArrayRef);
		}

		RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).map_err(|source| {
			FerrosGrynnError::arrow_property_batch_failure("add DataFusion presence columns to")
				.with_technical_source(source)
		})
	}

	pub fn datafusion_table(&self) -> FerrosGrynnResult<Arc<MemTable>> {
		let batch = self.datafusion_record_batch()?;
		MemTable::try_new(batch.schema(), vec![vec![batch]]).map(Arc::new).map_err(|source| {
			FerrosGrynnError::data_fusion_property_batch_failure("create a memory table for")
				.with_technical_source(source)
		})
	}

	pub fn register_datafusion_table<IntoStr: Into<String>>(
		&self,
		context: &SessionContext,
		name: IntoStr,
	) -> FerrosGrynnResult<Option<Arc<dyn TableProvider>>> {
		let table: Arc<dyn TableProvider> = self.datafusion_table()?;
		context.register_table(name.into(), table).map_err(|source| {
			FerrosGrynnError::data_fusion_property_batch_failure("register a memory table for")
				.with_technical_source(source)
		})
	}

	pub fn presence_field_name(key: crate::values::runtime::PropertyKeyId) -> String {
		format!("__fg_presence_{}", key.get())
	}
}

#[cfg(test)]
mod tests {
	use ::datafusion::prelude::SessionContext;

	use super::PropertyBatch;
	use crate::values::runtime::{PropertyDictionary, PropertyMap, Value};

	#[tokio::test]
	async fn registers_only_after_arrow_batch_preserves_sentinels() {
		let mut dictionary = PropertyDictionary::new();
		let score = dictionary.intern("score").unwrap();
		let mut rows = vec![PropertyMap::new(); 3];
		rows[0].insert(score, 10_i64);
		rows[1].insert(score, Value::Null);

		let batch = PropertyBatch::from_rows(&rows, &dictionary).unwrap();
		assert_eq!(batch.to_rows().unwrap(), rows);

		let context = SessionContext::new();
		batch.register_datafusion_table(&context, "properties").unwrap();
		assert!(context.table_exist("properties").unwrap());
		let error = batch.register_datafusion_table(&context, "properties").unwrap_err();
		assert_eq!(error.status().code().to_string(), "FG-50F02");
		let frame = context.table("properties").await.unwrap();
		let schema = frame.schema();
		assert!(schema.field_with_unqualified_name("score").is_ok());
		assert!(
			schema.field_with_unqualified_name(&PropertyBatch::presence_field_name(score)).is_ok()
		);
	}
}
