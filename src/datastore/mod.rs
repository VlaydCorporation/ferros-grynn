//! Database schema and normalized atom/graph state.

/// Database-global atom registry.
mod atom_store;
/// Volatile/local backend ownership and policy conversion.
mod backend;
/// Durable database correspondence authority and its lookup indexes.
mod bridge_registry;
pub(crate) use bridge_registry::BridgeRegistry;
/// Retained source definitions independent of active class/graph lifetimes.
mod changefeed;
/// Payload-free process counters and snapshot-bound publication observations.
mod changefeed_observation;
/// Bounded TTL prefix publication and closed-source retirement.
mod changefeed_retention;
pub(crate) use changefeed::{CapturedCommit, ChangefeedCatalog, ChangefeedDefinition};
/// Independently versioned MVCC conflict targets.
mod conflict;
/// Shared datastore constants.
mod consts;
/// Accepted invocation bytes, causal ancestry and atomic completion markers.
mod event_outbox;
/// Accepted invocation attempts run sequentially on the maintenance worker.
mod event_worker;
pub(crate) use event_outbox::{EventCause, EventInvocation, EventInvocationState, EventOutbox};
/// Snapshot-root adapter for derived incidence projections.
mod graph_index_relation;
/// Directed landmark snapshot artifacts and their checked physical adapters.
mod graph_path;
pub(crate) use graph_path::LandmarkPathArtifact;
/// Bounded neighbourhood sets, cardinality sketches and landmark refinement.
mod graph_neighbourhood;
pub(crate) use graph_neighbourhood::NeighbourhoodArtifact;
/// Sound reachability labels and bounded exact refinement.
mod graph_reachability;
pub(crate) use graph_reachability::ReachabilityArtifact;
/// Database-versus-graph physical identity.
mod identity;
/// Atomic bulk-import coordination and public reports.
mod import;
/// Shared complete owner-chain and terminal incidence resolution.
mod incidence;
/// MVCC-owned class, label, and explicit scalar indexes.
mod index_store;
/// Canonical graph-local LevelPath projection.
mod level_path;
/// Restorable subscription recipes and transactional lifecycle.
mod live_catalog;
/// Computed history capture epochs, independent of queue delivery.
mod live_history;
/// Payload-free process observations over LIVE preparation and publication.
mod live_observation;
/// Bounded whole-commit consumer state and publication admission.
mod live_queue;
pub(crate) use live_catalog::{
	BindingAdmission,
	LiveBindingDefinition,
	LiveCatalog,
	LiveDefinition,
	LiveRhsScopeAnchor,
	expression_dependencies,
	query_dependencies,
};
pub(crate) use live_history::{
	LiveCaptureEpoch,
	LiveCapturePolicy,
	LiveHistoryMetadata,
	capture_source,
};
pub(crate) use live_queue::SubscriptionHandle;
/// Durable maintenance jobs and their public projections.
mod maintenance;
/// Graph-local occurrences and sparse overlays.
mod occurrence;
/// Backend and transaction configuration.
mod options;
/// Immutable MVCC root joining global and graph-local state.
mod root;
/// Read-only effective resource budgets and pressure counters.
mod runtime_status;
/// Versioned class and graph definitions.
mod schema;
/// Persistent GrynnQL statement driver.
mod session;
/// Fixed-root source history shared by transaction reads and LIVE catch-up.
mod source_history;
/// In-memory database owner and commit serialization.
mod store;
/// Full qualified tensor captures and their checked durable encodings.
mod tensor_artifact;
/// Shared durable-analyzer execution used by scans and index builders.
mod text_analyzer;
pub(crate) use tensor_artifact::TensorArtifact;
/// Snapshot-isolated reads, mutations, and commit validation.
mod transaction;
/// Exact vector-distance oracle used by scans and ANN reranking.
mod vector_distance;
/// Snapshot-owned logical conflict revisions.
mod versions;
pub(crate) use atom_store::{AtomStoreRoot, BaseAtom, GlobalAtomId};
pub(crate) use backend::{PublicationDelta, StoreBackend, StoreInner, validate_local_options};
pub(crate) use conflict::ConflictKey;
pub(crate) use identity::DataSpaceId;
pub use import::{
	ImportChangefeedMode,
	ImportConstraintMode,
	ImportEventsMode,
	ImportIndexMode,
	ImportItemOutcome,
	ImportOptions,
	ImportReport,
	ImportSession,
	ImportViolation,
	ImportViolationPolicy,
	QuarantineId,
	QuarantineReader,
	QuarantinedItem,
};
pub(crate) use index_store::{
	IndexAddress,
	IndexBinding,
	IndexEntries,
	IndexStoreRoot,
	PreparedIndexChange,
	index_context,
	rebuild_index_store,
	rebuild_lookup_index_store,
};
pub(crate) use level_path::LEVEL_PATH_PROPERTY;
pub use maintenance::{
	EventJobInfo,
	JobId,
	MaintenanceJobInfo,
	MaintenanceJobKind,
	MaintenanceJobState,
};
pub(crate) use maintenance::{
	MaintenanceAnalyzeMode,
	MaintenanceCoordinator,
	MaintenanceRunOutcome,
	MaintenanceTarget,
};
pub(crate) use occurrence::{
	GraphOccurrence,
	GraphRoot,
	OccurrenceState,
	OccurrenceStateRef,
	OverlayValue,
	PropertyOverlay,
};
pub use options::{
	CheckpointMode,
	CheckpointPolicy,
	CommitOptions,
	DatastoreOptions,
	InMemoryOptions,
	IoMode,
	IsolationLevel,
	LocalOptions,
	MaintenanceOptions,
	TransactionAccess,
	TransactionOptions,
};
pub(crate) use root::{DatabaseRoot, InvariantViolationReport};
pub use runtime_status::LocalStorageStatus;
pub use schema::{
	AnalyzerFilter,
	AnalyzerId,
	AnalyzerLanguage,
	AnalyzerTokenizer,
	ClassField,
	ClassId,
	ClassIndex,
	ClassIndexKind,
	ClassIndexState,
	ClassSchema,
	ClassSchemaMode,
	DiskAnnIndexOptions,
	EventAsyncOptions,
	EventDefinition,
	EventExecution,
	EventId,
	EventTarget,
	FulltextIndexOptions,
	FunctionEffect,
	FunctionId,
	GeometryIndexOptions,
	GraphIndex,
	GraphIndexCost,
	GraphIndexDefinition,
	GraphSchema,
	HnswIndexOptions,
	IndexId,
	IndexMaintenanceMode,
	IndexScope,
	NeighbourhoodAlgorithm,
	PathIndexAlgorithm,
	ReachabilityAlgorithm,
	SchemaAnalyzer,
	SchemaCatalog,
	SchemaFunction,
	SchemaFunctionParameter,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
	TensorCacheFormat,
	VectorDistance,
};
pub(crate) use schema::{ClassIndexOptions, FieldUpdate};
pub use session::GqlSession;
pub(crate) use session::validate_control_structure;
pub use store::Datastore;
pub(crate) use text_analyzer::{
	AnalyzedToken,
	analyze_builtin,
	analyze_builtin_bounded,
	analyze_function_result,
	analyze_function_result_bounded,
};
pub use transaction::{AtomSnapshot, CommitResult, Transaction, TransactionGraphView};
pub(crate) use transaction::{
	GqlLiveAck,
	GqlLiveAttachment,
	QueryGeometryIndexRead,
	QueryLandmarkPath,
	QueryNeighbourhoodEstimate,
	QueryNeighbourhoodSketch,
	QuerySnapshot,
	QueryStatisticsDemand,
	QueryStorageCursor,
	QueryStorageRow,
	QueryTensorCache,
	ScalarComparison,
	ScalarIndexConstraint,
	ScalarIndexWindow,
	TransactionSavepoint,
};
pub(crate) use vector_distance::{exact_vector_distance, validate_ann_vector};
