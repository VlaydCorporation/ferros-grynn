//! Checked immutable neighbourhood file layout over shared array sections.

use super::{
	derived_index::{ArraySection, GraphArtifactIdentity, SectionKind},
	page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Canonical section-root offsets in page zero.
const ROOT_OFFSETS: [usize; 8] = [144, 152, 160, 168, 176, 184, 192, 200];

/// Concrete materialized algorithms; adjacency and ON_DEMAND have no file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NeighbourhoodTag {
	/// Exact distinct sets for every center.
	Exact = 0,
	/// Approximate register sketches for explicit estimates only.
	Sketch = 1,
	/// Exact cached landmark sets, completed by traversal.
	Landmark = 2,
}

/// Complete header metadata; readiness is recorded only in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighbourhoodHeader {
	/// Captured definition and graph owner.
	pub identity: GraphArtifactIdentity,
	/// Sparse node-slot extent including holes.
	pub node_capacity: u64,
	/// Sparse edge-slot extent including holes.
	pub edge_capacity: u64,
	/// Selected live vertex count in canonical identity order.
	pub vertices: u64,
	/// Distinct selected relation arcs.
	pub edges: u64,
	/// Cached center count; zero for SKETCH.
	pub centers: u64,
	/// Exact flattened member count; zero for SKETCH.
	pub members: u64,
	/// Complete flattened HLL register length; zero for exact lists.
	pub register_bytes: u64,
	/// Hard cap including all pages and metadata.
	pub max_bytes: u64,
	/// Complete canonical definition length.
	pub descriptor_bytes: u64,
	/// HLL precision or requested landmark count; zero for EXACT.
	pub parameter: u32,
	/// Materialized maximum radius, 2..=4.
	pub radius: u8,
	/// Selected concrete algorithm.
	pub algorithm: NeighbourhoodTag,
	/// 0=OUT, 1=IN, 2=BOTH.
	pub direction: u8,
	/// 0=NODE, 1=ATOM.
	pub mode: u8,
	/// Qualified incidence requires exact scoped-query fallback.
	pub has_cross_level: bool,
}

/// Exact full-page packing used for build admission and durable restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighbourhoodLayout {
	/// Validated ownership and shape.
	pub header: NeighbourhoodHeader,
	/// Vertex references, CSR, centers, row offsets, members, registers and
	/// descriptor.
	pub sections: [ArraySection; 8],
	/// Exact file length including page zero.
	pub page_count: u64,
}

impl NeighbourhoodLayout {
	/// Checks the complete shape and budget before allocation or page writes.
	///
	/// # Errors
	/// Rejects ownership, parameter, count, overflow and artifact budget
	/// errors.
	pub fn new(header: NeighbourhoodHeader) -> FerrosGrynnResult<Self> {
		let capacity = header
			.node_capacity
			.checked_add(if header.mode == 1 {
				header.edge_capacity
			} else {
				0
			})
			.ok_or_else(|| invalid("neighbourhood vertex capacity overflows"))?;
		if header.identity.index == 0
			|| header.identity.version == 0
			|| header.identity.graph == 0
			|| header.identity.revision == 0
			|| header.node_capacity > 1_u64 << 40
			|| header.edge_capacity > 1_u64 << 40
			|| header.vertices > capacity
			|| header.vertices == 0 && header.edges != 0
			|| header.mode > 1
			|| header.direction > 2
			|| !(2..=4).contains(&header.radius)
			|| header.max_bytes == 0
			|| header.descriptor_bytes == 0
			|| header.descriptor_bytes > 8 << 20
		{
			return Err(invalid("neighbourhood header ownership or extent is invalid"));
		}
		let rows = header
			.centers
			.checked_mul(u64::from(header.radius))
			.ok_or_else(|| invalid("neighbourhood row count overflows"))?;
		let offsets = match header.algorithm {
			NeighbourhoodTag::Sketch => {
				if !(4..=18).contains(&header.parameter)
					|| header.centers != 0
					|| header.members != 0
				{
					return Err(invalid("neighbourhood sketch shape is invalid"));
				}
				let expected = header
					.vertices
					.checked_mul(u64::from(header.radius))
					.and_then(|rows| rows.checked_mul(1_u64 << header.parameter))
					.ok_or_else(|| invalid("neighbourhood sketch bytes overflow"))?;
				if header.register_bytes != expected {
					return Err(invalid("neighbourhood sketch register count is invalid"));
				}
				0
			}
			NeighbourhoodTag::Exact | NeighbourhoodTag::Landmark => {
				let expected = if header.algorithm == NeighbourhoodTag::Exact {
					if header.parameter != 0 {
						return Err(invalid("EXACT neighbourhood has algorithm parameters"));
					}
					header.vertices
				} else {
					if !(1..=256).contains(&header.parameter) {
						return Err(invalid("neighbourhood landmark count is invalid"));
					}
					header.vertices.min(u64::from(header.parameter))
				};
				if header.centers != expected
					|| header.register_bytes != 0
					|| header.members < rows
					|| rows.checked_mul(header.vertices).is_none_or(|max| header.members > max)
				{
					return Err(invalid("neighbourhood exact list count is invalid"));
				}
				rows.checked_add(1)
					.ok_or_else(|| invalid("neighbourhood offset count overflows"))?
			}
		};
		let member_kind = if header.algorithm == NeighbourhoodTag::Landmark {
			SectionKind::NeighbourhoodLandmarkMembers
		} else {
			SectionKind::NeighbourhoodMembers
		};
		let mut sections = [
			(SectionKind::NeighbourhoodVertexMap, 8, header.vertices),
			(
				SectionKind::CondensationCsrOffsets,
				8,
				header
					.vertices
					.checked_add(1)
					.ok_or_else(|| invalid("neighbourhood CSR offsets overflow"))?,
			),
			(SectionKind::CondensationCsrTargets, 8, header.edges),
			(SectionKind::NeighbourhoodCenters, 8, header.centers),
			(SectionKind::NeighbourhoodOffsets, 8, offsets),
			(member_kind, 8, header.members),
			(SectionKind::NeighbourhoodRegisters, 1, header.register_bytes),
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

	/// Streams canonical pages sequentially through one reusable aligned page.
	///
	/// # Errors
	/// Rejects forged layout or propagates cancellation, encoding and I/O
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
			return Err(invalid("neighbourhood writer layout is noncanonical"));
		}
		check()?;
		let mut page = Box::new(AlignedPage::zeroed());
		self.encode_header(&mut page)?;
		write(PageNo::new(0), &page)?;
		for section in &self.sections {
			section.stream(&mut page, &mut element, &mut write, check)?;
		}
		check()
	}

	/// Validates ownership, roots, parameters, checksum and exact file length.
	///
	/// # Errors
	/// Rejects malformed metadata before any allocation from its counts.
	pub fn decode_header(
		page: &AlignedPage,
		identity: GraphArtifactIdentity,
		actual_pages: u64,
	) -> FerrosGrynnResult<Self> {
		let common = page.validate(PageNo::new(0))?;
		let bytes = page.as_bytes();
		if common.page_type() != PageType::IndexNeighbourhoodHeader
			|| !common.flags().is_empty()
			|| word(bytes, 32) != identity.index
			|| word(bytes, 36) != identity.version
			|| number(bytes, 40) != identity.graph
			|| number(bytes, 48) != identity.revision
			|| number(bytes, 120) != identity.fingerprint
			|| bytes[216] & !1 != 0
			|| bytes[217..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("neighbourhood header ownership or reserved bytes are invalid"));
		}
		let algorithm = match bytes[213] {
			0 => NeighbourhoodTag::Exact,
			1 => NeighbourhoodTag::Sketch,
			2 => NeighbourhoodTag::Landmark,
			_ => return Err(invalid("neighbourhood algorithm tag is unknown")),
		};
		let layout = Self::new(NeighbourhoodHeader {
			identity,
			node_capacity: number(bytes, 56),
			edge_capacity: number(bytes, 64),
			vertices: number(bytes, 72),
			edges: number(bytes, 80),
			centers: number(bytes, 88),
			members: number(bytes, 96),
			register_bytes: number(bytes, 104),
			max_bytes: number(bytes, 112),
			descriptor_bytes: number(bytes, 128),
			parameter: word(bytes, 208),
			radius: bytes[212],
			algorithm,
			direction: bytes[214],
			mode: bytes[215],
			has_cross_level: bytes[216] & 1 != 0,
		})?;
		if layout.page_count != actual_pages
			|| layout.page_count != number(bytes, 136)
			|| layout
				.sections
				.iter()
				.zip(ROOT_OFFSETS)
				.any(|(section, at)| section.root.get() != number(bytes, at))
		{
			return Err(invalid("neighbourhood section roots or file extent are noncanonical"));
		}
		Ok(layout)
	}

	/// Admits full encoded and decoded views before allocation from metadata.
	///
	/// # Errors
	/// Rejects forged layout and scratch exhaustion.
	pub fn admit_decode(&self, scratch_bytes: usize) -> FerrosGrynnResult<()> {
		if Self::new(self.header)? != *self {
			return Err(invalid("neighbourhood decoder layout is noncanonical"));
		}
		ArraySection::admit_decode(
			&self.sections,
			self.header.vertices,
			self.header.edges,
			scratch_bytes,
		)
	}

	/// Loads checked sections after aggregate workspace admission.
	///
	/// # Errors
	/// Rejects malformed pages, allocation failure, scratch or cancellation.
	pub fn load_sections<Read, Check>(
		&self,
		scratch_bytes: usize,
		mut read: Read,
		check: &mut Check,
	) -> FerrosGrynnResult<[Vec<u8>; 8]>
	where
		Read: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		self.admit_decode(scratch_bytes)?;
		ArraySection::load_sections(
			&self.sections,
			self.page_count,
			scratch_bytes,
			&mut read,
			check,
		)
	}

	/// Encodes metadata into the final header destination.
	///
	/// # Errors
	/// Returns the checked page sealing error.
	fn encode_header(&self, page: &mut AlignedPage) -> FerrosGrynnResult<()> {
		page.reset(
			PageType::IndexNeighbourhoodHeader,
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
			(72, header.vertices),
			(80, header.edges),
			(88, header.centers),
			(96, header.members),
			(104, header.register_bytes),
			(112, header.max_bytes),
			(120, header.identity.fingerprint),
			(128, header.descriptor_bytes),
			(136, self.page_count),
		] {
			bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
		}
		for (section, at) in self.sections.iter().zip(ROOT_OFFSETS) {
			bytes[at..at + 8].copy_from_slice(&section.root.get().to_le_bytes());
		}
		bytes[208..212].copy_from_slice(&header.parameter.to_le_bytes());
		bytes[212] = header.radius;
		bytes[213] = header.algorithm as u8;
		bytes[214] = header.direction;
		bytes[215] = header.mode;
		bytes[216] = u8::from(header.has_cross_level);
		page.seal()
	}
}

/// Reads a complete checked header field.
fn word(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
/// Reads a complete checked header field.
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
/// Typed secondary corruption.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests;
