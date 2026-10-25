//! Import-only transaction state rotation and final validation.

use std::sync::{Arc, atomic::Ordering};

use super::{Mutation, Transaction};
use crate::{
	datastore::{DatabaseRoot, ImportConstraintMode, ImportIndexMode, IndexId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

#[cfg(test)]
mod tests;

impl Mutation {
	/// Returns whether this mutation belongs to the Stage 10 DML whitelist.
	fn allowed_in_import(&self) -> bool {
		matches!(
			self,
			Self::InternProperty { .. }
				| Self::CreateAtom { .. }
				| Self::SetBaseProperty { .. }
				| Self::RemoveBaseProperty { .. }
				| Self::DeleteAtom { .. }
				| Self::AttachNode { .. }
				| Self::AttachEdge { .. }
				| Self::Detach { .. }
				| Self::SetOverlay { .. }
				| Self::HideProperty { .. }
				| Self::ResetProperty { .. }
				| Self::SetOccurrenceLabel { .. }
				| Self::ReplaceOccurrenceState { .. }
		)
	}
}

impl Transaction {
	/// Borrows the transaction-visible schema used to encode rejected payloads.
	pub(crate) fn import_schema(&self) -> &crate::datastore::SchemaCatalog {
		self.working.schema.as_ref()
	}

	/// Applies and validates one logical import item as an atomic mutation.
	///
	/// # Errors
	///
	/// Returns the item mutation error, a forbidden-mutation status, or an
	/// immediate graph-family validation failure.
	pub(crate) fn atomic_import_item<T, Apply>(&mut self, apply: Apply) -> FerrosGrynnResult<bool>
	where
		Apply: FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	{
		let captures = self
			.working
			.changefeeds()
			.definitions
			.values()
			.any(|definition| definition.state == crate::changefeed::ChangefeedState::Active);
		let capture_images =
			captures && self.import_changefeed.mode == crate::datastore::ImportChangefeedMode::On;
		if capture_images {
			self.prepare_import_capture_segment()?;
		}
		self.prepare_mutation()?;
		let dispatch = self.has_operation_events();
		let savepoint = (self.rollback_mutation_errors || dispatch).then(|| self.gql_savepoint());
		let checkpoint = self.change_journal.checkpoint();
		let mutation_start = self.mutations.len();
		let owned = self.begin_change_operation();
		// The item shares a Changefeed ordinal, but each call dispatches Events
		// before its caller control and logical before/after images disappear.
		if !dispatch {
			self.atomic_mutation_depth = 1;
		}
		let result = apply(self);
		if !dispatch {
			self.atomic_mutation_depth = 0;
		}
		let result = result.and_then(|_value| {
			if self.mutations[mutation_start..].iter().any(|mutation| !mutation.allowed_in_import())
			{
				return Err(FerrosGrynnError::query_feature_not_supported(
					"schema mutation inside BEGIN IMPORT",
				));
			}
			if self.validates_import_constraints_immediately() {
				let graphs = self
					.working
					.schema
					.graphs()
					.map(|graph| graph.reference().clone())
					.collect::<Vec<_>>();
				for graph in &graphs {
					self.validate_graph_family(graph)?;
				}
			}
			let changed = self.mutations.len() != mutation_start;
			self.finish_change_operation(owned);
			let captured = match (capture_images, captures) {
				(true, _) => false,
				(false, true) => self.retain_import_suppression(mutation_start)?,
				(false, false) => {
					self.change_journal.clear_segment();
					false
				}
			};
			Ok(changed || captured)
		});
		if result.is_err() {
			if let Some(savepoint) = savepoint {
				self.restore_gql_savepoint(savepoint);
			} else {
				self.change_journal.restore(checkpoint);
			}
		}
		result
	}

	/// Stamps and clears transaction-local tracking at one private segment
	/// boundary.
	/// Returns capture admission/replay failures without clearing pending work.
	pub(crate) fn seal_import_segment(&mut self, commit_ts: u64) -> FerrosGrynnResult<bool> {
		self.validate_write_set_payload()?;
		let captured = self.retain_import_segment()?;
		for key in self.writes.keys().copied().chain(self.range_writes.iter().copied()) {
			self.working.mark_version(key, commit_ts);
		}
		self.reads.lock().clear();
		self.missing_records.lock().clear();
		self.last_base_read = None;
		self.range_writes.clear();
		self.writes.clear();
		self.requirements.clear();
		self.fanout_reservations.clear();
		self.write_bytes = 0;
		self.payload_bytes = 0;
		self.mutations.clear();
		self.change_journal.clear_segment();
		Ok(captured)
	}

	/// Performs final validation, derived-state rebuild, and identity-clock
	/// capture.
	///
	/// # Errors
	///
	/// Returns transaction-lifecycle, graph-family, full-validation, index,
	/// statistics, or identity-clock failures.
	pub(crate) fn finish_import(
		mut self,
		commit_ts: u64,
		indexes: ImportIndexMode,
		full_validation: bool,
	) -> FerrosGrynnResult<(Self, Vec<IndexId>)> {
		self.ensure_live()?;
		self.validate_write_set_payload()?;
		let graphs = self
			.working
			.schema
			.all_graphs()
			.map(|graph| graph.reference().clone())
			.collect::<Vec<_>>();
		for graph in &graphs {
			self.validate_graph_family(graph)?;
		}
		self.working.validate_all_qualified_incidence()?;
		self.validate_pending_qualified_incidence(&self.working)?;
		self.working.validate_preserved_qualified_incidence(
			&self.snapshot,
			|graph, connector| {
				self.snapshot
					.atom(connector.global_id())
					.is_some_and(|atom| self.has_qualified_anchor(graph, atom.record_id()))
			},
		)?;
		if full_validation {
			self.working.validate_full_import_state()?;
		}

		self.working.commit_ts = commit_ts;
		let rebuild = indexes == ImportIndexMode::Deferred || full_validation;
		let built_indexes = if rebuild {
			self.working.rebuild_indexes()?;
			self.working
				.schema
				.classes()
				.flat_map(|class| class.indexes())
				.filter_map(|index| index.id())
				.collect()
		} else {
			Vec::new()
		};
		self.working.advance_identity_clocks(
			self.store.inner.next_schema_id.load(Ordering::Relaxed),
			self.store.inner.next_graph_id.load(Ordering::Relaxed),
		);
		Ok((self, built_indexes))
	}

	/// Reuses the accepted primary image and spool after a control race. No
	/// expression evaluation or waiting takes place under the commit mutex.
	pub(in crate::datastore) fn prepare_import_publication(
		&mut self,
		current: &Arc<DatabaseRoot>,
		maximum: usize,
	) -> FerrosGrynnResult<()> {
		self.working.rebase_import_controls(&self.snapshot, current, self.id, maximum)?;
		self.publish_import_events(maximum)?;
		self.working.rebuild_statistics_at(&self.store.inner.import_scratch_root())?;
		let stamp = self.working.commit_ts;
		self.publish_import_changes(stamp)?;
		let capture = match self.import_changefeed.mode {
			crate::datastore::ImportChangefeedMode::On => super::LiveHistoryCapture::Enabled,
			crate::datastore::ImportChangefeedMode::Off => {
				super::LiveHistoryCapture::ImportSuppressed
			}
		};
		super::prepare_live_publication(
			current,
			&mut self.working,
			self.store.inner.query_scratch_root(),
			capture,
			super::LiveQueryTargets::All,
			&self.store.inner.live_observation,
		)
	}

	/// Transfers the complete rechecked candidate to the existing persistence
	/// barrier only after optimistic publication succeeds.
	pub(in crate::datastore) fn into_import_root(self) -> DatabaseRoot {
		self.working
	}

	/// Borrows the final candidate only for transient queue admission.
	pub(in crate::datastore) const fn import_publication_root(&self) -> &DatabaseRoot {
		&self.working
	}

	/// Returns whether graph-family constraints are checked after every item.
	pub(crate) fn validates_import_constraints_immediately(&self) -> bool {
		self.import_constraints != Some(ImportConstraintMode::Deferred)
	}
}
