mod ast;
pub mod config;
mod lexer;
mod parser;
mod stmt;
mod token;

pub use ast::{
	BinaryOperator,
	ClosureBody,
	ClosureExpr,
	ClosureParameter,
	Expr,
	ExprKind,
	FulltextOperator,
	Identifier,
	KnnDistance,
	KnnSearchArgument,
	Literal,
	ObjectField,
	Program,
	TypeName,
	UnaryOperator,
};
pub use config::GQLConfig;
use fg_gql_status::FerrosGrynnResult;
pub(crate) use stmt::SelectUnionPurpose;
pub use stmt::{
	AlterStatement,
	AlterTarget,
	AnalyzeModeName,
	AnalyzerFilterName,
	AnalyzerTokenizerName,
	AssertInvariantsStatement,
	Assignment,
	AttachStatement,
	BasicIndexDefinition,
	BeginStatement,
	Block,
	BuildGraphSource,
	BuildGraphStatement,
	CatchClause,
	ChangefeedClause,
	ClassKindName,
	ClassSchemaModeName,
	CommitStatement,
	CreateStatement,
	DefineAnalyzerStatement,
	DefineClassStatement,
	DefineEventStatement,
	DefineFieldStatement,
	DefineFunctionStatement,
	DefineGraphIndexStatement,
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
	ForStatement,
	FunctionEffectName,
	FunctionParameterDefinition,
	FunctionSignatureName,
	GraphContract,
	GraphFamilyName,
	GraphIndexFamilyName,
	GraphPropertyAssignment,
	GraphPropertyMutation,
	GraphPropertyPath,
	GraphValidatorName,
	IfStatement,
	ImportGraphRef,
	ImportOption,
	ImportOptionName,
	ImportOptionValue,
	ImportStatement,
	IndexScopeName,
	InfoStatement,
	InfoTarget,
	KeepLevelPathClause,
	KillStatement,
	LetStatement,
	LetValue,
	LiveAckStatement,
	LiveLetStatement,
	LiveOutputName,
	LiveOverflowName,
	LiveSelectStatement,
	MaintenanceIndexOwner,
	MaintenanceOperationName,
	MaintenanceStatement,
	MaintenanceTargetName,
	MatchConstraints,
	MatchContains,
	MatchDirection,
	MatchEdge,
	MatchEndpoint,
	MatchEndpointKind,
	MatchHyperPattern,
	MatchHyperSide,
	MatchLength,
	MatchLinearPattern,
	MatchMetaScope,
	MatchMode,
	MatchPathFilter,
	MatchPathMode,
	MatchPathObjective,
	MatchPattern,
	MatchReturnItem,
	MatchSelector,
	MatchStatement,
	MatchStep,
	MatchWithinDepth,
	MatchWithinPattern,
	MutateStatement,
	ParameterCommentChange,
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
	UpdateGraphStatement,
	VectorDistanceName,
	VectorElementName,
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
