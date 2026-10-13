//! Compact logical operation ranges; capture images are built during replay.

use std::{mem::size_of, ops::Range};

use super::Transaction;
use crate::{
	datastore::{ClassId, ConflictKey, GlobalAtomId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::GraphId,
};

/// One stable logical target; local addresses and properties are not copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CaptureIntent {
	/// Global atom address translated by the commit-local replay map.
	pub(super) global: GlobalAtomId,
	/// Source class, including targets removed before commit.
	pub(super) class: ClassId,
	/// Graph-local source, or None for database-global DML.
	pub(super) graph: Option<GraphId>,
	/// Logical value observed by a successful targeted call.
	pub(super) observed_key: ConflictKey,
	/// Snapshot version checked only for a publishable FULL no-op.
	pub(super) observed_version: u64,
	/// A successful external target produced no mutations, even in a mixed
	/// statement.
	pub(super) noop: bool,
}

/// One completed external operation, including its internal cascade mutations.
#[derive(Debug, Clone)]
pub(super) struct CaptureOperation {
	/// Checked operation ordinal within the transaction/import.
	pub(super) ordinal: u64,
	/// Consecutive physical-independent mutation range.
	pub(super) mutations: Range<usize>,
	/// Consecutive explicitly targeted logical identities.
	pub(super) intents: Range<usize>,
}

/// Lightweight rollback position, without a root or image clone.
#[derive(Debug, Clone, Copy)]
pub(super) struct JournalCheckpoint {
	/// Completed operation count.
	operations: usize,
	/// Target count.
	intents: usize,
	/// Logical bytes reserved by retained metadata.
	bytes: usize,
	/// Next successful operation ordinal.
	next_ordinal: u64,
	/// Outermost operation's mutation and intent starts.
	active: Option<(usize, usize)>,
	/// Deduplication can mark the last already retained intent inside a
	/// savepoint.
	last_intent: Option<CaptureIntent>,
}

/// Transaction-owned flat journal, bounded before buffer growth.
#[derive(Debug, Default)]
pub(super) struct ChangeJournal {
	/// Completed operations in execution order.
	pub(super) operations: Vec<CaptureOperation>,
	/// Targets observed before equality fast paths.
	pub(super) intents: Vec<CaptureIntent>,
	/// Logical metadata reservation.
	pub(super) bytes: usize,
	/// Next checked successful-operation ordinal.
	next_ordinal: u64,
	/// Current outer operation's mutation and intent starts.
	active: Option<(usize, usize)>,
}

impl ChangeJournal {
	/// Counts reserved slots retained by both flat journal buffers.
	pub(super) const fn allocation_bytes(&self) -> usize {
		self.operations
			.capacity()
			.saturating_mul(size_of::<CaptureOperation>())
			.saturating_add(self.intents.capacity().saturating_mul(size_of::<CaptureIntent>()))
	}

	/// Captures metadata positions for an existing transaction savepoint.
	pub(super) fn checkpoint(&self) -> JournalCheckpoint {
		JournalCheckpoint {
			operations: self.operations.len(),
			intents: self.intents.len(),
			bytes: self.bytes,
			next_ordinal: self.next_ordinal,
			active: self.active,
			last_intent: self.intents.last().copied(),
		}
	}

	/// Discards unsuccessful operations, targets and ordinal reservations.
	pub(super) fn restore(&mut self, checkpoint: JournalCheckpoint) {
		self.operations.truncate(checkpoint.operations);
		self.intents.truncate(checkpoint.intents);
		if let (Some(last), Some(saved)) = (self.intents.last_mut(), checkpoint.last_intent) {
			*last = saved;
		}
		self.bytes = checkpoint.bytes;
		self.next_ordinal = checkpoint.next_ordinal;
		self.active = checkpoint.active;
	}

	/// Releases a sealed segment's ranges without resetting import ordinals.
	pub(super) fn clear_segment(&mut self) {
		self.operations.clear();
		self.intents.clear();
		self.bytes = 0;
		self.active = None;
	}
}

impl Transaction {
	/// Opens one outer operation after reserving its fixed metadata.
	pub(super) fn begin_change_operation(&mut self) -> bool {
		if self.change_journal.active.is_some() {
			return false;
		}
		self.change_journal.active =
			Some((self.mutations.len(), self.change_journal.intents.len()));
		true
	}

	/// Completes a boundary; empty target ranges release the fixed reservation.
	pub(super) fn finish_change_operation(&mut self, owned: bool) {
		if !owned {
			return;
		}
		if let Some((mutations, intents)) = self.change_journal.active.take() {
			if self.change_journal.intents.len() == intents {
				return;
			}
			if mutations == self.mutations.len() {
				self.mark_change_noops(intents);
			}
			self.change_journal.operations.push(CaptureOperation {
				ordinal: self.change_journal.next_ordinal,
				mutations: mutations..self.mutations.len(),
				intents: intents..self.change_journal.intents.len(),
			});
			self.change_journal.next_ordinal += 1;
		}
	}

	/// Records an observed target before mutation or an equality early return.
	pub(super) fn note_change_target(
		&mut self,
		global: GlobalAtomId,
		graph: Option<GraphId>,
		observed_key: ConflictKey,
	) -> FerrosGrynnResult<()> {
		let (class, _kind) = self.atom_domain(global)?;
		let intent = CaptureIntent {
			global,
			class,
			graph,
			observed_key,
			observed_version: self.snapshot.version(observed_key),
			noop: false,
		};
		if self.change_journal.intents.last().is_some_and(|last| {
			last.global == intent.global
				&& last.graph == intent.graph
				&& last.observed_key == intent.observed_key
		}) && self
			.change_journal
			.active
			.is_some_and(|(_, start)| start < self.change_journal.intents.len())
		{
			return Ok(());
		}
		self.admit_change_target(0)?;
		self.change_journal.intents.push(intent);
		Ok(())
	}

	/// Admits target metadata together with the pending primary write before
	/// mutation.
	pub(super) fn admit_change_target(&mut self, primary_bytes: usize) -> FerrosGrynnResult<()> {
		if self.change_journal.active.is_none() {
			return Err(FerrosGrynnError::invalid_argument(
				"logical change target has no operation boundary",
			));
		}
		let first = self
			.change_journal
			.active
			.is_some_and(|(_, start)| start == self.change_journal.intents.len());
		let metadata = size_of::<CaptureIntent>()
			+ if first {
				size_of::<CaptureOperation>()
			} else {
				0
			};
		self.ensure_capacity(
			primary_bytes.checked_add(metadata).ok_or_else(|| self.limit_error(usize::MAX))?,
		)?;
		let maximum = if self.import_constraints.is_some()
			&& self.import_changefeed.mode == super::super::ImportChangefeedMode::On
		{
			self.store
				.inner
				.transaction_limits()
				.max_write_set_bytes
				.min(256 * 1024 * 1024)
				.saturating_sub(self.import_changefeed.retained_bytes())
				.saturating_sub(self.write_bytes)
				.saturating_sub(primary_bytes)
		} else {
			256 * 1024 * 1024
		};
		if self
			.change_journal
			.active
			.is_some_and(|(_, start)| start == self.change_journal.intents.len())
		{
			self.change_journal.next_ordinal.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("changefeed operation ordinal is exhausted")
			})?;
			reserve_journal(
				&mut self.change_journal.operations,
				self.change_journal.intents.capacity().saturating_mul(size_of::<CaptureIntent>()),
				maximum,
			)?;
			self.change_journal.bytes += size_of::<CaptureOperation>();
		}
		reserve_journal(
			&mut self.change_journal.intents,
			self.change_journal.operations.capacity().saturating_mul(size_of::<CaptureOperation>()),
			maximum,
		)?;
		self.change_journal.bytes += size_of::<CaptureIntent>();
		Ok(())
	}

	/// Records an already admitted new atom without copying its properties.
	pub(super) fn record_created_change_target(&mut self, global: GlobalAtomId, class: ClassId) {
		let observed_key = ConflictKey::BaseAtom(global);
		self.change_journal.intents.push(CaptureIntent {
			global,
			class,
			graph: None,
			observed_key,
			observed_version: self.snapshot.version(observed_key),
			noop: false,
		});
	}

	/// Adapts a successful GQL candidate before its equality fast path.
	pub(crate) fn note_dml_change_target(
		&mut self,
		record: &crate::values::RecordId,
		graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		let global = self.require_atom(record)?;
		let key = match graph {
			Some(graph) => ConflictKey::GraphOverlay(graph, global),
			None => ConflictKey::BaseAtom(global),
		};
		self.note_change_target(global, graph, key)
	}

	/// Marks only the successful external candidate's intents after equality
	/// exits.
	pub(crate) fn mark_change_noops(&mut self, start: usize) {
		for intent in &mut self.change_journal.intents[start..] {
			intent.noop = true;
		}
	}

	/// Returns a candidate boundary without exposing the private journal.
	pub(crate) fn change_intent_count(&self) -> usize {
		self.change_journal.intents.len()
	}
}

/// Grows amortized buffers only after a checked aggregate scratch admission.
fn reserve_journal<T>(
	buffer: &mut Vec<T>,
	other_bytes: usize,
	maximum: usize,
) -> FerrosGrynnResult<()> {
	if buffer.len() < buffer.capacity() {
		return Ok(());
	}
	let capacity = buffer.capacity().max(4).checked_mul(2).ok_or_else(|| {
		FerrosGrynnError::invalid_argument("changefeed journal capacity overflows")
	})?;
	let bytes = capacity
		.checked_mul(size_of::<T>())
		.and_then(|bytes| bytes.checked_add(other_bytes))
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("changefeed journal scratch overflows")
		})?;
	if bytes > maximum {
		return Err(FerrosGrynnError::storage_decode_limit_exceeded(
			"changefeed journal scratch",
			bytes as u64,
			maximum as u64,
		));
	}
	buffer.try_reserve_exact(capacity - buffer.len()).map_err(|error| {
		FerrosGrynnError::storage_decode_limit_exceeded(
			"changefeed journal allocation",
			bytes as u64,
			maximum as u64,
		)
		.with_technical_source(error)
	})?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, InMemoryOptions},
		graph::{AtomKind, GraphKind},
		values::{Object, PropertyMap, RecordId, Value},
	};

	/// Creates an inherited occurrence with a named base field through the SDK.
	fn setup(
		options: InMemoryOptions,
	) -> FerrosGrynnResult<(Datastore, RecordId, GraphId, crate::values::PropertyKeyId)> {
		let store = Datastore::in_memory_with(options);
		let mut tx = store.begin();
		tx.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		let graph = tx.define_graph("scope", GraphKind::Graph)?;
		let key = tx.intern_property("value")?;
		let record = "item:a".parse::<RecordId>()?;
		let mut properties = PropertyMap::new();
		properties.insert(key, 1);
		tx.create_atom(record.clone(), properties)?;
		tx.attach_node(graph.id(), &record, None, None)?;
		tx.commit()?;
		Ok((store, record, graph.id(), key))
	}

	#[test]
	fn changefeed_create_admits_metadata_before_private_allocation() -> FerrosGrynnResult<()> {
		let (store, _record, _graph, _key) = setup(InMemoryOptions::default())?;
		let root = (*store.inner.current_root()).clone();
		let metadata = size_of::<CaptureOperation>() + size_of::<CaptureIntent>();
		let mut options = InMemoryOptions::default();
		options.max_write_set_bytes =
			crate::datastore::consts::BASE_ATOM_WRITE_BYTES + metadata - 1;
		let limited = Datastore::from_root_for_test(options, root);
		let mut tx = limited.begin();
		let record = "item:rejected".parse::<RecordId>()?;
		assert!(tx.create_atom(record.clone(), PropertyMap::new()).is_err());
		assert!(tx.working.resolve_atom(&record).is_none());
		assert_eq!(tx.write_set_bytes(), 0);
		assert!(tx.change_journal.operations.is_empty());
		assert!(tx.change_journal.intents.is_empty());
		Ok(())
	}

	#[test]
	fn changefeed_journal_tracks_noops_and_groups_internal_setters() -> FerrosGrynnResult<()> {
		let (store, record, graph, key) = setup(InMemoryOptions::default())?;
		let mut tx = store.begin();
		let missing = crate::values::PropertyKeyId::new(99);
		tx.remove_base_property(&record, missing)?;
		tx.reset_property(graph, &record, key)?;
		tx.set_occurrence_label(graph, &record, None)?;
		tx.replace_occurrence_state(graph, &record, crate::datastore::OccurrenceState::Node {
			nested_graph: None,
		})?;
		assert_eq!(tx.change_journal.operations.len(), 4);
		assert!(
			tx.change_journal.operations.iter().all(|operation| operation.mutations.is_empty())
		);
		for (ordinal, operation) in tx.change_journal.operations.iter().enumerate() {
			assert_eq!(operation.ordinal, ordinal as u64);
		}
		for outcome in
			tx.execute("UPDATE item:a SET value = 1, extra = 2 RETURN NONE", &Object::new())?
		{
			let _result = outcome.result?;
		}
		assert_eq!(tx.change_journal.operations.len(), 5);
		let operation = &tx.change_journal.operations[4];
		assert_eq!(operation.intents.len(), 1);
		let before = tx.change_journal.checkpoint();
		assert!(tx.attach_node(graph, &record, None, None).is_err());
		assert_eq!(tx.change_journal.operations.len(), before.operations);
		assert_eq!(tx.change_journal.bytes, before.bytes);
		for outcome in
			tx.execute("UPDATE item:missing SET value = 3 RETURN NONE", &Object::new())?
		{
			let _result = outcome.result?;
		}
		assert_eq!(tx.change_journal.operations.len(), before.operations);
		let rollback = tx.atomic_mutation(|transaction| -> FerrosGrynnResult<()> {
			transaction.set_base_property(&record, key, Value::from(7))?;
			Err(FerrosGrynnError::invalid_argument("fixture operation fails"))
		});
		assert!(rollback.is_err());
		assert_eq!(tx.change_journal.operations.len(), before.operations);
		assert_eq!(tx.change_journal.bytes, before.bytes);
		assert_eq!(
			tx.atom(&record)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture atom is absent"))?
				.properties()
				.get(key),
			Some(&Value::from(1))
		);
		Ok(())
	}

	#[test]
	fn changefeed_journal_caps_and_ordinals_survive_rollback_and_seals() -> FerrosGrynnResult<()> {
		let mut options = InMemoryOptions::default();
		options.max_write_set_bytes = 16 * crate::datastore::consts::SCHEMA_WRITE_BYTES;
		let (store, record, _graph, _key) = setup(options)?;
		let mut tx = store.begin();
		let missing = crate::values::PropertyKeyId::new(99);
		tx.change_journal.next_ordinal = u64::MAX;
		assert!(tx.remove_base_property(&record, missing).is_err());
		assert!(tx.change_journal.operations.is_empty());
		assert_eq!(tx.change_journal.bytes, 0);
		tx.change_journal.next_ordinal = 0;
		tx.remove_base_property(&record, missing)?;
		let _captured = tx.seal_import_segment(tx.snapshot.commit_ts)?;
		assert_eq!(tx.change_journal.next_ordinal, 1);
		tx.remove_base_property(&record, missing)?;
		assert_eq!(tx.change_journal.operations[0].ordinal, 1);
		loop {
			if tx.remove_base_property(&record, missing).is_err() {
				break;
			}
		}
		assert!(tx.write_set_bytes() <= store.inner.transaction_limits().max_write_set_bytes);
		assert!(tx.mutations.is_empty());
		Ok(())
	}

	#[test]
	fn changefeed_replay_ranges_share_rebased_allocation_map() -> FerrosGrynnResult<()> {
		let (store, _record, _graph, key) = setup(InMemoryOptions::default())?;
		let mut left = store.begin();
		let mut right = store.begin();
		let first = "item:left".parse::<RecordId>()?;
		let second = "item:right".parse::<RecordId>()?;
		left.create_atom(first.clone(), PropertyMap::new())?;
		left.set_base_property(&first, key, Value::from(9))?;
		right.create_atom(second.clone(), PropertyMap::new())?;
		right.commit()?;
		let mut replay = (*store.inner.current_root()).clone();
		let mut remap = super::super::commit::ReplayRemap::default();
		let published_at = replay.commit_ts + 1;
		for operation in &left.change_journal.operations {
			Transaction::replay_mutations(
				&mut replay,
				&left.mutations[operation.mutations.clone()],
				published_at,
				true,
				&mut remap,
			)?;
		}
		let global = replay
			.resolve_atom(&first)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("replayed atom is absent"))?;
		assert_eq!(
			replay.atom(global).and_then(|atom| atom.properties().get(key)),
			Some(&Value::from(9))
		);
		let other = replay
			.resolve_atom(&second)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("concurrent atom is absent"))?;
		assert!(replay.atom(other).is_some_and(|atom| atom.properties().get(key).is_none()));
		Ok(())
	}
}
