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
	BasicIndexDefinition,
	BinaryOperator,
	CreateStatement,
	DefineFieldStatement,
	DefineGraphStatement,
	DefineIndexStatement,
	DefineTableStatement,
	DefinitionMode,
	DeleteStatement,
	DmlData,
	DmlReturn,
	DmlReturnItem,
	DmlTarget,
	ExpectClause,
	Expr,
	ExprKind,
	Identifier,
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
	MatchStatement,
	MatchStep,
	MutateStatement,
	ObjectField,
	ParserSettings,
	Program,
	RecordKey,
	RemoveStatement,
	RemoveTarget,
	ReturnStatement,
	SelectItem,
	SelectStatement,
	SelectTarget,
	Statement,
	StatementKind,
	TableKindName,
	TableSchemaModeName,
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
				| StatementKind::DefineGraph(_)
				| StatementKind::DefineTable(_)
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
