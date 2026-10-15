mod changefeed;
/// Cancellation, deadlines and execution budgets regression tests.
mod control;
/// Record mutation statements regression tests.
mod dml;
mod event;
/// Query plans and EXPLAIN output regression tests.
mod explain;
/// Graph mutation statements regression tests.
mod graph_dml;
/// Owned graph query results regression tests.
mod graph_result;
/// Import statements and lifecycle regression tests.
mod import;
mod landmark;
mod live;
/// MATCH query semantics regression tests.
mod r#match;
/// Atom-walk pattern matching regression tests.
mod match_atomwalk;
/// Hyperedge pattern matching regression tests.
mod match_hyper;
/// Nested graph pattern matching regression tests.
mod match_nested;
/// MATCH results against reference expectations regression tests.
mod match_oracle;
/// Standard MATCH forms regression tests.
mod match_standard;
/// Optimizer decisions through query execution regression tests.
mod optimizer_probe;
/// Public schema definitions regression tests.
mod schema;
/// SELECT projection, grouping and ordering regression tests.
mod select;
mod tensor_analytics;
mod tensor_paths;
/// Transaction statement behavior regression tests.
mod transaction;
/// Traversal queries and path results regression tests.
mod traversal;
