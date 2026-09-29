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
		atomic::{AtomicBool, Ordering},
	},
};

use parking_lot::Mutex;

use super::ir::{LogicalClosure, NamedFunctionKey};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Closure, RuntimeId, Value},
};

const MAX_RETAINED_BYTES: usize = 256 * 1024 * 1024;

/// Session-owned closure registry with a lock-free empty fast path.
#[derive(Debug, Default)]
pub(crate) struct LanguageClosureState {
	registry: Mutex<LanguageClosureRegistry>,
	populated: AtomicBool,
}

impl LanguageClosureState {
	pub(crate) fn has_entries(&self) -> bool {
		self.populated.load(Ordering::Acquire)
	}

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

	pub(in crate::gql) fn resolve(
		&self,
		closure: &Closure,
	) -> FerrosGrynnResult<LanguageClosureEntry> {
		self.registry.lock().resolve(closure)
	}

	pub(in crate::gql) fn resolve_named(
		&self,
		function: NamedFunctionKey,
	) -> FerrosGrynnResult<Arc<LogicalClosure>> {
		self.registry.lock().resolve_named(function)
	}

	pub(in crate::gql) fn cached_named(
		&self,
		function: NamedFunctionKey,
	) -> Option<Arc<LogicalClosure>> {
		self.registry.lock().named_entries.get(&function).cloned()
	}

	pub(in crate::gql) fn cache_named_plan(&self, plan: &LogicalClosure) -> FerrosGrynnResult<()> {
		self.registry.lock().cache_named(plan)?;
		self.populated.store(true, Ordering::Release);
		Ok(())
	}

	#[cfg(test)]
	pub(crate) fn retained_bytes(&self) -> usize {
		self.registry.lock().retained_bytes()
	}
}

/// One immutable language closure and the lexical values captured at creation.
#[derive(Debug, Clone)]
pub(crate) struct LanguageClosureEntry {
	pub(in crate::gql) plan: Arc<LogicalClosure>,
	pub(in crate::gql) captures: Vec<(u32, Value)>,
	statement_nonce: u64,
	retained_bytes: usize,
}

/// Concrete registry shared by executions of one persistent session.
#[derive(Debug)]
pub(crate) struct LanguageClosureRegistry {
	owner: RuntimeId,
	entries: BTreeMap<RuntimeId, LanguageClosureEntry>,
	retained_bytes: usize,
	named_epoch: Option<u64>,
	named_entries: BTreeMap<NamedFunctionKey, Arc<LogicalClosure>>,
	named_bytes: usize,
}

impl Default for LanguageClosureRegistry {
	fn default() -> Self {
		Self {
			owner: RuntimeId::next(),
			entries: BTreeMap::new(),
			retained_bytes: 0,
			named_epoch: None,
			named_entries: BTreeMap::new(),
			named_bytes: 0,
		}
	}
}

impl LanguageClosureRegistry {
	pub(in crate::gql) fn register(
		&mut self,
		plan: &LogicalClosure,
		slots: &[Value],
		statement_nonce: u64,
	) -> FerrosGrynnResult<Closure> {
		self.cache_named(plan)?;
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
		let bytes = plan.retained_plan_bytes.saturating_add(captures.iter().fold(
			0_usize,
			|bytes, (_, value)| {
				bytes
					.saturating_add(size_of::<(u32, Value)>())
					.saturating_add(value.estimated_heap_bytes())
			},
		));
		let retained = self.retained_bytes.saturating_add(self.named_bytes).saturating_add(bytes);
		if retained > MAX_RETAINED_BYTES {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"closure registry".to_owned(),
				MAX_RETAINED_BYTES as u64,
			));
		}
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

	fn cache_named(&mut self, plan: &LogicalClosure) -> FerrosGrynnResult<()> {
		let Some(function) = plan.named_function else {
			return Ok(());
		};
		if self.named_epoch != Some(function.schema_epoch) {
			self.named_entries.clear();
			self.named_bytes = 0;
			self.named_epoch = Some(function.schema_epoch);
		}
		if self.named_entries.contains_key(&function) {
			return Ok(());
		}
		let retained = self
			.retained_bytes
			.saturating_add(self.named_bytes)
			.saturating_add(plan.retained_plan_bytes);
		if retained > MAX_RETAINED_BYTES {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"named function cache".to_owned(),
				MAX_RETAINED_BYTES as u64,
			));
		}
		self.named_bytes = self.named_bytes.saturating_add(plan.retained_plan_bytes);
		self.named_entries.insert(function, Arc::new(plan.clone()));
		Ok(())
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

	fn resolve_named(&self, function: NamedFunctionKey) -> FerrosGrynnResult<Arc<LogicalClosure>> {
		self.named_entries.get(&function).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_argument(
				"Compiled named function is unavailable for the current schema epoch.",
			)
		})
	}

	#[cfg(test)]
	fn retained_bytes(&self) -> usize {
		self.retained_bytes.saturating_add(self.named_bytes)
	}
}
