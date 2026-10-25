use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	datastore::{
		AnalyzerFilter,
		AnalyzerId,
		ClassField,
		ClassIndex,
		ClassIndexState,
		ClassSchema,
		ClassSchemaMode,
		ConflictKey,
		FieldUpdate,
		FunctionEffect,
		FunctionId,
		GraphIndex,
		IndexId,
		IndexScope,
		SchemaAnalyzer,
		SchemaFunction,
		SchemaParameter,
		consts::SCHEMA_WRITE_BYTES,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphRef,
};

impl Transaction {
	/// Resolves one transaction-visible durable analyzer by name.
	pub(crate) fn analyzer(
		&mut self,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<SchemaAnalyzer>>> {
		self.prepare_read()?;
		let analyzer = self.working.schema.analyzer(name);
		match analyzer.as_ref().and_then(|analyzer| analyzer.id()) {
			Some(id) => self.record_read(ConflictKey::SchemaAnalyzer(id)),
			None => self.record_read(ConflictKey::AnalyzerCatalogRange),
		}
		Ok(analyzer)
	}

	/// Returns every transaction-visible durable analyzer.
	pub(crate) fn analyzers(&mut self) -> FerrosGrynnResult<Vec<Arc<SchemaAnalyzer>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::AnalyzerCatalogRange);
		Ok(self.working.schema.analyzers().cloned().collect())
	}

	/// Defines one durable text analyzer.
	pub(crate) fn define_analyzer(
		&mut self,
		analyzer: SchemaAnalyzer,
	) -> FerrosGrynnResult<AnalyzerId> {
		self.atomic_mutation(|transaction| transaction.define_analyzer_inner(analyzer))
	}

	/// Mapper-key interning and schema installation share one rollback
	/// boundary.
	fn define_analyzer_inner(
		&mut self,
		mut analyzer: SchemaAnalyzer,
	) -> FerrosGrynnResult<AnalyzerId> {
		self.prepare_schema_mutation()?;
		if analyzer.id.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"define_analyzer requires an unpublished analyzer",
			));
		}
		if self.working.schema.analyzer(analyzer.name.as_str()).is_some() {
			return Err(analyzer_exists(analyzer.name.as_str()));
		}
		self.validate_analyzer_function(&analyzer)?;
		let payload =
			Self::mutation_payload_charge(super::write_set::analyzer_payload_bytes(&analyzer));
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		self.intern_analyzer_mapper_keys(&analyzer)?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		let id = AnalyzerId::from_schema_id(self.store.inner.allocate_schema_id()?);
		analyzer.id = Some(id);
		self.working.install_analyzer(analyzer.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaAnalyzer(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::AnalyzerCatalogRange);
		self.record_mutation(Mutation::DefineAnalyzer(analyzer));
		Ok(id)
	}

	/// Replaces one analyzer definition while preserving identity and name.
	pub(crate) fn replace_analyzer(
		&mut self,
		name: &str,
		replacement: SchemaAnalyzer,
	) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|transaction| transaction.replace_analyzer_inner(name, replacement))
	}

	/// A resource failure cannot leave newly interned mapper keys behind.
	fn replace_analyzer_inner(
		&mut self,
		name: &str,
		mut replacement: SchemaAnalyzer,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		let current = self.working.schema.analyzer(name).ok_or_else(|| analyzer_missing(name))?;
		if replacement.name() != current.name() {
			return Err(FerrosGrynnError::invalid_argument(
				"analyzer identity and name are immutable",
			));
		}
		let id = current.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published analyzer has no AnalyzerId")
		})?;
		replacement.id = Some(id);
		replacement.version = current.version().next()?;
		self.validate_analyzer_function(&replacement)?;
		let payload =
			Self::mutation_payload_charge(super::write_set::analyzer_payload_bytes(&replacement));
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		self.intern_analyzer_mapper_keys(&replacement)?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		self.working.replace_analyzer(replacement.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaAnalyzer(id), SCHEMA_WRITE_BYTES);
		self.record_mutation(Mutation::ReplaceAnalyzer(replacement));
		Ok(())
	}

	/// Removes one unreferenced durable analyzer.
	pub(crate) fn remove_analyzer(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let id = self
			.working
			.schema
			.analyzer(name)
			.and_then(|analyzer| analyzer.id())
			.ok_or_else(|| analyzer_missing(name))?;
		self.working.remove_analyzer(id)?;
		self.insert_write(ConflictKey::SchemaAnalyzer(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::AnalyzerCatalogRange);
		self.mutations.push(Mutation::RemoveAnalyzer(id));
		Ok(())
	}

	/// Validates analyzer function.
	fn validate_analyzer_function(&mut self, analyzer: &SchemaAnalyzer) -> FerrosGrynnResult<()> {
		let Some((id, version)) = analyzer.function() else {
			return Ok(());
		};
		let function = self.function_by_id(id)?.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!("function {id} does not exist"))
		})?;
		if function.version() != version
			|| function.effect() != FunctionEffect::Pure
			|| function.parameters().len() != 1
			|| function
				.parameters()
				.next()
				.is_none_or(|parameter| parameter.value_type() != &crate::values::Type::String)
			|| function.result()
				!= &crate::values::Type::Array(Box::new(crate::values::Type::Object))
		{
			return Err(FerrosGrynnError::invalid_argument(
				"analyzer FUNCTION must be PURE fn(string) -> array<object>",
			));
		}
		Ok(())
	}

	/// Returns the intern analyzer mapper keys result.
	fn intern_analyzer_mapper_keys(&mut self, analyzer: &SchemaAnalyzer) -> FerrosGrynnResult<()> {
		for filter in analyzer.filters() {
			if let AnalyzerFilter::Mapper(entries) = filter {
				for source in entries.keys() {
					self.intern_property(source.as_str())?;
				}
			}
		}
		Ok(())
	}

	/// Returns every transaction-visible overload for one qualified name.
	pub(crate) fn functions_named(
		&mut self,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<SchemaFunction>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::FunctionCatalogRange);
		Ok(self
			.working
			.schema
			.functions()
			.filter(|function| {
				function.namespace().as_str() == namespace && function.name().as_str() == name
			})
			.cloned()
			.collect())
	}

	/// Resolves one transaction-visible function by stable identity.
	pub(crate) fn function_by_id(
		&mut self,
		id: FunctionId,
	) -> FerrosGrynnResult<Option<Arc<SchemaFunction>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::SchemaFunction(id));
		Ok(self.working.schema.functions.get(&id).cloned())
	}

	/// Returns all transaction-visible named-function definitions.
	pub(crate) fn functions(&mut self) -> FerrosGrynnResult<Vec<Arc<SchemaFunction>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::FunctionCatalogRange);
		Ok(self.working.schema.functions().cloned().collect())
	}

	/// Defines one durable named-function overload.
	pub(crate) fn define_function(
		&mut self,
		mut function: SchemaFunction,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		if function.id.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"define_function requires an unpublished function",
			));
		}
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::function_payload_bytes(&function),
		)))?;
		if self.working.schema.function_signatures.contains_key(&function.signature()) {
			return Err(FerrosGrynnError::invalid_argument("function overload already exists"));
		}
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let id = FunctionId::from_schema_id(self.store.inner.allocate_schema_id()?);
		function.id = Some(id);
		self.working.install_function(function.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaFunction(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::FunctionCatalogRange);
		self.record_mutation(Mutation::DefineFunction(function));
		Ok(())
	}

	/// Replaces one function body and metadata while preserving its signature.
	pub(crate) fn replace_function(
		&mut self,
		mut replacement: SchemaFunction,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::function_payload_bytes(&replacement),
		)))?;
		let id = replacement
			.id
			.ok_or_else(|| FerrosGrynnError::invalid_argument("replacement function has no id"))?;
		replacement.version = replacement.version.next()?;
		self.working.replace_function(replacement.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaFunction(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::FunctionCatalogRange);
		self.record_mutation(Mutation::ReplaceFunction(replacement));
		Ok(())
	}

	/// Removes one named-function overload.
	pub(crate) fn remove_function(&mut self, id: FunctionId) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		self.working.remove_function(id, self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaFunction(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::FunctionCatalogRange);
		self.mutations.push(Mutation::RemoveFunction(id));
		Ok(())
	}

	/// Resolves one transaction-visible durable database parameter.
	pub(crate) fn parameter(
		&mut self,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<SchemaParameter>>> {
		self.prepare_read()?;
		let parameter = self.working.schema.parameter(name);
		match parameter.as_ref().and_then(|parameter| parameter.id()) {
			Some(id) => self.record_read(ConflictKey::SchemaParameter(id)),
			None => self.record_read(ConflictKey::ParameterCatalogRange),
		}
		Ok(parameter)
	}

	/// Returns all transaction-visible durable parameter values by name.
	pub(crate) fn parameter_bindings(&mut self) -> FerrosGrynnResult<crate::values::Object> {
		Ok(self
			.parameters()?
			.into_iter()
			.map(|parameter| (parameter.name().clone(), parameter.value().clone()))
			.collect())
	}

	/// Returns all transaction-visible durable parameter definitions.
	pub(crate) fn parameters(&mut self) -> FerrosGrynnResult<Vec<Arc<SchemaParameter>>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::ParameterCatalogRange);
		Ok(self.working.schema.parameters().cloned().collect())
	}

	/// Defines one durable database parameter.
	pub(crate) fn define_parameter(
		&mut self,
		mut parameter: SchemaParameter,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		if parameter.id.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"define_parameter requires an unpublished parameter",
			));
		}
		if self.working.schema.parameter(parameter.name.as_str()).is_some() {
			return Err(parameter_exists(parameter.name.as_str()));
		}
		parameter.value.ensure_storable()?;
		let payload =
			Self::mutation_payload_charge(super::write_set::parameter_payload_bytes(&parameter));
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		self.intern_value_keys(&parameter.value)?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		let id = self.store.inner.allocate_schema_id()?;
		parameter.id = Some(id);
		self.working.install_parameter(parameter.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaParameter(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::ParameterCatalogRange);
		self.record_mutation(Mutation::DefineParameter(parameter));
		Ok(())
	}

	/// Replaces a durable parameter value and/or comment while preserving
	/// identity.
	pub(crate) fn replace_parameter(
		&mut self,
		name: &str,
		value: Option<crate::values::Value>,
		comment: Option<FieldUpdate<crate::values::Text>>,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let previous =
			self.working.schema.parameter(name).ok_or_else(|| parameter_missing(name))?;
		let comment_bytes = match &comment {
			Some(FieldUpdate::Set(comment)) => comment.estimated_heap_bytes(),
			Some(FieldUpdate::Clear) => 0,
			None => previous.comment().map_or(0, crate::values::Text::estimated_heap_bytes),
		};
		let payload = Self::mutation_payload_charge(
			previous
				.name()
				.estimated_heap_bytes()
				.saturating_add(
					value.as_ref().unwrap_or_else(|| previous.value()).estimated_heap_bytes(),
				)
				.saturating_add(comment_bytes),
		);
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		let mut replacement = previous.as_ref().clone();
		let id = replacement
			.id
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("parameter has no SchemaId"))?;
		if let Some(value) = value {
			value.ensure_storable()?;
			self.intern_value_keys(&value)?;
			replacement.value = value;
		}
		if let Some(comment) = comment {
			replacement.comment = comment.into_optional();
		}
		replacement.version = replacement.version.next()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(payload))?;
		self.working.replace_parameter(replacement.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::SchemaParameter(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::ParameterCatalogRange);
		self.record_mutation(Mutation::ReplaceParameter(replacement));
		Ok(())
	}

	/// Removes one durable database parameter.
	pub(crate) fn remove_parameter(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let id = self
			.working
			.schema
			.parameter(name)
			.and_then(|parameter| parameter.id())
			.ok_or_else(|| parameter_missing(name))?;
		self.working.remove_parameter(id)?;
		self.insert_write(ConflictKey::SchemaParameter(id), SCHEMA_WRITE_BYTES);
		self.mark_range_change(ConflictKey::ParameterCatalogRange);
		self.mutations.push(Mutation::RemoveParameter(id));
		Ok(())
	}

	/// Resolves a class from the transaction-visible schema root.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn class(&mut self, name: &str) -> FerrosGrynnResult<Option<Arc<ClassSchema>>> {
		self.prepare_read()?;
		Ok(self.working.schema.class(name))
	}

	/// Resolves a named graph from the transaction-visible schema root.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn named_graph(&mut self, name: &str) -> FerrosGrynnResult<Option<GraphRef>> {
		self.prepare_read()?;
		let schema = self.working.schema.named_graph(name);
		if let Some(schema) = &schema {
			self.record_read(ConflictKey::GraphDefinition(schema.id()));
		}
		Ok(schema.map(|schema| schema.reference().clone()))
	}

	/// Returns all transaction-visible class definitions in stable identity
	/// order.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn classes(&mut self) -> FerrosGrynnResult<Vec<Arc<ClassSchema>>> {
		self.prepare_read()?;
		Ok(self.working.schema.classes().cloned().collect())
	}

	/// Returns all transaction-visible graph definitions in stable identity
	/// order.
	///
	/// # Errors
	///
	/// Returns a snapshot-lifetime or read-committed refresh error.
	pub fn graphs(&mut self) -> FerrosGrynnResult<Vec<crate::datastore::GraphSchema>> {
		self.prepare_read()?;
		self.record_read(ConflictKey::GraphCatalogRange);
		Ok(self.working.schema.graphs().map(|graph| graph.as_ref().clone()).collect())
	}

	/// Returns one graph-owned definition from the current transaction
	/// snapshot.
	///
	/// # Errors
	/// Returns a missing graph or transaction refresh error.
	pub fn graph_index(
		&mut self,
		graph: &str,
		name: &str,
	) -> FerrosGrynnResult<Option<GraphIndex>> {
		self.prepare_read()?;
		let schema = self
			.working
			.schema
			.named_graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph))?;
		self.record_read(ConflictKey::GraphDefinition(schema.id()));
		Ok(schema.indexes().find(|index| index.name().as_str() == name).cloned())
	}

	/// Defines a graph-owned index through the shared schema identity clock.
	///
	/// # Errors
	/// Returns a graph, duplicate-definition, dependency, transaction, or
	/// resource error.
	pub fn define_graph_index(
		&mut self,
		graph: &str,
		mut index: GraphIndex,
	) -> FerrosGrynnResult<IndexId> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let current = self
			.working
			.schema
			.named_graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph))?;
		if current.indexes().len() == 4096 {
			return Err(FerrosGrynnError::invalid_argument(
				"graph supports at most 4096 user indexes",
			));
		}
		if index.id.is_some()
			|| index.graph.is_some()
			|| current.indexes().any(|existing| existing.name() == index.name())
		{
			return Err(FerrosGrynnError::invalid_argument("graph index is already defined"));
		}
		index.validate()?;
		index.validate_family(current.reference().kind())?;
		self.validate_graph_index_functions(&mut index)?;
		let write_bytes = SCHEMA_WRITE_BYTES.saturating_add(index.descriptor()?.len());
		self.ensure_capacity(write_bytes)?;
		let id = IndexId::from_schema_id(self.store.inner.allocate_schema_id()?);
		index.id = Some(id);
		index.graph = Some(current.id());
		let mut indexes = current.indexes().cloned().collect::<Vec<_>>();
		indexes.push(index);
		indexes.sort_by_key(GraphIndex::id);
		self.publish_graph_replacement(current.replace_indexes(indexes)?, write_bytes)?;
		Ok(id)
	}

	/// Replaces a graph user index while preserving its identity and advancing
	/// its version. Materialized replacements require a new rebuild, including
	/// definitions cloned from a Ready index; OnDemand replacements remain
	/// Ready.
	///
	/// # Errors
	/// Returns a missing graph/index, definition, dependency, transaction, or
	/// resource error.
	pub fn overwrite_graph_index(
		&mut self,
		graph: &str,
		mut index: GraphIndex,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		let current = self
			.working
			.schema
			.named_graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph))?;
		let mut indexes = current.indexes().cloned().collect::<Vec<_>>();
		let position = indexes
			.iter()
			.position(|old| old.name() == index.name())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph user index is absent"))?;
		index.validate()?;
		index.validate_family(current.reference().kind())?;
		self.validate_graph_index_functions(&mut index)?;
		index.id = indexes[position].id;
		index.graph = Some(current.id());
		index.version = indexes[position].version.next()?;
		// An artifact belongs to one definition version, never to its Ready
		// clone.
		index.state = if index.is_on_demand() {
			ClassIndexState::Ready
		} else {
			ClassIndexState::Stale
		};
		index.build_revision = 0;
		let write_bytes = SCHEMA_WRITE_BYTES.saturating_add(index.descriptor()?.len());
		self.ensure_capacity(write_bytes)?;
		indexes[position] = index;
		self.publish_graph_replacement(current.replace_indexes(indexes)?, write_bytes)
	}

	/// Checks the additive function signature shared by SDK and GQL
	/// definitions.
	fn validate_graph_index_functions(&mut self, index: &mut GraphIndex) -> FerrosGrynnResult<()> {
		if !index.is_on_demand()
			&& let crate::datastore::GraphIndexDefinition::Path {
				cost:
					crate::datastore::GraphIndexCost::Function {
						function,
						version,
					},
				..
			} = index.definition()
		{
			let dependencies = crate::gql::PureFunctionEvaluator::path_dependencies(
				&self.working,
				*function,
				*version,
			)?;
			index.dependencies.extend(dependencies);
			index.validate()?;
		}
		for &(function, version) in &index.dependencies {
			let dependency = self.function_by_id(function)?.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("graph index function dependency is absent")
			})?;
			if dependency.version() != version
				|| !index.is_on_demand() && dependency.effect() != FunctionEffect::Pure
			{
				return Err(FerrosGrynnError::invalid_argument(
					"graph index requires the captured PURE function version",
				));
			}
		}
		if let crate::datastore::GraphIndexDefinition::Path {
			cost: crate::datastore::GraphIndexCost::Function {
				function,
				version,
			},
			..
		} = index.definition()
		{
			let function = self.function_by_id(*function)?.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("graph COST function is absent")
			})?;
			if function.version() != *version
				|| !(function.parameters().len() == 1
					|| index.is_on_demand()
						&& function.parameters().len() == 2
						&& function.parameters().nth(1).is_some_and(|parameter| {
							parameter.value_type() == &crate::values::Type::Number
						}))
				|| function.parameters().next().is_none_or(|parameter| {
					!matches!(
						parameter.value_type(),
						crate::values::Type::Any
							| crate::values::Type::AtomOccurrence(crate::graph::AtomKind::Edge)
					)
				})
				|| !matches!(
					function.result(),
					crate::values::Type::Int
						| crate::values::Type::Float
						| crate::values::Type::Decimal
						| crate::values::Type::Number
				)
				|| !index.is_on_demand()
					&& (function.effect() != FunctionEffect::Pure || !function.is_cost())
			{
				return Err(FerrosGrynnError::invalid_argument(
					"graph COST requires a numeric edge function; materialization requires PURE \
					 COST",
				));
			}
		}
		Ok(())
	}

	/// Removes a graph-owned user definition from the transaction-private
	/// catalog.
	///
	/// # Errors
	/// Returns a missing graph/index, transaction, or resource error.
	/// Engine-owned label, adjacency, and endpoint capabilities cannot be
	/// removed by this API.
	pub fn remove_graph_index(&mut self, graph: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let current = self
			.working
			.schema
			.named_graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph))?;
		let mut indexes = current.indexes().cloned().collect::<Vec<_>>();
		let position =
			indexes.iter().position(|index| index.name().as_str() == name).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("graph user index is absent or engine-owned")
			})?;
		indexes.remove(position);
		self.publish_graph_replacement(current.replace_indexes(indexes)?, SCHEMA_WRITE_BYTES)
	}

	/// Publishes a graph catalog replacement through normal FCW/SSI replay.
	pub(super) fn publish_graph_replacement(
		&mut self,
		replacement: crate::datastore::GraphSchema,
		write_bytes: usize,
	) -> FerrosGrynnResult<()> {
		let payload =
			Self::mutation_payload_charge(super::write_set::graph_payload_bytes(&replacement));
		self.ensure_capacity(write_bytes.saturating_add(payload))?;
		let id = replacement.id();
		self.working.replace_graph_schema(replacement.clone(), self.snapshot.commit_ts)?;
		self.insert_write(ConflictKey::GraphDefinition(id), write_bytes);
		self.mark_range_change(ConflictKey::GraphCatalogRange);
		self.record_mutation(Mutation::ReplaceGraph(replacement));
		Ok(())
	}

	/// Replaces graph properties.
	pub(crate) fn replace_graph_properties(
		&mut self,
		name: &str,
		properties: crate::values::PropertyMap,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(
			SCHEMA_WRITE_BYTES
				.saturating_add(Self::mutation_payload_charge(properties.estimated_heap_bytes())),
		)?;
		let current = self
			.working
			.schema
			.named_graph(name)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?;
		let replacement = current.replace_properties(properties)?;
		self.publish_graph_replacement(replacement, SCHEMA_WRITE_BYTES)
	}

	/// Replaces graph definition.
	pub(crate) fn replace_graph_definition(
		&mut self,
		name: &str,
		properties: Option<crate::values::PropertyMap>,
		constraints: crate::graph::GraphTopologyConstraints,
		root: Option<crate::values::RecordId>,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let current = self
			.working
			.schema
			.named_graph(name)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?;
		let replacement = current.replace_definition(properties, constraints, root)?;
		self.publish_graph_replacement(replacement, SCHEMA_WRITE_BYTES)
	}

	/// Adds one field to an existing class.
	///
	/// # Errors
	///
	/// Returns a schema, identity-capacity, read-only, snapshot, or
	/// write-budget error.
	/// Failed admission preserves both the class and the property dictionary.
	pub fn define_field(&mut self, class: &str, field: ClassField) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|transaction| transaction.define_field_inner(class, field))
	}

	/// Restores dictionary interning when final replacement admission fails.
	fn define_field_inner(&mut self, class: &str, mut field: ClassField) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::field_payload_bytes(&field),
		)))?;
		let mut definition = self.require_class(class)?;
		let class_id = definition.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		field.id = Some(self.store.inner.allocate_schema_id()?);
		field.class_id = Some(class_id);
		field.key = Some(self.intern_property(field.name().as_str())?);
		definition = definition.with_field(field)?;
		self.publish_class_replacement(definition)
	}

	/// Replaces one field type while preserving the field identity.
	///
	/// Existing values must already satisfy the new declaration; until the full
	/// value coercion matrix lands, a populated field cannot change type.
	///
	/// # Errors
	///
	/// Returns a schema, dependency, data, read-only, snapshot, or write-budget
	/// error.
	pub fn overwrite_field(&mut self, class: &str, field: ClassField) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::field_payload_bytes(&field),
		)))?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.fields
			.iter()
			.position(|current| current.name() == field.name())
			.ok_or_else(|| FerrosGrynnError::unknown_field(field.name().as_str()))?;
		let current = definition.fields[position].clone();
		if current.value_type() != field.value_type()
			&& self.class_property_is_populated(&definition, &current)?
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{}.{}' contains values and cannot change type without validation",
				class,
				field.name()
			)));
		}
		let mut replacement = field;
		replacement.id = current.id;
		replacement.class_id = current.class_id;
		replacement.key = current.key;
		replacement.version = current.version.next()?;
		definition.fields[position] = replacement;
		self.publish_class_replacement(definition)
	}

	/// Removes one unused field definition.
	///
	/// # Errors
	///
	/// Returns an error when data or an index still depends on the field.
	pub fn remove_field(&mut self, class: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.fields
			.iter()
			.position(|field| field.name().as_str() == name)
			.ok_or_else(|| FerrosGrynnError::unknown_field(name))?;
		if definition.indexes.iter().any(|index| index.fields().any(|field| field.as_str() == name))
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{class}.{name}' is referenced by an index"
			)));
		}
		if self.class_property_is_populated(&definition, &definition.fields[position])? {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{class}.{name}' still contains values"
			)));
		}
		definition.fields.remove(position);
		self.publish_class_replacement(definition)
	}

	/// Adds one explicit base or effective class index.
	///
	/// # Errors
	///
	/// Returns a schema, graph, uniqueness, identity-capacity, or resource
	/// error.
	pub fn define_index(&mut self, class: &str, mut index: ClassIndex) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::index_payload_bytes(&index),
		)))?;
		self.validate_index_scope(index.scope())?;
		index.id = Some(IndexId::from_schema_id(self.store.inner.allocate_schema_id()?));
		let definition = self.require_class(class)?.with_index(index)?;
		self.publish_class_replacement(definition)
	}

	/// Replaces one class index while preserving its stable identity.
	///
	/// # Errors
	///
	/// Returns a schema, graph, uniqueness, read-only, snapshot, or resource
	/// error.
	pub fn overwrite_index(&mut self, class: &str, mut index: ClassIndex) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::index_payload_bytes(&index),
		)))?;
		self.validate_index_scope(index.scope())?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.indexes
			.iter()
			.position(|current| current.name() == index.name())
			.ok_or_else(|| Self::index_missing(class, index.name().as_str()))?;
		index.id = definition.indexes[position].id;
		index.version = definition.indexes[position].version.next()?;
		definition.indexes.remove(position);
		definition = definition.with_index(index)?;
		self.publish_class_replacement(definition)
	}

	/// Marks one index unavailable before explicit removal.
	///
	/// # Errors
	///
	/// Returns a schema, read-only, snapshot, or write-budget error.
	pub fn prepare_index_removal(&mut self, class: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let index = definition
			.indexes
			.iter_mut()
			.find(|index| index.name().as_str() == name)
			.ok_or_else(|| Self::index_missing(class, name))?;
		index.state = ClassIndexState::PreparedForRemoval;
		index.version = index.version.next()?;
		self.publish_class_replacement(definition)
	}

	/// Removes one explicit class index.
	///
	/// # Errors
	///
	/// Returns a schema, read-only, snapshot, or write-budget error.
	pub fn remove_index(&mut self, class: &str, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		let position = definition
			.indexes
			.iter()
			.position(|index| index.name().as_str() == name)
			.ok_or_else(|| Self::index_missing(class, name))?;
		definition.indexes.remove(position);
		self.publish_class_replacement(definition)
	}

	/// Changes only the validation mode of an existing class.
	///
	/// # Errors
	///
	/// Returns an error when undeclared stored properties prevent Schemafull.
	pub fn set_class_schema_mode(
		&mut self,
		class: &str,
		mode: ClassSchemaMode,
	) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let mut definition = self.require_class(class)?;
		if mode == ClassSchemaMode::Schemafull && self.class_has_undeclared_properties(&definition)
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"class '{class}' contains undeclared properties"
			)));
		}
		definition.mode = mode;
		self.publish_class_replacement(definition)
	}

	/// Removes an empty class.
	///
	/// # Errors
	///
	/// Returns an error when the class is absent, owns atoms, or mutation
	/// policy rejects the operation.
	pub fn remove_class(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|transaction| transaction.remove_class_inner(name))
	}

	/// Removes an owner and its retained-source policy as one fallible
	/// mutation.
	fn remove_class_inner(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let class = self.require_class(name)?;
		let id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		self.remove_source_events(crate::datastore::EventTarget::Class(id))?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		self.working.remove_class(id, self.snapshot.commit_ts)?;
		self.retire_missing_changefeeds()?;
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::RemoveClass(id));
		Ok(())
	}

	/// Removes a graph while retaining its global atoms.
	///
	/// # Errors
	///
	/// Returns an error when the graph is absent or mutation policy rejects the
	/// operation.
	pub fn remove_graph(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.atomic_mutation(|transaction| transaction.remove_graph_inner(name))
	}

	/// Removes a graph tree and its retained policies without partial private
	/// writes.
	fn remove_graph_inner(&mut self, name: &str) -> FerrosGrynnResult<()> {
		self.prepare_schema_mutation()?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		let graph = self
			.working
			.schema
			.named_graph(name)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))?
			.id();
		let ranges = self
			.working
			.graph(graph)
			.into_iter()
			.flat_map(crate::datastore::GraphRoot::occurrences)
			.map(|occurrence| self.atom_domain(occurrence.global_id()))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let removed = self
			.working
			.schema
			.all_graphs()
			.filter(|schema| schema.id() == graph || schema.owner_root() == Some(graph))
			.map(|schema| schema.id())
			.collect::<Vec<_>>();
		for removed in removed {
			self.admit_bridge_graph_removal(removed, None)?;
		}
		self.remove_source_events(crate::datastore::EventTarget::Graph(graph))?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES)?;
		self.working.remove_graph(graph)?;
		self.retire_missing_changefeeds()?;
		self.insert_write(ConflictKey::GraphDefinition(graph), 0);
		self.mark_range_change(ConflictKey::GraphCatalogRange);
		for (class, kind) in ranges {
			self.mark_range_change(ConflictKey::GraphClassRange(graph, class));
			self.mark_range_change(ConflictKey::GraphKindRange(graph, kind));
		}
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.mutations.push(Mutation::RemoveGraph(graph));
		Ok(())
	}

	/// Publishes class replacement.
	fn publish_class_replacement(&mut self, mut definition: ClassSchema) -> FerrosGrynnResult<()> {
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::class_payload_bytes(&definition),
		)))?;
		definition.version = definition.version.next()?;
		self.working.replace_class(definition.clone(), self.snapshot.commit_ts)?;
		self.write_bytes += SCHEMA_WRITE_BYTES;
		self.record_mutation(Mutation::ReplaceClass(definition));
		Ok(())
	}

	/// Returns the require class result.
	fn require_class(&self, name: &str) -> FerrosGrynnResult<ClassSchema> {
		let class = self
			.working
			.schema
			.class(name)
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(name))?;
		self.ensure_capacity(SCHEMA_WRITE_BYTES.saturating_add(Self::mutation_payload_charge(
			super::write_set::class_payload_bytes(&class),
		)))?;
		Ok(class.as_ref().clone())
	}

	/// Validates index scope.
	fn validate_index_scope(&self, scope: IndexScope) -> FerrosGrynnResult<()> {
		if let IndexScope::Effective {
			graph,
		} = scope
			&& self.working.schema.graph(graph).is_none()
		{
			return Err(FerrosGrynnError::graph_does_not_exist(graph.to_string()));
		}
		Ok(())
	}

	/// Returns the class property is populated result.
	fn class_property_is_populated(
		&self,
		class: &ClassSchema,
		field: &ClassField,
	) -> FerrosGrynnResult<bool> {
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("published class is missing ClassId")
		})?;
		let Some(key) = field.key else {
			return Ok(false);
		};
		for atom in self.working.atoms.atoms().filter(|atom| atom.class_id() == class_id) {
			if atom.properties().get(key).is_some() {
				return Ok(true);
			}
			for (graph, local) in self.working.memberships(atom.id()).into_iter().flatten() {
				if self
					.working
					.occurrence(*graph, *local)
					.and_then(|occurrence| occurrence.overlay())
					.and_then(|overlay| overlay.get(key))
					.is_some()
				{
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	/// Returns whether the class has undeclared properties condition holds.
	fn class_has_undeclared_properties(&self, class: &ClassSchema) -> bool {
		let Some(class_id) = class.id() else {
			return true;
		};
		let declared = class
			.fields
			.iter()
			.filter_map(|field| field.key)
			.collect::<std::collections::BTreeSet<_>>();
		self.working
			.atoms
			.atoms()
			.filter(|atom| atom.class_id() == class_id)
			.any(|atom| atom.properties().iter().any(|(key, _value)| !declared.contains(&key)))
	}

	/// Returns the index missing result.
	fn index_missing(class: &str, name: &str) -> FerrosGrynnError {
		FerrosGrynnError::invalid_argument(format!("index '{class}.{name}' does not exist"))
	}
}

/// Returns the parameter exists result.
fn parameter_exists(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("parameter '${name}' already exists"))
}

/// Returns the parameter missing result.
fn parameter_missing(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("parameter '${name}' does not exist"))
}

/// Returns the analyzer exists result.
fn analyzer_exists(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("analyzer '{name}' already exists"))
}

/// Returns the analyzer missing result.
fn analyzer_missing(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("analyzer '{name}' does not exist"))
}

#[cfg(test)]
/// Regression tests for schema behavior.
mod tests {
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		datastore::{
			ClassField,
			ClassIndex,
			ClassIndexKind,
			ClassSchema,
			ClassSchemaMode,
			Datastore,
			IndexScope,
		},
		values::{AtomKind, Type},
	};

	/// Verifies class ddl preserves identities and rebuilds indexes.
	#[test]
	fn class_ddl_preserves_identities_and_rebuilds_indexes() {
		let store = Datastore::in_memory();
		let mut transaction = store.begin();
		let class_id = transaction
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		transaction.define_field("person", ClassField::new("email", Type::String)).assert_ok();
		transaction
			.define_index(
				"person",
				ClassIndex::new("by_email", ClassIndexKind::Unique, IndexScope::Base, ["email"])
					.assert_ok(),
			)
			.assert_ok();
		transaction.set_class_schema_mode("person", ClassSchemaMode::Schemafull).assert_ok();
		transaction.commit().assert_ok();

		let class = store.class("person").assert_some();
		assert_eq!(class.id(), Some(class_id));
		assert_eq!(class.fields().len(), 1);
		assert_eq!(class.indexes().len(), 1);
		assert_eq!(class.schema_mode(), ClassSchemaMode::Schemafull);
	}

	/// Verifies class removal rejects live atoms.
	#[test]
	fn class_removal_rejects_live_atoms() {
		let store = Datastore::in_memory();
		let mut transaction = store.begin();
		transaction
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		transaction
			.create_atom(
				crate::values::RecordId::new(
					"person",
					crate::values::RecordIdPart::String("alice".into()),
				)
				.assert_ok(),
				crate::values::PropertyMap::new(),
			)
			.assert_ok();
		assert!(transaction.remove_class("person").is_err());
	}
}
