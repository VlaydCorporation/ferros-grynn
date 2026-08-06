use crate::{
	gql_status::TextSpan,
	values::{RecordId, Type, Value},
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
	Let {
		slot: u32,
		value: TypedExpr,
	},
	Return {
		value: TypedExpr,
	},
	Select(LogicalSelect),
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
