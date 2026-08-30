//! Keywords recognized by the current parser profile.

pub(in crate::syn) const AFTER: &str = "AFTER";
pub(in crate::syn) const AND: &str = "AND";
pub(in crate::syn) const AS: &str = "AS";
pub(in crate::syn) const AT: &str = "AT";
pub(in crate::syn) const ATOM: &str = "ATOM";
pub(in crate::syn) const BY: &str = "BY";
pub(in crate::syn) const DISTINCT: &str = "DISTINCT";
pub(in crate::syn) const FALSE: &str = "FALSE";
pub(in crate::syn) const FROM: &str = "FROM";
pub(in crate::syn) const GRAPH: &str = "GRAPH";
pub(in crate::syn) const IN: &str = "IN";
pub(in crate::syn) const IS: &str = "IS";
pub(in crate::syn) const LET: &str = "LET";
pub(in crate::syn) const LIMIT: &str = "LIMIT";
pub(in crate::syn) const MATCH: &str = "MATCH";
pub(in crate::syn) const NONE: &str = "NONE";
pub(in crate::syn) const NOT: &str = "NOT";
pub(in crate::syn) const NULL: &str = "NULL";
pub(in crate::syn) const ONLY: &str = "ONLY";
pub(in crate::syn) const OR: &str = "OR";
pub(in crate::syn) const RETURN: &str = "RETURN";
pub(in crate::syn) const SELECT: &str = "SELECT";
pub(in crate::syn) const START: &str = "START";
pub(in crate::syn) const TRUE: &str = "TRUE";
pub(in crate::syn) const VALUE: &str = "VALUE";
pub(in crate::syn) const WHERE: &str = "WHERE";

/// Statement starts recognized only to return the explicit unsupported status.
pub(in crate::syn) const UNSUPPORTED_STATEMENTS: &[&str] = &[
	"IF", "CASE", "FOR", "WHILE", "BREAK", "CONTINUE", "THROW", "TRY", "BEGIN", "COMMIT", "CANCEL",
	"EXPLAIN", "CREATE", "UPDATE", "UPSERT", "DELETE", "RELATE", "OPTIONAL", "TRAVERSE", "BUILD",
	"DEFINE", "REMOVE", "INFO", "SHOW", "KILL", "REBUILD", "ALTER", "COMPACT", "LIVE",
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
