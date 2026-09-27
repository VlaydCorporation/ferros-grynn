//! Internal traverser state and bounded bulking helpers.

use std::{
	hash::{Hash, Hasher},
	num::NonZeroU64,
};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{PathTraceId, SnapshotId},
	values::{Number, Object, Uuid, Value},
};

/// One private traversal-stream element.
#[derive(Debug, Clone)]
pub(super) struct Traverser {
	pub(super) current: Value,
	pub(super) trace: Option<PathTraceId>,
	pub(super) trace_origin: Option<Uuid>,
	pub(super) trace_snapshot: Option<SnapshotId>,
	pub(super) trace_cost: Number,
	pub(super) sack: Value,
	pub(super) named_sacks: Object,
	pub(super) env: Object,
	pub(super) bulk: NonZeroU64,
}

impl Traverser {
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

	pub(super) fn can_merge(&self, other: &Self) -> bool {
		self.trace.is_none()
			&& other.trace.is_none()
			&& self.current == other.current
			&& self.sack == other.sack
			&& self.named_sacks == other.named_sacks
			&& self.env == other.env
	}

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

	pub(super) fn merge_fingerprint(&self) -> u64 {
		let mut hasher = rapidhash::fast::RapidHasher::default();
		self.current.hash(&mut hasher);
		self.sack.hash(&mut hasher);
		self.named_sacks.hash(&mut hasher);
		self.env.hash(&mut hasher);
		hasher.finish()
	}

	pub(super) fn with_single_bulk(mut self) -> Self {
		self.bulk = NonZeroU64::MIN;
		self
	}

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
