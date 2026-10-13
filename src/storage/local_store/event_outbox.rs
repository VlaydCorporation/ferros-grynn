//! Strict invocation journal publication and recovery over shared history
//! pages.

use std::{collections::BTreeSet, sync::Arc};

use super::{
	LocalStore,
	history_publication::{append_group, reserve},
	layout::{FileImage, invalid},
};
use crate::{
	datastore::{ConflictKey, DataSpaceId, DatabaseRoot, EventExecution, EventOutbox},
	gql_status::FerrosGrynnResult,
	storage::{
		HistoryOwner,
		HistorySegment,
		catalog::{DatabaseCatalog, FileCatalog},
		database_superblock::DatabaseSuperblock,
		event_outbox::{decode_payload, journal},
		file_registry::{DataFileRole, FileRegistry},
		history_codec::{
			CodecBudget,
			pages::{decode_header, decode_locator, decode_pages},
		},
		hybrid_engine::HybridReadEngine,
		image_io::read_page_cached,
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{DataFileKind, PageKey, StorageSnapshot},
		wal::FrameWal,
	},
};

/// Whole-checkpoint decoding includes payloads, diagnostics and bounded map
/// copies.
const MAX_RECOVERY_SCRATCH_BYTES: usize = 512 * 1024 * 1024;
/// One accepted payload uses the ordinary query barrier ceiling.
const MAX_PAYLOAD_SCRATCH_BYTES: usize = 256 * 1024 * 1024;
/// Pending payloads (64 MiB), pending diagnostics (39.1 MiB), terminal
/// retention (8 MiB) and metadata produce less than 128 MiB of journal
/// capacity. Retained input, temporary pages and WAL staging together remain
/// below 640 MiB; the fixed ceiling keeps claim, completion and cancellation
/// writable at every limit.
const MAX_PUBLICATION_SCRATCH_BYTES: usize = 768 * 1024 * 1024;
/// A replacement checkpoint bounds replay work independently of retry volume.
const COMPACT_BYTES: u64 = 128 * 1024 * 1024;

#[cfg(test)]
thread_local! {
	/// Drives actual checkpoint publication without writing a 128 MiB journal first.
	static TEST_COMPACT_TARGET: std::cell::Cell<u64> = const { std::cell::Cell::new(COMPACT_BYTES) };
}

/// Uses the production replay ceiling except during a scoped checkpoint test.
fn compact_target() -> u64 {
	#[cfg(test)]
	{
		TEST_COMPACT_TARGET.with(std::cell::Cell::get)
	}
	#[cfg(not(test))]
	{
		COMPACT_BYTES
	}
}

/// Restores the per-thread compaction target after a real local publication
/// test.
#[cfg(test)]
pub(super) struct CompactionTargetGuard(u64);

#[cfg(test)]
impl CompactionTargetGuard {
	/// Changes only the threshold; the codec, pages and WAL remain production
	/// paths.
	pub(super) fn set(target: u64) -> Self {
		Self(TEST_COMPACT_TARGET.with(|current| current.replace(target)))
	}
}

#[cfg(test)]
impl Drop for CompactionTargetGuard {
	fn drop(&mut self) {
		TEST_COMPACT_TARGET.with(|current| current.set(self.0));
	}
}

impl LocalStore {
	/// Installs the already allocated descriptor binding after the WAL barrier.
	pub(crate) fn event_outbox_segments(&self) -> Arc<Vec<HistorySegment>> {
		Arc::clone(&self.state.lock().event_outbox_segments)
	}
}

/// Adds one complete logical publication and retains only reachable segments.
pub(super) fn prepare(
	root: &DatabaseRoot,
	previous_outbox: &Arc<EventOutbox>,
	previous_files: &FileCatalog,
	files: &mut FileCatalog,
	catalog: &mut DatabaseCatalog,
	images: &mut Vec<FileImage>,
) -> FerrosGrynnResult<()> {
	let retained_pages = images.iter().try_fold(0usize, |bytes, image| {
		bytes
			.checked_add(image.pages.len().saturating_mul(PAGE_SIZE))
			.ok_or_else(|| invalid("EVENT staged page bytes overflow"))
	})?;
	let retained = retained_bytes(root.event_outbox())?
		.checked_add(retained_pages)
		.ok_or_else(|| invalid("EVENT retained input bytes overflow"))?;
	let mut budget = CodecBudget::new(MAX_PUBLICATION_SCRATCH_BYTES, retained)?;
	if !Arc::ptr_eq(root.event_outbox(), previous_outbox) && root.event_outbox() != previous_outbox
	{
		let journal_bytes =
			catalog.event_outbox_segments.iter().try_fold(0u64, |bytes, segment| {
				bytes
					.checked_add(segment.header.pages.saturating_mul(PAGE_SIZE as u64))
					.ok_or_else(|| invalid("EVENT journal page bytes overflow"))
			})?;
		let checkpoint =
			catalog.event_outbox_segments.is_empty() || journal_bytes >= compact_target();
		let payload = journal::encode(
			(!checkpoint).then_some(previous_outbox.as_ref()),
			root.event_outbox(),
			root.commit_ts,
			&mut budget,
		)?;
		if checkpoint {
			catalog.event_outbox_segments.clear();
		}
		append_group(
			root,
			HistoryOwner::EventOutbox(root.database_uuid),
			&payload,
			(root.commit_ts, root.committed_at_ns),
			&mut catalog.event_outbox_segments,
			files,
			images,
			&mut budget,
		)?;
	}
	budget.admit(crate::values::storage::tree_allocation_bytes(
		catalog.event_outbox_segments.len(),
		std::mem::size_of::<crate::storage::types::DataFileId>(),
	)?)?;
	let reachable =
		catalog.event_outbox_segments.iter().map(|segment| segment.file).collect::<BTreeSet<_>>();
	files.files.retain(|entry| {
		entry.file_id.kind() != Some(DataFileKind::EventOutbox)
			|| reachable.contains(&entry.file_id)
	});
	for file in reachable {
		if files.files.iter().any(|entry| entry.file_id == file) {
			continue;
		}
		let registration = images
			.iter()
			.find(|image| image.entry.file_id == file)
			.map(|image| &image.entry)
			.or_else(|| previous_files.files.iter().find(|entry| entry.file_id == file))
			.ok_or_else(|| invalid("reachable EVENT journal is absent from file catalog"))?
			.clone();
		reserve(&mut files.files, 1, &mut budget)?;
		files.files.push(registration);
	}
	files.files.sort_unstable_by_key(|entry| (entry.space_id, entry.file_id));
	files.encode()?;
	catalog.encode()?;
	Ok(())
}

/// Requires exact catalog/file bindings; invocation journals cannot degrade to
/// GAP.
pub(super) fn validate_files(
	files: &FileCatalog,
	catalog: &DatabaseCatalog,
	registry: &FileRegistry,
) -> FerrosGrynnResult<()> {
	for segment in &catalog.event_outbox_segments {
		let registered =
			files.files.iter().find(|file| file.file_id == segment.file).ok_or_else(|| {
				invalid("EVENT journal is absent from authoritative file catalog")
			})?;
		if registered.space_id != DataSpaceId::Database
			|| registered.role != DataFileRole::WalBackfilled
			|| registered.companion_of.is_some()
			|| registered.path.as_str()
				!= format!("events/{}/{}.feo", catalog.database_uuid, segment.header.segment)
			|| !registry.contains(DataSpaceId::Database, segment.file)
		{
			return Err(invalid("EVENT journal binding differs or its file is absent"));
		}
	}
	if catalog.event_outbox_segments.len()
		!= files
			.files
			.iter()
			.filter(|file| file.file_id.kind() == Some(DataFileKind::EventOutbox))
			.count()
	{
		return Err(invalid("EVENT file catalog has an unreferenced journal"));
	}
	Ok(())
}

/// Replays whole publications into a private candidate before exposing any
/// root.
pub(super) fn recover(
	root: &mut DatabaseRoot,
	catalog: &DatabaseCatalog,
	files: &FileCatalog,
	registry: &FileRegistry,
	wal: &FrameWal,
	engine: &HybridReadEngine,
	snapshot: StorageSnapshot,
	authority: &DatabaseSuperblock,
) -> FerrosGrynnResult<()> {
	validate_files(files, catalog, registry)?;
	let mut candidate = EventOutbox::default();
	let mut previous_stamp = 0;
	for segment in &catalog.event_outbox_segments {
		if segment.header.last_stamp > root.commit_ts
			|| segment.header.last_time > root.committed_at_ns
		{
			return Err(invalid("EVENT journal exceeds recovered publication clock"));
		}
		let mut read = |page, output: &mut AlignedPage| {
			read_page_cached(
				wal,
				registry,
				engine,
				snapshot,
				authority.generation,
				authority.system_backfilled_lsn,
				PageKey::new(DataSpaceId::Database, segment.file, page),
				output,
			)
		};
		let mut page = AlignedPage::zeroed();
		read(PageNo::new(0), &mut page)?;
		if decode_header(&page)? != segment.header {
			return Err(invalid("EVENT journal header differs from catalog"));
		}
		let mut first = 1u64;
		let mut count = 0u64;
		while first < segment.header.pages {
			read(PageNo::new(first), &mut page)?;
			let locator = decode_locator(&page, PageNo::new(first), segment.header.pages)?;
			if locator.stamp <= previous_stamp
				|| count == 0 && locator.stamp != segment.header.first_stamp
				|| locator.stamp > segment.header.last_stamp
			{
				return Err(invalid("EVENT journal group clocks disagree with catalog"));
			}
			let retained = retained_bytes(&candidate)?;
			let mut budget = CodecBudget::new(MAX_RECOVERY_SCRATCH_BYTES, retained)?;
			let bytes = decode_pages(locator, segment.header.pages, &mut read, &mut budget)?;
			if previous_stamp == 0 && bytes.get(6) != Some(&1) {
				return Err(invalid("EVENT journal starts without a complete checkpoint"));
			}
			journal::decode(&bytes, &mut candidate, locator.stamp, &mut budget)?;
			previous_stamp = locator.stamp;
			first = first
				.checked_add(u64::from(locator.pages))
				.ok_or_else(|| invalid("EVENT page cursor overflows"))?;
			count += 1;
		}
		if count != segment.header.commits || previous_stamp != segment.header.last_stamp {
			return Err(invalid("EVENT journal complete group count differs"));
		}
	}
	// Pending corruption remains fatal even if the corresponding definition was
	// removed.
	for invocation in candidate.invocations.values() {
		if invocation.updated_at_ns > root.committed_at_ns {
			return Err(invalid("EVENT lifecycle time exceeds recovered clock"));
		}
		if let Some(bytes) = &invocation.payload {
			let decoded = decode_payload(bytes, MAX_PAYLOAD_SCRATCH_BYTES)?;
			let EventExecution::Async(options) = decoded.definition.execution() else {
				return Err(invalid("pending EVENT payload lost ASYNC options"));
			};
			if decoded.definition.id() != Some(invocation.event)
				|| decoded.definition.version() != invocation.version
				|| decoded.definition.target() != invocation.target
				|| decoded.definition.published_at > invocation.accepted_at
				|| options.retries() != invocation.retries
				|| invocation.ceiling > options.max_depth()
			{
				return Err(invalid("pending EVENT descriptor differs from invocation identity"));
			}
		}
		root.mark_version(ConflictKey::EventInvocation(invocation.id), invocation.updated_at);
	}
	root.mark_version(ConflictKey::EventOutboxRange, root.commit_ts);
	*root.event_outbox_mut() = Arc::new(candidate);
	root.set_event_outbox_segments(Arc::new(catalog.event_outbox_segments.clone()));
	Ok(())
}

/// Counts pending bytes and bounded diagnostics already held while encoding.
fn retained_bytes(outbox: &EventOutbox) -> FerrosGrynnResult<usize> {
	outbox.invocations.values().try_fold(outbox.pending_bytes, |bytes, invocation| {
		bytes
			.checked_add(512 + invocation.diagnostic.as_ref().map_or(0, crate::values::Text::len))
			.ok_or_else(|| invalid("EVENT retained metadata bytes overflow"))
	})
}
