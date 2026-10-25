//! Bounded exact sets, mergeable sketches and exact landmark refinement.

mod artifact;
pub use artifact::NeighbourhoodArtifact;

use super::NeighbourhoodAlgorithm;
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::ProjectedRelation,
	math::HyperLogLog,
};

/// Separate physical artifact and live builder workspace ceilings.
#[derive(Clone, Copy)]
pub(super) struct NeighbourhoodBuildLimits {
	/// Complete pages including common descriptor/relation metadata.
	pub(super) max_bytes: u64,
	/// Header, maps, vertex references, CSR and descriptor pages.
	pub(super) common_pages: u64,
	/// Effective maintenance scratch ceiling.
	pub(super) scratch_bytes: usize,
	/// Conservative live source relation admission.
	pub(super) live_scratch_bytes: usize,
}

/// Canonical distinct sets, indexed by center then radius minus one.
#[derive(Debug)]
pub struct NeighbourhoodLists {
	/// Sorted selected center ordinals; EXACT contains every vertex.
	pub centers: Vec<usize>,
	/// Exclusive cumulative offsets including the final member count.
	pub offsets: Vec<usize>,
	/// Sorted distinct member ordinals in each row, including its seed.
	pub members: Vec<usize>,
}

/// Algorithm payload; readiness remains in the graph catalog.
#[derive(Debug)]
pub enum NeighbourhoodLabels {
	/// Exact sets for all vertices and radii.
	Exact(
		/// Exact sets for all vertices and radii.
		NeighbourhoodLists,
	),
	/// Radius-major, then vertex-major sketches with the same ordinal hash
	/// domain.
	Sketch(
		/// Radius-major, then vertex-major sketches with the same ordinal hash
		/// domain.
		Vec<HyperLogLog>,
	),
	/// Exact sets for deterministic landmarks, completed by bounded traversal.
	Landmark(
		/// Exact sets for deterministic landmarks, completed by bounded
		/// traversal.
		NeighbourhoodLists,
	),
	/// Adjacency, a large radius or AUTO rejected materialization.
	OnDemand,
}

/// Builds a single bounded neighbourhood policy without publishing partial
/// rows.
///
/// # Errors
/// Rejects invalid radii/parameters, cancellation and explicit resource
/// overflow. AUTO falls back only on artifact budget/degree policy, never on
/// scratch failure.
pub(super) fn build_neighbourhood_labels<Check>(
	relation: &ProjectedRelation,
	algorithm: NeighbourhoodAlgorithm,
	radius: u32,
	limits: NeighbourhoodBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<NeighbourhoodLabels>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	check()?;
	if radius == 0
		|| radius > 4
			&& !matches!(algorithm, NeighbourhoodAlgorithm::Auto | NeighbourhoodAlgorithm::OnDemand)
	{
		return Err(invalid("materialized neighbourhood radius must be in 1..=4"));
	}
	if matches!(algorithm, NeighbourhoodAlgorithm::Sketch(precision) if !(4..=18).contains(&precision))
		|| matches!(algorithm, NeighbourhoodAlgorithm::Landmark(0 | 257..))
	{
		return Err(invalid("neighbourhood algorithm parameter is invalid"));
	}
	if radius == 1 || radius > 4 || algorithm == NeighbourhoodAlgorithm::OnDemand {
		return Ok(NeighbourhoodLabels::OnDemand);
	}
	let count = relation.vertices.len();
	let auto = algorithm == NeighbourhoodAlgorithm::Auto;
	if auto
		&& relation.forward.targets.len()
			> count.saturating_mul(if radius == 2 {
				10
			} else {
				5
			}) {
		return Ok(NeighbourhoodLabels::OnDemand);
	}
	match algorithm {
		NeighbourhoodAlgorithm::Sketch(precision) => {
			build_sketches(relation, precision, radius, limits, check)
				.map(NeighbourhoodLabels::Sketch)
		}
		NeighbourhoodAlgorithm::Exact
		| NeighbourhoodAlgorithm::Auto
		| NeighbourhoodAlgorithm::Landmark(_) => build_lists(relation, algorithm, radius, limits, check),
		NeighbourhoodAlgorithm::OnDemand => Ok(NeighbourhoodLabels::OnDemand),
	}
}

/// Propagates identical seed-domain sketches one hop per round after complete
/// admission.
fn build_sketches<Check>(
	relation: &ProjectedRelation,
	precision: u8,
	radius: u32,
	limits: NeighbourhoodBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<HyperLogLog>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let count = relation.vertices.len();
	let registers = 1_usize << precision;
	let rows = count.checked_mul(radius as usize).ok_or_else(|| resource(limits))?;
	let bytes = rows.checked_mul(registers).ok_or_else(|| resource(limits))?;
	admit_artifact(limits, &[(bytes, 1)])?;
	let live = count
		.checked_mul(radius as usize + 1)
		.and_then(|rows| rows.checked_mul(registers + 64))
		.ok_or_else(|| resource(limits))?;
	admit_scratch(limits, live)?;
	let mut seeds = reserve(count, limits)?;
	for vertex in 0..count {
		check()?;
		let mut sketch = HyperLogLog::new(precision)?;
		sketch.insert(&(vertex as u64).to_le_bytes());
		seeds.push(sketch);
	}
	let mut sketches = reserve(rows, limits)?;
	for depth in 0..radius as usize {
		let previous = if depth == 0 {
			&seeds[..]
		} else {
			&sketches[(depth - 1) * count..depth * count]
		};
		let mut current = reserve(count, limits)?;
		for source in 0..count {
			check()?;
			let mut sketch = HyperLogLog::new(precision)?;
			sketch.merge(&previous[source])?;
			for target in relation.forward.neighbours(source) {
				check()?;
				sketch.merge(&previous[target])?;
			}
			current.push(sketch);
		}
		sketches.extend(current);
	}
	Ok(sketches)
}

/// Packs canonical distinct sets for all centers or selected landmarks.
fn build_lists<Check>(
	relation: &ProjectedRelation,
	algorithm: NeighbourhoodAlgorithm,
	radius: u32,
	limits: NeighbourhoodBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<NeighbourhoodLabels>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let count = relation.vertices.len();
	let auto = algorithm == NeighbourhoodAlgorithm::Auto;
	let centers_count = match algorithm {
		NeighbourhoodAlgorithm::Landmark(0 | 257..) => {
			return Err(invalid("neighbourhood landmark count must be in 1..=256"));
		}
		NeighbourhoodAlgorithm::Landmark(count) => usize::from(count).min(relation.vertices.len()),
		_ => count,
	};
	let rows = centers_count.checked_mul(radius as usize).ok_or_else(|| resource(limits))?;
	let offsets_count = rows.checked_add(1).ok_or_else(|| resource(limits))?;
	if !fits(limits, &[(centers_count, 8), (offsets_count, 8), (0, 8)])? {
		return if auto {
			Ok(NeighbourhoodLabels::OnDemand)
		} else {
			Err(resource(limits))
		};
	}
	let workspace = count
		.checked_mul(32)
		.and_then(|bytes| rows.checked_mul(16).and_then(|rows| bytes.checked_add(rows)))
		.and_then(|bytes| bytes.checked_add(1024))
		.ok_or_else(|| resource(limits))?;
	admit_scratch(limits, workspace)?;
	let mut centers = reserve(count, limits)?;
	centers.extend(0..count);
	if centers_count != count {
		centers.sort_unstable_by_key(|vertex| {
			(
				std::cmp::Reverse(
					relation.forward.offsets[*vertex + 1] - relation.forward.offsets[*vertex]
						+ relation.reverse.offsets[*vertex + 1]
						- relation.reverse.offsets[*vertex],
				),
				*vertex,
			)
		});
		centers.truncate(centers_count);
		centers.sort_unstable();
	}
	let mut offsets = reserve(offsets_count, limits)?;
	offsets.push(0);
	let mut members = Vec::new();
	for center in &centers {
		let distance = distances(relation, *center, radius, limits, check)?;
		let added = distance.iter().try_fold(0_usize, |sum, depth| {
			sum.checked_add(if *depth <= radius {
				(radius - (*depth).max(1) + 1) as usize
			} else {
				0
			})
			.ok_or_else(|| resource(limits))
		})?;
		let next = members.len().checked_add(added).ok_or_else(|| resource(limits))?;
		if !fits(limits, &[(centers_count, 8), (offsets_count, 8), (next, 8)])? {
			return if auto {
				Ok(NeighbourhoodLabels::OnDemand)
			} else {
				Err(resource(limits))
			};
		}
		if next > members.capacity() {
			let capacity = next.max(members.capacity().saturating_mul(2));
			let peak = capacity
				.checked_add(members.capacity())
				.and_then(|words| words.checked_mul(8))
				.and_then(|bytes| bytes.checked_add(workspace))
				.ok_or_else(|| resource(limits))?;
			admit_scratch(limits, peak)?;
			members
				.try_reserve_exact(capacity - members.len())
				.map_err(|cause| resource(limits).with_technical_source(cause))?;
		}
		for depth in 1..=radius {
			check()?;
			members.extend(
				distance
					.iter()
					.enumerate()
					.filter_map(|(vertex, distance)| (*distance <= depth).then_some(vertex)),
			);
			offsets.push(members.len());
		}
	}
	let lists = NeighbourhoodLists {
		centers,
		offsets,
		members,
	};
	Ok(if matches!(algorithm, NeighbourhoodAlgorithm::Landmark(_)) {
		NeighbourhoodLabels::Landmark(lists)
	} else {
		NeighbourhoodLabels::Exact(lists)
	})
}

impl NeighbourhoodLists {
	/// Returns the exact cached set for a compatible center/radius.
	pub(super) fn row(&self, center: usize, radius: u32, max_radius: u32) -> Option<&[usize]> {
		if radius == 0 || radius > max_radius {
			return None;
		}
		let center = self.centers.binary_search(&center).ok()?;
		let row = center.checked_mul(max_radius as usize)?.checked_add(radius as usize - 1)?;
		let start = *self.offsets.get(row)?;
		let end = *self.offsets.get(row.checked_add(1)?)?;
		self.members.get(start..end)
	}
}

/// Traverses exact members, optionally completing landmark caches using the
/// remaining radius, without letting cached
/// membership suppress a later shorter ordinary route through the same member.
///
/// # Errors
/// Rejects source/radius, cancellation, scratch or work overflow without
/// partial output.
pub(super) fn neighbourhood_members<Check>(
	relation: &ProjectedRelation,
	lists: Option<&NeighbourhoodLists>,
	source: usize,
	radius: u32,
	max_radius: u32,
	max_steps: u64,
	limits: NeighbourhoodBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<usize>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	if source >= relation.vertices.len() {
		return Err(invalid("neighbourhood source is absent"));
	}
	admit_scratch(
		limits,
		relation.vertices.len().checked_mul(32).ok_or_else(|| resource(limits))?,
	)?;
	let mut distance = reserve(relation.vertices.len(), limits)?;
	distance.resize(relation.vertices.len(), u32::MAX);
	let mut included = reserve(relation.vertices.len(), limits)?;
	included.resize(relation.vertices.len(), false);
	let mut queue = reserve(relation.vertices.len(), limits)?;
	queue.push(source);
	distance[source] = 0;
	included[source] = true;
	let mut steps = 0_u64;
	let mut cursor = 0;
	while cursor < queue.len() {
		check()?;
		let vertex = queue[cursor];
		cursor += 1;
		let depth = distance[vertex];
		if depth == radius {
			continue;
		}
		if let Some(cached) = lists.and_then(|lists| lists.row(vertex, radius - depth, max_radius))
		{
			for member in cached {
				work_step(&mut steps, max_steps, check)?;
				let target = included
					.get_mut(*member)
					.ok_or_else(|| invalid("cached neighbourhood member is out of bounds"))?;
				*target = true;
			}
		} else {
			for target in relation.forward.neighbours(vertex) {
				work_step(&mut steps, max_steps, check)?;
				included[target] = true;
				if distance[target] == u32::MAX {
					distance[target] = depth + 1;
					queue.push(target);
				}
			}
		}
	}
	let mut members = reserve(queue.capacity(), limits)?;
	members.extend(
		included
			.into_iter()
			.enumerate()
			.filter_map(|(vertex, included)| included.then_some(vertex)),
	);
	Ok(members)
}

/// Computes shortest hop depths through the already checked local relation.
fn distances<Check>(
	relation: &ProjectedRelation,
	source: usize,
	radius: u32,
	limits: NeighbourhoodBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<u32>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let mut distance = reserve(relation.vertices.len(), limits)?;
	distance.resize(relation.vertices.len(), u32::MAX);
	let mut queue = reserve(relation.vertices.len(), limits)?;
	distance[source] = 0;
	queue.push(source);
	let mut cursor = 0;
	while cursor < queue.len() {
		check()?;
		let source = queue[cursor];
		cursor += 1;
		if distance[source] == radius {
			continue;
		}
		for target in relation.forward.neighbours(source) {
			check()?;
			if distance[target] == u32::MAX {
				distance[target] = distance[source] + 1;
				queue.push(target);
			}
		}
	}
	Ok(distance)
}

/// Accounts real packed section pages, not only member payload bytes.
fn fits(limits: NeighbourhoodBuildLimits, sections: &[(usize, u64)]) -> FerrosGrynnResult<bool> {
	let pages = sections.iter().try_fold(limits.common_pages, |pages, (count, width)| {
		(*count as u64)
			.checked_mul(*width)
			.and_then(|bytes| pages.checked_add(bytes.div_ceil(16_320)))
			.ok_or_else(|| resource(limits))
	})?;
	Ok(pages.checked_mul(16_384).is_some_and(|bytes| bytes <= limits.max_bytes))
}

/// Rejects an explicit physical artifact overflow before its row allocation.
fn admit_artifact(
	limits: NeighbourhoodBuildLimits,
	sections: &[(usize, u64)],
) -> FerrosGrynnResult<()> {
	if fits(limits, sections)? {
		Ok(())
	} else {
		Err(resource(limits))
	}
}

/// Checks live aggregate workspace before growth.
fn admit_scratch(limits: NeighbourhoodBuildLimits, bytes: usize) -> FerrosGrynnResult<()> {
	if limits.live_scratch_bytes.checked_add(bytes).is_none_or(|bytes| bytes > limits.scratch_bytes)
	{
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"neighbourhood scratch bytes",
			limits.scratch_bytes as u64,
		));
	}
	Ok(())
}

/// Reserves only after the caller's aggregate admission.
fn reserve<T>(count: usize, limits: NeighbourhoodBuildLimits) -> FerrosGrynnResult<Vec<T>> {
	let mut values = Vec::new();
	values
		.try_reserve_exact(count)
		.map_err(|cause| resource(limits).with_technical_source(cause))?;
	Ok(values)
}

/// Enforces a combined traversal/cache-member work ceiling.
fn work_step<Check>(steps: &mut u64, max_steps: u64, check: &mut Check) -> FerrosGrynnResult<()>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	check()?;
	if *steps == max_steps {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"neighbourhood refinement steps",
			max_steps,
		));
	}
	*steps += 1;
	Ok(())
}

/// Typed artifact capacity failure.
fn resource(limits: NeighbourhoodBuildLimits) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(
		"neighbourhood artifact bytes",
		limits.max_bytes,
	)
}

/// Typed domain or checked-reference failure.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(reason)
}

#[cfg(test)]
mod tests;
