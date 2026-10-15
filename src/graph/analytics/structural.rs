//! Sparse motif intersections and normalized clustering, without dense
//! products.
#![allow(
	clippy::cast_precision_loss,
	reason = "Exact motif counts become floating coefficients only at reduction"
)]

use std::collections::BTreeMap;

use super::{
	AnalyticsOrientation,
	ClusteringOptions,
	ClusteringResult,
	TriangleCounts,
	TriangleOptions,
	TriangleResult,
	WeightSelection,
	finish,
	scalar::{Strengths, finite},
};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::tensor::{
		CsrMatrix,
		TensorProjection,
		control::{Control, invalid, overflow},
	},
};

/// Charges a binary-search bound before looking up a canonical sparse cell.
fn lookup<'a, T>(
	matrix: &'a CsrMatrix<T>,
	row: usize,
	column: usize,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<Option<&'a T>> {
	control.work(1 + (matrix.shape().1.checked_add(1).ok_or_else(overflow)?.ilog2() as usize))?;
	matrix.get(row, column)
}

/// Validates symmetric topology and, when requested, exact selected strengths.
pub(super) fn symmetric(
	projection: &TensorProjection,
	weights: Option<WeightSelection>,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<()> {
	control.work(projection.binary.shape().0)?;
	for (row, column, _) in projection.binary.entries() {
		if row == column {
			control.step()?;
			continue;
		}
		if lookup(&projection.binary, column, row, control)? != Some(&true) {
			return Err(invalid("undirected analytics require symmetric projected topology"));
		}
		let equal = match weights {
			None => true,
			Some(WeightSelection::Unweighted) => {
				lookup(&projection.multiplicity, row, column, control)?
					== lookup(&projection.multiplicity, column, row, control)?
			}
			Some(WeightSelection::Stored) => {
				lookup(&projection.weights, row, column, control)?
					== lookup(&projection.weights, column, row, control)?
			}
		};
		if !equal {
			return Err(invalid("undirected analytics require symmetric selected strengths"));
		}
	}
	Ok(())
}

/// Intersects sorted rows, using binary lookup when their degrees are skewed.
fn intersection<T: Copy>(
	matrix: &CsrMatrix<T>,
	first: usize,
	second: usize,
	control: &mut Control<'_>,
	emit: &mut impl FnMut(usize, T, T, &mut Control<'_>) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let first_len = matrix.offsets()[first + 1] - matrix.offsets()[first];
	let second_len = matrix.offsets()[second + 1] - matrix.offsets()[second];
	if first_len == 0 || second_len == 0 {
		return Ok(());
	}
	control.work(2)?;
	if matrix.column_indices()[matrix.offsets()[first + 1] - 1]
		< matrix.column_indices()[matrix.offsets()[second]]
		|| matrix.column_indices()[matrix.offsets()[second + 1] - 1]
			< matrix.column_indices()[matrix.offsets()[first]]
	{
		return Ok(());
	}
	let (short, other, swapped) = if first_len <= second_len {
		(first, second, false)
	} else {
		(second, first, true)
	};
	let lookup_bound = 2 + matrix.shape().1.checked_add(1).ok_or_else(overflow)?.ilog2() as usize;
	if first_len
		.min(second_len)
		.checked_mul(lookup_bound)
		.is_some_and(|work| work < first_len.saturating_add(second_len))
	{
		for at in matrix.offsets()[short]..matrix.offsets()[short + 1] {
			control.step()?;
			let column = matrix.column_indices()[at];
			if let Some(&value) = lookup(matrix, other, column, control)? {
				let own = matrix.values()[at];
				let (first_value, second_value) = if swapped {
					(value, own)
				} else {
					(own, value)
				};
				emit(column, first_value, second_value, control)?;
			}
		}
		return Ok(());
	}
	let mut a = matrix.offsets()[first];
	let mut b = matrix.offsets()[second];
	while a < matrix.offsets()[first + 1] && b < matrix.offsets()[second + 1] {
		control.step()?;
		let ac = matrix.column_indices()[a];
		let bc = matrix.column_indices()[b];
		match ac.cmp(&bc) {
			std::cmp::Ordering::Less => a += 1,
			std::cmp::Ordering::Greater => b += 1,
			std::cmp::Ordering::Equal => {
				emit(ac, matrix.values()[a], matrix.values()[b], control)?;
				a += 1;
				b += 1;
			}
		}
	}
	Ok(())
}

/// Adds a nonnegative pair contribution, admitting map storage before
/// insertion.
pub(super) fn add_pair(
	pairs: &mut BTreeMap<(usize, usize), f64>,
	pair: (usize, usize),
	value: f64,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<()> {
	control.step()?;
	if let Some(before) = pairs.get_mut(&pair) {
		*before = finite(*before + value)?;
	} else {
		control.admit(192)?;
		pairs.insert(pair, value);
	}
	Ok(())
}

/// Streams canonical map pairs into admitted CSR arrays.
pub(super) fn from_pairs<T: Copy>(
	n: usize,
	pairs: BTreeMap<(usize, usize), T>,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<CsrMatrix<T>> {
	let mut offsets = control.array(n.checked_add(1).ok_or_else(overflow)?)?;
	let mut columns = control.array(pairs.len())?;
	let mut values = control.array(pairs.len())?;
	let mut pairs = pairs.into_iter().peekable();
	for row in 0..n {
		control.step()?;
		offsets.push(columns.len());
		while pairs.peek().is_some_and(|(pair, _)| pair.0 == row) {
			control.step()?;
			if let Some(((source, column), value)) = pairs.next() {
				if source >= n || column >= n {
					return Err(invalid("analytical sparse pair is outside its domain"));
				}
				columns.push(column);
				values.push(value);
			}
		}
	}
	if pairs.peek().is_some() {
		return Err(invalid("analytical sparse source is outside its domain"));
	}
	offsets.push(columns.len());
	Ok(CsrMatrix::from_canonical((n, n), offsets, columns, values))
}

/// Counts distinct undirected triples or directed cycles/transitive motifs.
///
/// # Errors
/// Rejects asymmetric undirected inputs, count overflow, cancellation or
/// budgets.
pub fn triangles<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	options: TriangleOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<TriangleResult> {
	let mut control = Control::new(options.limits, check)?;
	if options.orientation == AnalyticsOrientation::Undirected {
		symmetric(projection, None, &mut control)?;
	}
	let mut local = control.filled(projection.atoms.len(), TriangleCounts::default())?;
	let mut totals = TriangleCounts::default();
	let matrix = &projection.binary;
	control.work(matrix.shape().0)?;
	for (a, b, _) in matrix.entries() {
		control.step()?;
		if a == b {
			continue;
		}
		if options.orientation == AnalyticsOrientation::Undirected {
			if a >= b {
				continue;
			}
			intersection(matrix, a, b, &mut control, &mut |c, _, _, control| {
				control.step()?;
				if c > b {
					totals.undirected = totals.undirected.checked_add(1).ok_or_else(overflow)?;
					for node in [a, b, c] {
						local[node].undirected =
							local[node].undirected.checked_add(1).ok_or_else(overflow)?;
					}
				}
				Ok(())
			})?;
		} else {
			if a < b {
				for at in matrix.offsets()[b]..matrix.offsets()[b + 1] {
					control.step()?;
					let c = matrix.column_indices()[at];
					if c != b && c > a && lookup(matrix, c, a, &mut control)? == Some(&true) {
						totals.cycles = totals.cycles.checked_add(1).ok_or_else(overflow)?;
						for node in [a, b, c] {
							local[node].cycles =
								local[node].cycles.checked_add(1).ok_or_else(overflow)?;
						}
					}
				}
			}
			intersection(matrix, a, b, &mut control, &mut |c, _, _, control| {
				control.step()?;
				if c != a && c != b {
					totals.transitive = totals.transitive.checked_add(1).ok_or_else(overflow)?;
					for node in [a, b, c] {
						local[node].transitive =
							local[node].transitive.checked_add(1).ok_or_else(overflow)?;
					}
				}
				Ok(())
			})?;
		}
	}
	Ok(TriangleResult {
		local: finish(projection, local, options, None, &mut control)?,
		totals,
	})
}

/// Returns normalized weighted or binary Fagiolo/undirected clustering.
///
/// # Errors
/// Rejects incompatible orientation, weights, overflow, cancellation or
/// budgets.
pub fn clustering<CheckArg: FnMut() -> FerrosGrynnResult<()>>(
	projection: &TensorProjection,
	options: ClusteringOptions,
	check: &mut CheckArg,
) -> FerrosGrynnResult<ClusteringResult> {
	let mut control = Control::new(options.limits, check)?;
	if options.orientation == AnalyticsOrientation::Undirected {
		symmetric(
			projection,
			(options.weights == WeightSelection::Stored).then_some(options.weights),
			&mut control,
		)?;
	}
	let n = projection.atoms.len();
	let strengths = if options.weights == WeightSelection::Stored {
		Some(Strengths::new(projection, options.weights, &mut control)?)
	} else {
		None
	};
	let mut maximum = 0.0_f64;
	if let Some(strengths) = &strengths {
		for row in 0..n {
			control.step()?;
			for at in projection.weights.offsets()[row]..projection.weights.offsets()[row + 1] {
				control.step()?;
				if row != projection.weights.column_indices()[at] {
					maximum = maximum.max(strengths.cells[at]);
				}
			}
		}
	}
	let mut incoming = control.filled(n, 0_u64)?;
	let mut outgoing = control.filled(n, 0_u64)?;
	let mut reciprocal = control.filled(n, 0_u64)?;
	let mut pairs = BTreeMap::new();
	for row in 0..n {
		control.step()?;
		for at in projection.binary.offsets()[row]..projection.binary.offsets()[row + 1] {
			control.step()?;
			let column = projection.binary.column_indices()[at];
			if row == column {
				continue;
			}
			outgoing[row] = outgoing[row].checked_add(1).ok_or_else(overflow)?;
			incoming[column] = incoming[column].checked_add(1).ok_or_else(overflow)?;
			if lookup(&projection.binary, column, row, &mut control)? == Some(&true) {
				reciprocal[row] = reciprocal[row].checked_add(1).ok_or_else(overflow)?;
			}
			let value = match &strengths {
				None => 1.0,
				Some(strengths) => {
					if maximum == 0.0 {
						0.0
					} else {
						(strengths.cells[at] / maximum).cbrt()
					}
				}
			};
			add_pair(&mut pairs, (row, column), value, &mut control)?;
			if options.orientation == AnalyticsOrientation::Directed {
				add_pair(&mut pairs, (column, row), value, &mut control)?;
			}
		}
	}
	let matrix = from_pairs(n, pairs, &mut control)?;
	let mut numerators = control.filled(n, 0.0)?;
	control.work(matrix.shape().0)?;
	for (a, b, ab) in matrix.entries() {
		control.step()?;
		if a >= b {
			continue;
		}
		intersection(&matrix, a, b, &mut control, &mut |c, ac, bc, control| {
			control.step()?;
			if c > b {
				let amount = finite(2.0 * ab * ac * bc)?;
				for node in [a, b, c] {
					numerators[node] = finite(numerators[node] + amount)?;
				}
			}
			Ok(())
		})?;
	}
	let mut values = control.array(n)?;
	let mut numerator = 0.0;
	let mut denominator = 0.0;
	let mut mean = 0.0;
	for row in 0..n {
		control.step()?;
		let possible = if options.orientation == AnalyticsOrientation::Undirected {
			outgoing[row].checked_mul(outgoing[row].saturating_sub(1)).ok_or_else(overflow)?
		} else {
			let total = outgoing[row].checked_add(incoming[row]).ok_or_else(overflow)?;
			total
				.checked_mul(total.saturating_sub(1))
				.and_then(|count| count.checked_sub(2_u64.checked_mul(reciprocal[row])?))
				.and_then(|count| count.checked_mul(2))
				.ok_or_else(overflow)?
		};
		let value = if possible == 0 {
			0.0
		} else {
			numerators[row] / possible as f64
		};
		values.push(value);
		mean = finite(mean + value)?;
		numerator = finite(numerator + numerators[row])?;
		denominator = finite(denominator + possible as f64)?;
	}
	if n > 0 {
		mean /= n as f64;
	}
	Ok(ClusteringResult {
		local: finish(projection, values, options, None, &mut control)?,
		mean,
		transitivity: if denominator == 0.0 {
			0.0
		} else {
			numerator / denominator
		},
	})
}
