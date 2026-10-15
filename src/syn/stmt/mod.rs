mod bridge;
mod changefeed;
pub use bridge::BridgeClause;
/// Executable blocks, loops and control-flow syntax.
mod control;
/// Atom creation, update and deletion syntax.
mod dml;
/// Class/graph event definitions and ordered lifecycle actions.
mod event;
mod explain;
/// Graph membership and incidence mutation syntax.
mod graph_dml;
/// Graph property statement syntax.
mod graph_properties;
mod graph_result;
/// Bounded import statement syntax.
mod import;
/// Lexical binding declarations.
mod r#let;
mod live;
mod maintenance;
/// Graph patterns, path bounds and MATCH syntax.
mod r#match;
/// RETURN statement syntax.
mod r#return;
/// Schema definition, alteration and removal syntax.
mod schema;
/// SELECT sources, clauses and projections.
mod select;
/// BEGIN, COMMIT and CANCEL syntax.
mod transaction;
mod traverse;

pub use changefeed::{ChangefeedClause, ShowChangesStatement, ShowChangesTarget};
pub use control::{
	AssertInvariantsStatement,
	Block,
	CatchClause,
	ForStatement,
	IfStatement,
	TryStatement,
	WhileStatement,
};
pub use event::{
	DefineEventStatement,
	EventAlterAction,
	EventAsyncName,
	EventBody,
	EventOwnerName,
};
pub use explain::{ExplainFormat, ExplainMode, ExplainStatement};
pub use graph_result::{
	BuildGraphSource,
	BuildGraphStatement,
	GraphContract,
	GraphFamilyName,
	GraphValidatorName,
	ReturnGraph,
};
pub use r#let::{LetStatement, LetValue};
pub use live::{
	LiveAckStatement,
	LiveLetStatement,
	LiveOutputName,
	LiveOverflowName,
	LiveSelectStatement,
};
pub use maintenance::{
	AnalyzeModeName,
	KillStatement,
	MaintenanceIndexOwner,
	MaintenanceOperationName,
	MaintenanceStatement,
	MaintenanceTargetName,
};
pub use r#match::{
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
};
use reblessive::Stk;
pub use r#return::ReturnStatement;
pub use schema::{
	AlterStatement,
	AlterTarget,
	AnalyzerFilterName,
	AnalyzerTokenizerName,
	BasicIndexDefinition,
	ClassKindName,
	ClassSchemaModeName,
	DefineAnalyzerStatement,
	DefineClassStatement,
	DefineFieldStatement,
	DefineFunctionStatement,
	DefineGraphIndexStatement,
	DefineGraphStatement,
	DefineIndexStatement,
	DefineParameterStatement,
	DefinitionMode,
	FunctionEffectName,
	FunctionParameterDefinition,
	FunctionSignatureName,
	GraphIndexFamilyName,
	IndexScopeName,
	InfoStatement,
	InfoTarget,
	ParameterCommentChange,
	RemoveStatement,
	RemoveTarget,
	VectorDistanceName,
	VectorElementName,
};
pub use select::{
	RecordKey,
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
	SelectUnionPurpose,
};
pub use transaction::{
	BeginStatement,
	CommitStatement,
	ExecutionModeName,
	TransactionIsolationName,
};
pub use traverse::{TraverseSource, TraverseStatement, TraverseStep};

use super::{
	ast::{Expr, ExprKind, Identifier, TypeName, cover},
	lexer::keywords,
	parser::Parser,
	token::{Symbol, TokenKind},
};
use crate::gql_status::{FerrosGrynnResult, TextSpan};

/// Statement syntax with its source span and optional timeout.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Statement {
	/// Selected operation or representation discriminator.
	pub kind: StatementKind,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
	/// Optional time limit attached to this operation.
	pub timeout: Option<TimeoutValue>,
}

/// Statement-local timeout syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeoutValue {
	/// Timeout supplied as a literal duration.
	Literal {
		/// Payload retained for the operation or its expected result.
		value: Box<str>,
		/// Source byte range used to report diagnostics for this syntax
		/// element.
		span: TextSpan,
	},
	/// Timeout supplied through a query parameter.
	Parameter(
		/// Timeout supplied through a query parameter.
		Identifier,
	),
}

/// Executable and declarative statement forms recognized by the parser.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StatementKind {
	/// A EXPLAIN statement or control operation.
	Explain(
		/// A EXPLAIN statement or control operation.
		ExplainStatement,
	),
	/// A BEGIN statement or control operation.
	Begin(
		/// A BEGIN statement or control operation.
		BeginStatement,
	),
	/// A COMMIT statement or control operation.
	Commit(
		/// A COMMIT statement or control operation.
		CommitStatement,
	),
	/// A CANCEL statement or control operation.
	Cancel,
	/// A EXPRESSION statement or control operation.
	Expression(
		/// A EXPRESSION statement or control operation.
		Expr,
	),
	/// A IF statement or control operation.
	If(
		/// A IF statement or control operation.
		IfStatement,
	),
	/// A FOR statement or control operation.
	For(
		/// A FOR statement or control operation.
		ForStatement,
	),
	/// A WHILE statement or control operation.
	While(
		/// A WHILE statement or control operation.
		WhileStatement,
	),
	/// A BREAK statement or control operation.
	Break,
	/// A CONTINUE statement or control operation.
	Continue,
	/// A TRY statement or control operation.
	Try(
		/// A TRY statement or control operation.
		TryStatement,
	),
	/// A THROW statement or control operation.
	Throw(
		/// A THROW statement or control operation.
		Expr,
	),
	/// A SLEEP statement or control operation.
	Sleep(
		/// A SLEEP statement or control operation.
		TimeoutValue,
	),
	/// A ASSERT INVARIANTS statement or control operation.
	AssertInvariants(
		/// A ASSERT INVARIANTS statement or control operation.
		AssertInvariantsStatement,
	),
	/// A BUILD GRAPH statement or control operation.
	BuildGraph(
		/// A BUILD GRAPH statement or control operation.
		BuildGraphStatement,
	),
	/// A ATTACH statement or control operation.
	Attach(
		/// A ATTACH statement or control operation.
		AttachStatement,
	),
	/// A CREATE statement or control operation.
	Create(
		/// A CREATE statement or control operation.
		CreateStatement,
	),
	/// A UPDATE statement or control operation.
	Update(
		/// A UPDATE statement or control operation.
		MutateStatement,
	),
	/// A UPDATE GRAPH statement or control operation.
	UpdateGraph(
		/// A UPDATE GRAPH statement or control operation.
		UpdateGraphStatement,
	),
	/// A UPSERT statement or control operation.
	Upsert(
		/// A UPSERT statement or control operation.
		MutateStatement,
	),
	/// A DELETE statement or control operation.
	Delete(
		/// A DELETE statement or control operation.
		DeleteStatement,
	),
	/// A DETACH statement or control operation.
	Detach(
		/// A DETACH statement or control operation.
		DetachStatement,
	),
	/// A DEFINE GRAPH statement or control operation.
	DefineGraph(
		/// A DEFINE GRAPH statement or control operation.
		DefineGraphStatement,
	),
	/// A DEFINE CLASS statement or control operation.
	DefineClass(
		/// A DEFINE CLASS statement or control operation.
		DefineClassStatement,
	),
	/// A DEFINE FIELD statement or control operation.
	DefineField(
		/// A DEFINE FIELD statement or control operation.
		DefineFieldStatement,
	),
	/// A DEFINE ANALYZER statement or control operation.
	DefineAnalyzer(
		/// A DEFINE ANALYZER statement or control operation.
		DefineAnalyzerStatement,
	),
	/// A DEFINE FUNCTION statement or control operation.
	DefineFunction(
		/// A DEFINE FUNCTION statement or control operation.
		DefineFunctionStatement,
	),
	/// Versioned source-owned event definition.
	DefineEvent(
		/// Versioned source-owned event definition.
		DefineEventStatement,
	),
	/// A DEFINE INDEX statement or control operation.
	DefineIndex(
		/// A DEFINE INDEX statement or control operation.
		DefineIndexStatement,
	),
	/// Graph-owned snapshot index definition.
	DefineGraphIndex(
		/// Graph-owned snapshot index definition.
		DefineGraphIndexStatement,
	),
	/// A DEFINE PARAMETER statement or control operation.
	DefineParameter(
		/// A DEFINE PARAMETER statement or control operation.
		DefineParameterStatement,
	),
	/// A REMOVE statement or control operation.
	Remove(
		/// A REMOVE statement or control operation.
		RemoveStatement,
	),
	/// A ALTER statement or control operation.
	Alter(
		/// A ALTER statement or control operation.
		AlterStatement,
	),
	/// A INFO statement or control operation.
	Info(
		/// A INFO statement or control operation.
		InfoStatement,
	),
	/// Durable whole-commit history read.
	ShowChanges(
		/// Durable whole-commit history read.
		ShowChangesStatement,
	),
	/// A MAINTENANCE statement or control operation.
	Maintenance(
		/// A MAINTENANCE statement or control operation.
		MaintenanceStatement,
	),
	/// A KILL statement or control operation.
	Kill(
		/// A KILL statement or control operation.
		KillStatement,
	),
	/// A IMPORT statement or control operation.
	Import(
		/// A IMPORT statement or control operation.
		ImportStatement,
	),
	/// A LET statement or control operation.
	Let(
		/// A LET statement or control operation.
		LetStatement,
	),
	/// Registers one transactional LIVE query incarnation.
	LiveSelect(
		/// Registers one transactional LIVE query incarnation.
		LiveSelectStatement,
	),
	/// Defines one transactional reactive binding identity.
	LiveLet(
		/// Defines one transactional reactive binding identity.
		LiveLetStatement,
	),
	/// Confirms one issued whole-commit delivery.
	AckLive(
		/// Confirms one issued whole-commit delivery.
		LiveAckStatement,
	),
	/// A MATCH statement or control operation.
	Match(
		/// A MATCH statement or control operation.
		MatchStatement,
	),
	/// A RELATE statement or control operation.
	Relate(
		/// A RELATE statement or control operation.
		RelateStatement,
	),
	/// A RETURN statement or control operation.
	Return(
		/// A RETURN statement or control operation.
		ReturnStatement,
	),
	/// A RETURN GRAPH statement or control operation.
	ReturnGraph(
		/// A RETURN GRAPH statement or control operation.
		ReturnGraph,
	),
	/// A SELECT statement or control operation.
	Select(
		/// A SELECT statement or control operation.
		SelectStatement,
	),
	/// A TRAVERSE statement or control operation.
	Traverse(
		/// A TRAVERSE statement or control operation.
		TraverseStatement,
	),
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
pub use graph_dml::{
	AttachStatement,
	DetachStatement,
	KeepLevelPathClause,
	RelateEndpoint,
	RelateEndpointTarget,
	RelateStatement,
};
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
