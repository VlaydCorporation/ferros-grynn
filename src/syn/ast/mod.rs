use super::stmt::{Block, MatchStatement, RecordKey, SelectStatement, Statement};
use crate::gql_status::TextSpan;

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Program {
	pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Identifier {
	pub value: Box<str>,
	pub quoted: bool,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TypeName {
	pub value: Box<str>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Expr {
	pub kind: ExprKind,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExprKind {
	Literal(Literal),
	Parameter(Identifier),
	Name(Identifier),
	Array(Vec<Expr>),
	Set(Vec<Expr>),
	Object(Vec<ObjectField>),
	Range {
		start: Option<Box<Expr>>,
		end: Option<Box<Expr>>,
		inclusive_end: bool,
	},
	Unary {
		op: UnaryOperator,
		expr: Box<Expr>,
	},
	Binary {
		left: Box<Expr>,
		op: BinaryOperator,
		right: Box<Expr>,
	},
	/// Fulltext predicate with an optional statement-local match slot.
	Fulltext {
		left: Box<Expr>,
		op: FulltextOperator,
		right: Box<Expr>,
	},
	/// Infix top-K vector predicate.
	Knn {
		left: Box<Expr>,
		k: Box<Expr>,
		search: Option<KnnSearchArgument>,
		right: Box<Expr>,
	},
	Conditional {
		condition: Box<Expr>,
		then_value: Box<Expr>,
		else_value: Box<Expr>,
	},
	Case {
		operand: Option<Box<Expr>>,
		branches: Vec<(Expr, Expr)>,
		else_value: Option<Box<Expr>>,
	},
	Field {
		base: Box<Expr>,
		field: Identifier,
		null_safe: bool,
	},
	Destructure {
		base: Box<Expr>,
		fields: Vec<Identifier>,
	},
	Index {
		base: Box<Expr>,
		index: Box<Expr>,
	},
	Filter {
		base: Box<Expr>,
		predicate: Box<Expr>,
	},
	Call {
		callee: Box<Expr>,
		type_argument: Option<TypeName>,
		distinct: bool,
		argument_names: Vec<Option<Identifier>>,
		arguments: Vec<Expr>,
	},
	Subquery(Box<SelectStatement>),
	/// MATCH query accepted only as the child of `tv::match(...)`.
	MatchSubquery(Box<MatchStatement>),
	Cast {
		target: TypeName,
		expr: Box<Expr>,
	},
	/// Session-scoped language closure.
	Closure(ClosureExpr),
}

/// Query-term combination encoded by one Fulltext operator token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FulltextOperator {
	/// OR over analyzed terms without an observable match slot.
	Any,
	/// OR over analyzed terms with one explicit statement-local slot.
	Slot(u16),
	/// All distinct analyzed terms must occur.
	All,
	/// At least one analyzed term must occur.
	AnyOf,
}

/// Optional search-control argument inside `<| ... |>`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum KnnSearchArgument {
	/// Exact or approximate distance contract.
	Distance(KnnDistance),
	/// Dynamic ANN candidate-list size.
	Ef(Box<Expr>),
}

/// Parsed KNN distance before type and durable-index resolution.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum KnnDistance {
	Euclidean,
	Cosine,
	Manhattan,
	Minkowski(Box<Expr>),
	InnerProduct,
	CosineNormalized,
	Custom(Box<Expr>),
}

/// One declared closure parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClosureParameter {
	/// Parameter binding local to the closure invocation.
	pub name: Identifier,
	/// Optional explicit parameter type; omitted parameters use `any`.
	pub declared_type: Option<TypeName>,
}

/// Closure body executed through the ordinary typed expression/block evaluator.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ClosureBody {
	/// Single-expression closure body.
	Expression(Box<Expr>),
	/// Statement block with lexical scope and explicit `RETURN` support.
	Block(Block),
}

/// Parsed language closure before capture and effect analysis.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ClosureExpr {
	/// Ordered invocation parameters.
	pub parameters: Vec<ClosureParameter>,
	/// Optional declared return type.
	pub result_type: Option<TypeName>,
	/// Executable body.
	pub body: ClosureBody,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ObjectField {
	pub name: Identifier,
	pub value: Expr,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Literal {
	None,
	Null,
	Bool(bool),
	Number(Box<str>),
	String(Box<str>),
	Record {
		class: Box<str>,
		key: RecordKey,
	},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnaryOperator {
	Not,
	Truthy,
	Negate,
	Positive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BinaryOperator {
	Power,
	Multiply,
	Divide,
	Remainder,
	Add,
	Subtract,
	Less,
	LessOrEqual,
	Greater,
	GreaterOrEqual,
	Equal,
	NotEqual,
	Is,
	NotIs,
	In,
	NotIn,
	AllIn,
	AnyIn,
	NoneIn,
	AnyEqual,
	AllEqual,
	Contains,
	ContainsNot,
	ContainsAny,
	ContainsAll,
	And,
	Or,
	Coalesce,
	Elvis,
	Pipeline,
	Inside,
	NotInside,
	AllInside,
	AnyInside,
	NoneInside,
	Outside,
	Intersects,
}

pub(super) const fn cover(start: TextSpan, end: TextSpan) -> TextSpan {
	let end = end.offset + end.len;
	TextSpan::new(start.offset, end - start.offset)
}
