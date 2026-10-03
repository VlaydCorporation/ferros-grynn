//! Canonical sequential community moves over bounded sparse projections.

use std::collections::BTreeMap;

use num::{BigRational, Zero};

use super::{
	AnalyticsOrientation,
	CommunityResult,
	ConvergenceMetadata,
	LabelPropagationOptions,
	LouvainOptions,
	WeightSelection,
	finish,
	scalar::{Strengths, finite},
	structural::{add_pair, from_pairs, symmetric},
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		TraversalDirection,
		tensor::{
			CsrMatrix,
			TensorProjection,
			control::{Control, invalid, overflow},
		},
	},
	math::{add_numbers, number_is_finite},
	values::Number,
};

/// One sparse level, with deterministic incoming rows and strength reductions.
struct Level {
	/// Canonical outgoing strengths, including internal loop contributions.
	matrix: CsrMatrix<f64>,
	/// Canonical incoming strengths.
	reverse: CsrMatrix<f64>,
	/// Raw outgoing strengths.
	outgoing: Vec<f64>,
	/// Raw incoming strengths.
	incoming: Vec<f64>,
	/// Minimum original qualified ordinal represented by each level vertex.
	minimum: Vec<usize>,
	/// Sum of all ordered strengths; undirected loops contribute twice.
	total: f64,
	/// Integral strengths and total <=2^53 admit exact i128 gain products.
	integer: bool,
}

impl Level {
	/// Captures selected strengths and doubles undirected diagonal weights.
	fn capture(
		projection: &TensorProjection,
		weights: WeightSelection,
		undirected: bool,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Self> {
		if undirected {
			symmetric(projection, Some(weights), control)?;
		}
		let strengths = Strengths::new(projection, weights, control)?;
		let n = projection.atoms.len();
		let mut offsets = control.array(n.checked_add(1).ok_or_else(overflow)?)?;
		let mut columns = control.array(projection.weights.stored_cells())?;
		let mut values = strengths.cells;
		for offset in projection.weights.offsets() {
			control.step()?;
			offsets.push(*offset);
		}
		for column in projection.weights.column_indices() {
			control.step()?;
			columns.push(*column);
		}
		if undirected {
			for row in 0..n {
				control.step()?;
				for at in offsets[row]..offsets[row + 1] {
					control.step()?;
					if columns[at] == row {
						values[at] = finite(2.0 * values[at])?;
					}
				}
			}
		}
		let mut minimum = control.array(n)?;
		for row in 0..n {
			control.step()?;
			minimum.push(row);
		}
		Self::new(CsrMatrix::from_canonical((n, n), offsets, columns, values), minimum, control)
	}

	/// Builds reverse rows and raw degree arrays for one complete level.
	fn new(
		matrix: CsrMatrix<f64>,
		minimum: Vec<usize>,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Self> {
		let n = matrix.shape().0;
		let mut outgoing = control.filled(n, 0.0)?;
		let mut incoming = control.filled(n, 0.0)?;
		let mut reverse = BTreeMap::new();
		let mut total = 0.0;
		let mut integer = true;
		let mut integer_total = 0_i128;
		control.work(matrix.shape().0)?;
		for (row, column, value) in matrix.entries() {
			control.step()?;
			outgoing[row] = finite(outgoing[row] + value)?;
			incoming[column] = finite(incoming[column] + value)?;
			total = finite(total + value)?;
			if integer {
				match integer_strength(*value) {
					Ok(whole) => {
						integer_total += whole;
						integer = integer_total <= (1_i128 << 53);
					}
					Err(_cause) => integer = false,
				}
			}
			add_pair(&mut reverse, (column, row), *value, control)?;
		}
		Ok(Self {
			matrix,
			reverse: from_pairs(n, reverse, control)?,
			outgoing,
			incoming,
			minimum,
			total,
			integer,
		})
	}

	/// Computes directed modularity; symmetric levels use the undirected form.
	fn modularity(
		&self,
		labels: &[usize],
		resolution: f64,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<f64> {
		if self.total == 0.0 {
			return Ok(0.0);
		}
		let n = labels.len();
		let mut outgoing = control.filled(n, 0.0)?;
		let mut incoming = control.filled(n, 0.0)?;
		let mut internal = 0.0;
		for (row, label) in labels.iter().copied().enumerate() {
			control.step()?;
			outgoing[label] = finite(outgoing[label] + self.outgoing[row])?;
			incoming[label] = finite(incoming[label] + self.incoming[row])?;
		}
		control.work(self.matrix.shape().0)?;
		for (row, column, value) in self.matrix.entries() {
			control.step()?;
			if labels[row] == labels[column] {
				internal = finite(internal + value / self.total)?;
			}
		}
		for row in 0..n {
			control.step()?;
			internal = finite(
				internal - resolution * (outgoing[row] / self.total) * (incoming[row] / self.total),
			)?;
		}
		Ok(internal)
	}
}

/// Reused per-node votes, avoiding allocation for every local update.
struct Votes<T> {
	/// Vote values indexed by current label.
	values: Vec<T>,
	/// Labels visited during the current update.
	touched: Vec<usize>,
	/// Whether a label has a current vote entry.
	seen: Vec<bool>,
	/// Checked Number sums for LPA, finite floating sums for modularity moves.
	plus: fn(T, T) -> FerrosGrynnResult<T>,
}

impl<T: Copy + Default> Votes<T> {
	/// Admits fixed-size vote buffers once for one level.
	fn new(
		n: usize,
		plus: fn(T, T) -> FerrosGrynnResult<T>,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Self> {
		Ok(Self {
			values: control.filled(n, T::default())?,
			touched: control.array(n)?,
			seen: control.filled(n, false)?,
			plus,
		})
	}

	/// Resets exactly the labels touched by the previous update.
	fn clear(&mut self, control: &mut Control<'_>) -> FerrosGrynnResult<()> {
		for label in self.touched.drain(..) {
			control.step()?;
			self.values[label] = T::default();
			self.seen[label] = false;
		}
		Ok(())
	}

	/// Adds one finite vote while preserving a bounded touched-label list.
	fn add(&mut self, label: usize, amount: T, control: &mut Control<'_>) -> FerrosGrynnResult<()> {
		control.step()?;
		if !self.seen[label] {
			self.seen[label] = true;
			self.touched.push(label);
		}
		self.values[label] = (self.plus)(self.values[label], amount)?;
		Ok(())
	}

	/// Scans one canonical sparse row, optionally excluding the vertex loop.
	fn row(
		&mut self,
		matrix: &CsrMatrix<T>,
		node: usize,
		labels: &[usize],
		loops: bool,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<()> {
		control.step()?;
		for at in matrix.offsets()[node]..matrix.offsets()[node + 1] {
			control.step()?;
			let other = matrix.column_indices()[at];
			if loops || other != node {
				self.add(labels[other], matrix.values()[at], control)?;
			}
		}
		Ok(())
	}
}

/// Owns canonical community identities before returning a complete assignment.
fn result<P>(
	projection: &TensorProjection,
	labels: Vec<usize>,
	parameters: P,
	iterations: usize,
	levels: usize,
	modularity: Option<f64>,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<CommunityResult<P>> {
	let mut minimum = control.filled(labels.len(), usize::MAX)?;
	for (node, label) in labels.iter().copied().enumerate() {
		control.step()?;
		minimum[label] = minimum[label].min(node);
	}
	let mut identities = control.array(labels.len())?;
	for label in labels {
		control.step()?;
		let identity = &projection.atoms[minimum[label]];
		control.admit(identity.heap_bytes())?;
		identities.push(identity.clone());
	}
	Ok(CommunityResult {
		assignments: finish(
			projection,
			identities,
			parameters,
			Some(ConvergenceMetadata {
				iterations,
				residual: 0.0,
			}),
			control,
		)?,
		modularity,
		levels,
	})
}

/// Sums a floating modularity vote with finite-result validation.
fn add_float(a: f64, b: f64) -> FerrosGrynnResult<f64> {
	finite(a + b)
}

/// Captures exact Number votes and their checked canonical transpose.
fn label_relation(
	projection: &TensorProjection,
	weights: WeightSelection,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<(CsrMatrix<Number>, CsrMatrix<Number>)> {
	if weights == WeightSelection::Stored {
		if projection.options.weights != WeightSelection::Stored {
			return Err(invalid("stored votes require a STORED projection"));
		}
		for arc in &projection.arcs {
			control.step()?;
			let weight = projection.connector_weights[arc.connector];
			if !number_is_finite(weight) || weight < Number::Int(0) {
				return Err(invalid("label propagation weights must be finite and nonnegative"));
			}
		}
	}
	let n = projection.atoms.len();
	let mut offsets = control.array(n.checked_add(1).ok_or_else(overflow)?)?;
	let mut columns = control.array(projection.weights.stored_cells())?;
	let mut values = control.array(projection.weights.stored_cells())?;
	for offset in projection.weights.offsets() {
		control.step()?;
		offsets.push(*offset);
	}
	let mut reverse = BTreeMap::new();
	for row in 0..n {
		control.step()?;
		for at in offsets[row]..offsets[row + 1] {
			control.step()?;
			let column = projection.weights.column_indices()[at];
			let value = match weights {
				WeightSelection::Stored => projection.weights.values()[at],
				WeightSelection::Unweighted => Number::Int(
					i64::try_from(projection.multiplicity.values()[at])
						.map_err(|_cause| overflow())?,
				),
			};
			columns.push(column);
			values.push(value);
			control.admit(192)?;
			reverse.insert((column, row), value);
		}
	}
	Ok((
		CsrMatrix::from_canonical((n, n), offsets, columns, values),
		from_pairs(n, reverse, control)?,
	))
}

/// Runs sequential canonical label propagation, preserving current maxima on
/// ties.
///
/// # Errors
/// Rejects weights, invalid limits, overflow, cancellation or nonconvergence.
pub fn label_propagation(
	projection: &TensorProjection,
	options: LabelPropagationOptions,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<CommunityResult<LabelPropagationOptions>> {
	if options.max_sweeps == 0 {
		return Err(invalid("label propagation sweep limit must be positive"));
	}
	let mut control = Control::new(options.limits, check)?;
	let (matrix, reverse) = label_relation(projection, options.weights, &mut control)?;
	let n = projection.atoms.len();
	let mut labels = control.array(n)?;
	for row in 0..n {
		control.step()?;
		labels.push(row);
	}
	let mut votes = Votes::new(n, add_numbers, &mut control)?;
	if n == 0 {
		return result(projection, labels, options, 0, 0, None, &mut control);
	}
	for sweep in 1..=options.max_sweeps {
		let mut changed = false;
		for node in 0..n {
			control.step()?;
			votes.clear(&mut control)?;
			if options.votes != TraversalDirection::Backward {
				votes.row(&matrix, node, &labels, true, &mut control)?;
			}
			if options.votes != TraversalDirection::Forward {
				votes.row(&reverse, node, &labels, true, &mut control)?;
			}
			let old = labels[node];
			let mut best = old;
			let mut maximum = votes.values[old];
			for candidate in &votes.touched {
				control.step()?;
				let value = votes.values[*candidate];
				if value > maximum || (value == maximum && best != old && *candidate < best) {
					best = *candidate;
					maximum = value;
				}
			}
			if best != old {
				labels[node] = best;
				changed = true;
			}
		}
		if !changed {
			return result(projection, labels, options, sweep, 1, None, &mut control);
		}
	}
	Err(FerrosGrynnError::graph_algorithm_did_not_converge(
		"label propagation",
		options.max_sweeps as u64,
		1.0,
	))
}

/// Incident and post-removal community strengths for one insertion candidate.
#[derive(Clone, Copy)]
struct MoveScore {
	/// Point estimate used only to rank proven profitable moves.
	point: f64,
	/// Incident strength connecting the moving node and the candidate group.
	incident: f64,
	/// Group outgoing strength, excluding the moving vertex.
	outgoing: f64,
	/// Group incoming strength, excluding the moving vertex.
	incoming: f64,
}

impl MoveScore {
	/// Evaluates the directed insertion score without overflowing raw products.
	fn new(
		incident: f64,
		outgoing: f64,
		incoming: f64,
		level: &Level,
		node: usize,
		resolution: f64,
	) -> FerrosGrynnResult<Self> {
		let point = finite(
			incident / level.total
				- resolution
					* ((level.outgoing[node] / level.total) * (incoming / level.total)
						+ (level.incoming[node] / level.total) * (outgoing / level.total)),
		)?;
		Ok(Self {
			point,
			incident,
			outgoing,
			incoming,
		})
	}
}

/// Converts a certified nonnegative whole strength bounded by 2^53.
#[allow(
	clippy::cast_possible_truncation,
	reason = "Finite integral range is checked before conversion"
)]
fn integer_strength(value: f64) -> FerrosGrynnResult<i128> {
	if !value.is_finite() || value < 0.0 || value > (1_u64 << 53) as f64 || value.fract() != 0.0 {
		return Err(invalid("certified integral modularity strength is malformed"));
	}
	Ok(i128::from(value as i64))
}

/// Certifies close gains from exact dyadic sparse strengths without epsilon
/// ties. The sign uses delta_internal*m - gamma*delta_degree_products, without
/// division.
fn positive_gain(
	level: &Level,
	labels: &[usize],
	node: usize,
	old: usize,
	new: usize,
	before: MoveScore,
	after: MoveScore,
	resolution: f64,
	exact_scratch: &mut bool,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<bool> {
	control.work(32)?;
	if level.integer && resolution == 1.0 {
		let internal = integer_strength(after.incident)?
			.checked_sub(integer_strength(before.incident)?)
			.ok_or_else(overflow)?;
		let incoming = integer_strength(after.incoming)?
			.checked_sub(integer_strength(before.incoming)?)
			.ok_or_else(overflow)?;
		let outgoing = integer_strength(after.outgoing)?
			.checked_sub(integer_strength(before.outgoing)?)
			.ok_or_else(overflow)?;
		let total = integer_strength(level.total)?;
		let out_node = integer_strength(level.outgoing[node])?;
		let in_node = integer_strength(level.incoming[node])?;
		let gain = internal
			.checked_mul(total)
			.and_then(|value| value.checked_sub(out_node.checked_mul(incoming)?))
			.and_then(|value| value.checked_sub(in_node.checked_mul(outgoing)?))
			.ok_or_else(overflow)?;
		return Ok(gain > 0);
	}
	// Each charged operation bounds at most 32 scalar arithmetic operations,
	// including all prior reductions and mutable degree subtract/add steps.
	let error_operations = (control.work as f64).next_up() * 32.0;
	let rounding = error_operations * f64::EPSILON;
	if rounding < 0.5 {
		let error = (1.0 + resolution + before.point.abs() + after.point.abs()) * rounding
			/ (1.0 - rounding);
		if after.point - before.point > error.next_up() {
			return Ok(true);
		}
		if after.point - before.point < -error.next_up() {
			return Ok(false);
		}
	}
	if !*exact_scratch {
		// Finite f64 dyadics span at most 2098 source bits. Summing at most
		// usize::MAX cells adds at most 64 bits; degree products and gamma
		// remain below 8192 bits. 256 KiB also covers transient products/GCD.
		control.admit(256 * 1024)?;
		*exact_scratch = true;
	}
	let mut total = BigRational::zero();
	let mut out_node = BigRational::zero();
	let mut in_node = BigRational::zero();
	let mut out_old = BigRational::zero();
	let mut in_old = BigRational::zero();
	let mut out_new = BigRational::zero();
	let mut in_new = BigRational::zero();
	let mut incident_old = BigRational::zero();
	let mut incident_new = BigRational::zero();
	control.work(level.matrix.shape().0)?;
	for (row, column, weight) in level.matrix.entries() {
		control.work(32)?;
		let weight = BigRational::from_float(*weight)
			.ok_or_else(|| invalid("modularity strength is not finite"))?;
		total += &weight;
		if row == node {
			out_node += &weight;
		} else if labels[row] == old {
			out_old += &weight;
		} else if labels[row] == new {
			out_new += &weight;
		}
		if column == node {
			in_node += &weight;
		} else if labels[column] == old {
			in_old += &weight;
		} else if labels[column] == new {
			in_new += &weight;
		}
		if row == node && column != node {
			if labels[column] == old {
				incident_old += &weight;
			} else if labels[column] == new {
				incident_new += &weight;
			}
		}
		if column == node && row != node {
			if labels[row] == old {
				incident_old += &weight;
			} else if labels[row] == new {
				incident_new += &weight;
			}
		}
	}
	control.work(32)?;
	let gamma = BigRational::from_float(resolution)
		.ok_or_else(|| invalid("modularity resolution is not finite"))?;
	let gain = (incident_new - incident_old) * total
		- gamma * (out_node * (in_new - in_old) + in_node * (out_new - out_old));
	Ok(gain > BigRational::zero())
}

/// Converges one Louvain local-move phase using strictly positive objective
/// gains.
fn local_moves(
	level: &Level,
	options: LouvainOptions,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<(Vec<usize>, usize, f64)> {
	let n = level.minimum.len();
	let mut labels = control.array(n)?;
	let mut outgoing = control.array(n)?;
	let mut incoming = control.array(n)?;
	let mut count = control.filled(n, 1_usize)?;
	for row in 0..n {
		control.step()?;
		labels.push(row);
		outgoing.push(level.outgoing[row]);
		incoming.push(level.incoming[row]);
	}
	let mut votes = Votes::new(n, add_float, control)?;
	let mut exact_scratch = false;
	if level.total == 0.0 {
		return Ok((labels, 0, 0.0));
	}
	for sweep in 1..=options.max_sweeps {
		let mut changed = false;
		for node in 0..n {
			control.step()?;
			votes.clear(control)?;
			votes.row(&level.matrix, node, &labels, false, control)?;
			votes.row(&level.reverse, node, &labels, false, control)?;
			let old = labels[node];
			votes.add(old, 0.0, control)?;
			count[old] = count[old].checked_sub(1).ok_or_else(overflow)?;
			outgoing[old] = if count[old] == 0 {
				0.0
			} else {
				(outgoing[old] - level.outgoing[node]).max(0.0)
			};
			incoming[old] = if count[old] == 0 {
				0.0
			} else {
				(incoming[old] - level.incoming[node]).max(0.0)
			};
			let old_score = MoveScore::new(
				votes.values[old],
				outgoing[old],
				incoming[old],
				level,
				node,
				options.resolution,
			)?;
			let singleton = MoveScore::new(0.0, 0.0, 0.0, level, node, options.resolution)?;
			if count[old] > 0
				&& positive_gain(
					level,
					&labels,
					node,
					old,
					usize::MAX,
					old_score,
					singleton,
					options.resolution,
					&mut exact_scratch,
					control,
				)? {
				// ponytail: singleton search is linear only for potentially
				// profitable splits; index vacant groups if
				// measured split-heavy workloads need it.
				for candidate in 0..n {
					control.step()?;
					if count[candidate] == 0 {
						votes.add(candidate, 0.0, control)?;
						break;
					}
				}
			}
			let mut best = old;
			let mut best_score = old_score;
			for candidate in &votes.touched {
				control.step()?;
				if *candidate == old {
					continue;
				}
				let score = MoveScore::new(
					votes.values[*candidate],
					outgoing[*candidate],
					incoming[*candidate],
					level,
					node,
					options.resolution,
				)?;
				if positive_gain(
					level,
					&labels,
					node,
					old,
					*candidate,
					old_score,
					score,
					options.resolution,
					&mut exact_scratch,
					control,
				)? && (best == old
					|| score.point > best_score.point
					|| (score.point == best_score.point && *candidate < best))
				{
					best = *candidate;
					best_score = score;
				}
			}
			if best != old {
				labels[node] = best;
				changed = true;
			}
			let chosen = labels[node];
			count[chosen] = count[chosen].checked_add(1).ok_or_else(overflow)?;
			outgoing[chosen] = finite(outgoing[chosen] + level.outgoing[node])?;
			incoming[chosen] = finite(incoming[chosen] + level.incoming[node])?;
		}
		if !changed {
			let objective = level.modularity(&labels, options.resolution, control)?;
			return Ok((labels, sweep, objective));
		}
	}
	Err(FerrosGrynnError::graph_algorithm_did_not_converge(
		"Louvain local moves",
		options.max_sweeps as u64,
		1.0,
	))
}

/// Runs deterministic local moves and aggregation with directed modularity.
///
/// # Errors
/// Rejects incompatible orientation, weights, numeric/resource limits or
/// nonconvergence; no incomplete community assignment is returned.
pub fn louvain(
	projection: &TensorProjection,
	options: LouvainOptions,
	check: &mut impl FnMut() -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<CommunityResult<LouvainOptions>> {
	if !options.resolution.is_finite()
		|| options.resolution <= 0.0
		|| options.max_sweeps == 0
		|| options.max_levels == 0
	{
		return Err(invalid(
			"Louvain resolution and convergence limits must be positive and finite",
		));
	}
	let mut control = Control::new(options.limits, check)?;
	let n = projection.atoms.len();
	let mut level = Level::capture(
		projection,
		options.weights,
		options.orientation == AnalyticsOrientation::Undirected,
		&mut control,
	)?;
	let mut original = control.array(n)?;
	for node in 0..n {
		control.step()?;
		original.push(node);
	}
	if n == 0 {
		return result(projection, original, options, 0, 0, Some(0.0), &mut control);
	}
	let mut iterations = 0_usize;
	for depth in 1..=options.max_levels {
		let (labels, sweeps, objective) = local_moves(&level, options, &mut control)?;
		iterations = iterations.checked_add(sweeps).ok_or_else(overflow)?;
		let size = level.minimum.len();
		let mut minimum = control.filled(size, usize::MAX)?;
		for (node, label) in labels.iter().copied().enumerate() {
			control.step()?;
			minimum[label] = minimum[label].min(level.minimum[node]);
		}
		let mut ordered = control.array(size)?;
		for (label, min) in minimum.iter().copied().enumerate() {
			control.step()?;
			if min != usize::MAX {
				ordered.push((min, label));
			}
		}
		control.sort(ordered.len())?;
		ordered.sort_unstable();
		let next_size = ordered.len();
		let mut mapping = control.filled(size, 0_usize)?;
		let mut next_minimum = control.array(next_size)?;
		for (ordinal, (min, label)) in ordered.into_iter().enumerate() {
			control.step()?;
			mapping[label] = ordinal;
			next_minimum.push(min);
		}
		for node in &mut original {
			control.step()?;
			*node = mapping[labels[*node]];
		}
		if next_size == size || next_size <= 1 {
			let mut identities = control.array(n)?;
			for group in &original {
				control.step()?;
				identities.push(next_minimum[*group]);
			}
			return result(
				projection,
				identities,
				options,
				iterations,
				depth,
				Some(objective),
				&mut control,
			);
		}
		if depth == options.max_levels {
			return Err(FerrosGrynnError::graph_algorithm_did_not_converge(
				"Louvain aggregation",
				depth as u64,
				1.0,
			));
		}
		let mut pairs = BTreeMap::new();
		control.work(level.matrix.shape().0)?;
		for (row, column, value) in level.matrix.entries() {
			control.step()?;
			add_pair(
				&mut pairs,
				(mapping[labels[row]], mapping[labels[column]]),
				*value,
				&mut control,
			)?;
		}
		level =
			Level::new(from_pairs(next_size, pairs, &mut control)?, next_minimum, &mut control)?;
	}
	Err(invalid("Louvain exhausted an invalid level control"))
}
