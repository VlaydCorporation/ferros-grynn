use crate::gql_status::TextSpan;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
	pub statements: Vec<Statement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
	pub value: Box<str>,
	pub quoted: bool,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeName {
	pub value: Box<str>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
	pub kind: StatementKind,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementKind {
	Let(LetStatement),
	Return(ReturnStatement),
	Select(SelectStatement),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LetStatement {
	pub name: Identifier,
	pub declared_type: Option<TypeName>,
	pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnStatement {
	pub value: Expr,
	pub alias: Option<Identifier>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectStatement {
	pub only: bool,
	pub value: bool,
	pub projection: Vec<SelectItem>,
	pub target: SelectTarget,
	pub filter: Option<Expr>,
	pub limit: Option<Expr>,
	pub start: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
	Wildcard(TextSpan),
	Expr {
		expr: Expr,
		alias: Option<Identifier>,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectTarget {
	Table(Identifier),
	Record {
		table: Identifier,
		key: RecordKey,
		span: TextSpan,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordKey {
	Integer(Box<str>, TextSpan),
	String(Box<str>, TextSpan),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
	pub kind: ExprKind,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
	Literal(Literal),
	Parameter(Identifier),
	Name(Identifier),
	Array(Vec<Expr>),
	Object(Vec<ObjectField>),
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
	Field {
		base: Box<Expr>,
		field: Identifier,
		null_safe: bool,
	},
	Index {
		base: Box<Expr>,
		index: Box<Expr>,
	},
	Call {
		callee: Box<Expr>,
		arguments: Vec<Expr>,
	},
	Cast {
		target: TypeName,
		expr: Box<Expr>,
	},
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectField {
	pub name: Identifier,
	pub value: Expr,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
	None,
	Null,
	Bool(bool),
	Number(Box<str>),
	String(Box<str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOperator {
	Not,
	Assert,
	Negate,
	Positive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
	And,
	Or,
	Coalesce,
	Elvis,
	Pipeline,
}

pub(super) fn cover(start: TextSpan, end: TextSpan) -> TextSpan {
	let end = end.offset + end.len;
	TextSpan::new(start.offset, end - start.offset)
}
