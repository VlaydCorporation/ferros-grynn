// These internals become reachable from the public Datastore API when that
// layer lands.
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod analysis;
#[allow(dead_code, reason = "used by the Datastore adapter in the next vertical slice")]
mod execution;
#[allow(dead_code, reason = "the crate-private executor owns the logical IR")]
mod ir;

use std::time::Duration;

pub use crate::syn::{
	BinaryOperator,
	Expr,
	ExprKind,
	Identifier,
	LetStatement,
	Literal,
	ObjectField,
	ParserSettings,
	Program,
	RecordKey,
	ReturnStatement,
	SelectItem,
	SelectStatement,
	SelectTarget,
	Statement,
	StatementKind,
	TypeName,
	UnaryOperator,
};
use crate::{
	gql_status::{FerrosGrynnResult, Outcome},
	values::Value,
};

pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	crate::syn::parse(source, settings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatementKind {
	Let,
	Return,
	Select,
}

#[derive(Debug, Clone)]
pub struct QueryResult {
	pub kind: QueryStatementKind,
	pub elapsed: Duration,
	pub result: FerrosGrynnResult<Outcome<Value>>,
}
