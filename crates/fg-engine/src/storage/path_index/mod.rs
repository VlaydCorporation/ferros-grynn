//! Checked LANDMARK_SSSP path file metadata and shared array sections.

use super::{
	derived_index::{ArraySection, GraphArtifactIdentity, SectionKind},
	page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Vertex, landmark, directed distance and definition roots in page zero.
const ROOT_OFFSETS: [usize; 5] = [72, 80, 88, 96, 120];

/// Ownership and shape of one immutable directed landmark artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandmarkPathHeader {
	/// Captured definition and graph identity.
	pub identity: GraphArtifactIdentity,
	/// Dense vertex count in canonical GlobalAtomId order.
	pub vertices: u64,
	/// Requested deterministic landmark count, 1..=256.
	pub requested_landmarks: u32,
	/// Complete semantic descriptor bytes.
	pub descriptor_bytes: u64,
	/// Full pages and metadata cap.
	pub max_bytes: u64,
	/// 1=HOP, 2=WEIGHT, 3=versioned PURE additive FUNCTION.
	pub cost_kind: u8,
	/// Function identity, zero except for FUNCTION.
	pub function: u32,
	/// Captured function version, zero except for FUNCTION.
	pub function_version: u32,
	/// 0=OUT, 1=IN, 2=BOTH.
	pub direction: u8,
	/// 0=NODE, 1=ATOM.
	pub mode: u8,
	/// Qualified incidence requires scoped exact query fallback.
	pub has_cross_level: bool,
}

/// Canonical full-page projection over five shared array sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandmarkPathLayout {
	/// Checked identity, cost model and extents.
	pub header: LandmarkPathHeader,
	/// Vertex refs, landmark ordinals, distances from/to and definition bytes.
	pub sections: [ArraySection; 5],
	/// Exact file page count including the header.
	pub page_count: u64,
}

impl LandmarkPathLayout {
	/// Projects all pages before allocation or output.
	///
	/// # Errors
	/// Rejects ownership, function/cost/model/extents or complete artifact
	/// budget.
	pub fn new(header: LandmarkPathHeader) -> FerrosGrynnResult<Self> {
		if header.identity.index == 0
			|| header.identity.version == 0
			|| header.identity.graph == 0
			|| header.identity.revision == 0
			|| header.vertices > 1_u64 << (40 + u32::from(header.mode == 1))
			|| !(1..=256).contains(&header.requested_landmarks)
			|| header.mode > 1
			|| header.direction > 2
			|| !(1..=3).contains(&header.cost_kind)
			|| header.max_bytes == 0
			|| header.descriptor_bytes == 0
			|| header.descriptor_bytes > 8 << 20
			|| if header.cost_kind == 3 {
				header.function == 0 || header.function_version == 0
			} else {
				header.function != 0 || header.function_version != 0
			} {
			return Err(invalid("landmark path header ownership or parameters are invalid"));
		}
		let landmarks = header.vertices.min(u64::from(header.requested_landmarks));
		let cells = header
			.vertices
			.checked_mul(landmarks)
			.ok_or_else(|| invalid("landmark distance extent overflows"))?;
		let distance_width = if header.cost_kind == 1 {
			8
		} else {
			16
		};
		let mut sections = [
			(SectionKind::PathVertexMap, 8, header.vertices),
			(SectionKind::LandmarkRefs, 8, landmarks),
			(SectionKind::LandmarkDistanceFrom, distance_width, cells),
			(SectionKind::LandmarkDistanceTo, distance_width, cells),
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

	/// Writes page zero followed by consecutive validated section pages.
	///
	/// # Errors
	/// Propagates cancellation, element/output errors or a forged layout.
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
			return Err(invalid("landmark path writer layout is noncanonical"));
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

	/// Checks ownership, reserved bytes, cost encoding, roots and exact file
	/// length.
	///
	/// # Errors
	/// Rejects corrupt metadata before allocating from any page counts.
	pub fn decode_header(
		page: &AlignedPage,
		identity: GraphArtifactIdentity,
		actual_pages: u64,
	) -> FerrosGrynnResult<Self> {
		let common = page.validate(PageNo::new(0))?;
		let bytes = page.as_bytes();
		if common.page_type() != PageType::IndexPathHeader
			|| !common.flags().is_empty()
			|| word(bytes, 32) != identity.index
			|| word(bytes, 36) != identity.version
			|| number(bytes, 40) != identity.graph
			|| number(bytes, 48) != identity.revision
			|| number(bytes, 152) != identity.fingerprint
			|| bytes[144] != 0
			|| word(bytes, 148) & !1 != 0
			|| number(bytes, 104) != PageNo::NULL.get()
			|| number(bytes, 112) != PageNo::NULL.get()
			|| bytes[177..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid(
				"landmark path ownership, algorithm or reserved bytes are invalid",
			));
		}
		let layout = Self::new(LandmarkPathHeader {
			identity,
			vertices: number(bytes, 56),
			requested_landmarks: word(bytes, 68),
			descriptor_bytes: number(bytes, 160),
			max_bytes: number(bytes, 128),
			function: word(bytes, 136),
			function_version: word(bytes, 140),
			cost_kind: bytes[146],
			direction: bytes[145],
			mode: bytes[176],
			has_cross_level: word(bytes, 148) & 1 != 0,
		})?;
		if layout.page_count != actual_pages
			|| layout.page_count != number(bytes, 168)
			|| u64::from(word(bytes, 64)) != layout.sections[1].count
			|| bytes[147]
				!= if layout.header.cost_kind == 1 {
					1
				} else {
					2
				}
			|| layout
				.sections
				.iter()
				.zip(ROOT_OFFSETS)
				.any(|(section, at)| section.root.get() != number(bytes, at))
		{
			return Err(invalid(
				"landmark path roots, distance encoding or file extent are noncanonical",
			));
		}
		Ok(layout)
	}

	/// Admits the complete payload plus decoded vertex/table views before
	/// reads.
	///
	/// # Errors
	/// Rejects a forged layout or aggregate scratch exhaustion.
	pub fn admit_decode(&self, scratch_bytes: usize) -> FerrosGrynnResult<()> {
		if Self::new(self.header)? != *self {
			return Err(invalid("landmark path decoder layout is noncanonical"));
		}
		ArraySection::admit_decode(&self.sections, self.header.vertices, 0, scratch_bytes)
	}

	/// Loads exact canonical sections through the common bounded loader.
	///
	/// # Errors
	/// Rejects malformed pages, resource limits or cancellation without partial
	/// data.
	pub fn load_sections<Read, Check>(
		&self,
		scratch_bytes: usize,
		read: Read,
		check: &mut Check,
	) -> FerrosGrynnResult<[Vec<u8>; 5]>
	where
		Read: FnMut(PageNo, &mut AlignedPage) -> FerrosGrynnResult<()>,
		Check: FnMut() -> FerrosGrynnResult<()>,
	{
		self.admit_decode(scratch_bytes)?;
		ArraySection::load_sections(&self.sections, self.page_count, scratch_bytes, read, check)
	}

	/// Encodes the checked header into its final aligned destination.
	///
	/// # Errors
	/// Propagates the common page sealing error.
	fn encode_header(&self, page: &mut AlignedPage) -> FerrosGrynnResult<()> {
		page.reset(PageType::IndexPathHeader, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
		let bytes = page.as_bytes_mut();
		let header = self.header;
		for (at, value) in [
			(32, header.identity.index),
			(36, header.identity.version),
			(64, header.vertices.min(u64::from(header.requested_landmarks)) as u32),
			(68, header.requested_landmarks),
			(136, header.function),
			(140, header.function_version),
			(148, u32::from(header.has_cross_level)),
		] {
			bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
		}
		for (at, value) in [
			(40, header.identity.graph),
			(48, header.identity.revision),
			(56, header.vertices),
			(104, PageNo::NULL.get()),
			(112, PageNo::NULL.get()),
			(128, header.max_bytes),
			(152, header.identity.fingerprint),
			(160, header.descriptor_bytes),
			(168, self.page_count),
		] {
			bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
		}
		for (section, at) in self.sections.iter().zip(ROOT_OFFSETS) {
			bytes[at..at + 8].copy_from_slice(&section.root.get().to_le_bytes());
		}
		bytes[144] = 0;
		bytes[145] = header.direction;
		bytes[146] = header.cost_kind;
		bytes[147] = if header.cost_kind == 1 {
			1
		} else {
			2
		};
		bytes[176] = header.mode;
		page.seal()
	}
}

/// Reads one checked complete header word.
fn word(bytes: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
/// Reads one checked complete header number.
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
/// Typed corruption at the physical path artifact boundary.
fn invalid(reason: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests;
