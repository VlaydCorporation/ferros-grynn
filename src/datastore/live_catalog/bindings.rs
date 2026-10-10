//! Bounded identity dependencies and immutable reactive binding definitions.

use std::sync::Arc;

use super::{BindingAdmission, LiveCatalog, query_dependencies};
use crate::{
	datastore::{FunctionId, SchemaVersion},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{
		LiveBindingId,
		LiveBindingInfo,
		LiveBindingOptions,
		LiveBindingRevision,
		LiveExpr,
		LiveState,
		LiveVersion,
	},
	values::{Object, Value},
};

/// Logical recipe and its latest successfully evaluated owned value.
#[derive(Debug, Clone)]
pub struct LiveBindingDefinition {
	/// Identity survives expression replacement and lexical shadowing.
	pub(crate) id: LiveBindingId,
	/// Replacement revision, independent of value publication.
	pub(crate) version: LiveVersion,
	/// Private, published, faulted or explicitly removed state.
	pub(crate) state: LiveState,
	/// Ordinary typed lowering remains owned by GQL.
	pub(crate) expression: Arc<LiveExpr>,
	/// Frozen ordinary parameters, including lexical values.
	pub(crate) parameters: Arc<Object>,
	/// Canonical expression/capture descriptor, never executable IR.
	pub(crate) descriptor: Arc<[u8]>,
	/// Checked per-evaluation limits.
	pub(crate) options: LiveBindingOptions,
	/// Latest successful exact value retained while paused.
	pub(crate) value: Arc<Value>,
	/// Canonical independent historical encoding for representation comparison.
	pub(crate) value_descriptor: Arc<[u8]>,
	/// Sorted direct stable identities, including nested SELECT inputs.
	pub(crate) dependencies: Vec<LiveBindingId>,
	/// Concrete versions of all compiled transitive PURE calls.
	pub(crate) functions: Vec<(FunctionId, SchemaVersion)>,
	/// Record source spelling is qualified by its original class incarnation.
	pub(crate) record_sources: Vec<(crate::values::Text, crate::datastore::ClassId)>,
	/// First successful publication, zero while private.
	pub(crate) defined_at: u64,
	/// Latest definition/state publication.
	pub(crate) published_at: u64,
	/// Commit publishing the latest value revision.
	pub(crate) value_at: u64,
	/// Typed evaluation diagnostic; source DML remains eligible to commit.
	pub(crate) diagnostic: Option<FerrosGrynnError>,
}

impl LiveBindingDefinition {
	/// Admits the owned vectors before copy-on-write clones this definition.
	pub(crate) fn admit_copy(&self, budget: &mut BindingAdmission) -> FerrosGrynnResult<()> {
		let bytes =
			self.dependencies
				.len()
				.checked_mul(std::mem::size_of::<LiveBindingId>())
				.and_then(|bytes| {
					bytes.checked_add(
						self.functions
							.len()
							.checked_mul(std::mem::size_of::<(FunctionId, SchemaVersion)>())?,
					)
				})
				.and_then(|bytes| {
					bytes.checked_add(self.record_sources.len().checked_mul(
						std::mem::size_of::<(crate::values::Text, crate::datastore::ClassId)>(),
					)?)
				})
				.and_then(|bytes| bytes.checked_add(std::mem::size_of::<Self>()))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE LET definition copy"))?;
		budget.admit(bytes, 1)?;
		for (name, _) in &self.record_sources {
			budget.admit(name.len(), 1)?;
		}
		Ok(())
	}

	/// Returns independent metadata; no reader or snapshot is retained.
	pub(crate) fn info(&self) -> LiveBindingInfo {
		self.info_with_value(self.value.as_ref().clone())
	}

	/// GQL admits vectors and the exact value before materializing metadata.
	pub(crate) fn info_controlled(
		&self,
		control: &crate::gql::ExecutionControl,
	) -> FerrosGrynnResult<(LiveBindingInfo, crate::gql::BarrierAccount)> {
		control.check()?;
		let mut budget = BindingAdmission::new(
			&LiveBindingOptions::default()
				.with_budgets(control.barrier_memory_available(), control.max_loop_iterations)?,
		);
		let before = budget.remaining_options()?.scratch_bytes();
		self.admit_copy(&mut budget)?;
		let mut account = control.barrier_account();
		if !account.reserve(before - budget.remaining_options()?.scratch_bytes()) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"LIVE LET metadata",
				control.barrier_memory_limit() as u64,
			));
		}
		let value = control.copy_reactive_value(self.value.as_ref())?;
		Ok((self.info_with_value(value), account))
	}

	/// Both metadata consumers share one authoritative field projection.
	fn info_with_value(&self, value: Value) -> LiveBindingInfo {
		LiveBindingInfo {
			revision: LiveBindingRevision {
				id: self.id,
				definition: self.version,
				value: self.value_at.into(),
			},
			state: self.state,
			expression: self.expression.clone(),
			value,
			parameters: self.parameters.clone(),
			options: self.options.clone(),
			dependencies: self.dependencies.clone(),
			functions: self.functions.clone(),
			diagnostic: self.diagnostic.clone(),
		}
	}
}

impl LiveCatalog {
	/// Rejects cycles and absent/removed dependencies before
	/// evaluation/publication.
	pub(crate) fn binding_order(
		&self,
		budget: &mut BindingAdmission,
	) -> FerrosGrynnResult<Vec<LiveBindingId>> {
		crate::live::dependencies::binding_order(
			self.bindings
				.values()
				.filter(|entry| entry.state != LiveState::Closed)
				.map(|entry| (entry.id, entry.dependencies.as_slice())),
			budget,
		)
	}

	/// Expression replacement retains identity and checks a concrete old
	/// revision.
	pub(crate) fn replace_binding(
		&mut self,
		binding: Arc<LiveBindingDefinition>,
		expected: LiveVersion,
	) -> FerrosGrynnResult<()> {
		let entry = self.bindings.get_mut(&binding.id).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
		})?;
		if entry.state == LiveState::Closed || entry.version != expected {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE LET definition changed or was removed",
			));
		}
		*entry = binding;
		Ok(())
	}

	/// A removed identity cannot be rebound by a later definition with the same
	/// name.
	pub(crate) fn remove_binding(
		&mut self,
		id: LiveBindingId,
		stamp: u64,
		budget: &mut BindingAdmission,
		check: &dyn Fn() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		check()?;
		let entry = self.bindings.get(&id).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
		})?;
		if entry.state == LiveState::Closed {
			return Ok(());
		}
		for dependent in self.bindings.values() {
			check()?;
			budget.admit(0, 1)?;
			if dependent.state != LiveState::Closed
				&& dependent.dependencies.binary_search(&id).is_ok()
			{
				return Err(FerrosGrynnError::invalid_argument("LIVE LET has dependent bindings"));
			}
		}
		for query in self.queries.values() {
			check()?;
			budget.admit(0, 1)?;
			if query.state != LiveState::Closed
				&& query_dependencies(&query.query, budget)?.binary_search(&id).is_ok()
			{
				return Err(FerrosGrynnError::invalid_argument(
					"LIVE LET has dependent subscriptions",
				));
			}
		}
		let entry = self.bindings.get_mut(&id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("LIVE LET removal lost its definition")
		})?;
		check()?;
		entry.admit_copy(budget)?;
		let entry = Arc::make_mut(entry);
		entry.state = LiveState::Closed;
		entry.published_at = stamp;
		entry.diagnostic = None;
		Ok(())
	}
}
