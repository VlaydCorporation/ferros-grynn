//! Logical mutation boundaries shared by SDK calls and GQL statements.

use super::Transaction;
use crate::gql_status::FerrosGrynnResult;

impl Transaction {
	/// Groups a simple SDK call without adding a root/image savepoint.
	pub(super) fn logical_operation<T>(
		&mut self,
		mutate: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		if self.atomic_mutation_depth == 0 && self.has_operation_events() {
			return self.atomic_mutation(mutate);
		}
		let checkpoint = self.change_journal.checkpoint();
		let intent_start = self.change_intent_count();
		let spool = self.import_changefeed.checkpoint();
		let owned = self.begin_change_operation();
		let mutation_start = self.mutations.len();
		match mutate(self) {
			Ok(value) => {
				if owned && mutation_start == self.mutations.len() {
					self.mark_change_noops(intent_start);
				}
				self.finish_change_operation(owned);
				Ok(value)
			}
			Err(error) => {
				self.change_journal.restore(checkpoint);
				self.import_changefeed.restore(spool);
				Err(error)
			}
		}
	}

	/// Executes one statement-sized mutation with rollback to the transaction's
	/// prior private state when validation or execution fails.
	pub(crate) fn atomic_mutation<T>(
		&mut self,
		mutate: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		if self.atomic_mutation_depth != 0 {
			return mutate(self);
		}
		self.prepare_mutation()?;
		if !self.rollback_mutation_errors && !self.has_operation_events() {
			let checkpoint = self.change_journal.checkpoint();
			let owned = self.begin_change_operation();
			self.atomic_mutation_depth = 1;
			let result = mutate(self);
			self.atomic_mutation_depth = 0;
			if result.is_ok() {
				self.finish_change_operation(owned);
			} else {
				self.change_journal.restore(checkpoint);
			}
			return result;
		}
		let savepoint = self.gql_savepoint();
		let dispatch = self.has_operation_events();
		let previous_inputs = self.event_inputs.take();
		if dispatch {
			self.event_inputs = Some(Box::new(super::event_dispatch::EventInputs::new(
				self.event_control.as_deref(),
			)));
		}
		let mutation_start = self.mutations.len();
		let owned = self.begin_change_operation();
		self.atomic_mutation_depth = 1;
		let result = mutate(self);
		self.atomic_mutation_depth = 0;
		let result = result.and_then(|value| {
			if dispatch {
				self.dispatch_operation_events(&savepoint.working, mutation_start)?;
			}
			Ok(value)
		});
		self.event_inputs = previous_inputs;
		match result {
			Ok(value) => {
				self.finish_change_operation(owned);
				Ok(value)
			}
			Err(error) => {
				self.restore_gql_savepoint(savepoint);
				Err(error)
			}
		}
	}
}
