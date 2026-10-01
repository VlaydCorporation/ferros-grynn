//! HNSW search over resident topology with lazily loaded vector payloads.

use std::{
	cmp::{Ordering, Reverse},
	collections::{BTreeSet, BinaryHeap},
	sync::Arc,
};

use super::{
	codec::{DecodedHnswIndex, VectorAlgorithm, validate_vector},
	delta::{DecodedVectorDelta, VectorDeltaRecord},
	search::VectorSearchHit,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Vector,
};

#[derive(Clone, Copy)]
struct Candidate {
	distance: f64,
	ordinal: u32,
}

impl PartialEq for Candidate {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Candidate {
	fn cmp(&self, other: &Self) -> Ordering {
		self.distance.total_cmp(&other.distance).then_with(|| self.ordinal.cmp(&other.ordinal))
	}
}

/// Searches resident HNSW topology while the caller supplies vectors from a
/// process-wide bounded cache or the immutable side-file.
pub(crate) fn search_cached_hnsw_with_distance(
	index: &DecodedHnswIndex,
	delta: Option<&DecodedVectorDelta>,
	query: &Vector,
	k: usize,
	breadth: usize,
	load_vector: &mut impl FnMut(u32) -> FerrosGrynnResult<Arc<Vector>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
	if index.spec.algorithm()? != VectorAlgorithm::Hnsw {
		return Err(invalid("cached HNSW search received another vector algorithm"));
	}
	if k == 0 || breadth < k {
		return Err(FerrosGrynnError::invalid_argument(
			"ANN search requires K > 0 and breadth >= K",
		));
	}
	validate_vector(index.spec, query)?;
	if let Some(delta) = delta {
		if delta.spec.base != index.spec {
			return Err(invalid("vector delta definition disagrees with its HNSW base"));
		}
	}
	let shadowed = delta
		.into_iter()
		.flat_map(|delta| delta.records.iter())
		.map(VectorDeltaRecord::atom)
		.collect::<BTreeSet<_>>();
	let requested = k.saturating_add(shadowed.len()).max(k);
	let candidates = match index.entrypoint {
		Some(entrypoint) => {
			let entrypoint =
				greedy_upper_levels(index, query, entrypoint, load_vector, distance_function)?;
			bounded_level_zero(
				index,
				query,
				entrypoint,
				breadth.max(requested),
				load_vector,
				distance_function,
			)?
		}
		None => BTreeSet::new(),
	};
	let mut hits = rerank(
		index,
		query,
		candidates.iter().copied(),
		&shadowed,
		load_vector,
		distance_function,
	)?;
	append_delta(index, delta, query, &mut hits, distance_function)?;
	if hits.len() < k && candidates.len() < index.nodes.len() {
		hits =
			rerank(index, query, 0..index.nodes.len(), &shadowed, load_vector, distance_function)?;
		append_delta(index, delta, query, &mut hits, distance_function)?;
	}
	hits.sort_unstable_by(|left, right| {
		left.distance.total_cmp(&right.distance).then_with(|| left.atom.cmp(&right.atom))
	});
	hits.dedup_by_key(|hit| hit.atom);
	hits.truncate(k);
	Ok(hits)
}

fn greedy_upper_levels(
	index: &DecodedHnswIndex,
	query: &Vector,
	mut current: u32,
	load_vector: &mut impl FnMut(u32) -> FerrosGrynnResult<Arc<Vector>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<u32> {
	let top = node(index, current)?.neighbors.len();
	for level in (1..top).rev() {
		loop {
			let mut best = Candidate {
				distance: distance_to(index, query, current, load_vector, distance_function)?,
				ordinal: current,
			};
			for neighbor in &node(index, current)?.neighbors[level] {
				let candidate = Candidate {
					distance: distance_to(index, query, *neighbor, load_vector, distance_function)?,
					ordinal: *neighbor,
				};
				if candidate < best {
					best = candidate;
				}
			}
			if best.ordinal == current {
				break;
			}
			current = best.ordinal;
		}
	}
	Ok(current)
}

fn bounded_level_zero(
	index: &DecodedHnswIndex,
	query: &Vector,
	entrypoint: u32,
	breadth: usize,
	load_vector: &mut impl FnMut(u32) -> FerrosGrynnResult<Arc<Vector>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<BTreeSet<usize>> {
	let initial = Candidate {
		distance: distance_to(index, query, entrypoint, load_vector, distance_function)?,
		ordinal: entrypoint,
	};
	let mut frontier = BinaryHeap::from([Reverse(initial)]);
	let mut nearest = BinaryHeap::from([initial]);
	let mut seen =
		BTreeSet::from([usize::try_from(entrypoint)
			.map_err(|_error| invalid("HNSW entrypoint exceeds usize"))?]);
	while let Some(Reverse(candidate)) = frontier.pop() {
		if nearest.len() >= breadth && nearest.peek().is_some_and(|worst| candidate > *worst) {
			break;
		}
		for neighbor in &node(index, candidate.ordinal)?.neighbors[0] {
			let neighbor_index = usize::try_from(*neighbor)
				.map_err(|_error| invalid("HNSW neighbor exceeds usize"))?;
			if !seen.insert(neighbor_index) {
				continue;
			}
			let candidate = Candidate {
				distance: distance_to(index, query, *neighbor, load_vector, distance_function)?,
				ordinal: *neighbor,
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
	nearest
		.into_iter()
		.map(|candidate| {
			usize::try_from(candidate.ordinal)
				.map_err(|_error| invalid("HNSW candidate ordinal exceeds usize"))
		})
		.collect()
}

fn rerank(
	index: &DecodedHnswIndex,
	query: &Vector,
	ordinals: impl IntoIterator<Item = usize>,
	shadowed: &BTreeSet<crate::values::storage::DiskAtomRef>,
	load_vector: &mut impl FnMut(u32) -> FerrosGrynnResult<Arc<Vector>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
	let mut hits = Vec::new();
	for ordinal in ordinals {
		let ordinal = u32::try_from(ordinal)
			.map_err(|_error| invalid("HNSW candidate ordinal exceeds u32"))?;
		let distance = distance_to(index, query, ordinal, load_vector, distance_function)?;
		for atom in &node(index, ordinal)?.postings {
			if !shadowed.contains(atom) {
				hits.push(VectorSearchHit {
					atom: *atom,
					distance,
				});
			}
		}
	}
	Ok(hits)
}

fn append_delta(
	index: &DecodedHnswIndex,
	delta: Option<&DecodedVectorDelta>,
	query: &Vector,
	hits: &mut Vec<VectorSearchHit>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<()> {
	for record in delta.into_iter().flat_map(|delta| delta.records.iter()) {
		if let VectorDeltaRecord::Upsert {
			atom,
			vector,
		} = record
		{
			hits.push(VectorSearchHit {
				atom: *atom,
				distance: vector_distance(index, query, vector, distance_function)?,
			});
		}
	}
	Ok(())
}

fn distance_to(
	index: &DecodedHnswIndex,
	query: &Vector,
	ordinal: u32,
	load_vector: &mut impl FnMut(u32) -> FerrosGrynnResult<Arc<Vector>>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<f64> {
	let vector = load_vector(ordinal)?;
	vector_distance(index, query, &vector, distance_function)
}

fn vector_distance(
	index: &DecodedHnswIndex,
	left: &Vector,
	right: &Vector,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<f64> {
	match index.spec.distance()? {
		crate::datastore::VectorDistance::Custom {
			function,
			version,
		} => distance_function(function, version, left, right),
		distance => crate::datastore::exact_vector_distance(left, right, distance),
	}
}

fn node(index: &DecodedHnswIndex, ordinal: u32) -> FerrosGrynnResult<&super::codec::HnswGraphNode> {
	index
		.nodes
		.get(usize::try_from(ordinal).map_err(|_error| invalid("HNSW ordinal exceeds usize"))?)
		.ok_or_else(|| invalid("HNSW ordinal lies outside its resident graph"))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
