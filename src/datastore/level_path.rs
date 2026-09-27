#![allow(
	clippy::missing_docs_in_private_items,
	reason = "private containment helpers implement the module-level canonical LevelPath contract"
)]

//! Canonical graph-local LevelPath projection.

use std::collections::BTreeSet;

use super::{DatabaseRoot, GlobalAtomId, GraphOccurrence, OccurrenceState};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomRef, EdgeEndpoint, GraphId, GraphRef},
	values::{Array, Object, Value},
};

/// Reserved graph-local overlay property managed by graph DML.
pub const LEVEL_PATH_PROPERTY: &str = "levelpath";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ContainmentOwner {
	graph: GraphRef,
	owner: AtomRef,
}

type ContainmentChain = Vec<ContainmentOwner>;

impl DatabaseRoot {
	/// Builds the canonical LevelPath object for one visible occurrence.
	pub(crate) fn level_path_value(
		&self,
		graph: GraphId,
		global: GlobalAtomId,
	) -> FerrosGrynnResult<Value> {
		let local = self.membership(graph, global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
		})?;
		let occurrence = self.occurrence(graph, local).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"reverse membership references an absent LevelPath occurrence",
			)
		})?;
		match occurrence.state() {
			OccurrenceState::Node {
				..
			} => {
				let mut value = Object::new();
				value.insert("kind", "atom");
				value.insert("chains", self.containment_chains_value(graph)?);
				Ok(Value::Object(value))
			}
			OccurrenceState::Edge {
				topology,
				..
			} => {
				if topology.is_hyper() {
					return Err(FerrosGrynnError::unsupported_level_path_target("hyperedge"));
				}
				let (source, target) = match topology {
					crate::graph::EdgeTopology::Directed {
						source,
						target,
					}
					| crate::graph::EdgeTopology::Bidirectional {
						source,
						target,
					} if source.len() == 1 && target.len() == 1 => (&source[0], &target[0]),
					crate::graph::EdgeTopology::Undirected {
						endpoints,
					} if endpoints.len() == 2 => (&endpoints[0], &endpoints[1]),
					_ => {
						return Err(FerrosGrynnError::invalid_storage_file(
							"binary LevelPath edge has invalid endpoint cardinality",
						));
					}
				};
				let mut value = Object::new();
				value.insert("kind", "binary_edge");
				value.insert("source", self.endpoint_level_path(graph, source)?);
				value.insert("target", self.endpoint_level_path(graph, target)?);
				Ok(Value::Object(value))
			}
		}
	}

	fn endpoint_level_path(
		&self,
		graph: GraphId,
		endpoint: &EdgeEndpoint,
	) -> FerrosGrynnResult<Value> {
		let (graph, occurrence) = self.resolve_level_path_endpoint(graph, endpoint)?;
		let atom = self.atom_ref(occurrence)?;
		let mut value = Object::new();
		value.insert("atom", atom);
		value.insert("chains", self.containment_chains_value(graph)?);
		Ok(Value::Object(value))
	}

	fn resolve_level_path_endpoint(
		&self,
		graph: GraphId,
		endpoint: &EdgeEndpoint,
	) -> FerrosGrynnResult<(GraphId, &GraphOccurrence)> {
		match endpoint {
			EdgeEndpoint::Local(local) => self
				.occurrence(graph, *local)
				.map(|occurrence| (graph, occurrence))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"LevelPath edge references an absent local endpoint",
					)
				}),
			EdgeEndpoint::Nested(qualified) => {
				let mut current = graph;
				for owner in qualified.scope() {
					let occurrence = self.occurrence(current, *owner).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"LevelPath endpoint scope references an absent owner",
						)
					})?;
					current = occurrence_child(occurrence).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"LevelPath endpoint scope contains a non-meta owner",
						)
					})?;
				}
				self.occurrence(current, qualified.atom())
					.map(|occurrence| (current, occurrence))
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"LevelPath endpoint references an absent nested atom",
						)
					})
			}
		}
	}

	fn containment_chains_value(&self, child: GraphId) -> FerrosGrynnResult<Value> {
		let chains = self.containment_chains(child, &mut BTreeSet::new())?;
		Ok(Value::Array(Array::new(
			chains
				.into_iter()
				.map(|chain| {
					Value::Array(Array::new(
						chain
							.into_iter()
							.map(|owner| {
								let mut value = Object::new();
								value.insert("graph", owner.graph);
								value.insert("owner", owner.owner);
								Value::Object(value)
							})
							.collect(),
					))
				})
				.collect(),
		)))
	}

	fn containment_chains(
		&self,
		child: GraphId,
		visiting: &mut BTreeSet<(GraphId, GlobalAtomId)>,
	) -> FerrosGrynnResult<Vec<ContainmentChain>> {
		let parents = self.containment_parents(child)?;
		if parents.is_empty() {
			return Ok(vec![Vec::new()]);
		}
		#[allow(
			clippy::mutable_key_type,
			reason = "GraphRef ordering uses immutable graph identity and revision fields"
		)]
		let mut chains = BTreeSet::new();
		for (graph, owner_global, owner) in parents {
			let key = (graph.id(), owner_global);
			if !visiting.insert(key) {
				return Err(FerrosGrynnError::graph_constraint_violation(
					graph.name().to_string(),
					"LevelPath containment contains a cycle",
				));
			}
			for mut chain in self.containment_chains(graph.id(), visiting)? {
				chain.push(ContainmentOwner {
					graph: graph.clone(),
					owner: owner.clone(),
				});
				chains.insert(chain);
			}
			visiting.remove(&key);
		}
		Ok(chains.into_iter().collect())
	}

	fn containment_parents(
		&self,
		child: GraphId,
	) -> FerrosGrynnResult<Vec<(GraphRef, GlobalAtomId, AtomRef)>> {
		let mut parents = Vec::new();
		for graph in self.graphs.values() {
			for occurrence in graph.occurrences() {
				if occurrence_child(occurrence) == Some(child) {
					parents.push((
						graph.reference().clone(),
						occurrence.global_id(),
						self.atom_ref(occurrence)?,
					));
				}
			}
		}
		parents.sort_by(|left, right| (left.0.id(), &left.2).cmp(&(right.0.id(), &right.2)));
		parents.dedup_by(|left, right| left.0.id() == right.0.id() && left.2 == right.2);
		Ok(parents)
	}

	fn atom_ref(&self, occurrence: &GraphOccurrence) -> FerrosGrynnResult<AtomRef> {
		let atom = self.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"LevelPath occurrence references an absent base atom",
			)
		})?;
		Ok(AtomRef::new(atom.kind(), atom.record_id().clone()))
	}
}

fn occurrence_child(occurrence: &GraphOccurrence) -> Option<GraphId> {
	match occurrence.state() {
		OccurrenceState::Node {
			nested_graph,
		} => *nested_graph,
		OccurrenceState::Edge {
			transition_graph,
			..
		} => *transition_graph,
	}
}

#[cfg(test)]
mod tests {
	use super::LEVEL_PATH_PROPERTY;

	#[test]
	fn reserves_the_canonical_property_name() {
		assert_eq!(LEVEL_PATH_PROPERTY, "levelpath");
	}
}
