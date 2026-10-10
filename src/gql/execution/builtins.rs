use std::collections::BTreeSet;

use num_traits::ToPrimitive;

use super::{binary, cast};
use crate::{
	gql::{ir::Builtin, options::ExecutionControl},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	syn::BinaryOperator,
	values::{Array, Duration, Matcher, Number, Regex, Set, Type, Ulid, Uuid, Value},
};

pub(super) fn execute(
	function: &Builtin,
	arguments: Vec<Value>,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<Value> {
	execute_with_fields(function, arguments, control, None)
}

/// Admits bounded PURE values before executing collection/string helpers.
pub(super) fn execute_with_fields(
	function: &Builtin,
	arguments: Vec<Value>,
	control: Option<&ExecutionControl>,
	fields: Option<&dyn super::AtomFieldRead>,
) -> FerrosGrynnResult<Value> {
	if let Some(fields) = fields.filter(|fields| fields.bounded()) {
		// Dynamic regex compilation has no preallocation proof.
		// Reject it before entering external allocators under a hard COST cap.
		if matches!(function, Builtin::RegexMatcher) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"unbounded PURE helper scratch",
				control.map_or(0, ExecutionControl::barrier_memory_limit) as u64,
			));
		}
		for value in &arguments {
			fields.admit_value(value)?;
		}
		let input_bytes = arguments
			.iter()
			.try_fold(0_usize, |bytes, value| bytes.checked_add(value.estimated_heap_bytes()))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE builtin input"))?;
		let multiplier = if matches!(function, Builtin::StringSplit) {
			2 * size_of::<Value>() + 4
		} else {
			4
		};
		let bytes = input_bytes
			.checked_mul(multiplier)
			.and_then(|bytes| bytes.checked_add(arguments.len().checked_mul(128)?))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("PURE builtin scratch"))?;
		fields.admit(bytes)?;
		fields.work(
			u64::try_from(input_bytes)
				.map_err(|_cause| FerrosGrynnError::operation_overflow("PURE builtin work"))?,
		)?;
	}
	match function {
		Builtin::GeoWithin | Builtin::GeoIntersects => {
			let (left, right) = two(arguments)?;
			binary(
				if matches!(function, Builtin::GeoWithin) {
					BinaryOperator::Inside
				} else {
					BinaryOperator::Intersects
				},
				left,
				right,
			)
		}
		Builtin::GeoDistance => {
			let (left, right) = two(arguments)?;
			if matches!(left, Value::None | Value::Null)
				|| matches!(right, Value::None | Value::Null)
			{
				return Ok(Value::None);
			}
			let (Value::Geometry(left), Value::Geometry(right)) = (left, right) else {
				return Err(type_error("geo::distance", "point geometry, point geometry"));
			};
			left.distance(&right).map(Value::from)
		}
		Builtin::MathAbs => math_abs(one(arguments)?),
		Builtin::MathSqrt => number_f64(one(arguments)?, "math::sqrt", control).and_then(|value| {
			(value >= 0.0).then_some(Value::from(value.sqrt())).ok_or_else(|| domain("math::sqrt"))
		}),
		Builtin::MathPow => {
			let (left, right) = two(arguments)?;
			binary(BinaryOperator::Power, left, right)
		}
		Builtin::MathFloor => number_f64(one(arguments)?, "math::floor", control)
			.map(|value| Value::from(value.floor())),
		Builtin::MathCeil => number_f64(one(arguments)?, "math::ceil", control)
			.map(|value| Value::from(value.ceil())),
		Builtin::StringUpper => {
			string(one(arguments)?, "string::upper").map(|value| Value::from(value.to_uppercase()))
		}
		Builtin::StringLower => {
			string(one(arguments)?, "string::lower").map(|value| Value::from(value.to_lowercase()))
		}
		Builtin::StringLen => {
			string(one(arguments)?, "string::len").and_then(|value| length(value.chars().count()))
		}
		Builtin::StringContains => {
			let (haystack, needle) = two(arguments)?;
			Ok(Value::Bool(
				string(haystack, "string::contains")?
					.contains(string(needle, "string::contains")?.as_str()),
			))
		}
		Builtin::StringMatches => {
			let (value, pattern) = two(arguments)?;
			let value = string(value, "string::matches")?;
			if let Some(fields) = fields.filter(|fields| fields.bounded()) {
				let pattern = match &pattern {
					Value::Regex(pattern) => pattern.as_str(),
					Value::String(pattern) => pattern.as_str(),
					_ => return Err(type_error("string::matches", "regex or string")),
				};
				// Existing escaping identifies the literal fast path without
				// compiling an AST. ponytail: complex patterns
				// need a proven compiler allocation envelope.
				if regex::escape(pattern) != pattern {
					return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
						"regular expression compiler scratch",
						control.map_or(0, ExecutionControl::barrier_memory_limit) as u64,
					));
				}
				let work = value
					.len()
					.checked_add(1)
					.and_then(|n| pattern.len().checked_add(1).and_then(|m| n.checked_mul(m)))
					.ok_or_else(|| FerrosGrynnError::operation_overflow("literal regex work"))?;
				fields.work(u64::try_from(work).map_err(|_cause| {
					FerrosGrynnError::operation_overflow("literal regex work")
				})?)?;
				return Ok(Value::Bool(value.contains(pattern)));
			}
			let pattern = match pattern {
				Value::Regex(pattern) => pattern,
				Value::String(pattern) => Regex::new(pattern).map_err(|error| {
					FerrosGrynnError::invalid_argument(format!(
						"invalid regular expression: {error}"
					))
				})?,
				_ => return Err(type_error("string::matches", "regex or string")),
			};
			Ok(Value::Bool(pattern.is_match(&value)))
		}
		Builtin::StringSlice => {
			let mut values = arguments.into_iter();
			let value = string(values.next().ok_or_else(missing_argument)?, "string::slice")?;
			let start =
				non_negative_usize(values.next().ok_or_else(missing_argument)?, "string::slice")?;
			let end =
				non_negative_usize(values.next().ok_or_else(missing_argument)?, "string::slice")?;
			if start > end {
				return Err(domain("string::slice"));
			}
			let start = scalar_byte_offset(&value, start)?;
			let end = scalar_byte_offset(&value, end)?;
			Ok(Value::from(&value[start..end]))
		}
		Builtin::RegexMatcher => {
			let (value, pattern) = two(arguments)?;
			let value = string(value, "regex::matcher")?;
			let pattern = regex(pattern, "regex::matcher")?;
			Ok(Value::Matcher(Matcher::new(value.into(), pattern)))
		}
		Builtin::RegexNext => {
			let Value::Matcher(matcher) = one(arguments)? else {
				return Err(type_error("regex::next", "matcher"));
			};
			Ok(Value::Bool(matcher.advance()))
		}
		Builtin::StringSplit => {
			let (value, separator) = two(arguments)?;
			let value = string(value, "string::split")?;
			let separator = string(separator, "string::split")?;
			let mut values = Vec::new();
			for (index, value) in value.split(separator.as_str()).enumerate() {
				check(control, index)?;
				values.push(Value::from(value));
			}
			Ok(Value::Array(Array::new(values)))
		}
		Builtin::StringConcat => {
			let mut result = String::new();
			for (index, value) in arguments.into_iter().enumerate() {
				check(control, index)?;
				result.push_str(&string(value, "string::concat")?);
			}
			Ok(Value::from(result))
		}
		Builtin::TimeNow(value) => Ok(Value::DateTime(value.clone())),
		Builtin::TimeFormat => {
			let (value, format) = two(arguments)?;
			let Value::DateTime(value) = value else {
				return Err(type_error("time::format", "datetime"));
			};
			let format = string(format, "time::format")?;
			format_time(&value, format.as_str(), fields)
		}
		Builtin::TimeAdd => {
			let (value, duration) = two(arguments)?;
			let (Value::DateTime(value), Value::Duration(duration)) = (value, duration) else {
				return Err(type_error("time::add", "datetime, duration"));
			};
			value
				.as_inner()
				.checked_add(duration.as_inner())
				.map(crate::values::DateTime::from)
				.map(Value::DateTime)
				.map_err(FerrosGrynnError::from)
		}
		Builtin::TimeDiff => {
			let (start, end) = two(arguments)?;
			let (Value::DateTime(start), Value::DateTime(end)) = (start, end) else {
				return Err(type_error("time::diff", "datetime, datetime"));
			};
			start
				.as_inner()
				.until(end.as_inner())
				.map(Duration::from)
				.map(Value::Duration)
				.map_err(FerrosGrynnError::from)
		}
		Builtin::ArrayLen => {
			array(one(arguments)?, "array::len").and_then(|value| length(value.len()))
		}
		Builtin::ArrayConcat => {
			let (left, right) = two(arguments)?;
			let mut values = Vec::new();
			for (index, value) in array(left, "array::concat")?
				.into_iter()
				.chain(array(right, "array::concat")?)
				.enumerate()
			{
				check(control, index)?;
				values.push(value);
			}
			Ok(Value::Array(Array::new(values)))
		}
		Builtin::ArrayDistinct => {
			let values = array(one(arguments)?, "array::distinct")?;
			let mut seen = BTreeSet::new();
			let mut result = Vec::new();
			for (index, value) in values.into_iter().enumerate() {
				check(control, index)?;
				if seen.insert(value.clone()) {
					result.push(value);
				}
			}
			Ok(Value::Array(Array::new(result)))
		}
		Builtin::ArrayFirst => {
			Ok(array(one(arguments)?, "array::first")?.into_iter().next().unwrap_or(Value::None))
		}
		Builtin::ArrayFlatten => {
			let values = array(one(arguments)?, "array::flatten")?;
			let mut result = Array::new(Vec::new());
			let mut index = 0;
			for value in values {
				check(control, index)?;
				index += 1;
				if let Value::Array(nested) = value {
					for value in nested {
						check(control, index)?;
						index += 1;
						result.push(value);
					}
				} else {
					result.push(value);
				}
			}
			Ok(Value::Array(result))
		}
		Builtin::ArrayPush(_)
		| Builtin::ArrayPop(_)
		| Builtin::ArrayClear(_)
		| Builtin::SetAdd(_)
		| Builtin::SetClear(_) => Err(FerrosGrynnError::invalid_argument(
			"Mutable collection helper reached the read-only built-in evaluator.",
		)),
		Builtin::SetLen => set(one(arguments)?, "set::len").and_then(|value| length(value.len())),
		Builtin::SetUnion => {
			let (left, right) = two_sets(arguments, "set::union")?;
			let mut result = Set::new();
			for (index, value) in left.into_iter().chain(right).enumerate() {
				check(control, index)?;
				result.insert(value);
			}
			Ok(Value::Set(result))
		}
		Builtin::SetIntersect => {
			let (left, right) = two_sets(arguments, "set::intersect")?;
			let mut result = Set::new();
			for (index, value) in left.into_iter().enumerate() {
				check(control, index)?;
				if right.contains(&value) {
					result.insert(value);
				}
			}
			Ok(Value::Set(result))
		}
		Builtin::SetDiff => {
			let (left, right) = two_sets(arguments, "set::diff")?;
			let mut result = Set::new();
			for (index, value) in left.into_iter().enumerate() {
				check(control, index)?;
				if !right.contains(&value) {
					result.insert(value);
				}
			}
			Ok(Value::Set(result))
		}
		Builtin::SetHas => {
			let (values, value) = two(arguments)?;
			Ok(Value::Bool(set(values, "set::has")?.contains(&value)))
		}
		Builtin::TypeIs(target) => Ok(Value::Bool(type_matches(target, &one(arguments)?))),
		Builtin::TypeTo(target) => cast(one(arguments)?, target, fields, control),
		Builtin::MetaId => meta_id(one(arguments)?),
		Builtin::MetaClass => meta_class(one(arguments)?),
		Builtin::MetaType => Ok(Value::from(one(arguments)?.value_type().to_string())),
		Builtin::GraphInner | Builtin::GraphLevelPath => Err(FerrosGrynnError::invalid_argument(
			"graph occurrence functions require the MATCH graph resolver",
		)),
		Builtin::SearchAnalyze {
			..
		}
		| Builtin::SearchScore(_)
		| Builtin::SearchHighlight(_) => Err(FerrosGrynnError::invalid_argument(
			"search::analyze requires query execution context",
		)),
		Builtin::PathCostWith(_) => Err(FerrosGrynnError::invalid_argument(
			"custom path cost requires query execution context",
		)),
		Builtin::PathAtoms => {
			let path = path(one(arguments)?, "path::atoms")?;
			Ok(Value::Array(Array::new(path.atoms().cloned().map(Value::from).collect())))
		}
		Builtin::PathNodes => {
			let path = path(one(arguments)?, "path::nodes")?;
			Ok(Value::Array(Array::new(path.nodes().cloned().map(Value::from).collect())))
		}
		Builtin::PathEdges => {
			let path = path(one(arguments)?, "path::edges")?;
			Ok(Value::Array(Array::new(path.edges().cloned().map(Value::from).collect())))
		}
		Builtin::PathLength => {
			let path = path(one(arguments)?, "path::length")?;
			length(path.length())
		}
		Builtin::PathCost => {
			let path = path(one(arguments)?, "path::cost")?;
			path.cost().map(Value::Number).map_or_else(|| length(path.length()), Ok)
		}
		Builtin::OptionIsSome => {
			Ok(Value::Bool(!matches!(one(arguments)?, Value::None | Value::Null)))
		}
		Builtin::OptionIsNone => {
			Ok(Value::Bool(matches!(one(arguments)?, Value::None | Value::Null)))
		}
		Builtin::Uuid => Ok(Value::Uuid(Uuid::now_v7())),
		Builtin::Ulid => Ok(Value::Ulid(Ulid::new())),
		Builtin::RandId => random_id(arguments),
	}
}

fn random_id(arguments: Vec<Value>) -> FerrosGrynnResult<Value> {
	let length = match arguments.into_iter().next() {
		Some(value) => non_negative_usize(value, "rand::id")?,
		None => 20,
	};
	if !(1..=1024).contains(&length) {
		return Err(domain("rand::id"));
	}
	const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
	let mut output = String::with_capacity(length);
	while output.len() < length {
		for byte in Uuid::new_v4().as_inner().as_bytes() {
			if *byte >= 252 {
				continue;
			}
			output.push(char::from(ALPHABET[usize::from(*byte % 36)]));
			if output.len() == length {
				break;
			}
		}
	}
	Ok(Value::from(output))
}

fn non_negative_usize(value: Value, function: &str) -> FerrosGrynnResult<usize> {
	let Value::Number(Number::Int(value)) = value else {
		return Err(type_error(function, "non-negative integer"));
	};
	usize::try_from(value).map_err(|_| domain(function))
}

fn scalar_byte_offset(value: &str, offset: usize) -> FerrosGrynnResult<usize> {
	if offset == 0 {
		return Ok(0);
	}
	value
		.char_indices()
		.nth(offset)
		.map(|(index, _)| index)
		.or_else(|| (value.chars().count() == offset).then_some(value.len()))
		.ok_or_else(|| domain("string::slice"))
}

fn missing_argument() -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument("Built-in argument is missing.")
}

fn check(control: Option<&ExecutionControl>, index: usize) -> FerrosGrynnResult<()> {
	if index.is_multiple_of(1024)
		&& let Some(control) = control
	{
		control.check()?;
	}
	Ok(())
}

fn math_abs(value: Value) -> FerrosGrynnResult<Value> {
	match value {
		Value::Number(Number::Int(value)) => value
			.checked_abs()
			.map(Value::from)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("math::abs")),
		Value::Number(Number::Float(value)) => Ok(Value::from(value.abs())),
		Value::Number(Number::Decimal(value)) => Ok(Value::Number(Number::Decimal(value.abs()))),
		_ => Err(type_error("math::abs", "number")),
	}
}

fn meta_id(value: Value) -> FerrosGrynnResult<Value> {
	match value {
		Value::Record(record) => Ok(record.id().clone().into_value()),
		Value::Atom(atom) => Ok(atom.id().id().clone().into_value()),
		Value::AtomOccurrence(occurrence) => Ok(occurrence.atom().id().id().clone().into_value()),
		_ => Err(type_error("meta::id", "record or atom")),
	}
}

fn meta_class(value: Value) -> FerrosGrynnResult<Value> {
	match value {
		Value::Record(record) => Ok(Value::from(record.class())),
		Value::Atom(atom) => Ok(Value::from(atom.id().class())),
		Value::AtomOccurrence(occurrence) => Ok(Value::from(occurrence.atom().id().class())),
		_ => Err(type_error("meta::class", "record or atom")),
	}
}

fn type_matches(target: &Type, value: &Value) -> bool {
	crate::gql::analysis::compatible(target, &value.value_type())
}

fn number_f64(
	value: Value,
	function: &str,
	control: Option<&ExecutionControl>,
) -> FerrosGrynnResult<f64> {
	let Value::Number(value) = value else {
		return Err(type_error(function, "number"));
	};
	match value {
		Number::Int(integer) => Ok(integer as f64),
		Number::Float(float) => Ok(float),
		Number::Decimal(decimal) => {
			let float =
				decimal.to_f64().ok_or_else(|| type_error(function, "float-compatible decimal"))?;
			if Number::Float(float) != Number::Decimal(decimal)
				&& let Some(control) = control
			{
				control.notify(crate::gql_status::Status::lossy_decimal_to_float(function));
			}
			Ok(float)
		}
	}
}

fn string(value: Value, function: &str) -> FerrosGrynnResult<String> {
	match value {
		Value::String(value) => Ok(value.to_string()),
		_ => Err(type_error(function, "string")),
	}
}

fn regex(value: Value, function: &str) -> FerrosGrynnResult<Regex> {
	match value {
		Value::Regex(value) => Ok(value),
		Value::String(value) => Regex::new(value).map_err(|error| {
			FerrosGrynnError::invalid_argument(format!("invalid regular expression: {error}"))
		}),
		_ => Err(type_error(function, "regex or string")),
	}
}

fn array(value: Value, function: &str) -> FerrosGrynnResult<Array> {
	match value {
		Value::Array(value) => Ok(value),
		_ => Err(type_error(function, "array")),
	}
}

fn set(value: Value, function: &str) -> FerrosGrynnResult<Set> {
	match value {
		Value::Set(value) => Ok(value),
		_ => Err(type_error(function, "set")),
	}
}

fn path(value: Value, function: &str) -> FerrosGrynnResult<Box<crate::graph::Path>> {
	match value {
		Value::Path(value) => Ok(value),
		_ => Err(type_error(function, "path")),
	}
}

fn one(mut arguments: Vec<Value>) -> FerrosGrynnResult<Value> {
	arguments
		.pop()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("Built-in argument is missing."))
}

fn two(arguments: Vec<Value>) -> FerrosGrynnResult<(Value, Value)> {
	let mut arguments = arguments.into_iter();
	let left = arguments
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("Built-in argument is missing."))?;
	let right = arguments
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("Built-in argument is missing."))?;
	Ok((left, right))
}

fn two_sets(arguments: Vec<Value>, function: &str) -> FerrosGrynnResult<(Set, Set)> {
	let (left, right) = two(arguments)?;
	Ok((set(left, function)?, set(right, function)?))
}

fn length(value: usize) -> FerrosGrynnResult<Value> {
	i64::try_from(value)
		.map(Value::from)
		.map_err(|_| FerrosGrynnError::operation_overflow("collection length"))
}

/// Counts a deterministic formatting stream before allocating its exact output.
fn format_time(
	value: &crate::values::DateTime,
	format: &str,
	fields: Option<&dyn super::AtomFieldRead>,
) -> FerrosGrynnResult<Value> {
	if let Some(fields) = fields {
		fields.admit(
			value
				.as_inner()
				.time_zone()
				.iana_name()
				.map_or(512, |name| name.len().saturating_add(512)),
		)?;
		fields.work(
			u64::try_from(format.len())
				.map_err(|_cause| FerrosGrynnError::operation_overflow("time format scan"))?
				.checked_mul(2)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("time format scan"))?,
		)?;
	}
	let broken = jiff::fmt::strtime::BrokenDownTime::from(value.as_inner());
	let config = jiff::fmt::strtime::Config::new().lenient(true);
	let mut counter = TimeFormatWriter {
		output: None,
		bytes: 0,
		fields,
		failure: None,
	};
	let counted =
		broken.format_with_config(&config, format, &mut jiff::fmt::StdFmtWrite(&mut counter));
	if let Some(error) = counter.failure {
		return Err(error);
	}
	counted.map_err(FerrosGrynnError::from)?;
	let bytes = counter.bytes;
	if let Some(fields) = fields {
		fields.admit(
			bytes
				.checked_mul(2)
				.and_then(|bytes| bytes.checked_add(512))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("time format output"))?,
		)?;
	}
	let mut output = String::new();
	output.try_reserve_exact(bytes).map_err(|cause| {
		FerrosGrynnError::traversal_resource_limit_exceeded("time format allocation", bytes as u64)
			.with_technical_source(cause)
	})?;
	let mut writer = TimeFormatWriter {
		output: Some(&mut output),
		bytes: 0,
		fields,
		failure: None,
	};
	let written =
		broken.format_with_config(&config, format, &mut jiff::fmt::StdFmtWrite(&mut writer));
	if let Some(error) = writer.failure {
		return Err(error);
	}
	written.map_err(FerrosGrynnError::from)?;
	Ok(Value::from(output))
}

/// Bounded two-pass writer; lenient formatting cannot swallow admission
/// failures.
struct TimeFormatWriter<'a> {
	/// Absent in the allocation-free counting pass.
	output: Option<&'a mut String>,
	/// Checked bytes already formatted in this pass.
	bytes: usize,
	/// Optional embedding-wide resource admission.
	fields: Option<&'a dyn super::AtomFieldRead>,
	/// Original typed failure preserved across lenient directive retries.
	failure: Option<FerrosGrynnError>,
}

impl std::fmt::Write for TimeFormatWriter<'_> {
	fn write_str(&mut self, s: &str) -> std::fmt::Result {
		if self.failure.is_some() {
			return Err(std::fmt::Error);
		}
		let result = (|| {
			let bytes = self
				.bytes
				.checked_add(s.len())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("time format output"))?;
			if let Some(fields) = self.fields {
				fields.work(u64::try_from(s.len()).map_err(|_cause| {
					FerrosGrynnError::operation_overflow("time format work")
				})?)?;
			}
			if let Some(output) = &mut self.output {
				if bytes > output.capacity() {
					return Err(FerrosGrynnError::invalid_argument(
						"time format changed between immutable passes",
					));
				}
				output.push_str(s);
			}
			self.bytes = bytes;
			Ok(())
		})();
		if let Err(error) = result {
			self.failure = Some(error);
			return Err(std::fmt::Error);
		}
		Ok(())
	}
}

fn type_error(function: &str, expected: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("{function} expects {expected}."))
}

fn domain(function: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_argument(format!("{function} received a value outside its domain."))
}
