//! Internal traverser state and bounded bulking helpers.

use std::{
	hash::{Hash, Hasher},
	num::NonZeroU64,
};

use fg_runtime::__internal::ValueInternal as _;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{PathTraceId, SnapshotId},
	values::{Number, Object, Uuid, Value},
};

/// One private traversal-stream element.
#[derive(Debug, Clone)]
pub(super) struct Traverser {
	/// Current value carried by the traverser.
	pub(super) current: Value,
	/// Optional predecessor-chain identifier for the current traverser.
	pub(super) trace: Option<PathTraceId>,
	/// Graph identity from which the retained path trace originated.
	pub(super) trace_origin: Option<Uuid>,
	/// Snapshot in which the retained path trace was observed.
	pub(super) trace_snapshot: Option<SnapshotId>,
	/// Accumulated numeric cost of the retained traversal path.
	pub(super) trace_cost: Number,
	/// Unnamed value carried as mutable traverser-local state.
	pub(super) sack: Value,
	/// Named values carried as mutable traverser-local state.
	pub(super) named_sacks: Object,
	/// Lexical bindings carried with the traverser.
	pub(super) env: Object,
	/// Number of equivalent traversers represented by this entry.
	pub(super) bulk: NonZeroU64,
}

impl Traverser {
	/// Constructs `Traverser` from the supplied initial state.
	pub(super) fn new(current: Value) -> Self {
		Self {
			current,
			trace: None,
			trace_origin: None,
			trace_snapshot: None,
			trace_cost: Number::Int(0),
			sack: Value::None,
			named_sacks: Object::new(),
			env: Object::new(),
			bulk: NonZeroU64::MIN,
		}
	}

	/// Reports whether the operation can merge for `Traverser`.
	pub(super) fn can_merge(&self, other: &Self) -> bool {
		self.trace.is_none()
			&& other.trace.is_none()
			&& self.current == other.current
			&& match (self.trace_cost, other.trace_cost) {
				(Number::Int(left), Number::Int(right)) => left == right,
				(Number::Float(left), Number::Float(right)) => left.to_bits() == right.to_bits(),
				(Number::Decimal(left), Number::Decimal(right)) => {
					left.serialize() == right.serialize()
				}
				_ => false,
			}
			&& self.sack == other.sack
			&& self.named_sacks == other.named_sacks
			&& self.env == other.env
	}

	/// Returns merge for `Traverser`.
	pub(super) fn merge(&mut self, other: &Self) -> FerrosGrynnResult<()> {
		let bulk = self
			.bulk
			.get()
			.checked_add(other.bulk.get())
			.and_then(NonZeroU64::new)
			.ok_or_else(FerrosGrynnError::traversal_bulk_overflow)?;
		self.bulk = bulk;
		Ok(())
	}

	/// Merges fingerprint for `Traverser`.
	pub(super) fn merge_fingerprint(&self) -> u64 {
		let mut hasher = rapidhash::fast::RapidHasher::default();
		self.current.hash(&mut hasher);
		if !matches!(self.trace_cost, Number::Int(0)) {
			std::mem::discriminant(&self.trace_cost).hash(&mut hasher);
			self.trace_cost.hash(&mut hasher);
		}
		self.sack.hash(&mut hasher);
		self.named_sacks.hash(&mut hasher);
		self.env.hash(&mut hasher);
		hasher.finish()
	}

	/// Configures single bulk for `Traverser`.
	pub(super) fn with_single_bulk(mut self) -> Self {
		self.bulk = NonZeroU64::MIN;
		self
	}

	/// Returns estimated heap bytes for `Traverser`.
	pub(super) fn estimated_heap_bytes(&self) -> usize {
		std::mem::size_of::<Self>()
			.saturating_add(self.current.estimated_heap_bytes())
			.saturating_add(self.sack.estimated_heap_bytes())
			.saturating_add(
				self.named_sacks
					.iter()
					.chain(self.env.iter())
					.map(|(name, value)| name.len().saturating_add(value.estimated_heap_bytes()))
					.fold(0_usize, usize::saturating_add),
			)
	}
}

#[cfg(test)]
/// Regression tests for model behavior.
mod tests {
	use super::*;

	/// Verifies bulking preserves exact cost prefix representation.
	#[test]
	fn bulking_preserves_exact_cost_prefix_representation() {
		let seed = Traverser::new(Value::None);
		assert!(seed.can_merge(&seed.clone()));
		for prefix in
			[Number::Int(1), Number::Float(0.0), Number::Decimal(rust_decimal::Decimal::ZERO)]
		{
			let mut other = seed.clone();
			other.trace_cost = prefix;
			assert!(!seed.can_merge(&other));
			assert!(other.can_merge(&other.clone()));
		}
	}
}
