//! Borrowed value traversal shared by root evaluators and reactive slot reads.

use std::mem::size_of;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Value,
};

/// Charges the complete owned representation before allocating a clone.
pub(super) fn admit_value(
	value: &Value,
	limit: usize,
	admit: &dyn Fn(usize) -> FerrosGrynnResult<()>,
	work: &dyn Fn(u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	admit(2 * size_of::<&Value>())?;
	let mut pending = vec![value];
	while let Some(value) = pending.pop() {
		work(1)?;
		let count = match value {
			Value::Array(values) => values.len(),
			Value::Set(values) => values.len(),
			Value::Object(values) => values.len(),
			Value::Range(_) => 2,
			Value::Path(path) => {
				work(path.steps().len() as u64)?;
				0
			}
			_ => 0,
		};
		work(count as u64)?;
		admit(
			count
				.checked_mul(2 * size_of::<Value>() + 128 + 2 * size_of::<&Value>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE collection scratch"))?,
		)?;
		pending.try_reserve(count).map_err(|cause| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"PURE value traversal",
				limit as u64,
			)
			.with_technical_source(cause)
		})?;
		match value {
			Value::Array(values) => pending.extend(values.iter()),
			Value::Set(values) => pending.extend(values.iter()),
			Value::Object(values) => {
				for (name, value) in values.iter() {
					admit(name.len())?;
					pending.push(value);
				}
			}
			Value::Range(range) => {
				for bound in [range.start(), range.end()] {
					if let std::ops::Bound::Included(value) | std::ops::Bound::Excluded(value) =
						bound
					{
						pending.push(value);
					}
				}
			}
			_ => admit(value.estimated_heap_bytes())?,
		}
	}
	Ok(())
}
