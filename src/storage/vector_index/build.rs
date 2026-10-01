//! Deterministic HNSW and DiskANN graph construction.

use std::{
	borrow::Borrow,
	cmp::{Ordering, Reverse},
	collections::{BTreeMap, BTreeSet, BinaryHeap},
	mem::size_of,
};

use diskann::{
	graph::config::{PruneKind, defaults::GRAPH_SLACK_FACTOR},
	neighbor::{Neighbor, NeighborPriorityQueue},
};
use diskann_vector::{
	PureDistanceFunction,
	distance::{Cosine, FullL2, InnerProduct, Metric},
};
use rapidhash::v3::rapidhash_v3;

use super::{
	codec::{
		BuiltVectorIndex,
		VectorAlgorithm,
		VectorBuildSpec,
		VectorDocument,
		VectorNode,
		deterministic_level,
		encode_vector_index,
		maximum_degree,
		validate_vector,
	},
	hnsw_memory::hnsw_build_memory_bytes,
};
use crate::{
	datastore::{VectorDistance, exact_vector_distance},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		Vector,
		storage::{StorageValue, codec::CanonicalCodec},
	},
};

const MAX_HASH_COLLISION_CANDIDATES: usize = 4096;

struct Group<V> {
	vector_hash: u64,
	vector: V,
	postings: Vec<crate::values::storage::DiskAtomRef>,
}

#[derive(Clone, Copy)]
struct BuildCandidate {
	distance: f64,
	ordinal: u32,
}

impl PartialEq for BuildCandidate {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for BuildCandidate {}

impl PartialOrd for BuildCandidate {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for BuildCandidate {
	fn cmp(&self, other: &Self) -> Ordering {
		self.distance.total_cmp(&other.distance).then_with(|| self.ordinal.cmp(&other.ordinal))
	}
}

/// Builds one deterministic immutable HNSW or DiskANN generation.
pub(in crate::storage) fn build_vector_index<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	documents: Vec<VectorDocument<V>>,
) -> FerrosGrynnResult<BuiltVectorIndex> {
	build_with_hasher(spec, documents, rapidhash_v3)
}

/// Builds one vector generation with a concrete durable custom-distance
/// callback owned by the datastore composition boundary.
pub(in crate::storage) fn build_vector_index_with_distance<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	documents: Vec<VectorDocument<V>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<BuiltVectorIndex> {
	build_with_hasher_and_distance(spec, documents, rapidhash_v3, distance_function)
}

/// Builds deterministic graph nodes for a caller-owned streaming encoder.
pub(in crate::storage) fn build_vector_graph_with_distance<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	documents: Vec<VectorDocument<V>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<(Option<u32>, Vec<VectorNode<V>>)> {
	build_graph_with_hasher_and_distance(spec, documents, rapidhash_v3, distance_function)
}

fn build_with_hasher<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	documents: Vec<VectorDocument<V>>,
	hash: impl Fn(&[u8]) -> u64,
) -> FerrosGrynnResult<BuiltVectorIndex> {
	let mut unavailable = |function, _version, _left: &Vector, _right: &Vector| {
		Err(FerrosGrynnError::invalid_storage_file(format!(
			"custom vector distance function {function} requires the datastore evaluator"
		)))
	};
	build_with_hasher_and_distance(spec, documents, hash, &mut unavailable)
}

fn build_with_hasher_and_distance<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	documents: Vec<VectorDocument<V>>,
	hash: impl Fn(&[u8]) -> u64,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<BuiltVectorIndex> {
	let (entrypoint, nodes) =
		build_graph_with_hasher_and_distance(spec, documents, hash, distance_function)?;
	encode_vector_index(spec, entrypoint, &nodes)
}

fn build_graph_with_hasher_and_distance<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	mut documents: Vec<VectorDocument<V>>,
	hash: impl Fn(&[u8]) -> u64,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<(Option<u32>, Vec<VectorNode<V>>)> {
	if spec.algorithm()? == VectorAlgorithm::Hnsw {
		enforce_hnsw_build_budget(spec, &documents, hnsw_build_memory_bytes()?)?;
	}
	documents.sort_unstable_by_key(|document| document.atom);
	if documents.windows(2).any(|pair| pair[0].atom == pair[1].atom) {
		return Err(invalid("vector input contains a duplicate document address"));
	}
	let groups = group_documents(spec, documents, hash)?;
	let mut nodes = groups
		.into_iter()
		.map(|group| {
			let levels = match spec.algorithm()? {
				VectorAlgorithm::Hnsw => deterministic_level(spec, group.postings[0])? + 1,
				VectorAlgorithm::DiskAnn => 1,
			};
			Ok(VectorNode {
				postings: group.postings,
				vector: group.vector,
				vector_hash: group.vector_hash,
				neighbors: vec![Vec::new(); levels],
			})
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let entrypoint = build_adjacency(spec, &mut nodes, distance_function)?;
	Ok((entrypoint, nodes))
}

fn enforce_hnsw_build_budget<V>(
	spec: VectorBuildSpec,
	documents: &[VectorDocument<V>],
	budget: usize,
) -> FerrosGrynnResult<()> {
	let estimated = estimated_hnsw_build_bytes(spec, documents)?;
	if estimated > budget {
		return Err(FerrosGrynnError::invalid_argument(format!(
			"HNSW build needs an estimated {estimated} bytes, exceeding FG_HNSW_BUILD_MEM={budget}",
		)));
	}
	Ok(())
}

fn estimated_hnsw_build_bytes<V>(
	spec: VectorBuildSpec,
	documents: &[VectorDocument<V>],
) -> FerrosGrynnResult<usize> {
	let crate::datastore::ClassIndexOptions::Hnsw(options) = spec.options else {
		return Err(invalid("HNSW build budget received another index kind"));
	};
	let count = documents.len();
	let element_width = match spec.element()? {
		crate::values::VectorKind::F64 | crate::values::VectorKind::I64 => 8,
		crate::values::VectorKind::F32 | crate::values::VectorKind::I32 => 4,
		crate::values::VectorKind::F16 | crate::values::VectorKind::I16 => 2,
		crate::values::VectorKind::I8 | crate::values::VectorKind::U8 => 1,
	};
	let vector_bytes = usize::from(spec.dimension()?.get())
		.checked_mul(element_width)
		.ok_or_else(|| invalid("HNSW vector payload estimate exceeds usize"))?;
	let mut adjacency_slots = 0_usize;
	let mut level_count = 0_usize;
	for document in documents {
		let levels = deterministic_level(spec, document.atom)?
			.checked_add(1)
			.ok_or_else(|| invalid("HNSW level estimate exceeds usize"))?;
		level_count = checked_add(level_count, levels, "HNSW level estimate")?;
		let upper = levels.saturating_sub(1);
		adjacency_slots = checked_add(
			adjacency_slots,
			usize::from(options.m0().get()),
			"HNSW adjacency estimate",
		)?;
		adjacency_slots = checked_add(
			adjacency_slots,
			checked_mul(upper, usize::from(options.m().get()), "HNSW upper adjacency estimate")?,
			"HNSW adjacency estimate",
		)?;
	}
	let fixed = checked_mul(
		count,
		size_of::<VectorDocument<V>>()
			.saturating_add(size_of::<Group<V>>())
			.saturating_add(size_of::<VectorNode<V>>())
			.saturating_add(size_of::<crate::values::storage::DiskAtomRef>()),
		"HNSW fixed build estimate",
	)?;
	let canonical_grouping = checked_mul(
		count,
		vector_bytes
			.saturating_add(3)
			.saturating_add(size_of::<(Vec<u8>, usize)>())
			.saturating_add(64),
		"HNSW grouping estimate",
	)?;
	let topology = checked_add(
		checked_mul(level_count, size_of::<Vec<u32>>(), "HNSW level headers")?,
		checked_mul(adjacency_slots, size_of::<u32>(), "HNSW adjacency bytes")?,
		"HNSW topology estimate",
	)?;
	let encoded = checked_add(
		checked_mul(count, vector_bytes.saturating_add(35), "HNSW encoded records")?,
		checked_add(
			checked_mul(count, 8, "HNSW encoded postings")?,
			checked_mul(level_count.saturating_add(adjacency_slots), 4, "HNSW encoded topology")?,
			"HNSW encoded topology",
		)?,
		"HNSW encoded estimate",
	)?;
	let work = checked_add(
		checked_mul(count, 64, "HNSW visited-set estimate")?,
		checked_mul(
			usize::from(options.ef_construction().get()),
			size_of::<BuildCandidate>().saturating_mul(2).saturating_add(64),
			"HNSW candidate estimate",
		)?,
		"HNSW working-set estimate",
	)?;
	[
		fixed,
		canonical_grouping,
		topology,
		checked_mul(count, vector_bytes, "HNSW retained vector estimate")?,
		checked_mul(encoded, 2, "HNSW encoder peak estimate")?,
		work,
	]
	.into_iter()
	.try_fold(0_usize, |total, bytes| checked_add(total, bytes, "HNSW build estimate"))
}

fn checked_add(left: usize, right: usize, context: &str) -> FerrosGrynnResult<usize> {
	left.checked_add(right).ok_or_else(|| invalid(format!("{context} exceeds usize")))
}

fn checked_mul(left: usize, right: usize, context: &str) -> FerrosGrynnResult<usize> {
	left.checked_mul(right).ok_or_else(|| invalid(format!("{context} exceeds usize")))
}

fn group_documents<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	documents: Vec<VectorDocument<V>>,
	hash: impl Fn(&[u8]) -> u64,
) -> FerrosGrynnResult<Vec<Group<V>>> {
	let hashed = spec.hashed_vector();
	let mut groups = Vec::<Group<V>>::new();
	let mut exact_vectors = BTreeMap::<Vec<u8>, usize>::new();
	let mut collision_ranges = BTreeMap::<u64, Vec<(Vec<u8>, usize)>>::new();
	for document in documents {
		validate_vector(spec, document.vector.borrow())?;
		let bytes = CanonicalCodec::encode(
			StorageValue::Vector(document.vector.borrow().clone()).as_ref(),
		)?;
		if !hashed {
			if let Some(group) = exact_vectors.get(&bytes).copied() {
				groups[group].postings.push(document.atom);
				continue;
			}
			let ordinal = groups.len();
			let vector_hash = rapidhash_v3(&bytes);
			exact_vectors.insert(bytes, ordinal);
			groups.push(Group {
				vector_hash,
				vector: document.vector,
				postings: vec![document.atom],
			});
			continue;
		}
		let key = hash(&bytes);
		let candidates = collision_ranges.entry(key).or_default();
		if let Some(group) = candidates
			.iter()
			.find_map(|(candidate_bytes, group)| (candidate_bytes == &bytes).then_some(*group))
		{
			groups[group].postings.push(document.atom);
			continue;
		}
		if candidates.len() >= MAX_HASH_COLLISION_CANDIDATES {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"HASHED_VECTOR collision candidates",
				MAX_HASH_COLLISION_CANDIDATES as u64,
				(candidates.len() + 1) as u64,
			));
		}
		let ordinal = groups.len();
		groups.push(Group {
			vector_hash: rapidhash_v3(&bytes),
			vector: document.vector,
			postings: vec![document.atom],
		});
		candidates.push((bytes, ordinal));
	}
	groups.sort_unstable_by_key(|group| group.postings[0]);
	Ok(groups)
}

fn build_adjacency<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &mut [VectorNode<V>],
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Option<u32>> {
	if nodes.is_empty() {
		return Ok(None);
	}
	u32::try_from(nodes.len()).map_err(|_error| invalid("vector node count exceeds u32"))?;
	match spec.algorithm()? {
		VectorAlgorithm::Hnsw => build_hnsw_adjacency(spec, nodes, distance_function).map(Some),
		VectorAlgorithm::DiskAnn => {
			build_diskann_adjacency(spec, nodes, distance_function).map(Some)
		}
	}
}

fn build_hnsw_adjacency<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &mut [VectorNode<V>],
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<u32> {
	let options = match spec.options {
		crate::datastore::ClassIndexOptions::Hnsw(options) => options,
		_ => return Err(invalid("HNSW builder received another index kind")),
	};
	let mut entrypoint = 0_usize;
	let mut highest_level = nodes[0]
		.neighbors
		.len()
		.checked_sub(1)
		.ok_or_else(|| invalid("HNSW node has no levels"))?;
	for source in 1..nodes.len() {
		let source_levels = nodes[source].neighbors.len();
		let source_top =
			source_levels.checked_sub(1).ok_or_else(|| invalid("HNSW node has no levels"))?;
		let mut current = entrypoint;
		for level in (source_levels..=highest_level).rev() {
			current = greedy_hnsw_level(spec, nodes, source, current, level, distance_function)?;
		}
		let mut entrypoints = vec![current];
		for level in (0..source_levels.min(highest_level + 1)).rev() {
			let candidates = search_hnsw_level(
				spec,
				nodes,
				source,
				entrypoints,
				level,
				usize::from(options.ef_construction().get()),
				distance_function,
			)?;
			entrypoints = candidates
				.iter()
				.map(|candidate| {
					usize::try_from(candidate.ordinal)
						.map_err(|_error| invalid("HNSW candidate exceeds usize"))
				})
				.collect::<FerrosGrynnResult<_>>()?;
			let selected = select_hnsw_neighbors(
				spec,
				nodes,
				candidates,
				maximum_degree(spec, level)?,
				distance_function,
			)?;
			nodes[source].neighbors[level] = selected.clone();
			for neighbor in selected {
				let neighbor = usize::try_from(neighbor)
					.map_err(|_error| invalid("HNSW reciprocal ordinal exceeds usize"))?;
				let mut candidates = nodes[neighbor].neighbors[level].clone();
				candidates.push(
					u32::try_from(source)
						.map_err(|_error| invalid("HNSW source ordinal exceeds u32"))?,
				);
				let candidates =
					rank_hnsw_candidates(spec, nodes, neighbor, candidates, distance_function)?;
				nodes[neighbor].neighbors[level] = select_hnsw_neighbors(
					spec,
					nodes,
					candidates,
					maximum_degree(spec, level)?,
					distance_function,
				)?;
			}
		}
		if source_top > highest_level {
			entrypoint = source;
			highest_level = source_top;
		}
	}
	u32::try_from(entrypoint).map_err(|_error| invalid("HNSW entrypoint exceeds u32"))
}

fn greedy_hnsw_level<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	source: usize,
	mut current: usize,
	level: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<usize> {
	loop {
		let mut best = BuildCandidate {
			distance: build_distance(
				spec,
				nodes[source].vector.borrow(),
				nodes[current].vector.borrow(),
				distance_function,
			)?,
			ordinal: u32::try_from(current)
				.map_err(|_error| invalid("HNSW ordinal exceeds u32"))?,
		};
		for neighbor in nodes[current].neighbors.get(level).into_iter().flatten() {
			let neighbor = usize::try_from(*neighbor)
				.map_err(|_error| invalid("HNSW neighbor exceeds usize"))?;
			if neighbor >= source {
				continue;
			}
			let candidate = BuildCandidate {
				distance: build_distance(
					spec,
					nodes[source].vector.borrow(),
					nodes[neighbor].vector.borrow(),
					distance_function,
				)?,
				ordinal: u32::try_from(neighbor)
					.map_err(|_error| invalid("HNSW ordinal exceeds u32"))?,
			};
			if candidate < best {
				best = candidate;
			}
		}
		let next = usize::try_from(best.ordinal)
			.map_err(|_error| invalid("HNSW ordinal exceeds usize"))?;
		if next == current {
			return Ok(current);
		}
		current = next;
	}
}

fn search_hnsw_level<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	source: usize,
	entrypoints: impl IntoIterator<Item = usize>,
	level: usize,
	breadth: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<BuildCandidate>> {
	let mut frontier = BinaryHeap::new();
	let mut nearest = BinaryHeap::new();
	let mut seen = BTreeSet::new();
	for entrypoint in entrypoints {
		let entrypoint = entrypoint.min(source - 1);
		if !seen.insert(entrypoint) {
			continue;
		}
		let initial = BuildCandidate {
			distance: build_distance(
				spec,
				nodes[source].vector.borrow(),
				nodes[entrypoint].vector.borrow(),
				distance_function,
			)?,
			ordinal: u32::try_from(entrypoint)
				.map_err(|_error| invalid("HNSW entrypoint exceeds u32"))?,
		};
		frontier.push(Reverse(initial));
		nearest.push(initial);
	}
	if nearest.is_empty() {
		return Err(invalid("HNSW search has no entrypoint"));
	}
	while nearest.len() > breadth {
		let _ = nearest.pop();
	}
	while let Some(Reverse(candidate)) = frontier.pop() {
		if nearest.len() >= breadth && nearest.peek().is_some_and(|worst| candidate > *worst) {
			break;
		}
		let ordinal = usize::try_from(candidate.ordinal)
			.map_err(|_error| invalid("HNSW candidate exceeds usize"))?;
		for neighbor in nodes[ordinal].neighbors.get(level).into_iter().flatten() {
			let neighbor = usize::try_from(*neighbor)
				.map_err(|_error| invalid("HNSW neighbor exceeds usize"))?;
			if neighbor >= source || !seen.insert(neighbor) {
				continue;
			}
			let candidate = BuildCandidate {
				distance: build_distance(
					spec,
					nodes[source].vector.borrow(),
					nodes[neighbor].vector.borrow(),
					distance_function,
				)?,
				ordinal: u32::try_from(neighbor)
					.map_err(|_error| invalid("HNSW neighbor exceeds u32"))?,
			};
			if nearest.len() < breadth || nearest.peek().is_some_and(|worst| candidate < *worst) {
				frontier.push(Reverse(candidate));
				nearest.push(candidate);
				if nearest.len() > breadth {
					let _ = nearest.pop();
				}
			}
		}
	}
	Ok(nearest.into_iter().collect())
}

fn rank_hnsw_candidates<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	source: usize,
	candidates: impl IntoIterator<Item = u32>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<BuildCandidate>> {
	let mut unique = BTreeSet::new();
	let mut ranked = Vec::new();
	for candidate in candidates {
		let candidate_index = usize::try_from(candidate)
			.map_err(|_error| invalid("vector neighbor exceeds usize"))?;
		if candidate_index == source || candidate_index >= nodes.len() || !unique.insert(candidate)
		{
			continue;
		}
		ranked.push(BuildCandidate {
			distance: build_distance(
				spec,
				nodes[source].vector.borrow(),
				nodes[candidate_index].vector.borrow(),
				distance_function,
			)?,
			ordinal: candidate,
		});
	}
	Ok(ranked)
}

fn select_hnsw_neighbors<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	mut ranked: Vec<BuildCandidate>,
	maximum: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<u32>> {
	ranked.sort_unstable_by(|left, right| {
		left.distance.total_cmp(&right.distance).then_with(|| left.ordinal.cmp(&right.ordinal))
	});
	if ranked.len() <= maximum {
		return Ok(ranked.into_iter().map(|candidate| candidate.ordinal).collect());
	}
	let mut selected = Vec::with_capacity(maximum);
	let mut pruned = Vec::new();
	for candidate in ranked {
		let source_distance = candidate.distance;
		let candidate = candidate.ordinal;
		let candidate_index =
			usize::try_from(candidate).map_err(|_error| invalid("HNSW candidate exceeds usize"))?;
		let mut diverse = true;
		for neighbor in &selected {
			let neighbor_index = usize::try_from(*neighbor)
				.map_err(|_error| invalid("HNSW selected neighbor exceeds usize"))?;
			let neighbor_distance = build_distance(
				spec,
				nodes[candidate_index].vector.borrow(),
				nodes[neighbor_index].vector.borrow(),
				distance_function,
			)?;
			if source_distance > neighbor_distance {
				diverse = false;
				break;
			}
		}
		if diverse {
			selected.push(candidate);
			if selected.len() == maximum {
				break;
			}
		} else {
			pruned.push(candidate);
		}
	}
	let remaining = maximum.saturating_sub(selected.len());
	selected.extend(pruned.into_iter().take(remaining));
	Ok(selected)
}

fn build_diskann_adjacency<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &mut [VectorNode<V>],
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<u32> {
	let options = match spec.options {
		crate::datastore::ClassIndexOptions::DiskAnn(options) => options,
		_ => return Err(invalid("DiskANN builder received another index kind")),
	};
	let degree = usize::from(options.degree().get()).min(nodes.len().saturating_sub(1));
	let degree_with_slack = ((degree as f32) * GRAPH_SLACK_FACTOR).floor() as usize;
	let degree_with_slack = degree_with_slack.max(degree);
	let entrypoint = diskann_entrypoint(spec, nodes)?;
	let initial = (0..nodes.len())
		.map(|source| seed_diskann_neighbors(spec, nodes.len(), source, degree))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for (node, neighbors) in nodes.iter_mut().zip(initial) {
		node.neighbors[0] = neighbors;
	}
	for source in 0..nodes.len() {
		refine_diskann_node(
			spec,
			nodes,
			source,
			entrypoint,
			degree,
			degree_with_slack,
			usize::from(options.build_list_size().get()),
			options.alpha(),
			distance_function,
		)?;
	}
	for source in (0..nodes.len()).rev() {
		refine_diskann_node(
			spec,
			nodes,
			source,
			entrypoint,
			degree,
			degree_with_slack,
			usize::from(options.build_list_size().get()),
			options.alpha(),
			distance_function,
		)?;
	}
	for source in 0..nodes.len() {
		if nodes[source].neighbors[0].len() <= degree {
			continue;
		}
		let candidates = nodes[source].neighbors[0].clone();
		nodes[source].neighbors[0] = robust_prune_neighbors(
			spec,
			nodes,
			source,
			candidates,
			degree,
			options.alpha(),
			distance_function,
		)?;
	}
	u32::try_from(entrypoint).map_err(|_error| invalid("DiskANN entrypoint exceeds u32"))
}

fn diskann_entrypoint<V>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
) -> FerrosGrynnResult<usize> {
	nodes
		.iter()
		.enumerate()
		.min_by_key(|(_ordinal, node)| {
			let mut bytes = [0_u8; 24];
			bytes[..8].copy_from_slice(&spec.build_seed.to_le_bytes());
			bytes[8..16].copy_from_slice(&spec.build_revision.to_le_bytes());
			bytes[16..].copy_from_slice(&node.postings[0].raw().to_le_bytes());
			(rapidhash_v3(&bytes), node.postings[0])
		})
		.map(|(ordinal, _node)| ordinal)
		.ok_or_else(|| invalid("non-empty DiskANN has no entrypoint"))
}

fn seed_diskann_neighbors(
	spec: VectorBuildSpec,
	count: usize,
	source: usize,
	degree: usize,
) -> FerrosGrynnResult<Vec<u32>> {
	if degree == 0 {
		return Ok(Vec::new());
	}
	let mut selected = BTreeSet::new();
	for offset in 1..count {
		let adjacent = if offset % 2 == 1 {
			(source + offset.div_ceil(2)) % count
		} else {
			(source + count - offset / 2) % count
		};
		if adjacent != source {
			selected.insert(adjacent);
		}
		if selected.len() >= degree.min(2) {
			break;
		}
	}
	let mut attempt = 0_u64;
	while selected.len() < degree {
		let mut bytes = [0_u8; 32];
		bytes[..8].copy_from_slice(&spec.build_seed.to_le_bytes());
		bytes[8..16].copy_from_slice(&spec.build_revision.to_le_bytes());
		bytes[16..24].copy_from_slice(&(source as u64).to_le_bytes());
		bytes[24..].copy_from_slice(&attempt.to_le_bytes());
		let candidate = usize::try_from(rapidhash_v3(&bytes) % count as u64)
			.map_err(|_error| invalid("DiskANN seed ordinal exceeds usize"))?;
		if candidate != source {
			selected.insert(candidate);
		}
		attempt =
			attempt.checked_add(1).ok_or_else(|| invalid("DiskANN seed attempt overflows u64"))?;
	}
	selected
		.into_iter()
		.map(|ordinal| u32::try_from(ordinal).map_err(|_error| invalid("DiskANN seed exceeds u32")))
		.collect()
}

#[allow(
	clippy::too_many_arguments,
	reason = "the DiskANN refinement loop keeps its build inputs explicit"
)]
fn refine_diskann_node<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &mut [VectorNode<V>],
	source: usize,
	entrypoint: usize,
	degree: usize,
	degree_with_slack: usize,
	build_list_size: usize,
	alpha: f32,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<()> {
	if degree == 0 {
		return Ok(());
	}
	let mut candidates = search_diskann_graph(
		spec,
		nodes,
		source,
		entrypoint,
		build_list_size.max(degree),
		distance_function,
	)?;
	candidates.extend(nodes[source].neighbors[0].iter().copied());
	let selected =
		robust_prune_neighbors(spec, nodes, source, candidates, degree, alpha, distance_function)?;
	nodes[source].neighbors[0] = selected.clone();
	let source_ordinal =
		u32::try_from(source).map_err(|_error| invalid("DiskANN source ordinal exceeds u32"))?;
	for neighbor in selected {
		let neighbor = usize::try_from(neighbor)
			.map_err(|_error| invalid("DiskANN reciprocal ordinal exceeds usize"))?;
		if nodes[neighbor].neighbors[0].contains(&source_ordinal) {
			continue;
		}
		if nodes[neighbor].neighbors[0].len() < degree_with_slack {
			nodes[neighbor].neighbors[0].push(source_ordinal);
			continue;
		}
		let mut candidates = nodes[neighbor].neighbors[0].clone();
		candidates.push(source_ordinal);
		nodes[neighbor].neighbors[0] = robust_prune_neighbors(
			spec,
			nodes,
			neighbor,
			candidates,
			degree,
			alpha,
			distance_function,
		)?;
	}
	Ok(())
}

fn search_diskann_graph<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	source: usize,
	entrypoint: usize,
	breadth: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<u32>> {
	let start = if entrypoint == source {
		nodes[source].neighbors[0]
			.first()
			.copied()
			.unwrap_or_else(|| u32::try_from(source).unwrap_or(0))
	} else {
		u32::try_from(entrypoint).map_err(|_error| invalid("DiskANN entrypoint exceeds u32"))?
	};
	if usize::try_from(start).ok() == Some(source) {
		return Ok(Vec::new());
	}
	let mut queue = NeighborPriorityQueue::new(breadth.max(1));
	let start_index =
		usize::try_from(start).map_err(|_error| invalid("DiskANN start ordinal exceeds usize"))?;
	let mut seen = BTreeSet::from([start_index]);
	queue.insert(Neighbor::new(
		start,
		distance_f32(spec, nodes, source, start_index, distance_function)?,
	));
	while let Some(candidate) = queue.closest_notvisited() {
		let ordinal = usize::try_from(*candidate.id())
			.map_err(|_error| invalid("DiskANN candidate ordinal exceeds usize"))?;
		for neighbor in &nodes[ordinal].neighbors[0] {
			let neighbor_index = usize::try_from(*neighbor)
				.map_err(|_error| invalid("DiskANN neighbor ordinal exceeds usize"))?;
			if neighbor_index == source || !seen.insert(neighbor_index) {
				continue;
			}
			queue.insert(Neighbor::new(
				*neighbor,
				distance_f32(spec, nodes, source, neighbor_index, distance_function)?,
			));
		}
	}
	Ok(queue.iter().map(|candidate| *candidate.id()).collect())
}

fn distance_f32<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	left: usize,
	right: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<f32> {
	let distance = build_distance(
		spec,
		nodes[left].vector.borrow(),
		nodes[right].vector.borrow(),
		distance_function,
	)? as f32;
	if !distance.is_finite() {
		return Err(invalid("DiskANN build distance exceeds finite f32"));
	}
	Ok(distance)
}

#[allow(
	clippy::too_many_arguments,
	reason = "the pruning kernel keeps its immutable build inputs explicit"
)]
fn robust_prune_neighbors<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	nodes: &[VectorNode<V>],
	source: usize,
	candidates: impl IntoIterator<Item = u32>,
	degree: usize,
	alpha: f32,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<u32>> {
	let mut unique = BTreeSet::new();
	let mut ranked = Vec::new();
	for candidate in candidates {
		let candidate_index = usize::try_from(candidate)
			.map_err(|_error| invalid("DiskANN prune ordinal exceeds usize"))?;
		if candidate_index == source || candidate_index >= nodes.len() || !unique.insert(candidate)
		{
			continue;
		}
		ranked.push((
			distance_f32(spec, nodes, source, candidate_index, distance_function)?,
			nodes[candidate_index].postings[0],
			candidate,
		));
	}
	ranked.sort_unstable_by(|left, right| {
		left.0.total_cmp(&right.0).then_with(|| left.1.cmp(&right.1))
	});
	let prune_kind = diskann_prune_kind(spec.distance()?)?;
	let mut selected = Vec::<u32>::new();
	let mut current_alpha = 1.0_f32;
	while selected.len() < degree {
		for (source_distance, _identity, candidate) in &ranked {
			if selected.len() >= degree || selected.contains(candidate) {
				continue;
			}
			let candidate_index = usize::try_from(*candidate)
				.map_err(|_error| invalid("DiskANN prune ordinal exceeds usize"))?;
			let mut factor = 0.0_f32;
			for existing in &selected {
				let existing_index = usize::try_from(*existing)
					.map_err(|_error| invalid("DiskANN selected ordinal exceeds usize"))?;
				factor = prune_kind.update_occlude_factor(
					*source_distance,
					distance_f32(spec, nodes, candidate_index, existing_index, distance_function)?,
					factor,
					current_alpha,
				);
				if factor > current_alpha {
					break;
				}
			}
			if factor <= current_alpha {
				selected.push(*candidate);
			}
		}
		if current_alpha == alpha {
			break;
		}
		current_alpha = (current_alpha * alpha.min(1.2)).min(alpha);
	}
	if selected.len() < degree {
		for (_distance, _identity, candidate) in ranked {
			if !selected.contains(&candidate) {
				selected.push(candidate);
			}
			if selected.len() >= degree {
				break;
			}
		}
	}
	Ok(selected)
}

fn diskann_prune_kind(distance: VectorDistance) -> FerrosGrynnResult<PruneKind> {
	let metric = match distance {
		VectorDistance::Euclidean => Metric::L2,
		VectorDistance::Cosine => Metric::Cosine,
		VectorDistance::InnerProduct => Metric::InnerProduct,
		VectorDistance::CosineNormalized => Metric::CosineNormalized,
		_ => return Err(invalid("DiskANN pruning received an unsupported distance")),
	};
	Ok(PruneKind::from_metric(metric))
}

fn build_distance(
	spec: VectorBuildSpec,
	left: &Vector,
	right: &Vector,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<f64> {
	if let VectorDistance::Custom {
		function,
		version,
	} = spec.distance()?
	{
		let distance = distance_function(function, version, left, right)?;
		if !distance.is_finite() || distance < 0.0 {
			return Err(FerrosGrynnError::invalid_argument(
				"custom vector distance must be finite and non-negative",
			));
		}
		return Ok(distance);
	}
	if spec.algorithm()? == VectorAlgorithm::DiskAnn {
		if let Some(value) = diskann_vector_distance(left, right, spec.distance()?)? {
			return Ok(value);
		}
	}
	exact_vector_distance(left, right, spec.distance()?)
}

fn diskann_vector_distance(
	left: &Vector,
	right: &Vector,
	distance: VectorDistance,
) -> FerrosGrynnResult<Option<f64>> {
	macro_rules! evaluate {
		($kind:ty, $left:expr, $right:expr) => {{
			let value = match distance {
				VectorDistance::Euclidean => {
					<FullL2 as PureDistanceFunction<&[$kind], &[$kind], f32>>::evaluate(
						$left, $right,
					)
				}
				VectorDistance::Cosine => {
					<Cosine as PureDistanceFunction<&[$kind], &[$kind], f32>>::evaluate(
						$left, $right,
					)
				}
				VectorDistance::InnerProduct => {
					<InnerProduct as PureDistanceFunction<&[$kind], &[$kind], f32>>::evaluate(
						$left, $right,
					)
				}
				_ => return Ok(None),
			};
			Ok(Some(f64::from(value)))
		}};
	}
	match (left, right) {
		(Vector::F32(left), Vector::F32(right)) => {
			if distance == VectorDistance::CosineNormalized {
				return normalized_f32_distance(left.iter().copied(), right.iter().copied())
					.map(Some);
			}
			evaluate!(f32, left.as_slice(), right.as_slice())
		}
		(Vector::F16(left), Vector::F16(right)) => {
			if distance == VectorDistance::CosineNormalized {
				return normalized_f32_distance(
					left.iter().map(|value| value.to_f32()),
					right.iter().map(|value| value.to_f32()),
				)
				.map(Some);
			}
			evaluate!(half::f16, left.as_slice(), right.as_slice())
		}
		(Vector::I8(left), Vector::I8(right)) => evaluate!(i8, left.as_slice(), right.as_slice()),
		(Vector::U8(left), Vector::U8(right)) => evaluate!(u8, left.as_slice(), right.as_slice()),
		_ => Ok(None),
	}
}

fn normalized_f32_distance(
	left: impl ExactSizeIterator<Item = f32>,
	right: impl ExactSizeIterator<Item = f32>,
) -> FerrosGrynnResult<f64> {
	if left.len() != right.len() {
		return Err(FerrosGrynnError::invalid_argument(
			"cosine distance requires vectors with equal dimensions",
		));
	}
	let (mut dot, mut left_norm, mut right_norm) = (0.0_f32, 0.0_f32, 0.0_f32);
	for (left, right) in left.zip(right) {
		dot += left * right;
		left_norm += left * left;
		right_norm += right * right;
	}
	if left_norm == 0.0 || right_norm == 0.0 {
		return Err(FerrosGrynnError::invalid_argument(
			"cosine distance is undefined for a zero vector",
		));
	}
	Ok(f64::from(1.0 - dot / (left_norm.sqrt() * right_norm.sqrt())))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::{collections::BTreeSet, num::NonZeroU16};

	use super::{
		build_vector_graph_with_distance,
		build_vector_index,
		build_with_hasher,
		enforce_hnsw_build_budget,
		estimated_hnsw_build_bytes,
		invalid,
		normalized_f32_distance,
		rank_hnsw_candidates,
		search_hnsw_level,
		select_hnsw_neighbors,
	};
	use crate::{
		datastore::{
			ClassId,
			ClassIndexOptions,
			DiskAnnIndexOptions,
			FunctionId,
			HnswIndexOptions,
			IndexId,
			IndexScope,
			SchemaVersion,
			VectorDistance,
			exact_vector_distance,
		},
		graph::AtomKind,
		storage::vector_index::{
			VectorBuildSpec,
			VectorDocument,
			VectorNode,
			decode_vector_index,
			search_vector_index_with_distance,
		},
		values::{PropertyKeyId, Vector, VectorKind, storage::DiskAtomRef},
	};

	#[test]
	fn hnsw_build_budget_rejects_the_same_input_below_its_estimate() {
		let options = HnswIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
		)
		.unwrap();
		let spec = spec(ClassIndexOptions::Hnsw(options));
		let documents = vec![
			VectorDocument {
				atom: atom(1),
				vector: Vector::from(vec![0.0_f32, 1.0]),
			},
			VectorDocument {
				atom: atom(2),
				vector: Vector::from(vec![1.0_f32, 0.0]),
			},
		];
		let estimate = estimated_hnsw_build_bytes(spec, &documents).unwrap();

		assert!(enforce_hnsw_build_budget(spec, &documents, estimate).is_ok());
		assert!(enforce_hnsw_build_budget(spec, &documents, estimate - 1).is_err());
	}

	#[test]
	fn hnsw_build_keeps_nearest_candidates_from_multiple_entrypoints() {
		let options = HnswIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(1).unwrap(),
			VectorDistance::Euclidean,
		)
		.unwrap();
		let build_spec = spec(ClassIndexOptions::Hnsw(options));
		let nodes = vec![
			VectorNode {
				postings: vec![atom(1)],
				vector: Vector::from(vec![10.0_f32]),
				vector_hash: 0,
				neighbors: vec![vec![1]],
			},
			VectorNode {
				postings: vec![atom(2)],
				vector: Vector::from(vec![5.0_f32]),
				vector_hash: 0,
				neighbors: vec![vec![2]],
			},
			VectorNode {
				postings: vec![atom(3)],
				vector: Vector::from(vec![0.0_f32]),
				vector_hash: 0,
				neighbors: vec![Vec::new()],
			},
			VectorNode {
				postings: vec![atom(4)],
				vector: Vector::from(vec![0.0_f32]),
				vector_hash: 0,
				neighbors: vec![Vec::new()],
			},
		];
		let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"built-in distance unexpectedly invoked a UDF",
			))
		};

		let actual = search_hnsw_level(build_spec, &nodes, 3, [0, 2], 0, 2, &mut unavailable)
			.unwrap()
			.into_iter()
			.map(|candidate| candidate.ordinal)
			.collect::<BTreeSet<_>>();

		assert_eq!(actual, BTreeSet::from([1, 2]));
	}

	#[test]
	fn hnsw_neighbor_selection_keeps_diverse_and_pruned_connections()
	-> crate::gql_status::FerrosGrynnResult<()> {
		let dimensions =
			NonZeroU16::new(2).ok_or_else(|| invalid("HNSW diversity test dimension is zero"))?;
		let options =
			HnswIndexOptions::defaults(VectorKind::F32, dimensions, VectorDistance::Euclidean)?;
		let build_spec = spec(ClassIndexOptions::Hnsw(options));
		let nodes = vec![
			VectorNode {
				postings: vec![atom(1)],
				vector: Vector::from(vec![1.0_f32, 0.0]),
				vector_hash: 0,
				neighbors: vec![Vec::new()],
			},
			VectorNode {
				postings: vec![atom(2)],
				vector: Vector::from(vec![1.1_f32, 0.0]),
				vector_hash: 0,
				neighbors: vec![Vec::new()],
			},
			VectorNode {
				postings: vec![atom(3)],
				vector: Vector::from(vec![0.0_f32, 2.0]),
				vector_hash: 0,
				neighbors: vec![Vec::new()],
			},
			VectorNode {
				postings: vec![atom(4)],
				vector: Vector::from(vec![0.0_f32, 0.0]),
				vector_hash: 0,
				neighbors: vec![Vec::new()],
			},
		];
		let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"built-in distance unexpectedly invoked a UDF",
			))
		};

		let ranked = rank_hnsw_candidates(build_spec, &nodes, 3, [0, 1, 2], &mut unavailable)?;
		let selected = select_hnsw_neighbors(build_spec, &nodes, ranked, 2, &mut unavailable)?;

		assert_eq!(selected, vec![0, 2]);
		Ok(())
	}

	fn atom(slot: u64) -> DiskAtomRef {
		DiskAtomRef::from_parts(AtomKind::Node, slot).unwrap()
	}

	fn spec(options: ClassIndexOptions) -> VectorBuildSpec {
		VectorBuildSpec {
			index: IndexId::new(7).unwrap(),
			index_version: SchemaVersion::INITIAL,
			scope: IndexScope::Base,
			class: ClassId::new(3).unwrap(),
			field: PropertyKeyId::new(2),
			options,
			build_revision: 11,
			build_seed: 19,
		}
	}

	#[test]
	fn hnsw_bytes_are_deterministic_and_round_trip() {
		let options = HnswIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
		)
		.unwrap();
		let documents = || {
			vec![
				VectorDocument {
					atom: atom(2),
					vector: Vector::from(vec![1.0_f32, 0.0]),
				},
				VectorDocument {
					atom: atom(1),
					vector: Vector::from(vec![0.0_f32, 0.0]),
				},
				VectorDocument {
					atom: atom(3),
					vector: Vector::from(vec![2.0_f32, 0.0]),
				},
			]
		};
		let left = build_vector_index(spec(ClassIndexOptions::Hnsw(options)), documents()).unwrap();
		let right =
			build_vector_index(spec(ClassIndexOptions::Hnsw(options)), documents()).unwrap();
		assert_eq!(
			left.pages.iter().map(|page| page.as_bytes()).collect::<Vec<_>>(),
			right.pages.iter().map(|page| page.as_bytes()).collect::<Vec<_>>(),
		);
		let decoded = decode_vector_index(&left.pages).unwrap();
		assert_eq!(decoded.nodes.len(), 3);
		assert!(decoded.entrypoint.is_some());
	}

	#[test]
	fn graph_builder_keeps_borrowed_vector_payloads() {
		let options = HnswIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
		)
		.unwrap();
		let source = [Vector::from(vec![0.0_f32, 1.0]), Vector::from(vec![1.0_f32, 0.0])];
		let documents = vec![
			VectorDocument {
				atom: atom(2),
				vector: &source[1],
			},
			VectorDocument {
				atom: atom(1),
				vector: &source[0],
			},
		];
		let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"built-in distance unexpectedly invoked a UDF",
			))
		};
		let (_entrypoint, nodes) = build_vector_graph_with_distance(
			spec(ClassIndexOptions::Hnsw(options)),
			documents,
			&mut unavailable,
		)
		.unwrap();

		assert!(std::ptr::eq(nodes[0].vector, &source[0]));
		assert!(std::ptr::eq(nodes[1].vector, &source[1]));
	}

	#[test]
	fn hashed_vector_compares_full_bytes_under_forced_collisions() {
		let options = DiskAnnIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
			true,
		)
		.unwrap();
		let documents = vec![
			VectorDocument {
				atom: atom(1),
				vector: Vector::from(vec![1.0_f32, 2.0]),
			},
			VectorDocument {
				atom: atom(2),
				vector: Vector::from(vec![3.0_f32, 4.0]),
			},
			VectorDocument {
				atom: atom(3),
				vector: Vector::from(vec![1.0_f32, 2.0]),
			},
		];
		let built =
			build_with_hasher(spec(ClassIndexOptions::DiskAnn(options)), documents, |_bytes| 0)
				.unwrap();
		let decoded = decode_vector_index(&built.pages).unwrap();
		assert_eq!(decoded.nodes.len(), 2);
		assert_eq!(decoded.nodes[0].postings, vec![atom(1), atom(3)]);
		assert_eq!(decoded.nodes[1].postings, vec![atom(2)]);
	}

	#[test]
	fn hashed_vector_changes_lookup_not_diskann_graph() {
		let build = |hashed_vector| {
			let options = DiskAnnIndexOptions::defaults(
				VectorKind::F32,
				NonZeroU16::new(2).unwrap(),
				VectorDistance::Euclidean,
				hashed_vector,
			)
			.unwrap();
			let documents = vec![
				VectorDocument {
					atom: atom(1),
					vector: Vector::from(vec![1.0_f32, 2.0]),
				},
				VectorDocument {
					atom: atom(2),
					vector: Vector::from(vec![3.0_f32, 4.0]),
				},
				VectorDocument {
					atom: atom(3),
					vector: Vector::from(vec![1.0_f32, 2.0]),
				},
			];
			let built =
				build_vector_index(spec(ClassIndexOptions::DiskAnn(options)), documents).unwrap();
			decode_vector_index(&built.pages).unwrap()
		};
		let plain = build(false);
		let hashed = build(true);

		assert_eq!(plain.entrypoint, hashed.entrypoint);
		assert_eq!(plain.nodes, hashed.nodes);
	}

	#[test]
	fn vector_codec_rejects_checksum_and_resealed_header_corruption() {
		let options = HnswIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
		)
		.unwrap();
		let documents = || {
			vec![VectorDocument {
				atom: atom(1),
				vector: Vector::from(vec![1.0_f32, 2.0]),
			}]
		};

		let mut checksum =
			build_vector_index(spec(ClassIndexOptions::Hnsw(options)), documents()).unwrap();
		checksum.pages[0].as_bytes_mut()[47] = 1;
		assert!(decode_vector_index(&checksum.pages).is_err());

		let mut reserved =
			build_vector_index(spec(ClassIndexOptions::Hnsw(options)), documents()).unwrap();
		reserved.pages[0].as_bytes_mut()[47] = 1;
		reserved.pages[0].seal().unwrap();
		assert!(decode_vector_index(&reserved.pages).is_err());
	}

	#[test]
	fn hnsw_build_stays_below_an_all_pairs_distance_scan() {
		let count = 512_u16;
		let distance = VectorDistance::Custom {
			function: FunctionId::new(17).unwrap(),
			version: SchemaVersion::INITIAL,
		};
		let options = HnswIndexOptions::new(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			distance,
			NonZeroU16::new(4).unwrap(),
			NonZeroU16::new(16).unwrap(),
		)
		.unwrap();
		let documents = (0..count)
			.map(|ordinal| {
				let value = f32::from(ordinal);
				VectorDocument {
					atom: atom(u64::from(ordinal) + 1),
					vector: Vector::from(vec![value, value.mul_add(0.25, 1.0)]),
				}
			})
			.collect::<Vec<_>>();
		let mut evaluations = 0_usize;
		let mut custom = |_function, _version, left: &Vector, right: &Vector| {
			evaluations = evaluations.saturating_add(1);
			let (Vector::F32(left), Vector::F32(right)) = (left, right) else {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"test distance received a non-F32 vector",
				));
			};
			Ok(left
				.iter()
				.zip(right)
				.map(|(left, right)| f64::from(left - right).powi(2))
				.sum::<f64>()
				.sqrt())
		};
		let (_entrypoint, nodes) = build_vector_graph_with_distance(
			spec(ClassIndexOptions::Hnsw(options)),
			documents,
			&mut custom,
		)
		.unwrap();
		let count = usize::from(count);
		let all_pairs = count.checked_mul(count - 1).unwrap() / 2;
		assert!(
			evaluations < all_pairs,
			"HNSW evaluated {evaluations} distances for {count} nodes"
		);
		assert!(nodes.iter().all(|node| {
			node.neighbors.iter().enumerate().all(|(level, neighbors)| {
				neighbors.len()
					<= if level == 0 {
						8
					} else {
						4
					}
			})
		}));
	}

	#[test]
	fn diskann_build_is_degree_bounded_and_reachable_from_entrypoint() {
		let count = 128_u16;
		let options = DiskAnnIndexOptions::new(
			VectorKind::F32,
			NonZeroU16::new(2).unwrap(),
			VectorDistance::Euclidean,
			NonZeroU16::new(8).unwrap(),
			NonZeroU16::new(16).unwrap(),
			1.2,
			false,
		)
		.unwrap();
		let documents = (0..count)
			.map(|ordinal| {
				let angle = f32::from(ordinal) / f32::from(count) * std::f32::consts::TAU;
				VectorDocument {
					atom: atom(u64::from(ordinal) + 1),
					vector: Vector::from(vec![angle.cos(), angle.sin()]),
				}
			})
			.collect::<Vec<_>>();
		let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"built-in distance unexpectedly invoked a UDF",
			))
		};
		let (entrypoint, nodes) = build_vector_graph_with_distance(
			spec(ClassIndexOptions::DiskAnn(options)),
			documents,
			&mut unavailable,
		)
		.unwrap();
		assert!(nodes.iter().all(|node| node.neighbors[0].len() <= 8));
		let mut pending = vec![entrypoint.unwrap()];
		let mut visited = BTreeSet::new();
		while let Some(ordinal) = pending.pop() {
			if !visited.insert(ordinal) {
				continue;
			}
			let index = usize::try_from(ordinal).unwrap();
			pending.extend(nodes[index].neighbors[0].iter().copied());
		}
		assert_eq!(visited.len(), nodes.len());
	}

	#[test]
	#[ignore = "configurable HNSW recall probe for the Stage-23 performance gate"]
	fn hnsw_recall_across_search_breadths() -> crate::gql_status::FerrosGrynnResult<()> {
		let count = std::env::var("FG_HNSW_RECALL_COUNT")
			.map_or(Ok(100_000), |value| value.parse::<usize>())
			.map_err(|error| invalid(format!("invalid FG_HNSW_RECALL_COUNT: {error}")))?;
		if count == 0 {
			return Err(invalid("HNSW probe count is zero"));
		}
		let dimensions = std::env::var("FG_HNSW_RECALL_DIMENSIONS")
			.map_or(Ok(32), |value| value.parse::<u16>())
			.map_err(|error| invalid(format!("invalid FG_HNSW_RECALL_DIMENSIONS: {error}")))?;
		let dimensions =
			NonZeroU16::new(dimensions).ok_or_else(|| invalid("HNSW probe dimension is zero"))?;
		let source = (0..count)
			.map(|ordinal| {
				let source = if ordinal % 5 == 0 {
					ordinal % 100
				} else {
					ordinal
				};
				Vector::from(
					(0..usize::from(dimensions.get()))
						.map(|axis| {
							let mut value = (source as u64).wrapping_add(
								0x9e37_79b9_7f4a_7c15_u64.wrapping_mul(axis as u64 + 1),
							);
							value ^= value >> 30;
							value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
							value ^= value >> 27;
							value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
							value ^= value >> 31;
							(value as u32) as f32 / u32::MAX as f32
						})
						.collect::<Vec<_>>(),
				)
			})
			.collect::<Vec<_>>();
		let options =
			HnswIndexOptions::defaults(VectorKind::F32, dimensions, VectorDistance::Euclidean)?;
		let documents = source
			.iter()
			.enumerate()
			.map(|(ordinal, vector)| VectorDocument {
				atom: atom(ordinal as u64 + 1),
				vector,
			})
			.collect();
		let built = build_vector_index(spec(ClassIndexOptions::Hnsw(options)), documents)?;
		let index = decode_vector_index(&built.pages)?;
		let entrypoint =
			index.entrypoint.ok_or_else(|| invalid("non-empty HNSW has no entrypoint"))?;
		let mut pending = vec![entrypoint];
		let mut reachable = BTreeSet::new();
		while let Some(ordinal) = pending.pop() {
			if !reachable.insert(ordinal) {
				continue;
			}
			let ordinal = usize::try_from(ordinal)
				.map_err(|_error| invalid("HNSW probe ordinal exceeds usize"))?;
			pending.extend(index.nodes[ordinal].neighbors[0].iter().copied());
		}
		let node_count = index.nodes.len();
		let total_degree = index.nodes.iter().map(|node| node.neighbors[0].len()).sum::<usize>();
		eprintln!(
			"HNSW {count} rows / {node_count} unique vectors x {} dimensions; level-0 \
			 reachability: {}/{node_count}; average out-degree: {:.2}",
			dimensions.get(),
			reachable.len(),
			total_degree as f64 / node_count as f64,
		);
		let queries = (0..20).map(|ordinal| (7 + ordinal * 7_919) % count).collect::<Vec<_>>();
		let mut expected = Vec::with_capacity(queries.len());
		for query_ordinal in &queries {
			let query = &source[*query_ordinal];
			let mut exact = source
				.iter()
				.enumerate()
				.map(|(ordinal, vector)| {
					Ok((
						exact_vector_distance(query, vector, VectorDistance::Euclidean)?,
						atom(ordinal as u64 + 1),
					))
				})
				.collect::<crate::gql_status::FerrosGrynnResult<Vec<_>>>()?;
			exact.sort_unstable_by(|left, right| {
				left.0.total_cmp(&right.0).then_with(|| left.1.cmp(&right.1))
			});
			expected.push(
				exact.into_iter().take(100).map(|(_distance, atom)| atom).collect::<Vec<_>>(),
			);
		}
		let mut failures = Vec::new();
		for k in [1_usize, 10, 100].into_iter().filter(|k| *k <= count) {
			let expected = expected
				.iter()
				.map(|values| values.iter().take(k).copied().collect::<BTreeSet<_>>())
				.collect::<Vec<_>>();
			let expected_total = expected.iter().map(BTreeSet::len).sum::<usize>();
			let default_breadth = options.default_search_breadth(k);
			let mut default_matched = 0;
			let mut breadths = vec![k, 64, 150, 200, 300, 512, default_breadth, 1_024];
			breadths.retain(|breadth| *breadth >= k);
			breadths.sort_unstable();
			breadths.dedup();
			for breadth in breadths {
				let mut matched = 0;
				for (query_ordinal, expected) in queries.iter().zip(&expected) {
					let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
						Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"built-in distance unexpectedly invoked a UDF",
						))
					};
					let actual = search_vector_index_with_distance(
						&index,
						&source[*query_ordinal],
						k,
						breadth,
						&mut unavailable,
					)?
					.into_iter()
					.map(|hit| hit.atom)
					.collect::<BTreeSet<_>>();
					matched += expected.intersection(&actual).count();
				}
				if breadth == default_breadth {
					default_matched = matched;
				}
				eprintln!("HNSW breadth {breadth}: recall@{k} = {matched}/{expected_total}");
			}
			if k == 10 && default_matched * 100 < expected_total * 95 {
				failures.push(format!(
					"default HNSW breadth {default_breadth} recall@{k} was \
					 {default_matched}/{expected_total}",
				));
			}
		}
		if !failures.is_empty() {
			return Err(invalid(failures.join("; ")));
		}
		Ok(())
	}

	#[test]
	fn default_ann_recall_at_ten_meets_the_exact_oracle() {
		let source = (0_u16..128)
			.map(|ordinal| {
				let vector = (0_u16..16)
					.map(|dimension| {
						let mixed = (u32::from(ordinal) * 73
							+ u32::from(dimension) * 151
							+ u32::from(ordinal) * u32::from(dimension) * 19)
							% 997;
						f32::from(u16::try_from(mixed).unwrap()) / 997.0
					})
					.collect::<Vec<_>>();
				(atom(u64::from(ordinal) + 1), Vector::from(vector))
			})
			.collect::<Vec<_>>();
		let dimensions = NonZeroU16::new(16).unwrap();
		let options = [
			ClassIndexOptions::Hnsw(
				HnswIndexOptions::defaults(VectorKind::F32, dimensions, VectorDistance::Euclidean)
					.unwrap(),
			),
			ClassIndexOptions::DiskAnn(
				DiskAnnIndexOptions::defaults(
					VectorKind::F32,
					dimensions,
					VectorDistance::Euclidean,
					false,
				)
				.unwrap(),
			),
		];
		for options in options {
			let documents = source
				.iter()
				.map(|(atom, vector)| VectorDocument {
					atom: *atom,
					vector: vector.clone(),
				})
				.collect();
			let built = build_vector_index(spec(options), documents).unwrap();
			let index = decode_vector_index(&built.pages).unwrap();
			let mut matched = 0_usize;
			let mut expected_total = 0_usize;
			for query_ordinal in [0_usize, 37, 79, 127] {
				let query = &source[query_ordinal].1;
				let mut exact = source
					.iter()
					.map(|(atom, vector)| {
						(
							exact_vector_distance(query, vector, VectorDistance::Euclidean)
								.unwrap(),
							*atom,
						)
					})
					.collect::<Vec<_>>();
				exact.sort_unstable_by(|left, right| {
					left.0.total_cmp(&right.0).then_with(|| left.1.cmp(&right.1))
				});
				let expected = exact
					.into_iter()
					.take(10)
					.map(|(_distance, atom)| atom)
					.collect::<BTreeSet<_>>();
				let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
					Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"built-in distance unexpectedly invoked a UDF",
					))
				};
				let actual =
					search_vector_index_with_distance(&index, query, 10, 64, &mut unavailable)
						.unwrap()
						.into_iter()
						.map(|hit| hit.atom)
						.collect::<BTreeSet<_>>();
				matched += expected.intersection(&actual).count();
				expected_total += expected.len();
			}
			assert!(
				matched * 100 >= expected_total * 95,
				"default ANN recall@10 was {matched}/{expected_total} for {options:?}"
			);
		}
	}

	#[test]
	fn normalized_distance_streams_components_without_materializing_copies() {
		let same = normalized_f32_distance([3.0, 4.0].into_iter(), [6.0, 8.0].into_iter()).unwrap();
		let orthogonal =
			normalized_f32_distance([1.0, 0.0].into_iter(), [0.0, 1.0].into_iter()).unwrap();
		assert!(same.abs() <= f64::from(f32::EPSILON));
		assert!((orthogonal - 1.0).abs() <= f64::from(f32::EPSILON));
		assert!(normalized_f32_distance([1.0].into_iter(), [1.0, 2.0].into_iter()).is_err());
		assert!(normalized_f32_distance([0.0, 0.0].into_iter(), [1.0, 0.0].into_iter()).is_err());
	}
}
