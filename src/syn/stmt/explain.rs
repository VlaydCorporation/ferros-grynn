//! Prefix EXPLAIN syntax.

use super::{
	FerrosGrynnResult,
	InfoStatement,
	InfoTarget,
	LetStatement,
	LetValue,
	Parser,
	Statement,
	StatementKind,
	Stk,
	keywords,
};

/// Detail and execution mode requested by EXPLAIN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExplainMode {
	/// Normalized and rewritten logical plan without execution.
	Logical,
	/// Physical alternatives, estimates, costs, and warnings without execution.
	Full,
	/// Physical plan plus one real execution of the wrapped statement.
	Analyze,
}

/// Public representation selected for one explain result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExplainFormat {
	/// Human-readable textual explanation.
	Text,
	/// Structured JSON-like explanation value.
	Json,
}

/// One structurally valid prefix EXPLAIN wrapper.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ExplainStatement {
	/// Execution or declaration policy selected by this clause.
	pub mode: ExplainMode,
	/// Textual or structured representation requested for EXPLAIN output.
	pub format: ExplainFormat,
	/// Statement whose plan or execution is being explained.
	pub statement: Box<Statement>,
}

impl Parser<'_> {
	/// Parses the explain statement syntax.
	pub(in crate::syn) async fn explain_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<ExplainStatement> {
		let mode = if self.consume_keyword(keywords::ANALYZE).is_some() {
			ExplainMode::Analyze
		} else if self.consume_keyword(keywords::FULL).is_some() {
			ExplainMode::Full
		} else {
			ExplainMode::Logical
		};
		let format = if self.consume_keyword(keywords::FORMAT).is_some() {
			if self.consume_keyword(keywords::JSON).is_some() {
				ExplainFormat::Json
			} else if self.consume_keyword(keywords::TEXT).is_some() {
				ExplainFormat::Text
			} else {
				return Err(
					self.syntax("EXPLAIN FORMAT requires TEXT or JSON.", self.current().span)
				);
			}
		} else {
			ExplainFormat::Text
		};
		if self.current_word().is_some_and(|word| word.eq_ignore_ascii_case(keywords::EXPLAIN)) {
			return Err(self.syntax("Nested EXPLAIN is not supported.", self.current().span));
		}
		let statement = context.run(|context| self.statement(context)).await?;
		if !explainable(&statement.kind) {
			return Err(self.syntax(
				"EXPLAIN supports DDL, DML, SELECT, MATCH, TRAVERSE, and graph-result statements.",
				statement.span,
			));
		}
		Ok(ExplainStatement {
			mode,
			format,
			statement: Box::new(statement),
		})
	}
}

/// Returns whether the explainable condition holds.
fn explainable(statement: &StatementKind) -> bool {
	if matches!(
		statement,
		StatementKind::LiveSelect(_)
			| StatementKind::LiveLet(_)
			| StatementKind::AckLive(_)
			| StatementKind::Kill(_)
			| StatementKind::Let(LetStatement {
				value: LetValue::LiveSelect(_),
				..
			})
			| StatementKind::Info(InfoStatement {
				target: InfoTarget::Live(_) | InfoTarget::LiveLet(_),
				..
			})
	) {
		return true;
	}
	matches!(
		statement,
		StatementKind::Attach(_)
			| StatementKind::Create(_)
			| StatementKind::Update(_)
			| StatementKind::UpdateGraph(_)
			| StatementKind::Upsert(_)
			| StatementKind::Delete(_)
			| StatementKind::Detach(_)
			| StatementKind::DefineGraph(_)
			| StatementKind::DefineClass(_)
			| StatementKind::ShowChanges(_)
			| StatementKind::DefineField(_)
			| StatementKind::DefineIndex(_)
			| StatementKind::DefineGraphIndex(_)
			| StatementKind::Remove(_)
			| StatementKind::Alter(_)
			| StatementKind::Match(_)
			| StatementKind::Relate(_)
			| StatementKind::Select(_)
			| StatementKind::Traverse(_)
			| StatementKind::ReturnGraph(_)
			| StatementKind::BuildGraph(_)
	)
}
