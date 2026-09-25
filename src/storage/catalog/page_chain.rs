use std::collections::BTreeSet;

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::page::{
		AlignedPage,
		PAGE_HEADER_SIZE,
		PAGE_SIZE,
		PageFlags,
		PageLsn,
		PageNo,
		PageType,
	},
};

const MAGIC: [u8; 4] = *b"FGC1";
const VERSION: u8 = 1;
const KIND_OFFSET: usize = PAGE_HEADER_SIZE + MAGIC.len();
const VERSION_OFFSET: usize = KIND_OFFSET + 1;
const RESERVED_A_OFFSET: usize = VERSION_OFFSET + 1;
const NEXT_PAGE_OFFSET: usize = RESERVED_A_OFFSET + 2;
const CHUNK_LEN_OFFSET: usize = NEXT_PAGE_OFFSET + 8;
const TOTAL_LEN_OFFSET: usize = CHUNK_LEN_OFFSET + 4;
const RESERVED_B_OFFSET: usize = TOTAL_LEN_OFFSET + 4;
const PAYLOAD_OFFSET: usize = RESERVED_B_OFFSET + 8;
const PAYLOAD_BYTES: usize = PAGE_SIZE - PAYLOAD_OFFSET;
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

/// Logical catalog carried by a `SystemCatalog` page chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::storage) enum CatalogKind {
	/// `FGDC v8` database catalog.
	Database = 1,
	/// `FGFC v5` physical-file catalog.
	Files = 2,
}

impl CatalogKind {
	const fn from_raw(value: u8) -> Option<Self> {
		match value {
			1 => Some(Self::Database),
			2 => Some(Self::Files),
			_ => None,
		}
	}
}

/// Builds a checksummed page chain for one bounded catalog payload.
pub(in crate::storage) fn encode_page_chain(
	kind: CatalogKind,
	root: PageNo,
	continuation_start: PageNo,
	payload: &[u8],
	page_lsn: PageLsn,
) -> FerrosGrynnResult<(Vec<PageNo>, Vec<AlignedPage>)> {
	validate_size(payload.len())?;
	let page_count = payload.len().div_ceil(PAYLOAD_BYTES).max(1);
	let page_count_u64 =
		u64::try_from(page_count).map_err(|_error| invalid("catalog page count exceeds u64"))?;
	let total_len =
		u32::try_from(payload.len()).map_err(|_error| invalid("catalog payload exceeds u32"))?;
	let mut page_numbers = Vec::with_capacity(page_count);
	let mut pages = Vec::with_capacity(page_count);
	for index in 0..page_count {
		let index =
			u64::try_from(index).map_err(|_error| invalid("catalog page index exceeds u64"))?;
		let page_no = if index == 0 {
			root
		} else {
			checked_page(continuation_start, index - 1)?
		};
		let next_page = if index + 1 == page_count_u64 {
			PageNo::NULL
		} else {
			checked_page(continuation_start, index)?
		};
		let start = usize::try_from(index)
			.ok()
			.and_then(|index| index.checked_mul(PAYLOAD_BYTES))
			.ok_or_else(|| invalid("catalog chunk offset overflows usize"))?;
		let end = start.saturating_add(PAYLOAD_BYTES).min(payload.len());
		let chunk = &payload[start..end];
		let mut page =
			AlignedPage::new(PageType::SchemaRoot, PageFlags::empty(), page_lsn, page_no);
		let bytes = page.as_bytes_mut();
		bytes[PAGE_HEADER_SIZE..KIND_OFFSET].copy_from_slice(&MAGIC);
		bytes[KIND_OFFSET] = kind as u8;
		bytes[VERSION_OFFSET] = VERSION;
		bytes[NEXT_PAGE_OFFSET..CHUNK_LEN_OFFSET].copy_from_slice(&next_page.get().to_le_bytes());
		bytes[CHUNK_LEN_OFFSET..TOTAL_LEN_OFFSET].copy_from_slice(
			&u32::try_from(chunk.len())
				.map_err(|_error| invalid("catalog chunk exceeds u32"))?
				.to_le_bytes(),
		);
		bytes[TOTAL_LEN_OFFSET..RESERVED_B_OFFSET].copy_from_slice(&total_len.to_le_bytes());
		bytes[PAYLOAD_OFFSET..PAYLOAD_OFFSET + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
		page_numbers.push(page_no);
		pages.push(page);
	}
	Ok((page_numbers, pages))
}

/// Loads one bounded page chain without trusting persisted links or lengths.
pub(in crate::storage) fn decode_page_chain(
	read_page: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	expected_kind: CatalogKind,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut page_no = root;
	let mut visited = BTreeSet::new();
	let mut output = Vec::new();
	let mut expected_total = None;
	loop {
		if page_no == PageNo::NULL || !visited.insert(page_no) {
			return Err(invalid("catalog page chain is null or cyclic"));
		}
		let mut page = AlignedPage::zeroed();
		read_page(page_no, &mut page)?;
		let header = page.validate(page_no)?;
		if header.page_type() != PageType::SchemaRoot || !header.flags().is_empty() {
			return Err(invalid("catalog chain contains an invalid page type or flags"));
		}
		let bytes = page.as_bytes();
		if bytes[PAGE_HEADER_SIZE..KIND_OFFSET] != MAGIC
			|| CatalogKind::from_raw(bytes[KIND_OFFSET]) != Some(expected_kind)
			|| bytes[VERSION_OFFSET] != VERSION
		{
			return Err(invalid("catalog page marker, kind, or version is invalid"));
		}
		expect_zero(&bytes[RESERVED_A_OFFSET..NEXT_PAGE_OFFSET], "catalog page reserved bytes")?;
		expect_zero(&bytes[RESERVED_B_OFFSET..PAYLOAD_OFFSET], "catalog page reserved bytes")?;
		let next_page = PageNo::new(read_u64(bytes, NEXT_PAGE_OFFSET));
		let chunk_len = usize::try_from(read_u32(bytes, CHUNK_LEN_OFFSET))
			.map_err(|_error| invalid("catalog chunk length does not fit usize"))?;
		let total_len = usize::try_from(read_u32(bytes, TOTAL_LEN_OFFSET))
			.map_err(|_error| invalid("catalog total length does not fit usize"))?;
		validate_size(total_len)?;
		if chunk_len > PAYLOAD_BYTES || expected_total.is_some_and(|value| value != total_len) {
			return Err(invalid("catalog page contains inconsistent lengths"));
		}
		if expected_total.is_none() {
			output.reserve(total_len);
			expected_total = Some(total_len);
		}
		if chunk_len > total_len.saturating_sub(output.len()) {
			return Err(invalid("catalog chunks exceed the declared total length"));
		}
		output.extend_from_slice(&bytes[PAYLOAD_OFFSET..PAYLOAD_OFFSET + chunk_len]);
		expect_zero(&bytes[PAYLOAD_OFFSET + chunk_len..], "catalog page trailing bytes")?;
		if next_page == PageNo::NULL {
			break;
		}
		if output.len() == total_len {
			return Err(invalid("catalog page chain continues after its declared payload"));
		}
		page_no = next_page;
	}
	if output.len() != expected_total.unwrap_or(0) {
		return Err(invalid("catalog page chain ended before its declared payload"));
	}
	Ok(output)
}

fn checked_page(start: PageNo, offset: u64) -> FerrosGrynnResult<PageNo> {
	start
		.get()
		.checked_add(offset)
		.map(PageNo::new)
		.ok_or_else(|| invalid("catalog continuation page number overflows u64"))
}

fn validate_size(length: usize) -> FerrosGrynnResult<()> {
	if length > MAX_CATALOG_BYTES {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"catalog bytes",
			MAX_CATALOG_BYTES as u64,
			length as u64,
		));
	}
	Ok(())
}

fn expect_zero(bytes: &[u8], field: &str) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid(format!("{field} are non-zero")));
	}
	Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exact_header_and_multi_page_round_trip() -> FerrosGrynnResult<()> {
		let payload = vec![0x5a; PAYLOAD_BYTES + 7];
		let (page_numbers, pages) = encode_page_chain(
			CatalogKind::Database,
			PageNo::new(3),
			PageNo::new(9),
			&payload,
			PageLsn::ZERO,
		)?;
		assert_eq!(pages.len(), 2);
		let first = pages[0].as_bytes();
		assert_eq!(&first[PAGE_HEADER_SIZE..KIND_OFFSET], b"FGC1");
		assert_eq!(first[KIND_OFFSET], CatalogKind::Database as u8);
		assert_eq!(first[VERSION_OFFSET], VERSION);
		assert_eq!(read_u64(first, NEXT_PAGE_OFFSET), 9);
		assert_eq!(read_u32(first, CHUNK_LEN_OFFSET) as usize, PAYLOAD_BYTES);
		assert_eq!(read_u32(first, TOTAL_LEN_OFFSET) as usize, payload.len());

		let decoded = decode_page_chain(
			&mut |page_no, destination| {
				let source = page_numbers
					.iter()
					.position(|candidate| *candidate == page_no)
					.and_then(|index| pages.get(index))
					.ok_or_else(|| {
						invalid(format!("test catalog page {} is absent", page_no.get()))
					})?;
				destination.as_bytes_mut().copy_from_slice(source.as_bytes());
				Ok(())
			},
			PageNo::new(3),
			CatalogKind::Database,
		)?;
		assert_eq!(decoded, payload);
		Ok(())
	}

	#[test]
	fn rejects_non_zero_trailing_bytes() -> FerrosGrynnResult<()> {
		let (_page_numbers, mut pages) = encode_page_chain(
			CatalogKind::Files,
			PageNo::new(0),
			PageNo::new(1),
			b"payload",
			PageLsn::ZERO,
		)?;
		let page = &mut pages[0];
		page.as_bytes_mut()[PAYLOAD_OFFSET + 7] = 1;
		page.seal()?;
		assert!(
			decode_page_chain(
				&mut |_page_no, destination| {
					destination.as_bytes_mut().copy_from_slice(page.as_bytes());
					Ok(())
				},
				PageNo::new(0),
				CatalogKind::Files,
			)
			.is_err()
		);
		Ok(())
	}
}
