mod control;
mod dml;
mod explain;
mod graph_dml;
mod graph_properties;
mod graph_result;
mod import;
mod r#let;
mod maintenance;
mod r#match;
mod r#return;
mod schema;
mod select;
mod transaction;
mod traverse;

pub use control::{
	AssertInvariantsStatement,
	Block,
	CatchClause,
	ForStatement,
	IfStatement,
	TryStatement,
	WhileStatement,
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
	DefineGraphStatement,
	DefineIndexStatement,
	DefineParameterStatement,
	DefinitionMode,
	FunctionEffectName,
	FunctionParameterDefinition,
	FunctionSignatureName,
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
	Explain(ExplainStatement),
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
	Sleep(TimeoutValue),
	AssertInvariants(AssertInvariantsStatement),
	BuildGraph(BuildGraphStatement),
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
	DefineAnalyzer(DefineAnalyzerStatement),
	DefineFunction(DefineFunctionStatement),
	DefineIndex(DefineIndexStatement),
	DefineParameter(DefineParameterStatement),
	Remove(RemoveStatement),
	Alter(AlterStatement),
	Info(InfoStatement),
	Maintenance(MaintenanceStatement),
	Kill(KillStatement),
	Import(ImportStatement),
	Let(LetStatement),
	Match(MatchStatement),
	Relate(RelateStatement),
	Return(ReturnStatement),
	ReturnGraph(ReturnGraph),
	Select(SelectStatement),
	Traverse(TraverseStatement),
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
pub use graph_dml::{AttachStatement, DetachStatement, KeepLevelPathClause, RelateStatement};
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
