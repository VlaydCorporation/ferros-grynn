//! Borrowed, checked sections shared by immutable graph index artifacts.

mod array_section;
pub use array_section::ArraySection;

/// Logical ownership captured by the catalog for an immutable graph artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphArtifactIdentity {
	/// Nonzero definition identity.
	pub index: u32,
	/// Nonzero definition version.
	pub version: u32,
	/// Nonzero graph occurrence namespace.
	pub graph: u64,
	/// Logical build publication timestamp, not a physical WAL position.
	pub revision: u64,
	/// Hash checked alongside complete canonical definition bytes.
	pub fingerprint: u64,
}

use super::page::{AlignedPage, PAGE_SIZE, PageFlags, PageLsn, PageNo, PageType};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// First payload byte of an array section page.
const ARRAY_START: usize = 64;
/// First offset-directory byte of a record section page.
const RECORDS_START: usize = 56;

/// Normative section identities; incompatible page families are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SectionKind {
	/// Local node slot to component ordinal.
	NodeComponentMap = 0x0001,
	/// Local edge slot to component ordinal.
	EdgeComponentMap = 0x0002,
	/// Component ordinal to representative atom.
	SccRepresentativeMap = 0x0003,
	/// Component adjacency offsets.
	CondensationCsrOffsets = 0x0004,
	/// Component adjacency targets.
	CondensationCsrTargets = 0x0005,
	/// Path vertex ordinal to atom reference.
	PathVertexMap = 0x0006,
	/// Deterministically selected landmark references.
	LandmarkRefs = 0x0007,
	/// Distances from each landmark.
	LandmarkDistanceFrom = 0x0008,
	/// Distances to each landmark.
	LandmarkDistanceTo = 0x0009,
	/// Complete canonical graph index definition bytes.
	DefinitionDescriptor = 0x000a,
	/// Canonical pattern descriptor bytes.
	PatternDescriptor = 0x0010,
	/// Canonical non-executable pattern description.
	PatternCanonicalIr = 0x0011,
	/// Binding names and types.
	PatternBindingSchema = 0x0012,
	/// Captured dependency versions.
	PatternDependencies = 0x0013,
	/// Exact binding rows, including multiplicity.
	PatternBindingRows = 0x0014,
	/// Exact tree ancestor intervals.
	TreeCoverIntervals = 0x0100,
	/// GRAIL dimension intervals.
	GrailIntervals = 0x0101,
	/// Bounded exact or approximate FERRARI intervals.
	FerrariIntervals = 0x0102,
	/// Incoming and outgoing BFL filters.
	BflBitsets = 0x0103,
	/// Chain identity and position.
	ChainCoordinates = 0x0104,
	/// Lowest reachable position in each chain.
	ChainReachabilityCells = 0x0105,
	/// Exact target ranks used by FERRARI covers.
	PostorderRanks = 0x0106,
	/// Selected neighborhood centers in vertex-ordinal order.
	NeighbourhoodCenters = 0x0200,
	/// Center-major radius-set offsets.
	NeighbourhoodOffsets = 0x0201,
	/// Exact distinct neighborhood members.
	NeighbourhoodMembers = 0x0202,
	/// Radius-major and vertex-major HLL register bytes.
	NeighbourhoodRegisters = 0x0203,
	/// Exact distinct landmark neighborhood members.
	NeighbourhoodLandmarkMembers = 0x0204,
	/// Canonical vertex ordinal to graph-local atom reference.
	NeighbourhoodVertexMap = 0x0205,
	/// Qualified axes, revision vector, original topology and containment
	/// bytes.
	TensorMetadata = 0x0300,
	/// Source row offsets.
	TensorSourceOffsets = 0x0301,
	/// Sorted distinct source connector ordinals.
	TensorSourceOrdinals = 0x0302,
	/// Target row offsets.
	TensorTargetOffsets = 0x0303,
	/// Sorted distinct target connector ordinals.
	TensorTargetOrdinals = 0x0304,
	/// Row-major, LSB-first source membership.
	TensorSourceBits = 0x0305,
	/// Row-major, LSB-first target membership.
	TensorTargetBits = 0x0306,
}

impl SectionKind {
	/// Checks a requested array width and returns its concrete page family.
	fn array_type(self, width: u16) -> FerrosGrynnResult<PageType> {
		let (page_type, valid) = match self {
			Self::NodeComponentMap
			| Self::EdgeComponentMap
			| Self::SccRepresentativeMap
			| Self::CondensationCsrOffsets
			| Self::CondensationCsrTargets
			| Self::PathVertexMap
			| Self::LandmarkRefs
			| Self::PostorderRanks
			| Self::NeighbourhoodCenters
			| Self::NeighbourhoodOffsets
			| Self::NeighbourhoodVertexMap
			| Self::TensorSourceOffsets
			| Self::TensorSourceOrdinals
			| Self::TensorTargetOffsets
			| Self::TensorTargetOrdinals => (PageType::IndexDerivedArray, width == 8),
			Self::DefinitionDescriptor
			| Self::TensorMetadata
			| Self::TensorSourceBits
			| Self::TensorTargetBits => (PageType::IndexDerivedArray, width == 1),
			Self::LandmarkDistanceFrom | Self::LandmarkDistanceTo => {
				(PageType::IndexDerivedArray, matches!(width, 8 | 16))
			}
			Self::TreeCoverIntervals => (PageType::IndexReachabilityTreeCover, width == 16),
			Self::GrailIntervals => (PageType::IndexReachabilityGrail, width == 16),
			Self::FerrariIntervals => (PageType::IndexReachabilityFerrari, width == 24),
			Self::BflBitsets => (
				PageType::IndexReachabilityBfl,
				(16..=1024).contains(&width) && width.is_multiple_of(2),
			),
			Self::ChainCoordinates => (PageType::IndexReachabilityChainCover, width == 16),
			Self::ChainReachabilityCells => (PageType::IndexReachabilityChainCover, width == 8),
			Self::NeighbourhoodMembers => (PageType::IndexNeighbourhoodExact, width == 8),
			Self::NeighbourhoodRegisters => (PageType::IndexNeighbourhoodSketch, width == 1),
			Self::NeighbourhoodLandmarkMembers => {
				(PageType::IndexNeighbourhoodLandmarkList, width == 8)
			}
			_ => return Err(invalid("record section cannot use an array page")),
		};
		if !valid {
			return Err(invalid("derived array element width is incompatible with its section"));
		}
		Ok(page_type)
	}

	/// Rejects array sections at the variable-record boundary.
	fn check_records(self) -> FerrosGrynnResult<()> {
		if !(0x0010..=0x0014).contains(&(self as u16)) {
			return Err(invalid("array section cannot use a records page"));
		}
		Ok(())
	}
}

/// A validated array borrows its payload without allocating from disk lengths.
pub(super) struct ArrayPage<'a> {
	/// Forward page link, or NULL for the final page.
	pub(super) next: PageNo,
	/// Ordinal of the first element in this page.
	pub(super) first_ordinal: u64,
	/// Exact-width element bytes.
	pub(super) elements: &'a [u8],
}

/// A validated records page retains only its bounded directory and payload.
pub(super) struct RecordsPage<'a> {
	/// Forward page link, or NULL for the final page.
	pub(super) next: PageNo,
	/// Ordinal of the first record in this page.
	pub(super) first_ordinal: u64,
	/// Validated offsets including the exclusive end of the final record.
	offsets: &'a [u8],
	/// Exact bytes named by the offset directory.
	payload: &'a [u8],
}

impl RecordsPage<'_> {
	/// Iterates validated records in ordinal order, including empty records.
	pub(super) fn records(&self) -> impl Iterator<Item = &[u8]> {
		self.offsets.windows(4).step_by(2).map(|pair| {
			let start = usize::from(u16::from_le_bytes([pair[0], pair[1]]));
			let end = usize::from(u16::from_le_bytes([pair[2], pair[3]]));
			&self.payload[start..end]
		})
	}
}

/// Encodes one nonempty array page into its final aligned destination.
///
/// # Errors
/// Returns an error for incompatible widths, excessive payload, ordinal
/// overflow or a backward link. Empty sections use a NULL root instead.
pub(super) fn encode_array(
	page: &mut AlignedPage,
	at: PageNo,
	next: PageNo,
	kind: SectionKind,
	width: u16,
	first_ordinal: u64,
	elements: &[u8],
) -> FerrosGrynnResult<()> {
	let page_type = kind.array_type(width)?;
	let width = usize::from(width);
	if elements.is_empty()
		|| elements.len() > PAGE_SIZE - ARRAY_START
		|| !elements.len().is_multiple_of(width)
	{
		return Err(invalid("derived array payload exceeds its exact page layout"));
	}
	let count = elements.len() / width;
	check_ordinal(first_ordinal, count as u64)?;
	check_link(at, next, u64::MAX)?;
	page.reset(page_type, PageFlags::empty(), PageLsn::ZERO, at);
	let bytes = page.as_bytes_mut();
	bytes[32..40].copy_from_slice(&next.get().to_le_bytes());
	bytes[40..42].copy_from_slice(&(kind as u16).to_le_bytes());
	bytes[42..44].copy_from_slice(&(width as u16).to_le_bytes());
	bytes[44..48].copy_from_slice(&(count as u32).to_le_bytes());
	bytes[48..56].copy_from_slice(&first_ordinal.to_le_bytes());
	bytes[56..60].copy_from_slice(&(elements.len() as u32).to_le_bytes());
	bytes[ARRAY_START..ARRAY_START + elements.len()].copy_from_slice(elements);
	page.seal()
}

/// Validates ownership, lengths, links, ordinals and all reserved bytes.
///
/// # Errors
/// Returns corruption for a mismatched family, width, ordinal, extent or tail.
pub(super) fn decode_array(
	page: &AlignedPage,
	at: PageNo,
	page_count: u64,
	kind: SectionKind,
	width: u16,
	first_ordinal: u64,
) -> FerrosGrynnResult<ArrayPage<'_>> {
	let page_type = kind.array_type(width)?;
	check_header(page, at, page_count, page_type, kind)?;
	let bytes = page.as_bytes();
	let count = word(bytes, 44) as usize;
	let used = word(bytes, 56) as usize;
	let expected = count
		.checked_mul(usize::from(width))
		.ok_or_else(|| invalid("derived array length overflows"))?;
	if count == 0
		|| short(bytes, 42) != width
		|| used != expected
		|| used > PAGE_SIZE - ARRAY_START
		|| number(bytes, 48) != first_ordinal
		|| word(bytes, 60) != 0
		|| bytes[ARRAY_START + used..].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("derived array metadata or reserved tail is invalid"));
	}
	check_ordinal(first_ordinal, count as u64)?;
	let next = PageNo::new(number(bytes, 32));
	check_link(at, next, page_count)?;
	Ok(ArrayPage {
		next,
		first_ordinal,
		elements: &bytes[ARRAY_START..ARRAY_START + used],
	})
}

/// Encodes bounded records with canonical relative offsets.
///
/// # Errors
/// Returns an error before writing for excessive sizes or invalid ordinals.
pub(super) fn encode_records(
	page: &mut AlignedPage,
	at: PageNo,
	next: PageNo,
	kind: SectionKind,
	first_ordinal: u64,
	records: &[&[u8]],
) -> FerrosGrynnResult<()> {
	kind.check_records()?;
	let directory = records
		.len()
		.checked_add(1)
		.and_then(|count| count.checked_mul(2))
		.ok_or_else(|| invalid("derived records directory overflows"))?;
	let used = records
		.iter()
		.try_fold(directory, |used, record| used.checked_add(record.len()))
		.ok_or_else(|| invalid("derived records payload overflows"))?;
	if records.is_empty() || used > PAGE_SIZE - RECORDS_START {
		return Err(invalid("derived records exceed one page"));
	}
	check_ordinal(first_ordinal, records.len() as u64)?;
	check_link(at, next, u64::MAX)?;
	page.reset(PageType::IndexDerivedRecords, PageFlags::empty(), PageLsn::ZERO, at);
	let bytes = page.as_bytes_mut();
	bytes[32..40].copy_from_slice(&next.get().to_le_bytes());
	bytes[40..42].copy_from_slice(&(kind as u16).to_le_bytes());
	bytes[42..44].copy_from_slice(&(records.len() as u16).to_le_bytes());
	bytes[44..46].copy_from_slice(&(used as u16).to_le_bytes());
	bytes[48..56].copy_from_slice(&first_ordinal.to_le_bytes());
	let mut offset = 0;
	for (index, record) in records.iter().enumerate() {
		let start = RECORDS_START + directory + offset;
		bytes[start..start + record.len()].copy_from_slice(record);
		offset += record.len();
		let position = RECORDS_START + (index + 1) * 2;
		bytes[position..position + 2].copy_from_slice(&(offset as u16).to_le_bytes());
	}
	page.seal()
}

/// Borrows records only after validating the complete offset directory.
///
/// # Errors
/// Returns corruption for invalid counts, offsets, links, ordinals or padding.
pub(super) fn decode_records(
	page: &AlignedPage,
	at: PageNo,
	page_count: u64,
	kind: SectionKind,
	first_ordinal: u64,
) -> FerrosGrynnResult<RecordsPage<'_>> {
	kind.check_records()?;
	check_header(page, at, page_count, PageType::IndexDerivedRecords, kind)?;
	let bytes = page.as_bytes();
	let count = usize::from(short(bytes, 42));
	let used = usize::from(short(bytes, 44));
	let directory =
		(count + 1).checked_mul(2).ok_or_else(|| invalid("derived records directory overflows"))?;
	if count == 0
		|| used < directory
		|| used > PAGE_SIZE - RECORDS_START
		|| short(bytes, 46) != 0
		|| number(bytes, 48) != first_ordinal
		|| bytes[RECORDS_START + used..].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("derived record metadata or reserved tail is invalid"));
	}
	let payload_bytes = used - directory;
	let offsets = &bytes[RECORDS_START..RECORDS_START + directory];
	let mut previous = 0;
	for (index, offset) in offsets.as_chunks::<2>().0.iter().enumerate() {
		let value = usize::from(u16::from_le_bytes([offset[0], offset[1]]));
		if value < previous
			|| value > payload_bytes
			|| (index == 0 && value != 0)
			|| (index == count && value != payload_bytes)
		{
			return Err(invalid("derived record offsets are noncanonical or out of bounds"));
		}
		previous = value;
	}
	check_ordinal(first_ordinal, count as u64)?;
	let next = PageNo::new(number(bytes, 32));
	check_link(at, next, page_count)?;
	Ok(RecordsPage {
		next,
		first_ordinal,
		offsets,
		payload: &bytes[RECORDS_START + directory..RECORDS_START + used],
	})
}

/// Validates a required ordinal reference, with optional NULL sentinel.
///
/// # Errors
/// Returns corruption if the referenced object is outside its captured extent.
pub(super) fn check_reference(value: u64, limit: u64, nullable: bool) -> FerrosGrynnResult<()> {
	if value < limit || (nullable && value == u64::MAX) {
		return Ok(());
	}
	Err(invalid("derived reference exceeds its captured extent"))
}

/// Streams an exact array chain through one reusable aligned page.
///
/// # Errors
/// Returns corruption or the callback error without publishing a partial
/// artifact.
pub(super) fn visit_array_section(
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	page_count: u64,
	kind: SectionKind,
	width: u16,
	total: u64,
	mut visit: impl FnMut(u64, &[u8]) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	kind.array_type(width)?;
	if (root == PageNo::NULL) != (total == 0) {
		return Err(invalid("derived array root and length disagree"));
	}
	let mut page = Box::new(AlignedPage::zeroed());
	let mut next = root;
	let mut ordinal = 0;
	while next != PageNo::NULL {
		check_page_no(next, page_count)?;
		read(next, &mut page)?;
		let decoded = decode_array(&page, next, page_count, kind, width, ordinal)?;
		let count = (decoded.elements.len() / usize::from(width)) as u64;
		let end = check_ordinal(ordinal, count)?;
		if end > total {
			return Err(invalid("derived array exceeds declared total"));
		}
		for element in decoded.elements.chunks_exact(usize::from(width)) {
			visit(ordinal, element)?;
			ordinal += 1;
		}
		next = decoded.next;
	}
	if ordinal != total {
		return Err(invalid("derived array chain is truncated"));
	}
	Ok(())
}

/// Streams an exact records chain through one reusable aligned page.
///
/// # Errors
/// Returns corruption or the callback error without publishing a partial
/// artifact.
pub(super) fn visit_record_section(
	mut read: impl FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
	root: PageNo,
	page_count: u64,
	kind: SectionKind,
	total: u64,
	mut visit: impl FnMut(u64, &[u8]) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	kind.check_records()?;
	if (root == PageNo::NULL) != (total == 0) {
		return Err(invalid("derived records root and length disagree"));
	}
	let mut page = Box::new(AlignedPage::zeroed());
	let mut next = root;
	let mut ordinal = 0;
	while next != PageNo::NULL {
		check_page_no(next, page_count)?;
		read(next, &mut page)?;
		let decoded = decode_records(&page, next, page_count, kind, ordinal)?;
		let count = (decoded.offsets.len() / 2 - 1) as u64;
		if check_ordinal(ordinal, count)? > total {
			return Err(invalid("derived records exceed declared total"));
		}
		for record in decoded.records() {
			visit(ordinal, record)?;
			ordinal += 1;
		}
		next = decoded.next;
	}
	if ordinal != total {
		return Err(invalid("derived records chain is truncated"));
	}
	Ok(())
}

/// Checks physical identity before interpreting section-specific payload.
fn check_header(
	page: &AlignedPage,
	at: PageNo,
	page_count: u64,
	page_type: PageType,
	kind: SectionKind,
) -> FerrosGrynnResult<()> {
	check_page_no(at, page_count)?;
	let header = page.validate(at)?;
	if header.page_type() != page_type
		|| !header.flags().is_empty()
		|| short(page.as_bytes(), 40) != kind as u16
	{
		return Err(invalid("derived section page family, flags or kind is incompatible"));
	}
	Ok(())
}

/// Page zero belongs to the owning artifact header, never to a section.
fn check_page_no(at: PageNo, page_count: u64) -> FerrosGrynnResult<()> {
	if at.get() == 0 || at.get() >= page_count {
		return Err(invalid("derived section page lies outside the artifact"));
	}
	Ok(())
}

/// Forward links forbid cycles without allocating a visited-page set.
fn check_link(at: PageNo, next: PageNo, page_count: u64) -> FerrosGrynnResult<()> {
	check_page_no(at, page_count)?;
	if next != PageNo::NULL && (next.get() <= at.get() || next.get() >= page_count) {
		return Err(invalid("derived section link is backward or outside the artifact"));
	}
	Ok(())
}

/// An exclusive ordinal end must remain representable.
fn check_ordinal(first: u64, count: u64) -> FerrosGrynnResult<u64> {
	first.checked_add(count).ok_or_else(|| invalid("derived section ordinal overflows"))
}

/// Reads a fixed header field from a complete page.
fn short(bytes: &[u8], at: usize) -> u16 {
	u16::from_le_bytes([bytes[at], bytes[at + 1]])
}
/// Reads a fixed header field from a complete page.
fn word(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
/// Reads a fixed header field from a complete page.
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
/// Typed corruption at the artifact storage boundary.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests;
