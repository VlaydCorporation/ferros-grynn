//! Checked tensor cache header and sparse/dense membership sections.

use super::{
	derived_index::{ArraySection, GraphArtifactIdentity, SectionKind},
	page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Concrete encoding; AUTO is a build policy and never a physical tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TensorCacheTag {
	/// Sorted row offsets and connector ordinals.
	Csr = 1,
	/// Row-major LSB-first bit planes.
	Dense = 2,
}

/// Complete page-zero metadata; availability belongs to the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TensorCacheHeader {
	/// Exact owning definition and captured publication.
	pub identity: GraphArtifactIdentity,
	/// Full ATOM axis length, including referenced boundary endpoints.
	pub rows: u64,
	/// Connector axis length.
	pub connectors: u64,
	/// Distinct logical source memberships.
	pub source_cells: u64,
	/// Distinct logical target memberships.
	pub target_cells: u64,
	/// Qualified dictionaries, dependency vector, topology and containment
	/// bytes.
	pub metadata_bytes: u64,
	/// Complete canonical semantic definition bytes.
	pub descriptor_bytes: u64,
	/// Hard cap including every full page.
	pub max_bytes: u64,
	/// Selected concrete membership representation.
	pub format: TensorCacheTag,
	/// Whether the induced domain includes nested child graphs.
	pub rooted: bool,
}

/// Canonical consecutive section packing with exact full-page admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorCacheLayout {
	/// Checked ownership and dimensions.
	pub header: TensorCacheHeader,
	/// Metadata, descriptor, source/target CSR and source/target dense planes.
	pub sections: [ArraySection; 8],
	/// Exact file extent including page zero.
	pub page_count: u64,
}

impl TensorCacheLayout {
	/// Projects the complete file before any allocation or write.
	///
	/// # Errors
	/// Rejects ownership, dimension overflow, incompatible counts and byte
	/// caps.
	pub fn new(header: TensorCacheHeader) -> FerrosGrynnResult<Self> {
		let cells = header
			.rows
			.checked_mul(header.connectors)
			.ok_or_else(|| invalid("tensor shape overflows"))?;
		if header.identity.index == 0
			|| header.identity.version == 0
			|| header.identity.graph == 0
			|| header.identity.revision == 0
			|| header.source_cells > cells
			|| header.target_cells > cells
			|| header.metadata_bytes == 0
			|| header.descriptor_bytes == 0
			|| header.descriptor_bytes > 8 << 20
			|| header.max_bytes == 0
		{
			return Err(invalid("tensor ownership or extent is invalid"));
		}
		let sparse = header.format == TensorCacheTag::Csr;
		let offsets = if sparse {
			header.rows.checked_add(1).ok_or_else(|| invalid("tensor offsets overflow"))?
		} else {
			0
		};
		let bits = if sparse {
			0
		} else {
			cells.div_ceil(8)
		};
		let mut sections = [
			(SectionKind::TensorMetadata, 1, header.metadata_bytes),
			(SectionKind::DefinitionDescriptor, 1, header.descriptor_bytes),
			(SectionKind::TensorSourceOffsets, 8, offsets),
			(
				SectionKind::TensorSourceOrdinals,
				8,
				if sparse {
					header.source_cells
				} else {
					0
				},
			),
			(SectionKind::TensorTargetOffsets, 8, offsets),
			(
				SectionKind::TensorTargetOrdinals,
				8,
				if sparse {
					header.target_cells
				} else {
					0
				},
			),
			(SectionKind::TensorSourceBits, 1, bits),
			(SectionKind::TensorTargetBits, 1, bits),
		]
		.map(|(kind, width, count)| ArraySection {
			kind,
			width,
			count,
			root: PageNo::NULL,
		});
		let page_count = ArraySection::pack(&mut sections, header.max_bytes)?;
		Ok(Self {
			header,
			sections,
			page_count,
		})
	}

	/// Streams page zero and the complete sections through one aligned buffer.
	///
	/// # Errors
	/// Rejects a forged layout or propagates cancellation, encoding and I/O
	/// errors.
	pub fn stream<Element, Write, Check>(
		&self,
		mut element: Element,
		mut write: Write,
		check: &mut Check,
	) -> FerrosGrynnResult<()>
	where
		Element: FnMut(SectionKind, u64, &mut [u8]) -> FerrosGrynnResult<()>,
		Write: FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		if Self::new(self.header)? != *self {
			return Err(invalid("tensor layout is noncanonical"));
		}
		check()?;
		let mut page = Box::new(AlignedPage::zeroed());
		page.reset(PageType::TensorCache, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
		let bytes = page.as_bytes_mut();
		let header = self.header;
		bytes[32..36].copy_from_slice(&header.identity.index.to_le_bytes());
		bytes[36..40].copy_from_slice(&header.identity.version.to_le_bytes());
		for (at, value) in [
			(40, header.identity.graph),
			(48, header.identity.revision),
			(56, header.identity.fingerprint),
			(64, header.rows),
			(72, header.connectors),
			(80, header.source_cells),
			(88, header.target_cells),
			(96, header.metadata_bytes),
			(104, header.descriptor_bytes),
			(112, header.max_bytes),
			(120, self.page_count),
		] {
			bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
		}
		bytes[128] = header.format as u8;
		bytes[129] = u8::from(header.rooted);
		for (index, section) in self.sections.iter().enumerate() {
			let at = 136 + index * 8;
			bytes[at..at + 8].copy_from_slice(&section.root.get().to_le_bytes());
		}
		page.seal()?;
		write(PageNo::new(0), &page)?;
		for section in &self.sections {
			section.stream(&mut page, &mut element, &mut write, check)?;
		}
		check()
	}

	/// Checks the complete header before allocating from its dimensions.
	///
	/// # Errors
	/// Rejects checksum, identity, tags, reserved bytes, roots or extent
	/// mismatch.
	pub fn decode_header(
		page: &AlignedPage,
		identity: GraphArtifactIdentity,
		actual_pages: u64,
	) -> FerrosGrynnResult<Self> {
		let common = page.validate(PageNo::new(0))?;
		let bytes = page.as_bytes();
		if common.page_type() != PageType::TensorCache
			|| !common.flags().is_empty()
			|| word(bytes, 32) != identity.index
			|| word(bytes, 36) != identity.version
			|| number(bytes, 40) != identity.graph
			|| number(bytes, 48) != identity.revision
			|| number(bytes, 56) != identity.fingerprint
			|| bytes[129] > 1
			|| bytes[130..136].iter().chain(&bytes[200..]).any(|byte| *byte != 0)
		{
			return Err(invalid("tensor header identity or reserved bytes are invalid"));
		}
		let format = match bytes[128] {
			1 => TensorCacheTag::Csr,
			2 => TensorCacheTag::Dense,
			_ => return Err(invalid("tensor physical format is invalid")),
		};
		let layout = Self::new(TensorCacheHeader {
			identity,
			rows: number(bytes, 64),
			connectors: number(bytes, 72),
			source_cells: number(bytes, 80),
			target_cells: number(bytes, 88),
			metadata_bytes: number(bytes, 96),
			descriptor_bytes: number(bytes, 104),
			max_bytes: number(bytes, 112),
			format,
			rooted: bytes[129] == 1,
		})?;
		if layout.page_count != actual_pages
			|| layout.page_count != number(bytes, 120)
			|| layout
				.sections
				.iter()
				.enumerate()
				.any(|(index, section)| section.root.get() != number(bytes, 136 + index * 8))
		{
			return Err(invalid("tensor roots or file extent are noncanonical"));
		}
		Ok(layout)
	}

	/// Loads exact sections only after aggregate decoded workspace admission.
	///
	/// # Errors
	/// Rejects forged layout, byte caps, malformed pages or cancellation.
	pub fn load_sections<Read, Check>(
		&self,
		scratch_bytes: usize,
		read: Read,
		check: &mut Check,
	) -> FerrosGrynnResult<[Vec<u8>; 8]>
	where
		Read: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		if Self::new(self.header)? != *self {
			return Err(invalid("tensor layout is noncanonical"));
		}
		ArraySection::admit_decode(
			&self.sections,
			self.header.rows,
			self.header.connectors,
			scratch_bytes,
		)?;
		let data = ArraySection::load_sections(
			&self.sections,
			self.page_count,
			scratch_bytes,
			read,
			check,
		)?;
		self.validate_membership(&data)?;
		Ok(data)
	}

	/// Checks CSR ordinals or dense population and padding without allocation.
	///
	/// # Errors
	/// Rejects partial sections, nonmonotonic offsets, duplicates or
	/// out-of-range cells.
	pub fn validate_membership(&self, data: &[Vec<u8>; 8]) -> FerrosGrynnResult<()> {
		if Self::new(self.header)? != *self {
			return Err(invalid("tensor layout is noncanonical"));
		}
		for (section, bytes) in self.sections.iter().zip(data) {
			if section.count.checked_mul(u64::from(section.width)) != Some(bytes.len() as u64) {
				return Err(invalid("tensor section length differs from its extent"));
			}
		}
		for (offsets, ordinals, bits, count) in [
			(&data[2], &data[3], &data[6], self.header.source_cells),
			(&data[4], &data[5], &data[7], self.header.target_cells),
		] {
			if self.header.format == TensorCacheTag::Dense {
				let cells = self
					.header
					.rows
					.checked_mul(self.header.connectors)
					.ok_or_else(|| invalid("tensor shape overflows"))?;
				let population = bits
					.iter()
					.try_fold(0_u64, |sum, byte| sum.checked_add(u64::from(byte.count_ones())))
					.ok_or_else(|| invalid("tensor dense population overflows"))?;
				if cells % 8 != 0 && bits.last().is_some_and(|byte| *byte >> (cells % 8) != 0)
					|| population != count
				{
					return Err(invalid("tensor dense population or padding is invalid"));
				}
			} else {
				let mut previous = 0_u64;
				for (row, bytes) in offsets.as_chunks::<8>().0.iter().enumerate() {
					let end = u64::from_le_bytes(*bytes);
					if end < previous || end > count || row == 0 && end != 0 {
						return Err(invalid("tensor CSR offsets are noncanonical"));
					}
					let mut column = None;
					for ordinal in previous..end {
						let at = usize::try_from(ordinal)
							.ok()
							.and_then(|at| at.checked_mul(8))
							.ok_or_else(|| invalid("tensor ordinal exceeds platform"))?;
						let value = number(ordinals, at);
						if value >= self.header.connectors
							|| column.is_some_and(|last| last >= value)
						{
							return Err(invalid(
								"tensor CSR columns are not sorted distinct ordinals",
							));
						}
						column = Some(value);
					}
					previous = end;
				}
				if previous != count {
					return Err(invalid("tensor CSR ends before its declared cells"));
				}
			}
		}
		Ok(())
	}
}

/// Reads a fixed field after validating the complete page.
fn word(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
/// Reads a fixed field after validating the complete page or section length.
fn number(bytes: &[u8], at: usize) -> u64 {
	u64::from_le_bytes([
		bytes[at],
		bytes[at + 1],
		bytes[at + 2],
		bytes[at + 3],
		bytes[at + 4],
		bytes[at + 5],
		bytes[at + 6],
		bytes[at + 7],
	])
}
/// Typed corruption at the physical tensor boundary.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Regression tests for tensor index behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};

	/// Tiny canonical section fixture shared by both physical representations.
	fn fixture(format: TensorCacheTag) -> (TensorCacheLayout, [Vec<u8>; 8]) {
		let header = TensorCacheHeader {
			identity: GraphArtifactIdentity {
				index: 7,
				version: 2,
				graph: 3,
				revision: 9,
				fingerprint: 11,
			},
			rows: 3,
			connectors: 2,
			source_cells: 2,
			target_cells: 2,
			metadata_bytes: 2,
			descriptor_bytes: 3,
			max_bytes: 1 << 20,
			format,
			rooted: true,
		};
		let layout = TensorCacheLayout::new(header).assert_ok();
		let mut data: [Vec<u8>; 8] = std::array::from_fn(|_| Vec::new());
		data[0] = vec![1, 2];
		data[1] = vec![3, 4, 5];
		if format == TensorCacheTag::Csr {
			for (section, words) in
				[(2, vec![0_u64, 1, 2, 2]), (3, vec![1, 0]), (4, vec![0, 1, 1, 2]), (5, vec![0, 1])]
			{
				data[section] = words.into_iter().flat_map(u64::to_le_bytes).collect();
			}
		} else {
			data[6] = vec![6];
			data[7] = vec![33];
		}
		(layout, data)
	}

	/// Verifies tensor sections round trip and reject forged extents ordinals
	/// and padding.
	#[test]
	fn tensor_sections_round_trip_and_reject_forged_extents_ordinals_and_padding() {
		for format in [TensorCacheTag::Csr, TensorCacheTag::Dense] {
			let (layout, data) = fixture(format);
			let mut pages: Vec<Box<AlignedPage>> = Vec::new();
			layout
				.stream(
					|kind, ordinal, destination| {
						let section = layout
							.sections
							.iter()
							.position(|section| section.kind == kind)
							.assert_some();
						let at = ordinal as usize * destination.len();
						destination.copy_from_slice(&data[section][at..at + destination.len()]);
						Ok(())
					},
					|at, page| {
						assert_eq!(at.get() as usize, pages.len());
						let mut copy = Box::new(AlignedPage::zeroed());
						copy.as_bytes_mut().copy_from_slice(page.as_bytes());
						pages.push(copy);
						Ok(())
					},
					&mut || Ok(()),
				)
				.assert_ok();
			assert_eq!(&pages[0].as_bytes()[32..40], &[7, 0, 0, 0, 2, 0, 0, 0]);
			let restored = TensorCacheLayout::decode_header(
				&pages[0],
				layout.header.identity,
				pages.len() as u64,
			)
			.assert_ok();
			assert_eq!(restored, layout);
			let loaded = restored
				.load_sections(
					1 << 20,
					|at, destination| {
						destination
							.as_bytes_mut()
							.copy_from_slice(pages[at.get() as usize].as_bytes());
						Ok(())
					},
					&mut || Ok(()),
				)
				.assert_ok();
			assert_eq!(loaded, data);
			let read_calls = std::cell::Cell::new(0);
			assert!(
				restored
					.load_sections(
						100,
						|_, _| {
							read_calls.set(read_calls.get() + 1);
							Ok(())
						},
						&mut || Ok(())
					)
					.is_err()
			);
			assert_eq!(read_calls.get(), 0, "admission precedes page reads");
			for at in [128, 129, 130, 136, 200] {
				let old = pages[0].as_bytes()[at];
				pages[0].as_bytes_mut()[at] = 255;
				pages[0].seal().assert_ok();
				assert!(
					TensorCacheLayout::decode_header(
						&pages[0],
						layout.header.identity,
						pages.len() as u64
					)
					.is_err()
				);
				pages[0].as_bytes_mut()[at] = old;
				pages[0].seal().assert_ok();
			}
			let mut forged = data.clone();
			if format == TensorCacheTag::Csr {
				forged[3][..8].copy_from_slice(&2_u64.to_le_bytes());
			} else {
				forged[6][0] |= 128;
			}
			assert!(layout.validate_membership(&forged).is_err());
			let mut forged_layout = layout.clone();
			forged_layout.sections[3].count = 1;
			assert!(forged_layout.validate_membership(&data).is_err());
			let mut header = layout.header;
			header.rows = u64::MAX;
			assert!(TensorCacheLayout::new(header).is_err());
			header = layout.header;
			header.max_bytes = 16_383;
			assert!(TensorCacheLayout::new(header).is_err());
		}
	}
}
