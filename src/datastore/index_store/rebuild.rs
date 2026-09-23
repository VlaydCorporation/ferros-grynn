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
	for graph in database.schema.graphs() {
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

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
