use std::time::Duration;

use crate::{
	datastore::{ClassIndexKind, ClassSchemaMode, IndexScope},
	gql_status::TextSpan,
	syn::{DefinitionMode, MatchDirection},
	values::{DateTime, GraphKind, GraphRef, RecordId, Text, Type, Value},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct NodeId(pub(super) u32);

#[derive(Debug, Clone)]
pub(super) struct LogicalProgram {
	pub(super) statements: Vec<LogicalStatement>,
	pub(super) initial_slots: Vec<(Box<str>, u32)>,
	pub(super) slot_count: usize,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalStatement {
	Explain(LogicalExplain),
	Schema(LogicalSchemaStatement),
	Dml(LogicalDmlStatement),
	GraphDml(LogicalGraphDmlStatement),
	Let {
		name: Box<str>,
		slot: u32,
		value: TypedExpr,
	},
	Return {
		value: TypedExpr,
	},
	Expression {
		value: TypedExpr,
	},
	If {
		branches: Vec<(TypedExpr, LogicalBlock)>,
		else_block: Option<LogicalBlock>,
	},
	For {
		slot: u32,
		iterable: TypedExpr,
		body: LogicalBlock,
	},
	While {
		condition: TypedExpr,
		body: LogicalBlock,
	},
	Break,
	Continue,
	Try {
		body: LogicalBlock,
		catch: Option<(u32, LogicalBlock)>,
	},
	Throw {
		value: TypedExpr,
	},
	AssertInvariants {
		graph: Option<GraphRef>,
		report: usize,
	},
	/// Executes a read-only graph pattern match.
	Match(LogicalMatch),
	Select(LogicalSelect),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalExplain {
	pub(super) mode: crate::syn::ExplainMode,
	pub(super) format: crate::syn::ExplainFormat,
	pub(super) statement: Box<LogicalStatement>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalBlock {
	pub(super) statements: Vec<LogicalBlockStatement>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalBlockStatement {
	pub(super) statement: LogicalStatement,
	pub(super) timeout: Option<Duration>,
}

/// One analyzed graph-membership or graph-incidence mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalGraphDmlStatement {
	Attach {
		record: LogicalRecordTarget,
		graph: GraphRef,
		label: Option<Text>,
	},
	Detach {
		record: LogicalRecordTarget,
		graph: GraphRef,
		cascade: bool,
	},
	Relate {
		atom_endpoints: bool,
		source: LogicalRecordTarget,
		edge: LogicalRelateEdge,
		target: LogicalRecordTarget,
		graph: GraphRef,
		label: Option<Text>,
		direction: MatchDirection,
		weight: Option<TypedExpr>,
	},
}

impl LogicalGraphDmlStatement {
	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Attach {
				..
			} => crate::gql::QueryStatementKind::Attach,
			Self::Detach {
				..
			} => crate::gql::QueryStatementKind::Detach,
			Self::Relate {
				..
			} => crate::gql::QueryStatementKind::Relate,
		}
	}
}

/// Exact class-qualified record target used by graph DML.
#[derive(Debug, Clone)]
pub(super) struct LogicalRecordTarget {
	pub(super) class: Box<str>,
	pub(super) key: TypedExpr,
}

/// Edge atom and graph-local payload prepared for RELATE.
#[derive(Debug, Clone)]
pub(super) struct LogicalRelateEdge {
	pub(super) class: Box<str>,
	pub(super) key: Option<TypedExpr>,
	pub(super) schema: Schema,
	pub(super) data: Option<LogicalDmlData>,
	pub(super) returning: LogicalDmlReturn,
	pub(super) expect: Option<LogicalExpect>,
}

/// One analyzed data-mutation statement.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlStatement {
	pub(super) kind: LogicalDmlKind,
	pub(super) graph: Option<GraphRef>,
	pub(super) label: Option<Option<crate::values::Text>>,
	pub(super) cascade: bool,
	pub(super) targets: Vec<LogicalDmlTarget>,
}

/// Mutation operation shared by every target in a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogicalDmlKind {
	Create,
	Update,
	Upsert,
	Delete,
}

impl LogicalDmlKind {
	pub(super) const fn statement_kind(self) -> crate::gql::QueryStatementKind {
		match self {
			Self::Create => crate::gql::QueryStatementKind::Create,
			Self::Update => crate::gql::QueryStatementKind::Update,
			Self::Upsert => crate::gql::QueryStatementKind::Upsert,
			Self::Delete => crate::gql::QueryStatementKind::Delete,
		}
	}
}

/// One class-specific mutation plan. Expressions are typed against this
/// target because a statement may name classes with different schemas.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlTarget {
	pub(super) class: Box<str>,
	pub(super) selector: LogicalDmlSelector,
	pub(super) schema: Schema,
	pub(super) data: Option<LogicalDmlData>,
	pub(super) filter: Option<TypedExpr>,
	pub(super) returning: LogicalDmlReturn,
	pub(super) expect: Option<LogicalExpect>,
}

/// Records selected or generated for one DML target.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlSelector {
	Class,
	Record(TypedExpr),
	Range(u32),
}

/// An analyzed record mutation mode.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlData {
	Content(TypedExpr),
	Merge(TypedExpr),
	Patch(TypedExpr),
	Replace(TypedExpr),
	Set(Vec<(Box<str>, TypedExpr)>),
	Unset(Vec<Box<str>>),
	Reset(Vec<Box<str>>),
}

/// An analyzed DML result projection.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlReturn {
	None,
	Before,
	After,
	Diff,
	Fields(Vec<LogicalDmlReturnItem>),
	Value(TypedExpr),
}

/// One object-shaped return item.
#[derive(Debug, Clone)]
pub(super) enum LogicalDmlReturnItem {
	Wildcard,
	Expr {
		name: Box<str>,
		value: TypedExpr,
	},
}

/// A pre-image requirement checked before the first mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalExpect {
	Exists(bool),
	Predicate(TypedExpr),
}

/// Typed core schema command executed by the datastore adapter.
#[derive(Debug, Clone)]
pub(super) enum LogicalSchemaStatement {
	DefineGraph {
		mode: DefinitionMode,
		name: Box<str>,
		kind: GraphKind,
		content: Option<TypedExpr>,
	},
	UpdateGraph {
		graph: GraphRef,
		mutation: LogicalGraphPropertyMutation,
		returning: LogicalDmlReturn,
	},
	DefineClass {
		mode: DefinitionMode,
		name: Box<str>,
		kind: crate::values::AtomKind,
		schema_mode: ClassSchemaMode,
	},
	DefineField {
		mode: DefinitionMode,
		class: Box<str>,
		name: Box<str>,
		value_type: Type,
	},
	DefineIndex {
		mode: DefinitionMode,
		class: Box<str>,
		name: Box<str>,
		kind: ClassIndexKind,
		fields: Vec<Box<str>>,
		scope: IndexScope,
	},
	RemoveGraph {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveClass {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveField {
		if_exists: bool,
		class: Box<str>,
		name: Box<str>,
	},
	RemoveIndex {
		if_exists: bool,
		class: Box<str>,
		name: Box<str>,
	},
	AlterClassMode {
		if_exists: bool,
		name: Box<str>,
		mode: ClassSchemaMode,
	},
	AlterFieldType {
		if_exists: bool,
		class: Box<str>,
		name: Box<str>,
		value_type: Type,
	},
	PrepareIndexRemoval {
		class: Box<str>,
		name: Box<str>,
	},
	InfoDatabase,
	InfoClass {
		name: Box<str>,
	},
	InfoIndex {
		class: Box<str>,
		name: Box<str>,
	},
}

/// One catalog-owned graph-property mutation.
#[derive(Debug, Clone)]
pub(super) enum LogicalGraphPropertyMutation {
	Set(Vec<(Vec<Box<str>>, TypedExpr)>),
	Unset(Vec<Vec<Box<str>>>),
}

impl LogicalSchemaStatement {
	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::DefineGraph {
				..
			}
			| Self::DefineClass {
				..
			}
			| Self::DefineField {
				..
			}
			| Self::DefineIndex {
				..
			} => crate::gql::QueryStatementKind::Define,
			Self::UpdateGraph {
				..
			} => crate::gql::QueryStatementKind::Update,
			Self::RemoveGraph {
				..
			}
			| Self::RemoveClass {
				..
			}
			| Self::RemoveField {
				..
			}
			| Self::RemoveIndex {
				..
			} => crate::gql::QueryStatementKind::Remove,
			Self::AlterClassMode {
				..
			}
			| Self::AlterFieldType {
				..
			}
			| Self::PrepareIndexRemoval {
				..
			} => crate::gql::QueryStatementKind::Alter,
			Self::InfoDatabase
			| Self::InfoClass {
				..
			}
			| Self::InfoIndex {
				..
			} => crate::gql::QueryStatementKind::Info,
		}
	}
}

#[derive(Debug, Clone)]
/// An analyzed MATCH statement tied to one catalog graph.
pub(super) struct LogicalMatch {
	/// Resolved graph reference captured during analysis.
	pub(super) graph: GraphRef,
	/// Analyzed patterns in their source order.
	pub(super) patterns: Vec<LogicalMatchPattern>,
	/// Columns bound by aliases in all patterns.
	pub(super) bindings: Schema,
	/// Optional row predicate.
	pub(super) filter: Option<TypedExpr>,
	/// Expressions evaluated for each surviving row.
	pub(super) projection: Vec<TypedExpr>,
	/// Public result-column schema.
	pub(super) result: Schema,
}

#[derive(Debug, Clone)]
/// One linear MATCH pattern before runtime reordering.
pub(super) struct LogicalMatchPattern {
	/// Initial endpoint binding.
	pub(super) start: MatchBinding,
	/// Linear expansion operations.
	pub(super) ops: Vec<MatchOp>,
	/// Stable source-order tie breaker for the optimizer.
	pub(super) original_index: usize,
}

#[derive(Debug, Clone)]
/// A linear MATCH expansion operation.
pub(super) enum MatchOp {
	/// Expands through exactly one connector edge.
	Expand(Expand),
	/// Expands through a finite TRAIL path.
	PathExpand(PathExpand),
}

#[derive(Debug, Clone)]
/// A fixed-length connector expansion.
pub(super) struct Expand {
	/// Source endpoint binding.
	pub(super) source: MatchBinding,
	/// Connector-edge binding.
	pub(super) edge: MatchBinding,
	/// Target endpoint binding.
	pub(super) target: MatchBinding,
	/// Required traversal direction.
	pub(super) direction: crate::syn::MatchDirection,
}

#[derive(Debug, Clone)]
/// A bounded variable-length TRAIL expansion.
pub(super) struct PathExpand {
	/// Source endpoint binding.
	pub(super) source: MatchBinding,
	/// Connector-edge constraints applied to every hop.
	pub(super) edge: MatchBinding,
	/// Target endpoint binding.
	pub(super) target: MatchBinding,
	/// Required traversal direction.
	pub(super) direction: crate::syn::MatchDirection,
	/// Inclusive minimum hop count.
	pub(super) min: u32,
	/// Inclusive maximum hop count.
	pub(super) max: u32,
	/// Optional column receiving the public path value.
	pub(super) path_column: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Runtime domain accepted by a MATCH endpoint.
pub(super) enum EndpointDomain {
	/// Node atoms only.
	Node,
	/// Edge atoms only.
	Edge,
}

impl EndpointDomain {
	pub(super) const fn atom_kind(self) -> crate::values::AtomKind {
		match self {
			Self::Node => crate::values::AtomKind::Node,
			Self::Edge => crate::values::AtomKind::Edge,
		}
	}
}

#[derive(Debug, Clone)]
/// Alias and label constraints for one MATCH atom.
pub(super) struct MatchBinding {
	/// Required atom domain.
	pub(super) domain: EndpointDomain,
	/// Global class or exact-record selector.
	pub(super) selector: LogicalMatchSelector,
	/// Optional binding-column index.
	pub(super) column: Option<usize>,
	/// Optional graph-local label predicate.
	pub(super) label: Option<Box<str>>,
}

#[derive(Debug, Clone)]
/// Global selector resolved before graph-local label filtering.
pub(super) enum LogicalMatchSelector {
	/// Any atom in the endpoint domain.
	Any,
	/// Any atom of one global class.
	Class(Box<str>),
	/// One exact global record identity.
	Record(LogicalRecordTarget),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelect {
	pub(super) source: LogicalSelectSource,
	pub(super) joins: Vec<LogicalSelectJoin>,
	pub(super) access: LogicalSelectAccess,
	pub(super) input_schema: Schema,
	pub(super) filter: Option<TypedExpr>,
	pub(super) split: Vec<TypedExpr>,
	pub(super) group: Option<Vec<TypedExpr>>,
	pub(super) projection: Vec<LogicalSelectItem>,
	pub(super) result: Schema,
	pub(super) omit: Vec<Vec<Box<str>>>,
	pub(super) fetch: Vec<Vec<Box<str>>>,
	pub(super) order: Vec<LogicalOrder>,
	pub(super) start: Option<LogicalSelectStart>,
	pub(super) limit: Option<TypedExpr>,
	pub(super) value: bool,
	pub(super) only: bool,
	pub(super) tempfiles: bool,
	pub(super) outer_columns: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) enum LogicalSelectAccess {
	#[default]
	Automatic,
	NoIndex,
	Indexes(Vec<LogicalIndexHint>),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalIndexHint {
	pub(super) source: Option<Box<str>>,
	pub(super) index: Box<str>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelectSource {
	pub(super) kind: LogicalSelectSourceKind,
	pub(super) schema: Schema,
	pub(super) wildcard: Vec<LogicalWildcardField>,
	pub(super) alias: Box<str>,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalSelectSourceKind {
	Atoms {
		target: ScanTarget,
		graph: Option<LogicalGraphReference>,
	},
	Graphs {
		references: Option<Vec<LogicalGraphReference>>,
	},
	Derived(Box<LogicalSelect>),
}

#[derive(Debug, Clone)]
pub(super) enum LogicalGraphReference {
	Static(GraphRef),
	Expression(TypedExpr),
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelectJoin {
	pub(super) kind: crate::syn::SelectJoinKind,
	pub(super) source: LogicalSelectSource,
	pub(super) predicate: Option<TypedExpr>,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalSelectItem {
	Wildcard(Vec<LogicalWildcardField>),
	Expression {
		name: Box<str>,
		expression: TypedExpr,
	},
}

#[derive(Debug, Clone)]
pub(super) enum LogicalWildcardField {
	Column {
		name: Box<str>,
		index: usize,
	},
	Properties {
		index: usize,
	},
}

#[derive(Debug, Clone)]
pub(super) struct LogicalOrder {
	pub(super) expression: TypedExpr,
	pub(super) random: bool,
	pub(super) direction: crate::syn::SelectOrderDirection,
	pub(super) nulls: crate::syn::SelectNulls,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalSelectStart {
	Offset(TypedExpr),
	After {
		cursor: TypedExpr,
		read_committed: bool,
	},
}

#[derive(Debug, Clone)]
pub(super) enum ScanTarget {
	Class(Box<str>),
	Record(RecordId),
}

impl ScanTarget {
	pub(super) fn class(&self) -> &str {
		match self {
			Self::Class(class) => class,
			Self::Record(record) => record.class(),
		}
	}
}

#[derive(Debug, Clone, Default)]
pub(super) struct Schema {
	pub(super) columns: Vec<Column>,
}

impl Schema {
	pub(super) fn index_of(&self, name: &str) -> Option<usize> {
		self.columns.iter().position(|column| column.name.eq_ignore_ascii_case(name))
	}

	pub(super) fn matching_indices<'a>(
		&'a self,
		name: &'a str,
	) -> impl Iterator<Item = usize> + 'a {
		self.columns.iter().enumerate().filter_map(move |(index, column)| {
			column.name.eq_ignore_ascii_case(name).then_some(index)
		})
	}

	pub(super) fn qualified_index(&self, qualifier: &str, name: &str) -> Option<usize> {
		self.columns.iter().position(|column| {
			column.qualifier.as_deref().is_some_and(|value| value.eq_ignore_ascii_case(qualifier))
				&& column.name.eq_ignore_ascii_case(name)
		})
	}
}

#[derive(Debug, Clone)]
pub(super) struct Column {
	pub(super) name: Box<str>,
	pub(super) qualifier: Option<Box<str>>,
	pub(super) ty: Type,
}

#[derive(Debug, Clone)]
pub(super) struct TypedExpr {
	pub(super) kind: TypedExprKind,
	pub(super) ty: Type,
	pub(super) span: TextSpan,
}

#[derive(Debug, Clone)]
pub(super) enum TypedExprKind {
	Constant(Value),
	Slot(u32),
	Column(usize),
	Array(Vec<TypedExpr>),
	Set(Vec<TypedExpr>),
	Object(Vec<(Box<str>, TypedExpr)>),
	Range {
		start: Option<Box<TypedExpr>>,
		end: Option<Box<TypedExpr>>,
		inclusive_end: bool,
	},
	Unary {
		op: crate::syn::UnaryOperator,
		expr: Box<TypedExpr>,
	},
	Binary {
		left: Box<TypedExpr>,
		op: crate::syn::BinaryOperator,
		right: Box<TypedExpr>,
	},
	Conditional {
		condition: Box<TypedExpr>,
		then_value: Box<TypedExpr>,
		else_value: Box<TypedExpr>,
	},
	Case {
		operand: Option<Box<TypedExpr>>,
		branches: Vec<(TypedExpr, TypedExpr)>,
		else_value: Option<Box<TypedExpr>>,
	},
	Field {
		base: Box<TypedExpr>,
		field: Box<str>,
		null_safe: bool,
	},
	CurrentField(Box<str>),
	Destructure {
		base: Box<TypedExpr>,
		fields: Vec<Box<str>>,
	},
	Index {
		base: Box<TypedExpr>,
		index: Box<TypedExpr>,
	},
	Filter {
		base: Box<TypedExpr>,
		predicate: Box<TypedExpr>,
	},
	Builtin {
		function: Builtin,
		arguments: Vec<TypedExpr>,
	},
	Aggregate {
		function: Aggregate,
		argument: Option<Box<TypedExpr>>,
		distinct: bool,
	},
	Subquery {
		select: Box<LogicalSelect>,
		collection: bool,
	},
	Cast {
		target: Type,
		expr: Box<TypedExpr>,
	},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Aggregate {
	Count,
	Sum,
	Average,
	Minimum,
	Maximum,
	ArrayGroup,
	VariancePopulation,
	StandardDeviationPopulation,
	VarianceSample,
	StandardDeviationSample,
}

/// Statically resolved built-ins in the executable Stage 12 profile.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Builtin {
	MathAbs,
	MathSqrt,
	MathPow,
	MathFloor,
	MathCeil,
	StringUpper,
	StringLower,
	StringLen,
	StringContains,
	StringSplit,
	StringConcat,
	TimeNow(DateTime),
	TimeFormat,
	TimeAdd,
	TimeDiff,
	ArrayLen,
	ArrayConcat,
	ArrayDistinct,
	ArrayFirst,
	ArrayFlatten,
	SetLen,
	SetUnion,
	SetIntersect,
	SetDiff,
	SetHas,
	TypeIs(Type),
	TypeTo(Type),
	MetaId,
	MetaClass,
	MetaType,
	OptionIsSome,
	OptionIsNone,
}
