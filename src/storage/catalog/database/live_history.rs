//! Checked private LIVE capture epochs and authoritative physical references.

use super::{
	LiveHistoryCatalogEntry,
	codec::{Decoder, invalid},
};
use crate::{
	changefeed::{ChangefeedConfig, ChangefeedId},
	datastore::{LiveCaptureEpoch, LiveCapturePolicy, LiveHistoryMetadata, SchemaVersion},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveId, LiveVersion},
	storage::history_codec::CodecBudget,
};

/// Fixed header and mandatory first capture epoch.
pub(super) const MIN_BYTES: usize = 128;
/// Fixed fields preceding the variable sections.
const HEADER_BYTES: usize = 80;
/// Inclusive clock, presence, source revision, UUID and fixed duration.
const EPOCH_BYTES: usize = 48;

/// Admits complete canonical bytes before any buffer growth.
pub(super) fn encode(
	output: &mut Vec<u8>,
	entry: &LiveHistoryCatalogEntry,
) -> FerrosGrynnResult<()> {
	let extra = HEADER_BYTES
		.checked_add(
			entry
				.metadata
				.epochs
				.len()
				.checked_mul(EPOCH_BYTES)
				.ok_or_else(|| invalid("LIVE epoch bytes overflow"))?,
		)
		.and_then(|bytes| {
			bytes.checked_add(entry.segments.len().checked_mul(super::history_segment::BYTES)?)
		})
		.ok_or_else(|| invalid("LIVE history section bytes overflow"))?;
	let required =
		output.len().checked_add(extra).ok_or_else(|| invalid("LIVE catalog length overflows"))?;
	if required > 16 * 1024 * 1024 {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"database catalog bytes",
			required as u64,
			16 * 1024 * 1024,
		));
	}
	let epochs = u32::try_from(entry.metadata.epochs.len())
		.map_err(|_error| invalid("LIVE epoch count overflows"))?;
	let segments = u32::try_from(entry.segments.len())
		.map_err(|_error| invalid("LIVE segment count overflows"))?;
	output.try_reserve_exact(extra).map_err(|_error| invalid("LIVE catalog allocation failed"))?;
	output.extend_from_slice(entry.id.as_uuid().as_inner().as_bytes());
	let metadata = &entry.metadata;
	for number in [metadata.version.get(), metadata.last_stamp, metadata.purged_through] {
		output.extend_from_slice(&number.to_le_bytes());
	}
	output.extend_from_slice(&metadata.purged_time_ns.to_le_bytes());
	for number in [metadata.retained_page, metadata.retained_groups] {
		output.extend_from_slice(&number.to_le_bytes());
	}
	output.extend_from_slice(&epochs.to_le_bytes());
	output.extend_from_slice(&segments.to_le_bytes());
	for epoch in &metadata.epochs {
		output.extend_from_slice(&epoch.started_at.to_le_bytes());
		output.push(u8::from(epoch.policy.is_some()));
		output.extend_from_slice(&[0; 3]);
		output.extend_from_slice(
			&epoch.policy.map_or(0, |policy| policy.revision.get()).to_le_bytes(),
		);
		output.extend_from_slice(
			epoch
				.policy
				.map_or(uuid::Uuid::nil(), |policy| *policy.source.as_uuid().as_inner())
				.as_bytes(),
		);
		output
			.extend_from_slice(&epoch.policy.map_or(0, |policy| policy.retention_ns).to_le_bytes());
	}
	for segment in &entry.segments {
		super::history_segment::encode(output, *segment);
	}
	Ok(())
}

/// Checks counts, lengths and reserved bytes before allocating owned metadata.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	budget: &mut CodecBudget,
) -> FerrosGrynnResult<LiveHistoryCatalogEntry> {
	let id = LiveId::from_uuid(uuid(decoder)?.into())
		.map_err(|_error| invalid("LIVE history identity is nil"))?;
	let version = LiveVersion::new(decoder.read_u64()?)
		.map_err(|_error| invalid("LIVE history version is zero"))?;
	let last_stamp = decoder.read_u64()?;
	let purged_through = decoder.read_u64()?;
	let purged_time_ns = time(decoder)?;
	let retained_page = decoder.read_u64()?;
	let retained_groups = decoder.read_u64()?;
	let epoch_count = decoder.read_count("LIVE capture epoch count")?;
	let segment_count = decoder.read_count("LIVE history segment count")?;
	let bytes = epoch_count
		.checked_mul(EPOCH_BYTES)
		.and_then(|bytes| {
			bytes.checked_add(segment_count.checked_mul(super::history_segment::BYTES)?)
		})
		.ok_or_else(|| invalid("LIVE history counts overflow"))?;
	if epoch_count == 0 || bytes > decoder.remaining_bytes() {
		return Err(invalid("LIVE history counts exceed remaining bytes"));
	}
	budget.admit(
		epoch_count
			.checked_mul(std::mem::size_of::<LiveCaptureEpoch>())
			.and_then(|bytes| {
				bytes.checked_add(
					segment_count
						.checked_mul(std::mem::size_of::<crate::storage::HistorySegment>())?,
				)
			})
			.ok_or_else(|| invalid("LIVE history allocation overflows"))?,
	)?;
	let mut epochs = Vec::new();
	epochs
		.try_reserve_exact(epoch_count)
		.map_err(|_error| invalid("LIVE epoch allocation failed"))?;
	for _ in 0..epoch_count {
		let started_at = decoder.read_u64()?;
		let presence = decoder.read_u8()?;
		decoder.expect_zero(3, "LIVE epoch reserved bytes")?;
		let revision = decoder.read_u32()?;
		let source = uuid(decoder)?;
		let retention_ns = time(decoder)?;
		let policy = match presence {
			0 if revision == 0 && source.is_nil() && retention_ns == 0 => None,
			1 => {
				ChangefeedConfig::from_nanoseconds(retention_ns)
					.map_err(|_error| invalid("LIVE epoch retention is invalid"))?;
				Some(LiveCapturePolicy {
					source: ChangefeedId::from_uuid(source.into())
						.map_err(|_error| invalid("LIVE epoch source is nil"))?,
					revision: SchemaVersion::from_storage(revision)?,
					retention_ns,
				})
			}
			_ => return Err(invalid("LIVE epoch presence or absent fields are invalid")),
		};
		epochs.push(LiveCaptureEpoch {
			started_at,
			policy,
		});
	}
	let mut segments = Vec::new();
	segments
		.try_reserve_exact(segment_count)
		.map_err(|_error| invalid("LIVE segment allocation failed"))?;
	for _ in 0..segment_count {
		segments.push(super::history_segment::decode(decoder)?);
	}
	Ok(LiveHistoryCatalogEntry {
		id,
		metadata: LiveHistoryMetadata {
			version,
			last_stamp,
			purged_through,
			purged_time_ns,
			retained_page,
			retained_groups,
			epochs,
		},
		segments,
	})
}

/// Reads UUID bytes; each enclosing field enforces its presence semantics.
fn uuid(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<uuid::Uuid> {
	uuid::Uuid::from_slice(decoder.read_exact(16)?)
		.map_err(|_error| invalid("LIVE history UUID is malformed"))
}

/// Reads exact integer nanoseconds without numeric conversion.
fn time(decoder: &mut Decoder<'_>) -> FerrosGrynnResult<i128> {
	Ok(i128::from_le_bytes(
		decoder
			.read_exact(16)?
			.try_into()
			.map_err(|_error| invalid("LIVE history time is truncated"))?,
	))
}

/// Validates private owners against subscriptions and the shared file
/// namespace.
pub(super) fn validate_entries(
	catalog: &super::DatabaseCatalog,
	files: &mut std::collections::BTreeSet<crate::storage::types::DataFileId>,
	segments: &mut std::collections::BTreeSet<uuid::Uuid>,
) -> FerrosGrynnResult<()> {
	if catalog.live_history.is_empty() {
		return Ok(());
	}
	let feeds = catalog.changefeeds.len();
	let feed_height = usize::BITS - feeds.leading_zeros();
	let query_height = usize::BITS - catalog.live_queries.len().leading_zeros();
	let work = feeds
		.checked_mul(feed_height as usize + 1)
		.and_then(|work| {
			work.checked_add(
				catalog
					.live_history
					.len()
					.checked_mul(feed_height as usize + query_height as usize + 2)?,
			)
		})
		.ok_or_else(|| invalid("LIVE catalog validation work overflows"))?;
	if work > 1_000_000 {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"LIVE catalog validation work",
			work as u64,
			1_000_000,
		));
	}
	let mut budget = CodecBudget::new(256 * 1024 * 1024, 0)?;
	budget.admit(crate::values::storage::tree_allocation_bytes(
		feeds,
		std::mem::size_of::<(crate::changefeed::ChangefeedSource, &super::ChangefeedCatalogEntry)>(
		),
	)?)?;
	let sources = catalog
		.changefeeds
		.iter()
		.map(|feed| (feed.source, feed))
		.collect::<std::collections::BTreeMap<_, _>>();
	let mut previous = None;
	for entry in &catalog.live_history {
		if previous.is_some_and(|id| id >= entry.id) {
			return Err(invalid("LIVE history owners are not strictly ordered"));
		}
		previous = Some(entry.id);
		let at = catalog
			.live_queries
			.binary_search_by_key(&entry.id, |query| query.id)
			.map_err(|_error| invalid("LIVE history owner is absent"))?;
		let query = &catalog.live_queries[at];
		let metadata = &entry.metadata;
		let source = query.capture_source;
		let final_policy = if query.state == crate::live::LiveState::Closed {
			None
		} else {
			sources
				.get(&source)
				.filter(|feed| feed.state == crate::changefeed::ChangefeedState::Active)
				.map(|feed| LiveCapturePolicy::from_source(feed.id, feed.version, feed.config))
				.transpose()?
		};
		if metadata.epochs.last().is_none_or(|epoch| epoch.policy != final_policy) {
			return Err(invalid("LIVE final capture epoch disagrees with stable source policy"));
		}
		metadata.validate(query.starting_after, u64::MAX)?;
		if metadata.version != query.version
			|| metadata.purged_through != 0 && metadata.purged_time_ns > catalog.committed_at_ns
		{
			return Err(invalid("LIVE history incarnation or purge clock disagrees with catalog"));
		}
		let partial = metadata.retained_page != 0;
		if partial
			&& !entry.segments.first().is_some_and(|segment| {
				metadata.retained_page > 1
					&& metadata.retained_page < segment.header.pages
					&& metadata.retained_groups > 0
					&& metadata.retained_groups < segment.header.commits
					&& metadata.purged_through >= segment.header.first_stamp
					&& metadata.purged_through < segment.header.last_stamp
					&& metadata.purged_time_ns >= segment.header.first_time
					&& metadata.purged_time_ns <= segment.header.last_time
			}) {
			return Err(invalid("LIVE retained prefix position is invalid"));
		}
		let mut previous_segment = None;
		for (ordinal, segment) in entry.segments.iter().enumerate() {
			let header = segment.header;
			header.validate()?;
			if segment.file.kind() != Some(crate::storage::types::DataFileKind::Changefeed)
				|| !files.insert(segment.file)
				|| !segments.insert(header.segment)
				|| header.database != catalog.database_uuid
				|| header.owner != crate::storage::HistoryOwner::Live(entry.id)
				|| (!partial || ordinal != 0) && header.first_stamp <= metadata.purged_through
				|| header.first_stamp <= query.starting_after
				|| header.last_stamp > metadata.last_stamp
				|| header.last_time > catalog.committed_at_ns
				|| metadata.purged_through != 0
					&& (!partial || ordinal != 0)
					&& header.first_time < metadata.purged_time_ns
			{
				return Err(invalid("LIVE segment identity or authoritative bounds disagree"));
			}
			if previous_segment.is_some_and(|(stamp, time)| {
				stamp >= header.first_stamp || time > header.first_time
			}) {
				return Err(invalid("LIVE segments overlap or are out of order"));
			}
			previous_segment = Some((header.last_stamp, header.last_time));
		}
	}
	Ok(())
}
