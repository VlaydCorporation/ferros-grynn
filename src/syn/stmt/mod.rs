mod control;
mod dml;
mod graph_dml;
mod graph_properties;
mod import;
mod r#let;
mod r#match;
mod r#return;
mod schema;
mod select;
mod transaction;

pub use control::{
	AssertInvariantsStatement,
	Block,
	CatchClause,
	ForStatement,
	IfStatement,
	TryStatement,
	WhileStatement,
};
pub use r#let::LetStatement;
pub use r#match::{
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
};
use reblessive::Stk;
pub use r#return::ReturnStatement;
pub use schema::{
	AlterStatement,
	AlterTarget,
	BasicIndexDefinition,
	ClassKindName,
	ClassSchemaModeName,
	DefineClassStatement,
	DefineFieldStatement,
	DefineGraphStatement,
	DefineIndexStatement,
	DefinitionMode,
	IndexScopeName,
	InfoStatement,
	InfoTarget,
	RemoveStatement,
	RemoveTarget,
};
pub use select::{
	RecordKey,
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
};
pub use transaction::{
	BeginStatement,
	CommitStatement,
	ExecutionModeName,
	TransactionIsolationName,
};

use super::{
	ast::{Expr, ExprKind, Identifier, TypeName, cover},
	lexer::keywords,
	parser::Parser,
	token::{Symbol, TokenKind},
};
use crate::gql_status::{FerrosGrynnResult, TextSpan};

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Statement {
	pub kind: StatementKind,
	pub span: TextSpan,
	pub timeout: Option<TimeoutValue>,
}

/// Statement-local timeout syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeoutValue {
	Literal {
		value: Box<str>,
		span: TextSpan,
	},
	Parameter(Identifier),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StatementKind {
	Begin(BeginStatement),
	Commit(CommitStatement),
	Cancel,
	Expression(Expr),
	If(IfStatement),
	For(ForStatement),
	While(WhileStatement),
	Break,
	Continue,
	Try(TryStatement),
	Throw(Expr),
	AssertInvariants(AssertInvariantsStatement),
	Attach(AttachStatement),
	Create(CreateStatement),
	Update(MutateStatement),
	UpdateGraph(UpdateGraphStatement),
	Upsert(MutateStatement),
	Delete(DeleteStatement),
	Detach(DetachStatement),
	DefineGraph(DefineGraphStatement),
	DefineClass(DefineClassStatement),
	DefineField(DefineFieldStatement),
	DefineIndex(DefineIndexStatement),
	Remove(RemoveStatement),
	Alter(AlterStatement),
	Info(InfoStatement),
	Import(ImportStatement),
	Let(LetStatement),
	Match(MatchStatement),
	Relate(RelateStatement),
	Return(ReturnStatement),
	Select(SelectStatement),
}
pub use dml::{
	Assignment,
	CreateStatement,
	DeleteStatement,
	DmlData,
	DmlReturn,
	DmlReturnItem,
	DmlTarget,
	ExpectClause,
	MutateStatement,
};
pub use graph_dml::{AttachStatement, DetachStatement, RelateStatement};
pub use graph_properties::{
	GraphPropertyAssignment,
	GraphPropertyMutation,
	GraphPropertyPath,
	UpdateGraphStatement,
};
pub use import::{
	ImportGraphRef,
	ImportOption,
	ImportOptionName,
	ImportOptionValue,
	ImportStatement,
};
