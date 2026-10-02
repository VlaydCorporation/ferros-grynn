// These internals become reachable from the public Datastore API when that
// layer lands.
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod analysis;
pub(crate) mod closure;
pub(crate) mod cursor;
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod execution;
pub(crate) mod import;
#[allow(dead_code, reason = "the crate-private executor owns the logical IR")]
mod ir;
pub(crate) mod optimizer;
mod options;
mod pure_function;
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
pub(crate) use pure_function::PureFunctionEvaluator;

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
		BuildGraphSource,
		BuildGraphStatement,
		CatchClause,
		ClassKindName,
		ClassSchemaModeName,
		ClosureBody,
		ClosureExpr,
		ClosureParameter,
		CommitStatement,
		CreateStatement,
		DefineClassStatement,
		DefineFieldStatement,
		DefineFunctionStatement,
		DefineGraphStatement,
		DefineIndexStatement,
		DefineParameterStatement,
		DefinitionMode,
		DeleteStatement,
		DetachStatement,
		DmlData,
		DmlReturn,
		DmlReturnItem,
		DmlTarget,
		ExecutionModeName,
		ExpectClause,
		ExplainFormat,
		ExplainMode,
		ExplainStatement,
		Expr,
		ExprKind,
		ForStatement,
		FulltextOperator,
		GraphContract,
		GraphFamilyName,
		GraphPropertyAssignment,
		GraphPropertyMutation,
		GraphPropertyPath,
		GraphValidatorName,
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
		KnnDistance,
		KnnSearchArgument,
		LetStatement,
		LetValue,
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
		ParameterCommentChange,
		ParserSettings,
		Program,
		RecordKey,
		RelateStatement,
		RemoveStatement,
		RemoveTarget,
		ReturnGraph,
		ReturnStatement,
		SelectAccessHint,
		SelectGroup,
		SelectIndexHint,
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
		TraverseSource,
		TraverseStatement,
		TraverseStep,
		TryStatement,
		TypeName,
		UnaryOperator,
		UpdateGraphStatement,
		VectorDistanceName,
		VectorElementName,
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
	Explain,
	Begin,
	Commit,
	Cancel,
	Expression,
	If,
	For,
	While,
	Try,
	Throw,
	Sleep,
	Assert,
	BuildGraph,
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
	Maintenance,
	Kill,
	Let,
	Match,
	Return,
	ReturnGraph,
	Select,
	Traverse,
	Import,
}

/// Concrete execution semantics selected for one explicit transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ExecutionMode {
	/// Preserves the ordinary stable baseline where it is already defined.
	#[default]
	BestEffort,
	/// Requires replay-stable planning and result ordering.
	Deterministic,
	/// Permits unordered pure execution while retaining exact semantics.
	Fast,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum TransactionCommand {
	Begin {
		options: crate::datastore::TransactionOptions,
		execution_mode: ExecutionMode,
	},
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
			let execution_mode = match begin.execution_mode {
				ExecutionModeName::BestEffort => ExecutionMode::BestEffort,
				ExecutionModeName::Deterministic => ExecutionMode::Deterministic,
				ExecutionModeName::Fast => ExecutionMode::Fast,
			};
			if execution_mode == ExecutionMode::Deterministic
				&& isolation == crate::datastore::IsolationLevel::ReadCommitted
			{
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"EXECUTION MODE DETERMINISTIC requires SNAPSHOT or SERIALIZABLE isolation.",
				));
			}
			TransactionCommand::Begin {
				options: crate::datastore::TransactionOptions::new(isolation, access),
				execution_mode,
			}
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
		StatementKind::Explain(_) => QueryStatementKind::Explain,
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
		StatementKind::Sleep(_) => QueryStatementKind::Sleep,
		StatementKind::AssertInvariants(_) => QueryStatementKind::Assert,
		StatementKind::BuildGraph(_) => QueryStatementKind::BuildGraph,
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
		| StatementKind::DefineIndex(_)
		| StatementKind::DefineGraphIndex(_)
		| StatementKind::DefineAnalyzer(_)
		| StatementKind::DefineFunction(_)
		| StatementKind::DefineParameter(_) => QueryStatementKind::Define,
		StatementKind::Remove(_) => QueryStatementKind::Remove,
		StatementKind::Alter(_) => QueryStatementKind::Alter,
		StatementKind::Info(_) => QueryStatementKind::Info,
		StatementKind::Maintenance(_) => QueryStatementKind::Maintenance,
		StatementKind::Kill(_) => QueryStatementKind::Kill,
		StatementKind::Import(_) => QueryStatementKind::Import,
		StatementKind::Let(_) => QueryStatementKind::Let,
		StatementKind::Match(_) => QueryStatementKind::Match,
		StatementKind::Relate(_) => QueryStatementKind::Relate,
		StatementKind::Return(_) => QueryStatementKind::Return,
		StatementKind::ReturnGraph(_) => QueryStatementKind::ReturnGraph,
		StatementKind::Select(_) => QueryStatementKind::Select,
		StatementKind::Traverse(_) => QueryStatementKind::Traverse,
	}
}

pub(crate) fn statement_mutates_state(statement: &StatementKind) -> bool {
	if let StatementKind::Explain(explain) = statement {
		return explain.mode == ExplainMode::Analyze
			&& statement_mutates_state(&explain.statement.kind);
	}
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
			| StatementKind::DefineGraphIndex(_)
			| StatementKind::DefineAnalyzer(_)
			| StatementKind::DefineFunction(_)
			| StatementKind::DefineParameter(_)
			| StatementKind::Remove(_)
			| StatementKind::Alter(_)
			| StatementKind::Maintenance(_)
			| StatementKind::Kill(_)
			| StatementKind::Import(_)
			| StatementKind::BuildGraph(crate::syn::BuildGraphStatement {
				name: Some(_),
				..
			})
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
