/// Owned syntax-tree expressions and common nodes.
mod ast;
/// Depth configuration shared by GrynnQL parser entry points.
pub mod config;
/// UTF-8 token scanning and literal recognition.
mod lexer;
/// Parser state, precedence and depth limits.
mod parser;
/// Statement syntax nodes and parsing.
mod stmt;
/// Token kinds and UTF-8 source spans.
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
	BridgeClause,
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
	MatchDistinctness,
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

/// Configured depth limits for parsing one GrynnQL source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParserSettings {
	/// Maximum nesting depth accepted while parsing object values.
	pub max_object_parsing_depth: u32,
	/// Maximum expression or statement nesting depth accepted by the parser.
	pub max_query_parsing_depth: u32,
}

impl Default for ParserSettings {
	/// Constructs the default initial state of `ParserSettings`.
	fn default() -> Self {
		Self::from(&GQLConfig::default())
	}
}

impl From<&GQLConfig> for ParserSettings {
	/// Converts `&GQLConfig` into `ParserSettings`.
	fn from(config: &GQLConfig) -> Self {
		Self {
			max_object_parsing_depth: config.max_object_parsing_depth,
			max_query_parsing_depth: config.max_query_parsing_depth,
		}
	}
}

/// Parses and validates the input syntax for this type.
///
/// # Errors
///
/// Propagates validation or execution failures from the underlying operation.
pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	parser::parse(source, settings)
}

/// Examines actual tokens, excluding quoted names, strings and comments.
fn has_bridge_clause(source: &str) -> FerrosGrynnResult<bool> {
	let tokens = lexer::lex(source)?;
	Ok(tokens.windows(2).any(|pair| matches!((&pair[0].kind, &pair[1].kind), (token::TokenKind::Word { value: left, quoted: false }, token::TokenKind::Word { value: right, quoted: false }) if left.eq_ignore_ascii_case("WITH") && right.eq_ignore_ascii_case("BRIDGE"))))
}

#[cfg(test)]
/// Regression tests for syn behavior.
mod property_tests {
	use proptest::prelude::*;

	use super::{ParserSettings, parse};

	proptest! {
		/// Verifies parser never panics.
		#[test]
		fn parser_never_panics(source in ".{0,512}") {
			drop(parse(&source, ParserSettings::default()));
		}
	}
}

/// Owner-side lowering constructors, separate from the public syntax SDK.
pub mod split_internal;
