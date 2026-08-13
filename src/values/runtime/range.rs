use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	ops::Bound,
};

use super::Value;
use crate::gql_status::FerrosGrynnError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Range {
	start: Bound<Box<Value>>,
	end: Bound<Box<Value>>,
}

impl PartialOrd for Range {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Range {
	fn cmp(&self, other: &Self) -> Ordering {
		compare_start(&self.start, &other.start).then_with(|| compare_end(&self.end, &other.end))
	}
}

impl Range {
	pub fn new(start: Bound<Value>, end: Bound<Value>) -> Result<Self, FerrosGrynnError> {
		validate_bounds(&start, &end)?;
		Ok(Self {
			start: box_bound(start),
			end: box_bound(end),
		})
	}

	pub fn closed<Start, End>(start: Start, end: End) -> Result<Self, FerrosGrynnError>
	where
		Start: Into<Value>,
		End: Into<Value>,
	{
		Self::new(Bound::Included(start.into()), Bound::Included(end.into()))
	}

	pub fn start(&self) -> Bound<&Value> {
		borrow_bound(&self.start)
	}

	pub fn end(&self) -> Bound<&Value> {
		borrow_bound(&self.end)
	}

	/// Moves both endpoints out of the range without cloning their values.
	pub fn into_bounds(self) -> (Bound<Value>, Bound<Value>) {
		(unbox_bound(self.start), unbox_bound(self.end))
	}

	pub fn contains(&self, value: &Value) -> bool {
		let compatible = self
			.start()
			.into_value()
			.or_else(|| self.end().into_value())
			.is_none_or(|bound| bound.kind().is_compatible_with(value.kind()));
		if !compatible {
			return false;
		}

		let after_start = match self.start() {
			Bound::Included(start) => value >= start,
			Bound::Excluded(start) => value > start,
			Bound::Unbounded => true,
		};
		let before_end = match self.end() {
			Bound::Included(end) => value <= end,
			Bound::Excluded(end) => value < end,
			Bound::Unbounded => true,
		};
		after_start && before_end
	}
}

fn validate_bounds(start: &Bound<Value>, end: &Bound<Value>) -> Result<(), FerrosGrynnError> {
	let Some(start_value) = bound_value(start) else {
		return Ok(());
	};
	let Some(end_value) = bound_value(end) else {
		return Ok(());
	};

	if !start_value.kind().is_compatible_with(end_value.kind()) || start_value > end_value {
		return Err(FerrosGrynnError::conversion_error(
			format!("{start_value}..{end_value}"),
			"Range",
		));
	}
	Ok(())
}

const fn bound_value(bound: &Bound<Value>) -> Option<&Value> {
	match bound {
		Bound::Included(value) | Bound::Excluded(value) => Some(value),
		Bound::Unbounded => None,
	}
}

fn box_bound(bound: Bound<Value>) -> Bound<Box<Value>> {
	match bound {
		Bound::Included(value) => Bound::Included(Box::new(value)),
		Bound::Excluded(value) => Bound::Excluded(Box::new(value)),
		Bound::Unbounded => Bound::Unbounded,
	}
}

fn unbox_bound(bound: Bound<Box<Value>>) -> Bound<Value> {
	match bound {
		Bound::Included(value) => Bound::Included(*value),
		Bound::Excluded(value) => Bound::Excluded(*value),
		Bound::Unbounded => Bound::Unbounded,
	}
}

fn borrow_bound(bound: &Bound<Box<Value>>) -> Bound<&Value> {
	match bound {
		Bound::Included(value) => Bound::Included(value.as_ref()),
		Bound::Excluded(value) => Bound::Excluded(value.as_ref()),
		Bound::Unbounded => Bound::Unbounded,
	}
}

fn compare_start(left: &Bound<Box<Value>>, right: &Bound<Box<Value>>) -> Ordering {
	match (left, right) {
		(Bound::Unbounded, Bound::Unbounded) => Ordering::Equal,
		(Bound::Unbounded, _) => Ordering::Less,
		(_, Bound::Unbounded) => Ordering::Greater,
		(Bound::Included(left), Bound::Included(right))
		| (Bound::Excluded(left), Bound::Excluded(right)) => left.cmp(right),
		(Bound::Included(left), Bound::Excluded(right)) => left.cmp(right).then(Ordering::Less),
		(Bound::Excluded(left), Bound::Included(right)) => left.cmp(right).then(Ordering::Greater),
	}
}

fn compare_end(left: &Bound<Box<Value>>, right: &Bound<Box<Value>>) -> Ordering {
	match (left, right) {
		(Bound::Unbounded, Bound::Unbounded) => Ordering::Equal,
		(Bound::Unbounded, _) => Ordering::Greater,
		(_, Bound::Unbounded) => Ordering::Less,
		(Bound::Included(left), Bound::Included(right))
		| (Bound::Excluded(left), Bound::Excluded(right)) => left.cmp(right),
		(Bound::Included(left), Bound::Excluded(right)) => left.cmp(right).then(Ordering::Less),
		(Bound::Excluded(left), Bound::Included(right)) => left.cmp(right).then(Ordering::Greater),
	}
}

trait BoundExt<T> {
	fn into_value(self) -> Option<T>;
}

impl<T> BoundExt<T> for Bound<T> {
	fn into_value(self) -> Option<T> {
		match self {
			Bound::Included(value) | Bound::Excluded(value) => Some(value),
			Bound::Unbounded => None,
		}
	}
}

impl Display for Range {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self.start() {
			Bound::Included(value) => write!(f, "[{value}")?,
			Bound::Excluded(value) => write!(f, "({value}")?,
			Bound::Unbounded => f.write_str("(-inf")?,
		}
		f.write_str(", ")?;
		match self.end() {
			Bound::Included(value) => write!(f, "{value}]"),
			Bound::Excluded(value) => write!(f, "{value})"),
			Bound::Unbounded => f.write_str("+inf)"),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::Range;

	#[test]
	fn validates_bound_domains_and_order() {
		assert!(Range::closed(1_i64, 2.0_f64).is_ok());
		assert!(Range::closed(2_i64, 1_i64).is_err());
		assert!(Range::closed(1_i64, "two").is_err());
	}

	#[test]
	fn checks_inclusion() {
		let range = Range::closed(1_i64, 3_i64).unwrap();
		assert!(range.contains(&2_i64.into()));
		assert!(!range.contains(&4_i64.into()));
		assert!(!range.contains(&"2".into()));
	}
}
