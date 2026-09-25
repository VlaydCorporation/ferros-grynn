use super::stmt::Statement;
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
		arguments: Vec<Expr>,
	},
	Cast {
		target: TypeName,
		expr: Box<Expr>,
	},
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
}

pub(super) const fn cover(start: TextSpan, end: TextSpan) -> TextSpan {
	let end = end.offset + end.len;
	TextSpan::new(start.offset, end - start.offset)
}
