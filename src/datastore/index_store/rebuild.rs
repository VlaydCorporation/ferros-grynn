//! Deterministic reconstruction of derived indexes after snapshot reopen.

use super::{IndexStoreRoot, index_context};
use crate::{
	datastore::DatabaseRoot,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

/// Rebuilds every engine-owned and explicit index from normalized source
/// data. Durable index pages are validated separately; this reconstruction is
/// also the recovery fallback for derived index state.
pub(crate) fn rebuild_index_store(database: &DatabaseRoot) -> FerrosGrynnResult<IndexStoreRoot> {
	let mut indexes = IndexStoreRoot::default();
	for graph in database.schema.all_graphs() {
		indexes.register_graph(graph.id())?;
	}
	for class in database.schema.classes() {
		indexes.register_class(class)?;
	}

	let mut context = index_context(&database.schema, &database.dictionary)?;
	for atom in database.atoms.atoms() {
		let class = database
			.schema
			.class_by_id(atom.class_id())
			.ok_or_else(|| invalid("base atom references an absent class"))?;
		let prepared = indexes.prepare_base_insert(&class, atom.properties(), &mut context)?;
		indexes.apply_base_insert(prepared, atom.id())?;
	}
	for (graph_id, graph) in &database.graphs {
		for occurrence in graph.occurrences() {
			let atom = database
				.atom(occurrence.global_id())
				.ok_or_else(|| invalid("graph occurrence references an absent base atom"))?;
			let prepared = indexes.prepare_restored_occurrence_insert(
				*graph_id,
				atom,
				occurrence,
				&mut context,
			)?;
			indexes.apply_occurrence_insert(*graph_id, occurrence, prepared)?;
		}
	}
	Ok(indexes)
}

/// Rebuilds only engine-owned class and label lookups while retaining
/// schema-defined indexes as lazy immutable side-file bindings.
pub(crate) fn rebuild_lookup_index_store(
	database: &DatabaseRoot,
) -> FerrosGrynnResult<IndexStoreRoot> {
	let mut indexes = IndexStoreRoot::default();
	for graph in database.schema.all_graphs() {
		indexes.register_graph(graph.id())?;
	}
	for class in database.schema.classes() {
		indexes.register_class_deferred(class)?;
	}
	for atom in database.atoms.atoms() {
		if database.schema.class_by_id(atom.class_id()).is_none() {
			return Err(invalid("base atom references an absent class"));
		}
		indexes.classes.entry(atom.class_id()).or_default().insert(atom.id());
	}
	for (graph_id, graph) in &database.graphs {
		let graph_indexes = indexes
			.graphs
			.get_mut(graph_id)
			.ok_or_else(|| invalid("graph occurrences have no registered index root"))?;
		for occurrence in graph.occurrences() {
			let atom = database
				.atom(occurrence.global_id())
				.ok_or_else(|| invalid("graph occurrence references an absent base atom"))?;
			graph_indexes.classes.entry(atom.class_id()).or_default().insert(occurrence.local_id());
			if let Some(label) = occurrence.label() {
				let kind = occurrence
					.local_id()
					.kind()
					.ok_or_else(|| invalid("live occurrence kind is null"))?;
				graph_indexes
					.labels
					.entry((kind, label.clone()))
					.or_default()
					.insert(occurrence.local_id());
			}
		}
	}
	Ok(indexes)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
