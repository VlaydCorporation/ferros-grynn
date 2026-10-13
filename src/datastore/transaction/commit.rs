use std::{
	collections::{BTreeMap, BTreeSet},
	sync::{Arc, atomic::Ordering},
	time::Instant,
};

use super::{CommitResult, Mutation, Transaction};
use crate::{
	datastore::{
		ConflictKey,
		DatabaseRoot,
		IsolationLevel,
		MaintenanceTarget,
		consts::EFFECTIVE_FANOUT_WRITE_BYTES,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, EdgeEndpoint, EdgeTopology, GraphId, QualifiedAtomId, remap_topology},
};

/// Commit-local address translation for allocations made from an older
/// immutable root. Logical conflict checks run before this map is created.
#[derive(Default)]
pub(super) struct ReplayRemap {
	globals: BTreeMap<super::GlobalAtomId, super::GlobalAtomId>,
	locals: BTreeMap<(GraphId, AtomId), AtomId>,
}

impl ReplayRemap {
	pub(super) fn global(&self, id: super::GlobalAtomId) -> super::GlobalAtomId {
		self.globals.get(&id).copied().unwrap_or(id)
	}

	fn local(&self, graph: GraphId, id: AtomId) -> AtomId {
		self.locals.get(&(graph, id)).copied().unwrap_or(id)
	}

	fn conflict(&self, key: ConflictKey) -> ConflictKey {
		match key {
			ConflictKey::BaseAtom(global) => ConflictKey::BaseAtom(self.global(global)),
			ConflictKey::GraphMembership(graph, global) => {
				ConflictKey::GraphMembership(graph, self.global(global))
			}
			ConflictKey::GraphOverlay(graph, global) => {
				ConflictKey::GraphOverlay(graph, self.global(global))
			}
			ConflictKey::GraphIncidence(graph, local) => {
				ConflictKey::GraphIncidence(graph, self.local(graph, local))
			}
			other => other,
		}
	}

	/// Builds the logical qualified topology used for exact mutation images.
	fn topology(
		&self,
		root: &DatabaseRoot,
		graph: GraphId,
		topology: &EdgeTopology,
	) -> FerrosGrynnResult<EdgeTopology> {
		if topology.is_cross_level() {
			return topology.try_map_endpoints(|endpoint| match endpoint {
				EdgeEndpoint::Local(local) => Ok(EdgeEndpoint::Local(self.local(graph, *local))),
				EdgeEndpoint::Nested(qualified) => {
					if qualified.scope().len() > 64 {
						return Err(FerrosGrynnError::invalid_argument(
							"edge endpoint scope exceeds 64 levels",
						));
					}
					let mut current = graph;
					let mut scope = Vec::with_capacity(qualified.scope().len());
					for owner in qualified.scope() {
						let actual = self.local(current, *owner);
						scope.push(actual);
						current = root
							.occurrence(current, actual)
							.and_then(|owner| owner.child_graph())
							.ok_or_else(|| {
								FerrosGrynnError::invalid_storage_file(
									"replayed qualified owner is absent or has no graph",
								)
							})?;
					}
					Ok(EdgeEndpoint::Nested(QualifiedAtomId::new(
						scope,
						self.local(current, qualified.atom()),
					)?))
				}
			});
		}
		let ids = topology
			.endpoints()
			.filter_map(|endpoint| match endpoint {
				EdgeEndpoint::Local(local) => Some(*local),
				EdgeEndpoint::Nested(qualified) => qualified.scope().first().copied(),
			})
			.map(|local| (local, self.local(graph, local)))
			.collect();
		remap_topology(topology, &ids)
	}

	fn occurrence_state(
		&self,
		root: &DatabaseRoot,
		graph: GraphId,
		state: &super::OccurrenceState,
	) -> FerrosGrynnResult<super::OccurrenceState> {
		Ok(match state {
			super::OccurrenceState::Node {
				nested_graph,
			} => super::OccurrenceState::Node {
				nested_graph: *nested_graph,
			},
			super::OccurrenceState::Edge {
				topology,
				weight,
				transition_graph,
			} => super::OccurrenceState::Edge {
				topology: self.topology(root, graph, topology)?,
				weight: *weight,
				transition_graph: *transition_graph,
			},
		})
	}
}

/// Complete replay image retained until the existing publication barrier.
struct PreparedCommit {
	/// Validated candidate containing final primary/catalog/history state.
	root: DatabaseRoot,
	/// One translation shared by replay, versions and index deltas.
	remap: ReplayRemap,
	/// The existing durable whole-commit clock.
	commit_ts: u64,
	/// Deferred definitions to enqueue after successful publication.
	stale_indexes: Vec<(
		crate::datastore::ClassId,
		crate::datastore::IndexId,
		crate::datastore::SchemaVersion,
	)>,
}

/// Optional SDK feedback retains only the accepted graph, never a primary root.
struct GraphFeedback {
	/// Checked subscription incarnation requested by this commit.
	target: Option<(crate::live::LiveId, crate::live::LiveVersion)>,
	/// Exact checkpoint from the candidate which passed the durable barrier.
	checkpoint: Option<Arc<crate::live::LiveGraphBatch>>,
}

impl GraphFeedback {
	/// Copies the already admitted checkpoint before physical publication.
	fn capture(&mut self, root: &DatabaseRoot) {
		let Some((id, version)) = self.target else {
			return;
		};
		self.checkpoint = root
			.live_catalog()
			.queries
			.get(&id)
			.filter(|definition| {
				definition.version == version && definition.state == crate::live::LiveState::Active
			})
			.and_then(|_| root.live_catalog().graph_checkpoints.get(&id))
			.cloned();
	}
}

impl Transaction {
	/// Registration supplies no new inputs to existing NOW queries without
	/// bindings.
	fn registration_query_targets(
		&self,
		before: &DatabaseRoot,
		next: &DatabaseRoot,
	) -> super::LiveQueryTargets {
		let only_registration = !self.mutations.is_empty()
			&& self.mutations.iter().all(|mutation| matches!(mutation, Mutation::RegisterLive(_)))
			&& self.change_journal.intents.is_empty()
			&& self.change_journal.operations.is_empty();
		if !only_registration
			|| !before.live_catalog().bindings.is_empty()
			|| !next.live_catalog().bindings.is_empty()
			|| before
				.live_catalog()
				.queries
				.values()
				.any(|entry| !matches!(entry.options.since(), crate::live::LiveSince::Now))
		{
			return super::LiveQueryTargets::All;
		}
		// Revision stamps are installed after preparation, so compare actual
		// inputs.
		let unchanged = Arc::ptr_eq(&before.schema, &next.schema)
			&& Arc::ptr_eq(&before.dictionary, &next.dictionary)
			&& Arc::ptr_eq(&before.atoms, &next.atoms)
			&& Arc::ptr_eq(&before.indexes, &next.indexes)
			&& Arc::ptr_eq(&before.statistics, &next.statistics)
			&& Arc::ptr_eq(before.changefeeds(), next.changefeeds())
			&& before.graphs.len() == next.graphs.len()
			&& before.graphs.iter().zip(&next.graphs).all(
				|((left_id, left), (right_id, right))| {
					left_id == right_id && Arc::ptr_eq(left, right)
				},
			);
		if unchanged {
			super::LiveQueryTargets::Pending
		} else {
			super::LiveQueryTargets::All
		}
	}

	/// Revalidates the complete private root before the normal atomic commit.
	///
	/// # Errors
	///
	/// Returns a graph-family, normalized-state, conflict, persistence, or
	/// publication failure without publishing a partial root.
	pub fn commit_with_validation(self) -> FerrosGrynnResult<CommitResult> {
		self.commit_with_validation_controlled(None, &|| Ok(()))
	}

	/// Validates the private root with cooperative publication controls.
	///
	/// # Errors
	///
	/// Returns validation, cancellation, deadline or publication failures.
	pub fn commit_with_validation_options(
		self,
		options: &crate::datastore::CommitOptions,
	) -> FerrosGrynnResult<CommitResult> {
		self.commit_with_validation_controlled(options.deadline, &|| options.check())
	}

	/// Shares the query's existing controls without starting a new deadline.
	pub(crate) fn commit_with_validation_controlled(
		mut self,
		deadline: Option<Instant>,
		check: &impl Fn() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<CommitResult> {
		check()?;
		let graphs = self
			.working
			.schema
			.all_graphs()
			.map(|graph| graph.reference().clone())
			.collect::<Vec<_>>();
		for graph in &graphs {
			check()?;
			self.validate_graph_family(graph)?;
		}
		self.working.validate_full_import_state()?;
		self.commit_controlled(deadline, check)
	}

	/// Validates and atomically publishes all transaction-private mutations.
	///
	/// # Errors
	///
	/// Returns a snapshot-age, FCW/SSI serialization, replay, or fanout budget
	/// error without publishing a partial root.
	pub fn commit(self) -> FerrosGrynnResult<CommitResult> {
		self.commit_controlled(None, &|| Ok(()))
	}

	/// Publishes with cooperative cancellation and an absolute deadline.
	/// LIVE BLOCK uses the earlier of this deadline and its own timeout.
	///
	/// # Errors
	///
	/// Returns commit failures or cancellation/deadline errors before the
	/// durable barrier; unsuccessful preparation publishes no queue state.
	pub fn commit_with_options(
		self,
		options: &crate::datastore::CommitOptions,
	) -> FerrosGrynnResult<CommitResult> {
		self.commit_controlled(options.deadline, &|| options.check())
	}

	/// Preserves the statement's control boundary through queue admission.
	pub(crate) fn commit_controlled(
		self,
		deadline: Option<Instant>,
		check: &impl Fn() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<CommitResult> {
		self.commit_with_graph_feedback(deadline, check, &mut GraphFeedback {
			target: None,
			checkpoint: None,
		})
	}

	/// Returns a source commit and its exact accepted graph checkpoint.
	pub(crate) fn commit_live_graph(
		self,
		cursor: &crate::live::LiveCursor,
	) -> FerrosGrynnResult<(CommitResult, Option<Arc<crate::live::LiveGraphBatch>>)> {
		let mut feedback = GraphFeedback {
			target: Some((cursor.live_id(), cursor.version())),
			checkpoint: None,
		};
		let committed = self.commit_with_graph_feedback(None, &|| Ok(()), &mut feedback)?;
		Ok((committed, feedback.checkpoint))
	}

	/// Ordinary and graph SDK commits share validation and publication
	/// unchanged.
	fn commit_with_graph_feedback(
		self,
		deadline: Option<Instant>,
		check: &impl Fn() -> FerrosGrynnResult<()>,
		feedback: &mut GraphFeedback,
	) -> FerrosGrynnResult<CommitResult> {
		check()?;
		self.ensure_live()?;
		// Only mutations or targeted intents can publish, including a FULL
		// policy enabled concurrently. Their BLOCK origin remains unchanged.
		let could_publish = !self.mutations.is_empty() || !self.change_journal.intents.is_empty();
		let live_started = could_publish.then(Instant::now);
		let store = self.store.clone();
		let _notification = store.inner.live_consumers.notification();
		if !could_publish {
			check()?;
			let _commit = store.inner.commit.lock();
			let current = store.inner.current_root();
			self.validate_commit_snapshot(&current)?;
			check()?;
			feedback.capture(&current);
			return Ok(CommitResult {
				transaction: self.id,
				commit_ts: current.commit_ts,
			});
		}
		if !self.mutations.is_empty() || self.has_full_capture(&self.working) {
			self.ensure_commit_write_allowed()?;
		}
		let defines_bindings = self.mutations.iter().any(|mutation| {
			matches!(
				mutation,
				Mutation::DefineLiveBinding(_)
					| Mutation::ReplaceLiveBinding { .. }
					| Mutation::RegisterLive(_)
			)
		});
		for _attempt in 0..8 {
			check()?;
			let observed = store.inner.current_root();
			let writes = !self.mutations.is_empty() || self.has_full_capture(&observed);
			if writes
				&& (defines_bindings
					|| !observed.live_catalog().bindings.is_empty()
					|| !observed.live_catalog().queries.is_empty())
			{
				self.validate_commit_snapshot(&observed)?;
				// User expression evaluation borrows only this immutable
				// candidate and finishes before attempting the
				// publication mutex.
				let historical = self.mutations.iter().any(|mutation| {
					matches!(mutation, Mutation::RegisterLive(definition)
						if !matches!(definition.options.since(), crate::live::LiveSince::Now))
				});
				let history_pin = if historical {
					let _commit = store.inner.commit.lock();
					if !Arc::ptr_eq(&observed, &store.inner.current_root()) {
						continue;
					}
					store.inner.capture_changefeed_snapshot(observed.commit_ts)?
				} else {
					None
				};
				let mut prepared = self.prepare_commit_root(&observed, history_pin)?;
				let queues = store.inner.live_consumers.reserve(
					&prepared.root,
					live_started.unwrap_or_else(Instant::now),
					deadline,
					&|| {
						check()?;
						self.ensure_live()
					},
				)?;
				let _commit = store.inner.commit.lock();
				let current = store.inner.current_root();
				if !Arc::ptr_eq(&observed, &current) || !queues.valid() {
					continue;
				}
				self.validate_commit_snapshot(&current)?;
				check()?;
				prepared.root.advance_identity_clocks(
					store.inner.next_schema_id.load(Ordering::Relaxed),
					store.inner.next_graph_id.load(Ordering::Relaxed),
				);
				return self.publish_commit_root(&current, prepared, Some(queues), feedback);
			}
			let _commit = store.inner.commit.lock();
			let current = store.inner.current_root();
			let writes = !self.mutations.is_empty() || self.has_full_capture(&current);
			// A concurrent registration may have enabled evaluation between the
			// fast-path inspection and lock acquisition.
			if writes
				&& (defines_bindings
					|| !current.live_catalog().bindings.is_empty()
					|| !current.live_catalog().queries.is_empty())
			{
				continue;
			}
			self.validate_commit_snapshot(&current)?;
			check()?;
			if !writes {
				feedback.capture(&current);
				return Ok(CommitResult {
					transaction: self.id,
					commit_ts: current.commit_ts,
				});
			}
			let prepared = self.prepare_commit_root(&current, None)?;
			check()?;
			return self.publish_commit_root(&current, prepared, None, feedback);
		}
		Err(FerrosGrynnError::serialization_failure(
			self.id,
			"the LIVE candidate root changed during bounded commit preparation",
		))
	}

	/// Rechecks ordinary writer lease, FCW/SSI and fanout against one root.
	fn validate_commit_snapshot(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		self.ensure_live()?;
		self.validate_gql_live_acks()?;
		if !self.mutations.is_empty() || self.has_full_capture(current) {
			self.ensure_commit_write_allowed()?;
		}
		self.validate_against(current)?;
		self.validate_commit_fanout(current)
	}

	/// Publishes one rechecked image using the existing WAL barrier and clock.
	fn publish_commit_root(
		&self,
		current: &Arc<DatabaseRoot>,
		prepared: PreparedCommit,
		queues: Option<crate::datastore::live_queue::Reservations<'_>>,
		feedback: &mut GraphFeedback,
	) -> FerrosGrynnResult<CommitResult> {
		let store = &self.store;
		let PreparedCommit {
			root: mut next,
			remap,
			commit_ts,
			stale_indexes,
		} = prepared;
		for key in self.writes.keys() {
			next.mark_version(remap.conflict(*key), commit_ts);
		}
		for key in &self.range_writes {
			next.mark_version(*key, commit_ts);
		}
		if self.mutations.iter().any(|mutation| {
			matches!(mutation, Mutation::EnqueueEvent(_) | Mutation::TransitionEvent { .. })
		}) {
			next.retain_event_revisions();
		}
		let graph_catalog_only = self.mutations.iter().all(|mutation| {
			matches!(
				mutation,
				Mutation::InternProperty { .. }
					| Mutation::DefineLiveBinding(_)
					| Mutation::ReplaceLiveBinding { .. }
					| Mutation::RemoveLiveBinding(_)
					| Mutation::ConfigureChangefeed { .. }
					| Mutation::RegisterLive(_)
					| Mutation::CloseLive(_)
					| Mutation::AcknowledgeLive(_)
					| Mutation::ReplaceGraph(_)
					| Mutation::DefineParameter(_)
					| Mutation::ReplaceParameter(_)
					| Mutation::RemoveParameter(_)
					| Mutation::DefineFunction(_)
					| Mutation::DefineEvent(_)
					| Mutation::ReplaceEvent(_)
					| Mutation::RemoveEvent(_)
					| Mutation::EnqueueEvent(_)
					| Mutation::TransitionEvent { .. }
					| Mutation::ReplaceFunction(_)
					| Mutation::RemoveFunction(_)
					| Mutation::DefineAnalyzer(_)
					| Mutation::ReplaceAnalyzer(_)
					| Mutation::RemoveAnalyzer(_)
			)
		}) && self.mutations.iter().any(|mutation| {
			matches!(
				mutation,
				Mutation::ReplaceGraph(_)
					| Mutation::DefineLiveBinding(_)
					| Mutation::ReplaceLiveBinding { .. }
					| Mutation::RemoveLiveBinding(_)
					| Mutation::ConfigureChangefeed { .. }
					| Mutation::RegisterLive(_)
					| Mutation::CloseLive(_)
					| Mutation::AcknowledgeLive(_)
					| Mutation::DefineParameter(_)
					| Mutation::ReplaceParameter(_)
					| Mutation::RemoveParameter(_)
					| Mutation::DefineFunction(_)
					| Mutation::DefineEvent(_)
					| Mutation::ReplaceEvent(_)
					| Mutation::RemoveEvent(_)
					| Mutation::EnqueueEvent(_)
					| Mutation::TransitionEvent { .. }
					| Mutation::ReplaceFunction(_)
					| Mutation::RemoveFunction(_)
					| Mutation::DefineAnalyzer(_)
					| Mutation::ReplaceAnalyzer(_)
					| Mutation::RemoveAnalyzer(_)
			)
		});
		let dictionary_changed = self
			.mutations
			.iter()
			.any(|mutation| matches!(mutation, Mutation::InternProperty { .. }));
		let dependency_indexes_changed = self.mutations.iter().any(|mutation| {
			matches!(mutation, Mutation::ReplaceAnalyzer(_) | Mutation::ReplaceFunction(_))
		});
		let changed_records = self.index_delta_records(current, &next, &remap);
		let delta_eligible = self.mutations.iter().all(mutation_allows_physical_index_delta);
		feedback.capture(&next);
		let mut published = Arc::new(next);
		let next = Arc::get_mut(&mut published).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("unpublished commit root is shared")
		})?;
		store.inner.persist_root(
			next,
			Some(current),
			delta_eligible.then_some(&changed_records),
			graph_catalog_only,
			dictionary_changed,
		)?;
		*store.inner.current.write() = published;
		if self.mutations.iter().any(|mutation| {
			matches!(mutation, Mutation::EnqueueEvent(_) | Mutation::TransitionEvent { .. })
		}) {
			store.inner.maintenance.wake_events();
		}
		if let Some(queues) = queues {
			queues.publish();
		}
		if dependency_indexes_changed {
			// The Stale catalog state is the correctness boundary. A ledger I/O
			// failure may delay availability, but cannot expose an incompatible
			// physical binding to query planning.
			let _job = store.inner.maintenance.recover_stale_indexes(&store.inner);
		}
		for (class, index, version) in stale_indexes {
			// Availability may lag when the durable job ledger itself is
			// unavailable, but the published Stale state keeps query results
			// exact.
			let _job = store.record_deferred_index_change(
				MaintenanceTarget::Index {
					class,
					index,
					version,
				},
				commit_ts,
			);
		}
		Ok(CommitResult {
			transaction: self.id,
			commit_ts,
		})
	}

	/// Builds the final immutable image separately from physical publication.
	fn prepare_commit_root(
		&self,
		current: &Arc<DatabaseRoot>,
		history_pin: Option<crate::storage::LocalChangefeedSnapshot>,
	) -> FerrosGrynnResult<PreparedCommit> {
		let commit_ts = current.commit_ts.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::serialization_failure(self.id, "the commit clock is exhausted")
		})?;
		// Primary and operation-image replay share one hydrated before-root.
		// The published root may still hold only lazy statistics headers.
		let mut before = current.as_ref().clone();
		if !self.is_control_commit() {
			self.store.inner.hydrate_statistics(&mut before)?;
		}
		let mut next = before.clone();
		let affected_graphs = self.affected_graphs(current);
		let remap = Self::apply_mutations(&mut next, &self.mutations, commit_ts, true).map_err(
			|error| {
				FerrosGrynnError::serialization_failure(
					self.id,
					"a transaction prerequisite changed before commit",
				)
				.with_cause(error)
			},
		)?;
		let stale_indexes = if self.mutations.iter().any(mutation_affects_explicit_indexes) {
			next.mark_deferred_indexes_stale()?
		} else {
			Vec::new()
		};
		next.advance_identity_clocks(
			self.store.inner.next_schema_id.load(Ordering::Relaxed),
			self.store.inner.next_graph_id.load(Ordering::Relaxed),
		);
		let validation_root = Arc::new(next.clone());
		for graph in affected_graphs {
			let Some(schema) = validation_root.schema.graph(graph) else {
				continue;
			};
			self.validate_graph_family_in_root(validation_root.clone(), schema.reference())?;
		}
		drop(validation_root);
		self.validate_pending_qualified_incidence(&next)?;
		if self.mutations.iter().any(|mutation| {
			matches!(
				mutation,
				Mutation::Detach { .. }
					| Mutation::DeleteAtom { .. }
					| Mutation::RemoveGraph(_)
					| Mutation::ReplaceOccurrenceState { .. }
					| Mutation::InstallBuiltGraphTree(_)
			)
		}) {
			next.validate_all_qualified_incidence()?;
			next.validate_preserved_qualified_incidence(&before, |graph, connector|
				before.atom(connector.global_id()).is_some_and(|atom| self.has_qualified_anchor(graph, atom.record_id())) ||
				self.mutations.iter().any(|mutation| matches!(mutation, Mutation::InstallBuiltGraphTree(tree) if tree.graph_ids().any(|built| built == graph))))?;
		}
		self.validate_full_noops(current, &next)?;
		next.commit_ts = commit_ts;
		next.advance_commit_time(jiff::Timestamp::now().as_nanosecond());
		let event_time = next.committed_at_ns;
		for mutation in &self.mutations {
			let id = match mutation {
				Mutation::EnqueueEvent(invocation) => invocation.id,
				Mutation::TransitionEvent {
					id,
					..
				} => *id,
				_ => continue,
			};
			Arc::make_mut(next.event_outbox_mut()).stamp_time(id, commit_ts, event_time)?;
		}
		self.capture_changes(&before, &mut next, commit_ts)?;
		if self.mutations.iter().any(|mutation| matches!(mutation, Mutation::RegisterLive(_))) {
			for definition in next
				.live_catalog()
				.queries
				.values()
				.filter(|entry| entry.state == crate::live::LiveState::Pending)
			{
				super::live::validate_source(&next, definition.query.source())?;
				if super::live::source_class(&next, definition.query.source())?
					!= definition.source_class
				{
					return Err(FerrosGrynnError::invalid_argument(
						"LIVE source owner changed before activation",
					));
				}
			}
			Arc::make_mut(next.live_catalog_mut()).activate(commit_ts)?;
			super::live_backfill::prepare(
				&self.store.inner,
				current,
				&mut next,
				history_pin,
				self.query_scratch_root(),
			)?;
		}
		if self.is_control_commit() {
			let catalog = Arc::make_mut(next.live_catalog_mut());
			catalog.prepared = Arc::default();
			catalog.prepared_encoded = Arc::default();
			catalog.prepared_graphs = Arc::default();
			crate::datastore::live_history::prepare(&mut next)?;
		} else {
			let targets = self.registration_query_targets(&before, &next);
			super::prepare_live_publication(
				current,
				&mut next,
				self.query_scratch_root(),
				super::LiveHistoryCapture::Enabled,
				targets,
				&self.store.inner.live_observation,
			)?;
		}
		Ok(PreparedCommit {
			root: next,
			remap,
			commit_ts,
			stale_indexes,
		})
	}

	fn affected_graphs(&self, current: &DatabaseRoot) -> BTreeSet<GraphId> {
		let mut graphs = BTreeSet::new();
		for mutation in &self.mutations {
			match mutation {
				Mutation::DefineGraph(schema) | Mutation::ReplaceGraph(schema) => {
					graphs.insert(schema.id());
				}
				Mutation::InstallBuiltGraphTree(build) => {
					graphs.extend(build.graph_ids());
					graphs.extend(current.schema.all_graphs().map(|schema| schema.id()));
					graphs.extend(self.working.schema.all_graphs().map(|schema| schema.id()));
				}
				Mutation::AttachNode {
					graph,
					..
				}
				| Mutation::AttachEdge {
					graph,
					..
				}
				| Mutation::Detach {
					graph,
					..
				}
				| Mutation::ReplaceOccurrenceState {
					graph,
					..
				} => {
					graphs.insert(*graph);
				}
				Mutation::DeleteAtom {
					global,
				} => {
					for root in [current, &self.working] {
						if let Some(memberships) = root.memberships(*global) {
							graphs.extend(memberships.keys().copied());
						}
					}
				}
				_ => {}
			}
		}
		graphs
	}

	fn index_delta_records(
		&self,
		current: &DatabaseRoot,
		next: &DatabaseRoot,
		remap: &ReplayRemap,
	) -> BTreeSet<crate::values::RecordId> {
		self.writes
			.keys()
			.filter_map(|key| match remap.conflict(*key) {
				ConflictKey::BaseAtom(global)
				| ConflictKey::GraphMembership(_, global)
				| ConflictKey::GraphOverlay(_, global) => current
					.atom(global)
					.or_else(|| next.atom(global))
					.map(|atom| atom.record_id().clone()),
				_ => None,
			})
			.collect()
	}

	fn validate_against(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		self.validate_write_requirements(current)?;
		if self.options.isolation == IsolationLevel::Serializable
			&& (!current.schema.events.is_empty() || !self.snapshot.schema.events.is_empty())
			&& (!self.change_journal.intents.is_empty()
				|| self.mutations.iter().any(|mutation| {
					matches!(
						mutation,
						Mutation::RemoveGraph(_)
							| Mutation::RemoveClass(_)
							| Mutation::InstallBuiltGraphTree(_)
					)
				}))
			&& current.version(ConflictKey::EventCatalogRange)
				!= self.snapshot.version(ConflictKey::EventCatalogRange)
		{
			return Err(FerrosGrynnError::serialization_failure(
				self.id,
				"EVENT definitions changed before source publication",
			));
		}
		if self.options.isolation == IsolationLevel::Serializable
			&& (!self.mutations.is_empty() || self.has_full_capture(current))
		{
			for (key, observed) in self.reads.lock().iter() {
				if current.version(*key) != *observed
					&& !self.is_private_allocation_collision(current, *key)
				{
					return Err(FerrosGrynnError::serialization_failure(
						self.id,
						"a logical value read from the snapshot was concurrently changed",
					));
				}
			}
			for record in self.missing_records.lock().iter() {
				if current.resolve_atom(record).is_some() {
					return Err(FerrosGrynnError::serialization_failure(
						self.id,
						"a previously absent RecordId was concurrently created",
					));
				}
			}
		}
		Ok(())
	}

	pub(super) fn validate_write_requirements(
		&self,
		current: &DatabaseRoot,
	) -> FerrosGrynnResult<()> {
		for (key, observed) in self.writes.iter().chain(&self.requirements) {
			if current.version(*key) != *observed
				&& !self.is_private_allocation_collision(current, *key)
			{
				return Err(FerrosGrynnError::serialization_failure(
					self.id,
					"a logical write target or structural prerequisite changed",
				));
			}
		}
		Ok(())
	}

	/// Distinguishes a stale physical address chosen by a private root from a
	/// real logical conflict. The latter remains a serialization failure.
	pub(super) fn is_private_allocation_collision(
		&self,
		current: &DatabaseRoot,
		key: ConflictKey,
	) -> bool {
		let created_record = |global| {
			self.mutations.iter().find_map(|mutation| match mutation {
				Mutation::CreateAtom {
					id,
					record_id,
					..
				} if *id == global => Some(record_id),
				_ => None,
			})
		};
		match key {
			ConflictKey::BaseAtom(global)
			| ConflictKey::GraphMembership(_, global)
			| ConflictKey::GraphOverlay(_, global) => {
				created_record(global).is_some_and(|record| current.resolve_atom(record).is_none())
			}
			ConflictKey::GraphIncidence(graph, local) => self
				.mutations
				.iter()
				.find_map(|mutation| match mutation {
					Mutation::AttachNode {
						graph: candidate_graph,
						local: candidate_local,
						global,
						..
					}
					| Mutation::AttachEdge {
						graph: candidate_graph,
						local: candidate_local,
						global,
						..
					} if *candidate_graph == graph && *candidate_local == local => Some(*global),
					_ => None,
				})
				.is_some_and(|global| {
					created_record(global).map_or_else(
						|| current.membership(graph, global).is_none(),
						|record| current.resolve_atom(record).is_none(),
					)
				}),
			_ => false,
		}
	}

	fn validate_commit_fanout(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		let initial_fanout_bytes =
			self.fanout_reservations.len().saturating_mul(EFFECTIVE_FANOUT_WRITE_BYTES);
		let non_fanout_bytes = self
			.write_bytes
			.saturating_sub(initial_fanout_bytes)
			.saturating_add(self.change_journal.bytes);
		let mut actual = BTreeSet::new();
		for mutation in &self.mutations {
			let (global, key) = match mutation {
				Mutation::SetBaseProperty {
					global,
					key,
					..
				}
				| Mutation::RemoveBaseProperty {
					global,
					key,
				} => (global, key),
				_ => continue,
			};
			let current_memberships = (!self.mutations.iter().any(
				|mutation| matches!(mutation, Mutation::CreateAtom { id, .. } if id == global),
			))
			.then(|| current.memberships(*global))
			.flatten();
			let graphs = current_memberships
				.into_iter()
				.flat_map(|memberships| memberships.keys().copied())
				.chain(
					self.working
						.memberships(*global)
						.into_iter()
						.flat_map(|memberships| memberships.keys().copied()),
				)
				.collect::<BTreeSet<_>>();
			for graph in graphs {
				let overlay_written =
					self.writes.contains_key(&ConflictKey::GraphOverlay(graph, *global));
				let membership_created =
					self.writes.contains_key(&ConflictKey::GraphMembership(graph, *global));
				let inherits = if overlay_written || membership_created {
					self.working.inherits_property(graph, *global, *key)
				} else {
					current.inherits_property(graph, *global, *key)
				};
				if inherits {
					actual.insert((*global, graph, *key));
				}
			}
		}
		let actual_bytes = actual
			.len()
			.checked_mul(EFFECTIVE_FANOUT_WRITE_BYTES)
			.and_then(|fanout| non_fanout_bytes.checked_add(fanout))
			.unwrap_or(usize::MAX);
		if actual_bytes > self.store.inner.transaction_limits().max_write_set_bytes {
			return Err(self.limit_error(actual_bytes));
		}
		Ok(())
	}

	pub(super) fn apply_mutations(
		root: &mut DatabaseRoot,
		mutations: &[Mutation],
		published_at: u64,
		rebase_allocations: bool,
	) -> FerrosGrynnResult<ReplayRemap> {
		if !mutations.is_empty() && root.indexes.has_deferred_indexes() {
			root.rebuild_indexes()?;
		}
		let mut remap = ReplayRemap::default();
		Self::replay_mutations(root, mutations, published_at, rebase_allocations, &mut remap)?;
		root.publish_statistics_revision(published_at);
		for source in root.retire_missing_changefeeds(published_at)? {
			root.mark_version(ConflictKey::ChangefeedDefinition(source), published_at);
			root.mark_version(ConflictKey::ChangefeedCatalogRange, published_at);
		}
		Ok(remap)
	}

	/// Replays consecutive operation ranges using one allocation translation.
	pub(super) fn replay_mutations(
		root: &mut DatabaseRoot,
		mutations: &[Mutation],
		published_at: u64,
		rebase_allocations: bool,
		remap: &mut ReplayRemap,
	) -> FerrosGrynnResult<()> {
		for mutation in mutations {
			match mutation {
				Mutation::DefineLiveBinding(definition) => {
					let catalog = Arc::make_mut(root.live_catalog_mut());
					if catalog.bindings.contains_key(&definition.id) {
						return Err(FerrosGrynnError::invalid_argument(
							"LIVE LET identity is already registered",
						));
					}
					catalog.bindings.insert(definition.id, definition.clone());
				}
				Mutation::ReplaceLiveBinding {
					definition,
					expected,
				} => {
					Arc::make_mut(root.live_catalog_mut())
						.replace_binding(definition.clone(), *expected)?;
				}
				Mutation::RemoveLiveBinding(id) => {
					Arc::make_mut(root.live_catalog_mut()).remove_binding(
						*id,
						published_at,
						&mut crate::datastore::BindingAdmission::new(
							&crate::live::LiveBindingOptions::default(),
						),
						&|| Ok(()),
					)?;
				}
				Mutation::RegisterLive(definition) => {
					super::live::validate_source(root, definition.query.source())?;
					Arc::make_mut(root.live_catalog_mut()).register(definition.clone())?;
				}
				Mutation::CloseLive(id) => {
					Arc::make_mut(root.live_catalog_mut()).close(*id, published_at)?;
				}
				Mutation::AcknowledgeLive(token) => {
					if token.database_id() != root.database_uuid.into()
						|| token.versionstamp().get() > root.commit_ts
					{
						return Err(FerrosGrynnError::invalid_argument(
							"LIVE ACK origin/position changed",
						));
					}
					Arc::make_mut(root.live_catalog_mut()).acknowledge(
						token.live_id(),
						token.version(),
						token.versionstamp().get(),
					)?;
				}
				Mutation::ConfigureChangefeed {
					source,
					id,
					name,
					config,
				} => {
					let present = match source {
						crate::changefeed::ChangefeedSource::Class(class) => {
							root.schema.class_by_id(*class).is_some()
						}
						crate::changefeed::ChangefeedSource::Graph(graph) => {
							root.schema.graph(*graph).is_some()
						}
					};
					if config.is_some() && !present {
						return Err(FerrosGrynnError::invalid_argument(
							"changefeed owner changed before publication",
						));
					}
					Arc::make_mut(root.changefeeds_mut()).configure(
						*source,
						*id,
						name.clone(),
						*config,
						published_at,
					)?;
				}
				Mutation::InternProperty {
					key,
					name,
				} => {
					root.restore_property_name(*key, name.clone())?;
				}
				Mutation::DefineClass(class) => {
					root.install_class(class.clone(), published_at)?;
				}
				Mutation::DefineParameter(parameter) => {
					root.install_parameter(parameter.clone(), published_at)?;
				}
				Mutation::DefineFunction(function) => {
					root.install_function(function.clone(), published_at)?;
				}
				Mutation::DefineEvent(event) => {
					root.install_event(event.clone(), published_at)?;
				}
				Mutation::EnqueueEvent(invocation) => {
					Arc::make_mut(root.event_outbox_mut())
						.enqueue(Arc::clone(invocation), published_at)?;
				}
				Mutation::TransitionEvent {
					id,
					expected_attempts,
					state,
					due_at_ns,
					diagnostic,
				} => {
					Arc::make_mut(root.event_outbox_mut()).transition(
						*id,
						*expected_attempts,
						*state,
						*due_at_ns,
						diagnostic.clone(),
						published_at,
					)?;
				}
				Mutation::DefineAnalyzer(analyzer) => {
					root.install_analyzer(analyzer.clone(), published_at)?;
				}
				Mutation::DefineGraph(graph) => {
					root.install_graph(graph.clone(), published_at)?;
				}
				Mutation::ReplaceClass(class) => {
					root.replace_class(class.clone(), published_at)?;
				}
				Mutation::ReplaceParameter(parameter) => {
					root.replace_parameter(parameter.clone(), published_at)?;
				}
				Mutation::ReplaceFunction(function) => {
					root.replace_function(function.clone(), published_at)?;
				}
				Mutation::ReplaceEvent(event) => {
					root.replace_event(event.clone(), published_at)?;
				}
				Mutation::ReplaceAnalyzer(analyzer) => {
					root.replace_analyzer(analyzer.clone(), published_at)?;
				}
				Mutation::ReplaceGraph(graph) => {
					root.replace_graph_schema(graph.clone(), published_at)?;
				}
				Mutation::InstallBuiltGraphTree(build) => {
					build.apply(root, published_at)?;
				}
				Mutation::RemoveClass(class) => {
					root.remove_class(*class, published_at)?;
				}
				Mutation::RemoveParameter(parameter) => {
					root.remove_parameter(*parameter)?;
				}
				Mutation::RemoveFunction(function) => {
					root.remove_function(*function, published_at)?;
				}
				Mutation::RemoveEvent(id) => {
					root.remove_event(*id)?;
				}
				Mutation::RemoveAnalyzer(analyzer) => {
					root.remove_analyzer(*analyzer)?;
				}
				Mutation::RemoveGraph(graph) => {
					root.remove_graph(*graph)?;
				}
				Mutation::CreateAtom {
					id,
					class_id,
					record_id,
					properties,
				} => {
					let class = root
						.schema
						.class_by_id(*class_id)
						.ok_or_else(|| FerrosGrynnError::class_does_not_exist(record_id.class()))?;
					if rebase_allocations {
						let actual =
							root.insert_atom(&class, record_id.clone(), properties.clone())?;
						remap.globals.insert(*id, actual);
					} else {
						root.insert_atom_at(*id, &class, record_id.clone(), properties.clone())?;
					}
				}
				Mutation::SetBaseProperty {
					global,
					key,
					value,
				} => {
					root.set_base_property(remap.global(*global), *key, value.clone())?;
				}
				Mutation::RemoveBaseProperty {
					global,
					key,
				} => {
					root.remove_base_property(remap.global(*global), *key)?;
				}
				Mutation::DeleteAtom {
					global,
				} => {
					root.remove_atom(remap.global(*global))?;
				}
				Mutation::AttachNode {
					graph,
					local,
					global,
					label,
					nested_graph,
				} => {
					if rebase_allocations {
						let actual = root.attach_node(
							*graph,
							remap.global(*global),
							label.clone(),
							*nested_graph,
						)?;
						remap.locals.insert((*graph, *local), actual);
					} else {
						root.attach_node_at(
							*graph,
							*local,
							remap.global(*global),
							label.clone(),
							*nested_graph,
						)?;
					}
				}
				Mutation::AttachEdge {
					graph,
					local,
					global,
					label,
					topology,
					weight,
					transition_graph,
				} => {
					let topology = remap.topology(root, *graph, topology)?;
					if rebase_allocations {
						let actual = root.attach_edge(
							*graph,
							remap.global(*global),
							label.clone(),
							topology,
							*weight,
							*transition_graph,
						)?;
						remap.locals.insert((*graph, *local), actual);
					} else {
						root.attach_edge_at(
							*graph,
							*local,
							remap.global(*global),
							label.clone(),
							topology,
							*weight,
							*transition_graph,
						)?;
					}
				}
				Mutation::Detach {
					graph,
					global,
				} => {
					let global = remap.global(*global);
					root.detach(*graph, global)?.ok_or_else(|| {
						FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
					})?;
				}
				Mutation::SetOverlay {
					graph,
					local,
					key,
					value,
				} => {
					root.set_overlay(*graph, remap.local(*graph, *local), *key, value.clone())?;
				}
				Mutation::HideProperty {
					graph,
					local,
					key,
				} => {
					root.hide_property(*graph, remap.local(*graph, *local), *key)?;
				}
				Mutation::ResetProperty {
					graph,
					local,
					key,
				} => {
					root.reset_property(*graph, remap.local(*graph, *local), *key)?;
				}
				Mutation::SetOccurrenceLabel {
					graph,
					local,
					label,
				} => {
					root.set_occurrence_label(*graph, remap.local(*graph, *local), label.clone())?;
				}
				Mutation::ReplaceOccurrenceState {
					graph,
					local,
					state,
				} => {
					root.replace_occurrence_state(
						*graph,
						remap.local(*graph, *local),
						remap.occurrence_state(root, *graph, state)?,
					)?;
				}
			}
		}
		Ok(())
	}
}

fn mutation_affects_explicit_indexes(mutation: &Mutation) -> bool {
	matches!(
		mutation,
		Mutation::InstallBuiltGraphTree(_)
			| Mutation::CreateAtom { .. }
			| Mutation::SetBaseProperty { .. }
			| Mutation::RemoveBaseProperty { .. }
			| Mutation::DeleteAtom { .. }
			| Mutation::AttachNode { .. }
			| Mutation::AttachEdge { .. }
			| Mutation::Detach { .. }
			| Mutation::SetOverlay { .. }
			| Mutation::HideProperty { .. }
			| Mutation::ResetProperty { .. }
			| Mutation::SetOccurrenceLabel { .. }
			| Mutation::ReplaceOccurrenceState { .. }
	)
}

fn mutation_allows_physical_index_delta(mutation: &Mutation) -> bool {
	matches!(
		mutation,
		Mutation::EnqueueEvent(_)
			| Mutation::TransitionEvent { .. }
			| Mutation::InternProperty { .. }
			| Mutation::ReplaceClass(_)
			| Mutation::CreateAtom { .. }
			| Mutation::SetBaseProperty { .. }
			| Mutation::RemoveBaseProperty { .. }
			| Mutation::DeleteAtom { .. }
			| Mutation::AttachNode { .. }
			| Mutation::AttachEdge { .. }
			| Mutation::Detach { .. }
			| Mutation::SetOverlay { .. }
			| Mutation::HideProperty { .. }
			| Mutation::ResetProperty { .. }
			| Mutation::SetOccurrenceLabel { .. }
			| Mutation::ReplaceOccurrenceState { .. }
	)
}
