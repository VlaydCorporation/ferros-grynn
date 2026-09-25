use std::{
	collections::BTreeSet,
	sync::{Arc, atomic::Ordering},
};

use super::{CommitResult, Mutation, Transaction};
use crate::{
	datastore::{ConflictKey, DatabaseRoot, IsolationLevel, consts::EFFECTIVE_FANOUT_WRITE_BYTES},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

impl Transaction {
	/// Revalidates the complete private root before the normal atomic commit.
	///
	/// # Errors
	///
	/// Returns a graph-family, normalized-state, conflict, persistence, or
	/// publication failure without publishing a partial root.
	pub fn commit_with_validation(mut self) -> FerrosGrynnResult<CommitResult> {
		let graphs =
			self.working.schema.graphs().map(|graph| graph.reference().clone()).collect::<Vec<_>>();
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
		Self::apply_mutations(&mut next, &self.mutations, commit_ts).map_err(|error| {
			FerrosGrynnError::serialization_failure(
				self.id,
				"a transaction prerequisite changed before commit",
			)
			.with_cause(error)
		})?;
		next.advance_identity_clocks(
			store.inner.next_schema_id.load(Ordering::Relaxed),
			store.inner.next_graph_id.load(Ordering::Relaxed),
		);
		next.commit_ts = commit_ts;
		for key in self.writes.keys() {
			next.mark_version(*key, commit_ts);
		}
		for key in &self.range_writes {
			next.mark_version(*key, commit_ts);
		}
		store.inner.persist_root(&next)?;
		*store.inner.current.write() = Arc::new(next);
		Ok(CommitResult {
			transaction: self.id,
			commit_ts,
		})
	}

	fn validate_against(&self, current: &DatabaseRoot) -> FerrosGrynnResult<()> {
		self.validate_write_requirements(current)?;
		if self.options.isolation == IsolationLevel::Serializable && !self.mutations.is_empty() {
			for (key, observed) in self.reads.lock().iter() {
				if current.version(*key) != *observed {
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
			if current.version(*key) != *observed {
				return Err(FerrosGrynnError::serialization_failure(
					self.id,
					"a logical write target or structural prerequisite changed",
				));
			}
		}
		Ok(())
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
			let graphs = current
				.memberships(*global)
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
	) -> FerrosGrynnResult<()> {
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
				Mutation::DefineGraph(graph) => {
					root.install_graph(graph.clone(), published_at)?;
				}
				Mutation::ReplaceClass(class) => {
					root.replace_class(class.clone(), published_at)?;
				}
				Mutation::RemoveClass(class) => {
					root.remove_class(*class)?;
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
					root.insert_atom_at(*id, &class, record_id.clone(), properties.clone())?;
				}
				Mutation::SetBaseProperty {
					global,
					key,
					value,
				} => {
					root.set_base_property(*global, *key, value.clone())?;
				}
				Mutation::RemoveBaseProperty {
					global,
					key,
				} => {
					root.remove_base_property(*global, *key)?;
				}
				Mutation::DeleteAtom {
					global,
				} => {
					root.remove_atom(*global)?;
				}
				Mutation::AttachNode {
					graph,
					local,
					global,
					label,
					nested_graph,
				} => {
					root.attach_node_at(*graph, *local, *global, label.clone(), *nested_graph)?;
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
					root.attach_edge_at(
						*graph,
						*local,
						*global,
						label.clone(),
						topology.clone(),
						*weight,
						*transition_graph,
					)?;
				}
				Mutation::Detach {
					graph,
					global,
				} => {
					root.detach(*graph, *global)?.ok_or_else(|| {
						FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
					})?;
				}
				Mutation::SetOverlay {
					graph,
					local,
					key,
					value,
				} => {
					root.set_overlay(*graph, *local, *key, value.clone())?;
				}
				Mutation::HideProperty {
					graph,
					local,
					key,
				} => {
					root.hide_property(*graph, *local, *key)?;
				}
				Mutation::ResetProperty {
					graph,
					local,
					key,
				} => {
					root.reset_property(*graph, *local, *key)?;
				}
				Mutation::SetOccurrenceLabel {
					graph,
					local,
					label,
				} => {
					root.set_occurrence_label(*graph, *local, label.clone())?;
				}
				Mutation::ReplaceOccurrenceState {
					graph,
					local,
					state,
				} => {
					root.replace_occurrence_state(*graph, *local, state.clone())?;
				}
			}
		}
		root.publish_statistics_revision(published_at);
		Ok(())
	}
}
