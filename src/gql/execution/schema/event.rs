//! Event DDL delegates source compilation and mutations to the same SDK
//! boundary.

use crate::{
	datastore::{EventDefinition, EventExecution, EventTarget, Transaction},
	gql::{
		ExecutionControl,
		ir::{LogicalEventChange, LogicalEventOwner, LogicalEventStatement},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	syn::DefinitionMode,
	values::{Object, Value},
};

/// Applies one checked lifecycle command with the caller's compilation budget.
pub(super) fn execute(
	transaction: &mut Transaction,
	statement: &LogicalEventStatement,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	control.check()?;
	match statement {
		LogicalEventStatement::Define {
			mode,
			name,
			owner,
			execution,
			when,
			then,
		} => {
			let target = resolve_owner(transaction, owner, false)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("EVENT source is absent"))?;
			let existing = transaction.named_event(target, name)?;
			if existing.is_some() && *mode == DefinitionMode::IfNotExists {
				return Ok(Value::None);
			}
			let definition =
				EventDefinition::new(name.as_ref(), target, when.clone(), then.clone())?
					.with_execution(*execution);
			match (existing, mode) {
				(Some(existing), DefinitionMode::Overwrite) => {
					let id = existing.id().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("published EVENT has no identity")
					})?;
					transaction.replace_event_controlled(id, definition, Some(control))?;
				}
				_ => {
					let _id = transaction.define_event_controlled(definition, Some(control))?;
				}
			}
		}
		LogicalEventStatement::Remove {
			if_exists,
			name,
			owner,
		} => {
			let Some(target) = resolve_owner(transaction, owner, *if_exists)? else {
				return Ok(Value::None);
			};
			match transaction.named_event(target, name)? {
				Some(event) => transaction.remove_event(event.id().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("published EVENT has no identity")
				})?)?,
				None if *if_exists => {}
				None => return Err(FerrosGrynnError::invalid_argument("EVENT does not exist")),
			}
		}
		LogicalEventStatement::Alter {
			if_exists,
			name,
			owner,
			actions,
		} => {
			let Some(target) = resolve_owner(transaction, owner, *if_exists)? else {
				return Ok(Value::None);
			};
			let Some(current) = transaction.named_event(target, name)? else {
				if *if_exists {
					return Ok(Value::None);
				}
				return Err(FerrosGrynnError::invalid_argument("EVENT does not exist"));
			};
			let id = current.id().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("published EVENT has no identity")
			})?;
			let mut replacement = current.as_ref().clone();
			for action in actions {
				control.check()?;
				match action {
					LogicalEventChange::Execution(value) => replacement.execution = *value,
					LogicalEventChange::When(value) => replacement.when = value.clone(),
					LogicalEventChange::Then(value) => replacement.then = value.clone(),
					LogicalEventChange::Comment(value) => replacement.comment = value.clone(),
				}
			}
			transaction.replace_event_controlled(id, replacement, Some(control))?;
		}
	}
	Ok(Value::None)
}

/// Resolves one stable owner and distinguishes absence from lookup failures.
fn resolve_owner(
	transaction: &mut Transaction,
	owner: &LogicalEventOwner,
	if_exists: bool,
) -> FerrosGrynnResult<Option<EventTarget>> {
	let target = match owner {
		LogicalEventOwner::Class(name) => {
			transaction.class(name)?.and_then(|class| class.id()).map(EventTarget::Class)
		}
		LogicalEventOwner::Graph(name) => {
			transaction.named_graph(name)?.map(|graph| EventTarget::Graph(graph.id()))
		}
	};
	if target.is_none() && !if_exists {
		return Err(FerrosGrynnError::invalid_argument("EVENT source does not exist"));
	}
	Ok(target)
}

/// Projects checked source definitions with admission before object growth.
pub(super) fn info(
	transaction: &mut Transaction,
	target: EventTarget,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Object> {
	let definitions = transaction.events_for_source(target)?;
	let mut account = control.barrier_account();
	let mut result = Object::new();
	for event in definitions {
		control.check()?;
		control.admit_reactive_work(1)?;
		let bytes = event
			.name()
			.len()
			.checked_add(event.when().len())
			.and_then(|bytes| bytes.checked_add(event.then().len()))
			.and_then(|bytes| bytes.checked_add(event.comment().map_or(0, |value| value.len())))
			.and_then(|bytes| bytes.checked_add(2048))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("EVENT INFO bytes"))?;
		if !account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"EVENT INFO scratch",
				bytes as u64,
			));
		}
		let (asynchronous, retries, depth) = match event.execution() {
			EventExecution::Sync => (false, Value::None, Value::None),
			EventExecution::Async(options) => {
				(true, Value::from(options.retries()), Value::from(options.max_depth()))
			}
		};
		let id = event
			.id()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("published EVENT has no identity"))?;
		result.insert(
			event.name().clone(),
			Object::from_iter([
				("id", Value::from(id.get())),
				("version", Value::from(event.version().get())),
				("async", Value::Bool(asynchronous)),
				("retry", retries),
				("max_depth", depth),
				("when", Value::from(event.when().clone())),
				("then", Value::from(event.then().clone())),
				("comment", event.comment().cloned().map_or(Value::None, Value::from)),
			]),
		);
	}
	Ok(result)
}
