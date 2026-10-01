//! Bounded external-sort and page-at-a-time Fulltext generation.

use std::path::Path;

use rapidhash::v3::rapidhash_v3;

use super::{FulltextBuildSpec, FulltextDocument, StreamLocation, encode_header};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		direct::DirectFile,
		external_sort_run_budget,
		graph_index::{ExternalIndexSorter, StreamedIndex},
		immutable_stream::{ImmutableStreamLocation, ImmutableStreamWriter},
		page::{AlignedPage, PageFlags, PageLsn, PageNo, PageType},
	},
	values::storage::DiskAtomRef,
};

const DOCUMENT_RECORD_PREFIX: u8 = 0;
const TOKEN_RECORD_PREFIX: u8 = 1;
const DOCUMENT_RECORD_BYTES: usize = 16;
const POSTING_SEGMENT_HEADER_BYTES: usize = 16;
const POSTING_HEADER_BYTES: usize = 24;
const DICTIONARY_FIXED_BYTES: usize = 48;

/// Builds one immutable Fulltext side-file without retaining source rows,
/// posting groups, or encoded pages in memory.
///
/// `feed` is invoked exactly once and may evaluate analyzer UDFs while passing
/// one completed source document at a time to the supplied consumer.
///
/// # Errors
///
/// Returns an error for malformed analyzer output, duplicate source identities,
/// external-sort/resource overflow, or failed page I/O.
pub(in crate::storage) fn write_fulltext_index_stream(
	spec: FulltextBuildSpec,
	database_root: &Path,
	file: &DirectFile,
	feed: impl FnOnce(
		&mut dyn FnMut(FulltextDocument) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<StreamedIndex> {
	if file.page_count() != 0 {
		return Err(invalid("Fulltext streaming target is not empty"));
	}
	let mut header = Box::new(AlignedPage::zeroed());
	header.reset(PageType::IndexFulltextHeader, PageFlags::empty(), PageLsn::ZERO, PageNo::new(0));
	header.seal()?;
	file.write_page(PageNo::new(0), &header)?;

	let budget = external_sort_run_budget();
	if budget < 2 {
		return Err(FerrosGrynnError::invalid_argument(
			"Fulltext external-sort budget must be at least two bytes",
		));
	}
	let auxiliary_budget = (budget / 4).max(1);
	let primary_budget = budget - auxiliary_budget;
	let mut records = ExternalIndexSorter::with_budget(database_root, primary_budget);
	let mut document_count = 0_u64;
	let mut total_document_length = 0_u64;
	feed(&mut |document| {
		let document_length = u32::try_from(document.tokens.len())
			.map_err(|_error| invalid("Fulltext document token count exceeds u32"))?;
		document_count = document_count
			.checked_add(1)
			.ok_or_else(|| invalid("Fulltext document count overflows u64"))?;
		total_document_length = total_document_length
			.checked_add(u64::from(document_length))
			.ok_or_else(|| invalid("Fulltext total document length overflows u64"))?;
		records.push_raw(document_key(document.atom), u64::from(document_length))?;
		for token in document.tokens {
			if token.term.is_empty() || token.start >= token.end {
				return Err(invalid("Fulltext analyzer emitted an invalid token"));
			}
			records.push_raw(
				token_key(token.term.as_bytes(), document.atom, token.start, token.end)?,
				u64::from(document_length),
			)?;
		}
		Ok(())
	})?;
	let records = records.finish()?;
	let mut metadata = ExternalIndexSorter::with_budget(database_root, auxiliary_budget);
	let mut document_writer =
		Some(ImmutableStreamWriter::new(file, PageType::IndexFulltextPosting));
	let mut document_location = None;
	let mut previous_document = None;
	let mut postings = None::<PostingAssembler<'_>>;
	records.visit_raw(|key, address| match key.first().copied() {
		Some(DOCUMENT_RECORD_PREFIX) => {
			if postings.is_some() || key.len() != 9 {
				return Err(invalid("Fulltext document sort record is malformed"));
			}
			let atom =
				DiskAtomRef::from_raw(u64::from_be_bytes(key[1..9].try_into().map_err(
					|_error| invalid("Fulltext document address has the wrong width"),
				)?))?;
			if previous_document.is_some_and(|previous| previous >= atom) {
				return Err(invalid("Fulltext input contains a duplicate document address"));
			}
			let length = u32::try_from(address)
				.map_err(|_error| invalid("Fulltext document length exceeds u32"))?;
			let mut encoded = [0_u8; DOCUMENT_RECORD_BYTES];
			encoded[..8].copy_from_slice(&atom.raw().to_le_bytes());
			encoded[8..12].copy_from_slice(&length.to_le_bytes());
			document_writer
				.as_mut()
				.ok_or_else(|| invalid("Fulltext document stream was already closed"))?
				.write_all(&encoded)?;
			previous_document = Some(atom);
			Ok(())
		}
		Some(TOKEN_RECORD_PREFIX) => {
			if postings.is_none() {
				document_location = Some(
					document_writer
						.take()
						.ok_or_else(|| invalid("Fulltext document stream is absent"))?
						.finish()?,
				);
				postings = Some(PostingAssembler::new(file, spec.options.highlights()));
			}
			let token = decode_token_key(key, address)?;
			postings
				.as_mut()
				.ok_or_else(|| invalid("Fulltext posting assembler is absent"))?
				.push(token, &mut metadata)
		}
		_ => Err(invalid("Fulltext external-sort record prefix is invalid")),
	})?;
	let document_location = match document_location {
		Some(location) => location,
		None => document_writer
			.take()
			.ok_or_else(|| invalid("Fulltext document stream is absent"))?
			.finish()?,
	};
	let (posting_location, term_count) = match postings {
		Some(postings) => postings.finish(&mut metadata)?,
		None => (ImmutableStreamWriter::new(file, PageType::IndexFulltextPosting).finish()?, 0),
	};
	drop(records);

	let metadata = metadata.finish()?;
	let mut dictionary = ImmutableStreamWriter::new(file, PageType::IndexFulltextDict);
	let mut hashes = ExternalIndexSorter::with_budget(database_root, primary_budget);
	let mut previous_term = None::<Vec<u8>>;
	metadata.visit_raw(|key, hash| {
		let entry = decode_dictionary_metadata(key)?;
		if previous_term.as_ref().is_some_and(|previous| previous >= &entry.term) {
			return Err(invalid("Fulltext dictionary metadata is not strictly ordered"));
		}
		let dictionary_offset = dictionary.position();
		let record_length = DICTIONARY_FIXED_BYTES
			.checked_add(entry.term.len())
			.ok_or_else(|| invalid("Fulltext dictionary record length overflows usize"))?;
		let mut fixed = [0_u8; DICTIONARY_FIXED_BYTES];
		fixed[..4].copy_from_slice(
			&u32::try_from(record_length)
				.map_err(|_error| invalid("Fulltext dictionary record exceeds u32"))?
				.to_le_bytes(),
		);
		fixed[4..8].copy_from_slice(
			&u32::try_from(entry.term.len())
				.map_err(|_error| invalid("Fulltext term length exceeds u32"))?
				.to_le_bytes(),
		);
		fixed[8..16].copy_from_slice(&hash.to_le_bytes());
		fixed[16..24].copy_from_slice(&entry.posting_offset.to_le_bytes());
		fixed[24..32].copy_from_slice(&entry.posting_length.to_le_bytes());
		fixed[32..36].copy_from_slice(&entry.document_frequency.to_le_bytes());
		fixed[40..48].copy_from_slice(&entry.total_term_frequency.to_le_bytes());
		dictionary.write_all(&fixed)?;
		dictionary.write_all(&entry.term)?;
		let mut hash_key = Vec::with_capacity(20);
		hash_key.extend_from_slice(&hash.to_be_bytes());
		hash_key.extend_from_slice(&dictionary_offset.to_be_bytes());
		hash_key.extend_from_slice(
			&u32::try_from(entry.term.len())
				.map_err(|_error| invalid("Fulltext term length exceeds u32"))?
				.to_be_bytes(),
		);
		hashes.push_raw(hash_key, 0)?;
		previous_term = Some(entry.term);
		Ok(())
	})?;
	let dictionary_location = dictionary.finish()?;
	drop(metadata);

	let hashes = hashes.finish()?;
	let mut hash_writer = ImmutableStreamWriter::new(file, PageType::IndexFulltextHash);
	let mut previous_hash = None;
	hashes.visit_raw(|key, _unused| {
		if key.len() != 20 {
			return Err(invalid("Fulltext hash sort record has the wrong width"));
		}
		let hash = u64::from_be_bytes(
			key[..8].try_into().map_err(|_error| invalid("Fulltext hash has the wrong width"))?,
		);
		let offset = u64::from_be_bytes(
			key[8..16]
				.try_into()
				.map_err(|_error| invalid("Fulltext dictionary offset has the wrong width"))?,
		);
		let term_length = u32::from_be_bytes(
			key[16..20]
				.try_into()
				.map_err(|_error| invalid("Fulltext hash term length has the wrong width"))?,
		);
		if previous_hash.is_some_and(|previous| previous >= (hash, offset)) {
			return Err(invalid("Fulltext hash records are not strictly ordered"));
		}
		let mut encoded = [0_u8; 24];
		encoded[..8].copy_from_slice(&hash.to_le_bytes());
		encoded[8..16].copy_from_slice(&offset.to_le_bytes());
		encoded[16..20].copy_from_slice(&term_length.to_le_bytes());
		hash_writer.write_all(&encoded)?;
		previous_hash = Some((hash, offset));
		Ok(())
	})?;
	let hash_location = hash_writer.finish()?;

	encode_header(
		&mut header,
		spec,
		document_count,
		total_document_length,
		term_count,
		location(dictionary_location),
		location(hash_location),
		location(posting_location),
		location(document_location),
	)?;
	file.write_page(PageNo::new(0), &header)?;
	Ok(StreamedIndex {
		root: PageNo::new(0),
		page_count: file.page_count(),
	})
}

struct TokenRecord {
	term: Vec<u8>,
	atom: DiskAtomRef,
	document_length: u32,
	start: u32,
	end: u32,
}

struct DictionaryMetadata {
	term: Vec<u8>,
	posting_offset: u64,
	posting_length: u64,
	document_frequency: u32,
	total_term_frequency: u64,
}

struct PostingAssembler<'a> {
	writer: ImmutableStreamWriter<'a>,
	highlights: bool,
	term: Vec<u8>,
	term_offset: u64,
	term_document_frequency: u32,
	term_frequency: u64,
	atom: Option<DiskAtomRef>,
	atom_offset: u64,
	atom_document_length: u32,
	atom_frequency: u32,
	term_count: u64,
}

impl<'a> PostingAssembler<'a> {
	fn new(file: &'a DirectFile, highlights: bool) -> Self {
		Self {
			writer: ImmutableStreamWriter::new(file, PageType::IndexFulltextPosting),
			highlights,
			term: Vec::new(),
			term_offset: 0,
			term_document_frequency: 0,
			term_frequency: 0,
			atom: None,
			atom_offset: 0,
			atom_document_length: 0,
			atom_frequency: 0,
			term_count: 0,
		}
	}

	fn push(
		&mut self,
		token: TokenRecord,
		metadata: &mut ExternalIndexSorter,
	) -> FerrosGrynnResult<()> {
		if self.term != token.term {
			if !self.term.is_empty() && self.term >= token.term {
				return Err(invalid("Fulltext terms are not strictly ordered"));
			}
			self.finish_term(metadata)?;
			self.term = token.term.clone();
			self.term_offset = self.writer.position();
			self.writer.write_all(&[0_u8; POSTING_SEGMENT_HEADER_BYTES])?;
		}
		if self.atom != Some(token.atom) {
			if self.atom.is_some_and(|previous| previous >= token.atom) {
				return Err(invalid("Fulltext postings are not strictly ordered"));
			}
			self.finish_atom()?;
			self.atom = Some(token.atom);
			self.atom_offset = self.writer.position();
			self.atom_document_length = token.document_length;
			self.atom_frequency = 0;
			self.term_document_frequency = self
				.term_document_frequency
				.checked_add(1)
				.ok_or_else(|| invalid("Fulltext document frequency exceeds u32"))?;
			let mut header = [0_u8; POSTING_HEADER_BYTES];
			header[..8].copy_from_slice(&token.atom.raw().to_le_bytes());
			header[12..16].copy_from_slice(&token.document_length.to_le_bytes());
			self.writer.write_all(&header)?;
		} else if self.atom_document_length != token.document_length {
			return Err(invalid("Fulltext token records disagree on document length"));
		}
		self.atom_frequency = self
			.atom_frequency
			.checked_add(1)
			.ok_or_else(|| invalid("Fulltext term frequency exceeds u32"))?;
		self.term_frequency = self
			.term_frequency
			.checked_add(1)
			.ok_or_else(|| invalid("Fulltext total term frequency overflows u64"))?;
		if self.highlights {
			self.writer.write_all(&token.start.to_le_bytes())?;
			self.writer.write_all(&token.end.to_le_bytes())?;
		}
		Ok(())
	}

	fn finish(
		mut self,
		metadata: &mut ExternalIndexSorter,
	) -> FerrosGrynnResult<(ImmutableStreamLocation, u64)> {
		self.finish_term(metadata)?;
		Ok((self.writer.finish()?, self.term_count))
	}

	fn finish_atom(&mut self) -> FerrosGrynnResult<()> {
		if self.atom.take().is_none() {
			return Ok(());
		}
		self.writer.patch(self.atom_offset + 8, &self.atom_frequency.to_le_bytes())?;
		let spans = if self.highlights {
			self.atom_frequency
		} else {
			0
		};
		self.writer.patch(self.atom_offset + 16, &spans.to_le_bytes())?;
		self.atom_frequency = 0;
		Ok(())
	}

	fn finish_term(&mut self, metadata: &mut ExternalIndexSorter) -> FerrosGrynnResult<()> {
		if self.term.is_empty() {
			return Ok(());
		}
		self.finish_atom()?;
		let posting_length = self
			.writer
			.position()
			.checked_sub(self.term_offset)
			.ok_or_else(|| invalid("Fulltext posting length underflows"))?;
		self.writer.patch(self.term_offset, &posting_length.to_le_bytes())?;
		self.writer.patch(self.term_offset + 8, &self.term_document_frequency.to_le_bytes())?;
		let key = dictionary_metadata_key(
			&self.term,
			self.term_offset,
			posting_length,
			self.term_document_frequency,
			self.term_frequency,
		)?;
		metadata.push_raw(key, rapidhash_v3(&self.term))?;
		self.term_count = self
			.term_count
			.checked_add(1)
			.ok_or_else(|| invalid("Fulltext term count overflows u64"))?;
		self.term.clear();
		self.term_document_frequency = 0;
		self.term_frequency = 0;
		Ok(())
	}
}

fn document_key(atom: DiskAtomRef) -> Vec<u8> {
	let mut key = Vec::with_capacity(9);
	key.push(DOCUMENT_RECORD_PREFIX);
	key.extend_from_slice(&atom.raw().to_be_bytes());
	key
}

fn token_key(term: &[u8], atom: DiskAtomRef, start: u32, end: u32) -> FerrosGrynnResult<Vec<u8>> {
	let mut key = Vec::with_capacity(term.len().saturating_add(18));
	key.push(TOKEN_RECORD_PREFIX);
	encode_term(&mut key, term)?;
	key.extend_from_slice(&atom.raw().to_be_bytes());
	key.extend_from_slice(&start.to_be_bytes());
	key.extend_from_slice(&end.to_be_bytes());
	Ok(key)
}

fn decode_token_key(key: &[u8], document_length: u64) -> FerrosGrynnResult<TokenRecord> {
	if key.first().copied() != Some(TOKEN_RECORD_PREFIX) {
		return Err(invalid("Fulltext token record has the wrong prefix"));
	}
	let (term, suffix) = decode_term(&key[1..])?;
	if suffix.len() != 16 {
		return Err(invalid("Fulltext token record has the wrong suffix width"));
	}
	let atom = DiskAtomRef::from_raw(u64::from_be_bytes(
		suffix[..8]
			.try_into()
			.map_err(|_error| invalid("Fulltext token address has the wrong width"))?,
	))?;
	let start = u32::from_be_bytes(
		suffix[8..12]
			.try_into()
			.map_err(|_error| invalid("Fulltext token start has the wrong width"))?,
	);
	let end = u32::from_be_bytes(
		suffix[12..16]
			.try_into()
			.map_err(|_error| invalid("Fulltext token end has the wrong width"))?,
	);
	if term.is_empty() || start >= end {
		return Err(invalid("Fulltext token record contains an invalid term or span"));
	}
	Ok(TokenRecord {
		term,
		atom,
		document_length: u32::try_from(document_length)
			.map_err(|_error| invalid("Fulltext token document length exceeds u32"))?,
		start,
		end,
	})
}

fn dictionary_metadata_key(
	term: &[u8],
	posting_offset: u64,
	posting_length: u64,
	document_frequency: u32,
	total_term_frequency: u64,
) -> FerrosGrynnResult<Vec<u8>> {
	let mut key = Vec::with_capacity(term.len().saturating_add(30));
	encode_term(&mut key, term)?;
	key.extend_from_slice(&posting_offset.to_be_bytes());
	key.extend_from_slice(&posting_length.to_be_bytes());
	key.extend_from_slice(&document_frequency.to_be_bytes());
	key.extend_from_slice(&total_term_frequency.to_be_bytes());
	Ok(key)
}

fn decode_dictionary_metadata(key: &[u8]) -> FerrosGrynnResult<DictionaryMetadata> {
	let (term, suffix) = decode_term(key)?;
	if suffix.len() != 28 {
		return Err(invalid("Fulltext dictionary metadata has the wrong suffix width"));
	}
	Ok(DictionaryMetadata {
		term,
		posting_offset: u64::from_be_bytes(
			suffix[..8]
				.try_into()
				.map_err(|_error| invalid("Fulltext posting offset has the wrong width"))?,
		),
		posting_length: u64::from_be_bytes(
			suffix[8..16]
				.try_into()
				.map_err(|_error| invalid("Fulltext posting length has the wrong width"))?,
		),
		document_frequency: u32::from_be_bytes(
			suffix[16..20]
				.try_into()
				.map_err(|_error| invalid("Fulltext document frequency has the wrong width"))?,
		),
		total_term_frequency: u64::from_be_bytes(
			suffix[20..28]
				.try_into()
				.map_err(|_error| invalid("Fulltext term frequency has the wrong width"))?,
		),
	})
}

fn encode_term(output: &mut Vec<u8>, term: &[u8]) -> FerrosGrynnResult<()> {
	if term.is_empty() || std::str::from_utf8(term).is_err() {
		return Err(invalid("Fulltext sort term is empty or invalid UTF-8"));
	}
	for byte in term {
		if *byte == 0 {
			output.extend_from_slice(&[0, 1]);
		} else {
			output.push(*byte);
		}
	}
	output.extend_from_slice(&[0, 0]);
	Ok(())
}

fn decode_term(input: &[u8]) -> FerrosGrynnResult<(Vec<u8>, &[u8])> {
	let mut term = Vec::new();
	let mut cursor = 0_usize;
	while cursor < input.len() {
		let byte = input[cursor];
		cursor += 1;
		if byte != 0 {
			term.push(byte);
			continue;
		}
		let escape =
			*input.get(cursor).ok_or_else(|| invalid("Fulltext escaped term is truncated"))?;
		cursor += 1;
		match escape {
			0 => {
				if term.is_empty() || std::str::from_utf8(&term).is_err() {
					return Err(invalid("Fulltext escaped term is empty or invalid UTF-8"));
				}
				return Ok((term, &input[cursor..]));
			}
			1 => term.push(0),
			_ => return Err(invalid("Fulltext escaped term uses an unknown marker")),
		}
	}
	Err(invalid("Fulltext escaped term has no terminator"))
}

const fn location(location: ImmutableStreamLocation) -> StreamLocation {
	StreamLocation {
		first: location.first,
		pages: location.pages,
	}
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}
