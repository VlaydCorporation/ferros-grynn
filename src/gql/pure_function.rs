//! Evaluation boundary for durable PURE functions used by physical builders.

use std::{collections::BTreeMap, sync::Arc};

use super::{
	QueryExecutionOptions,
	analysis::{QueryCatalog, compile_named_function},
	closure::LanguageClosureState,
	execution::{invoke_logical_closure, query_schema},
	ir::NamedFunctionKey,
	options::ExecutionControl,
};
use crate::{
	datastore::{
		AnalyzedToken,
		AnalyzerId,
		ClassSchema,
		ClassSchemaMode,
		DatabaseRoot,
		FunctionEffect,
		FunctionId,
		SchemaAnalyzer,
		SchemaFunction,
		SchemaVersion,
		analyze_function_result,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphRef},
	values::{Number, Value, Vector},
};

/// Concrete evaluator reused while one immutable root is encoded.
pub(crate) struct PureFunctionEvaluator<'a> {
	catalog: RootCatalog<'a>,
	cache: Arc<LanguageClosureState>,
	control: ExecutionControl,
}

impl<'a> PureFunctionEvaluator<'a> {
	/// Captures the exact catalog root being persisted.
	pub(crate) fn new(root: &'a DatabaseRoot) -> Self {
		let cache = Arc::new(LanguageClosureState::default());
		let control = ExecutionControl::with_runtime(
			&QueryExecutionOptions::default(),
			None,
			None,
			Arc::clone(&cache),
		);
		Self {
			catalog: RootCatalog(root),
			cache,
			control,
		}
	}

	/// Runs one analyzer function and validates its public token contract.
	pub(crate) fn analyze(
		&self,
		analyzer: &SchemaAnalyzer,
		input: &str,
	) -> FerrosGrynnResult<Vec<AnalyzedToken>> {
		let (function, version) = analyzer.function().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("function analyzer has no function identity")
		})?;
		let result = self.invoke(function, version, &[Value::from(input)])?;
		analyze_function_result(analyzer, input, &result)
	}

	/// Runs one versioned custom HNSW distance.
	pub(crate) fn distance(
		&self,
		function: FunctionId,
		version: SchemaVersion,
		left: &Vector,
		right: &Vector,
	) -> FerrosGrynnResult<f64> {
		let result = self.invoke(function, version, &[
			Value::Vector(left.clone()),
			Value::Vector(right.clone()),
		])?;
		let Value::Number(Number::Float(distance)) = result else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"custom vector distance did not return float",
			));
		};
		if !distance.is_finite() || distance < 0.0 {
			return Err(FerrosGrynnError::invalid_argument(
				"custom vector distance must be finite and non-negative",
			));
		}
		Ok(distance)
	}

	fn invoke(
		&self,
		id: FunctionId,
		version: SchemaVersion,
		arguments: &[Value],
	) -> FerrosGrynnResult<Value> {
		let function = self
			.catalog
			.function(id)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("function does not exist"))?;
		if function.version() != version {
			return Err(FerrosGrynnError::invalid_storage_file(
				"physical index captured a different function version",
			));
		}
		if function.effect() != FunctionEffect::Pure {
			return Err(FerrosGrynnError::invalid_storage_file(
				"physical index function is not PURE",
			));
		}
		let key = NamedFunctionKey {
			id,
			version,
			schema_epoch: self.catalog.schema_epoch(),
			max_depth: function.max_depth(),
			effect: function.effect(),
		};
		if self.cache.cached_named(key).is_none() {
			let plan = compile_named_function(
				&self.catalog,
				&function,
				&[],
				&BTreeMap::new(),
				Some(&self.cache),
			)?;
			self.cache.cache_named_plan(&plan)?;
		}
		let plan = self.control.resolve_named_function(key)?;
		let _frame = self.control.enter_named_function(key)?;
		invoke_logical_closure(&self.control, &plan, &[], arguments, function.body().as_str())
	}
}

struct RootCatalog<'a>(&'a DatabaseRoot);

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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::SchemaFunctionParameter,
		values::{Text, Type},
	};

	#[test]
	fn pure_block_function_runs_without_query_read() -> FerrosGrynnResult<()> {
		let mut root = DatabaseRoot::default();
		let id = FunctionId::new(1).expect("non-zero function id");
		let mut function = SchemaFunction::new(
			Text::from("fn"),
			Text::from("double"),
			vec![SchemaFunctionParameter::new(Text::from("value"), Type::Int)?],
			Type::Int,
			FunctionEffect::Pure,
			false,
			None,
			Text::from("{ LET $next = $value * 2; RETURN $next; }"),
		)?;
		function.id = Some(id);
		Arc::make_mut(&mut root.schema).functions.insert(id, Arc::new(function));
		let evaluator = PureFunctionEvaluator::new(&root);
		assert_eq!(
			evaluator.invoke(id, SchemaVersion::INITIAL, &[Value::from(21_i64)])?,
			Value::from(42_i64)
		);
		Ok(())
	}
}
