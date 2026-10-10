//! Immutable subscription definitions, independent of attached consumers.

use std::{collections::BTreeMap, sync::Arc};

/// Stable binding identities and bounded DAG ordering share the registry owner.
mod bindings;
pub use bindings::LiveBindingDefinition;

use super::DatabaseRoot;
pub use crate::live::dependencies::{
	BindingAdmission,
	expression_dependencies,
	query_dependencies,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveCursor, LiveId, LiveInfo, LiveOptions, LiveQuery, LiveState, LiveVersion},
	values::Text,
};

/// Logical definitions share the existing immutable history root binding.
#[derive(Clone, Default)]
pub struct LiveCatalog {
	/// Each incarnation remains addressable after explicit close.
	pub(crate) queries: BTreeMap<LiveId, Arc<LiveDefinition>>,
	/// Independent reactive identities; removed recipes remain addressable.
	pub(crate) bindings: BTreeMap<crate::live::LiveBindingId, Arc<LiveBindingDefinition>>,
	/// Capture epochs and trusted bounds survive source policy changes.
	pub(crate) histories: BTreeMap<LiveId, Arc<super::LiveHistoryMetadata>>,
	/// Complete admitted publication payloads for this root's commit.
	pub(crate) prepared: Arc<[(LiveId, Arc<crate::live::LivePayload>)]>,
	/// Bounded restorable envelopes prepared before the commit mutex.
	pub(crate) prepared_encoded: Arc<[(LiveId, Arc<[u8]>)]>,
}

/// One successful root JOIN String scope retains its declaration owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRhsScopeAnchor {
	/// Positive FROM/JOIN declaration ordinal; primary policy is stored
	/// separately.
	pub(crate) source: u32,
	/// Resolved graph identity, retained while the string operand is unchanged.
	pub(crate) graph: crate::graph::GraphId,
	/// Last successful graph-name operand, independent of later graph rename.
	pub(crate) name: Text,
}

impl LiveRhsScopeAnchor {
	/// Accounts retained vector entries and owned names before cloning
	/// metadata.
	pub(crate) fn retained_bytes(anchors: &[Self]) -> FerrosGrynnResult<usize> {
		anchors.iter().try_fold(
			anchors
				.len()
				.checked_mul(std::mem::size_of::<Self>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE RHS anchor vector"))?,
			|bytes, anchor| {
				bytes
					.checked_add(anchor.name.len())
					.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE RHS anchor names"))
			},
		)
	}
}

/// Restorable logical recipe; queues and executable plans are not catalog data.
#[derive(Debug, Clone)]
pub struct LiveDefinition {
	/// Stable subscription incarnation.
	pub(crate) id: LiveId,
	/// Checked definition version.
	pub(crate) version: LiveVersion,
	/// Private or published lifecycle.
	pub(crate) state: LiveState,
	/// Owned ordinary captures and stable source identities.
	pub(crate) query: Arc<LiveQuery>,
	/// Canonical bytes admitted before private registration.
	pub(crate) descriptor: Arc<[u8]>,
	/// Immutable class binding, including a record source's original namespace.
	pub(crate) source_class: Option<super::ClassId>,
	/// Last successful capture owner, read without evaluating query
	/// expressions.
	pub(crate) capture_source: crate::changefeed::ChangefeedSource,
	/// Last successful dynamic graph-name operand, independent of later rename.
	pub(crate) capture_scope_name: Option<Text>,
	/// Independently anchored successful expression-scoped JOIN sources.
	pub(crate) rhs_scope_anchors: Vec<LiveRhsScopeAnchor>,
	/// Validated delivery and evaluation limits.
	pub(crate) options: LiveOptions,
	/// Registration commit; zero for an unpublished definition.
	pub(crate) defined_at: u64,
	/// Resolved exclusive history floor, independent of registration time.
	pub(crate) starting_after: u64,
	/// Latest lifecycle publication commit.
	pub(crate) published_at: u64,
	/// Confirmed complete-commit position.
	pub(crate) acknowledged: u64,
	/// Concrete transitive PURE versions captured by registration.
	pub(crate) functions: Vec<(super::FunctionId, super::SchemaVersion)>,
	/// Nested record sources retain their class incarnation across recreation.
	pub(crate) record_sources: Vec<(crate::values::Text, super::ClassId)>,
	/// An active evaluation failure suspends delivery without rejecting source
	/// DML.
	pub(crate) diagnostic: Option<FerrosGrynnError>,
}

impl LiveDefinition {
	/// Returns owned metadata without attaching a consumer or holding pins.
	pub(crate) fn info(&self, root: &DatabaseRoot) -> FerrosGrynnResult<LiveInfo> {
		self.info_bounded(root, self.options.scratch_bytes())
	}

	/// Introspection shares the caller's cap before retaining dependencies.
	pub(crate) fn info_bounded(
		&self,
		root: &DatabaseRoot,
		maximum_bytes: usize,
	) -> FerrosGrynnResult<LiveInfo> {
		let options = crate::live::LiveBindingOptions::default()
			.with_budgets(self.options.scratch_bytes().min(maximum_bytes), self.options.work())?;
		let mut admission = BindingAdmission::new(&options);
		let history = root.live_catalog().histories.get(&self.id);
		if history.is_some_and(|history| history.version != self.version) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE metadata history has another query version",
			));
		}
		let epochs = history.map_or(0, |history| history.epochs.len());
		let bytes = self
			.functions
			.len()
			.checked_mul(std::mem::size_of::<(super::FunctionId, super::SchemaVersion)>())
			.and_then(|bytes| {
				bytes.checked_add(
					self.record_sources
						.len()
						.checked_mul(std::mem::size_of::<(Text, super::ClassId)>())?,
				)
			})
			.and_then(|bytes| {
				bytes.checked_add(
					epochs.checked_mul(std::mem::size_of::<crate::live::LiveCaptureEpoch>())?,
				)
			})
			.and_then(|bytes| bytes.checked_add(std::mem::size_of::<LiveInfo>()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE owned metadata vectors"))?;
		let work = self
			.functions
			.len()
			.checked_add(self.record_sources.len())
			.and_then(|count| count.checked_add(epochs.checked_mul(2)?))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE metadata work"))?;
		admission.admit(bytes, work as u64)?;
		for (name, _) in &self.record_sources {
			admission.admit(name.len(), name.len() as u64)?;
		}
		let history = history
			.map(|history| -> FerrosGrynnResult<crate::live::LiveHistoryInfo> {
				history.validate(self.starting_after, root.commit_ts)?;
				let mut epochs = Vec::new();
				epochs.try_reserve_exact(history.epochs.len()).map_err(|error| {
					FerrosGrynnError::traversal_resource_limit_exceeded(
						"LIVE metadata epochs",
						options.scratch_bytes() as u64,
					)
					.with_technical_source(error)
				})?;
				for epoch in &history.epochs {
					let policy = epoch
						.policy
						.map(|policy| -> FerrosGrynnResult<crate::live::LiveCapturePolicy> {
							Ok(crate::live::LiveCapturePolicy {
								source: policy.source,
								revision: policy.revision,
								retention: crate::changefeed::ChangefeedConfig::from_nanoseconds(
									policy.retention_ns,
								)?
								.retention(),
							})
						})
						.transpose()?;
					epochs.push(crate::live::LiveCaptureEpoch {
						started_at: epoch.started_at.into(),
						policy,
					});
				}
				let purged_at = if history.purged_through == 0 {
					None
				} else {
					Some(crate::values::DateTime::from(
						jiff::Timestamp::from_nanosecond(history.purged_time_ns)?
							.to_zoned(jiff::tz::TimeZone::UTC),
					))
				};
				Ok(crate::live::LiveHistoryInfo {
					last_stamp: history.last_stamp.into(),
					purged_through: history.purged_through.into(),
					purged_at,
					epochs,
				})
			})
			.transpose()?;
		let dependencies = query_dependencies(&self.query, &mut admission)?;
		admission.admit(
			dependencies
				.len()
				.checked_mul(std::mem::size_of::<crate::live::LiveBindingRevision>() * 2)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE metadata bindings"))?,
			0,
		)?;
		let bindings = dependencies
			.into_iter()
			.map(|id| {
				let entry = root.live_catalog().bindings.get(&id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"LIVE query has an absent binding identity",
					)
				})?;
				Ok(crate::live::LiveBindingRevision {
					id,
					definition: entry.version,
					value: entry.value_at.into(),
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(LiveInfo {
			id: self.id,
			version: self.version,
			state: self.state,
			query: self.query.clone(),
			options: self.options.clone(),
			defined_at: self.defined_at.into(),
			published_at: self.published_at.into(),
			snapshot: root.commit_ts.into(),
			starting_after: self.starting_after.into(),
			history,
			functions: self.functions.clone(),
			record_sources: self.record_sources.clone(),
			capture_source: self.capture_source,
			source_class: self.source_class,
			cursor: LiveCursor::new(
				root.database_uuid.into(),
				self.id,
				self.version,
				self.acknowledged.into(),
			)?,
			bindings,
			diagnostic: self.diagnostic.clone(),
		})
	}
}

impl LiveCatalog {
	/// Resolves one stable value identity within the exact candidate catalog.
	pub(crate) fn binding_value(
		&self,
		id: crate::live::LiveBindingId,
	) -> FerrosGrynnResult<&crate::values::Value> {
		let entry = self.bindings.get(&id).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
		})?;
		match entry.state {
			LiveState::Active | LiveState::Pending => Ok(entry.value.as_ref()),
			LiveState::Closed => {
				Err(FerrosGrynnError::invalid_argument("LIVE LET identity was removed"))
			}
			LiveState::Paused => Err(entry.diagnostic.clone().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("paused LIVE LET has no diagnostic")
			})?),
		}
	}

	/// Checks catalog watermarks against the authoritative recovered commit
	/// clock.
	pub(crate) fn validate_recovered(&self, stamp: u64) -> FerrosGrynnResult<()> {
		for entry in self.queries.values() {
			if entry.defined_at == 0
				|| entry.state == LiveState::Pending
				|| entry.defined_at > entry.published_at
				|| entry.published_at > stamp
				|| !entry.options.since().matches_start(entry.starting_after, entry.defined_at)
				|| entry.acknowledged < entry.starting_after
				|| entry.acknowledged > stamp
				|| (entry.state == LiveState::Paused) != entry.diagnostic.is_some()
			{
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE catalog bounds exceed the authoritative recovered commit clock",
				));
			}
		}
		for entry in self.bindings.values() {
			if entry.defined_at == 0
				|| entry.state == LiveState::Pending
				|| entry.defined_at > entry.published_at
				|| entry.published_at > stamp
				|| entry.value_at < entry.defined_at
				|| entry.value_at > entry.published_at
				|| (entry.state == LiveState::Paused) != entry.diagnostic.is_some()
			{
				return Err(FerrosGrynnError::invalid_storage_file(
					"LIVE LET catalog bounds/state exceed the recovered commit contract",
				));
			}
		}
		self.binding_order(&mut BindingAdmission::new(&crate::live::LiveBindingOptions::default()))
			.map_err(|cause| {
				FerrosGrynnError::invalid_storage_file(
					"LIVE LET recovered dependency DAG is invalid",
				)
				.with_cause(cause)
			})?;
		Ok(())
	}

	/// Inserts one private definition; replay checks the incarnation again.
	pub(crate) fn register(&mut self, definition: Arc<LiveDefinition>) -> FerrosGrynnResult<()> {
		if self.queries.contains_key(&definition.id) {
			return Err(FerrosGrynnError::invalid_argument("LIVE identity is already registered"));
		}
		self.queries.insert(definition.id, definition);
		Ok(())
	}

	/// Explicit close is idempotent and keeps the old recipe available.
	pub(crate) fn close(&mut self, id: LiveId, stamp: u64) -> FerrosGrynnResult<()> {
		let definition = self
			.queries
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE identity is not registered"))?;
		let definition = Arc::make_mut(definition);
		definition.state = LiveState::Closed;
		definition.diagnostic = None;
		definition.published_at = stamp;
		Ok(())
	}

	/// Confirms a validated issued whole-commit position without changing the
	/// query incarnation or lifecycle version.
	pub(crate) fn acknowledge(
		&mut self,
		id: LiveId,
		version: LiveVersion,
		stamp: u64,
	) -> FerrosGrynnResult<()> {
		let definition = self
			.queries
			.get_mut(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE identity is not registered"))?;
		if definition.version != version || stamp < definition.starting_after {
			return Err(FerrosGrynnError::invalid_argument("LIVE ACK version/position mismatch"));
		}
		if stamp > definition.acknowledged {
			Arc::make_mut(definition).acknowledged = stamp;
		}
		Ok(())
	}

	/// Activates final private definitions once the complete commit is ready.
	pub(crate) fn activate(&mut self, stamp: u64) -> FerrosGrynnResult<()> {
		for definition in self.queries.values_mut() {
			if definition.defined_at == 0 {
				let definition = Arc::make_mut(definition);
				definition.defined_at = stamp;
				definition.published_at = stamp;
				if matches!(definition.options.since(), crate::live::LiveSince::Now) {
					definition.starting_after = stamp;
				}
				definition.acknowledged = definition.starting_after;
				if !definition.options.since().matches_start(definition.starting_after, stamp) {
					return Err(FerrosGrynnError::invalid_argument(
						"LIVE historical position exceeds its registration commit",
					));
				}
				if definition.state == LiveState::Pending {
					definition.state = LiveState::Active;
				}
			}
		}
		Ok(())
	}
}
