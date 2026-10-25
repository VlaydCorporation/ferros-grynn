//! Snapshot-owned computed history and capture-policy epochs.

use std::{collections::BTreeMap, sync::Arc};

use fg_runtime::__internal::DurationInternal as _;

use super::{BindingAdmission, DatabaseRoot, SchemaVersion};
use crate::{
	changefeed::{ChangefeedId, ChangefeedSource, ChangefeedState},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBindingOptions, LiveId, LiveSource, LiveState, LiveVersion},
};

/// Exact source incarnation and revision enabling computed durable history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveCapturePolicy {
	/// Qualified source history identity, independent of its mutable name.
	pub(crate) source: ChangefeedId,
	/// Final source configuration revision used at publication.
	pub(crate) revision: SchemaVersion,
	/// Positive fixed retention, without calendar months or years.
	pub(crate) retention_ns: i128,
}

/// A changed policy starts at one whole commit; None marks lost durable
/// coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveCaptureEpoch {
	/// Inclusive first commit governed by the final policy.
	pub(crate) started_at: u64,
	/// None explicitly disables durable capture without rebinding the query.
	pub(crate) policy: Option<LiveCapturePolicy>,
}

/// Logical history bounds remain authoritative without physical segment bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveHistoryMetadata {
	/// Query incarnation version encoded by every computed envelope.
	pub(crate) version: LiveVersion,
	/// Last recorded computed group, zero before the first result change;
	/// retention never lowers this authoritative watermark.
	pub(crate) last_stamp: u64,
	/// Last completely removed group, retained even if no segments remain.
	pub(crate) purged_through: u64,
	/// Captured UTC time of the final removed whole commit.
	pub(crate) purged_time_ns: i128,
	/// First available group page in a partially purged first segment.
	pub(crate) retained_page: u64,
	/// Complete groups removed from the first retained segment.
	pub(crate) retained_groups: u64,
	/// Sorted policy transitions; starts when durable capture first becomes
	/// available.
	pub(crate) epochs: Vec<LiveCaptureEpoch>,
}

impl LiveHistoryMetadata {
	/// Creates metadata without serializing an initial row set.
	fn new(version: LiveVersion) -> Self {
		Self {
			version,
			last_stamp: 0,
			purged_through: 0,
			purged_time_ns: 0,
			retained_page: 0,
			retained_groups: 0,
			epochs: Vec::new(),
		}
	}

	/// Validates clocks and capture descriptors independently of optional
	/// files.
	pub(crate) fn validate(&self, starting_after: u64, snapshot: u64) -> FerrosGrynnResult<()> {
		if self.epochs.is_empty()
			|| self.last_stamp > snapshot
			|| self.purged_through > self.last_stamp
			|| self.purged_through == 0 && self.purged_time_ns != 0
			|| (self.retained_page == 0) != (self.retained_groups == 0)
			|| self
				.epochs
				.first()
				.is_none_or(|epoch| epoch.started_at < starting_after || epoch.policy.is_none())
			|| self.last_stamp != 0
				&& self.epochs.first().is_some_and(|epoch| self.last_stamp < epoch.started_at)
			|| self.epochs.windows(2).any(|pair| {
				pair[0].started_at >= pair[1].started_at || pair[0].policy == pair[1].policy
			}) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE capture bounds or epochs are invalid",
			));
		}
		for epoch in &self.epochs {
			if epoch.started_at == 0 || epoch.started_at > snapshot {
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE capture epoch exceeds the snapshot clock",
				));
			}
			if let Some(policy) = epoch.policy {
				crate::changefeed::ChangefeedConfig::from_nanoseconds(policy.retention_ns)?;
			}
		}
		if self.purged_through != 0 {
			jiff::Timestamp::from_nanosecond(self.purged_time_ns)?;
		}
		Ok(())
	}
}

impl LiveCapturePolicy {
	/// Shares final validated source policy interpretation with catalog
	/// recovery.
	pub(crate) fn from_source(
		source: ChangefeedId,
		revision: SchemaVersion,
		config: crate::changefeed::ChangefeedConfig,
	) -> FerrosGrynnResult<Self> {
		Ok(Self {
			source,
			revision,
			retention_ns: config.retention().fixed_nanoseconds().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE source retention is not fixed")
			})?,
		})
	}
}

/// Applies final source policy to already-computed envelopes before
/// publication. Volatile envelopes stay available for the queue but never
/// become durable rows.
pub fn prepare(root: &mut DatabaseRoot) -> FerrosGrynnResult<()> {
	if root.live_catalog().queries.is_empty() {
		return Ok(());
	}
	let mut budget = BindingAdmission::new(&LiveBindingOptions::default());
	let count = root.live_catalog().queries.len();
	let feeds = root.changefeeds().definitions.len();
	let encoded = root.live_catalog().prepared_encoded.len();
	let feed_height = usize::BITS - feeds.leading_zeros();
	let encoded_height = usize::BITS - encoded.leading_zeros();
	let work = feeds
		.checked_mul(feed_height as usize + 1)
		.and_then(|work| {
			work.checked_add(count.checked_mul(feed_height as usize + encoded_height as usize + 2)?)
		})
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE capture lookup work"))?;
	budget.admit(
		crate::values::storage::tree_allocation_bytes(
			feeds,
			std::mem::size_of::<(ChangefeedSource, &super::ChangefeedDefinition)>(),
		)?,
		work as u64,
	)?;
	let mut sources = BTreeMap::new();
	for feed in root.changefeeds().definitions.values() {
		let current = sources.entry(feed.source).or_insert(feed.as_ref());
		if feed.defined_at > current.defined_at {
			*current = feed.as_ref();
		}
	}
	budget.admit(
		count
			.checked_mul(512)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE capture descriptors"))?,
		count as u64,
	)?;
	let history_count = root
		.live_history()
		.len()
		.checked_add(count)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE retained history entries"))?;
	budget.admit(
		crate::values::storage::tree_allocation_bytes(
			history_count,
			std::mem::size_of::<((LiveId, u64), Arc<[u8]>)>(),
		)?,
		0,
	)?;
	let policies = root
		.live_catalog()
		.queries
		.values()
		.map(|query| {
			let source = query.capture_source;
			let policy = if query.state == LiveState::Closed {
				None
			} else {
				sources
					.get(&source)
					.filter(|feed| feed.state == ChangefeedState::Active)
					.map(|feed| LiveCapturePolicy::from_source(feed.id, feed.version, feed.config))
					.transpose()?
			};
			Ok((query.id, query.version, query.starting_after, policy))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for (id, version, starting_after, policy) in policies {
		let original = root.live_catalog().histories.get(&id).cloned();
		if policy.is_none() && original.is_none() {
			continue;
		}
		let epochs = original.as_ref().map_or(0, |history| history.epochs.len());
		budget.admit(
			epochs
				.checked_add(1)
				.and_then(|count| count.checked_mul(3 * std::mem::size_of::<LiveCaptureEpoch>()))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE capture epochs"))?,
			epochs as u64,
		)?;
		let mut history = original
			.as_ref()
			.map_or_else(|| LiveHistoryMetadata::new(version), |history| history.as_ref().clone());
		if history.version != version {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE capture version differs from its query",
			));
		}
		if history.epochs.last().is_none_or(|epoch| epoch.policy != policy) {
			history.epochs.try_reserve_exact(1).map_err(|error| {
				FerrosGrynnError::traversal_resource_limit_exceeded(
					"LIVE capture epochs",
					epochs as u64,
				)
				.with_technical_source(error)
			})?;
			history.epochs.push(LiveCaptureEpoch {
				started_at: root.commit_ts,
				policy,
			});
		}
		let envelope = if policy.is_some() {
			let encoded = &root.live_catalog().prepared_encoded;
			encoded
				.binary_search_by_key(&id, |(owner, _)| *owner)
				.ok()
				.map(|at| Arc::clone(&encoded[at].1))
		} else {
			None
		};
		if let Some(bytes) = envelope {
			let info = crate::storage::live_history::inspect(&bytes, bytes.len() as u64)?;
			if info.id != id || info.version != version {
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE prepared envelope has another incarnation",
				));
			}
			// Catalog-only maintenance can preserve prepared handles from an
			// older root. Such envelopes must not be appended a
			// second time.
			if info.stamp == root.commit_ts {
				if info.stamp <= history.last_stamp {
					return Err(FerrosGrynnError::invalid_storage_file(
						"LIVE computed history clock moved backwards",
					));
				}
				history.last_stamp = info.stamp;
				Arc::make_mut(root.live_history_mut()).insert((id, info.stamp), bytes);
			}
		}
		history.validate(starting_after, root.commit_ts)?;
		if original.as_deref() != Some(&history) {
			root.mark_version(super::ConflictKey::LiveCatalogRange, root.commit_ts);
		}
		Arc::make_mut(root.live_catalog_mut()).histories.insert(id, Arc::new(history));
	}
	Ok(())
}

/// CLASS/record queries bind global source policy; graph queries bind their
/// qualified graph policy and never inherit a base class feed implicitly.
/// Resolves stable capture ownership without current-name lookup.
pub fn capture_source(
	query: &crate::live::LiveQuery,
	source_class: Option<super::ClassId>,
) -> FerrosGrynnResult<ChangefeedSource> {
	Ok(match query.source() {
		LiveSource::Class(class) => ChangefeedSource::Class(*class),
		LiveSource::Record(_) => match source_class {
			Some(class) => ChangefeedSource::Class(class),
			None => {
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE record source lost its class binding",
				));
			}
		},
		LiveSource::Graph(graph)
		| LiveSource::GraphClass {
			graph,
			..
		}
		| LiveSource::GraphRecord {
			graph,
			..
		} => ChangefeedSource::Graph(graph.id()),
		LiveSource::GraphExpr(_)
		| LiveSource::GraphClassExpr {
			..
		}
		| LiveSource::GraphRecordExpr {
			..
		} => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE expression scope requires a resolved capture owner",
			));
		}
	})
}

#[cfg(test)]
mod tests;
