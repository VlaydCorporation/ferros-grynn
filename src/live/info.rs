//! Owned registry metadata; no snapshots, pages or executable plans escape.

use std::sync::Arc;

use super::{
	LiveBindingId,
	LiveBindingOptions,
	LiveCursor,
	LiveExpr,
	LiveId,
	LiveOptions,
	LiveQuery,
	LiveVersion,
};
use crate::{
	changefeed::{ChangefeedId, ChangefeedSource, Versionstamp},
	gql_status::FerrosGrynnError,
	values::{DateTime, Duration, Object, Text, Value},
};

/// Exact source policy captured for computed durable results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveCapturePolicy {
	/// Stable source history identity.
	pub(crate) source: ChangefeedId,
	/// Captured source configuration version.
	pub(crate) revision: crate::datastore::SchemaVersion,
	/// Positive fixed retention duration.
	pub(crate) retention: Duration,
}

impl LiveCapturePolicy {
	/// Returns the source history incarnation.
	pub const fn source(self) -> ChangefeedId {
		self.source
	}

	/// Returns the captured configuration revision.
	pub const fn revision(self) -> crate::datastore::SchemaVersion {
		self.revision
	}

	/// Returns its checked fixed retention.
	pub const fn retention(self) -> Duration {
		self.retention
	}
}

/// Whole-commit transition in durable result coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveCaptureEpoch {
	/// Inclusive first commit governed by this policy.
	pub(crate) started_at: Versionstamp,
	/// Absence explicitly disables durable capture.
	pub(crate) policy: Option<LiveCapturePolicy>,
}

impl LiveCaptureEpoch {
	/// Returns the inclusive policy boundary.
	pub const fn started_at(self) -> Versionstamp {
		self.started_at
	}

	/// Returns the captured source policy, or explicit unavailable coverage.
	pub const fn policy(self) -> Option<LiveCapturePolicy> {
		self.policy
	}
}

/// Authoritative history watermarks, independent of physical file retention.
#[derive(Debug, Clone)]
pub struct LiveHistoryInfo {
	/// Last recorded durable group; zero before any result changes.
	pub(crate) last_stamp: Versionstamp,
	/// Whole-commit purge floor, preserved after complete retirement.
	pub(crate) purged_through: Versionstamp,
	/// Exact commit time of the last purged group, absent before any purge.
	pub(crate) purged_at: Option<DateTime>,
	/// Ordered source capture policy transitions.
	pub(crate) epochs: Vec<LiveCaptureEpoch>,
}

impl LiveHistoryInfo {
	/// Returns the last durable group ever recorded; purge does not lower it.
	/// This is not the first currently available group.
	pub const fn last_stamp(&self) -> Versionstamp {
		self.last_stamp
	}

	/// Returns the last completely purged whole commit.
	pub const fn purged_through(&self) -> Versionstamp {
		self.purged_through
	}

	/// Returns the captured UTC time of that purged group.
	pub const fn purged_at(&self) -> Option<&DateTime> {
		self.purged_at.as_ref()
	}

	/// Returns ordered policy changes, including explicit capture suppression.
	pub fn epochs(&self) -> &[LiveCaptureEpoch] {
		&self.epochs
	}
}

/// Registry lifecycle visible through a transaction or committed datastore
/// read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveState {
	/// Private registration awaiting successful publication.
	Pending,
	/// Published and eligible for automatic evaluation.
	Active,
	/// Evaluation failed; explicit replacement is required before resuming.
	Paused,
	/// Explicitly stopped; durable history may remain available.
	Closed,
}

/// Exact definition and value revisions captured for a reactive parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveBindingRevision {
	/// Stable identity, independent of lexical names.
	pub(crate) id: LiveBindingId,
	/// Version of the defining expression.
	pub(crate) definition: LiveVersion,
	/// Commit publishing the captured value.
	pub(crate) value: Versionstamp,
}

impl LiveBindingRevision {
	/// Returns the binding identity.
	pub const fn id(self) -> LiveBindingId {
		self.id
	}

	/// Returns the expression definition version.
	pub const fn definition(self) -> LiveVersion {
		self.definition
	}

	/// Returns the commit publishing this value revision.
	pub const fn value(self) -> Versionstamp {
		self.value
	}
}

/// Immutable subscription information independent of its consumer handle.
///
/// ```
/// use ferros_grynn::{
/// 	changefeed::{ChangefeedConfig, ChangefeedSource},
/// 	datastore::{ClassSchema, Datastore},
/// 	graph::AtomKind,
/// 	live::{LiveOptions, LiveQuery, LiveSource},
/// };
/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
/// let store = Datastore::in_memory();
/// let mut tx = store.begin();
/// let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
/// let source = tx.configure_changefeed(
/// 	ChangefeedSource::Class(class),
/// 	ChangefeedConfig::new("P1D".parse()?)?,
/// )?;
/// let id = tx.register_live(LiveQuery::new(LiveSource::Class(class)), LiveOptions::default())?;
/// let stamp = tx.commit()?.versionstamp();
/// let info = store.begin().live_info(id)?;
/// drop(store);
/// assert_eq!(info.starting_after(), stamp);
/// let history = info.history().ok_or_else(|| {
/// 	ferros_grynn::gql_status::FerrosGrynnError::invalid_argument("history missing")
/// })?;
/// assert_eq!(history.epochs()[0].policy().map(|policy| policy.source()), Some(source));
/// # Ok(()) }
/// ```
#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt requires hard tabs in Rustdoc examples"
)]
#[derive(Debug, Clone)]
pub struct LiveInfo {
	/// Stable subscription incarnation.
	pub(crate) id: LiveId,
	/// Definition version captured by its cursor and computed history.
	pub(crate) version: LiveVersion,
	/// Transaction-visible or committed registry state.
	pub(crate) state: LiveState,
	/// Logical recipe; executable plans remain private to GQL.
	pub(crate) query: Arc<LiveQuery>,
	/// Validated queue and evaluation policy.
	pub(crate) options: LiveOptions,
	/// Commit registering the subscription, or zero while private.
	pub(crate) defined_at: Versionstamp,
	/// Latest publication of definition metadata, separate from result changes.
	pub(crate) published_at: Versionstamp,
	/// Snapshot clock governing all metadata in this value.
	pub(crate) snapshot: Versionstamp,
	/// Exclusive initial delivery boundary.
	pub(crate) starting_after: Versionstamp,
	/// Absent when this incarnation has never acquired durable coverage.
	pub(crate) history: Option<LiveHistoryInfo>,
	/// Concrete transitive published PURE function versions.
	pub(crate) functions: Vec<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>,
	/// Captured record namespace owners, independent of later name reuse.
	pub(crate) record_sources: Vec<(Text, crate::datastore::ClassId)>,
	/// Last successfully resolved primary source, independent of name reuse.
	pub(crate) capture_source: ChangefeedSource,
	/// Frozen primary record/class namespace owner, absent for graph-wide
	/// input.
	pub(crate) source_class: Option<crate::datastore::ClassId>,
	/// Confirmed whole-commit position, separate from outstanding delivery.
	pub(crate) cursor: LiveCursor,
	/// Current snapshot revisions of directly referenced binding identities.
	pub(crate) bindings: Vec<LiveBindingRevision>,
	/// Owned typed diagnostic for a paused or disconnected definition.
	pub(crate) diagnostic: Option<FerrosGrynnError>,
}

impl LiveInfo {
	/// Returns the stable subscription incarnation.
	pub const fn id(&self) -> LiveId {
		self.id
	}

	/// Returns its definition version.
	pub const fn version(&self) -> LiveVersion {
		self.version
	}

	/// Returns registry state without attaching a consumer.
	pub const fn state(&self) -> LiveState {
		self.state
	}

	/// Returns the restorable logical recipe.
	pub fn query(&self) -> &LiveQuery {
		&self.query
	}

	/// Returns delivery and evaluation options.
	pub const fn options(&self) -> &LiveOptions {
		&self.options
	}

	/// Returns the registration commit.
	pub const fn defined_at(&self) -> Versionstamp {
		self.defined_at
	}

	/// Returns the latest publication of this definition's metadata.
	pub const fn published_at(&self) -> Versionstamp {
		self.published_at
	}

	/// Returns the snapshot governing these owned metadata.
	pub const fn snapshot(&self) -> Versionstamp {
		self.snapshot
	}

	/// Returns the exclusive initial delivery boundary.
	pub const fn starting_after(&self) -> Versionstamp {
		self.starting_after
	}

	/// Returns trusted durable bounds and epochs, without opening history
	/// files.
	pub const fn history(&self) -> Option<&LiveHistoryInfo> {
		self.history.as_ref()
	}

	/// Returns exact transitive published PURE versions captured by this query.
	pub fn function_dependencies(
		&self,
	) -> &[(crate::datastore::FunctionId, crate::datastore::SchemaVersion)] {
		&self.functions
	}

	/// Returns frozen namespace owners used by record-valued sources.
	pub fn record_sources(&self) -> &[(Text, crate::datastore::ClassId)] {
		&self.record_sources
	}

	/// Returns the last successfully resolved primary capture source.
	pub const fn capture_source(&self) -> ChangefeedSource {
		self.capture_source
	}

	/// Returns the original primary class namespace owner, including record
	/// sources.
	pub const fn source_class(&self) -> Option<crate::datastore::ClassId> {
		self.source_class
	}

	/// Returns the current bounded exact dependency evaluation strategy.
	pub const fn read_strategy(&self) -> &'static str {
		"conservative_rescan"
	}

	/// Returns the last confirmed position.
	pub const fn cursor(&self) -> &LiveCursor {
		&self.cursor
	}

	/// Returns current snapshot revisions of direct reactive dependencies.
	/// Saved deliveries retain the revisions actually used by their evaluation.
	pub fn bindings(&self) -> &[LiveBindingRevision] {
		&self.bindings
	}

	/// Returns the owned pause/disconnect diagnostic, if any.
	pub const fn diagnostic(&self) -> Option<&FerrosGrynnError> {
		self.diagnostic.as_ref()
	}
}

/// Immutable information about one independently versioned reactive binding.
#[derive(Debug, Clone)]
pub struct LiveBindingInfo {
	/// Stable binding identity.
	pub(crate) revision: LiveBindingRevision,
	/// Private, active, paused or removed binding state.
	pub(crate) state: LiveState,
	/// Logical expression; ordinary parameter inputs were captured by value.
	pub(crate) expression: Arc<LiveExpr>,
	/// Owned frozen ordinary inputs independent of lexical/session mutation.
	pub(crate) parameters: Arc<Object>,
	/// Checked evaluation budgets of the current definition.
	pub(crate) options: LiveBindingOptions,
	/// Latest committed value; returned values own their payloads.
	pub(crate) value: Value,
	/// Stable direct reactive dependencies, sorted by identity.
	pub(crate) dependencies: Vec<LiveBindingId>,
	/// Concrete versions captured for every transitive published PURE call.
	pub(crate) functions: Vec<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)>,
	/// Typed diagnostic on failure; primary source changes remain committed.
	pub(crate) diagnostic: Option<FerrosGrynnError>,
}

impl LiveBindingInfo {
	/// Returns the stable identity and captured revisions.
	pub const fn revision(&self) -> LiveBindingRevision {
		self.revision
	}

	/// Returns binding lifecycle state.
	pub const fn state(&self) -> LiveState {
		self.state
	}

	/// Returns the current logical expression.
	pub fn expression(&self) -> &LiveExpr {
		&self.expression
	}

	/// Returns ordinary parameters captured by value for this definition.
	pub fn parameters(&self) -> &Object {
		&self.parameters
	}

	/// Returns checked per-evaluation limits.
	pub const fn options(&self) -> &LiveBindingOptions {
		&self.options
	}

	/// Returns its latest successfully published value.
	pub const fn value(&self) -> &Value {
		&self.value
	}

	/// Returns direct stable binding dependencies.
	pub fn dependencies(&self) -> &[LiveBindingId] {
		&self.dependencies
	}

	/// Returns the concrete transitive PURE definitions captured by this
	/// revision.
	pub fn function_dependencies(
		&self,
	) -> &[(crate::datastore::FunctionId, crate::datastore::SchemaVersion)] {
		&self.functions
	}

	/// Returns the typed fault diagnosis, if any.
	pub const fn diagnostic(&self) -> Option<&FerrosGrynnError> {
		self.diagnostic.as_ref()
	}
}
