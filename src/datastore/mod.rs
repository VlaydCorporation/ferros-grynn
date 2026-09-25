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
/// Graph-local occurrences and sparse overlays.
mod occurrence;
/// Backend and transaction configuration.
mod options;
/// Immutable MVCC root joining global and graph-local state.
mod root;
/// Versioned class and graph definitions.
mod schema;
/// Persistent GrynnQL statement driver.
mod session;
/// In-memory database owner and commit serialization.
mod store;
/// Snapshot-isolated reads, mutations, and commit validation.
mod transaction;

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
};
pub(crate) use index_store::{
	IndexBinding,
	IndexEntries,
	IndexStoreRoot,
	index_context,
	rebuild_index_store,
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
	TransactionAccess,
	TransactionOptions,
};
pub(crate) use root::{DatabaseRoot, InvariantViolationReport};
pub use schema::{
	ClassField,
	ClassId,
	ClassIndex,
	ClassIndexKind,
	ClassIndexState,
	ClassSchema,
	ClassSchemaMode,
	GraphSchema,
	IndexId,
	IndexScope,
	SchemaCatalog,
	SchemaId,
	SchemaParameter,
	SchemaVersion,
	SystemIndex,
	SystemIndexKind,
};
pub use session::GqlSession;
pub(crate) use session::validate_control_structure;
pub use store::Datastore;
pub use transaction::{AtomSnapshot, CommitResult, Transaction, TransactionGraphView};
pub(crate) use transaction::{
	QuerySnapshot,
	QueryStorageCursor,
	QueryStorageRow,
	ScalarComparison,
	ScalarIndexConstraint,
	TransactionSavepoint,
};
