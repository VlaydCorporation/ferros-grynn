//! Canonical page framing shared by HNSW and DiskANN base generations.

use std::{borrow::Borrow, collections::BTreeSet, num::NonZeroU16};

use rapidhash::v3::rapidhash_v3;

use crate::{
	datastore::{
		ClassId,
		ClassIndexKind,
		ClassIndexOptions,
		DiskAnnIndexOptions,
		FunctionId,
		HnswIndexOptions,
		IndexId,
		IndexScope,
		SchemaVersion,
		VectorDistance,
		validate_ann_vector,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::page::{
		AlignedPage,
		PAGE_HEADER_SIZE,
		PAGE_SIZE,
		PageFlags,
		PageHeader,
		PageLsn,
		PageNo,
		PageType,
	},
	values::{
		PropertyKeyId,
		Vector,
		VectorKind,
		storage::{
			DiskAtomRef,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

const SIDE_FORMAT_VERSION: u16 = 2;
const NULL_PAGE: u64 = u64::MAX;
const NULL_NODE: u32 = u32::MAX;
const FLAG_HASHED_VECTOR: u8 = 1;
const HEADER_FIXED_END: usize = 184;
const STREAM_HEADER_BYTES: usize = 48;
pub(crate) const STREAM_PAYLOAD_BYTES: usize = PAGE_SIZE - STREAM_HEADER_BYTES;
const DIRECTORY_ENTRY_BYTES: usize = 16;
pub(crate) const VECTOR_NODE_HEADER_BYTES: usize = 24;
const DISK_ATOM_REF_BYTES: usize = 8;
const DISKANN_VECTOR_VERSION: &str = "0.59.0";

/// Physical algorithm encoded by one vector side-file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VectorAlgorithm {
	Hnsw,
	DiskAnn,
}

impl VectorAlgorithm {
	pub(crate) const fn from_kind(kind: ClassIndexKind) -> Option<Self> {
		match kind {
			ClassIndexKind::Hnsw => Some(Self::Hnsw),
			ClassIndexKind::DiskAnn => Some(Self::DiskAnn),
			_ => None,
		}
	}

	pub(crate) const fn kind(self) -> ClassIndexKind {
		match self {
			Self::Hnsw => ClassIndexKind::Hnsw,
			Self::DiskAnn => ClassIndexKind::DiskAnn,
		}
	}

	pub(super) const fn tag(self) -> u8 {
		match self {
			Self::Hnsw => 1,
			Self::DiskAnn => 2,
		}
	}

	fn from_tag(tag: u8) -> FerrosGrynnResult<Self> {
		match tag {
			1 => Ok(Self::Hnsw),
			2 => Ok(Self::DiskAnn),
			_ => Err(invalid("vector index algorithm tag is invalid")),
		}
	}
}

/// One source row presented to a deterministic vector builder.
pub(in crate::storage) struct VectorDocument<V = Vector> {
	pub(in crate::storage) atom: DiskAtomRef,
	pub(in crate::storage) vector: V,
}

/// Catalog metadata captured by one immutable vector generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VectorBuildSpec {
	pub(crate) index: IndexId,
	pub(crate) index_version: SchemaVersion,
	pub(crate) scope: IndexScope,
	pub(crate) class: ClassId,
	pub(crate) field: PropertyKeyId,
	pub(crate) options: ClassIndexOptions,
	pub(crate) build_revision: u64,
	pub(crate) build_seed: u64,
}

impl VectorBuildSpec {
	pub(crate) fn algorithm(self) -> FerrosGrynnResult<VectorAlgorithm> {
		match self.options {
			ClassIndexOptions::Hnsw(_) => Ok(VectorAlgorithm::Hnsw),
			ClassIndexOptions::DiskAnn(_) => Ok(VectorAlgorithm::DiskAnn),
			_ => Err(invalid("vector side-file has non-vector catalog options")),
		}
	}

	pub(crate) fn element(self) -> FerrosGrynnResult<VectorKind> {
		match self.options {
			ClassIndexOptions::Hnsw(options) => Ok(options.element()),
			ClassIndexOptions::DiskAnn(options) => Ok(options.element()),
			_ => Err(invalid("vector side-file has non-vector catalog options")),
		}
	}

	pub(crate) fn dimension(self) -> FerrosGrynnResult<NonZeroU16> {
		match self.options {
			ClassIndexOptions::Hnsw(options) => Ok(options.dimension()),
			ClassIndexOptions::DiskAnn(options) => Ok(options.dimension()),
			_ => Err(invalid("vector side-file has non-vector catalog options")),
		}
	}

	pub(crate) fn distance(self) -> FerrosGrynnResult<VectorDistance> {
		match self.options {
			ClassIndexOptions::Hnsw(options) => Ok(options.distance()),
			ClassIndexOptions::DiskAnn(options) => Ok(options.distance()),
			_ => Err(invalid("vector side-file has non-vector catalog options")),
		}
	}

	pub(crate) fn hashed_vector(self) -> bool {
		matches!(self.options, ClassIndexOptions::DiskAnn(options) if options.hashed_vector())
	}
}

/// One decoded graph ordinal with full-precision vector bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VectorNode<V = Vector> {
	pub(crate) postings: Vec<DiskAtomRef>,
	pub(crate) vector: V,
	pub(crate) vector_hash: u64,
	/// Level zero first. DiskANN always has exactly one level.
	pub(crate) neighbors: Vec<Vec<u32>>,
}

/// Fully validated immutable vector generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedVectorIndex {
	pub(crate) spec: VectorBuildSpec,
	pub(crate) entrypoint: Option<u32>,
	pub(crate) nodes: Vec<VectorNode>,
	pub(crate) fingerprint: u64,
	pub(crate) provider_fingerprint: u64,
}

/// Encoded immutable vector file before catalog publication.
pub(in crate::storage) struct BuiltVectorIndex {
	pub(in crate::storage) root: PageNo,
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamLocation {
	pub(crate) first: u64,
	pub(crate) pages: u64,
}

/// Byte range of one vector node in the immutable data stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VectorNodeLocation {
	pub(crate) offset: u64,
	pub(crate) length: u32,
}

/// Bounds needed to read one vector node without materializing an unbounded
/// `HASHED_VECTOR` posting list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VectorNodeRecordLayout {
	pub(crate) record_length: usize,
	pub(crate) posting_count: usize,
	pub(crate) postings_end: usize,
	pub(crate) vector_length: usize,
	pub(crate) level_count: usize,
	pub(crate) vector_hash: u64,
}

/// One page-loaded DiskANN node. `postings` may be a stable prefix while
/// `posting_count` records the complete durable group size.
pub(crate) struct PagedVectorNode {
	pub(crate) node: VectorNode,
	pub(crate) posting_count: usize,
}

/// Encodes already-built graph nodes into stable heap page slots.
pub(super) fn encode_vector_index<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	entrypoint: Option<u32>,
	nodes: &[VectorNode<V>],
) -> FerrosGrynnResult<BuiltVectorIndex> {
	validate_nodes(spec, entrypoint, nodes)?;
	let mut stream = Vec::new();
	let mut directory = Vec::with_capacity(nodes.len().saturating_mul(DIRECTORY_ENTRY_BYTES));
	for node in nodes {
		let vector = encode_vector_payload(node)?;
		let record_start = stream.len();
		let record_length = vector_node_record_length(node, vector.len())?;
		stream.extend_from_slice(&encode_vector_node_header(node, vector.len(), record_length)?);
		for atom in &node.postings {
			write_u64_vec(&mut stream, atom.raw());
		}
		for neighbors in &node.neighbors {
			write_u32_vec(
				&mut stream,
				u32::try_from(neighbors.len())
					.map_err(|_error| invalid("vector neighbor count exceeds u32"))?,
			);
			for neighbor in neighbors {
				write_u32_vec(&mut stream, *neighbor);
			}
		}
		stream.extend_from_slice(&vector);
		if stream.len().checked_sub(record_start) != Some(record_length) {
			return Err(invalid("vector record encoder disagrees with its computed length"));
		}
		write_u64_vec(
			&mut directory,
			u64::try_from(record_start)
				.map_err(|_error| invalid("vector record offset exceeds u64"))?,
		);
		write_u32_vec(
			&mut directory,
			u32::try_from(record_length)
				.map_err(|_error| invalid("vector record length exceeds u32"))?,
		);
		write_u32_vec(&mut directory, 0);
	}

	let stream_pages = page_count_for_stream(stream.len())?;
	let directory_pages = page_count_for_stream(directory.len())?;
	let total_pages = 1_u64
		.checked_add(stream_pages)
		.ok_or_else(|| invalid("vector page count overflows u64"))?
		.checked_add(directory_pages)
		.ok_or_else(|| invalid("vector page count overflows u64"))?;
	let total_pages = usize::try_from(total_pages)
		.map_err(|_error| invalid("vector page count exceeds usize"))?;
	let mut pages = Vec::with_capacity(total_pages);
	pages.resize_with(total_pages, AlignedPage::zeroed);
	let location = encode_stream(&mut pages, 1, &stream)?;
	let directory_first = 1_u64
		.checked_add(stream_pages)
		.ok_or_else(|| invalid("vector directory root overflows u64"))?;
	let directory_location = encode_stream(&mut pages, directory_first, &directory)?;
	let posting_count = nodes.iter().try_fold(0_usize, |total, node| {
		total
			.checked_add(node.postings.len())
			.ok_or_else(|| invalid("vector posting count overflows usize"))
	})?;
	encode_header(
		&mut pages[0],
		spec,
		entrypoint,
		nodes.len(),
		posting_count,
		directory_location,
		location,
	)?;
	Ok(BuiltVectorIndex {
		root: PageNo::new(0),
		pages,
	})
}

/// Decodes and structurally validates one HNSW or DiskANN generation.
pub(crate) fn decode_vector_index(pages: &[AlignedPage]) -> FerrosGrynnResult<DecodedVectorIndex> {
	if pages.is_empty() {
		return Err(invalid("vector index contains no header page"));
	}
	let header = decode_vector_header(&pages[0])?;
	let expected = 1_u64
		.checked_add(header.directory.pages)
		.ok_or_else(|| invalid("vector page count overflows u64"))?
		.checked_add(header.stream.pages)
		.ok_or_else(|| invalid("vector page count overflows u64"))?;
	if usize::try_from(expected).ok() != Some(pages.len()) {
		return Err(invalid("vector header page count disagrees with the file"));
	}
	let stream = decode_stream(pages, header.stream)?;
	let directory = decode_directory(
		&decode_stream(pages, header.directory)?,
		header.node_count,
		stream.len(),
	)?;
	let nodes = decode_nodes(header.spec, &directory, &stream)?;
	validate_nodes(header.spec, header.entrypoint, &nodes)?;
	let posting_count = nodes.iter().try_fold(0_usize, |total, node| {
		total
			.checked_add(node.postings.len())
			.ok_or_else(|| invalid("vector posting count overflows usize"))
	})?;
	if posting_count != header.posting_count
		|| header.rebuild_threshold != rebuild_threshold(posting_count)?
	{
		return Err(invalid("vector rebuild threshold disagrees with the posting count"));
	}
	let fingerprint = implementation_fingerprint(header.spec)?;
	let provider_fingerprint = provider_fingerprint(header.spec.algorithm()?);
	if header.fingerprint != fingerprint || header.provider_fingerprint != provider_fingerprint {
		return Err(invalid("vector implementation/provider fingerprint is unsupported"));
	}
	Ok(DecodedVectorIndex {
		spec: header.spec,
		entrypoint: header.entrypoint,
		nodes,
		fingerprint,
		provider_fingerprint,
	})
}

pub(crate) struct PagedVectorHeader {
	pub(crate) spec: VectorBuildSpec,
	pub(crate) entrypoint: Option<u32>,
	pub(crate) node_count: u32,
	pub(crate) directory: StreamLocation,
	pub(crate) stream: StreamLocation,
	pub(crate) fingerprint: u64,
	pub(crate) provider_fingerprint: u64,
	pub(crate) rebuild_threshold: u64,
	pub(crate) posting_count: usize,
}

pub(super) fn encode_header(
	page: &mut AlignedPage,
	spec: VectorBuildSpec,
	entrypoint: Option<u32>,
	node_count: usize,
	posting_count: usize,
	directory: StreamLocation,
	stream: StreamLocation,
) -> FerrosGrynnResult<()> {
	let algorithm = spec.algorithm()?;
	let (primary, secondary, tertiary, alpha_bits) = option_parameters(spec.options)?;
	let (distance_tag, distance_function, distance_version, distance_aux) =
		encode_distance(spec.distance()?);
	page.reset(PageType::IndexVector, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u16(bytes, 32, SIDE_FORMAT_VERSION);
	bytes[34] = algorithm.tag();
	bytes[35] = u8::from(spec.hashed_vector()) * FLAG_HASHED_VECTOR;
	write_u32(bytes, 36, spec.index.get());
	write_u32(bytes, 40, spec.index_version.get());
	match spec.scope {
		IndexScope::Base => {
			bytes[44] = 0;
			write_u64(bytes, 48, 0);
		}
		IndexScope::Effective {
			graph,
		} => {
			bytes[44] = 1;
			write_u64(bytes, 48, graph.get());
		}
	}
	bytes[45] = encode_vector_kind(spec.element()?);
	bytes[46] = distance_tag;
	write_u32(bytes, 56, spec.class.get());
	write_u32(bytes, 60, spec.field.get());
	write_u16(bytes, 64, spec.dimension()?.get());
	write_u16(bytes, 66, primary);
	write_u16(bytes, 68, secondary);
	write_u16(bytes, 70, tertiary);
	write_u32(bytes, 72, alpha_bits);
	write_u32(bytes, 76, distance_function);
	write_u32(bytes, 80, distance_version);
	write_u64(bytes, 88, distance_aux);
	write_u64(bytes, 96, spec.build_revision);
	write_u64(bytes, 104, spec.build_seed);
	write_u32(bytes, 112, entrypoint.unwrap_or(NULL_NODE));
	write_u32(
		bytes,
		116,
		u32::try_from(node_count).map_err(|_error| invalid("vector node count exceeds u32"))?,
	);
	write_u64(bytes, 120, stream.first);
	write_u64(bytes, 128, stream.pages);
	write_u64(bytes, 136, implementation_fingerprint(spec)?);
	write_u64(bytes, 144, provider_fingerprint(algorithm));
	write_u64(bytes, 152, rebuild_threshold(posting_count)?);
	write_u64(bytes, 160, directory.first);
	write_u64(bytes, 168, directory.pages);
	write_u64(
		bytes,
		176,
		u64::try_from(posting_count)
			.map_err(|_error| invalid("vector posting count exceeds u64"))?,
	);
	page.seal()
}

pub(crate) fn decode_vector_header(page: &AlignedPage) -> FerrosGrynnResult<PagedVectorHeader> {
	let header = page.validate(PageNo::new(0))?;
	if header.page_type() != PageType::IndexVector || !header.flags().is_empty() {
		return Err(invalid("vector header page has the wrong type or flags"));
	}
	let bytes = page.as_bytes();
	if read_u16(bytes, 32)? != SIDE_FORMAT_VERSION
		|| bytes[47] != 0
		|| bytes[84..88].iter().any(|byte| *byte != 0)
		|| bytes[HEADER_FIXED_END..].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("vector header version or reserved bytes are invalid"));
	}
	let algorithm = VectorAlgorithm::from_tag(bytes[34])?;
	if bytes[35] & !FLAG_HASHED_VECTOR != 0
		|| (algorithm == VectorAlgorithm::Hnsw && bytes[35] != 0)
	{
		return Err(invalid("vector header flags are invalid for its algorithm"));
	}
	let index = IndexId::new(read_u32(bytes, 36)?)
		.ok_or_else(|| invalid("vector header contains a null IndexId"))?;
	let index_version = SchemaVersion::new(read_u32(bytes, 40)?)
		.ok_or_else(|| invalid("vector header contains a zero index version"))?;
	let graph = read_u64(bytes, 48)?;
	let scope = match bytes[44] {
		0 if graph == 0 => IndexScope::Base,
		1 => IndexScope::Effective {
			graph: crate::graph::GraphId::new(graph)
				.ok_or_else(|| invalid("vector effective scope has a null GraphId"))?,
		},
		_ => return Err(invalid("vector scope discriminator is invalid")),
	};
	let element = decode_vector_kind(bytes[45])?;
	let dimension = NonZeroU16::new(read_u16(bytes, 64)?)
		.ok_or_else(|| invalid("vector header contains a zero dimension"))?;
	let distance = decode_distance(
		bytes[46],
		read_u32(bytes, 76)?,
		read_u32(bytes, 80)?,
		read_u64(bytes, 88)?,
	)?;
	let primary = NonZeroU16::new(read_u16(bytes, 66)?)
		.ok_or_else(|| invalid("vector header contains a zero primary parameter"))?;
	let secondary = NonZeroU16::new(read_u16(bytes, 68)?)
		.ok_or_else(|| invalid("vector header contains a zero secondary parameter"))?;
	let tertiary = read_u16(bytes, 70)?;
	let alpha_bits = read_u32(bytes, 72)?;
	let options = match algorithm {
		VectorAlgorithm::Hnsw => {
			if alpha_bits != 0 {
				return Err(invalid("HNSW header contains DiskANN alpha"));
			}
			let options = HnswIndexOptions::new(element, dimension, distance, primary, secondary)?;
			if options.m0().get() != tertiary {
				return Err(invalid("HNSW M0 does not equal 2M"));
			}
			ClassIndexOptions::Hnsw(options)
		}
		VectorAlgorithm::DiskAnn => {
			if tertiary != 0 {
				return Err(invalid("DiskANN header contains a non-zero reserved parameter"));
			}
			ClassIndexOptions::DiskAnn(DiskAnnIndexOptions::new(
				element,
				dimension,
				distance,
				primary,
				secondary,
				f32::from_bits(alpha_bits),
				bytes[35] & FLAG_HASHED_VECTOR != 0,
			)?)
		}
	};
	let spec = VectorBuildSpec {
		index,
		index_version,
		scope,
		class: ClassId::new(read_u32(bytes, 56)?)
			.ok_or_else(|| invalid("vector header contains a null ClassId"))?,
		field: PropertyKeyId::new(read_u32(bytes, 60)?),
		options,
		build_revision: read_u64(bytes, 96)?,
		build_seed: read_u64(bytes, 104)?,
	};
	let node_count = read_u32(bytes, 116)?;
	let entrypoint = match read_u32(bytes, 112)? {
		NULL_NODE if node_count == 0 => None,
		NULL_NODE => return Err(invalid("non-empty vector graph has no entrypoint")),
		value if value < node_count => Some(value),
		_ => return Err(invalid("vector entrypoint lies outside the graph")),
	};
	let directory = StreamLocation {
		first: read_u64(bytes, 160)?,
		pages: read_u64(bytes, 168)?,
	};
	let stream = StreamLocation {
		first: read_u64(bytes, 120)?,
		pages: read_u64(bytes, 128)?,
	};
	let posting_count = usize::try_from(read_u64(bytes, 176)?)
		.map_err(|_error| invalid("vector posting count exceeds usize"))?;
	validate_stream_locations(node_count, directory, stream)?;
	let decoded = PagedVectorHeader {
		spec,
		entrypoint,
		node_count,
		directory,
		stream,
		fingerprint: read_u64(bytes, 136)?,
		provider_fingerprint: read_u64(bytes, 144)?,
		rebuild_threshold: read_u64(bytes, 152)?,
		posting_count,
	};
	if decoded.fingerprint != implementation_fingerprint(spec)?
		|| decoded.provider_fingerprint != provider_fingerprint(algorithm)
		|| decoded.rebuild_threshold != rebuild_threshold(posting_count)?
		|| (node_count == 0) != (posting_count == 0)
	{
		return Err(invalid("vector header fingerprint or counts are inconsistent"));
	}
	Ok(decoded)
}

fn validate_stream_locations(
	node_count: u32,
	directory: StreamLocation,
	stream: StreamLocation,
) -> FerrosGrynnResult<()> {
	let directory_bytes = usize::try_from(node_count)
		.ok()
		.and_then(|count| count.checked_mul(DIRECTORY_ENTRY_BYTES))
		.ok_or_else(|| invalid("vector directory size exceeds usize"))?;
	let expected_directory_pages = page_count_for_stream(directory_bytes)?;
	if directory.pages != expected_directory_pages {
		return Err(invalid("vector directory page count disagrees with its node count"));
	}
	if node_count == 0 {
		if directory.first != NULL_PAGE
			|| directory.pages != 0
			|| stream.first != NULL_PAGE
			|| stream.pages != 0
		{
			return Err(invalid("empty vector graph has non-empty stream locations"));
		}
		return Ok(());
	}
	let expected_directory_first = 1_u64
		.checked_add(stream.pages)
		.ok_or_else(|| invalid("vector directory root overflows u64"))?;
	if stream.first != 1 || directory.first != expected_directory_first || stream.pages == 0 {
		return Err(invalid("vector directory and data streams are not contiguous"));
	}
	Ok(())
}

/// Validates that a registered side-file contains exactly the pages declared
/// by its immutable header.
pub(crate) fn validate_vector_file_pages(
	header: &PagedVectorHeader,
	file_pages: u64,
) -> FerrosGrynnResult<()> {
	let expected = 1_u64
		.checked_add(header.directory.pages)
		.and_then(|count| count.checked_add(header.stream.pages))
		.ok_or_else(|| invalid("vector file page count overflows u64"))?;
	if expected != file_pages {
		return Err(invalid("vector header page count disagrees with the file"));
	}
	Ok(())
}

/// Resolves one ordinal to the page and within-payload offset of its directory
/// entry. Directory entries never straddle pages.
pub(crate) fn vector_directory_address(
	header: &PagedVectorHeader,
	ordinal: u32,
) -> FerrosGrynnResult<(PageNo, usize)> {
	if ordinal >= header.node_count {
		return Err(invalid("vector ordinal lies outside its directory"));
	}
	let byte_offset = usize::try_from(ordinal)
		.ok()
		.and_then(|value| value.checked_mul(DIRECTORY_ENTRY_BYTES))
		.ok_or_else(|| invalid("vector directory offset exceeds usize"))?;
	let page_ordinal = byte_offset / STREAM_PAYLOAD_BYTES;
	let within = byte_offset % STREAM_PAYLOAD_BYTES;
	if within + DIRECTORY_ENTRY_BYTES > STREAM_PAYLOAD_BYTES {
		return Err(invalid("vector directory entry straddles pages"));
	}
	let page = header
		.directory
		.first
		.checked_add(
			u64::try_from(page_ordinal)
				.map_err(|_error| invalid("vector directory page ordinal exceeds u64"))?,
		)
		.ok_or_else(|| invalid("vector directory page number overflows u64"))?;
	Ok((PageNo::new(page), within))
}

/// Decodes one page from a known contiguous vector stream after its immutable
/// page image has already passed common-header and checksum validation.
pub(crate) fn decode_validated_vector_stream_page(
	page: &AlignedPage,
	page_no: PageNo,
	location: StreamLocation,
	header: PageHeader,
) -> FerrosGrynnResult<&[u8]> {
	if location.pages == 0
		|| page_no.get() < location.first
		|| page_no.get() >= location.first.saturating_add(location.pages)
	{
		return Err(invalid("vector page lies outside its declared stream"));
	}
	let ordinal = page_no.get() - location.first;
	if header.page_no() != page_no
		|| header.page_type() != PageType::IndexVectorData
		|| !header.flags().is_empty()
	{
		return Err(invalid("vector stream page has the wrong type or flags"));
	}
	let bytes = page.as_bytes();
	let expected_next = if ordinal + 1 == location.pages {
		NULL_PAGE
	} else {
		page_no.get() + 1
	};
	let payload_len = usize::try_from(read_u32(bytes, PAGE_HEADER_SIZE + 8)?)
		.map_err(|_error| invalid("vector page payload exceeds usize"))?;
	if read_u64(bytes, PAGE_HEADER_SIZE)? != expected_next
		|| payload_len > STREAM_PAYLOAD_BYTES
		|| (ordinal + 1 != location.pages && payload_len != STREAM_PAYLOAD_BYTES)
		|| bytes[PAGE_HEADER_SIZE + 12..STREAM_HEADER_BYTES].iter().any(|byte| *byte != 0)
		|| bytes[STREAM_HEADER_BYTES + payload_len..].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("vector stream chain, payload, or padding is malformed"));
	}
	Ok(&bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + payload_len])
}

fn decode_nodes(
	spec: VectorBuildSpec,
	directory: &[VectorNodeLocation],
	stream: &[u8],
) -> FerrosGrynnResult<Vec<VectorNode>> {
	let mut nodes = Vec::with_capacity(directory.len());
	for location in directory {
		let cursor = usize::try_from(location.offset)
			.map_err(|_error| invalid("vector record offset exceeds usize"))?;
		let length = usize::try_from(location.length)
			.map_err(|_error| invalid("vector record length exceeds usize"))?;
		let end = cursor
			.checked_add(length)
			.ok_or_else(|| invalid("vector record boundary overflows usize"))?;
		let record =
			stream.get(cursor..end).ok_or_else(|| invalid("vector record crosses its stream"))?;
		nodes.push(decode_vector_node_record(spec, record)?);
	}
	Ok(nodes)
}

fn decode_directory(
	bytes: &[u8],
	node_count: u32,
	stream_length: usize,
) -> FerrosGrynnResult<Vec<VectorNodeLocation>> {
	let count =
		usize::try_from(node_count).map_err(|_error| invalid("vector node count exceeds usize"))?;
	let expected = count
		.checked_mul(DIRECTORY_ENTRY_BYTES)
		.ok_or_else(|| invalid("vector directory length overflows usize"))?;
	if bytes.len() != expected {
		return Err(invalid("vector directory length disagrees with its node count"));
	}
	let mut locations = Vec::with_capacity(count);
	let mut expected_offset = 0_u64;
	for entry in bytes.chunks_exact(DIRECTORY_ENTRY_BYTES) {
		let location = decode_vector_directory_entry(entry)?;
		if location.offset != expected_offset || location.length < 24 {
			return Err(invalid("vector directory is not a contiguous record map"));
		}
		expected_offset = location
			.offset
			.checked_add(u64::from(location.length))
			.ok_or_else(|| invalid("vector directory boundary overflows u64"))?;
		locations.push(location);
	}
	if usize::try_from(expected_offset).ok() != Some(stream_length) {
		return Err(invalid("vector directory does not consume the data stream exactly"));
	}
	Ok(locations)
}

pub(super) fn encode_vector_payload<V: Borrow<Vector>>(
	node: &VectorNode<V>,
) -> FerrosGrynnResult<Vec<u8>> {
	CanonicalCodec::encode(StorageValue::Vector(node.vector.borrow().clone()).as_ref())
}

pub(super) fn vector_node_record_length<V>(
	node: &VectorNode<V>,
	vector_length: usize,
) -> FerrosGrynnResult<usize> {
	let postings = node
		.postings
		.len()
		.checked_mul(DISK_ATOM_REF_BYTES)
		.ok_or_else(|| invalid("vector posting bytes overflow usize"))?;
	let adjacency = node.neighbors.iter().try_fold(0_usize, |total, neighbors| {
		total
			.checked_add(4)
			.and_then(|total| total.checked_add(neighbors.len().checked_mul(4)?))
			.ok_or_else(|| invalid("vector adjacency bytes overflow usize"))
	})?;
	VECTOR_NODE_HEADER_BYTES
		.checked_add(postings)
		.and_then(|length| length.checked_add(adjacency))
		.and_then(|length| length.checked_add(vector_length))
		.ok_or_else(|| invalid("vector node record length overflows usize"))
}

pub(super) fn encode_vector_node_header<V>(
	node: &VectorNode<V>,
	vector_length: usize,
	record_length: usize,
) -> FerrosGrynnResult<[u8; VECTOR_NODE_HEADER_BYTES]> {
	let mut header = [0_u8; VECTOR_NODE_HEADER_BYTES];
	header[..4].copy_from_slice(
		&u32::try_from(record_length)
			.map_err(|_error| invalid("vector node record exceeds u32"))?
			.to_le_bytes(),
	);
	header[4..8].copy_from_slice(
		&u32::try_from(vector_length)
			.map_err(|_error| invalid("canonical vector payload exceeds u32"))?
			.to_le_bytes(),
	);
	header[8..12].copy_from_slice(
		&u32::try_from(node.postings.len())
			.map_err(|_error| invalid("vector posting count exceeds u32"))?
			.to_le_bytes(),
	);
	header[12..14].copy_from_slice(
		&u16::try_from(node.neighbors.len())
			.map_err(|_error| invalid("vector graph level count exceeds u16"))?
			.to_le_bytes(),
	);
	header[16..24].copy_from_slice(&node.vector_hash.to_le_bytes());
	Ok(header)
}

/// Decodes one 16-byte ordinal-directory entry.
pub(crate) fn decode_vector_directory_entry(bytes: &[u8]) -> FerrosGrynnResult<VectorNodeLocation> {
	if bytes.len() != DIRECTORY_ENTRY_BYTES || read_u32(bytes, 12)? != 0 {
		return Err(invalid("vector directory entry is malformed"));
	}
	Ok(VectorNodeLocation {
		offset: read_u64(bytes, 0)?,
		length: read_u32(bytes, 8)?,
	})
}

/// Decodes one complete record selected through the ordinal directory.
pub(crate) fn decode_vector_node_record(
	spec: VectorBuildSpec,
	record: &[u8],
) -> FerrosGrynnResult<VectorNode> {
	let header = record
		.get(..VECTOR_NODE_HEADER_BYTES)
		.ok_or_else(|| invalid("vector node header crosses its record"))?;
	let layout = decode_vector_node_record_layout(
		header,
		u32::try_from(record.len()).map_err(|_error| invalid("vector record exceeds u32"))?,
	)?;
	let postings = record
		.get(VECTOR_NODE_HEADER_BYTES..layout.postings_end)
		.ok_or_else(|| invalid("vector postings cross their record"))?;
	let tail = record
		.get(layout.postings_end..)
		.ok_or_else(|| invalid("vector node tail crosses its record"))?;
	let decoded = decode_vector_node_parts(spec, layout, postings, tail)?;
	if decoded.node.postings.len() != decoded.posting_count {
		return Err(invalid("complete vector record omitted postings"));
	}
	Ok(decoded.node)
}

/// Validates the fixed header of one vector node against its directory entry.
pub(crate) fn decode_vector_node_record_layout(
	header: &[u8],
	directory_length: u32,
) -> FerrosGrynnResult<VectorNodeRecordLayout> {
	if header.len() != VECTOR_NODE_HEADER_BYTES {
		return Err(invalid("vector node header has the wrong length"));
	}
	let record_length = usize::try_from(read_u32(header, 0)?)
		.map_err(|_error| invalid("vector record length exceeds usize"))?;
	let vector_length = usize::try_from(read_u32(header, 4)?)
		.map_err(|_error| invalid("vector payload length exceeds usize"))?;
	let posting_count = usize::try_from(read_u32(header, 8)?)
		.map_err(|_error| invalid("vector posting count exceeds usize"))?;
	let level_count = usize::from(read_u16(header, 12)?);
	let directory_length = usize::try_from(directory_length)
		.map_err(|_error| invalid("vector directory length exceeds usize"))?;
	if record_length != directory_length
		|| posting_count == 0
		|| level_count == 0
		|| read_u16(header, 14)? != 0
	{
		return Err(invalid("vector node metadata is invalid"));
	}
	let postings_end = VECTOR_NODE_HEADER_BYTES
		.checked_add(
			posting_count
				.checked_mul(DISK_ATOM_REF_BYTES)
				.ok_or_else(|| invalid("vector postings length overflows usize"))?,
		)
		.ok_or_else(|| invalid("vector postings boundary overflows usize"))?;
	let minimum_length = postings_end
		.checked_add(
			level_count
				.checked_mul(4)
				.ok_or_else(|| invalid("vector neighbor headers overflow usize"))?,
		)
		.and_then(|length| length.checked_add(vector_length))
		.ok_or_else(|| invalid("vector record minimum length overflows usize"))?;
	if minimum_length > record_length {
		return Err(invalid("vector postings or payload cross their record"));
	}
	Ok(VectorNodeRecordLayout {
		record_length,
		posting_count,
		postings_end,
		vector_length,
		level_count,
		vector_hash: read_u64(header, 16)?,
	})
}

/// Decodes a bounded posting prefix and the complete bounded node tail.
pub(crate) fn decode_vector_node_parts(
	spec: VectorBuildSpec,
	layout: VectorNodeRecordLayout,
	posting_bytes: &[u8],
	tail: &[u8],
) -> FerrosGrynnResult<PagedVectorNode> {
	let maximum_posting_bytes = layout
		.posting_count
		.checked_mul(DISK_ATOM_REF_BYTES)
		.ok_or_else(|| invalid("vector postings length overflows usize"))?;
	if posting_bytes.is_empty()
		|| !posting_bytes.len().is_multiple_of(DISK_ATOM_REF_BYTES)
		|| posting_bytes.len() > maximum_posting_bytes
		|| layout
			.postings_end
			.checked_add(tail.len())
			.is_none_or(|length| length != layout.record_length)
	{
		return Err(invalid("vector node parts do not match their record layout"));
	}
	let mut postings = Vec::with_capacity(posting_bytes.len() / DISK_ATOM_REF_BYTES);
	for bytes in posting_bytes.chunks_exact(DISK_ATOM_REF_BYTES) {
		let atom = DiskAtomRef::from_raw(u64::from_le_bytes(
			bytes.try_into().map_err(|_error| invalid("vector posting has the wrong width"))?,
		))?;
		if postings.last().is_some_and(|previous| previous >= &atom) {
			return Err(invalid("vector postings are not strictly ordered"));
		}
		postings.push(atom);
	}
	let mut at = 0_usize;
	let mut neighbors = Vec::with_capacity(layout.level_count);
	for _ in 0..layout.level_count {
		let count = usize::try_from(read_u32(tail, at)?)
			.map_err(|_error| invalid("vector neighbor count exceeds usize"))?;
		at =
			at.checked_add(4).ok_or_else(|| invalid("vector neighbor boundary overflows usize"))?;
		let neighbor_end = at
			.checked_add(
				count
					.checked_mul(4)
					.ok_or_else(|| invalid("vector neighbor bytes overflow usize"))?,
			)
			.ok_or_else(|| invalid("vector neighbor boundary overflows usize"))?;
		if neighbor_end > tail.len() {
			return Err(invalid("vector neighbors cross their record"));
		}
		let mut level = Vec::with_capacity(count);
		while at < neighbor_end {
			level.push(read_u32(tail, at)?);
			at += 4;
		}
		neighbors.push(level);
	}
	if at.checked_add(layout.vector_length) != Some(tail.len()) {
		return Err(invalid("canonical vector payload length disagrees with its record"));
	}
	let vector_bytes = &tail[at..];
	if rapidhash_v3(vector_bytes) != layout.vector_hash {
		return Err(invalid("canonical vector payload hash does not match"));
	}
	let StorageValue::Vector(vector) =
		CanonicalCodec::decode(vector_bytes, DecodeLimits::default())?
	else {
		return Err(invalid("vector node payload is not a canonical vector"));
	};
	validate_vector(spec, &vector)?;
	Ok(PagedVectorNode {
		node: VectorNode {
			postings,
			vector,
			vector_hash: layout.vector_hash,
			neighbors,
		},
		posting_count: layout.posting_count,
	})
}

pub(super) fn validate_nodes<V: Borrow<Vector>>(
	spec: VectorBuildSpec,
	entrypoint: Option<u32>,
	nodes: &[VectorNode<V>],
) -> FerrosGrynnResult<()> {
	let algorithm = spec.algorithm()?;
	if nodes.is_empty() != entrypoint.is_none()
		|| entrypoint
			.is_some_and(|value| usize::try_from(value).map_or(true, |value| value >= nodes.len()))
	{
		return Err(invalid("vector graph entrypoint is inconsistent"));
	}
	let mut all_postings = BTreeSet::new();
	for (ordinal, node) in nodes.iter().enumerate() {
		validate_vector(spec, node.vector.borrow())?;
		let bytes =
			CanonicalCodec::encode(StorageValue::Vector(node.vector.borrow().clone()).as_ref())?;
		if node.vector_hash != rapidhash_v3(&bytes)
			|| node.postings.is_empty()
			|| node.postings.windows(2).any(|pair| pair[0] >= pair[1])
			|| node.postings.iter().any(|atom| !all_postings.insert(*atom))
		{
			return Err(invalid("vector node payload, hash, or postings are inconsistent"));
		}
		let expected_levels = match algorithm {
			VectorAlgorithm::Hnsw => deterministic_level(spec, node.postings[0])? + 1,
			VectorAlgorithm::DiskAnn => 1,
		};
		if node.neighbors.len() != expected_levels {
			return Err(invalid("vector node level count is not deterministic"));
		}
		for (level, neighbors) in node.neighbors.iter().enumerate() {
			let maximum = maximum_degree(spec, level)?;
			if neighbors.len() > maximum {
				return Err(invalid("vector adjacency exceeds its configured degree"));
			}
			let mut unique = BTreeSet::new();
			for neighbor in neighbors {
				let neighbor = usize::try_from(*neighbor)
					.map_err(|_error| invalid("vector neighbor exceeds usize"))?;
				if neighbor >= nodes.len() || neighbor == ordinal || !unique.insert(neighbor) {
					return Err(invalid(
						"vector adjacency contains an invalid or duplicate ordinal",
					));
				}
				if level >= nodes[neighbor].neighbors.len() {
					return Err(invalid("vector adjacency targets a node absent from its level"));
				}
			}
		}
	}
	Ok(())
}

pub(super) fn validate_vector(spec: VectorBuildSpec, vector: &Vector) -> FerrosGrynnResult<()> {
	if vector.kind() != spec.element()? || vector.len() != usize::from(spec.dimension()?.get()) {
		return Err(invalid("vector kind or dimension disagrees with the index definition"));
	}
	validate_ann_vector(vector)
}

pub(super) fn deterministic_level(
	spec: VectorBuildSpec,
	atom: DiskAtomRef,
) -> FerrosGrynnResult<usize> {
	if spec.algorithm()? == VectorAlgorithm::DiskAnn {
		return Ok(0);
	}
	let mut seed = Vec::with_capacity(32);
	seed.extend_from_slice(&spec.index.get().to_le_bytes());
	seed.extend_from_slice(&spec.index_version.get().to_le_bytes());
	seed.extend_from_slice(&spec.build_revision.to_le_bytes());
	seed.extend_from_slice(&spec.build_seed.to_le_bytes());
	seed.extend_from_slice(&atom.raw().to_le_bytes());
	let hash = rapidhash_v3(&seed);
	let m = match spec.options {
		ClassIndexOptions::Hnsw(options) => u32::from(options.m().get()),
		_ => return Err(invalid("HNSW level requested for non-HNSW options")),
	};
	let mut level = 0_usize;
	let mut sample = hash;
	while level < 31 && sample % u64::from(m.max(2)) == 0 {
		level += 1;
		sample /= u64::from(m.max(2));
	}
	Ok(level)
}

pub(super) fn maximum_degree(spec: VectorBuildSpec, level: usize) -> FerrosGrynnResult<usize> {
	match spec.options {
		ClassIndexOptions::Hnsw(options) => Ok(usize::from(if level == 0 {
			options.m0().get()
		} else {
			options.m().get()
		})),
		ClassIndexOptions::DiskAnn(options) if level == 0 => {
			Ok(usize::from(options.degree().get()))
		}
		ClassIndexOptions::DiskAnn(_) => Err(invalid("DiskANN has no adjacency above level zero")),
		_ => Err(invalid("vector degree requested for non-vector options")),
	}
}

fn option_parameters(options: ClassIndexOptions) -> FerrosGrynnResult<(u16, u16, u16, u32)> {
	match options {
		ClassIndexOptions::Hnsw(options) => {
			Ok((options.m().get(), options.ef_construction().get(), options.m0().get(), 0))
		}
		ClassIndexOptions::DiskAnn(options) => Ok((
			options.degree().get(),
			options.build_list_size().get(),
			0,
			options.alpha().to_bits(),
		)),
		_ => Err(invalid("vector side-file has non-vector options")),
	}
}

fn encode_distance(distance: VectorDistance) -> (u8, u32, u32, u64) {
	match distance {
		VectorDistance::Euclidean => (0, 0, 0, 0),
		VectorDistance::Cosine => (1, 0, 0, 0),
		VectorDistance::Manhattan => (2, 0, 0, 0),
		VectorDistance::Minkowski(exponent) => (3, 0, 0, exponent.to_bits()),
		VectorDistance::InnerProduct => (4, 0, 0, 0),
		VectorDistance::CosineNormalized => (5, 0, 0, 0),
		VectorDistance::Custom {
			function,
			version,
		} => (6, function.get(), version.get(), 0),
	}
}

fn decode_distance(
	tag: u8,
	function: u32,
	version: u32,
	aux: u64,
) -> FerrosGrynnResult<VectorDistance> {
	let distance = match (tag, function, version, aux) {
		(0, 0, 0, 0) => VectorDistance::Euclidean,
		(1, 0, 0, 0) => VectorDistance::Cosine,
		(2, 0, 0, 0) => VectorDistance::Manhattan,
		(3, 0, 0, bits) => VectorDistance::Minkowski(f64::from_bits(bits)),
		(4, 0, 0, 0) => VectorDistance::InnerProduct,
		(5, 0, 0, 0) => VectorDistance::CosineNormalized,
		(6, function, version, 0) => VectorDistance::Custom {
			function: FunctionId::new(function)
				.ok_or_else(|| invalid("custom vector distance has a null FunctionId"))?,
			version: SchemaVersion::new(version)
				.ok_or_else(|| invalid("custom vector distance has a zero version"))?,
		},
		_ => return Err(invalid("vector distance encoding is malformed")),
	};
	distance.validate()?;
	Ok(distance)
}

fn encode_vector_kind(kind: VectorKind) -> u8 {
	match kind {
		VectorKind::F64 => 0,
		VectorKind::F32 => 1,
		VectorKind::F16 => 2,
		VectorKind::I64 => 3,
		VectorKind::I32 => 4,
		VectorKind::I16 => 5,
		VectorKind::I8 => 6,
		VectorKind::U8 => 7,
	}
}

fn decode_vector_kind(tag: u8) -> FerrosGrynnResult<VectorKind> {
	match tag {
		0 => Ok(VectorKind::F64),
		1 => Ok(VectorKind::F32),
		2 => Ok(VectorKind::F16),
		3 => Ok(VectorKind::I64),
		4 => Ok(VectorKind::I32),
		5 => Ok(VectorKind::I16),
		6 => Ok(VectorKind::I8),
		7 => Ok(VectorKind::U8),
		_ => Err(invalid("vector element kind tag is invalid")),
	}
}

pub(super) fn implementation_fingerprint(spec: VectorBuildSpec) -> FerrosGrynnResult<u64> {
	let mut bytes = Vec::with_capacity(96);
	bytes.extend_from_slice(b"ferros-grynn/vector-index/v1");
	bytes.push(spec.algorithm()?.tag());
	bytes.push(encode_vector_kind(spec.element()?));
	let distance = encode_distance(spec.distance()?);
	bytes.push(distance.0);
	bytes.extend_from_slice(&distance.1.to_le_bytes());
	bytes.extend_from_slice(&distance.2.to_le_bytes());
	bytes.extend_from_slice(&distance.3.to_le_bytes());
	bytes.extend_from_slice(&spec.dimension()?.get().to_le_bytes());
	let parameters = option_parameters(spec.options)?;
	bytes.extend_from_slice(&parameters.0.to_le_bytes());
	bytes.extend_from_slice(&parameters.1.to_le_bytes());
	bytes.extend_from_slice(&parameters.2.to_le_bytes());
	bytes.extend_from_slice(&parameters.3.to_le_bytes());
	bytes.push(u8::from(spec.hashed_vector()));
	bytes.extend_from_slice(&spec.build_seed.to_le_bytes());
	Ok(rapidhash_v3(&bytes))
}

fn provider_fingerprint(algorithm: VectorAlgorithm) -> u64 {
	if algorithm == VectorAlgorithm::Hnsw {
		return 0;
	}
	let value = format!("diskann/{}/diskann-vector/{DISKANN_VECTOR_VERSION}", diskann::version(),);
	rapidhash_v3(value.as_bytes())
}

fn rebuild_threshold(posting_count: usize) -> FerrosGrynnResult<u64> {
	let posting_count = u64::try_from(posting_count)
		.map_err(|_error| invalid("vector posting count exceeds u64"))?;
	Ok(4096.min((posting_count / 1000).max(1)))
}

fn page_count_for_stream(bytes: usize) -> FerrosGrynnResult<u64> {
	if bytes == 0 {
		return Ok(0);
	}
	u64::try_from(bytes.div_ceil(STREAM_PAYLOAD_BYTES))
		.map_err(|_error| invalid("vector stream page count exceeds u64"))
}

fn encode_stream(
	pages: &mut [AlignedPage],
	first: u64,
	payload: &[u8],
) -> FerrosGrynnResult<StreamLocation> {
	let count = page_count_for_stream(payload.len())?;
	if count == 0 {
		return Ok(StreamLocation {
			first: NULL_PAGE,
			pages: 0,
		});
	}
	for ordinal in 0..count {
		let page_no = first
			.checked_add(ordinal)
			.ok_or_else(|| invalid("vector page number overflows u64"))?;
		let page_index = usize::try_from(page_no)
			.map_err(|_error| invalid("vector page number exceeds usize"))?;
		let start = usize::try_from(ordinal)
			.map_err(|_error| invalid("vector stream ordinal exceeds usize"))?
			.checked_mul(STREAM_PAYLOAD_BYTES)
			.ok_or_else(|| invalid("vector stream offset overflows usize"))?;
		let end = start.saturating_add(STREAM_PAYLOAD_BYTES).min(payload.len());
		let next = if ordinal + 1 == count {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let page = pages
			.get_mut(page_index)
			.ok_or_else(|| invalid("vector stream page lies outside allocated slots"))?;
		page.reset(
			PageType::IndexVectorData,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(page_no),
		);
		let bytes = page.as_bytes_mut();
		write_u64(bytes, PAGE_HEADER_SIZE, next);
		write_u32(
			bytes,
			PAGE_HEADER_SIZE + 8,
			u32::try_from(end - start)
				.map_err(|_error| invalid("vector page payload exceeds u32"))?,
		);
		bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + end - start]
			.copy_from_slice(&payload[start..end]);
		page.seal()?;
	}
	Ok(StreamLocation {
		first,
		pages: count,
	})
}

fn decode_stream(pages: &[AlignedPage], location: StreamLocation) -> FerrosGrynnResult<Vec<u8>> {
	if location.pages == 0 {
		if location.first != NULL_PAGE {
			return Err(invalid("empty vector stream has a non-null root"));
		}
		return Ok(Vec::new());
	}
	if location.first == NULL_PAGE {
		return Err(invalid("non-empty vector stream has a null root"));
	}
	let capacity = usize::try_from(location.pages)
		.ok()
		.and_then(|count| count.checked_mul(STREAM_PAYLOAD_BYTES))
		.ok_or_else(|| invalid("vector stream allocation exceeds usize"))?;
	let mut output = Vec::with_capacity(capacity);
	for ordinal in 0..location.pages {
		let page_no = location
			.first
			.checked_add(ordinal)
			.ok_or_else(|| invalid("vector page number overflows u64"))?;
		let page = pages
			.get(usize::try_from(page_no).map_err(|_error| invalid("page exceeds usize"))?)
			.ok_or_else(|| invalid("vector stream references a missing page"))?;
		let header = page.validate(PageNo::new(page_no))?;
		if header.page_type() != PageType::IndexVectorData || !header.flags().is_empty() {
			return Err(invalid("vector stream page has the wrong type or flags"));
		}
		let bytes = page.as_bytes();
		let expected_next = if ordinal + 1 == location.pages {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let payload_len = usize::try_from(read_u32(bytes, PAGE_HEADER_SIZE + 8)?)
			.map_err(|_error| invalid("vector page payload exceeds usize"))?;
		if read_u64(bytes, PAGE_HEADER_SIZE)? != expected_next
			|| payload_len > STREAM_PAYLOAD_BYTES
			|| (ordinal + 1 != location.pages && payload_len != STREAM_PAYLOAD_BYTES)
			|| bytes[PAGE_HEADER_SIZE + 12..STREAM_HEADER_BYTES].iter().any(|byte| *byte != 0)
			|| bytes[STREAM_HEADER_BYTES + payload_len..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("vector stream chain, payload, or padding is malformed"));
		}
		output.extend_from_slice(&bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + payload_len]);
	}
	Ok(output)
}

fn write_u16_vec(output: &mut Vec<u8>, value: u16) {
	output.extend_from_slice(&value.to_le_bytes());
}

fn write_u32_vec(output: &mut Vec<u8>, value: u32) {
	output.extend_from_slice(&value.to_le_bytes());
}

fn write_u64_vec(output: &mut Vec<u8>, value: u64) {
	output.extend_from_slice(&value.to_le_bytes());
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u16> {
	bytes
		.get(offset..offset + 2)
		.and_then(|bytes| bytes.try_into().ok())
		.map(u16::from_le_bytes)
		.ok_or_else(|| invalid("vector u16 field crosses its input"))
}

fn read_u32(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u32> {
	bytes
		.get(offset..offset + 4)
		.and_then(|bytes| bytes.try_into().ok())
		.map(u32::from_le_bytes)
		.ok_or_else(|| invalid("vector u32 field crosses its input"))
}

fn read_u64(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u64> {
	bytes
		.get(offset..offset + 8)
		.and_then(|bytes| bytes.try_into().ok())
		.map(u64::from_le_bytes)
		.ok_or_else(|| invalid("vector u64 field crosses its input"))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
