use std::{collections::BTreeMap, sync::Arc};

use crate::{
	datastore::{
		AnalyzerId,
		ClassId,
		ClassIndexOptions,
		ClassIndexState,
		ClassSchema,
		DatabaseRoot,
		FunctionId,
		GraphSchema,
		IndexId,
		SchemaAnalyzer,
		SchemaFunction,
		SchemaId,
		SchemaParameter,
		SchemaVersion,
		VectorDistance,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

impl DatabaseRoot {
	/// Installs one newly published durable text analyzer.
	pub(crate) fn install_analyzer(
		&mut self,
		mut analyzer: SchemaAnalyzer,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = analyzer.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a published analyzer requires AnalyzerId")
		})?;
		if self.schema.analyzers.contains_key(&id)
			|| self.schema.analyzer_names.contains_key(analyzer.name.as_str())
		{
			return Err(analyzer_exists(analyzer.name.as_str()));
		}
		analyzer.published_at = published_at;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.analyzer_versions.insert(id, published_at);
		catalog.analyzer_names.insert(analyzer.name.to_string(), id);
		catalog.analyzers.insert(id, Arc::new(analyzer));
		Ok(())
	}

	/// Replaces one analyzer while preserving its identity and name.
	pub(crate) fn replace_analyzer(
		&mut self,
		mut replacement: SchemaAnalyzer,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = replacement.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a replacement analyzer requires AnalyzerId")
		})?;
		let current = self.schema.analyzers.get(&id).ok_or_else(|| analyzer_missing(id))?;
		if current.name != replacement.name {
			return Err(FerrosGrynnError::invalid_argument(
				"analyzer identity and name are immutable",
			));
		}
		replacement.published_at = published_at;
		let analyzer_versions = BTreeMap::from([(id, replacement.version)]);
		let (dependent_classes, stale_indexes) =
			self.dependent_index_replacements(&analyzer_versions, None, published_at)?;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.analyzer_versions.insert(id, published_at);
		catalog.analyzers.insert(id, Arc::new(replacement));
		for (class_id, class) in dependent_classes {
			catalog.class_versions.insert(class_id, published_at);
			catalog.classes.insert(class_id, Arc::new(class));
		}
		let indexes = Arc::make_mut(&mut self.indexes);
		for index in stale_indexes {
			indexes.remove_explicit_index(index);
		}
		Ok(())
	}

	/// Removes one analyzer that has no dependent indexes.
	pub(crate) fn remove_analyzer(&mut self, id: AnalyzerId) -> FerrosGrynnResult<()> {
		if self.schema.classes.values().any(|class| {
			class.indexes.iter().any(|index| {
				index.fulltext_options().is_some_and(|options| options.analyzer() == id)
			})
		}) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"analyzer {id} is referenced by a Fulltext index"
			)));
		}
		let current =
			self.schema.analyzers.get(&id).cloned().ok_or_else(|| analyzer_missing(id))?;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.analyzers.remove(&id);
		catalog.analyzer_names.remove(current.name.as_str());
		catalog.analyzer_versions.remove(&id);
		Ok(())
	}

	/// Installs one newly published named-function overload.
	pub(crate) fn install_function(
		&mut self,
		mut function: SchemaFunction,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = function.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a published function requires FunctionId")
		})?;
		let signature = function.signature();
		if self.schema.functions.contains_key(&id)
			|| self.schema.function_signatures.contains_key(&signature)
		{
			return Err(function_exists(&function));
		}
		function.published_at = published_at;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.function_versions.insert(id, published_at);
		catalog.function_signatures.insert(signature, id);
		catalog.functions.insert(id, Arc::new(function));
		Ok(())
	}

	/// Replaces one function definition while preserving identity and
	/// signature.
	pub(crate) fn replace_function(
		&mut self,
		mut replacement: SchemaFunction,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = replacement.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a replacement function requires FunctionId")
		})?;
		let current = self.schema.functions.get(&id).ok_or_else(|| function_missing(id))?;
		if current.signature() != replacement.signature() {
			return Err(FerrosGrynnError::invalid_argument(
				"function identity and overload signature are immutable",
			));
		}
		replacement.published_at = published_at;
		let mut dependent_analyzers = Vec::new();
		let mut analyzer_versions = BTreeMap::new();
		for analyzer in self.schema.analyzers.values() {
			if analyzer.function.is_some_and(|(function, _version)| function == id) {
				let mut analyzer = analyzer.as_ref().clone();
				analyzer.version = analyzer.version.next()?;
				analyzer.function = Some((id, replacement.version));
				analyzer.published_at = published_at;
				let analyzer_id = analyzer.id.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("published analyzer has no AnalyzerId")
				})?;
				analyzer_versions.insert(analyzer_id, analyzer.version);
				dependent_analyzers.push(analyzer);
			}
		}
		let (dependent_classes, stale_indexes) = self.dependent_index_replacements(
			&analyzer_versions,
			Some((id, replacement.version)),
			published_at,
		)?;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.function_versions.insert(id, published_at);
		catalog.functions.insert(id, Arc::new(replacement));
		for analyzer in dependent_analyzers {
			let analyzer_id = analyzer.id.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("published analyzer has no AnalyzerId")
			})?;
			catalog.analyzer_versions.insert(analyzer_id, published_at);
			catalog.analyzers.insert(analyzer_id, Arc::new(analyzer));
		}
		for (class_id, class) in dependent_classes {
			catalog.class_versions.insert(class_id, published_at);
			catalog.classes.insert(class_id, Arc::new(class));
		}
		let indexes = Arc::make_mut(&mut self.indexes);
		for index in stale_indexes {
			indexes.remove_explicit_index(index);
		}
		self.invalidate_graph_function_indexes(
			id,
			self.schema.functions.get(&id).map(|function| function.version()),
			published_at,
		)?;
		Ok(())
	}

	/// Removes one named-function overload by stable identity.
	pub(crate) fn remove_function(
		&mut self,
		id: FunctionId,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		if self
			.schema
			.analyzers
			.values()
			.any(|analyzer| analyzer.function.is_some_and(|(function, _version)| function == id))
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"function {id} is referenced by a durable analyzer"
			)));
		}
		if self.schema.classes.values().any(|class| {
			class.indexes.iter().any(|index| {
				index.hnsw_options().is_some_and(|options| {
					matches!(
						options.distance(),
						VectorDistance::Custom {
							function,
							..
						} if function == id
					)
				})
			})
		}) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"function {id} is referenced by an HNSW index"
			)));
		}
		let current =
			self.schema.functions.get(&id).cloned().ok_or_else(|| function_missing(id))?;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.functions.remove(&id);
		catalog.function_signatures.remove(&current.signature());
		catalog.function_versions.remove(&id);
		self.invalidate_graph_function_indexes(id, None, published_at)?;
		Ok(())
	}

	/// Invalidates versioned graph function dependencies in the same private
	/// root.
	fn invalidate_graph_function_indexes(
		&mut self,
		function: FunctionId,
		version: Option<SchemaVersion>,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let graphs = self
			.schema
			.all_graphs()
			.filter(|graph| {
				graph.indexes().any(|index| index.dependencies().any(|(id, _)| *id == function))
			})
			.cloned()
			.collect::<Vec<_>>();
		for graph in graphs {
			let mut indexes = graph.indexes().cloned().collect::<Vec<_>>();
			for index in &mut indexes {
				if !index.dependencies().any(|(id, _)| *id == function) {
					continue;
				}
				if let Some(version) = version {
					index.dependencies.retain(|(id, _)| *id != function);
					index.dependencies.insert((function, version));
					if let crate::datastore::GraphIndexDefinition::Path {
						cost:
							crate::datastore::GraphIndexCost::Function {
								function: id,
								version: captured,
							},
						..
					} = &mut index.definition
						&& *id == function
					{
						*captured = version;
					}
				}
				index.version = index.version.next()?;
				index.state = ClassIndexState::Stale;
				if let Some(id) = index.id() {
					Arc::make_mut(&mut self.path_artifacts).remove(&id);
				}
			}
			let mut replacement = graph.replace_indexes(indexes)?;
			replacement.published_at = published_at;
			let catalog = Arc::make_mut(&mut self.schema);
			catalog.graph_versions.insert(graph.id(), published_at);
			catalog.graphs.insert(graph.id(), Arc::new(replacement));
			self.mark_version(
				crate::datastore::ConflictKey::GraphDefinition(graph.id()),
				published_at,
			);
			self.mark_version(crate::datastore::ConflictKey::GraphCatalogRange, published_at);
		}
		Ok(())
	}

	/// Prepares catalog replacements for every Fulltext or HNSW definition
	/// whose captured analyzer/function version changed.
	fn dependent_index_replacements(
		&self,
		analyzer_versions: &BTreeMap<AnalyzerId, SchemaVersion>,
		function: Option<(FunctionId, SchemaVersion)>,
		published_at: u64,
	) -> FerrosGrynnResult<(Vec<(ClassId, ClassSchema)>, Vec<IndexId>)> {
		let mut classes = Vec::new();
		let mut stale_indexes = Vec::new();
		for (class_id, current) in &self.schema.classes {
			let mut replacement = current.as_ref().clone();
			let mut changed = false;
			for index in &mut replacement.indexes {
				let options = match index.options {
					ClassIndexOptions::Fulltext(mut options) => {
						let Some(version) = analyzer_versions.get(&options.analyzer) else {
							continue;
						};
						options.analyzer_version = *version;
						ClassIndexOptions::Fulltext(options)
					}
					ClassIndexOptions::Hnsw(mut options) => {
						let Some((function, version)) = function else {
							continue;
						};
						if !matches!(
							options.distance,
							VectorDistance::Custom {
								function: candidate,
								..
							} if candidate == function
						) {
							continue;
						}
						options.distance = VectorDistance::Custom {
							function,
							version,
						};
						ClassIndexOptions::Hnsw(options)
					}
					ClassIndexOptions::Scalar
					| ClassIndexOptions::DiskAnn(_)
					| ClassIndexOptions::Geometry(_) => continue,
				};
				index.options = options;
				index.version = index.version.next()?;
				index.state = ClassIndexState::Stale;
				stale_indexes.push(index.id.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"published dependent index has no IndexId",
					)
				})?);
				changed = true;
			}
			if changed {
				replacement.version = replacement.version.next()?;
				replacement.published_at = published_at;
				classes.push((*class_id, replacement));
			}
		}
		Ok((classes, stale_indexes))
	}

	/// Installs one newly published durable database parameter.
	pub(crate) fn install_parameter(
		&mut self,
		mut parameter: SchemaParameter,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = parameter.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a published parameter requires SchemaId")
		})?;
		if self.schema.parameters.contains_key(parameter.name.as_str())
			|| self.schema.parameters.values().any(|current| current.id == Some(id))
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"parameter '${}' already exists",
				parameter.name
			)));
		}
		parameter.published_at = published_at;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.parameter_versions.insert(parameter.name.to_string(), published_at);
		catalog.parameters.insert(parameter.name.to_string(), Arc::new(parameter));
		Ok(())
	}

	/// Replaces one durable database parameter while preserving its identity.
	pub(crate) fn replace_parameter(
		&mut self,
		mut replacement: SchemaParameter,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = replacement.id.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a replacement parameter requires SchemaId")
		})?;
		let current = self
			.schema
			.parameter(replacement.name.as_str())
			.ok_or_else(|| parameter_missing(replacement.name.as_str()))?;
		if current.id != Some(id) {
			return Err(FerrosGrynnError::invalid_argument(
				"parameter identity and name are immutable",
			));
		}
		replacement.published_at = published_at;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.parameter_versions.insert(replacement.name.to_string(), published_at);
		catalog.parameters.insert(replacement.name.to_string(), Arc::new(replacement));
		Ok(())
	}

	/// Removes one durable database parameter by stable identity.
	pub(crate) fn remove_parameter(&mut self, id: SchemaId) -> FerrosGrynnResult<()> {
		let name = self
			.schema
			.parameters
			.iter()
			.find_map(|(name, parameter)| (parameter.id == Some(id)).then(|| name.clone()))
			.ok_or_else(|| parameter_missing(&id.to_string()))?;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.parameters.remove(&name);
		catalog.parameter_versions.remove(&name);
		Ok(())
	}

	/// Replaces one graph definition without changing physical identity or
	/// graph-local bindings.
	pub(crate) fn replace_graph_schema(
		&mut self,
		mut replacement: GraphSchema,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = replacement.id();
		let current = self
			.schema
			.graph(id)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(id.to_string()))?;
		if current.reference() != replacement.reference()
			|| current.system_indexes().take(2).ne(replacement.system_indexes().take(2))
			|| current
				.edge_endpoint_index()
				.is_some_and(|capability| replacement.edge_endpoint_index() != Some(capability))
		{
			return Err(FerrosGrynnError::invalid_argument(
				"graph identity, family, storage mode, and system indexes are immutable",
			));
		}
		if replacement.indexes.len() > 4096 {
			return Err(FerrosGrynnError::invalid_argument(
				"graph supports at most 4096 user indexes",
			));
		}
		let mut names = std::collections::BTreeSet::new();
		let mut previous = None;
		for index in replacement.indexes() {
			index.validate()?;
			index.validate_family(replacement.reference().kind())?;
			let index_id = index.id().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("published graph index has no identity")
			})?;
			if index.graph() != Some(id)
				|| previous.is_some_and(|previous| previous >= index_id)
				|| !names.insert(index.name().as_str())
			{
				return Err(FerrosGrynnError::invalid_argument(
					"graph indexes have invalid ownership, ordering, or names",
				));
			}
			let new_definition = current
				.indexes()
				.all(|old| old.id() != Some(index_id) || old.version() != index.version());
			if new_definition {
				for &(function, version) in index.dependencies() {
					if self.schema.functions.get(&function).is_none_or(|dependency| {
						dependency.version() != version
							|| !index.is_on_demand()
								&& dependency.effect() != crate::datastore::FunctionEffect::Pure
					}) {
						return Err(FerrosGrynnError::invalid_argument(
							"graph index function dependency changed before publication",
						));
					}
				}
			}
			previous = Some(index_id);
		}
		replacement.published_at = published_at;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.graphs.insert(id, Arc::new(replacement));
		catalog.graph_versions.insert(id, published_at);
		if !self.reachability_artifacts.is_empty() {
			Arc::make_mut(&mut self.reachability_artifacts).retain(|index, artifact| {
				artifact.layout.header.identity.graph != id.get()
					|| catalog.graphs.get(&id).is_some_and(|graph| {
						graph.indexes().any(|definition| {
							definition.id() == Some(*index)
								&& definition.state() == ClassIndexState::Ready
								&& definition.version().get()
									== artifact.layout.header.identity.version
								&& definition.build_revision()
									== artifact.layout.header.identity.revision
						})
					})
			});
		}
		if !self.neighbourhood_artifacts.is_empty() {
			Arc::make_mut(&mut self.neighbourhood_artifacts).retain(|index, artifact| {
				artifact.layout.header.identity.graph != id.get()
					|| catalog.graphs.get(&id).is_some_and(|graph| {
						graph.indexes().any(|definition| {
							definition.id() == Some(*index)
								&& definition.state() == ClassIndexState::Ready
								&& definition.version().get()
									== artifact.layout.header.identity.version
								&& definition.build_revision()
									== artifact.layout.header.identity.revision
						})
					})
			});
		}
		if !self.tensor_artifacts.is_empty() {
			Arc::make_mut(&mut self.tensor_artifacts).retain(|index, artifact| {
				artifact.layout.header.identity.graph != id.get()
					|| catalog.graphs.get(&id).is_some_and(|graph| {
						graph.indexes().any(|definition| {
							definition.id() == Some(*index)
								&& definition.state() == ClassIndexState::Ready
								&& definition.version().get()
									== artifact.layout.header.identity.version
								&& definition.build_revision()
									== artifact.layout.header.identity.revision
						})
					})
			});
		}
		if !self.path_artifacts.is_empty() {
			Arc::make_mut(&mut self.path_artifacts).retain(|index, artifact| {
				artifact.layout.header.identity.graph != id.get()
					|| catalog.graphs.get(&id).is_some_and(|graph| {
						graph.indexes().any(|definition| {
							definition.id() == Some(*index)
								&& definition.state() == ClassIndexState::Ready
								&& definition.version().get()
									== artifact.layout.header.identity.version
								&& definition.build_revision()
									== artifact.layout.header.identity.revision
						})
					})
			});
		}
		Ok(())
	}

	/// Replaces one published class definition without changing its identity,
	/// name, or atom kind.
	pub(crate) fn replace_class(
		&mut self,
		mut replacement: ClassSchema,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let id = replacement.id().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("a replacement class requires ClassId")
		})?;
		let current = self
			.schema
			.class_by_id(id)
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(replacement.name().as_str()))?;
		if current.name() != replacement.name() || current.kind() != replacement.kind() {
			return Err(FerrosGrynnError::invalid_argument(
				"class identity, name, and atom kind are immutable",
			));
		}

		let mut next = self.clone();
		let mut dictionary = next.dictionary.as_ref().clone();
		for field in &mut replacement.fields {
			if field.id.is_none() || field.class_id != Some(id) {
				return Err(FerrosGrynnError::invalid_argument(
					"published class fields require stable identities and the owning ClassId",
				));
			}
			field.key = Some(dictionary.intern(field.name.clone())?);
		}
		if replacement.indexes.iter().any(|index| index.id.is_none()) {
			return Err(FerrosGrynnError::invalid_argument(
				"published class indexes require stable IndexId values",
			));
		}
		for index in &mut replacement.indexes {
			if index.state() == crate::datastore::ClassIndexState::Ready
				&& index.build_revision == 0
			{
				index.build_revision = published_at;
			}
		}
		replacement.published_at = published_at;
		Arc::make_mut(&mut next.schema).classes.insert(id, Arc::new(replacement));
		Arc::make_mut(&mut next.schema).class_versions.insert(id, published_at);
		next.dictionary = Arc::new(dictionary);
		next.rebuild_derived_state()?;
		*self = next;
		Ok(())
	}

	/// Removes an empty class and all of its derived index/statistics state.
	pub(crate) fn remove_class(&mut self, id: ClassId) -> FerrosGrynnResult<()> {
		let current = self
			.schema
			.class_by_id(id)
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(id.to_string()))?;
		if self.atoms.atoms().any(|atom| atom.class_id() == id) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"class '{}' still owns atoms",
				current.name()
			)));
		}
		let mut next = self.clone();
		let catalog = Arc::make_mut(&mut next.schema);
		catalog.classes.remove(&id);
		catalog.class_names.remove(current.name().as_str());
		catalog.class_versions.remove(&id);
		next.rebuild_derived_state()?;
		*self = next;
		Ok(())
	}

	/// Removes one graph and every graph-local occurrence while preserving all
	/// database-global atoms. Owned internal descendants are removed with their
	/// public root.
	pub(crate) fn remove_graph(&mut self, graph: GraphId) -> FerrosGrynnResult<()> {
		self.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		let mut next = self.clone();
		let mut remove = next
			.schema
			.all_graphs()
			.filter(|schema| schema.owner_root() == Some(graph))
			.map(|schema| schema.id())
			.collect::<Vec<_>>();
		remove.push(graph);
		for graph in remove {
			next.invalidate_relation_artifacts(graph);
			let schema = next
				.schema
				.graph(graph)
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
			let globals = next
				.graph(graph)
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?
				.occurrences()
				.map(|occurrence| occurrence.global_id())
				.collect::<Vec<_>>();
			for global in globals {
				next.detach(graph, global)?;
			}
			next.graphs.remove(&graph);
			if !next.path_artifacts.is_empty() {
				Arc::make_mut(&mut next.path_artifacts)
					.retain(|_, artifact| artifact.layout.header.identity.graph != graph.get());
			}
			if !next.neighbourhood_artifacts.is_empty() {
				Arc::make_mut(&mut next.neighbourhood_artifacts)
					.retain(|_, artifact| artifact.layout.header.identity.graph != graph.get());
			}
			if !next.reachability_artifacts.is_empty() {
				Arc::make_mut(&mut next.reachability_artifacts)
					.retain(|_, artifact| artifact.layout.header.identity.graph != graph.get());
			}
			let catalog = Arc::make_mut(&mut next.schema);
			catalog.graphs.remove(&graph);
			catalog.graph_names.remove(schema.name().as_str());
			catalog.graph_versions.remove(&graph);
		}
		next.rebuild_derived_state()?;
		*self = next;
		Ok(())
	}
}

fn parameter_missing(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("parameter '${name}' does not exist"))
}

fn analyzer_exists(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("analyzer '{name}' already exists"))
}

fn analyzer_missing(id: AnalyzerId) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("analyzer {id} does not exist"))
}

fn function_exists(function: &SchemaFunction) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!(
		"function '{}::{}' with the same parameter types already exists",
		function.namespace(),
		function.name()
	))
}

fn function_missing(id: FunctionId) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("function {id} does not exist"))
}
