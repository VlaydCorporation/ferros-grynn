// mod common;
/// Owned database adapters and durable correspondence references.
pub use fg_engine::bridge;
/// GrynnQL semantic analysis, logical planning and query execution.
pub use fg_engine::gql;
pub use fg_engine::{changefeed, datastore, live, stats};
/// Allocator selection, heap estimates and memory accounting.
use fg_memory as memory;
/// Checked arithmetic and mathematical helpers shared across project
/// subsystems.
pub use fg_runtime::math;
pub use fg_runtime::{ecs, graph};
pub use memory::{
	HeapEstimatorCache,
	Measurable,
	NO_HEAP_ESTIMATOR_CACHE_INSTANCE,
	NoHeapEstimatorCache,
};

/// GrynnQL tokens, syntax trees and parser entry points.
pub mod syn {
	pub use fg_syntax::{
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
		BinaryOperator,
		Block,
		BridgeClause,
		BuildGraphSource,
		BuildGraphStatement,
		CatchClause,
		ChangefeedClause,
		ClassKindName,
		ClassSchemaModeName,
		ClosureBody,
		ClosureExpr,
		ClosureParameter,
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
		Expr,
		ExprKind,
		ForStatement,
		FulltextOperator,
		FunctionEffectName,
		FunctionParameterDefinition,
		FunctionSignatureName,
		GQLConfig,
		GraphContract,
		GraphFamilyName,
		GraphIndexFamilyName,
		GraphPropertyAssignment,
		GraphPropertyMutation,
		GraphPropertyPath,
		GraphValidatorName,
		Identifier,
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
		KnnDistance,
		KnnSearchArgument,
		LetStatement,
		LetValue,
		Literal,
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
		ObjectField,
		ParameterCommentChange,
		ParserSettings,
		Program,
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
		TypeName,
		UnaryOperator,
		UpdateGraphStatement,
		VectorDistanceName,
		VectorElementName,
		WhileStatement,
		config,
		parse,
	};
}
#[cfg(feature = "storage-engine-prototype-api")]
#[doc(hidden)]
pub use fg_engine::storage_engine_prototype_api;
/// Owned runtime values, explicit storage representations and columnar
/// adapters.
pub mod values;
// mod asynchronous;

/// Shared assertions available only in the unit-test harness.
#[cfg(test)]
#[path = "../tests/common/assertions.rs"]
pub mod test_assertions;

/// Project-wide structured statuses, diagnostics, outcomes and errors.
pub mod gql_status {
	pub use fg_gql_status::*;
}

/// Builds the existing typed LIVE selection recipe.
pub use fg_engine::live_graph_select;
/// Implements the shared zero-owned-heap contract.
pub use fg_memory::impl_zero_heap;
