//! Canonical property records stored by the database-global AtomStore.

use super::page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{
		PropertyMap,
		RecordId,
		Value,
		storage::{
			DecodeContext,
			EncodeContext,
			StorageValue,
			TypeTag,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

/// Byte offset of the slotted-page record count.
const SLOT_COUNT_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of the first unused slot-directory byte.
const FREE_START_OFFSET: usize = SLOT_COUNT_OFFSET + 2;
/// Byte offset of the first byte occupied by record data.
const FREE_END_OFFSET: usize = FREE_START_OFFSET + 2;
/// Byte offset of the reserved slotted-page header field.
const RESERVED_OFFSET: usize = FREE_END_OFFSET + 2;
/// Byte offset at which slot entries begin.
const SLOT_DIRECTORY_OFFSET: usize = RESERVED_OFFSET + 2;
/// Encoded byte width of one slot entry.
const SLOT_ENTRY_BYTES: usize = 4;
/// Largest record stored directly in one property page.
pub(super) const MAX_INLINE_BYTES: usize = (PAGE_SIZE - PAGE_HEADER_SIZE) / 2;
/// Byte offset of the next-page identity in an overflow page.
const LARGE_NEXT_PAGE_OFFSET: usize = PAGE_HEADER_SIZE;
/// Byte offset of the overflow-page payload length.
const LARGE_DATA_LEN_OFFSET: usize = LARGE_NEXT_PAGE_OFFSET + 8;
/// Byte offset of the reserved overflow-page field.
const LARGE_RESERVED_OFFSET: usize = LARGE_DATA_LEN_OFFSET + 4;
/// Byte offset at which overflow payload bytes begin.
const LARGE_DATA_OFFSET: usize = LARGE_RESERVED_OFFSET + 4;
/// Maximum payload bytes stored by one overflow page.
pub(super) const LARGE_CHUNK_BYTES: usize = PAGE_SIZE - LARGE_DATA_OFFSET;

/// Mutable builder for one canonical slotted property page.
pub(super) struct PropertyPageBuilder {
	/// Page image under construction.
	page: Box<AlignedPage>,
	/// Number of populated slot entries.
	slot_count: u16,
	/// First unused byte in the growing slot directory.
	free_start: u16,
	/// First byte occupied by records growing backward from the page end.
	free_end: u16,
}

impl PropertyPageBuilder {
	/// Creates one empty property heap page.
	pub(super) fn new(page_no: PageNo, page_lsn: PageLsn) -> Self {
		Self::new_typed(PageType::PropHeap, page_no, page_lsn)
	}

	/// Creates one empty graph-overlay slotted page.
	pub(super) fn new_overlay(page_no: PageNo, page_lsn: PageLsn) -> Self {
		Self::new_typed(PageType::GraphOverlay, page_no, page_lsn)
	}

	fn new_typed(page_type: PageType, page_no: PageNo, page_lsn: PageLsn) -> Self {
		Self {
			page: Box::new(AlignedPage::new(page_type, PageFlags::empty(), page_lsn, page_no)),
			slot_count: 0,
			free_start: SLOT_DIRECTORY_OFFSET as u16,
			free_end: PAGE_SIZE as u16,
		}
	}

	/// Inserts one record and returns `None` when another page is required.
	pub(super) fn try_insert(&mut self, record: &[u8]) -> FerrosGrynnResult<Option<u16>> {
		if record.len() > MAX_INLINE_BYTES {
			return Ok(None);
		}
		let required = record
			.len()
			.checked_add(SLOT_ENTRY_BYTES)
			.ok_or_else(|| invalid(0, "property record size overflows usize"))?;
		if required > usize::from(self.free_end - self.free_start) {
			return Ok(None);
		}
		let record_len = u16::try_from(record.len())
			.map_err(|_error| invalid(0, "inline property record exceeds u16"))?;
		let record_offset = self
			.free_end
			.checked_sub(record_len)
			.ok_or_else(|| invalid(0, "property page free range underflows"))?;
		let slot_offset = usize::from(self.free_start);
		let bytes = self.page.as_bytes_mut();
		write_u16(bytes, slot_offset, record_offset);
		write_u16(bytes, slot_offset + 2, record_len);
		bytes[usize::from(record_offset)..usize::from(record_offset) + record.len()]
			.copy_from_slice(record);
		let slot = self.slot_count;
		self.slot_count = self
			.slot_count
			.checked_add(1)
			.ok_or_else(|| invalid(0, "property slot count overflows u16"))?;
		self.free_start = self
			.free_start
			.checked_add(SLOT_ENTRY_BYTES as u16)
			.ok_or_else(|| invalid(0, "property slot directory overflows u16"))?;
		self.free_end = record_offset;
		Ok(Some(slot))
	}

	/// Seals metadata and returns the complete page.
	pub(super) fn finish(mut self) -> FerrosGrynnResult<Box<AlignedPage>> {
		let bytes = self.page.as_bytes_mut();
		write_u16(bytes, SLOT_COUNT_OFFSET, self.slot_count);
		write_u16(bytes, FREE_START_OFFSET, self.free_start);
		write_u16(bytes, FREE_END_OFFSET, self.free_end);
		self.page.seal()?;
		Ok(self.page)
	}
}

/// Reads one record after validating the complete slotted page.
pub(super) fn read_property_record(
	page: &AlignedPage,
	page_no: PageNo,
	slot: u16,
	expected_len: u32,
) -> FerrosGrynnResult<&[u8]> {
	read_slotted_record(page, page_no, slot, expected_len, PageType::PropHeap)
}

/// Reads one graph-overlay record after validating its complete slotted page.
pub(super) fn read_overlay_record(
	page: &AlignedPage,
	page_no: PageNo,
	slot: u16,
	expected_len: u32,
) -> FerrosGrynnResult<&[u8]> {
	read_slotted_record(page, page_no, slot, expected_len, PageType::GraphOverlay)
}

fn read_slotted_record(
	page: &AlignedPage,
	page_no: PageNo,
	slot: u16,
	expected_len: u32,
	page_type: PageType,
) -> FerrosGrynnResult<&[u8]> {
	let header = page.validate(page_no)?;
	if header.page_type() != page_type || !header.flags().is_empty() {
		return Err(invalid(page_no.get(), "unexpected slotted-record page type or flags"));
	}
	let bytes = page.as_bytes();
	expect_zero(&bytes[RESERVED_OFFSET..SLOT_DIRECTORY_OFFSET], page_no, "property padding")?;
	let slot_count = read_u16(bytes, SLOT_COUNT_OFFSET);
	let free_start = read_u16(bytes, FREE_START_OFFSET);
	let free_end = read_u16(bytes, FREE_END_OFFSET);
	let expected_free_start = SLOT_DIRECTORY_OFFSET
		.checked_add(usize::from(slot_count) * SLOT_ENTRY_BYTES)
		.ok_or_else(|| invalid(page_no.get(), "property slot directory overflows usize"))?;
	if usize::from(free_start) != expected_free_start
		|| free_start > free_end
		|| usize::from(free_end) > PAGE_SIZE
		|| slot >= slot_count
	{
		return Err(invalid(page_no.get(), "property page metadata is inconsistent"));
	}
	let slot_offset = SLOT_DIRECTORY_OFFSET + usize::from(slot) * SLOT_ENTRY_BYTES;
	let record_offset = usize::from(read_u16(bytes, slot_offset));
	let record_len = usize::from(read_u16(bytes, slot_offset + 2));
	let record_end = record_offset
		.checked_add(record_len)
		.ok_or_else(|| invalid(page_no.get(), "property record range overflows usize"))?;
	if record_offset == 0
		|| record_offset < usize::from(free_end)
		|| record_end > PAGE_SIZE
		|| u32::try_from(record_len).ok() != Some(expected_len)
	{
		return Err(invalid(page_no.get(), "property record range or length is invalid"));
	}
	Ok(&bytes[record_offset..record_end])
}

/// Encodes a property map through the canonical value codec.
pub(super) fn encode_property_map(
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

/// Encodes one complete canonical `RecordId` value.
pub(super) fn encode_record_id(
	record_id: &RecordId,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	CanonicalCodec::encode(
		StorageValue::from_runtime(Value::from(record_id.clone()), context)?.as_ref(),
	)
}

/// Decodes one complete canonical `RecordId` value.
pub(super) fn decode_record_id(
	record: &[u8],
	context: &DecodeContext<'_>,
	limits: DecodeLimits,
) -> FerrosGrynnResult<RecordId> {
	match CanonicalCodec::decode(record, limits)?.into_runtime(context)? {
		Value::Record(record_id) => Ok(*record_id),
		_ => Err(FerrosGrynnError::invalid_storage_value_encoding(
			0,
			"atom RecordId reference does not contain a canonical RecordId",
		)),
	}
}

/// Decodes one canonical property record under the supplied allocation limits.
pub(super) fn decode_property_map(
	record: &[u8],
	context: &DecodeContext<'_>,
	limits: DecodeLimits,
) -> FerrosGrynnResult<PropertyMap> {
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
	let mut properties = PropertyMap::with_capacity(values.len());
	for (key, value) in values {
		properties.insert(key, value.into_runtime(context)?);
	}
	Ok(properties)
}

/// Splits one non-inline record into a contiguous large-object chain.
pub(super) fn encode_large_property(
	first_page: PageNo,
	page_lsn: PageLsn,
	record: &[u8],
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	encode_large_record(first_page, page_lsn, record, PageType::PropLargeHeap)
}

/// Splits one non-inline graph overlay into a contiguous overflow chain.
pub(super) fn encode_large_overlay(
	first_page: PageNo,
	page_lsn: PageLsn,
	record: &[u8],
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	encode_large_record(first_page, page_lsn, record, PageType::GraphOverlay)
}

fn encode_large_record(
	first_page: PageNo,
	page_lsn: PageLsn,
	record: &[u8],
	page_type: PageType,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	if record.len() <= MAX_INLINE_BYTES {
		return Err(invalid(first_page.get(), "record fits an inline property page"));
	}
	let page_count = record.len().div_ceil(LARGE_CHUNK_BYTES);
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let offset = u64::try_from(index)
			.map_err(|_error| invalid(first_page.get(), "large property index exceeds u64"))?;
		let page_no =
			PageNo::new(first_page.get().checked_add(offset).ok_or_else(|| {
				invalid(first_page.get(), "large property page number overflows")
			})?);
		let next = if index + 1 == page_count {
			PageNo::NULL
		} else {
			PageNo::new(
				page_no
					.get()
					.checked_add(1)
					.ok_or_else(|| invalid(page_no.get(), "large property link overflows"))?,
			)
		};
		let start = index
			.checked_mul(LARGE_CHUNK_BYTES)
			.ok_or_else(|| invalid(page_no.get(), "large property offset overflows usize"))?;
		let end = start.saturating_add(LARGE_CHUNK_BYTES).min(record.len());
		let chunk = &record[start..end];
		let mut page = AlignedPage::new(page_type, PageFlags::OVERFLOW, page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		write_u64(bytes, LARGE_NEXT_PAGE_OFFSET, next.get());
		write_u32(
			bytes,
			LARGE_DATA_LEN_OFFSET,
			u32::try_from(chunk.len())
				.map_err(|_error| invalid(page_no.get(), "large property chunk exceeds u32"))?,
		);
		bytes[LARGE_DATA_OFFSET..LARGE_DATA_OFFSET + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
		pages.push(page);
	}
	Ok(pages)
}

/// Decodes an already loaded contiguous large-object chain.
pub(super) fn decode_large_property(
	first_page: PageNo,
	pages: &[AlignedPage],
	expected_len: u32,
	limit: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	decode_large_record(first_page, pages, expected_len, limit, PageType::PropLargeHeap)
}

/// Decodes an already loaded graph-overlay overflow chain.
pub(super) fn decode_large_overlay(
	first_page: PageNo,
	pages: &[AlignedPage],
	expected_len: u32,
	limit: usize,
) -> FerrosGrynnResult<Vec<u8>> {
	decode_large_record(first_page, pages, expected_len, limit, PageType::GraphOverlay)
}

fn decode_large_record(
	first_page: PageNo,
	pages: &[AlignedPage],
	expected_len: u32,
	limit: usize,
	page_type: PageType,
) -> FerrosGrynnResult<Vec<u8>> {
	let expected_len = usize::try_from(expected_len)
		.map_err(|_error| invalid(first_page.get(), "large property length exceeds usize"))?;
	if expected_len > limit {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"large property bytes",
			limit as u64,
			expected_len as u64,
		));
	}
	let mut output = Vec::with_capacity(expected_len);
	for (index, page) in pages.iter().enumerate() {
		let page_no = PageNo::new(
			first_page
				.get()
				.checked_add(u64::try_from(index).map_err(|_error| {
					invalid(first_page.get(), "large property index exceeds u64")
				})?)
				.ok_or_else(|| invalid(first_page.get(), "large property page number overflows"))?,
		);
		let header = page.validate(page_no)?;
		if header.page_type() != page_type || header.flags() != PageFlags::OVERFLOW {
			return Err(invalid(page_no.get(), "invalid large-property page header"));
		}
		let bytes = page.as_bytes();
		expect_zero(
			&bytes[LARGE_RESERVED_OFFSET..LARGE_DATA_OFFSET],
			page_no,
			"large property padding",
		)?;
		let data_len = usize::try_from(read_u32(bytes, LARGE_DATA_LEN_OFFSET))
			.map_err(|_error| invalid(page_no.get(), "large property chunk exceeds usize"))?;
		if data_len > LARGE_CHUNK_BYTES || output.len().saturating_add(data_len) > expected_len {
			return Err(invalid(page_no.get(), "large property chunk exceeds its bounds"));
		}
		let expected_next = if index + 1 == pages.len() {
			PageNo::NULL
		} else {
			PageNo::new(
				page_no
					.get()
					.checked_add(1)
					.ok_or_else(|| invalid(page_no.get(), "large property link overflows"))?,
			)
		};
		if read_u64(bytes, LARGE_NEXT_PAGE_OFFSET) != expected_next.get() {
			return Err(invalid(page_no.get(), "large property chain link is inconsistent"));
		}
		output.extend_from_slice(&bytes[LARGE_DATA_OFFSET..LARGE_DATA_OFFSET + data_len]);
		expect_zero(
			&bytes[LARGE_DATA_OFFSET + data_len..],
			page_no,
			"large property trailing bytes",
		)?;
	}
	if output.len() != expected_len {
		return Err(invalid(first_page.get(), "large property chain length mismatch"));
	}
	Ok(output)
}

/// Validates a reserved or unused byte range.
fn expect_zero(bytes: &[u8], page_no: PageNo, field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid(page_no.get(), format!("{field} are non-zero")));
	}
	Ok(())
}

/// Writes one little-endian `u16` at a validated fixed offset.
fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u32` at a validated fixed offset.
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes one little-endian `u64` at a validated fixed offset.
fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Reads one little-endian `u16` from a validated fixed offset.
fn read_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0; 2];
	value.copy_from_slice(&bytes[offset..offset + 2]);
	u16::from_le_bytes(value)
}

/// Reads one little-endian `u32` from a validated fixed offset.
fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

/// Reads one little-endian `u64` from a validated fixed offset.
fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

/// Builds the canonical invalid-page error for this codec.
fn invalid(page_no: u64, reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_page(page_no, reason)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::values::{PropertyDictionary, PropertyKeyId, Value};

	#[test]
	fn property_map_round_trips_through_a_slotted_page() -> FerrosGrynnResult<()> {
		let mut properties = PropertyMap::new();
		properties.insert(PropertyKeyId::new(3), Value::from("value"));
		let mut context = EncodeContext::new(PropertyDictionary::new());
		let record = encode_property_map(&properties, &mut context)?;
		let mut builder = PropertyPageBuilder::new(PageNo::new(7), PageLsn::new(11));
		let slot = builder
			.try_insert(&record)?
			.ok_or_else(|| invalid(7, "test record unexpectedly requires a large page"))?;
		let page = builder.finish()?;
		let stored = read_property_record(
			&page,
			PageNo::new(7),
			slot,
			u32::try_from(record.len()).map_err(|_error| invalid(7, "test record exceeds u32"))?,
		)?;
		let decoded = decode_property_map(stored, &context.decoder(), DecodeLimits::default())?;
		assert_eq!(decoded, properties);
		Ok(())
	}

	#[test]
	fn large_property_chain_round_trips_and_validates_links() -> FerrosGrynnResult<()> {
		let record = vec![0xa5; MAX_INLINE_BYTES + LARGE_CHUNK_BYTES];
		let pages = encode_large_property(PageNo::new(9), PageLsn::new(13), &record)?;
		let length =
			u32::try_from(record.len()).map_err(|_error| invalid(9, "test record exceeds u32"))?;
		assert_eq!(decode_large_property(PageNo::new(9), &pages, length, record.len())?, record);
		Ok(())
	}
}
