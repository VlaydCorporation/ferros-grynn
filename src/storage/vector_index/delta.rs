//! Common exact replacement/tombstone delta for HNSW and DiskANN.

use super::codec::{VectorBuildSpec, implementation_fingerprint, validate_vector};
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
	values::{
		Vector,
		storage::{
			DiskAtomRef,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

const DELTA_FORMAT_VERSION: u16 = 1;
const DELTA_FLAG: u8 = 1;
const NULL_PAGE: u64 = u64::MAX;
const HEADER_FIXED_END: usize = 96;
const STREAM_HEADER_BYTES: usize = 48;
const STREAM_PAYLOAD_BYTES: usize = PAGE_SIZE - STREAM_HEADER_BYTES;

/// Logical identity captured by one exact vector delta generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VectorDeltaSpec {
	pub(crate) base: VectorBuildSpec,
	pub(crate) revision: u64,
}

/// Complete replacement or deletion for one stable row address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VectorDeltaRecord {
	Upsert {
		atom: DiskAtomRef,
		vector: Vector,
	},
	Delete {
		atom: DiskAtomRef,
	},
}

impl VectorDeltaRecord {
	pub(crate) const fn atom(&self) -> DiskAtomRef {
		match self {
			Self::Upsert {
				atom,
				..
			}
			| Self::Delete {
				atom,
			} => *atom,
		}
	}
}

/// Fully validated exact delta projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedVectorDelta {
	pub(crate) spec: VectorDeltaSpec,
	pub(crate) records: Vec<VectorDeltaRecord>,
}

struct StreamLocation {
	first: u64,
	pages: u64,
}

/// Builds one deterministic delta file from complete row replacements.
pub(crate) fn build_vector_delta(
	spec: VectorDeltaSpec,
	mut records: Vec<VectorDeltaRecord>,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	records.sort_unstable_by_key(VectorDeltaRecord::atom);
	if records.windows(2).any(|pair| pair[0].atom() == pair[1].atom()) {
		return Err(invalid("vector delta contains duplicate row addresses"));
	}
	let mut stream = Vec::new();
	for record in &records {
		let start = stream.len();
		write_u32_vec(&mut stream, 0);
		match record {
			VectorDeltaRecord::Upsert {
				atom,
				vector,
			} => {
				validate_vector(spec.base, vector)?;
				let vector = CanonicalCodec::encode(StorageValue::Vector(vector.clone()).as_ref())?;
				stream.push(1);
				stream.extend_from_slice(&[0; 3]);
				write_u64_vec(&mut stream, atom.raw());
				write_u32_vec(
					&mut stream,
					u32::try_from(vector.len())
						.map_err(|_error| invalid("vector delta payload exceeds u32"))?,
				);
				write_u32_vec(&mut stream, 0);
				stream.extend_from_slice(&vector);
			}
			VectorDeltaRecord::Delete {
				atom,
			} => {
				stream.push(2);
				stream.extend_from_slice(&[0; 3]);
				write_u64_vec(&mut stream, atom.raw());
				write_u32_vec(&mut stream, 0);
				write_u32_vec(&mut stream, 0);
			}
		}
		let length = stream
			.len()
			.checked_sub(start)
			.ok_or_else(|| invalid("vector delta record length underflows"))?;
		stream[start..start + 4].copy_from_slice(
			&u32::try_from(length)
				.map_err(|_error| invalid("vector delta record exceeds u32"))?
				.to_le_bytes(),
		);
	}
	let stream_pages = page_count(stream.len())?;
	let total = usize::try_from(
		1_u64
			.checked_add(stream_pages)
			.ok_or_else(|| invalid("vector delta page count overflows u64"))?,
	)
	.map_err(|_error| invalid("vector delta page count exceeds usize"))?;
	let mut pages = Vec::with_capacity(total);
	pages.resize_with(total, AlignedPage::zeroed);
	let location = encode_stream(&mut pages, &stream)?;
	encode_header(&mut pages[0], spec, records.len(), location)?;
	Ok(pages)
}

/// Decodes one delta against the exact catalog/base definition.
pub(crate) fn decode_vector_delta(
	pages: &[AlignedPage],
	expected: VectorBuildSpec,
) -> FerrosGrynnResult<DecodedVectorDelta> {
	if pages.is_empty() {
		return Err(invalid("vector delta contains no header page"));
	}
	let (spec, record_count, location) = decode_header(&pages[0], expected)?;
	let expected_pages = 1_u64
		.checked_add(location.pages)
		.ok_or_else(|| invalid("vector delta page count overflows u64"))?;
	if usize::try_from(expected_pages).ok() != Some(pages.len()) {
		return Err(invalid("vector delta page count disagrees with its file"));
	}
	let stream = decode_stream(pages, location)?;
	let records = decode_records(spec, record_count, &stream)?;
	Ok(DecodedVectorDelta {
		spec,
		records,
	})
}

fn encode_header(
	page: &mut AlignedPage,
	spec: VectorDeltaSpec,
	record_count: usize,
	stream: StreamLocation,
) -> FerrosGrynnResult<()> {
	page.reset(PageType::IndexVector, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u16(bytes, 32, DELTA_FORMAT_VERSION);
	bytes[34] = DELTA_FLAG;
	bytes[35] = spec.base.algorithm()?.tag();
	write_u32(bytes, 36, spec.base.index.get());
	write_u32(bytes, 40, spec.base.index_version.get());
	match spec.base.scope {
		crate::datastore::IndexScope::Base => {
			bytes[44] = 0;
			write_u64(bytes, 48, 0);
		}
		crate::datastore::IndexScope::Effective {
			graph,
		} => {
			bytes[44] = 1;
			write_u64(bytes, 48, graph.get());
		}
	}
	write_u64(bytes, 56, spec.revision);
	write_u32(
		bytes,
		64,
		u32::try_from(record_count)
			.map_err(|_error| invalid("vector delta record count exceeds u32"))?,
	);
	write_u64(bytes, 72, stream.first);
	write_u64(bytes, 80, stream.pages);
	write_u64(bytes, 88, implementation_fingerprint(spec.base)?);
	page.seal()
}

fn decode_header(
	page: &AlignedPage,
	expected: VectorBuildSpec,
) -> FerrosGrynnResult<(VectorDeltaSpec, u32, StreamLocation)> {
	let header = page.validate(PageNo::new(0))?;
	if header.page_type() != PageType::IndexVector || !header.flags().is_empty() {
		return Err(invalid("vector delta header has the wrong page type or flags"));
	}
	let bytes = page.as_bytes();
	if read_u16(bytes, 32)? != DELTA_FORMAT_VERSION
		|| bytes[34] != DELTA_FLAG
		|| bytes[35] != expected.algorithm()?.tag()
		|| bytes[45..48].iter().any(|byte| *byte != 0)
		|| bytes[68..72].iter().any(|byte| *byte != 0)
		|| bytes[HEADER_FIXED_END..].iter().any(|byte| *byte != 0)
		|| read_u32(bytes, 36)? != expected.index.get()
		|| read_u32(bytes, 40)? != expected.index_version.get()
		|| read_u64(bytes, 88)? != implementation_fingerprint(expected)?
	{
		return Err(invalid("vector delta header disagrees with its base definition"));
	}
	let graph = read_u64(bytes, 48)?;
	let scope_matches = match expected.scope {
		crate::datastore::IndexScope::Base => bytes[44] == 0 && graph == 0,
		crate::datastore::IndexScope::Effective {
			graph: expected_graph,
		} => bytes[44] == 1 && graph == expected_graph.get(),
	};
	if !scope_matches {
		return Err(invalid("vector delta scope disagrees with its base definition"));
	}
	Ok((
		VectorDeltaSpec {
			base: expected,
			revision: read_u64(bytes, 56)?,
		},
		read_u32(bytes, 64)?,
		StreamLocation {
			first: read_u64(bytes, 72)?,
			pages: read_u64(bytes, 80)?,
		},
	))
}

fn decode_records(
	spec: VectorDeltaSpec,
	record_count: u32,
	stream: &[u8],
) -> FerrosGrynnResult<Vec<VectorDeltaRecord>> {
	let capacity = usize::try_from(record_count)
		.map_err(|_error| invalid("vector delta record count exceeds usize"))?;
	let mut records = Vec::with_capacity(capacity);
	let mut cursor = 0_usize;
	while cursor < stream.len() {
		if records.len() == capacity || cursor.checked_add(24).is_none_or(|end| end > stream.len())
		{
			return Err(invalid("vector delta record header crosses its stream"));
		}
		let length = usize::try_from(read_u32(stream, cursor)?)
			.map_err(|_error| invalid("vector delta record length exceeds usize"))?;
		if length < 24 {
			return Err(invalid("vector delta record is shorter than its header"));
		}
		let end = cursor
			.checked_add(length)
			.ok_or_else(|| invalid("vector delta record boundary overflows usize"))?;
		if end > stream.len() || stream[cursor + 5..cursor + 8].iter().any(|byte| *byte != 0) {
			return Err(invalid("vector delta record boundary or reserved bytes are invalid"));
		}
		let atom = DiskAtomRef::from_raw(read_u64(stream, cursor + 8)?)?;
		let vector_length = usize::try_from(read_u32(stream, cursor + 16)?)
			.map_err(|_error| invalid("vector delta payload length exceeds usize"))?;
		if read_u32(stream, cursor + 20)? != 0 {
			return Err(invalid("vector delta reserved field is non-zero"));
		}
		let record = match stream[cursor + 4] {
			1 if cursor.checked_add(24 + vector_length) == Some(end) => {
				let StorageValue::Vector(vector) =
					CanonicalCodec::decode(&stream[cursor + 24..end], DecodeLimits::default())?
				else {
					return Err(invalid("vector delta replacement is not a canonical vector"));
				};
				validate_vector(spec.base, &vector)?;
				VectorDeltaRecord::Upsert {
					atom,
					vector,
				}
			}
			2 if vector_length == 0 && length == 24 => VectorDeltaRecord::Delete {
				atom,
			},
			_ => return Err(invalid("vector delta operation or payload length is invalid")),
		};
		if records.last().is_some_and(|previous: &VectorDeltaRecord| previous.atom() >= atom) {
			return Err(invalid("vector delta records are not strictly ordered"));
		}
		records.push(record);
		cursor = end;
	}
	if records.len() != capacity {
		return Err(invalid("vector delta record count disagrees with its stream"));
	}
	Ok(records)
}

fn page_count(bytes: usize) -> FerrosGrynnResult<u64> {
	if bytes == 0 {
		return Ok(0);
	}
	u64::try_from(bytes.div_ceil(STREAM_PAYLOAD_BYTES))
		.map_err(|_error| invalid("vector delta page count exceeds u64"))
}

fn encode_stream(pages: &mut [AlignedPage], payload: &[u8]) -> FerrosGrynnResult<StreamLocation> {
	let count = page_count(payload.len())?;
	if count == 0 {
		return Ok(StreamLocation {
			first: NULL_PAGE,
			pages: 0,
		});
	}
	for ordinal in 0..count {
		let page_no = 1_u64
			.checked_add(ordinal)
			.ok_or_else(|| invalid("vector delta page number overflows u64"))?;
		let index = usize::try_from(page_no)
			.map_err(|_error| invalid("vector delta page number exceeds usize"))?;
		let start = usize::try_from(ordinal)
			.map_err(|_error| invalid("vector delta ordinal exceeds usize"))?
			.checked_mul(STREAM_PAYLOAD_BYTES)
			.ok_or_else(|| invalid("vector delta offset overflows usize"))?;
		let end = start.saturating_add(STREAM_PAYLOAD_BYTES).min(payload.len());
		let next = if ordinal + 1 == count {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let page = pages
			.get_mut(index)
			.ok_or_else(|| invalid("vector delta page lies outside allocated slots"))?;
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
				.map_err(|_error| invalid("vector delta payload exceeds u32"))?,
		);
		bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + end - start]
			.copy_from_slice(&payload[start..end]);
		page.seal()?;
	}
	Ok(StreamLocation {
		first: 1,
		pages: count,
	})
}

fn decode_stream(pages: &[AlignedPage], location: StreamLocation) -> FerrosGrynnResult<Vec<u8>> {
	if location.pages == 0 {
		if location.first != NULL_PAGE {
			return Err(invalid("empty vector delta stream has a non-null root"));
		}
		return Ok(Vec::new());
	}
	if location.first != 1 {
		return Err(invalid("vector delta stream root is not page one"));
	}
	let capacity = usize::try_from(location.pages)
		.ok()
		.and_then(|count| count.checked_mul(STREAM_PAYLOAD_BYTES))
		.ok_or_else(|| invalid("vector delta stream allocation exceeds usize"))?;
	let mut output = Vec::with_capacity(capacity);
	for ordinal in 0..location.pages {
		let page_no = 1_u64
			.checked_add(ordinal)
			.ok_or_else(|| invalid("vector delta page number overflows u64"))?;
		let page = pages
			.get(usize::try_from(page_no).map_err(|_error| invalid("page exceeds usize"))?)
			.ok_or_else(|| invalid("vector delta references a missing page"))?;
		let header = page.validate(PageNo::new(page_no))?;
		let bytes = page.as_bytes();
		let expected_next = if ordinal + 1 == location.pages {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let payload = usize::try_from(read_u32(bytes, PAGE_HEADER_SIZE + 8)?)
			.map_err(|_error| invalid("vector delta payload exceeds usize"))?;
		if header.page_type() != PageType::IndexVectorData
			|| !header.flags().is_empty()
			|| read_u64(bytes, PAGE_HEADER_SIZE)? != expected_next
			|| payload > STREAM_PAYLOAD_BYTES
			|| (ordinal + 1 != location.pages && payload != STREAM_PAYLOAD_BYTES)
			|| bytes[PAGE_HEADER_SIZE + 12..STREAM_HEADER_BYTES].iter().any(|byte| *byte != 0)
			|| bytes[STREAM_HEADER_BYTES + payload..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("vector delta stream page is malformed"));
		}
		output.extend_from_slice(&bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + payload]);
	}
	Ok(output)
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
		.ok_or_else(|| invalid("vector delta u16 field crosses its input"))
}

fn read_u32(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u32> {
	bytes
		.get(offset..offset + 4)
		.and_then(|bytes| bytes.try_into().ok())
		.map(u32::from_le_bytes)
		.ok_or_else(|| invalid("vector delta u32 field crosses its input"))
}

fn read_u64(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u64> {
	bytes
		.get(offset..offset + 8)
		.and_then(|bytes| bytes.try_into().ok())
		.map(u64::from_le_bytes)
		.ok_or_else(|| invalid("vector delta u64 field crosses its input"))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::num::NonZeroU16;

	use super::{VectorDeltaRecord, VectorDeltaSpec, build_vector_delta, decode_vector_delta};
	use crate::{
		datastore::{
			ClassId,
			ClassIndexOptions,
			HnswIndexOptions,
			IndexId,
			IndexScope,
			SchemaVersion,
			VectorDistance,
		},
		graph::AtomKind,
		storage::vector_index::VectorBuildSpec,
		values::{PropertyKeyId, Vector, VectorKind, storage::DiskAtomRef},
	};

	#[test]
	fn replacement_and_tombstone_round_trip_canonically() {
		let base = VectorBuildSpec {
			index: IndexId::new(9).unwrap(),
			index_version: SchemaVersion::INITIAL,
			scope: IndexScope::Base,
			class: ClassId::new(4).unwrap(),
			field: PropertyKeyId::new(3),
			options: ClassIndexOptions::Hnsw(
				HnswIndexOptions::defaults(
					VectorKind::F32,
					NonZeroU16::new(2).unwrap(),
					VectorDistance::Euclidean,
				)
				.unwrap(),
			),
			build_revision: 7,
			build_seed: 8,
		};
		let spec = VectorDeltaSpec {
			base,
			revision: 12,
		};
		let first = DiskAtomRef::from_parts(AtomKind::Node, 1).unwrap();
		let second = DiskAtomRef::from_parts(AtomKind::Node, 2).unwrap();
		let records = vec![
			VectorDeltaRecord::Delete {
				atom: second,
			},
			VectorDeltaRecord::Upsert {
				atom: first,
				vector: Vector::from(vec![1.0_f32, 2.0]),
			},
		];
		let pages = build_vector_delta(spec, records.clone()).unwrap();
		let decoded = decode_vector_delta(&pages, base).unwrap();
		assert_eq!(decoded.spec, spec);
		assert_eq!(decoded.records, records.into_iter().rev().collect::<Vec<_>>());
	}
}
