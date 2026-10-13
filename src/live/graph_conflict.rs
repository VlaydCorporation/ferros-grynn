//! Owned conflict information leaves the resolution decision with the caller.

use std::sync::Arc;

use super::{LiveGraphDelta, LiveGraphImage, LiveGraphTarget};
use crate::{changefeed::Versionstamp, gql_status::FerrosGrynnError};

/// Complete pending incoming and local graph changes with qualified conflicts.
#[derive(Debug, Clone)]
pub struct LiveGraphConflict {
	/// Pins the complete unaccepted remote delta for caller inspection.
	pub(crate) incoming: Arc<LiveGraphDelta>,
	/// Pins the local candidate captured at the conflict boundary.
	pub(crate) local: Arc<LiveGraphImage>,
	/// Retains canonical local targets awaiting write-back or resolution.
	pub(crate) local_changes: Vec<LiveGraphTarget>,
	/// Owns the qualified overlapping or structurally incompatible targets.
	pub(crate) conflicts: Vec<LiveGraphTarget>,
	/// Retains an optional graph-invariant reason beyond target overlap.
	pub(crate) diagnostic: Option<FerrosGrynnError>,
}

impl LiveGraphConflict {
	/// Explains a topology or containment incompatibility beyond target
	/// overlap.
	pub const fn diagnostic(&self) -> Option<&FerrosGrynnError> {
		self.diagnostic.as_ref()
	}

	/// Borrows the entire incoming delta; it has not been applied or ACKed.
	pub fn incoming(&self) -> &LiveGraphDelta {
		&self.incoming
	}

	/// Borrows the local candidate captured at this conflict boundary.
	pub fn local(&self) -> &LiveGraphImage {
		&self.local
	}

	/// Borrows local changes in canonical qualified order.
	pub fn local_changes(&self) -> &[LiveGraphTarget] {
		&self.local_changes
	}

	/// Borrows intersecting or structurally incompatible qualified targets.
	pub fn conflicts(&self) -> &[LiveGraphTarget] {
		&self.conflicts
	}

	/// Returns the incoming durable watermark awaiting a user decision.
	pub fn remote_revision(&self) -> Versionstamp {
		self.incoming.revision()
	}
}
