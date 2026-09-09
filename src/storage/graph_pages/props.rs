use std::collections::BTreeSet;

#[allow(clippy::wildcard_imports, reason = "page codecs use the graph-page facade contract")]
use super::*;
use crate::values::PropertyKeyId;

/// Mutable builder for one canonical property slotted page.
pub(in crate::storage) struct PropertyPageBuilder {
	/// Complete aligned page being filled.
	page: Box<AlignedPage>,
	/// Next slot-directory entry.
	slot_count: u16,
	/// First unused byte after the slot directory.
	free_start: u16,
	/// First byte of the packed record zone.
	free_end: u16,
}

impl PropertyPageBuilder {
	/// Creates one empty property page.
	pub(in crate::storage) fn new(page_no: PageNo, page_lsn: PageLsn) -> Self {
		Self {
			page: Box::new(AlignedPage::new(
				PageType::PropHeap,
				PageFlags::empty(),
				page_lsn,
				page_no,
			)),
			slot_count: 0,
			free_start: PROPERTY_SLOT_DIRECTORY_OFFSET as u16,
			free_end: PAGE_SIZE as u16,
		}
	}

	/// Inserts one record when it fits, returning its stable directory slot.
	///
	/// Returns `Ok(None)` when a different page is required.
	pub(in crate::storage) fn try_insert(
		&mut self,
		record: &[u8],
	) -> FerrosGrynnResult<Option<u16>> {
		if record.len() > MAX_INLINE_PROPERTY_BYTES {
			return Ok(None);
		}
		let required = record
			.len()
			.checked_add(PROPERTY_SLOT_ENTRY_BYTES)
			.ok_or_else(|| invalid_page(0, "property record size overflows usize"))?;
		if required > usize::from(self.free_end - self.free_start) {
			return Ok(None);
		}
		let record_len = u16::try_from(record.len())
			.map_err(|_error| invalid_page(0, "inline property record exceeds u16"))?;
		let record_offset = self
			.free_end
			.checked_sub(record_len)
			.ok_or_else(|| invalid_page(0, "property page free_end underflow"))?;
		let slot_offset = usize::from(self.free_start);
		let bytes = self.page.as_bytes_mut();
		write_u16(bytes, slot_offset, record_offset);
		write_u16(bytes, slot_offset + size_of::<u16>(), record_len);
		bytes[usize::from(record_offset)..usize::from(record_offset) + record.len()]
			.copy_from_slice(record);
		let slot = self.slot_count;
		self.slot_count = self
			.slot_count
			.checked_add(1)
			.ok_or_else(|| invalid_page(0, "property slot count overflows u16"))?;
		self.free_start = self
			.free_start
			.checked_add(PROPERTY_SLOT_ENTRY_BYTES as u16)
			.ok_or_else(|| invalid_page(0, "property slot directory overflows u16"))?;
		self.free_end = record_offset;
		Ok(Some(slot))
	}

	/// Seals metadata and returns the complete page.
	pub(in crate::storage) fn finish(mut self) -> FerrosGrynnResult<Box<AlignedPage>> {
		let bytes = self.page.as_bytes_mut();
		write_u16(bytes, PROPERTY_SLOT_COUNT_OFFSET, self.slot_count);
		write_u16(bytes, PROPERTY_FREE_START_OFFSET, self.free_start);
		write_u16(bytes, PROPERTY_FREE_END_OFFSET, self.free_end);
		self.page.seal()?;
		Ok(self.page)
	}
}

/// Reads one property record after validating the complete slotted page.
pub(in crate::storage) fn read_property_record(
	page: &AlignedPage,
	page_no: PageNo,
	slot: u16,
	expected_len: u32,
) -> FerrosGrynnResult<&[u8]> {
	let header = page.validate(page_no)?;
	if header.page_type() != PageType::PropHeap || !header.flags().is_empty() {
		return Err(invalid_page(page_no.get(), "expected an unflagged PropHeap page"));
	}
	let bytes = page.as_bytes();
	validate_zero(
		&bytes[PROPERTY_RESERVED_OFFSET..PROPERTY_SLOT_DIRECTORY_OFFSET],
		page_no,
		"property page padding",
	)?;
	let slot_count = read_u16(bytes, PROPERTY_SLOT_COUNT_OFFSET);
	let free_start = read_u16(bytes, PROPERTY_FREE_START_OFFSET);
	let free_end = read_u16(bytes, PROPERTY_FREE_END_OFFSET);
	let expected_free_start = PROPERTY_SLOT_DIRECTORY_OFFSET
		.checked_add(usize::from(slot_count) * PROPERTY_SLOT_ENTRY_BYTES)
		.ok_or_else(|| invalid_page(page_no.get(), "property slot directory overflows usize"))?;
	if usize::from(free_start) != expected_free_start
		|| free_start > free_end
		|| usize::from(free_end) > PAGE_SIZE
		|| slot >= slot_count
	{
		return Err(invalid_page(page_no.get(), "property page metadata is inconsistent"));
	}
	let slot_offset =
		PROPERTY_SLOT_DIRECTORY_OFFSET + usize::from(slot) * PROPERTY_SLOT_ENTRY_BYTES;
	let record_offset = usize::from(read_u16(bytes, slot_offset));
	let record_len = usize::from(read_u16(bytes, slot_offset + size_of::<u16>()));
	if record_offset == 0 {
		return Err(invalid_page(page_no.get(), "property slot is a tombstone"));
	}
	let record_end = record_offset
		.checked_add(record_len)
		.ok_or_else(|| invalid_page(page_no.get(), "property record range overflows usize"))?;
	if record_offset < usize::from(free_end)
		|| record_end > PAGE_SIZE
		|| u32::try_from(record_len).ok() != Some(expected_len)
	{
		return Err(invalid_page(page_no.get(), "property record range or length is invalid"));
	}
	Ok(&bytes[record_offset..record_end])
}

/// Encodes a runtime property map using the existing canonical value codec.
pub(in crate::storage) fn encode_property_map(
	properties: &PropertyMap,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	let values = properties
		.iter()
		.map(|(key, value)| {
			StorageValue::from_runtime(value.clone(), context).map(|value| (key, value))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let encoded = CanonicalCodec::encode(StorageValue::Object(values).as_ref())?;
	if encoded.first().copied() != Some(TypeTag::Object.byte()) {
		return Err(FerrosGrynnError::invalid_storage_value_encoding(
			0,
			"property codec did not produce a canonical object",
		));
	}
	Ok(encoded[1..].to_vec())
}

/// Decodes one canonical property record with allocation limits applied by
/// the existing value codec before materialization.
pub(in crate::storage) fn decode_property_map(
	record: &[u8],
	context: &DecodeContext<'_>,
	limits: DecodeLimits,
) -> FerrosGrynnResult<PropertyMap> {
	let values = decode_property_entries(record, limits)?;
	let mut properties = PropertyMap::with_capacity(values.len());
	for (key, value) in values {
		properties.insert(key, value.into_runtime(context)?);
	}
	Ok(properties)
}

/// Decodes only property keys required by a physical index update.
///
/// Values outside `keys` remain validated by the canonical codec but are not
/// converted to runtime values, so unrelated atom references do not require a
/// graph-wide decode context.
pub(in crate::storage) fn decode_property_fields(
	record: &[u8],
	keys: &BTreeSet<PropertyKeyId>,
	context: &DecodeContext<'_>,
	limits: DecodeLimits,
) -> FerrosGrynnResult<PropertyMap> {
	let values = decode_property_entries(record, limits)?;
	let mut properties = PropertyMap::with_capacity(keys.len().min(values.len()));
	for (key, value) in values {
		if keys.contains(&key) {
			properties.insert(key, value.into_runtime(context)?);
		}
	}
	Ok(properties)
}

/// Validates one canonical property record and returns its top-level count
/// without resolving values that cannot affect count statistics.
pub(in crate::storage) fn count_property_entries(
	record: &[u8],
	limits: DecodeLimits,
) -> FerrosGrynnResult<usize> {
	decode_property_entries(record, limits).map(|values| values.len())
}

fn decode_property_entries(
	record: &[u8],
	limits: DecodeLimits,
) -> FerrosGrynnResult<Vec<(crate::values::PropertyKeyId, StorageValue)>> {
	let total = record.len().checked_add(1).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_value_encoding(0, "property length overflow")
	})?;
	if total > limits.max_total_allocation {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"property bytes",
			limits.max_total_allocation as u64,
			total as u64,
		));
	}
	let mut encoded = Vec::with_capacity(total);
	encoded.push(TypeTag::Object.byte());
	encoded.extend_from_slice(record);
	let StorageValue::Object(values) = CanonicalCodec::decode(&encoded, limits)? else {
		return Err(FerrosGrynnError::invalid_storage_value_encoding(
			0,
			"property record is not a canonical object body",
		));
	};
	Ok(values)
}

/// Splits one large canonical property record into a contiguous chunk chain.
pub(in crate::storage) fn encode_large_property(
	first_page: PageNo,
	page_lsn: PageLsn,
	record: &[u8],
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	if record.len() <= MAX_INLINE_PROPERTY_BYTES {
		return Err(invalid_page(
			first_page.get(),
			"small property record does not require a large-object chain",
		));
	}
	let page_count = record.len().div_ceil(LARGE_PROPERTY_CHUNK_BYTES);
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let index_u64 = u64::try_from(index)
			.map_err(|_error| invalid_page(first_page.get(), "large chunk index exceeds u64"))?;
		let page_no =
			first_page.get().checked_add(index_u64).map(PageNo::new).ok_or_else(|| {
				invalid_page(first_page.get(), "large chunk page number overflows u64")
			})?;
		let next_page =
			if index + 1 == page_count {
				None
			} else {
				Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
					invalid_page(page_no.get(), "large chunk next page overflows")
				})?))
			};
		let start = index
			.checked_mul(LARGE_PROPERTY_CHUNK_BYTES)
			.ok_or_else(|| invalid_page(page_no.get(), "large chunk offset overflows usize"))?;
		let end = start.saturating_add(LARGE_PROPERTY_CHUNK_BYTES).min(record.len());
		let chunk = &record[start..end];
		let mut page =
			AlignedPage::new(PageType::PropLargeHeap, PageFlags::OVERFLOW, page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		write_optional_page(bytes, LARGE_NEXT_PAGE_OFFSET, next_page);
		write_u32(
			bytes,
			LARGE_DATA_LEN_OFFSET,
			u32::try_from(chunk.len())
				.map_err(|_error| invalid_page(page_no.get(), "large chunk exceeds u32"))?,
		);
		bytes[LARGE_DATA_OFFSET..LARGE_DATA_OFFSET + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
		pages.push(page);
	}
	Ok(pages)
}

/// Decodes one already loaded large-property chain in physical order.
pub(in crate::storage) fn decode_large_property(
	first_page: PageNo,
	pages: &[AlignedPage],
	expected_len: u32,
	limit: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	let expected_len = usize::try_from(expected_len).map_err(|_error| {
		invalid_page(first_page.get(), "large property length does not fit usize")
	})?;
	if expected_len > limit {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"large property bytes",
			limit as u64,
			expected_len as u64,
		));
	}
	let mut output = Vec::with_capacity(expected_len);
	for (index, page) in pages.iter().enumerate() {
		let index_u64 = u64::try_from(index)
			.map_err(|_error| invalid_page(first_page.get(), "large chunk index exceeds u64"))?;
		let page_no = PageNo::new(
			first_page
				.get()
				.checked_add(index_u64)
				.ok_or_else(|| invalid_page(first_page.get(), "large chunk page overflows"))?,
		);
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::PropLargeHeap || header.flags() != PageFlags::OVERFLOW {
			return Err(invalid_page(page_no.get(), "invalid large-property page header"));
		}
		let bytes = page.as_bytes();
		validate_zero(
			&bytes[LARGE_RESERVED_OFFSET..LARGE_DATA_OFFSET],
			page_no,
			"large chunk padding",
		)?;
		let data_len =
			usize::try_from(read_u32(bytes, LARGE_DATA_LEN_OFFSET)).map_err(|_error| {
				invalid_page(page_no.get(), "large chunk length does not fit usize")
			})?;
		if data_len > LARGE_PROPERTY_CHUNK_BYTES
			|| output.len().saturating_add(data_len) > expected_len
		{
			return Err(invalid_page(page_no.get(), "large chunk length exceeds its bounds"));
		}
		let expected_next = if index + 1 == pages.len() {
			None
		} else {
			Some(PageNo::new(page_no.get().checked_add(1).ok_or_else(|| {
				invalid_page(page_no.get(), "large chunk next-page number overflows")
			})?))
		};
		if read_optional_page(bytes, LARGE_NEXT_PAGE_OFFSET) != expected_next {
			return Err(invalid_page(page_no.get(), "large property chain link is inconsistent"));
		}
		output.extend_from_slice(&bytes[LARGE_DATA_OFFSET..LARGE_DATA_OFFSET + data_len]);
		validate_zero(
			&bytes[LARGE_DATA_OFFSET + data_len..],
			page_no,
			"large chunk trailing bytes",
		)?;
	}
	if output.len() != expected_len {
		return Err(invalid_page(first_page.get(), "large property chain length mismatch"));
	}
	Ok(output)
}
