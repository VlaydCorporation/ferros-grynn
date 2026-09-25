//! Keywords recognized by the current parser profile.

pub(in crate::syn) const AFTER: &str = "AFTER";
pub(in crate::syn) const ALL: &str = "ALL";
pub(in crate::syn) const ATTACH: &str = "ATTACH";
pub(in crate::syn) const AND: &str = "AND";
pub(in crate::syn) const ALTER: &str = "ALTER";
pub(in crate::syn) const AS: &str = "AS";
pub(in crate::syn) const ASC: &str = "ASC";
pub(in crate::syn) const ASSERT: &str = "ASSERT";
pub(in crate::syn) const AT: &str = "AT";
pub(in crate::syn) const ATOM: &str = "ATOM";
pub(in crate::syn) const BEFORE: &str = "BEFORE";
pub(in crate::syn) const BREAK: &str = "BREAK";
pub(in crate::syn) const BASE: &str = "BASE";
pub(in crate::syn) const BEGIN: &str = "BEGIN";
pub(in crate::syn) const BEST_EFFORT: &str = "BEST_EFFORT";
pub(in crate::syn) const BY: &str = "BY";
pub(in crate::syn) const CATCH: &str = "CATCH";
pub(in crate::syn) const CASE: &str = "CASE";
pub(in crate::syn) const CHANGEFEED: &str = "CHANGEFEED";
pub(in crate::syn) const CLASS: &str = "CLASS";
pub(in crate::syn) const CASCADE: &str = "CASCADE";
pub(in crate::syn) const COLUMNS: &str = "COLUMNS";
pub(in crate::syn) const COMMENT: &str = "COMMENT";
pub(in crate::syn) const COMMIT: &str = "COMMIT";
pub(in crate::syn) const COMMITTED: &str = "COMMITTED";
pub(in crate::syn) const CONTENT: &str = "CONTENT";
pub(in crate::syn) const CONTINUE: &str = "CONTINUE";
pub(in crate::syn) const CONTAINS: &str = "CONTAINS";
pub(in crate::syn) const CONTAINSALL: &str = "CONTAINSALL";
pub(in crate::syn) const CONTAINSANY: &str = "CONTAINSANY";
pub(in crate::syn) const CONTAINSNOT: &str = "CONTAINSNOT";
pub(in crate::syn) const COUNT: &str = "COUNT";
pub(in crate::syn) const CROSS: &str = "CROSS";
pub(in crate::syn) const CREATE: &str = "CREATE";
pub(in crate::syn) const CANCEL: &str = "CANCEL";
pub(in crate::syn) const DB: &str = "DB";
pub(in crate::syn) const DEFINE: &str = "DEFINE";
pub(in crate::syn) const DISKANN: &str = "DISKANN";
pub(in crate::syn) const DISTINCT: &str = "DISTINCT";
pub(in crate::syn) const DELETE: &str = "DELETE";
pub(in crate::syn) const DETACH: &str = "DETACH";
pub(in crate::syn) const DESC: &str = "DESC";
pub(in crate::syn) const DEFERRED: &str = "DEFERRED";
pub(in crate::syn) const DIFF: &str = "DIFF";
pub(in crate::syn) const DETERMINISTIC: &str = "DETERMINISTIC";
pub(in crate::syn) const DROP: &str = "DROP";
pub(in crate::syn) const EDGE: &str = "EDGE";
pub(in crate::syn) const ELSE: &str = "ELSE";
pub(in crate::syn) const EFFECTIVE: &str = "EFFECTIVE";
pub(in crate::syn) const END: &str = "END";
pub(in crate::syn) const EVENTS: &str = "EVENTS";
pub(in crate::syn) const EXECUTION: &str = "EXECUTION";
pub(in crate::syn) const EXISTS: &str = "EXISTS";
pub(in crate::syn) const EXPECT: &str = "EXPECT";
pub(in crate::syn) const FALSE: &str = "FALSE";
pub(in crate::syn) const FETCH: &str = "FETCH";
pub(in crate::syn) const FIELD: &str = "FIELD";
pub(in crate::syn) const FAST: &str = "FAST";
pub(in crate::syn) const FIELDS: &str = "FIELDS";
pub(in crate::syn) const FIRST: &str = "FIRST";
pub(in crate::syn) const FOR: &str = "FOR";
pub(in crate::syn) const FULLTEXT: &str = "FULLTEXT";
pub(in crate::syn) const FROM: &str = "FROM";
pub(in crate::syn) const GEOMETRY: &str = "GEOMETRY";
pub(in crate::syn) const GRAPH: &str = "GRAPH";
pub(in crate::syn) const GRAPHS: &str = "GRAPHS";
pub(in crate::syn) const GROUP: &str = "GROUP";
pub(in crate::syn) const HNSW: &str = "HNSW";
pub(in crate::syn) const IF: &str = "IF";
pub(in crate::syn) const IN: &str = "IN";
pub(in crate::syn) const INVARIANTS: &str = "INVARIANTS";
pub(in crate::syn) const ALLIN: &str = "ALLIN";
pub(in crate::syn) const ANYIN: &str = "ANYIN";
pub(in crate::syn) const NONEIN: &str = "NONEIN";
pub(in crate::syn) const INDEX: &str = "INDEX";
pub(in crate::syn) const INFO: &str = "INFO";
pub(in crate::syn) const IMPORT: &str = "IMPORT";
pub(in crate::syn) const IMMEDIATE: &str = "IMMEDIATE";
pub(in crate::syn) const INDEXES: &str = "INDEXES";
pub(in crate::syn) const INNER: &str = "INNER";
pub(in crate::syn) const ISOLATION: &str = "ISOLATION";
pub(in crate::syn) const IS: &str = "IS";
pub(in crate::syn) const JOIN: &str = "JOIN";
pub(in crate::syn) const LET: &str = "LET";
pub(in crate::syn) const LEVEL: &str = "LEVEL";
pub(in crate::syn) const LABEL: &str = "LABEL";
pub(in crate::syn) const LAST: &str = "LAST";
pub(in crate::syn) const LEFT: &str = "LEFT";
pub(in crate::syn) const LIMIT: &str = "LIMIT";
pub(in crate::syn) const MATCH: &str = "MATCH";
pub(in crate::syn) const MODE: &str = "MODE";
pub(in crate::syn) const MAINTAINED: &str = "MAINTAINED";
pub(in crate::syn) const MERGE: &str = "MERGE";
pub(in crate::syn) const NONE: &str = "NONE";
pub(in crate::syn) const NODE: &str = "NODE";
pub(in crate::syn) const NOT: &str = "NOT";
pub(in crate::syn) const NULL: &str = "NULL";
pub(in crate::syn) const NULLS: &str = "NULLS";
pub(in crate::syn) const ON: &str = "ON";
pub(in crate::syn) const ONLY: &str = "ONLY";
pub(in crate::syn) const OMIT: &str = "OMIT";
pub(in crate::syn) const OFF: &str = "OFF";
pub(in crate::syn) const OR: &str = "OR";
pub(in crate::syn) const ORDER: &str = "ORDER";
pub(in crate::syn) const OVERWRITE: &str = "OVERWRITE";
pub(in crate::syn) const PATCH: &str = "PATCH";
pub(in crate::syn) const PREPARE: &str = "PREPARE";
pub(in crate::syn) const READ: &str = "READ";
pub(in crate::syn) const REPORT: &str = "REPORT";
pub(in crate::syn) const REMOVE: &str = "REMOVE";
pub(in crate::syn) const REJECT: &str = "REJECT";
pub(in crate::syn) const RELATE: &str = "RELATE";
pub(in crate::syn) const REPLACE: &str = "REPLACE";
pub(in crate::syn) const RESET: &str = "RESET";
pub(in crate::syn) const RETURN: &str = "RETURN";
pub(in crate::syn) const ROOT: &str = "ROOT";
pub(in crate::syn) const ROOTS: &str = "ROOTS";
pub(in crate::syn) const SCHEMAFULL: &str = "SCHEMAFULL";
pub(in crate::syn) const SCHEMALESS: &str = "SCHEMALESS";
pub(in crate::syn) const SCOPE: &str = "SCOPE";
pub(in crate::syn) const SELECT: &str = "SELECT";
pub(in crate::syn) const SERIALIZABLE: &str = "SERIALIZABLE";
pub(in crate::syn) const SEGMENT: &str = "SEGMENT";
pub(in crate::syn) const SET: &str = "SET";
pub(in crate::syn) const SIZE: &str = "SIZE";
pub(in crate::syn) const SNAPSHOT: &str = "SNAPSHOT";
pub(in crate::syn) const SKIP: &str = "SKIP";
pub(in crate::syn) const SPLIT: &str = "SPLIT";
pub(in crate::syn) const START: &str = "START";
pub(in crate::syn) const TEMPFILES: &str = "TEMPFILES";
pub(in crate::syn) const THEN: &str = "THEN";
pub(in crate::syn) const THROW: &str = "THROW";
pub(in crate::syn) const TRUE: &str = "TRUE";
pub(in crate::syn) const TRY: &str = "TRY";
pub(in crate::syn) const TO: &str = "TO";
pub(in crate::syn) const TIMEOUT: &str = "TIMEOUT";
pub(in crate::syn) const TYPE: &str = "TYPE";
pub(in crate::syn) const UNIQUE: &str = "UNIQUE";
pub(in crate::syn) const UNSET: &str = "UNSET";
pub(in crate::syn) const UPDATE: &str = "UPDATE";
pub(in crate::syn) const UPSERT: &str = "UPSERT";
pub(in crate::syn) const VALIDATE: &str = "VALIDATE";
pub(in crate::syn) const VALIDATION: &str = "VALIDATION";
pub(in crate::syn) const VALUE: &str = "VALUE";
pub(in crate::syn) const VIOLATION: &str = "VIOLATION";
pub(in crate::syn) const WITH: &str = "WITH";
pub(in crate::syn) const WHERE: &str = "WHERE";
pub(in crate::syn) const WEIGHTED: &str = "WEIGHTED";
pub(in crate::syn) const WHEN: &str = "WHEN";
pub(in crate::syn) const WHILE: &str = "WHILE";

/// Statement starts recognized only to return the explicit unsupported status.
pub(in crate::syn) const UNSUPPORTED_STATEMENTS: &[&str] =
	&["EXPLAIN", "OPTIONAL", "TRAVERSE", "BUILD", "SHOW", "KILL", "REBUILD", "COMPACT", "LIVE"];

/// MATCH modifiers recognized only to return the explicit unsupported status.
pub(in crate::syn) const UNSUPPORTED_MATCH_MODIFIERS: &[&str] = &[
	"OPTIONAL",
	"DISTINCT",
	"REPEATABLE",
	"DIFFERENT",
	"HYPER",
	"META",
	"METAVERTEX",
	"METAEDGE",
	"CONSTRAINTS",
	"WITHIN",
	"CROSS_LEVEL",
	"WALK",
	"TRAIL",
	"SIMPLE",
	"ACYCLIC",
	"SHORTEST",
	"WEIGHTED",
];

/// SELECT clauses recognized only to return the explicit unsupported status.
pub(in crate::syn) const UNSUPPORTED_SELECT_CLAUSES: &[&str] =
	&["WITH", "SPLIT", "GROUP", "ORDER", "FETCH", "OMIT", "TEMPFILES"];
