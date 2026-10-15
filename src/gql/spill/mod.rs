//! Transient, checksummed query runs shared by SELECT barriers and cursors.

mod value_codec;

use std::{
	cmp::Ordering,
	fs::{self, File},
	io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
	mem::size_of,
	path::{Path, PathBuf},
};

use rapidhash::v3::rapidhash_v3;

use self::value_codec::{decode_query_value, encode_query_value};
use super::{
	cursor::{CursorAnchor, CursorRecord},
	options::BarrierAccount,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Array, Value, storage::EncodeContext},
};

/// Configured magic value.
const MAGIC: &[u8; 8] = b"FGQRUN1\0";
/// Configured header bytes value.
const HEADER_BYTES: u64 = MAGIC.len() as u64;
/// Configured record prefix bytes value.
const RECORD_PREFIX_BYTES: usize = size_of::<u32>() + size_of::<u64>();
/// Configured max record bytes value.
const MAX_RECORD_BYTES: usize = 128 * 1024 * 1024;
/// Configured merge fan in value.
const MERGE_FAN_IN: usize = 64;

#[derive(Debug, Clone, Copy)]
/// Sort field state.
pub struct SortField {
	/// Descending associated with this sort field.
	pub(crate) descending: bool,
	/// Nulls first associated with this sort field.
	pub(crate) nulls_first: bool,
}

/// Sorted records alternatives.
pub enum SortedRecords {
	/// Memory case of sorted records.
	Memory {
		/// Records associated with this sorted records.
		records: Vec<CursorRecord>,
		/// Memory admission account retained with the owned data.
		account: BarrierAccount,
	},
	/// Disk case of sorted records.
	Disk {
		/// Spool associated with this sorted records.
		spool: QuerySpool,
		/// Position relative to the beginning of the associated buffer.
		offset: u64,
	},
}

impl SortedRecords {
	/// Converts this value into stream.
	pub(crate) fn into_stream(self) -> FerrosGrynnResult<RecordStream> {
		match self {
			Self::Memory {
				records,
				account,
			} => Ok(RecordStream {
				inner: RecordStreamInner::Memory {
					records: records.into_iter(),
					_account: account,
				},
			}),
			Self::Disk {
				spool,
				offset,
			} => {
				let mut reader = RunReader::open(&spool.path, spool.logical_end)?;
				reader.seek(offset)?;
				Ok(RecordStream {
					inner: RecordStreamInner::Disk {
						_spool: spool,
						reader,
					},
				})
			}
		}
	}
}

/// Record stream state.
pub struct RecordStream {
	/// Inner associated with this record stream.
	inner: RecordStreamInner,
}

/// Record stream inner alternatives.
enum RecordStreamInner {
	/// Memory case of record stream inner.
	Memory {
		/// Records associated with this record stream inner.
		records: std::vec::IntoIter<CursorRecord>,
		/// Memory admission account retained for the lifetime of this value.
		_account: BarrierAccount,
	},
	/// Disk case of record stream inner.
	Disk {
		/// Spool associated with this record stream inner.
		_spool: QuerySpool,
		/// Reader associated with this record stream inner.
		reader: RunReader,
	},
}

impl RecordStream {
	/// Returns the next record stream.
	pub(crate) fn next(&mut self) -> FerrosGrynnResult<Option<CursorRecord>> {
		match &mut self.inner {
			RecordStreamInner::Memory {
				records,
				..
			} => Ok(records.next()),
			RecordStreamInner::Disk {
				_spool: spool,
				reader,
			} => reader.next(&spool.context),
		}
	}
}

/// Query sorter state.
pub struct QuerySorter {
	/// Root associated with this query sorter.
	root: PathBuf,
	/// Session associated with this query sorter.
	session: Option<QueryRunSession>,
	/// Query-owned codec state; the same allocation moves into a disk spool.
	context: Box<EncodeContext>,
	/// Fields associated with this query sorter.
	fields: Vec<SortField>,
	/// Memory admission account retained with the owned data.
	account: BarrierAccount,
	/// Force disk associated with this query sorter.
	force_disk: bool,
	/// Buffered bytes associated with this query sorter.
	buffered_bytes: usize,
	/// Buffered associated with this query sorter.
	buffered: Vec<CursorRecord>,
	/// Runs associated with this query sorter.
	runs: Vec<Run>,
	/// Next run associated with this query sorter.
	next_run: u64,
}

impl QuerySorter {
	/// Creates a new `QuerySorter` value.
	pub(crate) fn new(
		root: &Path,
		context: Box<EncodeContext>,
		fields: Vec<SortField>,
		account: BarrierAccount,
		force_disk: bool,
	) -> Self {
		Self {
			root: root.to_owned(),
			session: None,
			context,
			fields,
			account,
			force_disk,
			buffered_bytes: 0,
			buffered: Vec::new(),
			runs: Vec::new(),
			next_run: 0,
		}
	}

	/// Pushes query sorter.
	pub(crate) fn push(&mut self, record: CursorRecord) -> FerrosGrynnResult<()> {
		let bytes = estimated_record_bytes(&record);
		let mut reserved = self.account.reserve(bytes);
		if !reserved && !self.buffered.is_empty() {
			self.spill_buffered()?;
			reserved = self.account.reserve(bytes);
		}
		if !reserved {
			return self.spill_record(record);
		}
		self.buffered_bytes = self
			.buffered_bytes
			.checked_add(bytes)
			.ok_or_else(|| invalid_run("query barrier accounting overflow"))?;
		self.buffered.push(record);
		Ok(())
	}

	/// Finishes query sorter.
	pub(crate) fn finish(mut self) -> FerrosGrynnResult<SortedRecords> {
		if self.runs.is_empty() && (!self.force_disk || self.buffered.is_empty()) {
			sort_records(&mut self.buffered, &self.fields);
			return Ok(SortedRecords::Memory {
				records: std::mem::take(&mut self.buffered),
				account: std::mem::replace(&mut self.account, BarrierAccount::new(0)),
			});
		}
		self.spill_buffered()?;
		while self.runs.len() > 1 {
			let input = std::mem::take(&mut self.runs);
			let mut output = Vec::with_capacity(input.len().div_ceil(MERGE_FAN_IN));
			for group in input.chunks(MERGE_FAN_IN) {
				let path = self.new_run_path()?;
				let run = merge_group(group, path, &self.fields, &mut self.context)?;
				self.account.record_spill(run.logical_end);
				output.push(run);
				for run in group {
					fs::remove_file(&run.path)
						.map_err(|source| run_io("remove merged query run", source))?;
				}
			}
			self.runs = output;
		}
		let run =
			self.runs.pop().ok_or_else(|| invalid_run("query sorter produced no final run"))?;
		Ok(SortedRecords::Disk {
			spool: QuerySpool {
				_session: self
					.session
					.take()
					.ok_or_else(|| invalid_run("query sorter has no run session"))?,
				path: run.path,
				logical_end: run.logical_end,
				context: self.context,
			},
			offset: HEADER_BYTES,
		})
	}

	/// Returns the spill buffered result.
	fn spill_buffered(&mut self) -> FerrosGrynnResult<()> {
		if self.buffered.is_empty() {
			return Ok(());
		}
		sort_records(&mut self.buffered, &self.fields);
		let path = self.new_run_path()?;
		let mut writer = RunWriter::create(path)?;
		for record in self.buffered.drain(..) {
			writer.push(record, &mut self.context)?;
		}
		let run = writer.finish()?;
		self.account.record_spill(run.logical_end);
		self.runs.push(run);
		self.buffered_bytes = 0;
		self.account.release_all();
		Ok(())
	}

	/// Returns the spill record result.
	fn spill_record(&mut self, record: CursorRecord) -> FerrosGrynnResult<()> {
		let path = self.new_run_path()?;
		let mut writer = RunWriter::create(path)?;
		writer.push(record, &mut self.context)?;
		let run = writer.finish()?;
		self.account.record_spill(run.logical_end);
		self.runs.push(run);
		Ok(())
	}

	/// Returns the new run path result.
	fn new_run_path(&mut self) -> FerrosGrynnResult<PathBuf> {
		let ordinal = self.next_run;
		self.next_run = self
			.next_run
			.checked_add(1)
			.ok_or_else(|| invalid_run("query run ordinal overflow"))?;
		let session = match &self.session {
			Some(session) => session,
			None => {
				self.session = Some(QueryRunSession::create(&self.root)?);
				self.session
					.as_ref()
					.ok_or_else(|| invalid_run("query sorter has no run session"))?
			}
		};
		Ok(session.path.join(format!("{ordinal:016x}.run")))
	}
}

/// Spool page state.
pub struct SpoolPage {
	/// Records associated with this spool page.
	pub(crate) records: Vec<CursorRecord>,
	/// Next offset associated with this spool page.
	pub(crate) next_offset: u64,
	/// Eof associated with this spool page.
	pub(crate) eof: bool,
}

#[derive(Debug)]
/// Query spool state.
pub struct QuerySpool {
	/// Session associated with this query spool.
	_session: QueryRunSession,
	/// Path associated with this query spool.
	path: PathBuf,
	/// Logical end associated with this query spool.
	logical_end: u64,
	/// Query-owned codec state retained unchanged from its producing sorter.
	context: Box<EncodeContext>,
}

impl QuerySpool {
	/// Returns the page result.
	pub(crate) fn page(&self, offset: u64, count: usize) -> FerrosGrynnResult<SpoolPage> {
		let mut reader = RunReader::open(&self.path, self.logical_end)?;
		reader.seek(offset)?;
		let mut records = Vec::with_capacity(count.min(1024));
		while records.len() < count {
			let Some(record) = reader.next(&self.context)? else {
				break;
			};
			records.push(record);
		}
		Ok(SpoolPage {
			records,
			next_offset: reader.position,
			eof: reader.position == self.logical_end,
		})
	}

	/// Returns the page after rows result.
	pub(crate) fn page_after_rows(
		&self,
		offset: u64,
		rows: usize,
		count: usize,
	) -> FerrosGrynnResult<SpoolPage> {
		let mut reader = RunReader::open(&self.path, self.logical_end)?;
		reader.seek(offset)?;
		for _ in 0..rows {
			if reader.next(&self.context)?.is_none() {
				break;
			}
		}
		self.page(reader.position, count)
	}

	/// Returns the offset after result.
	pub(crate) fn offset_after(
		&self,
		offset: u64,
		anchor: &CursorAnchor,
		fields: &[SortField],
	) -> FerrosGrynnResult<u64> {
		let mut reader = RunReader::open(&self.path, self.logical_end)?;
		reader.seek(offset)?;
		loop {
			let record_offset = reader.position;
			let Some(record) = reader.next(&self.context)? else {
				return Ok(reader.position);
			};
			if compare_record_anchor(&record, anchor, fields).is_gt() {
				return Ok(record_offset);
			}
		}
	}

	/// Returns the for each result.
	pub(crate) fn for_each(
		&self,
		offset: u64,
		mut visit: impl FnMut(CursorRecord) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		let mut reader = RunReader::open(&self.path, self.logical_end)?;
		reader.seek(offset)?;
		while let Some(record) = reader.next(&self.context)? {
			visit(record)?;
		}
		Ok(())
	}
}

#[derive(Debug)]
/// Query run session state.
struct QueryRunSession {
	/// Path associated with this query run session.
	path: PathBuf,
}

impl QueryRunSession {
	/// Creates query run session.
	fn create(root: &Path) -> FerrosGrynnResult<Self> {
		let root = root.join("tmp").join("query");
		fs::create_dir_all(&root).map_err(|source| run_io("create query-run root", source))?;
		let path = root.join(uuid::Uuid::now_v7().to_string());
		fs::create_dir(&path).map_err(|source| run_io("create query-run session", source))?;
		Ok(Self {
			path,
		})
	}
}

impl Drop for QueryRunSession {
	/// Runs cleanup for this `QueryRunSession` before its fields are released.
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.path));
	}
}

/// Run state.
struct Run {
	/// Path associated with this run.
	path: PathBuf,
	/// Logical end associated with this run.
	logical_end: u64,
}

/// Run writer state.
struct RunWriter {
	/// Path associated with this run writer.
	path: PathBuf,
	/// Writer associated with this run writer.
	writer: BufWriter<File>,
	/// Position associated with this run writer.
	position: u64,
}

impl RunWriter {
	/// Creates run writer.
	fn create(path: PathBuf) -> FerrosGrynnResult<Self> {
		let file = File::create_new(&path).map_err(|source| run_io("create query run", source))?;
		let mut writer = BufWriter::new(file);
		writer.write_all(MAGIC).map_err(|source| run_io("write query-run header", source))?;
		Ok(Self {
			path,
			writer,
			position: HEADER_BYTES,
		})
	}

	/// Pushes run writer.
	fn push(&mut self, record: CursorRecord, context: &mut EncodeContext) -> FerrosGrynnResult<()> {
		let payload = encode_record(record, context)?;
		let length = u32::try_from(payload.len())
			.map_err(|_error| invalid_run("query-run record exceeds u32"))?;
		let checksum = rapidhash_v3(&payload);
		self.writer
			.write_all(&length.to_le_bytes())
			.and_then(|()| self.writer.write_all(&checksum.to_le_bytes()))
			.and_then(|()| self.writer.write_all(&payload))
			.map_err(|source| run_io("write query-run record", source))?;
		self.position = self
			.position
			.checked_add(RECORD_PREFIX_BYTES as u64)
			.and_then(|position| position.checked_add(payload.len() as u64))
			.ok_or_else(|| invalid_run("query-run length overflow"))?;
		Ok(())
	}

	/// Finishes run writer.
	fn finish(mut self) -> FerrosGrynnResult<Run> {
		self.writer.flush().map_err(|source| run_io("flush query run", source))?;
		Ok(Run {
			path: self.path,
			logical_end: self.position,
		})
	}
}

/// Run reader state.
struct RunReader {
	/// Reader associated with this run reader.
	reader: BufReader<File>,
	/// Logical end associated with this run reader.
	logical_end: u64,
	/// Position associated with this run reader.
	position: u64,
}

impl RunReader {
	/// Opens run reader.
	fn open(path: &Path, logical_end: u64) -> FerrosGrynnResult<Self> {
		let file = File::open(path).map_err(|source| run_io("open query run", source))?;
		let actual = file.metadata().map_err(|source| run_io("stat query run", source))?.len();
		if actual != logical_end || logical_end < HEADER_BYTES {
			return Err(invalid_run("query-run length is invalid"));
		}
		let mut reader = BufReader::new(file);
		let mut magic = [0_u8; MAGIC.len()];
		reader.read_exact(&mut magic).map_err(|source| run_io("read query-run header", source))?;
		if &magic != MAGIC {
			return Err(invalid_run("query-run magic or version is invalid"));
		}
		Ok(Self {
			reader,
			logical_end,
			position: HEADER_BYTES,
		})
	}

	/// Returns the seek result.
	fn seek(&mut self, offset: u64) -> FerrosGrynnResult<()> {
		if !(HEADER_BYTES..=self.logical_end).contains(&offset) {
			return Err(invalid_run("query cursor points outside its run"));
		}
		self.reader
			.seek(SeekFrom::Start(offset))
			.map_err(|source| run_io("seek query run", source))?;
		self.position = offset;
		Ok(())
	}

	/// Returns the next run reader.
	fn next(&mut self, context: &EncodeContext) -> FerrosGrynnResult<Option<CursorRecord>> {
		if self.position == self.logical_end {
			return Ok(None);
		}
		if self.logical_end - self.position < RECORD_PREFIX_BYTES as u64 {
			return Err(invalid_run("query run ends inside a record prefix"));
		}
		let mut prefix = [0_u8; RECORD_PREFIX_BYTES];
		self.reader
			.read_exact(&mut prefix)
			.map_err(|source| run_io("read query-run prefix", source))?;
		let length = usize::try_from(u32::from_le_bytes(
			prefix[..4]
				.try_into()
				.map_err(|_error| invalid_run("query-run length prefix is invalid"))?,
		))
		.map_err(|_error| invalid_run("query-run length does not fit usize"))?;
		if length > MAX_RECORD_BYTES
			|| self
				.position
				.checked_add(RECORD_PREFIX_BYTES as u64)
				.and_then(|position| position.checked_add(length as u64))
				.is_none_or(|end| end > self.logical_end)
		{
			return Err(invalid_run("query-run record length is invalid"));
		}
		let checksum = u64::from_le_bytes(
			prefix[4..]
				.try_into()
				.map_err(|_error| invalid_run("query-run checksum prefix is invalid"))?,
		);
		let mut payload = vec![0_u8; length];
		self.reader
			.read_exact(&mut payload)
			.map_err(|source| run_io("read query-run payload", source))?;
		if rapidhash_v3(&payload) != checksum {
			return Err(invalid_run("query-run checksum mismatch"));
		}
		self.position += RECORD_PREFIX_BYTES as u64 + length as u64;
		decode_record(&payload, context).map(Some)
	}
}

/// Merges group.
fn merge_group(
	runs: &[Run],
	path: PathBuf,
	fields: &[SortField],
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Run> {
	let mut readers = runs
		.iter()
		.map(|run| RunReader::open(&run.path, run.logical_end))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut heads = readers
		.iter_mut()
		.map(|reader| reader.next(context))
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut writer = RunWriter::create(path)?;
	loop {
		let next = heads
			.iter()
			.enumerate()
			.filter_map(|(index, value)| value.as_ref().map(|value| (index, value)))
			.min_by(|(_left_index, left), (_right_index, right)| {
				compare_records(left, right, fields)
			})
			.map(|(index, _value)| index);
		let Some(index) = next else {
			break;
		};
		let record =
			heads[index].take().ok_or_else(|| invalid_run("query merge selected an empty head"))?;
		writer.push(record, context)?;
		heads[index] = readers[index].next(context)?;
	}
	writer.finish()
}

/// Encodes record.
fn encode_record(record: CursorRecord, context: &mut EncodeContext) -> FerrosGrynnResult<Vec<u8>> {
	let value = Value::Array(Array::new(vec![
		Value::Array(Array::new(record.anchor.order_keys)),
		Value::Array(Array::new(record.anchor.stable_key)),
		record.value,
	]));
	encode_query_value(value, context)
}

/// Decodes record.
fn decode_record(bytes: &[u8], context: &EncodeContext) -> FerrosGrynnResult<CursorRecord> {
	let value = decode_query_value(bytes, context)?;
	let Value::Array(values) = value else {
		return Err(invalid_run("query-run record is not an array"));
	};
	let mut values = values.into_iter();
	let Some(Value::Array(order_keys)) = values.next() else {
		return Err(invalid_run("query-run order key is invalid"));
	};
	let Some(Value::Array(stable_key)) = values.next() else {
		return Err(invalid_run("query-run stable key is invalid"));
	};
	let value = values.next().ok_or_else(|| invalid_run("query-run value is absent"))?;
	if values.next().is_some() {
		return Err(invalid_run("query-run record contains trailing values"));
	}
	Ok(CursorRecord {
		value,
		anchor: CursorAnchor {
			order_keys: order_keys.into_iter().collect(),
			stable_key: stable_key.into_iter().collect(),
		},
	})
}

/// Returns the estimated record bytes result.
fn estimated_record_bytes(record: &CursorRecord) -> usize {
	size_of::<CursorRecord>()
		.saturating_add(record.value.estimated_heap_bytes())
		.saturating_add(record.anchor.order_keys.capacity().saturating_mul(size_of::<Value>()))
		.saturating_add(record.anchor.stable_key.capacity().saturating_mul(size_of::<Value>()))
		.saturating_add(
			record
				.anchor
				.order_keys
				.iter()
				.chain(&record.anchor.stable_key)
				.map(Value::estimated_heap_bytes)
				.sum::<usize>(),
		)
}

/// Compares records.
pub fn compare_records(
	left: &CursorRecord,
	right: &CursorRecord,
	fields: &[SortField],
) -> Ordering {
	for ((left, right), field) in
		left.anchor.order_keys.iter().zip(&right.anchor.order_keys).zip(fields)
	{
		let left_null = matches!(left, Value::None | Value::Null);
		let right_null = matches!(right, Value::None | Value::Null);
		let ordering = if left_null && right_null {
			left.cmp(right)
		} else if left_null != right_null {
			if field.nulls_first {
				left_null.cmp(&right_null).reverse()
			} else {
				left_null.cmp(&right_null)
			}
		} else {
			let ordering = left.cmp(right);
			if field.descending {
				ordering.reverse()
			} else {
				ordering
			}
		};
		if ordering != Ordering::Equal {
			return ordering;
		}
	}
	left.anchor.stable_key.cmp(&right.anchor.stable_key)
}

/// Compares record anchor.
pub fn compare_record_anchor(
	left: &CursorRecord,
	right: &CursorAnchor,
	fields: &[SortField],
) -> Ordering {
	let right = CursorRecord {
		value: Value::None,
		anchor: right.clone(),
	};
	compare_records(left, &right, fields)
}

/// Sorts records.
fn sort_records(records: &mut [CursorRecord], fields: &[SortField]) {
	records.sort_by(|left, right| compare_records(left, right, fields));
}

/// Returns the invalid run result.
fn invalid_run(message: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

/// Returns the run io result.
fn run_io(operation: &str, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
/// Regression tests for spill behavior.
mod tests {
	use std::sync::Arc;

	use super::*;
	#[cfg(test)]
	use crate::test_assertions::AssertOption as _;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{
		gql::cursor::{CursorAnchor, CursorRecord},
		graph::{EndpointRole, PathDirection, PathStep, SnapshotId},
		values::{
			AtomKind,
			AtomOccurrenceRef,
			AtomRef,
			GraphId,
			GraphKind,
			GraphRef,
			GraphStorageMode,
			Path,
			PropertyDictionary,
			QueryCursor,
			storage::DiskGraphRef,
		},
	};

	/// Returns scratch for this module.
	fn scratch(name: &str) -> PathBuf {
		std::env::temp_dir().join(format!("fg-query-run-{name}-{}", crate::values::Uuid::now_v7()))
	}

	/// Returns record for this module.
	fn record(value: Value) -> CursorRecord {
		CursorRecord {
			anchor: CursorAnchor {
				order_keys: vec![Value::from(1_i64)],
				stable_key: vec![Value::from(1_i64)],
			},
			value,
		}
	}

	/// Verifies forced disk batches records into bounded runs.
	#[test]
	fn forced_disk_batches_records_into_bounded_runs() {
		let root = scratch("forced-batch");
		let mut sorter = QuerySorter::new(
			&root,
			Box::new(EncodeContext::new(PropertyDictionary::new())),
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			BarrierAccount::new(1024 * 1024),
			true,
		);
		for value in 0..128_i64 {
			sorter.push(record(Value::from(value))).assert_ok();
		}
		assert!(sorter.runs.is_empty(), "forced disk must not create one run per row");
		assert_eq!(sorter.buffered.len(), 128);
		assert!(matches!(sorter.finish().assert_ok(), SortedRecords::Disk { .. }));
		drop(fs::remove_dir_all(root));
	}

	/// Verifies descending order keeps none and null in canonical order.
	#[test]
	fn descending_order_keeps_none_and_null_in_canonical_order() {
		let fields = [SortField {
			descending: true,
			nulls_first: true,
		}];
		let mut records = [record(Value::Null), record(Value::None)];
		for (ordinal, record) in records.iter_mut().enumerate() {
			record.anchor.order_keys = vec![record.value.clone()];
			record.anchor.stable_key = vec![Value::from(ordinal)];
		}
		sort_records(&mut records, &fields);
		assert_eq!(records[0].value, Value::None);
		assert_eq!(records[1].value, Value::Null);
	}

	/// Verifies rejects corrupt runs and removes the session directory.
	#[test]
	fn rejects_corrupt_runs_and_removes_the_session_directory() {
		let root = scratch("corrupt");
		let mut sorter = QuerySorter::new(
			&root,
			Box::new(EncodeContext::new(PropertyDictionary::new())),
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			BarrierAccount::new(1),
			true,
		);
		sorter.push(record(Value::from("payload"))).assert_ok();
		let (spool, offset) = match sorter.finish().assert_ok() {
			SortedRecords::Disk {
				spool,
				offset,
			} => Some((spool, offset)),
			SortedRecords::Memory {
				..
			} => None,
		}
		.assert_some_with("forced sorter must create a disk run");
		let session = spool.path.parent().assert_some().to_owned();
		let mut bytes = fs::read(&spool.path).assert_ok();
		let last = bytes.last_mut().assert_some();
		*last ^= 0xff;
		fs::write(&spool.path, bytes).assert_ok();
		assert!(spool.page(offset, 1).is_err());
		drop(spool);
		assert!(!session.exists());
		drop(fs::remove_dir_all(root));
	}

	/// Verifies rejects runtime only values without leaking runs.
	#[test]
	fn rejects_runtime_only_values_without_leaking_runs() {
		let root = scratch("runtime-value");
		let mut sorter = QuerySorter::new(
			&root,
			Box::new(EncodeContext::new(PropertyDictionary::new())),
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			BarrierAccount::new(1),
			true,
		);
		let error = sorter.push(record(Value::from(QueryCursor::new(1, 1)))).assert_err();
		drop(sorter);
		assert_eq!(error.status().code().to_string(), "FG-22F21");
		let query_root = root.join("tmp").join("query");
		assert!(
			!query_root.exists() || fs::read_dir(&query_root).assert_ok().next().is_none(),
			"failed spill must not retain a query-run session"
		);
		drop(fs::remove_dir_all(root));
	}

	/// Verifies occurrence and path round trip through disk run.
	#[test]
	fn occurrence_and_path_round_trip_through_disk_run() {
		let root = scratch("occurrence-path");
		let graph = Arc::new(
			GraphRef::new(
				GraphId::new(9).assert_some(),
				"knowledge",
				GraphKind::Graph,
				GraphStorageMode::InMemory,
			)
			.assert_ok(),
		);
		let atom = AtomRef::new(AtomKind::Node, "person:1".parse().assert_ok());
		let occurrence = AtomOccurrenceRef::new(atom, graph.clone());
		let path = Path::new(
			crate::values::Uuid::new_v4(),
			SnapshotId::new(3, 4),
			vec![PathStep::new(
				occurrence.clone(),
				None,
				PathDirection::Forward,
				EndpointRole::Source,
			)],
			Some(0_i64.into()),
		);
		let expected = CursorRecord {
			anchor: CursorAnchor {
				order_keys: vec![Value::from(occurrence.clone())],
				stable_key: vec![Value::from(occurrence)],
			},
			value: Value::from(path),
		};
		let mut context = EncodeContext::new(PropertyDictionary::new());
		context.register_class_name(1, "person".into()).assert_ok();
		context.register_graph(graph.as_ref().clone(), DiskGraphRef::new(graph.id())).assert_ok();
		let mut sorter = QuerySorter::new(
			&root,
			Box::new(context),
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			BarrierAccount::new(1),
			true,
		);
		sorter.push(expected.clone()).assert_ok();
		let mut stream = sorter.finish().assert_ok().into_stream().assert_ok();
		let actual = stream.next().assert_ok().assert_some();
		assert_eq!(actual.anchor.order_keys, expected.anchor.order_keys);
		assert_eq!(actual.anchor.stable_key, expected.anchor.stable_key);
		assert_eq!(actual.value, expected.value);
		assert!(stream.next().assert_ok().is_none());
		drop(stream);
		drop(fs::remove_dir_all(root));
	}
}
