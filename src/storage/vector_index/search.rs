//! Bounded graph search with mandatory full-precision reranking.

use std::{
	cmp::{Ordering, Reverse},
	collections::{BTreeSet, BinaryHeap},
};

use super::{
	codec::{
		DecodedVectorIndex,
		PagedVectorHeader,
		PagedVectorNode,
		VectorAlgorithm,
		validate_vector,
	},
	delta::{DecodedVectorDelta, VectorDeltaRecord},
};
use crate::{
	datastore::exact_vector_distance,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Vector, storage::DiskAtomRef},
};

const PAGED_NODE_BATCH: usize = 32;

/// One stable exact-reranked result from an immutable ANN generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VectorSearchHit {
	pub(crate) atom: DiskAtomRef,
	pub(crate) distance: f64,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
	distance: f64,
	ordinal: u32,
}

#[derive(Debug, Clone, Copy)]
struct RankedHit(VectorSearchHit);

impl PartialEq for RankedHit {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}

impl Eq for RankedHit {}

impl PartialOrd for RankedHit {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for RankedHit {
	fn cmp(&self, other: &Self) -> Ordering {
		self.0.distance.total_cmp(&other.0.distance).then_with(|| self.0.atom.cmp(&other.0.atom))
	}
}

struct TopHits {
	limit: usize,
	hits: BinaryHeap<RankedHit>,
}

impl TopHits {
	fn new(limit: usize) -> Self {
		Self {
			limit,
			hits: BinaryHeap::with_capacity(limit.saturating_add(1)),
		}
	}

	fn push(&mut self, hit: VectorSearchHit) {
		if self.limit == 0 {
			return;
		}
		self.hits.push(RankedHit(hit));
		if self.hits.len() > self.limit {
			let _ = self.hits.pop();
		}
	}

	fn into_sorted(self) -> Vec<VectorSearchHit> {
		let mut hits = self.hits.into_iter().map(|hit| hit.0).collect::<Vec<_>>();
		hits.sort_unstable_by(|left, right| {
			left.distance.total_cmp(&right.distance).then_with(|| left.atom.cmp(&right.atom))
		});
		hits
	}
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

/// Searches one immutable graph with the concrete versioned custom-distance
/// evaluator owned by the datastore composition boundary.
pub(crate) fn search_vector_index_with_distance(
	index: &DecodedVectorIndex,
	query: &Vector,
	k: usize,
	breadth: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
	if k == 0 || breadth < k {
		return Err(FerrosGrynnError::invalid_argument(
			"ANN search requires K > 0 and breadth >= K",
		));
	}
	validate_vector(index.spec, query)?;
	let Some(mut entrypoint) = index.entrypoint else {
		return Ok(Vec::new());
	};
	if index.spec.algorithm()? == VectorAlgorithm::Hnsw {
		entrypoint = greedy_upper_levels(index, query, entrypoint, distance_function)?;
	}
	let candidate_nodes = bounded_level_zero(index, query, entrypoint, breadth, distance_function)?;
	let mut hits = rerank(index, query, candidate_nodes.iter().copied(), distance_function)?;
	if hits.len() < k && candidate_nodes.len() < index.nodes.len() {
		// A disconnected/corrupt-by-construction candidate frontier must never
		// produce a false partial top-K. The exact fallback is deliberate.
		hits = rerank(index, query, 0..index.nodes.len(), distance_function)?;
	}
	hits.sort_unstable_by(|left, right| {
		left.distance.total_cmp(&right.distance).then_with(|| left.atom.cmp(&right.atom))
	});
	hits.dedup_by_key(|hit| hit.atom);
	hits.truncate(k);
	Ok(hits)
}

/// Merges an exact delta while using the same custom-distance evaluator as
/// the immutable base graph.
pub(crate) fn search_vector_index_with_delta_and_distance(
	index: &DecodedVectorIndex,
	delta: &DecodedVectorDelta,
	query: &Vector,
	k: usize,
	breadth: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
	if delta.spec.base != index.spec {
		return Err(invalid("vector delta definition disagrees with its base"));
	}
	let shadowed = delta.records.iter().map(VectorDeltaRecord::atom).collect::<BTreeSet<_>>();
	let requested = k.saturating_add(shadowed.len()).max(k);
	let mut hits = search_vector_index_with_distance(
		index,
		query,
		requested,
		breadth.max(requested),
		distance_function,
	)?
	.into_iter()
	.filter(|hit| !shadowed.contains(&hit.atom))
	.collect::<Vec<_>>();
	for record in &delta.records {
		if let VectorDeltaRecord::Upsert {
			atom,
			vector,
		} = record
		{
			hits.push(VectorSearchHit {
				atom: *atom,
				distance: search_distance(index, query, vector, distance_function)?,
			});
		}
	}
	if hits.len() < k {
		hits = rerank(index, query, 0..index.nodes.len(), distance_function)?
			.into_iter()
			.filter(|hit| !shadowed.contains(&hit.atom))
			.collect();
		for record in &delta.records {
			if let VectorDeltaRecord::Upsert {
				atom,
				vector,
			} = record
			{
				hits.push(VectorSearchHit {
					atom: *atom,
					distance: search_distance(index, query, vector, distance_function)?,
				});
			}
		}
	}
	hits.sort_unstable_by(|left, right| {
		left.distance.total_cmp(&right.distance).then_with(|| left.atom.cmp(&right.atom))
	});
	hits.dedup_by_key(|hit| hit.atom);
	hits.truncate(k);
	Ok(hits)
}

/// Searches DiskANN through an ordinal loader without materializing the base
/// generation. The loader owns page-windowing and snapshot validation.
pub(crate) fn search_paged_diskann(
	header: &PagedVectorHeader,
	delta: Option<&DecodedVectorDelta>,
	query: &Vector,
	k: usize,
	breadth: usize,
	mut load: impl FnMut(&[u32], usize) -> FerrosGrynnResult<Vec<PagedVectorNode>>,
) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
	if header.spec.algorithm()? != VectorAlgorithm::DiskAnn {
		return Err(invalid("paged DiskANN search received another vector algorithm"));
	}
	if k == 0 || breadth < k {
		return Err(FerrosGrynnError::invalid_argument(
			"ANN search requires K > 0 and breadth >= K",
		));
	}
	validate_vector(header.spec, query)?;
	if let Some(delta) = delta {
		if delta.spec.base != header.spec {
			return Err(invalid("vector delta definition disagrees with its base"));
		}
	}
	let shadowed = delta
		.into_iter()
		.flat_map(|delta| delta.records.iter())
		.map(VectorDeltaRecord::atom)
		.collect::<BTreeSet<_>>();
	let posting_limit = k.checked_add(shadowed.len()).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("ANN posting prefix exceeds platform limits")
	})?;
	let requested = breadth.max(posting_limit);
	let candidates = paged_level_zero(header, query, requested, &mut load)?;
	let mut top = TopHits::new(k);
	append_paged_hits(
		header,
		query,
		candidates.iter().copied(),
		&shadowed,
		posting_limit,
		&mut top,
		&mut load,
	)?;
	append_delta_hits(header, delta, query, &mut top)?;
	let mut hits = top.into_sorted();
	if hits.len() < k
		&& candidates.len()
			< usize::try_from(header.node_count)
				.map_err(|_error| invalid("vector node count exceeds usize"))?
	{
		let mut exact = TopHits::new(k);
		append_paged_hits(
			header,
			query,
			0..header.node_count,
			&shadowed,
			posting_limit,
			&mut exact,
			&mut load,
		)?;
		append_delta_hits(header, delta, query, &mut exact)?;
		hits = exact.into_sorted();
	}
	Ok(hits)
}

fn paged_level_zero(
	header: &PagedVectorHeader,
	query: &Vector,
	breadth: usize,
	load: &mut impl FnMut(&[u32], usize) -> FerrosGrynnResult<Vec<PagedVectorNode>>,
) -> FerrosGrynnResult<BTreeSet<u32>> {
	let Some(entrypoint) = header.entrypoint else {
		return Ok(BTreeSet::new());
	};
	let entry = load_paged_node(load, entrypoint, 1)?;
	validate_paged_node(header, entrypoint, &entry)?;
	let mut frontier = BinaryHeap::new();
	let initial = Candidate {
		distance: exact_vector_distance(query, &entry.node.vector, header.spec.distance()?)?,
		ordinal: entrypoint,
	};
	frontier.push(Reverse(initial));
	let mut nearest = BinaryHeap::from([initial]);
	let mut seen = BTreeSet::from([entrypoint]);
	while let Some(Reverse(candidate)) = frontier.pop() {
		if nearest.len() >= breadth && nearest.peek().is_some_and(|worst| candidate > *worst) {
			break;
		}
		let source = load_paged_node(load, candidate.ordinal, 1)?;
		validate_paged_node(header, candidate.ordinal, &source)?;
		let neighbors = source.node.neighbors[0]
			.iter()
			.copied()
			.filter(|neighbor| seen.insert(*neighbor))
			.collect::<Vec<_>>();
		for ordinals in neighbors.chunks(PAGED_NODE_BATCH) {
			let nodes = load(ordinals, 1)?;
			if nodes.len() != ordinals.len() {
				return Err(invalid("paged DiskANN loader returned the wrong node count"));
			}
			for (neighbor, node) in ordinals.iter().copied().zip(nodes) {
				validate_paged_node(header, neighbor, &node)?;
				let candidate = Candidate {
					distance: exact_vector_distance(
						query,
						&node.node.vector,
						header.spec.distance()?,
					)?,
					ordinal: neighbor,
				};
				if nearest.len() < breadth || nearest.peek().is_some_and(|worst| candidate < *worst)
				{
					frontier.push(Reverse(candidate));
					nearest.push(candidate);
					if nearest.len() > breadth {
						let _ = nearest.pop();
					}
				}
			}
		}
	}
	Ok(nearest.into_iter().map(|candidate| candidate.ordinal).collect())
}

fn append_paged_hits(
	header: &PagedVectorHeader,
	query: &Vector,
	ordinals: impl IntoIterator<Item = u32>,
	shadowed: &BTreeSet<DiskAtomRef>,
	posting_limit: usize,
	top: &mut TopHits,
	load: &mut impl FnMut(&[u32], usize) -> FerrosGrynnResult<Vec<PagedVectorNode>>,
) -> FerrosGrynnResult<()> {
	let mut batch = Vec::with_capacity(PAGED_NODE_BATCH);
	for ordinal in ordinals {
		batch.push(ordinal);
		if batch.len() == PAGED_NODE_BATCH {
			append_paged_batch(header, query, &batch, shadowed, posting_limit, top, load)?;
			batch.clear();
		}
	}
	if !batch.is_empty() {
		append_paged_batch(header, query, &batch, shadowed, posting_limit, top, load)?;
	}
	Ok(())
}

fn append_paged_batch(
	header: &PagedVectorHeader,
	query: &Vector,
	ordinals: &[u32],
	shadowed: &BTreeSet<DiskAtomRef>,
	posting_limit: usize,
	top: &mut TopHits,
	load: &mut impl FnMut(&[u32], usize) -> FerrosGrynnResult<Vec<PagedVectorNode>>,
) -> FerrosGrynnResult<()> {
	let nodes = load(ordinals, posting_limit)?;
	if nodes.len() != ordinals.len() {
		return Err(invalid("paged DiskANN loader returned the wrong node count"));
	}
	for (ordinal, node) in ordinals.iter().copied().zip(nodes) {
		validate_paged_node(header, ordinal, &node)?;
		let distance = exact_vector_distance(query, &node.node.vector, header.spec.distance()?)?;
		for atom in node.node.postings {
			if !shadowed.contains(&atom) {
				top.push(VectorSearchHit {
					atom,
					distance,
				});
			}
		}
	}
	Ok(())
}

fn load_paged_node(
	load: &mut impl FnMut(&[u32], usize) -> FerrosGrynnResult<Vec<PagedVectorNode>>,
	ordinal: u32,
	posting_limit: usize,
) -> FerrosGrynnResult<PagedVectorNode> {
	let mut nodes = load(std::slice::from_ref(&ordinal), posting_limit)?;
	if nodes.len() != 1 {
		return Err(invalid("paged DiskANN loader returned the wrong node count"));
	}
	Ok(nodes.remove(0))
}

fn append_delta_hits(
	header: &PagedVectorHeader,
	delta: Option<&DecodedVectorDelta>,
	query: &Vector,
	top: &mut TopHits,
) -> FerrosGrynnResult<()> {
	for record in delta.into_iter().flat_map(|delta| delta.records.iter()) {
		if let VectorDeltaRecord::Upsert {
			atom,
			vector,
		} = record
		{
			top.push(VectorSearchHit {
				atom: *atom,
				distance: exact_vector_distance(query, vector, header.spec.distance()?)?,
			});
		}
	}
	Ok(())
}

fn validate_paged_node(
	header: &PagedVectorHeader,
	ordinal: u32,
	loaded: &PagedVectorNode,
) -> FerrosGrynnResult<()> {
	let node = &loaded.node;
	if ordinal >= header.node_count
		|| node.neighbors.len() != 1
		|| node.postings.is_empty()
		|| node.postings.len() > loaded.posting_count
	{
		return Err(invalid("paged DiskANN node metadata is inconsistent"));
	}
	let maximum = match header.spec.options {
		crate::datastore::ClassIndexOptions::DiskAnn(options) => {
			usize::from(options.degree().get())
		}
		_ => return Err(invalid("paged DiskANN header has non-DiskANN options")),
	};
	if node.neighbors[0].len() > maximum {
		return Err(invalid("paged DiskANN adjacency exceeds its configured degree"));
	}
	let mut unique = BTreeSet::new();
	if node.neighbors[0].iter().any(|neighbor| {
		*neighbor >= header.node_count || *neighbor == ordinal || !unique.insert(*neighbor)
	}) {
		return Err(invalid("paged DiskANN adjacency contains an invalid ordinal"));
	}
	Ok(())
}

fn greedy_upper_levels(
	index: &DecodedVectorIndex,
	query: &Vector,
	mut current: u32,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<u32> {
	let top = index.nodes
		[usize::try_from(current).map_err(|_error| invalid("ordinal exceeds usize"))?]
	.neighbors
	.len();
	for level in (1..top).rev() {
		loop {
			let current_index = usize::try_from(current)
				.map_err(|_error| invalid("vector ordinal exceeds usize"))?;
			let mut best = Candidate {
				distance: search_distance(
					index,
					query,
					&index.nodes[current_index].vector,
					distance_function,
				)?,
				ordinal: current,
			};
			for neighbor in &index.nodes[current_index].neighbors[level] {
				let neighbor_index = usize::try_from(*neighbor)
					.map_err(|_error| invalid("vector neighbor exceeds usize"))?;
				let candidate = Candidate {
					distance: search_distance(
						index,
						query,
						&index.nodes[neighbor_index].vector,
						distance_function,
					)?,
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
	index: &DecodedVectorIndex,
	query: &Vector,
	entrypoint: u32,
	breadth: usize,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<BTreeSet<usize>> {
	let entry =
		usize::try_from(entrypoint).map_err(|_error| invalid("vector entrypoint exceeds usize"))?;
	let initial = Candidate {
		distance: search_distance(index, query, &index.nodes[entry].vector, distance_function)?,
		ordinal: entrypoint,
	};
	let mut frontier = BinaryHeap::from([Reverse(initial)]);
	let mut nearest = BinaryHeap::from([initial]);
	let mut seen = BTreeSet::from([entry]);
	while let Some(Reverse(candidate)) = frontier.pop() {
		if nearest.len() >= breadth && nearest.peek().is_some_and(|worst| candidate > *worst) {
			break;
		}
		let source = usize::try_from(candidate.ordinal)
			.map_err(|_error| invalid("vector ordinal exceeds usize"))?;
		for neighbor in &index.nodes[source].neighbors[0] {
			let neighbor_ordinal = *neighbor;
			let neighbor = usize::try_from(neighbor_ordinal)
				.map_err(|_error| invalid("vector neighbor exceeds usize"))?;
			if !seen.insert(neighbor) {
				continue;
			}
			let candidate = Candidate {
				distance: search_distance(
					index,
					query,
					&index.nodes[neighbor].vector,
					distance_function,
				)?,
				ordinal: neighbor_ordinal,
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
	Ok(nearest
		.into_iter()
		.map(|candidate| {
			usize::try_from(candidate.ordinal)
				.map_err(|_error| invalid("vector candidate ordinal exceeds usize"))
		})
		.collect::<FerrosGrynnResult<_>>()?)
}

fn rerank(
	index: &DecodedVectorIndex,
	query: &Vector,
	ordinals: impl Iterator<Item = usize>,
	distance_function: &mut dyn FnMut(
		crate::datastore::FunctionId,
		crate::datastore::SchemaVersion,
		&Vector,
		&Vector,
	) -> FerrosGrynnResult<f64>,
) -> FerrosGrynnResult<Vec<VectorSearchHit>> {
	let mut hits = Vec::new();
	for ordinal in ordinals {
		let node = index
			.nodes
			.get(ordinal)
			.ok_or_else(|| invalid("vector candidate ordinal is absent"))?;
		let distance = search_distance(index, query, &node.vector, distance_function)?;
		for atom in &node.postings {
			hits.push(VectorSearchHit {
				atom: *atom,
				distance,
			});
		}
	}
	Ok(hits)
}

fn search_distance(
	index: &DecodedVectorIndex,
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
		distance => exact_vector_distance(left, right, distance),
	}
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::{cell::Cell, collections::BTreeSet, num::NonZeroU16};

	use super::{bounded_level_zero, search_paged_diskann};
	use crate::{
		datastore::{
			ClassId,
			ClassIndexOptions,
			DiskAnnIndexOptions,
			HnswIndexOptions,
			IndexId,
			IndexScope,
			SchemaVersion,
			VectorDistance,
		},
		graph::AtomKind,
		storage::vector_index::{
			PagedVectorHeader,
			PagedVectorNode,
			StreamLocation,
			VectorBuildSpec,
			VectorNode,
		},
		values::{PropertyKeyId, Vector, VectorKind, storage::DiskAtomRef},
	};

	#[test]
	fn hnsw_query_keeps_nearest_candidate_discovered_at_breadth_boundary() {
		let options = HnswIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(1).unwrap(),
			VectorDistance::Euclidean,
		)
		.unwrap();
		let index = crate::storage::vector_index::DecodedVectorIndex {
			spec: VectorBuildSpec {
				index: IndexId::new(7).unwrap(),
				index_version: SchemaVersion::INITIAL,
				scope: IndexScope::Base,
				class: ClassId::new(3).unwrap(),
				field: PropertyKeyId::new(2),
				options: ClassIndexOptions::Hnsw(options),
				build_revision: 11,
				build_seed: 19,
			},
			entrypoint: Some(0),
			nodes: vec![
				VectorNode {
					postings: vec![DiskAtomRef::from_parts(AtomKind::Node, 1).unwrap()],
					vector: Vector::from(vec![10.0_f32]),
					vector_hash: 0,
					neighbors: vec![vec![1]],
				},
				VectorNode {
					postings: vec![DiskAtomRef::from_parts(AtomKind::Node, 2).unwrap()],
					vector: Vector::from(vec![5.0_f32]),
					vector_hash: 0,
					neighbors: vec![vec![2]],
				},
				VectorNode {
					postings: vec![DiskAtomRef::from_parts(AtomKind::Node, 3).unwrap()],
					vector: Vector::from(vec![0.0_f32]),
					vector_hash: 0,
					neighbors: vec![Vec::new()],
				},
			],
			fingerprint: 0,
			provider_fingerprint: 0,
		};
		let mut unavailable = |_function, _version, _left: &Vector, _right: &Vector| {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"built-in distance unexpectedly invoked a UDF",
			))
		};

		let actual =
			bounded_level_zero(&index, &Vector::from(vec![0.0_f32]), 0, 2, &mut unavailable)
				.unwrap();

		assert_eq!(actual, BTreeSet::from([1, 2]));
	}

	#[test]
	fn hashed_diskann_requests_only_the_top_k_posting_prefix() {
		let options = DiskAnnIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(1).unwrap(),
			VectorDistance::Euclidean,
			true,
		)
		.unwrap();
		let spec = VectorBuildSpec {
			index: IndexId::new(7).unwrap(),
			index_version: SchemaVersion::INITIAL,
			scope: IndexScope::Base,
			class: ClassId::new(3).unwrap(),
			field: PropertyKeyId::new(2),
			options: ClassIndexOptions::DiskAnn(options),
			build_revision: 11,
			build_seed: 19,
		};
		let header = PagedVectorHeader {
			spec,
			entrypoint: Some(0),
			node_count: 1,
			directory: StreamLocation {
				first: 1,
				pages: 1,
			},
			stream: StreamLocation {
				first: 2,
				pages: 8,
			},
			fingerprint: 0,
			provider_fingerprint: 0,
			rebuild_threshold: 10,
			posting_count: 10_000,
		};
		let maximum_requested = Cell::new(0_usize);
		let results = search_paged_diskann(
			&header,
			None,
			&Vector::from(vec![0.0_f32]),
			3,
			3,
			|ordinals, posting_limit| {
				assert!(ordinals.iter().all(|ordinal| *ordinal == 0));
				maximum_requested.set(maximum_requested.get().max(posting_limit));
				Ok(ordinals
					.iter()
					.map(|_ordinal| PagedVectorNode {
						node: VectorNode {
							postings: (1..=posting_limit)
								.map(|slot| {
									DiskAtomRef::from_parts(
										AtomKind::Node,
										u64::try_from(slot).unwrap(),
									)
									.unwrap()
								})
								.collect(),
							vector: Vector::from(vec![0.0_f32]),
							vector_hash: 0,
							neighbors: vec![Vec::new()],
						},
						posting_count: 10_000,
					})
					.collect())
			},
		)
		.unwrap();

		assert_eq!(maximum_requested.get(), 3);
		assert_eq!(results.len(), 3);
		assert_eq!(results[0].atom.slot(), 1);
		assert_eq!(results[2].atom.slot(), 3);
	}

	#[test]
	fn paged_diskann_keeps_nearest_candidate_discovered_at_breadth_boundary() {
		let options = DiskAnnIndexOptions::defaults(
			VectorKind::F32,
			NonZeroU16::new(1).unwrap(),
			VectorDistance::Euclidean,
			false,
		)
		.unwrap();
		let header = PagedVectorHeader {
			spec: VectorBuildSpec {
				index: IndexId::new(7).unwrap(),
				index_version: SchemaVersion::INITIAL,
				scope: IndexScope::Base,
				class: ClassId::new(3).unwrap(),
				field: PropertyKeyId::new(2),
				options: ClassIndexOptions::DiskAnn(options),
				build_revision: 11,
				build_seed: 19,
			},
			entrypoint: Some(0),
			node_count: 3,
			directory: StreamLocation {
				first: 1,
				pages: 1,
			},
			stream: StreamLocation {
				first: 2,
				pages: 1,
			},
			fingerprint: 0,
			provider_fingerprint: 0,
			rebuild_threshold: 1,
			posting_count: 3,
		};
		let nodes = [(1_u64, 10.0_f32, vec![1]), (2, 5.0, vec![2]), (3, 0.0, Vec::new())];
		let maximum_batch = Cell::new(0_usize);
		let results = search_paged_diskann(
			&header,
			None,
			&Vector::from(vec![0.0_f32]),
			2,
			2,
			|ordinals, _posting_limit| {
				maximum_batch.set(maximum_batch.get().max(ordinals.len()));
				Ok(ordinals
					.iter()
					.map(|ordinal| {
						let (slot, value, neighbors) = &nodes[usize::try_from(*ordinal).unwrap()];
						PagedVectorNode {
							node: VectorNode {
								postings: vec![
									DiskAtomRef::from_parts(AtomKind::Node, *slot).unwrap(),
								],
								vector: Vector::from(vec![*value]),
								vector_hash: 0,
								neighbors: vec![neighbors.clone()],
							},
							posting_count: 1,
						}
					})
					.collect())
			},
		)
		.unwrap();

		assert!(maximum_batch.get() > 1);
		assert_eq!(results.iter().map(|hit| hit.atom.slot()).collect::<Vec<_>>(), [3, 2]);
	}
}
