//! Transaction-private reactive bindings use the shared typed expression path.

use std::sync::Arc;

use super::{Mutation, Transaction};
use crate::{
	datastore::{
		BindingAdmission,
		ConflictKey,
		LiveBindingDefinition,
		consts::SCHEMA_WRITE_BYTES,
		expression_dependencies,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	live::{LiveBindingId, LiveBindingInfo, LiveBindingOptions, LiveExpr, LiveState, LiveVersion},
	storage::live_descriptor::{self, LiveDescriptor},
	values::{Object, Value},
};

#[allow(
	clippy::tabs_in_doc_comments,
	reason = "nightly rustfmt requires hard tabs in Rustdoc examples"
)]
impl Transaction {
	/// Defines a reactive identity with frozen ordinary inputs, without GQL
	/// text.
	///
	/// ```
	/// use ferros_grynn::{
	/// 	datastore::Datastore,
	/// 	live::{LiveBindingOptions, LiveExpr},
	/// 	values::{Object, Value},
	/// };
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// let store = Datastore::in_memory();
	/// let mut tx = store.begin();
	/// let id = tx.define_live_binding(
	/// 	LiveExpr::Literal(Value::from(18_i64)),
	/// 	Object::new(),
	/// 	LiveBindingOptions::default(),
	/// )?;
	/// tx.commit()?;
	/// let mut tx = store.begin();
	/// tx.replace_live_binding_value(id, Value::from(21_i64))?;
	/// tx.commit()?;
	/// assert_eq!(store.begin().live_binding_value(id)?, Value::from(21_i64));
	/// # Ok(()) }
	/// ```
	///
	/// # Errors
	///
	/// Returns source, typing, effect, dependency, snapshot, read-only,
	/// import-DDL or resource errors. Failed registration leaves no
	/// reactive identity.
	pub fn define_live_binding(
		&mut self,
		expression: LiveExpr,
		parameters: Object,
		options: LiveBindingOptions,
	) -> FerrosGrynnResult<LiveBindingId> {
		self.define_live_binding_controlled(expression, parameters, options, None)
	}

	/// GQL registration shares the caller's cancellation and timeout policy.
	pub(crate) fn define_live_binding_controlled(
		&mut self,
		expression: LiveExpr,
		parameters: Object,
		options: LiveBindingOptions,
		parent: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<LiveBindingId> {
		self.run_gql_statement(|tx| {
			tx.prepare_schema_mutation()?;
			let id = LiveBindingId::from_uuid(crate::values::Uuid::now_v7())?;
			tx.stage_live_binding(id, expression, parameters, options, None, parent)?;
			Ok(id)
		})
	}

	/// Replaces the expression and its frozen inputs while retaining the
	/// identity.
	///
	/// # Errors
	///
	/// Returns missing/removed identity, cycle, typing, effect, snapshot,
	/// import-DDL, read-only or resource errors. Failure preserves the
	/// preceding definition.
	pub fn replace_live_binding_expression(
		&mut self,
		id: LiveBindingId,
		expression: LiveExpr,
		parameters: Object,
		options: LiveBindingOptions,
	) -> FerrosGrynnResult<()> {
		self.replace_live_binding_controlled(id, expression, parameters, options, None)
	}

	/// Expression replacement retains the SDK savepoint and caller
	/// interruption.
	pub(crate) fn replace_live_binding_controlled(
		&mut self,
		id: LiveBindingId,
		expression: LiveExpr,
		parameters: Object,
		options: LiveBindingOptions,
		parent: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<()> {
		self.run_gql_statement(|tx| {
			tx.prepare_schema_mutation()?;
			let previous =
				tx.working.live_catalog().bindings.get(&id).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
				})?;
			if previous.state == LiveState::Closed {
				return Err(FerrosGrynnError::invalid_argument("LIVE LET identity was removed"));
			}
			tx.stage_live_binding(id, expression, parameters, options, Some(previous), parent)
		})
	}

	/// Installs a literal expression, discarding all preceding reactive
	/// dependencies.
	///
	/// # Errors
	///
	/// Returns missing/removed identity, runtime-only value, snapshot,
	/// import-DDL, read-only or resource errors. Ordinary execute
	/// parameters remain immutable.
	pub fn replace_live_binding_value(
		&mut self,
		id: LiveBindingId,
		value: Value,
	) -> FerrosGrynnResult<()> {
		self.run_gql_statement(|tx| {
			tx.prepare_schema_mutation()?;
			let previous =
				tx.working.live_catalog().bindings.get(&id).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
				})?;
			if previous.state == LiveState::Closed {
				return Err(FerrosGrynnError::invalid_argument("LIVE LET identity was removed"));
			}
			let options = previous.options.clone();
			tx.stage_live_binding(
				id,
				LiveExpr::Literal(value),
				Object::new(),
				options,
				Some(previous),
				None,
			)
		})
	}

	/// Returns the snapshot-owned latest value as an independent owned value.
	///
	/// # Errors
	///
	/// Returns missing/removed/faulted identity or snapshot errors.
	pub fn live_binding_value(&mut self, id: LiveBindingId) -> FerrosGrynnResult<Value> {
		self.prepare_read()?;
		self.record_read(ConflictKey::LiveBinding(id));
		self.working.live_catalog().binding_value(id).cloned()
	}

	/// Reads expression/value revisions and frozen inputs from this exact
	/// snapshot.
	///
	/// # Errors
	///
	/// Returns missing-identity or snapshot errors; removed identities remain
	/// inspectable.
	pub fn live_binding_info(&mut self, id: LiveBindingId) -> FerrosGrynnResult<LiveBindingInfo> {
		self.prepare_read()?;
		self.record_read(ConflictKey::LiveBinding(id));
		Ok(self
			.working
			.live_catalog()
			.bindings
			.get(&id)
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
			})?
			.info())
	}

	/// Introspection shares lexical read admission and the ordinary SSI
	/// identity.
	pub(crate) fn query_live_binding_info(
		&self,
		snapshot: &crate::datastore::QuerySnapshot,
		id: LiveBindingId,
		control: &crate::gql::ExecutionControl,
	) -> FerrosGrynnResult<(LiveBindingInfo, crate::gql::BarrierAccount)> {
		self.ensure_live()?;
		self.record_read(ConflictKey::LiveBinding(id));
		snapshot
			.logical_root()
			.live_catalog()
			.bindings
			.get(&id)
			.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
			})?
			.info_controlled(control)
	}

	/// Removes an identity after checking active binding and subscription
	/// dependents.
	///
	/// # Errors
	///
	/// Returns dependency, missing-identity, snapshot, import-DDL, read-only or
	/// resource errors. Closing a consumer alone does not remove its
	/// subscription.
	pub fn remove_live_binding(&mut self, id: LiveBindingId) -> FerrosGrynnResult<()> {
		self.remove_live_binding_controlled(id, LiveBindingOptions::default(), None)
	}

	/// GQL removal shares caller admission and cancellation throughout the
	/// dependent scan, before mutating the private catalog.
	pub(crate) fn remove_live_binding_controlled(
		&mut self,
		id: LiveBindingId,
		options: LiveBindingOptions,
		parent: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<()> {
		let check = || parent.map_or(Ok(()), crate::gql::ExecutionControl::check);
		self.run_gql_statement(|tx| {
			check()?;
			tx.prepare_schema_mutation()?;
			let previous = tx.working.live_catalog().bindings.get(&id).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("LIVE LET identity is not registered")
			})?;
			if previous.state == LiveState::Closed {
				return Ok(());
			}
			tx.ensure_capacity(SCHEMA_WRITE_BYTES)?;
			let mut budget = BindingAdmission::new(&options);
			tx.admit_live_catalog_copy(&mut budget)?;
			let mut candidate = tx.working.live_catalog().as_ref().clone();
			candidate.remove_binding(id, tx.snapshot.commit_ts, &mut budget, &check)?;
			*tx.working.live_catalog_mut() = Arc::new(candidate);
			tx.insert_write(ConflictKey::LiveBinding(id), 0);
			tx.write_bytes += SCHEMA_WRITE_BYTES;
			tx.mark_range_change(ConflictKey::LiveCatalogRange);
			tx.mutations.push(Mutation::RemoveLiveBinding(id));
			Ok(())
		})
	}

	/// Checks root/catalog clone cost before staging a definition privately.
	fn admit_live_catalog_copy(&self, budget: &mut BindingAdmission) -> FerrosGrynnResult<()> {
		let catalog = self.working.live_catalog();
		let count = catalog
			.queries
			.len()
			.checked_add(catalog.bindings.len())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE catalog identities"))?;
		budget.admit(
			count
				.checked_mul(256)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE catalog copies"))?,
			count as u64,
		)
	}

	/// Prepares/evaluates/admit-checks everything before changing the private
	/// catalog.
	fn stage_live_binding(
		&mut self,
		id: LiveBindingId,
		expression: LiveExpr,
		parameters: Object,
		options: LiveBindingOptions,
		previous: Option<Arc<LiveBindingDefinition>>,
		parent: Option<&crate::gql::ExecutionControl>,
	) -> FerrosGrynnResult<()> {
		let mut budget = BindingAdmission::new(&options);
		let dependencies = expression_dependencies(&expression, &mut budget)?;
		self.admit_live_catalog_copy(&mut budget)?;
		for dependency in &dependencies {
			self.record_read(ConflictKey::LiveBinding(*dependency));
		}
		let recipe = LiveDescriptor::Binding {
			expression,
			parameters,
		};
		let descriptor = live_descriptor::encode(&recipe, options.scratch_bytes())?;
		budget.admit(descriptor.len(), 0)?;
		let LiveDescriptor::Binding {
			expression,
			parameters,
		} = recipe
		else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"LIVE LET recipe has an invalid kind",
			));
		};
		let root = Arc::new(self.working.clone());
		let values = |dependency| {
			root.live_catalog()
				.bindings
				.get(&dependency)
				.filter(|entry| matches!(entry.state, LiveState::Active | LiveState::Pending))
				.map(|entry| entry.value.as_ref())
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"LIVE LET dependency is absent, removed or paused",
					)
				})
		};
		let evaluated = crate::gql::live_lowering::evaluate_binding_controlled(
			&root,
			&expression,
			&parameters,
			&values,
			&budget.remaining_options()?,
			self.query_scratch_root(),
			&|key| self.record_read(key),
			parent,
		)?;
		budget.admit(
			evaluated
				.value
				.estimated_heap_bytes()
				.checked_add(std::mem::size_of::<Value>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE LET value"))?,
			evaluated.work_used,
		)?;
		budget.admit(
			evaluated
				.functions
				.len()
				.checked_mul(16)
				.and_then(|bytes| {
					bytes.checked_add(evaluated.record_sources.len().checked_mul(32)?)
				})
				.ok_or_else(|| {
					FerrosGrynnError::operation_overflow("LIVE LET function/source revisions")
				})?,
			0,
		)?;
		let value_descriptor = live_descriptor::encode_binding_value(
			&evaluated.value,
			budget.remaining_options()?.scratch_bytes(),
		)?;
		budget.admit(value_descriptor.len(), 0)?;
		let version = match &previous {
			Some(previous) => {
				LiveVersion::new(previous.version.get().checked_add(1).ok_or_else(|| {
					FerrosGrynnError::operation_overflow("LIVE LET definition version")
				})?)?
			}
			None => LiveVersion::default(),
		};
		let source_bytes = evaluated
			.record_sources
			.iter()
			.try_fold(0_usize, |bytes, (name, _)| {
				bytes.checked_add(name.len())?.checked_add(std::mem::size_of::<(
					crate::values::Text,
					crate::datastore::ClassId,
				)>())
			})
			.ok_or_else(|| {
				FerrosGrynnError::operation_overflow("LIVE LET retained source owners")
			})?;
		let cost = descriptor
			.len()
			.checked_add(value_descriptor.len())
			.and_then(|cost| cost.checked_add(SCHEMA_WRITE_BYTES))
			.and_then(|cost| cost.checked_add(dependencies.len().checked_mul(16)?))
			.and_then(|cost| cost.checked_add(evaluated.functions.len().checked_mul(16)?))
			.and_then(|cost| cost.checked_add(source_bytes))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE LET retained definition"))?;
		self.ensure_capacity(cost)?;
		let definition = Arc::new(LiveBindingDefinition {
			id,
			version,
			state: LiveState::Pending,
			expression: Arc::new(expression),
			parameters: Arc::new(parameters),
			descriptor: descriptor.into(),
			options,
			value: Arc::new(evaluated.value),
			value_descriptor: value_descriptor.into(),
			dependencies,
			functions: evaluated.functions,
			record_sources: evaluated.record_sources,
			defined_at: previous.as_ref().map_or(0, |entry| entry.defined_at),
			published_at: previous.as_ref().map_or(0, |entry| entry.published_at),
			value_at: previous.as_ref().map_or(0, |entry| entry.value_at),
			diagnostic: None,
		});
		let mut candidate = self.working.live_catalog().as_ref().clone();
		if let Some(previous) = &previous {
			candidate.replace_binding(definition.clone(), previous.version)?;
		} else if candidate.bindings.insert(id, definition.clone()).is_some() {
			return Err(FerrosGrynnError::invalid_argument(
				"LIVE LET identity is already registered",
			));
		}
		candidate.binding_order(&mut budget)?;
		self.record_read(ConflictKey::LiveCatalogRange);
		*self.working.live_catalog_mut() = Arc::new(candidate);
		self.insert_write(ConflictKey::LiveBinding(id), 0);
		self.write_bytes += cost;
		self.mark_range_change(ConflictKey::LiveCatalogRange);
		self.mutations.push(match previous {
			Some(previous) => Mutation::ReplaceLiveBinding {
				definition,
				expected: previous.version,
			},
			None => Mutation::DefineLiveBinding(definition),
		});
		Ok(())
	}
}

#[cfg(test)]
/// Regression tests for live binding behavior.
mod tests {
	use super::*;
	use crate::{
		datastore::{
			ClassField,
			ClassSchema,
			Datastore,
			FunctionEffect,
			SchemaFunction,
			SchemaFunctionParameter,
		},
		graph::AtomKind,
		live::{LiveBinaryOperator, LiveOptions, LiveProjection, LiveQuery, LiveSource},
		values::{PropertyMap, Type},
	};

	/// Metadata clones stay charged while their dependency output is being
	/// projected into an independent row.
	#[test]
	fn live_binding_info_retains_vector_admission() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let id = tx
			.define_live_binding(
				LiveExpr::Literal(1.into()),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let mut definition = tx
			.working
			.live_catalog()
			.bindings
			.get(&id)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture binding missing"))
			.assert_ok()
			.as_ref()
			.clone();
		definition.dependencies = vec![id; 256];
		definition.functions = vec![
			(
				crate::datastore::FunctionId::new(1)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("fixture function id is zero")
					})
					.assert_ok(),
				crate::datastore::SchemaVersion::INITIAL,
			);
			4096
		];
		let control = crate::gql::ExecutionControl::new(
			&crate::gql::QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::new(64 * 1024)
					.ok_or_else(|| FerrosGrynnError::invalid_argument("fixture budget is zero"))
					.assert_ok(),
				..crate::gql::QueryExecutionOptions::default()
			},
			None,
		);
		let (info, account) = definition.info_controlled(&control).assert_ok();
		let mut projection = control.barrier_account();
		let bytes = info.dependencies().len() * 128;
		assert!(!projection.reserve(bytes));
		drop(info);
		drop(account);
		assert!(projection.reserve(bytes));
	}

	/// All callbacks, views and maintenance use owned public types without GQL
	/// text.
	#[test]
	fn live_binding_frozen_inputs_replacements_savepoints_and_old_snapshots() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let mut parameters = Object::new();
		parameters.insert("fixed", Value::from(18_i64));
		let first = tx
			.define_live_binding(
				LiveExpr::Parameter("fixed".into()),
				parameters.clone(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		parameters.insert("fixed", Value::from(99_i64));
		let second = tx
			.define_live_binding(
				LiveExpr::Binding(first)
					.binary(LiveBinaryOperator::Add, LiveExpr::Literal(Value::from(2_i64))),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		assert_eq!(tx.live_binding_value(first).assert_ok(), Value::from(18_i64));
		let savepoint = tx.gql_savepoint();
		tx.replace_live_binding_value(first, Value::from(21_i64)).assert_ok();
		assert!(tx.live_binding_info(first).assert_ok().parameters().is_empty());
		tx.restore_gql_savepoint(savepoint);
		assert_eq!(tx.live_binding_value(first).assert_ok(), Value::from(18_i64));
		tx.commit().assert_ok();
		let mut old = store.begin();
		let mut tx = store.begin();
		tx.replace_live_binding_value(first, Value::from(25_i64)).assert_ok();
		let stamp = tx.commit().assert_ok().versionstamp();
		let mut current = store.begin();
		assert_eq!(current.live_binding_value(second).assert_ok(), Value::from(27_i64));
		assert_eq!(current.live_binding_info(second).assert_ok().revision().value(), stamp);
		assert_eq!(old.live_binding_value(second).assert_ok(), Value::from(20_i64));
		assert_eq!(current.live_binding_info(first).assert_ok().revision().definition().get(), 2);
	}

	/// Failed DAG edits leave the old recipe and all transaction mutations
	/// intact.
	#[test]
	fn live_binding_cycles_and_dependent_removal_are_atomic() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let first = tx
			.define_live_binding(
				LiveExpr::Literal(Value::from(1_i64)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let second = tx
			.define_live_binding(
				LiveExpr::Binding(first),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let mutations = tx.mutation_count();
		assert!(
			tx.replace_live_binding_expression(
				first,
				LiveExpr::Binding(second),
				Object::new(),
				LiveBindingOptions::default()
			)
			.is_err()
		);
		assert_eq!(tx.mutation_count(), mutations);
		assert!(tx.live_binding_info(first).assert_ok().dependencies().is_empty());
		assert!(tx.remove_live_binding(first).is_err());
		let live = tx
			.register_live(
				LiveQuery::new(LiveSource::Class(class))
					.with_projection(LiveProjection::Value(LiveExpr::Binding(first))),
				LiveOptions::default(),
			)
			.assert_ok();
		tx.remove_live_binding(second).assert_ok();
		assert!(tx.remove_live_binding(first).is_err());
		tx.close_live(live).assert_ok();
		tx.remove_live_binding(first).assert_ok();
		tx.commit().assert_ok();
		let mut current = store.begin();
		assert_eq!(current.live_binding_info(first).assert_ok().state(), LiveState::Closed);
		assert!(current.live_binding_value(first).is_err());
		assert_eq!(current.live_info(live).assert_ok().bindings().len(), 1);
	}

	/// Data and reactive parameter edits observe one final replay image.
	#[test]
	fn live_binding_select_uses_final_data_and_parameter_values() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let count = tx
			.define_live_binding(
				LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::Class(class)).with_projection(
						LiveProjection::Value(LiveExpr::Call {
							name: "count".into(),
							arguments: Vec::new(),
						}),
					),
				)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let threshold = tx
			.define_live_binding(
				LiveExpr::Literal(Value::from(10_i64)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let allowed = tx
			.define_live_binding(
				LiveExpr::Binding(count)
					.binary(LiveBinaryOperator::Greater, LiveExpr::Binding(threshold)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		tx.create_atom("person:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
		tx.create_atom("person:b".parse().assert_ok(), PropertyMap::new()).assert_ok();
		tx.replace_live_binding_value(threshold, Value::from(1_i64)).assert_ok();
		let stamp = tx.commit().assert_ok().versionstamp();
		let mut current = store.begin();
		assert_eq!(current.live_binding_value(count).assert_ok(), Value::from(2_i64));
		assert_eq!(current.live_binding_value(allowed).assert_ok(), Value::Bool(true));
		assert_eq!(current.live_binding_info(allowed).assert_ok().revision().value(), stamp);
	}

	/// Active evaluation faults pause affected identities while committing
	/// source DML.
	#[test]
	fn live_binding_fault_commits_primary_and_requires_explicit_replacement() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let record: crate::values::RecordId = "person:a".parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		let count = tx
			.define_live_binding(
				LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::Class(class)).with_projection(
						LiveProjection::Value(LiveExpr::Call {
							name: "count".into(),
							arguments: Vec::new(),
						}),
					),
				)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let inverse = tx
			.define_live_binding(
				LiveExpr::Literal(Value::from(10_i64))
					.binary(LiveBinaryOperator::Divide, LiveExpr::Binding(count)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let parent = tx
			.define_live_binding(
				LiveExpr::Binding(inverse)
					.binary(LiveBinaryOperator::Multiply, LiveExpr::Literal(Value::from(2_i64))),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		tx.commit().assert_ok();
		let mut old = store.begin();
		let mut tx = store.begin();
		tx.delete_atom(&record).assert_ok();
		tx.commit().assert_ok();
		let mut current = store.begin();
		assert!(current.atom(&record).assert_ok().is_none());
		assert_eq!(current.live_binding_info(inverse).assert_ok().state(), LiveState::Paused);
		assert_eq!(current.live_binding_info(parent).assert_ok().state(), LiveState::Paused);
		let diagnostic = current
			.live_binding_value(inverse)
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("test expected a binding fault"))
			.assert_ok();
		assert_eq!(diagnostic.status().code().to_string(), "FG-G1F09");
		assert!(diagnostic.cause().is_some());
		assert_eq!(old.live_binding_value(inverse).assert_ok(), Value::from(10_i64));
		current.replace_live_binding_value(inverse, Value::from(3_i64)).assert_ok();
		current
			.replace_live_binding_expression(
				parent,
				LiveExpr::Binding(inverse)
					.binary(LiveBinaryOperator::Multiply, LiveExpr::Literal(Value::from(2_i64))),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		current.commit().assert_ok();
		let mut current = store.begin();
		assert_eq!(current.live_binding_value(parent).assert_ok(), Value::from(6_i64));
	}

	/// Concrete named-function versions cannot silently change active
	/// expressions.
	#[test]
	fn live_recipe_call_pure_versions_pause_until_recaptured() {
		use crate::test_assertions::AssertResult as _;

		for named in [false, true] {
			let store = Datastore::in_memory();
			let mut tx = store.begin();
			let function = || {
				SchemaFunction::new(
					"fn",
					"identity",
					vec![SchemaFunctionParameter::new("value", Type::Any)?],
					Type::Any,
					FunctionEffect::Pure,
					false,
					None,
					"{ RETURN $value; }",
				)
			};
			tx.define_function(function().assert_ok()).assert_ok();
			let expression = if named {
				LiveExpr::CallWithOptions {
					name: "fn::identity".into(),
					type_argument: None,
					distinct: false,
					arguments: vec![(
						Some("value".into()),
						LiveExpr::Literal(Value::from(9_007_199_254_740_993_i64)),
					)],
				}
			} else {
				LiveExpr::Call {
					name: "fn::identity".into(),
					arguments: vec![LiveExpr::Literal(Value::from(9_007_199_254_740_993_i64))],
				}
			};
			let id = tx
				.define_live_binding(
					expression.clone(),
					Object::new(),
					LiveBindingOptions::default(),
				)
				.assert_ok();
			tx.commit().assert_ok();
			let mut tx = store.begin();
			assert_eq!(tx.live_binding_info(id).assert_ok().function_dependencies().len(), 1);
			let function_id = tx.live_binding_info(id).assert_ok().function_dependencies()[0].0;
			let published = tx
				.function_by_id(function_id)
				.assert_ok()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("test function is absent"))
				.assert_ok();
			tx.replace_function(published.as_ref().clone()).assert_ok();
			tx.commit().assert_ok();
			let mut tx = store.begin();
			assert_eq!(tx.live_binding_info(id).assert_ok().state(), LiveState::Paused);
			tx.replace_live_binding_expression(
				id,
				expression,
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
			tx.commit().assert_ok();
			let mut tx = store.begin();
			assert_eq!(
				tx.live_binding_value(id).assert_ok(),
				Value::from(9_007_199_254_740_993_i64)
			);
			assert_eq!(tx.live_binding_info(id).assert_ok().function_dependencies()[0].1.get(), 2);
		}
	}

	/// Exhausting an active evaluation cap preserves source DML atomically.
	#[test]
	fn live_binding_active_work_limit_pauses_after_successful_source_commit() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let id = tx
			.define_live_binding(
				LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::Class(class))
						.with_filter(
							LiveExpr::field("id").assert_ok().binary(
								LiveBinaryOperator::NotEqual,
								LiveExpr::Literal(Value::Null),
							),
						)
						.with_projection(LiveProjection::Value(LiveExpr::Call {
							name: "count".into(),
							arguments: Vec::new(),
						})),
				)),
				Object::new(),
				LiveBindingOptions::default().with_budgets(256 * 1024 * 1024, 500).assert_ok(),
			)
			.assert_ok();
		tx.commit().assert_ok();
		let mut tx = store.begin();
		for ordinal in 0..1024 {
			tx.create_atom(format!("person:{ordinal}").parse().assert_ok(), PropertyMap::new())
				.assert_ok();
		}
		tx.commit().assert_ok();
		let mut current = store.begin();
		assert!(current.atom(&"person:1023".parse().assert_ok()).assert_ok().is_some());
		assert_eq!(current.live_binding_info(id).assert_ok().state(), LiveState::Paused);
		assert_eq!(current.live_binding_info(id).assert_ok().value(), &Value::from(0_i64));
		assert!(current.live_binding_value(id).err().is_some_and(|error| error.cause().is_some()));
	}

	/// Concurrent independent writers replay/evaluate against the winning root.
	#[test]
	fn live_binding_concurrent_writers_rebase_final_values() {
		use crate::{
			datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
			test_assertions::AssertResult as _,
		};
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		let class =
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let id = tx
			.define_live_binding(
				LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::Class(class)).with_projection(
						LiveProjection::Value(LiveExpr::Call {
							name: "count".into(),
							arguments: Vec::new(),
						}),
					),
				)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		tx.commit().assert_ok();
		let start = Arc::new(std::sync::Barrier::new(4));
		let mut handles = Vec::new();
		for ordinal in 0..4 {
			let store = store.clone();
			let start = start.clone();
			handles.push(std::thread::spawn(move || -> FerrosGrynnResult<()> {
				let mut tx = store.begin_with(TransactionOptions::new(
					IsolationLevel::Snapshot,
					TransactionAccess::ReadWrite,
				));
				tx.create_atom(format!("person:{ordinal}").parse()?, PropertyMap::new())?;
				start.wait();
				tx.commit()?;
				Ok(())
			}));
		}
		for handle in handles {
			handle
				.join()
				.map_err(|_panic| FerrosGrynnError::invalid_argument("test writer panicked"))
				.assert_ok()
				.assert_ok();
		}
		assert_eq!(store.begin().live_binding_value(id).assert_ok(), Value::from(4_i64));
		for ordinal in 0..4 {
			assert!(
				store
					.begin()
					.atom(&format!("person:{ordinal}").parse().assert_ok())
					.assert_ok()
					.is_some()
			);
		}
	}

	/// Failed admission and failed final activation leave no published
	/// identity/data.
	#[test]
	fn live_binding_record_source_does_not_rebind_to_recreated_class() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		let expression = LiveExpr::Select(Box::new(
			LiveQuery::new(LiveSource::Record("person:a".parse().assert_ok())).with_projection(
				LiveProjection::Value(LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				}),
			),
		));
		let id = tx
			.define_live_binding(expression.clone(), Object::new(), LiveBindingOptions::default())
			.assert_ok();
		tx.commit().assert_ok();
		let mut tx = store.begin();
		tx.remove_class("person").assert_ok();
		tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
		tx.create_atom("person:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
		tx.commit().assert_ok();
		let mut current = store.begin();
		assert_eq!(current.live_binding_info(id).assert_ok().state(), LiveState::Paused);
		assert_eq!(current.live_binding_info(id).assert_ok().value(), &Value::from(0_i64));
		current
			.replace_live_binding_expression(
				id,
				expression,
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		current.commit().assert_ok();
		assert_eq!(store.begin().live_binding_value(id).assert_ok(), Value::from(1_i64));
	}

	/// Pending registrations retain their registration-time source incarnation.
	#[test]
	fn live_binding_pending_registration_cannot_recapture_recreated_source() {
		use crate::test_assertions::AssertResult as _;

		for replace in [false, true] {
			let store = Datastore::in_memory();
			let mut setup = store.begin();
			setup
				.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
				.assert_ok();
			let expression = LiveExpr::Select(Box::new(
				LiveQuery::new(LiveSource::Record("person:a".parse().assert_ok())).with_projection(
					LiveProjection::Value(LiveExpr::Call {
						name: "count".into(),
						arguments: Vec::new(),
					}),
				),
			));
			let id = setup
				.define_live_binding(
					expression.clone(),
					Object::new(),
					LiveBindingOptions::default(),
				)
				.assert_ok();
			setup.commit().assert_ok();
			let mut tx = store.begin();
			let pending = if replace {
				tx.replace_live_binding_expression(
					id,
					expression,
					Object::new(),
					LiveBindingOptions::default(),
				)
				.assert_ok();
				id
			} else {
				tx.define_live_binding(expression, Object::new(), LiveBindingOptions::default())
					.assert_ok()
			};
			tx.remove_class("person").assert_ok();
			tx.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok()).assert_ok();
			tx.create_atom("person:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
			assert!(tx.commit().is_err());
			assert!(store.begin().atom(&"person:a".parse().assert_ok()).assert_ok().is_none());
			if !replace {
				assert!(store.begin().live_binding_info(pending).is_err());
			}
			assert_eq!(store.begin().live_binding_value(id).assert_ok(), Value::from(0_i64));
		}
	}

	/// Binding revisions reported through subscription metadata participate in
	/// SSI.
	#[test]
	fn live_binding_info_reads_prevent_stale_serializable_writes() {
		use crate::{
			datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
			test_assertions::AssertResult as _,
		};
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(ClassSchema::new("person", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let count = setup
			.define_live_binding(
				LiveExpr::Select(Box::new(
					LiveQuery::new(LiveSource::Class(class)).with_projection(
						LiveProjection::Value(LiveExpr::Call {
							name: "count".into(),
							arguments: Vec::new(),
						}),
					),
				)),
				Object::new(),
				LiveBindingOptions::default(),
			)
			.assert_ok();
		let id = setup
			.register_live(
				LiveQuery::new(LiveSource::Class(class))
					.with_projection(LiveProjection::Value(LiveExpr::Binding(count))),
				LiveOptions::default(),
			)
			.assert_ok();
		setup.commit().assert_ok();
		for explicit in [false, true] {
			let mut reader = store.begin_with(TransactionOptions::new(
				IsolationLevel::Serializable,
				TransactionAccess::ReadWrite,
			));
			let before = reader.live_info(id).assert_ok().bindings()[0].value();
			let mut writer = store.begin();
			if explicit {
				writer.replace_live_binding_value(count, Value::from(7_i64)).assert_ok();
			} else {
				writer.create_atom("person:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
			}
			let stamp = writer.commit().assert_ok().versionstamp();
			assert!(stamp > before);
			reader
				.create_atom(
					if explicit {
						"person:explicit"
					} else {
						"person:automatic"
					}
					.parse()
					.assert_ok(),
					PropertyMap::new(),
				)
				.assert_ok();
			assert!(reader.commit().is_err());
		}
	}

	/// Failed admission and failed final activation leave no published
	/// identity/data.
	#[test]
	fn live_binding_resource_and_final_registration_faults_publish_nothing() {
		use crate::test_assertions::AssertResult as _;

		let store = Datastore::in_memory();
		let mut tx = store.begin();
		assert!(
			tx.define_live_binding(
				LiveExpr::Literal(Value::from(1_i64)),
				Object::new(),
				LiveBindingOptions::default().with_budgets(16, 1).assert_ok()
			)
			.is_err()
		);
		assert!(tx.working.live_catalog().bindings.is_empty());
		tx.define_class(
			ClassSchema::new("person", AtomKind::Node, [ClassField::new("age", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
		let age = tx.intern_property("age").assert_ok();
		let record: crate::values::RecordId = "person:a".parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::from_iter([(age, Value::from(1_i64))]))
			.assert_ok();
		let age_value = LiveExpr::Select(Box::new(
			LiveQuery::new(LiveSource::Record(record.clone()))
				.with_projection(LiveProjection::Value(LiveExpr::field("age").assert_ok())),
		));
		tx.define_live_binding(
			LiveExpr::Literal(Value::from(1_i64)).binary(LiveBinaryOperator::Divide, age_value),
			Object::new(),
			LiveBindingOptions::default(),
		)
		.assert_ok();
		tx.set_base_property(&record, age, Value::from(0_i64)).assert_ok();
		assert!(tx.commit().is_err());
		let mut current = store.begin();
		assert!(current.atom(&record).assert_ok().is_none());
		assert!(current.working.live_catalog().bindings.is_empty());
	}
}
