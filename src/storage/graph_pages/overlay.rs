use super::*;
use crate::{
	datastore::{OverlayValue, PropertyOverlay},
	storage::{
		atom_page::PropertyRef,
		property_page::{
			LARGE_CHUNK_BYTES,
			MAX_INLINE_BYTES,
			PropertyPageBuilder,
			decode_large_overlay,
			encode_large_overlay,
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
	next_page: u64,
	current: Option<(PageNo, PropertyPageBuilder)>,
	pages: Vec<(PageNo, AlignedPage)>,
}

impl GraphOverlayStore {
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
			let pages = encode_large_overlay(first, page_lsn, record)?;
			self.next_page = self
				.next_page
				.checked_add(
					u64::try_from(pages.len())
						.map_err(|_error| invalid_page(0, "overlay page count exceeds u64"))?,
				)
				.ok_or_else(|| invalid_page(0, "overlay page clock is exhausted"))?;
			for (offset, page) in pages.into_iter().enumerate() {
				let page_no = PageNo::new(
					first
						.get()
						.checked_add(
							u64::try_from(offset)
								.map_err(|_error| invalid_page(0, "overlay offset exceeds u64"))?,
						)
						.ok_or_else(|| invalid_page(0, "overlay page number overflows"))?,
				);
				self.pages.push((page_no, page));
			}
			return PropertyRef::large(first, length);
		}

		if self.current.is_none() {
			self.start_page(page_lsn)?;
		}
		if let Some((page_no, page)) = self.current.as_mut()
			&& let Some(slot) = page.try_insert(record)?
		{
			return PropertyRef::inline(*page_no, slot, length);
		}
		self.finish_current()?;
		self.start_page(page_lsn)?;
		let (page_no, page) =
			self.current.as_mut().ok_or_else(|| invalid_page(0, "overlay page was not started"))?;
		let slot = page
			.try_insert(record)?
			.ok_or_else(|| invalid_page(0, "overlay does not fit an empty page"))?;
		PropertyRef::inline(*page_no, slot, length)
	}

	pub(in crate::storage) fn finish(mut self) -> FerrosGrynnResult<Vec<AlignedPage>> {
		self.finish_current()?;
		self.pages.sort_unstable_by_key(|(page_no, _page)| *page_no);
		for (index, (page_no, _page)) in self.pages.iter().enumerate() {
			if page_no.get() != index as u64 {
				return Err(invalid_page(0, "overlay pages are not contiguous"));
			}
		}
		Ok(self.pages.into_iter().map(|(_page_no, page)| page).collect())
	}

	fn start_page(&mut self, page_lsn: PageLsn) -> FerrosGrynnResult<()> {
		let page_no = PageNo::new(self.next_page);
		self.next_page = self
			.next_page
			.checked_add(1)
			.ok_or_else(|| invalid_page(0, "overlay page clock is exhausted"))?;
		self.current = Some((page_no, PropertyPageBuilder::new_overlay(page_no, page_lsn)));
		Ok(())
	}

	fn finish_current(&mut self) -> FerrosGrynnResult<()> {
		if let Some((page_no, page)) = self.current.take() {
			self.pages.push((page_no, *page.finish()?));
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

fn page_at(pages: &[AlignedPage], page_no: PageNo) -> FerrosGrynnResult<&AlignedPage> {
	let index = usize::try_from(page_no.get())
		.map_err(|_error| invalid_page(page_no.get(), "overlay page exceeds usize"))?;
	pages.get(index).ok_or_else(|| invalid_page(page_no.get(), "overlay page exceeds its file"))
}

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
mod tests {
	use super::*;
	use crate::values::{PropertyDictionary, Value};

	#[test]
	fn sparse_overlay_round_trips_hidden_and_null() -> FerrosGrynnResult<()> {
		let overlay = PropertyOverlay::from_entries([
			(PropertyKeyId::new(2), OverlayValue::Hidden),
			(PropertyKeyId::new(7), OverlayValue::Value(Value::Null)),
		])?;
		let mut encode = EncodeContext::new(PropertyDictionary::new());
		let record = encode_overlay(&overlay, &mut encode)?;
		let decode = encode.decoder();
		assert_eq!(decode_overlay(&record, &decode, DecodeLimits::default())?, overlay);

		let mut store = GraphOverlayStore::default();
		let reference = store.insert(&record, PageLsn::new(3))?;
		let pages = store.finish()?;
		assert_eq!(read_overlay_record(&pages, reference, 1 << 20)?, Some(record));
		Ok(())
	}
}
