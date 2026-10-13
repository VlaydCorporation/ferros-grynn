//! Exact owned result images and whole-commit pull delivery.

use std::sync::Arc;

use super::{LiveBindingRevision, LiveCursor, LiveId, LiveIdentity, LiveVersion};
use crate::{
	changefeed::Versionstamp,
	gql_status::FerrosGrynnError,
	values::{DateTime, Value},
};

/// Exact net result change, independent of DIFF/PATCH presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEvent {
	/// Checked ordinal within the whole commit batch.
	pub(crate) ordinal: u32,
	/// Identity of the source row, preserved even for scalar projections.
	pub(crate) identity: LiveIdentity,
	/// Exact previous result; None denotes absence from WHERE result.
	pub(crate) before: Option<Value>,
	/// Exact new result; None denotes exit from WHERE result.
	pub(crate) after: Option<Value>,
	/// Derived native output prepared from exact images; never physical
	/// history.
	pub(crate) output: Option<Box<super::PreparedOutput>>,
}

impl LiveEvent {
	/// Returns the complete-commit deduplication ordinal.
	pub const fn ordinal(&self) -> u32 {
		self.ordinal
	}

	/// Returns the qualified source row identity.
	pub const fn identity(&self) -> &LiveIdentity {
		&self.identity
	}

	/// Returns the exact previous projected result, including scalar None
	/// values.
	pub const fn before(&self) -> Option<&Value> {
		self.before.as_ref()
	}

	/// Returns the exact new projected result, including scalar None values.
	pub const fn after(&self) -> Option<&Value> {
		self.after.as_ref()
	}

	/// Returns prepared DIFF/PATCH for the immutable query output mode.
	/// Full mode uses `before`/`after` directly; raw images remain available.
	///
	/// ```
	/// use ferros_grynn::{
	/// 	gql_status::FerrosGrynnResult,
	/// 	live::{LiveBindingOptions, LiveEvent},
	/// };
	/// fn json(event: &LiveEvent) -> FerrosGrynnResult<Option<String>> {
	/// 	event.output().map(|output| output.to_json(&LiveBindingOptions::default())).transpose()
	/// }
	/// ```
	#[allow(
		clippy::tabs_in_doc_comments,
		reason = "nightly rustfmt requires hard tabs in Rustdoc examples"
	)]
	pub fn output(&self) -> Option<&super::LiveEventOutput> {
		self.output.as_ref().map(|output| &output.output)
	}
}

/// Complete net result changes for one subscription and database commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveBatch {
	/// Subscription incarnation used for deduplication.
	pub(crate) id: LiveId,
	/// Query version producing these results.
	pub(crate) version: LiveVersion,
	/// Existing durable database commit clock.
	pub(crate) stamp: Versionstamp,
	/// Persisted monotonic UTC commit time.
	pub(crate) committed_at: DateTime,
	/// All changed identities; never a partial whole-commit prefix.
	pub(crate) events: Vec<LiveEvent>,
	/// Exact reactive parameter revisions used to compute these results.
	pub(crate) bindings: Vec<LiveBindingRevision>,
}

impl LiveBatch {
	/// Returns the stable subscription incarnation.
	pub const fn id(&self) -> LiveId {
		self.id
	}

	/// Returns the computing query version.
	pub const fn version(&self) -> LiveVersion {
		self.version
	}

	/// Returns the exact whole-commit position.
	pub const fn versionstamp(&self) -> Versionstamp {
		self.stamp
	}

	/// Returns persisted UTC publication time.
	pub const fn committed_at(&self) -> &DateTime {
		&self.committed_at
	}

	/// Returns all net changes in canonical source identity order.
	pub fn events(&self) -> &[LiveEvent] {
		&self.events
	}

	/// Returns captured binding revisions.
	pub fn bindings(&self) -> &[LiveBindingRevision] {
		&self.bindings
	}
}

/// Complete owned graph changes for one subscription and database commit.
#[derive(Debug, Clone)]
pub struct LiveGraphBatch {
	/// Identifies the stable LIVE result incarnation.
	pub(crate) id: LiveId,
	/// Identifies the accepted logical recipe version.
	pub(crate) version: LiveVersion,
	/// Stores the publication time used by bounded history retention.
	pub(crate) committed_at: DateTime,
	/// Owns the complete qualified graph revision and its predecessor
	/// checkpoint.
	pub(crate) delta: super::LiveGraphDelta,
	/// Retains the exact reactive revisions captured with this graph result.
	pub(crate) bindings: Vec<LiveBindingRevision>,
}

impl LiveGraphBatch {
	/// One shared admission authority for retained checkpoints and queue data.
	pub(crate) fn estimated_heap_bytes(&self) -> crate::gql_status::FerrosGrynnResult<usize> {
		let overflow = || FerrosGrynnError::operation_overflow("LIVE graph batch bytes");
		let mut bytes = 512_usize
			.checked_add(self.delta.after().estimated_heap_bytes()?)
			.ok_or_else(overflow)?;
		if let Some(before) = self.delta.before() {
			bytes = bytes.checked_add(before.estimated_heap_bytes()?).ok_or_else(overflow)?;
		}
		bytes = bytes
			.checked_add(
				self.bindings
					.capacity()
					.checked_mul(std::mem::size_of::<LiveBindingRevision>())
					.ok_or_else(overflow)?,
			)
			.ok_or_else(overflow)?;
		bytes = bytes
			.checked_add(
				self.delta
					.changes
					.capacity()
					.checked_mul(std::mem::size_of::<super::LiveGraphTarget>())
					.ok_or_else(overflow)?,
			)
			.ok_or_else(overflow)?;
		for target in self.delta.changes() {
			let heap = match target {
				super::LiveGraphTarget::Graph(scope) => scope.estimated_heap_bytes(),
				super::LiveGraphTarget::Atom(atom) => atom
					.scope()
					.estimated_heap_bytes()
					.checked_add(atom.atom().id().estimated_heap_bytes())
					.ok_or_else(overflow)?,
			};
			bytes = bytes.checked_add(heap).ok_or_else(overflow)?;
		}
		Ok(bytes)
	}

	/// Returns the subscription incarnation producing this graph.
	pub const fn id(&self) -> LiveId {
		self.id
	}

	/// Returns the accepted query definition version.
	pub const fn version(&self) -> LiveVersion {
		self.version
	}

	/// Returns the whole remote commit watermark.
	pub const fn versionstamp(&self) -> Versionstamp {
		self.delta.revision()
	}

	/// Borrows the complete graph delta and its checkpoint images.
	pub const fn delta(&self) -> &super::LiveGraphDelta {
		&self.delta
	}

	/// Borrows the persisted UTC publication time.
	pub const fn committed_at(&self) -> &DateTime {
		&self.committed_at
	}

	/// Borrows the captured reactive parameter revisions.
	pub fn bindings(&self) -> &[LiveBindingRevision] {
		&self.bindings
	}
}

/// Explicit reason history or computed result coverage is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveGapReason {
	/// A slow consumer exhausted its bounded volatile queue.
	QueueOverflow,
	/// Volatile history was lost on restart or detach.
	VolatileHistoryLost,
	/// Source/dependency capture did not cover the requested historical inputs.
	InsufficientCoverage,
	/// Maintenance removed retained computed history.
	Retention,
	/// Trusted catalog bounds cover a missing or corrupted segment.
	SegmentCorrupt,
	/// Import explicitly suppressed durable capture.
	ImportSuppressed,
	/// Evaluation failed while primary changes still committed.
	EvaluationFault,
	/// A source or retained subscription incarnation no longer exists.
	SourceRetired,
}

/// Explicit unavailable interval; unknown trusted bounds remain optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGap {
	/// Stable subscription incarnation.
	pub(crate) id: LiveId,
	/// Cause of missing result coverage.
	pub(crate) reason: LiveGapReason,
	/// First unavailable stamp, if independently known.
	pub(crate) from: Option<Versionstamp>,
	/// Last unavailable stamp, if independently known.
	pub(crate) through: Option<Versionstamp>,
}

impl LiveGap {
	/// Returns the affected subscription.
	pub const fn id(&self) -> LiveId {
		self.id
	}

	/// Returns the explicit loss-of-history reason.
	pub const fn reason(&self) -> LiveGapReason {
		self.reason
	}

	/// Returns the first trustworthy unavailable position.
	pub const fn from(&self) -> Option<Versionstamp> {
		self.from
	}

	/// Returns the last trustworthy unavailable position.
	pub const fn through(&self) -> Option<Versionstamp> {
		self.through
	}
}

/// One immutable delivery payload with exact whole-commit semantics.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LivePayload {
	/// All net changes of one commit.
	Batch(LiveBatch),
	/// Complete graph revision using qualified logical identities.
	Graph(Arc<LiveGraphBatch>),
	/// Explicit unavailable interval, followed by available tail where
	/// possible.
	Gap(LiveGap),
	/// Evaluation fault and the unavailable result interval it caused.
	Fault {
		/// Exact typed evaluator/resource diagnosis.
		diagnostic: FerrosGrynnError,
		/// Explicit coverage loss despite successful primary publication.
		gap: LiveGap,
	},
}

/// Owned immutable delivery; its token is separate from confirmed cursor().
#[derive(Debug, Clone)]
pub struct LiveDelivery {
	/// Position that can be acknowledged only after this delivery was issued.
	pub(crate) token: LiveCursor,
	/// Admitted immutable payload shared with the bounded delivery queue.
	pub(crate) payload: Arc<LivePayload>,
}

impl LiveDelivery {
	/// Returns the issued whole-delivery ACK token.
	pub const fn token(&self) -> &LiveCursor {
		&self.token
	}

	/// Returns the complete immutable owned payload.
	pub fn payload(&self) -> &LivePayload {
		&self.payload
	}
}

/// Pull result; an idle active subscription is distinct from termination.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LiveNext {
	/// No currently queued item; the subscription remains attached and active.
	Idle,
	/// Complete delivery, repeated by next until acknowledged.
	Delivery(LiveDelivery),
	/// Subscription stopped or consumer disconnected.
	Closed,
}

#[cfg(test)]
mod tests {
	use std::hash::{DefaultHasher, Hash, Hasher};

	use super::*;
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		graph::{
			AtomKind,
			AtomOccurrenceRef,
			AtomRef,
			GraphId,
			GraphKind,
			GraphRef,
			GraphStorageMode,
		},
	};

	/// Catalog presentation changes cannot turn the same qualified row into
	/// an exit followed by an entry.
	#[test]
	fn live_occurrence_identity_survives_graph_rename() -> FerrosGrynnResult<()> {
		let id =
			GraphId::new(1).ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph"))?;
		let atom = AtomRef::new(AtomKind::Node, "person:a".parse()?);
		let identity = |name| -> FerrosGrynnResult<_> {
			Ok(LiveIdentity::Occurrence(AtomOccurrenceRef::new(
				atom.clone(),
				Arc::new(GraphRef::new(id, name, GraphKind::Graph, GraphStorageMode::InMemory)?),
			)))
		};
		let first = identity("before")?;
		let renamed = identity("after")?;
		assert_eq!(first, renamed);
		let hash = |value: &LiveIdentity| {
			let mut hasher = DefaultHasher::new();
			value.hash(&mut hasher);
			hasher.finish()
		};
		assert_eq!(hash(&first), hash(&renamed));
		Ok(())
	}
}
