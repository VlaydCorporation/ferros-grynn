// These internals become reachable from the public Datastore API when that
// layer lands.
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod analysis;
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod execution;
#[allow(dead_code, reason = "the crate-private executor owns the logical IR")]
mod ir;

use std::time::Duration;

pub(crate) use execution::{execute_parsed_transaction, execute_transaction};

pub use crate::syn::{
	AlterStatement,
	AlterTarget,
	Assignment,
	AttachStatement,
	BasicIndexDefinition,
	BinaryOperator,
	ClassKindName,
	ClassSchemaModeName,
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
	ExpectClause,
	Expr,
	ExprKind,
	Identifier,
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
	SelectItem,
	SelectStatement,
	SelectTarget,
	Statement,
	StatementKind,
	TypeName,
	UnaryOperator,
};

/// Returns whether a program contains a state-mutating statement.
pub(crate) fn program_mutates_state(program: &Program) -> bool {
	program.statements.iter().any(|statement| {
		matches!(
			statement.kind,
			StatementKind::Create(_)
				| StatementKind::Update(_)
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
		)
	})
}
use crate::{
	gql_status::{FerrosGrynnResult, Outcome},
	values::Value,
};

pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	crate::syn::parse(source, settings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryStatementKind {
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
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct QueryResult {
	pub kind: QueryStatementKind,
	pub elapsed: Duration,
	pub result: FerrosGrynnResult<Outcome<Value>>,
}
