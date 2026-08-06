use std::collections::BTreeMap;

use super::ir::{
	Column,
	LogicalNode,
	LogicalNodeKind,
	LogicalProgram,
	LogicalSelect,
	LogicalStatement,
	ScanTarget,
	Schema,
	TypedExpr,
	TypedExprKind,
};
use crate::{
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, TextSpan},
	syn::{
		BinaryOperator,
		Expr,
		ExprKind,
		Literal,
		Program,
		RecordKey,
		SelectItem,
		SelectTarget,
		StatementKind,
		TypeName,
		UnaryOperator,
	},
	values::{Number, Object, RecordId, RecordIdPart, Type, Value},
};

pub(super) trait QueryCatalog {
	fn schema(&self, table: &str) -> FerrosGrynnResult<Schema>;
}

pub(super) fn analyze(
	program: &Program,
	source: &str,
	catalog: &dyn QueryCatalog,
	variables: &Object,
) -> FerrosGrynnResult<LogicalProgram> {
	Analyzer::new(source, catalog, variables).program(program)
}

struct Analyzer<'a> {
	source: &'a str,
	catalog: &'a dyn QueryCatalog,
	slots: BTreeMap<Box<str>, Slot>,
	initial_slots: Vec<(Box<str>, u32)>,
}

#[derive(Debug, Clone)]
struct Slot {
	id: u32,
	ty: Type,
}

impl<'a> Analyzer<'a> {
	fn new(source: &'a str, catalog: &'a dyn QueryCatalog, variables: &Object) -> Self {
		let mut slots = BTreeMap::new();
		let mut initial_slots = Vec::new();
		for (index, (name, value)) in variables.iter().enumerate() {
			let id = index as u32;
			slots.insert(name.as_str().into(), Slot {
				id,
				ty: value.value_type(),
			});
			initial_slots.push((name.as_str().into(), id));
		}
		Self {
			source,
			catalog,
			slots,
			initial_slots,
		}
	}

	fn program(mut self, program: &Program) -> FerrosGrynnResult<LogicalProgram> {
		let mut statements = Vec::with_capacity(program.statements.len());
		for statement in &program.statements {
			match &statement.kind {
				StatementKind::Let(statement) => {
					let value = self.expr(&statement.value, None)?;
					if let Some(declared) = &statement.declared_type {
						let declared = self.type_name(declared)?;
						if !compatible(&declared, &value.ty) {
							return Err(self.semantic(
								&format!("LET value has type {}, expected {declared}.", value.ty),
								statement.value.span,
							));
						}
					}
					let slot = self.bind(&statement.name.value, value.ty.clone());
					statements.push(LogicalStatement::Let {
						slot,
						value,
					});
				}
				StatementKind::Return(statement) => {
					statements.push(LogicalStatement::Return {
						value: self.expr(&statement.value, None)?,
					});
				}
				StatementKind::Select(select) => {
					statements.push(LogicalStatement::Select(self.select(select, statement.span)?));
				}
			}
		}
		Ok(LogicalProgram {
			statements,
			initial_slots: self.initial_slots,
			slot_count: self.slots.len(),
		})
	}

	fn select(
		&self,
		statement: &crate::syn::SelectStatement,
		span: TextSpan,
	) -> FerrosGrynnResult<LogicalSelect> {
		let target = self.scan_target(&statement.target)?;
		let scan_schema = self.catalog.schema(target.table())?;
		let mut plan = LogicalSelect {
			nodes: Vec::new(),
			root: super::ir::NodeId(0),
			value: statement.value,
			only: statement.only,
		};
		let mut root = plan.push(LogicalNode {
			kind: LogicalNodeKind::Scan {
				target,
			},
			schema: scan_schema.clone(),
			span,
		});
		if let Some(filter) = &statement.filter {
			let predicate = self.expr(filter, Some(&scan_schema))?;
			root = plan.push(LogicalNode {
				kind: LogicalNodeKind::Filter {
					input: root,
					predicate,
				},
				schema: scan_schema.clone(),
				span: filter.span,
			});
		}

		let mut expressions = Vec::new();
		let mut columns = Vec::new();
		for (index, item) in statement.projection.iter().enumerate() {
			match item {
				SelectItem::Wildcard(item_span) => {
					if statement.value {
						return Err(self.semantic("SELECT VALUE does not accept `*`.", *item_span));
					}
					for (column_index, column) in scan_schema.columns.iter().enumerate() {
						expressions.push(TypedExpr {
							kind: TypedExprKind::Column(column_index),
							ty: column.ty.clone(),
							span: *item_span,
						});
						columns.push(column.clone());
					}
				}
				SelectItem::Expr {
					expr,
					alias,
				} => {
					let typed = self.expr(expr, Some(&scan_schema))?;
					let name: Box<str> = alias
						.as_ref()
						.map(|alias| alias.value.clone())
						.or_else(|| match &expr.kind {
							ExprKind::Name(name) => Some(name.value.clone()),
							_ => None,
						})
						.unwrap_or_else(|| format!("value_{index}").into_boxed_str());
					columns.push(Column {
						name,
						ty: typed.ty.clone(),
					});
					expressions.push(typed);
				}
			}
		}
		let project_schema = Schema {
			columns,
		};
		root = plan.push(LogicalNode {
			kind: LogicalNodeKind::Project {
				input: root,
				expressions,
			},
			schema: project_schema.clone(),
			span,
		});
		if statement.limit.is_some() || statement.start.is_some() {
			let limit = statement.limit.as_ref().map(|expr| self.expr(expr, None)).transpose()?;
			let start = statement.start.as_ref().map(|expr| self.expr(expr, None)).transpose()?;
			for expression in limit.iter().chain(start.iter()) {
				if !is_integer(&expression.ty) {
					return Err(
						self.semantic("LIMIT and START require an integer.", expression.span)
					);
				}
			}
			root = plan.push(LogicalNode {
				kind: LogicalNodeKind::Limit {
					input: root,
					limit,
					start,
				},
				schema: project_schema,
				span,
			});
		}
		plan.root = root;
		Ok(plan)
	}

	fn expr(&self, expr: &Expr, schema: Option<&Schema>) -> FerrosGrynnResult<TypedExpr> {
		let (kind, ty) = match &expr.kind {
			ExprKind::Literal(literal) => {
				let value = self.literal(literal, expr.span)?;
				let ty = value.value_type();
				(TypedExprKind::Constant(value), ty)
			}
			ExprKind::Parameter(name) => {
				let slot = self.slots.get(name.value.as_ref()).ok_or_else(|| {
					self.semantic(&format!("Unknown parameter `${}`.", name.value), name.span)
				})?;
				(TypedExprKind::Slot(slot.id), slot.ty.clone())
			}
			ExprKind::Name(name) => {
				let Some(schema) = schema else {
					return Err(
						self.semantic(&format!("Unknown field `{}`.", name.value), name.span)
					);
				};
				let index = schema.index_of(&name.value).ok_or_else(|| {
					self.semantic(&format!("Unknown field `{}`.", name.value), name.span)
				})?;
				(TypedExprKind::Column(index), schema.columns[index].ty.clone())
			}
			ExprKind::Array(values) => {
				let values = values
					.iter()
					.map(|value| self.expr(value, schema))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let item_type =
					values.iter().map(|value| value.ty.clone()).reduce(unify).unwrap_or(Type::Any);
				(TypedExprKind::Array(values), Type::Array(Box::new(item_type)))
			}
			ExprKind::Object(fields) => {
				let fields = fields
					.iter()
					.map(|field| {
						self.expr(&field.value, schema)
							.map(|value| (field.name.value.clone(), value))
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				(TypedExprKind::Object(fields), Type::Object)
			}
			ExprKind::Unary {
				op,
				expr: inner,
			} => {
				let inner = self.expr(inner, schema)?;
				let ty = match op {
					UnaryOperator::Not => Type::Bool,
					UnaryOperator::Assert => inner.ty.clone(),
					UnaryOperator::Negate | UnaryOperator::Positive if is_number(&inner.ty) => {
						inner.ty.clone()
					}
					UnaryOperator::Negate | UnaryOperator::Positive => {
						return Err(
							self.semantic("Unary numeric operator requires a number.", expr.span)
						);
					}
				};
				(
					TypedExprKind::Unary {
						op: *op,
						expr: Box::new(inner),
					},
					ty,
				)
			}
			ExprKind::Binary {
				left,
				op,
				right,
			} => {
				if matches!(op, BinaryOperator::Pipeline) {
					return Err(self.unsupported("expression pipeline", expr.span));
				}
				let left = self.expr(left, schema)?;
				let right = self.expr(right, schema)?;
				let ty = self.binary_type(*op, &left.ty, &right.ty, expr.span)?;
				(
					TypedExprKind::Binary {
						left: Box::new(left),
						op: *op,
						right: Box::new(right),
					},
					ty,
				)
			}
			ExprKind::Conditional {
				condition,
				then_value,
				else_value,
			} => {
				let condition = self.expr(condition, schema)?;
				let then_value = self.expr(then_value, schema)?;
				let else_value = self.expr(else_value, schema)?;
				let ty = unify(then_value.ty.clone(), else_value.ty.clone());
				(
					TypedExprKind::Conditional {
						condition: Box::new(condition),
						then_value: Box::new(then_value),
						else_value: Box::new(else_value),
					},
					ty,
				)
			}
			ExprKind::Field {
				base,
				field,
				null_safe,
			} => {
				let base = self.expr(base, schema)?;
				(
					TypedExprKind::Field {
						base: Box::new(base),
						field: field.value.clone(),
						null_safe: *null_safe,
					},
					Type::Any,
				)
			}
			ExprKind::Index {
				base,
				index,
			} => {
				let base = self.expr(base, schema)?;
				let ty = match &base.ty {
					Type::Array(item) => (**item).clone(),
					_ => Type::Any,
				};
				(
					TypedExprKind::Index {
						base: Box::new(base),
						index: Box::new(self.expr(index, schema)?),
					},
					ty,
				)
			}
			ExprKind::Call {
				..
			} => return Err(self.unsupported("function calls", expr.span)),
			ExprKind::Cast {
				target,
				expr: inner,
			} => {
				let target = self.type_name(target)?;
				(
					TypedExprKind::Cast {
						target: target.clone(),
						expr: Box::new(self.expr(inner, schema)?),
					},
					target,
				)
			}
		};
		Ok(TypedExpr {
			kind,
			ty,
			span: expr.span,
		})
	}

	fn binary_type(
		&self,
		op: BinaryOperator,
		left: &Type,
		right: &Type,
		span: TextSpan,
	) -> FerrosGrynnResult<Type> {
		match op {
			BinaryOperator::Power
			| BinaryOperator::Multiply
			| BinaryOperator::Divide
			| BinaryOperator::Remainder
			| BinaryOperator::Add
			| BinaryOperator::Subtract => {
				if is_number(left) && is_number(right) {
					Ok(unify(left.clone(), right.clone()))
				} else {
					Err(self.semantic("Arithmetic operators require numbers.", span))
				}
			}
			BinaryOperator::Less
			| BinaryOperator::LessOrEqual
			| BinaryOperator::Greater
			| BinaryOperator::GreaterOrEqual
			| BinaryOperator::Equal
			| BinaryOperator::NotEqual
			| BinaryOperator::Is
			| BinaryOperator::NotIs
			| BinaryOperator::In
			| BinaryOperator::NotIn
			| BinaryOperator::And
			| BinaryOperator::Or => Ok(Type::Bool),
			BinaryOperator::Coalesce | BinaryOperator::Elvis => {
				Ok(unify(left.clone(), right.clone()))
			}
			BinaryOperator::Pipeline => Err(self.unsupported("expression pipeline", span)),
		}
	}

	fn literal(&self, literal: &Literal, span: TextSpan) -> FerrosGrynnResult<Value> {
		match literal {
			Literal::None => Ok(Value::None),
			Literal::Null => Ok(Value::Null),
			Literal::Bool(value) => Ok(Value::Bool(*value)),
			Literal::String(value) => Ok(Value::from(value.as_ref())),
			Literal::Number(value) => {
				value.replace('_', "").parse::<Number>().map(Value::Number).map_err(|error| {
					self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), span)
				})
			}
		}
	}

	fn scan_target(&self, target: &SelectTarget) -> FerrosGrynnResult<ScanTarget> {
		match target {
			SelectTarget::Table(table) => Ok(ScanTarget::Table(table.value.clone())),
			SelectTarget::Record {
				table,
				key,
				span,
			} => {
				let key = match key {
					RecordKey::Integer(value, _) => value
						.parse::<i64>()
						.map(RecordIdPart::Int)
						.map_err(|_| self.semantic("Record integer ID is out of range.", *span))?,
					RecordKey::String(value, _) => RecordIdPart::String(value.as_ref().into()),
				};
				RecordId::new(table.value.as_ref(), key).map(ScanTarget::Record).map_err(|error| {
					self.locate(error.with_phase(ExecutionPhase::SemanticAnalysis), *span)
				})
			}
		}
	}

	fn type_name(&self, name: &TypeName) -> FerrosGrynnResult<Type> {
		let ty = match name.value.to_ascii_lowercase().as_str() {
			"any" => Type::Any,
			"none" => Type::None,
			"null" => Type::Null,
			"bool" => Type::Bool,
			"number" => Type::Number,
			"int" => Type::Int,
			"float" => Type::Float,
			"decimal" => Type::Decimal,
			"string" => Type::String,
			"object" => Type::Object,
			"record" => Type::Record,
			_ => return Err(self.semantic(&format!("Unknown type `{}`.", name.value), name.span)),
		};
		Ok(ty)
	}

	fn bind(&mut self, name: &str, ty: Type) -> u32 {
		if let Some(slot) = self.slots.get_mut(name) {
			slot.ty = ty;
			return slot.id;
		}
		let id = self.slots.len() as u32;
		self.slots.insert(name.into(), Slot {
			id,
			ty,
		});
		id
	}

	fn semantic(&self, message: &str, span: TextSpan) -> FerrosGrynnError {
		self.locate(
			FerrosGrynnError::invalid_argument(message)
				.with_phase(ExecutionPhase::SemanticAnalysis),
			span,
		)
	}

	fn unsupported(&self, feature: &str, span: TextSpan) -> FerrosGrynnError {
		self.locate(
			FerrosGrynnError::query_feature_not_supported(feature)
				.with_phase(ExecutionPhase::SemanticAnalysis),
			span,
		)
	}

	fn locate(&self, error: FerrosGrynnError, span: TextSpan) -> FerrosGrynnError {
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}
}

fn is_number(ty: &Type) -> bool {
	matches!(ty, Type::Any | Type::Number | Type::Int | Type::Float | Type::Decimal)
}

fn is_integer(ty: &Type) -> bool {
	matches!(ty, Type::Any | Type::Number | Type::Int)
}

fn compatible(expected: &Type, actual: &Type) -> bool {
	expected == actual
		|| matches!(expected, Type::Any)
		|| matches!(expected, Type::Number) && is_number(actual)
		|| matches!(expected, Type::Option(inner) if matches!(actual, Type::None | Type::Null) || compatible(inner, actual))
		|| matches!(expected, Type::Union(types) if types.iter().any(|ty| compatible(ty, actual)))
}

fn unify(left: Type, right: Type) -> Type {
	if left == right {
		return left;
	}
	if matches!(left, Type::Any) || matches!(right, Type::Any) {
		return Type::Any;
	}
	if is_number(&left) && is_number(&right) {
		return Type::Number;
	}
	if matches!(left, Type::None | Type::Null) {
		return Type::Option(Box::new(right));
	}
	if matches!(right, Type::None | Type::Null) {
		return Type::Option(Box::new(left));
	}
	let mut values = match left {
		Type::Union(values) => values,
		value => vec![value],
	};
	match right {
		Type::Union(right) => values.extend(right),
		value => values.push(value),
	}
	values.sort();
	values.dedup();
	Type::Union(values)
}
