//! Exact replacement/tombstone delta for one immutable Fulltext generation.

use std::collections::BTreeMap;

use rapidhash::v3::rapidhash_v3;

use super::fulltext_index::FulltextBuildSpec;
use crate::{
	datastore::{AnalyzedToken, IndexScope},
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
	values::{Text, storage::DiskAtomRef},
};

/// Format version for format.
const FORMAT_VERSION: u16 = 1;
/// Configured delta tag value.
const DELTA_TAG: u8 = 1;
/// Configured null page value.
const NULL_PAGE: u64 = u64::MAX;
/// Configured header fixed end value.
const HEADER_FIXED_END: usize = 136;
/// Configured stream header bytes value.
const STREAM_HEADER_BYTES: usize = 48;
/// Configured stream payload bytes value.
const STREAM_PAYLOAD_BYTES: usize = PAGE_SIZE - STREAM_HEADER_BYTES;

/// Exact identity and revision captured by one Fulltext delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FulltextDeltaSpec {
	/// Base associated with this fulltext delta spec.
	pub(crate) base: FulltextBuildSpec,
	/// Revision identifying this version of the data.
	pub(crate) revision: u64,
}

/// Complete replacement or removal of one indexed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FulltextDeltaRecord {
	/// Upsert case of fulltext delta record.
	Upsert {
		/// Atom associated with this fulltext delta record.
		atom: DiskAtomRef,
		/// Tokens associated with this fulltext delta record.
		tokens: Vec<AnalyzedToken>,
	},
	/// Delete case of fulltext delta record.
	Delete {
		/// Atom associated with this fulltext delta record.
		atom: DiskAtomRef,
	},
}

impl FulltextDeltaRecord {
	/// Returns the atom result.
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

/// Fully validated logical projection of one Fulltext delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFulltextDelta {
	/// Spec associated with this decoded fulltext delta.
	pub(crate) spec: FulltextDeltaSpec,
	/// Records associated with this decoded fulltext delta.
	pub(crate) records: Vec<FulltextDeltaRecord>,
}

#[derive(Debug, Clone, Copy)]
/// Stream location state.
struct StreamLocation {
	/// First associated with this stream location.
	first: u64,
	/// Pages associated with this stream location.
	pages: u64,
}

/// Builds deterministic replacement records ordered by physical atom address.
pub fn build_fulltext_delta<RecordsArg: IntoIterator<Item = FulltextDeltaRecord>>(
	spec: FulltextDeltaSpec,
	records: RecordsArg,
) -> FerrosGrynnResult<Vec<AlignedPage>> {
	let records =
		records.into_iter().map(|record| (record.atom(), record)).collect::<BTreeMap<_, _>>();
	let mut payload = Vec::new();
	for record in records.values() {
		let start = payload.len();
		write_u32_vec(&mut payload, 0);
		match record {
			FulltextDeltaRecord::Upsert {
				atom,
				tokens,
			} => {
				payload.push(1);
				payload.extend_from_slice(&[0; 3]);
				write_u64_vec(&mut payload, atom.raw());
				write_u32_vec(
					&mut payload,
					u32::try_from(tokens.len())
						.map_err(|_error| invalid("Fulltext delta token count exceeds u32"))?,
				);
				write_u32_vec(&mut payload, 0);
				for token in tokens {
					if token.term.is_empty() || token.start >= token.end {
						return Err(invalid("Fulltext delta contains an invalid analyzer token"));
					}
					write_u32_vec(
						&mut payload,
						u32::try_from(token.term.len())
							.map_err(|_error| invalid("Fulltext delta term exceeds u32"))?,
					);
					write_u32_vec(&mut payload, token.start);
					write_u32_vec(&mut payload, token.end);
					write_u32_vec(&mut payload, 0);
					payload.extend_from_slice(token.term.as_bytes());
				}
			}
			FulltextDeltaRecord::Delete {
				atom,
			} => {
				payload.push(2);
				payload.extend_from_slice(&[0; 3]);
				write_u64_vec(&mut payload, atom.raw());
				write_u32_vec(&mut payload, 0);
				write_u32_vec(&mut payload, 0);
			}
		}
		let length = payload
			.len()
			.checked_sub(start)
			.ok_or_else(|| invalid("Fulltext delta record length underflows"))?;
		payload[start..start + 4].copy_from_slice(
			&u32::try_from(length)
				.map_err(|_error| invalid("Fulltext delta record exceeds u32"))?
				.to_le_bytes(),
		);
	}
	let stream_pages = page_count(payload.len())?;
	let total = usize::try_from(
		1_u64
			.checked_add(stream_pages)
			.ok_or_else(|| invalid("Fulltext delta page count overflows u64"))?,
	)
	.map_err(|_error| invalid("Fulltext delta page count exceeds usize"))?;
	let mut pages = Vec::with_capacity(total);
	pages.resize_with(total, AlignedPage::zeroed);
	let stream = encode_stream(&mut pages, &payload)?;
	encode_header(&mut pages[0], spec, records.len(), stream)?;
	Ok(pages)
}

/// Decodes and validates a delta against its exact immutable base definition.
pub fn decode_fulltext_delta(
	pages: &[AlignedPage],
	expected: FulltextBuildSpec,
) -> FerrosGrynnResult<DecodedFulltextDelta> {
	if pages.is_empty() {
		return Err(invalid("Fulltext delta contains no header page"));
	}
	let (spec, record_count, stream) = decode_header(&pages[0], expected)?;
	let expected_pages = 1_u64
		.checked_add(stream.pages)
		.ok_or_else(|| invalid("Fulltext delta page count overflows u64"))?;
	if usize::try_from(expected_pages).ok() != Some(pages.len()) {
		return Err(invalid("Fulltext delta page count disagrees with its file"));
	}
	let payload = decode_stream(pages, stream)?;
	let records = decode_records(record_count, &payload)?;
	Ok(DecodedFulltextDelta {
		spec,
		records,
	})
}

/// Encodes header.
fn encode_header(
	page: &mut AlignedPage,
	spec: FulltextDeltaSpec,
	record_count: usize,
	stream: StreamLocation,
) -> FerrosGrynnResult<()> {
	page.reset(PageType::IndexFulltextHeader, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u16(bytes, 32, FORMAT_VERSION);
	bytes[34] = DELTA_TAG;
	bytes[35] = u8::from(spec.base.options.highlights());
	write_u32(bytes, 36, spec.base.index.get());
	write_u32(bytes, 40, spec.base.index_version.get());
	write_u32(bytes, 44, spec.base.analyzer.get());
	write_u32(bytes, 48, spec.base.analyzer_version.get());
	match spec.base.scope {
		IndexScope::Base => {
			bytes[52] = 0;
			write_u64(bytes, 56, 0);
		}
		IndexScope::Effective {
			graph,
		} => {
			bytes[52] = 1;
			write_u64(bytes, 56, graph.get());
		}
	}
	write_u32(bytes, 64, spec.base.class.get());
	write_u32(bytes, 68, spec.base.field.get());
	write_u64(bytes, 72, spec.base.options.k1().to_bits());
	write_u64(bytes, 80, spec.base.options.b().to_bits());
	write_u64(bytes, 88, spec.base.build_revision);
	write_u64(bytes, 96, spec.revision);
	write_u32(
		bytes,
		104,
		u32::try_from(record_count)
			.map_err(|_error| invalid("Fulltext delta record count exceeds u32"))?,
	);
	write_u64(bytes, 112, stream.first);
	write_u64(bytes, 120, stream.pages);
	write_u64(bytes, 128, fingerprint(spec.base));
	page.seal()
}

/// Decodes header.
fn decode_header(
	page: &AlignedPage,
	expected: FulltextBuildSpec,
) -> FerrosGrynnResult<(FulltextDeltaSpec, u32, StreamLocation)> {
	let header = page.validate(PageNo::new(0))?;
	let bytes = page.as_bytes();
	if header.page_type() != PageType::IndexFulltextHeader
		|| !header.flags().is_empty()
		|| read_u16(bytes, 32)? != FORMAT_VERSION
		|| bytes[34] != DELTA_TAG
		|| bytes[35] != u8::from(expected.options.highlights())
		|| read_u32(bytes, 36)? != expected.index.get()
		|| read_u32(bytes, 40)? != expected.index_version.get()
		|| read_u32(bytes, 44)? != expected.analyzer.get()
		|| read_u32(bytes, 48)? != expected.analyzer_version.get()
		|| read_u32(bytes, 64)? != expected.class.get()
		|| read_u32(bytes, 68)? != expected.field.get()
		|| read_u64(bytes, 72)? != expected.options.k1().to_bits()
		|| read_u64(bytes, 80)? != expected.options.b().to_bits()
		|| read_u64(bytes, 88)? != expected.build_revision
		|| read_u64(bytes, 128)? != fingerprint(expected)
		|| bytes[53..56].iter().any(|byte| *byte != 0)
		|| bytes[108..112].iter().any(|byte| *byte != 0)
		|| bytes[HEADER_FIXED_END..].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("Fulltext delta header disagrees with its base definition"));
	}
	let graph = read_u64(bytes, 56)?;
	let scope_matches = match expected.scope {
		IndexScope::Base => bytes[52] == 0 && graph == 0,
		IndexScope::Effective {
			graph: expected_graph,
		} => bytes[52] == 1 && graph == expected_graph.get(),
	};
	if !scope_matches {
		return Err(invalid("Fulltext delta scope disagrees with its base definition"));
	}
	Ok((
		FulltextDeltaSpec {
			base: expected,
			revision: read_u64(bytes, 96)?,
		},
		read_u32(bytes, 104)?,
		StreamLocation {
			first: read_u64(bytes, 112)?,
			pages: read_u64(bytes, 120)?,
		},
	))
}

/// Decodes records.
fn decode_records(count: u32, payload: &[u8]) -> FerrosGrynnResult<Vec<FulltextDeltaRecord>> {
	let mut cursor = 0_usize;
	let mut records = Vec::with_capacity(
		usize::try_from(count).map_err(|_error| invalid("record count exceeds usize"))?,
	);
	while cursor < payload.len() {
		let length = usize::try_from(read_u32(payload, cursor)?)
			.map_err(|_error| invalid("Fulltext delta record length exceeds usize"))?;
		let end = cursor
			.checked_add(length)
			.filter(|end| *end <= payload.len())
			.ok_or_else(|| invalid("Fulltext delta record crosses its stream"))?;
		if length < 24 || payload[cursor + 5..cursor + 8].iter().any(|byte| *byte != 0) {
			return Err(invalid("Fulltext delta record header is malformed"));
		}
		let atom = DiskAtomRef::from_raw(read_u64(payload, cursor + 8)?)?;
		let token_count = usize::try_from(read_u32(payload, cursor + 16)?)
			.map_err(|_error| invalid("Fulltext delta token count exceeds usize"))?;
		if read_u32(payload, cursor + 20)? != 0
			|| records.last().is_some_and(|previous: &FulltextDeltaRecord| previous.atom() >= atom)
		{
			return Err(invalid("Fulltext delta records are malformed or unsorted"));
		}
		let record = match payload[cursor + 4] {
			1 => {
				let mut token_cursor = cursor + 24;
				let mut tokens = Vec::with_capacity(token_count);
				for _ in 0..token_count {
					if token_cursor.checked_add(16).is_none_or(|header| header > end) {
						return Err(invalid("Fulltext delta token header crosses its record"));
					}
					let length = usize::try_from(read_u32(payload, token_cursor)?)
						.map_err(|_error| invalid("Fulltext delta term length exceeds usize"))?;
					let start = read_u32(payload, token_cursor + 4)?;
					let finish = read_u32(payload, token_cursor + 8)?;
					if length == 0
						|| start >= finish
						|| read_u32(payload, token_cursor + 12)? != 0
						|| token_cursor.checked_add(16 + length).is_none_or(|finish| finish > end)
					{
						return Err(invalid("Fulltext delta token metadata is invalid"));
					}
					let term = std::str::from_utf8(
						&payload[token_cursor + 16..token_cursor + 16 + length],
					)
					.map_err(|_error| invalid("Fulltext delta term is not UTF-8"))?;
					tokens.push(AnalyzedToken {
						term: Text::from(term),
						start,
						end: finish,
					});
					token_cursor += 16 + length;
				}
				if token_cursor != end {
					return Err(invalid("Fulltext delta upsert has trailing bytes"));
				}
				FulltextDeltaRecord::Upsert {
					atom,
					tokens,
				}
			}
			2 if token_count == 0 && length == 24 => FulltextDeltaRecord::Delete {
				atom,
			},
			_ => return Err(invalid("Fulltext delta record tag or payload is invalid")),
		};
		records.push(record);
		cursor = end;
	}
	if records.len() != usize::try_from(count).unwrap_or(usize::MAX) {
		return Err(invalid("Fulltext delta record count disagrees with its stream"));
	}
	Ok(records)
}

/// Returns the fingerprint result.
fn fingerprint(spec: FulltextBuildSpec) -> u64 {
	let mut bytes = Vec::with_capacity(64);
	bytes.extend_from_slice(&spec.index.get().to_le_bytes());
	bytes.extend_from_slice(&spec.index_version.get().to_le_bytes());
	bytes.extend_from_slice(&spec.analyzer.get().to_le_bytes());
	bytes.extend_from_slice(&spec.analyzer_version.get().to_le_bytes());
	bytes.extend_from_slice(&spec.class.get().to_le_bytes());
	bytes.extend_from_slice(&spec.field.get().to_le_bytes());
	bytes.extend_from_slice(&spec.options.k1().to_bits().to_le_bytes());
	bytes.extend_from_slice(&spec.options.b().to_bits().to_le_bytes());
	bytes.push(u8::from(spec.options.highlights()));
	match spec.scope {
		IndexScope::Base => bytes.push(0),
		IndexScope::Effective {
			graph,
		} => {
			bytes.push(1);
			bytes.extend_from_slice(&graph.get().to_le_bytes());
		}
	}
	bytes.extend_from_slice(&spec.build_revision.to_le_bytes());
	rapidhash_v3(&bytes)
}

/// Returns the page count result.
fn page_count(bytes: usize) -> FerrosGrynnResult<u64> {
	if bytes == 0 {
		return Ok(0);
	}
	u64::try_from(bytes.div_ceil(STREAM_PAYLOAD_BYTES))
		.map_err(|_error| invalid("Fulltext delta stream page count exceeds u64"))
}

/// Encodes stream.
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
			.ok_or_else(|| invalid("Fulltext delta page number overflows u64"))?;
		let start = usize::try_from(ordinal)
			.map_err(|_error| invalid("Fulltext delta ordinal exceeds usize"))?
			.checked_mul(STREAM_PAYLOAD_BYTES)
			.ok_or_else(|| invalid("Fulltext delta offset overflows usize"))?;
		let end = (start + STREAM_PAYLOAD_BYTES).min(payload.len());
		let next = if ordinal + 1 == count {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let page = pages
			.get_mut(usize::try_from(page_no).map_err(|_error| invalid("page exceeds usize"))?)
			.ok_or_else(|| invalid("Fulltext delta page lies outside allocated slots"))?;
		page.reset(
			PageType::IndexFulltextPosting,
			PageFlags::empty(),
			PageLsn::ZERO,
			PageNo::new(page_no),
		);
		write_u64(page.as_bytes_mut(), PAGE_HEADER_SIZE, next);
		write_u32(
			page.as_bytes_mut(),
			PAGE_HEADER_SIZE + 8,
			u32::try_from(end - start)
				.map_err(|_error| invalid("Fulltext delta payload exceeds u32"))?,
		);
		page.as_bytes_mut()[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + end - start]
			.copy_from_slice(&payload[start..end]);
		page.seal()?;
	}
	Ok(StreamLocation {
		first: 1,
		pages: count,
	})
}

/// Decodes stream.
fn decode_stream(pages: &[AlignedPage], location: StreamLocation) -> FerrosGrynnResult<Vec<u8>> {
	if location.pages == 0 {
		if location.first != NULL_PAGE {
			return Err(invalid("empty Fulltext delta stream has a non-null root"));
		}
		return Ok(Vec::new());
	}
	if location.first != 1 {
		return Err(invalid("Fulltext delta stream root is not page one"));
	}
	let mut output = Vec::new();
	for ordinal in 0..location.pages {
		let page_no = 1_u64
			.checked_add(ordinal)
			.ok_or_else(|| invalid("Fulltext delta page number overflows u64"))?;
		let page = pages
			.get(usize::try_from(page_no).map_err(|_error| invalid("page exceeds usize"))?)
			.ok_or_else(|| invalid("Fulltext delta references a missing page"))?;
		let header = page.validate(PageNo::new(page_no))?;
		let expected_next = if ordinal + 1 == location.pages {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let length = usize::try_from(read_u32(page.as_bytes(), PAGE_HEADER_SIZE + 8)?)
			.map_err(|_error| invalid("Fulltext delta payload exceeds usize"))?;
		if header.page_type() != PageType::IndexFulltextPosting
			|| !header.flags().is_empty()
			|| read_u64(page.as_bytes(), PAGE_HEADER_SIZE)? != expected_next
			|| length > STREAM_PAYLOAD_BYTES
			|| (ordinal + 1 != location.pages && length != STREAM_PAYLOAD_BYTES)
			|| page.as_bytes()[PAGE_HEADER_SIZE + 12..STREAM_HEADER_BYTES]
				.iter()
				.any(|byte| *byte != 0)
			|| page.as_bytes()[STREAM_HEADER_BYTES + length..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("Fulltext delta stream page is malformed"));
		}
		output
			.extend_from_slice(&page.as_bytes()[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + length]);
	}
	Ok(output)
}

/// Writes u16.
fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
	bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

/// Writes u32.
fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
	bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Writes u64.
fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
	bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Writes u32 vec.
fn write_u32_vec(bytes: &mut Vec<u8>, value: u32) {
	bytes.extend_from_slice(&value.to_le_bytes());
}

/// Writes u64 vec.
fn write_u64_vec(bytes: &mut Vec<u8>, value: u64) {
	bytes.extend_from_slice(&value.to_le_bytes());
}

/// Reads u16.
fn read_u16(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u16> {
	let value = bytes
		.get(offset..offset + 2)
		.ok_or_else(|| invalid("Fulltext delta u16 crosses its byte range"))?;
	Ok(u16::from_le_bytes(value.try_into().map_err(|_error| invalid("invalid u16"))?))
}

/// Reads u32.
fn read_u32(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u32> {
	let value = bytes
		.get(offset..offset + 4)
		.ok_or_else(|| invalid("Fulltext delta u32 crosses its byte range"))?;
	Ok(u32::from_le_bytes(value.try_into().map_err(|_error| invalid("invalid u32"))?))
}

/// Reads u64.
fn read_u64(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u64> {
	let value = bytes
		.get(offset..offset + 8)
		.ok_or_else(|| invalid("Fulltext delta u64 crosses its byte range"))?;
	Ok(u64::from_le_bytes(value.try_into().map_err(|_error| invalid("invalid u64"))?))
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
/// Regression tests for fulltext index delta behavior.
mod tests {
	use super::*;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		datastore::{AnalyzerId, FulltextIndexOptions, IndexId, SchemaVersion},
		graph::AtomKind,
		values::PropertyKeyId,
	};

	/// Returns spec for this module.
	fn spec() -> FulltextBuildSpec {
		let analyzer = AnalyzerId::new(2).assert_some();
		FulltextBuildSpec {
			index: IndexId::new(3).assert_some(),
			index_version: SchemaVersion::INITIAL,
			analyzer,
			analyzer_version: SchemaVersion::INITIAL,
			scope: IndexScope::Base,
			class: crate::datastore::ClassId::new(4).assert_some(),
			field: PropertyKeyId::new(5),
			options: FulltextIndexOptions::defaults(analyzer, SchemaVersion::INITIAL, true),
			build_revision: 6,
		}
	}

	/// Verifies round trips upserts and tombstones.
	#[test]
	fn round_trips_upserts_and_tombstones() {
		let expected = spec();
		let pages = build_fulltext_delta(
			FulltextDeltaSpec {
				base: expected,
				revision: 9,
			},
			[
				FulltextDeltaRecord::Upsert {
					atom: DiskAtomRef::from_parts(AtomKind::Node, 1).assert_ok(),
					tokens: vec![AnalyzedToken {
						term: Text::from("term"),
						start: 1,
						end: 5,
					}],
				},
				FulltextDeltaRecord::Delete {
					atom: DiskAtomRef::from_parts(AtomKind::Edge, 2).assert_ok(),
				},
			],
		)
		.assert_ok();
		let decoded = decode_fulltext_delta(&pages, expected).assert_ok();
		assert_eq!(decoded.spec.revision, 9);
		assert_eq!(decoded.records.len(), 2);
	}
}
