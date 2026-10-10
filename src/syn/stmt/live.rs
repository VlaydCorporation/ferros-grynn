//! LIVE registration syntax reuses the ordinary SELECT parser.

use reblessive::Stk;

use super::{Expr, FerrosGrynnResult, Identifier, Parser, SelectItem, SelectStatement, keywords};
use crate::syn::token::Symbol;

/// Defines an independent reactive identity with frozen ordinary captures.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LiveLetStatement {
	/// Lexical spelling resolves the identity within its declaration scope.
	pub name: Identifier,
	/// Read-only expression retained as a recoverable logical recipe.
	pub value: Expr,
}

/// Confirms a previously issued whole commit through the owning session.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LiveAckStatement {
	/// Lexical UUID of the subscription incarnation.
	pub query: Identifier,
	/// Expression yielding Bytes containing the complete fixed cursor envelope.
	pub cursor: Expr,
}

/// Presentation of complete projected changes, without altering capture.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveOutputName {
	/// Complete before/after values.
	#[default]
	Full,
	/// Changed object fields.
	Diff,
	/// Ordered JSON Pointer operations.
	Patch,
}

/// Explicit queue overflow behavior, resolved into checked runtime options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LiveOverflowName {
	/// Preserve an explicit whole-commit loss notification.
	Gap,
	/// Terminate delivery after reporting loss.
	Disconnect,
	/// Wait for admitted capacity before publication.
	Block,
}

/// One LIVE SELECT recipe; delivery never cuts a commit in half.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LiveSelectStatement {
	/// Explicit delivery presentation; ordinary SELECT syntax remains shared.
	pub output: LiveOutputName,
	/// SELECT syntax remains subject to the ordinary semantic analyzer.
	pub select: SelectStatement,
	/// Exclusive versionstamp or inclusive UTC timestamp.
	pub since: Option<Expr>,
	/// Omission uses GAP.
	pub overflow: Option<LiveOverflowName>,
	/// Checked event count; omission uses the SDK default.
	pub buffer: Option<Expr>,
}

impl Parser<'_> {
	/// Parses after LIVE LET using the ordinary expression parser.
	pub(in crate::syn) async fn live_let_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<LiveLetStatement> {
		let name = self.parameter()?;
		self.expect_symbol(Symbol::Equal, "Expected `=` in LIVE LET.")?;
		let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		Ok(LiveLetStatement {
			name,
			value,
		})
	}

	/// Recognizes the two consumer control commands without consuming their
	/// identity.
	pub(in crate::syn) fn at_consumer_control(&self) -> bool {
		self.at_keyword(keywords::KILL) || self.at_keyword(keywords::ACK)
	}

	/// Shares control dispatch while preserving legacy maintenance KILL
	/// parsing.
	pub(in crate::syn) async fn consumer_control_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<crate::syn::StatementKind> {
		if self.consume_keyword(keywords::KILL).is_some() {
			Ok(crate::syn::StatementKind::Kill(self.kill_statement()?))
		} else {
			self.expect_keyword(keywords::ACK, "Expected ACK consumer control.")?;
			Ok(crate::syn::StatementKind::AckLive(
				context.run(|context| self.live_ack_statement(context)).await?,
			))
		}
	}

	/// Parses a lexical identity and cursor expression; the datastore checks
	/// issuance.
	pub(in crate::syn) async fn live_ack_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<LiveAckStatement> {
		self.expect_keyword(keywords::LIVE, "Expected LIVE after ACK.")?;
		let query = self.parameter()?;
		self.expect_keyword(keywords::CURSOR, "Expected CURSOR after ACK LIVE identity.")?;
		let cursor = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		self.expect_statement_end("ACK LIVE")?;
		Ok(LiveAckStatement {
			query,
			cursor,
		})
	}

	/// Parses after LIVE, including ordered SINCE and queue policy clauses.
	pub(in crate::syn) async fn live_select_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<LiveSelectStatement> {
		self.expect_keyword(keywords::SELECT, "Expected SELECT after LIVE.")?;
		let mode = self
			.consume_keyword(keywords::DIFF)
			.map(|token| (LiveOutputName::Diff, token.span))
			.or_else(|| {
				self.consume_keyword(keywords::PATCH)
					.map(|token| (LiveOutputName::Patch, token.span))
			});
		let output = mode.map_or(LiveOutputName::Full, |(output, _)| output);
		let implicit = if self.at_keyword(keywords::FROM)
			|| self.at_keyword(keywords::ONLY) && self.peek_keyword(1, keywords::FROM)
		{
			mode.map(|(_, span)| SelectItem::Wildcard(span))
		} else {
			None
		};
		let select = context.run(|context| self.select_with_projection(context, implicit)).await?;
		let since = if self.consume_keyword(keywords::SINCE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let overflow = if self.consume_keyword(keywords::ON).is_some() {
			self.expect_keyword(keywords::OVERFLOW, "Expected OVERFLOW after ON.")?;
			if self.consume_keyword(keywords::GAP).is_some() {
				Some(LiveOverflowName::Gap)
			} else if self.consume_keyword(keywords::DISCONNECT).is_some() {
				Some(LiveOverflowName::Disconnect)
			} else {
				self.expect_keyword(keywords::BLOCK, "Expected GAP, DISCONNECT or BLOCK.")?;
				Some(LiveOverflowName::Block)
			}
		} else {
			None
		};
		let buffer = if self.consume_keyword(keywords::BUFFER).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		Ok(LiveSelectStatement {
			output,
			select,
			since,
			overflow,
			buffer,
		})
	}
}

#[cfg(test)]
mod tests {
	use crate::syn::{LetValue, ParserSettings, StatementKind, parse};

	/// Binding declarations retain expression syntax and UTF-8 name spans;
	/// malformed administrative forms fail at the syntax boundary.
	#[test]
	fn live_let_parser_lifecycle_and_boundaries() -> crate::gql_status::FerrosGrynnResult<()> {
		let source = "LIVE LET $`порог` = 18; ALTER LIVE LET $`порог` VALUE (SELECT VALUE count() \
		              FROM person); INFO FOR LIVE LET $`порог`; REMOVE LIVE LET $`порог`;";
		let program = parse(source, ParserSettings::default())?;
		assert_eq!(program.statements.len(), 4);
		let StatementKind::LiveLet(binding) = &program.statements[0].kind else {
			return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"LIVE LET syntax missing",
			));
		};
		assert_eq!(binding.name.value.as_ref(), "порог");
		assert!(source.is_char_boundary(binding.name.span.offset as usize));
		let end = binding.name.span.end().ok_or_else(|| {
			crate::gql_status::FerrosGrynnError::operation_overflow("fixture name span")
		})?;
		assert!(source.is_char_boundary(end as usize));
		for source in [
			"LIVE LET name = 1;",
			"LIVE LET $name VALUE 1;",
			"LIVE LET $name =;",
			"ALTER LIVE LET $name = 2;",
			"INFO FOR LIVE LET name;",
			"REMOVE LIVE LET;",
		] {
			assert!(parse(source, ParserSettings::default()).is_err(), "{source}");
		}
		Ok(())
	}

	/// Standalone and LET registration preserve SELECT syntax and policy.
	#[test]
	fn live_select_parser_registration_and_boundaries() -> crate::gql_status::FerrosGrynnResult<()>
	{
		let program = parse(
			"LET $sub = LIVE SELECT age FROM person WHERE age >= $min FETCH manager SINCE $stamp \
			 ON OVERFLOW BLOCK BUFFER 12; LIVE SELECT * FROM person;",
			ParserSettings::default(),
		)?;
		assert!(
			matches!(&program.statements[0].kind, StatementKind::Let(binding) if matches!(&binding.value, LetValue::LiveSelect(select) if select.since.is_some() && select.buffer.is_some()))
		);
		assert!(matches!(program.statements[1].kind, StatementKind::LiveSelect(_)));
		for source in [
			"LIVE SELECT * FROM person ON OVERFLOW unknown;",
			"LIVE SELECT * FROM person BUFFER;",
			"LIVE SELECT * FROM person SINCE 1 SINCE 2;",
		] {
			assert!(parse(source, ParserSettings::default()).is_err(), "{source}");
		}
		Ok(())
	}
}
