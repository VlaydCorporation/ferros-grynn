#![allow(
	clippy::missing_docs_in_private_items,
	clippy::redundant_pub_crate,
	reason = "the crate-private registry is documented as one session-lifetime contract"
)]

//! Session-owned GrynnQL language-closure registry.

use std::{
	collections::{BTreeMap, BTreeSet},
	mem::size_of,
	sync::{
		Arc,
		atomic::{AtomicBool, AtomicU64, Ordering},
	},
};

use parking_lot::Mutex;

use super::ir::{LogicalClosure, NamedFunctionKey};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Closure, RuntimeId, Value},
};

/// Fallback byte budget for retained closures and named-function plans
/// when no scratch limit is supplied.
const MAX_RETAINED_BYTES: usize = 256 * 1024 * 1024;

/// Session-owned closure registry with a lock-free empty fast path.
#[derive(Debug, Default)]
pub(crate) struct LanguageClosureState {
	/// Retained closure registry or time spent restoring a startup registry.
	registry: Mutex<LanguageClosureRegistry>,
	/// Whether runtime closure state has been populated with retained entries.
	populated: AtomicBool,
	/// Whether compilation is subject to the configured scratch-memory limit.
	compiler_bounded: bool,
	/// Optional embedding work cap, charged before tokenization/analysis.
	compiler_work: Option<AtomicU64>,
	/// Registration compiler interruption without retaining the caller cache.
	compiler_checks: Option<super::options::ExecutionChecks>,
}

impl LanguageClosureState {
	/// Shares one admission cap between retained plans and live compiler
	/// frames.
	pub(in crate::gql) fn bounded(limit: usize) -> Self {
		Self {
			registry: Mutex::new(LanguageClosureRegistry {
				scratch_limit: Some(limit),
				..LanguageClosureRegistry::default()
			}),
			populated: AtomicBool::new(false),
			compiler_bounded: true,
			compiler_work: None,
			compiler_checks: None,
		}
	}

	/// Adds a conservative source-byte work cap to the existing compiler scope.
	pub(in crate::gql) fn bounded_with_work(limit: usize, work: u64) -> Self {
		let mut state = Self::bounded(limit);
		state.compiler_work = Some(AtomicU64::new(work));
		state
	}

	/// Binds checks only for the lifetime of this private compiler instance.
	pub(in crate::gql) fn with_compile_checks(
		mut self,
		checks: Option<super::options::ExecutionChecks>,
	) -> Self {
		self.compiler_checks = checks;
		self
	}

	/// Admits compilation before source, tokens, syntax or typed IR are
	/// allocated.
	pub(in crate::gql) fn admit_compile(
		&self,
		source_bytes: usize,
	) -> FerrosGrynnResult<CompileReservation<'_>> {
		self.check_compile()?;
		let work = if self.compiler_work.is_some() {
			u64::try_from(source_bytes)
				.ok()
				.and_then(|bytes| bytes.checked_add(1))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE compiler work"))?
		} else {
			0
		};
		self.reserve_compile(
			source_bytes.checked_mul(1024).and_then(|bytes| bytes.checked_add(16_384)),
			work,
		)
	}

	/// Raw signature copies share the existing account without source
	/// expansion.
	pub(in crate::gql) fn admit_compile_metadata(
		&self,
		bytes: usize,
		work: u64,
	) -> FerrosGrynnResult<CompileReservation<'_>> {
		self.check_compile()?;
		self.reserve_compile(Some(bytes), work)
	}

	/// Borrowed metadata traversal inherits compiler cancellation and deadline.
	pub(in crate::gql) fn check_compile(&self) -> FerrosGrynnResult<()> {
		if let Some(checks) = &self.compiler_checks {
			checks.check()?;
		}
		Ok(())
	}

	/// The sole work/scratch ledger admits either expanded source or raw
	/// copies.
	fn reserve_compile(
		&self,
		bytes: Option<usize>,
		work: u64,
	) -> FerrosGrynnResult<CompileReservation<'_>> {
		if let Some(remaining) = &self.compiler_work {
			remaining
				.fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
					remaining.checked_sub(work)
				})
				.map_err(|_work| {
					FerrosGrynnError::traversal_resource_limit_exceeded("PURE compiler work", work)
				})?;
		}
		if !self.compiler_bounded {
			return Ok(CompileReservation {
				state: self,
				bytes: 0,
			});
		}
		let mut registry = self.registry.lock();
		let bytes = if let Some(limit) = registry.scratch_limit {
			let bytes = bytes.ok_or_else(|| compiler_resource(limit))?;
			let next = registry
				.compiling_bytes
				.checked_add(bytes)
				.and_then(|bytes| bytes.checked_add(registry.retained_bytes()))
				.ok_or_else(|| compiler_resource(limit))?;
			if next > limit {
				return Err(compiler_resource(limit));
			}
			registry.compiling_bytes += bytes;
			bytes
		} else {
			0
		};
		Ok(CompileReservation {
			state: self,
			bytes,
		})
	}

	/// Reports whether the value has entries for `LanguageClosureState`.
	pub(crate) fn has_entries(&self) -> bool {
		self.populated.load(Ordering::Acquire)
	}

	/// Identifies compiler frames with an existing embedding scratch ceiling.
	pub(in crate::gql) const fn is_compiler_bounded(&self) -> bool {
		self.compiler_bounded
	}

	/// Hands the compiler's unspent gas to a bounded embedding's runtime.
	pub(in crate::gql) fn remaining_compile_work(&self) -> Option<u64> {
		self.compiler_work.as_ref().map(|work| work.load(Ordering::Acquire))
	}

	/// Returns register for `LanguageClosureState`.
	pub(in crate::gql) fn register(
		&self,
		plan: &LogicalClosure,
		slots: &[Value],
		statement_nonce: u64,
	) -> FerrosGrynnResult<Closure> {
		let closure = self.registry.lock().register(plan, slots, statement_nonce)?;
		self.populated.store(true, Ordering::Release);
		Ok(closure)
	}

	/// Finishes statement for `LanguageClosureState`.
	pub(crate) fn finish_statement<'a>(
		&self,
		statement_nonce: u64,
		roots: impl IntoIterator<Item = &'a Value>,
	) {
		if !self.has_entries() {
			return;
		}
		let mut registry = self.registry.lock();
		registry.finish_statement(statement_nonce, roots);
		self.populated.store(!registry.entries.is_empty(), Ordering::Release);
	}

	/// Returns resolve for `LanguageClosureState`.
	pub(in crate::gql) fn resolve(
		&self,
		closure: &Closure,
	) -> FerrosGrynnResult<LanguageClosureEntry> {
		self.registry.lock().resolve(closure)
	}

	/// Resolves named for `LanguageClosureState`.
	pub(in crate::gql) fn resolve_named(
		&self,
		function: NamedFunctionKey,
	) -> FerrosGrynnResult<Arc<LogicalClosure>> {
		self.registry.lock().resolve_named(function)
	}

	/// Returns cached named for `LanguageClosureState`.
	pub(in crate::gql) fn cached_named(
		&self,
		function: NamedFunctionKey,
	) -> Option<Arc<LogicalClosure>> {
		self.registry.lock().named_entries.get(&function).cloned()
	}

	/// Returns cache named plan for `LanguageClosureState`.
	pub(in crate::gql) fn cache_named_plan(&self, plan: &LogicalClosure) -> FerrosGrynnResult<()> {
		if let Some(checks) = &self.compiler_checks {
			checks.check()?;
		}
		self.registry.lock().cache_named(plan)?;
		self.populated.store(true, Ordering::Release);
		Ok(())
	}

	/// Retains only compiled plans; the destination never inherits compiler
	/// checks, budgets or caller state from this temporary scope.
	pub(in crate::gql) fn promote_named_to(&self, target: &Self) -> FerrosGrynnResult<()> {
		if let Some(checks) = &self.compiler_checks {
			checks.check()?;
		}
		let registry = self.registry.lock();
		let mut destination = target.registry.lock();
		for plan in registry.named_entries.values() {
			if let Some(function) = plan.named_function
				&& destination.admit_named(function, plan.retained_plan_bytes)?
			{
				destination.named_entries.insert(function, Arc::clone(plan));
			}
		}
		target.populated.store(
			!destination.entries.is_empty() || !destination.named_entries.is_empty(),
			Ordering::Release,
		);
		Ok(())
	}

	/// Retained plans are charged by bounded embeddings before invocation.
	pub(crate) fn retained_bytes(&self) -> usize {
		self.registry.lock().retained_bytes()
	}

	/// Admission count for the immutable cache's transitive named-call keys.
	pub(in crate::gql) fn named_dependency_count(&self) -> usize {
		self.registry.lock().named_entries.len()
	}

	/// Captured identities for already compiled transitive named calls.
	pub(in crate::gql) fn named_dependencies(
		&self,
	) -> BTreeSet<(crate::datastore::FunctionId, crate::datastore::SchemaVersion)> {
		self.registry.lock().named_entries.keys().map(|key| (key.id, key.version)).collect()
	}
}

/// Releases a live compiler admission on success, error and cancellation.
pub(in crate::gql) struct CompileReservation<'a> {
	/// Compiler registry whose temporary admission is released when this guard
	/// drops.
	state: &'a LanguageClosureState,
	/// Number of bytes retained by the compilation reservation.
	bytes: usize,
}

impl Drop for CompileReservation<'_> {
	/// Runs cleanup for this `CompileReservation<'_>` before its fields are
	/// released.
	fn drop(&mut self) {
		if self.bytes != 0 {
			let mut registry = self.state.registry.lock();
			registry.compiling_bytes -= self.bytes;
		}
	}
}

/// Returns compiler resource for this module.
fn compiler_resource(limit: usize) -> FerrosGrynnError {
	FerrosGrynnError::traversal_resource_limit_exceeded("PURE compiler scratch", limit as u64)
}

#[cfg(test)]
/// Regression tests for closure behavior.
mod tests {
	use super::*;

	/// Verifies compiler frames share the cap and release on error.
	#[test]
	fn compiler_frames_share_the_cap_and_release_on_error() {
		use crate::test_assertions::AssertResult as _;

		let state = LanguageClosureState::bounded(65_536);
		let parent = state.admit_compile(16).assert_ok();
		assert!(state.admit_compile(17).is_err());
		let child = state.admit_compile(16).assert_ok();
		assert!(state.admit_compile(0).is_err());
		drop(child);
		drop(parent);
		assert_eq!(state.registry.lock().compiling_bytes, 0);
		let _replacement = state.admit_compile(48).assert_ok();
		assert!(state.admit_compile(usize::MAX).is_err());
	}
}

/// One immutable language closure and the lexical values captured at creation.
#[derive(Debug, Clone)]
pub(crate) struct LanguageClosureEntry {
	/// Shared logical closure plan retained for runtime evaluation.
	pub(in crate::gql) plan: Arc<LogicalClosure>,
	/// Owned values captured from the creating lexical environment.
	pub(in crate::gql) captures: Vec<(u32, Value)>,
	/// Statement identity separating runtime entries from different executions.
	statement_nonce: u64,
	/// Bytes charged for owned values retained by this entry or registry.
	retained_bytes: usize,
}

/// Concrete registry shared by executions of one persistent session.
#[derive(Debug)]
pub(crate) struct LanguageClosureRegistry {
	/// Runtime owner identity separating the retained closure entries.
	owner: RuntimeId,
	/// Owned entries retained for keyed lookup or iteration.
	entries: BTreeMap<RuntimeId, LanguageClosureEntry>,
	/// Bytes charged for owned values retained by this entry or registry.
	retained_bytes: usize,
	/// Catalog epoch for which the named-function cache is valid.
	named_epoch: Option<u64>,
	/// Named-function plans cached by their resolved schema keys.
	named_entries: BTreeMap<NamedFunctionKey, Arc<LogicalClosure>>,
	/// Bytes retained by cached named-function plans.
	named_bytes: usize,
	/// Optional memory limit applied before retaining temporary compilation
	/// state.
	scratch_limit: Option<usize>,
	/// Bytes currently reserved for closure compilation in progress.
	compiling_bytes: usize,
}

impl Default for LanguageClosureRegistry {
	/// Constructs the default initial state of `LanguageClosureRegistry`.
	fn default() -> Self {
		Self {
			owner: RuntimeId::next(),
			entries: BTreeMap::new(),
			retained_bytes: 0,
			named_epoch: None,
			named_entries: BTreeMap::new(),
			named_bytes: 0,
			scratch_limit: None,
			compiling_bytes: 0,
		}
	}
}

impl LanguageClosureRegistry {
	/// Returns register for `LanguageClosureRegistry`.
	pub(in crate::gql) fn register(
		&mut self,
		plan: &LogicalClosure,
		slots: &[Value],
		statement_nonce: u64,
	) -> FerrosGrynnResult<Closure> {
		self.cache_named(plan)?;
		let bytes = plan.captures.iter().try_fold(plan.retained_plan_bytes, |bytes, slot| {
			let value = slots.get(*slot as usize).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"Closure capture references an unavailable lexical slot.",
				)
			})?;
			bytes
				.checked_add(size_of::<(u32, Value)>())
				.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
				.ok_or_else(|| compiler_resource(self.scratch_limit.unwrap_or(MAX_RETAINED_BYTES)))
		})?;
		let retained = self.retained_bytes.saturating_add(self.named_bytes).saturating_add(bytes);
		let limit = self.scratch_limit.unwrap_or(MAX_RETAINED_BYTES);
		if retained.saturating_add(self.compiling_bytes) > limit {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"closure registry".to_owned(),
				limit as u64,
			));
		}
		let captures = plan
			.captures
			.iter()
			.map(|slot| {
				slots.get(*slot as usize).cloned().map(|value| (*slot, value)).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"Closure capture references an unavailable lexical slot.",
					)
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let closure = Closure::session(
			self.owner,
			plan.parameters.iter().map(|(_, ty)| ty.clone()).collect(),
			plan.result.clone(),
		);
		self.entries.insert(closure.id(), LanguageClosureEntry {
			plan: Arc::new(plan.clone()),
			captures,
			statement_nonce,
			retained_bytes: bytes,
		});
		self.retained_bytes = self.retained_bytes.saturating_add(bytes);
		Ok(closure)
	}

	/// Returns cache named for `LanguageClosureRegistry`.
	fn cache_named(&mut self, plan: &LogicalClosure) -> FerrosGrynnResult<()> {
		let Some(function) = plan.named_function else {
			return Ok(());
		};
		if self.admit_named(function, plan.retained_plan_bytes)? {
			self.named_entries.insert(function, Arc::new(plan.clone()));
		}
		Ok(())
	}

	/// Applies the same epoch and retained-byte authority to owned and shared
	/// plans before either insertion allocates.
	fn admit_named(&mut self, function: NamedFunctionKey, bytes: usize) -> FerrosGrynnResult<bool> {
		if self.named_epoch != Some(function.schema_epoch) {
			self.named_entries.clear();
			self.named_bytes = 0;
			self.named_epoch = Some(function.schema_epoch);
		}
		if self.named_entries.contains_key(&function) {
			return Ok(false);
		}
		let retained = self.retained_bytes.saturating_add(self.named_bytes).saturating_add(bytes);
		let limit = self.scratch_limit.unwrap_or(MAX_RETAINED_BYTES);
		if retained.saturating_add(self.compiling_bytes) > limit {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"named function cache".to_owned(),
				limit as u64,
			));
		}
		self.named_bytes = self.named_bytes.saturating_add(bytes);
		Ok(true)
	}

	/// Releases closures created by one statement unless its result or a
	/// persistent binding retains them, directly or through another closure.
	pub(crate) fn finish_statement<'a>(
		&mut self,
		statement_nonce: u64,
		roots: impl IntoIterator<Item = &'a Value>,
	) {
		if self.entries.is_empty() {
			return;
		}
		let mut retained = BTreeSet::new();
		for root in roots {
			root.collect_session_closure_ids(&mut retained);
		}
		let mut pending = retained.iter().copied().collect::<Vec<_>>();
		while let Some(id) = pending.pop() {
			let Some(entry) = self.entries.get(&id) else {
				continue;
			};
			let before = retained.len();
			for (_, capture) in &entry.captures {
				capture.collect_session_closure_ids(&mut retained);
			}
			if retained.len() != before {
				pending.extend(retained.iter().copied().filter(|id| {
					self.entries
						.get(id)
						.is_some_and(|entry| entry.statement_nonce == statement_nonce)
				}));
			}
		}
		let discarded = self
			.entries
			.iter()
			.filter_map(|(id, entry)| {
				(entry.statement_nonce == statement_nonce && !retained.contains(id)).then_some(*id)
			})
			.collect::<Vec<_>>();
		for id in discarded {
			if let Some(entry) = self.entries.remove(&id) {
				self.retained_bytes = self.retained_bytes.saturating_sub(entry.retained_bytes);
			}
		}
	}

	/// Returns resolve for `LanguageClosureRegistry`.
	pub(in crate::gql) fn resolve(
		&self,
		closure: &Closure,
	) -> FerrosGrynnResult<LanguageClosureEntry> {
		let owner = closure
			.session_owner()
			.ok_or_else(|| FerrosGrynnError::closure_requires_session(closure.id().get()))?;
		if owner != self.owner {
			return Err(FerrosGrynnError::closure_session_mismatch(
				closure.id().get(),
				owner.get(),
				self.owner.get(),
			));
		}
		self.entries
			.get(&closure.id())
			.cloned()
			.ok_or_else(|| FerrosGrynnError::closure_expired(closure.id().get()))
	}

	/// Resolves named for `LanguageClosureRegistry`.
	fn resolve_named(&self, function: NamedFunctionKey) -> FerrosGrynnResult<Arc<LogicalClosure>> {
		self.named_entries.get(&function).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"Compiled named function is unavailable for the current schema epoch.",
			)
		})
	}

	/// Returns retained bytes for `LanguageClosureRegistry`.
	fn retained_bytes(&self) -> usize {
		self.retained_bytes.saturating_add(self.named_bytes)
	}
}
