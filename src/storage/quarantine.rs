//! Self-contained, streaming `FGQR v1` quarantine files.

use std::{
	fs::{self, File, OpenOptions},
	io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
	ops::Bound,
	path::{Path, PathBuf},
	str::FromStr,
};

use rapidhash::v3::{DEFAULT_RAPID_SECRETS, RapidStreamHasherV3, rapidhash_v3};

use crate::{
	datastore::SchemaCatalog,
	gql_status::{FerrosGrynnError, FerrosGrynnResult, StatusCode},
	graph::{GraphKind, GraphRef, GraphStorageMode},
	values::{
		PropertyDictionary,
		PropertyKeyId,
		Uuid,
		Value,
		storage::{
			DiskGraphRef,
			EncodeContext,
			StorageValue,
			codec::{CanonicalCodec, DecodeLimits},
		},
	},
};

const MAGIC: [u8; 4] = *b"FGQR";
const FOOTER_MAGIC: [u8; 4] = *b"FGQF";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 72;
const FOOTER_BYTES: usize = 32;
const HEADER_CHECKSUM_OFFSET: usize = 64;
const FOOTER_CHECKSUM_OFFSET: usize = 24;
const MAX_CONTEXT_ENTRIES: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 128 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 4096;

/// One decoded rejected import item.
pub(crate) struct QuarantineRecord {
	pub(crate) ordinal: u64,
	pub(crate) payload: Value,
	pub(crate) code: StatusCode,
	pub(crate) diagnostic: String,
}

/// Streaming writer whose incomplete file is removed on every unwind path.
pub(crate) struct QuarantineWriter {
	id: Uuid,
	path: PathBuf,
	writer: BufWriter<File>,
	context: EncodeContext,
	record_count: u32,
	record_bytes: u64,
	last_ordinal: u64,
	armed: bool,
}

impl QuarantineWriter {
	/// Creates one unreachable temporary quarantine file.
	pub(crate) fn create(root: &Path, id: Uuid) -> FerrosGrynnResult<Self> {
		let directory = root.join("tmp").join("import");
		fs::create_dir_all(&directory)
			.map_err(|source| io_failure("create import quarantine scratch", source))?;
		let path = directory.join(format!("{id}.fgq.tmp"));
		let file = OpenOptions::new()
			.read(true)
			.write(true)
			.create_new(true)
			.open(&path)
			.map_err(|source| io_failure("create import quarantine", source))?;
		let mut writer = BufWriter::new(file);
		writer
			.write_all(&[0; HEADER_BYTES])
			.map_err(|source| io_failure("write import quarantine header", source))?;
		Ok(Self {
			id,
			path,
			writer,
			context: EncodeContext::new(PropertyDictionary::new()),
			record_count: 0,
			record_bytes: 0,
			last_ordinal: 0,
			armed: true,
		})
	}

	/// Appends one canonical payload and its bounded diagnostic.
	pub(crate) fn write_record(
		&mut self,
		ordinal: u64,
		payload: Value,
		code: StatusCode,
		diagnostic: &str,
		schema: &SchemaCatalog,
	) -> FerrosGrynnResult<()> {
		if ordinal == 0 || ordinal <= self.last_ordinal {
			return Err(invalid("quarantine item ordinals are not strictly increasing"));
		}
		if diagnostic.len() > MAX_DIAGNOSTIC_BYTES || diagnostic.contains('\0') {
			return Err(invalid("quarantine diagnostic is outside its format domain"));
		}
		payload.ensure_storable()?;
		register_references(&payload, schema, &mut self.context)?;
		let storage = StorageValue::from_runtime(payload, &mut self.context)?;
		let payload = CanonicalCodec::encode(storage.as_ref())?;
		if payload.len() > MAX_PAYLOAD_BYTES {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"quarantine payload bytes",
				MAX_PAYLOAD_BYTES as u64,
				payload.len() as u64,
			));
		}
		let payload_len = u32::try_from(payload.len())
			.map_err(|_error| invalid("quarantine payload length overflows u32"))?;
		let diagnostic_len = u16::try_from(diagnostic.len())
			.map_err(|_error| invalid("quarantine diagnostic length overflows u16"))?;
		self.writer
			.write_all(&ordinal.to_le_bytes())
			.and_then(|()| self.writer.write_all(code.body().as_bytes()))
			.and_then(|()| self.writer.write_all(&[0; 3]))
			.and_then(|()| self.writer.write_all(&payload_len.to_le_bytes()))
			.and_then(|()| self.writer.write_all(&diagnostic_len.to_le_bytes()))
			.and_then(|()| self.writer.write_all(&0_u16.to_le_bytes()))
			.and_then(|()| self.writer.write_all(&payload))
			.and_then(|()| self.writer.write_all(diagnostic.as_bytes()))
			.map_err(|source| io_failure("append import quarantine record", source))?;
		let record_len = 24_u64
			.checked_add(u64::from(payload_len))
			.and_then(|length| length.checked_add(u64::from(diagnostic_len)))
			.ok_or_else(|| invalid("quarantine record length overflows"))?;
		self.record_bytes = self
			.record_bytes
			.checked_add(record_len)
			.ok_or_else(|| invalid("quarantine record area length overflows"))?;
		self.record_count = self
			.record_count
			.checked_add(1)
			.ok_or_else(|| invalid("quarantine record count is exhausted"))?;
		self.last_ordinal = ordinal;
		Ok(())
	}

	/// Seals, synchronizes, and returns one still-unpublished artifact.
	pub(crate) fn finish(mut self) -> FerrosGrynnResult<QuarantineArtifact> {
		let context_start = (HEADER_BYTES as u64)
			.checked_add(self.record_bytes)
			.ok_or_else(|| invalid("quarantine context offset overflows"))?;
		let context_counts = write_context(&mut self.writer, &self.context)?;
		self.writer
			.flush()
			.map_err(|source| io_failure("flush import quarantine context", source))?;
		let context_end = self
			.writer
			.stream_position()
			.map_err(|source| io_failure("measure import quarantine context", source))?;
		let context_bytes = context_end
			.checked_sub(context_start)
			.ok_or_else(|| invalid("quarantine context position moved backwards"))?;
		let header = encode_header(
			self.id,
			self.record_count,
			context_counts,
			self.record_bytes,
			context_bytes,
		)?;
		self.writer
			.seek(SeekFrom::Start(0))
			.and_then(|_| self.writer.write_all(&header))
			.and_then(|()| self.writer.flush())
			.map_err(|source| io_failure("seal import quarantine header", source))?;
		let checksum = hash_prefix(&self.path, context_end)?;
		let footer = encode_footer(u64::from(self.record_count), checksum);
		self.writer
			.seek(SeekFrom::Start(context_end))
			.and_then(|_| self.writer.write_all(&footer))
			.and_then(|()| self.writer.flush())
			.map_err(|source| io_failure("seal import quarantine footer", source))?;
		self.writer
			.get_ref()
			.sync_all()
			.map_err(|source| io_failure("synchronize import quarantine", source))?;
		self.armed = false;
		Ok(QuarantineArtifact {
			id: self.id,
			path: self.path.clone(),
			armed: true,
		})
	}
}

impl Drop for QuarantineWriter {
	fn drop(&mut self) {
		if self.armed {
			drop(fs::remove_file(&self.path));
		}
	}
}

/// Sealed file that remains unreachable until its catalog publication wins.
pub(crate) struct QuarantineArtifact {
	id: Uuid,
	path: PathBuf,
	armed: bool,
}

impl QuarantineArtifact {
	pub(crate) const fn id(&self) -> Uuid {
		self.id
	}

	/// Moves the synced file to its canonical immutable location while
	/// retaining rollback cleanup ownership.
	pub(crate) fn install(&mut self, root: &Path) -> FerrosGrynnResult<PathBuf> {
		let path = quarantine_path(root, self.id);
		let parent = path.parent().ok_or_else(|| invalid("quarantine path has no parent"))?;
		fs::create_dir_all(parent)
			.map_err(|source| io_failure("create quarantine directory", source))?;
		fs::rename(&self.path, &path)
			.map_err(|source| io_failure("install import quarantine", source))?;
		self.path = path.clone();
		Ok(path)
	}

	/// Disarms deletion after the same catalog/WAL commit is durable.
	pub(crate) fn publish(&mut self) {
		self.armed = false;
	}
}

impl Drop for QuarantineArtifact {
	fn drop(&mut self) {
		if self.armed {
			drop(fs::remove_file(&self.path));
		}
	}
}

/// Exact reader that validates the complete immutable file before yielding its
/// first record, then decodes records one at a time.
pub(crate) struct QuarantineArchiveReader {
	id: Uuid,
	reader: BufReader<File>,
	context: EncodeContext,
	remaining: u32,
	records_end: u64,
	last_ordinal: u64,
}

impl QuarantineArchiveReader {
	pub(crate) fn open(path: &Path) -> FerrosGrynnResult<Self> {
		let file =
			File::open(path).map_err(|source| io_failure("open import quarantine", source))?;
		let file_len = file
			.metadata()
			.map_err(|source| io_failure("inspect import quarantine", source))?
			.len();
		if file_len < (HEADER_BYTES + FOOTER_BYTES) as u64 {
			return Err(invalid("quarantine file is truncated"));
		}
		let mut reader = BufReader::new(file);
		let mut header = [0; HEADER_BYTES];
		reader
			.read_exact(&mut header)
			.map_err(|source| io_failure("read import quarantine header", source))?;
		let decoded = decode_header(&header)?;
		let records_end = (HEADER_BYTES as u64)
			.checked_add(decoded.record_bytes)
			.ok_or_else(|| invalid("quarantine record end overflows"))?;
		let context_end = records_end
			.checked_add(decoded.context_bytes)
			.ok_or_else(|| invalid("quarantine context end overflows"))?;
		let expected_len = context_end
			.checked_add(FOOTER_BYTES as u64)
			.ok_or_else(|| invalid("quarantine file length overflows"))?;
		if file_len != expected_len {
			return Err(invalid("quarantine lengths do not consume the file"));
		}
		reader
			.seek(SeekFrom::Start(context_end))
			.map_err(|source| io_failure("seek import quarantine footer", source))?;
		let mut footer = [0; FOOTER_BYTES];
		reader
			.read_exact(&mut footer)
			.map_err(|source| io_failure("read import quarantine footer", source))?;
		let file_checksum = decode_footer(&footer, u64::from(decoded.record_count))?;
		if hash_prefix(path, context_end)? != file_checksum {
			return Err(invalid("quarantine file checksum does not match"));
		}
		reader
			.seek(SeekFrom::Start(records_end))
			.map_err(|source| io_failure("seek import quarantine context", source))?;
		let context = read_context(&mut reader, decoded.context_counts)?;
		if reader
			.stream_position()
			.map_err(|source| io_failure("measure import quarantine context", source))?
			!= context_end
		{
			return Err(invalid("quarantine context length is not canonical"));
		}
		reader
			.seek(SeekFrom::Start(HEADER_BYTES as u64))
			.map_err(|source| io_failure("seek import quarantine records", source))?;
		Ok(Self {
			id: decoded.id,
			reader,
			context,
			remaining: decoded.record_count,
			records_end,
			last_ordinal: 0,
		})
	}

	pub(crate) const fn id(&self) -> Uuid {
		self.id
	}

	pub(crate) fn next_record(&mut self) -> FerrosGrynnResult<Option<QuarantineRecord>> {
		if self.remaining == 0 {
			return Ok(None);
		}
		let ordinal = read_u64(&mut self.reader, "quarantine record ordinal")?;
		if ordinal == 0 || ordinal <= self.last_ordinal {
			return Err(invalid("quarantine record ordinals are not strictly increasing"));
		}
		let mut status = [0; 5];
		read_exact(&mut self.reader, &mut status, "quarantine status code")?;
		let code = StatusCode::from_str(&format!(
			"FG-{}",
			std::str::from_utf8(&status)
				.map_err(|_error| invalid("quarantine status code is not ASCII"))?
		))
		.map_err(|_error| invalid("quarantine status code is malformed"))?;
		let mut reserved = [0; 3];
		read_exact(&mut self.reader, &mut reserved, "quarantine record reserved bytes")?;
		if reserved != [0; 3] {
			return Err(invalid("quarantine record reserved bytes are non-zero"));
		}
		let payload_len =
			read_bounded_u32(&mut self.reader, MAX_PAYLOAD_BYTES, "quarantine payload length")?;
		let diagnostic_len =
			usize::from(read_u16(&mut self.reader, "quarantine diagnostic length")?);
		if diagnostic_len > MAX_DIAGNOSTIC_BYTES {
			return Err(invalid("quarantine diagnostic exceeds its format limit"));
		}
		if read_u16(&mut self.reader, "quarantine record reserved bytes")? != 0 {
			return Err(invalid("quarantine record reserved bytes are non-zero"));
		}
		let mut payload = vec![0; payload_len];
		read_exact(&mut self.reader, &mut payload, "quarantine payload")?;
		let mut diagnostic = vec![0; diagnostic_len];
		read_exact(&mut self.reader, &mut diagnostic, "quarantine diagnostic")?;
		let position = self
			.reader
			.stream_position()
			.map_err(|source| io_failure("measure quarantine record", source))?;
		if position > self.records_end || (self.remaining == 1 && position != self.records_end) {
			return Err(invalid("quarantine records do not consume their declared area"));
		}
		let diagnostic = String::from_utf8(diagnostic)
			.map_err(|_error| invalid("quarantine diagnostic is not UTF-8"))?;
		if diagnostic.contains('\0') {
			return Err(invalid("quarantine diagnostic contains NUL"));
		}
		let payload = CanonicalCodec::decode(&payload, DecodeLimits::default())?
			.into_runtime(&self.context.decoder())?;
		self.remaining -= 1;
		self.last_ordinal = ordinal;
		Ok(Some(QuarantineRecord {
			ordinal,
			payload,
			code,
			diagnostic,
		}))
	}
}

#[derive(Clone, Copy)]
struct ContextCounts {
	properties: u32,
	classes: u32,
	graphs: u32,
	time_zones: u32,
}

struct Header {
	id: Uuid,
	record_count: u32,
	context_counts: ContextCounts,
	record_bytes: u64,
	context_bytes: u64,
}

fn encode_header(
	id: Uuid,
	record_count: u32,
	counts: ContextCounts,
	record_bytes: u64,
	context_bytes: u64,
) -> FerrosGrynnResult<[u8; HEADER_BYTES]> {
	let mut bytes = [0; HEADER_BYTES];
	bytes[..4].copy_from_slice(&MAGIC);
	bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
	bytes[8..24].copy_from_slice(id.as_inner().as_bytes());
	bytes[24..28].copy_from_slice(&record_count.to_le_bytes());
	bytes[28..32].copy_from_slice(&counts.properties.to_le_bytes());
	bytes[32..36].copy_from_slice(&counts.classes.to_le_bytes());
	bytes[36..40].copy_from_slice(&counts.graphs.to_le_bytes());
	bytes[40..44].copy_from_slice(&counts.time_zones.to_le_bytes());
	bytes[48..56].copy_from_slice(&record_bytes.to_le_bytes());
	bytes[56..64].copy_from_slice(&context_bytes.to_le_bytes());
	let checksum = rapidhash_v3(&bytes[..HEADER_CHECKSUM_OFFSET]);
	bytes[HEADER_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
	Ok(bytes)
}

fn decode_header(bytes: &[u8; HEADER_BYTES]) -> FerrosGrynnResult<Header> {
	if bytes[..4] != MAGIC {
		return Err(invalid("quarantine magic does not match FGQR"));
	}
	if slice_u16(bytes, 4) != VERSION {
		return Err(invalid("quarantine version is incompatible with FGQR v1"));
	}
	if bytes[6..8].iter().chain(&bytes[44..48]).any(|byte| *byte != 0) {
		return Err(invalid("quarantine header reserved bytes are non-zero"));
	}
	if slice_u64(bytes, HEADER_CHECKSUM_OFFSET) != rapidhash_v3(&bytes[..HEADER_CHECKSUM_OFFSET]) {
		return Err(invalid("quarantine header checksum does not match"));
	}
	let mut uuid = [0; 16];
	uuid.copy_from_slice(&bytes[8..24]);
	let counts = ContextCounts {
		properties: slice_u32(bytes, 28),
		classes: slice_u32(bytes, 32),
		graphs: slice_u32(bytes, 36),
		time_zones: slice_u32(bytes, 40),
	};
	for count in [counts.properties, counts.classes, counts.graphs, counts.time_zones] {
		if usize::try_from(count).unwrap_or(usize::MAX) > MAX_CONTEXT_ENTRIES {
			return Err(invalid("quarantine context count exceeds its format limit"));
		}
	}
	Ok(Header {
		id: Uuid::from(uuid::Uuid::from_bytes(uuid)),
		record_count: slice_u32(bytes, 24),
		context_counts: counts,
		record_bytes: slice_u64(bytes, 48),
		context_bytes: slice_u64(bytes, 56),
	})
}

fn encode_footer(record_count: u64, checksum: u64) -> [u8; FOOTER_BYTES] {
	let mut bytes = [0; FOOTER_BYTES];
	bytes[..4].copy_from_slice(&FOOTER_MAGIC);
	bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
	bytes[8..16].copy_from_slice(&record_count.to_le_bytes());
	bytes[16..24].copy_from_slice(&checksum.to_le_bytes());
	let footer_checksum = rapidhash_v3(&bytes[..FOOTER_CHECKSUM_OFFSET]);
	bytes[FOOTER_CHECKSUM_OFFSET..].copy_from_slice(&footer_checksum.to_le_bytes());
	bytes
}

fn decode_footer(bytes: &[u8; FOOTER_BYTES], record_count: u64) -> FerrosGrynnResult<u64> {
	if bytes[..4] != FOOTER_MAGIC
		|| slice_u16(bytes, 4) != VERSION
		|| bytes[6..8].iter().any(|byte| *byte != 0)
	{
		return Err(invalid("quarantine footer marker or version is invalid"));
	}
	if slice_u64(bytes, 8) != record_count {
		return Err(invalid("quarantine footer record count does not match"));
	}
	if slice_u64(bytes, FOOTER_CHECKSUM_OFFSET) != rapidhash_v3(&bytes[..FOOTER_CHECKSUM_OFFSET]) {
		return Err(invalid("quarantine footer checksum does not match"));
	}
	Ok(slice_u64(bytes, 16))
}

fn write_context(
	writer: &mut BufWriter<File>,
	context: &EncodeContext,
) -> FerrosGrynnResult<ContextCounts> {
	let counts = ContextCounts {
		properties: u32::try_from(context.property_dictionary().len())
			.map_err(|_error| invalid("quarantine property dictionary exceeds u32"))?,
		classes: u32::try_from(context.class_names().len())
			.map_err(|_error| invalid("quarantine class dictionary exceeds u32"))?,
		graphs: u32::try_from(context.graph_references().len())
			.map_err(|_error| invalid("quarantine graph dictionary exceeds u32"))?,
		time_zones: u32::try_from(context.time_zone_names().len())
			.map_err(|_error| invalid("quarantine time-zone dictionary exceeds u32"))?,
	};
	for (_id, name) in context.property_dictionary().iter() {
		write_string(writer, name.as_str(), "quarantine property name")?;
	}
	for (id, name) in context.class_names() {
		writer
			.write_all(&id.to_le_bytes())
			.map_err(|source| io_failure("write quarantine class identity", source))?;
		write_string(writer, name.as_str(), "quarantine class name")?;
	}
	for (disk, graph) in context.graph_references() {
		writer
			.write_all(&disk.raw().to_le_bytes())
			.and_then(|()| writer.write_all(&[graph_kind(graph.kind())]))
			.and_then(|()| writer.write_all(&[storage_mode(graph.storage_mode())]))
			.and_then(|()| writer.write_all(&0_u16.to_le_bytes()))
			.map_err(|source| io_failure("write quarantine graph identity", source))?;
		write_string(writer, graph.name().as_str(), "quarantine graph name")?;
	}
	for name in context.time_zone_names() {
		write_string(writer, name.as_str(), "quarantine time-zone name")?;
	}
	Ok(counts)
}

fn read_context(
	reader: &mut BufReader<File>,
	counts: ContextCounts,
) -> FerrosGrynnResult<EncodeContext> {
	let mut properties = PropertyDictionary::with_capacity(counts.properties as usize);
	for raw in 0..counts.properties {
		let name = read_string(reader, "quarantine property name")?;
		properties.restore(PropertyKeyId::new(raw), name.into())?;
	}
	let mut context = EncodeContext::new(properties);
	let mut previous_class = 0;
	for _ in 0..counts.classes {
		let id = read_u32(reader, "quarantine class identity")?;
		if id <= previous_class {
			return Err(invalid("quarantine class identities are not strictly increasing"));
		}
		let name = read_string(reader, "quarantine class name")?;
		context.register_class_name(id, name.into())?;
		previous_class = id;
	}
	let mut previous_graph = 0;
	for _ in 0..counts.graphs {
		let id = read_u64(reader, "quarantine graph identity")?;
		if id <= previous_graph {
			return Err(invalid("quarantine graph identities are not strictly increasing"));
		}
		let kind = decode_graph_kind(read_u8(reader, "quarantine graph kind")?)?;
		let mode = decode_storage_mode(read_u8(reader, "quarantine graph storage mode")?)?;
		if read_u16(reader, "quarantine graph reserved bytes")? != 0 {
			return Err(invalid("quarantine graph reserved bytes are non-zero"));
		}
		let name = read_string(reader, "quarantine graph name")?;
		let graph_id = crate::graph::GraphId::new(id)
			.ok_or_else(|| invalid("quarantine graph identity zero is reserved"))?;
		let graph = GraphRef::new(graph_id, name, kind, mode)?;
		context.register_graph(graph, DiskGraphRef::new(graph_id))?;
		previous_graph = id;
	}
	for expected in 1..=counts.time_zones {
		let name = read_string(reader, "quarantine time-zone name")?;
		if context.register_time_zone_name(name.into())? != expected {
			return Err(invalid("quarantine time-zone names are not canonical"));
		}
	}
	Ok(context)
}

fn register_references(
	value: &Value,
	schema: &SchemaCatalog,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
	match value {
		Value::Record(record) => register_class(record.class(), schema, context),
		Value::Atom(atom) => register_class(atom.id().class(), schema, context),
		Value::GraphRef(graph) => {
			context.register_graph(graph.as_ref().clone(), DiskGraphRef::new(graph.id()))
		}
		Value::Array(values) => {
			for value in values {
				register_references(value, schema, context)?;
			}
			Ok(())
		}
		Value::Set(values) => {
			for value in values {
				register_references(value, schema, context)?;
			}
			Ok(())
		}
		Value::Object(values) => {
			for value in values.values() {
				register_references(value, schema, context)?;
			}
			Ok(())
		}
		Value::Range(range) => {
			for bound in [range.start(), range.end()] {
				if let Bound::Included(value) | Bound::Excluded(value) = bound {
					register_references(value, schema, context)?;
				}
			}
			Ok(())
		}
		_ => Ok(()),
	}
}

fn register_class(
	name: &str,
	schema: &SchemaCatalog,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<()> {
	let class = schema
		.class(name)
		.ok_or_else(|| FerrosGrynnError::invalid_argument(format!("unknown class `{name}`")))?;
	let id =
		class.id().ok_or_else(|| invalid("quarantine payload references an unpublished class"))?;
	context.register_class_name(id.get(), class.name().clone())
}

fn write_string(writer: &mut BufWriter<File>, value: &str, field: &str) -> FerrosGrynnResult<()> {
	if value.len() > MAX_NAME_BYTES || value.contains('\0') {
		return Err(invalid(format!("{field} is outside its format domain")));
	}
	let length = u32::try_from(value.len())
		.map_err(|_error| invalid(format!("{field} length overflows u32")))?;
	writer
		.write_all(&length.to_le_bytes())
		.and_then(|()| writer.write_all(value.as_bytes()))
		.map_err(|source| io_failure(format!("write {field}"), source))
}

fn read_string(reader: &mut BufReader<File>, field: &str) -> FerrosGrynnResult<String> {
	let length = read_bounded_u32(reader, MAX_NAME_BYTES, field)?;
	let mut bytes = vec![0; length];
	read_exact(reader, &mut bytes, field)?;
	let value =
		String::from_utf8(bytes).map_err(|_error| invalid(format!("{field} is not UTF-8")))?;
	if value.contains('\0') {
		return Err(invalid(format!("{field} contains NUL")));
	}
	Ok(value)
}

fn hash_prefix(path: &Path, length: u64) -> FerrosGrynnResult<u64> {
	let mut file =
		File::open(path).map_err(|source| io_failure("open quarantine checksum", source))?;
	let mut hasher = RapidStreamHasherV3::new(&DEFAULT_RAPID_SECRETS);
	let mut remaining = length;
	let mut buffer = [0; 64 * 1024];
	while remaining != 0 {
		let wanted = usize::try_from(remaining.min(buffer.len() as u64))
			.map_err(|_error| invalid("quarantine checksum window exceeds usize"))?;
		file.read_exact(&mut buffer[..wanted])
			.map_err(|source| io_failure("read quarantine checksum", source))?;
		hasher.write(&buffer[..wanted]);
		remaining -= wanted as u64;
	}
	Ok(hasher.finish())
}

pub(crate) fn quarantine_path(root: &Path, id: Uuid) -> PathBuf {
	root.join("admin").join("quarantine").join(format!("{id}.fgq"))
}

fn graph_kind(value: GraphKind) -> u8 {
	match value {
		GraphKind::Graph => 0,
		GraphKind::Tree => 1,
		GraphKind::RootedTree => 2,
		GraphKind::Forest => 3,
		GraphKind::Dag => 4,
		GraphKind::Hypergraph => 5,
		GraphKind::Metagraph => 6,
		GraphKind::Subgraph => 7,
	}
}

fn decode_graph_kind(value: u8) -> FerrosGrynnResult<GraphKind> {
	match value {
		0 => Ok(GraphKind::Graph),
		1 => Ok(GraphKind::Tree),
		2 => Ok(GraphKind::RootedTree),
		3 => Ok(GraphKind::Forest),
		4 => Ok(GraphKind::Dag),
		5 => Ok(GraphKind::Hypergraph),
		6 => Ok(GraphKind::Metagraph),
		7 => Ok(GraphKind::Subgraph),
		_ => Err(invalid("quarantine graph kind is unknown")),
	}
}

fn storage_mode(value: GraphStorageMode) -> u8 {
	match value {
		GraphStorageMode::InMemory => 0,
		GraphStorageMode::DiskBacked => 1,
		GraphStorageMode::Hybrid => 2,
	}
}

fn decode_storage_mode(value: u8) -> FerrosGrynnResult<GraphStorageMode> {
	match value {
		0 => Ok(GraphStorageMode::InMemory),
		1 => Ok(GraphStorageMode::DiskBacked),
		2 => Ok(GraphStorageMode::Hybrid),
		_ => Err(invalid("quarantine graph storage mode is unknown")),
	}
}

fn read_exact(
	reader: &mut BufReader<File>,
	output: &mut [u8],
	field: &str,
) -> FerrosGrynnResult<()> {
	reader.read_exact(output).map_err(|source| io_failure(format!("read {field}"), source))
}

fn read_u8(reader: &mut BufReader<File>, field: &str) -> FerrosGrynnResult<u8> {
	let mut bytes = [0; 1];
	read_exact(reader, &mut bytes, field)?;
	Ok(bytes[0])
}

fn read_u16(reader: &mut BufReader<File>, field: &str) -> FerrosGrynnResult<u16> {
	let mut bytes = [0; 2];
	read_exact(reader, &mut bytes, field)?;
	Ok(u16::from_le_bytes(bytes))
}

fn read_u32(reader: &mut BufReader<File>, field: &str) -> FerrosGrynnResult<u32> {
	let mut bytes = [0; 4];
	read_exact(reader, &mut bytes, field)?;
	Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut BufReader<File>, field: &str) -> FerrosGrynnResult<u64> {
	let mut bytes = [0; 8];
	read_exact(reader, &mut bytes, field)?;
	Ok(u64::from_le_bytes(bytes))
}

fn read_bounded_u32(
	reader: &mut BufReader<File>,
	maximum: usize,
	field: &str,
) -> FerrosGrynnResult<usize> {
	let value = usize::try_from(read_u32(reader, field)?)
		.map_err(|_error| invalid(format!("{field} exceeds this platform")))?;
	if value > maximum {
		return Err(invalid(format!("{field} exceeds its format limit")));
	}
	Ok(value)
}

fn slice_u16(bytes: &[u8], offset: usize) -> u16 {
	let mut value = [0; 2];
	value.copy_from_slice(&bytes[offset..offset + 2]);
	u16::from_le_bytes(value)
}

fn slice_u32(bytes: &[u8], offset: usize) -> u32 {
	let mut value = [0; 4];
	value.copy_from_slice(&bytes[offset..offset + 4]);
	u32::from_le_bytes(value)
}

fn slice_u64(bytes: &[u8], offset: usize) -> u64 {
	let mut value = [0; 8];
	value.copy_from_slice(&bytes[offset..offset + 8]);
	u64::from_le_bytes(value)
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(reason)
}

fn io_failure(operation: impl Into<String>, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::ClassSchema,
		graph::AtomKind,
		values::{Object, RecordId, RecordIdPart},
	};

	#[test]
	fn fg_qr_v1_streams_values_and_rejects_corruption() -> FerrosGrynnResult<()> {
		let directory = std::env::temp_dir().join(format!("fgqr-test-{}", Uuid::now_v7()));
		fs::create_dir(&directory)
			.map_err(|source| io_failure("create quarantine test directory", source))?;
		let result = (|| {
			let store = crate::datastore::Datastore::in_memory();
			let mut transaction = store.begin();
			transaction.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
			transaction.commit()?;
			let schema = store.inner.current_root().schema.clone();
			let id = Uuid::now_v7();
			let mut writer = QuarantineWriter::create(&directory, id)?;
			let mut payload = Object::new();
			payload.insert(
				"record",
				Value::from(RecordId::new("person", RecordIdPart::String("a".into()))?),
			);
			writer.write_record(
				1,
				Value::Object(payload),
				StatusCode::from_str("FG-22000").map_err(|_error| invalid("test status code"))?,
				"rejected",
				&schema,
			)?;
			let mut artifact = writer.finish()?;
			let final_path = artifact.install(&directory)?;
			artifact.publish();
			let mut reader = QuarantineArchiveReader::open(&final_path)?;
			assert_eq!(reader.id(), id);
			let record =
				reader.next_record()?.ok_or_else(|| invalid("test quarantine record is absent"))?;
			assert_eq!(record.ordinal, 1);
			assert_eq!(record.code.to_string(), "FG-22000");
			assert_eq!(record.diagnostic, "rejected");
			assert!(matches!(record.payload, Value::Object(_)));
			assert!(reader.next_record()?.is_none());

			let mut bytes = fs::read(&final_path)
				.map_err(|source| io_failure("read quarantine test file", source))?;
			bytes[HEADER_BYTES] ^= 1;
			let corrupt = directory.join("corrupt.fgq");
			fs::write(&corrupt, bytes)
				.map_err(|source| io_failure("write corrupt quarantine test file", source))?;
			assert!(QuarantineArchiveReader::open(&corrupt).is_err());
			Ok(())
		})();
		drop(fs::remove_dir_all(&directory));
		result
	}
}
