//! Retained source definitions; public values and physical formats live
//! elsewhere.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
	changefeed::{
		ChangefeedCaptureEpoch,
		ChangefeedConfig,
		ChangefeedId,
		ChangefeedInfo,
		ChangefeedSource,
		ChangefeedState,
		Versionstamp,
	},
	datastore::{DatabaseRoot, SchemaVersion},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::Text,
};

/// One retained source incarnation, including after its owner is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
	clippy::redundant_pub_crate,
	reason = "the private catalog is re-exported internally for storage publication"
)]
pub(crate) struct ChangefeedDefinition {
	/// Stable retained source incarnation.
	pub(crate) id: ChangefeedId,
	/// Stable owning class or graph identity.
	pub(crate) source: ChangefeedSource,
	/// Owned source name retained after schema removal.
	pub(crate) name: Text,
	/// Latest validated capture configuration.
	pub(crate) config: ChangefeedConfig,
	/// Current capture lifecycle in this snapshot.
	pub(crate) state: ChangefeedState,
	/// Checked definition revision.
	pub(crate) version: SchemaVersion,
	/// Commit establishing this source incarnation.
	pub(crate) defined_at: u64,
	/// Latest configuration publication commit.
	pub(crate) published_at: u64,
	/// Latest recorded history commit, or zero.
	pub(crate) last_stamp: u64,
	/// Durable unavailable-prefix floor, or zero.
	pub(crate) purged_through: u64,
	/// UTC time of the final removed commit; zero when no prefix was purged.
	pub(crate) purged_time_ns: i128,
	/// First live group page in the first retained segment, zero without a
	/// prefix.
	pub(crate) retained_page: u64,
	/// Complete expired groups skipped within that segment.
	pub(crate) retained_groups: u64,
	/// Commit stopping capture or retiring its owner.
	pub(crate) closed_at: Option<u64>,
	/// UTC publication time stopping capture; absent while active.
	pub(crate) closed_time_ns: Option<i128>,
	/// Ordered historical capture policies.
	pub(crate) epochs: Vec<ChangefeedCaptureEpoch>,
}

impl ChangefeedDefinition {
	/// Projects constant-size committed read metadata without cloning
	/// epochs/images.
	pub(crate) fn read_binding(
		&self,
		root: &DatabaseRoot,
	) -> crate::changefeed::ChangefeedReadBinding {
		crate::changefeed::ChangefeedReadBinding {
			database: root.database_uuid.into(),
			id: self.id,
			source: self.source,
			revision: self.version,
			configuration: self.config,
			state: self.state,
			snapshot: root.commit_ts.into(),
			last: self.last_stamp.into(),
			purged_through: self.purged_through.into(),
		}
	}

	/// Coalesces configuration actions of one commit to its final policy.
	pub(crate) fn set_capture_epoch(
		&mut self,
		stamp: u64,
		configuration: Option<ChangefeedConfig>,
	) -> FerrosGrynnResult<()> {
		/// Admission cap implied by the durable catalog payload limit.
		const MAX_EPOCHS: usize = (16 * 1024 * 1024) / 32;
		let epoch = ChangefeedCaptureEpoch {
			started_at: stamp.into(),
			configuration,
		};
		if let Some(last) = self.epochs.last_mut() {
			if last.started_at().get() > stamp {
				return Err(FerrosGrynnError::invalid_argument(
					"capture epoch clock moved backwards",
				));
			}
			if last.started_at().get() == stamp {
				*last = epoch;
				return Ok(());
			}
		}
		if self.epochs.len() >= MAX_EPOCHS {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"changefeed capture epochs",
				(self.epochs.len() + 1) as u64,
				MAX_EPOCHS as u64,
			));
		}
		self.epochs.push(epoch);
		Ok(())
	}

	/// Admits the complete owned metadata before cloning historical epochs.
	pub(crate) fn info_memory_bytes(&self) -> FerrosGrynnResult<usize> {
		self.epochs
			.len()
			.checked_mul(size_of::<ChangefeedCaptureEpoch>())
			.and_then(|bytes| bytes.checked_add(self.name.len()))
			.and_then(|bytes| bytes.checked_add(size_of::<ChangefeedInfo>()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed metadata bytes"))
	}

	/// Projects owned metadata into one fixed database snapshot.
	pub(crate) fn info(&self, root: &DatabaseRoot) -> ChangefeedInfo {
		ChangefeedInfo {
			database: root.database_uuid.into(),
			id: self.id,
			source: self.source,
			name: self.name.clone(),
			config: self.config,
			state: self.state,
			version: self.version,
			defined_at: Versionstamp::new(self.defined_at),
			published_at: Versionstamp::new(self.published_at),
			last_stamp: Versionstamp::new(self.last_stamp),
			purged_through: Versionstamp::new(self.purged_through),
			snapshot_stamp: Versionstamp::new(root.commit_ts),
			epochs: self.epochs.clone(),
		}
	}
}

/// Immutable-root-owned definitions, with no process-local subscriber state.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[allow(
	clippy::redundant_pub_crate,
	reason = "the private catalog is re-exported internally for storage publication"
)]
pub(crate) struct ChangefeedCatalog {
	/// Retained source descriptors keyed by incarnation.
	pub(crate) definitions: BTreeMap<ChangefeedId, Arc<ChangefeedDefinition>>,
}

/// One immutable captured commit; physical encoding belongs to storage.
#[derive(Debug, Clone)]
#[allow(clippy::redundant_pub_crate, reason = "shared internally with the physical feed codec")]
pub(crate) struct CapturedCommit {
	/// Exact ordinary view, empty when only FULL contains successful no-ops.
	pub(crate) net: crate::changefeed::ChangeBatch,
	/// Exact operation view only when historically captured in FULL mode.
	pub(crate) full: Option<crate::changefeed::ChangeBatch>,
	/// Whether complete originals must be encoded rather than inverse diffs.
	pub(crate) include_original: bool,
	/// Explicit suppression control, if this commit omitted eligible changes.
	pub(crate) gap: Option<crate::changefeed::ChangefeedGap>,
}

impl ChangefeedCatalog {
	/// Resolves the current retained incarnation of a stable owner identity.
	pub(crate) fn for_source(
		&self,
		source: ChangefeedSource,
	) -> Option<&Arc<ChangefeedDefinition>> {
		self.definitions
			.values()
			.filter(|entry| entry.source == source)
			.max_by_key(|entry| entry.defined_at)
	}

	/// Applies the sole authoritative source configuration during replay.
	pub(crate) fn configure(
		&mut self,
		source: ChangefeedSource,
		id: ChangefeedId,
		name: Text,
		config: Option<ChangefeedConfig>,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		let previous = self.definitions.get(&id);
		if previous.is_some_and(|entry| entry.source != source) {
			return Err(FerrosGrynnError::invalid_argument(
				"changefeed identity belongs to another source",
			));
		}
		let mut definition = match (previous, config) {
			(None, None) => {
				return Err(FerrosGrynnError::invalid_argument("changefeed is not configured"));
			}
			(None, Some(config)) => ChangefeedDefinition {
				id,
				source,
				name,
				config,
				state: ChangefeedState::Active,
				version: SchemaVersion::INITIAL,
				defined_at: published_at,
				published_at,
				last_stamp: 0,
				purged_through: 0,
				purged_time_ns: 0,
				retained_page: 0,
				retained_groups: 0,
				closed_at: None,
				closed_time_ns: None,
				epochs: Vec::new(),
			},
			(Some(previous), config) => {
				let mut definition = previous.as_ref().clone();
				definition.version = definition.version.next()?;
				definition.published_at = published_at;
				definition.closed_time_ns = None;
				if let Some(config) = config {
					definition.config = config;
					definition.state = ChangefeedState::Active;
					definition.closed_at = None;
				} else {
					definition.state = ChangefeedState::Stopped;
					definition.closed_at = Some(published_at);
				}
				definition
			}
		};
		definition.set_capture_epoch(published_at, config)?;
		self.definitions.insert(id, Arc::new(definition));
		Ok(())
	}
}
