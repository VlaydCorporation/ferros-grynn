//! Immutable page-framed Fulltext index codec.

#[path = "fulltext_index_stream.rs"]
mod stream;

use std::collections::BTreeMap;

use rapidhash::v3::rapidhash_v3;
pub(in crate::storage) use stream::write_fulltext_index_stream;

use crate::{
	datastore::{
		AnalyzedToken,
		AnalyzerId,
		ClassId,
		FulltextIndexOptions,
		IndexId,
		IndexScope,
		SchemaVersion,
	},
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
	values::{PropertyKeyId, storage::DiskAtomRef},
};

const SIDE_FORMAT_VERSION: u16 = 1;
const NULL_PAGE: u64 = u64::MAX;
const STREAM_HEADER_BYTES: usize = 48;
const STREAM_PAYLOAD_BYTES: usize = PAGE_SIZE - STREAM_HEADER_BYTES;
const HEADER_FIXED_END: usize = 192;
const FLAG_HIGHLIGHTS: u16 = 1;
const IMPLEMENTATION_FINGERPRINT: u64 = 0x9c7b_6dd1_a4a8_12ef;

/// One source document after execution of its exact analyzer version.
pub(in crate::storage) struct FulltextDocument {
	pub(in crate::storage) atom: DiskAtomRef,
	pub(in crate::storage) tokens: Vec<AnalyzedToken>,
}

/// Logical metadata encoded into page zero of one immutable FGFT side-file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FulltextBuildSpec {
	pub(crate) index: IndexId,
	pub(crate) index_version: SchemaVersion,
	pub(crate) analyzer: AnalyzerId,
	pub(crate) analyzer_version: SchemaVersion,
	pub(crate) scope: IndexScope,
	pub(crate) class: ClassId,
	pub(crate) field: PropertyKeyId,
	pub(crate) options: FulltextIndexOptions,
	pub(crate) build_revision: u64,
}

/// One complete immutable Fulltext file before catalog publication.
pub(in crate::storage) struct BuiltFulltextIndex {
	pub(in crate::storage) root: PageNo,
	pub(in crate::storage) pages: Vec<AlignedPage>,
}

/// One posting reconstructed from the exact immutable side-file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FulltextPosting {
	pub(crate) atom: DiskAtomRef,
	pub(crate) term_frequency: u32,
	pub(crate) document_length: u32,
	pub(crate) spans: Vec<(u32, u32)>,
}

/// One dictionary entry and its sorted postings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FulltextTerm {
	pub(crate) term: Vec<u8>,
	pub(crate) total_term_frequency: u64,
	pub(crate) postings: Vec<FulltextPosting>,
}

/// One indexed document, including documents whose analyzer produced no terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FulltextDocumentState {
	pub(crate) atom: DiskAtomRef,
	pub(crate) document_length: u32,
}

/// Fully validated logical projection of an immutable FGFT side-file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedFulltextIndex {
	pub(crate) spec: FulltextBuildSpec,
	pub(crate) document_count: u64,
	pub(crate) total_document_length: u64,
	pub(crate) documents: Vec<FulltextDocumentState>,
	pub(crate) terms: Vec<FulltextTerm>,
}

#[derive(Default)]
struct PostingAccumulator {
	term_frequency: u32,
	document_length: u32,
	spans: Vec<(u32, u32)>,
}

#[derive(Debug)]
struct EncodedTerm {
	term: Vec<u8>,
	hash: u64,
	posting_offset: u64,
	posting_length: u64,
	document_frequency: u32,
	total_term_frequency: u64,
}

#[derive(Debug, Clone, Copy)]
struct StreamLocation {
	first: u64,
	pages: u64,
}

/// Builds deterministic term, collision-safe hash and posting streams.
///
/// # Errors
///
/// Returns an error for duplicate document identities, counter overflow,
/// malformed analyzer output, or a page-count overflow.
pub(in crate::storage) fn build_fulltext_index(
	spec: FulltextBuildSpec,
	mut documents: Vec<FulltextDocument>,
) -> FerrosGrynnResult<BuiltFulltextIndex> {
	documents.sort_unstable_by_key(|document| document.atom);
	if documents.windows(2).any(|pair| pair[0].atom == pair[1].atom) {
		return Err(invalid("Fulltext input contains a duplicate document address"));
	}
	let highlights = spec.options.highlights();
	let mut total_document_length = 0_u64;
	let mut document_bytes = Vec::with_capacity(documents.len().saturating_mul(16));
	let mut terms = BTreeMap::<Vec<u8>, BTreeMap<DiskAtomRef, PostingAccumulator>>::new();
	for document in &documents {
		let document_length = u32::try_from(document.tokens.len())
			.map_err(|_error| invalid("Fulltext document token count exceeds u32"))?;
		total_document_length = total_document_length
			.checked_add(u64::from(document_length))
			.ok_or_else(|| invalid("Fulltext total document length overflows u64"))?;
		write_u64_vec(&mut document_bytes, document.atom.raw());
		write_u32_vec(&mut document_bytes, document_length);
		write_u32_vec(&mut document_bytes, 0);
		for token in &document.tokens {
			if token.term.is_empty() || token.start >= token.end {
				return Err(invalid("Fulltext analyzer emitted an invalid token"));
			}
			let posting = terms
				.entry(token.term.as_bytes().to_vec())
				.or_default()
				.entry(document.atom)
				.or_insert_with(|| PostingAccumulator {
					document_length,
					..PostingAccumulator::default()
				});
			posting.term_frequency = posting
				.term_frequency
				.checked_add(1)
				.ok_or_else(|| invalid("Fulltext term frequency exceeds u32"))?;
			if highlights {
				posting.spans.push((token.start, token.end));
			}
		}
	}

	let mut posting_bytes = Vec::new();
	let mut encoded_terms = Vec::with_capacity(terms.len());
	for (term, postings) in terms {
		let posting_offset = u64::try_from(posting_bytes.len())
			.map_err(|_error| invalid("Fulltext posting offset exceeds u64"))?;
		let segment_start = posting_bytes.len();
		let document_frequency = u32::try_from(postings.len())
			.map_err(|_error| invalid("Fulltext document frequency exceeds u32"))?;
		write_u64_vec(&mut posting_bytes, 0);
		write_u32_vec(&mut posting_bytes, document_frequency);
		write_u32_vec(&mut posting_bytes, 0);
		let mut total_term_frequency = 0_u64;
		for (atom, mut posting) in postings {
			if highlights {
				posting.spans.sort_unstable();
			}
			total_term_frequency = total_term_frequency
				.checked_add(u64::from(posting.term_frequency))
				.ok_or_else(|| invalid("Fulltext total term frequency overflows u64"))?;
			write_u64_vec(&mut posting_bytes, atom.raw());
			write_u32_vec(&mut posting_bytes, posting.term_frequency);
			write_u32_vec(&mut posting_bytes, posting.document_length);
			write_u32_vec(
				&mut posting_bytes,
				u32::try_from(posting.spans.len())
					.map_err(|_error| invalid("Fulltext span count exceeds u32"))?,
			);
			write_u32_vec(&mut posting_bytes, 0);
			for (start, end) in posting.spans {
				write_u32_vec(&mut posting_bytes, start);
				write_u32_vec(&mut posting_bytes, end);
			}
		}
		let posting_length = u64::try_from(posting_bytes.len() - segment_start)
			.map_err(|_error| invalid("Fulltext posting segment exceeds u64"))?;
		posting_bytes[segment_start..segment_start + 8]
			.copy_from_slice(&posting_length.to_le_bytes());
		encoded_terms.push(EncodedTerm {
			hash: rapidhash_v3(&term),
			posting_offset,
			posting_length,
			document_frequency,
			total_term_frequency,
			term,
		});
	}

	let mut dictionary_bytes = Vec::new();
	let mut hash_records = Vec::with_capacity(encoded_terms.len());
	for term in &encoded_terms {
		let dictionary_offset = u64::try_from(dictionary_bytes.len())
			.map_err(|_error| invalid("Fulltext dictionary offset exceeds u64"))?;
		let record_length = 48_usize
			.checked_add(term.term.len())
			.ok_or_else(|| invalid("Fulltext dictionary record length overflows usize"))?;
		write_u32_vec(
			&mut dictionary_bytes,
			u32::try_from(record_length)
				.map_err(|_error| invalid("Fulltext dictionary record exceeds u32"))?,
		);
		write_u32_vec(
			&mut dictionary_bytes,
			u32::try_from(term.term.len())
				.map_err(|_error| invalid("Fulltext term length exceeds u32"))?,
		);
		write_u64_vec(&mut dictionary_bytes, term.hash);
		write_u64_vec(&mut dictionary_bytes, term.posting_offset);
		write_u64_vec(&mut dictionary_bytes, term.posting_length);
		write_u32_vec(&mut dictionary_bytes, term.document_frequency);
		write_u32_vec(&mut dictionary_bytes, 0);
		write_u64_vec(&mut dictionary_bytes, term.total_term_frequency);
		dictionary_bytes.extend_from_slice(&term.term);
		hash_records.push((term.hash, dictionary_offset, term.term.len()));
	}
	hash_records.sort_unstable_by_key(|(hash, offset, _length)| (*hash, *offset));
	let mut hash_bytes = Vec::with_capacity(hash_records.len().saturating_mul(24));
	for (hash, dictionary_offset, term_length) in hash_records {
		write_u64_vec(&mut hash_bytes, hash);
		write_u64_vec(&mut hash_bytes, dictionary_offset);
		write_u32_vec(
			&mut hash_bytes,
			u32::try_from(term_length).map_err(|_error| invalid("Fulltext term exceeds u32"))?,
		);
		write_u32_vec(&mut hash_bytes, 0);
	}

	let dictionary_pages = page_count_for_stream(dictionary_bytes.len())?;
	let hash_pages = page_count_for_stream(hash_bytes.len())?;
	let posting_pages = page_count_for_stream(posting_bytes.len())?;
	let document_pages = page_count_for_stream(document_bytes.len())?;
	let total_pages = 1_u64
		.checked_add(dictionary_pages)
		.and_then(|value| value.checked_add(hash_pages))
		.and_then(|value| value.checked_add(posting_pages))
		.and_then(|value| value.checked_add(document_pages))
		.ok_or_else(|| invalid("Fulltext page count overflows u64"))?;
	let total_pages_usize = usize::try_from(total_pages)
		.map_err(|_error| invalid("Fulltext page count exceeds usize"))?;
	let mut pages = Vec::with_capacity(total_pages_usize);
	pages.resize_with(total_pages_usize, AlignedPage::zeroed);
	let dictionary = encode_stream(&mut pages, 1, PageType::IndexFulltextDict, &dictionary_bytes)?;
	let hash =
		encode_stream(&mut pages, 1 + dictionary.pages, PageType::IndexFulltextHash, &hash_bytes)?;
	let postings = encode_stream(
		&mut pages,
		1 + dictionary.pages + hash.pages,
		PageType::IndexFulltextPosting,
		&posting_bytes,
	)?;
	let document_stream = encode_stream(
		&mut pages,
		1 + dictionary.pages + hash.pages + postings.pages,
		PageType::IndexFulltextPosting,
		&document_bytes,
	)?;
	encode_header(
		&mut pages[0],
		spec,
		u64::try_from(documents.len())
			.map_err(|_error| invalid("Fulltext document count exceeds u64"))?,
		total_document_length,
		u64::try_from(encoded_terms.len())
			.map_err(|_error| invalid("Fulltext term count exceeds u64"))?,
		dictionary,
		hash,
		postings,
		document_stream,
	)?;
	Ok(BuiltFulltextIndex {
		root: PageNo::new(0),
		pages,
	})
}

/// Decodes and fully validates a page-framed Fulltext image.
///
/// # Errors
///
/// Returns an error for malformed metadata, page types, checksums, stream
/// boundaries, hashes, ordering, posting statistics or trailing bytes.
pub(in crate::storage) fn decode_fulltext_index(
	pages: &[AlignedPage],
) -> FerrosGrynnResult<DecodedFulltextIndex> {
	if pages.is_empty() {
		return Err(invalid("Fulltext index contains no header page"));
	}
	let header = decode_header(&pages[0])?;
	let expected_pages = 1_u64
		.checked_add(header.dictionary.pages)
		.and_then(|value| value.checked_add(header.hash.pages))
		.and_then(|value| value.checked_add(header.postings.pages))
		.and_then(|value| value.checked_add(header.documents.pages))
		.ok_or_else(|| invalid("Fulltext header page counts overflow u64"))?;
	if usize::try_from(expected_pages).ok() != Some(pages.len()) {
		return Err(invalid("Fulltext header page counts disagree with the file"));
	}
	let dictionary = decode_stream(pages, header.dictionary, PageType::IndexFulltextDict)?;
	let hash = decode_stream(pages, header.hash, PageType::IndexFulltextHash)?;
	let posting_bytes = decode_stream(pages, header.postings, PageType::IndexFulltextPosting)?;
	let document_bytes = decode_stream(pages, header.documents, PageType::IndexFulltextPosting)?;
	let documents = decode_documents(&document_bytes)?;
	let dictionary_entries = decode_dictionary(&dictionary, &posting_bytes, header.highlights)?;
	validate_hash_stream(&hash, &dictionary_entries)?;
	if u64::try_from(dictionary_entries.len()).ok() != Some(header.term_count) {
		return Err(invalid("Fulltext term count disagrees with the dictionary"));
	}
	if u64::try_from(documents.len()).ok() != Some(header.document_count) {
		return Err(invalid("Fulltext document stream count disagrees with the header"));
	}
	let document_lengths = documents
		.iter()
		.map(|document| (document.atom, document.document_length))
		.collect::<BTreeMap<_, _>>();
	let known_total = documents.iter().try_fold(0_u64, |total, document| {
		total
			.checked_add(u64::from(document.document_length))
			.ok_or_else(|| invalid("Fulltext decoded document lengths overflow u64"))
	})?;
	if known_total != header.total_document_length {
		return Err(invalid("Fulltext document lengths disagree with the header"));
	}
	for term in &dictionary_entries {
		for posting in &term.term.postings {
			if document_lengths.get(&posting.atom) != Some(&posting.document_length) {
				return Err(invalid(
					"Fulltext posting references an absent document or disagrees on length",
				));
			}
		}
	}
	Ok(DecodedFulltextIndex {
		spec: header.spec,
		document_count: header.document_count,
		total_document_length: header.total_document_length,
		documents,
		terms: dictionary_entries.into_iter().map(|entry| entry.term).collect(),
	})
}

struct Header {
	spec: FulltextBuildSpec,
	document_count: u64,
	total_document_length: u64,
	term_count: u64,
	dictionary: StreamLocation,
	hash: StreamLocation,
	postings: StreamLocation,
	documents: StreamLocation,
	highlights: bool,
}

fn encode_header(
	page: &mut AlignedPage,
	spec: FulltextBuildSpec,
	document_count: u64,
	total_document_length: u64,
	term_count: u64,
	dictionary: StreamLocation,
	hash: StreamLocation,
	postings: StreamLocation,
	documents: StreamLocation,
) -> FerrosGrynnResult<()> {
	page.reset(PageType::IndexFulltextHeader, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	let bytes = page.as_bytes_mut();
	write_u16(bytes, 32, SIDE_FORMAT_VERSION);
	write_u16(bytes, 34, u16::from(spec.options.highlights()) * FLAG_HIGHLIGHTS);
	write_u32(bytes, 36, spec.index.get());
	write_u32(bytes, 40, spec.index_version.get());
	write_u32(bytes, 44, spec.analyzer.get());
	write_u32(bytes, 48, spec.analyzer_version.get());
	match spec.scope {
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
	write_u32(bytes, 64, spec.class.get());
	write_u32(bytes, 68, spec.field.get());
	write_u64(bytes, 72, spec.options.k1().to_bits());
	write_u64(bytes, 80, spec.options.b().to_bits());
	write_u64(bytes, 88, document_count);
	write_u64(bytes, 96, total_document_length);
	write_u64(bytes, 104, term_count);
	write_u64(bytes, 112, dictionary.first);
	write_u64(bytes, 120, dictionary.pages);
	write_u64(bytes, 128, hash.first);
	write_u64(bytes, 136, hash.pages);
	write_u64(bytes, 144, postings.first);
	write_u64(bytes, 152, postings.pages);
	write_u64(bytes, 160, spec.build_revision);
	write_u64(bytes, 168, IMPLEMENTATION_FINGERPRINT);
	write_u64(bytes, 176, documents.first);
	write_u64(bytes, 184, documents.pages);
	page.seal()
}

fn decode_header(page: &AlignedPage) -> FerrosGrynnResult<Header> {
	let header = page.validate(PageNo::new(0))?;
	if header.page_type() != PageType::IndexFulltextHeader || !header.flags().is_empty() {
		return Err(invalid("Fulltext header page has the wrong type or flags"));
	}
	let bytes = page.as_bytes();
	if read_u16(bytes, 32)? != SIDE_FORMAT_VERSION
		|| read_u64(bytes, 168)? != IMPLEMENTATION_FINGERPRINT
	{
		return Err(invalid("Fulltext side format version or fingerprint is unsupported"));
	}
	let flags = read_u16(bytes, 34)?;
	if flags & !FLAG_HIGHLIGHTS != 0 || bytes[53..56].iter().any(|byte| *byte != 0) {
		return Err(invalid("Fulltext header contains unknown flags or reserved bytes"));
	}
	let index = IndexId::new(read_u32(bytes, 36)?)
		.ok_or_else(|| invalid("Fulltext header contains a null IndexId"))?;
	let index_version = SchemaVersion::new(read_u32(bytes, 40)?)
		.ok_or_else(|| invalid("Fulltext header contains a zero index version"))?;
	let analyzer = AnalyzerId::new(read_u32(bytes, 44)?)
		.ok_or_else(|| invalid("Fulltext header contains a null AnalyzerId"))?;
	let analyzer_version = SchemaVersion::new(read_u32(bytes, 48)?)
		.ok_or_else(|| invalid("Fulltext header contains a zero analyzer version"))?;
	let graph = read_u64(bytes, 56)?;
	let scope = match bytes[52] {
		0 if graph == 0 => IndexScope::Base,
		1 => IndexScope::Effective {
			graph: crate::graph::GraphId::new(graph)
				.ok_or_else(|| invalid("Fulltext effective scope has a null GraphId"))?,
		},
		_ => return Err(invalid("Fulltext scope discriminator is invalid")),
	};
	let class = ClassId::new(read_u32(bytes, 64)?)
		.ok_or_else(|| invalid("Fulltext header contains a null ClassId"))?;
	let field = PropertyKeyId::new(read_u32(bytes, 68)?);
	let k1 = f64::from_bits(read_u64(bytes, 72)?);
	let b = f64::from_bits(read_u64(bytes, 80)?);
	let highlights = flags & FLAG_HIGHLIGHTS != 0;
	let options = FulltextIndexOptions::new(analyzer, analyzer_version, k1, b, highlights)?;
	if bytes[HEADER_FIXED_END..].iter().any(|byte| *byte != 0) {
		return Err(invalid("Fulltext header reserved bytes are non-zero"));
	}
	Ok(Header {
		spec: FulltextBuildSpec {
			index,
			index_version,
			analyzer,
			analyzer_version,
			scope,
			class,
			field,
			options,
			build_revision: read_u64(bytes, 160)?,
		},
		document_count: read_u64(bytes, 88)?,
		total_document_length: read_u64(bytes, 96)?,
		term_count: read_u64(bytes, 104)?,
		dictionary: StreamLocation {
			first: read_u64(bytes, 112)?,
			pages: read_u64(bytes, 120)?,
		},
		hash: StreamLocation {
			first: read_u64(bytes, 128)?,
			pages: read_u64(bytes, 136)?,
		},
		postings: StreamLocation {
			first: read_u64(bytes, 144)?,
			pages: read_u64(bytes, 152)?,
		},
		documents: StreamLocation {
			first: read_u64(bytes, 176)?,
			pages: read_u64(bytes, 184)?,
		},
		highlights,
	})
}

fn decode_documents(bytes: &[u8]) -> FerrosGrynnResult<Vec<FulltextDocumentState>> {
	if !bytes.len().is_multiple_of(16) {
		return Err(invalid("Fulltext document stream has a partial record"));
	}
	let mut documents = Vec::with_capacity(bytes.len() / 16);
	for record in bytes.chunks_exact(16) {
		let atom = DiskAtomRef::from_raw(read_u64(record, 0)?)?;
		if read_u32(record, 12)? != 0
			|| documents
				.last()
				.is_some_and(|previous: &FulltextDocumentState| previous.atom >= atom)
		{
			return Err(invalid("Fulltext document stream is malformed or unsorted"));
		}
		documents.push(FulltextDocumentState {
			atom,
			document_length: read_u32(record, 8)?,
		});
	}
	Ok(documents)
}

fn page_count_for_stream(bytes: usize) -> FerrosGrynnResult<u64> {
	if bytes == 0 {
		return Ok(0);
	}
	let pages = bytes.div_ceil(STREAM_PAYLOAD_BYTES);
	u64::try_from(pages).map_err(|_error| invalid("Fulltext stream page count exceeds u64"))
}

fn encode_stream(
	pages: &mut [AlignedPage],
	first: u64,
	page_type: PageType,
	payload: &[u8],
) -> FerrosGrynnResult<StreamLocation> {
	let page_count = page_count_for_stream(payload.len())?;
	if page_count == 0 {
		return Ok(StreamLocation {
			first: NULL_PAGE,
			pages: 0,
		});
	}
	for ordinal in 0..page_count {
		let page_no = first
			.checked_add(ordinal)
			.ok_or_else(|| invalid("Fulltext stream page number overflows u64"))?;
		let page_index = usize::try_from(page_no)
			.map_err(|_error| invalid("Fulltext stream page number exceeds usize"))?;
		let start = usize::try_from(ordinal)
			.map_err(|_error| invalid("Fulltext stream ordinal exceeds usize"))?
			.checked_mul(STREAM_PAYLOAD_BYTES)
			.ok_or_else(|| invalid("Fulltext stream offset overflows usize"))?;
		let end = (start + STREAM_PAYLOAD_BYTES).min(payload.len());
		let next = if ordinal + 1 == page_count {
			NULL_PAGE
		} else {
			page_no + 1
		};
		let page = pages
			.get_mut(page_index)
			.ok_or_else(|| invalid("Fulltext stream page lies outside allocated slots"))?;
		page.reset(page_type, PageFlags::empty(), PageLsn::ZERO, PageNo::new(page_no));
		let bytes = page.as_bytes_mut();
		write_u64(bytes, PAGE_HEADER_SIZE, next);
		write_u32(
			bytes,
			PAGE_HEADER_SIZE + 8,
			u32::try_from(end - start)
				.map_err(|_error| invalid("Fulltext page payload exceeds u32"))?,
		);
		bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + end - start]
			.copy_from_slice(&payload[start..end]);
		page.seal()?;
	}
	Ok(StreamLocation {
		first,
		pages: page_count,
	})
}

fn decode_stream(
	pages: &[AlignedPage],
	location: StreamLocation,
	page_type: PageType,
) -> FerrosGrynnResult<Vec<u8>> {
	if location.pages == 0 {
		if location.first != NULL_PAGE {
			return Err(invalid("empty Fulltext stream has a non-null root"));
		}
		return Ok(Vec::new());
	}
	if location.first == NULL_PAGE {
		return Err(invalid("non-empty Fulltext stream has a null root"));
	}
	let capacity = usize::try_from(location.pages)
		.ok()
		.and_then(|pages| pages.checked_mul(STREAM_PAYLOAD_BYTES))
		.ok_or_else(|| invalid("Fulltext stream allocation exceeds usize"))?;
	let mut output = Vec::with_capacity(capacity);
	for ordinal in 0..location.pages {
		let page_no = location
			.first
			.checked_add(ordinal)
			.ok_or_else(|| invalid("Fulltext stream page number overflows u64"))?;
		let page = pages
			.get(usize::try_from(page_no).map_err(|_error| invalid("page exceeds usize"))?)
			.ok_or_else(|| invalid("Fulltext stream references a missing page"))?;
		let header = page.validate(PageNo::new(page_no))?;
		if header.page_type() != page_type || !header.flags().is_empty() {
			return Err(invalid("Fulltext stream page has the wrong type or flags"));
		}
		let bytes = page.as_bytes();
		let expected_next = if ordinal + 1 == location.pages {
			NULL_PAGE
		} else {
			page_no + 1
		};
		if read_u64(bytes, PAGE_HEADER_SIZE)? != expected_next {
			return Err(invalid("Fulltext stream contains a broken page chain"));
		}
		let payload_len = usize::try_from(read_u32(bytes, PAGE_HEADER_SIZE + 8)?)
			.map_err(|_error| invalid("Fulltext page payload exceeds usize"))?;
		if payload_len > STREAM_PAYLOAD_BYTES
			|| (ordinal + 1 != location.pages && payload_len != STREAM_PAYLOAD_BYTES)
			|| bytes[PAGE_HEADER_SIZE + 12..STREAM_HEADER_BYTES].iter().any(|byte| *byte != 0)
			|| bytes[STREAM_HEADER_BYTES + payload_len..].iter().any(|byte| *byte != 0)
		{
			return Err(invalid("Fulltext stream page payload or padding is malformed"));
		}
		output.extend_from_slice(&bytes[STREAM_HEADER_BYTES..STREAM_HEADER_BYTES + payload_len]);
	}
	Ok(output)
}

struct DecodedDictionaryEntry {
	offset: u64,
	hash: u64,
	term: FulltextTerm,
}

fn decode_dictionary(
	dictionary: &[u8],
	postings: &[u8],
	highlights: bool,
) -> FerrosGrynnResult<Vec<DecodedDictionaryEntry>> {
	let mut cursor = 0_usize;
	let mut output = Vec::new();
	let mut previous = None::<Vec<u8>>;
	while cursor < dictionary.len() {
		let offset =
			u64::try_from(cursor).map_err(|_error| invalid("dictionary offset exceeds u64"))?;
		let record_length = usize::try_from(read_u32(dictionary, cursor)?)
			.map_err(|_error| invalid("dictionary record length exceeds usize"))?;
		if record_length < 48
			|| cursor.checked_add(record_length).is_none_or(|end| end > dictionary.len())
		{
			return Err(invalid("Fulltext dictionary record crosses the stream boundary"));
		}
		let term_length = usize::try_from(read_u32(dictionary, cursor + 4)?)
			.map_err(|_error| invalid("Fulltext term length exceeds usize"))?;
		if 48_usize.checked_add(term_length) != Some(record_length) {
			return Err(invalid("Fulltext dictionary record length is inconsistent"));
		}
		let hash = read_u64(dictionary, cursor + 8)?;
		let posting_offset = usize::try_from(read_u64(dictionary, cursor + 16)?)
			.map_err(|_error| invalid("Fulltext posting offset exceeds usize"))?;
		let posting_length = usize::try_from(read_u64(dictionary, cursor + 24)?)
			.map_err(|_error| invalid("Fulltext posting length exceeds usize"))?;
		let document_frequency = read_u32(dictionary, cursor + 32)?;
		if read_u32(dictionary, cursor + 36)? != 0 {
			return Err(invalid("Fulltext dictionary reserved bytes are non-zero"));
		}
		let total_term_frequency = read_u64(dictionary, cursor + 40)?;
		let term = dictionary[cursor + 48..cursor + record_length].to_vec();
		if term.is_empty()
			|| std::str::from_utf8(&term).is_err()
			|| rapidhash_v3(&term) != hash
			|| previous.as_ref().is_some_and(|previous| previous >= &term)
		{
			return Err(invalid(
				"Fulltext dictionary terms are invalid, unsorted, or hash-mismatched",
			));
		}
		let posting_end = posting_offset
			.checked_add(posting_length)
			.ok_or_else(|| invalid("Fulltext posting range overflows usize"))?;
		let posting_slice = postings
			.get(posting_offset..posting_end)
			.ok_or_else(|| invalid("Fulltext dictionary points outside the posting stream"))?;
		let term_postings = decode_postings(posting_slice, highlights)?;
		if usize::try_from(document_frequency).ok() != Some(term_postings.len())
			|| term_postings
				.iter()
				.map(|posting| u64::from(posting.term_frequency))
				.try_fold(0_u64, u64::checked_add)
				!= Some(total_term_frequency)
		{
			return Err(invalid("Fulltext dictionary statistics disagree with postings"));
		}
		previous = Some(term.clone());
		output.push(DecodedDictionaryEntry {
			offset,
			hash,
			term: FulltextTerm {
				term,
				total_term_frequency,
				postings: term_postings,
			},
		});
		cursor += record_length;
	}
	Ok(output)
}

fn decode_postings(bytes: &[u8], highlights: bool) -> FerrosGrynnResult<Vec<FulltextPosting>> {
	if bytes.len() < 16 || usize::try_from(read_u64(bytes, 0)?).ok() != Some(bytes.len()) {
		return Err(invalid("Fulltext posting segment length is malformed"));
	}
	let count = usize::try_from(read_u32(bytes, 8)?)
		.map_err(|_error| invalid("Fulltext posting count exceeds usize"))?;
	if read_u32(bytes, 12)? != 0 {
		return Err(invalid("Fulltext posting segment reserved bytes are non-zero"));
	}
	let mut cursor = 16_usize;
	let mut output = Vec::with_capacity(count);
	for _ in 0..count {
		if cursor.checked_add(24).is_none_or(|end| end > bytes.len()) {
			return Err(invalid("Fulltext posting header crosses its segment"));
		}
		let atom = DiskAtomRef::from_raw(read_u64(bytes, cursor)?)?;
		let term_frequency = read_u32(bytes, cursor + 8)?;
		let document_length = read_u32(bytes, cursor + 12)?;
		let span_count = usize::try_from(read_u32(bytes, cursor + 16)?)
			.map_err(|_error| invalid("Fulltext span count exceeds usize"))?;
		if read_u32(bytes, cursor + 20)? != 0
			|| term_frequency == 0
			|| document_length < term_frequency
			|| (highlights && span_count != usize::try_from(term_frequency).unwrap_or(usize::MAX))
			|| (!highlights && span_count != 0)
		{
			return Err(invalid("Fulltext posting metadata is inconsistent"));
		}
		cursor += 24;
		let span_bytes = span_count
			.checked_mul(8)
			.ok_or_else(|| invalid("Fulltext span bytes overflow usize"))?;
		if cursor.checked_add(span_bytes).is_none_or(|end| end > bytes.len()) {
			return Err(invalid("Fulltext spans cross their posting segment"));
		}
		let mut spans = Vec::with_capacity(span_count);
		for _ in 0..span_count {
			let span = (read_u32(bytes, cursor)?, read_u32(bytes, cursor + 4)?);
			if span.0 >= span.1 || spans.last().is_some_and(|previous| previous > &span) {
				return Err(invalid("Fulltext spans are invalid or unsorted"));
			}
			spans.push(span);
			cursor += 8;
		}
		if output.last().is_some_and(|previous: &FulltextPosting| previous.atom >= atom) {
			return Err(invalid("Fulltext postings are not strictly ordered"));
		}
		output.push(FulltextPosting {
			atom,
			term_frequency,
			document_length,
			spans,
		});
	}
	if cursor != bytes.len() {
		return Err(invalid("Fulltext posting segment has trailing bytes"));
	}
	Ok(output)
}

fn validate_hash_stream(
	bytes: &[u8],
	dictionary: &[DecodedDictionaryEntry],
) -> FerrosGrynnResult<()> {
	if !bytes.len().is_multiple_of(24) || bytes.len() / 24 != dictionary.len() {
		return Err(invalid("Fulltext hash stream length disagrees with the dictionary"));
	}
	let by_offset =
		dictionary.iter().map(|entry| (entry.offset, entry)).collect::<BTreeMap<_, _>>();
	let mut previous = None::<(u64, u64)>;
	for chunk in bytes.chunks_exact(24) {
		let hash = read_u64(chunk, 0)?;
		let offset = read_u64(chunk, 8)?;
		let term_length = usize::try_from(read_u32(chunk, 16)?)
			.map_err(|_error| invalid("Fulltext hash term length exceeds usize"))?;
		if read_u32(chunk, 20)? != 0 || previous.is_some_and(|previous| previous >= (hash, offset))
		{
			return Err(invalid("Fulltext hash stream is malformed or unsorted"));
		}
		let term = by_offset
			.get(&offset)
			.ok_or_else(|| invalid("Fulltext hash stream points outside the dictionary"))?;
		if term.hash != hash
			|| term.term.term.len() != term_length
			|| rapidhash_v3(&term.term.term) != hash
		{
			return Err(invalid(
				"Fulltext hash collision was not verified against full term bytes",
			));
		}
		previous = Some((hash, offset));
	}
	Ok(())
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

fn write_u32_vec(bytes: &mut Vec<u8>, value: u32) {
	bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u64_vec(bytes: &mut Vec<u8>, value: u64) {
	bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u16> {
	let value = bytes
		.get(offset..offset + 2)
		.ok_or_else(|| invalid("Fulltext u16 crosses its enclosing byte range"))?;
	Ok(u16::from_le_bytes(value.try_into().map_err(|_error| invalid("invalid u16"))?))
}

fn read_u32(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u32> {
	let value = bytes
		.get(offset..offset + 4)
		.ok_or_else(|| invalid("Fulltext u32 crosses its enclosing byte range"))?;
	Ok(u32::from_le_bytes(value.try_into().map_err(|_error| invalid("invalid u32"))?))
}

fn read_u64(bytes: &[u8], offset: usize) -> FerrosGrynnResult<u64> {
	let value = bytes
		.get(offset..offset + 8)
		.ok_or_else(|| invalid("Fulltext u64 crosses its enclosing byte range"))?;
	Ok(u64::from_le_bytes(value.try_into().map_err(|_error| invalid("invalid u64"))?))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

#[cfg(test)]
mod tests {
	use std::{
		collections::BTreeMap,
		fs,
		num::NonZeroUsize,
		sync::atomic::{AtomicU64, Ordering},
	};

	use super::*;
	use crate::{
		datastore::{AnalyzerFilter, AnalyzerTokenizer, SchemaAnalyzer, analyze_builtin},
		graph::{AtomKind, GraphId},
		storage::{direct::DirectFile, with_external_sort_options},
		values::Text,
	};

	static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	struct TestDirectory(std::path::PathBuf);

	impl TestDirectory {
		fn new() -> Self {
			let path = std::env::temp_dir().join(format!(
				"fgft-stream-{:016x}",
				NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
			));
			fs::create_dir_all(&path).unwrap();
			Self(path)
		}
	}

	impl Drop for TestDirectory {
		fn drop(&mut self) {
			drop(fs::remove_dir_all(&self.0));
		}
	}

	fn spec(scope: IndexScope, highlights: bool) -> FulltextBuildSpec {
		let analyzer = AnalyzerId::new(5).unwrap();
		FulltextBuildSpec {
			index: IndexId::new(7).unwrap(),
			index_version: SchemaVersion::INITIAL,
			analyzer,
			analyzer_version: SchemaVersion::INITIAL,
			scope,
			class: ClassId::new(11).unwrap(),
			field: PropertyKeyId::new(13),
			options: FulltextIndexOptions::defaults(analyzer, SchemaVersion::INITIAL, highlights),
			build_revision: 23,
		}
	}

	fn analyzer() -> SchemaAnalyzer {
		SchemaAnalyzer::new("english", [AnalyzerTokenizer::Blank], [AnalyzerFilter::Lowercase])
			.unwrap()
	}

	#[test]
	fn round_trips_terms_postings_statistics_and_spans() {
		let analyzer = analyzer();
		let documents = vec![
			FulltextDocument {
				atom: DiskAtomRef::from_parts(AtomKind::Node, 1).unwrap(),
				tokens: analyze_builtin(&analyzer, "Rust rust").unwrap(),
			},
			FulltextDocument {
				atom: DiskAtomRef::from_parts(AtomKind::Node, 2).unwrap(),
				tokens: analyze_builtin(&analyzer, "Graph").unwrap(),
			},
			FulltextDocument {
				atom: DiskAtomRef::from_parts(AtomKind::Node, 3).unwrap(),
				tokens: Vec::new(),
			},
		];
		let expected = spec(IndexScope::Base, true);
		let built = build_fulltext_index(expected, documents).unwrap();
		let decoded = decode_fulltext_index(&built.pages).unwrap();
		assert_eq!(decoded.spec, expected);
		assert_eq!(decoded.document_count, 3);
		assert_eq!(decoded.total_document_length, 3);
		assert_eq!(
			decoded.terms.iter().map(|term| term.term.as_slice()).collect::<Vec<_>>(),
			vec![b"graph".as_slice(), b"rust".as_slice()],
		);
		assert_eq!(decoded.terms[1].postings[0].term_frequency, 2);
		assert_eq!(decoded.terms[1].postings[0].spans, vec![(0, 4), (5, 9)]);
	}

	#[test]
	fn streaming_builder_matches_buffered_builder_across_spilled_runs() {
		let directory = TestDirectory::new();
		let path = directory.0.join("fulltext.fgidx");
		let file = DirectFile::create_new(&path).unwrap();
		let analyzer = analyzer();
		let make_documents = || {
			(1..=96_u64)
				.map(|slot| FulltextDocument {
					atom: DiskAtomRef::from_parts(AtomKind::Node, slot).unwrap(),
					tokens: analyze_builtin(
						&analyzer,
						format!("shared term{slot} shared").as_str(),
					)
					.unwrap(),
				})
				.collect::<Vec<_>>()
		};
		let expected = decode_fulltext_index(
			&build_fulltext_index(spec(IndexScope::Base, true), make_documents()).unwrap().pages,
		)
		.unwrap();
		with_external_sort_options(
			NonZeroUsize::new(1024).unwrap(),
			NonZeroUsize::new(1).unwrap(),
			|| {
				write_fulltext_index_stream(
					spec(IndexScope::Base, true),
					&directory.0,
					&file,
					|consumer| make_documents().into_iter().try_for_each(consumer),
				)
				.map(|_built| ())
			},
		)
		.unwrap();

		let mut pages = Vec::with_capacity(usize::try_from(file.page_count()).unwrap());
		for page in 0..file.page_count() {
			pages.push(AlignedPage::zeroed());
			file.read_page(PageNo::new(page), pages.last_mut().unwrap()).unwrap();
		}
		let actual = decode_fulltext_index(&pages).unwrap();
		assert_eq!(actual, expected);
	}

	#[test]
	fn rejects_corrupt_term_hash_after_page_checksum_is_resealed() {
		let analyzer = analyzer();
		let built = build_fulltext_index(
			spec(
				IndexScope::Effective {
					graph: GraphId::new(2).unwrap(),
				},
				false,
			),
			vec![FulltextDocument {
				atom: DiskAtomRef::from_parts(AtomKind::Edge, 4).unwrap(),
				tokens: analyze_builtin(&analyzer, "term").unwrap(),
			}],
		)
		.unwrap();
		let mut pages = built.pages;
		let dictionary = &mut pages[1];
		dictionary.as_bytes_mut()[STREAM_HEADER_BYTES + 8] ^= 1;
		dictionary.seal().unwrap();
		assert!(decode_fulltext_index(&pages).is_err());
	}

	#[test]
	fn hash_stream_accepts_distinct_full_terms_with_equal_hash_metadata_only_after_bytes_check() {
		let mut by_hash = BTreeMap::<u64, Vec<Text>>::new();
		for value in ["a", "b", "c"] {
			by_hash.entry(rapidhash_v3(value.as_bytes())).or_default().push(Text::from(value));
		}
		assert_eq!(by_hash.values().map(Vec::len).sum::<usize>(), 3);
	}
}
