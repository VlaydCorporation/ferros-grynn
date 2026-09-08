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
	MatchStatement,
	MatchStep,
};
use reblessive::Stk;
pub use r#return::ReturnStatement;
pub use schema::{
	AlterStatement,
	AlterTarget,
	BasicIndexDefinition,
	DefineFieldStatement,
	DefineGraphStatement,
	DefineIndexStatement,
	DefineTableStatement,
	DefinitionMode,
	InfoStatement,
	InfoTarget,
	RemoveStatement,
	RemoveTarget,
	TableKindName,
	TableSchemaModeName,
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
	DefineGraph(DefineGraphStatement),
	DefineTable(DefineTableStatement),
	DefineField(DefineFieldStatement),
	DefineIndex(DefineIndexStatement),
	Remove(RemoveStatement),
	Alter(AlterStatement),
	Info(InfoStatement),
	Let(LetStatement),
	Match(MatchStatement),
	Return(ReturnStatement),
	Select(SelectStatement),
}
