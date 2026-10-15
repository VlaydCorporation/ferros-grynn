use super::stmt::{Block, MatchStatement, RecordKey, SelectStatement, Statement};
use crate::gql_status::TextSpan;

/// Ordered syntax statements parsed from one GrynnQL source.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Program {
	/// Statements retained in source execution order.
	pub statements: Vec<Statement>,
}

/// Parsed identifier retaining its spelling, quoting choice and source span.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Identifier {
	/// Payload retained for the operation or its expected result.
	pub value: Box<str>,
	/// Whether the identifier was explicitly quoted in the source text.
	pub quoted: bool,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Type spelling and source span retained before semantic resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TypeName {
	/// Payload retained for the operation or its expected result.
	pub value: Box<str>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Expression syntax paired with its source span.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Expr {
	/// Selected operation or representation discriminator.
	pub kind: ExprKind,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Expression forms recognized by the GrynnQL syntax front end.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExprKind {
	/// Literal runtime value written directly in the source.
	Literal(
		/// Literal runtime value written directly in the source.
		Literal,
	),
	/// Reference to a named query parameter.
	Parameter(
		/// Reference to a named query parameter.
		Identifier,
	),
	/// Reference to an identifier resolved by semantic analysis.
	Name(
		/// Reference to an identifier resolved by semantic analysis.
		Identifier,
	),
	/// Array expression whose elements are evaluated in order.
	Array(
		/// Array expression whose elements are evaluated in order.
		Vec<Expr>,
	),
	/// Set expression constructed from evaluated elements.
	Set(
		/// Set expression constructed from evaluated elements.
		Vec<Expr>,
	),
	/// Object expression composed of named fields.
	Object(
		/// Object expression composed of named fields.
		Vec<ObjectField>,
	),
	/// Range expression with independently optional bounds.
	Range {
		/// Initial endpoint or lower range bound selected by the syntax
		/// element.
		start: Option<Box<Expr>>,
		/// Final endpoint or upper range bound selected by the syntax element.
		end: Option<Box<Expr>>,
		/// Whether the range includes its explicitly specified end value.
		inclusive_end: bool,
	},
	/// Unary operator applied to one expression.
	Unary {
		/// Operator applied to the retained operand expressions.
		op: UnaryOperator,
		/// Expression retained for later semantic analysis and execution.
		expr: Box<Expr>,
	},
	/// Binary operator applied to two expressions.
	Binary {
		/// Left operand or fixture endpoint used by this operation.
		left: Box<Expr>,
		/// Operator applied to the retained operand expressions.
		op: BinaryOperator,
		/// Right operand or fixture endpoint used by this operation.
		right: Box<Expr>,
	},
	/// Fulltext predicate with an optional statement-local match slot.
	Fulltext {
		/// Left operand or fixture endpoint used by this operation.
		left: Box<Expr>,
		/// Operator applied to the retained operand expressions.
		op: FulltextOperator,
		/// Right operand or fixture endpoint used by this operation.
		right: Box<Expr>,
	},
	/// Infix top-K vector predicate.
	Knn {
		/// Left operand or fixture endpoint used by this operation.
		left: Box<Expr>,
		/// Expression selecting how many nearest neighbours the predicate
		/// requests.
		k: Box<Expr>,
		/// Optional distance or candidate-list control for vector search.
		search: Option<KnnSearchArgument>,
		/// Right operand or fixture endpoint used by this operation.
		right: Box<Expr>,
	},
	/// Condition selecting between two expression branches.
	Conditional {
		/// Expression deciding which branch is evaluated.
		condition: Box<Expr>,
		/// Expression evaluated when the condition succeeds.
		then_value: Box<Expr>,
		/// Alternative expression evaluated when no earlier branch succeeds.
		else_value: Box<Expr>,
	},
	/// Ordered condition-result branches with an optional alternative.
	Case {
		/// Optional CASE operand compared with the retained branch conditions.
		operand: Option<Box<Expr>>,
		/// Ordered condition and result pairs of the CASE expression.
		branches: Vec<(Expr, Expr)>,
		/// Alternative expression evaluated when no earlier branch succeeds.
		else_value: Option<Box<Expr>>,
	},
	/// Property access on the value produced by a base expression.
	Field {
		/// Expression producing the value on which a field, index or filter
		/// operates.
		base: Box<Expr>,
		/// Property name addressed by this expression or schema declaration.
		field: Identifier,
		/// Whether access tolerates an absent or null intermediate value.
		null_safe: bool,
	},
	/// Selection of multiple named fields from a base expression.
	Destructure {
		/// Expression producing the value on which a field, index or filter
		/// operates.
		base: Box<Expr>,
		/// Ordered property names addressed by the clause.
		fields: Vec<Identifier>,
	},
	/// Indexed access into a base expression value.
	Index {
		/// Expression producing the value on which a field, index or filter
		/// operates.
		base: Box<Expr>,
		/// Index expression or named index selected by this syntax element.
		index: Box<Expr>,
	},
	/// Predicate-based filtering of a base expression value.
	Filter {
		/// Expression producing the value on which a field, index or filter
		/// operates.
		base: Box<Expr>,
		/// Condition restricting the values or incidences accepted by this
		/// element.
		predicate: Box<Expr>,
	},
	/// Invocation of a callable with positional or named arguments.
	Call {
		/// Expression resolving the callable to invoke.
		callee: Box<Expr>,
		/// Optional type argument retained by the call syntax.
		type_argument: Option<TypeName>,
		/// Whether duplicate values are removed before producing the result.
		distinct: bool,
		/// Optional parameter names corresponding to call arguments.
		argument_names: Vec<Option<Identifier>>,
		/// Positional arguments supplied to the resolved operation.
		arguments: Vec<Expr>,
	},
	/// Nested query used as an expression.
	Subquery(
		/// Nested query used as an expression.
		Box<SelectStatement>,
	),
	/// MATCH query accepted only as the child of `tv::match(...)`.
	MatchSubquery(
		/// MATCH query accepted only as the child of `tv::match(...)`.
		Box<MatchStatement>,
	),
	/// Explicit conversion to a declared type.
	Cast {
		/// Declared target type of the explicit cast expression.
		target: TypeName,
		/// Expression retained for later semantic analysis and execution.
		expr: Box<Expr>,
	},
	/// Session-scoped language closure.
	Closure(
		/// Session-scoped language closure.
		ClosureExpr,
	),
}

/// Query-term combination encoded by one Fulltext operator token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FulltextOperator {
	/// OR over analyzed terms without an observable match slot.
	Any,
	/// OR over analyzed terms with one explicit statement-local slot.
	Slot(
		/// OR over analyzed terms with one explicit statement-local slot.
		u16,
	),
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
	Distance(
		/// Exact or approximate distance contract.
		KnnDistance,
	),
	/// Dynamic ANN candidate-list size.
	Ef(
		/// Dynamic ANN candidate-list size.
		Box<Expr>,
	),
}

/// Parsed KNN distance before type and durable-index resolution.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum KnnDistance {
	/// Euclidean distance between numeric vectors.
	Euclidean,
	/// Cosine distance between numeric vectors.
	Cosine,
	/// L1 distance between numeric vectors.
	Manhattan,
	/// Minkowski distance with an explicit exponent.
	Minkowski(
		/// Minkowski distance with an explicit exponent.
		Box<Expr>,
	),
	/// Inner-product distance for vector search.
	InnerProduct,
	/// Cosine-distance form for normalized vectors.
	CosineNormalized,
	/// User-selected custom vector distance expression.
	Custom(
		/// User-selected custom vector distance expression.
		Box<Expr>,
	),
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
	Expression(
		/// Single-expression closure body.
		Box<Expr>,
	),
	/// Statement block with lexical scope and explicit `RETURN` support.
	Block(
		/// Statement block with lexical scope and explicit `RETURN` support.
		Block,
	),
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

/// Named object-literal field with its expression and source span.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ObjectField {
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Payload retained for the operation or its expected result.
	pub value: Expr,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Literal values represented directly in the syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Literal {
	/// Literal syntax representing an absent value.
	None,
	/// Literal syntax representing an explicit null value.
	Null,
	/// Literal syntax representing a Boolean value.
	Bool(
		/// Literal syntax representing a Boolean value.
		bool,
	),
	/// Literal syntax representing a numeric value.
	Number(
		/// Literal syntax representing a numeric value.
		Box<str>,
	),
	/// Literal syntax representing text.
	String(
		/// Literal syntax representing text.
		Box<str>,
	),
	/// Literal syntax representing a class-qualified record identity.
	Record {
		/// Class selected by the schema declaration, query target or fixture.
		class: Box<str>,
		/// Record key expression retained for target resolution.
		key: RecordKey,
	},
}

/// Single-operand logical or numeric operator in expression syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnaryOperator {
	/// Logical negation.
	Not,
	/// Conversion to a truth value.
	Truthy,
	/// Numeric sign negation.
	Negate,
	/// Explicit positive numeric sign.
	Positive,
}

/// Two-operand arithmetic, comparison, collection or logical operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BinaryOperator {
	/// Raise the left operand to the right operand power.
	Power,
	/// Multiply both operands.
	Multiply,
	/// Divide the left operand by the right operand.
	Divide,
	/// Compute the division remainder.
	Remainder,
	/// Add or concatenate the operands according to their types.
	Add,
	/// Subtract the right operand from the left operand.
	Subtract,
	/// Compare whether the left operand is smaller.
	Less,
	/// Compare whether the left operand is smaller or equal.
	LessOrEqual,
	/// Compare whether the left operand is larger.
	Greater,
	/// Compare whether the left operand is larger or equal.
	GreaterOrEqual,
	/// Compare operands for equality.
	Equal,
	/// Compare operands for inequality.
	NotEqual,
	/// Apply the IS comparison predicate.
	Is,
	/// Negate the IS comparison predicate.
	NotIs,
	/// Test whether the left value belongs to the right collection.
	In,
	/// Test whether the left value does not belong to the right collection.
	NotIn,
	/// Require every selected value to belong to the right collection.
	AllIn,
	/// Require at least one selected value to belong to the right collection.
	AnyIn,
	/// Require no selected values to belong to the right collection.
	NoneIn,
	/// Require at least one selected value to equal the right operand.
	AnyEqual,
	/// Require every selected value to equal the right operand.
	AllEqual,
	/// Test whether the left collection contains the right value.
	Contains,
	/// Test whether the left collection excludes the right value.
	ContainsNot,
	/// Require the left collection to contain at least one selected value.
	ContainsAny,
	/// Require the left collection to contain every selected value.
	ContainsAll,
	/// Combine operand truth values with logical conjunction.
	And,
	/// Combine operand truth values with logical disjunction.
	Or,
	/// Use the right operand when the left value satisfies the coalescing
	/// condition.
	Coalesce,
	/// Use the right operand when the left value is not truthy.
	Elvis,
	/// Pass the left result into the right pipeline operation.
	Pipeline,
	/// Test whether the left geometry lies inside the right geometry.
	Inside,
	/// Negate the geometry-inside predicate.
	NotInside,
	/// Require all selected geometries to lie inside the right geometry.
	AllInside,
	/// Require at least one selected geometry to lie inside the right geometry.
	AnyInside,
	/// Require no selected geometry to lie inside the right geometry.
	NoneInside,
	/// Test whether the left geometry lies outside the right geometry.
	Outside,
	/// Test whether the geometries intersect.
	Intersects,
}

/// Parses the cover syntax.
pub(super) const fn cover(start: TextSpan, end: TextSpan) -> TextSpan {
	let end = end.offset + end.len;
	TextSpan::new(start.offset, end - start.offset)
}
