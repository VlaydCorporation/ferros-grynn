//! One operation's DTO/context ownership and sequential encoding scratch.

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Logical reservations exclude the borrowed input and initially prepared
/// context.
pub(super) struct EncodeBudget {
	/// Ceiling supplied by the physical publication owner.
	maximum: usize,
	/// DTO backing and newly retained context, alive until encoding finishes.
	retained: usize,
	/// Only this portion survives after the DTO and its scratch are dropped.
	context: usize,
}

impl EncodeBudget {
	/// Starts a record without charging the caller's existing dictionaries
	/// twice.
	pub(super) const fn new(maximum: usize) -> Self {
		Self {
			maximum,
			retained: 0,
			context: 0,
		}
	}

	/// Admits an overlap without retaining sequential scratch between records.
	pub(super) fn scratch(&self, bytes: usize) -> FerrosGrynnResult<()> {
		let actual = Self::add(self.retained, bytes)?;
		if actual > self.maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"runtime storage encoding",
				actual as u64,
				self.maximum as u64,
			));
		}
		Ok(())
	}

	/// Reserves DTO backing before allocating or copying it.
	pub(super) fn retain(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		self.scratch(bytes)?;
		self.retained += bytes;
		Ok(())
	}

	/// Precharges context growth and old/new overlap before dictionary
	/// mutation.
	pub(super) fn retain_context(
		&mut self,
		growth: usize,
		overlap: usize,
	) -> FerrosGrynnResult<()> {
		let context = Self::add(self.context, growth)?;
		self.scratch(Self::add(growth, overlap)?)?;
		self.retained += growth;
		self.context = context;
		Ok(())
	}

	/// Returns only newly retained context ownership, excluding transient DTOs.
	pub(super) const fn context_bytes(&self) -> usize {
		self.context
	}

	/// Leaves output and encoder scratch within the same live ownership
	/// envelope.
	pub(super) fn remaining(&self) -> usize {
		self.maximum - self.retained
	}

	/// Allocates one exact requested backing after its logical admission.
	pub(super) fn vector<T>(&mut self, count: usize) -> FerrosGrynnResult<Vec<T>> {
		self.retain(Self::array_bytes::<T>(count)?)?;
		let mut result = Vec::new();
		result.try_reserve_exact(count).map_err(|_error| {
			FerrosGrynnError::invalid_storage_value_encoding(0, "storage DTO allocation failed")
		})?;
		Ok(result)
	}

	/// Rejects length arithmetic overflow before choosing an allocation size.
	pub(super) fn array_bytes<T>(count: usize) -> FerrosGrynnResult<usize> {
		count.checked_mul(size_of::<T>()).ok_or_else(Self::overflow)
	}

	/// Shares checked arithmetic with the context and reserved Set writer.
	pub(super) fn add(left: usize, right: usize) -> FerrosGrynnResult<usize> {
		left.checked_add(right).ok_or_else(Self::overflow)
	}

	/// Identifies an unrepresentable logical reservation without saturating it.
	fn overflow() -> FerrosGrynnError {
		FerrosGrynnError::operation_overflow("runtime storage encoding")
	}
}
