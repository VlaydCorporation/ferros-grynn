// These internals become reachable from the public Datastore API when that
// layer lands.
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod analysis;
pub(crate) mod cursor;
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod execution;
pub(crate) mod import;
#[allow(dead_code, reason = "the crate-private executor owns the logical IR")]
mod ir;
mod options;
pub(crate) mod spill;

use std::time::Duration;

#[cfg(test)]
pub(crate) use execution::execute_parsed_transaction_with_bindings;
pub(crate) use execution::{
	execute_parsed_statement_with_bindings,
	execute_transaction,
	execute_transaction_with_options,
};
pub(crate) use options::{ExecutionControl, resolve_timeout};
pub use options::{QueryCancellation, QueryExecutionOptions};

use crate::{
	gql_status::{FerrosGrynnResult, Outcome},
	values::Value,
};
pub use crate::{
	syn::{
		AlterStatement,
		AlterTarget,
		AssertInvariantsStatement,
		Assignment,
		AttachStatement,
		BasicIndexDefinition,
		BeginStatement,
		BinaryOperator,
		Block,
		CatchClause,
		ClassKindName,
		ClassSchemaModeName,
		CommitStatement,
		CreateStatement,
		DefineClassStatement,
		DefineFieldStatement,
		DefineGraphStatement,
		DefineIndexStatement,
		DefinitionMode,
		DeleteStatement,
		DetachStatement,
		DmlData,
		DmlReturn,
		DmlReturnItem,
		DmlTarget,
		ExecutionModeName,
		ExpectClause,
		Expr,
		ExprKind,
		ForStatement,
		GraphPropertyAssignment,
		GraphPropertyMutation,
		GraphPropertyPath,
		Identifier,
		IfStatement,
		ImportGraphRef,
		ImportOption,
		ImportOptionName,
		ImportOptionValue,
		ImportStatement,
		IndexScopeName,
		InfoStatement,
		InfoTarget,
		LetStatement,
		Literal,
		MatchDirection,
		MatchEdge,
		MatchEndpoint,
		MatchEndpointKind,
		MatchLength,
		MatchPattern,
		MatchReturnItem,
		MatchSelector,
		MatchStatement,
		MatchStep,
		MutateStatement,
		ObjectField,
		ParserSettings,
		Program,
		RecordKey,
		RelateStatement,
		RemoveStatement,
		RemoveTarget,
		ReturnStatement,
		SelectGroup,
		SelectItem,
		SelectJoin,
		SelectJoinKind,
		SelectNulls,
		SelectOrder,
		SelectOrderDirection,
		SelectPath,
		SelectSource,
		SelectSourceKind,
		SelectStart,
		SelectStatement,
		SelectTarget,
		Statement,
		StatementKind,
		TimeoutValue,
		TransactionIsolationName,
		TryStatement,
		TypeName,
		UnaryOperator,
		UpdateGraphStatement,
		WhileStatement,
	},
	values::QueryCursor,
};

pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	crate::syn::parse(source, settings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryStatementKind {
	Begin,
	Commit,
	Cancel,
	Expression,
	If,
	For,
	While,
	Try,
	Throw,
	Assert,
	Create,
	Update,
	Upsert,
	Delete,
	Attach,
	Detach,
	Relate,
	Define,
	Remove,
	Alter,
	Info,
	Let,
	Match,
	Return,
	Select,
	Import,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TransactionCommand {
	Begin(crate::datastore::TransactionOptions),
	Commit {
		full_validation: bool,
	},
	Cancel,
}

pub(crate) fn analyze_transaction_statement(
	statement: &Statement,
) -> FerrosGrynnResult<Option<TransactionCommand>> {
	let command = match &statement.kind {
		StatementKind::Begin(begin) => {
			if begin.execution_mode != ExecutionModeName::BestEffort {
				return Err(crate::gql_status::FerrosGrynnError::query_feature_not_supported(
					match begin.execution_mode {
						ExecutionModeName::Deterministic => "EXECUTION MODE DETERMINISTIC",
						ExecutionModeName::Fast => "EXECUTION MODE FAST",
						ExecutionModeName::BestEffort => unreachable!(),
					},
				));
			}
			let isolation = match begin.isolation {
				TransactionIsolationName::ReadCommitted => {
					crate::datastore::IsolationLevel::ReadCommitted
				}
				TransactionIsolationName::Snapshot => crate::datastore::IsolationLevel::Snapshot,
				TransactionIsolationName::Serializable => {
					crate::datastore::IsolationLevel::Serializable
				}
			};
			let access = if begin.read_only {
				crate::datastore::TransactionAccess::ReadOnly
			} else {
				crate::datastore::TransactionAccess::ReadWrite
			};
			TransactionCommand::Begin(crate::datastore::TransactionOptions::new(isolation, access))
		}
		StatementKind::Commit(commit) => TransactionCommand::Commit {
			full_validation: commit.full_validation,
		},
		StatementKind::Cancel => TransactionCommand::Cancel,
		_ => return Ok(None),
	};
	Ok(Some(command))
}

pub(crate) const fn statement_kind(statement: &StatementKind) -> QueryStatementKind {
	match statement {
		StatementKind::Begin(_) => QueryStatementKind::Begin,
		StatementKind::Commit(_) => QueryStatementKind::Commit,
		StatementKind::Cancel => QueryStatementKind::Cancel,
		StatementKind::Expression(_) | StatementKind::Break | StatementKind::Continue => {
			QueryStatementKind::Expression
		}
		StatementKind::If(_) => QueryStatementKind::If,
		StatementKind::For(_) => QueryStatementKind::For,
		StatementKind::While(_) => QueryStatementKind::While,
		StatementKind::Try(_) => QueryStatementKind::Try,
		StatementKind::Throw(_) => QueryStatementKind::Throw,
		StatementKind::AssertInvariants(_) => QueryStatementKind::Assert,
		StatementKind::Attach(_) => QueryStatementKind::Attach,
		StatementKind::Create(_) => QueryStatementKind::Create,
		StatementKind::Update(_) => QueryStatementKind::Update,
		StatementKind::UpdateGraph(_) => QueryStatementKind::Update,
		StatementKind::Upsert(_) => QueryStatementKind::Upsert,
		StatementKind::Delete(_) => QueryStatementKind::Delete,
		StatementKind::Detach(_) => QueryStatementKind::Detach,
		StatementKind::DefineGraph(_)
		| StatementKind::DefineClass(_)
		| StatementKind::DefineField(_)
		| StatementKind::DefineIndex(_) => QueryStatementKind::Define,
		StatementKind::Remove(_) => QueryStatementKind::Remove,
		StatementKind::Alter(_) => QueryStatementKind::Alter,
		StatementKind::Info(_) => QueryStatementKind::Info,
		StatementKind::Import(_) => QueryStatementKind::Import,
		StatementKind::Let(_) => QueryStatementKind::Let,
		StatementKind::Match(_) => QueryStatementKind::Match,
		StatementKind::Relate(_) => QueryStatementKind::Relate,
		StatementKind::Return(_) => QueryStatementKind::Return,
		StatementKind::Select(_) => QueryStatementKind::Select,
	}
}

pub(crate) fn statement_mutates_state(statement: &StatementKind) -> bool {
	if matches!(
		statement,
		StatementKind::Create(_)
			| StatementKind::Update(_)
			| StatementKind::UpdateGraph(_)
			| StatementKind::Upsert(_)
			| StatementKind::Delete(_)
			| StatementKind::Attach(_)
			| StatementKind::Detach(_)
			| StatementKind::Relate(_)
			| StatementKind::DefineGraph(_)
			| StatementKind::DefineClass(_)
			| StatementKind::DefineField(_)
			| StatementKind::DefineIndex(_)
			| StatementKind::Remove(_)
			| StatementKind::Alter(_)
			| StatementKind::Import(_)
	) {
		return true;
	}
	match statement {
		StatementKind::If(statement) => {
			statement.branches.iter().any(|(_, block)| block_mutates_state(block))
				|| statement.else_block.as_ref().is_some_and(block_mutates_state)
		}
		StatementKind::For(statement) => block_mutates_state(&statement.body),
		StatementKind::While(statement) => block_mutates_state(&statement.body),
		StatementKind::Try(statement) => {
			block_mutates_state(&statement.body)
				|| statement.catch.as_ref().is_some_and(|catch| block_mutates_state(&catch.body))
		}
		_ => false,
	}
}

fn block_mutates_state(block: &Block) -> bool {
	block.statements.iter().any(|statement| statement_mutates_state(&statement.kind))
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct QueryResult {
	pub kind: QueryStatementKind,
	pub elapsed: Duration,
	pub result: FerrosGrynnResult<Outcome<Value>>,
	/// Opaque continuation for the next ordered result page.
	pub continuation: Option<QueryCursor>,
}
