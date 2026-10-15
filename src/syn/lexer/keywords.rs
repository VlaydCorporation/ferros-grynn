//! Keywords recognized by the current parser profile.

/// Whole-delivery confirmation, never an arbitrary numeric watermark.
pub(in crate::syn) const ACK: &str = "ACK";
/// Serialized origin/query/version bound LIVE token operand.
pub(in crate::syn) const CURSOR: &str = "CURSOR";

/// GrynnQL keyword `AFTER`.
pub(in crate::syn) const AFTER: &str = "AFTER";
/// GrynnQL keyword `ALPHA`.
pub(in crate::syn) const ALPHA: &str = "ALPHA";
/// GrynnQL keyword `ALL`.
pub(in crate::syn) const ALL: &str = "ALL";
/// GrynnQL keyword `ATTACH`.
pub(in crate::syn) const ATTACH: &str = "ATTACH";
/// GrynnQL keyword `AND`.
pub(in crate::syn) const AND: &str = "AND";
/// GrynnQL keyword `ALTER`.
pub(in crate::syn) const ALTER: &str = "ALTER";
/// GrynnQL keyword `ANALYZE`.
pub(in crate::syn) const ANALYZE: &str = "ANALYZE";
/// GrynnQL keyword `ANALYZER`.
pub(in crate::syn) const ANALYZER: &str = "ANALYZER";
/// GrynnQL keyword `AS`.
pub(in crate::syn) const AS: &str = "AS";
/// GrynnQL keyword `ASC`.
pub(in crate::syn) const ASC: &str = "ASC";
/// GrynnQL keyword `ASSERT`.
pub(in crate::syn) const ASSERT: &str = "ASSERT";
/// GrynnQL keyword `AT`.
pub(in crate::syn) const AT: &str = "AT";
/// GrynnQL keyword `ATOM`.
pub(in crate::syn) const ATOM: &str = "ATOM";
/// GrynnQL keyword `BEFORE`.
pub(in crate::syn) const BEFORE: &str = "BEFORE";
/// GrynnQL keyword `BM25`.
pub(in crate::syn) const BM25: &str = "BM25";
/// GrynnQL keyword `BREAK`.
pub(in crate::syn) const BREAK: &str = "BREAK";
/// GrynnQL keyword `BIPARTITE`.
pub(in crate::syn) const BIPARTITE: &str = "BIPARTITE";
/// GrynnQL keyword `BUILD`.
pub(in crate::syn) const BUILD: &str = "BUILD";
/// GrynnQL keyword `BASE`.
pub(in crate::syn) const BASE: &str = "BASE";
/// GrynnQL keyword `BEGIN`.
pub(in crate::syn) const BEGIN: &str = "BEGIN";
/// GrynnQL keyword `BEST_EFFORT`.
pub(in crate::syn) const BEST_EFFORT: &str = "BEST_EFFORT";
/// GrynnQL keyword `BY`.
pub(in crate::syn) const BY: &str = "BY";
/// GrynnQL keyword `CATCH`.
pub(in crate::syn) const CATCH: &str = "CATCH";
/// GrynnQL keyword `CASE`.
pub(in crate::syn) const CASE: &str = "CASE";
/// GrynnQL keyword `CHANGEFEED`.
pub(in crate::syn) const CHANGEFEED: &str = "CHANGEFEED";
/// Whole-commit history statement payload.
pub(in crate::syn) const CHANGES: &str = "CHANGES";
/// History-read statement introducer.
pub(in crate::syn) const SHOW: &str = "SHOW";
/// Inclusive time or exclusive stamp lower bound.
pub(in crate::syn) const SINCE: &str = "SINCE";
/// Complete before-image capture option.
pub(in crate::syn) const ORIGINAL: &str = "ORIGINAL";
/// Introduces the complete-original capture flag.
pub(in crate::syn) const INCLUDE: &str = "INCLUDE";
/// GrynnQL keyword `CLASS`.
pub(in crate::syn) const CLASS: &str = "CLASS";
/// GrynnQL keyword `CASCADE`.
pub(in crate::syn) const CASCADE: &str = "CASCADE";
/// GrynnQL keyword `COLUMNS`.
pub(in crate::syn) const COLUMNS: &str = "COLUMNS";
/// GrynnQL keyword `COMMENT`.
pub(in crate::syn) const COMMENT: &str = "COMMENT";
/// GrynnQL keyword `COMPLETE`.
pub(in crate::syn) const COMPLETE: &str = "COMPLETE";
/// GrynnQL keyword `COMMIT`.
pub(in crate::syn) const COMMIT: &str = "COMMIT";
/// GrynnQL keyword `COMMITTED`.
pub(in crate::syn) const COMMITTED: &str = "COMMITTED";
/// GrynnQL keyword `COMPACT`.
pub(in crate::syn) const COMPACT: &str = "COMPACT";
/// GrynnQL keyword `CONCURRENTLY`.
pub(in crate::syn) const CONCURRENTLY: &str = "CONCURRENTLY";
/// GrynnQL keyword `CONSTRAINTS`.
pub(in crate::syn) const CONSTRAINTS: &str = "CONSTRAINTS";
/// GrynnQL keyword `CONTENT`.
pub(in crate::syn) const CONTENT: &str = "CONTENT";
/// GrynnQL keyword `CONTINUE`.
pub(in crate::syn) const CONTINUE: &str = "CONTINUE";
/// GrynnQL keyword `CONTAINS`.
pub(in crate::syn) const CONTAINS: &str = "CONTAINS";
/// GrynnQL keyword `CONTAINSALL`.
pub(in crate::syn) const CONTAINSALL: &str = "CONTAINSALL";
/// GrynnQL keyword `CONTAINSANY`.
pub(in crate::syn) const CONTAINSANY: &str = "CONTAINSANY";
/// GrynnQL keyword `CONTAINSNOT`.
pub(in crate::syn) const CONTAINSNOT: &str = "CONTAINSNOT";
/// GrynnQL keyword `INSIDE`.
pub(in crate::syn) const INSIDE: &str = "INSIDE";
/// GrynnQL keyword `NOTINSIDE`.
pub(in crate::syn) const NOTINSIDE: &str = "NOTINSIDE";
/// GrynnQL keyword `ALLINSIDE`.
pub(in crate::syn) const ALLINSIDE: &str = "ALLINSIDE";
/// GrynnQL keyword `ANYINSIDE`.
pub(in crate::syn) const ANYINSIDE: &str = "ANYINSIDE";
/// GrynnQL keyword `NONEINSIDE`.
pub(in crate::syn) const NONEINSIDE: &str = "NONEINSIDE";
/// GrynnQL keyword `OUTSIDE`.
pub(in crate::syn) const OUTSIDE: &str = "OUTSIDE";
/// GrynnQL keyword `INTERSECTS`.
pub(in crate::syn) const INTERSECTS: &str = "INTERSECTS";
/// GrynnQL keyword `COSINE`.
pub(in crate::syn) const COSINE: &str = "COSINE";
/// GrynnQL keyword `COSINE_NORMALIZED`.
pub(in crate::syn) const COSINE_NORMALIZED: &str = "COSINE_NORMALIZED";
/// GrynnQL keyword `COUNT`.
pub(in crate::syn) const COUNT: &str = "COUNT";
/// GrynnQL keyword `CROSS`.
pub(in crate::syn) const CROSS: &str = "CROSS";
/// GrynnQL keyword `CREATE`.
pub(in crate::syn) const CREATE: &str = "CREATE";
/// GrynnQL keyword `CANCEL`.
pub(in crate::syn) const CANCEL: &str = "CANCEL";
/// GrynnQL keyword `DB`.
pub(in crate::syn) const DB: &str = "DB";
/// GrynnQL keyword `DATABASE`.
pub(in crate::syn) const DATABASE: &str = "DATABASE";
/// GrynnQL keyword `DAG`.
pub(in crate::syn) const DAG: &str = "DAG";
/// GrynnQL keyword `DEFINE`.
pub(in crate::syn) const DEFINE: &str = "DEFINE";
/// GrynnQL keyword `DISKANN`.
pub(in crate::syn) const DISKANN: &str = "DISKANN";
/// GrynnQL keyword `DISTINCT`.
pub(in crate::syn) const DISTINCT: &str = "DISTINCT";
/// GrynnQL keyword `DELETE`.
pub(in crate::syn) const DELETE: &str = "DELETE";
/// GrynnQL keyword `DEGREE`.
pub(in crate::syn) const DEGREE: &str = "DEGREE";
/// GrynnQL keyword `DEFER`.
pub(in crate::syn) const DEFER: &str = "DEFER";
/// GrynnQL keyword `DETACH`.
pub(in crate::syn) const DETACH: &str = "DETACH";
/// GrynnQL keyword `DESC`.
pub(in crate::syn) const DESC: &str = "DESC";
/// GrynnQL keyword `DIFFERENT`.
pub(in crate::syn) const DIFFERENT: &str = "DIFFERENT";
/// GrynnQL keyword `DIFF`.
pub(in crate::syn) const DIFF: &str = "DIFF";
/// GrynnQL keyword `DIMENSION`.
pub(in crate::syn) const DIMENSION: &str = "DIMENSION";
/// GrynnQL keyword `DIST`.
pub(in crate::syn) const DIST: &str = "DIST";
/// GrynnQL keyword `DETERMINISTIC`.
pub(in crate::syn) const DETERMINISTIC: &str = "DETERMINISTIC";
/// GrynnQL keyword `DROP`.
pub(in crate::syn) const DROP: &str = "DROP";
/// GrynnQL keyword `EDGE`.
pub(in crate::syn) const EDGE: &str = "EDGE";
/// GrynnQL keyword `EDGES`.
pub(in crate::syn) const EDGES: &str = "EDGES";
/// GrynnQL keyword `ELSE`.
pub(in crate::syn) const ELSE: &str = "ELSE";
/// GrynnQL keyword `ELEMENTS`.
pub(in crate::syn) const ELEMENTS: &str = "ELEMENTS";
/// GrynnQL keyword `EFFECTIVE`.
pub(in crate::syn) const EFFECTIVE: &str = "EFFECTIVE";
/// GrynnQL keyword `END`.
pub(in crate::syn) const END: &str = "END";
/// GrynnQL keyword `EFC`.
pub(in crate::syn) const EFC: &str = "EFC";
/// GrynnQL keyword `EF`.
pub(in crate::syn) const EF: &str = "EF";
/// GrynnQL keyword `EUCLIDEAN`.
pub(in crate::syn) const EUCLIDEAN: &str = "EUCLIDEAN";
/// GrynnQL keyword `EVENTS`.
pub(in crate::syn) const EVENTS: &str = "EVENTS";
/// GrynnQL keyword `EXECUTION`.
pub(in crate::syn) const EXECUTION: &str = "EXECUTION";
/// GrynnQL keyword `EXPLAIN`.
pub(in crate::syn) const EXPLAIN: &str = "EXPLAIN";
/// GrynnQL keyword `EXISTS`.
pub(in crate::syn) const EXISTS: &str = "EXISTS";
/// GrynnQL keyword `EXPECT`.
pub(in crate::syn) const EXPECT: &str = "EXPECT";
/// GrynnQL keyword `FALSE`.
pub(in crate::syn) const FALSE: &str = "FALSE";
/// GrynnQL keyword `F16`.
pub(in crate::syn) const F16: &str = "F16";
/// GrynnQL keyword `F32`.
pub(in crate::syn) const F32: &str = "F32";
/// GrynnQL keyword `F64`.
pub(in crate::syn) const F64: &str = "F64";
/// GrynnQL keyword `FETCH`.
pub(in crate::syn) const FETCH: &str = "FETCH";
/// GrynnQL keyword `FIELD`.
pub(in crate::syn) const FIELD: &str = "FIELD";
/// GrynnQL keyword `FILTERS`.
pub(in crate::syn) const FILTERS: &str = "FILTERS";
/// GrynnQL keyword `FAST`.
pub(in crate::syn) const FAST: &str = "FAST";
/// GrynnQL keyword `FIELDS`.
pub(in crate::syn) const FIELDS: &str = "FIELDS";
/// GrynnQL keyword `FIRST`.
pub(in crate::syn) const FIRST: &str = "FIRST";
/// GrynnQL keyword `FOR`.
pub(in crate::syn) const FOR: &str = "FOR";
/// GrynnQL keyword `FOREST`.
pub(in crate::syn) const FOREST: &str = "FOREST";
/// GrynnQL keyword `FORMAT`.
pub(in crate::syn) const FORMAT: &str = "FORMAT";
/// GrynnQL keyword `FULL`.
pub(in crate::syn) const FULL: &str = "FULL";
/// GrynnQL keyword `FULLTEXT`.
pub(in crate::syn) const FULLTEXT: &str = "FULLTEXT";
/// GrynnQL keyword `FUNCTION`.
pub(in crate::syn) const FUNCTION: &str = "FUNCTION";
/// GrynnQL keyword `FROM`.
pub(in crate::syn) const FROM: &str = "FROM";
/// GrynnQL keyword `GEOMETRY`.
pub(in crate::syn) const GEOMETRY: &str = "GEOMETRY";
/// GrynnQL keyword `GRAPH`.
pub(in crate::syn) const GRAPH: &str = "GRAPH";
/// GrynnQL keyword `GRAPHS`.
pub(in crate::syn) const GRAPHS: &str = "GRAPHS";
/// GrynnQL keyword `GROUP`.
pub(in crate::syn) const GROUP: &str = "GROUP";
/// GrynnQL keyword `HEAD`.
pub(in crate::syn) const HEAD: &str = "HEAD";
/// GrynnQL keyword `HNSW`.
pub(in crate::syn) const HNSW: &str = "HNSW";
/// GrynnQL keyword `HIGHLIGHTS`.
pub(in crate::syn) const HIGHLIGHTS: &str = "HIGHLIGHTS";
/// GrynnQL keyword `HASHED_VECTOR`.
pub(in crate::syn) const HASHED_VECTOR: &str = "HASHED_VECTOR";
/// GrynnQL keyword `HYPER`.
pub(in crate::syn) const HYPER: &str = "HYPER";
/// GrynnQL keyword `HYPEREDGES`.
pub(in crate::syn) const HYPEREDGES: &str = "HYPEREDGES";
/// GrynnQL keyword `IF`.
pub(in crate::syn) const IF: &str = "IF";
/// GrynnQL keyword `I8`.
pub(in crate::syn) const I8: &str = "I8";
/// GrynnQL keyword `I16`.
pub(in crate::syn) const I16: &str = "I16";
/// GrynnQL keyword `I32`.
pub(in crate::syn) const I32: &str = "I32";
/// GrynnQL keyword `I64`.
pub(in crate::syn) const I64: &str = "I64";
/// GrynnQL keyword `IN`.
pub(in crate::syn) const IN: &str = "IN";
/// GrynnQL keyword `INVARIANTS`.
pub(in crate::syn) const INVARIANTS: &str = "INVARIANTS";
/// GrynnQL keyword `ALLIN`.
pub(in crate::syn) const ALLIN: &str = "ALLIN";
/// GrynnQL keyword `ANYIN`.
pub(in crate::syn) const ANYIN: &str = "ANYIN";
/// GrynnQL keyword `NONEIN`.
pub(in crate::syn) const NONEIN: &str = "NONEIN";
/// GrynnQL keyword `INDEX`.
pub(in crate::syn) const INDEX: &str = "INDEX";
/// GrynnQL keyword `INFO`.
pub(in crate::syn) const INFO: &str = "INFO";
/// GrynnQL keyword `IMPORT`.
pub(in crate::syn) const IMPORT: &str = "IMPORT";
/// GrynnQL keyword `IMPURE`.
pub(in crate::syn) const IMPURE: &str = "IMPURE";
/// GrynnQL keyword `INDEXES`.
pub(in crate::syn) const INDEXES: &str = "INDEXES";
/// GrynnQL keyword `INNER`.
pub(in crate::syn) const INNER: &str = "INNER";
/// GrynnQL keyword `INNER_PRODUCT`.
pub(in crate::syn) const INNER_PRODUCT: &str = "INNER_PRODUCT";
/// GrynnQL keyword `ISOLATION`.
pub(in crate::syn) const ISOLATION: &str = "ISOLATION";
/// GrynnQL keyword `IS`.
pub(in crate::syn) const IS: &str = "IS";
/// GrynnQL keyword `JOIN`.
pub(in crate::syn) const JOIN: &str = "JOIN";
/// GrynnQL keyword `JOB`.
pub(in crate::syn) const JOB: &str = "JOB";
/// GrynnQL keyword `JOBS`.
pub(in crate::syn) const JOBS: &str = "JOBS";
/// GrynnQL keyword `JSON`.
pub(in crate::syn) const JSON: &str = "JSON";
/// GrynnQL keyword `KEEP`.
pub(in crate::syn) const KEEP: &str = "KEEP";
/// GrynnQL keyword `KILL`.
pub(in crate::syn) const KILL: &str = "KILL";
/// GrynnQL keyword `LET`.
pub(in crate::syn) const LET: &str = "LET";
/// GrynnQL keyword `LEVEL`.
pub(in crate::syn) const LEVEL: &str = "LEVEL";
/// GrynnQL keyword `LEVELPATH`.
pub(in crate::syn) const LEVELPATH: &str = "LEVELPATH";
/// GrynnQL keyword `L_BUILD`.
pub(in crate::syn) const L_BUILD: &str = "L_BUILD";
/// GrynnQL keyword `LABEL`.
pub(in crate::syn) const LABEL: &str = "LABEL";
/// GrynnQL keyword `LAST`.
pub(in crate::syn) const LAST: &str = "LAST";
/// GrynnQL keyword `LEFT`.
pub(in crate::syn) const LEFT: &str = "LEFT";
/// GrynnQL keyword `LIMIT`.
pub(in crate::syn) const LIMIT: &str = "LIMIT";
/// GrynnQL keyword `MATCH`.
pub(in crate::syn) const MATCH: &str = "MATCH";
/// GrynnQL keyword `MANHATTAN`.
pub(in crate::syn) const MANHATTAN: &str = "MANHATTAN";
/// GrynnQL keyword `M`.
pub(in crate::syn) const M: &str = "M";
/// GrynnQL keyword `MAXDEPTH`.
pub(in crate::syn) const MAXDEPTH: &str = "MAXDEPTH";
/// GrynnQL keyword `META`.
pub(in crate::syn) const META: &str = "META";
/// GrynnQL keyword `METAEDGE`.
pub(in crate::syn) const METAEDGE: &str = "METAEDGE";
/// GrynnQL keyword `METAEDGES`.
pub(in crate::syn) const METAEDGES: &str = "METAEDGES";
/// GrynnQL keyword `METAVERTEX`.
pub(in crate::syn) const METAVERTEX: &str = "METAVERTEX";
/// GrynnQL keyword `METAVERTICES`.
pub(in crate::syn) const METAVERTICES: &str = "METAVERTICES";
/// GrynnQL keyword `MODE`.
pub(in crate::syn) const MODE: &str = "MODE";
/// GrynnQL keyword `MINKOWSKI`.
pub(in crate::syn) const MINKOWSKI: &str = "MINKOWSKI";
/// GrynnQL keyword `MERGE`.
pub(in crate::syn) const MERGE: &str = "MERGE";
/// GrynnQL keyword `NONE`.
pub(in crate::syn) const NONE: &str = "NONE";
/// GrynnQL keyword `NOINDEX`.
pub(in crate::syn) const NOINDEX: &str = "NOINDEX";
/// GrynnQL keyword `NODE`.
pub(in crate::syn) const NODE: &str = "NODE";
/// GrynnQL keyword `NODES`.
pub(in crate::syn) const NODES: &str = "NODES";
/// GrynnQL keyword `NOT`.
pub(in crate::syn) const NOT: &str = "NOT";
/// GrynnQL keyword `NULL`.
pub(in crate::syn) const NULL: &str = "NULL";
/// GrynnQL keyword `NULLS`.
pub(in crate::syn) const NULLS: &str = "NULLS";
/// GrynnQL keyword `ON`.
pub(in crate::syn) const ON: &str = "ON";
/// GrynnQL keyword `ONLY`.
pub(in crate::syn) const ONLY: &str = "ONLY";
/// GrynnQL keyword `OPTIONAL`.
pub(in crate::syn) const OPTIONAL: &str = "OPTIONAL";
/// GrynnQL keyword `OMIT`.
pub(in crate::syn) const OMIT: &str = "OMIT";
/// GrynnQL keyword `OR`.
pub(in crate::syn) const OR: &str = "OR";
/// GrynnQL keyword `ORDER`.
pub(in crate::syn) const ORDER: &str = "ORDER";
/// GrynnQL keyword `OVERWRITE`.
pub(in crate::syn) const OVERWRITE: &str = "OVERWRITE";
/// GrynnQL keyword `PATCH`.
pub(in crate::syn) const PATCH: &str = "PATCH";
/// GrynnQL keyword `PARAM`.
pub(in crate::syn) const PARAM: &str = "PARAM";
/// GrynnQL keyword `PREPARE`.
pub(in crate::syn) const PREPARE: &str = "PREPARE";
/// GrynnQL keyword `PURE`.
pub(in crate::syn) const PURE: &str = "PURE";
/// GrynnQL keyword `READ`.
pub(in crate::syn) const READ: &str = "READ";
/// GrynnQL keyword `REPORT`.
pub(in crate::syn) const REPORT: &str = "REPORT";
/// GrynnQL keyword `REMOVE`.
pub(in crate::syn) const REMOVE: &str = "REMOVE";
/// GrynnQL keyword `REBUILD`.
pub(in crate::syn) const REBUILD: &str = "REBUILD";
/// GrynnQL keyword `REJECT`.
pub(in crate::syn) const REJECT: &str = "REJECT";
/// GrynnQL keyword `REPEATABLE`.
pub(in crate::syn) const REPEATABLE: &str = "REPEATABLE";
/// GrynnQL keyword `RELATE`.
pub(in crate::syn) const RELATE: &str = "RELATE";
/// GrynnQL keyword `REPLACE`.
pub(in crate::syn) const REPLACE: &str = "REPLACE";
/// GrynnQL keyword `RESET`.
pub(in crate::syn) const RESET: &str = "RESET";
/// GrynnQL keyword `RETURN`.
pub(in crate::syn) const RETURN: &str = "RETURN";
/// GrynnQL keyword `ROOT`.
pub(in crate::syn) const ROOT: &str = "ROOT";
/// GrynnQL keyword `ROOTED_TREE`.
pub(in crate::syn) const ROOTED_TREE: &str = "ROOTED_TREE";
/// GrynnQL keyword `ROOTS`.
pub(in crate::syn) const ROOTS: &str = "ROOTS";
/// GrynnQL keyword `SCHEMAFULL`.
pub(in crate::syn) const SCHEMAFULL: &str = "SCHEMAFULL";
/// GrynnQL keyword `SCHEMALESS`.
pub(in crate::syn) const SCHEMALESS: &str = "SCHEMALESS";
/// GrynnQL keyword `SCOPE`.
pub(in crate::syn) const SCOPE: &str = "SCOPE";
/// GrynnQL keyword `SELECT`.
pub(in crate::syn) const SELECT: &str = "SELECT";
/// GrynnQL keyword `SERIALIZABLE`.
pub(in crate::syn) const SERIALIZABLE: &str = "SERIALIZABLE";
/// GrynnQL keyword `SEGMENT`.
pub(in crate::syn) const SEGMENT: &str = "SEGMENT";
/// GrynnQL keyword `SET`.
pub(in crate::syn) const SET: &str = "SET";
/// GrynnQL keyword `SIZE`.
pub(in crate::syn) const SIZE: &str = "SIZE";
/// GrynnQL keyword `SHORTEST`.
pub(in crate::syn) const SHORTEST: &str = "SHORTEST";
/// GrynnQL keyword `SIMPLE`.
pub(in crate::syn) const SIMPLE: &str = "SIMPLE";
/// GrynnQL keyword `SLEEP`.
pub(in crate::syn) const SLEEP: &str = "SLEEP";
/// GrynnQL keyword `SNAPSHOT`.
pub(in crate::syn) const SNAPSHOT: &str = "SNAPSHOT";
/// GrynnQL keyword `SPLIT`.
pub(in crate::syn) const SPLIT: &str = "SPLIT";
/// GrynnQL keyword `START`.
pub(in crate::syn) const START: &str = "START";
/// GrynnQL keyword `STABLE`.
pub(in crate::syn) const STABLE: &str = "STABLE";
/// GrynnQL keyword `TEMPFILES`.
pub(in crate::syn) const TEMPFILES: &str = "TEMPFILES";
/// GrynnQL keyword `TAIL`.
pub(in crate::syn) const TAIL: &str = "TAIL";
/// GrynnQL keyword `THEN`.
pub(in crate::syn) const THEN: &str = "THEN";
/// GrynnQL keyword `TEXT`.
pub(in crate::syn) const TEXT: &str = "TEXT";
/// GrynnQL keyword `THROW`.
pub(in crate::syn) const THROW: &str = "THROW";
/// GrynnQL keyword `TOKENIZERS`.
pub(in crate::syn) const TOKENIZERS: &str = "TOKENIZERS";
/// GrynnQL keyword `TREE`.
pub(in crate::syn) const TREE: &str = "TREE";
/// GrynnQL keyword `TRUE`.
pub(in crate::syn) const TRUE: &str = "TRUE";
/// GrynnQL keyword `TRY`.
pub(in crate::syn) const TRY: &str = "TRY";
/// GrynnQL keyword `TO`.
pub(in crate::syn) const TO: &str = "TO";
/// GrynnQL keyword `TIMEOUT`.
pub(in crate::syn) const TIMEOUT: &str = "TIMEOUT";
/// GrynnQL keyword `TYPE`.
pub(in crate::syn) const TYPE: &str = "TYPE";
/// GrynnQL keyword `U8`.
pub(in crate::syn) const U8: &str = "U8";
/// GrynnQL keyword `UNIQUE`.
pub(in crate::syn) const UNIQUE: &str = "UNIQUE";
/// GrynnQL keyword `UNSET`.
pub(in crate::syn) const UNSET: &str = "UNSET";
/// GrynnQL keyword `UPDATE`.
pub(in crate::syn) const UPDATE: &str = "UPDATE";
/// GrynnQL keyword `UPSERT`.
pub(in crate::syn) const UPSERT: &str = "UPSERT";
/// GrynnQL keyword `VALIDATE`.
pub(in crate::syn) const VALIDATE: &str = "VALIDATE";
/// GrynnQL keyword `VALIDATION`.
pub(in crate::syn) const VALIDATION: &str = "VALIDATION";
/// GrynnQL keyword `VALUE`.
pub(in crate::syn) const VALUE: &str = "VALUE";
/// GrynnQL keyword `VIOLATION`.
pub(in crate::syn) const VIOLATION: &str = "VIOLATION";
/// GrynnQL keyword `WITH`.
pub(in crate::syn) const WITH: &str = "WITH";
/// GrynnQL keyword `WHERE`.
pub(in crate::syn) const WHERE: &str = "WHERE";
/// GrynnQL keyword `WEIGHTED`.
pub(in crate::syn) const WEIGHTED: &str = "WEIGHTED";
/// GrynnQL keyword `WITHIN`.
pub(in crate::syn) const WITHIN: &str = "WITHIN";
/// GrynnQL keyword `WALK`.
pub(in crate::syn) const WALK: &str = "WALK";

/// GrynnQL keyword `ACYCLIC`.
pub(in crate::syn) const ACYCLIC: &str = "ACYCLIC";
/// GrynnQL keyword `ATOMS`.
pub(in crate::syn) const ATOMS: &str = "ATOMS";
/// GrynnQL keyword `COST`.
pub(in crate::syn) const COST: &str = "COST";
/// GrynnQL keyword `CROSS_LEVEL`.
pub(in crate::syn) const CROSS_LEVEL: &str = "CROSS_LEVEL";
/// GrynnQL keyword `TRAIL`.
pub(in crate::syn) const TRAIL: &str = "TRAIL";
/// GrynnQL keyword `TRAVERSE`.
pub(in crate::syn) const TRAVERSE: &str = "TRAVERSE";
/// GrynnQL keyword `WHEN`.
pub(in crate::syn) const WHEN: &str = "WHEN";
/// GrynnQL keyword `WHILE`.
pub(in crate::syn) const WHILE: &str = "WHILE";

/// Statement starts recognized only to return the explicit unsupported status.
pub(in crate::syn) const UNSUPPORTED_STATEMENTS: &[&str] = &[];

/// Reactive query/binding statement prefix.
pub(in crate::syn) const LIVE: &str = "LIVE";
/// Introduces queue overflow policy.
pub(in crate::syn) const OVERFLOW: &str = "OVERFLOW";
/// Explicit delivery loss policy.
pub(in crate::syn) const GAP: &str = "GAP";
/// Terminal overflow policy.
pub(in crate::syn) const DISCONNECT: &str = "DISCONNECT";
/// Capacity wait policy before publication.
pub(in crate::syn) const BLOCK: &str = "BLOCK";
/// Maximum event entries retained by a queue.
pub(in crate::syn) const BUFFER: &str = "BUFFER";
