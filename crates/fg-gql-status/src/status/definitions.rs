use std::fmt::{self, Display, Formatter};

use ferros_grynn_macros::define_status_codes;

use super::{
	Condition,
	Domain,
	ErrorCategory,
	ErrorClassification,
	NotificationClassification,
	StatusCode,
	StatusDefinition,
	StatusKind,
};
use crate::{
	diagnostic::Severity,
	formatting::{DisplayValue, Identifier, StringLiteral, ValueType},
};

define_status_codes!(
	SuccessfulCompletion, "FG-00000" => {
		kind: Completion,
		condition: Condition::SuccessfulCompletion,
		domain: Domain::Query,
	},
	OmittedResult, "FG-00001" => {
		kind: Completion,
		condition: Condition::SuccessfulCompletion,
		domain: Domain::Query,
		subcondition: "omitted result",
	},
	NoData, "FG-02000" => {
		kind: Completion,
		condition: Condition::NoData,
		domain: Domain::Query,
	},
	DeprecatedFeature, "FG-01F01" => {
		kind: Notification {
			classification: NotificationClassification::Deprecation,
			severity: Severity::Warning,
		},
		condition: Condition::Warning,
		domain: Domain::Query,
		subcondition: "deprecated feature",
		message: "{feature} is deprecated; use {replacement} instead.",
		params: {
			feature: String => Identifier,
			replacement: String => Identifier,
		},
	},
	PerformanceAdvisory, "FG-03F01" => {
		kind: Notification {
			classification: NotificationClassification::Performance,
			severity: Severity::Information,
		},
		condition: Condition::Informational,
		domain: Domain::Query,
		subcondition: "performance advisory",
		message: "{message}",
		params: {
			message: String => DisplayValue,
		},
	},
	InvalidArgument, "FG-22N11" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "invalid argument",
		message: "{message}",
		params: {
			message: String => DisplayValue,
		},
	},
	ConversionError, "FG-22N37" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "invalid coercion",
		message: "Cannot convert {value} to {target_type}; the rejected value was {value}.",
		params: {
			value: String => StringLiteral,
			target_type: String => ValueType,
		},
	},
	DivisionByZero, "FG-22012" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "division by zero",
	},
	IntervalFieldOverflow, "FG-22015" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "interval field overflow",
	},
	OperationOverflow, "FG-22N28" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "operation overflow",
		message: "The result of operation {operation} cannot be represented.",
		params: {
			operation: String => StringLiteral,
		},
	},
	NumericValueOutOfRange, "FG-22003" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "numeric value out of range",
		message: "The numeric value {value} is outside the required range.",
		params: {
			value: String => DisplayValue,
		},
	},
	SpecifiedNumericValueOutOfRange, "FG-22N03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "specified numeric value out of range",
		message: "Expected {component} to be of type {value_type} in the range {lower} to {upper}, but found {value}.",
		params: {
			component: String => StringLiteral,
			value_type: String => ValueType,
			lower: i64 => DisplayValue,
			upper: i64 => DisplayValue,
			value: String => DisplayValue,
		},
	},
	StructBuildError, "FG-22F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "struct build error",
		message: "Cannot build struct {identifier}.",
		params: {
			identifier: String => Identifier,
		},
	},
	UnknownField, "FG-22F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Semantic,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "unknown field",
		message: "Unknown field {field}.",
		params: {
			field: String => Identifier,
		},
	},
	NonExactDivision, "FG-22F03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Query,
		subcondition: "non-exact division",
	},
	TemporalProcessingError, "FG-22F04" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::External,
		subcondition: "temporal value processing failed",
		message: "The temporal value could not be processed.",
	},
	InvalidTextBoundary, "FG-22F05" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid UTF-8 character boundary",
		message: "Byte index {index} is not a UTF-8 character boundary for a string of {length} bytes.",
		params: {
			index: u64 => DisplayValue,
			length: u64 => DisplayValue,
		},
	},
	InvalidGeometryCoordinateDimension, "FG-22F06" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid geometry coordinate dimension",
		message: "A WGS-84 position must contain exactly two coordinates, but found {actual}.",
		params: {
			actual: u64 => DisplayValue,
		},
	},
	NonFiniteGeometryCoordinate, "FG-22F07" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "non-finite geometry coordinate",
		message: "The geometry coordinate at index {index} must be finite.",
		params: {
			index: u64 => DisplayValue,
		},
	},
	GeometryCoordinateOutOfBounds, "FG-22F08" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "geometry coordinate out of bounds",
		message: "Geometry coordinate {component} must be in the range {lower} to {upper}, but found {value}.",
		params: {
			component: String => Identifier,
			lower: i64 => DisplayValue,
			upper: i64 => DisplayValue,
			value: f64 => DisplayValue,
		},
	},
	InvalidGeoJsonGeometry, "FG-22F09" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid GeoJSON geometry",
		message: "The input is not valid GeoJSON geometry syntax.",
	},
	ExpectedGeoJsonGeometry, "FG-22F10" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "expected GeoJSON geometry",
		message: "Expected a GeoJSON geometry, but found {actual}.",
		params: {
			actual: String => ValueType,
		},
	},
	InvalidRecordIdSyntax, "FG-22F11" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid record ID syntax",
		message: "A record ID must use the class:id form.",
	},
	EmptyRecordIdClass, "FG-22F12" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "empty record ID class",
		message: "The class component of a record ID cannot be empty.",
	},
	EmptyRecordIdPart, "FG-22F13" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "empty record ID part",
		message: "The value component of a record ID cannot be empty.",
	},
	InvalidRecordIdPart, "FG-22F14" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid record ID part",
		message: "A value of type {value_type} cannot be used in a record ID.",
		params: {
			value_type: String => ValueType,
		},
	},
	InvalidCompositeRecordId, "FG-22F15" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid composite record ID",
		message: "A composite record ID must contain a valid JSON string, array, or object.",
	},
	ReadOnlyTransaction, "FG-25006" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::InvalidTransactionState,
		domain: Domain::Transaction,
		subcondition: "read-only transaction",
		message: "Transaction {transaction} is read-only.",
		params: {
			transaction: u64 => DisplayValue,
		},
	},
	SnapshotExpired, "FG-25F01" => {
		kind: Error {
			classification: ErrorClassification::TransientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::InvalidTransactionState,
		domain: Domain::Transaction,
		subcondition: "snapshot age limit exceeded",
		message: "Transaction {transaction} exceeded the snapshot age limit of {max_age_seconds} seconds.",
		params: {
			transaction: u64 => DisplayValue,
			max_age_seconds: u64 => DisplayValue,
		},
		hint: "Retry the operation in a new transaction.",
	},
	SnapshotCancelled, "FG-25F02" => {
		kind: Error {
			classification: ErrorClassification::TransientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::InvalidTransactionState,
		domain: Domain::Transaction,
		subcondition: "snapshot cancelled",
		message: "Transaction {transaction} was cancelled by storage maintenance.",
		params: {
			transaction: u64 => DisplayValue,
		},
		hint: "Retry the operation in a new transaction.",
	},
	ImportInProgress, "FG-25F03" => {
		kind: Error {
			classification: ErrorClassification::TransientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::InvalidTransactionState,
		domain: Domain::Transaction,
		subcondition: "import in progress",
		message: "A database-wide import is in progress; concurrent writers are not allowed.",
		hint: "Retry the complete write after the import finishes.",
	},
	SerializationFailure, "FG-40001" => {
		kind: Error {
			classification: ErrorClassification::TransientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::TransactionRollback,
		domain: Domain::Transaction,
		subcondition: "serialization failure",
		message: "Transaction {transaction} could not be serialized because {reason}.",
		params: {
			transaction: u64 => DisplayValue,
			reason: String => DisplayValue,
		},
		hint: "Retry the complete transaction.",
	},
	ConditionFailed, "FG-40F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::TransactionRollback,
		domain: Domain::Transaction,
		subcondition: "mutation precondition failed",
		message: "The EXPECT precondition failed for {target}.",
		params: {
			target: String => DisplayValue,
		},
	},
	ColumnarTypeConflict, "FG-22F16" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "incompatible property column values",
		message: "Property {key_id} cannot be materialized as one Arrow column because types {left} and {right} are incompatible.",
		params: {
			key_id: u64 => DisplayValue,
			left: String => ValueType,
			right: String => ValueType,
		},
	},
	UnsupportedColumnarValue, "FG-22F17" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "unsupported property column value",
		message: "Property {key_id} contains {value_type}, which cannot yet be materialized in an Arrow property batch.",
		params: {
			key_id: u64 => DisplayValue,
			value_type: String => ValueType,
		},
	},
	InvalidPropertyBatch, "FG-22F18" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Value,
		subcondition: "invalid property batch",
		message: "The property batch is invalid: {reason}.",
		params: {
			reason: String => DisplayValue,
		},
	},
	AtomDoesNotExist, "FG-G1F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "atom does not exist",
		message: "The atom of type {atom_type} with id {id} does not exist.",
		params: {
			atom_type: String => ValueType,
			id: String => DisplayValue,
		},
	},
	GraphWriteBackUnavailable, "FG-G1F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "graph write-back is unavailable",
		message: "This graph has no materialization origin and cannot be written back.",
	},
	GraphWorkspaceStale, "FG-G1F03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "graph workspace is stale",
		message: "The workspace is bound to graph {expected_identity} at revision {expected_revision}, but the graph is {actual_identity} at revision {actual_revision}.",
		params: {
			expected_identity: String => DisplayValue,
			expected_revision: u64 => DisplayValue,
			actual_identity: String => DisplayValue,
			actual_revision: u64 => DisplayValue,
		},
	},
	SnapshotUnavailable, "FG-G1F04" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "graph snapshot is unavailable",
		message: "Graph {identity} snapshot {snapshot} is unavailable.",
		params: {
			identity: String => DisplayValue,
			snapshot: String => DisplayValue,
		},
	},
	GraphIdentityMismatch, "FG-G1F05" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "graph identity mismatch",
		message: "The value belongs to graph {expected}, but resolver graph {actual} was supplied.",
		params: {
			expected: String => DisplayValue,
			actual: String => DisplayValue,
		},
	},
	LiveGraphStateError, "FG-G1F06" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "invalid live graph state",
		message: "The live graph operation is invalid in its current state: {reason}.",
		params: {
			reason: String => DisplayValue,
		},
	},
	GraphDoesNotExist, "FG-G1F07" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "graph does not exist",
		message: "Graph {graph} does not exist.",
		params: {
			graph: String => StringLiteral,
		},
	},
	GraphAlreadyExists, "FG-G1F08" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DependentObjectError,
		domain: Domain::Graph,
		subcondition: "graph already exists",
		message: "Graph {graph} already exists.",
		params: {
			graph: String => StringLiteral,
		},
	},
	EmptyEdgeEndpoints, "FG-G2F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GraphTypeViolation,
		domain: Domain::Graph,
		subcondition: "edge endpoints are empty",
		message: "An edge must have at least one endpoint.",
	},
	EdgeCreationError, "FG-G2F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GraphTypeViolation,
		domain: Domain::Graph,
		subcondition: "cannot create edge",
		message: "The edge could not be created.",
	},
	AtomKindMismatch, "FG-G2F03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::GraphTypeViolation,
		domain: Domain::Graph,
		subcondition: "atom kind mismatch",
		message: "Atom {id} has type {actual}, but this operation requires {expected}.",
		params: {
			id: String => DisplayValue,
			actual: String => ValueType,
			expected: String => ValueType,
		},
	},
	GraphConstraintViolation, "FG-G2F04" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::GraphTypeViolation,
		domain: Domain::Graph,
		subcondition: "graph constraint violation",
		message: "The value does not satisfy the {graph_type} graph constraint: {reason}.",
		params: {
			graph_type: String => ValueType,
			reason: String => DisplayValue,
		},
	},
	QueryTooLarge, "FG-54F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Query,
		subcondition: "query too large",
		message: "The query exceeds the maximum supported size of 4,294,967,295 bytes.",
	},
	RecordIdNestingTooDeep, "FG-54F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Value,
		subcondition: "record ID nesting limit exceeded",
		message: "Record ID nesting exceeds the maximum depth of {maximum_depth}.",
		params: {
			maximum_depth: u64 => DisplayValue,
		},
	},
	GraphCapacityExceeded, "FG-54F03" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Graph,
		subcondition: "graph capacity exceeded",
		message: "The graph has exhausted the available {atom_type} slot space.",
		params: {
			atom_type: String => ValueType,
		},
	},
	PropertyDictionaryCapacityExceeded, "FG-54F04" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Value,
		subcondition: "property dictionary capacity exceeded",
		message: "The property dictionary has exhausted the available 32-bit key space.",
	},
	UnknownStorageTypeTag, "FG-22S01" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "unknown storage value type tag",
		message: "Storage value tag {tag} at byte offset {offset} is not defined by format version 1.",
		params: {
			tag: u64 => DisplayValue,
			offset: u64 => DisplayValue,
		},
	},
	InvalidStorageValueEncoding, "FG-22S02" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "invalid storage value encoding",
		message: "Storage value at byte offset {offset} is invalid: {reason}.",
		params: {
			offset: u64 => DisplayValue,
			reason: String => DisplayValue,
		},
	},
	DecimalPrecisionExceeded, "FG-22S03" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "decimal precision exceeds the runtime representation",
		message: "Decimal mantissa {mantissa} with scale {scale} cannot be represented by rust_decimal.",
		params: {
			mantissa: String => DisplayValue,
			scale: u64 => DisplayValue,
		},
	},
	UnresolvedStorageReference, "FG-22S04" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "unresolved storage reference",
		message: "The {reference_type} storage reference {reference} is not present in the conversion context.",
		params: {
			reference_type: String => ValueType,
			reference: String => DisplayValue,
		},
	},
	UnsupportedStorageValue, "FG-22S05" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Type,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "runtime value is not persistable",
		message: "Runtime value type {source_type} cannot be represented as StorageValue.",
		params: {
			source_type: String => ValueType,
		},
	},
	InvalidArchiveEnvelope, "FG-22S06" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "invalid archive envelope",
		message: "The versioned archive envelope is invalid: {reason}.",
		params: {
			reason: String => DisplayValue,
		},
	},
	StorageContextConflict, "FG-22S07" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "conflicting storage conversion context",
		message: "The storage conversion context contains conflicting {reference_type} mapping for {reference}.",
		params: {
			reference_type: String => ValueType,
			reference: String => DisplayValue,
		},
	},
	InvalidStoragePage, "FG-22S08" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "invalid storage page",
		message: "Storage page {page_no} is invalid: {reason}.",
		params: {
			page_no: u64 => DisplayValue,
			reason: String => DisplayValue,
		},
	},
	InvalidStorageFile, "FG-22S09" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::DataException,
		domain: Domain::Storage,
		subcondition: "invalid storage file",
		message: "The storage file layout is invalid: {reason}.",
		params: {
			reason: String => DisplayValue,
		},
	},
	StorageDecodeLimitExceeded, "FG-54S01" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Storage,
		subcondition: "storage value decode limit exceeded",
		message: "Decoded {limit} is {actual}, exceeding the configured maximum of {maximum}.",
		params: {
			limit: String => Identifier,
			actual: u64 => DisplayValue,
			maximum: u64 => DisplayValue,
		},
	},
	UnsupportedStorageFormat, "FG-51S01" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::SystemConfigurationOrOperationException,
		domain: Domain::Storage,
		subcondition: "unsupported storage format",
		message: "Storage format version {actual} is unsupported; this engine reads version {supported}.",
		params: {
			actual: u64 => DisplayValue,
			supported: u64 => DisplayValue,
		},
	},
	DirectIoUnavailable, "FG-51I01" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::SystemConfigurationOrOperationException,
		domain: Domain::Io,
		subcondition: "direct I/O unavailable",
		message: "Direct I/O is unavailable: {reason}.",
		params: {
			reason: String => DisplayValue,
		},
	},
	StorageIoFailure, "FG-51I02" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::SystemConfigurationOrOperationException,
		domain: Domain::Io,
		subcondition: "storage I/O failure",
		message: "Storage I/O operation {operation} failed.",
		params: {
			operation: String => DisplayValue,
		},
	},
	LocalDatabaseBusy, "FG-51I03" => {
		kind: Error {
			classification: ErrorClassification::TransientError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::SystemConfigurationOrOperationException,
		domain: Domain::Io,
		subcondition: "local database already owned",
		message: "The local database is already open by another owner.",
	},
	IoModeUnavailable, "FG-51I04" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::SystemConfigurationOrOperationException,
		domain: Domain::Io,
		subcondition: "storage I/O mode unavailable",
		message: "Storage I/O mode {mode} is unavailable: {reason}.",
		params: {
			mode: String => Identifier,
			reason: String => DisplayValue,
		},
	},
	QueryParseError, "FG-42001" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::SyntaxErrorOrAccessRuleViolation,
		domain: Domain::Query,
		subcondition: "invalid syntax",
		message: "{message}",
		params: {
			message: String => DisplayValue,
		},
	},
	ClassDoesNotExist, "FG-42F01" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Semantic,
			severity: Severity::Error,
		},
		condition: Condition::SyntaxErrorOrAccessRuleViolation,
		domain: Domain::Query,
		subcondition: "class does not exist",
		message: "Class {class} does not exist.",
		params: {
			class: String => Identifier,
		},
	},
	ClassAlreadyExists, "FG-42F02" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Semantic,
			severity: Severity::Error,
		},
		condition: Condition::SyntaxErrorOrAccessRuleViolation,
		domain: Domain::Query,
		subcondition: "class already exists",
		message: "Class {class} already exists.",
		params: {
			class: String => Identifier,
		},
	},
	QueryFeatureNotSupported, "FG-50F03" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Planning,
			severity: Severity::Error,
		},
		condition: Condition::GeneralProcessingException,
		domain: Domain::Query,
		subcondition: "feature not supported",
		message: "Query feature {feature} is not supported yet.",
		params: {
			feature: String => Identifier,
		},
	},
	QueryTextTooLarge, "FG-54F05" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Query,
		subcondition: "query text size limit exceeded",
		message: "Query text exceeds the maximum size of {maximum_bytes} bytes.",
		params: {
			maximum_bytes: u64 => DisplayValue,
		},
	},
	QueryNestingTooDeep, "FG-54F06" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Syntax,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Query,
		subcondition: "query nesting limit exceeded",
		message: "Query nesting exceeds the maximum depth of {maximum_depth}.",
		params: {
			maximum_depth: u64 => DisplayValue,
		},
	},
	TransactionWriteSetLimitExceeded, "FG-54F07" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Transaction,
		subcondition: "transaction write-set limit exceeded",
		message: "Transaction {transaction} write set uses {actual_bytes} bytes, exceeding the {maximum_bytes} byte limit.",
		params: {
			transaction: u64 => DisplayValue,
			actual_bytes: u64 => DisplayValue,
			maximum_bytes: u64 => DisplayValue,
		},
	},
	ImportRejectLimitExceeded, "FG-54F08" => {
		kind: Error {
			classification: ErrorClassification::ClientError,
			category: ErrorCategory::Transaction,
			severity: Severity::Error,
		},
		condition: Condition::ProgramLimitExceeded,
		domain: Domain::Transaction,
		subcondition: "import reject limit exceeded",
		message: "Import skipped more than the configured limit of {limit} items.",
		params: {
			limit: u64 => DisplayValue,
		},
	},
	UnexpectedExternalFailure, "FG-50F00" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GeneralProcessingException,
		domain: Domain::External,
		subcondition: "unexpected external component failure",
		message: "Component {component} failed while performing {operation}.",
		params: {
			component: String => Identifier,
			operation: String => StringLiteral,
		},
	},
	ArrowPropertyBatchFailure, "FG-50F01" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GeneralProcessingException,
		domain: Domain::Value,
		subcondition: "Arrow property batch processing failed",
		message: "Apache Arrow failed while attempting to {operation} a property batch.",
		params: {
			operation: String => StringLiteral,
		},
	},
	DataFusionPropertyBatchFailure, "FG-50F02" => {
		kind: Error {
			classification: ErrorClassification::DatabaseError,
			category: ErrorCategory::Runtime,
			severity: Severity::Error,
		},
		condition: Condition::GeneralProcessingException,
		domain: Domain::Value,
		subcondition: "DataFusion property batch processing failed",
		message: "Apache DataFusion failed while attempting to {operation} a property batch.",
		params: {
			operation: String => StringLiteral,
		},
	}
);

impl Display for Status {
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		self.fmt_message(f)
	}
}
