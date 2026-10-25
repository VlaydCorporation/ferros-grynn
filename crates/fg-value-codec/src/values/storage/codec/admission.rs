//! One logical reader policy shared by decoding and DTO preflight.

use fg_runtime::__internal::GeometryInternal as _;

use super::{DecodeLimits, limit_error};
use crate::{gql_status::FerrosGrynnResult, values::Geometry};

/// Decoder charges differ from allocator capacity and writer clone estimates.
pub(in crate::values::storage) struct DecodeAdmission {
	/// Operation-specific reader policy; defaults are not format constants.
	pub(super) limits: DecodeLimits,
	/// Retained logical charge, temporarily raised during geometry validation.
	pub(super) allocated: usize,
}

impl DecodeAdmission {
	/// Starts an independent record without charging the caller's input bytes.
	pub(in crate::values::storage) const fn new(limits: DecodeLimits) -> Self {
		Self {
			limits,
			allocated: 0,
		}
	}

	/// Preserves the reader's inclusive depth boundary.
	pub(in crate::values::storage) fn check_depth(&self, depth: usize) -> FerrosGrynnResult<()> {
		if depth > self.limits.max_depth {
			return Err(limit_error("nesting depth", self.limits.max_depth, depth));
		}
		Ok(())
	}

	/// Checks a collection before its typed vector is reserved.
	pub(in crate::values::storage) fn check_collection_len(
		&self,
		count: usize,
	) -> FerrosGrynnResult<()> {
		if count > self.limits.max_collection_len {
			return Err(limit_error("collection length", self.limits.max_collection_len, count));
		}
		Ok(())
	}

	/// Checks an already parsed length without touching the source cursor.
	pub(in crate::values::storage) fn check_length(
		name: &str,
		maximum: usize,
		length: usize,
	) -> FerrosGrynnResult<()> {
		if length > maximum {
			return Err(limit_error(name, maximum, length));
		}
		Ok(())
	}

	/// Counts geometry positions and container members using the reader policy.
	pub(super) fn geometry_elements(elements: &mut usize, count: usize) -> FerrosGrynnResult<()> {
		*elements = elements
			.checked_add(count)
			.filter(|count| *count <= Geometry::MAX_ELEMENTS)
			.ok_or_else(|| limit_error("geometry elements", Geometry::MAX_ELEMENTS, usize::MAX))?;
		Ok(())
	}

	/// Charges the exact logical backing used by a typed decoder vector.
	pub(super) fn charge_array<T>(&mut self, count: usize) -> FerrosGrynnResult<()> {
		let bytes = count.checked_mul(size_of::<T>()).ok_or_else(|| {
			limit_error("allocation bytes", self.limits.max_total_allocation, usize::MAX)
		})?;
		self.charge(bytes)
	}

	/// Rejects overflow and excessive charge before changing the counter.
	pub(super) fn charge(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		let total = self.allocated.checked_add(bytes).ok_or_else(|| {
			limit_error("allocation bytes", self.limits.max_total_allocation, usize::MAX)
		})?;
		if total > self.limits.max_total_allocation {
			return Err(limit_error("allocation bytes", self.limits.max_total_allocation, total));
		}
		self.allocated = total;
		Ok(())
	}
}
