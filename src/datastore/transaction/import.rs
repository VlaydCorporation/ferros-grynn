//! Import-only transaction state rotation and final validation.

use std::sync::atomic::Ordering;

use super::{Mutation, Transaction};
use crate::{
	datastore::{DatabaseRoot, ImportConstraintMode, ImportIndexMode, IndexId},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
};

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
		self.atomic_mutation(|transaction| {
			let mutation_start = transaction.mutations.len();
			let _value = apply(transaction)?;
			if transaction.mutations[mutation_start..]
				.iter()
				.any(|mutation| !mutation.allowed_in_import())
			{
				return Err(FerrosGrynnError::query_feature_not_supported(
					"schema mutation inside BEGIN IMPORT",
				));
			}
			if transaction.validates_import_constraints_immediately() {
				let graphs = transaction
					.working
					.schema
					.graphs()
					.map(|graph| graph.reference().clone())
					.collect::<Vec<_>>();
				for graph in &graphs {
					transaction.validate_graph_family(graph)?;
				}
			}
			Ok(transaction.mutations.len() != mutation_start)
		})
	}

	/// Stamps and clears transaction-local tracking at one private segment
	/// boundary.
	pub(crate) fn seal_import_segment(&mut self, commit_ts: u64) {
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
		self.mutations.clear();
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
	) -> FerrosGrynnResult<(DatabaseRoot, u64, Vec<IndexId>)> {
		self.ensure_live()?;
		let graphs =
			self.working.schema.graphs().map(|graph| graph.reference().clone()).collect::<Vec<_>>();
		for graph in &graphs {
			self.validate_graph_family(graph)?;
		}
		if full_validation {
			self.working.validate_full_import_state()?;
		}

		self.working.commit_ts = commit_ts;
		let scratch_root = self.store.inner.import_scratch_root();
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
		self.working.rebuild_statistics_at(&scratch_root)?;
		self.working.advance_identity_clocks(
			self.store.inner.next_schema_id.load(Ordering::Relaxed),
			self.store.inner.next_graph_id.load(Ordering::Relaxed),
		);
		let base_commit_ts = self.snapshot.commit_ts;
		Ok((self.working, base_commit_ts, built_indexes))
	}

	/// Returns whether graph-family constraints are checked after every item.
	pub(crate) fn validates_import_constraints_immediately(&self) -> bool {
		self.import_constraints != Some(ImportConstraintMode::Deferred)
	}
}
