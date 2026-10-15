//! Streaming traversal transformations, filters, state, and control steps.

use std::num::NonZeroU64;

use super::*;

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	/// Applies stream for `TraversalMachine<'_, Read>`.
	pub(super) fn apply_stream(
		&mut self,
		stage: usize,
		step: &PreparedStep,
		mut traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		self.control.check()?;
		if navigation::is_navigation(step.kind) {
			return self.navigate(stage, step, traverser);
		}
		match step.kind {
			TraverseStepKind::Where => {
				let predicate =
					self.invoke_argument(step, 0, &[traverser.current.clone()], false)?;
				Ok(predicate.is_truthy().then_some(traverser).into_iter().collect())
			}
			TraverseStepKind::HasAttribute => {
				let field = string_argument(step, 0)?;
				let present = !self.field_value(&traverser.current, field)?.is_none();
				Ok(present.then_some(traverser).into_iter().collect())
			}
			TraverseStepKind::Has => {
				let field = string_argument(step, 0)?;
				let value = self.field_value(&traverser.current, field)?;
				let accepted = step.arguments.get(1).is_none_or(|expected| value == *expected);
				Ok(accepted.then_some(traverser).into_iter().collect())
			}
			TraverseStepKind::HasLabel => {
				let label = self.field_value(&traverser.current, "label")?;
				let accepted = step.arguments.iter().any(|expected| expected == &label);
				Ok(accepted.then_some(traverser).into_iter().collect())
			}
			TraverseStepKind::Map => {
				traverser.current =
					self.invoke_argument(step, 0, &[traverser.current.clone()], false)?;
				Ok(vec![traverser])
			}
			TraverseStepKind::FlatMap => {
				let values = expand_value(self.invoke_argument(
					step,
					0,
					&[traverser.current.clone()],
					false,
				)?)?;
				Ok(values
					.into_iter()
					.map(|value| {
						let mut output = traverser.clone();
						output.current = value;
						output
					})
					.collect())
			}
			TraverseStepKind::Algorithm => {
				let target = step.algorithm_input.as_ref().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("tv::algo input type is missing.")
				})?;
				let current = cast(
					traverser.current.clone(),
					target,
					self.read.bounded_fields(),
					Some(self.control),
				)?;
				let offset = usize::from(step.named_function.is_none());
				let mut arguments = Vec::with_capacity(step.arguments.len() + 1 - offset);
				arguments.push(current);
				arguments.extend(step.arguments[offset..].iter().cloned());
				let value = if let Some(function) = step.named_function {
					let plan = self.control.resolve_named_function(function)?;
					invoke_logical_closure_with_read(
						self.read,
						&plan,
						&[],
						&arguments,
						self.source,
						self.control,
						self.runtime,
						ClosureInvocationMode::ReadOnlyTraversal,
					)?
				} else {
					self.invoke(closure_argument(step, 0)?, &arguments, false)?
				};
				Ok(expand_algorithm_value(value)?
					.into_iter()
					.map(|value| {
						let mut output = traverser.clone();
						output.current = value;
						output
					})
					.collect())
			}
			TraverseStepKind::Unfold => {
				let values = expand_value(traverser.current.clone())?;
				Ok(values
					.into_iter()
					.map(|value| {
						let mut output = traverser.clone();
						output.current = value;
						output
					})
					.collect())
			}
			TraverseStepKind::Values => {
				let mut values = Vec::with_capacity(step.arguments.len());
				for argument in &step.arguments {
					let field = argument.as_str().ok_or_else(|| {
						FerrosGrynnError::traversal_source_type_mismatch(
							"property name string".to_owned(),
							argument.kind().to_string(),
						)
					})?;
					let value = self.field_value(&traverser.current, field)?;
					if !value.is_none() {
						values.push(value);
					}
				}
				Ok(values
					.into_iter()
					.map(|value| {
						let mut output = traverser.clone();
						output.current = value;
						output
					})
					.collect())
			}
			TraverseStepKind::As => {
				let name = string_argument(step, 0)?;
				traverser.env.insert(name, traverser.current.clone());
				Ok(vec![traverser])
			}
			TraverseStepKind::Select => {
				if step.arguments.len() == 1 {
					let name = string_argument(step, 0)?;
					traverser.current = traverser.env.get(name).cloned().unwrap_or(Value::None);
				} else {
					let mut selected = Object::new();
					for argument in &step.arguments {
						let name = argument.as_str().ok_or_else(|| {
							FerrosGrynnError::traversal_source_type_mismatch(
								"binding name string".to_owned(),
								argument.kind().to_string(),
							)
						})?;
						selected
							.insert(name, traverser.env.get(name).cloned().unwrap_or(Value::None));
					}
					traverser.current = Value::Object(selected);
				}
				Ok(vec![traverser])
			}
			TraverseStepKind::Project => {
				let mut projected = Object::new();
				for argument in &step.arguments {
					let name = argument.as_str().ok_or_else(|| {
						FerrosGrynnError::traversal_source_type_mismatch(
							"projection name string".to_owned(),
							argument.kind().to_string(),
						)
					})?;
					projected.insert(name, traverser.env.get(name).cloned().unwrap_or(Value::None));
				}
				traverser.current = Value::Object(projected);
				Ok(vec![traverser])
			}
			TraverseStepKind::InitializeSack => {
				traverser.sack = step.arguments[0].clone();
				Ok(vec![traverser])
			}
			TraverseStepKind::Sack => {
				traverser.sack = self.invoke_argument(
					step,
					0,
					&[traverser.sack.clone(), traverser.current.clone()],
					false,
				)?;
				Ok(vec![traverser])
			}
			TraverseStepKind::GetSack => {
				traverser.current = traverser.sack.clone();
				Ok(vec![traverser])
			}
			TraverseStepKind::InitializeNamedSack => {
				let name = string_argument(step, 0)?;
				traverser.named_sacks.insert(name, step.arguments[1].clone());
				Ok(vec![traverser])
			}
			TraverseStepKind::NamedSack => {
				let name = string_argument(step, 0)?;
				let previous = traverser.named_sacks.get(name).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_argument(format!(
						"Named sack `{name}` has not been initialized."
					))
				})?;
				let closure = closure_argument(step, 1)?;
				let value = self.invoke(closure, &[previous, traverser.current.clone()], false)?;
				traverser.named_sacks.insert(name, value);
				Ok(vec![traverser])
			}
			TraverseStepKind::GetNamedSack => {
				let name = string_argument(step, 0)?;
				traverser.current = traverser.named_sacks.get(name).cloned().ok_or_else(|| {
					FerrosGrynnError::invalid_argument(format!(
						"Named sack `{name}` has not been initialized."
					))
				})?;
				Ok(vec![traverser])
			}
			TraverseStepKind::SideEffect => {
				let name = string_argument(step, 0)?.to_owned();
				let closure = closure_argument(step, 1)?.clone();
				let mut value =
					self.side_effects.get(name.as_str()).cloned().unwrap_or(Value::None);
				for _ in 0..traverser.bulk.get() {
					value = self.invoke(&closure, &[traverser.current.clone(), value], true)?;
				}
				self.side_effects.insert(name.as_str(), value.clone());
				traverser.env.insert(name.as_str(), value);
				Ok(vec![traverser])
			}
			TraverseStepKind::Limit => self.limit(stage, traverser),
			TraverseStepKind::Skip => self.skip(stage, traverser),
			TraverseStepKind::SimplePath | TraverseStepKind::CyclicPath => {
				let path = self.materialize_path(&traverser)?;
				#[allow(
					clippy::mutable_key_type,
					reason = "occurrence ordering uses immutable graph and atom identities"
				)]
				let mut seen = std::collections::BTreeSet::new();
				let simple = path.steps().iter().all(|step| seen.insert(step.atom().clone()));
				let accepted = if step.kind == TraverseStepKind::SimplePath {
					simple
				} else {
					!simple
				};
				Ok(accepted.then_some(traverser).into_iter().collect())
			}
			TraverseStepKind::Path => {
				traverser.current = Value::Path(Box::new(self.materialize_path(&traverser)?));
				Ok(vec![traverser])
			}
			TraverseStepKind::Repeat => self.repeat(step, traverser),
			TraverseStepKind::Branch => self.branch(step, traverser),
			TraverseStepKind::Choose => self.choose(step, traverser),
			TraverseStepKind::Coalesce => self.coalesce(step, traverser),
			TraverseStepKind::Emit => Ok(vec![traverser]),
			TraverseStepKind::Match => self.match_child(step, traverser),
			kind if barrier::is_barrier(kind) => Err(FerrosGrynnError::invalid_argument(
				"A traversal barrier reached the streaming executor.",
			)),
			kind => Err(FerrosGrynnError::query_feature_not_supported(format!(
				"traversal step {kind:?}"
			))),
		}
	}

	/// Matches child for `TraversalMachine<'_, Read>`.
	fn match_child(
		&self,
		step: &PreparedStep,
		traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let plan = step.match_plan.as_deref().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("tv::match has no typed MATCH child plan.")
		})?;
		let value = execute_match_with_environment(
			&*self.read,
			self.snapshot,
			plan,
			self.slots,
			self.source,
			self.control,
			&traverser.env,
		)?;
		let Value::Array(rows) = value else {
			return Err(FerrosGrynnError::invalid_argument("MATCH child did not return an array."));
		};
		let mut output = Vec::with_capacity(rows.len());
		for row in rows {
			let Value::Object(row) = row else {
				return Err(FerrosGrynnError::invalid_argument(
					"MATCH child row is not an object.",
				));
			};
			let mut next = traverser.clone();
			for (name, value) in &row {
				next.env.insert(name.clone(), value.clone());
			}
			next.current = plan
				.bindings
				.columns
				.iter()
				.rev()
				.filter_map(|column| row.get(&column.name))
				.find(|value| matches!(value, Value::AtomOccurrence(_)))
				.cloned()
				.unwrap_or(Value::Object(row));
			output.push(next);
		}
		Ok(output)
	}

	/// Returns invoke argument for `TraversalMachine<'_, Read>`.
	pub(super) fn invoke_argument(
		&mut self,
		step: &PreparedStep,
		position: usize,
		arguments: &[Value],
		allow_mutation: bool,
	) -> FerrosGrynnResult<Value> {
		self.invoke(closure_argument(step, position)?, arguments, allow_mutation)
	}

	/// Returns invoke for `TraversalMachine<'_, Read>`.
	fn invoke(
		&mut self,
		closure: &Closure,
		arguments: &[Value],
		allow_mutation: bool,
	) -> FerrosGrynnResult<Value> {
		let value = invoke_closure(
			self.read,
			closure,
			arguments,
			self.source,
			self.control,
			self.runtime,
			if allow_mutation {
				ClosureInvocationMode::TraversalSideEffect
			} else {
				ClosureInvocationMode::ReadOnlyTraversal
			},
		)?;
		if allow_mutation {
			self.snapshot = self.read.refresh_snapshot()?;
			self.occurrences.clear();
			self.local_indexes.clear();
			self.scoped_indexes.clear();
		}
		Ok(value)
	}

	/// Returns limit for `TraversalMachine<'_, Read>`.
	fn limit(
		&mut self,
		stage: usize,
		mut traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let StageState::Limit(remaining) = &mut self.states[stage] else {
			return Err(FerrosGrynnError::invalid_argument(
				"Traversal limit state is unavailable.",
			));
		};
		let take = traverser.bulk.get().min(*remaining);
		*remaining -= take;
		let Some(take) = NonZeroU64::new(take) else {
			return Ok(Vec::new());
		};
		traverser.bulk = take;
		Ok(vec![traverser])
	}

	/// Returns skip for `TraversalMachine<'_, Read>`.
	fn skip(
		&mut self,
		stage: usize,
		mut traverser: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let StageState::Skip(remaining) = &mut self.states[stage] else {
			return Err(FerrosGrynnError::invalid_argument("Traversal skip state is unavailable."));
		};
		let skipped = traverser.bulk.get().min(*remaining);
		*remaining -= skipped;
		let Some(retained) = NonZeroU64::new(traverser.bulk.get() - skipped) else {
			return Ok(Vec::new());
		};
		traverser.bulk = retained;
		Ok(vec![traverser])
	}

	/// Returns repeat for `TraversalMachine<'_, Read>`.
	fn repeat(
		&mut self,
		step: &PreparedStep,
		input: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let repeat = step.repeat.as_ref().ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Traversal repeat plan is missing its body.")
		})?;
		let mut frontier_memory = self.control.barrier_account();
		reserve_traverser(&mut frontier_memory, &input, self.control, "repeat frontier")?;
		let mut active = vec![input];
		let mut emitted = Vec::new();
		let maximum = repeat.times.unwrap_or(self.control.max_loop_iterations as usize);
		for _ in 0..maximum {
			let mut next = Vec::new();
			for traverser in active {
				frontier_memory.release(traverser.estimated_heap_bytes());
				record_iteration(self.runtime, self.control)?;
				let value =
					self.invoke(&repeat.body, std::slice::from_ref(&traverser.current), false)?;
				let values = collection_or_one(value)?;
				for value in values {
					let mut output = traverser.clone();
					output.current = value;
					let done = if let Some(closure) = &repeat.until {
						self.invoke(closure, &[output.current.clone()], false)?.is_truthy()
					} else {
						false
					};
					if repeat.emit || done {
						reserve_traverser(
							&mut frontier_memory,
							&output,
							self.control,
							"repeat emitted states",
						)?;
						emitted.push(output.clone());
					}
					if !done {
						reserve_traverser(
							&mut frontier_memory,
							&output,
							self.control,
							"repeat frontier",
						)?;
						next.push(output);
					}
				}
			}
			active = next;
			if active.is_empty() {
				break;
			}
		}
		if !repeat.emit {
			if repeat.times.is_none() && !active.is_empty() {
				return Err(FerrosGrynnError::control_limit_exceeded(
					"loop iterations",
					self.control.max_loop_iterations,
				));
			}
			emitted.extend(active);
		} else {
			for terminal in active {
				if !emitted.iter().any(|value| value.can_merge(&terminal)) {
					emitted.push(terminal);
				} else {
					frontier_memory.release(terminal.estimated_heap_bytes());
				}
			}
		}
		Ok(emitted)
	}

	/// Returns branch for `TraversalMachine<'_, Read>`.
	fn branch(
		&mut self,
		step: &PreparedStep,
		input: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let mut output = Vec::new();
		for argument in &step.arguments {
			let closure = value_closure(argument)?;
			for value in collection_or_one(self.invoke(
				closure,
				std::slice::from_ref(&input.current),
				false,
			)?)? {
				let mut branch = input.clone();
				branch.current = value;
				output.push(branch);
			}
		}
		Ok(output)
	}

	/// Returns choose for `TraversalMachine<'_, Read>`.
	fn choose(
		&mut self,
		step: &PreparedStep,
		input: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		let predicate =
			self.invoke_argument(step, 0, std::slice::from_ref(&input.current), false)?;
		let branch = if predicate.is_truthy() {
			1
		} else {
			2
		};
		let Some(closure) = step.arguments.get(branch) else {
			return Ok(if predicate.is_truthy() {
				vec![input]
			} else {
				Vec::new()
			});
		};
		let mut output = Vec::new();
		for value in collection_or_one(self.invoke(
			value_closure(closure)?,
			std::slice::from_ref(&input.current),
			false,
		)?)? {
			let mut selected = input.clone();
			selected.current = value;
			output.push(selected);
		}
		Ok(output)
	}

	/// Returns coalesce for `TraversalMachine<'_, Read>`.
	fn coalesce(
		&mut self,
		step: &PreparedStep,
		input: Traverser,
	) -> FerrosGrynnResult<Vec<Traverser>> {
		for argument in &step.arguments {
			let savepoint = self.read.savepoint();
			let values = collection_or_one(self.invoke(
				value_closure(argument)?,
				std::slice::from_ref(&input.current),
				false,
			)?)?;
			if !values.is_empty() {
				return Ok(values
					.into_iter()
					.map(|value| {
						let mut selected = input.clone();
						selected.current = value;
						selected
					})
					.collect());
			}
			self.read.restore_savepoint(savepoint)?;
			self.snapshot = self.read.refresh_snapshot()?;
			self.occurrences.clear();
			self.local_indexes.clear();
			self.scoped_indexes.clear();
		}
		Ok(Vec::new())
	}
}

/// Reserves traverser for this module.
fn reserve_traverser(
	account: &mut BarrierAccount,
	traverser: &Traverser,
	control: &ExecutionControl,
	resource: &str,
) -> FerrosGrynnResult<()> {
	if account.reserve(traverser.estimated_heap_bytes()) {
		Ok(())
	} else {
		Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			resource.to_owned(),
			control.barrier_memory_limit() as u64,
		))
	}
}

/// Returns string argument for this module.
fn string_argument(step: &PreparedStep, position: usize) -> FerrosGrynnResult<&str> {
	step.arguments.get(position).and_then(Value::as_str).ok_or_else(|| {
		FerrosGrynnError::traversal_source_type_mismatch(
			"string".to_owned(),
			step.arguments
				.get(position)
				.map_or_else(|| "missing".to_owned(), |value| value.kind().to_string()),
		)
	})
}

/// Returns closure argument for this module.
fn closure_argument(step: &PreparedStep, position: usize) -> FerrosGrynnResult<&Closure> {
	step.arguments.get(position).map_or_else(
		|| {
			Err(FerrosGrynnError::traversal_source_type_mismatch(
				"closure".to_owned(),
				"missing".to_owned(),
			))
		},
		value_closure,
	)
}

/// Returns value closure for this module.
fn value_closure(value: &Value) -> FerrosGrynnResult<&Closure> {
	let Value::Closure(closure) = value else {
		return Err(FerrosGrynnError::traversal_source_type_mismatch(
			"closure".to_owned(),
			value.kind().to_string(),
		));
	};
	Ok(closure)
}

/// Returns expand algorithm value for this module.
fn expand_algorithm_value(value: Value) -> FerrosGrynnResult<Vec<Value>> {
	match value {
		Value::Array(values) => Ok(values.into_iter().collect()),
		Value::Set(values) => Ok(values.into_iter().collect()),
		Value::Range(range) => Ok(super::integer_range(range)?.collect()),
		Value::None => Ok(Vec::new()),
		value => Ok(vec![value]),
	}
}

/// Returns collection or one for this module.
fn collection_or_one(value: Value) -> FerrosGrynnResult<Vec<Value>> {
	match value {
		Value::Array(values) => Ok(values.into_iter().collect()),
		Value::Set(values) => Ok(values.into_iter().collect()),
		Value::Range(range) => Ok(super::integer_range(range)?.collect()),
		Value::None => Ok(Vec::new()),
		value => Ok(vec![value]),
	}
}
