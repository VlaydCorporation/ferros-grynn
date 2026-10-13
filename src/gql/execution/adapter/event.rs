//! Frozen function sources with current transaction data and schema metadata.

use std::sync::Arc;

use super::{QueryCatalog, TransactionRead};
use crate::{
	datastore::{
		AnalyzerId,
		ClassSchema,
		ClassSchemaMode,
		FunctionId,
		SchemaAnalyzer,
		SchemaCatalog,
		SchemaFunction,
		Transaction,
	},
	gql::{
		analysis::analyze_with_runtime,
		closure::LanguageClosureState,
		ir::{LogicalProgram, Schema},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphRef},
	syn::Program,
	values::{Object, Type},
};

/// Compiles against one root and captures its exact transitive source versions.
pub(in crate::gql::execution) fn analyze_event_source(
	transaction: &mut Transaction,
	source: &str,
	program: &Program,
	cache: &LanguageClosureState,
	frozen: Option<&[Arc<SchemaFunction>]>,
	origin: Option<&SchemaCatalog>,
) -> FerrosGrynnResult<(LogicalProgram, Vec<Arc<SchemaFunction>>)> {
	let mut snapshot_admission = None;
	let snapshot = transaction.query_snapshot_admitted(false, |bytes| {
		snapshot_admission = Some(cache.admit_compile_metadata(bytes, 0)?);
		Ok(())
	})?;
	let _snapshot_admission = snapshot_admission;
	let read = TransactionRead {
		scalar_graph: None,
		transaction,
		snapshot,
		metadata_only: false,
	};
	let logical = if frozen.is_none() && origin.is_none() {
		analyze_with_runtime(program, source, &read, &Object::new(), cache)?
	} else {
		let catalog = EventSourceCatalog {
			current: &read,
			functions: frozen,
			origin,
			cache,
		};
		analyze_with_runtime(program, source, &catalog, &Object::new(), cache)?
	};
	let count = cache.named_dependency_count();
	let _reservation = cache.admit_compile_metadata(
		count
			.checked_mul(256)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT function dependencies"))?,
		count as u64,
	)?;
	let mut functions = Vec::new();
	functions.try_reserve_exact(count).map_err(|error| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"EVENT function dependencies",
			count as u64,
		)
		.with_technical_source(error)
	})?;
	for (id, version) in cache.named_dependencies() {
		let definition = match frozen {
			None => read.transaction.query_schema_function(&read.snapshot, id)?,
			Some(sources) => sources.iter().find(|source| source.id() == Some(id)).cloned(),
		}
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("compiled EVENT function source is absent")
		})?;
		if definition.version() != version {
			return Err(FerrosGrynnError::invalid_argument(
				"compiled EVENT function source version differs",
			));
		}
		functions.push(definition);
	}
	Ok((logical, functions))
}

/// Accepted compilation can use retired graph metadata; execution uses current
/// data.
struct EventSourceCatalog<'read> {
	/// Classes, graphs and other metadata retain the current attempt snapshot.
	current: &'read dyn QueryCatalog,
	/// Accepted transitive descriptors, independent of their present catalog
	/// owner.
	functions: Option<&'read [Arc<SchemaFunction>]>,
	/// Only enqueue compilation sees this pre-mutation metadata, never
	/// historical rows.
	origin: Option<&'read SchemaCatalog>,
	/// Overload list copies use the same bounded compiler admission.
	cache: &'read LanguageClosureState,
}

impl QueryCatalog for EventSourceCatalog<'_> {
	fn admit_analysis(&self, bytes: usize, work: u64) -> FerrosGrynnResult<()> {
		self.current.admit_analysis(bytes, work)
	}

	fn lexical_binding_type(&self, name: &str) -> Option<&Type> {
		self.current.lexical_binding_type(name)
	}

	fn lexical_binding_is_reactive(&self, name: &str) -> bool {
		self.current.lexical_binding_is_reactive(name)
	}

	fn schema_epoch(&self) -> u64 {
		self.current.schema_epoch()
	}

	fn schema(&self, class: &str) -> FerrosGrynnResult<Schema> {
		self.current.schema(class)
	}

	fn class_definition(&self, class: &str) -> FerrosGrynnResult<Arc<ClassSchema>> {
		self.current.class_definition(class)
	}

	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<ClassSchemaMode> {
		self.current.schema_mode(class)
	}

	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind> {
		self.current.class_kind(class)
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		self.graph_if_exists(name)?.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))
	}

	fn graph_if_exists(&self, name: &str) -> FerrosGrynnResult<Option<GraphRef>> {
		if let Some(graph) = self.current.graph_if_exists(name)? {
			return Ok(Some(graph));
		}
		let Some(graph) = self.origin.and_then(|origin| origin.named_graph(name)) else {
			return Ok(None);
		};
		let _reservation = self
			.cache
			.admit_compile_metadata(graph.reference().name().len().saturating_add(256), 1)?;
		Ok(Some(graph.reference().clone()))
	}

	fn functions(
		&self,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<SchemaFunction>>> {
		let Some(functions) = self.functions else {
			return self.current.functions(namespace, name);
		};
		let matches = |source: &&Arc<SchemaFunction>| {
			source.namespace().as_str() == namespace && source.name().as_str() == name
		};
		let count = functions.iter().filter(matches).count();
		let _reservation = self.cache.admit_compile_metadata(
			count
				.checked_mul(std::mem::size_of::<Arc<SchemaFunction>>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT overload list"))?,
			count as u64,
		)?;
		Ok(functions.iter().filter(matches).cloned().collect())
	}

	fn function(&self, id: FunctionId) -> FerrosGrynnResult<Option<Arc<SchemaFunction>>> {
		match self.functions {
			Some(functions) => Ok(functions.iter().find(|source| source.id() == Some(id)).cloned()),
			None => self.current.function(id),
		}
	}

	fn analyzer(&self, name: &str) -> FerrosGrynnResult<Option<Arc<SchemaAnalyzer>>> {
		self.current.analyzer(name)
	}

	fn analyzer_by_id(&self, id: AnalyzerId) -> FerrosGrynnResult<Option<Arc<SchemaAnalyzer>>> {
		self.current.analyzer_by_id(id)
	}
}
