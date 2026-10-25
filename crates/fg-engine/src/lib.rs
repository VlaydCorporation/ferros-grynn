//! Datastore composition, physical persistence and GQL execution.
// mod common;
/// Owned database adapters and durable correspondence references.
pub mod bridge;
pub mod changefeed;
pub mod datastore;
pub use fg_runtime::ecs;
/// GrynnQL semantic analysis, logical planning and query execution.
pub mod gql;
pub mod graph {
	pub(crate) use fg_runtime::__internal::{
		DirectedLandmarks,
		DistanceInterval,
		LandmarkBuildLimits,
		LandmarkDistanceRows,
		PathTraceArena,
		PathTraceId,
		ProjectedCondensation,
		ProjectedRelation,
		RelationCsr,
		SlotAllocator,
		bellman_ford_with_check,
		bfs_with_check,
		condensation_with_check,
		dijkstra_to_with_control,
		estimated_topology_heap_bytes,
		find_cuts_with_check,
		has_cycle_directed_with_check,
		has_cycle_undirected_with_check,
		minimum_spanning_forest_with_check,
		nested_graph_reference,
		remap_topology,
		strongly_connected_components_with_check,
		topological_sort_with_check,
		validate_nested_references,
		validate_topology_constraints,
		visit_endpoint_steps,
		weakly_connected_components_with_check,
	};
	pub use fg_runtime::graph::*;
	pub mod tensor {
		pub(crate) use fg_runtime::__internal::{tensor_build as build, tensor_control as control};
		pub use fg_runtime::graph::tensor::*;
	}
}
pub mod live;
/// Checked arithmetic and mathematical helpers shared across project
/// subsystems.
pub mod math {
	pub(crate) use fg_runtime::__internal::HyperLogLog;
	pub use fg_runtime::math::*;
}
/// Allocator selection, heap estimates and memory accounting.
use fg_memory as memory;
pub use memory::{
	HeapEstimatorCache,
	Measurable,
	NO_HEAP_ESTIMATOR_CACHE_INSTANCE,
	NoHeapEstimatorCache,
};
pub mod stats;
mod storage;
/// GrynnQL tokens, syntax trees and parser entry points.
pub mod syn {
	pub(crate) use fg_syntax::{
		__internal as split_internal,
		__internal::{SelectUnionPurpose, has_bridge_clause},
	};
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
pub use storage::prototype_api as storage_engine_prototype_api;
/// Owned runtime values, explicit storage representations and columnar
/// adapters.
pub mod values;
// mod asynchronous;

/// Shared assertions available only in the unit-test harness.
#[cfg(test)]
pub use fg_test_support as test_assertions;

/// Project-wide structured statuses, diagnostics, outcomes and errors.
pub mod gql_status {
	pub use fg_gql_status::*;
}

/// Implements the shared zero-owned-heap contract.
pub use fg_memory::impl_zero_heap;
