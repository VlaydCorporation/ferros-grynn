use std::sync::Arc;

use crate::{
	datastore::{
		ClassId,
		ClassSchema,
		DatabaseRoot,
		FunctionId,
		GraphSchema,
		SchemaFunction,
		SchemaId,
		SchemaParameter,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

impl DatabaseRoot {
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
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.function_versions.insert(id, published_at);
		catalog.functions.insert(id, Arc::new(replacement));
		Ok(())
	}

	/// Removes one named-function overload by stable identity.
	pub(crate) fn remove_function(&mut self, id: FunctionId) -> FerrosGrynnResult<()> {
		let current =
			self.schema.functions.get(&id).cloned().ok_or_else(|| function_missing(id))?;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.functions.remove(&id);
		catalog.function_signatures.remove(&current.signature());
		catalog.function_versions.remove(&id);
		Ok(())
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
			|| current.system_indexes().ne(replacement.system_indexes())
		{
			return Err(FerrosGrynnError::invalid_argument(
				"graph identity, family, storage mode, and system indexes are immutable",
			));
		}
		replacement.published_at = published_at;
		let catalog = Arc::make_mut(&mut self.schema);
		catalog.graphs.insert(id, Arc::new(replacement));
		catalog.graph_versions.insert(id, published_at);
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
