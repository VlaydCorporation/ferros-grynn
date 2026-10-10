//! Shared immutable catalog for PURE builders and root-bound reactive reads.

use std::sync::Arc;

use super::{analysis::QueryCatalog, execution::query_schema};
use crate::{
	datastore::{
		AnalyzerId,
		ClassSchema,
		ClassSchemaMode,
		DatabaseRoot,
		FunctionId,
		SchemaAnalyzer,
		SchemaFunction,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphRef},
};

/// Catalog resolution cannot refresh or mutate its captured root.
pub(super) struct RootCatalog<'a>(pub(super) &'a DatabaseRoot);

impl QueryCatalog for RootCatalog<'_> {
	fn schema_epoch(&self) -> u64 {
		self.0.commit_ts
	}

	fn schema(&self, class: &str) -> FerrosGrynnResult<super::ir::Schema> {
		self.class_definition(class).map(|class| query_schema(&class))
	}

	fn class_definition(&self, class: &str) -> FerrosGrynnResult<Arc<ClassSchema>> {
		self.0.schema.class(class).ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!("class `{class}` does not exist"))
		})
	}

	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<ClassSchemaMode> {
		self.class_definition(class).map(|class| class.schema_mode())
	}

	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind> {
		self.class_definition(class).map(|class| class.kind())
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		self.graph_if_exists(name)?.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!("graph `{name}` does not exist"))
		})
	}

	fn graph_if_exists(&self, name: &str) -> FerrosGrynnResult<Option<GraphRef>> {
		Ok(self.0.schema.named_graph(name).map(|graph| graph.reference().clone()))
	}

	fn functions(
		&self,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<SchemaFunction>>> {
		Ok(self
			.0
			.schema
			.functions()
			.filter(|function| {
				function.namespace().as_str() == namespace && function.name().as_str() == name
			})
			.cloned()
			.collect())
	}

	fn function(&self, id: FunctionId) -> FerrosGrynnResult<Option<Arc<SchemaFunction>>> {
		Ok(self.0.schema.functions().find(|function| function.id() == Some(id)).cloned())
	}

	fn analyzer(&self, name: &str) -> FerrosGrynnResult<Option<Arc<SchemaAnalyzer>>> {
		Ok(self.0.schema.analyzer(name))
	}

	fn analyzer_by_id(&self, id: AnalyzerId) -> FerrosGrynnResult<Option<Arc<SchemaAnalyzer>>> {
		Ok(self.0.schema.analyzer_by_id(id))
	}
}
