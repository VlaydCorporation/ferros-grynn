//! Canonical qualified dirty changes pause incoming application and ACK.

use std::cell::RefCell;

use super::{
	Arc,
	BTreeMap,
	BindingAdmission,
	FerrosGrynnError,
	FerrosGrynnResult,
	LiveFamily,
	LiveGraph,
	LiveGraphImage,
	LiveGraphNext,
	LiveGraphSubscription,
	LiveNext,
	LiveOptions,
	LivePayload,
	Mutable,
	PendingConflict,
	PendingFlush,
	PropertyKeyId,
	Text,
	Versionstamp,
	restore_named,
};
use crate::live::{
	LiveGraphConflict,
	LiveGraphDelta,
	LiveGraphScopeId,
	LiveGraphSource,
	LiveGraphTarget,
};

/// Builds a bounded SDK property registry from the accepted logical image.
pub(super) fn names(
	image: &LiveGraphImage,
	options: &LiveOptions,
) -> FerrosGrynnResult<BTreeMap<PropertyKeyId, Text>> {
	let limits = super::super::LiveBindingOptions::default()
		.with_budgets(options.scratch_bytes(), options.work())?;
	let mut budget = BindingAdmission::new(&limits);
	names_admitted(image, &mut budget)
}

/// Copies names only after the shared subscription budget admits each entry.
fn names_admitted(
	image: &LiveGraphImage,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<BTreeMap<PropertyKeyId, Text>> {
	let mut result = BTreeMap::new();
	for (name, key) in &image.property_keys {
		budget.admit(256 + name.len(), 1)?;
		result.insert(*key, name.clone());
	}
	Ok(result)
}

/// Checks canonical qualified targets and scope-wide edits with bounded binary
/// searches.
fn overlaps(
	local: &LiveGraphTarget,
	incoming: &[LiveGraphTarget],
	admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<bool> {
	// Canonical targets put scopes before atoms; bounded binary searches avoid
	// fanout products.
	let comparisons = (usize::BITS - incoming.len().leading_zeros() + 1) as u64;
	admit(0, comparisons * 2)?;
	if incoming.binary_search(local).is_ok() {
		return Ok(true);
	}
	Ok(match local {
		LiveGraphTarget::Graph(scope) => incoming
			.binary_search_by(|target| match target {
				LiveGraphTarget::Graph(_) => std::cmp::Ordering::Less,
				LiveGraphTarget::Atom(atom) => atom.scope().cmp(scope),
			})
			.is_ok(),
		LiveGraphTarget::Atom(atom) => incoming
			.binary_search_by(|target| match target {
				LiveGraphTarget::Graph(scope) => scope.cmp(atom.scope()),
				LiveGraphTarget::Atom(_) => std::cmp::Ordering::Greater,
			})
			.is_ok(),
	})
}

/// Distinguishes graph invariant failures from resource and transport errors.
fn structural_conflict(error: &FerrosGrynnError) -> bool {
	use crate::gql_status::{Condition, Status};
	matches!(
		error.status().status(),
		Status::InvalidArgument { .. } | Status::InvalidStorageFile { .. }
	) || error.status().condition() == Condition::GraphTypeViolation
}

/// Accounts an additional owned target before copying its qualified route.
fn target_bytes(target: &LiveGraphTarget) -> FerrosGrynnResult<usize> {
	let heap = match target {
		LiveGraphTarget::Graph(scope) => scope.estimated_heap_bytes(),
		LiveGraphTarget::Atom(atom) => atom
			.scope()
			.estimated_heap_bytes()
			.checked_add(atom.atom().id().estimated_heap_bytes())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("conflict target bytes"))?,
	};
	heap.checked_add(512)
		.ok_or_else(|| FerrosGrynnError::operation_overflow("conflict target admission"))
}

impl<G: LiveFamily<State = Mutable, Mutable = G>> LiveGraphSubscription<Mutable, G> {
	/// Commits all mapped local changes through the caller's explicit
	/// transaction. The exact result is adopted by ordered polling; a
	/// successful result proves the source commit even if subsequent
	/// materialization or ACK fails.
	///
	/// # Errors
	/// Returns unresolved conflict, mapping, stale source, resource or commit
	/// errors before publication. Failed commits preserve every local decision.
	pub fn flush(
		&mut self,
		mut transaction: crate::datastore::Transaction,
	) -> FerrosGrynnResult<crate::datastore::CommitResult> {
		self.ensure_open()?;
		if self.pending.is_some() || self.unconfirmed.is_some() || self.flush.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"flush requires resolved conflicts and confirmed prior delivery",
			));
		}
		let budget = self.budget()?;
		let local = self.capture_local(&budget)?;
		let remote = self
			.remote
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("flush base absent"))?;
		let dirty = LiveGraphDelta::between(
			Some(Arc::clone(remote)),
			Arc::clone(&local),
			0.into(),
			1.into(),
			&|bytes, work| budget.borrow_mut().admit(bytes, work),
		)?;
		transaction.flush_live_graph(
			&self.subscription.inner,
			self.cursor(),
			remote,
			&local,
			dirty.changes(),
			&self.options,
		)?;
		let (committed, checkpoint) = transaction.commit_live_graph(self.cursor())?;
		self.flush = checkpoint.map(|checkpoint| PendingFlush {
			checkpoint,
			local,
		});
		Ok(committed)
	}

	/// Replays edits made after flush over its exact committed checkpoint.
	fn apply_pending_flush(&mut self) -> FerrosGrynnResult<Option<LiveGraphNext>> {
		let pending = self
			.flush
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("flush feedback absent"))?;
		let budget = self.budget()?;
		let local = self.capture_local(&budget)?;
		let post_flush = LiveGraphDelta::between(
			Some(Arc::clone(&pending.local)),
			Arc::clone(&local),
			0.into(),
			1.into(),
			&|bytes, work| budget.borrow_mut().admit(bytes, work),
		)?;
		let prepared = LiveGraphImage::merge_local(
			&pending.local,
			pending.checkpoint.delta().after(),
			&local,
			post_flush.changes(),
			&mut |bytes, work| budget.borrow_mut().admit(bytes, work),
		)
		.and_then(|image| {
			let candidate = self.prepare_candidate(&image, pending.checkpoint.versionstamp())?;
			Ok((Arc::new(image), candidate))
		});
		let (image, candidate) = match prepared {
			Ok(prepared) => prepared,
			Err(error) if !post_flush.changes().is_empty() && structural_conflict(&error) => {
				let remote = self.remote.as_ref().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("flush remote base absent")
				})?;
				let incoming = LiveGraphDelta::between(
					Some(Arc::clone(remote)),
					Arc::clone(&pending.checkpoint.delta.after),
					self.revision,
					pending.checkpoint.versionstamp(),
					&|bytes, work| budget.borrow_mut().admit(bytes, work),
				)?;
				for target in post_flush.changes() {
					budget.borrow_mut().admit(target_bytes(target)?, 1)?;
				}
				let conflict = Arc::new(LiveGraphConflict {
					incoming: Arc::new(incoming),
					local,
					conflicts: post_flush.changes().to_vec(),
					local_changes: post_flush.changes,
					diagnostic: Some(error),
				});
				self.pending = Some(PendingConflict {
					delivery: None,
					local_base: Arc::clone(&pending.local),
					conflict: Arc::clone(&conflict),
					reported: true,
				});
				return Ok(Some(LiveGraphNext::Conflict(conflict)));
			}
			Err(error) => return Err(error),
		};
		let names = self.merged_names(&image)?;
		self.graph = Some(LiveGraph::new_mutable(candidate));
		self.names = names;
		self.remote = Some(Arc::clone(&pending.checkpoint.delta.after));
		self.revision = pending.checkpoint.versionstamp();
		self.flush = None;
		Ok(None)
	}

	/// Resolves a property name for this local materialization's sparse key
	/// space.
	pub fn property_key(&self, name: &str) -> Option<PropertyKeyId> {
		self.names.iter().find_map(|(key, value)| (value.as_str() == name).then_some(*key))
	}

	/// Adds a bounded local key; source write-back resolves its name
	/// explicitly.
	///
	/// # Errors
	/// Returns key-space exhaustion or the existing subscription resource
	/// limit.
	pub fn intern_property<NameArg: Into<Text>>(
		&mut self,
		name: NameArg,
	) -> FerrosGrynnResult<PropertyKeyId> {
		let name = name.into();
		if let Some(key) = self.property_key(name.as_str()) {
			return Ok(key);
		}
		let mut raw = u32::MAX;
		while self.names.contains_key(&PropertyKeyId::new(raw)) {
			raw = raw.checked_sub(1).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("local property key space exhausted")
			})?;
		}
		let bytes = self
			.names
			.values()
			.try_fold(256 + name.len(), |bytes, name| bytes.checked_add(256 + name.len()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("local property names"))?;
		if bytes > self.options.scratch_bytes() {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"local property names",
				bytes as u64,
			));
		}
		let key = PropertyKeyId::new(raw);
		self.names.insert(key, name);
		Ok(key)
	}

	/// Captures one locked local revision and retains its accepted source
	/// mappings.
	fn capture_local(
		&self,
		budget: &RefCell<BindingAdmission>,
	) -> FerrosGrynnResult<Arc<LiveGraphImage>> {
		let remote = self
			.remote
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("mutable remote image absent"))?;
		let snapshot = self
			.materialization()?
			.static_snapshot_admitted(&mut |bytes, work| budget.borrow_mut().admit(bytes, work))?;
		self.capture_snapshot(snapshot, remote, budget)
	}

	/// Converts the admitted local snapshot using stable SDK property handles.
	fn capture_snapshot(
		&self,
		snapshot: G::Frozen,
		remote: &LiveGraphImage,
		budget: &RefCell<BindingAdmission>,
	) -> FerrosGrynnResult<Arc<LiveGraphImage>> {
		let graph = G::into_materialized(snapshot);
		let mut resolved = BTreeMap::new();
		for (key, name) in &self.names {
			if let Some(existing) = resolved.get(key) {
				if existing != name {
					return Err(FerrosGrynnError::invalid_argument(
						"local and remote property handles conflict",
					));
				}
			} else {
				budget.borrow_mut().admit(256 + name.len(), 1)?;
				resolved.insert(*key, name.clone());
			}
		}
		let mapping = |atom: &crate::graph::AtomRef,
		               graph: Option<&crate::graph::GraphRef>|
		 -> FerrosGrynnResult<Option<LiveGraphSource>> {
			let root = remote.scopes.get(&LiveGraphScopeId::Root);
			let key = graph.map_or(LiveGraphScopeId::Root, |graph| {
				if root
					.and_then(|scope| scope.source.as_ref())
					.is_some_and(|source| source.id() == graph.id())
				{
					LiveGraphScopeId::Root
				} else {
					LiveGraphScopeId::Source(graph.id())
				}
			});
			Ok(remote.scopes.get(&key).and_then(|scope| {
				scope
					.nodes
					.get(atom)
					.and_then(|node| node.source)
					.or_else(|| scope.edges.get(atom).and_then(|edge| edge.source))
			}))
		};
		let local = LiveGraphImage::capture_with_names(
			&graph,
			&resolved,
			&|bytes, work| budget.borrow_mut().admit(bytes, work),
			&mapping,
		)?;
		if local.family != remote.family
			|| local.validators != remote.validators
			|| local.root != remote.root
		{
			return Err(FerrosGrynnError::invalid_argument(
				"local mutation cannot alter the LIVE result family contract",
			));
		}
		Ok(Arc::new(local))
	}

	/// Creates the existing scratch/work admission authority for one SDK
	/// operation.
	fn budget(&self) -> FerrosGrynnResult<RefCell<BindingAdmission>> {
		Ok(RefCell::new(BindingAdmission::new(
			&super::super::LiveBindingOptions::default()
				.with_budgets(self.options.scratch_bytes(), self.options.work())?,
		)))
	}

	/// Restores and validates the selected family before changing SDK state.
	fn prepare_candidate(
		&self,
		image: &LiveGraphImage,
		revision: Versionstamp,
	) -> FerrosGrynnResult<G> {
		let names = self.merged_names(image)?;
		G::thaw_frozen(restore_named::<G::Frozen>(
			image,
			self.id(),
			revision,
			&self.options,
			self.remote.as_deref(),
			Some(&names),
		)?)
	}

	/// Validates the chosen image before changing its base or confirming
	/// delivery.
	fn resolve(
		&mut self,
		image: Arc<LiveGraphImage>,
		reset_flush_baseline: bool,
	) -> FerrosGrynnResult<LiveGraphNext> {
		self.ensure_open()?;
		let pending = self
			.pending
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("no pending graph conflict"))?;
		let revision = pending.conflict.remote_revision();
		let candidate = self.prepare_candidate(&image, revision)?;
		if let Some(delivery) = pending.delivery.clone() {
			return self.accept_prepared(&delivery, image, candidate, reset_flush_baseline);
		}
		let names = self.merged_names(&image)?;
		let remote = Arc::clone(&pending.conflict.incoming.after);
		let delivery = match self.subscription.next()? {
			LiveNext::Delivery(delivery) if matches!(delivery.payload(), LivePayload::Graph(batch) if batch.versionstamp() == revision) => {
				Some(delivery)
			}
			_ => None,
		};
		self.graph = Some(LiveGraph::new_mutable(candidate));
		self.names = names;
		self.remote = Some(remote);
		self.revision = revision;
		self.pending = None;
		self.flush = None;
		if let Some(delivery) = delivery {
			self.unconfirmed = Some(delivery.token().clone());
			self.subscription.ack(delivery.token())?;
			self.unconfirmed = None;
		}
		Ok(LiveGraphNext::Updated {
			revision,
		})
	}

	/// Publishes one checked candidate and then confirms its complete delivery
	/// token.
	fn accept_prepared(
		&mut self,
		delivery: &super::super::LiveDelivery,
		image: Arc<LiveGraphImage>,
		candidate: G,
		reset_flush_baseline: bool,
	) -> FerrosGrynnResult<LiveGraphNext> {
		self.ensure_open()?;
		let LivePayload::Graph(batch) = delivery.payload() else {
			return Err(FerrosGrynnError::invalid_argument(
				"graph resolution requires a graph delivery",
			));
		};
		let names = self.merged_names(&image)?;
		let flush_baseline = if let Some(flush) = &self.flush {
			if batch.versionstamp() < flush.checkpoint.versionstamp() {
				if reset_flush_baseline {
					Some(Arc::clone(&batch.delta.after))
				} else {
					let remote = self
						.remote
						.as_ref()
						.ok_or_else(|| FerrosGrynnError::invalid_argument("flush base absent"))?;
					let budget = self.budget()?;
					let changed = LiveGraphDelta::between(
						Some(Arc::clone(remote)),
						Arc::clone(&flush.local),
						0.into(),
						1.into(),
						&|bytes, work| budget.borrow_mut().admit(bytes, work),
					)?;
					Some(Arc::new(LiveGraphImage::merge_local(
						remote,
						batch.delta().after(),
						&flush.local,
						changed.changes(),
						&mut |bytes, work| budget.borrow_mut().admit(bytes, work),
					)?))
				}
			} else {
				None
			}
		} else {
			None
		};
		self.graph = Some(LiveGraph::new_mutable(candidate));
		self.names = names;
		self.remote = Some(Arc::clone(&batch.delta.after));
		self.revision = batch.versionstamp();
		self.pending = None;
		if let (Some(flush), Some(baseline)) = (&mut self.flush, flush_baseline) {
			flush.local = baseline;
		}
		self.unconfirmed = Some(delivery.token().clone());
		self.subscription.ack(delivery.token())?;
		self.unconfirmed = None;
		Ok(LiveGraphNext::Updated {
			revision: self.revision,
		})
	}

	/// One shared admission authority preserves the sparse local key registry.
	fn merged_names(
		&self,
		image: &LiveGraphImage,
	) -> FerrosGrynnResult<BTreeMap<PropertyKeyId, Text>> {
		let budget = self.budget()?;
		let mut names = BTreeMap::new();
		let mut assigned = BTreeMap::new();
		for (key, name) in &self.names {
			budget.borrow_mut().admit(512 + name.len(), 1)?;
			names.insert(*key, name.clone());
			assigned.insert(name.as_str(), *key);
		}
		for (name, preferred) in &image.property_keys {
			budget.borrow_mut().admit(0, 1)?;
			if assigned.contains_key(name.as_str()) {
				continue;
			}
			let mut key = *preferred;
			if names.contains_key(&key) {
				key = PropertyKeyId::new(u32::MAX);
			}
			while names.contains_key(&key) {
				budget.borrow_mut().admit(0, 1)?;
				key = PropertyKeyId::new(key.get().checked_sub(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("local property key space exhausted")
				})?);
			}
			budget.borrow_mut().admit(512 + name.len(), 1)?;
			names.insert(key, name.clone());
			assigned.insert(name.as_str(), key);
		}
		Ok(names)
	}

	/// Rejects resolution or write-back after the consumer has terminated.
	fn ensure_open(&mut self) -> FerrosGrynnResult<()> {
		if matches!(self.subscription.next()?, LiveNext::Closed) {
			return Err(FerrosGrynnError::invalid_argument("LIVE graph consumer is closed"));
		}
		Ok(())
	}

	/// Borrows pending conflict information without changing its ACK or
	/// decision.
	pub fn conflict(&self) -> Option<&LiveGraphConflict> {
		self.pending.as_ref().map(|pending| pending.conflict.as_ref())
	}

	/// Polls a whole delta; overlapping qualified targets suspend apply and
	/// ACK.
	///
	/// # Errors
	/// Returns graph, resource, source or acknowledgement errors without a
	/// partial candidate.
	#[allow(
		clippy::should_implement_trait,
		reason = "idle-aware fallible polling is not an Iterator"
	)]
	pub fn next(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		if let Some(token) = &self.unconfirmed {
			if matches!(self.subscription.next()?, LiveNext::Closed) {
				return Ok(LiveGraphNext::Closed);
			}
			self.subscription.ack(token)?;
			self.unconfirmed = None;
			return Ok(LiveGraphNext::Updated {
				revision: self.revision,
			});
		}
		if let Some(pending) = &mut self.pending {
			if !pending.reported {
				pending.reported = true;
				return Ok(LiveGraphNext::Conflict(Arc::clone(&pending.conflict)));
			}
			if matches!(self.subscription.next()?, LiveNext::Closed) {
				return Ok(LiveGraphNext::Closed);
			}
			return Ok(LiveGraphNext::Idle);
		}
		let delivery = match self.subscription.next()? {
			LiveNext::Delivery(delivery) => delivery,
			LiveNext::Closed => return Ok(LiveGraphNext::Closed),
			LiveNext::Idle => {
				if self.flush.is_some() {
					if let Some(event) = self.apply_pending_flush()? {
						return Ok(event);
					}
					return Ok(LiveGraphNext::Updated {
						revision: self.revision,
					});
				}
				return Ok(LiveGraphNext::Idle);
			}
		};
		let batch = match delivery.payload() {
			LivePayload::Gap(gap) => return Ok(LiveGraphNext::Gap(gap.clone())),
			LivePayload::Fault {
				diagnostic,
				gap,
			} => {
				return Ok(LiveGraphNext::Fault {
					diagnostic: diagnostic.clone(),
					gap: gap.clone(),
				});
			}
			LivePayload::Batch(_) => {
				return Err(FerrosGrynnError::invalid_storage_file(
					"mutable graph received row data",
				));
			}
			LivePayload::Graph(batch) => Arc::clone(batch),
		};
		if self
			.flush
			.as_ref()
			.is_some_and(|flush| batch.versionstamp() >= flush.checkpoint.versionstamp())
			&& let Some(event) = self.apply_pending_flush()?
		{
			return Ok(event);
		}
		if batch.versionstamp() <= self.revision {
			self.unconfirmed = Some(delivery.token().clone());
			self.subscription.ack(delivery.token())?;
			self.unconfirmed = None;
			return Ok(LiveGraphNext::Updated {
				revision: self.revision,
			});
		}
		if batch.delta().base() != self.revision {
			return Ok(self.gap(batch.delta().base()));
		}
		let budget = self.budget()?;
		let local = self.capture_local(&budget)?;
		let remote = self
			.remote
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("mutable base absent"))?;
		let dirty = LiveGraphDelta::between(
			Some(Arc::clone(remote)),
			Arc::clone(&local),
			0.into(),
			1.into(),
			&|bytes, work| budget.borrow_mut().admit(bytes, work),
		)?;
		for target in dirty.changes().iter().chain(batch.delta().changes()) {
			budget.borrow_mut().admit(target_bytes(target)?, 1)?;
		}
		let mut conflicts = Vec::new();
		for target in dirty.changes() {
			if overlaps(target, batch.delta().changes(), &mut |bytes, work| {
				budget.borrow_mut().admit(bytes, work)
			})? {
				conflicts.push(target.clone());
			}
		}
		let diagnostic = if conflicts.is_empty() {
			let prepared = LiveGraphImage::merge_local(
				remote,
				batch.delta().after(),
				&local,
				dirty.changes(),
				&mut |bytes, work| budget.borrow_mut().admit(bytes, work),
			)
			.and_then(|image| {
				let candidate = self.prepare_candidate(&image, batch.versionstamp())?;
				Ok((image, candidate))
			});
			match prepared {
				Ok((image, candidate)) => {
					return self.accept_prepared(&delivery, Arc::new(image), candidate, false);
				}
				Err(error) if !dirty.changes().is_empty() && structural_conflict(&error) => {
					conflicts.extend(dirty.changes().iter().cloned());
					Some(error)
				}
				Err(error) => return Err(error),
			}
		} else {
			None
		};
		if !conflicts.is_empty() {
			budget.borrow_mut().admit(
				conflicts
					.len()
					.checked_mul(512)
					.ok_or_else(|| FerrosGrynnError::operation_overflow("qualified conflicts"))?,
				conflicts.len() as u64,
			)?;
			let conflict = Arc::new(LiveGraphConflict {
				incoming: Arc::new(batch.delta.clone()),
				local,
				local_changes: dirty.changes,
				conflicts,
				diagnostic,
			});
			self.pending = Some(PendingConflict {
				delivery: Some(delivery),
				local_base: Arc::clone(remote),
				conflict: Arc::clone(&conflict),
				reported: true,
			});
			return Ok(LiveGraphNext::Conflict(conflict));
		}
		Err(FerrosGrynnError::invalid_argument("structural conflict has no local targets"))
	}

	/// Waits on one consumer; pending conflict waits only for lifecycle or
	/// commands.
	///
	/// # Errors
	/// Returns the same checked candidate or ACK errors as `next`.
	pub async fn next_event(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		loop {
			let event = self.next()?;
			if !matches!(event, LiveGraphNext::Idle) {
				return Ok(event);
			}
			self.subscription.inner.ready(self.pending.is_some()).await?;
		}
	}

	/// Accepts remote state and discards local changes after full validation.
	///
	/// # Errors
	/// Failed preparation preserves the conflict and former local state.
	pub fn accept_remote(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		self.ensure_open()?;
		let pending = self
			.pending
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("no pending graph conflict"))?;
		self.resolve(Arc::clone(&pending.conflict.incoming.after), true)
	}

	/// Discards local changes while accepting the complete pending remote
	/// delta.
	///
	/// # Errors
	/// Returns the same checked resolution errors as `accept_remote`.
	pub fn discard_local(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		self.accept_remote()
	}

	/// Retains local field decisions over the new remote base.
	///
	/// # Errors
	/// Incompatible topology/containment keeps the old state and conflict
	/// intact.
	pub fn keep_local(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		self.ensure_open()?;
		let pending = self
			.pending
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("no pending graph conflict"))?;
		let base = &pending.local_base;
		let budget = self.budget()?;
		let local = self.capture_local(&budget)?;
		let dirty = LiveGraphDelta::between(
			Some(Arc::clone(base)),
			Arc::clone(&local),
			0.into(),
			1.into(),
			&|bytes, work| budget.borrow_mut().admit(bytes, work),
		)?;
		let merged = LiveGraphImage::merge_local(
			base,
			pending.conflict.incoming.after(),
			&local,
			dirty.changes(),
			&mut |bytes, work| budget.borrow_mut().admit(bytes, work),
		)?;
		self.resolve(Arc::new(merged), false)
	}

	/// Builds an explicit merged candidate from the complete incoming image.
	///
	/// # Errors
	/// Callback/validator failure preserves the former local state and
	/// conflict.
	pub fn rebase<MergeArg: FnOnce(&mut G, &LiveGraphConflict) -> FerrosGrynnResult<()>>(
		&mut self,
		merge: MergeArg,
	) -> FerrosGrynnResult<LiveGraphNext> {
		self.ensure_open()?;
		let pending = self
			.pending
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("no pending graph conflict"))?;
		let mut candidate = self.prepare_candidate(
			pending.conflict.incoming.after(),
			pending.conflict.remote_revision(),
		)?;
		// Merge into a detached candidate so a failing callback cannot
		// partially overwrite local state.
		merge(&mut candidate, &pending.conflict)?;
		let working = LiveGraph::new_mutable(candidate);
		let budget = self.budget()?;
		let image = self.capture_snapshot(
			working.static_snapshot_admitted(&mut |bytes, work| {
				budget.borrow_mut().admit(bytes, work)
			})?,
			pending.conflict.incoming.after(),
			&budget,
		)?;
		self.resolve(image, true)
	}

	/// Resumes ordinary polling only after an explicit conflict resolution.
	///
	/// # Errors
	/// Returns a pending-conflict error instead of bypassing its ACK barrier.
	pub fn resume(&mut self) -> FerrosGrynnResult<LiveGraphNext> {
		if self.pending.is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"pending graph conflict requires explicit resolution",
			));
		}
		self.next()
	}
}

#[cfg(test)]
/// Regression tests for mutable behavior.
mod tests {
	use super::*;
	use crate::{
		graph::{AtomKind, AtomRef},
		live::LiveGraphAtomId,
	};

	/// Verifies structural conflict keeps resource errors as errors.
	#[test]
	fn structural_conflict_keeps_resource_errors_as_errors() {
		assert!(!structural_conflict(&FerrosGrynnError::traversal_resource_limit_exceeded(
			"work", 1
		)));
		assert!(structural_conflict(&FerrosGrynnError::graph_constraint_violation("dag", "cycle")));
	}

	/// Verifies qualified conflict search is bounded and checks scope changes.
	#[test]
	fn qualified_conflict_search_is_bounded_and_checks_scope_changes() {
		use crate::test_assertions::AssertResult as _;

		let scope = LiveGraphScopeId::Root;
		let targets = (0..20_000)
			.map(|id| {
				Ok(LiveGraphTarget::Atom(LiveGraphAtomId {
					scope: scope.clone(),
					atom: AtomRef::new(AtomKind::Node, format!("node:{id}").parse()?),
				}))
			})
			.collect::<FerrosGrynnResult<std::collections::BTreeSet<_>>>()
			.assert_ok()
			.into_iter()
			.collect::<Vec<_>>();
		let missing = LiveGraphTarget::Atom(LiveGraphAtomId {
			scope: scope.clone(),
			atom: AtomRef::new(AtomKind::Node, "other:missing".parse().assert_ok()),
		});
		let mut work = 0;
		let mut admit = |_, steps| {
			work += steps;
			Ok(())
		};
		assert!(!overlaps(&missing, &targets, &mut admit).assert_ok());
		assert!(overlaps(&targets[10_000], &targets, &mut admit).assert_ok());
		assert!(overlaps(&LiveGraphTarget::Graph(scope.clone()), &targets, &mut admit).assert_ok());
		assert!(overlaps(&missing, &[LiveGraphTarget::Graph(scope)], &mut admit).assert_ok());
		assert!(work < 128);
		assert!(
			overlaps(&missing, &targets, &mut |_, _| Err(
				FerrosGrynnError::traversal_resource_limit_exceeded("test work", 1)
			))
			.is_err()
		);
	}
}
