//! Exact owned-value representation comparison for durable result images.

use super::{Number, Object, Value};

/// Numeric variants and decimal scale remain observable to stored consumers.
#[allow(clippy::float_cmp, reason = "historical image comparison uses exact numeric cells")]
pub fn same_number(left: Number, right: Number) -> bool {
	match (left, right) {
		(Number::Int(left), Number::Int(right)) => left == right,
		(Number::Float(left), Number::Float(right)) => {
			left == right || left.is_nan() && right.is_nan()
		}
		(Number::Decimal(left), Number::Decimal(right)) => {
			left.mantissa() == right.mantissa() && left.scale() == right.scale()
		}
		_ => false,
	}
}

/// Preserves numeric, timezone, duration and vector representation recursively.
pub fn same_value(left: &Value, right: &Value) -> bool {
	match (left, right) {
		(Value::Number(left), Value::Number(right)) => same_number(*left, *right),
		(Value::Object(left), Value::Object(right)) => same_object(left, right),
		(Value::Array(left), Value::Array(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|(left, right)| same_value(left, right))
		}
		(Value::Set(left), Value::Set(right)) => {
			left.len() == right.len()
				&& left.iter().zip(right).all(|(left, right)| same_value(left, right))
		}
		(Value::Range(left), Value::Range(right)) => {
			same_bound(left.start(), right.start()) && same_bound(left.end(), right.end())
		}
		(Value::Vector(left), Value::Vector(right)) => left.kind() == right.kind() && left == right,
		(Value::DateTime(left), Value::DateTime(right)) => {
			left.as_inner().timestamp() == right.as_inner().timestamp()
				&& left.as_inner().time_zone() == right.as_inner().time_zone()
		}
		(Value::Duration(left), Value::Duration(right)) => {
			duration_units(*left) == duration_units(*right)
		}
		(Value::AtomOccurrence(left), Value::AtomOccurrence(right)) => same_occurrence(left, right),
		(Value::Path(left), Value::Path(right)) => {
			left.origin() == right.origin()
				&& left.snapshot() == right.snapshot()
				&& left.steps().len() == right.steps().len()
				&& left.steps().iter().zip(right.steps()).all(|(left, right)| {
					same_occurrence(left.atom(), right.atom())
						&& left.direction() == right.direction()
						&& left.role() == right.role()
						&& match (left.via_edge(), right.via_edge()) {
							(None, None) => true,
							(Some(left), Some(right)) => same_occurrence(left, right),
							_ => false,
						}
				})
				&& match (left.cost(), right.cost()) {
					(None, None) => true,
					(Some(left), Some(right)) => same_number(left, right),
					_ => false,
				}
		}
		_ => left.kind() == right.kind() && left == right,
	}
}

/// Historical descriptor names and modes remain visible although identity
/// equality ignores them.
fn same_occurrence(left: &super::AtomOccurrenceRef, right: &super::AtomOccurrenceRef) -> bool {
	left.atom() == right.atom()
		&& left.graph().id() == right.graph().id()
		&& left.graph().name() == right.graph().name()
		&& left.graph().kind() == right.graph().kind()
		&& left.graph().storage_mode() == right.graph().storage_mode()
}

/// Object keys retain the canonical ordered runtime representation.
pub fn same_object(left: &Object, right: &Object) -> bool {
	left.len() == right.len()
		&& left.iter().zip(right).all(|((lk, lv), (rk, rv))| lk == rk && same_value(lv, rv))
}

/// Inclusive and exclusive range markers stay distinct.
fn same_bound(left: std::ops::Bound<&Value>, right: std::ops::Bound<&Value>) -> bool {
	match (left, right) {
		(std::ops::Bound::Unbounded, std::ops::Bound::Unbounded) => true,
		(std::ops::Bound::Included(left), std::ops::Bound::Included(right))
		| (std::ops::Bound::Excluded(left), std::ops::Bound::Excluded(right)) => same_value(left, right),
		_ => false,
	}
}

/// Stored durations preserve each signed component without unit folding.
fn duration_units(value: super::Duration) -> [i64; 10] {
	let span = value.as_inner();
	[
		i64::from(span.get_years()),
		i64::from(span.get_months()),
		i64::from(span.get_weeks()),
		i64::from(span.get_days()),
		i64::from(span.get_hours()),
		span.get_minutes(),
		span.get_seconds(),
		span.get_milliseconds(),
		span.get_microseconds(),
		span.get_nanoseconds(),
	]
}
