//! Immutable logical metadata for active and retained historical sources.

use super::{ChangefeedConfig, ChangefeedId, ChangefeedSource, Versionstamp};
use crate::{
	datastore::SchemaVersion,
	values::{Text, Uuid},
};

/// Lifecycle of a source definition retained in the selected snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangefeedState {
	/// Successful commits capture this source according to its policy.
	Active,
	/// Capture was explicitly stopped; retained history remains readable.
	Stopped,
	/// The owning class or graph was removed; retained history remains
	/// readable.
	Retired,
}

/// Historical capture policy applying from an inclusive whole-commit stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangefeedCaptureEpoch {
	/// Inclusive first commit covered by this capture epoch.
	pub(crate) started_at: Versionstamp,
	/// Final validated definition revision governing this epoch.
	pub(crate) revision: SchemaVersion,
	/// Capture policy, or None while capture is suppressed.
	pub(crate) configuration: Option<ChangefeedConfig>,
}

impl ChangefeedCaptureEpoch {
	/// Returns the captured source definition revision.
	pub const fn revision(self) -> SchemaVersion {
		self.revision
	}

	/// Returns the first commit covered by this policy.
	pub const fn started_at(self) -> Versionstamp {
		self.started_at
	}

	/// Returns capture flags, or None while capture is suppressed.
	pub const fn configuration(self) -> Option<ChangefeedConfig> {
		self.configuration
	}
}

/// Owned source metadata independent of physical file or page identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangefeedInfo {
	/// Durable database origin for cursor validation.
	pub(crate) database: Uuid,
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
	pub(crate) defined_at: Versionstamp,
	/// Latest configuration publication commit.
	pub(crate) published_at: Versionstamp,
	/// Latest recorded history commit, or zero.
	pub(crate) last_stamp: Versionstamp,
	/// Durable unavailable-prefix floor, or zero.
	pub(crate) purged_through: Versionstamp,
	/// Selected immutable upper commit boundary.
	pub(crate) snapshot_stamp: Versionstamp,
	/// Ordered historical capture policies.
	pub(crate) epochs: Vec<ChangefeedCaptureEpoch>,
}

impl ChangefeedInfo {
	/// Returns retained historical policies in increasing commit order.
	pub fn capture_epochs(&self) -> &[ChangefeedCaptureEpoch] {
		&self.epochs
	}

	/// Returns the durable database origin used by resume cursors.
	pub const fn database_id(&self) -> Uuid {
		self.database
	}

	/// Returns the exact source incarnation.
	pub const fn id(&self) -> ChangefeedId {
		self.id
	}

	/// Returns the stable owning class or graph identity.
	pub const fn source(&self) -> ChangefeedSource {
		self.source
	}

	/// Returns the source name captured by its retained definition.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the latest retained policy, including after capture stops.
	pub const fn configuration(&self) -> ChangefeedConfig {
		self.config
	}

	/// Returns the capture lifecycle state.
	pub const fn state(&self) -> ChangefeedState {
		self.state
	}

	/// Returns the checked source-definition revision.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the commit which established this source incarnation.
	pub const fn defined_at(&self) -> Versionstamp {
		self.defined_at
	}

	/// Returns the latest source-configuration publication position.
	pub const fn published_at(&self) -> Versionstamp {
		self.published_at
	}

	/// Returns the latest recorded batch position, or zero if none exists.
	pub const fn last_versionstamp(&self) -> Versionstamp {
		self.last_stamp
	}

	/// Returns the durable prefix floor, or zero before any purge.
	pub const fn purged_through(&self) -> Versionstamp {
		self.purged_through
	}

	/// Returns the fixed upper boundary selected by the transaction snapshot.
	pub const fn snapshot_versionstamp(&self) -> Versionstamp {
		self.snapshot_stamp
	}
}
