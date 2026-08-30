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
pub struct Statement {
	pub kind: StatementKind,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StatementKind {
	Let(LetStatement),
	Match(MatchStatement),
	Return(ReturnStatement),
	Select(SelectStatement),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStatement {
	pub atom: bool,
	pub patterns: Vec<MatchPattern>,
	pub source: Identifier,
	pub filter: Option<Expr>,
	pub projection: Vec<MatchReturnItem>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchPattern {
	pub path_alias: Option<Identifier>,
	pub start: MatchEndpoint,
	pub steps: Vec<MatchStep>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEndpoint {
	pub kind: MatchEndpointKind,
	pub alias: Option<Identifier>,
	pub labels: Vec<Identifier>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatchEndpointKind {
	Node,
	Edge,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStep {
	pub direction: MatchDirection,
	pub edge: MatchEdge,
	pub target: MatchEndpoint,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEdge {
	pub alias: Option<Identifier>,
	pub labels: Vec<Identifier>,
	pub length: Option<MatchLength>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatchDirection {
	Forward,
	Backward,
	Undirected,
	Bidirectional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchLength {
	pub min: u32,
	pub max: u32,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchReturnItem {
	pub expr: Expr,
	pub alias: Option<Identifier>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LetStatement {
	pub name: Identifier,
	pub declared_type: Option<TypeName>,
	pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReturnStatement {
	pub value: Expr,
	pub alias: Option<Identifier>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
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
#[non_exhaustive]
pub enum SelectItem {
	Wildcard(TextSpan),
	Expr {
		expr: Expr,
		alias: Option<Identifier>,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectTarget {
	Table(Identifier),
	Record {
		table: Identifier,
		key: RecordKey,
		span: TextSpan,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordKey {
	Integer(Box<str>, TextSpan),
	String(Box<str>, TextSpan),
}

impl RecordKey {
	pub(crate) const fn span(&self) -> TextSpan {
		match self {
			Self::Integer(_, span) | Self::String(_, span) => *span,
		}
	}
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
	Assert,
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
