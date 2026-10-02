//! Bounded directed landmark distances and conservative ALT lower bounds.

use std::{cmp::Ordering, collections::BinaryHeap};

use num_traits::ToPrimitive;

use super::relation::{ProjectedRelation, RelationCsr};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	math::number_is_finite,
	values::Number,
};

/// A nonnegative enclosure; `[infinity,infinity]` denotes unreachable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceInterval {
	/// Lower bound rounded toward negative infinity.
	pub lower: f64,
	/// Upper bound rounded toward positive infinity.
	pub upper: f64,
}

impl DistanceInterval {
	/// Unreachable distances participate in no ALT subtraction term.
	pub const INFINITY: Self = Self {
		lower: f64::INFINITY,
		upper: f64::INFINITY,
	};
	/// The empty path has exact zero cost.
	pub const ZERO: Self = Self {
		lower: 0.0,
		upper: 0.0,
	};

	/// Checks an interval decoded from an artifact.
	///
	/// # Errors
	/// Rejects NaN, negative, reversed or noncanonical signed-zero bounds.
	pub fn new(lower: f64, upper: f64) -> FerrosGrynnResult<Self> {
		if lower.is_nan()
			|| upper.is_nan()
			|| lower.is_sign_negative()
			|| upper.is_sign_negative()
			|| lower > upper
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"landmark distance interval is invalid",
			));
		}
		Ok(Self {
			lower,
			upper,
		})
	}

	/// Encloses the exact runtime number without discarding integer/decimal
	/// bits.
	///
	/// # Errors
	/// Rejects negative/non-finite costs or a failed enclosing conversion.
	pub fn from_number(number: Number) -> FerrosGrynnResult<Self> {
		if !number_is_finite(number) || number < Number::Int(0) {
			return Err(FerrosGrynnError::invalid_argument(
				"landmark costs must be finite and nonnegative",
			));
		}
		let candidate = match number {
			Number::Int(value) => value as f64,
			Number::Float(value) => value,
			Number::Decimal(value) => value.to_f64().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("landmark decimal conversion failed")
			})?,
		};
		let (lower, upper) = match Number::Float(candidate).cmp(&number) {
			Ordering::Less => (candidate, candidate.next_up()),
			Ordering::Equal => (candidate, candidate),
			Ordering::Greater => (candidate.next_down(), candidate),
		};
		if Number::Float(lower) > number || Number::Float(upper) < number {
			return Err(FerrosGrynnError::invalid_argument(
				"landmark numeric conversion does not enclose its source",
			));
		}
		Self::new(lower.max(0.0), upper.max(0.0))
	}

	/// Adds nonnegative intervals, preserving monotonic relaxation even when a
	/// tiny positive edge rounds to the same float as its prefix.
	pub fn add(self, edge: Self) -> Self {
		if self.lower.is_infinite() || edge.lower.is_infinite() {
			return Self::INFINITY;
		}
		Self {
			lower: if edge.lower == 0.0 {
				self.lower
			} else {
				(self.lower + edge.lower).next_down().max(self.lower)
			},
			upper: if edge.upper == 0.0 {
				self.upper
			} else {
				(self.upper + edge.upper).next_up()
			},
		}
	}

	/// Uses only finite lower-minus-upper terms and rounds the subtraction
	/// down.
	pub fn lower_difference(self, subtrahend: Self) -> f64 {
		if self.lower.is_finite() && subtrahend.upper.is_finite() {
			(self.lower - subtrahend.upper).next_down().max(0.0)
		} else {
			0.0
		}
	}
}

/// One row-major directed table; hops retain their full u64 representation.
#[derive(Debug)]
pub enum LandmarkDistanceRows {
	/// Hop counts with u64::MAX as infinity.
	Hops(Vec<u64>),
	/// Weighted or additive PURE costs enclosed by outward rounding.
	Intervals(Vec<DistanceInterval>),
}

/// Distances from and to deterministically selected landmark ordinals.
#[derive(Debug)]
pub struct DirectedLandmarks {
	/// Dense vertex count of the captured projection.
	pub vertices: usize,
	/// Farthest-point selection order, with canonical ordinal ties.
	pub landmarks: Vec<usize>,
	/// landmark-major distances from landmarks.
	pub from: LandmarkDistanceRows,
	/// landmark-major distances to landmarks.
	pub to: LandmarkDistanceRows,
}

/// Payload and scratch limits, independent of page/header accounting.
#[derive(Debug, Clone, Copy)]
pub struct LandmarkBuildLimits {
	/// Maximum bytes of both distance tables and landmark ordinals.
	pub result_bytes: u64,
	/// Total admitted scratch including the borrowed relation/caller data.
	pub scratch_bytes: usize,
	/// Conservative bytes already retained by the caller.
	pub live_scratch_bytes: usize,
}

impl DirectedLandmarks {
	/// Builds directed distances for an already canonical, checked relation.
	/// Weighted costs align with forward CSR arcs; callers collapse parallel
	/// connectors using their least compatible additive cost.
	///
	/// # Errors
	/// Rejects malformed relations/costs, invalid count, budgets or
	/// cancellation.
	pub fn build<Check>(
		relation: &ProjectedRelation,
		weights: Option<&[Number]>,
		count: u16,
		limits: LandmarkBuildLimits,
		check: &mut Check,
	) -> FerrosGrynnResult<Self>
	where
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		check()?;
		if !(1..=256).contains(&count) {
			return Err(FerrosGrynnError::invalid_argument("landmark count must be in 1..=256"));
		}
		let vertices = relation.vertices.len();
		relation.forward.validate(vertices, check)?;
		relation.reverse.validate(vertices, check)?;
		if relation.forward.targets.len() != relation.reverse.targets.len() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"landmark reverse CSR extent disagrees",
			));
		}
		for source in 0..vertices {
			for target in relation.reverse.neighbours(source) {
				check()?;
				let start = relation.forward.offsets[target];
				let end = relation.forward.offsets[target + 1];
				if relation.forward.targets[start..end].binary_search(&source).is_err() {
					return Err(FerrosGrynnError::invalid_storage_file(
						"landmark reverse CSR has a foreign arc",
					));
				}
			}
		}
		let count = usize::from(count).min(vertices);
		let width = if weights.is_some() {
			16_usize
		} else {
			8
		};
		let cells = vertices.checked_mul(count).ok_or_else(|| resource(limits))?;
		let bytes = cells
			.checked_mul(width)
			.and_then(|bytes| bytes.checked_mul(2))
			.and_then(|bytes| count.checked_mul(8).and_then(|refs| bytes.checked_add(refs)))
			.ok_or_else(|| resource(limits))?;
		let scratch = relation
			.forward
			.targets
			.len()
			.checked_mul(64)
			.and_then(|arcs| vertices.checked_mul(128).and_then(|vertex| arcs.checked_add(vertex)))
			.and_then(|work| work.checked_add(bytes))
			.and_then(|work| work.checked_add(limits.live_scratch_bytes))
			.and_then(|work| work.checked_add(8192));
		if bytes as u64 > limits.result_bytes {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"landmark distance payload bytes",
				limits.result_bytes,
			));
		}
		if scratch.is_none_or(|bytes| bytes > limits.scratch_bytes) {
			return Err(resource(limits));
		}
		let intervals = weights
			.map(|weights| {
				if weights.len() != relation.forward.targets.len() {
					return Err(FerrosGrynnError::invalid_argument(
						"landmark costs do not align with CSR arcs",
					));
				}
				let mut result = reserve(weights.len(), limits)?;
				for number in weights {
					check()?;
					result.push(DistanceInterval::from_number(*number)?);
				}
				Ok(result)
			})
			.transpose()?;
		let mut selected = filled(vertices, false, limits, check)?;
		let mut landmarks = reserve(count, limits)?;
		let mut from = if weights.is_some() {
			LandmarkDistanceRows::Intervals(reserve(cells, limits)?)
		} else {
			LandmarkDistanceRows::Hops(reserve(cells, limits)?)
		};
		let mut to = if weights.is_some() {
			LandmarkDistanceRows::Intervals(reserve(cells, limits)?)
		} else {
			LandmarkDistanceRows::Hops(reserve(cells, limits)?)
		};
		let mut nearest_hop = filled(
			if weights.is_none() {
				vertices
			} else {
				0
			},
			u64::MAX,
			limits,
			check,
		)?;
		let mut nearest_weight = filled(
			if weights.is_some() {
				vertices
			} else {
				0
			},
			f64::INFINITY,
			limits,
			check,
		)?;
		for _ in 0..count {
			check()?;
			let mut landmark = None;
			for vertex in 0..vertices {
				check()?;
				if selected[vertex] {
					continue;
				}
				if landmark.is_none_or(|old| {
					if weights.is_some() {
						nearest_weight[vertex] > nearest_weight[old]
					} else {
						nearest_hop[vertex] > nearest_hop[old]
					}
				}) {
					landmark = Some(vertex);
				}
			}
			let landmark = landmark.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("landmark selection exhausted vertices")
			})?;
			selected[landmark] = true;
			landmarks.push(landmark);
			match (&mut from, &mut to, &intervals) {
				(LandmarkDistanceRows::Hops(from), LandmarkDistanceRows::Hops(to), None) => {
					let forward = hop_row(&relation.forward, landmark, limits, check)?;
					let reverse = hop_row(&relation.reverse, landmark, limits, check)?;
					for vertex in 0..vertices {
						nearest_hop[vertex] =
							nearest_hop[vertex].min(forward[vertex].min(reverse[vertex]));
					}
					from.extend(forward);
					to.extend(reverse);
				}
				(
					LandmarkDistanceRows::Intervals(from),
					LandmarkDistanceRows::Intervals(to),
					Some(weights),
				) => {
					let forward = interval_row(relation, weights, landmark, false, limits, check)?;
					let reverse = interval_row(relation, weights, landmark, true, limits, check)?;
					for vertex in 0..vertices {
						nearest_weight[vertex] = nearest_weight[vertex]
							.min(forward[vertex].lower.min(reverse[vertex].lower));
					}
					from.extend(forward);
					to.extend(reverse);
				}
				_ => {
					return Err(FerrosGrynnError::invalid_storage_file(
						"landmark distance encodings disagree",
					));
				}
			}
		}
		Ok(Self {
			vertices,
			landmarks,
			from,
			to,
		})
	}

	/// Returns the directed ALT bound, ignoring unreachable subtraction terms.
	///
	/// # Errors
	/// Rejects out-of-range vertices or inconsistent table
	/// dimensions/encodings.
	pub fn lower_bound(&self, vertex: usize, target: usize) -> FerrosGrynnResult<Number> {
		if vertex >= self.vertices || target >= self.vertices {
			return Err(FerrosGrynnError::invalid_argument("ALT vertex is out of bounds"));
		}
		let mut hop = 0_u64;
		let mut weight = 0.0_f64;
		for ordinal in 0..self.landmarks.len() {
			let offset = ordinal.checked_mul(self.vertices).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("ALT table extent overflows")
			})?;
			match (&self.from, &self.to) {
				(LandmarkDistanceRows::Hops(from), LandmarkDistanceRows::Hops(to)) => {
					let (a, b, c, d) = (
						cell(from, offset, target)?,
						cell(from, offset, vertex)?,
						cell(to, offset, vertex)?,
						cell(to, offset, target)?,
					);
					if a != u64::MAX && b != u64::MAX {
						hop = hop.max(a.saturating_sub(b));
					}
					if c != u64::MAX && d != u64::MAX {
						hop = hop.max(c.saturating_sub(d));
					}
				}
				(LandmarkDistanceRows::Intervals(from), LandmarkDistanceRows::Intervals(to)) => {
					weight = weight
						.max(
							cell(from, offset, target)?
								.lower_difference(cell(from, offset, vertex)?),
						)
						.max(cell(to, offset, vertex)?.lower_difference(cell(to, offset, target)?));
				}
				_ => {
					return Err(FerrosGrynnError::invalid_storage_file(
						"ALT distance encodings disagree",
					));
				}
			}
		}
		match &self.from {
			LandmarkDistanceRows::Hops(_) => i64::try_from(hop).map(Number::Int).map_err(|cause| {
				FerrosGrynnError::operation_overflow("ALT hop bound").with_technical_source(cause)
			}),
			LandmarkDistanceRows::Intervals(_) => Ok(Number::Float(weight)),
		}
	}
}

/// Reads one checked row-major cell without unchecked extent addition.
fn cell<T: Copy>(rows: &[T], offset: usize, vertex: usize) -> FerrosGrynnResult<T> {
	offset
		.checked_add(vertex)
		.and_then(|cell| rows.get(cell))
		.copied()
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("ALT distance cell is absent"))
}

/// Computes one exact hop row, including the seed and disconnected vertices.
fn hop_row<Check>(
	csr: &RelationCsr,
	source: usize,
	limits: LandmarkBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<u64>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let vertices = csr.offsets.len() - 1;
	let mut distances = filled(vertices, u64::MAX, limits, check)?;
	let mut queue = reserve(vertices, limits)?;
	distances[source] = 0;
	queue.push(source);
	let mut cursor = 0;
	while cursor < queue.len() {
		check()?;
		let vertex = queue[cursor];
		cursor += 1;
		for target in csr.neighbours(vertex) {
			check()?;
			if distances[target] == u64::MAX {
				distances[target] = distances[vertex]
					.checked_add(1)
					.filter(|distance| *distance != u64::MAX)
					.ok_or_else(|| resource(limits))?;
				queue.push(target);
			}
		}
	}
	Ok(distances)
}

/// A min-heap entry with finite nonnegative cost and canonical ordinal ties.
#[derive(Debug, Clone, Copy)]
struct DistanceQueue {
	/// One lower or upper tentative distance.
	distance: f64,
	/// Dense vertex ordinal.
	vertex: usize,
}

impl PartialEq for DistanceQueue {
	fn eq(&self, other: &Self) -> bool {
		self.cmp(other) == Ordering::Equal
	}
}
impl Eq for DistanceQueue {}
impl PartialOrd for DistanceQueue {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}
impl Ord for DistanceQueue {
	fn cmp(&self, other: &Self) -> Ordering {
		other.distance.total_cmp(&self.distance).then_with(|| other.vertex.cmp(&self.vertex))
	}
}

/// Combines independently minimized lower/upper paths into a safe enclosure.
fn interval_row<Check>(
	relation: &ProjectedRelation,
	weights: &[DistanceInterval],
	source: usize,
	reverse: bool,
	limits: LandmarkBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<DistanceInterval>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let lower = interval_side(relation, weights, source, reverse, false, limits, check)?;
	let upper = interval_side(relation, weights, source, reverse, true, limits, check)?;
	let mut rows = reserve(relation.vertices.len(), limits)?;
	for (lower, upper) in lower.into_iter().zip(upper) {
		check()?;
		rows.push(DistanceInterval::new(lower, upper)?);
	}
	Ok(rows)
}

/// Monotone nonnegative relaxation needs at most one live expansion per
/// vertex and E+1 heap insertions; stale entries are ignored, never finalized.
#[allow(
	clippy::too_many_arguments,
	reason = "one bounded SSSP kernel shares forward/reverse and lower/upper modes"
)]
fn interval_side<Check>(
	relation: &ProjectedRelation,
	weights: &[DistanceInterval],
	source: usize,
	reverse: bool,
	upper: bool,
	limits: LandmarkBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<f64>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let csr = if reverse {
		&relation.reverse
	} else {
		&relation.forward
	};
	let mut distances = filled(relation.vertices.len(), f64::INFINITY, limits, check)?;
	let mut queue = BinaryHeap::new();
	let capacity = weights.len().checked_add(1).ok_or_else(|| resource(limits))?;
	queue
		.try_reserve_exact(capacity)
		.map_err(|cause| resource(limits).with_technical_source(cause))?;
	distances[source] = 0.0;
	queue.push(DistanceQueue {
		distance: 0.0,
		vertex: source,
	});
	while let Some(state) = queue.pop() {
		check()?;
		if state.distance > distances[state.vertex] {
			continue;
		}
		for arc in csr.offsets[state.vertex]..csr.offsets[state.vertex + 1] {
			check()?;
			let target = csr.targets[arc];
			let original = if reverse {
				let start = relation.forward.offsets[target];
				let end = relation.forward.offsets[target + 1];
				start
					+ relation.forward.targets[start..end].binary_search(&state.vertex).map_err(
						|_ordinal| {
							FerrosGrynnError::invalid_storage_file("reverse landmark arc is absent")
						},
					)?
			} else {
				arc
			};
			let edge = weights[original];
			let prefix = DistanceInterval {
				lower: state.distance,
				upper: state.distance,
			};
			let candidate = prefix.add(edge);
			let candidate = if upper {
				candidate.upper
			} else {
				candidate.lower
			};
			if candidate < distances[target] {
				distances[target] = candidate;
				if queue.len() == capacity {
					return Err(resource(limits));
				}
				queue.push(DistanceQueue {
					distance: candidate,
					vertex: target,
				});
			}
		}
	}
	Ok(distances)
}

/// Reserves an exact vector extent inside the checked aggregate admission.
fn reserve<T>(count: usize, limits: LandmarkBuildLimits) -> FerrosGrynnResult<Vec<T>> {
	let mut values = Vec::new();
	values
		.try_reserve_exact(count)
		.map_err(|cause| resource(limits).with_technical_source(cause))?;
	Ok(values)
}

/// Initializes admitted arrays with bounded cancellation intervals.
fn filled<T: Clone, Check>(
	count: usize,
	value: T,
	limits: LandmarkBuildLimits,
	check: &mut Check,
) -> FerrosGrynnResult<Vec<T>>
where
	Check: FnMut() -> FerrosGrynnResult<()>,
{
	let mut values = reserve(count, limits)?;
	for ordinal in 0..count {
		if ordinal.is_multiple_of(256) {
			check()?;
		}
		values.push(value.clone());
	}
	Ok(values)
}

/// Maps every admission failure to the existing typed traversal resource error.
fn resource(limits: LandmarkBuildLimits) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(
		"landmark distance bytes or scratch",
		limits.scratch_bytes as u64,
	)
}

#[cfg(test)]
mod tests;
