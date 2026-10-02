//! Snapshot-root adapter for the shared local incidence relation.

use super::{DatabaseRoot, GraphIndex, OccurrenceState};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::ProjectedRelation,
};

/// Applies the definition's exact connector labels before building its single
/// local projection. Qualified endpoints remain marked for scoped fallback.
///
/// # Errors
/// Returns an ownership, resource, cancellation or incidence validation error.
pub(super) fn build_graph_index_relation(
	database: &DatabaseRoot,
	index: &GraphIndex,
	scratch_bytes: usize,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<ProjectedRelation> {
	let graph = index.graph().and_then(|graph| database.graph(graph)).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("graph index relation has no owning graph")
	})?;
	let topologies = graph
		.occurrences()
		.filter(|occurrence| {
			index.edge_labels().len() == 0
				|| index.edge_labels().any(|label| occurrence.label() == Some(label))
		})
		.filter_map(|occurrence| {
			if let OccurrenceState::Edge {
				topology,
				..
			} = occurrence.state()
			{
				Some((occurrence.local_id(), topology))
			} else {
				None
			}
		});
	ProjectedRelation::build(
		graph.occurrences().map(|occurrence| occurrence.local_id()),
		topologies,
		index.mode(),
		index.direction(),
		scratch_bytes,
		|atom| {
			graph.occurrence(atom).map(|occurrence| occurrence.global_id().raw()).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("projected vertex has no global identity")
			})
		},
		check,
	)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, GraphIndexDefinition, ReachabilityAlgorithm},
		graph::{AtomKind, EdgeTopology, GraphKind, TraversalDirection, TraversalSemantics},
		values::{PropertyMap, RecordId},
	};

	#[test]
	fn snapshot_relation_uses_definition_labels_direction_mode_and_global_order() {
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		tx.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		tx.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
		let graph = tx.define_graph("network", GraphKind::Graph).unwrap();
		let records = ["person:b", "person:a", "relation:base", "relation:connector"]
			.map(|record| record.parse::<RecordId>().unwrap());
		for record in &records {
			tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
		}
		let a = tx.attach_node(graph.id(), &records[1], None, None).unwrap();
		let b = tx.attach_node(graph.id(), &records[0], None, None).unwrap();
		let base = tx
			.attach_edge(
				graph.id(),
				&records[2],
				Some("drop".into()),
				EdgeTopology::directed([a], [b]),
				None,
				None,
			)
			.unwrap();
		let connector = tx
			.attach_edge(
				graph.id(),
				&records[3],
				Some("keep".into()),
				EdgeTopology::directed([base], [b]),
				None,
				None,
			)
			.unwrap();
		let index = GraphIndex::new(
			"relation",
			GraphIndexDefinition::Reachability {
				algorithm: ReachabilityAlgorithm::OnDemand,
			},
			None,
		)
		.unwrap()
		.with_mode(TraversalSemantics::Atom)
		.with_direction(TraversalDirection::Backward)
		.with_edge_labels(["keep"])
		.unwrap();
		tx.define_graph_index("network", index).unwrap();
		tx.commit().unwrap();
		let snapshot = store.inner.current_root();
		let schema = snapshot.schema.graph(graph.id()).unwrap();
		let index = schema.indexes().next().unwrap();
		let relation =
			build_graph_index_relation(&snapshot, index, 16_384, &mut || Ok(())).unwrap();
		assert_eq!(relation.vertices, [b, a, base, connector]);
		assert_eq!(
			relation.forward.neighbours(relation.ordinal_by_atom[&b]).collect::<Vec<_>>(),
			[relation.ordinal_by_atom[&base]]
		);
		assert!(relation.forward.neighbours(relation.ordinal_by_atom[&a]).next().is_none());
		let condensation = relation.condensation(16_384, &mut || Ok(())).unwrap();
		assert_eq!(condensation.representatives, [b, a, base, connector]);
	}
}
