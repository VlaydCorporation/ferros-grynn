//! Bounded feed page preparation and catalog bindings for one WAL publication.

use std::{
	collections::{BTreeMap, BTreeSet},
	ops::Bound,
	sync::Arc,
};

use super::{
	LocalStore,
	layout::{FileImage, invalid},
};
use crate::{
	changefeed::{ChangeGraphState, ChangeIdentity, ChangeImage, ChangeOverlay},
	datastore::{CapturedCommit, DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	storage::{
		ChangefeedSegments,
		HistoryOwner,
		HistorySegment,
		catalog::{CatalogPath, DatabaseCatalog, FileCatalog, FileCatalogEntry},
		changefeed::{
			CodecBudget,
			SegmentHeader,
			encode_group,
			encode_header,
			encode_pages,
			payload_pages,
		},
		file_registry::{DataFileRole, FileRegistry, RegisteredDataFile},
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{DataFileId, DataFileKind},
	},
};

/// Rotation target; a complete oversized group occupies a segment by itself.
const SEGMENT_TARGET_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(test)]
thread_local! {
	/// Scoped test target exercises real rotation with small multi-page groups.
	static TEST_SEGMENT_TARGET: std::cell::Cell<u64> = const { std::cell::Cell::new(SEGMENT_TARGET_BYTES) };
}

/// Uses the fixed production target unless a test scopes a smaller rotation.
fn segment_target() -> u64 {
	#[cfg(test)]
	{
		TEST_SEGMENT_TARGET.with(std::cell::Cell::get)
	}
	#[cfg(not(test))]
	{
		SEGMENT_TARGET_BYTES
	}
}

/// Restores the per-thread rotation target after a focused lifecycle test.
#[cfg(test)]
pub(super) struct SegmentTargetGuard(u64);

#[cfg(test)]
impl SegmentTargetGuard {
	/// Selects a small target without altering production configuration.
	pub(super) fn set(target: u64) -> Self {
		Self(TEST_SEGMENT_TARGET.with(|current| current.replace(target)))
	}
}

#[cfg(test)]
impl Drop for SegmentTargetGuard {
	fn drop(&mut self) {
		TEST_SEGMENT_TARGET.with(|current| current.set(self.0));
	}
}
/// Aggregate encoder scratch including retained images and prepared feed pages.
const MAX_SCRATCH_BYTES: usize = 256 * 1024 * 1024;

/// One shared set of handles pinned by transactions before a later retirement.
pub(super) type ChangefeedFiles = Arc<BTreeMap<DataFileId, Arc<RegisteredDataFile>>>;

/// Segment files may be unavailable while their independent catalog bounds
/// remain sufficient to report a precise corruption GAP.
pub(super) fn degradable(
	files: &FileCatalog,
) -> rapidhash::RapidHashSet<(DataSpaceId, DataFileId)> {
	files
		.files
		.iter()
		.filter(|file| file.file_id.kind() == Some(DataFileKind::Changefeed))
		.map(|file| (file.space_id, file.file_id))
		.collect()
}

/// Checks authoritative source clocks against the recovered global commit,
/// including sources whose segments have already been purged or never existed.
pub(super) fn validate_recovered(catalog: &DatabaseCatalog, stamp: u64) -> FerrosGrynnResult<()> {
	for entry in &catalog.changefeeds {
		if [
			entry.defined_at,
			entry.published_at,
			entry.last_stamp,
			entry.purged_through,
			entry.closed_at.unwrap_or(0),
		]
		.into_iter()
		.any(|clock| clock > stamp)
			|| entry.segments.iter().any(|segment| segment.header.last_stamp > stamp)
		{
			return Err(invalid("changefeed catalog exceeds recovered commit clock"));
		}
	}
	for entry in &catalog.live_history {
		let starting_after = catalog
			.live_queries
			.iter()
			.find(|query| query.id == entry.id)
			.ok_or_else(|| invalid("LIVE history owner is absent"))?
			.starting_after;
		entry.metadata.validate(starting_after, stamp)?;
	}
	Ok(())
}

impl LocalStore {
	/// Shares the published physical descriptor vector with the logical root.
	pub(crate) fn changefeed_segments(&self) -> ChangefeedSegments {
		Arc::clone(&self.state.lock().changefeed_segments)
	}

	/// Shares computed-history bounds with the same immutable logical snapshot.
	pub(crate) fn live_segments(&self) -> crate::storage::LiveHistorySegments {
		Arc::clone(&self.state.lock().live_segments)
	}
}

/// Validates descriptor-to-file bindings before any optional segment read.
pub(super) fn bind_files(
	files: &FileCatalog,
	catalog: &DatabaseCatalog,
	registry: &FileRegistry,
) -> FerrosGrynnResult<ChangefeedFiles> {
	let count = history_segments(catalog).count();
	let mut budget = CodecBudget::new(MAX_SCRATCH_BYTES, 0)?;
	budget.admit(crate::values::storage::tree_allocation_bytes(
		count,
		std::mem::size_of::<(DataFileId, Arc<RegisteredDataFile>)>(),
	)?)?;
	let mut result = BTreeMap::new();
	let mut referenced = 0usize;
	for segment in history_segments(catalog) {
		referenced =
			referenced.checked_add(1).ok_or_else(|| invalid("feed binding count overflows"))?;
		let registered = files
			.files
			.iter()
			.find(|file| file.file_id == segment.file)
			.ok_or_else(|| invalid("feed segment is absent from authoritative file catalog"))?;
		if registered.space_id != DataSpaceId::Database
			|| registered.role != DataFileRole::WalBackfilled
			|| registered.companion_of.is_some()
			|| registered.path.as_str()
				!= format!("changefeed/{}/{}.fcf", segment.owner().uuid(), segment.header.segment)
		{
			return Err(invalid(
				"feed segment file binding disagrees with authoritative descriptor",
			));
		}
		if registry.contains(DataSpaceId::Database, segment.file) {
			result
				.insert(segment.file, registry.resolve_file(DataSpaceId::Database, segment.file)?);
		}
	}
	if referenced
		!= files
			.files
			.iter()
			.filter(|file| file.file_id.kind() == Some(DataFileKind::Changefeed))
			.count()
	{
		return Err(invalid("feed file catalog has an unreferenced segment"));
	}
	Ok(Arc::new(result))
}

/// Selects ordered unpublished result groups independently of registration
/// time. Persisted segment tails and purge floors exclude duplicate staging.
pub(super) fn fresh_live_groups(
	root: &DatabaseRoot,
	id: crate::live::LiveId,
) -> impl Iterator<Item = (&(crate::live::LiveId, u64), &Arc<[u8]>)> {
	let persisted = root
		.live_segments()
		.get(&id)
		.and_then(|segments| segments.last())
		.map_or(0, |segment| segment.bounds().1);
	let purged = root.live_catalog().histories.get(&id).map_or(0, |history| history.purged_through);
	root.live_history().range((
		Bound::Excluded((id, persisted.max(purged))),
		Bound::Included((id, root.commit_ts)),
	))
}

/// Retains reachable files and stages every admitted unpublished source/result
/// group. Existing payload pages are append-only; page zero is replaced.
pub(super) fn prepare(
	root: &DatabaseRoot,
	previous: &FileCatalog,
	files: &mut FileCatalog,
	catalog: &mut DatabaseCatalog,
	images: &mut Vec<FileImage>,
) -> FerrosGrynnResult<()> {
	let mut retained = 0usize;
	for entry in &catalog.changefeeds {
		if let Some(commit) = root.change_history().get(&(entry.id, root.commit_ts)) {
			retained = retained
				.checked_add(captured_bytes(commit)?)
				.ok_or_else(|| invalid("feed retained input bytes overflow"))?;
		}
	}
	for entry in &catalog.live_history {
		for (_, bytes) in fresh_live_groups(root, entry.id) {
			retained = retained
				.checked_add(bytes.len())
				.ok_or_else(|| invalid("LIVE retained input bytes overflow"))?;
		}
	}
	let mut budget = CodecBudget::new(MAX_SCRATCH_BYTES, retained)?;
	for entry in &mut catalog.changefeeds {
		if entry
			.segments
			.iter()
			.any(|segment| segment.owner() != HistoryOwner::Changefeed(entry.id))
		{
			return Err(invalid("source preparation received a segment with another owner"));
		}
		let Some(commit) = root.change_history().get(&(entry.id, root.commit_ts)) else {
			continue;
		};
		if commit.revision != entry.version {
			return Err(invalid("source group revision differs from final capture policy"));
		}
		let payload = encode_group(commit, &mut budget)?;
		append_group(
			root,
			HistoryOwner::Changefeed(entry.id),
			&payload,
			(root.commit_ts, root.committed_at_ns),
			&mut entry.segments,
			files,
			images,
			&mut budget,
		)?;
	}
	for entry in &mut catalog.live_history {
		if entry.segments.iter().any(|segment| segment.owner() != HistoryOwner::Live(entry.id)) {
			return Err(invalid("LIVE preparation received a segment with another owner"));
		}
		for ((_, stamp), payload) in fresh_live_groups(root, entry.id) {
			let info = crate::storage::live_history::inspect(payload, payload.len() as u64)?;
			let policy = entry
				.metadata
				.epochs
				.iter()
				.rev()
				.find(|epoch| epoch.started_at <= *stamp)
				.and_then(|epoch| epoch.policy);
			if info.id != entry.id
				|| info.version != entry.metadata.version
				|| info.stamp != *stamp
				|| info.time > root.committed_at_ns
				|| policy.is_none()
			{
				return Err(invalid("LIVE capture envelope disagrees with its publication bounds"));
			}
			append_group(
				root,
				HistoryOwner::Live(entry.id),
				payload,
				(info.stamp, info.time),
				&mut entry.segments,
				files,
				images,
				&mut budget,
			)?;
		}
	}
	let count = history_segments(catalog).count();
	budget.admit(crate::values::storage::tree_allocation_bytes(
		count,
		std::mem::size_of::<DataFileId>(),
	)?)?;
	let reachable = history_segments(catalog).map(|segment| segment.file).collect::<BTreeSet<_>>();
	files.files.retain(|entry| {
		entry.file_id.kind() != Some(DataFileKind::Changefeed) || reachable.contains(&entry.file_id)
	});
	for file in reachable {
		if files.files.iter().any(|entry| entry.file_id == file) {
			continue;
		}
		let registration = images
			.iter()
			.find(|image| image.entry.file_id == file)
			.map(|image| &image.entry)
			.or_else(|| previous.files.iter().find(|entry| entry.file_id == file))
			.ok_or_else(|| invalid("reachable feed segment is absent from the file catalog"))?;
		let registration = registration.clone();
		reserve(&mut files.files, 1, &mut budget)?;
		files.files.push(registration);
	}
	files.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
	files.encode()?;
	catalog.encode()?;
	Ok(())
}

/// Stages one whole result or source group through the existing WAL transport.
fn append_group(
	root: &DatabaseRoot,
	owner: HistoryOwner,
	payload: &[u8],
	clock: (u64, i128),
	segments: &mut Vec<HistorySegment>,
	files: &mut FileCatalog,
	images: &mut Vec<FileImage>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let (stamp, time) = clock;
	if stamp == 0 || stamp > root.commit_ts || time > root.committed_at_ns {
		return Err(invalid("feed group exceeds the publication clock"));
	}
	let group_pages = u64::from(payload_pages(payload.len())?);
	let append = segments.last().copied().filter(|segment| {
		segment
			.header
			.pages
			.checked_add(group_pages)
			.and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
			.is_some_and(|bytes| bytes <= segment_target())
	});
	// Page packing checks the complete payload before allocating
	// continuations.
	let first = append.map_or(1, |segment| segment.header.pages);
	let (locator, pages) = encode_pages(payload, stamp, first, budget)?;
	let mut segment = match append {
		Some(segment) => segment,
		None => {
			let file = DataFileId::new(files.next_file_instance_id, DataFileKind::Changefeed)
				.ok_or_else(|| invalid("feed file identity clock is exhausted"))?;
			files.next_file_instance_id = files
				.next_file_instance_id
				.checked_add(1)
				.ok_or_else(|| invalid("feed file identity clock is exhausted"))?;
			HistorySegment {
				file,
				header: SegmentHeader {
					database: root.database_uuid,
					owner,
					segment: uuid::Uuid::now_v7(),
					pages: 1,
					commits: 0,
					first_stamp: stamp,
					last_stamp: stamp,
					first_time: time,
					last_time: time,
				},
			}
		}
	};
	if segment.header.commits != 0
		&& (segment.header.last_stamp >= stamp || segment.header.last_time > time)
	{
		return Err(invalid("feed append clocks moved backwards"));
	}
	segment.header.pages = first
		.checked_add(u64::from(locator.pages))
		.ok_or_else(|| invalid("feed page ceiling overflows"))?;
	segment.header.commits = segment
		.header
		.commits
		.checked_add(1)
		.ok_or_else(|| invalid("feed group count overflows"))?;
	segment.header.last_stamp = stamp;
	segment.header.last_time = time;
	let registration = FileCatalogEntry {
		space_id: DataSpaceId::Database,
		file_id: segment.file,
		companion_of: None,
		role: DataFileRole::WalBackfilled,
		path: CatalogPath::parse(&format!(
			"changefeed/{}/{}.fcf",
			owner.uuid(),
			segment.header.segment
		))?,
	};
	let count =
		pages.len().checked_add(1).ok_or_else(|| invalid("feed staging page count overflows"))?;
	budget.admit(
		count
			.checked_mul(2 * PAGE_SIZE + std::mem::size_of::<PageNo>() + 512)
			.ok_or_else(|| invalid("feed staging bytes overflow"))?,
	)?;
	let mut final_pages = Vec::new();
	final_pages
		.try_reserve_exact(count)
		.map_err(|_error| invalid("feed staging allocation failed"))?;
	final_pages.resize_with(count, AlignedPage::zeroed);
	encode_header(segment.header, &mut final_pages[0])?;
	for (source, output) in pages.iter().zip(&mut final_pages[1..]) {
		output.as_bytes_mut().copy_from_slice(source.as_bytes());
	}
	let page_numbers = std::iter::once(PageNo::new(0))
		.chain((first..segment.header.pages).map(PageNo::new))
		.collect();
	reserve(images, 1, budget)?;
	images.push(FileImage {
		entry: registration,
		page_numbers,
		pages: final_pages,
		prewritten_page_count: None,
	});
	if append.is_some() {
		let last = segments.last_mut().ok_or_else(|| invalid("feed append segment disappeared"))?;
		*last = segment;
	} else {
		reserve(segments, 1, budget)?;
		segments.push(segment);
	}
	Ok(())
}

/// One authoritative reachability set covers source and private result files.
fn history_segments(catalog: &DatabaseCatalog) -> impl Iterator<Item = &HistorySegment> {
	catalog
		.changefeeds
		.iter()
		.flat_map(|entry| &entry.segments)
		.chain(catalog.live_history.iter().flat_map(|entry| &entry.segments))
}

/// Reserves complete replacement vector capacity before amortized growth.
fn reserve<T>(
	output: &mut Vec<T>,
	additional: usize,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<()> {
	let required = output
		.len()
		.checked_add(additional)
		.ok_or_else(|| invalid("feed vector count overflows"))?;
	if required <= output.capacity() {
		return Ok(());
	}
	let capacity = output
		.capacity()
		.max(4)
		.checked_mul(2)
		.map(|count| count.max(required))
		.ok_or_else(|| invalid("feed vector capacity overflows"))?;
	budget.admit(
		capacity
			.checked_mul(std::mem::size_of::<T>())
			.ok_or_else(|| invalid("feed vector bytes overflow"))?,
	)?;
	output
		.try_reserve_exact(capacity - output.len())
		.map_err(|_error| invalid("feed vector allocation failed"))
}

/// Charges owned capture inputs before physical encoding creates any copies.
fn captured_bytes(commit: &CapturedCommit) -> FerrosGrynnResult<usize> {
	let mut bytes = 512usize;
	for entry in
		commit.net.entries().iter().chain(commit.full.iter().flat_map(|batch| batch.entries()))
	{
		bytes = bytes
			.checked_add(std::mem::size_of_val(entry))
			.ok_or_else(|| invalid("feed input bytes overflow"))?;
		for image in [entry.before(), entry.after()].into_iter().flatten() {
			bytes = bytes
				.checked_add(image_bytes(image)?)
				.ok_or_else(|| invalid("feed input bytes overflow"))?;
		}
	}
	Ok(bytes)
}

/// Bounds historical names, values and qualified topology already held by
/// capture.
fn image_bytes(image: &ChangeImage) -> FerrosGrynnResult<usize> {
	let mut bytes = 4096usize;
	let atom = match image.identity() {
		ChangeIdentity::Record(atom) => atom,
		ChangeIdentity::Occurrence(occurrence) => {
			bytes = bytes
				.checked_add(occurrence.graph().name().len() + 256)
				.ok_or_else(|| invalid("feed image bytes overflow"))?;
			occurrence.atom()
		}
	};
	bytes = bytes
		.checked_add(crate::values::runtime::estimated_record_id_heap_bytes(atom.id()))
		.ok_or_else(|| invalid("feed record bytes overflow"))?;
	for (name, value) in image.properties().iter() {
		bytes = bytes
			.checked_add(256)
			.and_then(|bytes| bytes.checked_add(name.len()))
			.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
			.ok_or_else(|| invalid("feed property bytes overflow"))?;
	}
	for (name, overlay) in image.overlays() {
		let value = match overlay {
			ChangeOverlay::Hidden => 0,
			ChangeOverlay::Value(value) => value.estimated_heap_bytes(),
		};
		bytes = bytes
			.checked_add(256)
			.and_then(|bytes| bytes.checked_add(name.len()))
			.and_then(|bytes| bytes.checked_add(value))
			.ok_or_else(|| invalid("feed overlay bytes overflow"))?;
	}
	if let Some(label) = image.label() {
		bytes =
			bytes.checked_add(label.len()).ok_or_else(|| invalid("feed label bytes overflow"))?;
	}
	if let Some(ChangeGraphState::Edge {
		source,
		target,
		..
	}) = image.graph_state()
	{
		for endpoint in source.iter().chain(target) {
			for occurrence in std::iter::once(endpoint.occurrence()).chain(endpoint.owners()) {
				bytes = bytes
					.checked_add(512)
					.and_then(|bytes| bytes.checked_add(occurrence.graph().name().len()))
					.and_then(|bytes| {
						bytes.checked_add(crate::values::runtime::estimated_record_id_heap_bytes(
							occurrence.atom().id(),
						))
					})
					.ok_or_else(|| invalid("feed endpoint bytes overflow"))?;
			}
		}
	}
	Ok(bytes)
}
