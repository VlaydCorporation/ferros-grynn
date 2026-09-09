//! Keywords recognized by the current parser profile.

pub(in crate::syn) const AFTER: &str = "AFTER";
pub(in crate::syn) const AND: &str = "AND";
pub(in crate::syn) const ALTER: &str = "ALTER";
pub(in crate::syn) const AS: &str = "AS";
pub(in crate::syn) const AT: &str = "AT";
pub(in crate::syn) const ATOM: &str = "ATOM";
pub(in crate::syn) const BEFORE: &str = "BEFORE";
pub(in crate::syn) const BY: &str = "BY";
pub(in crate::syn) const CHANGEFEED: &str = "CHANGEFEED";
pub(in crate::syn) const COLUMNS: &str = "COLUMNS";
pub(in crate::syn) const COMMENT: &str = "COMMENT";
pub(in crate::syn) const CONTENT: &str = "CONTENT";
pub(in crate::syn) const COUNT: &str = "COUNT";
pub(in crate::syn) const CREATE: &str = "CREATE";
pub(in crate::syn) const DB: &str = "DB";
pub(in crate::syn) const DEFINE: &str = "DEFINE";
pub(in crate::syn) const DISKANN: &str = "DISKANN";
pub(in crate::syn) const DISTINCT: &str = "DISTINCT";
pub(in crate::syn) const DELETE: &str = "DELETE";
pub(in crate::syn) const DIFF: &str = "DIFF";
pub(in crate::syn) const DROP: &str = "DROP";
pub(in crate::syn) const EXISTS: &str = "EXISTS";
pub(in crate::syn) const EXPECT: &str = "EXPECT";
pub(in crate::syn) const FALSE: &str = "FALSE";
pub(in crate::syn) const FIELD: &str = "FIELD";
pub(in crate::syn) const FIELDS: &str = "FIELDS";
pub(in crate::syn) const FOR: &str = "FOR";
pub(in crate::syn) const FULLTEXT: &str = "FULLTEXT";
pub(in crate::syn) const FROM: &str = "FROM";
pub(in crate::syn) const GEOMETRY: &str = "GEOMETRY";
pub(in crate::syn) const GRAPH: &str = "GRAPH";
pub(in crate::syn) const HNSW: &str = "HNSW";
pub(in crate::syn) const IF: &str = "IF";
pub(in crate::syn) const IN: &str = "IN";
pub(in crate::syn) const INDEX: &str = "INDEX";
pub(in crate::syn) const INFO: &str = "INFO";
pub(in crate::syn) const IS: &str = "IS";
pub(in crate::syn) const LET: &str = "LET";
pub(in crate::syn) const LIMIT: &str = "LIMIT";
pub(in crate::syn) const MATCH: &str = "MATCH";
pub(in crate::syn) const MERGE: &str = "MERGE";
pub(in crate::syn) const NONE: &str = "NONE";
pub(in crate::syn) const NOT: &str = "NOT";
pub(in crate::syn) const NULL: &str = "NULL";
pub(in crate::syn) const ON: &str = "ON";
pub(in crate::syn) const ONLY: &str = "ONLY";
pub(in crate::syn) const OR: &str = "OR";
pub(in crate::syn) const OVERWRITE: &str = "OVERWRITE";
pub(in crate::syn) const PATCH: &str = "PATCH";
pub(in crate::syn) const PREPARE: &str = "PREPARE";
pub(in crate::syn) const REMOVE: &str = "REMOVE";
pub(in crate::syn) const RELATE: &str = "RELATE";
pub(in crate::syn) const REPLACE: &str = "REPLACE";
pub(in crate::syn) const RETURN: &str = "RETURN";
pub(in crate::syn) const ROOT: &str = "ROOT";
pub(in crate::syn) const ROOTS: &str = "ROOTS";
pub(in crate::syn) const SCHEMAFULL: &str = "SCHEMAFULL";
pub(in crate::syn) const SCHEMALESS: &str = "SCHEMALESS";
pub(in crate::syn) const SELECT: &str = "SELECT";
pub(in crate::syn) const SET: &str = "SET";
pub(in crate::syn) const START: &str = "START";
pub(in crate::syn) const TRUE: &str = "TRUE";
pub(in crate::syn) const TABLE: &str = "TABLE";
pub(in crate::syn) const TYPE: &str = "TYPE";
pub(in crate::syn) const UNIQUE: &str = "UNIQUE";
pub(in crate::syn) const UNSET: &str = "UNSET";
pub(in crate::syn) const UPDATE: &str = "UPDATE";
pub(in crate::syn) const UPSERT: &str = "UPSERT";
pub(in crate::syn) const VALIDATE: &str = "VALIDATE";
pub(in crate::syn) const VALUE: &str = "VALUE";
pub(in crate::syn) const WHERE: &str = "WHERE";

/// Statement starts recognized only to return the explicit unsupported status.
pub(in crate::syn) const UNSUPPORTED_STATEMENTS: &[&str] = &[
	"IF", "CASE", "FOR", "WHILE", "BREAK", "CONTINUE", "THROW", "TRY", "BEGIN", "COMMIT", "CANCEL",
	"EXPLAIN", "OPTIONAL", "TRAVERSE", "BUILD", "SHOW", "KILL", "REBUILD", "COMPACT", "LIVE",
];

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
	&["WITH", "SPLIT", "GROUP", "ORDER", "FETCH", "OMIT", "TIMEOUT", "TEMPFILES"];
