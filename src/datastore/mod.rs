//! Database schema and normalized atom/graph state.

/// Database-global atom registry.
mod atom_store;
/// Volatile/local backend ownership and policy conversion.
mod backend;
/// Independently versioned MVCC conflict targets.
mod conflict;
/// Shared datastore constants.
mod consts;
/// Database-versus-graph physical identity.
mod identity;
/// Atomic bulk-import coordination and public reports.
mod import;
/// MVCC-owned class, label, and explicit scalar indexes.
mod index_store;
/// Canonical graph-local LevelPath projection.
mod level_path;
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
/// In-memory database owner and commit serialization.
mod store;
/// Shared durable-analyzer execution used by scans and index builders.
mod text_analyzer;
/// Snapshot-isolated reads, mutations, and commit validation.
mod transaction;
/// Exact vector-distance oracle used by scans and ANN reranking.
mod vector_distance;
pub(crate) use atom_store::{AtomStoreRoot, BaseAtom, GlobalAtomId};
pub(crate) use backend::{StoreBackend, StoreInner, validate_local_options};
pub(crate) use conflict::ConflictKey;
pub(crate) use identity::DataSpaceId;
pub use import::{
	ImportConstraintMode,
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
	IndexBinding,
	IndexEntries,
	IndexStoreRoot,
	index_context,
	rebuild_index_store,
	rebuild_lookup_index_store,
};
pub(crate) use level_path::LEVEL_PATH_PROPERTY;
pub use maintenance::{JobId, MaintenanceJobInfo, MaintenanceJobKind, MaintenanceJobState};
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
	OverlayValue,
	PropertyOverlay,
};
pub use options::{
	CheckpointMode,
	CheckpointPolicy,
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
pub(crate) use schema::ClassIndexOptions;
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
	FulltextIndexOptions,
	FunctionEffect,
	FunctionId,
	GraphSchema,
	HnswIndexOptions,
	IndexId,
	IndexMaintenanceMode,
	IndexScope,
	SchemaAnalyzer,
	SchemaCatalog,
	SchemaFunction,
	SchemaFunctionParameter,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
	VectorDistance,
};
pub use session::GqlSession;
pub(crate) use session::validate_control_structure;
pub use store::Datastore;
pub(crate) use text_analyzer::{AnalyzedToken, analyze_builtin, analyze_function_result};
pub use transaction::{AtomSnapshot, CommitResult, Transaction, TransactionGraphView};
pub(crate) use transaction::{
	QuerySnapshot,
	QueryStatisticsDemand,
	QueryStorageCursor,
	QueryStorageRow,
	ScalarComparison,
	ScalarIndexConstraint,
	ScalarIndexWindow,
	TransactionSavepoint,
};
pub(crate) use vector_distance::{exact_vector_distance, validate_ann_vector};
