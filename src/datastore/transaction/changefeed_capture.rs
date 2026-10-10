//! Final-policy capture over validated replay ranges with one allocation map.

use std::{collections::BTreeMap, sync::Arc};

use super::{
	Mutation,
	Transaction,
	changefeed_images::{CaptureBudget, image, same_image},
	commit::ReplayRemap,
};
use crate::{
	changefeed::{
		ChangeBatch,
		ChangeEntry,
		ChangeIdentity,
		ChangeImage,
		ChangefeedGap,
		ChangefeedGapReason,
		ChangefeedId,
		ChangefeedSource,
		ChangefeedState,
		Versionstamp,
	},
	datastore::{CapturedCommit, ChangefeedDefinition, ConflictKey, DatabaseRoot},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
	values::{DateTime, RecordId},
};

/// Per-source exact coalescing and optional operation history.
#[derive(Default)]
struct Accumulator {
	/// First before and latest after for each stable logical identity.
	net: BTreeMap<ChangeIdentity, ChangeEntry>,
	/// Ordered successful operations, including explicitly targeted no-ops.
	full: Vec<ChangeEntry>,
}

/// One source target in an external operation; addresses remain replay-private.
struct Target {
	/// Graph-local source, or None for the global record.
	graph: Option<GraphId>,
	/// Explicitly targeted identities retain FULL no-ops.
	explicit: bool,
}

impl Transaction {
	/// Checks whether an otherwise empty transaction publishes targeted FULL
	/// work.
	pub(super) fn has_full_capture(&self, root: &DatabaseRoot) -> bool {
		self.change_journal.intents.iter().any(|intent| {
			let source =
				intent.graph.map_or(ChangefeedSource::Class(intent.class), ChangefeedSource::Graph);
			root.changefeeds().for_source(source).is_some_and(|definition| {
				definition.state == ChangefeedState::Active && definition.config.full()
			})
		})
	}

	/// Validates dependencies of successful equality exits only for final FULL
	/// sources.
	pub(super) fn validate_full_noops(
		&self,
		current: &DatabaseRoot,
		next: &DatabaseRoot,
	) -> FerrosGrynnResult<()> {
		for operation in &self.change_journal.operations {
			for intent in &self.change_journal.intents[operation.intents.clone()] {
				if !intent.noop {
					continue;
				}
				let source = intent
					.graph
					.map_or(ChangefeedSource::Class(intent.class), ChangefeedSource::Graph);
				let definition = next.changefeeds().for_source(source);
				if !definition.is_some_and(|definition| {
					definition.state == ChangefeedState::Active && definition.config.full()
				}) {
					continue;
				}
				let mut keys = [
					Some(intent.observed_key),
					Some(ConflictKey::BaseAtom(intent.global)),
					None,
					None,
				];
				if let Some(graph) = intent.graph {
					keys[2] = Some(ConflictKey::GraphMembership(graph, intent.global));
					keys[3] = Some(ConflictKey::GraphOverlay(graph, intent.global));
				}
				for key in keys.into_iter().flatten() {
					let observed = if key == intent.observed_key {
						intent.observed_version
					} else {
						self.snapshot.version(key)
					};
					if current.version(key) != observed
						&& !self.is_private_allocation_collision(current, key)
					{
						return Err(FerrosGrynnError::serialization_failure(
							self.id,
							"a FULL no-op target changed before publication",
						));
					}
				}
			}
		}
		Ok(())
	}

	/// Captures validated logical operation boundaries using the final source
	/// descriptors.
	pub(super) fn capture_changes(
		&self,
		current: &DatabaseRoot,
		next: &mut DatabaseRoot,
		stamp: u64,
	) -> FerrosGrynnResult<()> {
		let (groups, bytes) = self.capture_groups(current, next, stamp, 0, 0)?;
		if groups.is_empty() {
			return Ok(());
		}
		let mut budget = CaptureBudget {
			bytes,
			maximum: self
				.store
				.inner
				.transaction_limits()
				.max_write_set_bytes
				.min(256 * 1024 * 1024),
		};
		for (feed, group) in groups {
			admit_history_publication(next, feed, &mut budget)?;
			install_capture(next, feed, stamp, group);
		}
		Ok(())
	}

	/// Materializes one bounded replay slice without exposing its groups in a
	/// root.
	#[allow(
		clippy::mutable_key_type,
		reason = "immutable RecordId and occurrence identities retain stable canonical ordering \
		          despite interned Text storage"
	)]
	pub(super) fn capture_groups(
		&self,
		current: &DatabaseRoot,
		next: &DatabaseRoot,
		stamp: u64,
		start: usize,
		retained: usize,
	) -> FerrosGrynnResult<(BTreeMap<ChangefeedId, CapturedCommit>, usize)> {
		if self.change_journal.operations.is_empty() || next.changefeeds().definitions.is_empty() {
			return Ok((BTreeMap::new(), retained));
		}
		let maximum =
			self.store.inner.transaction_limits().max_write_set_bytes.min(256 * 1024 * 1024);
		let mut budget = CaptureBudget {
			bytes: retained,
			maximum,
		};
		budget.admit(self.admitted_journal_bytes())?;
		budget.admit(current.graphs.len().checked_mul(128).ok_or_else(|| {
			FerrosGrynnError::operation_overflow("changefeed replay root metadata")
		})?)?;
		let mut accumulators: BTreeMap<ChangefeedId, Accumulator> = BTreeMap::new();
		let mut suppressed: BTreeMap<ChangefeedId, Arc<ChangefeedDefinition>> = BTreeMap::new();
		let mut replay = current.clone();
		if self.import_constraints.is_none()
			&& !self.mutations.is_empty()
			&& replay.indexes.has_deferred_indexes()
		{
			replay.rebuild_indexes()?;
		}
		let mut remap = ReplayRemap::default();
		let mut position = start;
		self.store
			.inner
			.changefeed_observation
			.operations(self.change_journal.operations.len() as u64);
		for operation in &self.change_journal.operations {
			Self::replay_mutations(
				&mut replay,
				&self.mutations[position..operation.mutations.start],
				stamp,
				true,
				&mut remap,
			)?;
			let mut targets: BTreeMap<(ChangefeedId, RecordId), Target> = BTreeMap::new();
			for intent in &self.change_journal.intents[operation.intents.clone()] {
				let created =
					self.mutations[operation.mutations.clone()].iter().find_map(|mutation| {
						match mutation {
							Mutation::CreateAtom {
								id,
								record_id,
								..
							} if *id == intent.global => Some(record_id),
							_ => None,
						}
					});
				let record = created.or_else(|| {
					replay.atom(remap.global(intent.global)).map(|atom| atom.record_id())
				});
				let source = intent
					.graph
					.map_or(ChangefeedSource::Class(intent.class), ChangefeedSource::Graph);
				Self::add_capture_target(
					next,
					current,
					source,
					record,
					intent.graph,
					true,
					&mut targets,
					&mut suppressed,
					&mut budget,
				)?;
				if intent.graph.is_none() && created.is_none() {
					for graph in replay
						.memberships(remap.global(intent.global))
						.into_iter()
						.flat_map(|memberships| memberships.keys())
					{
						Self::add_capture_target(
							next,
							current,
							ChangefeedSource::Graph(*graph),
							record,
							Some(*graph),
							false,
							&mut targets,
							&mut suppressed,
							&mut budget,
						)?;
					}
				}
			}
			let mut before = BTreeMap::new();
			for (key, target) in &targets {
				budget.admit(128usize.saturating_add(
					crate::values::runtime::estimated_record_id_heap_bytes(&key.1),
				))?;
				let definition = next.changefeeds().definitions.get(&key.0).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("changefeed final descriptor disappeared")
				})?;
				before.insert(
					key.clone(),
					target_image(&replay, &key.1, target.graph, definition.source, &mut budget)?,
				);
			}
			Self::replay_mutations(
				&mut replay,
				&self.mutations[operation.mutations.clone()],
				stamp,
				true,
				&mut remap,
			)?;
			for (key, target) in targets {
				let feed = key.0;
				let before = before.remove(&key).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("changefeed target lost its before image")
				})?;
				let definition = next.changefeeds().definitions.get(&feed).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("changefeed final descriptor disappeared")
				})?;
				let after =
					target_image(&replay, &key.1, target.graph, definition.source, &mut budget)?;
				let changed = !same_image(before.as_deref(), after.as_deref());
				if !changed && !target.explicit {
					continue;
				}
				let Some(identity) =
					after.as_ref().or(before.as_ref()).map(|image| image.identity())
				else {
					continue;
				};
				budget.admit(512usize.saturating_add(
					crate::values::runtime::estimated_record_id_heap_bytes(&key.1),
				))?;
				let identity = identity.clone();
				let accumulator = accumulators.entry(feed).or_default();
				if let Some(entry) = accumulator.net.get_mut(&identity) {
					entry.after = after.clone();
				} else {
					accumulator.net.insert(identity, ChangeEntry {
						ordinal: 0,
						operation: operation.ordinal,
						before: before.clone(),
						after: after.clone(),
					});
				}
				let definition = next.changefeeds().definitions.get(&feed).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("changefeed final descriptor disappeared")
				})?;
				if definition.config.full() {
					budget.admit(std::mem::size_of::<ChangeEntry>())?;
					let ordinal = u64::try_from(accumulator.full.len()).map_err(|_error| {
						FerrosGrynnError::operation_overflow("changefeed entry ordinal")
					})?;
					budget.reserve(&mut accumulator.full)?;
					accumulator.full.push(ChangeEntry {
						ordinal,
						operation: operation.ordinal,
						before,
						after,
					});
				}
			}
			position = operation.mutations.end;
		}
		let timestamp =
			jiff::Timestamp::from_nanosecond(next.committed_at_ns).map_err(|error| {
				FerrosGrynnError::invalid_argument("changefeed commit time is invalid")
					.with_technical_source(error)
			})?;
		let committed_at = DateTime::new(timestamp.to_zoned(jiff::tz::TimeZone::UTC));
		let mut groups = BTreeMap::new();
		for (feed, accumulator) in accumulators {
			let definition = next.changefeeds().definitions.get(&feed).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("changefeed final descriptor disappeared")
			})?;
			let mut entries = Vec::new();
			for mut entry in accumulator.net.into_values() {
				if same_image(entry.before.as_deref(), entry.after.as_deref()) {
					continue;
				}
				budget.admit(std::mem::size_of::<ChangeEntry>())?;
				entry.ordinal = u64::try_from(entries.len()).map_err(|_error| {
					FerrosGrynnError::operation_overflow("changefeed net ordinal")
				})?;
				budget.reserve(&mut entries)?;
				entries.push(entry);
			}
			if entries.is_empty() && accumulator.full.is_empty() {
				continue;
			}
			let batch = ChangeBatch {
				source: feed,
				versionstamp: stamp.into(),
				committed_at: committed_at.clone(),
				entries,
			};
			let full = definition.config.full().then(|| ChangeBatch {
				source: feed,
				versionstamp: stamp.into(),
				committed_at: committed_at.clone(),
				entries: accumulator.full,
			});
			let group = CapturedCommit {
				revision: definition.version,
				net: batch,
				full,
				include_original: definition.config.include_original(),
				gap: None,
			};
			budget.admit(256)?;
			groups.insert(feed, group);
		}
		for (feed, definition) in suppressed {
			let batch = ChangeBatch {
				source: feed,
				versionstamp: stamp.into(),
				committed_at: committed_at.clone(),
				entries: Vec::new(),
			};
			budget.admit(256)?;
			groups.insert(feed, CapturedCommit {
				revision: definition.version,
				net: batch,
				full: None,
				include_original: definition.config.include_original(),
				gap: Some(ChangefeedGap {
					source: feed,
					reason: ChangefeedGapReason::SourceDropped,
					from: Some(Versionstamp::new(stamp)),
					through: Some(Versionstamp::new(stamp)),
				}),
			});
		}
		self.store.inner.changefeed_observation.scratch(budget.bytes as u64);
		Ok((groups, budget.bytes))
	}

	/// Admits a source target without extracting payloads for disabled capture.
	#[allow(
		clippy::too_many_arguments,
		reason = "one bounded capture admission shares target and policy context"
	)]
	#[allow(
		clippy::mutable_key_type,
		reason = "immutable RecordId keys never change their canonical identity"
	)]
	fn add_capture_target(
		next: &DatabaseRoot,
		current: &DatabaseRoot,
		source: ChangefeedSource,
		record: Option<&RecordId>,
		graph: Option<GraphId>,
		explicit: bool,
		targets: &mut BTreeMap<(ChangefeedId, RecordId), Target>,
		suppressed: &mut BTreeMap<ChangefeedId, Arc<ChangefeedDefinition>>,
		budget: &mut CaptureBudget,
	) -> FerrosGrynnResult<()> {
		let Some(definition) = next.changefeeds().for_source(source) else {
			return Ok(());
		};
		if definition.state != ChangefeedState::Active {
			if current
				.changefeeds()
				.for_source(source)
				.is_some_and(|previous| previous.state == ChangefeedState::Active)
			{
				budget.admit(128)?;
				suppressed.insert(definition.id, definition.clone());
			}
			return Ok(());
		}
		let Some(record) = record else {
			return Ok(());
		};
		budget.admit(
			256usize.saturating_add(crate::values::runtime::estimated_record_id_heap_bytes(record)),
		)?;
		let key = (definition.id, record.clone());
		if let Some(target) = targets.get_mut(&key) {
			target.explicit |= explicit;
			return Ok(());
		}
		targets.insert(key, Target {
			graph,
			explicit,
		});
		Ok(())
	}
}

/// Resolves stable records rather than private slots, also across
/// delete/recreate.
fn target_image(
	root: &DatabaseRoot,
	record: &RecordId,
	graph: Option<GraphId>,
	source: ChangefeedSource,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<Option<Arc<ChangeImage>>> {
	let Some(global) = root.resolve_atom(record) else {
		return Ok(None);
	};
	if let ChangefeedSource::Class(class) = source
		&& !root.atom(global).is_some_and(|atom| atom.class_id() == class)
	{
		return Ok(None);
	}
	image(root, global, graph, budget)
}

/// Installs one already bounded immutable history group and updates catalog
/// authority.
pub(super) fn install_capture(
	root: &mut DatabaseRoot,
	feed: ChangefeedId,
	stamp: u64,
	group: CapturedCommit,
) {
	Arc::make_mut(root.change_history_mut()).insert((feed, stamp), Arc::new(group));
	if let Some(definition) = Arc::make_mut(root.changefeeds_mut()).definitions.get_mut(&feed) {
		Arc::make_mut(definition).last_stamp = stamp;
	}
	root.mark_version(ConflictKey::ChangefeedCatalogRange, stamp);
}

/// Charges immutable metadata COW before copying retained history/catalog maps.
pub(super) fn admit_history_publication(
	root: &DatabaseRoot,
	feed: ChangefeedId,
	budget: &mut CaptureBudget,
) -> FerrosGrynnResult<()> {
	let bytes = root
		.change_history()
		.len()
		.checked_mul(128)
		.and_then(|bytes| {
			root.changefeeds()
				.definitions
				.len()
				.checked_mul(128)
				.and_then(|catalog| bytes.checked_add(catalog))
		})
		.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed retained metadata"))?;
	budget.admit(bytes)?;
	if let Some(definition) = root.changefeeds().definitions.get(&feed) {
		budget.admit(
			std::mem::size_of::<ChangefeedDefinition>()
				.checked_add(definition.name.len())
				.and_then(|bytes| {
					definition
						.epochs
						.len()
						.checked_mul(
							std::mem::size_of::<crate::changefeed::ChangefeedCaptureEpoch>(),
						)
						.and_then(|epochs| bytes.checked_add(epochs))
				})
				.ok_or_else(|| FerrosGrynnError::operation_overflow("changefeed descriptor COW"))?,
		)?;
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		changefeed::ChangefeedConfig,
		datastore::{
			ClassSchema,
			Datastore,
			IsolationLevel,
			TransactionAccess,
			TransactionOptions,
		},
		graph::{AtomKind, GraphKind},
		values::{Number, PropertyKeyId, PropertyMap, Value},
	};

	/// Creates an unconfigured record and an inherited graph occurrence.
	fn fixture()
	-> FerrosGrynnResult<(Datastore, RecordId, crate::datastore::ClassId, GraphId, PropertyKeyId)>
	{
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let graph = tx.define_graph("scope", GraphKind::Graph)?;
		let key = tx.intern_property("value")?;
		let record = "item:a".parse::<RecordId>()?;
		let mut properties = PropertyMap::new();
		properties.insert(key, 1);
		tx.create_atom(record.clone(), properties)?;
		tx.attach_node(graph.id(), &record, None, None)?;
		tx.commit()?;
		Ok((store, record, class, graph.id(), key))
	}

	/// Validates a fixed FULL source policy.
	fn full_config() -> FerrosGrynnResult<ChangefeedConfig> {
		Ok(ChangefeedConfig::new("P1D".parse().map_err(|error: jiff::Error| {
			FerrosGrynnError::invalid_argument(error.to_string())
		})?)?
		.with_full(true))
	}

	#[test]
	fn changefeed_final_full_policy_captures_earlier_dml_and_exact_noops() -> FerrosGrynnResult<()>
	{
		let (store, record, class, _graph, key) = fixture()?;
		let mut tx = store.begin();
		tx.set_base_property(&record, key, Value::from(2))?;
		let feed = tx.configure_changefeed(ChangefeedSource::Class(class), full_config()?)?;
		tx.set_base_property(&record, key, Value::from(3))?;
		tx.remove_base_property(&record, PropertyKeyId::new(99))?;
		let stamp = tx.commit()?.commit_ts();
		let root = store.inner.current_root();
		let group = root
			.change_history()
			.get(&(feed, stamp))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture history is absent"))?;
		assert_eq!(group.net.entries().len(), 1);
		let entry = &group.net.entries()[0];
		assert_eq!(
			entry.before().and_then(|image| image.properties().get("value")),
			Some(&Value::from(1))
		);
		assert_eq!(
			entry.after().and_then(|image| image.properties().get("value")),
			Some(&Value::from(3))
		);
		let full = group
			.full
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture FULL history is absent"))?;
		assert_eq!(full.entries().len(), 3);
		for (ordinal, entry) in full.entries().iter().enumerate() {
			assert_eq!(entry.ordinal(), ordinal as u64);
			assert_eq!(entry.operation(), ordinal as u64);
		}
		assert!(same_image(full.entries()[2].before(), full.entries()[2].after()));
		assert_eq!(
			group.net.committed_at().as_inner().timestamp().as_nanosecond(),
			root.committed_at_ns
		);
		let mut restore = store.begin();
		restore.set_base_property(&record, key, Value::from(4))?;
		restore.set_base_property(&record, key, Value::from(3))?;
		let restored_stamp = restore.commit()?.commit_ts();
		let latest = store.inner.current_root();
		let restored = latest.change_history().get(&(feed, restored_stamp)).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("fixture restore history is absent")
		})?;
		assert!(restored.net.entries().is_empty());
		assert_eq!(restored.full.as_ref().map(|full| full.entries().len()), Some(2));
		Ok(())
	}

	#[test]
	fn changefeed_noop_only_full_commits_validate_and_advance_clock() -> FerrosGrynnResult<()> {
		let (store, record, class, graph, key) = fixture()?;
		let source = ChangefeedSource::Graph(graph);
		let mut configure = store.begin();
		let feed = configure.configure_changefeed(source, full_config()?)?;
		configure.commit()?;
		let before = store.inner.current_root().commit_ts;
		let mut tx = store.begin();
		tx.reset_property(graph, &record, key)?;
		let stamp = tx.commit()?.commit_ts();
		assert_eq!(stamp, before + 1);
		let root = store.inner.current_root();
		let group = root
			.change_history()
			.get(&(feed, stamp))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture no-op history is absent"))?;
		assert!(group.net.entries().is_empty());
		assert_eq!(group.full.as_ref().map(|full| full.entries().len()), Some(1));
		let mut noop = store.begin_with(TransactionOptions::new(
			IsolationLevel::Snapshot,
			TransactionAccess::ReadWrite,
		));
		noop.reset_property(graph, &record, key)?;
		let mut writer = store.begin();
		writer.set_base_property(&record, key, Value::from(9))?;
		writer.commit()?;
		assert!(noop.commit().is_err());
		let mut class_noop = store.begin();
		class_noop.remove_base_property(&record, PropertyKeyId::new(99))?;
		let before = store.inner.current_root().commit_ts;
		assert_eq!(class_noop.commit()?.commit_ts(), before);
		let mut enable = store.begin();
		enable.configure_changefeed(ChangefeedSource::Class(class), full_config()?)?;
		enable.commit()?;
		Ok(())
	}

	#[test]
	fn changefeed_net_uses_storage_numeric_tags_and_suppresses_masked_fanout()
	-> FerrosGrynnResult<()> {
		let (store, record, class, graph, key) = fixture()?;
		let mut configure = store.begin();
		let class_feed =
			configure.configure_changefeed(ChangefeedSource::Class(class), full_config()?)?;
		let graph_feed =
			configure.configure_changefeed(ChangefeedSource::Graph(graph), full_config()?)?;
		configure.hide_property(graph, &record, key)?;
		configure.commit()?;
		let mut tx = store.begin();
		tx.set_base_property(&record, key, Value::Number(Number::Float(1.0)))?;
		let stamp = tx.commit()?.commit_ts();
		let root = store.inner.current_root();
		assert_eq!(
			root.change_history().get(&(class_feed, stamp)).map(|group| group.net.entries().len()),
			Some(1)
		);
		assert!(!root.change_history().contains_key(&(graph_feed, stamp)));
		let mut overlay = store.begin();
		overlay.set_overlay(graph, &record, key, Value::from(7))?;
		let stamp = overlay.commit()?.commit_ts();
		let root = store.inner.current_root();
		let group = root.change_history().get(&(graph_feed, stamp)).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("fixture overlay history is absent")
		})?;
		let after = group.net.entries()[0]
			.after()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture overlay image is absent"))?;
		assert_eq!(after.properties().get("value"), Some(&Value::from(7)));
		assert_eq!(
			after.overlays().get("value"),
			Some(&crate::changefeed::ChangeOverlay::Value(Value::from(7)))
		);
		Ok(())
	}

	#[test]
	fn changefeed_drop_and_recreate_identity_do_not_capture_foreign_slots() -> FerrosGrynnResult<()>
	{
		let (store, _record, class, _graph, key) = fixture()?;
		let mut configure = store.begin();
		let feed =
			configure.configure_changefeed(ChangefeedSource::Class(class), full_config()?)?;
		configure.commit()?;
		let left_record = "item:left".parse::<RecordId>()?;
		let right_record = "item:right".parse::<RecordId>()?;
		let mut left = store.begin();
		let mut right = store.begin();
		left.create_atom(left_record.clone(), PropertyMap::new())?;
		right.create_atom(right_record, PropertyMap::new())?;
		right.commit()?;
		let stamp = left.commit()?.commit_ts();
		let root = store.inner.current_root();
		let group = root.change_history().get(&(feed, stamp)).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("fixture rebased history is absent")
		})?;
		assert!(group.net.entries()[0].before().is_none());
		let mut recreate = store.begin();
		recreate.delete_atom(&left_record)?;
		let mut properties = PropertyMap::new();
		properties.insert(key, 8);
		recreate.create_atom(left_record.clone(), properties)?;
		let stamp = recreate.commit()?.commit_ts();
		let root = store.inner.current_root();
		let group = root.change_history().get(&(feed, stamp)).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("fixture recreated history is absent")
		})?;
		assert_eq!(group.net.entries().len(), 1);
		assert!(group.net.entries()[0].before().is_some());
		assert!(group.net.entries()[0].after().is_some());
		let mut drop = store.begin();
		drop.set_base_property(&left_record, key, Value::from(9))?;
		drop.drop_changefeed(ChangefeedSource::Class(class))?;
		let stamp = drop.commit()?.commit_ts();
		let root = store.inner.current_root();
		let group = root
			.change_history()
			.get(&(feed, stamp))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture DROP gap is absent"))?;
		assert_eq!(
			group.gap.as_ref().map(ChangefeedGap::reason),
			Some(ChangefeedGapReason::SourceDropped)
		);
		assert!(group.net.entries().is_empty());
		Ok(())
	}

	#[test]
	fn changefeed_mixed_update_validates_each_full_noop_target() -> FerrosGrynnResult<()> {
		let (store, _record, class, _graph, key) = fixture()?;
		let second = "item:b".parse::<RecordId>()?;
		let mut setup = store.begin();
		let mut properties = PropertyMap::new();
		properties.insert(key, 2);
		setup.create_atom(second.clone(), properties)?;
		setup.configure_changefeed(ChangefeedSource::Class(class), full_config()?)?;
		setup.commit()?;
		let mut mixed = store.begin_with(TransactionOptions::new(
			IsolationLevel::Snapshot,
			TransactionAccess::ReadWrite,
		));
		for outcome in
			mixed.execute("UPDATE item SET value = 2 RETURN NONE", &crate::values::Object::new())?
		{
			let _result = outcome.result?;
		}
		assert!(mixed.change_journal.intents.iter().any(|intent| intent.noop));
		assert!(
			mixed.change_journal.operations.iter().any(|operation| !operation.mutations.is_empty())
		);
		let mut writer = store.begin();
		writer.set_base_property(&second, key, Value::from(3))?;
		writer.commit()?;
		assert!(mixed.commit().is_err());
		Ok(())
	}

	#[test]
	fn changefeed_unrelated_deleted_noop_and_final_retirement_do_not_reject_writes()
	-> FerrosGrynnResult<()> {
		let (store, record, _class, graph, key) = fixture()?;
		let mut setup = store.begin();
		let other = setup.define_class(ClassSchema::new("other", AtomKind::Node, [])?)?;
		let second = "other:b".parse::<RecordId>()?;
		setup.create_atom(second.clone(), PropertyMap::new())?;
		setup.configure_changefeed(ChangefeedSource::Class(other), full_config()?)?;
		setup.commit()?;
		let mut tx = store.begin_with(TransactionOptions::new(
			IsolationLevel::Snapshot,
			TransactionAccess::ReadWrite,
		));
		tx.remove_base_property(&record, PropertyKeyId::new(99))?;
		tx.set_base_property(&second, key, Value::from(3))?;
		let mut writer = store.begin();
		writer.delete_atom_cascade(&record, true)?;
		writer.commit()?;
		tx.commit()?;
		let mut restore = store.begin();
		restore.create_atom(record.clone(), PropertyMap::new())?;
		restore.attach_node(graph, &record, None, None)?;
		let feed = restore.configure_changefeed(ChangefeedSource::Graph(graph), full_config()?)?;
		restore.commit()?;
		let mut retired = store.begin_with(TransactionOptions::new(
			IsolationLevel::Snapshot,
			TransactionAccess::ReadWrite,
		));
		retired.reset_property(graph, &record, key)?;
		retired.remove_graph("scope")?;
		let mut writer = store.begin();
		writer.set_base_property(&record, key, Value::from(4))?;
		writer.commit()?;
		let stamp = retired.commit()?.commit_ts();
		assert_eq!(
			store
				.inner
				.current_root()
				.change_history()
				.get(&(feed, stamp))
				.and_then(|group| group.gap.as_ref())
				.map(ChangefeedGap::reason),
			Some(ChangefeedGapReason::SourceDropped)
		);
		Ok(())
	}

	#[test]
	fn changefeed_publication_admits_retained_epoch_cow_before_copy() -> FerrosGrynnResult<()> {
		let (store, _record, class, _graph, _key) = fixture()?;
		let mut setup = store.begin();
		let feed = setup.configure_changefeed(ChangefeedSource::Class(class), full_config()?)?;
		setup.commit()?;
		let mut root = (*store.inner.current_root()).clone();
		let definition = Arc::make_mut(
			Arc::make_mut(root.changefeeds_mut())
				.definitions
				.get_mut(&feed)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture feed is absent"))?,
		);
		for stamp in 10..110 {
			definition.set_capture_epoch(stamp, Some(full_config()?))?;
		}
		let old = root.clone();
		let mut budget = CaptureBudget {
			bytes: 0,
			maximum: 1024,
		};
		assert!(admit_history_publication(&root, feed, &mut budget).is_err());
		assert!(Arc::ptr_eq(old.changefeeds(), root.changefeeds()));
		Ok(())
	}

	#[test]
	fn changefeed_mixed_writeback_validates_unchanged_full_targets() -> FerrosGrynnResult<()> {
		let (store, record, _class, graph, _key) = fixture()?;
		let second = "item:b".parse::<RecordId>()?;
		let mut setup = store.begin();
		setup.create_atom(second.clone(), PropertyMap::new())?;
		setup.attach_node(graph, &second, None, None)?;
		setup.configure_changefeed(ChangefeedSource::Graph(graph), full_config()?)?;
		setup.commit()?;
		let mut tx = store.begin_with(TransactionOptions::new(
			IsolationLevel::Snapshot,
			TransactionAccess::ReadWrite,
		));
		let reference = tx
			.working
			.schema
			.graph(graph)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture graph is absent"))?
			.reference()
			.clone();
		let materialized = crate::graph::query_to_memory::<
			crate::graph::Graph<crate::graph::Frozen>,
			_,
		>(&reference.read(&tx)?)?;
		let first_local = crate::graph::GraphRead::atom_id(
			&materialized,
			&crate::graph::AtomRef::new(AtomKind::Node, record.clone()),
		)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture first node is absent"))?;
		let second_local = crate::graph::GraphRead::atom_id(
			&materialized,
			&crate::graph::AtomRef::new(AtomKind::Node, second.clone()),
		)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture second node is absent"))?;
		let mut materialized = materialized.thaw();
		materialized.set_node_label(first_local, Some("changed".into()))?;
		materialized.set_node_label(second_local, None)?;
		materialized.flush(&mut tx)?;
		let mut writer = store.begin();
		writer.set_occurrence_label(graph, &second, Some("concurrent".into()))?;
		writer.commit()?;
		assert!(tx.commit().is_err());
		Ok(())
	}
}
