//! Canonical packed array extents shared by graph artifact files.

use super::{SectionKind, decode_array, encode_array, invalid};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::page::{AlignedPage, PAGE_SIZE, PageNo},
};

/// One exact-width section and its consecutive page extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArraySection {
	/// Normative section identity.
	pub kind: SectionKind,
	/// Exact element width.
	pub width: u16,
	/// Exact element count.
	pub count: u64,
	/// First page or NULL for an empty section.
	pub root: PageNo,
}

impl ArraySection {
	/// Admits encoded payload, decoded views and conservative relation
	/// workspace.
	///
	/// # Errors
	/// Rejects invalid widths, checked-size overflow and scratch exhaustion.
	pub fn admit_decode(
		sections: &[Self],
		vertices: u64,
		edges: u64,
		scratch_bytes: usize,
	) -> FerrosGrynnResult<()> {
		let mut payload = 0_u64;
		for section in sections {
			section.capacity()?;
			payload = section
				.count
				.checked_mul(u64::from(section.width))
				.and_then(|bytes| payload.checked_add(bytes))
				.ok_or_else(|| invalid("graph artifact payload bytes overflow"))?;
		}
		let peak = payload
			.checked_mul(2)
			.and_then(|bytes| {
				vertices.checked_mul(512).and_then(|vertices| bytes.checked_add(vertices))
			})
			.and_then(|bytes| edges.checked_mul(128).and_then(|edges| bytes.checked_add(edges)))
			.and_then(|bytes| bytes.checked_add(65_536));
		if peak.is_none_or(|bytes| bytes > scratch_bytes as u64) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"graph artifact decode scratch bytes",
				scratch_bytes as u64,
			));
		}
		Ok(())
	}

	/// Returns exact page capacity after checking the section family and width.
	///
	/// # Errors
	/// Rejects invalid/zero widths before division or page allocation.
	pub fn capacity(self) -> FerrosGrynnResult<u64> {
		self.kind.array_type(self.width)?;
		Ok(((PAGE_SIZE - 64) / usize::from(self.width)) as u64)
	}

	/// Loads an admitted fixed section set with exact lengths and checked
	/// pages. Family layouts admit their complete decoded views before
	/// calling this.
	///
	/// # Errors
	/// Rejects payload overflow, scratch, malformed extents/pages or
	/// cancellation.
	pub fn load_sections<const N: usize, Read, Check>(
		sections: &[Self; N],
		page_count: u64,
		scratch_bytes: usize,
		mut read: Read,
		check: &mut Check,
	) -> FerrosGrynnResult<[Vec<u8>; N]>
	where
		Read: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		check()?;
		let resource = || {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"graph artifact decode scratch bytes",
				scratch_bytes as u64,
			)
		};
		let mut total = 0_u64;
		for section in sections {
			section.check_extent(page_count)?;
			total = section
				.count
				.checked_mul(u64::from(section.width))
				.and_then(|length| total.checked_add(length))
				.ok_or_else(resource)?;
		}
		if total > scratch_bytes as u64 {
			return Err(resource());
		}
		let mut data: [Vec<u8>; N] = std::array::from_fn(|_| Vec::new());
		for (section, bytes) in sections.iter().zip(&mut data) {
			check()?;
			let length = usize::try_from(
				section.count.checked_mul(u64::from(section.width)).ok_or_else(resource)?,
			)
			.map_err(|cause| resource().with_technical_source(cause))?;
			bytes
				.try_reserve_exact(length)
				.map_err(|cause| resource().with_technical_source(cause))?;
			section.visit(page_count, &mut read, |_, element| {
				check()?;
				bytes.extend_from_slice(element);
				Ok(())
			})?;
		}
		Ok(data)
	}

	/// Packs every section consecutively after page zero and projects full
	/// bytes.
	///
	/// # Errors
	/// Rejects section widths, overflow or MAX_BYTES before any page write.
	pub fn pack(sections: &mut [Self], max_bytes: u64) -> FerrosGrynnResult<u64> {
		let mut pages = 1_u64;
		for section in sections {
			let capacity = section.capacity()?;
			section.root = if section.count == 0 {
				PageNo::NULL
			} else {
				PageNo::new(pages)
			};
			pages = pages
				.checked_add(section.count.div_ceil(capacity))
				.ok_or_else(|| invalid("derived section extent overflows"))?;
		}
		if pages.checked_mul(PAGE_SIZE as u64).is_none_or(|bytes| bytes > max_bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"graph artifact bytes",
				max_bytes,
			));
		}
		Ok(pages)
	}

	/// Streams a checked section through the caller's final aligned
	/// destination.
	///
	/// # Errors
	/// Propagates cancellation, packing, element and output errors.
	pub fn stream<Element, Write, Check>(
		self,
		page: &mut AlignedPage,
		element: &mut Element,
		write: &mut Write,
		check: &mut Check,
	) -> FerrosGrynnResult<()>
	where
		Element: FnMut(SectionKind, u64, &mut [u8]) -> FerrosGrynnResult<()>,
		Write: FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		let capacity = self.check_extent(u64::MAX)?;
		let mut payload = [0_u8; PAGE_SIZE - 64];
		let mut ordinal = 0;
		while ordinal < self.count {
			check()?;
			let count = capacity.min(self.count - ordinal);
			let used = count as usize * usize::from(self.width);
			payload[..used].fill(0);
			for (offset, bytes) in
				payload[..used].chunks_exact_mut(usize::from(self.width)).enumerate()
			{
				element(self.kind, ordinal + offset as u64, bytes)?;
			}
			let at = PageNo::new(self.root.get() + ordinal / capacity);
			let next = if ordinal + count == self.count {
				PageNo::NULL
			} else {
				PageNo::new(at.get() + 1)
			};
			encode_array(page, at, next, self.kind, self.width, ordinal, &payload[..used])?;
			write(at, page)?;
			ordinal += count;
		}
		Ok(())
	}

	/// Visits exact canonical packing and links using one reusable aligned
	/// page.
	///
	/// # Errors
	/// Rejects bad extent, page identity/checksum, lengths, links or padding.
	pub fn visit<Read, Visit>(
		self,
		page_count: u64,
		mut read: Read,
		mut visit: Visit,
	) -> FerrosGrynnResult<()>
	where
		Read: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
		Visit: FnMut(u64, &[u8]) -> FerrosGrynnResult<()>,
	{
		let capacity = self.check_extent(page_count)?;
		if self.count == 0 {
			return Ok(());
		}
		let mut page = Box::new(AlignedPage::zeroed());
		let mut ordinal = 0;
		while ordinal < self.count {
			let at = PageNo::new(self.root.get() + ordinal / capacity);
			read(at, &mut page)?;
			let decoded = decode_array(&page, at, page_count, self.kind, self.width, ordinal)?;
			let count = capacity.min(self.count - ordinal);
			let next = if ordinal + count == self.count {
				PageNo::NULL
			} else {
				PageNo::new(at.get() + 1)
			};
			if decoded.elements.len() as u64 != count * u64::from(self.width)
				|| decoded.next != next
			{
				return Err(invalid("derived section packing or links are noncanonical"));
			}
			for bytes in decoded.elements.chunks_exact(usize::from(self.width)) {
				visit(ordinal, bytes)?;
				ordinal += 1;
			}
		}
		Ok(())
	}

	/// Checks all address arithmetic before allocating or traversing pages.
	fn check_extent(self, page_count: u64) -> FerrosGrynnResult<u64> {
		let capacity = self.capacity()?;
		if page_count == 0
			|| (self.root == PageNo::NULL) != (self.count == 0)
			|| self.count != 0
				&& (self.root.get() == 0
					|| self
						.root
						.get()
						.checked_add(self.count.div_ceil(capacity))
						.is_none_or(|end| end > page_count))
		{
			return Err(invalid("derived section root or extent is invalid"));
		}
		Ok(capacity)
	}
}
