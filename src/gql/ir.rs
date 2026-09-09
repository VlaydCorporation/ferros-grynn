use crate::{
	datastore::{TableIndexKind, TableKind, TableSchemaMode},
	gql_status::TextSpan,
	syn::DefinitionMode,
	values::{GraphKind, GraphRef, RecordId, Type, Value},
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
	Schema(LogicalSchemaStatement),
	Dml(LogicalDmlStatement),
	Let {
		slot: u32,
		value: TypedExpr,
	},
	Return {
		value: TypedExpr,
	},
	/// Executes a read-only graph pattern match.
	Match(LogicalMatch),
	Select(LogicalSelect),
}

/// One analyzed data-mutation statement.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlStatement {
	pub(super) kind: LogicalDmlKind,
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

/// One table-specific mutation plan. Expressions are typed against this
/// target because a statement may name tables with different schemas.
#[derive(Debug, Clone)]
pub(super) struct LogicalDmlTarget {
	pub(super) table: Box<str>,
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
	Table,
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
	},
	DefineTable {
		mode: DefinitionMode,
		name: Box<str>,
		kind: TableKind,
		schema_mode: TableSchemaMode,
	},
	DefineField {
		mode: DefinitionMode,
		table: Box<str>,
		name: Box<str>,
		value_type: Type,
	},
	DefineIndex {
		mode: DefinitionMode,
		table: Box<str>,
		name: Box<str>,
		kind: TableIndexKind,
		fields: Vec<Box<str>>,
	},
	RemoveGraph {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveTable {
		if_exists: bool,
		name: Box<str>,
	},
	RemoveField {
		if_exists: bool,
		table: Box<str>,
		name: Box<str>,
	},
	RemoveIndex {
		if_exists: bool,
		table: Box<str>,
		name: Box<str>,
	},
	AlterTableMode {
		if_exists: bool,
		name: Box<str>,
		mode: TableSchemaMode,
	},
	AlterFieldType {
		if_exists: bool,
		table: Box<str>,
		name: Box<str>,
		value_type: Type,
	},
	PrepareIndexRemoval {
		table: Box<str>,
		name: Box<str>,
	},
	InfoDatabase,
	InfoTable {
		name: Box<str>,
	},
	InfoIndex {
		table: Box<str>,
		name: Box<str>,
	},
}

impl LogicalSchemaStatement {
	pub(super) const fn statement_kind(&self) -> crate::gql::QueryStatementKind {
		match self {
			Self::DefineGraph {
				..
			}
			| Self::DefineTable {
				..
			}
			| Self::DefineField {
				..
			}
			| Self::DefineIndex {
				..
			} => crate::gql::QueryStatementKind::Define,
			Self::RemoveGraph {
				..
			}
			| Self::RemoveTable {
				..
			}
			| Self::RemoveField {
				..
			}
			| Self::RemoveIndex {
				..
			} => crate::gql::QueryStatementKind::Remove,
			Self::AlterTableMode {
				..
			}
			| Self::AlterFieldType {
				..
			}
			| Self::PrepareIndexRemoval {
				..
			} => crate::gql::QueryStatementKind::Alter,
			Self::InfoDatabase
			| Self::InfoTable {
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
	/// Target endpoint binding.
	pub(super) target: MatchBinding,
	/// Accepted connector labels.
	pub(super) edge_labels: Vec<Box<str>>,
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

#[derive(Debug, Clone)]
/// Alias and label constraints for one MATCH atom.
pub(super) struct MatchBinding {
	/// Required atom domain.
	pub(super) domain: EndpointDomain,
	/// Optional binding-column index.
	pub(super) column: Option<usize>,
	/// Alternative accepted labels.
	pub(super) labels: Vec<Box<str>>,
}

#[derive(Debug, Clone)]
pub(super) struct LogicalSelect {
	pub(super) nodes: Vec<LogicalNode>,
	pub(super) root: NodeId,
	pub(super) value: bool,
	pub(super) only: bool,
}

impl LogicalSelect {
	pub(super) fn push(&mut self, node: LogicalNode) -> NodeId {
		let id = NodeId(self.nodes.len() as u32);
		self.nodes.push(node);
		id
	}

	pub(super) fn node(&self, id: NodeId) -> &LogicalNode {
		&self.nodes[id.0 as usize]
	}
}

#[derive(Debug, Clone)]
pub(super) struct LogicalNode {
	pub(super) kind: LogicalNodeKind,
	pub(super) schema: Schema,
	pub(super) span: TextSpan,
}

#[derive(Debug, Clone)]
pub(super) enum LogicalNodeKind {
	Scan {
		target: ScanTarget,
	},
	Filter {
		input: NodeId,
		predicate: TypedExpr,
	},
	Project {
		input: NodeId,
		expressions: Vec<TypedExpr>,
	},
	Limit {
		input: NodeId,
		limit: Option<TypedExpr>,
		start: Option<TypedExpr>,
	},
}

#[derive(Debug, Clone)]
pub(super) enum ScanTarget {
	Table(Box<str>),
	Record(RecordId),
}

impl ScanTarget {
	pub(super) fn table(&self) -> &str {
		match self {
			Self::Table(table) => table,
			Self::Record(record) => record.table(),
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
}

#[derive(Debug, Clone)]
pub(super) struct Column {
	pub(super) name: Box<str>,
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
	Object(Vec<(Box<str>, TypedExpr)>),
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
	Field {
		base: Box<TypedExpr>,
		field: Box<str>,
		null_safe: bool,
	},
	Index {
		base: Box<TypedExpr>,
		index: Box<TypedExpr>,
	},
	Cast {
		target: Type,
		expr: Box<TypedExpr>,
	},
}
