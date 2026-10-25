// These internals become reachable from the public Datastore API when that
// layer lands.
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
/// Semantic analysis and lowering into logical plans.
mod analysis;
pub(crate) mod bindings;
pub(crate) mod closure;
/// Query continuation identities and retained spool state.
pub(crate) mod cursor;
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
/// Transactional query execution and result materialization.
mod execution;
pub(crate) mod import;
#[allow(dead_code, reason = "the crate-private executor owns the logical IR")]
/// Typed logical query plans and expressions.
mod ir;
/// Rust LIVE descriptors lower through the ordinary typed SELECT pipeline.
pub(crate) mod live_lowering;
pub(crate) mod optimizer;
/// Execution settings, cancellation and resource accounting.
mod options;
mod pure_function;
mod root_catalog;
pub(crate) mod spill;
mod value_admission;

use std::time::Duration;

#[cfg(test)]
pub(crate) use execution::execute_parsed_transaction_with_bindings;
pub(crate) use execution::{
	execute_event,
	execute_event_handler,
	execute_parsed_statement_with_bindings,
	execute_transaction,
	execute_transaction_with_options,
	validate_event_definition,
};
pub(crate) use options::{BarrierAccount, ExecutionControl};
pub use options::{QueryCancellation, QueryExecutionContext, QueryExecutionOptions};
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
		ChangefeedClause,
		ClassKindName,
		ClassSchemaModeName,
		ClosureBody,
		ClosureExpr,
		ClosureParameter,
		CommitStatement,
		CreateStatement,
		DefineClassStatement,
		DefineEventStatement,
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
		EventAlterAction,
		EventAsyncName,
		EventBody,
		EventOwnerName,
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
		LiveLetStatement,
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
		RelateEndpoint,
		RelateEndpointTarget,
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
		ShowChangesStatement,
		ShowChangesTarget,
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

/// Parses and validates the input syntax for this type.
///
/// # Errors
///
/// Propagates validation or execution failures from the underlying operation.
pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	crate::syn::parse(source, settings)
}

/// Public statement classification shared with the syntax owner.
pub use fg_syntax::__internal::QueryStatementKind;

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
/// Transaction command alternatives.
pub(crate) enum TransactionCommand {
	/// Begin case of transaction command.
	Begin {
		/// Options associated with this transaction command.
		options: crate::datastore::TransactionOptions,
		/// Execution mode associated with this transaction command.
		execution_mode: ExecutionMode,
	},
	/// Commit case of transaction command.
	Commit {
		/// Full validation associated with this transaction command.
		full_validation: bool,
	},
	/// Cancel case of transaction command.
	Cancel,
}

/// Analyzes transaction statement.
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

				_ => {
					return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
						"Unsupported syntax variant at the engine boundary.",
					));
				}
			};
			let access = if begin.read_only {
				crate::datastore::TransactionAccess::ReadOnly
			} else {
				crate::datastore::TransactionAccess::ReadWrite
			};
			let execution_mode =
				fg_syntax::__internal::EnumChoice::choose(&(begin.execution_mode), [
					ExecutionMode::BestEffort,
					ExecutionMode::Deterministic,
					ExecutionMode::Fast,
				]);
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

pub(crate) use fg_syntax::__internal::statement_kind;

/// Returns whether the statement mutates state condition holds.
pub(crate) fn statement_mutates_state(statement: &StatementKind) -> bool {
	if matches!(
		statement,
		StatementKind::Alter(crate::syn::AlterStatement {
			target: crate::syn::AlterTarget::Let { .. },
			..
		})
	) {
		return false;
	}
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
			| StatementKind::DefineEvent(_)
			| StatementKind::DefineParameter(_)
			| StatementKind::Remove(_)
			| StatementKind::Alter(_)
			| StatementKind::Maintenance(_)
			| StatementKind::Kill(_)
			| StatementKind::Import(_)
			| StatementKind::LiveSelect(_)
			| StatementKind::LiveLet(_)
			| StatementKind::AckLive(_)
			| StatementKind::Let(crate::syn::LetStatement {
				value: crate::syn::LetValue::LiveSelect(_),
				..
			})
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

/// Returns whether the block mutates state condition holds.
fn block_mutates_state(block: &Block) -> bool {
	block.statements.iter().any(|statement| statement_mutates_state(&statement.kind))
}

/// Statement outcome with elapsed time and an optional continuation cursor.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct QueryResult {
	/// Selected operation or representation discriminator.
	pub kind: QueryStatementKind,
	/// Time spent executing the statement represented by this result.
	pub elapsed: Duration,
	/// Statement failure or successful value with its completion statuses.
	pub result: FerrosGrynnResult<Outcome<Value>>,
	/// Opaque continuation for the next ordered result page.
	pub continuation: Option<QueryCursor>,
}
