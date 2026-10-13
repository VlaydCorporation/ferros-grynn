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
		self.level_path_value_bounded(graph, global, &|_bytes, _work| Ok(()))
	}

	/// Builds the same value with allocation/work admission before every copy.
	pub(crate) fn level_path_value_bounded(
		&self,
		graph: GraphId,
		global: GlobalAtomId,
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Value> {
		admit(256, 1)?;
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
				value.insert("chains", self.containment_chains_value(graph, admit)?);
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
				value.insert("source", self.endpoint_level_path(graph, source, admit)?);
				value.insert("target", self.endpoint_level_path(graph, target, admit)?);
				Ok(Value::Object(value))
			}
		}
	}

	fn endpoint_level_path(
		&self,
		graph: GraphId,
		endpoint: &EdgeEndpoint,
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Value> {
		admit(256, 1)?;
		let (graph, occurrence) =
			self.resolve_incidence_endpoint(graph, endpoint, |_graph, _owner| Ok(()))?;
		let atom = self.atom_ref(occurrence, admit)?;
		let mut value = Object::new();
		value.insert("atom", atom);
		value.insert("chains", self.containment_chains_value(graph, admit)?);
		Ok(Value::Object(value))
	}

	fn containment_chains_value(
		&self,
		child: GraphId,
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Value> {
		let chains = self.containment_chains(child, admit)?;
		let owners = chains
			.iter()
			.try_fold(0_usize, |count, chain| count.checked_add(chain.len()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LevelPath owners"))?;
		admit(
			owners
				.checked_mul(384)
				.and_then(|bytes| bytes.checked_add(chains.len().checked_mul(128)?))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LevelPath value"))?,
			owners as u64,
		)?;
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
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Vec<ContainmentChain>> {
		#[allow(
			clippy::mutable_key_type,
			reason = "GraphRef ordering uses immutable graph identity and revision fields"
		)]
		let mut chains = BTreeSet::new();
		admit(128, 1)?;
		let mut pending = vec![(child, Vec::<ContainmentOwner>::new())];
		while let Some((child, mut chain)) = pending.pop() {
			let parents = self.containment_parents(child, admit)?;
			if parents.is_empty() {
				admit(128, chain.len() as u64)?;
				chain.reverse();
				chains.insert(chain);
				continue;
			}
			for (graph, _owner_global, owner) in parents {
				admit(0, chain.len() as u64)?;
				if chain
					.iter()
					.any(|current| current.graph.id() == graph.id() && current.owner == owner)
				{
					return Err(FerrosGrynnError::graph_constraint_violation(
						graph.name().to_string(),
						"LevelPath containment contains a cycle",
					));
				}
				let shell_bytes = chain
					.len()
					.checked_add(1)
					.and_then(|len| {
						len.checked_mul(2 * size_of::<ContainmentOwner>() + 2 * size_of::<Value>())
					})
					.and_then(|bytes| bytes.checked_add(128))
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LevelPath chain storage")
					})?;
				let bytes = chain
					.iter()
					.try_fold(shell_bytes, |bytes, current| {
						bytes.checked_add(
							current.graph.name().len() + current.owner.id().estimated_heap_bytes(),
						)
					})
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LevelPath chain"))?;
				admit(bytes, chain.len() as u64)?;
				let mut next = chain.clone();
				let parent = graph.id();
				next.push(ContainmentOwner {
					graph,
					owner,
				});
				pending.push((parent, next));
			}
		}
		admit(
			chains
				.len()
				.checked_mul(size_of::<ContainmentChain>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LevelPath result"))?,
			chains.len() as u64,
		)?;
		Ok(chains.into_iter().collect())
	}

	fn containment_parents(
		&self,
		child: GraphId,
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Vec<(GraphRef, GlobalAtomId, AtomRef)>> {
		let mut parents = Vec::new();
		for graph in self.graphs.values() {
			admit(0, 1)?;
			for occurrence in graph.occurrences() {
				admit(0, 1)?;
				if occurrence.child_graph() == Some(child) {
					admit(256 + graph.reference().name().len(), 1)?;
					parents.push((
						graph.reference().clone(),
						occurrence.global_id(),
						self.atom_ref(occurrence, admit)?,
					));
				}
			}
		}
		admit(
			0,
			(parents.len() as u64)
				.checked_mul(u64::from(usize::BITS - parents.len().leading_zeros()) + 1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LevelPath sorting"))?,
		)?;
		parents.sort_by(|left, right| (left.0.id(), &left.2).cmp(&(right.0.id(), &right.2)));
		parents.dedup_by(|left, right| left.0.id() == right.0.id() && left.2 == right.2);
		Ok(parents)
	}

	fn atom_ref(
		&self,
		occurrence: &GraphOccurrence,
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<AtomRef> {
		let atom = self.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"LevelPath occurrence references an absent base atom",
			)
		})?;
		admit(atom.record_id().estimated_heap_bytes(), 1)?;
		Ok(AtomRef::new(atom.kind(), atom.record_id().clone()))
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
