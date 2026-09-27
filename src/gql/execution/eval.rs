#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;

pub(super) fn eval(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &[Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	eval_current(expr, row, slots, source, fields, None, control)
}

fn eval_current(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &[Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
	current: Option<&Value>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	if let Some(control) = control {
		control.check()?;
	}
	let result = match &expr.kind {
		TypedExprKind::Constant(value) => Ok(value.clone()),
		TypedExprKind::Slot(slot) => Ok(slots[*slot as usize].clone()),
		TypedExprKind::Column(column) => {
			row.and_then(|row| row.value(*column)).cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Column is missing from the input row.")
			})
		}
		TypedExprKind::Array(values) => values
			.iter()
			.map(|value| eval_current(value, row, slots, source, fields, current, control))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		TypedExprKind::Set(values) => values
			.iter()
			.map(|value| eval_current(value, row, slots, source, fields, current, control))
			.collect::<FerrosGrynnResult<crate::values::Set>>()
			.map(Value::Set),
		TypedExprKind::Object(object_fields) => object_fields
			.iter()
			.map(|(name, value)| {
				eval_current(value, row, slots, source, fields, current, control)
					.map(|value| (name.as_ref(), value))
			})
			.collect::<FerrosGrynnResult<Object>>()
			.map(Value::Object),
		TypedExprKind::Range {
			start,
			end,
			inclusive_end,
		} => {
			let start = start
				.as_ref()
				.map(|value| eval_current(value, row, slots, source, fields, current, control))
				.transpose()?
				.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Included);
			let end = end
				.as_ref()
				.map(|value| eval_current(value, row, slots, source, fields, current, control))
				.transpose()?
				.map_or(std::ops::Bound::Unbounded, |value| {
					if *inclusive_end {
						std::ops::Bound::Included(value)
					} else {
						std::ops::Bound::Excluded(value)
					}
				});
			crate::values::Range::new(start, end).map(Value::Range)
		}
		TypedExprKind::Unary {
			op,
			expr,
		} => unary(*op, eval_current(expr, row, slots, source, fields, current, control)?),
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			let left = eval_current(left, row, slots, source, fields, current, control)?;
			match op {
				BinaryOperator::And if !left.is_truthy() => Ok(Value::Bool(false)),
				BinaryOperator::Or if left.is_truthy() => Ok(Value::Bool(true)),
				BinaryOperator::Coalesce if !matches!(left, Value::None | Value::Null) => Ok(left),
				BinaryOperator::Elvis if left.is_truthy() => Ok(left),
				_ => binary(
					*op,
					left,
					eval_current(right, row, slots, source, fields, current, control)?,
				),
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			if eval_current(condition, row, slots, source, fields, current, control)?.is_truthy() {
				eval_current(then_value, row, slots, source, fields, current, control)
			} else {
				eval_current(else_value, row, slots, source, fields, current, control)
			}
		}
		TypedExprKind::Case {
			operand,
			branches,
			else_value,
		} => {
			let operand = operand
				.as_ref()
				.map(|value| eval_current(value, row, slots, source, fields, current, control))
				.transpose()?;
			let mut selected = None;
			for (condition, value) in branches {
				let condition =
					eval_current(condition, row, slots, source, fields, current, control)?;
				let matches = operand
					.as_ref()
					.map_or_else(|| condition.is_truthy(), |operand| operand == &condition);
				if matches {
					selected =
						Some(eval_current(value, row, slots, source, fields, current, control)?);
					break;
				}
			}
			if let Some(value) = selected {
				Ok(value)
			} else if let Some(value) = else_value {
				eval_current(value, row, slots, source, fields, current, control)
			} else {
				Ok(Value::None)
			}
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			if let TypedExprKind::Column(column) = base.kind
				&& let Some(atom) = row.and_then(|row| row.atoms.get(column)).and_then(|atom| *atom)
				&& let Some(fields) = fields
			{
				return fields
					.field(atom, field)
					.map_err(|error| locate_execution(error, expr, source));
			}
			let base = eval_current(base, row, slots, source, fields, current, control)?;
			match base {
				Value::Object(object) => Ok(object.get(field).cloned().unwrap_or(Value::None)),
				Value::Error(error) => Ok(error_field(&error, field)),
				Value::Record(record) if fields.is_some() => fields
					.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("Record resolver is unavailable.")
					})?
					.record_field(&record, field),
				Value::None | Value::Null if *null_safe => Ok(Value::None),
				value => Err(FerrosGrynnError::invalid_argument(format!(
					"Cannot access field `{field}` on {}.",
					value.kind()
				))),
			}
		}
		TypedExprKind::CurrentField(field) => current_field(current, field),
		TypedExprKind::Destructure {
			base,
			fields: names,
		} => {
			let Value::Object(base) =
				eval_current(base, row, slots, source, fields, current, control)?
			else {
				return Err(locate_execution(
					FerrosGrynnError::invalid_argument("Object destructuring requires an object."),
					expr,
					source,
				));
			};
			Ok(Value::Object(
				names
					.iter()
					.map(|name| (name.as_ref(), base.get(name).cloned().unwrap_or(Value::None)))
					.collect(),
			))
		}
		TypedExprKind::Index {
			base,
			index,
		} => index_value(
			eval_current(base, row, slots, source, fields, current, control)?,
			eval_current(index, row, slots, source, fields, current, control)?,
		),
		TypedExprKind::Filter {
			base,
			predicate,
		} => {
			let Value::Array(values) =
				eval_current(base, row, slots, source, fields, current, control)?
			else {
				return Err(locate_execution(
					FerrosGrynnError::invalid_argument("Collection filter requires an array."),
					expr,
					source,
				));
			};
			let mut filtered = Array::new(Vec::new());
			for value in values {
				if eval_current(predicate, row, slots, source, fields, Some(&value), control)?
					.is_truthy()
				{
					filtered.push(value);
				}
			}
			Ok(Value::Array(filtered))
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} if matches!(function, Builtin::GraphInner) => {
			let argument = arguments.first().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("graph::inner requires one argument")
			})?;
			let value = eval_current(argument, row, slots, source, fields, current, control)?;
			let Value::AtomOccurrence(occurrence) = value else {
				return Err(FerrosGrynnError::invalid_argument(
					"graph::inner requires an atom occurrence",
				));
			};
			fields
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"graph::inner requires a MATCH field resolver",
					)
				})?
				.inner_graph(&occurrence)
		}
		TypedExprKind::Builtin {
			function,
			arguments,
		} => arguments
			.iter()
			.map(|argument| eval_current(argument, row, slots, source, fields, current, control))
			.collect::<FerrosGrynnResult<Vec<_>>>()
			.and_then(|arguments| {
				crate::gql::execution::builtins::execute(function, arguments, control)
			}),
		TypedExprKind::Aggregate {
			..
		}
		| TypedExprKind::Subquery {
			..
		} => Err(FerrosGrynnError::invalid_argument(
			"SELECT aggregate or subquery reached the scalar evaluator before materialization.",
		)),
		TypedExprKind::Cast {
			target,
			expr,
		} => cast(eval_current(expr, row, slots, source, fields, current, control)?, target),
	};
	result.map_err(|error| locate_execution(error, expr, source))
}

fn unary(operator: UnaryOperator, value: Value) -> FerrosGrynnResult<Value> {
	match operator {
		UnaryOperator::Not => Ok(Value::Bool(!value.is_truthy())),
		UnaryOperator::Truthy => Ok(Value::Bool(value.is_truthy())),
		UnaryOperator::Positive => Ok(value),
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

pub(super) fn binary(
	operator: BinaryOperator,
	left: Value,
	right: Value,
) -> FerrosGrynnResult<Value> {
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
			let contains = contains(&right, &left)?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::NotIn) {
				!contains
			} else {
				contains
			}))
		}
		BinaryOperator::AllIn | BinaryOperator::AnyIn | BinaryOperator::NoneIn => {
			let values = collection_values(&left)?;
			let matches = values
				.iter()
				.map(|value| contains(&right, value))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			Ok(Value::Bool(match operator {
				BinaryOperator::AllIn => matches.into_iter().all(|value| value),
				BinaryOperator::AnyIn => matches.into_iter().any(|value| value),
				BinaryOperator::NoneIn => matches.into_iter().all(|value| !value),
				_ => false,
			}))
		}
		BinaryOperator::AnyEqual | BinaryOperator::AllEqual => {
			let values = collection_values(&left)?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::AnyEqual) {
				values.iter().any(|value| *value == &right)
			} else {
				values.iter().all(|value| *value == &right)
			}))
		}
		BinaryOperator::Contains | BinaryOperator::ContainsNot => {
			let contains = contains(&left, &right)?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::ContainsNot) {
				!contains
			} else {
				contains
			}))
		}
		BinaryOperator::ContainsAny | BinaryOperator::ContainsAll => {
			let values = collection_values(&right)?;
			let matches = values
				.iter()
				.map(|value| contains(&left, value))
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			Ok(Value::Bool(if matches!(operator, BinaryOperator::ContainsAny) {
				matches.into_iter().any(|value| value)
			} else {
				matches.into_iter().all(|value| value)
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

fn contains(container: &Value, needle: &Value) -> FerrosGrynnResult<bool> {
	match container {
		Value::Array(values) => Ok(values.iter().any(|value| value == needle)),
		Value::Set(values) => Ok(values.contains(needle)),
		Value::Object(values) => {
			Ok(needle.as_str().is_some_and(|needle| values.contains_key(needle)))
		}
		Value::String(value) => {
			Ok(needle.as_str().is_some_and(|needle| value.as_str().contains(needle)))
		}
		Value::Range(value) => Ok(value.contains(needle)),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"{} does not support containment.",
			value.kind()
		))),
	}
}

fn collection_values(value: &Value) -> FerrosGrynnResult<Vec<&Value>> {
	match value {
		Value::Array(values) => Ok(values.iter().collect()),
		Value::Set(values) => Ok(values.iter().collect()),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"{} is not an iterable collection.",
			value.kind()
		))),
	}
}

fn current_field(current: Option<&Value>, field: &str) -> FerrosGrynnResult<Value> {
	match current {
		Some(Value::Object(value)) => Ok(value.get(field).cloned().unwrap_or(Value::None)),
		Some(Value::Error(error)) => Ok(error_field(error, field)),
		Some(value) => Err(FerrosGrynnError::invalid_argument(format!(
			"Cannot resolve field `{field}` on {}.",
			value.kind()
		))),
		None => Err(FerrosGrynnError::invalid_argument(format!(
			"Field `{field}` has no current collection item."
		))),
	}
}

fn error_field(error: &crate::values::ErrorValue, field: &str) -> Value {
	match field.to_ascii_lowercase().as_str() {
		"code" => Value::from(error.code()),
		"message" => Value::from(error.message()),
		"condition" => Value::from(error.condition()),
		"classification" => Value::from(error.classification()),
		"category" => Value::from(error.category()),
		"domain" => Value::from(error.domain()),
		"causes" => Value::Array(
			error.causes().iter().cloned().map(|value| Value::Error(Box::new(value))).collect(),
		),
		_ => Value::None,
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
	if value.is_nan() {
		return Err(FerrosGrynnError::invalid_argument(
			"Exponentiation received a value outside its domain.",
		));
	}
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
			return decimal_power(left, right).map(Number::Decimal).map(Value::Number);
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	}
	.ok_or_else(|| FerrosGrynnError::operation_overflow(operator_name(operator)))?;
	Ok(Value::Number(Number::Decimal(value)))
}

fn decimal_power(base: Decimal, exponent: Decimal) -> FerrosGrynnResult<Decimal> {
	let exponent = i64::try_from(Number::Decimal(exponent)).map_err(|_error| {
		FerrosGrynnError::invalid_argument("Decimal exponent must be an integer.")
	})?;
	if exponent.is_negative() && base.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}

	let mut remaining = exponent.unsigned_abs();
	let mut factor = base;
	let mut value = Decimal::ONE;
	while remaining != 0 {
		if remaining & 1 == 1 {
			value = value
				.checked_mul(factor)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?;
		}
		remaining >>= 1;
		if remaining != 0 {
			factor = factor
				.checked_mul(factor)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?;
		}
	}
	if exponent.is_negative() {
		Decimal::ONE.checked_div(value).ok_or_else(|| FerrosGrynnError::operation_overflow("**"))
	} else {
		Ok(value)
	}
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

pub(super) fn cast(value: Value, target: &Type) -> FerrosGrynnResult<Value> {
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

pub(super) fn eval_usize(
	expr: &TypedExpr,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<usize> {
	let value = eval(expr, None, slots, source, None, Some(control))?;
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

pub(super) fn execution_phase(error: FerrosGrynnError) -> FerrosGrynnError {
	error.with_phase(ExecutionPhase::Execution)
}
