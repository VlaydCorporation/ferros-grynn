//! Sparse scalar and spectral kernels; no dense graph multiplication.
#![allow(
	clippy::cast_precision_loss,
	reason = "Scores are floating-point; counts and path costs remain exact"
)]

use super::{
	AnalyticsResult,
	CentralityOptions,
	ConvergenceMetadata,
	DegreeValues,
	KatzOptions,
	PageRankOptions,
	SpectralOptions,
	WeightSelection,
	distance::{Distances, full_state_scores, needs_full_state},
	finish,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		PathDirection,
		tensor::{
			GraphAlgorithmLimits,
			TensorProjection,
			control::{Control, invalid, overflow},
		},
	},
	math::{add_numbers, number_is_finite},
	values::Number,
};

/// Checks a floating analytical intermediate before it reaches a result.
pub(super) fn finite(value: f64) -> FerrosGrynnResult<f64> {
	if value.is_finite() {
		Ok(value)
	} else {
		Err(FerrosGrynnError::operation_overflow("graph analytical arithmetic"))
	}
}

/// Returns degree and strength using parallel connector and loop conventions.
///
/// # Errors
/// Rejects arithmetic overflow, cancellation or exhausted operation limits.
pub fn degree<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	limits: GraphAlgorithmLimits,
	check: &mut CheckArg,
) -> FerrosGrynnResult<AnalyticsResult<DegreeValues, GraphAlgorithmLimits>> {
	let mut control = Control::new(limits, check)?;
	let mut values = control.filled(projection.atoms.len(), DegreeValues::default())?;
	let mut previous = None;
	for arc in &projection.arcs {
		control.step()?;
		let key = (arc.source, arc.target, arc.connector);
		if previous == Some(key) {
			continue;
		}
		previous = Some(key);
		let undirected = arc.direction == PathDirection::Undirected;
		let amount = if undirected && arc.source == arc.target {
			2
		} else {
			1
		};
		let weight = projection.connector_weights[arc.connector];
		let weight = if amount == 2 {
			add_numbers(weight, weight)?
		} else {
			weight
		};
		values[arc.source].outgoing =
			values[arc.source].outgoing.checked_add(amount).ok_or_else(overflow)?;
		values[arc.target].incoming =
			values[arc.target].incoming.checked_add(amount).ok_or_else(overflow)?;
		values[arc.source].outgoing_strength =
			add_numbers(values[arc.source].outgoing_strength, weight)?;
		values[arc.target].incoming_strength =
			add_numbers(values[arc.target].incoming_strength, weight)?;
		values[arc.source].total =
			values[arc.source].total.checked_add(amount).ok_or_else(overflow)?;
		values[arc.source].total_strength = add_numbers(values[arc.source].total_strength, weight)?;
		if !undirected {
			values[arc.target].total =
				values[arc.target].total.checked_add(amount).ok_or_else(overflow)?;
			values[arc.target].total_strength =
				add_numbers(values[arc.target].total_strength, weight)?;
		}
	}
	let denominator = projection.atoms.len().saturating_sub(1) as f64;
	for value in &mut values {
		control.step()?;
		if denominator > 0.0 {
			value.normalized_incoming = value.incoming as f64 / denominator;
			value.normalized_outgoing = value.outgoing as f64 / denominator;
			value.normalized_total = value.total as f64 / denominator;
		}
	}
	finish(projection, values, limits, None, &mut control)
}

/// Finite nonnegative strength arrays aligned with the projection CSR.
pub(super) struct Strengths {
	/// Strength of every retained sparse cell.
	pub(super) cells: Vec<f64>,
	/// Original outgoing strengths; zero identifies dangling rows.
	outgoing: Vec<f64>,
	/// Outward-rounded upper bound on maximum outgoing strength.
	kappa: f64,
}

impl Strengths {
	/// Validates original weights before assembling floating analytical
	/// strengths.
	pub(super) fn new(
		projection: &TensorProjection,
		weights: WeightSelection,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<Self> {
		if weights == WeightSelection::Stored {
			if projection.options.weights != WeightSelection::Stored {
				return Err(invalid("stored strengths require a STORED projection"));
			}
			for arc in &projection.arcs {
				control.step()?;
				let weight = projection.connector_weights[arc.connector];
				if !number_is_finite(weight) || weight < Number::Int(0) {
					return Err(invalid(
						"spectral and PageRank weights must be finite and nonnegative",
					));
				}
			}
		}
		let mut cells = control.array(projection.weights.stored_cells())?;
		let mut outgoing = control.filled(projection.atoms.len(), 0.0)?;
		let mut kappa = 0.0_f64;
		for (row, total) in outgoing.iter_mut().enumerate() {
			control.step()?;
			let mut upper = 0.0;
			for at in projection.weights.offsets()[row]..projection.weights.offsets()[row + 1] {
				control.step()?;
				let value = match weights {
					WeightSelection::Unweighted => projection.multiplicity.values()[at] as f64,
					WeightSelection::Stored => {
						projection.weights.values()[at].as_f64().ok_or_else(|| {
							invalid("strength cannot be represented as a floating score")
						})?
					}
				};
				if !value.is_finite() || value < 0.0 {
					return Err(invalid("analytical strengths must be finite and nonnegative"));
				}
				*total = finite(*total + value)?;
				upper = finite((upper + value).next_up())?;
				cells.push(value);
			}
			kappa = kappa.max(upper);
		}
		Ok(Self {
			cells,
			outgoing,
			kappa,
		})
	}

	/// Multiplies by the original transposed adjacency in canonical sparse
	/// order.
	fn transpose(
		&self,
		projection: &TensorProjection,
		input: &[f64],
		output: &mut [f64],
		scale: f64,
		control: &mut Control<'_>,
	) -> FerrosGrynnResult<()> {
		for value in &mut *output {
			control.step()?;
			*value = 0.0;
		}
		for (row, source) in input.iter().enumerate() {
			control.step()?;
			for at in projection.weights.offsets()[row]..projection.weights.offsets()[row + 1] {
				control.step()?;
				let column = projection.weights.column_indices()[at];
				output[column] = finite(output[column] + (scale * self.cells[at]) * source)?;
			}
		}
		Ok(())
	}
}

/// Returns normalized PageRank with uniform teleportation and dangling mass.
///
/// # Errors
/// Rejects invalid weights/controls, overflow, cancellation, resource limits or
/// nonconvergence.
pub fn page_rank<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	options: PageRankOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<AnalyticsResult<f64, PageRankOptions>> {
	options.iteration.validate()?;
	if !options.damping.is_finite() || !(0.0..1.0).contains(&options.damping) {
		return Err(invalid("PageRank damping must be finite in [0,1)"));
	}
	let mut control = Control::new(options.limits, check)?;
	let strengths = Strengths::new(projection, options.weights, &mut control)?;
	let n = projection.atoms.len();
	let mut values = control.filled(
		n,
		if n == 0 {
			0.0
		} else {
			1.0 / n as f64
		},
	)?;
	let mut next = control.filled(n, 0.0)?;
	if n == 0 {
		return finish(
			projection,
			values,
			options,
			Some(ConvergenceMetadata {
				iterations: 0,
				residual: 0.0,
			}),
			&mut control,
		);
	}
	let mut residual = f64::INFINITY;
	for iteration in 1..=options.iteration.max_iterations {
		let mut dangling = 0.0;
		for (row, value) in values.iter().enumerate() {
			control.step()?;
			if strengths.outgoing[row] == 0.0 {
				dangling = finite(dangling + value)?;
			}
		}
		let teleport = ((1.0 - options.damping) + options.damping * dangling) / n as f64;
		for value in &mut next {
			control.step()?;
			*value = teleport;
		}
		for (row, value) in values.iter().enumerate() {
			control.step()?;
			if strengths.outgoing[row] == 0.0 {
				continue;
			}
			for at in projection.weights.offsets()[row]..projection.weights.offsets()[row + 1] {
				control.step()?;
				let column = projection.weights.column_indices()[at];
				next[column] = finite(
					next[column]
						+ options.damping * value * (strengths.cells[at] / strengths.outgoing[row]),
				)?;
			}
		}
		let mut sum = 0.0;
		for value in &next {
			control.step()?;
			sum = finite(sum + value)?;
		}
		if sum <= 0.0 {
			return Err(invalid("PageRank normalization is zero"));
		}
		residual = 0.0;
		for (before, after) in values.iter().zip(&mut next) {
			control.step()?;
			*after /= sum;
			residual = finite(residual + (*before - *after).abs())?;
		}
		std::mem::swap(&mut values, &mut next);
		if residual <= options.iteration.tolerance {
			return finish(
				projection,
				values,
				options,
				Some(ConvergenceMetadata {
					iterations: iteration,
					residual,
				}),
				&mut control,
			);
		}
	}
	Err(FerrosGrynnError::graph_algorithm_did_not_converge(
		"PageRank",
		options.iteration.max_iterations as u64,
		residual,
	))
}

/// Returns the canonical shifted-power eigenvector with original residual
/// checks.
///
/// # Errors
/// Rejects negative/nonfinite strengths, invalid controls, numeric overflow or
/// nonconvergence.
pub fn eigenvector<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	options: SpectralOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<AnalyticsResult<f64, SpectralOptions>> {
	options.iteration.validate()?;
	let mut control = Control::new(options.limits, check)?;
	let strengths = Strengths::new(projection, options.weights, &mut control)?;
	let n = projection.atoms.len();
	let mut values = control.filled(
		n,
		if n == 0 {
			0.0
		} else {
			1.0 / (n as f64).sqrt()
		},
	)?;
	let mut next = control.filled(n, 0.0)?;
	let mut image = control.filled(n, 0.0)?;
	if n == 0 {
		return finish(
			projection,
			values,
			options,
			Some(ConvergenceMetadata {
				iterations: 0,
				residual: 0.0,
			}),
			&mut control,
		);
	}
	let mut residual = f64::INFINITY;
	for iteration in 1..=options.iteration.max_iterations {
		strengths.transpose(projection, &values, &mut next, 1.0, &mut control)?;
		let mut norm = 0.0_f64;
		for (before, after) in values.iter().zip(&mut next) {
			control.step()?;
			*after = finite(*after + *before)?;
			norm = finite(norm.hypot(*after))?;
		}
		if norm == 0.0 {
			return Err(invalid("eigenvector normalization is zero"));
		}
		for value in &mut next {
			control.step()?;
			*value /= norm;
		}
		strengths.transpose(projection, &next, &mut image, 1.0, &mut control)?;
		let mut lambda = 0.0;
		for (value, image) in next.iter().zip(&image) {
			control.step()?;
			lambda = finite(lambda + value * image)?;
		}
		residual = 0.0;
		for (value, image) in next.iter().zip(&image) {
			control.step()?;
			residual = finite(residual.hypot(image - lambda * value))?;
		}
		std::mem::swap(&mut values, &mut next);
		if residual <= options.iteration.tolerance {
			return finish(
				projection,
				values,
				options,
				Some(ConvergenceMetadata {
					iterations: iteration,
					residual,
				}),
				&mut control,
			);
		}
	}
	Err(FerrosGrynnError::graph_algorithm_did_not_converge(
		"eigenvector",
		options.iteration.max_iterations as u64,
		residual,
	))
}

/// Returns Katz scores with a proven conservative contraction bound.
///
/// # Errors
/// Rejects invalid alpha/beta, strengths, overflow, resource limits or
/// nonconvergence.
pub fn katz<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	mut options: KatzOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<AnalyticsResult<f64, KatzOptions>> {
	options.spectral.iteration.validate()?;
	if !options.beta.is_finite() || options.beta < 0.0 {
		return Err(invalid("Katz beta must be finite and nonnegative"));
	}
	let mut control = Control::new(options.spectral.limits, check)?;
	let strengths = Strengths::new(projection, options.spectral.weights, &mut control)?;
	let alpha = options.alpha.unwrap_or(0.5 / strengths.kappa.max(1.0));
	if !alpha.is_finite() || alpha < 0.0 || (alpha * strengths.kappa).next_up() >= 1.0 {
		return Err(invalid("Katz alpha must satisfy the proven alpha*kappa<1 bound"));
	}
	options.alpha = Some(alpha);
	let n = projection.atoms.len();
	let mut values = control.filled(n, 0.0)?;
	let mut next = control.filled(n, 0.0)?;
	let mut image = control.filled(n, 0.0)?;
	if n == 0 {
		return finish(
			projection,
			values,
			options,
			Some(ConvergenceMetadata {
				iterations: 0,
				residual: 0.0,
			}),
			&mut control,
		);
	}
	let mut residual = f64::INFINITY;
	for iteration in 1..=options.spectral.iteration.max_iterations {
		strengths.transpose(projection, &values, &mut next, alpha, &mut control)?;
		for value in &mut next {
			control.step()?;
			*value = finite(*value + options.beta)?;
		}
		strengths.transpose(projection, &next, &mut image, alpha, &mut control)?;
		residual = 0.0;
		for (value, image) in next.iter().zip(&image) {
			control.step()?;
			residual = finite(residual + (value - (image + options.beta)).abs())?;
		}
		std::mem::swap(&mut values, &mut next);
		if residual <= options.spectral.iteration.tolerance {
			return finish(
				projection,
				values,
				options,
				Some(ConvergenceMetadata {
					iterations: iteration,
					residual,
				}),
				&mut control,
			);
		}
	}
	Err(FerrosGrynnError::graph_algorithm_did_not_converge(
		"Katz",
		options.spectral.iteration.max_iterations as u64,
		residual,
	))
}

/// Returns harmonic closeness from exact hop or positive Number distances.
///
/// # Errors
/// Rejects invalid costs, overflow, cancellation or resource exhaustion.
pub fn harmonic_closeness<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	options: CentralityOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<AnalyticsResult<f64, CentralityOptions>> {
	let mut control = Control::new(options.limits, check)?;
	if needs_full_state(projection, options.weights, &mut control)? {
		let mut scores = full_state_scores(projection, false, &mut control)?;
		for score in &mut scores {
			control.step()?;
			if options.normalized && projection.atoms.len() > 1 {
				*score /= (projection.atoms.len() - 1) as f64;
			}
		}
		return finish(projection, scores, options, None, &mut control);
	}
	let mut distances = Distances::new(projection, options.weights, &mut control)?;
	let mut scores = control.filled(projection.atoms.len(), 0.0)?;
	for (source, score) in scores.iter_mut().enumerate() {
		distances.search(source, false, &mut control)?;
		for (target, distance) in distances.values.iter().enumerate() {
			control.step()?;
			if source != target
				&& let Some(distance) = distance
			{
				*score = finite(
					*score
						+ 1.0
							/ distance.as_f64().ok_or_else(|| {
								invalid("distance cannot be represented as a floating score")
							})?,
				)?;
			}
		}
		if options.normalized && projection.atoms.len() > 1 {
			*score /= (projection.atoms.len() - 1) as f64;
		}
	}
	finish(projection, scores, options, None, &mut control)
}

/// Returns exact all-source Brandes centrality, with checked path counters.
///
/// # Errors
/// Rejects nonpositive costs, unrepresentable Number distance progression,
/// path-counter overflow, cancellation and resource exhaustion.
pub fn betweenness<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	options: CentralityOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<AnalyticsResult<f64, CentralityOptions>> {
	let mut control = Control::new(options.limits, check)?;
	if needs_full_state(projection, options.weights, &mut control)? {
		let mut scores = full_state_scores(projection, true, &mut control)?;
		normalize_betweenness(projection, &mut scores, options.normalized, &mut control)?;
		return finish(projection, scores, options, None, &mut control);
	}
	let mut distances = Distances::new(projection, options.weights, &mut control)?;
	let mut scores = control.filled(projection.atoms.len(), 0.0)?;
	let mut dependency = control.filled(projection.atoms.len(), 0.0)?;
	for source in 0..projection.atoms.len() {
		distances.search(source, true, &mut control)?;
		for value in &mut dependency {
			control.step()?;
			*value = 0.0;
		}
		for target in distances.order.iter().rev().copied() {
			control.step()?;
			let target_distance =
				distances.values[target].ok_or_else(|| invalid("settled distance is absent"))?;
			for (previous, cost, multiplicity) in distances.incoming(target) {
				control.step()?;
				if let Some(before) = distances.values[previous]
					&& add_numbers(before, cost)? == target_distance
				{
					let numerator = distances.counts[previous]
						.checked_mul(multiplicity)
						.ok_or_else(overflow)?;
					dependency[previous] = finite(
						dependency[previous]
							+ numerator as f64 / distances.counts[target] as f64
								* (1.0 + dependency[target]),
					)?;
				}
			}
			if target != source {
				scores[target] = finite(scores[target] + dependency[target])?;
			}
		}
	}
	normalize_betweenness(projection, &mut scores, options.normalized, &mut control)?;
	finish(projection, scores, options, None, &mut control)
}

/// Applies identical directed/undirected conventions to both exact searches.
fn normalize_betweenness(
	projection: &TensorProjection,
	scores: &mut [f64],
	normalized: bool,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<()> {
	control.work(projection.arcs.len())?;
	let undirected = projection.arcs.iter().all(|arc| arc.direction == PathDirection::Undirected);
	let n = projection.atoms.len();
	let factor = if normalized {
		if n > 2 {
			1.0 / (n - 1) as f64 / (n - 2) as f64
		} else {
			0.0
		}
	} else if undirected {
		0.5
	} else {
		1.0
	};
	for value in scores {
		control.step()?;
		*value *= factor;
	}
	Ok(())
}
