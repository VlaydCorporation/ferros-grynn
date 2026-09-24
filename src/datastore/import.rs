//! Atomic bulk-import coordination over transaction-private COW roots.

use std::{
	collections::BTreeMap,
	sync::{Arc, atomic::Ordering},
	time::{Duration, Instant},
};

use super::{
	Datastore,
	IndexId,
	StoreInner,
	Transaction,
	TransactionAccess,
	TransactionOptions,
	consts::{DEFAULT_IMPORT_SEGMENT_ITEMS, IMPORT_MAX_REPORTED_VIOLATIONS},
};
use crate::{
	gql_status::{
		Condition,
		Domain,
		ErrorClassification,
		FerrosGrynnError,
		FerrosGrynnResult,
		StatusCode,
	},
	graph::GraphId,
};

/// Index-maintenance policy used by one atomic import.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportIndexMode {
	/// Rebuild derived indexes once before publication.
	#[default]
	Deferred,
	/// Maintain indexes for every mutation in the private root.
	Maintained,
}

/// Constraint-validation policy used by one atomic import.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportConstraintMode {
	/// Defer whole-graph checks until final validation.
	#[default]
	Deferred,
	/// Validate graph-family constraints after every logical item.
	Immediate,
}

/// Action taken after one item-level import violation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportViolationPolicy {
	/// Abort and discard the complete import.
	#[default]
	Abort,
	/// Roll back only the failing item and continue within an explicit limit.
	Skip,
}

/// Validated public settings for one atomic import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportOptions {
	/// Index-maintenance policy.
	pub indexes: ImportIndexMode,
	/// Constraint-validation policy.
	pub constraints: ImportConstraintMode,
	/// Item-level violation policy.
	pub on_violation: ImportViolationPolicy,
	/// Required positive limit when [`ImportViolationPolicy::Skip`] is
	/// selected.
	pub reject_limit: Option<usize>,
	/// Positive count of applied items retained in one private segment.
	pub segment_size: usize,
	/// Whether final validation must include all affected pre-existing data.
	pub full_validation: bool,
}

impl Default for ImportOptions {
	fn default() -> Self {
		Self {
			indexes: ImportIndexMode::Deferred,
			constraints: ImportConstraintMode::Deferred,
			on_violation: ImportViolationPolicy::Abort,
			reject_limit: None,
			segment_size: DEFAULT_IMPORT_SEGMENT_ITEMS,
			full_validation: false,
		}
	}
}

impl ImportOptions {
	/// Validates cross-field option invariants before acquiring the writer
	/// lease.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for a zero segment size or an invalid
	/// `on_violation`/`reject_limit` combination.
	pub(crate) fn validate(self) -> FerrosGrynnResult<Self> {
		if self.segment_size == 0 {
			return Err(FerrosGrynnError::invalid_argument(
				"import segment_size must be greater than zero",
			));
		}
		match (self.on_violation, self.reject_limit) {
			(ImportViolationPolicy::Abort, None) | (ImportViolationPolicy::Skip, Some(1..)) => {
				Ok(self)
			}
			(ImportViolationPolicy::Abort, Some(_)) => Err(FerrosGrynnError::invalid_argument(
				"import reject_limit is valid only with the Skip violation policy",
			)),
			(ImportViolationPolicy::Skip, None | Some(0)) => {
				Err(FerrosGrynnError::invalid_argument(
					"the Skip violation policy requires a positive reject_limit",
				))
			}
		}
	}
}

/// One bounded diagnostic retained for a skipped import item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportViolation {
	/// One-based logical item ordinal.
	item: u64,
	/// Status code used for aggregate classification.
	code: StatusCode,
	/// Redacted user-facing diagnostic text.
	description: String,
}

impl ImportViolation {
	/// Returns the one-based logical item number.
	pub const fn item(&self) -> u64 {
		self.item
	}

	/// Returns the structured status code that classified the violation.
	pub const fn code(&self) -> StatusCode {
		self.code
	}

	/// Returns the redacted user-facing status description.
	pub fn description(&self) -> &str {
		&self.description
	}
}

/// Result of applying one logical item to an import session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportItemOutcome {
	/// The complete item was applied to the private root.
	Applied,
	/// The complete item was rolled back after an allowed violation.
	Skipped(ImportViolation),
}

/// Structured result of one successfully published import.
#[derive(Debug, Clone)]
pub struct ImportReport {
	/// Total logical items presented by the caller.
	processed_items: u64,
	/// Items retained in the published root.
	applied_items: u64,
	/// Items rolled back by the `Skip` policy.
	skipped_items: u64,
	/// Non-empty private mutation segments.
	segments: u64,
	/// Complete violation totals grouped by status code.
	violations: BTreeMap<StatusCode, u64>,
	/// Bounded detailed violations.
	details: Vec<ImportViolation>,
	/// Secondary indexes rebuilt during finalization.
	built_indexes: Vec<IndexId>,
	/// Aggregate phase durations.
	phase_timings: BTreeMap<String, Duration>,
	/// Per-index physical rebuild durations.
	index_timings: BTreeMap<IndexId, Duration>,
	/// Effective index policy.
	indexes: ImportIndexMode,
	/// Effective constraint policy.
	constraints: ImportConstraintMode,
	/// Whether finalization rechecked all affected existing data.
	full_validation: bool,
}

impl ImportReport {
	/// Returns all logical items presented to [`ImportSession::apply_item`].
	pub const fn processed_items(&self) -> u64 {
		self.processed_items
	}

	/// Returns successfully applied logical items.
	pub const fn applied_items(&self) -> u64 {
		self.applied_items
	}

	/// Returns atomically rolled-back logical items.
	pub const fn skipped_items(&self) -> u64 {
		self.skipped_items
	}

	/// Returns non-empty private transaction segments.
	pub const fn segments(&self) -> u64 {
		self.segments
	}

	/// Returns complete violation totals grouped by status code.
	pub const fn violations(&self) -> &BTreeMap<StatusCode, u64> {
		&self.violations
	}

	/// Returns at most `FG_IMPORT_MAX_REPORTED_VIOLATIONS` details.
	pub fn violation_details(&self) -> &[ImportViolation] {
		&self.details
	}

	/// Returns indexes rebuilt by deferred finalization.
	pub fn built_indexes(&self) -> &[IndexId] {
		&self.built_indexes
	}

	/// Returns elapsed time grouped by import phase.
	pub const fn phase_timings(&self) -> &BTreeMap<String, Duration> {
		&self.phase_timings
	}

	/// Returns elapsed rebuild time for individually instrumented indexes.
	pub const fn index_timings(&self) -> &BTreeMap<IndexId, Duration> {
		&self.index_timings
	}

	/// Returns the selected index policy.
	pub const fn index_mode(&self) -> ImportIndexMode {
		self.indexes
	}

	/// Returns the selected constraint policy.
	pub const fn constraint_mode(&self) -> ImportConstraintMode {
		self.constraints
	}

	/// Returns whether full final validation was requested.
	pub const fn full_validation(&self) -> bool {
		self.full_validation
	}
}

/// Transaction-backed callback surface for one atomic import.
pub struct ImportSession {
	/// Private import transaction, consumed only by finalization.
	transaction: Option<Transaction>,
	/// Validated import options.
	options: ImportOptions,
	/// Commit timestamp reserved for segment version stamps and publication.
	commit_ts: u64,
	/// Total presented logical items.
	processed: u64,
	/// Successfully applied logical items.
	applied: u64,
	/// Atomically rolled-back logical items.
	skipped: u64,
	/// Sealed non-empty private segments.
	segments: u64,
	/// Applied items in the current segment.
	applied_in_segment: usize,
	/// Complete violation totals grouped by status code.
	violations: BTreeMap<StatusCode, u64>,
	/// Bounded detailed violations.
	details: Vec<ImportViolation>,
	/// Whether at least one item changed the private root.
	changed: bool,
	/// First fatal failure, retained so callers cannot swallow it and publish.
	failure: Option<FerrosGrynnError>,
}

impl ImportSession {
	/// Creates the callback surface around one private transaction.
	fn new(transaction: Transaction, options: ImportOptions, commit_ts: u64) -> Self {
		Self {
			transaction: Some(transaction),
			options,
			commit_ts,
			processed: 0,
			applied: 0,
			skipped: 0,
			segments: 0,
			applied_in_segment: 0,
			violations: BTreeMap::new(),
			details: Vec::new(),
			changed: false,
			failure: None,
		}
	}

	/// Applies one logical input item atomically through the existing
	/// transaction API.
	///
	/// # Errors
	///
	/// Returns fatal mutation failures, the first violation in `Abort` mode, or
	/// a reject-limit error after rolling back the offending item.
	pub fn apply_item<T, Apply>(&mut self, apply: Apply) -> FerrosGrynnResult<ImportItemOutcome>
	where
		Apply: FnOnce(&mut Transaction) -> FerrosGrynnResult<T>,
	{
		if let Some(error) = &self.failure {
			return Err(error.clone());
		}
		increment(&mut self.processed, "import processed item count")?;
		let transaction = self.transaction.as_mut().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("the import session is already finished")
		})?;
		match transaction.atomic_import_item(apply) {
			Ok(changed) => {
				increment(&mut self.applied, "import applied item count")?;
				self.applied_in_segment =
					self.applied_in_segment.checked_add(1).ok_or_else(|| {
						FerrosGrynnError::operation_overflow("import segment item count")
					})?;
				self.changed |= changed;
				if self.applied_in_segment == self.options.segment_size {
					transaction.seal_import_segment(self.commit_ts);
					increment(&mut self.segments, "import segment count")?;
					self.applied_in_segment = 0;
				}
				Ok(ImportItemOutcome::Applied)
			}
			Err(error)
				if self.options.on_violation == ImportViolationPolicy::Skip
					&& is_item_violation(&error) =>
			{
				increment(&mut self.skipped, "import skipped item count")?;
				let violation = ImportViolation {
					item: self.processed,
					code: error.status().code(),
					description: error.status().description(),
				};
				increment(
					self.violations.entry(violation.code).or_insert(0),
					"import violation count",
				)?;
				if self.details.len() < IMPORT_MAX_REPORTED_VIOLATIONS {
					self.details.push(violation.clone());
				}
				let limit = self.options.reject_limit.unwrap_or(0);
				if usize::try_from(self.skipped).unwrap_or(usize::MAX) > limit {
					let error = FerrosGrynnError::import_reject_limit_exceeded(
						u64::try_from(limit).unwrap_or(u64::MAX),
					);
					self.failure = Some(error.clone());
					return Err(error);
				}
				Ok(ImportItemOutcome::Skipped(violation))
			}
			Err(error) => {
				self.failure = Some(error.clone());
				Err(error)
			}
		}
	}

	/// Seals the final segment and returns the transaction plus its report
	/// seed.
	///
	/// # Errors
	///
	/// Returns the retained fatal item failure, an already-finished error, or a
	/// counter-overflow status.
	fn finish(mut self) -> FerrosGrynnResult<(Transaction, ImportReport, bool)> {
		if let Some(error) = self.failure.take() {
			return Err(error);
		}
		let mut transaction = self.transaction.take().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("the import session is already finished")
		})?;
		if self.applied_in_segment != 0 {
			transaction.seal_import_segment(self.commit_ts);
			increment(&mut self.segments, "import segment count")?;
		}
		let report = ImportReport {
			processed_items: self.processed,
			applied_items: self.applied,
			skipped_items: self.skipped,
			segments: self.segments,
			violations: self.violations,
			details: self.details,
			built_indexes: Vec::new(),
			phase_timings: BTreeMap::new(),
			index_timings: BTreeMap::new(),
			indexes: self.options.indexes,
			constraints: self.options.constraints,
			full_validation: self.options.full_validation,
		};
		Ok((transaction, report, self.changed))
	}
}

impl Datastore {
	/// Runs an atomic callback import against one private database root.
	///
	/// # Errors
	///
	/// Returns option, writer-lease, item, validation, persistence, or
	/// publication errors without exposing a partial root.
	pub fn import<Run>(&self, options: ImportOptions, run: Run) -> FerrosGrynnResult<ImportReport>
	where
		Run: FnOnce(&mut ImportSession) -> FerrosGrynnResult<()>,
	{
		self.import_at(options, None, None, run)
	}

	/// Runs an import with analysis-time snapshot and graph-scope guards.
	///
	/// # Errors
	///
	/// Returns any option, lease, callback, validation, persistence, or
	/// publication failure without swapping the private root into visibility.
	pub(crate) fn import_at<Run>(
		&self,
		options: ImportOptions,
		expected_commit_ts: Option<u64>,
		graph_scope: Option<GraphId>,
		run: Run,
	) -> FerrosGrynnResult<ImportReport>
	where
		Run: FnOnce(&mut ImportSession) -> FerrosGrynnResult<()>,
	{
		let options = options.validate()?;
		let started = Instant::now();
		let owner = self.inner.next_transaction.fetch_add(1, Ordering::Relaxed);
		let (lease, transaction, commit_ts) = {
			let _commit = self.inner.commit.lock();
			let lease = ImportLease::acquire(self.inner.clone(), owner)?;
			let snapshot = self.inner.current_root();
			if expected_commit_ts.is_some_and(|expected| expected != snapshot.commit_ts) {
				return Err(FerrosGrynnError::serialization_failure(
					owner,
					"the database changed after import analysis",
				));
			}
			let commit_ts = snapshot.commit_ts.checked_add(1).ok_or_else(|| {
				FerrosGrynnError::serialization_failure(owner, "the commit clock is exhausted")
			})?;
			let transaction = Transaction::new_import(
				self.clone(),
				owner,
				TransactionOptions::new(
					crate::datastore::IsolationLevel::Serializable,
					TransactionAccess::ReadWrite,
				),
				snapshot,
				options.indexes,
				options.constraints,
				options.on_violation,
				graph_scope,
			);
			(lease, transaction, commit_ts)
		};

		let callback_started = Instant::now();
		let mut session = ImportSession::new(transaction, options, commit_ts);
		run(&mut session)?;
		let callback_elapsed = callback_started.elapsed();
		let (transaction, mut report, changed) = session.finish()?;

		let validation_started = Instant::now();
		let (mut root, base_commit_ts, built_indexes) =
			transaction.finish_import(commit_ts, options.indexes, options.full_validation)?;
		report.phase_timings.insert("validation".to_owned(), validation_started.elapsed());
		report.built_indexes = built_indexes;

		let publication_started = Instant::now();
		if changed {
			let _commit = self.inner.commit.lock();
			self.inner.ensure_import_owner(owner)?;
			let current = self.inner.current_root();
			if current.commit_ts != base_commit_ts {
				return Err(FerrosGrynnError::serialization_failure(
					owner,
					"the import base snapshot changed before publication",
				));
			}
			root.commit_ts = commit_ts;
			report.index_timings = self.inner.persist_import_root(&root)?;
			*self.inner.current.write() = Arc::new(root);
		}
		report.phase_timings.insert("callback".to_owned(), callback_elapsed);
		report.phase_timings.insert("publication".to_owned(), publication_started.elapsed());
		report.phase_timings.insert("total".to_owned(), started.elapsed());
		drop(lease);
		Ok(report)
	}
}

impl StoreInner {
	/// Rejects a normal writer while another transaction owns the import lease.
	///
	/// # Errors
	///
	/// Returns `ImportInProgress` when `transaction` is not the active owner.
	pub(crate) fn ensure_write_allowed(&self, transaction: u64) -> FerrosGrynnResult<()> {
		let owner = self.import_owner.load(Ordering::Acquire);
		if owner == 0 || owner == transaction {
			Ok(())
		} else {
			Err(FerrosGrynnError::import_in_progress())
		}
	}

	/// Verifies that publication still owns the database-wide import lease.
	///
	/// # Errors
	///
	/// Returns a serialization failure if the lease no longer belongs to the
	/// supplied transaction.
	fn ensure_import_owner(&self, transaction: u64) -> FerrosGrynnResult<()> {
		if self.import_owner.load(Ordering::Acquire) == transaction {
			Ok(())
		} else {
			Err(FerrosGrynnError::serialization_failure(
				transaction,
				"the import writer lease was lost",
			))
		}
	}
}

/// RAII guard for the database-wide import writer lease.
struct ImportLease {
	/// Store whose writer slot is guarded.
	store: Arc<StoreInner>,
	/// Transaction identity stored in the writer slot.
	owner: u64,
}

impl ImportLease {
	/// Acquires an empty writer slot for `owner`.
	///
	/// # Errors
	///
	/// Returns `ImportInProgress` if another import already owns the slot.
	fn acquire(store: Arc<StoreInner>, owner: u64) -> FerrosGrynnResult<Self> {
		store
			.import_owner
			.compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
			.map_err(|_active| FerrosGrynnError::import_in_progress())?;
		Ok(Self {
			store,
			owner,
		})
	}
}

impl Drop for ImportLease {
	fn drop(&mut self) {
		let _release = self.store.import_owner.compare_exchange(
			self.owner,
			0,
			Ordering::AcqRel,
			Ordering::Acquire,
		);
	}
}

/// Returns whether a client-side domain error can be isolated to one item.
fn is_item_violation(error: &FerrosGrynnError) -> bool {
	let status = error.status();
	if status.error_classification() != Some(ErrorClassification::ClientError) {
		return false;
	}
	(matches!(status.condition(), Condition::DataException)
		&& matches!(status.domain(), Domain::Query | Domain::Value | Domain::Graph))
		|| matches!(
			status.condition(),
			Condition::DependentObjectError | Condition::GraphTypeViolation
		)
		|| matches!(status.code().body(), "40F01" | "42F01")
}

/// Increments one report counter with a typed overflow error.
///
/// # Errors
///
/// Returns an operation-overflow status when the counter is exhausted.
fn increment(counter: &mut u64, operation: &'static str) -> FerrosGrynnResult<()> {
	*counter =
		counter.checked_add(1).ok_or_else(|| FerrosGrynnError::operation_overflow(operation))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::{
			ClassField,
			ClassIndex,
			ClassIndexKind,
			ClassSchema,
			ClassSchemaMode,
			IndexEntries,
			IndexScope,
			IsolationLevel,
			TransactionAccess,
			TransactionOptions,
		},
		graph::AtomKind,
		values::{PropertyKeyId, PropertyMap, RecordId, RecordIdPart, Type},
	};

	fn record(name: &str) -> RecordId {
		RecordId::new("person", RecordIdPart::String(name.into())).unwrap()
	}

	fn store_with_class() -> Datastore {
		let store = Datastore::in_memory();
		let mut transaction = store.begin();
		transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		transaction.commit().unwrap();
		store
	}

	fn store_with_unique_email() -> (Datastore, PropertyKeyId, IndexId) {
		let store = Datastore::in_memory();
		let mut transaction = store.begin();
		let class =
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("email", Type::String)])
				.unwrap()
				.with_index(
					ClassIndex::new("unique_email", ClassIndexKind::Unique, IndexScope::Base, [
						"email",
					])
					.unwrap(),
				)
				.unwrap();
		transaction.define_class(class).unwrap();
		transaction.commit().unwrap();
		let class = store.class("person").unwrap();
		let property = class.fields().next().and_then(|field| field.key).unwrap();
		let index = class.indexes().next().and_then(|index| index.id()).unwrap();
		(store, property, index)
	}

	fn email(value: &str, key: PropertyKeyId) -> PropertyMap {
		let mut properties = PropertyMap::new();
		properties.insert(key, value);
		properties
	}

	#[test]
	fn publishes_all_segments_once_and_preserves_old_readers() {
		let store = store_with_class();
		let mut old_reader = store.begin();
		let options = ImportOptions {
			segment_size: 1,
			..ImportOptions::default()
		};
		let report = store
			.import(options, |session| {
				for name in ["alice", "bob"] {
					session.apply_item(|transaction| {
						transaction.create_atom(record(name), PropertyMap::new())
					})?;
				}
				Ok(())
			})
			.unwrap();

		assert_eq!(report.processed_items(), 2);
		assert_eq!(report.applied_items(), 2);
		assert_eq!(report.segments(), 2);
		assert!(old_reader.atom(&record("alice")).unwrap().is_none());
		assert!(store.begin().atom(&record("alice")).unwrap().is_some());
		assert_eq!(store.inner.current_root().commit_ts, 2);
	}

	#[test]
	fn abort_discards_the_complete_private_root() {
		let store = store_with_class();
		let error = store
			.import(ImportOptions::default(), |session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})?;
				Err(FerrosGrynnError::invalid_argument("abort import"))
			})
			.unwrap_err();

		assert_eq!(error.status().code().to_string(), "FG-22N11");
		assert!(store.begin().atom(&record("alice")).unwrap().is_none());
		assert_eq!(store.inner.current_root().commit_ts, 1);
	}

	#[test]
	fn skip_never_downgrades_storage_failures() {
		let store = store_with_class();
		let options = ImportOptions {
			on_violation: ImportViolationPolicy::Skip,
			reject_limit: Some(1),
			..ImportOptions::default()
		};
		let error = store
			.import(options, |session| {
				session.apply_item(|_transaction| -> FerrosGrynnResult<()> {
					Err(FerrosGrynnError::invalid_storage_file("forced import storage failure"))
				})?;
				Ok(())
			})
			.unwrap_err();

		assert_eq!(error.status().code().to_string(), "FG-22S09");
		assert_eq!(store.inner.current_root().commit_ts, 1);
	}

	#[test]
	fn violation_details_are_bounded_while_totals_remain_exact() {
		let store = store_with_class();
		let duplicate_count = IMPORT_MAX_REPORTED_VIOLATIONS + 5;
		let options = ImportOptions {
			on_violation: ImportViolationPolicy::Skip,
			reject_limit: Some(duplicate_count),
			..ImportOptions::default()
		};
		let report = store
			.import(options, |session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})?;
				for _ in 0..duplicate_count {
					session.apply_item(|transaction| {
						transaction.create_atom(record("alice"), PropertyMap::new())
					})?;
				}
				Ok(())
			})
			.unwrap();

		assert_eq!(report.skipped_items(), duplicate_count as u64);
		assert_eq!(report.violation_details().len(), IMPORT_MAX_REPORTED_VIOLATIONS);
		assert_eq!(report.violations().values().sum::<u64>(), duplicate_count as u64);
	}

	#[test]
	fn unwind_discards_staging_and_releases_the_writer_lease() {
		let store = store_with_class();
		let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
			let _report = store.import(ImportOptions::default(), |session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})?;
				panic!("forced import unwind");
			});
		}));
		assert!(unwind.is_err());
		assert!(store.begin().atom(&record("alice")).unwrap().is_none());

		let mut writer = store.begin();
		writer.create_atom(record("after"), PropertyMap::new()).unwrap();
		writer.commit().unwrap();
		assert!(store.begin().atom(&record("after")).unwrap().is_some());
	}

	#[test]
	fn skip_rolls_back_one_item_and_enforces_the_limit() {
		let store = store_with_class();
		let options = ImportOptions {
			on_violation: ImportViolationPolicy::Skip,
			reject_limit: Some(1),
			..ImportOptions::default()
		};
		let report = store
			.import(options, |session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})?;
				let outcome = session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})?;
				assert!(matches!(outcome, ImportItemOutcome::Skipped(_)));
				Ok(())
			})
			.unwrap();
		assert_eq!(report.applied_items(), 1);
		assert_eq!(report.skipped_items(), 1);
		assert!(store.begin().atom(&record("alice")).unwrap().is_some());

		let failed = store.import(options, |session| {
			for _ in 0..2 {
				session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})?;
			}
			Ok(())
		});
		assert_eq!(failed.unwrap_err().status().code().to_string(), "FG-54F08");
		assert_eq!(store.inner.current_root().commit_ts, 2);
	}

	#[test]
	fn writer_lease_blocks_other_writers_while_readers_keep_the_old_root() {
		let store = store_with_class();
		let other = store.clone();
		store
			.import(ImportOptions::default(), |session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("pending"), PropertyMap::new())
				})?;
				let mut reader = other.begin_with(TransactionOptions::new(
					IsolationLevel::Serializable,
					TransactionAccess::ReadOnly,
				));
				assert!(reader.atom(&record("pending"))?.is_none());

				let mut writer = other.begin();
				let error = writer.create_atom(record("blocked"), PropertyMap::new()).unwrap_err();
				assert_eq!(error.status().code().to_string(), "FG-25F03");
				Ok(())
			})
			.unwrap();
		assert!(store.begin().atom(&record("pending")).unwrap().is_some());
	}

	#[test]
	fn ignored_abort_item_error_poisoned_the_complete_import() {
		let store = store_with_class();
		let result = store.import(ImportOptions::default(), |session| {
			session.apply_item(|transaction| {
				transaction.create_atom(record("alice"), PropertyMap::new())
			})?;
			let error = session
				.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				})
				.unwrap_err();
			assert_eq!(error.status().code().to_string(), "FG-G2F04");
			Ok(())
		});

		assert_eq!(result.unwrap_err().status().code().to_string(), "FG-G2F04");
		assert!(store.begin().atom(&record("alice")).unwrap().is_none());
		assert_eq!(store.inner.current_root().commit_ts, 1);
	}

	#[test]
	fn forbidden_schema_mutation_poisoned_import_before_allocating_an_id() {
		let store = store_with_class();
		let next_schema_id = store.inner.next_schema_id.load(Ordering::Relaxed);
		let result = store.import(ImportOptions::default(), |session| {
			let error = session
				.apply_item(|transaction| {
					transaction
						.define_class(ClassSchema::new("forbidden", AtomKind::Node, []).unwrap())
				})
				.unwrap_err();
			assert_eq!(error.status().code().to_string(), "FG-50F03");
			Ok(())
		});

		assert_eq!(result.unwrap_err().status().code().to_string(), "FG-50F03");
		assert!(store.class("forbidden").is_none());
		assert_eq!(store.inner.next_schema_id.load(Ordering::Relaxed), next_schema_id);
	}

	#[test]
	fn ignored_reject_limit_error_poisoned_the_complete_import() {
		let store = store_with_class();
		let options = ImportOptions {
			on_violation: ImportViolationPolicy::Skip,
			reject_limit: Some(1),
			..ImportOptions::default()
		};
		let result = store.import(options, |session| {
			session.apply_item(|transaction| {
				transaction.create_atom(record("alice"), PropertyMap::new())
			})?;
			for duplicate in 0..2 {
				let result = session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), PropertyMap::new())
				});
				if duplicate == 0 {
					assert!(matches!(result?, ImportItemOutcome::Skipped(_)));
				} else {
					assert_eq!(result.unwrap_err().status().code().to_string(), "FG-54F08");
				}
			}
			Ok(())
		});

		assert_eq!(result.unwrap_err().status().code().to_string(), "FG-54F08");
		assert!(store.begin().atom(&record("alice")).unwrap().is_none());
		assert_eq!(store.inner.current_root().commit_ts, 1);
	}

	#[test]
	fn deferred_unique_violation_discards_the_complete_import() {
		let (store, email_key, _index) = store_with_unique_email();
		let result = store.import(ImportOptions::default(), |session| {
			for name in ["alice", "bob"] {
				session.apply_item(|transaction| {
					transaction.create_atom(record(name), email("same@example.test", email_key))
				})?;
			}
			Ok(())
		});

		assert!(result.is_err());
		assert!(store.begin().atom(&record("alice")).unwrap().is_none());
		assert!(store.begin().atom(&record("bob")).unwrap().is_none());
		assert_eq!(store.inner.current_root().commit_ts, 1);
	}

	#[test]
	fn immediate_unique_violation_can_skip_one_item_with_deferred_indexes() {
		let (store, email_key, index) = store_with_unique_email();
		let options = ImportOptions {
			constraints: ImportConstraintMode::Immediate,
			on_violation: ImportViolationPolicy::Skip,
			reject_limit: Some(1),
			..ImportOptions::default()
		};
		let report = store
			.import(options, |session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("alice"), email("same@example.test", email_key))
				})?;
				let outcome = session.apply_item(|transaction| {
					transaction.create_atom(record("bob"), email("same@example.test", email_key))
				})?;
				assert!(matches!(outcome, ImportItemOutcome::Skipped(_)));
				Ok(())
			})
			.unwrap();

		assert_eq!(report.applied_items(), 1);
		assert_eq!(report.skipped_items(), 1);
		assert_eq!(report.built_indexes(), &[index]);
		assert!(store.begin().atom(&record("alice")).unwrap().is_some());
		assert!(store.begin().atom(&record("bob")).unwrap().is_none());
	}

	#[test]
	fn deferred_and_maintained_indexes_produce_the_same_root() {
		let run = |mode| {
			let (store, email_key, index) = store_with_unique_email();
			let report = store
				.import(
					ImportOptions {
						indexes: mode,
						..ImportOptions::default()
					},
					|session| {
						for (name, address) in
							[("alice", "alice@example.test"), ("bob", "bob@example.test")]
						{
							session.apply_item(|transaction| {
								transaction.create_atom(record(name), email(address, email_key))
							})?;
						}
						Ok(())
					},
				)
				.unwrap();
			let root = store.inner.current_root();
			let entries = match &root.indexes.base.get(&index).unwrap().entries {
				IndexEntries::Unique(entries) => entries.clone(),
				_ => panic!("test index must remain unique"),
			};
			(entries, root.statistics.as_ref().clone(), report)
		};

		let (deferred_entries, deferred_statistics, deferred_report) =
			run(ImportIndexMode::Deferred);
		let (maintained_entries, maintained_statistics, maintained_report) =
			run(ImportIndexMode::Maintained);
		assert_eq!(deferred_entries, maintained_entries);
		assert_eq!(deferred_statistics, maintained_statistics);
		assert_eq!(deferred_report.built_indexes().len(), 1);
		assert!(maintained_report.built_indexes().is_empty());
	}

	#[test]
	fn full_validation_rechecks_preexisting_atom_schema() {
		let store = Datastore::in_memory();
		let mut schema = store.begin();
		schema
			.define_class(
				ClassSchema::new("person", AtomKind::Node, [ClassField::new(
					"email",
					Type::String,
				)])
				.unwrap()
				.with_schema_mode(ClassSchemaMode::Schemafull),
			)
			.unwrap();
		schema.commit().unwrap();
		let email_key = store.property_dictionary().get("email").unwrap();
		let mut invalid = PropertyMap::new();
		invalid.insert(email_key, 42_i64);
		let mut seed = store.begin();
		seed.create_atom(record("invalid"), invalid).unwrap();
		seed.commit().unwrap();

		let result = store.import(
			ImportOptions {
				full_validation: true,
				..ImportOptions::default()
			},
			|session| {
				session.apply_item(|transaction| {
					transaction.create_atom(record("candidate"), PropertyMap::new())
				})?;
				Ok(())
			},
		);

		assert!(result.is_err());
		assert!(store.begin().atom(&record("candidate")).unwrap().is_none());
		assert!(store.begin().atom(&record("invalid")).unwrap().is_some());
	}
}
