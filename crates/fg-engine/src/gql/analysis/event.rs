//! Event DDL lowering preserves source recipes for shared SDK compilation.

use super::{Analyzer, LogicalSchemaStatement, Text};
use crate::{
	datastore::{EventAsyncOptions, EventDefinition, EventExecution},
	gql::ir::{LogicalEventChange, LogicalEventOwner, LogicalEventStatement},
	gql_status::{FerrosGrynnResult, TextSpan},
	syn::{DefineEventStatement, EventAlterAction, EventBody, EventOwnerName},
};

impl Analyzer<'_> {
	/// Retains the source after parser validation; SDK performs effect checks.
	pub(super) fn define_event(
		&self,
		definition: &DefineEventStatement,
	) -> FerrosGrynnResult<LogicalSchemaStatement> {
		let execution = match definition.asynchronous {
			None => EventExecution::Sync,
			Some(options) => {
				EventExecution::Async(EventAsyncOptions::new(options.retries, options.max_depth)?)
			}
		};
		Ok(LogicalSchemaStatement::Event(LogicalEventStatement::Define {
			mode: definition.mode,
			name: definition.name.value.clone(),
			owner: event_owner(&definition.owner),
			execution,
			when: self.event_source(definition.when.span)?,
			then: self.event_body_source(&definition.then)?,
		}))
	}

	/// Lowers ordered changes without executing predicate or handler
	/// expressions.
	pub(super) fn event_changes(
		&self,
		actions: &[EventAlterAction],
	) -> FerrosGrynnResult<Vec<LogicalEventChange>> {
		actions
			.iter()
			.map(|action| {
				Ok(match action {
					EventAlterAction::Async(options) => {
						LogicalEventChange::Execution(EventExecution::Async(
							EventAsyncOptions::new(options.retries, options.max_depth)?,
						))
					}
					EventAlterAction::Sync => LogicalEventChange::Execution(EventExecution::Sync),
					EventAlterAction::When(expression) => {
						LogicalEventChange::When(self.event_source(expression.span)?)
					}
					EventAlterAction::DropWhen => LogicalEventChange::When("true".into()),
					EventAlterAction::Then(body) => {
						LogicalEventChange::Then(self.event_body_source(body)?)
					}
					EventAlterAction::DropThen => LogicalEventChange::Then("none".into()),
					EventAlterAction::Comment(value) => {
						LogicalEventChange::Comment(Some(value.as_ref().into()))
					}
					EventAlterAction::DropComment => LogicalEventChange::Comment(None),

					_ => {
						return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"Unsupported syntax variant at the engine boundary.",
						));
					}
				})
			})
			.collect()
	}

	/// Checks source length before retaining an owned source descriptor.
	fn event_source(&self, span: TextSpan) -> FerrosGrynnResult<Text> {
		let source = self.source_slice(span)?;
		if source.len() > EventDefinition::source_byte_limit() {
			return Err(self.semantic("EVENT source exceeds 64 KiB.", span));
		}
		self.catalog.admit_analysis(source.len(), 1)?;
		Ok(source.into())
	}

	/// Expression lists use the ordinary ordered array evaluator as one body.
	fn event_body_source(&self, body: &EventBody) -> FerrosGrynnResult<Text> {
		if let EventBody::Expressions {
			span,
			..
		} = body
		{
			let source = self.source_slice(*span)?;
			let bytes = source
				.len()
				.checked_add(2)
				.ok_or_else(|| self.semantic("EVENT source length overflows.", *span))?;
			if bytes > EventDefinition::source_byte_limit() {
				return Err(self.semantic("EVENT source exceeds 64 KiB.", *span));
			}
			self.catalog.admit_analysis(bytes, 1)?;
			Ok(format!("[{source}]").into())
		} else {
			self.event_source(body.span())
		}
	}
}

/// Names remain logical until the shared transaction API resolves the owner.
pub(super) fn event_owner(owner: &EventOwnerName) -> LogicalEventOwner {
	fg_syntax::__internal::map_event_owner(
		owner,
		|name| LogicalEventOwner::Class(name.value.clone()),
		|name| LogicalEventOwner::Graph(name.value.clone()),
	)
}
