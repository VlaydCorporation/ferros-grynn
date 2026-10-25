//! Overlay payload conversion and append ownership, independent of Hot patches.

use super::super::{LocalState, LocalStore, layout::invalid, point_namespace::PublicationBudget};
use crate::{
	datastore::PropertyOverlay,
	gql_status::FerrosGrynnResult,
	storage::{
		atom_page::PropertyRef,
		graph_pages::{GraphOverlayStore, encode_overlay_bounded},
		graph_store::GraphFileSet,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		property_page::{
			LARGE_CHUNK_BYTES,
			MAX_INLINE_BYTES,
			decode_large_overlay,
			read_overlay_record,
		},
		types::PageKey,
	},
	values::storage::EncodeContext,
};

/// One graph-local allocator shares the final publication value dictionaries.
pub(in crate::storage::local_store) struct GraphOverlays<'a> {
	/// Older pages remain outside this WAL-visible append image.
	builder: GraphOverlayStore,
	/// Prepared dictionaries also serve subsequent index/catalog encoding.
	context: &'a mut EncodeContext,
}

impl<'a> GraphOverlays<'a> {
	/// Starts at the visible file tail without copying its immutable prefix.
	pub(super) const fn new(start: u64, context: &'a mut EncodeContext) -> Self {
		Self {
			builder: GraphOverlayStore::at_page(start),
			context,
		}
	}

	/// Converts one changed payload and stores its final physical reference.
	/// Backing capacity stays charged through publication, including spare
	/// slots.
	///
	/// # Errors
	/// Propagates conversion, canonical encoding, shared resource admission and
	/// append-page failures before publication.
	pub(in crate::storage::local_store) fn insert(
		&mut self,
		overlay: &PropertyOverlay,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<PropertyRef> {
		let (encoded, context_bytes) =
			encode_overlay_bounded(overlay, self.context, budget.remaining()?)?;
		budget.retain(context_bytes)?;
		let additional = self.builder.additional_pages(encoded.len())?;
		if additional != 0 {
			budget.retain(
				additional
					.checked_mul(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())
					.ok_or_else(|| invalid("overlay frame bytes overflow usize"))?,
			)?;
			budget.stage_pages(additional)?;
		}
		budget.scratch(encoded.capacity())?;
		let old_backing = self.builder.allocation_bytes()?;
		// The owner already retains old backing; the builder checks old + new.
		let maximum = budget
			.remaining()?
			.checked_sub(encoded.capacity())
			.and_then(|bytes| bytes.checked_add(old_backing))
			.ok_or_else(|| invalid("overlay append leaves no backing budget"))?;
		let reference = self.builder.insert_bounded(&encoded, PageLsn::ZERO, maximum)?;
		let growth = self
			.builder
			.allocation_bytes()?
			.checked_sub(old_backing)
			.ok_or_else(|| invalid("overlay append backing decreases"))?;
		// Retain spare capacity through Hot patches and final source-Vec
		// boxing.
		budget.retain(growth)?;
		Ok(reference)
	}

	/// Validates an accepted source without decoding the stored payload into
	/// another tree.
	///
	/// # Errors
	/// Rejects reference bounds, malformed pages, unequal canonical bytes and
	/// shared resource admission before the final Hot slot can replace its
	/// owner.
	pub(in crate::storage::local_store) fn validate_source(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		source: (GraphFileSet, u64),
		reference: PropertyRef,
		overlay: Option<&PropertyOverlay>,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<()> {
		let Some(overlay) = overlay else {
			return if reference == PropertyRef::None {
				Ok(())
			} else {
				Err(invalid("incidence source overlay presence disagrees with replay"))
			};
		};
		let (first, length, count) = match reference {
			PropertyRef::None => {
				return Err(invalid("incidence source overlay reference is absent"));
			}
			PropertyRef::Inline {
				page,
				length,
				..
			} => {
				let length = usize::try_from(length)
					.map_err(|_error| invalid("inline overlay length exceeds usize"))?;
				if length == 0 || length > MAX_INLINE_BYTES {
					return Err(invalid(
						"incidence source inline overlay exceeds its format limit",
					));
				}
				(page, length, 1)
			}
			PropertyRef::Large {
				first_page,
				length,
			} => {
				let length = usize::try_from(length)
					.map_err(|_error| invalid("large overlay length exceeds usize"))?;
				if length <= MAX_INLINE_BYTES {
					return Err(invalid("incidence source large overlay fits the inline format"));
				}
				(first_page, length, length.div_ceil(LARGE_CHUNK_BYTES))
			}
		};
		let (files, ceiling) = source;
		let count_u64 = u64::try_from(count)
			.map_err(|_error| invalid("source overlay page count exceeds u64"))?;
		if first.get().checked_add(count_u64).is_none_or(|end| end > ceiling) {
			return Err(invalid("incidence source overlay exceeds its header ceiling"));
		}
		// Append-only dictionaries preserve every identifier used by the old
		// payload.
		let (expected, context_bytes) =
			encode_overlay_bounded(overlay, self.context, budget.remaining()?)?;
		budget.retain(context_bytes)?;
		if expected.len() != length {
			return Err(invalid("incidence source overlay length disagrees with replay"));
		}
		match reference {
			PropertyRef::Inline {
				page,
				slot,
				length,
			} => {
				budget.scratch(
					expected
						.capacity()
						.checked_add(PAGE_SIZE)
						.ok_or_else(|| invalid("source overlay scratch overflows usize"))?,
				)?;
				let loaded = store
					.read_update_page(state, PageKey::new(files.space(), files.overlays, page))?;
				if read_overlay_record(&loaded, page, slot, length)? != expected.as_slice() {
					return Err(invalid("incidence source overlay bytes disagree with replay"));
				}
			}
			PropertyRef::Large {
				first_page,
				length,
			} => {
				let fixed = expected
					.capacity()
					.checked_add(expected.len())
					.and_then(|bytes| bytes.checked_add(PAGE_SIZE))
					.ok_or_else(|| invalid("source overlay buffers overflow usize"))?;
				budget.scratch(
					count
						.checked_mul(PAGE_SIZE)
						.and_then(|bytes| bytes.checked_add(fixed))
						.ok_or_else(|| invalid("source overlay pages overflow usize"))?,
				)?;
				let mut pages = Vec::new();
				pages
					.try_reserve_exact(count)
					.map_err(|_error| invalid("source overlay pages allocation failed"))?;
				budget.scratch(
					pages
						.capacity()
						.checked_mul(PAGE_SIZE)
						.and_then(|bytes| bytes.checked_add(fixed))
						.ok_or_else(|| invalid("source overlay backing overflows usize"))?,
				)?;
				pages.resize_with(count, AlignedPage::zeroed);
				for (index, page) in pages.iter_mut().enumerate() {
					let offset = u64::try_from(index)
						.map_err(|_error| invalid("source overlay offset exceeds u64"))?;
					// The complete range was checked against the old header
					// before allocation.
					let number = PageNo::new(
						first_page
							.get()
							.checked_add(offset)
							.ok_or_else(|| invalid("source overlay address overflows"))?,
					);
					store.read_update_page_into(
						state,
						PageKey::new(files.space(), files.overlays, number),
						page,
					)?;
				}
				let actual = decode_large_overlay(first_page, &pages, length, expected.len())?;
				let actual_peak = pages
					.capacity()
					.checked_mul(PAGE_SIZE)
					.and_then(|bytes| bytes.checked_add(expected.capacity()))
					.and_then(|bytes| bytes.checked_add(actual.capacity()))
					.and_then(|bytes| bytes.checked_add(PAGE_SIZE))
					.ok_or_else(|| invalid("source overlay actual backing overflows usize"))?;
				budget.scratch(actual_peak)?;
				if actual != expected {
					return Err(invalid("incidence source overlay bytes disagree with replay"));
				}
			}
			PropertyRef::None => {
				return Err(invalid("incidence source overlay reference disappeared"));
			}
		}
		Ok(())
	}

	/// Seals the contiguous append image without another source-page
	/// allocation.
	///
	/// # Errors
	/// Propagates page sealing and append-contiguity failures.
	pub(super) fn finish(self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.builder.finish()
	}
}
