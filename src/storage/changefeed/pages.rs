//! Append-only whole-commit pages; catalog ceilings define snapshot visibility.

use super::{CodecBudget, checked_payload, invalid};
use crate::{
	changefeed::ChangefeedId,
	gql_status::FerrosGrynnResult,
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

/// First byte of the private page body.
const BODY: usize = PAGE_HEADER_SIZE;
/// Complete explicit segment-header body before canonical zero padding.
const SEGMENT_BYTES: usize = 120;
/// Complete continuation framing before its checked payload slice.
const ENTRY_BYTES: usize = 48;
/// Maximum payload bytes in one continuation page.
const CHUNK_BYTES: usize = PAGE_SIZE - PAGE_HEADER_SIZE - ENTRY_BYTES;
/// Current development framing version; older decoders are not provided.
const VERSION: u16 = 1;

/// Computes checked continuation count before deciding whole-group rotation.
pub(in crate::storage) fn payload_pages(bytes: usize) -> FerrosGrynnResult<u32> {
	checked_payload(bytes as u64)?;
	if bytes == 0 {
		return Err(invalid("changefeed group payload is empty"));
	}
	u32::try_from(bytes.div_ceil(CHUNK_BYTES))
		.map_err(|_error| invalid("changefeed page count exceeds u32"))
}

/// Catalog-comparable segment identity and trusted whole-group bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct SegmentHeader {
	/// Durable database origin, never a process-local transaction identity.
	pub(in crate::storage) database: uuid::Uuid,
	/// Stable source incarnation owning this segment.
	pub(in crate::storage) feed: ChangefeedId,
	/// Unique append-only segment incarnation.
	pub(in crate::storage) segment: uuid::Uuid,
	/// Exclusive page ceiling including header page zero.
	pub(in crate::storage) pages: u64,
	/// Complete logical commit groups in this segment.
	pub(in crate::storage) commits: u64,
	/// First stored whole-commit stamp.
	pub(in crate::storage) first_stamp: u64,
	/// Last stored whole-commit stamp.
	pub(in crate::storage) last_stamp: u64,
	/// Earliest checked UTC epoch nanoseconds.
	pub(in crate::storage) first_time: i128,
	/// Latest checked UTC epoch nanoseconds.
	pub(in crate::storage) last_time: i128,
}

/// One checked catalog locator; continuation never resumes inside its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct CommitLocator {
	/// First contiguous continuation page, strictly after the segment header.
	pub(in crate::storage) first_page: u64,
	/// Exact continuation count, independently verified against payload bytes.
	pub(in crate::storage) pages: u32,
	/// Complete encoded logical commit length.
	pub(in crate::storage) bytes: u64,
	/// Stamp shared by every continuation and the decoded logical commit.
	pub(in crate::storage) stamp: u64,
}

impl SegmentHeader {
	/// Checks identities, counts and clocks independently of physical
	/// checksums.
	pub(in crate::storage) fn validate(self) -> FerrosGrynnResult<()> {
		if self.database.is_nil()
			|| self.segment.is_nil()
			|| self.pages < 2
			|| self.commits == 0
			|| self.commits >= self.pages
			|| self.first_stamp == 0
			|| self.first_stamp > self.last_stamp
			|| self.first_time > self.last_time
		{
			return Err(invalid("changefeed segment identities, counts or bounds are invalid"));
		}
		if self.commits - 1 > self.last_stamp - self.first_stamp
			|| self.commits == 1
				&& (self.first_stamp != self.last_stamp || self.first_time != self.last_time)
		{
			return Err(invalid(
				"changefeed segment commit count contradicts its stamp or time bounds",
			));
		}
		for time in [self.first_time, self.last_time] {
			jiff::Timestamp::from_nanosecond(time).map_err(|_error| {
				invalid("changefeed segment timestamp is outside the UTC range")
			})?;
		}
		Ok(())
	}
}

impl CommitLocator {
	/// Validates all allocation and page-reference bounds before page access.
	fn validate(self, ceiling: u64) -> FerrosGrynnResult<usize> {
		let bytes = checked_payload(self.bytes)?;
		let end = self
			.first_page
			.checked_add(u64::from(self.pages))
			.ok_or_else(|| invalid("changefeed continuation pages overflow"))?;
		if bytes == 0
			|| self.first_page == 0
			|| self.stamp == 0
			|| self.pages as usize != bytes.div_ceil(CHUNK_BYTES)
			|| end > ceiling
		{
			return Err(invalid("changefeed group reference or continuation count is invalid"));
		}
		Ok(bytes)
	}
}

/// Initializes final aligned storage in place, preserving the bulk-page
/// invariant.
pub(in crate::storage) fn encode_header(
	header: SegmentHeader,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	header.validate()?;
	output.reset(PageType::ChangefeedHeader, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	let bytes = output.as_bytes_mut();
	bytes[BODY..BODY + 4].copy_from_slice(b"FCFS");
	bytes[BODY + 4..BODY + 6].copy_from_slice(&VERSION.to_le_bytes());
	bytes[BODY + 8..BODY + 24].copy_from_slice(header.database.as_bytes());
	bytes[BODY + 24..BODY + 40].copy_from_slice(header.feed.as_uuid().as_inner().as_bytes());
	bytes[BODY + 40..BODY + 56].copy_from_slice(header.segment.as_bytes());
	for (offset, value) in [
		(56, header.pages),
		(64, header.commits),
		(72, header.first_stamp),
		(80, header.last_stamp),
	] {
		bytes[BODY + offset..BODY + offset + 8].copy_from_slice(&value.to_le_bytes());
	}
	bytes[BODY + 88..BODY + 104].copy_from_slice(&header.first_time.to_le_bytes());
	bytes[BODY + 104..BODY + 120].copy_from_slice(&header.last_time.to_le_bytes());
	output.seal()
}

/// Validates one complete header before trusting its identities and bounds.
pub(in crate::storage) fn decode_header(page: &AlignedPage) -> FerrosGrynnResult<SegmentHeader> {
	check_page(page, PageNo::new(0), PageType::ChangefeedHeader)?;
	let bytes = page.as_bytes();
	if &bytes[BODY..BODY + 4] != b"FCFS"
		|| u16::from_le_bytes([bytes[BODY + 4], bytes[BODY + 5]]) != VERSION
	{
		return Err(invalid("changefeed segment framing marker/version is invalid"));
	}
	check_zero(&bytes[BODY + 6..BODY + 8])?;
	check_zero(&bytes[BODY + SEGMENT_BYTES..])?;
	let header = SegmentHeader {
		database: read_uuid(&bytes[BODY + 8..BODY + 24])?,
		feed: ChangefeedId::from_uuid(read_uuid(&bytes[BODY + 24..BODY + 40])?.into())
			.map_err(|_error| invalid("changefeed source identity is nil"))?,
		segment: read_uuid(&bytes[BODY + 40..BODY + 56])?,
		pages: u64_at(bytes, BODY + 56),
		commits: u64_at(bytes, BODY + 64),
		first_stamp: u64_at(bytes, BODY + 72),
		last_stamp: u64_at(bytes, BODY + 80),
		first_time: i128::from_le_bytes(
			bytes[BODY + 88..BODY + 104]
				.try_into()
				.map_err(|_error| invalid("changefeed first timestamp is truncated"))?,
		),
		last_time: i128::from_le_bytes(
			bytes[BODY + 104..BODY + 120]
				.try_into()
				.map_err(|_error| invalid("changefeed last timestamp is truncated"))?,
		),
	};
	header.validate()?;
	Ok(header)
}

/// Admits a complete final page array before filling any aligned slot.
pub(in crate::storage) fn encode_pages(
	payload: &[u8],
	stamp: u64,
	first_page: u64,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<(CommitLocator, Vec<AlignedPage>)> {
	let pages = u32::try_from(payload.len().div_ceil(CHUNK_BYTES))
		.map_err(|_error| invalid("changefeed page count exceeds u32"))?;
	let locator = CommitLocator {
		first_page,
		pages,
		bytes: payload.len() as u64,
		stamp,
	};
	locator.validate(
		first_page
			.checked_add(u64::from(pages))
			.ok_or_else(|| invalid("changefeed page ceiling overflows"))?,
	)?;
	budget.admit(
		(pages as usize)
			.checked_mul(PAGE_SIZE)
			.ok_or_else(|| invalid("changefeed page allocation overflows"))?,
	)?;
	let mut output = Vec::new();
	output
		.try_reserve_exact(pages as usize)
		.map_err(|_error| invalid("changefeed continuation allocation failed"))?;
	output.resize_with(pages as usize, AlignedPage::zeroed);
	for (index, page) in output.iter_mut().enumerate() {
		page.reset(
			PageType::ChangefeedEntry,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(first_page + index as u64),
		);
		let start = index * CHUNK_BYTES;
		let end = start.saturating_add(CHUNK_BYTES).min(payload.len());
		let chunk = &payload[start..end];
		let bytes = page.as_bytes_mut();
		bytes[BODY..BODY + 4].copy_from_slice(b"FCFE");
		bytes[BODY + 4..BODY + 6].copy_from_slice(&VERSION.to_le_bytes());
		for (offset, value) in [(8, stamp), (16, first_page), (32, locator.bytes)] {
			bytes[BODY + offset..BODY + offset + 8].copy_from_slice(&value.to_le_bytes());
		}
		for (offset, value) in [(24, index as u32), (28, pages), (40, chunk.len() as u32)] {
			bytes[BODY + offset..BODY + offset + 4].copy_from_slice(&value.to_le_bytes());
		}
		bytes[BODY + ENTRY_BYTES..BODY + ENTRY_BYTES + chunk.len()].copy_from_slice(chunk);
		page.seal()?;
	}
	Ok((locator, output))
}

/// Reads one whole group using bounded output and one reusable aligned read
/// page.
pub(in crate::storage) fn decode_pages(
	locator: CommitLocator,
	ceiling: u64,
	read: &mut impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<Vec<u8>> {
	let length = locator.validate(ceiling)?;
	budget.admit(
		length
			.checked_add(PAGE_SIZE)
			.ok_or_else(|| invalid("changefeed read scratch overflows"))?,
	)?;
	let mut output = Vec::new();
	output
		.try_reserve_exact(length)
		.map_err(|_error| invalid("changefeed read allocation failed"))?;
	let mut page = AlignedPage::zeroed();
	for index in 0..locator.pages {
		let number = PageNo::new(locator.first_page + u64::from(index));
		read(number, &mut page)?;
		check_page(&page, number, PageType::ChangefeedEntry)?;
		let bytes = page.as_bytes();
		if &bytes[BODY..BODY + 4] != b"FCFE"
			|| u16::from_le_bytes([bytes[BODY + 4], bytes[BODY + 5]]) != VERSION
			|| u64_at(bytes, BODY + 8) != locator.stamp
			|| u64_at(bytes, BODY + 16) != locator.first_page
			|| u32_at(bytes, BODY + 24) != index
			|| u32_at(bytes, BODY + 28) != locator.pages
			|| u64_at(bytes, BODY + 32) != locator.bytes
		{
			return Err(invalid("changefeed continuation identity/ordinal/length is invalid"));
		}
		check_zero(&bytes[BODY + 6..BODY + 8])?;
		check_zero(&bytes[BODY + 44..BODY + ENTRY_BYTES])?;
		let chunk = u32_at(bytes, BODY + 40) as usize;
		if chunk != (length - output.len()).min(CHUNK_BYTES) {
			return Err(invalid("changefeed continuation chunk length is not canonical"));
		}
		output.extend_from_slice(&bytes[BODY + ENTRY_BYTES..BODY + ENTRY_BYTES + chunk]);
		check_zero(&bytes[BODY + ENTRY_BYTES + chunk..])?;
	}
	if output.len() != length {
		return Err(invalid("changefeed continuation chain is incomplete"));
	}
	Ok(output)
}

/// Decodes the first fragment's checked locator without allocating its payload.
pub(in crate::storage) fn decode_locator(
	page: &AlignedPage,
	number: PageNo,
	ceiling: u64,
) -> FerrosGrynnResult<CommitLocator> {
	check_page(page, number, PageType::ChangefeedEntry)?;
	let bytes = page.as_bytes();
	if &bytes[BODY..BODY + 4] != b"FCFE"
		|| u16::from_le_bytes([bytes[BODY + 4], bytes[BODY + 5]]) != VERSION
		|| u64_at(bytes, BODY + 16) != number.get()
		|| u32_at(bytes, BODY + 24) != 0
	{
		return Err(invalid("changefeed first continuation identity is invalid"));
	}
	check_zero(&bytes[BODY + 6..BODY + 8])?;
	check_zero(&bytes[BODY + 44..BODY + ENTRY_BYTES])?;
	let locator = CommitLocator {
		first_page: number.get(),
		pages: u32_at(bytes, BODY + 28),
		bytes: u64_at(bytes, BODY + 32),
		stamp: u64_at(bytes, BODY + 8),
	};
	let length = locator.validate(ceiling)?;
	let chunk = u32_at(bytes, BODY + 40) as usize;
	if chunk != length.min(CHUNK_BYTES) {
		return Err(invalid("changefeed first chunk length is not canonical"));
	}
	check_zero(&bytes[BODY + ENTRY_BYTES + chunk..])?;
	Ok(locator)
}

/// Borrows the checked first fragment after locator validation.
pub(in crate::storage) fn first_payload(page: &AlignedPage) -> FerrosGrynnResult<&[u8]> {
	let bytes = page.as_bytes();
	let chunk = u32_at(bytes, BODY + 40) as usize;
	bytes
		.get(BODY + ENTRY_BYTES..)
		.and_then(|body| body.get(..chunk))
		.ok_or_else(|| invalid("changefeed first payload exceeds page bounds"))
}

/// Checks the common format, checksum, page identity and private family flags.
fn check_page(page: &AlignedPage, number: PageNo, kind: PageType) -> FerrosGrynnResult<()> {
	let header = page.validate(number)?;
	if header.page_type() != kind || !header.flags().is_empty() {
		return Err(invalid("changefeed page type or flags are invalid"));
	}
	Ok(())
}
/// Rejects every reserved/trailing byte before decoding further fields.
fn check_zero(bytes: &[u8]) -> FerrosGrynnResult<()> {
	if bytes.iter().any(|byte| *byte != 0) {
		return Err(invalid("changefeed page reserved bytes are nonzero"));
	}
	Ok(())
}
/// Reads a UUID from an already bounded exact slice.
fn read_uuid(bytes: &[u8]) -> FerrosGrynnResult<uuid::Uuid> {
	uuid::Uuid::from_slice(bytes).map_err(|_error| invalid("changefeed UUID bytes are malformed"))
}
/// Reads a fixed field only at a format-controlled aligned-page offset.
fn u64_at(bytes: &[u8; PAGE_SIZE], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}
/// Reads a fixed field only at a format-controlled aligned-page offset.
fn u32_at(bytes: &[u8; PAGE_SIZE], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
	use super::{
		BODY,
		CHUNK_BYTES,
		CommitLocator,
		SegmentHeader,
		decode_header,
		decode_pages,
		encode_header,
		encode_pages,
	};
	use crate::{
		changefeed::ChangefeedId,
		gql_status::FerrosGrynnResult,
		storage::{
			changefeed::{CodecBudget, invalid},
			page::{AlignedPage, PAGE_SIZE},
		},
	};

	#[test]
	fn changefeed_segment_header_golden_and_reserved_bytes() -> FerrosGrynnResult<()> {
		let header = SegmentHeader {
			database: uuid::Uuid::from_u128(1),
			feed: ChangefeedId::from_uuid(uuid::Uuid::from_u128(2).into())?,
			segment: uuid::Uuid::from_u128(3),
			pages: 3,
			commits: 1,
			first_stamp: 9,
			last_stamp: 9,
			first_time: 10,
			last_time: 10,
		};
		let mut page = AlignedPage::zeroed();
		encode_header(header, &mut page)?;
		assert_eq!(decode_header(&page)?, header);
		assert_eq!(&page.as_bytes()[BODY..BODY + 8], &[b'F', b'C', b'F', b'S', 1, 0, 0, 0]);
		assert_eq!(&page.as_bytes()[BODY + 56..BODY + 88], &[
			3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0,
			0, 0, 0
		]);
		let mut golden = [0_u8; 120];
		golden[..8].copy_from_slice(b"FCFS\x01\0\0\0");
		golden[23] = 1;
		golden[39] = 2;
		golden[55] = 3;
		golden[56] = 3;
		golden[64] = 1;
		golden[72] = 9;
		golden[80] = 9;
		golden[88] = 10;
		golden[104] = 10;
		assert_eq!(&page.as_bytes()[BODY..BODY + 120], golden);
		for offset in [BODY + 6, BODY + 120, PAGE_SIZE - 1] {
			encode_header(header, &mut page)?;
			page.as_bytes_mut()[offset] = 1;
			page.seal()?;
			assert!(decode_header(&page).is_err());
		}
		for bad in [
			SegmentHeader {
				first_stamp: 10,
				..header
			},
			SegmentHeader {
				commits: 2,
				..header
			},
			SegmentHeader {
				last_stamp: 10,
				..header
			},
			SegmentHeader {
				last_time: 11,
				..header
			},
		] {
			assert!(encode_header(bad, &mut page).is_err());
		}
		Ok(())
	}

	#[test]
	fn changefeed_continuations_roundtrip_large_payload_and_validate_ordinals()
	-> FerrosGrynnResult<()> {
		let payload = vec![0x5a; CHUNK_BYTES * 2 + 9];
		let (locator, mut pages) = encode_pages(
			&payload,
			1_u64 << 54,
			7,
			&mut CodecBudget::new(16 * PAGE_SIZE, payload.len())?,
		)?;
		let mut read = |number: crate::storage::page::PageNo, output: &mut AlignedPage| {
			let page = pages
				.get((number.get() - 7) as usize)
				.ok_or_else(|| invalid("fixture page is absent"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		assert_eq!(
			decode_pages(locator, 10, &mut read, &mut CodecBudget::new(16 * PAGE_SIZE, 0)?)?,
			payload
		);
		let middle = pages.get_mut(1).ok_or_else(|| invalid("fixture middle page is absent"))?;
		middle.as_bytes_mut()[BODY + 24..BODY + 28].copy_from_slice(&0_u32.to_le_bytes());
		middle.seal()?;
		let mut read = |number: crate::storage::page::PageNo, output: &mut AlignedPage| {
			let page = pages
				.get((number.get() - 7) as usize)
				.ok_or_else(|| invalid("fixture page is absent"))?;
			output.as_bytes_mut().copy_from_slice(page.as_bytes());
			Ok(())
		};
		assert!(
			decode_pages(locator, 10, &mut read, &mut CodecBudget::new(16 * PAGE_SIZE, 0)?)
				.is_err()
		);
		Ok(())
	}

	#[test]
	fn changefeed_continuations_reject_untrusted_lengths_and_budgets_before_read()
	-> FerrosGrynnResult<()> {
		let mut reads = 0;
		let mut read = |_number, _page: &mut AlignedPage| {
			reads += 1;
			Err(invalid("page read must not happen"))
		};
		let locator = CommitLocator {
			first_page: 1,
			pages: 1,
			bytes: 1,
			stamp: 1,
		};
		for bad in [
			CommitLocator {
				bytes: u64::MAX,
				..locator
			},
			CommitLocator {
				pages: u32::MAX,
				..locator
			},
			CommitLocator {
				first_page: u64::MAX,
				..locator
			},
			CommitLocator {
				first_page: 0,
				..locator
			},
		] {
			assert!(
				decode_pages(bad, 2, &mut read, &mut CodecBudget::new(4 * PAGE_SIZE, 0)?).is_err()
			);
		}
		assert!(decode_pages(locator, 2, &mut read, &mut CodecBudget::new(1, 0)?).is_err());
		assert_eq!(reads, 0);
		assert!(encode_pages(&[1], 1, 1, &mut CodecBudget::new(PAGE_SIZE - 1, 0)?).is_err());
		Ok(())
	}

	#[test]
	fn changefeed_continuation_golden_boundaries_and_resealed_corruption() -> FerrosGrynnResult<()>
	{
		for length in [1, CHUNK_BYTES, CHUNK_BYTES + 1] {
			let payload = vec![0xa5; length];
			let (locator, mut pages) =
				encode_pages(&payload, 9, 1, &mut CodecBudget::new(16 * PAGE_SIZE, 0)?)?;
			let mut golden = [0_u8; 48];
			golden[..8].copy_from_slice(b"FCFE\x01\0\0\0");
			golden[8] = 9;
			golden[16] = 1;
			golden[28] = locator.pages as u8;
			golden[32..40].copy_from_slice(&(length as u64).to_le_bytes());
			golden[40..44].copy_from_slice(&(length.min(CHUNK_BYTES) as u32).to_le_bytes());
			assert_eq!(&pages[0].as_bytes()[BODY..BODY + 48], golden);
			let mut read = |number: crate::storage::page::PageNo, output: &mut AlignedPage| {
				output
					.as_bytes_mut()
					.copy_from_slice(pages[(number.get() - 1) as usize].as_bytes());
				Ok(())
			};
			assert_eq!(
				decode_pages(
					locator,
					1 + u64::from(locator.pages),
					&mut read,
					&mut CodecBudget::new(16 * PAGE_SIZE, 0)?
				)?,
				payload
			);
			assert!(
				decode_pages(
					locator,
					u64::from(locator.pages),
					&mut read,
					&mut CodecBudget::new(16 * PAGE_SIZE, 0)?
				)
				.is_err()
			);
			for offset in
				[BODY + 6, BODY + 8, BODY + 16, BODY + 28, BODY + 32, BODY + 40, BODY + 44]
			{
				let original = pages[0].as_bytes()[offset];
				pages[0].as_bytes_mut()[offset] ^= 1;
				pages[0].seal()?;
				let mut read = |number: crate::storage::page::PageNo, output: &mut AlignedPage| {
					output
						.as_bytes_mut()
						.copy_from_slice(pages[(number.get() - 1) as usize].as_bytes());
					Ok(())
				};
				assert!(
					decode_pages(
						locator,
						1 + u64::from(locator.pages),
						&mut read,
						&mut CodecBudget::new(16 * PAGE_SIZE, 0)?
					)
					.is_err()
				);
				pages[0].as_bytes_mut()[offset] = original;
				pages[0].seal()?;
			}
			if length % CHUNK_BYTES != 0 {
				let last =
					pages.last_mut().ok_or_else(|| invalid("fixture continuation is absent"))?;
				last.as_bytes_mut()[PAGE_SIZE - 1] = 1;
				last.seal()?;
				let mut read = |number: crate::storage::page::PageNo, output: &mut AlignedPage| {
					output
						.as_bytes_mut()
						.copy_from_slice(pages[(number.get() - 1) as usize].as_bytes());
					Ok(())
				};
				assert!(
					decode_pages(
						locator,
						1 + u64::from(locator.pages),
						&mut read,
						&mut CodecBudget::new(16 * PAGE_SIZE, 0)?
					)
					.is_err()
				);
			}
		}
		assert!(encode_pages(&[], 1, 1, &mut CodecBudget::new(PAGE_SIZE, 0)?).is_err());
		Ok(())
	}
}
