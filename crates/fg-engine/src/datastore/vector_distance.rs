use super::{VectorDistance, schema::FunctionId};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Vector,
};

/// Computes the scalar exact-distance oracle shared by scan fallback and ANN
/// reranking.
///
/// # Errors
///
/// Returns an error for different vector kinds or dimensions, non-finite
/// components or results, cosine on a zero vector, checked-integer overflow,
/// invalid Minkowski parameters, or a custom function that was not evaluated
/// by the query runtime.
pub fn exact_vector_distance(
	left: &Vector,
	right: &Vector,
	distance: VectorDistance,
) -> FerrosGrynnResult<f64> {
	distance.validate()?;
	if left.kind() != right.kind() {
		return Err(FerrosGrynnError::invalid_argument(
			"KNN vectors must have the same element type",
		));
	}
	if left.len() != right.len() {
		return Err(FerrosGrynnError::invalid_argument("KNN vectors must have the same dimension"));
	}
	validate_ann_vector(left)?;
	validate_ann_vector(right)?;

	let result = match distance {
		VectorDistance::Euclidean => {
			integer_l2(left, right)?.unwrap_or_else(|| float_l2(left, right).sqrt())
		}
		VectorDistance::Manhattan => {
			integer_l1(left, right)?.unwrap_or_else(|| float_l1(left, right))
		}
		VectorDistance::Minkowski(exponent) => minkowski(left, right, exponent),
		VectorDistance::Cosine | VectorDistance::CosineNormalized => cosine(left, right)?,
		VectorDistance::InnerProduct => -dot(left, right),
		VectorDistance::Custom {
			function,
			..
		} => {
			return Err(custom_distance_requires_runtime(function));
		}
	};
	if !result.is_finite() {
		return Err(FerrosGrynnError::invalid_argument("vector distance is not finite"));
	}
	Ok(if result == 0.0 {
		0.0
	} else {
		result
	})
}

/// Rejects non-finite components before a vector enters an ANN generation.
pub fn validate_ann_vector(vector: &Vector) -> FerrosGrynnResult<()> {
	let finite = match vector {
		Vector::F64(values) => values.iter().all(|value| value.is_finite()),
		Vector::F32(values) => values.iter().all(|value| value.is_finite()),
		Vector::F16(values) => values.iter().all(|value| value.is_finite()),
		Vector::I64(_) | Vector::I32(_) | Vector::I16(_) | Vector::I8(_) | Vector::U8(_) => true,

		_ => {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported runtime variant at the workspace boundary.",
			));
		}
	};
	if !finite {
		return Err(FerrosGrynnError::invalid_argument(
			"ANN vectors must contain only finite components",
		));
	}
	Ok(())
}

/// Returns the component result.
fn component(vector: &Vector, index: usize) -> f64 {
	fg_runtime::__internal::vector_component_as_f64(vector, index)
}

/// Returns the float l2 result.
fn float_l2(left: &Vector, right: &Vector) -> f64 {
	(0..left.len())
		.map(|index| component(left, index) - component(right, index))
		.map(|difference| difference * difference)
		.sum()
}

/// Returns the float l1 result.
fn float_l1(left: &Vector, right: &Vector) -> f64 {
	(0..left.len()).map(|index| (component(left, index) - component(right, index)).abs()).sum()
}

/// Returns the minkowski result.
fn minkowski(left: &Vector, right: &Vector, exponent: f64) -> f64 {
	(0..left.len())
		.map(|index| (component(left, index) - component(right, index)).abs().powf(exponent))
		.sum::<f64>()
		.powf(exponent.recip())
}

/// Returns the dot result.
fn dot(left: &Vector, right: &Vector) -> f64 {
	(0..left.len()).map(|index| component(left, index) * component(right, index)).sum()
}

/// Returns the cosine result.
fn cosine(left: &Vector, right: &Vector) -> FerrosGrynnResult<f64> {
	let mut product = 0.0_f64;
	let mut left_norm = 0.0_f64;
	let mut right_norm = 0.0_f64;
	for index in 0..left.len() {
		let left = component(left, index);
		let right = component(right, index);
		product += left * right;
		left_norm += left * left;
		right_norm += right * right;
	}
	if left_norm == 0.0 || right_norm == 0.0 {
		return Err(FerrosGrynnError::invalid_argument(
			"cosine distance is undefined for a zero vector",
		));
	}
	Ok(1.0 - product / (left_norm.sqrt() * right_norm.sqrt()))
}

/// Returns the integer l1 result.
fn integer_l1(left: &Vector, right: &Vector) -> FerrosGrynnResult<Option<f64>> {
	/// Accumulates an exact integer L1 distance with checked u128 addition.
	macro_rules! sum {
		($left:expr, $right:expr) => {{
			let total = $left.iter().zip($right).try_fold(0_u128, |total, (left, right)| {
				let difference = i128::from(*left).abs_diff(i128::from(*right));
				total.checked_add(difference).ok_or_else(integer_distance_overflow)
			})?;
			Ok(Some(total as f64))
		}};
	}
	match (left, right) {
		(Vector::I64(left), Vector::I64(right)) => sum!(left, right),
		(Vector::I32(left), Vector::I32(right)) => sum!(left, right),
		(Vector::I16(left), Vector::I16(right)) => sum!(left, right),
		(Vector::I8(left), Vector::I8(right)) => sum!(left, right),
		(Vector::U8(left), Vector::U8(right)) => sum!(left, right),
		_ => Ok(None),
	}
}

/// Returns the integer l2 result.
fn integer_l2(left: &Vector, right: &Vector) -> FerrosGrynnResult<Option<f64>> {
	/// Accumulates squared integer differences with checked u128 arithmetic
	/// before taking a square root.
	macro_rules! sum {
		($left:expr, $right:expr) => {{
			let total = $left.iter().zip($right).try_fold(0_u128, |total, (left, right)| {
				let difference = i128::from(*left).abs_diff(i128::from(*right));
				let square =
					difference.checked_mul(difference).ok_or_else(integer_distance_overflow)?;
				total.checked_add(square).ok_or_else(integer_distance_overflow)
			})?;
			Ok(Some((total as f64).sqrt()))
		}};
	}
	match (left, right) {
		(Vector::I64(left), Vector::I64(right)) => sum!(left, right),
		(Vector::I32(left), Vector::I32(right)) => sum!(left, right),
		(Vector::I16(left), Vector::I16(right)) => sum!(left, right),
		(Vector::I8(left), Vector::I8(right)) => sum!(left, right),
		(Vector::U8(left), Vector::U8(right)) => sum!(left, right),
		_ => Ok(None),
	}
}

/// Returns the integer distance overflow result.
fn integer_distance_overflow() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("integer vector distance accumulation overflowed u128")
}

/// Returns the custom distance requires runtime result.
fn custom_distance_requires_runtime(function: FunctionId) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!(
		"custom vector distance function {function} requires the GrynnQL runtime",
	))
}

#[cfg(test)]
/// Regression tests for vector distance behavior.
mod tests {
	use super::exact_vector_distance;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{datastore::VectorDistance, values::Vector};

	/// Verifies computes builtin distances without hidden integer float
	/// accumulation.
	#[test]
	fn computes_builtin_distances_without_hidden_integer_float_accumulation() {
		let left = Vector::from(vec![0_i64, i64::MAX]);
		let right = Vector::from(vec![0_i64, i64::MAX - 4]);
		assert_eq!(
			exact_vector_distance(&left, &right, VectorDistance::Euclidean)
				.assert_ok()
				.partial_cmp(&4.0),
			Some(std::cmp::Ordering::Equal)
		);
		assert_eq!(
			exact_vector_distance(&left, &right, VectorDistance::Manhattan)
				.assert_ok()
				.partial_cmp(&4.0),
			Some(std::cmp::Ordering::Equal)
		);

		let left = Vector::from(vec![1.0_f32, 0.0]);
		let right = Vector::from(vec![0.0_f32, 1.0]);
		assert_eq!(
			exact_vector_distance(&left, &right, VectorDistance::Cosine)
				.assert_ok()
				.partial_cmp(&1.0),
			Some(std::cmp::Ordering::Equal)
		);
		assert_eq!(
			exact_vector_distance(&left, &right, VectorDistance::Minkowski(2.0))
				.assert_ok()
				.partial_cmp(&2.0_f64.sqrt()),
			Some(std::cmp::Ordering::Equal)
		);
	}

	/// Verifies rejects non finite dimension kind and zero cosine.
	#[test]
	fn rejects_non_finite_dimension_kind_and_zero_cosine() {
		assert!(
			exact_vector_distance(
				&Vector::from(vec![f32::NAN]),
				&Vector::from(vec![0.0_f32]),
				VectorDistance::Euclidean,
			)
			.is_err(),
		);
		assert!(
			exact_vector_distance(
				&Vector::from(vec![0.0_f32]),
				&Vector::from(vec![0.0_f32]),
				VectorDistance::Cosine,
			)
			.is_err(),
		);
		assert!(
			exact_vector_distance(
				&Vector::from(vec![1_i8]),
				&Vector::from(vec![1_u8]),
				VectorDistance::Euclidean,
			)
			.is_err(),
		);
	}

	/// Verifies builtin distances cover supported kinds and dimensions.
	#[test]
	fn builtin_distances_cover_supported_kinds_and_dimensions() {
		use crate::test_assertions::AssertResult as _;

		for dimension in [1_usize, 32, 128, 768] {
			let cases = [
				(Vector::F64(vec![1.0; dimension]), Vector::F64(vec![2.0; dimension])),
				(Vector::F32(vec![1.0; dimension]), Vector::F32(vec![2.0; dimension])),
				(
					Vector::F16(vec![half::f16::from_f32(1.0); dimension]),
					Vector::F16(vec![half::f16::from_f32(2.0); dimension]),
				),
				(Vector::I64(vec![1; dimension]), Vector::I64(vec![2; dimension])),
				(Vector::I32(vec![1; dimension]), Vector::I32(vec![2; dimension])),
				(Vector::I16(vec![1; dimension]), Vector::I16(vec![2; dimension])),
				(Vector::I8(vec![1; dimension]), Vector::I8(vec![2; dimension])),
				(Vector::U8(vec![1; dimension]), Vector::U8(vec![2; dimension])),
			];
			for (left, right) in cases {
				let euclidean =
					exact_vector_distance(&left, &right, VectorDistance::Euclidean).assert_ok();
				let manhattan =
					exact_vector_distance(&left, &right, VectorDistance::Manhattan).assert_ok();
				assert!((euclidean - (dimension as f64).sqrt()).abs() <= f64::EPSILON);
				assert_eq!(
					manhattan.partial_cmp(&(dimension as f64)),
					Some(std::cmp::Ordering::Equal)
				);
			}

			let left = Vector::F32(vec![1.0; dimension]);
			let right = Vector::F32(vec![2.0; dimension]);
			let minkowski =
				exact_vector_distance(&left, &right, VectorDistance::Minkowski(3.0)).assert_ok();
			let cosine = exact_vector_distance(&left, &right, VectorDistance::Cosine).assert_ok();
			let normalized =
				exact_vector_distance(&left, &right, VectorDistance::CosineNormalized).assert_ok();
			let inner_product =
				exact_vector_distance(&left, &right, VectorDistance::InnerProduct).assert_ok();
			assert!((minkowski - (dimension as f64).cbrt()).abs() <= f64::EPSILON * 8.0);
			assert!(cosine.abs() <= f64::EPSILON);
			assert!(normalized.abs() <= f64::EPSILON);
			assert_eq!(
				inner_product.partial_cmp(&(-2.0 * dimension as f64)),
				Some(std::cmp::Ordering::Equal)
			);
		}
	}
}
