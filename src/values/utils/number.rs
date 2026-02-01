use crate::gql_status::{FerrosGrynnError, FerrosGrynnErrorBuilder, FerrosGrynnResult, Status};
use crate::values::storable::{ScalarValue, Value};
use crate::values::{AnyValue, AnyValueExt};
use num::Num;

pub fn cast_to_i64(name: &str, value: AnyValue) -> FerrosGrynnResult<i64> {
	if let AnyValue::Value(value) = &value
		&& let Value::Scalar(scalar) = value
		&& let ScalarValue::Number(number) = scalar
		&& number.is_integral()
	{
		Ok(number.as_i64())
	} else {
		Err(FerrosGrynnError::invalid_argument(format!(
			"{} must be an integral value, but was a {}",
			name,
			value.get_type_name()
		)))
	}
}

pub fn safe_f64_to_i64(name: &str, x: f64) -> FerrosGrynnResult<i64> {
	if !x.is_finite() {
		Err(FerrosGrynnErrorBuilder::from_status(Status::OverflowError {
			operation: "cast".to_string(),
		})
		.with_cause(FerrosGrynnError::infinite_fp())
		.build())
	} else if x < i64::MIN as f64 || x > i64::MAX as f64 {
		Err(FerrosGrynnErrorBuilder::from_status(Status::OverflowError {
			operation: "cast".to_string(),
		})
		.with_cause(FerrosGrynnError::out_of_range(
			name.to_string(),
			"float".to_string(),
			i64::MIN,
			i64::MAX,
			x.to_string(),
		))
		.build())
	} else {
		Ok(x.trunc() as i64)
	}
}

pub fn safe_cast_to_f64(name: &str, value: AnyValue, default: f64) -> FerrosGrynnResult<f64> {
	if value.is_no_value() {
		Ok(default)
	} else if let AnyValue::Value(value) = &value
		&& let Value::Scalar(scalar) = value
		&& let ScalarValue::Number(number) = scalar
		&& number.is_integral()
	{
		Ok(number.as_f64())
	} else {
		Err(FerrosGrynnError::invalid_argument(format!(
			"{} must be a number value, but was a {}",
			name,
			value.get_type_name()
		)))
	}
}