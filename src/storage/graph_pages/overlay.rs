use super::{AlignedPage, FerrosGrynnError, FerrosGrynnResult, PageLsn, PageNo, invalid_page};
use crate::{
	datastore::{OverlayValue, PropertyOverlay},
	storage::{
		atom_page::PropertyRef,
		property_page::{
			LARGE_CHUNK_BYTES,
			MAX_INLINE_BYTES,
			PropertyPageBuilder,
			append_large_overlay,
			decode_large_overlay,
			read_overlay_record as read_inline_overlay,
		},
	},
	values::{
		PropertyKeyId,
		storage::{
			DecodeContext,
			EncodeContext,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

/// Encodes one non-empty sparse overlay in strict key order.
pub(in crate::storage) fn encode_overlay(
	overlay: &PropertyOverlay,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	if overlay.iter().len() == 0 {
		return Err(invalid_page(0, "an empty graph overlay must not be stored"));
	}
	let mut output = Vec::new();
	write_uleb(
		&mut output,
		u64::try_from(overlay.iter().len())
			.map_err(|_error| invalid_page(0, "overlay entry count exceeds u64"))?,
	);
	for (key, state) in overlay.iter() {
		output.extend_from_slice(&key.get().to_le_bytes());
		match state {
			OverlayValue::Hidden => output.push(0),
			OverlayValue::Value(value) => {
				output.push(1);
				let storage = StorageValue::from_runtime(value.clone(), context)?;
				output.extend_from_slice(&CanonicalCodec::encode(storage.as_ref())?);
			}
		}
	}
	Ok(output)
}

/// Encodes sparse framing beside child DTOs, growing output and new context.
///
/// # Errors
/// Rejects reader limits and resource excess before the corresponding copy or
/// reservation. The publication owner discards its private context on failure.
pub(in crate::storage) fn encode_overlay_bounded(
	overlay: &PropertyOverlay,
	context: &mut EncodeContext,
	maximum: usize,
) -> FerrosGrynnResult<(Vec<u8>, usize)> {
	let count = overlay.iter().len();
	if count == 0 {
		return Err(invalid_page(0, "an empty graph overlay must not be stored"));
	}
	let limits = DecodeLimits::default();
	if count > limits.max_collection_len {
		return Err(overlay_limit("overlay entry count", count, limits.max_collection_len));
	}
	let count = u64::try_from(count)
		.map_err(|_error| invalid_page(0, "overlay entry count exceeds u64"))?;
	let mut output = Vec::new();
	// The longest uLEB prefix needs no separate temporary buffer.
	reserve_overlay(&mut output, 10, 0, 0, maximum)?;
	write_uleb(&mut output, count);
	let mut retained_context = 0_usize;
	for (key, state) in overlay.iter() {
		reserve_overlay(&mut output, 5, retained_context, 0, maximum)?;
		output.extend_from_slice(&key.get().to_le_bytes());
		match state {
			OverlayValue::Hidden => output.push(0),
			OverlayValue::Value(value) => {
				output.push(1);
				let held = retained_context
					.checked_add(output.capacity())
					.ok_or_else(|| invalid_page(0, "overlay retained bytes overflow usize"))?;
				let available = maximum
					.checked_sub(held)
					.ok_or_else(|| overlay_limit("overlay encoding", held, maximum))?;
				let (encoded, context_bytes) =
					StorageValue::encode_value_bounded(value, context, available)?;
				retained_context = retained_context
					.checked_add(context_bytes)
					.ok_or_else(|| invalid_page(0, "overlay context bytes overflow usize"))?;
				reserve_overlay(
					&mut output,
					encoded.len(),
					retained_context,
					encoded.capacity(),
					maximum,
				)?;
				output.extend_from_slice(&encoded);
			}
		}
	}
	Ok((output, retained_context))
}

/// Admits old/new output overlap while a separately encoded child remains live.
fn reserve_overlay(
	output: &mut Vec<u8>,
	additional: usize,
	context_bytes: usize,
	child_bytes: usize,
	maximum: usize,
) -> FerrosGrynnResult<()> {
	let required = output
		.len()
		.checked_add(additional)
		.ok_or_else(|| invalid_page(0, "overlay encoded length overflows usize"))?;
	let reader_limit = DecodeLimits::default().max_total_allocation;
	if required > reader_limit {
		return Err(overlay_limit("graph overlay bytes", required, reader_limit));
	}
	let held = context_bytes
		.checked_add(child_bytes)
		.and_then(|bytes| bytes.checked_add(output.capacity()))
		.ok_or_else(|| invalid_page(0, "overlay live backing overflows usize"))?;
	let available = maximum
		.checked_sub(held)
		.ok_or_else(|| overlay_limit("overlay encoding", held, maximum))?;
	if required > output.capacity() {
		if required > available {
			return Err(overlay_limit("overlay output growth", required, available));
		}
		let capacity = required.max(output.capacity().saturating_mul(2)).min(available);
		output
			.try_reserve_exact(capacity - output.len())
			.map_err(|_error| invalid_page(0, "overlay output allocation failed"))?;
	}
	let actual = context_bytes
		.checked_add(child_bytes)
		.and_then(|bytes| bytes.checked_add(output.capacity()))
		.ok_or_else(|| invalid_page(0, "overlay output capacity overflows usize"))?;
	if actual > maximum {
		return Err(overlay_limit("overlay output backing", actual, maximum));
	}
	Ok(())
}

/// Keeps typed resource diagnostics consistent with the physical owner.
fn overlay_limit(name: &'static str, actual: usize, maximum: usize) -> FerrosGrynnError {
	FerrosGrynnError::storage_decode_limit_exceeded(name, actual as u64, maximum as u64)
}

/// Decodes one exact sparse overlay record.
pub(in crate::storage) fn decode_overlay(
	record: &[u8],
	context: &DecodeContext<'_>,
	limits: DecodeLimits,
) -> FerrosGrynnResult<PropertyOverlay> {
	if record.len() > limits.max_total_allocation {
		return Err(crate::gql_status::FerrosGrynnError::storage_decode_limit_exceeded(
			"graph overlay bytes",
			limits.max_total_allocation as u64,
			record.len() as u64,
		));
	}
	let (count, mut offset) = read_uleb(record)?;
	let count = usize::try_from(count)
		.map_err(|_error| invalid_page(0, "overlay entry count exceeds usize"))?;
	if count == 0 || count > limits.max_collection_len {
		return Err(invalid_page(0, "overlay entry count is empty or exceeds its limit"));
	}
	let mut entries = Vec::with_capacity(count);
	let mut previous = None;
	for _ in 0..count {
		let header_end = offset
			.checked_add(5)
			.ok_or_else(|| invalid_page(0, "overlay entry header overflows usize"))?;
		if header_end > record.len() {
			return Err(invalid_page(0, "overlay entry header is truncated"));
		}
		let key = PropertyKeyId::new(u32::from_le_bytes(
			record[offset..offset + 4]
				.try_into()
				.map_err(|_error| invalid_page(0, "overlay key is truncated"))?,
		));
		if previous.is_some_and(|previous| previous >= key) {
			return Err(invalid_page(0, "overlay keys are not strictly increasing"));
		}
		previous = Some(key);
		offset += 4;
		let state = match record[offset] {
			0 => OverlayValue::Hidden,
			1 => {
				let (value, consumed) =
					CanonicalCodec::decode_prefix(&record[offset + 1..], limits)?;
				if consumed == 0 {
					return Err(invalid_page(0, "overlay value consumed no bytes"));
				}
				offset = offset
					.checked_add(consumed)
					.ok_or_else(|| invalid_page(0, "overlay value offset overflows usize"))?;
				OverlayValue::Value(value.into_runtime(context)?)
			}
			_ => return Err(invalid_page(0, "overlay state is unknown")),
		};
		offset = offset
			.checked_add(1)
			.ok_or_else(|| invalid_page(0, "overlay offset overflows usize"))?;
		entries.push((key, state));
	}
	if offset != record.len() {
		return Err(invalid_page(0, "overlay record contains trailing bytes"));
	}
	PropertyOverlay::from_entries(entries)
}

/// Append-only builder for graph-local overlay records.
#[derive(Default)]
pub(in crate::storage) struct GraphOverlayStore {
	/// Absolute append origin; existing pages remain outside this image.
	first_page: u64,
	/// Next page associated with this graph overlay store.
	next_page: u64,
	/// Current associated with this graph overlay store.
	current: Option<(PageNo, PropertyPageBuilder)>,
	/// Pages associated with this graph overlay store.
	pages: Vec<AlignedPage>,
}

impl GraphOverlayStore {
	/// Starts a contiguous append after the WAL-visible overlay-file tail.
	pub(in crate::storage) const fn at_page(first_page: u64) -> Self {
		Self {
			first_page,
			next_page: first_page,
			current: None,
			pages: Vec::new(),
		}
	}

	/// Predicts append frames using the same counters as the slotted-page
	/// writer.
	///
	/// # Errors
	/// Rejects empty/oversized records and invalid inline packing counters.
	pub(in crate::storage) fn additional_pages(&self, length: usize) -> FerrosGrynnResult<usize> {
		if length == 0 {
			return Err(invalid_page(0, "an empty overlay record must not be stored"));
		}
		u32::try_from(length).map_err(|_error| invalid_page(0, "overlay record exceeds u32"))?;
		if length > MAX_INLINE_BYTES {
			return Ok(length.div_ceil(LARGE_CHUNK_BYTES));
		}
		let Some((_number, mut builder)) = self.current else {
			return Ok(1);
		};
		Ok(usize::from(builder.try_insert_len(length)?.is_none()))
	}

	/// Reports all live page backing, including unused reserved slots.
	///
	/// # Errors
	/// Rejects capacity arithmetic outside the platform address range.
	pub(in crate::storage) fn allocation_bytes(&self) -> FerrosGrynnResult<usize> {
		self.pages
			.capacity()
			.checked_mul(size_of::<AlignedPage>())
			.ok_or_else(|| invalid_page(0, "overlay backing bytes overflow usize"))
	}

	/// Admits replacement backing before either large-chain resize or inline
	/// push.
	///
	/// # Errors
	/// Propagates size, resource and allocation failures before changing page
	/// bytes.
	pub(in crate::storage) fn insert_bounded(
		&mut self,
		record: &[u8],
		page_lsn: PageLsn,
		maximum_backing_bytes: usize,
	) -> FerrosGrynnResult<PropertyRef> {
		let required = self
			.pages
			.len()
			.checked_add(self.additional_pages(record.len())?)
			.ok_or_else(|| invalid_page(0, "overlay append count overflows usize"))?;
		let old_bytes = self.allocation_bytes()?;
		let peak = if required > self.pages.capacity() {
			required
				.checked_mul(size_of::<AlignedPage>())
				.and_then(|bytes| bytes.checked_add(old_bytes))
				.ok_or_else(|| invalid_page(0, "overlay backing overlap overflows usize"))?
		} else {
			old_bytes
		};
		if peak > maximum_backing_bytes {
			return Err(overlay_limit("overlay append backing", peak, maximum_backing_bytes));
		}
		if required > self.pages.capacity() {
			let maximum_capacity = (maximum_backing_bytes - old_bytes) / size_of::<AlignedPage>();
			let capacity =
				required.max(self.pages.capacity().saturating_mul(2)).min(maximum_capacity);
			self.pages
				.try_reserve_exact(capacity - self.pages.len())
				.map_err(|_error| invalid_page(0, "overlay append allocation failed"))?;
		}
		self.insert(record, page_lsn)
	}

	/// Stores one encoded overlay and returns its exact hot-slot reference.
	pub(in crate::storage) fn insert(
		&mut self,
		record: &[u8],
		page_lsn: PageLsn,
	) -> FerrosGrynnResult<PropertyRef> {
		if record.is_empty() {
			return Err(invalid_page(0, "an empty overlay record must not be stored"));
		}
		let length = u32::try_from(record.len())
			.map_err(|_error| invalid_page(0, "overlay record exceeds u32"))?;
		if record.len() > MAX_INLINE_BYTES {
			self.finish_current()?;
			let first = PageNo::new(self.next_page);
			let page_count = append_large_overlay(&mut self.pages, first, page_lsn, record)?;
			self.next_page = self
				.next_page
				.checked_add(
					u64::try_from(page_count)
						.map_err(|_error| invalid_page(0, "overlay page count exceeds u64"))?,
				)
				.ok_or_else(|| invalid_page(0, "overlay page clock is exhausted"))?;
			return PropertyRef::large(first, length);
		}

		if self.current.is_none() {
			self.start_page(page_lsn)?;
		}
		if let Some((page_no, builder)) = self.current.as_mut() {
			let relative = page_no
				.get()
				.checked_sub(self.first_page)
				.ok_or_else(|| invalid_page(0, "overlay page precedes append origin"))?;
			let index = usize::try_from(relative)
				.map_err(|_error| invalid_page(0, "overlay page number exceeds usize"))?;
			let page = self
				.pages
				.get_mut(index)
				.ok_or_else(|| invalid_page(0, "current overlay page is absent"))?;
			if let Some(slot) = builder.try_insert(page, record)? {
				return PropertyRef::inline(*page_no, slot, length);
			}
		}
		self.finish_current()?;
		self.start_page(page_lsn)?;
		let (page_no, builder) =
			self.current.as_mut().ok_or_else(|| invalid_page(0, "overlay page was not started"))?;
		let relative = page_no
			.get()
			.checked_sub(self.first_page)
			.ok_or_else(|| invalid_page(0, "overlay page precedes append origin"))?;
		let index = usize::try_from(relative)
			.map_err(|_error| invalid_page(0, "overlay page number exceeds usize"))?;
		let page = self
			.pages
			.get_mut(index)
			.ok_or_else(|| invalid_page(0, "current overlay page is absent"))?;
		let slot = builder
			.try_insert(page, record)?
			.ok_or_else(|| invalid_page(0, "overlay does not fit an empty page"))?;
		PropertyRef::inline(*page_no, slot, length)
	}

	/// Finishes graph overlay store.
	pub(in crate::storage) fn finish(mut self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.finish_current()?;
		if u64::try_from(self.pages.len()).ok() != self.next_page.checked_sub(self.first_page) {
			return Err(invalid_page(0, "overlay pages are not contiguous"));
		}
		Ok(self.pages)
	}

	/// Returns the start page result.
	fn start_page(&mut self, page_lsn: PageLsn) -> FerrosGrynnResult<()> {
		let page_no = PageNo::new(self.next_page);
		let relative = page_no
			.get()
			.checked_sub(self.first_page)
			.ok_or_else(|| invalid_page(0, "overlay page precedes append origin"))?;
		let index = usize::try_from(relative)
			.map_err(|_error| invalid_page(0, "overlay page number exceeds usize"))?;
		if index != self.pages.len() {
			return Err(invalid_page(0, "overlay pages are not contiguous"));
		}
		self.next_page = self
			.next_page
			.checked_add(1)
			.ok_or_else(|| invalid_page(0, "overlay page clock is exhausted"))?;
		self.pages.push(AlignedPage::zeroed());
		let page = self
			.pages
			.last_mut()
			.ok_or_else(|| invalid_page(0, "overlay page allocation failed"))?;
		self.current = Some((page_no, PropertyPageBuilder::new_overlay(page, page_no, page_lsn)));
		Ok(())
	}

	/// Finishes current.
	fn finish_current(&mut self) -> FerrosGrynnResult<()> {
		if let Some((page_no, builder)) = self.current.take() {
			let relative = page_no
				.get()
				.checked_sub(self.first_page)
				.ok_or_else(|| invalid_page(0, "overlay page precedes append origin"))?;
			let index = usize::try_from(relative)
				.map_err(|_error| invalid_page(0, "overlay page number exceeds usize"))?;
			let page = self
				.pages
				.get_mut(index)
				.ok_or_else(|| invalid_page(0, "current overlay page is absent"))?;
			builder.finish(page)?;
		}
		Ok(())
	}
}

/// Resolves one inline or large graph-overlay record.
pub(in crate::storage) fn read_overlay_record(
	pages: &[AlignedPage],
	reference: PropertyRef,
	limit: usize,
) -> FerrosGrynnResult<Option<Vec<u8>>> {
	match reference {
		PropertyRef::None => Ok(None),
		PropertyRef::Inline {
			page,
			slot,
			length,
		} => {
			let source = page_at(pages, page)?;
			read_inline_overlay(source, page, slot, length).map(|record| Some(record.to_vec()))
		}
		PropertyRef::Large {
			first_page,
			length,
		} => {
			let length_usize = usize::try_from(length)
				.map_err(|_error| invalid_page(first_page.get(), "overlay length exceeds usize"))?;
			let page_count = length_usize.div_ceil(LARGE_CHUNK_BYTES);
			let start = usize::try_from(first_page.get())
				.map_err(|_error| invalid_page(first_page.get(), "overlay page exceeds usize"))?;
			let end = start
				.checked_add(page_count)
				.ok_or_else(|| invalid_page(first_page.get(), "overlay chain range overflows"))?;
			let chain = pages
				.get(start..end)
				.ok_or_else(|| invalid_page(first_page.get(), "overlay chain exceeds its file"))?;
			decode_large_overlay(first_page, chain, length, limit).map(Some)
		}
	}
}

/// Returns the page at result.
fn page_at(pages: &[AlignedPage], page_no: PageNo) -> FerrosGrynnResult<&AlignedPage> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid_page(page_no.get(), "overlay page exceeds usize"))?;
	pages.get(index).ok_or_else(|| invalid_page(page_no.get(), "overlay page exceeds its file"))
}

/// Writes uleb.
fn write_uleb(output: &mut Vec<u8>, mut value: u64) {
	loop {
		let mut byte = (value & 0x7f) as u8;
		value >>= 7;
		if value != 0 {
			byte |= 0x80;
		}
		output.push(byte);
		if value == 0 {
			break;
		}
	}
}

/// Reads uleb.
fn read_uleb(bytes: &[u8]) -> FerrosGrynnResult<(u64, usize)> {
	let mut value = 0_u64;
	for (index, byte) in bytes.iter().copied().take(10).enumerate() {
		let shift = u32::try_from(index * 7)
			.map_err(|_error| invalid_page(0, "overlay count shift exceeds u32"))?;
		if shift == 63 && byte & 0x7e != 0 {
			return Err(invalid_page(0, "overlay count overflows u64"));
		}
		value |= u64::from(byte & 0x7f) << shift;
		if byte & 0x80 == 0 {
			if index > 0 && byte == 0 {
				return Err(invalid_page(0, "overlay count is not minimally encoded"));
			}
			return Ok((value, index + 1));
		}
	}
	Err(invalid_page(0, "overlay count is truncated or overlong"))
}

#[cfg(test)]
/// Regression tests for overlay behavior.
mod tests {
	use super::*;
	use crate::values::{PropertyDictionary, Value};

	/// Borrowed sparse encoding preserves the existing explicit wire contract.
	#[test]
	fn bounded_overlay_matches_owned_codec_and_context() {
		use crate::{
			test_assertions::AssertResult as _,
			values::{Array, Object, Set},
		};

		let context = || {
			let mut dictionary = PropertyDictionary::new();
			dictionary.intern("nested").assert_ok();
			EncodeContext::new(dictionary)
		};
		let paris = Value::DateTime("2024-01-01T12:00:00+01:00[Europe/Paris]".parse().assert_ok());
		let children =
			Value::Array(Array::new(vec![Value::Set(Set::from(vec![3_i64, 1, 2])), paris.clone()]));
		let nested = Value::Object(Object::from_iter([("nested", children)]));
		let overlay = PropertyOverlay::from_entries([
			(PropertyKeyId::new(1), OverlayValue::Hidden),
			(PropertyKeyId::new(2), OverlayValue::Value(Value::Null)),
			(PropertyKeyId::new(3), OverlayValue::Value(Value::Bytes(vec![7; 257].into()))),
			(PropertyKeyId::new(4), OverlayValue::Value(nested)),
			(
				PropertyKeyId::new(5),
				OverlayValue::Value(Value::DateTime(
					"2024-01-01T12:00:00-05:00[America/New_York]".parse().assert_ok(),
				)),
			),
			(PropertyKeyId::new(6), OverlayValue::Value(paris)),
		])
		.assert_ok();
		let mut owned_context = context();
		let expected = encode_overlay(&overlay, &mut owned_context).assert_ok();
		let mut borrowed_context = context();
		let (actual, retained) =
			encode_overlay_bounded(&overlay, &mut borrowed_context, 1 << 20).assert_ok();
		assert_eq!(actual, expected);
		assert!(retained > 0);
		assert_eq!(
			borrowed_context.time_zone_names().collect::<Vec<_>>(),
			owned_context.time_zone_names().collect::<Vec<_>>()
		);
		assert_eq!(
			decode_overlay(&actual, &borrowed_context.decoder(), DecodeLimits::default())
				.assert_ok(),
			overlay
		);
		let (repeat, retained) =
			encode_overlay_bounded(&overlay, &mut borrowed_context, 1 << 20).assert_ok();
		assert_eq!(repeat, expected);
		assert_eq!(retained, 0);
	}

	/// Earlier encoded entries and the next DTO must fit one shared envelope.
	#[test]
	fn bounded_overlay_rejects_aggregate_values_and_oversized_first_copy() {
		use crate::test_assertions::AssertResult as _;

		let mut context = EncodeContext::default();
		let entries = [1, 2].map(|key| {
			(PropertyKeyId::new(key), OverlayValue::Value(Value::Bytes(vec![7; 128].into())))
		});
		for entry in &entries {
			let overlay = PropertyOverlay::from_entries([entry.clone()]).assert_ok();
			encode_overlay_bounded(&overlay, &mut context, 384).assert_ok();
		}
		let overlay = PropertyOverlay::from_entries(entries).assert_ok();
		assert!(encode_overlay_bounded(&overlay, &mut context, 384).is_err());
		let oversized = PropertyOverlay::from_entries([(
			PropertyKeyId::new(1),
			OverlayValue::Value(Value::Bytes(vec![7; 1024].into())),
		)])
		.assert_ok();
		assert!(encode_overlay_bounded(&oversized, &mut context, 512).is_err());
		assert_eq!(context.time_zone_names().len(), 0);
		assert!(context.property_dictionary().is_empty());
	}

	/// Admission changes no packing bytes and rejects before the first page.
	#[test]
	fn bounded_overlay_store_preserves_inline_large_packing() {
		use crate::test_assertions::AssertResult as _;

		let mut bounded = GraphOverlayStore::at_page(7);
		assert!(bounded.insert_bounded(&[1], PageLsn::ZERO, 0).is_err());
		assert_eq!(bounded.allocation_bytes().assert_ok(), 0);
		let mut owned = GraphOverlayStore::at_page(7);
		for record in [vec![1; 16], vec![2; 48 * 1024], vec![3; 32]] {
			let predicted = bounded.additional_pages(record.len()).assert_ok();
			let previous = bounded.pages.len();
			assert_eq!(
				bounded.insert_bounded(&record, PageLsn::ZERO, 4 << 20).assert_ok(),
				owned.insert(&record, PageLsn::ZERO).assert_ok()
			);
			assert_eq!(bounded.pages.len() - previous, predicted);
		}
		let expected = owned.finish().assert_ok();
		let actual = bounded.finish().assert_ok();
		assert_eq!(actual.len(), expected.len());
		for (actual, expected) in actual.iter().zip(expected) {
			assert_eq!(actual.as_bytes(), expected.as_bytes());
		}
	}

	/// Absolute references survive inline/large/inline appends without prefix
	/// copies.
	#[test]
	fn overlay_append_uses_absolute_references_and_relative_buffers() {
		use crate::test_assertions::AssertResult as _;

		let mut store = GraphOverlayStore::at_page(7);
		let records = [vec![1; 16], vec![2; 48 * 1024], vec![3; 32]];
		let references = records
			.iter()
			.map(|record| store.insert(record, PageLsn::ZERO).assert_ok())
			.collect::<Vec<_>>();
		assert!(
			matches!(references[0], PropertyRef::Inline { page, .. } if page == PageNo::new(7))
		);
		assert!(
			matches!(references[1], PropertyRef::Large { first_page, .. } if first_page == PageNo::new(8))
		);
		let mut pages = (0..7).map(|_| AlignedPage::zeroed()).collect::<Vec<_>>();
		pages.extend(store.finish().assert_ok());
		for (reference, record) in references.into_iter().zip(records) {
			assert_eq!(read_overlay_record(&pages, reference, 1 << 20).assert_ok(), Some(record));
		}
	}

	/// Exhaustion is rejected before allocating the first append page.
	#[test]
	fn overlay_append_rejects_exhausted_page_clock() {
		let mut store = GraphOverlayStore::at_page(u64::MAX);
		assert!(store.insert(&[1], PageLsn::ZERO).is_err());
		assert!(store.pages.is_empty());
	}

	/// Verifies sparse overlay round trips hidden and null.
	#[test]
	fn sparse_overlay_round_trips_hidden_and_null() {
		use crate::test_assertions::AssertResult as _;

		let overlay = PropertyOverlay::from_entries([
			(PropertyKeyId::new(2), OverlayValue::Hidden),
			(PropertyKeyId::new(7), OverlayValue::Value(Value::Null)),
		])
		.assert_ok();
		let mut encode = EncodeContext::new(PropertyDictionary::new());
		let record = encode_overlay(&overlay, &mut encode).assert_ok();
		let decode = encode.decoder();
		assert_eq!(decode_overlay(&record, &decode, DecodeLimits::default()).assert_ok(), overlay);

		let mut store = GraphOverlayStore::default();
		let reference = store.insert(&record, PageLsn::new(3)).assert_ok();
		let pages = store.finish().assert_ok();
		assert_eq!(read_overlay_record(&pages, reference, 1 << 20).assert_ok(), Some(record));
	}
}
