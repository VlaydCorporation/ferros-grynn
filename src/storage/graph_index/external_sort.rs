//! Bounded-memory Direct-I/O runs for immutable index generation builds.

use std::{
	cmp::Reverse,
	collections::BinaryHeap,
	fs,
	mem::size_of,
	path::{Path, PathBuf},
};

use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	storage::{
		direct::DirectFile,
		page::{AlignedPage, PAGE_SIZE, PageNo},
	},
	values::storage::DiskAtomRef,
};

/// Default in-memory payload retained before one sorted run is spilled.
pub(super) const INDEX_SORT_RUN_BYTES: usize = 256 * 1024 * 1024;
/// Maximum number of run readers participating in one merge pass.
const INDEX_SORT_FAN_IN: usize = 64;
/// Fixed record prefix: key length followed by the physical atom reference.
const RUN_RECORD_PREFIX_BYTES: usize = size_of::<u32>() + size_of::<u64>();

/// One total-order item consumed by primary and scalar index packers.
pub(in crate::storage) struct IndexSortRecord {
	/// Order-preserving or primary lookup bytes.
	pub(super) key: Vec<u8>,
	/// Stable generation-local physical atom reference.
	pub(super) atom: DiskAtomRef,
}

/// Bounded accumulator that spills sorted runs only when its budget is full.
pub(in crate::storage) struct ExternalIndexSorter {
	temporary_root: PathBuf,
	run_budget: usize,
	buffered_bytes: usize,
	buffered: Vec<IndexSortRecord>,
	session: Option<SortSession>,
	runs: Vec<SortRun>,
	next_run: u64,
}

impl ExternalIndexSorter {
	/// Creates a sorter using the normative 256 MiB run budget.
	pub(in crate::storage) fn new(database_root: &Path) -> Self {
		Self::with_budget(database_root, INDEX_SORT_RUN_BYTES)
	}

	/// Creates a sorter with an explicit budget for deterministic tests.
	pub(super) fn with_budget(database_root: &Path, run_budget: usize) -> Self {
		Self {
			temporary_root: database_root.join("tmp").join("index-sort"),
			run_budget: run_budget.max(1),
			buffered_bytes: 0,
			buffered: Vec::new(),
			session: None,
			runs: Vec::new(),
			next_run: 0,
		}
	}

	/// Adds one encoded key/reference pair, spilling the previous run first
	/// when retaining this item would exceed the configured budget.
	pub(in crate::storage) fn push(
		&mut self,
		key: Vec<u8>,
		atom: DiskAtomRef,
	) -> FerrosGrynnResult<()> {
		if key.is_empty() || key.len() >= PAGE_SIZE {
			return Err(invalid_sort("external-sort key does not fit one B+tree page"));
		}
		let bytes = size_of::<IndexSortRecord>()
			.checked_add(key.len())
			.ok_or_else(|| invalid_sort("external-sort item size overflows usize"))?;
		if !self.buffered.is_empty()
			&& self.buffered_bytes.checked_add(bytes).is_none_or(|total| total > self.run_budget)
		{
			self.spill_buffered()?;
		}
		self.buffered_bytes = self
			.buffered_bytes
			.checked_add(bytes)
			.ok_or_else(|| invalid_sort("external-sort run size overflows usize"))?;
		self.buffered.push(IndexSortRecord {
			key,
			atom,
		});
		if self.buffered_bytes >= self.run_budget {
			self.spill_buffered()?;
		}
		Ok(())
	}

	/// Finishes all required merge passes and returns a replayable sorted
	/// input.
	pub(in crate::storage) fn finish(mut self) -> FerrosGrynnResult<SortedIndexRecords> {
		if self.runs.is_empty() {
			sort_records(&mut self.buffered);
			return Ok(SortedIndexRecords::Memory(self.buffered));
		}
		self.spill_buffered()?;
		while self.runs.len() > INDEX_SORT_FAN_IN {
			let input = std::mem::take(&mut self.runs);
			for group in input.chunks(INDEX_SORT_FAN_IN) {
				let output = self.new_run_path()?;
				let mut writer = RunWriter::create(output)?;
				merge_runs(group, |record| writer.push(&record))?;
				self.runs.push(writer.finish()?);
				for run in group {
					fs::remove_file(&run.path)
						.map_err(|source| sort_io("remove merged index-sort run", source))?;
				}
			}
		}
		let session = self
			.session
			.take()
			.ok_or_else(|| invalid_sort("spilled sorter has no temporary session"))?;
		Ok(SortedIndexRecords::Runs {
			_session: session,
			runs: self.runs,
		})
	}

	/// Sorts and writes the current bounded in-memory run.
	fn spill_buffered(&mut self) -> FerrosGrynnResult<()> {
		if self.buffered.is_empty() {
			return Ok(());
		}
		sort_records(&mut self.buffered);
		let path = self.new_run_path()?;
		let mut writer = RunWriter::create(path)?;
		for record in &self.buffered {
			writer.push(record)?;
		}
		self.runs.push(writer.finish()?);
		self.buffered.clear();
		self.buffered_bytes = 0;
		Ok(())
	}

	/// Allocates one collision-free filename below the owned session directory.
	fn new_run_path(&mut self) -> FerrosGrynnResult<PathBuf> {
		if self.session.is_none() {
			self.session = Some(SortSession::create(&self.temporary_root)?);
		}
		let ordinal = self.next_run;
		self.next_run = self
			.next_run
			.checked_add(1)
			.ok_or_else(|| invalid_sort("external-sort run ordinal overflows u64"))?;
		Ok(self
			.session
			.as_ref()
			.ok_or_else(|| invalid_sort("external-sort session disappeared"))?
			.path
			.join(format!("{ordinal:016x}.run")))
	}
}

/// Replayable output used directly by bottom-up B+tree packers.
pub(in crate::storage) enum SortedIndexRecords {
	/// Inputs below one run budget need no temporary I/O.
	Memory(Vec<IndexSortRecord>),
	/// Larger inputs retain only the final bounded merge inputs.
	Runs {
		/// Owns best-effort cleanup for all final runs.
		_session: SortSession,
		/// At most `INDEX_SORT_FAN_IN` runs after merge passes.
		runs: Vec<SortRun>,
	},
}

impl SortedIndexRecords {
	/// Visits every item in `(key, atom)` order without collecting the result.
	pub(in crate::storage) fn visit(
		&self,
		mut visit: impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory(records) => {
				for record in records {
					visit(&record.key, record.atom)?;
				}
				Ok(())
			}
			Self::Runs {
				runs,
				..
			} => merge_runs(runs, |record| visit(&record.key, record.atom)),
		}
	}
}

/// One owned temporary session removed on success, error, or unwind.
pub(in crate::storage) struct SortSession {
	path: PathBuf,
}

impl SortSession {
	/// Creates one unique session below the database-owned scratch directory.
	fn create(root: &Path) -> FerrosGrynnResult<Self> {
		fs::create_dir_all(root)
			.map_err(|source| sort_io("create index-sort temporary root", source))?;
		let path = root.join(uuid::Uuid::now_v7().to_string());
		fs::create_dir(&path).map_err(|source| sort_io("create index-sort session", source))?;
		Ok(Self {
			path,
		})
	}
}

impl Drop for SortSession {
	/// Best-effort cleanup is safe because the directory identity is unique to
	/// this sorter and never becomes catalog-reachable.
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.path));
	}
}

/// One immutable run and its meaningful byte length before final zero padding.
pub(in crate::storage) struct SortRun {
	path: PathBuf,
	logical_bytes: u64,
}

/// Sequential page-aligned writer for the private run format.
struct RunWriter {
	path: PathBuf,
	file: DirectFile,
	page: AlignedPage,
	page_no: u64,
	offset: usize,
	logical_bytes: u64,
}

impl RunWriter {
	/// Creates one exact new run file.
	fn create(path: PathBuf) -> FerrosGrynnResult<Self> {
		let file = DirectFile::create_new(&path)?;
		Ok(Self {
			path,
			file,
			page: AlignedPage::zeroed(),
			page_no: 0,
			offset: 0,
			logical_bytes: 0,
		})
	}

	/// Appends one bounded record to the aligned byte stream.
	fn push(&mut self, record: &IndexSortRecord) -> FerrosGrynnResult<()> {
		let key_len = u32::try_from(record.key.len())
			.map_err(|_error| invalid_sort("external-sort key length exceeds u32"))?;
		self.write_bytes(&key_len.to_le_bytes())?;
		self.write_bytes(&record.atom.raw().to_le_bytes())?;
		self.write_bytes(&record.key)
	}

	/// Writes a byte slice across as many aligned pages as required.
	fn write_bytes(&mut self, mut bytes: &[u8]) -> FerrosGrynnResult<()> {
		while !bytes.is_empty() {
			let count = bytes.len().min(PAGE_SIZE - self.offset);
			self.page.as_bytes_mut()[self.offset..self.offset + count]
				.copy_from_slice(&bytes[..count]);
			self.offset += count;
			self.logical_bytes = self
				.logical_bytes
				.checked_add(count as u64)
				.ok_or_else(|| invalid_sort("external-sort run length overflows u64"))?;
			bytes = &bytes[count..];
			if self.offset == PAGE_SIZE {
				self.flush_page()?;
			}
		}
		Ok(())
	}

	/// Appends the current complete or zero-padded physical page.
	fn flush_page(&mut self) -> FerrosGrynnResult<()> {
		self.file.write_page_raw(PageNo::new(self.page_no), &self.page)?;
		self.page_no = self
			.page_no
			.checked_add(1)
			.ok_or_else(|| invalid_sort("external-sort page number overflows u64"))?;
		self.offset = 0;
		self.page.as_bytes_mut().fill(0);
		Ok(())
	}

	/// Flushes the final padding and makes the run durable for the merge pass.
	fn finish(mut self) -> FerrosGrynnResult<SortRun> {
		if self.offset != 0 {
			self.flush_page()?;
		}
		self.file.sync_data()?;
		Ok(SortRun {
			path: self.path,
			logical_bytes: self.logical_bytes,
		})
	}
}

/// Sequential bounded decoder for one private run.
struct RunReader {
	file: DirectFile,
	logical_bytes: u64,
	position: u64,
	loaded_page: Option<u64>,
	page: AlignedPage,
}

impl RunReader {
	/// Opens one already synchronized run.
	fn open(run: &SortRun) -> FerrosGrynnResult<Self> {
		Ok(Self {
			file: DirectFile::open_read_only(&run.path)?,
			logical_bytes: run.logical_bytes,
			position: 0,
			loaded_page: None,
			page: AlignedPage::zeroed(),
		})
	}

	/// Decodes the next complete record, rejecting impossible bounded lengths.
	fn next(&mut self) -> FerrosGrynnResult<Option<IndexSortRecord>> {
		if self.position == self.logical_bytes {
			return Ok(None);
		}
		let mut prefix = [0_u8; RUN_RECORD_PREFIX_BYTES];
		self.read_exact(&mut prefix)?;
		let key_len =
			usize::try_from(u32::from_le_bytes(prefix[..4].try_into().map_err(|_error| {
				invalid_sort("external-sort key prefix has an invalid length")
			})?))
			.map_err(|_error| invalid_sort("external-sort key length does not fit usize"))?;
		if key_len == 0 || key_len >= PAGE_SIZE {
			return Err(invalid_sort("external-sort run contains an invalid key length"));
		}
		let atom =
			DiskAtomRef::from_raw(u64::from_le_bytes(prefix[4..].try_into().map_err(
				|_error| invalid_sort("external-sort atom prefix has an invalid length"),
			)?))?;
		let mut key = Vec::new();
		key.try_reserve_exact(key_len)
			.map_err(|source| invalid_sort(format!("reserve external-sort key: {source}")))?;
		key.resize(key_len, 0);
		self.read_exact(&mut key)?;
		Ok(Some(IndexSortRecord {
			key,
			atom,
		}))
	}

	/// Reads exactly one logical byte slice across aligned pages.
	fn read_exact(&mut self, mut output: &mut [u8]) -> FerrosGrynnResult<()> {
		let requested = u64::try_from(output.len())
			.map_err(|_error| invalid_sort("external-sort read length exceeds u64"))?;
		if self.position.checked_add(requested).is_none_or(|end| end > self.logical_bytes) {
			return Err(invalid_sort("external-sort run ends inside a record"));
		}
		while !output.is_empty() {
			let page_no = self.position / PAGE_SIZE as u64;
			if self.loaded_page != Some(page_no) {
				self.file.read_page_raw(PageNo::new(page_no), &mut self.page)?;
				self.loaded_page = Some(page_no);
			}
			let offset = usize::try_from(self.position % PAGE_SIZE as u64)
				.map_err(|_error| invalid_sort("external-sort page offset exceeds usize"))?;
			let count = output.len().min(PAGE_SIZE - offset);
			output[..count].copy_from_slice(&self.page.as_bytes()[offset..offset + count]);
			self.position += count as u64;
			output = &mut output[count..];
		}
		Ok(())
	}
}

/// Merges already sorted runs through a standard-library min-heap.
fn merge_runs(
	runs: &[SortRun],
	mut visit: impl FnMut(IndexSortRecord) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	if runs.len() > INDEX_SORT_FAN_IN {
		return Err(invalid_sort("external-sort merge fan-in exceeds 64"));
	}
	let mut readers = runs.iter().map(RunReader::open).collect::<FerrosGrynnResult<Vec<_>>>()?;
	let mut heap = BinaryHeap::<Reverse<(Vec<u8>, u64, usize)>>::new();
	for (index, reader) in readers.iter_mut().enumerate() {
		if let Some(record) = reader.next()? {
			heap.push(Reverse((record.key, record.atom.raw(), index)));
		}
	}
	while let Some(Reverse((key, atom, index))) = heap.pop() {
		visit(IndexSortRecord {
			key,
			atom: DiskAtomRef::from_raw(atom)?,
		})?;
		if let Some(record) = readers[index].next()? {
			heap.push(Reverse((record.key, record.atom.raw(), index)));
		}
	}
	Ok(())
}

/// Applies the complete deterministic total order within one run.
fn sort_records(records: &mut [IndexSortRecord]) {
	records.sort_unstable_by(|left, right| {
		left.key.cmp(&right.key).then_with(|| left.atom.cmp(&right.atom))
	});
}

/// Removes scratch left by a process that exited before RAII cleanup.
pub(in crate::storage) fn cleanup_external_index_sort(
	database_root: &Path,
) -> FerrosGrynnResult<()> {
	let root = database_root.join("tmp").join("index-sort");
	match fs::remove_dir_all(root) {
		Ok(()) => Ok(()),
		Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
		Err(source) => Err(sort_io("remove stale index-sort scratch", source)),
	}
}

/// Creates a typed format error for private run or ordering failures.
fn invalid_sort(message: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

/// Preserves one native scratch-I/O cause behind the storage boundary.
fn sort_io(operation: &str, source: std::io::Error) -> FerrosGrynnError {
	FerrosGrynnError::storage_io_failure(operation).with_technical_source(source)
}

#[cfg(test)]
mod tests {
	use std::sync::atomic::{AtomicU64, Ordering};

	use super::*;
	use crate::values::runtime::graph::AtomKind;

	/// Unique test-directory suffix within one process.
	static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

	/// Forced tiny runs merge into one replayable total order.
	#[test]
	fn tiny_runs_merge_and_cleanup() -> FerrosGrynnResult<()> {
		let root = std::env::current_dir()
			.map_err(|source| sort_io("resolve sorter test root", source))?
			.join(format!(
				".ferros-grynn-sort-{}-{}",
				std::process::id(),
				NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
			));
		fs::create_dir(&root).map_err(|source| sort_io("create sorter test root", source))?;
		let scratch = root.join("tmp").join("index-sort");
		let result = (|| {
			let mut sorter = ExternalIndexSorter::with_budget(&root, 40);
			for slot in (0..200_u64).rev() {
				sorter.push(
					format!("key-{slot:04}").into_bytes(),
					DiskAtomRef::from_parts(AtomKind::Node, slot)?,
				)?;
			}
			let sorted = sorter.finish()?;
			let mut slots = Vec::new();
			sorted.visit(|_key, atom| {
				slots.push(atom.slot());
				Ok(())
			})?;
			assert_eq!(slots, (0..200).collect::<Vec<_>>());
			drop(sorted);
			assert!(scratch.read_dir().is_ok_and(|mut entries| entries.next().is_none()));
			Ok(())
		})();
		drop(fs::remove_dir_all(&root));
		result
	}
}
