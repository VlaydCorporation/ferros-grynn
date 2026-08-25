#![allow(
	clippy::pattern_type_mismatch,
	reason = "the executor intentionally uses match ergonomics for borrowed IR and runtime values"
)]

use std::{sync::Arc, time::Instant};

use rust_decimal::Decimal;

use super::{
	QueryResult,
	QueryStatementKind,
	analysis::{QueryCatalog, analyze},
	ir::{
		Column,
		LogicalNodeKind,
		LogicalProgram,
		LogicalSelect,
		LogicalStatement,
		NodeId,
		ScanTarget,
		Schema,
		TypedExpr,
		TypedExprKind,
	},
};
use crate::{
	datastore::{PredicateEvaluator, QuerySnapshot, QueryStorageRow, TableSchema, Transaction},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, Outcome},
	math::{add_exact, divide_signed_exact, multiply_exact, subtract_exact},
	syn::{BinaryOperator, ParserSettings, Program, UnaryOperator},
	values::{Array, Number, Object, RecordId, Type, Value},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Snapshot(pub(super) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
	pub(super) values: Vec<Value>,
}

impl Row {
	pub(super) const fn new(values: Vec<Value>) -> Self {
		Self {
			values,
		}
	}
}

pub(super) trait RowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>>;
}

pub(super) trait QueryRead {
	fn snapshot(&self) -> FerrosGrynnResult<Snapshot>;

	fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog;

	fn scan<'a>(
		&'a self,
		snapshot: Snapshot,
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>>;

	fn get(&self, snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>>;
}

pub fn execute_transaction(
	transaction: &Transaction,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = transaction.query_snapshot()?;
	execute_text(
		&TransactionRead {
			transaction,
			snapshot,
		},
		source,
		variables,
		settings,
	)
}

struct TransactionRead<'a> {
	transaction: &'a Transaction,
	snapshot: QuerySnapshot,
}

impl QueryCatalog for TransactionRead<'_> {
	fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_table(&self.snapshot, table).map(|schema| query_schema(&schema))
	}
}

impl QueryRead for TransactionRead<'_> {
	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(self.snapshot.commit_ts()))
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn scan<'a>(
		&'a self,
		_snapshot: Snapshot,
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let rows = self.transaction.query_rows(&self.snapshot, table)?;
		let evaluator = Arc::new(QueryPredicate {
			predicate: predicate.cloned(),
			slots: slots.to_vec(),
			source: source.to_owned(),
		});
		self.transaction.register_query_predicate(&self.snapshot, table, &rows, evaluator)?;
		Ok(Box::new(StorageCursor {
			rows: rows.into_iter(),
		}))
	}

	fn get(&self, _snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
		let row = self.transaction.query_point(&self.snapshot, record)?;
		self.transaction.register_query_point(
			&self.snapshot,
			record.table(),
			record,
			row.as_ref(),
		)?;
		Ok(row.map(|row| Row::new(row.values)))
	}
}

fn query_schema(schema: &TableSchema) -> Schema {
	let mut columns = Vec::with_capacity(schema.fields().len() + 1);
	columns.push(Column {
		name: "id".into(),
		ty: Type::Record,
	});
	columns.extend(schema.fields().map(|field| Column {
		name: field.name().as_str().into(),
		ty: field.value_type().clone(),
	}));
	Schema {
		columns,
	}
}

struct QueryPredicate {
	predicate: Option<TypedExpr>,
	slots: Vec<Value>,
	source: String,
}

impl PredicateEvaluator for QueryPredicate {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		self.predicate.as_ref().map_or(Ok(true), |predicate| {
			eval(predicate, Some(&Row::new(row.values.clone())), &self.slots, &self.source)
				.map(|value| value.is_truthy())
		})
	}
}

struct StorageCursor {
	rows: std::vec::IntoIter<QueryStorageRow>,
}

impl RowCursor for StorageCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.rows.next().map(|row| Row::new(row.values)))
	}
}

pub(super) fn execute_text(
	read: &dyn QueryRead,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let program = crate::gql::parse(source, settings)?;
	execute_program(read, source, &program, variables)
}

fn execute_program(
	read: &dyn QueryRead,
	source: &str,
	program: &Program,
	variables: &Object,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = read.snapshot()?;
	let logical = analyze(program, source, read.catalog(snapshot), variables)?;
	let mut slots = initialize_slots(&logical, variables);
	let mut results = Vec::with_capacity(logical.statements.len());
	for statement in &logical.statements {
		let start = Instant::now();
		let (kind, result) = match statement {
			LogicalStatement::Let {
				slot,
				value,
			} => {
				let result = eval(value, None, &slots, source).map(|value| {
					slots[*slot as usize] = value;
					Outcome::success(Value::None)
				});
				(QueryStatementKind::Let, result)
			}
			LogicalStatement::Return {
				value,
			} => (
				QueryStatementKind::Return,
				eval(value, None, &slots, source).map(Outcome::success),
			),
			LogicalStatement::Select(select) => (
				QueryStatementKind::Select,
				execute_select(read, snapshot, select, &slots, source).map(Outcome::success),
			),
		};
		let failed = result.is_err();
		results.push(QueryResult {
			kind,
			elapsed: start.elapsed(),
			result,
		});
		if failed {
			break;
		}
	}
	Ok(results)
}

fn initialize_slots(program: &LogicalProgram, variables: &Object) -> Vec<Value> {
	let mut slots = vec![Value::None; program.slot_count];
	for (name, slot) in &program.initial_slots {
		if let Some(value) = variables.get(name) {
			slots[*slot as usize] = value.clone();
		}
	}
	slots
}

fn execute_select(
	read: &dyn QueryRead,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	let predicate = plan.nodes.iter().find_map(|node| match &node.kind {
		LogicalNodeKind::Filter {
			predicate,
			..
		} => Some(predicate),
		_ => None,
	});
	let mut cursor = build_cursor(read, snapshot, plan, plan.root, slots, source, predicate)?;
	let schema = &plan.node(plan.root).schema;
	let mut result = Array::new(Vec::new());
	while let Some(row) = cursor.next().map_err(execution_phase)? {
		let value = if plan.value {
			row.values.into_iter().next().unwrap_or(Value::None)
		} else {
			let object = schema
				.columns
				.iter()
				.zip(row.values)
				.map(|(column, value)| (column.name.as_ref(), value))
				.collect::<Object>();
			Value::Object(object)
		};
		result.push(value);
		if plan.only && result.len() > 1 {
			return Err(FerrosGrynnError::invalid_argument(
				"SELECT ONLY returned more than one row.",
			)
			.with_phase(ExecutionPhase::Execution));
		}
	}
	Ok(Value::Array(result))
}

fn build_cursor<'a>(
	read: &'a dyn QueryRead,
	snapshot: Snapshot,
	plan: &'a LogicalSelect,
	node_id: NodeId,
	slots: &'a [Value],
	source: &'a str,
	scan_predicate: Option<&'a TypedExpr>,
) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
	let node = plan.node(node_id);
	match &node.kind {
		LogicalNodeKind::Scan {
			target,
		} => {
			let inner: Box<dyn RowCursor + 'a> = match target {
				ScanTarget::Table(table) => {
					read.scan(snapshot, table, scan_predicate, slots, source)?
				}
				ScanTarget::Record(record) => Box::new(OneRowCursor {
					row: read.get(snapshot, record)?,
				}),
			};
			Ok(Box::new(ScanCursor {
				inner,
			}))
		}
		LogicalNodeKind::Filter {
			input,
			predicate,
		} => Ok(Box::new(FilterCursor {
			input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
			predicate,
			slots,
			source,
		})),
		LogicalNodeKind::Project {
			input,
			expressions,
		} => Ok(Box::new(ProjectCursor {
			input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
			expressions,
			slots,
			source,
		})),
		LogicalNodeKind::Limit {
			input,
			limit,
			start,
		} => {
			let limit = limit.as_ref().map(|value| eval_usize(value, slots, source)).transpose()?;
			let start = start
				.as_ref()
				.map(|value| eval_usize(value, slots, source))
				.transpose()?
				.unwrap_or(0);
			Ok(Box::new(LimitCursor {
				input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
				remaining: limit,
				skip: start,
			}))
		}
	}
}

struct ScanCursor<'a> {
	inner: Box<dyn RowCursor + 'a>,
}

impl RowCursor for ScanCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		self.inner.next()
	}
}

struct OneRowCursor {
	row: Option<Row>,
}

impl RowCursor for OneRowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.row.take())
	}
}

struct FilterCursor<'a> {
	input: Box<dyn RowCursor + 'a>,
	predicate: &'a TypedExpr,
	slots: &'a [Value],
	source: &'a str,
}

impl RowCursor for FilterCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		while let Some(row) = self.input.next()? {
			if eval(self.predicate, Some(&row), self.slots, self.source)?.is_truthy() {
				return Ok(Some(row));
			}
		}
		Ok(None)
	}
}

struct ProjectCursor<'a> {
	input: Box<dyn RowCursor + 'a>,
	expressions: &'a [TypedExpr],
	slots: &'a [Value],
	source: &'a str,
}

impl RowCursor for ProjectCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.input.next()? else {
			return Ok(None);
		};
		let values = self
			.expressions
			.iter()
			.map(|expression| eval(expression, Some(&row), self.slots, self.source))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Some(Row::new(values)))
	}
}

struct LimitCursor<'a> {
	input: Box<dyn RowCursor + 'a>,
	remaining: Option<usize>,
	skip: usize,
}

impl RowCursor for LimitCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		while self.skip > 0 {
			if self.input.next()?.is_none() {
				return Ok(None);
			}
			self.skip -= 1;
		}
		if self.remaining == Some(0) {
			return Ok(None);
		}
		let row = self.input.next()?;
		if row.is_some()
			&& let Some(remaining) = &mut self.remaining
		{
			*remaining -= 1;
		}
		Ok(row)
	}
}

fn eval(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	let result = match &expr.kind {
		TypedExprKind::Constant(value) => Ok(value.clone()),
		TypedExprKind::Slot(slot) => Ok(slots[*slot as usize].clone()),
		TypedExprKind::Column(column) => {
			row.and_then(|row| row.values.get(*column)).cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Column is missing from the input row.")
			})
		}
		TypedExprKind::Array(values) => values
			.iter()
			.map(|value| eval(value, row, slots, source))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		TypedExprKind::Object(fields) => fields
			.iter()
			.map(|(name, value)| {
				eval(value, row, slots, source).map(|value| (name.as_ref(), value))
			})
			.collect::<FerrosGrynnResult<Object>>()
			.map(Value::Object),
		TypedExprKind::Unary {
			op,
			expr,
		} => unary(*op, eval(expr, row, slots, source)?),
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			let left = eval(left, row, slots, source)?;
			match op {
				BinaryOperator::And if !left.is_truthy() => Ok(Value::Bool(false)),
				BinaryOperator::Or if left.is_truthy() => Ok(Value::Bool(true)),
				BinaryOperator::Coalesce if !matches!(left, Value::None | Value::Null) => Ok(left),
				BinaryOperator::Elvis if left.is_truthy() => Ok(left),
				_ => binary(*op, left, eval(right, row, slots, source)?),
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			if eval(condition, row, slots, source)?.is_truthy() {
				eval(then_value, row, slots, source)
			} else {
				eval(else_value, row, slots, source)
			}
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			let base = eval(base, row, slots, source)?;
			match base {
				Value::Object(object) => Ok(object.get(field).cloned().unwrap_or(Value::None)),
				Value::None | Value::Null if *null_safe => Ok(Value::None),
				Value::None | Value::Null => Ok(Value::None),
				value => Err(FerrosGrynnError::invalid_argument(format!(
					"Cannot access field `{field}` on {}.",
					value.kind()
				))),
			}
		}
		TypedExprKind::Index {
			base,
			index,
		} => index_value(eval(base, row, slots, source)?, eval(index, row, slots, source)?),
		TypedExprKind::Cast {
			target,
			expr,
		} => cast(eval(expr, row, slots, source)?, target),
	};
	result.map_err(|error| locate_execution(error, expr, source))
}

fn unary(operator: UnaryOperator, value: Value) -> FerrosGrynnResult<Value> {
	match operator {
		UnaryOperator::Not => Ok(Value::Bool(!value.is_truthy())),
		UnaryOperator::Assert if matches!(value, Value::None | Value::Null) => {
			Err(FerrosGrynnError::invalid_argument("Assertion operator received none or null."))
		}
		UnaryOperator::Assert | UnaryOperator::Positive => Ok(value),
		UnaryOperator::Negate => match value {
			Value::Number(Number::Int(value)) => value
				.checked_neg()
				.map(Value::from)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("unary -")),
			Value::Number(Number::Float(value)) => Ok(Value::from(-value)),
			Value::Number(Number::Decimal(value)) => Decimal::ZERO
				.checked_sub(value)
				.map(|value| Value::Number(Number::Decimal(value)))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("unary -")),
			value => Err(FerrosGrynnError::invalid_argument(format!(
				"Unary minus requires a number, found {}.",
				value.kind()
			))),
		},
	}
}

fn binary(operator: BinaryOperator, left: Value, right: Value) -> FerrosGrynnResult<Value> {
	match operator {
		BinaryOperator::Power
		| BinaryOperator::Multiply
		| BinaryOperator::Divide
		| BinaryOperator::Remainder
		| BinaryOperator::Add
		| BinaryOperator::Subtract => arithmetic(operator, left, right),
		BinaryOperator::Less => Ok(Value::Bool(left < right)),
		BinaryOperator::LessOrEqual => Ok(Value::Bool(left <= right)),
		BinaryOperator::Greater => Ok(Value::Bool(left > right)),
		BinaryOperator::GreaterOrEqual => Ok(Value::Bool(left >= right)),
		BinaryOperator::Equal | BinaryOperator::Is => Ok(Value::Bool(left == right)),
		BinaryOperator::NotEqual | BinaryOperator::NotIs => Ok(Value::Bool(left != right)),
		BinaryOperator::In | BinaryOperator::NotIn => {
			let contains =
				matches!(&right, Value::Array(values) if values.iter().any(|value| value == &left));
			Ok(Value::Bool(if matches!(operator, BinaryOperator::NotIn) {
				!contains
			} else {
				contains
			}))
		}
		BinaryOperator::And => Ok(Value::Bool(left.is_truthy() && right.is_truthy())),
		BinaryOperator::Or => Ok(Value::Bool(left.is_truthy() || right.is_truthy())),
		BinaryOperator::Coalesce => Ok(if matches!(left, Value::None | Value::Null) {
			right
		} else {
			left
		}),
		BinaryOperator::Elvis => Ok(if left.is_truthy() {
			left
		} else {
			right
		}),
		BinaryOperator::Pipeline => {
			Err(FerrosGrynnError::query_feature_not_supported("expression pipeline"))
		}
	}
}

fn arithmetic(operator: BinaryOperator, left: Value, right: Value) -> FerrosGrynnResult<Value> {
	let (Value::Number(left), Value::Number(right)) = (left, right) else {
		return Err(FerrosGrynnError::invalid_argument("Arithmetic operators require numbers."));
	};
	match (left, right) {
		(Number::Int(left), Number::Int(right)) => integer_arithmetic(operator, left, right),
		(Number::Float(left), Number::Float(right)) => float_arithmetic(operator, left, right),
		(Number::Float(left), Number::Int(right)) => float_arithmetic(operator, left, right as f64),
		(Number::Int(left), Number::Float(right)) => float_arithmetic(operator, left as f64, right),
		(Number::Decimal(left), Number::Decimal(right)) => {
			decimal_arithmetic(operator, left, right)
		}
		(Number::Decimal(left), Number::Int(right)) => {
			decimal_arithmetic(operator, left, Decimal::from(right))
		}
		(Number::Int(left), Number::Decimal(right)) => {
			decimal_arithmetic(operator, Decimal::from(left), right)
		}
		(Number::Float(_), Number::Decimal(_)) | (Number::Decimal(_), Number::Float(_)) => {
			Err(FerrosGrynnError::invalid_argument(
				"Float and decimal arithmetic requires an explicit cast.",
			))
		}
	}
}

fn integer_arithmetic(operator: BinaryOperator, left: i64, right: i64) -> FerrosGrynnResult<Value> {
	let value = match operator {
		BinaryOperator::Add => add_exact(&left, &right)?,
		BinaryOperator::Subtract => subtract_exact(&left, &right)?,
		BinaryOperator::Multiply => multiply_exact(&left, &right)?,
		BinaryOperator::Divide => divide_signed_exact(&left, &right)?,
		BinaryOperator::Remainder if right == 0 => return Err(FerrosGrynnError::division_by_zero()),
		BinaryOperator::Remainder => {
			left.checked_rem(right).ok_or_else(|| FerrosGrynnError::operation_overflow("%"))?
		}
		BinaryOperator::Power => {
			let exponent = u32::try_from(right).map_err(|_conversion_error| {
				FerrosGrynnError::invalid_argument("Integer exponent must be non-negative.")
			})?;
			left.checked_pow(exponent).ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	};
	Ok(Value::from(value))
}

fn float_arithmetic(operator: BinaryOperator, left: f64, right: f64) -> FerrosGrynnResult<Value> {
	if matches!(operator, BinaryOperator::Divide | BinaryOperator::Remainder) && right == 0.0 {
		return Err(FerrosGrynnError::division_by_zero());
	}
	let value = match operator {
		BinaryOperator::Add => left + right,
		BinaryOperator::Subtract => left - right,
		BinaryOperator::Multiply => left * right,
		BinaryOperator::Divide => left / right,
		BinaryOperator::Remainder => left % right,
		BinaryOperator::Power => left.powf(right),
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	};
	if value.is_infinite() {
		return Err(FerrosGrynnError::operation_overflow(operator_name(operator)));
	}
	Ok(Value::from(value))
}

fn decimal_arithmetic(
	operator: BinaryOperator,
	left: Decimal,
	right: Decimal,
) -> FerrosGrynnResult<Value> {
	if matches!(operator, BinaryOperator::Divide | BinaryOperator::Remainder) && right.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}
	let value = match operator {
		BinaryOperator::Add => left.checked_add(right),
		BinaryOperator::Subtract => left.checked_sub(right),
		BinaryOperator::Multiply => left.checked_mul(right),
		BinaryOperator::Divide => left.checked_div(right),
		BinaryOperator::Remainder => left.checked_rem(right),
		BinaryOperator::Power => {
			return Err(FerrosGrynnError::query_feature_not_supported("decimal exponentiation"));
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	}
	.ok_or_else(|| FerrosGrynnError::operation_overflow(operator_name(operator)))?;
	Ok(Value::Number(Number::Decimal(value)))
}

const fn operator_name(operator: BinaryOperator) -> &'static str {
	match operator {
		BinaryOperator::Power => "**",
		BinaryOperator::Multiply => "*",
		BinaryOperator::Divide => "/",
		BinaryOperator::Remainder => "%",
		BinaryOperator::Add => "+",
		BinaryOperator::Subtract => "-",
		_ => "?",
	}
}

fn index_value(base: Value, index: Value) -> FerrosGrynnResult<Value> {
	match (base, index) {
		(Value::Array(values), Value::Number(number)) => usize::try_from(number)
			.ok()
			.and_then(|index| values.get(index).cloned())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("Array index is out of bounds.")),
		(Value::Object(values), Value::String(key)) => {
			Ok(values.get(key.as_str()).cloned().unwrap_or(Value::None))
		}
		(value, _) => {
			Err(FerrosGrynnError::invalid_argument(format!("Cannot index {}.", value.kind())))
		}
	}
}

fn cast(value: Value, target: &Type) -> FerrosGrynnResult<Value> {
	match target {
		Type::Any => Ok(value),
		Type::Bool => Ok(Value::Bool(value.is_truthy())),
		Type::String => Ok(Value::from(value.to_string())),
		Type::Number => match value {
			Value::Number(_) => Ok(value),
			Value::String(value) => value.parse::<Number>().map(Value::Number),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "number")),
		},
		Type::Int => match value {
			Value::Number(value) => i64::try_from(value).map(Value::from),
			Value::String(value) => value
				.parse::<i64>()
				.map(Value::from)
				.map_err(|_parse_error| FerrosGrynnError::conversion_error(value.as_str(), "int")),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "int")),
		},
		Type::Float => match value {
			Value::Number(value) => f64::try_from(value).map(Value::from),
			Value::String(value) => value.parse::<f64>().map(Value::from).map_err(|_parse_error| {
				FerrosGrynnError::conversion_error(value.as_str(), "float")
			}),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "float")),
		},
		Type::Decimal => match value {
			Value::Number(value) => {
				Decimal::try_from(value).map(|value| Value::Number(Number::Decimal(value)))
			}
			Value::String(value) => {
				value.parse::<Decimal>().map(|value| Value::Number(Number::Decimal(value))).map_err(
					|_parse_error| FerrosGrynnError::conversion_error(value.as_str(), "decimal"),
				)
			}
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "decimal")),
		},
		Type::None if matches!(value, Value::None) => Ok(value),
		Type::Null if matches!(value, Value::Null) => Ok(value),
		_ => Err(FerrosGrynnError::conversion_error(value.to_string(), target.to_string())),
	}
}

fn eval_usize(expr: &TypedExpr, slots: &[Value], source: &str) -> FerrosGrynnResult<usize> {
	let value = eval(expr, None, slots, source)?;
	let Value::Number(number) = value else {
		return Err(locate_execution(
			FerrosGrynnError::invalid_argument("Expected a non-negative integer."),
			expr,
			source,
		));
	};
	usize::try_from(number).map_err(|error| locate_execution(error, expr, source))
}

fn locate_execution(error: FerrosGrynnError, expr: &TypedExpr, source: &str) -> FerrosGrynnError {
	let error = error.with_phase(ExecutionPhase::Execution);
	Location::from_source(None, source, expr.span)
		.map_or(error.clone(), |location| error.with_location(location))
}

fn execution_phase(error: FerrosGrynnError) -> FerrosGrynnError {
	error.with_phase(ExecutionPhase::Execution)
}

#[cfg(test)]
struct VecCursor {
	rows: std::vec::IntoIter<Row>,
}

#[cfg(test)]
impl VecCursor {
	fn new(rows: Vec<Row>) -> Self {
		Self {
			rows: rows.into_iter(),
		}
	}
}

#[cfg(test)]
impl RowCursor for VecCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.rows.next())
	}
}

#[cfg(test)]
mod tests {
	use std::{error::Error, io};

	use super::*;
	use crate::{
		gql::ir::{Column, Schema},
		gql_status::ExecutionPhase,
		values::RecordIdPart,
	};

	struct TestStore {
		schema: Schema,
		rows: Vec<(RecordId, Row)>,
		fail_scan: bool,
	}

	impl TestStore {
		fn people() -> Self {
			Self {
				schema: Schema {
					columns: vec![
						Column {
							name: "name".into(),
							ty: Type::String,
						},
						Column {
							name: "age".into(),
							ty: Type::Int,
						},
					],
				},
				rows: vec![
					person("alice", "Alice", 30),
					person("bob", "Bob", 20),
					person("carol", "Carol", 17),
				],
				fail_scan: false,
			}
		}
	}

	impl QueryCatalog for TestStore {
		fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
			if table.eq_ignore_ascii_case("person") {
				Ok(self.schema.clone())
			} else {
				Err(FerrosGrynnError::invalid_argument(format!("Unknown table `{table}`.")))
			}
		}
	}

	impl QueryRead for TestStore {
		fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
			Ok(Snapshot(7))
		}

		fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog {
			assert_eq!(snapshot, Snapshot(7));
			self
		}

		fn scan<'a>(
			&'a self,
			snapshot: Snapshot,
			table: &str,
			_predicate: Option<&TypedExpr>,
			_slots: &[Value],
			_source: &str,
		) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
			assert_eq!(snapshot, Snapshot(7));
			assert_eq!(table, "person");
			if self.fail_scan {
				return Err(FerrosGrynnError::unexpected_external_failure("test-store", "scan")
					.with_technical_source(io::Error::other("secret storage detail")));
			}
			Ok(Box::new(VecCursor::new(self.rows.iter().map(|(_, row)| row.clone()).collect())))
		}

		fn get(&self, snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
			assert_eq!(snapshot, Snapshot(7));
			Ok(self
				.rows
				.iter()
				.find(|(candidate, _)| candidate == record)
				.map(|(_, row)| row.clone()))
		}
	}

	fn person(id: &str, name: &str, age: i64) -> (RecordId, Row) {
		(
			RecordId::new("person", RecordIdPart::String(id.into())).expect("valid record ID"),
			Row::new(vec![Value::from(name), Value::from(age)]),
		)
	}

	fn run(store: &TestStore, source: &str) -> FerrosGrynnResult<Vec<QueryResult>> {
		execute_text(store, source, &Object::new(), ParserSettings::default())
	}

	#[test]
	fn executes_return_with_pratt_precedence() {
		let results = run(&TestStore::people(), "RETURN 1 + 2 * 3").expect("query succeeds");
		assert_eq!(
			results[0].result.as_ref().expect("statement succeeds").value(),
			&Value::from(7_i64)
		);
	}

	#[test]
	fn binds_let_filters_projects_and_limits() {
		let results = run(
			&TestStore::people(),
			"LET $min = 18; SELECT name FROM person WHERE age >= $min LIMIT 2",
		)
		.expect("query succeeds");
		assert_eq!(results[0].result.as_ref().expect("LET succeeds").value(), &Value::None);
		let Value::Array(rows) = results[1].result.as_ref().expect("SELECT succeeds").value()
		else {
			panic!("expected an array result");
		};
		assert_eq!(rows.len(), 2);
		assert_eq!(
			rows.get(0).and_then(Value::as_object).and_then(|row| row.get("name")),
			Some(&Value::from("Alice"))
		);
		assert_eq!(
			rows.get(1).and_then(Value::as_object).and_then(|row| row.get("name")),
			Some(&Value::from("Bob"))
		);
	}

	#[test]
	fn gets_record_and_materializes_select_value() {
		let results = run(&TestStore::people(), "SELECT VALUE name FROM person:alice")
			.expect("query succeeds");
		let Value::Array(values) = results[0].result.as_ref().expect("SELECT succeeds").value()
		else {
			panic!("expected an array result");
		};
		assert_eq!(values.as_slice(), &[Value::from("Alice")]);
	}

	#[test]
	fn rejects_unknown_field_during_analysis() {
		let error = run(&TestStore::people(), "SELECT missing FROM person")
			.expect_err("unknown field must fail before execution");
		assert_eq!(error.status().diagnostic().phase(), Some(ExecutionPhase::SemanticAnalysis));
	}

	#[test]
	fn keeps_none_and_null_distinct() {
		let results = run(
			&TestStore::people(),
			"RETURN none = null; RETURN [none, null]; SELECT VALUE name FROM person WHERE none = \
			 null; SELECT VALUE name FROM person WHERE none IS none LIMIT 1",
		)
		.expect("query succeeds");
		assert_eq!(
			results[0].result.as_ref().expect("comparison succeeds").value(),
			&Value::Bool(false)
		);
		let Value::Array(values) = results[1].result.as_ref().expect("array succeeds").value()
		else {
			panic!("expected array");
		};
		assert_eq!(values.as_slice(), &[Value::None, Value::Null]);
		let Value::Array(filtered_out) =
			results[2].result.as_ref().expect("false filter succeeds").value()
		else {
			panic!("expected array");
		};
		assert!(filtered_out.is_empty());
		let Value::Array(filtered_in) =
			results[3].result.as_ref().expect("true filter succeeds").value()
		else {
			panic!("expected array");
		};
		assert_eq!(filtered_in.as_slice(), &[Value::from("Alice")]);
	}

	#[test]
	fn preserves_domain_error_and_redacts_storage_source() {
		let mut store = TestStore::people();
		store.fail_scan = true;
		let results = run(&store, "SELECT name FROM person").expect("planning succeeds");
		let error = results[0].result.as_ref().expect_err("scan fails");
		assert_eq!(error.status().code().to_string(), "FG-50F00");
		assert!(error.technical_source().is_some());
		assert!(!format!("{error:?}").contains("secret storage detail"));
		assert!(Error::source(error).is_none());
	}
}
