//! Sequential page-framed byte streams for immutable side-files.

use super::{
	direct::DirectFile,
	page::{AlignedPage, PAGE_HEADER_SIZE, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// First byte available to the logical stream after the common page header.
pub(crate) const STREAM_HEADER_BYTES: usize = 48;
/// Payload capacity of one stream page.
pub(crate) const STREAM_PAYLOAD_BYTES: usize = PAGE_SIZE - STREAM_HEADER_BYTES;
/// Null link used by empty streams and the final page in a chain.
pub(crate) const NULL_STREAM_PAGE: u64 = u64::MAX;

/// Exact location and logical length of one contiguous page stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImmutableStreamLocation {
	pub(crate) first: u64,
	pub(crate) pages: u64,
	pub(crate) bytes: u64,
}

/// Appends a single typed stream directly to a [`DirectFile`].
///
/// A full page is retained until either another byte arrives or the stream is
/// finished. This makes the final link deterministic without a corrective I/O.
pub(crate) struct ImmutableStreamWriter<'a> {
	file: &'a DirectFile,
	page_type: PageType,
	first: u64,
	page_no: u64,
	page: Box<AlignedPage>,
	used: usize,
	logical_bytes: u64,
	written_pages: u64,
	started: bool,
}

impl<'a> ImmutableStreamWriter<'a> {
	/// Starts a stream at the current sequential end of `file`.
	pub(crate) fn new(file: &'a DirectFile, page_type: PageType) -> Self {
		let first = file.page_count();
		Self {
			file,
			page_type,
			first,
			page_no: first,
			page: Box::new(AlignedPage::zeroed()),
			used: 0,
			logical_bytes: 0,
			written_pages: 0,
			started: false,
		}
	}

	/// Returns the current logical byte offset.
	pub(crate) const fn position(&self) -> u64 {
		self.logical_bytes
	}

	/// Appends bytes without retaining earlier pages in memory.
	///
	/// # Errors
	///
	/// Returns an error on length overflow or failed page I/O.
	pub(crate) fn write_all(&mut self, mut input: &[u8]) -> FerrosGrynnResult<()> {
		while !input.is_empty() {
			if self.started && self.used == STREAM_PAYLOAD_BYTES {
				self.flush(false)?;
			}
			self.start_page();
			let count = input.len().min(STREAM_PAYLOAD_BYTES - self.used);
			let start = STREAM_HEADER_BYTES + self.used;
			self.page.as_bytes_mut()[start..start + count].copy_from_slice(&input[..count]);
			self.used += count;
			self.logical_bytes = self
				.logical_bytes
				.checked_add(count as u64)
				.ok_or_else(|| invalid("immutable stream length overflows u64"))?;
			input = &input[count..];
		}
		Ok(())
	}

	/// Rewrites bytes already appended to this stream.
	///
	/// This is used for bounded record headers whose final length is known only
	/// after their body has streamed. Previously flushed pages are read,
	/// validated, resealed and written back in place; the active heap page is
	/// patched directly.
	///
	/// # Errors
	///
	/// Returns an error when the requested range is outside the logical stream
	/// or when page validation/I/O fails.
	pub(crate) fn patch(&mut self, offset: u64, mut bytes: &[u8]) -> FerrosGrynnResult<()> {
		let end = offset
			.checked_add(bytes.len() as u64)
			.ok_or_else(|| invalid("immutable stream patch range overflows u64"))?;
		if end > self.logical_bytes {
			return Err(invalid("immutable stream patch lies beyond appended bytes"));
		}
		let mut cursor = offset;
		while !bytes.is_empty() {
			let page_ordinal = cursor / STREAM_PAYLOAD_BYTES as u64;
			let within = usize::try_from(cursor % STREAM_PAYLOAD_BYTES as u64)
				.map_err(|_error| invalid("immutable stream patch offset exceeds usize"))?;
			let count = bytes.len().min(STREAM_PAYLOAD_BYTES - within);
			if page_ordinal == self.written_pages {
				if !self.started || within + count > self.used {
					return Err(invalid("immutable stream patch crosses unwritten active bytes"));
				}
				let start = STREAM_HEADER_BYTES + within;
				self.page.as_bytes_mut()[start..start + count].copy_from_slice(&bytes[..count]);
			} else if page_ordinal < self.written_pages {
				let page_no = self
					.first
					.checked_add(page_ordinal)
					.ok_or_else(|| invalid("immutable stream patch page overflows u64"))?;
				let mut page = Box::new(AlignedPage::zeroed());
				let header = self.file.read_page(PageNo::new(page_no), &mut page)?;
				if header.page_type() != self.page_type || !header.flags().is_empty() {
					return Err(invalid("immutable stream patch found the wrong page type"));
				}
				let start = STREAM_HEADER_BYTES + within;
				page.as_bytes_mut()[start..start + count].copy_from_slice(&bytes[..count]);
				page.seal()?;
				self.file.write_page(PageNo::new(page_no), &page)?;
			} else {
				return Err(invalid("immutable stream patch skips an unwritten page"));
			}
			cursor = cursor
				.checked_add(count as u64)
				.ok_or_else(|| invalid("immutable stream patch cursor overflows u64"))?;
			bytes = &bytes[count..];
		}
		Ok(())
	}

	/// Seals the final page and returns the exact stream location.
	///
	/// # Errors
	///
	/// Returns an error when the final page cannot be sealed or written.
	pub(crate) fn finish(mut self) -> FerrosGrynnResult<ImmutableStreamLocation> {
		if self.started {
			self.flush(true)?;
		}
		Ok(ImmutableStreamLocation {
			first: if self.written_pages == 0 {
				NULL_STREAM_PAGE
			} else {
				self.first
			},
			pages: self.written_pages,
			bytes: self.logical_bytes,
		})
	}

	fn start_page(&mut self) {
		if self.started {
			return;
		}
		self.page.reset(
			self.page_type,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(self.page_no),
		);
		self.started = true;
	}

	fn flush(&mut self, final_page: bool) -> FerrosGrynnResult<()> {
		if !self.started || self.used == 0 {
			return Err(invalid("immutable stream attempted to flush an empty page"));
		}
		let next = if final_page {
			NULL_STREAM_PAGE
		} else {
			self.page_no
				.checked_add(1)
				.ok_or_else(|| invalid("immutable stream next page overflows u64"))?
		};
		let bytes = self.page.as_bytes_mut();
		bytes[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 8].copy_from_slice(&next.to_le_bytes());
		bytes[PAGE_HEADER_SIZE + 8..PAGE_HEADER_SIZE + 12].copy_from_slice(
			&u32::try_from(self.used)
				.map_err(|_error| invalid("immutable stream page payload exceeds u32"))?
				.to_le_bytes(),
		);
		self.page.seal()?;
		if self.file.page_count() != self.page_no {
			return Err(invalid("immutable stream writer lost sequential file position"));
		}
		self.file.write_page(PageNo::new(self.page_no), &self.page)?;
		self.page_no = self
			.page_no
			.checked_add(1)
			.ok_or_else(|| invalid("immutable stream page number overflows u64"))?;
		self.written_pages = self
			.written_pages
			.checked_add(1)
			.ok_or_else(|| invalid("immutable stream page count overflows u64"))?;
		self.page = Box::new(AlignedPage::zeroed());
		self.used = 0;
		self.started = false;
		Ok(())
	}
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
