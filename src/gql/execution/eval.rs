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
			.map(|value| eval(value, row, slots, source, fields))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		TypedExprKind::Object(object_fields) => object_fields
			.iter()
			.map(|(name, value)| {
				eval(value, row, slots, source, fields).map(|value| (name.as_ref(), value))
			})
			.collect::<FerrosGrynnResult<Object>>()
			.map(Value::Object),
		TypedExprKind::Unary {
			op,
			expr,
		} => unary(*op, eval(expr, row, slots, source, fields)?),
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			let left = eval(left, row, slots, source, fields)?;
			match op {
				BinaryOperator::And if !left.is_truthy() => Ok(Value::Bool(false)),
				BinaryOperator::Or if left.is_truthy() => Ok(Value::Bool(true)),
				BinaryOperator::Coalesce if !matches!(left, Value::None | Value::Null) => Ok(left),
				BinaryOperator::Elvis if left.is_truthy() => Ok(left),
				_ => binary(*op, left, eval(right, row, slots, source, fields)?),
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			if eval(condition, row, slots, source, fields)?.is_truthy() {
				eval(then_value, row, slots, source, fields)
			} else {
				eval(else_value, row, slots, source, fields)
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
			let base = eval(base, row, slots, source, fields)?;
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
		} => index_value(
			eval(base, row, slots, source, fields)?,
			eval(index, row, slots, source, fields)?,
		),
		TypedExprKind::Cast {
			target,
			expr,
		} => cast(eval(expr, row, slots, source, fields)?, target),
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

pub(super) fn eval_usize(
	expr: &TypedExpr,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<usize> {
	let value = eval(expr, None, slots, source, None)?;
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
