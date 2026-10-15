//! Durable source clauses and whole-commit history statements.

use super::{
	AlterTarget,
	Expr,
	FerrosGrynnResult,
	Identifier,
	Parser,
	Stk,
	TextSpan,
	TimeoutValue,
	TokenKind,
	keywords,
};

/// Literal fixed-duration capture policy, before semantic validation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChangefeedClause {
	/// Duration text, for example `7d`.
	pub duration: Box<str>,
	/// Literal byte span used by semantic diagnostics.
	pub span: TextSpan,
	/// Full before-image capture.
	pub include_original: bool,
	/// Successful operation capture, including targeted no-ops.
	pub full: bool,
}

/// Active or retained source selector; resolution belongs to the feed catalog.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ShowChangesTarget {
	/// Class name, including a retained historical name.
	Class(
		/// Class name, including a retained historical name.
		Identifier,
	),
	/// Graph name, including a retained historical name.
	GraphName(
		/// Graph name, including a retained historical name.
		Identifier,
	),
	/// Runtime qualified graph reference or graph-name string.
	Graph(
		/// Runtime qualified graph reference or graph-name string.
		Expr,
	),
	/// Runtime UUID or UUID string.
	Changefeed(
		/// Runtime UUID or UUID string.
		Expr,
	),
}

/// Whole-commit history read.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ShowChangesStatement {
	/// Requested source.
	pub target: ShowChangesTarget,
	/// Exclusive stamp, inclusive UTC timestamp, or opaque resume bytes.
	pub since: Expr,
	/// Whether the selected interval requires FULL coverage.
	pub full: bool,
	/// Optional whole-commit/GAP item count.
	pub limit: Option<Expr>,
}

impl Parser<'_> {
	/// Parses one source clause after consuming CHANGEFEED.
	pub(in crate::syn) fn changefeed_clause(&mut self) -> FerrosGrynnResult<ChangefeedClause> {
		let TimeoutValue::Literal {
			value,
			span,
		} = self.timeout_value()?
		else {
			return Err(
				self.syntax("CHANGEFEED requires a literal duration.", self.previous_span())
			);
		};
		let mut clause = ChangefeedClause {
			duration: value,
			span,
			include_original: false,
			full: false,
		};
		loop {
			if self.consume_keyword(keywords::INCLUDE).is_some() {
				if clause.include_original {
					return Err(
						self.syntax("INCLUDE ORIGINAL may appear only once.", self.previous_span())
					);
				}
				self.expect_keyword(keywords::ORIGINAL, "Expected ORIGINAL after INCLUDE.")?;
				clause.include_original = true;
			} else if self.consume_keyword(keywords::FULL).is_some() {
				if clause.full {
					return Err(
						self.syntax("CHANGEFEED FULL may appear only once.", self.previous_span())
					);
				}
				clause.full = true;
			} else {
				break;
			}
		}
		Ok(clause)
	}

	/// Parses the existing owner with one configure or DROP action.
	fn changefeed_action(&mut self) -> FerrosGrynnResult<Option<ChangefeedClause>> {
		if self.consume_keyword(keywords::DROP).is_some() {
			self.expect_keyword(keywords::CHANGEFEED, "Expected CHANGEFEED after DROP.")?;
			Ok(None)
		} else {
			self.expect_keyword(keywords::CHANGEFEED, "Expected CHANGEFEED configuration.")?;
			self.changefeed_clause().map(Some)
		}
	}

	/// Parses ALTER CLASS/GRAPH capture while keeping other owner actions
	/// intact.
	pub(in crate::syn) fn alter_changefeed(
		&mut self,
		graph: bool,
		if_exists: bool,
		name: Identifier,
	) -> FerrosGrynnResult<AlterTarget> {
		Ok(AlterTarget::Changefeed {
			graph,
			if_exists,
			name,
			configuration: self.changefeed_action()?,
		})
	}

	/// Parses SHOW CHANGES; semantic analysis owns all expression types.
	pub(in crate::syn) async fn show_changes_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<ShowChangesStatement> {
		self.expect_keyword(keywords::CHANGES, "Expected CHANGES after SHOW.")?;
		self.expect_keyword(keywords::FOR, "Expected FOR after SHOW CHANGES.")?;
		let target = if self.consume_keyword(keywords::CLASS).is_some() {
			ShowChangesTarget::Class(self.identifier()?)
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			if matches!(self.current().kind, TokenKind::Word { .. }) {
				ShowChangesTarget::GraphName(self.identifier()?)
			} else {
				ShowChangesTarget::Graph(
					context.run(|context| self.expression(context, 0, 1, 0)).await?,
				)
			}
		} else if self.consume_keyword(keywords::CHANGEFEED).is_some() {
			ShowChangesTarget::Changefeed(
				context.run(|context| self.expression(context, 0, 1, 0)).await?,
			)
		} else {
			return Err(
				self.syntax("Expected CLASS, GRAPH or CHANGEFEED source.", self.current().span)
			);
		};
		self.expect_keyword(keywords::SINCE, "Expected SINCE after source.")?;
		let since = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let full = self.consume_keyword(keywords::FULL).is_some();
		let limit = if self.consume_keyword(keywords::LIMIT).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		if !self.at_keyword(keywords::TIMEOUT) {
			self.expect_statement_end("SHOW CHANGES")?;
		}
		Ok(ShowChangesStatement {
			target,
			since,
			full,
			limit,
		})
	}
}
