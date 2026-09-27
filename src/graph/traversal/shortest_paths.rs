use std::{cmp::Ordering, collections::BinaryHeap, sync::Arc};

use num_traits::ToPrimitive;
use rapidhash::RapidHashMap;

use super::{
	algorithm_failure,
	connectivity::ensure_node,
	index::{TraversalDirection, TraversalIndex, TraversalSemantics, TraversalStep, local_steps},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomOccurrenceRef,
		EdgeRead,
		EndpointRole,
		GraphRead,
		GraphRef,
		Path,
		PathDirection,
		PathStep,
		SnapshotId,
		general::atom_does_not_exist,
	},
	values::{Number, Uuid},
};

#[derive(Debug, Clone)]
struct Predecessor {
	from: AtomId,
	step: TraversalStep,
}

#[derive(Debug, Clone)]
pub struct ShortestPaths {
	origin: Uuid,
	snapshot: SnapshotId,
	source: AtomId,
	distances: RapidHashMap<AtomId, f64>,
	predecessors: RapidHashMap<AtomId, Predecessor>,
}

impl ShortestPaths {
	pub const fn source(&self) -> AtomId {
		self.source
	}

	pub fn distance_to(&self, target: AtomId) -> Option<f64> {
		self.distances.get(&target).copied()
	}

	pub fn path_to(&self, target: AtomId) -> Option<Vec<AtomId>> {
		self.reconstruct(target)
			.map(|path| path.into_iter().map(|(atom, _)| atom).collect::<Vec<_>>())
	}

	pub fn edge_path_to(&self, target: AtomId) -> Option<Vec<AtomId>> {
		self.reconstruct(target).map(|path| {
			path.into_iter()
				.filter_map(|(_, predecessor)| predecessor.map(|value| value.step.via_edge))
				.collect::<Vec<_>>()
		})
	}

	pub fn materialize_path<R: GraphRead>(
		&self,
		graph: &R,
		graph_ref: Arc<GraphRef>,
		target: AtomId,
	) -> FerrosGrynnResult<Option<Path>> {
		if graph.identity() != self.origin {
			return Err(FerrosGrynnError::graph_identity_mismatch(
				self.origin.to_string(),
				graph.identity().to_string(),
			));
		}
		if graph.snapshot_id() != self.snapshot {
			return Err(FerrosGrynnError::snapshot_unavailable(
				self.origin.to_string(),
				self.snapshot.to_string(),
			));
		}
		let Some(path) = self.reconstruct(target) else {
			return Ok(None);
		};
		let mut steps = Vec::with_capacity(path.len());
		for (index, (atom, predecessor)) in path.into_iter().enumerate() {
			let atom_ref = AtomOccurrenceRef::new(
				graph.atom_ref(atom)?.ok_or_else(|| atom_does_not_exist(atom))?,
				graph_ref.clone(),
			);
			if index == 0 {
				steps.push(PathStep::new(
					atom_ref,
					None,
					PathDirection::Forward,
					EndpointRole::Source,
				));
				continue;
			}
			let predecessor = predecessor.ok_or_else(|| {
				algorithm_failure("shortest path", "predecessor chain is incomplete")
			})?;
			let edge_ref = AtomOccurrenceRef::new(
				graph
					.atom_ref(predecessor.step.via_edge)?
					.ok_or_else(|| atom_does_not_exist(predecessor.step.via_edge))?,
				graph_ref.clone(),
			);
			steps.push(PathStep::new(
				atom_ref,
				Some(edge_ref),
				predecessor.step.direction,
				predecessor.step.role,
			));
		}
		Ok(Some(Path::new(
			self.origin,
			self.snapshot,
			steps,
			self.distance_to(target).map(Number::Float),
		)))
	}

	fn reconstruct(&self, target: AtomId) -> Option<Vec<(AtomId, Option<Predecessor>)>> {
		self.distances.contains_key(&target).then_some(())?;
		let mut path = Vec::new();
		let mut current = target;
		for _ in 0..=self.predecessors.len() {
			if current == self.source {
				path.push((current, None));
				path.reverse();
				return Some(path);
			}
			let predecessor = self.predecessors.get(&current)?.clone();
			path.push((current, Some(predecessor.clone())));
			current = predecessor.from;
		}
		None
	}
}

#[derive(Debug, Clone, Copy)]
struct QueueEntry {
	distance: f64,
	node: AtomId,
}

impl PartialEq for QueueEntry {
	fn eq(&self, other: &Self) -> bool {
		self.distance.total_cmp(&other.distance) == Ordering::Equal && self.node == other.node
	}
}

impl Eq for QueueEntry {}

impl PartialOrd for QueueEntry {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for QueueEntry {
	fn cmp(&self, other: &Self) -> Ordering {
		other.distance.total_cmp(&self.distance).then_with(|| other.node.cmp(&self.node))
	}
}

pub fn dijkstra<R: GraphRead>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
) -> FerrosGrynnResult<ShortestPaths> {
	ensure_node(graph, source)?;
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let weights = collect_weights(graph, false, "Dijkstra")?;
	let mut distances = RapidHashMap::default();
	let mut predecessors = RapidHashMap::default();
	let mut queue = BinaryHeap::from([QueueEntry {
		distance: 0.0,
		node: source,
	}]);
	distances.insert(source, 0.0);

	while let Some(QueueEntry {
		distance,
		node,
	}) = queue.pop()
	{
		if distances.get(&node).is_some_and(|known| distance > *known) {
			continue;
		}
		for step in local_steps(&index, node, direction) {
			let Some(target) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let Some(weight) = weights.get(&step.via_edge).copied() else {
				return Err(algorithm_failure(
					"Dijkstra",
					format!("missing weight for edge {}", step.via_edge),
				));
			};
			let candidate = distance + weight;
			if !candidate.is_finite() {
				return Err(algorithm_failure("Dijkstra", "distance overflowed f64"));
			}
			if distances.get(&target).is_none_or(|known| candidate < *known) {
				distances.insert(target, candidate);
				predecessors.insert(target, Predecessor {
					from: node,
					step: step.clone(),
				});
				queue.push(QueueEntry {
					distance: candidate,
					node: target,
				});
			}
		}
	}
	Ok(shortest_paths(graph, source, distances, predecessors))
}

pub fn bellman_ford<R: GraphRead>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
) -> FerrosGrynnResult<ShortestPaths> {
	ensure_node(graph, source)?;
	let index = TraversalIndex::new(graph, TraversalSemantics::Node)?;
	let weights = collect_weights(graph, true, "Bellman-Ford")?;
	let arcs = local_arcs(graph, &index, &weights, direction)?;
	let mut distances = RapidHashMap::default();
	let mut predecessors = RapidHashMap::default();
	distances.insert(source, 0.0);

	for _ in 0..graph.node_count()?.saturating_sub(1) {
		let mut changed = false;
		for arc in &arcs {
			let Some(distance) = distances.get(&arc.from).copied() else {
				continue;
			};
			let candidate = distance + arc.weight;
			if !candidate.is_finite() {
				return Err(algorithm_failure("Bellman-Ford", "distance overflowed f64"));
			}
			if distances.get(&arc.to).is_none_or(|known| candidate < *known) {
				distances.insert(arc.to, candidate);
				predecessors.insert(arc.to, Predecessor {
					from: arc.from,
					step: arc.step.clone(),
				});
				changed = true;
			}
		}
		if !changed {
			break;
		}
	}

	for arc in &arcs {
		let Some(distance) = distances.get(&arc.from).copied() else {
			continue;
		};
		if distances.get(&arc.to).is_none_or(|known| distance + arc.weight < *known) {
			return Err(algorithm_failure(
				"Bellman-Ford",
				"a negative-weight cycle is reachable from the source",
			));
		}
	}
	Ok(shortest_paths(graph, source, distances, predecessors))
}

#[derive(Debug, Clone)]
struct WeightedArc {
	from: AtomId,
	to: AtomId,
	weight: f64,
	step: TraversalStep,
}

pub(super) fn collect_weights<R: GraphRead>(
	graph: &R,
	allow_negative: bool,
	algorithm: &'static str,
) -> FerrosGrynnResult<RapidHashMap<AtomId, f64>> {
	let mut weights = RapidHashMap::default();
	for id in graph.edge_ids()? {
		let id = id?;
		let edge = graph.edge(id)?.ok_or_else(|| {
			algorithm_failure(algorithm, format!("edge iterator yielded missing edge {id}"))
		})?;
		let weight = match edge.weight() {
			None => 1.0,
			Some(Number::Int(value)) => value as f64,
			Some(Number::Float(value)) => value,
			Some(Number::Decimal(value)) => value.to_f64().ok_or_else(|| {
				algorithm_failure(algorithm, format!("edge {id} weight is outside f64 range"))
			})?,
		};
		if !weight.is_finite() {
			return Err(algorithm_failure(algorithm, format!("edge {id} has a non-finite weight")));
		}
		if !allow_negative && weight < 0.0 {
			return Err(algorithm_failure(
				algorithm,
				format!("edge {id} has negative weight {weight}"),
			));
		}
		weights.insert(id, weight);
	}
	Ok(weights)
}

fn local_arcs<R: GraphRead>(
	graph: &R,
	index: &TraversalIndex,
	weights: &RapidHashMap<AtomId, f64>,
	direction: TraversalDirection,
) -> FerrosGrynnResult<Vec<WeightedArc>> {
	let mut arcs = Vec::new();
	for from in graph.node_ids()? {
		let from = from?;
		for step in local_steps(index, from, direction) {
			let Some(to) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let Some(weight) = weights.get(&step.via_edge).copied() else {
				return Err(algorithm_failure(
					"shortest path",
					format!("missing weight for edge {}", step.via_edge),
				));
			};
			arcs.push(WeightedArc {
				from,
				to,
				weight,
				step: step.clone(),
			});
		}
	}
	Ok(arcs)
}

fn shortest_paths<R: GraphRead>(
	graph: &R,
	source: AtomId,
	distances: RapidHashMap<AtomId, f64>,
	predecessors: RapidHashMap<AtomId, Predecessor>,
) -> ShortestPaths {
	ShortestPaths {
		origin: graph.identity(),
		snapshot: graph.snapshot_id(),
		source,
		distances,
		predecessors,
	}
}
