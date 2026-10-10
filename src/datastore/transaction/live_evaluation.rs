//! Final-state LIVE LET evaluation owns faults separately from source DML.

use std::{path::PathBuf, sync::Arc};

use crate::{
	datastore::{BindingAdmission, ConflictKey, DatabaseRoot, LiveBindingDefinition},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBindingOptions, LiveState},
	storage::live_descriptor,
};

/// Evaluates a validated DAG against one complete replayed commit image.
pub(super) fn evaluate_bindings(
	next: &mut DatabaseRoot,
	scratch_root: PathBuf,
	observations: &crate::datastore::live_observation::Counters,
) -> FerrosGrynnResult<()> {
	if next.live_catalog().bindings.is_empty() {
		return Ok(());
	}
	let mut admission = BindingAdmission::new(&LiveBindingOptions::default());
	let count = next
		.live_catalog()
		.bindings
		.len()
		.checked_add(next.live_catalog().queries.len())
		.and_then(|count| count.checked_add(next.live_catalog().histories.len()))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE candidate catalog"))?;
	admission.admit(
		count
			.checked_mul(256)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE candidate catalog"))?,
		count as u64,
	)?;
	let order = next.live_catalog().binding_order(&mut admission)?;
	for id in &order {
		next.live_catalog()
			.bindings
			.get(id)
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE LET ordering lost its definition")
			})?
			.admit_copy(&mut admission)?;
	}
	for entry in next
		.live_catalog()
		.bindings
		.values()
		.filter(|entry| entry.state == LiveState::Closed && entry.defined_at == 0)
	{
		entry.admit_copy(&mut admission)?;
	}
	let mut root = Arc::new(next.clone());
	// Detach the catalog once. Each evaluation borrows it; completed values
	// therefore update the same private map in topological order.
	Arc::make_mut(Arc::make_mut(&mut root).live_catalog_mut());
	for id in order {
		let definition = root.live_catalog().bindings.get(&id).cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("LIVE LET ordering lost its definition")
		})?;
		if definition.state == LiveState::Paused {
			continue;
		}
		observations.binding_attempt();
		let result = evaluate_one(&root, &definition, scratch_root.clone(), observations);
		let pending = definition.state == LiveState::Pending;
		let stamp = root.commit_ts;
		let candidate = Arc::make_mut(&mut root);
		let entry =
			Arc::make_mut(candidate.live_catalog_mut()).bindings.get_mut(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("LIVE LET evaluation lost its definition")
			})?;
		let entry = Arc::make_mut(entry);
		let changed = match result {
			Ok((evaluation, descriptor)) => {
				let changed = pending || descriptor.as_slice() != entry.value_descriptor.as_ref();
				if changed {
					entry.value = Arc::new(evaluation.value);
					entry.value_descriptor = descriptor.into();
					entry.value_at = stamp;
				}
				entry.functions = evaluation.functions;
				entry.record_sources = evaluation.record_sources;
				entry.state = LiveState::Active;
				entry.diagnostic = None;
				changed
			}
			Err(error) if pending => return Err(error),
			Err(error) => {
				entry.state = LiveState::Paused;
				// Only bounded identifiers/static classification are copied.
				// The original owned status remains a
				// structured in-memory cause.
				entry.diagnostic = Some(
					FerrosGrynnError::live_evaluation_fault(
						id.to_string(),
						error.status().code().to_string(),
						error.status().definition().subcondition.unwrap_or("evaluation failed"),
					)
					.with_cause(error),
				);
				true
			}
		};
		if entry.defined_at == 0 {
			entry.defined_at = stamp;
		}
		if changed {
			entry.published_at = stamp;
			candidate.mark_version(ConflictKey::LiveBinding(id), stamp);
		}
	}
	// Removed definitions created in this same transaction retain valid
	// catalog bounds without being reactivated or evaluated.
	let candidate = Arc::make_mut(&mut root);
	let stamp = candidate.commit_ts;
	for entry in Arc::make_mut(candidate.live_catalog_mut()).bindings.values_mut() {
		if entry.defined_at == 0 {
			let entry = Arc::make_mut(entry);
			entry.defined_at = stamp;
			entry.published_at = stamp;
			entry.value_at = stamp;
		}
	}
	*next = Arc::unwrap_or_clone(root);
	Ok(())
}

/// Keeps captured PURE versions fixed until explicit expression replacement.
fn evaluate_one(
	root: &Arc<DatabaseRoot>,
	definition: &LiveBindingDefinition,
	scratch_root: PathBuf,
	observations: &crate::datastore::live_observation::Counters,
) -> FerrosGrynnResult<(crate::gql::live_lowering::BindingEvaluation, Vec<u8>)> {
	let mut budget = BindingAdmission::new(&definition.options);
	let result = evaluate_one_admitted(root, definition, scratch_root, &mut budget);
	observations.admitted(&budget);
	result
}

/// Evaluates one binding with caller-owned admission.
fn evaluate_one_admitted(
	root: &Arc<DatabaseRoot>,
	definition: &LiveBindingDefinition,
	scratch_root: PathBuf,
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<(crate::gql::live_lowering::BindingEvaluation, Vec<u8>)> {
	// Registration/replacement captures identities immediately. Final
	// activation validates those captures before fresh logical lowering as
	// well.
	validate_captures(root, &definition.record_sources, &definition.functions, budget)?;
	let bindings = |id| {
		root.live_catalog()
			.bindings
			.get(&id)
			.filter(|entry| entry.state == LiveState::Active)
			.map(|entry| entry.value.as_ref())
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument(
					"LIVE LET dependency is absent, removed or paused",
				)
			})
	};
	let evaluation = crate::gql::live_lowering::evaluate_binding(
		root,
		&definition.expression,
		&definition.parameters,
		&bindings,
		&budget.remaining_options()?,
		scratch_root,
		&|_key| {},
	)?;
	if evaluation.functions != definition.functions {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE PURE overload resolution changed its captured versions",
		));
	}
	if evaluation.record_sources != definition.record_sources {
		return Err(FerrosGrynnError::invalid_argument(
			"LIVE record sources changed their captured owner identities",
		));
	}
	budget.admit(
		evaluation
			.value
			.estimated_heap_bytes()
			.checked_add(std::mem::size_of::<crate::values::Value>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE result value"))?,
		evaluation.work_used,
	)?;
	let descriptor = live_descriptor::encode_binding_value(
		&evaluation.value,
		budget.remaining_options()?.scratch_bytes(),
	)?;
	Ok((evaluation, descriptor))
}

/// Queries and bindings keep the same concrete source/function incarnations.
pub(super) fn validate_captures(
	root: &DatabaseRoot,
	sources: &[(crate::values::Text, crate::datastore::ClassId)],
	functions: &[(crate::datastore::FunctionId, crate::datastore::SchemaVersion)],
	budget: &mut BindingAdmission,
) -> FerrosGrynnResult<()> {
	for (name, owner) in sources {
		budget.admit(0, 1)?;
		if root.schema.class(name.as_str()).and_then(|class| class.id()) != Some(*owner) {
			return Err(FerrosGrynnError::invalid_argument(
				"a captured LIVE record source was removed or recreated",
			));
		}
	}
	for (id, version) in functions {
		let mut compatible = false;
		for function in root.schema.functions() {
			budget.admit(0, 1)?;
			if function.id() == Some(*id) {
				compatible = function.version() == *version
					&& function.effect() == crate::datastore::FunctionEffect::Pure;
				break;
			}
		}
		if !compatible {
			return Err(FerrosGrynnError::invalid_argument(
				"a captured LIVE PURE function version changed",
			));
		}
	}
	Ok(())
}
