//! Checked page layout and streaming codec for immutable reachability
//! artifacts.

use super::{
	derived_index::{ArraySection, GraphArtifactIdentity, SectionKind},
	page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Header root fields in the canonical section packing order.
const ROOT_OFFSETS: [usize; 8] = [88, 96, 104, 112, 120, 128, 136, 168];

/// Closed physical algorithm tags; ON_DEMAND has no side-file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReachabilityTag {
	/// Exact ancestor intervals for a forest projection.
	TreeCover = 0,
	/// Sound negative min-post filters.
	Grail = 1,
	/// Bounded exact and approximate postorder covers.
	Ferrari = 2,
	/// Incoming and outgoing bitset filters.
	Bfl = 3,
	/// Exact chain coordinates and minimum matrix.
	ChainCover = 4,
}

/// Complete catalog-independent header; roots and extent are derived exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReachabilityHeader {
	/// Published logical identity.
	pub identity: GraphArtifactIdentity,
	/// Sparse local node slot extent, including holes.
	pub node_capacity: u64,
	/// Sparse local edge slot extent, including holes.
	pub edge_capacity: u64,
	/// Canonical SCC count.
	pub components: u64,
	/// Distinct condensation arcs.
	pub edges: u64,
	/// Selected concrete physical algorithm.
	pub algorithm: ReachabilityTag,
	/// GRAIL dimensions, FERRARI ranges or BFL bits; otherwise zero.
	pub parameter: u32,
	/// Number of chains for CHAIN_COVER; otherwise zero.
	pub chains: u64,
	/// 0=OUT, 1=IN, 2=BOTH.
	pub direction: u8,
	/// 0=NODE, 1=ATOM.
	pub mode: u8,
	/// TREE_COVER was built on the reverse condensation.
	pub reversed: bool,
	/// Qualified incidence was excluded from this local projection.
	pub has_cross_level: bool,
	/// Hard cap including header, sections and padding.
	pub max_bytes: u64,
	/// Complete canonical definition byte length.
	pub descriptor_bytes: u64,
}

/// Full-page projection shared by writer, decoder and AUTO budget selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachabilityLayout {
	/// Validated ownership and parameters.
	pub header: ReachabilityHeader,
	/// Canonical root order, including the complete definition bytes.
	pub sections: [ArraySection; 8],
	/// Exact file extent, including page zero.
	pub page_count: u64,
}

impl ReachabilityLayout {
	/// Projects all sections before allocation or any output page write.
	///
	/// # Errors
	/// Rejects invalid parameters, overflow, inconsistent extents or MAX_BYTES.
	pub fn new(header: ReachabilityHeader) -> FerrosGrynnResult<Self> {
		let count = header.components;
		let vertices = header
			.node_capacity
			.checked_add(if header.mode == 1 {
				header.edge_capacity
			} else {
				0
			})
			.ok_or_else(|| invalid("reachability vertex extent overflows"))?;
		if header.identity.index == 0
			|| header.identity.version == 0
			|| header.identity.graph == 0
			|| header.identity.revision == 0
			|| header.direction > 2
			|| header.mode > 1
			|| count > vertices
			|| header.node_capacity > 1_u64 << 40
			|| header.edge_capacity > 1_u64 << 40
			|| header.descriptor_bytes == 0
			|| header.descriptor_bytes > 8 * 1024 * 1024
			|| header.max_bytes == 0
			|| (header.reversed && header.algorithm != ReachabilityTag::TreeCover)
			|| (header.algorithm != ReachabilityTag::ChainCover && header.chains != 0)
			|| (count == 0 && header.edges != 0)
		{
			return Err(invalid("reachability header identity, mode or extents are invalid"));
		}
		let multiply = |a: u64, b: u64| {
			a.checked_mul(b).ok_or_else(|| invalid("reachability section length overflows"))
		};
		let (kind, width, entries, aux_kind, aux_count) = match header.algorithm {
			ReachabilityTag::TreeCover if header.parameter == 0 => {
				(SectionKind::TreeCoverIntervals, 16, count, SectionKind::PostorderRanks, 0)
			}
			ReachabilityTag::Grail if (1..=32).contains(&header.parameter) => (
				SectionKind::GrailIntervals,
				16,
				multiply(count, u64::from(header.parameter))?,
				SectionKind::PostorderRanks,
				0,
			),
			ReachabilityTag::Ferrari if (1..=255).contains(&header.parameter) => (
				SectionKind::FerrariIntervals,
				24,
				multiply(count, u64::from(header.parameter))?,
				SectionKind::PostorderRanks,
				count,
			),
			ReachabilityTag::Bfl
				if (64..=4096).contains(&header.parameter)
					&& header.parameter.is_multiple_of(8) =>
			{
				(
					SectionKind::BflBitsets,
					(header.parameter / 4) as u16,
					count,
					SectionKind::PostorderRanks,
					0,
				)
			}
			ReachabilityTag::ChainCover
				if header.parameter == 0
					&& header.chains <= count
					&& (header.chains == 0) == (count == 0) =>
			{
				(
					SectionKind::ChainCoordinates,
					16,
					count,
					SectionKind::ChainReachabilityCells,
					multiply(count, header.chains)?,
				)
			}
			_ => return Err(invalid("reachability algorithm parameters are invalid")),
		};
		let mut sections = [
			(SectionKind::NodeComponentMap, 8, header.node_capacity),
			(SectionKind::EdgeComponentMap, 8, header.edge_capacity),
			(SectionKind::SccRepresentativeMap, 8, count),
			(
				SectionKind::CondensationCsrOffsets,
				8,
				count.checked_add(1).ok_or_else(|| invalid("reachability CSR extent overflows"))?,
			),
			(SectionKind::CondensationCsrTargets, 8, header.edges),
			(kind, width, entries),
			(aux_kind, 8, aux_count),
			(SectionKind::DefinitionDescriptor, 1, header.descriptor_bytes),
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

	/// Streams into final aligned destinations through one reusable page.
	///
	/// # Errors
	/// Propagates cancellation, packing, element or output errors. The caller
	/// keeps this generation unreachable until complete validation/publication.
	pub fn stream<
		ElementArg: FnMut(SectionKind, u64, &mut [u8]) -> FerrosGrynnResult<()>,
		WriteArg: FnMut(PageNo, &AlignedPage) -> FerrosGrynnResult<()>,
		CheckArg: FnMut() -> FerrosGrynnResult<()>,
	>(
		&self,
		mut element: ElementArg,
		mut write: WriteArg,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<()> {
		if Self::new(self.header)? != *self {
			return Err(invalid("reachability writer layout is noncanonical"));
		}
		let mut page = Box::new(AlignedPage::zeroed());
		check()?;
		self.encode_header(&mut page)?;
		write(PageNo::new(0), &page)?;
		for section in &self.sections {
			section.stream(&mut page, &mut element, &mut write, check)?;
		}
		check()
	}

	/// Decodes a header against the exact catalog identity and file extent.
	///
	/// # Errors
	/// Rejects corruption, unknown tags, reserved bytes, aliasing/noncanonical
	/// roots and lengths before allocation from disk metadata.
	pub fn decode_header(
		page: &AlignedPage,
		identity: GraphArtifactIdentity,
		actual_pages: u64,
	) -> FerrosGrynnResult<Self> {
		let common = page.validate(PageNo::new(0))?;
		let bytes = page.as_bytes();
		if common.page_type() != PageType::IndexReachabilityHeader
			|| !common.flags().is_empty()
			|| word(bytes, 32) != identity.index
			|| word(bytes, 36) != identity.version
			|| number(bytes, 40) != identity.graph
			|| number(bytes, 48) != identity.revision
			|| number(bytes, 160) != identity.fingerprint
			|| bytes[199] & !3 != 0
			|| bytes[200..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("reachability header ownership or reserved bytes are invalid"));
		}
		let algorithm = match bytes[196] {
			0 => ReachabilityTag::TreeCover,
			1 => ReachabilityTag::Grail,
			2 => ReachabilityTag::Ferrari,
			3 => ReachabilityTag::Bfl,
			4 => ReachabilityTag::ChainCover,
			_ => return Err(invalid("reachability algorithm tag is unknown")),
		};
		let layout = Self::new(ReachabilityHeader {
			identity,
			node_capacity: number(bytes, 56),
			edge_capacity: number(bytes, 64),
			components: number(bytes, 72),
			edges: number(bytes, 80),
			algorithm,
			parameter: word(bytes, 192),
			chains: number(bytes, 144),
			max_bytes: number(bytes, 152),
			descriptor_bytes: number(bytes, 176),
			mode: bytes[198],
			direction: bytes[197],
			reversed: bytes[199] & 1 != 0,
			has_cross_level: bytes[199] & 2 != 0,
		})?;
		if layout.page_count != actual_pages
			|| layout.page_count != number(bytes, 184)
			|| layout
				.sections
				.iter()
				.zip(ROOT_OFFSETS)
				.any(|(section, at)| section.root.get() != number(bytes, at))
		{
			return Err(invalid("reachability file extent or section roots are noncanonical"));
		}
		Ok(layout)
	}

	/// Admits complete decoded payload plus temporary maps, CSR and label
	/// views.
	///
	/// # Errors
	/// Returns a resource error before allocation from header lengths.
	pub fn admit_decode(&self, scratch_bytes: usize) -> FerrosGrynnResult<()> {
		if Self::new(self.header)? != *self {
			return Err(invalid("reachability decoder layout is noncanonical"));
		}
		ArraySection::admit_decode(
			&self.sections,
			self.header.components,
			self.header.edges,
			scratch_bytes,
		)
	}

	/// Loads exact sections only after aggregate scratch admission.
	///
	/// # Errors
	/// Rejects oversized metadata, malformed pages or cancellation without
	/// exposing partial decoded contents.
	pub fn load_sections<
		ReadArg: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
		CheckArg: FnMut() -> FerrosGrynnResult<()>,
	>(
		&self,
		scratch_bytes: usize,
		mut read: ReadArg,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<[Vec<u8>; 8]> {
		self.admit_decode(scratch_bytes)?;
		ArraySection::load_sections(
			&self.sections,
			self.page_count,
			scratch_bytes,
			&mut read,
			check,
		)
	}

	/// Writes ownership and computed roots into the final header destination.
	///
	/// # Errors
	/// Propagates the checked common header sealing error.
	fn encode_header(&self, page: &mut AlignedPage) -> FerrosGrynnResult<()> {
		page.reset(
			PageType::IndexReachabilityHeader,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(0),
		);
		let bytes = page.as_bytes_mut();
		let header = self.header;
		bytes[32..36].copy_from_slice(&header.identity.index.to_le_bytes());
		bytes[36..40].copy_from_slice(&header.identity.version.to_le_bytes());
		for (at, value) in [
			(40, header.identity.graph),
			(48, header.identity.revision),
			(56, header.node_capacity),
			(64, header.edge_capacity),
			(72, header.components),
			(80, header.edges),
			(144, header.chains),
			(152, header.max_bytes),
			(160, header.identity.fingerprint),
			(176, header.descriptor_bytes),
			(184, self.page_count),
		] {
			bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
		}
		for (section, at) in self.sections.iter().zip(ROOT_OFFSETS) {
			bytes[at..at + 8].copy_from_slice(&section.root.get().to_le_bytes());
		}
		bytes[192..196].copy_from_slice(&header.parameter.to_le_bytes());
		bytes[196] = header.algorithm as u8;
		bytes[197] = header.direction;
		bytes[198] = header.mode;
		bytes[199] = u8::from(header.reversed) | (u8::from(header.has_cross_level) << 1);
		page.seal()
	}
}

/// Reads a complete fixed-size header field.
fn word(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
/// Reads a complete fixed-size header field.
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
/// Typed corruption at the physical artifact boundary.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests;
