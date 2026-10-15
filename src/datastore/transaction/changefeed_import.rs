//! Bounded accepted-item spool; private segment rotation never discards
//! history.

use std::collections::BTreeMap;

use super::{
	Transaction,
	changefeed_capture::{admit_history_publication, install_capture},
	changefeed_journal::CaptureIntent,
	operation_image::{CaptureBudget, same_image, same_value},
};
use crate::{
	changefeed::{
		ChangeBatch,
		ChangeEntry,
		ChangeIdentity,
		ChangefeedGap,
		ChangefeedGapReason,
		ChangefeedId,
	},
	datastore::{CapturedCommit, DatabaseRoot, ImportChangefeedMode},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::DateTime,
};

/// A truncation-only spool savepoint, without image or collection copies.
#[derive(Clone, Copy)]
pub(super) struct SpoolCheckpoint {
	/// Retained accepted chunks.
	chunks: usize,
	/// Retained suppression controls.
	gaps: usize,
	/// Admitted retained metadata and payload bytes.
	bytes: usize,
	/// Compact NET targets retained at the boundary.
	targets: usize,
	/// Buffer capacity reservations survive rollback even when lengths shrink.
	capacity_bytes: usize,
	/// Metadata retained by the pending segment's immutable before-root.
	segment_bytes: usize,
	/// Pending inherited FULL targets in the current outer item.
	fanout: usize,
}

/// Private bounded chunks are immutable and become visible only at END.
#[derive(Default)]
pub(super) struct ImportCaptureSpool {
	/// Frozen import capture selection.
	pub(super) mode: ImportChangefeedMode,
	/// Single reserved final publication stamp.
	pub(super) stamp: u64,
	/// Accepted-item groups, retaining images across every segment.
	chunks: Vec<(ChangefeedId, CapturedCommit)>,
	/// Eligible suppressed item sources; duplicates are coalesced at END.
	gaps: Vec<ChangefeedId>,
	/// Aggregate pre-admitted spool bytes.
	bytes: usize,
	/// Accepted NET-only targets; OFF never copies primary payloads.
	targets: Vec<(ChangefeedId, CaptureIntent)>,
	/// Borrowed primary images used only to classify this item's inherited
	/// fanout.
	fanout: Vec<SuppressedFanout>,
	/// One immutable replay origin per ON segment, without per-item root
	/// copies.
	segment_before: Option<Box<DatabaseRoot>>,
	/// Admission retained until the segment origin is released.
	segment_bytes: usize,
}

/// One inherited field's first value in an OFF item's logical operation.
struct SuppressedFanout {
	/// Frozen graph source incarnation.
	feed: ChangefeedId,
	/// Occurrence's qualified graph identity.
	graph: crate::graph::GraphId,
	/// Effective inherited property.
	key: crate::values::PropertyKeyId,
	/// Global logical target before this item's first field change.
	global: crate::datastore::GlobalAtomId,
	/// An Arc clone holds the before value without copying a primary payload.
	before: std::sync::Arc<crate::datastore::BaseAtom>,
}

impl ImportCaptureSpool {
	/// Records rollback positions for item and TRY boundaries.
	pub(super) fn checkpoint(&self) -> SpoolCheckpoint {
		SpoolCheckpoint {
			chunks: self.chunks.len(),
			gaps: self.gaps.len(),
			bytes: self.bytes,
			targets: self.targets.len(),
			capacity_bytes: self.capacity_bytes(),
			segment_bytes: self.segment_bytes,
			fanout: self.fanout.len(),
		}
	}

	/// Discards failed item chunks, ordinals and their reservations.
	pub(super) fn restore(&mut self, checkpoint: SpoolCheckpoint) {
		self.chunks.truncate(checkpoint.chunks);
		self.gaps.truncate(checkpoint.gaps);
		self.targets.truncate(checkpoint.targets);
		self.fanout.truncate(checkpoint.fanout);
		self.bytes = checkpoint
			.bytes
			.saturating_add(self.capacity_bytes().saturating_sub(checkpoint.capacity_bytes))
			.saturating_add(self.segment_bytes.saturating_sub(checkpoint.segment_bytes));
	}

	/// Returns all retained ON allocations for shared transaction admission.
	pub(super) const fn retained_bytes(&self) -> usize {
		match self.mode {
			ImportChangefeedMode::On => self.bytes,
			ImportChangefeedMode::Off => 0,
		}
	}

	/// Counts fixed reserved slots, including spare capacity after item
	/// rollback.
	fn capacity_bytes(&self) -> usize {
		self.chunks
			.capacity()
			.saturating_mul(std::mem::size_of::<(ChangefeedId, CapturedCommit)>())
			.saturating_add(
				self.gaps.capacity().saturating_mul(std::mem::size_of::<ChangefeedId>()),
			)
			.saturating_add(
				self.targets
					.capacity()
					.saturating_mul(std::mem::size_of::<(ChangefeedId, CaptureIntent)>()),
			)
			.saturating_add(
				self.fanout.capacity().saturating_mul(std::mem::size_of::<SuppressedFanout>()),
			)
	}
}

impl Transaction {
	/// Captures the accepted operation ranges before their segment is cleared.
	pub(super) fn retain_import_changes(
		&mut self,
		before: &DatabaseRoot,
		start: usize,
	) -> FerrosGrynnResult<bool> {
		let maximum =
			self.store.inner.transaction_limits().max_write_set_bytes.min(256 * 1024 * 1024);
		let transient = before
			.graphs
			.len()
			.checked_mul(256)
			.and_then(|bytes| bytes.checked_add(self.write_bytes))
			.and_then(|bytes| bytes.checked_add(self.admitted_journal_bytes()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("import replay scratch"))?;
		let retained = self.import_changefeed.bytes;
		let mut allowance = CaptureBudget {
			bytes: self.import_changefeed.bytes,
			maximum,
		};
		allowance.admit(self.write_bytes)?;
		allowance.admit(before.graphs.len().checked_mul(128).ok_or_else(|| {
			FerrosGrynnError::operation_overflow("import capture root metadata")
		})?)?;
		let (groups, bytes) = self.capture_groups(
			before,
			&self.working,
			self.import_changefeed.stamp,
			start,
			allowance.bytes,
		)?;
		let changed = !groups.is_empty();
		let mut budget = CaptureBudget {
			bytes,
			maximum,
		};
		for (feed, group) in groups {
			budget.reserve(&mut self.import_changefeed.chunks)?;
			self.import_changefeed.chunks.push((feed, group));
		}
		self.import_changefeed.bytes = if changed {
			budget.bytes.checked_sub(transient).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("import replay scratch was not admitted")
			})?
		} else {
			retained
		};
		self.store.inner.changefeed_observation.scratch(budget.bytes as u64);
		self.change_journal.clear_segment();
		Ok(changed)
	}

	/// Admits and retains the private replay origin only once per ON segment.
	pub(super) fn prepare_import_capture_segment(&mut self) -> FerrosGrynnResult<()> {
		if self.import_changefeed.segment_before.is_some() {
			return Ok(());
		}
		let bytes = self
			.working
			.graphs
			.len()
			.checked_mul(128)
			.and_then(|bytes| bytes.checked_add(std::mem::size_of::<DatabaseRoot>()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("import capture root metadata"))?;
		let mut budget = CaptureBudget {
			bytes: self.import_changefeed.bytes,
			maximum: self
				.store
				.inner
				.transaction_limits()
				.max_write_set_bytes
				.min(256 * 1024 * 1024),
		};
		budget.admit(bytes)?;
		self.ensure_capacity(bytes)?;
		// The first captured item precedes ordinary mutation preparation.
		// Its retained origin must already support exact replay deltas.
		self.store.inner.hydrate_statistics(&mut self.working)?;
		self.import_changefeed.segment_before = Some(Box::new(self.working.clone()));
		self.import_changefeed.segment_bytes = bytes;
		self.import_changefeed.bytes = budget.bytes;
		Ok(())
	}

	/// Freezes the complete accepted segment before releasing its replay
	/// origin.
	pub(super) fn retain_import_segment(&mut self) -> FerrosGrynnResult<bool> {
		let Some(before) = self.import_changefeed.segment_before.take() else {
			return Ok(false);
		};
		let changed = self.retain_import_changes(&before, 0)?;
		self.import_changefeed.bytes -= self.import_changefeed.segment_bytes;
		self.import_changefeed.segment_bytes = 0;
		Ok(changed)
	}

	/// Retains only eligible source identities for an OFF item's final NET
	/// comparison.
	pub(super) fn retain_import_suppression(&mut self, start: usize) -> FerrosGrynnResult<bool> {
		let mut budget = CaptureBudget {
			bytes: self.import_changefeed.bytes,
			maximum: self
				.store
				.inner
				.transaction_limits()
				.max_write_set_bytes
				.min(256 * 1024 * 1024),
		};
		for target in self.import_changefeed.fanout.drain(..) {
			if !suppression_has_images(
				&self.working,
				&self.mutations[start..],
				target.global,
				Some(target.graph),
			) {
				continue;
			}
			let value = self
				.working
				.resolve_atom(target.before.record_id())
				.and_then(|global| {
					self.working.membership(target.graph, global).and_then(|local| {
						self.working
							.occurrence(target.graph, local)
							.map(|occurrence| (global, occurrence))
					})
				})
				.and_then(|(global, occurrence)| {
					occurrence.effective_value(
						self.working
							.atom(global)
							.and_then(|atom| atom.properties().get(target.key)),
						target.key,
					)
				});
			if !same_optional_value(target.before.properties().get(target.key), value) {
				budget.reserve(&mut self.import_changefeed.gaps)?;
				self.import_changefeed.gaps.push(target.feed);
			}
		}
		for intent in &self.change_journal.intents {
			if !suppression_has_images(
				&self.working,
				&self.mutations[start..],
				intent.global,
				intent.graph,
			) {
				continue;
			}
			let source = intent.graph.map_or(
				crate::changefeed::ChangefeedSource::Class(intent.class),
				crate::changefeed::ChangefeedSource::Graph,
			);
			retain_suppression_target(
				&self.working,
				&mut self.import_changefeed,
				*intent,
				source,
				true,
				&mut budget,
			)?;
			if intent.graph.is_none() {
				for root in [self.snapshot.as_ref(), &self.working] {
					for graph in root
						.memberships(intent.global)
						.into_iter()
						.flat_map(|memberships| memberships.keys())
					{
						let mut qualified = *intent;
						qualified.graph = Some(*graph);
						retain_suppression_target(
							&self.working,
							&mut self.import_changefeed,
							qualified,
							crate::changefeed::ChangefeedSource::Graph(*graph),
							false,
							&mut budget,
						)?;
					}
				}
			}
		}
		self.import_changefeed.bytes = budget.bytes;
		let changed = !self.import_changefeed.gaps.is_empty();
		self.change_journal.clear_segment();
		Ok(changed)
	}

	/// Retains first inherited values; eligibility is decided at the outer item
	/// boundary.
	pub(super) fn note_import_suppressed_fanout(
		&mut self,
		global: crate::datastore::GlobalAtomId,
		key: crate::values::PropertyKeyId,
		value: Option<&crate::values::Value>,
		fanout: &[crate::graph::GraphId],
	) -> FerrosGrynnResult<()> {
		if self.import_constraints.is_none()
			|| self.import_changefeed.mode != ImportChangefeedMode::Off
		{
			return Ok(());
		}
		let previous = self.working.atom(global).and_then(|atom| atom.properties().get(key));
		if same_optional_value(previous, value) {
			return Ok(());
		}
		let mut budget = CaptureBudget {
			bytes: self.import_changefeed.bytes,
			maximum: self
				.store
				.inner
				.transaction_limits()
				.max_write_set_bytes
				.min(256 * 1024 * 1024),
		};
		for graph in fanout {
			if let Some(definition) = self
				.working
				.changefeeds()
				.for_source(crate::changefeed::ChangefeedSource::Graph(*graph))
				&& definition.state == crate::changefeed::ChangefeedState::Active
				&& definition.config.full()
				&& !self.import_changefeed.fanout.iter().any(|target| {
					target.global == global && target.graph == *graph && target.key == key
				}) {
				budget.reserve(&mut self.import_changefeed.fanout)?;
				let before = self.working.atom_arc(global).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("suppressed fanout atom is absent")
				})?;
				self.import_changefeed.fanout.push(SuppressedFanout {
					feed: definition.id,
					graph: *graph,
					global,
					key,
					before,
				});
			}
		}
		self.import_changefeed.bytes = budget.bytes;
		Ok(())
	}

	/// Assigns one final time/stamp and installs complete accepted history
	/// after validation.
	#[allow(
		clippy::mutable_key_type,
		reason = "owned immutable change identities preserve canonical ordering despite interned \
		          Text storage"
	)]
	pub(super) fn publish_import_changes(&mut self, stamp: u64) -> FerrosGrynnResult<()> {
		if self.import_changefeed.chunks.is_empty()
			&& self.import_changefeed.gaps.is_empty()
			&& self.import_changefeed.targets.is_empty()
		{
			return Ok(());
		}
		// Retain the accepted spool until the root passes optimistic
		// publication. A control race must not consume accepted history.
		let spool = &self.import_changefeed;
		self.working.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
		let timestamp =
			jiff::Timestamp::from_nanosecond(self.working.committed_at_ns).map_err(|error| {
				FerrosGrynnError::invalid_argument("import changefeed commit time is invalid")
					.with_technical_source(error)
			})?;
		let committed_at = DateTime::new(timestamp.to_zoned(jiff::tz::TimeZone::UTC));
		let maximum =
			self.store.inner.transaction_limits().max_write_set_bytes.min(256 * 1024 * 1024);
		let mut budget = CaptureBudget {
			bytes: spool.bytes,
			maximum,
		};
		let mut net: BTreeMap<(ChangefeedId, ChangeIdentity), ChangeEntry> = BTreeMap::new();
		let mut full: BTreeMap<ChangefeedId, Vec<ChangeEntry>> = BTreeMap::new();
		let mut originals: BTreeMap<ChangefeedId, bool> = BTreeMap::new();
		for (feed, group) in &spool.chunks {
			budget.admit(128)?;
			originals.insert(*feed, group.include_original);
			for entry in &group.net.entries {
				let identity = entry
					.after
					.as_ref()
					.or(entry.before.as_ref())
					.map(|image| image.identity())
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("import NET entry has no identity")
					})?;
				budget.admit(512)?;
				let key = (*feed, identity.clone());
				if let Some(previous) = net.get_mut(&key) {
					previous.after = entry.after.clone();
				} else {
					net.insert(key, entry.clone());
				}
			}
			if let Some(batch) = &group.full {
				budget.admit(128)?;
				let entries = full.entry(*feed).or_default();
				for entry in &batch.entries {
					budget.reserve(entries)?;
					let mut entry = entry.clone();
					entry.ordinal = u64::try_from(entries.len()).map_err(|_error| {
						FerrosGrynnError::operation_overflow("import FULL ordinal")
					})?;
					entries.push(entry);
				}
			}
		}
		let mut groups = BTreeMap::new();
		for (feed, include_original) in originals {
			budget.admit(256)?;
			let batch = ChangeBatch {
				source: feed,
				versionstamp: stamp.into(),
				committed_at: committed_at.clone(),
				entries: Vec::new(),
			};
			let full = full.remove(&feed).map(|entries| ChangeBatch {
				source: feed,
				versionstamp: stamp.into(),
				committed_at: committed_at.clone(),
				entries,
			});
			groups.insert(feed, CapturedCommit {
				revision: self
					.working
					.changefeeds()
					.definitions
					.get(&feed)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("import capture revision is absent")
					})?
					.version,
				net: batch,
				full,
				include_original,
				gap: None,
			});
		}
		for ((feed, _identity), mut entry) in net {
			if same_image(entry.before.as_deref(), entry.after.as_deref()) {
				continue;
			}
			let group = groups.get_mut(&feed).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("import feed descriptor is absent")
			})?;
			budget.reserve(&mut group.net.entries)?;
			entry.ordinal = u64::try_from(group.net.entries.len())
				.map_err(|_error| FerrosGrynnError::operation_overflow("import NET ordinal"))?;
			group.net.entries.push(entry);
		}
		let mut gaps = Vec::new();
		for feed in &spool.gaps {
			budget.reserve(&mut gaps)?;
			gaps.push(*feed);
		}
		for (feed, intent) in &spool.targets {
			let record = self
				.snapshot
				.atom(intent.global)
				.or_else(|| self.working.atom(intent.global))
				.map(|atom| atom.record_id());
			if let Some(record) = record
				&& !same_suppressed_target(&self.snapshot, &self.working, record, intent.graph)?
			{
				budget.reserve(&mut gaps)?;
				gaps.push(*feed);
			}
		}
		for feed in gaps {
			if groups.contains_key(&feed) {
				continue;
			}
			budget.admit(256)?;
			let batch = ChangeBatch {
				source: feed,
				versionstamp: stamp.into(),
				committed_at: committed_at.clone(),
				entries: Vec::new(),
			};
			groups.insert(feed, CapturedCommit {
				revision: self
					.working
					.changefeeds()
					.definitions
					.get(&feed)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("import capture revision is absent")
					})?
					.version,
				net: batch,
				full: None,
				include_original: false,
				gap: Some(ChangefeedGap {
					source: feed,
					reason: ChangefeedGapReason::ImportSuppressed,
					from: Some(stamp.into()),
					through: Some(stamp.into()),
				}),
			});
		}
		for (feed, group) in groups {
			if group.net.entries.is_empty()
				&& group.full.as_ref().is_none_or(|batch| batch.entries.is_empty())
				&& group.gap.is_none()
			{
				continue;
			}
			admit_history_publication(&self.working, feed, &mut budget)?;
			install_capture(&mut self.working, feed, stamp, group);
		}
		Ok(())
	}
}

/// Checks image presence at the same outer boundary as ON without copying the
/// root.
fn suppression_has_images(
	root: &DatabaseRoot,
	mutations: &[super::Mutation],
	global: crate::datastore::GlobalAtomId,
	graph: Option<crate::graph::GraphId>,
) -> bool {
	if let Some(graph) = graph {
		let before = mutations
			.iter()
			.find_map(|mutation| match mutation {
				super::Mutation::AttachNode {
					graph: changed,
					global: target,
					..
				}
				| super::Mutation::AttachEdge {
					graph: changed,
					global: target,
					..
				} if *changed == graph && *target == global => Some(false),
				super::Mutation::Detach {
					graph: changed,
					global: target,
				} if *changed == graph && *target == global => Some(true),
				_ => None,
			})
			.unwrap_or(true);
		before || root.membership(graph, global).is_some()
	} else {
		!mutations.iter().any(
			|mutation| matches!(mutation, super::Mutation::CreateAtom { id, .. } if *id == global),
		) || root.atom(global).is_some()
	}
}

/// Admits one compact source intent; suppressed payloads never enter the spool.
fn retain_suppression_target(
	root: &DatabaseRoot,
	spool: &mut ImportCaptureSpool,
	intent: CaptureIntent,
	source: crate::changefeed::ChangefeedSource,
	explicit: bool,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<()> {
	let Some(definition) = root.changefeeds().for_source(source) else {
		return Ok(());
	};
	if definition.state != crate::changefeed::ChangefeedState::Active {
		return Ok(());
	}
	if definition.config.full() && explicit {
		budget.reserve(&mut spool.gaps)?;
		spool.gaps.push(definition.id);
	} else {
		budget.reserve(&mut spool.targets)?;
		spool.targets.push((definition.id, intent));
	}
	Ok(())
}

/// Compares optional property storage representations without copying payloads.
fn same_optional_value(
	left: Option<&crate::values::Value>,
	right: Option<&crate::values::Value>,
) -> bool {
	match (left, right) {
		(None, None) => true,
		(Some(left), Some(right)) => same_value(left, right),
		_ => false,
	}
}

/// Exact borrowed image comparison for OFF imports with frozen
/// schema/dictionaries.
fn same_suppressed_target(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	record: &crate::values::RecordId,
	graph: Option<crate::graph::GraphId>,
) -> FerrosGrynnResult<bool> {
	let left = before
		.resolve_atom(record)
		.filter(|global| graph.is_none_or(|graph| before.membership(graph, *global).is_some()));
	let right = after
		.resolve_atom(record)
		.filter(|global| graph.is_none_or(|graph| after.membership(graph, *global).is_some()));
	let (left, right) = match (left, right) {
		(None, None) => return Ok(true),
		(Some(left), Some(right)) => (left, right),
		_ => return Ok(false),
	};
	let left_atom = before.atom(left).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("suppressed before atom is absent")
	})?;
	let right_atom = after
		.atom(right)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("suppressed after atom is absent"))?;
	if left_atom.kind() != right_atom.kind() || left_atom.class_id() != right_atom.class_id() {
		return Ok(false);
	}
	let Some(graph) = graph else {
		return Ok(left_atom.properties().len() == right_atom.properties().len()
			&& left_atom.properties().iter().all(|(key, value)| {
				same_optional_value(Some(value), right_atom.properties().get(key))
			}));
	};
	let left_local = before.membership(graph, left).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("suppressed before membership is absent")
	})?;
	let right_local = after.membership(graph, right).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("suppressed after membership is absent")
	})?;
	let left_occurrence = before.occurrence(graph, left_local).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("suppressed before occurrence is absent")
	})?;
	let right_occurrence = after.occurrence(graph, right_local).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("suppressed after occurrence is absent")
	})?;
	if left_occurrence.label() != right_occurrence.label() {
		return Ok(false);
	}
	match (left_occurrence.overlay(), right_occurrence.overlay()) {
		(None, None) => {}
		(Some(left), Some(right)) if left.iter().len() == right.iter().len() => {
			for (key, decision) in left.iter() {
				let equal = match (decision, right.get(key)) {
					(
						crate::datastore::OverlayValue::Hidden,
						Some(crate::datastore::OverlayValue::Hidden),
					) => true,
					(
						crate::datastore::OverlayValue::Value(left),
						Some(crate::datastore::OverlayValue::Value(right)),
					) => same_value(left, right),
					_ => false,
				};
				if !equal {
					return Ok(false);
				}
			}
		}
		_ => return Ok(false),
	}
	for key in left_atom
		.properties()
		.iter()
		.map(|(key, _)| key)
		.chain(right_atom.properties().iter().map(|(key, _)| key))
	{
		if !same_optional_value(
			left_occurrence.effective_value(left_atom.properties().get(key), key),
			right_occurrence.effective_value(right_atom.properties().get(key), key),
		) {
			return Ok(false);
		}
	}
	match (left_occurrence.state().view(), right_occurrence.state().view()) {
		(
			crate::datastore::OccurrenceStateRef::Node {
				nested_graph: left,
			},
			crate::datastore::OccurrenceStateRef::Node {
				nested_graph: right,
			},
		) => Ok(left == right),
		(
			crate::datastore::OccurrenceStateRef::Edge {
				topology: left,
				weight: lw,
				transition_graph: lg,
			},
			crate::datastore::OccurrenceStateRef::Edge {
				topology: right,
				weight: rw,
				transition_graph: rg,
			},
		) => {
			let weights_equal = match (lw, rw) {
				(None, None) => true,
				(Some(left), Some(right)) => super::operation_image::same_number(*left, *right),
				_ => false,
			};
			if left.direction() != right.direction()
				|| lg != rg
				|| !weights_equal
				|| left.source().len() != right.source().len()
				|| left.target().len() != right.target().len()
			{
				return Ok(false);
			}
			for (left, right) in left
				.source()
				.iter()
				.chain(left.target())
				.zip(right.source().iter().chain(right.target()))
			{
				if !same_suppressed_endpoint(before, after, graph, left, right)? {
					return Ok(false);
				}
			}
			Ok(true)
		}
		_ => Ok(false),
	}
}

/// Compares each qualified route occurrence by public identity rather than
/// local slot.
fn same_suppressed_endpoint(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	origin: crate::graph::GraphId,
	left: &crate::graph::EdgeEndpoint,
	right: &crate::graph::EdgeEndpoint,
) -> FerrosGrynnResult<bool> {
	let parts = |endpoint: &crate::graph::EdgeEndpoint| match endpoint {
		crate::graph::EdgeEndpoint::Local(local) => (0, *local),
		crate::graph::EdgeEndpoint::Nested(qualified) => {
			(qualified.scope().len(), qualified.atom())
		}
	};
	let (left_length, left_terminal) = parts(left);
	let (right_length, right_terminal) = parts(right);
	if left_length != right_length {
		return Ok(false);
	}
	let left_scope = match left {
		crate::graph::EdgeEndpoint::Local(_) => &[][..],
		crate::graph::EdgeEndpoint::Nested(qualified) => qualified.scope(),
	};
	let right_scope = match right {
		crate::graph::EdgeEndpoint::Local(_) => &[][..],
		crate::graph::EdgeEndpoint::Nested(qualified) => qualified.scope(),
	};
	let mut left_graph = origin;
	let mut right_graph = origin;
	for (index, (left, right)) in left_scope
		.iter()
		.copied()
		.zip(right_scope.iter().copied())
		.chain(std::iter::once((left_terminal, right_terminal)))
		.enumerate()
	{
		if left_graph != right_graph {
			return Ok(false);
		}
		let left_occurrence = before.occurrence(left_graph, left).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("suppressed before endpoint is absent")
		})?;
		let right_occurrence = after.occurrence(right_graph, right).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("suppressed after endpoint is absent")
		})?;
		let left_atom = before.atom(left_occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("suppressed before endpoint atom is absent")
		})?;
		let right_atom = after.atom(right_occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("suppressed after endpoint atom is absent")
		})?;
		if left_atom.kind() != right_atom.kind() || left_atom.record_id() != right_atom.record_id()
		{
			return Ok(false);
		}
		let child = |occurrence: &crate::datastore::GraphOccurrence| match occurrence.state().view()
		{
			crate::datastore::OccurrenceStateRef::Node {
				nested_graph,
			} => *nested_graph,
			crate::datastore::OccurrenceStateRef::Edge {
				transition_graph,
				..
			} => *transition_graph,
		};
		if index < left_length {
			match (child(left_occurrence), child(right_occurrence)) {
				(Some(left), Some(right)) => {
					left_graph = left;
					right_graph = right;
				}
				_ => {
					return Err(FerrosGrynnError::invalid_storage_file(
						"suppressed endpoint owner has no child graph",
					));
				}
			}
		}
	}
	Ok(true)
}

#[cfg(test)]
/// Regression tests for changefeed import behavior.
mod tests {
	use super::*;
	use crate::{
		changefeed::{ChangefeedConfig, ChangefeedSource},
		datastore::{
			ClassSchema,
			Datastore,
			ImportOptions,
			ImportViolationPolicy,
			InMemoryOptions,
		},
		graph::AtomKind,
		values::{PropertyKeyId, PropertyMap, RecordId, Value},
	};

	/// Creates one configured FULL source without any initial history.
	fn fixture() -> FerrosGrynnResult<(Datastore, RecordId, ChangefeedId, PropertyKeyId)> {
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let key = tx.intern_property("value")?;
		let record = "item:a".parse::<RecordId>()?;
		let mut properties = PropertyMap::new();
		properties.insert(key, 1);
		tx.create_atom(record.clone(), properties)?;
		tx.commit()?;
		let mut configure = store.begin();
		let duration = "P1D"
			.parse()
			.map_err(|error: jiff::Error| FerrosGrynnError::invalid_argument(error.to_string()))?;
		let feed = configure.configure_changefeed(
			ChangefeedSource::Class(class),
			ChangefeedConfig::new(duration)?.with_full(true),
		)?;
		configure.commit()?;
		Ok((store, record, feed, key))
	}

	/// Verifies changefeed import on preserves accepted item images across
	/// seals.
	#[test]
	fn changefeed_import_on_preserves_accepted_item_images_across_seals() {
		use crate::test_assertions::AssertResult as _;

		for segment_size in [1, 3, 64] {
			let (store, record, feed, key) = fixture().assert_ok();
			let old = store.inner.current_root();
			let expected_stamp = old.commit_ts + 1;
			let options = ImportOptions {
				changefeed: ImportChangefeedMode::On,
				segment_size,
				on_violation: ImportViolationPolicy::Skip,
				reject_limit: Some(1),
				..ImportOptions::default()
			};
			let report = store
				.import(options, |session| {
					session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(2)))?;
					session.apply_item(|tx| tx.create_atom(record.clone(), PropertyMap::new()))?;
					session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(3)))?;
					session.apply_item(|tx| {
						tx.remove_base_property(&record, PropertyKeyId::new(99))
					})?;
					session.apply_item(|tx| tx.delete_atom(&record))?;
					session.apply_item(|tx| {
						let mut properties = PropertyMap::new();
						properties.insert(key, 3);
						tx.create_atom(record.clone(), properties)
					})?;
					assert!(old.change_history().is_empty());
					assert_eq!(store.inner.current_root().commit_ts, old.commit_ts);
					Ok(())
				})
				.assert_ok();
			assert_eq!(report.segments(), 5_usize.div_ceil(segment_size) as u64);
			assert_eq!(report.applied_items(), 5);
			assert_eq!(report.skipped_items(), 1);
			let root = store.inner.current_root();
			assert_eq!(root.commit_ts, expected_stamp);
			let group = root
				.change_history()
				.get(&(feed, expected_stamp))
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("fixture import history is absent")
				})
				.assert_ok();
			assert_eq!(group.net.entries().len(), 1);
			assert_eq!(
				group.net.entries()[0].before().and_then(|image| image.properties().get("value")),
				Some(&Value::from(1))
			);
			assert_eq!(
				group.net.entries()[0].after().and_then(|image| image.properties().get("value")),
				Some(&Value::from(3))
			);
			let full = group
				.full
				.as_ref()
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("fixture FULL import history is absent")
				})
				.assert_ok();
			assert_eq!(full.entries().len(), 5);
			assert!(full.entries()[3].after().is_none());
			assert!(full.entries()[4].before().is_none());
			for (ordinal, entry) in full.entries().iter().enumerate() {
				assert_eq!(entry.ordinal(), ordinal as u64);
				assert_eq!(entry.operation(), ordinal as u64);
			}
			assert_eq!(
				full.entries()[1].before().and_then(|image| image.properties().get("value")),
				Some(&Value::from(2))
			);
			assert_eq!(
				group.net.committed_at().as_inner().timestamp().as_nanosecond(),
				root.committed_at_ns
			);
		}
	}

	/// Verifies changefeed import seal failure cannot be swallowed before end.
	#[test]
	fn changefeed_import_seal_failure_cannot_be_swallowed_before_end() {
		use crate::test_assertions::AssertResult as _;

		for segment_size in [8, 64] {
			let (store, record, _feed, _key) = fixture().assert_ok();
			let mut populate = store.begin();
			let payload = populate.intern_property("payload").assert_ok();
			populate
				.set_base_property(&record, payload, Value::from("x".repeat(16 * 1024)))
				.assert_ok();
			populate.commit().assert_ok();
			let before = store.inner.current_root();
			let limits_defaults = InMemoryOptions::default();
			let limits = InMemoryOptions {
				max_write_set_bytes: 16 * 1024,
				..limits_defaults
			};
			let limited = Datastore::from_root_for_test(limits, (*before).clone());
			let mut rejected = false;
			let result = limited.import(
				ImportOptions {
					changefeed: ImportChangefeedMode::On,
					segment_size,
					..ImportOptions::default()
				},
				|session| {
					for _item in 0..8 {
						if let Err(error) = session.apply_item(|tx| {
							tx.remove_base_property(&record, PropertyKeyId::new(99))
						}) {
							assert_eq!(
								error.status().condition(),
								crate::gql_status::Condition::ProgramLimitExceeded
							);
							rejected = true;
							assert!(session.apply_item(|_tx| Ok(())).is_err());
							break;
						}
					}
					Ok(())
				},
			);
			assert!(result.is_err());
			assert_eq!(rejected, segment_size == 8);
			let after = limited.inner.current_root();
			assert_eq!(after.commit_ts, before.commit_ts);
			assert_eq!(after.change_history().len(), before.change_history().len());
			assert_eq!(
				after.atom_by_record(&record).map(|atom| atom.properties()),
				before.atom_by_record(&record).map(|atom| atom.properties())
			);
		}
	}

	/// Verifies changefeed import empty capture releases replay scratch.
	#[test]
	fn changefeed_import_empty_capture_releases_replay_scratch() {
		use crate::test_assertions::AssertResult as _;

		let (store, _record, _feed, _key) = fixture().assert_ok();
		let mut setup = store.begin();
		setup.define_class(ClassSchema::new("other", AtomKind::Node, []).assert_ok()).assert_ok();
		let record: RecordId = "other:a".parse().assert_ok();
		setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		setup.commit().assert_ok();
		let before = store.inner.current_root();
		let limits_defaults = InMemoryOptions::default();
		let limits = InMemoryOptions {
			max_write_set_bytes: 4096,
			..limits_defaults
		};
		let limited = Datastore::from_root_for_test(limits, (*before).clone());
		let report = limited
			.import(
				ImportOptions {
					changefeed: ImportChangefeedMode::On,
					segment_size: 1,
					..ImportOptions::default()
				},
				|session| {
					for _item in 0..200 {
						session.apply_item(|tx| {
							tx.remove_base_property(&record, PropertyKeyId::new(99))
						})?;
					}
					Ok(())
				},
			)
			.assert_ok();
		assert_eq!(report.applied_items(), 200);
		assert_eq!(limited.inner.current_root().commit_ts, before.commit_ts);
		assert!(limited.inner.current_root().change_history().is_empty());
	}

	/// Verifies changefeed import off emits gap and rejected or empty imports
	/// emit nothing.
	#[test]
	fn changefeed_import_off_emits_gap_and_rejected_or_empty_imports_emit_nothing() {
		use crate::test_assertions::AssertResult as _;

		let (store, record, feed, key) = fixture().assert_ok();
		let options = ImportOptions {
			segment_size: 1,
			..ImportOptions::default()
		};
		store
			.import(options, |session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(2)))?;
				Ok(())
			})
			.assert_ok();
		let root = store.inner.current_root();
		let stamp = root.commit_ts;
		assert_eq!(
			root.change_history()
				.get(&(feed, stamp))
				.and_then(|group| group.gap.as_ref())
				.map(ChangefeedGap::reason),
			Some(ChangefeedGapReason::ImportSuppressed)
		);
		store.import(options, |_session| Ok(())).assert_ok();
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		let rejected = ImportOptions {
			on_violation: ImportViolationPolicy::Skip,
			reject_limit: Some(1),
			..options
		};
		store
			.import(rejected, |session| {
				session.apply_item(|tx| tx.create_atom(record.clone(), PropertyMap::new()))?;
				Ok(())
			})
			.assert_ok();
		assert_eq!(store.inner.current_root().commit_ts, stamp);
		let full_noop = ImportOptions {
			changefeed: ImportChangefeedMode::On,
			..options
		};
		store
			.import(full_noop, |session| {
				session
					.apply_item(|tx| tx.remove_base_property(&record, PropertyKeyId::new(99)))?;
				Ok(())
			})
			.assert_ok();
		assert_eq!(store.inner.current_root().commit_ts, stamp + 1);
	}

	/// Verifies changefeed import spool rejection publishes neither primary nor
	/// history.
	#[test]
	fn changefeed_import_spool_rejection_publishes_neither_primary_nor_history() {
		use crate::test_assertions::AssertResult as _;

		let (store, record, _feed, _key) = fixture().assert_ok();
		let before = store.inner.current_root();
		let limits_defaults = InMemoryOptions::default();
		let limits = InMemoryOptions {
			max_write_set_bytes: 4096,
			..limits_defaults
		};
		let limited = Datastore::from_root_for_test(limits, (*before).clone());
		let options = ImportOptions {
			changefeed: ImportChangefeedMode::On,
			segment_size: 1,
			..ImportOptions::default()
		};
		let result = limited.import(options, |session| {
			for _item in 0..100 {
				session
					.apply_item(|tx| tx.remove_base_property(&record, PropertyKeyId::new(99)))?;
			}
			Ok(())
		});
		assert!(result.is_err());
		let after = limited.inner.current_root();
		assert_eq!(after.commit_ts, before.commit_ts);
		assert!(after.change_history().is_empty());
	}

	/// Verifies changefeed import off avoids oversized images and net restore
	/// gaps.
	#[test]
	fn changefeed_import_off_avoids_oversized_images_and_net_restore_gaps() {
		use crate::test_assertions::AssertResult as _;

		let (store, record, feed, key) = fixture().assert_ok();
		let mut populate = store.begin();
		let payload = populate.intern_property("payload").assert_ok();
		populate
			.set_base_property(&record, payload, Value::from("x".repeat(64 * 1024)))
			.assert_ok();
		populate.commit().assert_ok();
		let root = store.inner.current_root();
		let options_defaults = InMemoryOptions::default();
		let options = InMemoryOptions {
			max_write_set_bytes: 8192,
			..options_defaults
		};
		let limited = Datastore::from_root_for_test(options, (*root).clone());
		limited
			.import(ImportOptions::default(), |session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(2)))?;
				Ok(())
			})
			.assert_ok();
		let next = limited.inner.current_root();
		assert_eq!(
			next.change_history()
				.get(&(feed, next.commit_ts))
				.and_then(|group| group.gap.as_ref())
				.map(ChangefeedGap::reason),
			Some(ChangefeedGapReason::ImportSuppressed)
		);
		let result = limited.import(
			ImportOptions {
				changefeed: ImportChangefeedMode::On,
				..ImportOptions::default()
			},
			|session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(3)))?;
				Ok(())
			},
		);
		assert!(result.is_err());
		assert_eq!(limited.inner.current_root().commit_ts, next.commit_ts);
		let mut configure = store.begin();
		let class = configure
			.working
			.atom_by_record(&record)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture atom is absent"))
			.assert_ok()
			.class_id();
		let duration = "P1D"
			.parse()
			.map_err(|error: jiff::Error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok();
		configure
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::new(duration).assert_ok(),
			)
			.assert_ok();
		configure.commit().assert_ok();
		store
			.import(
				ImportOptions {
					segment_size: 1,
					..ImportOptions::default()
				},
				|session| {
					session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(8)))?;
					session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(1)))?;
					Ok(())
				},
			)
			.assert_ok();
		let root = store.inner.current_root();
		assert!(!root.change_history().contains_key(&(feed, root.commit_ts)));
	}

	/// Verifies changefeed import off graph fanout respects masks and full
	/// history.
	#[test]
	fn changefeed_import_off_graph_fanout_respects_masks_and_full_history() {
		use crate::test_assertions::AssertResult as _;

		let (store, record, _feed, key) = fixture().assert_ok();
		let mut setup = store.begin();
		let graph = setup.define_graph("scope", crate::graph::GraphKind::Graph).assert_ok();
		setup.attach_node(graph.id(), &record, None, None).assert_ok();
		let duration = "P1D"
			.parse()
			.map_err(|error: jiff::Error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok();
		let feed = setup
			.configure_changefeed(
				ChangefeedSource::Graph(graph.id()),
				ChangefeedConfig::new(duration).assert_ok().with_full(true),
			)
			.assert_ok();
		setup.hide_property(graph.id(), &record, key).assert_ok();
		setup.commit().assert_ok();
		store
			.import(ImportOptions::default(), |session| {
				session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(2)))?;
				Ok(())
			})
			.assert_ok();
		let root = store.inner.current_root();
		assert!(!root.change_history().contains_key(&(feed, root.commit_ts)));
		let mut reset = store.begin();
		reset.reset_property(graph.id(), &record, key).assert_ok();
		reset.commit().assert_ok();
		store
			.import(
				ImportOptions {
					segment_size: 1,
					..ImportOptions::default()
				},
				|session| {
					session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(3)))?;
					session.apply_item(|tx| tx.set_base_property(&record, key, Value::from(2)))?;
					Ok(())
				},
			)
			.assert_ok();
		let root = store.inner.current_root();
		assert_eq!(
			root.change_history()
				.get(&(feed, root.commit_ts))
				.and_then(|group| group.gap.as_ref())
				.map(ChangefeedGap::reason),
			Some(ChangefeedGapReason::ImportSuppressed)
		);
		for changefeed in [ImportChangefeedMode::On, ImportChangefeedMode::Off] {
			store
				.import(
					ImportOptions {
						changefeed,
						..ImportOptions::default()
					},
					|session| {
						session.apply_item(|tx| {
							tx.set_base_property(&record, key, Value::from(9))?;
							tx.set_base_property(&record, key, Value::from(2))?;
							Ok(())
						})?;
						Ok(())
					},
				)
				.assert_ok();
			let root = store.inner.current_root();
			assert!(!root.change_history().contains_key(&(feed, root.commit_ts)));
		}
	}

	/// Verifies changefeed import handled failed setter does not leave
	/// suppression.
	#[test]
	fn changefeed_import_handled_failed_setter_does_not_leave_suppression() {
		use crate::{
			datastore::{ClassField, ClassIndex, ClassIndexKind, IndexScope},
			test_assertions::AssertResult as _,
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let schema = ClassSchema::new("item", AtomKind::Node, [ClassField::new(
			"value",
			crate::values::Type::Int,
		)])
		.assert_ok()
		.with_index(
			ClassIndex::new("unique_value", ClassIndexKind::Unique, IndexScope::Base, ["value"])
				.assert_ok(),
		)
		.assert_ok();
		let class = setup.define_class(schema).assert_ok();
		let key = setup.intern_property("value").assert_ok();
		let graph = setup.define_graph("scope", crate::graph::GraphKind::Graph).assert_ok();
		let first = "item:a".parse::<RecordId>().assert_ok();
		for (record, value) in [(first.clone(), 1), ("item:b".parse::<RecordId>().assert_ok(), 2)] {
			let mut properties = PropertyMap::new();
			properties.insert(key, value);
			setup.create_atom(record.clone(), properties).assert_ok();
			setup.attach_node(graph.id(), &record, None, None).assert_ok();
		}
		setup.commit().assert_ok();
		let mut configure = store.begin();
		let duration = "P1D"
			.parse()
			.map_err(|error: jiff::Error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok();
		configure
			.configure_changefeed(
				ChangefeedSource::Class(class),
				ChangefeedConfig::new(duration).assert_ok().with_full(true),
			)
			.assert_ok();
		configure
			.configure_changefeed(
				ChangefeedSource::Graph(graph.id()),
				ChangefeedConfig::new(duration).assert_ok().with_full(true),
			)
			.assert_ok();
		configure.commit().assert_ok();
		let before = store.inner.current_root();
		store
			.import(
				ImportOptions {
					indexes: crate::datastore::ImportIndexMode::Maintained,
					..ImportOptions::default()
				},
				|session| {
					session.apply_item(|tx| {
						assert!(tx.set_base_property(&first, key, Value::from(2)).is_err());
						Ok(())
					})?;
					Ok(())
				},
			)
			.assert_ok();
		let after = store.inner.current_root();
		assert_eq!(after.commit_ts, before.commit_ts);
		assert!(after.change_history().is_empty());
	}

	/// Verifies changefeed import full on off agree when created targets
	/// disappear in one item.
	#[test]
	fn changefeed_import_full_on_off_agree_when_created_targets_disappear_in_one_item() {
		use crate::test_assertions::AssertResult as _;

		let (store, _record, class_feed, key) = fixture().assert_ok();
		let mut setup = store.begin();
		let graph = setup.define_graph("scope", crate::graph::GraphKind::Graph).assert_ok();
		let duration = "P1D"
			.parse()
			.map_err(|error: jiff::Error| FerrosGrynnError::invalid_argument(error.to_string()))
			.assert_ok();
		let graph_feed = setup
			.configure_changefeed(
				ChangefeedSource::Graph(graph.id()),
				ChangefeedConfig::new(duration).assert_ok().with_full(true),
			)
			.assert_ok();
		setup.commit().assert_ok();
		let temporary = "item:temporary".parse::<RecordId>().assert_ok();
		for changefeed in [ImportChangefeedMode::On, ImportChangefeedMode::Off] {
			store
				.import(
					ImportOptions {
						changefeed,
						..ImportOptions::default()
					},
					|session| {
						session.apply_item(|tx| {
							tx.create_atom(temporary.clone(), PropertyMap::new())?;
							tx.attach_node(graph.id(), &temporary, None, None)?;
							tx.set_base_property(&temporary, key, Value::from(1))?;
							tx.delete_atom_cascade(&temporary, true)?;
							Ok(())
						})?;
						Ok(())
					},
				)
				.assert_ok();
			let root = store.inner.current_root();
			assert!(!root.change_history().contains_key(&(class_feed, root.commit_ts)));
			assert!(!root.change_history().contains_key(&(graph_feed, root.commit_ts)));
			assert!(root.resolve_atom(&temporary).is_none());
		}
	}
}
