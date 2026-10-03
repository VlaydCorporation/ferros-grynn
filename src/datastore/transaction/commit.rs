use std::{
	collections::{BTreeMap, BTreeSet},
	sync::{Arc, atomic::Ordering},
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
	graph::{AtomId, EdgeEndpoint, EdgeTopology, GraphId, remap_topology},
};

/// Commit-local address translation for allocations made from an older
/// immutable root. Logical conflict checks run before this map is created.
#[derive(Default)]
pub(super) struct ReplayRemap {
	globals: BTreeMap<super::GlobalAtomId, super::GlobalAtomId>,
	locals: BTreeMap<(GraphId, AtomId), AtomId>,
}

impl ReplayRemap {
	fn global(&self, id: super::GlobalAtomId) -> super::GlobalAtomId {
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

	fn topology(&self, graph: GraphId, topology: &EdgeTopology) -> FerrosGrynnResult<EdgeTopology> {
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
				topology: self.topology(graph, topology)?,
				weight: *weight,
				transition_graph: *transition_graph,
			},
		})
	}
}

impl Transaction {
	/// Revalidates the complete private root before the normal atomic commit.
	///
	/// # Errors
	///
	/// Returns a graph-family, normalized-state, conflict, persistence, or
	/// publication failure without publishing a partial root.
	pub fn commit_with_validation(mut self) -> FerrosGrynnResult<CommitResult> {
		let graphs = self
			.working
			.schema
			.all_graphs()
			.map(|graph| graph.reference().clone())
			.collect::<Vec<_>>();
		for graph in &graphs {
			self.validate_graph_family(graph)?;
		}
		self.working.validate_full_import_state()?;
		self.commit()
	}

	/// Validates and atomically publishes all transaction-private mutations.
	///
	/// # Errors
	///
	/// Returns a snapshot-age, FCW/SSI serialization, replay, or fanout budget
	/// error without publishing a partial root.
	pub fn commit(self) -> FerrosGrynnResult<CommitResult> {
		self.ensure_live()?;
		let store = self.store.clone();
		if !self.mutations.is_empty() {
			store.inner.ensure_write_allowed(self.id)?;
		}
		let _commit = store.inner.commit.lock();
		self.ensure_live()?;
		if !self.mutations.is_empty() {
			store.inner.ensure_write_allowed(self.id)?;
		}
		let current = store.inner.current_root();
		self.validate_against(&current)?;
		self.validate_commit_fanout(&current)?;
		if self.mutations.is_empty() {
			return Ok(CommitResult {
				transaction: self.id,
				commit_ts: current.commit_ts,
			});
		}

		let commit_ts = current.commit_ts.checked_add(1).ok_or_else(|| {
			FerrosGrynnError::serialization_failure(self.id, "the commit clock is exhausted")
		})?;
		let mut next = (*current).clone();
		store.inner.hydrate_statistics(&mut next)?;
		let affected_graphs = self.affected_graphs(&current);
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
			store.inner.next_schema_id.load(Ordering::Relaxed),
			store.inner.next_graph_id.load(Ordering::Relaxed),
		);
		let validation_root = Arc::new(next.clone());
		for graph in affected_graphs {
			let Some(schema) = validation_root.schema.graph(graph) else {
				continue;
			};
			self.validate_graph_family_in_root(validation_root.clone(), schema.reference())?;
		}
		drop(validation_root);
		next.commit_ts = commit_ts;
		for key in self.writes.keys() {
			next.mark_version(remap.conflict(*key), commit_ts);
		}
		for key in &self.range_writes {
			next.mark_version(*key, commit_ts);
		}
		let graph_catalog_only = self.mutations.iter().all(|mutation| {
			matches!(
				mutation,
				Mutation::InternProperty { .. }
					| Mutation::ReplaceGraph(_)
					| Mutation::DefineParameter(_)
					| Mutation::ReplaceParameter(_)
					| Mutation::RemoveParameter(_)
					| Mutation::DefineFunction(_)
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
					| Mutation::DefineParameter(_)
					| Mutation::ReplaceParameter(_)
					| Mutation::RemoveParameter(_)
					| Mutation::DefineFunction(_)
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
		let changed_records = self.index_delta_records(&current, &next, &remap);
		let delta_eligible = self.mutations.iter().all(mutation_allows_physical_index_delta);
		store.inner.persist_root(
			&mut next,
			Some(&current),
			delta_eligible.then_some(&changed_records),
			graph_catalog_only,
			dictionary_changed,
		)?;
		*store.inner.current.write() = Arc::new(next);
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
		if self.options.isolation == IsolationLevel::Serializable && !self.mutations.is_empty() {
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
	fn is_private_allocation_collision(&self, current: &DatabaseRoot, key: ConflictKey) -> bool {
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
		let non_fanout_bytes = self.write_bytes.saturating_sub(initial_fanout_bytes);
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
		for mutation in mutations {
			match mutation {
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
					root.remove_class(*class)?;
				}
				Mutation::RemoveParameter(parameter) => {
					root.remove_parameter(*parameter)?;
				}
				Mutation::RemoveFunction(function) => {
					root.remove_function(*function, published_at)?;
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
					let topology = remap.topology(*graph, topology)?;
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
						remap.occurrence_state(*graph, state)?,
					)?;
				}
			}
		}
		root.publish_statistics_revision(published_at);
		Ok(remap)
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
		Mutation::InternProperty { .. }
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
