//! Event DDL syntax retains source expressions for the common analyzer.

use reblessive::Stk;

use super::{Block, DefinitionMode, Expr, Identifier, Parser, Symbol, TokenKind, cover};
use crate::gql_status::{FerrosGrynnResult, TextSpan};

/// Source owner named by event DDL before stable identity resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventOwnerName {
	/// Database-global class namespace.
	Class(Identifier),
	/// Graph occurrence namespace.
	Graph(Identifier),
}

/// Bounded asynchronous execution syntax, independent of runtime policy types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventAsyncName {
	/// Additional attempts after the initial attempt.
	pub retries: u8,
	/// Inclusive causal-depth ceiling.
	pub max_depth: u8,
}

/// An expression or ordinary lexical statement block used as a handler.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EventBody {
	/// One expression, evaluated through the common expression kernel.
	Expression(Expr),
	/// A lexical block with ordinary control flow.
	Block(Block),
	/// Ordered expression list accepted by ALTER EVENT THEN.
	Expressions {
		/// Expressions evaluated in declaration order.
		values: Vec<Expr>,
		/// Source range including separators between expressions.
		span: TextSpan,
	},
}

impl EventBody {
	/// Returns the source range including any block braces.
	pub const fn span(&self) -> TextSpan {
		match self {
			Self::Expression(expression) => expression.span,
			Self::Block(block) => block.span,
			Self::Expressions {
				span,
				..
			} => *span,
		}
	}
}

/// Source-owned event definition before semantic effect checks.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineEventStatement {
	/// Duplicate-definition behavior.
	pub mode: DefinitionMode,
	/// Name within one source owner.
	pub name: Identifier,
	/// Explicit class or graph owner.
	pub owner: EventOwnerName,
	/// None selects synchronous execution.
	pub asynchronous: Option<EventAsyncName>,
	/// Read-only predicate syntax.
	pub when: Expr,
	/// Handler expression or block.
	pub then: EventBody,
}

/// Ordered event changes preserve the ordinary ALTER last-action semantics.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EventAlterAction {
	/// Enables durable asynchronous execution with a checked policy.
	Async(EventAsyncName),
	/// Restores synchronous execution.
	Sync,
	/// Replaces the predicate source.
	When(Expr),
	/// Removes the predicate; every logical change matches.
	DropWhen,
	/// Replaces the handler source.
	Then(EventBody),
	/// Removes the handler; its body becomes the ordinary none expression.
	DropThen,
	/// Sets a durable comment.
	Comment(Box<str>),
	/// Removes the durable comment.
	DropComment,
}

impl Parser<'_> {
	/// Parses an explicit event source without interpreting catalog names.
	pub(in crate::syn) fn event_owner(&mut self) -> FerrosGrynnResult<EventOwnerName> {
		self.expect_keyword("ON", "Expected ON before EVENT source.")?;
		if self.consume_keyword("CLASS").is_some() {
			Ok(EventOwnerName::Class(self.identifier()?))
		} else {
			self.expect_keyword("GRAPH", "Expected CLASS or GRAPH after ON.")?;
			Ok(EventOwnerName::Graph(self.identifier()?))
		}
	}

	/// Parses the options after ASYNC with separate retry and depth ceilings.
	fn event_async(&mut self) -> FerrosGrynnResult<EventAsyncName> {
		let retries = if self.consume_keyword("RETRY").is_some() {
			self.event_integer(255, "RETRY")?
		} else {
			0
		};
		let max_depth = if self.consume_keyword("MAXDEPTH").is_some() {
			self.event_integer(16, "MAXDEPTH")?
		} else {
			3
		};
		Ok(EventAsyncName {
			retries,
			max_depth,
		})
	}

	/// Reads a non-negative integer literal, allowing zero for both policies.
	fn event_integer(&mut self, maximum: u8, name: &str) -> FerrosGrynnResult<u8> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(
				self.syntax(&format!("Expected {name} integer in 0..={maximum}."), token.span)
			);
		};
		let value = value.replace('_', "").parse::<u8>().map_err(|_error| {
			self.syntax(&format!("Expected {name} integer in 0..={maximum}."), token.span)
		})?;
		if value > maximum {
			return Err(
				self.syntax(&format!("Expected {name} integer in 0..={maximum}."), token.span)
			);
		}
		Ok(value)
	}

	/// Shares ordinary block and expression parsing for an event handler.
	async fn event_body(&mut self, context: &mut Stk, list: bool) -> FerrosGrynnResult<EventBody> {
		if self.at_symbol(Symbol::LeftBrace) {
			Ok(EventBody::Block(context.run(|context| self.block(context)).await?))
		} else {
			let expression = context.run(|context| self.expression(context, 0, 1, 0)).await?;
			if !list || self.consume_symbol(Symbol::Comma).is_none() {
				return Ok(EventBody::Expression(expression));
			}
			let start = expression.span;
			let mut values = vec![expression];
			let end = loop {
				let next = context.run(|context| self.expression(context, 0, 1, 0)).await?;
				let span = next.span;
				values.push(next);
				if self.consume_symbol(Symbol::Comma).is_none() {
					break span;
				}
			};
			Ok(EventBody::Expressions {
				values,
				span: cover(start, end),
			})
		}
	}

	/// Parses DEFINE EVENT after its target keyword.
	pub(in crate::syn) async fn define_event_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineEventStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		let owner = self.event_owner()?;
		let asynchronous = if self.consume_keyword("ASYNC").is_some() {
			Some(self.event_async()?)
		} else {
			None
		};
		self.expect_keyword("WHEN", "Expected WHEN in EVENT definition.")?;
		let when = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		self.expect_keyword("THEN", "Expected THEN in EVENT definition.")?;
		let then = context.run(|context| self.event_body(context, false)).await?;
		self.expect_statement_end("DEFINE EVENT")?;
		Ok(DefineEventStatement {
			mode,
			name,
			owner,
			asynchronous,
			when,
			then,
		})
	}

	/// Parses one or more ordered ALTER EVENT actions after the source owner.
	pub(in crate::syn) async fn event_alter_actions(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Vec<EventAlterAction>> {
		let mut actions = Vec::new();
		while !self.at_statement_end() {
			let action = if self.consume_keyword("ASYNC").is_some() {
				EventAlterAction::Async(self.event_async()?)
			} else if self.consume_keyword("WHEN").is_some() {
				EventAlterAction::When(
					context.run(|context| self.expression(context, 0, 1, 0)).await?,
				)
			} else if self.consume_keyword("THEN").is_some() {
				EventAlterAction::Then(context.run(|context| self.event_body(context, true)).await?)
			} else if self.consume_keyword("COMMENT").is_some() {
				EventAlterAction::Comment(self.string_value("Expected EVENT comment string.")?)
			} else if self.consume_keyword("DROP").is_some() {
				if self.consume_keyword("ASYNC").is_some() {
					EventAlterAction::Sync
				} else if self.consume_keyword("WHEN").is_some() {
					EventAlterAction::DropWhen
				} else if self.consume_keyword("THEN").is_some() {
					EventAlterAction::DropThen
				} else {
					self.expect_keyword(
						"COMMENT",
						"Expected ASYNC, WHEN, THEN or COMMENT after DROP.",
					)?;
					EventAlterAction::DropComment
				}
			} else {
				return Err(self.syntax("Unexpected ALTER EVENT action.", self.current().span));
			};
			actions.push(action);
		}
		if actions.is_empty() {
			return Err(
				self.syntax("ALTER EVENT requires at least one action.", self.current().span)
			);
		}
		Ok(actions)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::syn::{ParserSettings, StatementKind, parse};

	#[test]
	fn event_parser_accepts_unicode_names_and_policy_boundaries() -> FerrosGrynnResult<()> {
		let program = parse(
			"DEFINE EVENT `проверка` ON CLASS item ASYNC RETRY 255 MAXDEPTH 0 WHEN $event = \
			 'CREATE' THEN { RETURN $after; }; ALTER EVENT `проверка` ON CLASS item DROP ASYNC \
			 WHEN true THEN 1 COMMENT 'ok'; REMOVE EVENT IF EXISTS `проверка` ON CLASS item;",
			ParserSettings::default(),
		)?;
		let StatementKind::DefineEvent(event) = &program.statements[0].kind else {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"fixture EVENT syntax",
			));
		};
		assert_eq!(event.name.value.as_ref(), "проверка");
		assert_eq!(
			event.asynchronous.map(|options| (options.retries, options.max_depth)),
			Some((255, 0))
		);
		assert!(matches!(event.then, EventBody::Block(_)));
		Ok(())
	}

	#[test]
	fn event_parser_rejects_invalid_policy_and_missing_body() {
		for source in [
			"DEFINE EVENT e ON CLASS item ASYNC RETRY 256 WHEN true THEN 1;",
			"DEFINE EVENT e ON GRAPH scope ASYNC MAXDEPTH 17 WHEN true THEN 1;",
			"DEFINE EVENT e ON CLASS item ASYNC RETRY -1 WHEN true THEN 1;",
			"DEFINE EVENT e ON CLASS item WHEN true;",
			"ALTER EVENT e ON CLASS item;",
		] {
			assert!(parse(source, ParserSettings::default()).is_err(), "{source}");
		}
	}
}
