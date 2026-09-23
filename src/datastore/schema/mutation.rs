use std::sync::Arc;

use crate::{
	datastore::{ClassId, ClassSchema, DatabaseRoot},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

impl DatabaseRoot {
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
	/// database-global atoms.
	pub(crate) fn remove_graph(&mut self, graph: GraphId) -> FerrosGrynnResult<()> {
		let schema = self
			.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?;
		let mut next = self.clone();
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
		next.rebuild_derived_state()?;
		*self = next;
		Ok(())
	}
}
