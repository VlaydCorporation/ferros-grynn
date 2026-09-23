mod dml;
mod graph_dml;
mod r#let;
mod r#match;
mod r#return;
mod schema;
mod select;

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
pub use select::{RecordKey, SelectItem, SelectStatement, SelectTarget};

use super::{
	ast::{Expr, Identifier, TypeName, cover},
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
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StatementKind {
	Attach(AttachStatement),
	Create(CreateStatement),
	Update(MutateStatement),
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
