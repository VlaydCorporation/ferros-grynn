use std::{cmp::Ordering, collections::BinaryHeap, sync::Arc};

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
	math::{add_numbers, number_is_finite},
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
	distances: RapidHashMap<AtomId, Number>,
	predecessors: RapidHashMap<AtomId, Predecessor>,
}

impl ShortestPaths {
	pub const fn source(&self) -> AtomId {
		self.source
	}

	pub fn distance_to(&self, target: AtomId) -> Option<Number> {
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
		self.materialize_path_with_admission(graph, graph_ref, target, &mut |_bytes| Ok(()))
	}

	/// Admits reconstruction buffers and public identifiers before retaining
	/// them.
	///
	/// # Errors
	/// Propagates admission, identity, snapshot and atom resolution errors.
	pub(crate) fn materialize_path_with_admission<R: GraphRead>(
		&self,
		graph: &R,
		graph_ref: Arc<GraphRef>,
		target: AtomId,
		admit: &mut impl FnMut(usize) -> FerrosGrynnResult<()>,
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
		if !self.distances.contains_key(&target) {
			return Ok(None);
		}
		admit(
			self.predecessors
				.len()
				.checked_add(1)
				.and_then(|count| {
					count.checked_mul(
						3 * (size_of::<(AtomId, Option<Predecessor>)>() + size_of::<PathStep>()),
					)
				})
				.ok_or_else(|| FerrosGrynnError::operation_overflow("path reconstruction bytes"))?,
		)?;
		let Some(path) = self.reconstruct(target) else {
			return Ok(None);
		};
		let mut steps = Vec::with_capacity(path.len());
		for (index, (atom, predecessor)) in path.into_iter().enumerate() {
			let atom = graph.atom_ref(atom)?.ok_or_else(|| atom_does_not_exist(atom))?;
			admit(atom.id().estimated_heap_bytes())?;
			let atom_ref = AtomOccurrenceRef::new(atom, graph_ref.clone());
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
			let edge = graph
				.atom_ref(predecessor.step.via_edge)?
				.ok_or_else(|| atom_does_not_exist(predecessor.step.via_edge))?;
			admit(edge.id().estimated_heap_bytes())?;
			let edge_ref = AtomOccurrenceRef::new(edge, graph_ref.clone());
			steps.push(PathStep::new(
				atom_ref,
				Some(edge_ref),
				predecessor.step.direction,
				predecessor.step.role,
			));
		}
		Ok(Some(Path::new(self.origin, self.snapshot, steps, self.distance_to(target))))
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
	distance: Number,
	node: AtomId,
}

impl PartialEq for QueueEntry {
	fn eq(&self, other: &Self) -> bool {
		self.distance.cmp(&other.distance) == Ordering::Equal && self.node == other.node
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
		other.distance.cmp(&self.distance).then_with(|| other.node.cmp(&self.node))
	}
}

pub fn dijkstra<R: GraphRead>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
) -> FerrosGrynnResult<ShortestPaths> {
	dijkstra_with_check(graph, source, direction, &mut || Ok(()))
}

fn dijkstra_with_check<R: GraphRead, Check>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
	check: &mut Check,
) -> FerrosGrynnResult<ShortestPaths>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	check()?;
	ensure_node(graph, source)?;
	let index = TraversalIndex::new_with_check(graph, TraversalSemantics::Node, check)?;
	let weights = collect_weights_with_check(graph, false, "Dijkstra", check)?;
	dijkstra_search(
		graph,
		source,
		direction,
		&index,
		&weights,
		None,
		(&mut |_bytes| Ok(()), &mut |_node, _prefix, _upper| Ok(true), check, &mut || Ok(())),
	)
}

/// Runs the shared forward NODE search with bounded buffers and optional ALT
/// refinement. Queue order and strict predecessor improvement stay unchanged.
/// The returned flag proves that pruning cannot hide arithmetic failures.
///
/// # Errors
/// Validates all weights before refinement; propagates admission, cancellation,
/// branch budget, read and arithmetic errors.
pub fn dijkstra_to_with_control<R: GraphRead, Admit, Refine, Check, Visit>(
	graph: &R,
	source: AtomId,
	target: AtomId,
	admit: &mut Admit,
	refine: &mut Refine,
	check: &mut Check,
	visit: &mut Visit,
) -> FerrosGrynnResult<(ShortestPaths, bool)>
where
	Admit: FnMut(usize) -> FerrosGrynnResult<()>,
	Refine: FnMut(AtomId, Number, Option<Number>) -> FerrosGrynnResult<bool>,
	Check: FnMut() -> FerrosGrynnResult<()>,
	Visit: FnMut() -> FerrosGrynnResult<()>,
{
	check()?;
	ensure_node(graph, source)?;
	let index = TraversalIndex::new_with_admission(graph, TraversalSemantics::Node, admit, check)?;
	admit(
		graph
			.edge_count()?
			.checked_mul(4 * size_of::<(AtomId, Number)>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("Dijkstra weights bytes"))?,
	)?;
	let weights = collect_weights_with_check(graph, false, "Dijkstra", check)?;
	let pruning_safe = pruning_preserves_arithmetic(&weights, graph.node_count()?, check)?;
	let paths = dijkstra_search(
		graph,
		source,
		TraversalDirection::Forward,
		&index,
		&weights,
		Some(target),
		(
			admit,
			&mut |node, prefix, upper| {
				if pruning_safe {
					refine(node, prefix, upper)
				} else {
					Ok(true)
				}
			},
			check,
			visit,
		),
	)?;
	Ok((paths, pruning_safe))
}

/// Keeps full traversal when a mixed cast can decrease a prefix or when any
/// reachable addition could fail. Otherwise every simple prefix plus one arc
/// is monotone and representable, so a strict completion bound is safe.
fn pruning_preserves_arithmetic<Check>(
	weights: &RapidHashMap<AtomId, Number>,
	vertices: usize,
	check: &mut Check,
) -> FerrosGrynnResult<bool>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let mut maximum = Number::Int(0);
	let mut maximum_integer = 0_i64;
	let mut floating = false;
	let mut decimal = false;
	for (ordinal, &weight) in weights.values().enumerate() {
		check_candidate(check, ordinal)?;
		maximum = maximum.max(weight);
		match weight {
			Number::Int(weight) => maximum_integer = maximum_integer.max(weight),
			Number::Float(_) => floating = true,
			Number::Decimal(_) => decimal = true,
		}
	}
	let Ok(vertices_i64) = i64::try_from(vertices.max(1)) else {
		return Ok(false);
	};
	if !floating {
		// Decimal sums below the i64 ceiling are far inside their 96-bit
		// range. Pure integer prefixes also cannot overflow.
		return Ok(maximum <= Number::Int(i64::MAX / vertices_i64));
	}
	if decimal || maximum_integer > (1_i64 << 53) / vertices_i64 {
		return Ok(false);
	}
	// Integer-only prefixes cast exactly before the first float. Outward
	// interval addition bounds every possible floating reduction, including
	// subnormals, before allowing arithmetic work to be skipped.
	let maximum = super::landmark::DistanceInterval::from_number(maximum)?;
	let mut total = super::landmark::DistanceInterval::ZERO;
	for ordinal in 0..vertices {
		check_candidate(check, ordinal)?;
		total = total.add(maximum);
		if !total.upper.is_finite() {
			return Ok(false);
		}
	}
	Ok(true)
}

/// One loop for public all-target Dijkstra and query point refinement.
fn dijkstra_search<R: GraphRead, Admit, Refine, Check, Visit>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
	index: &TraversalIndex,
	weights: &RapidHashMap<AtomId, Number>,
	target: Option<AtomId>,
	callbacks: (&mut Admit, &mut Refine, &mut Check, &mut Visit),
) -> FerrosGrynnResult<ShortestPaths>
where
	Admit: FnMut(usize) -> FerrosGrynnResult<()>,
	Refine: FnMut(AtomId, Number, Option<Number>) -> FerrosGrynnResult<bool>,
	Check: FnMut() -> FerrosGrynnResult<()>,
	Visit: FnMut() -> FerrosGrynnResult<()>,
{
	let (admit, refine, check, visit) = callbacks;
	let state_bytes = 4
		* (size_of::<(AtomId, Predecessor)>()
			+ size_of::<(AtomId, Number)>()
			+ size_of::<QueueEntry>())
		+ 128;
	admit(state_bytes)?;
	let mut distances = RapidHashMap::default();
	let mut predecessors = RapidHashMap::default();
	let mut queue = BinaryHeap::from([QueueEntry {
		distance: Number::Int(0),
		node: source,
	}]);
	distances.insert(source, Number::Int(0));

	while let Some(QueueEntry {
		distance,
		node,
	}) = queue.pop()
	{
		check()?;
		if distances.get(&node).is_some_and(|known| distance > *known) {
			continue;
		}
		if !refine(node, distance, target.and_then(|target| distances.get(&target).copied()))? {
			continue;
		}
		for (candidate_index, step) in local_steps(index, node, direction).enumerate() {
			check_candidate(check, candidate_index)?;
			visit()?;
			let Some(target) = step.target.local_id().filter(|id| id.is_node()) else {
				continue;
			};
			let Some(weight) = weights.get(&step.via_edge).copied() else {
				return Err(algorithm_failure(
					"Dijkstra",
					format!("missing weight for edge {}", step.via_edge),
				));
			};
			let candidate = add_numbers(distance, weight)?;
			if distances.get(&target).is_none_or(|known| candidate < *known) {
				admit(state_bytes)?;
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
	bellman_ford_with_check(graph, source, direction, &mut || Ok(()))
}

pub(crate) fn bellman_ford_with_check<R: GraphRead, Check>(
	graph: &R,
	source: AtomId,
	direction: TraversalDirection,
	check: &mut Check,
) -> FerrosGrynnResult<ShortestPaths>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	check()?;
	ensure_node(graph, source)?;
	let index = TraversalIndex::new_with_check(graph, TraversalSemantics::Node, check)?;
	let weights = collect_weights_with_check(graph, true, "Bellman-Ford", check)?;
	let arcs = local_arcs(graph, &index, &weights, direction, check)?;
	let mut distances = RapidHashMap::default();
	let mut predecessors = RapidHashMap::default();
	distances.insert(source, Number::Int(0));

	for _ in 0..graph.node_count()?.saturating_sub(1) {
		check()?;
		let mut changed = false;
		for (candidate, arc) in arcs.iter().enumerate() {
			check_candidate(check, candidate)?;
			let Some(distance) = distances.get(&arc.from).copied() else {
				continue;
			};
			let candidate = add_numbers(distance, arc.weight)?;
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

	for (candidate, arc) in arcs.iter().enumerate() {
		check_candidate(check, candidate)?;
		let Some(distance) = distances.get(&arc.from).copied() else {
			continue;
		};
		let candidate = add_numbers(distance, arc.weight)?;
		if distances.get(&arc.to).is_none_or(|known| candidate < *known) {
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
	weight: Number,
	step: TraversalStep,
}

pub(super) fn collect_weights_with_check<R: GraphRead, Check>(
	graph: &R,
	allow_negative: bool,
	algorithm: &'static str,
	check: &mut Check,
) -> FerrosGrynnResult<RapidHashMap<AtomId, Number>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let mut weights = RapidHashMap::default();
	for (candidate, id) in graph.edge_ids()?.enumerate() {
		check_candidate(check, candidate)?;
		let id = id?;
		let edge = graph.edge(id)?.ok_or_else(|| {
			algorithm_failure(algorithm, format!("edge iterator yielded missing edge {id}"))
		})?;
		let weight = edge.weight().unwrap_or(Number::Int(1));
		if !number_is_finite(weight) {
			return Err(algorithm_failure(algorithm, format!("edge {id} has a non-finite weight")));
		}
		if !allow_negative && weight < Number::Int(0) {
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
	weights: &RapidHashMap<AtomId, Number>,
	direction: TraversalDirection,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<Vec<WeightedArc>> {
	let mut arcs = Vec::new();
	for (node_index, from) in graph.node_ids()?.enumerate() {
		check_candidate(check, node_index)?;
		let from = from?;
		for (candidate, step) in local_steps(index, from, direction).enumerate() {
			check_candidate(check, candidate)?;
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

fn check_candidate<Check>(check: &mut Check, candidate: usize) -> FerrosGrynnResult<()>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	if candidate & 1023 == 0 {
		check()?;
	}
	Ok(())
}

fn shortest_paths<R: GraphRead>(
	graph: &R,
	source: AtomId,
	distances: RapidHashMap<AtomId, Number>,
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
