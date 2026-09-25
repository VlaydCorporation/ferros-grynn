mod ast;
pub mod config;
mod lexer;
mod parser;
mod stmt;
mod token;

pub use ast::{
	BinaryOperator,
	Expr,
	ExprKind,
	Identifier,
	Literal,
	ObjectField,
	Program,
	TypeName,
	UnaryOperator,
};
pub use config::GQLConfig;
use fg_gql_status::FerrosGrynnResult;
pub use stmt::{
	AlterStatement,
	AlterTarget,
	AssertInvariantsStatement,
	Assignment,
	AttachStatement,
	BasicIndexDefinition,
	BeginStatement,
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
	ForStatement,
	GraphPropertyAssignment,
	GraphPropertyMutation,
	GraphPropertyPath,
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
	UpdateGraphStatement,
	WhileStatement,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParserSettings {
	pub max_object_parsing_depth: u32,
	pub max_query_parsing_depth: u32,
}

impl Default for ParserSettings {
	fn default() -> Self {
		Self::from(&GQLConfig::default())
	}
}

impl From<&GQLConfig> for ParserSettings {
	fn from(config: &GQLConfig) -> Self {
		Self {
			max_object_parsing_depth: config.max_object_parsing_depth,
			max_query_parsing_depth: config.max_query_parsing_depth,
		}
	}
}

pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	parser::parse(source, settings)
}

#[cfg(test)]
mod property_tests {
	use proptest::prelude::*;

	use super::{ParserSettings, parse};

	proptest! {
		#[test]
		fn parser_never_panics(source in ".{0,512}") {
			let _ = parse(&source, ParserSettings::default());
		}
	}
}
