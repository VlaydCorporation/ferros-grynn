//! Shared checked admission for tensor and analytical kernels.
#![allow(
	clippy::exhaustive_structs,
	clippy::exhaustive_enums,
	reason = "Internal versioned engine/runtime protocol; not re-exported by the SDK"
)]
#![allow(
	clippy::redundant_pub_crate,
	reason = "Crate-private controls are shared by tensor and analytics consumers"
)]

use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Resource ceilings for one graph operation, independent of its backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraphAlgorithmLimits {
	/// Aggregate retained and scratch admission, in bytes.
	pub memory_bytes: usize,
	/// Maximum incidence visits, arithmetic steps, and enumeration expansions.
	pub max_work: u64,
	/// Maximum complete paths retained by an all-shortest operation.
	pub max_paths: usize,
}

impl Default for GraphAlgorithmLimits {
	/// Constructs the default initial state of `GraphAlgorithmLimits`.
	fn default() -> Self {
		Self {
			memory_bytes: 256 * 1024 * 1024,
			max_work: 100_000_000,
			max_paths: 1_000_000,
		}
	}
}

/// Conservatively accounts overlapping allocations before they are made.
#[allow(
	clippy::partial_pub_fields,
	reason = "Only capture counters are shared; cancellation and provenance stay owner-private"
)]
pub struct Control<'a> {
	/// Per-operation hard resource ceilings.
	pub limits: GraphAlgorithmLimits,
	/// Conservatively admitted retained and scratch bytes.
	pub bytes: usize,
	/// Conservatively admitted algorithm operations.
	pub work: u64,
	/// Caller-owned cancellation/deadline callback.
	check: &'a mut dyn FnMut() -> FerrosGrynnResult<()>,
}

impl<'a> Control<'a> {
	/// Initializes this snapshot-bound view or checked operation state.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn new(
		limits: GraphAlgorithmLimits,
		check: &'a mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		if limits.memory_bytes == 0 || limits.max_work == 0 {
			return Err(FerrosGrynnError::invalid_argument(
				"graph algorithm limits must be positive",
			));
		}
		let mut result = Self {
			limits,
			bytes: 0,
			work: 0,
			check,
		};
		result.admit(1024)?;
		Ok(result)
	}

	/// Runs the caller cancellation or deadline check.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn poll(&mut self) -> FerrosGrynnResult<()> {
		(self.check)()
	}

	/// Admits one algorithm operation before performing its work.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn step(&mut self) -> FerrosGrynnResult<()> {
		self.work(1)
	}

	/// Admits an aggregate operation bound before entering a kernel.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn work(&mut self, count: usize) -> FerrosGrynnResult<()> {
		self.poll()?;
		self.work = self
			.work
			.checked_add(u64::try_from(count).map_err(|_cause| overflow())?)
			.filter(|next| *next <= self.limits.max_work)
			.ok_or_else(|| resource("graph algorithm work", self.limits.max_work))?;
		Ok(())
	}

	/// Admits bytes before allocating retained or scratch data.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn admit(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		self.poll()?;
		self.bytes = self
			.bytes
			.checked_add(bytes)
			.filter(|next| *next <= self.limits.memory_bytes)
			.ok_or_else(|| resource("graph algorithm bytes", self.limits.memory_bytes as u64))?;
		Ok(())
	}

	/// Reserves exactly an admitted logical array extent.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn array<T>(&mut self, count: usize) -> FerrosGrynnResult<Vec<T>> {
		self.admit(count.checked_mul(size_of::<T>()).ok_or_else(overflow)?)?;
		let mut values = Vec::new();
		values.try_reserve_exact(count).map_err(|cause| {
			resource("graph algorithm bytes", self.limits.memory_bytes as u64)
				.with_technical_source(cause)
		})?;
		Ok(values)
	}

	/// Admits and initializes a reusable fixed-size operation buffer.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn filled<T: Copy>(&mut self, count: usize, value: T) -> FerrosGrynnResult<Vec<T>> {
		self.work(count)?;
		let mut values = self.array(count)?;
		values.resize(count, value);
		Ok(values)
	}

	/// Charges a conservative comparison bound before in-place sorting.
	///
	/// # Errors
	/// Propagates validation, resource-admission or callback errors from the
	/// operation.
	pub fn sort(&mut self, count: usize) -> FerrosGrynnResult<()> {
		let rounds = usize::try_from(count.checked_add(1).ok_or_else(overflow)?.ilog2())
			.map_err(|_cause| overflow())?
			.checked_add(1)
			.ok_or_else(overflow)?;
		self.work(
			count.checked_mul(rounds).and_then(|work| work.checked_mul(4)).ok_or_else(overflow)?,
		)
	}
}

/// Creates the shared typed size or numeric overflow error.
pub fn overflow() -> FerrosGrynnError {
	FerrosGrynnError::operation_overflow("graph tensor size")
}
/// Creates a typed resource-limit error with its limiting bound.
pub fn resource(subject: &str, limit: u64) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded(subject, limit)
}
/// Creates a typed invalid-input error at the operation boundary.
pub fn invalid(message: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(message)
}
