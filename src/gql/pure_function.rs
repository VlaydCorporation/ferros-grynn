//! Evaluation boundary for durable PURE functions used by physical builders.

use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc};

use super::{
	QueryExecutionOptions,
	analysis::{QueryCatalog, compile_named_function},
	closure::LanguageClosureState,
	execution::{invoke_root_logical_closure, query_schema},
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
	/// Reuses bounded occurrence resolution and gas for query path predicates.
	pub(in crate::gql) fn fields<'b>(
		&'b self,
		read: &'b dyn Fn(crate::datastore::ConflictKey),
		work: &'b dyn Fn(u64) -> FerrosGrynnResult<()>,
	) -> impl crate::gql::execution::AtomFieldRead + 'b {
		crate::gql::execution::RootFields::new(self.catalog.0, &self.control, read, work)
	}

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

	/// Captures caller-owned scratch and cancellation for an immutable build.
	pub(crate) fn for_builder(
		root: &'a DatabaseRoot,
		scratch_bytes: usize,
		check: fn() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		check()?;
		let compiler_bytes = scratch_bytes / 2;
		let runtime_bytes = NonZeroUsize::new(scratch_bytes - compiler_bytes).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"PURE runtime scratch",
				scratch_bytes as u64,
			)
		})?;
		let cache = Arc::new(LanguageClosureState::bounded(compiler_bytes));
		let options = QueryExecutionOptions {
			barrier_memory_bytes: runtime_bytes,
			..QueryExecutionOptions::default()
		};
		let control = ExecutionControl::with_runtime(&options, None, None, Arc::clone(&cache))
			.with_cooperative_check(check);
		Ok(Self {
			catalog: RootCatalog(root),
			cache,
			control,
		})
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

	/// Evaluates the captured versioned additive edge COST through normal typed
	/// IR.
	///
	/// # Errors
	/// Rejects missing/non-edge occurrences, invalid signatures or numeric
	/// costs.
	pub(crate) fn path_cost(
		&self,
		function: FunctionId,
		version: SchemaVersion,
		graph: &GraphRef,
		local: crate::graph::AtomId,
	) -> FerrosGrynnResult<Number> {
		let occurrence = self
			.catalog
			.0
			.occurrence(graph.id(), local)
			.filter(|occurrence| occurrence.local_id().is_edge())
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("path COST requires an edge occurrence")
			})?;
		let atom =
			self.catalog.0.atom(occurrence.global_id()).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("COST edge base is absent")
			})?;
		let definition = self
			.catalog
			.function(function)?
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("COST function is absent"))?;
		if !Self::valid_path_cost(&definition, Some(1)) {
			return Err(FerrosGrynnError::invalid_argument(
				"path COST requires a unary PURE COST function",
			));
		}
		let argument = Value::from(crate::graph::AtomOccurrenceRef::new(
			crate::graph::AtomRef::new(atom.kind(), atom.record_id().clone()),
			Arc::new(graph.clone()),
		));
		let Value::Number(number) = self.invoke(function, version, &[argument])? else {
			return Err(FerrosGrynnError::invalid_argument("path COST did not return a number"));
		};
		crate::graph::DistanceInterval::from_number(number)?;
		Ok(number)
	}

	/// Resolves transitive versions through the same analyzer used for
	/// execution.
	///
	/// # Errors
	/// Rejects absent/non-PURE definitions, malformed bodies or cache budgets.
	pub(crate) fn path_dependencies(
		root: &'a DatabaseRoot,
		function: FunctionId,
		version: SchemaVersion,
	) -> FerrosGrynnResult<std::collections::BTreeSet<(FunctionId, SchemaVersion)>> {
		let evaluator = Self::new(root);
		evaluator.dependencies(function, version)
	}

	/// Resolves versions under this evaluator's compiler and cancellation caps.
	pub(crate) fn dependencies(
		&self,
		function: FunctionId,
		version: SchemaVersion,
	) -> FerrosGrynnResult<std::collections::BTreeSet<(FunctionId, SchemaVersion)>> {
		self.control.check()?;
		let definition = self
			.catalog
			.function(function)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("path COST function is absent"))?;
		if definition.version() != version || definition.effect() != FunctionEffect::Pure {
			return Err(FerrosGrynnError::invalid_argument(
				"path COST requires the captured PURE version",
			));
		}
		let plan = compile_named_function(
			&self.catalog,
			&definition,
			&[],
			&BTreeMap::new(),
			Some(&self.cache),
		)?;
		self.control.check()?;
		self.cache.cache_named_plan(&plan)?;
		let dependencies = self.cache.named_dependencies();
		if dependencies.len() > 1024 {
			return Err(FerrosGrynnError::invalid_argument(
				"path COST exceeds 1024 transitive function dependencies",
			));
		}
		Ok(dependencies)
	}

	/// Evaluates a versioned unary or prefix-dependent PURE COST contribution.
	pub(crate) fn path_contribution(
		&self,
		function: FunctionId,
		version: SchemaVersion,
		edge: &crate::graph::AtomOccurrenceRef,
		prefix: Number,
		read: &dyn Fn(crate::datastore::ConflictKey),
		work: &dyn Fn(u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Number> {
		let definition = self
			.catalog
			.function(function)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("COST function is absent"))?;
		let parameters = definition.parameters().len();
		if !Self::valid_path_cost(&definition, None) {
			return Err(FerrosGrynnError::invalid_argument(
				"COST requires a unary or prefix-dependent PURE COST function",
			));
		}
		let bytes = edge
			.atom()
			.id()
			.estimated_heap_bytes()
			.checked_add(2 * size_of::<Value>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("COST arguments"))?;
		let mut arguments_account = self.control.barrier_account();
		if !arguments_account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"COST arguments",
				self.control.barrier_memory_limit() as u64,
			));
		}
		let arguments = [Value::from(edge.clone()), Value::Number(prefix)];
		let Value::Number(number) =
			self.invoke_observed(function, version, &arguments[..parameters], read, work)?
		else {
			return Err(FerrosGrynnError::invalid_argument("COST did not return a number"));
		};
		if !crate::math::number_is_finite(number) || number < Number::Int(0) {
			return Err(FerrosGrynnError::invalid_argument(
				"COST requires finite nonnegative contributions",
			));
		}
		Ok(number)
	}

	/// Checks the existing numeric edge/PURE COST contract before typed
	/// invocation.
	pub(crate) fn valid_path_cost(function: &SchemaFunction, arity: Option<usize>) -> bool {
		use crate::values::Type;
		let count = function.parameters().len();
		function.is_cost()
			&& function.effect() == FunctionEffect::Pure
			&& (1..=2).contains(&count)
			&& arity.is_none_or(|arity| arity == count)
			&& function.parameters().next().is_some_and(|parameter| {
				matches!(parameter.value_type(), Type::Any | Type::AtomOccurrence(AtomKind::Edge))
			})
			&& (count == 1
				|| function
					.parameters()
					.nth(1)
					.is_some_and(|parameter| parameter.value_type() == &Type::Number))
			&& matches!(function.result(), Type::Int | Type::Float | Type::Decimal | Type::Number)
	}

	fn invoke(
		&self,
		id: FunctionId,
		version: SchemaVersion,
		arguments: &[Value],
	) -> FerrosGrynnResult<Value> {
		self.invoke_observed(id, version, arguments, &|_key| {}, &|_work| Ok(()))
	}

	/// Preserves snapshot observations and aggregate work across nested calls.
	fn invoke_observed(
		&self,
		id: FunctionId,
		version: SchemaVersion,
		arguments: &[Value],
		read: &dyn Fn(crate::datastore::ConflictKey),
		work: &dyn Fn(u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Value> {
		self.control.check()?;
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
		self.control.check()?;
		let plan = self.control.resolve_named_function(key)?;
		let _frame = self.control.enter_named_function(key)?;
		invoke_root_logical_closure(
			self.catalog.0,
			&self.control,
			&plan,
			arguments,
			function.body().as_str(),
			read,
			work,
		)
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
	use std::cell::Cell;

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

	thread_local! {
		/// Deterministic cancellation inside a function, independent of timing.
		static BUILDER_CHECKS: Cell<usize> = const { Cell::new(0) };
	}

	/// Cancels after typed execution has entered the loop and its child blocks.
	fn cancel_during_function() -> FerrosGrynnResult<()> {
		BUILDER_CHECKS.with(|checks| {
			let next = checks.get() + 1;
			checks.set(next);
			if next > 128 {
				Err(FerrosGrynnError::query_cancelled())
			} else {
				Ok(())
			}
		})
	}

	#[test]
	fn pure_builder_admits_compilers_frames_and_cancels_inside_calls() -> FerrosGrynnResult<()> {
		let mut root = DatabaseRoot::default();
		let id = FunctionId::new(1).ok_or_else(|| FerrosGrynnError::invalid_argument("test id"))?;
		let mut function = SchemaFunction::new(
			"fn",
			"iterate",
			vec![SchemaFunctionParameter::new("value", Type::Any)?],
			Type::Int,
			FunctionEffect::Pure,
			true,
			None,
			"{ FOR $i IN 0..1000000 { LET $copy = $i; }; RETURN 1; }",
		)?;
		function.id = Some(id);
		Arc::make_mut(&mut root.schema).functions.insert(id, Arc::new(function));
		let version = SchemaVersion::INITIAL;
		let evaluator = PureFunctionEvaluator::for_builder(&root, 64, || Ok(()))?;
		assert!(evaluator.dependencies(id, version).is_err());
		BUILDER_CHECKS.set(0);
		let evaluator = PureFunctionEvaluator::for_builder(&root, 2 << 20, cancel_during_function)?;
		let error = evaluator
			.invoke(id, version, &[Value::None])
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("loop did not cancel"))?;
		assert_eq!(error.status().code(), FerrosGrynnError::query_cancelled().status().code());
		assert_eq!(BUILDER_CHECKS.get(), 129);
		assert_eq!(evaluator.control.barrier_memory_available(), 1 << 20);
		BUILDER_CHECKS.set(0);
		evaluator.dependencies(id, version)?;
		let evaluator = PureFunctionEvaluator::for_builder(&root, 2 << 20, || Ok(()))?;
		let oversized = Value::from("x".repeat(2 << 20));
		let error = evaluator
			.invoke(id, version, &[oversized])
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("frame was not rejected"))?;
		assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
		assert_eq!(evaluator.control.barrier_memory_available(), 1 << 20);
		Ok(())
	}
}
