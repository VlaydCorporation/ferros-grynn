//! Base property page ownership under the shared publication envelope.

use super::{
	AlignedPage,
	EncodeContext,
	FerrosGrynnResult,
	PAGE_SIZE,
	PageKey,
	PageLsn,
	PropertyRef,
	PropertyStoreBuilder,
	PublicationBudget,
	encode_property_map_bounded,
	invalid,
};
use crate::{gql_status::FerrosGrynnError, values::storage::StorageValue};

/// Sequential record scratch is separate from retained final frames/WAL copies.
pub(super) struct BaseProperties<'a> {
	/// Append-only physical packing shared with the full-generation builder.
	builder: PropertyStoreBuilder,
	/// Final value dictionaries shared with graph/index publication.
	context: &'a mut EncodeContext,
	/// One resource envelope survives all Base and graph preparation phases.
	pub(super) budget: &'a mut PublicationBudget,
}

impl<'a> BaseProperties<'a> {
	/// Starts an empty append at the WAL-visible property tail.
	pub(super) const fn new(
		start: u64,
		context: &'a mut EncodeContext,
		budget: &'a mut PublicationBudget,
	) -> Self {
		Self {
			builder: PropertyStoreBuilder::at_page(start),
			context,
			budget,
		}
	}

	/// Stores a changed identity without changing its canonical representation.
	///
	/// # Errors
	/// Propagates codec, packing, allocation and publication budget failures.
	pub(super) fn record(
		&mut self,
		record: &crate::values::RecordId,
	) -> FerrosGrynnResult<PropertyRef> {
		let maximum = self.encoding_budget()?;
		let (encoded, context_bytes) =
			StorageValue::encode_record_bounded(record, self.context, maximum)?;
		self.budget.retain(context_bytes)?;
		self.insert(encoded)
	}

	/// Stores one Base map while preserving the immutable property prefix.
	///
	/// # Errors
	/// Propagates codec, packing, allocation and publication budget failures.
	pub(super) fn properties(
		&mut self,
		properties: &crate::values::PropertyMap,
	) -> FerrosGrynnResult<PropertyRef> {
		let maximum = self.encoding_budget()?;
		let (encoded, context_bytes) =
			encode_property_map_bounded(properties, self.context, maximum)?;
		self.budget.retain(context_bytes)?;
		self.insert(encoded)
	}

	/// Keeps live page backing beside conversion DTO, context growth and
	/// encoder scratch.
	fn encoding_budget(&self) -> FerrosGrynnResult<usize> {
		let backing = self.builder.allocation_bytes()?;
		self.budget.scratch(backing)?;
		self.budget
			.remaining()?
			.checked_sub(backing)
			.ok_or_else(|| invalid("Base conversion leaves no scratch budget"))
	}

	/// Admits final frames and their WAL copies before growing page storage.
	fn insert(&mut self, encoded: Vec<u8>) -> FerrosGrynnResult<PropertyRef> {
		let additional = self.builder.additional_pages(encoded.len())?;
		if additional != 0 {
			self.budget.retain(
				additional
					.checked_mul(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())
					.ok_or_else(|| invalid("Base property frame bytes overflow usize"))?,
			)?;
			self.budget.stage_pages(additional)?;
		}
		// The source page Vec remains alive as final Box frames are produced.
		self.budget.scratch(
			self.builder
				.allocation_bytes()?
				.checked_add(encoded.capacity())
				.ok_or_else(|| invalid("Base property scratch bytes overflow usize"))?,
		)?;
		let maximum =
			self.budget.remaining()?.checked_sub(encoded.capacity()).ok_or_else(|| {
				FerrosGrynnError::storage_decode_limit_exceeded(
					"Base encoded record",
					encoded.capacity() as u64,
					0,
				)
			})?;
		self.builder.insert_bounded(&encoded, PageLsn::ZERO, maximum)
	}

	/// Counts initialized source frames before reserving the final key vector.
	pub(super) fn page_count(&self) -> usize {
		self.builder.page_count()
	}

	/// Rechecks live backing after another phase adds retained reservations.
	///
	/// # Errors
	/// Rejects a combined backing/scratch peak before the next allocation.
	pub(super) fn check_scratch(&self, extra: usize) -> FerrosGrynnResult<()> {
		self.budget.scratch(
			self.builder
				.allocation_bytes()?
				.checked_add(extra)
				.ok_or_else(|| invalid("Base backing scratch bytes overflow usize"))?,
		)
	}

	/// Seals pages without another source-frame allocation.
	///
	/// # Errors
	/// Propagates page sealing or append-contiguity failures.
	pub(super) fn finish(self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.builder.finish()
	}
}
