use std::{
	fmt::{self, Display, Formatter},
	str::FromStr,
};

use serde_json::Value as JsonValue;

use super::{Array, Number, Object, Text, Ulid, Uuid, Value};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Configured max record id depth value.
const MAX_RECORD_ID_DEPTH: usize = 64;

/// Scalar or composite key identifying a record within its class.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum RecordIdPart {
	/// Record key represented by a signed integer.
	Int(
		/// Record key represented by a signed integer.
		i64,
	),
	/// Record key represented by text.
	String(
		/// Record key represented by text.
		Text,
	),
	/// Record key represented by a UUID.
	Uuid(
		/// Record key represented by a UUID.
		Uuid,
	),
	/// Record key represented by a ULID.
	Ulid(
		/// Record key represented by a ULID.
		Ulid,
	),
	/// Record key represented by an ordered sequence of values.
	Array(
		/// Record key represented by an ordered sequence of values.
		Array,
	),
	/// Record key represented by a text-keyed value map.
	Object(
		/// Record key represented by a text-keyed value map.
		Object,
	),
}

impl RecordIdPart {
	/// Attempts from value for `RecordIdPart`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn try_from_value(value: Value) -> FerrosGrynnResult<Self> {
		validate_value(&value, 0)?;
		Ok(match value {
			Value::Number(Number::Int(value)) => Self::Int(value),
			Value::String(value) => Self::String(value),
			Value::Uuid(value) => Self::Uuid(value),
			Value::Ulid(value) => Self::Ulid(value),
			Value::Array(value) => Self::Array(value),
			Value::Object(value) => Self::Object(value),
			_ => unreachable!("record ID value was validated before conversion"),
		})
	}

	/// Converts this value into value for `RecordIdPart`.
	pub fn into_value(self) -> Value {
		match self {
			Self::Int(value) => Value::from(value),
			Self::String(value) => Value::String(value),
			Self::Uuid(value) => Value::Uuid(value),
			Self::Ulid(value) => Value::Ulid(value),
			Self::Array(value) => Value::Array(value),
			Self::Object(value) => Value::Object(value),
		}
	}

	/// Validates record id part.
	fn validate(&self) -> FerrosGrynnResult<()> {
		match self {
			Self::Array(value) => {
				for value in value {
					validate_value(value, 1)?;
				}
				Ok(())
			}
			Self::Object(value) => {
				for value in value.values() {
					validate_value(value, 1)?;
				}
				Ok(())
			}
			Self::String(value) if value.is_empty() => {
				Err(FerrosGrynnError::empty_record_id_part())
			}
			Self::Int(_) | Self::String(_) | Self::Uuid(_) | Self::Ulid(_) => Ok(()),
		}
	}
}

/// Owned class name paired with a scalar or composite record key.
#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct RecordId {
	/// Class associated with this record id.
	class: Text,
	/// Identifier used to locate this value within its owning subsystem.
	id: RecordIdPart,
}

impl RecordId {
	/// Constructs `RecordId` from the supplied initial state.
	///
	/// # Errors
	///
	/// Returns `empty_record_id_class` diagnostics and propagates failures from
	/// called operations.
	pub fn new<IntoText: Into<Text>>(class: IntoText, id: RecordIdPart) -> FerrosGrynnResult<Self> {
		let class = class.into();
		if class.is_empty() {
			return Err(FerrosGrynnError::empty_record_id_class());
		}
		id.validate()?;
		Ok(Self {
			class,
			id,
		})
	}

	/// Constructs from value for `RecordId`.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn from_value<IntoText: Into<Text>>(class: IntoText, id: Value) -> FerrosGrynnResult<Self> {
		Self::new(class, RecordIdPart::try_from_value(id)?)
	}

	/// Returns class for `RecordId`.
	pub fn class(&self) -> &str {
		self.class.as_str()
	}

	/// Borrows the stored class of this `RecordId`.
	pub const fn class_text(&self) -> &Text {
		&self.class
	}

	/// Borrows the stored id of this `RecordId`.
	pub const fn id(&self) -> &RecordIdPart {
		&self.id
	}

	/// Converts this value into parts for `RecordId`.
	pub fn into_parts(self) -> (Text, RecordIdPart) {
		(self.class, self.id)
	}

	/// Estimates heap bytes owned by the class name and composite identifier.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		self.class
			.estimated_heap_bytes()
			.saturating_add(super::value::estimated_record_part_heap_bytes(&self.id))
	}
}

impl Display for RecordIdPart {
	/// Formats `RecordIdPart` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::Int(value) => Display::fmt(value, f),
			Self::String(value) => {
				if string_id_needs_quotes(value) {
					let escaped =
						serde_json::to_string(value.as_str()).map_err(|_err| fmt::Error)?;
					f.write_str(&escaped)
				} else {
					Display::fmt(value, f)
				}
			}
			Self::Uuid(value) => Display::fmt(value, f),
			Self::Ulid(value) => Display::fmt(value, f),
			Self::Array(value) => Display::fmt(&Value::Array(value.clone()), f),
			Self::Object(value) => Display::fmt(&Value::Object(value.clone()), f),
		}
	}
}

impl Display for RecordId {
	/// Formats `RecordId` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.class, self.id)
	}
}

impl FromStr for RecordId {
	type Err = FerrosGrynnError;

	/// Constructs from str for `RecordId`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let (class, id) =
			value.split_once(':').ok_or_else(FerrosGrynnError::invalid_record_id_syntax)?;
		if id.is_empty() {
			return Err(FerrosGrynnError::empty_record_id_part());
		}

		let id = if id.starts_with('"') {
			let value = serde_json::from_str::<String>(id).map_err(|source| {
				FerrosGrynnError::invalid_composite_record_id().with_technical_source(source)
			})?;
			RecordIdPart::String(value.into())
		} else if id.starts_with('[') || id.starts_with('{') {
			let value = serde_json::from_str::<JsonValue>(id).map_err(|source| {
				FerrosGrynnError::invalid_composite_record_id().with_technical_source(source)
			})?;
			RecordIdPart::try_from_value(value_from_json(value, 0)?)?
		} else if let Ok(value) = id.parse::<Uuid>() {
			RecordIdPart::Uuid(value)
		} else if let Ok(value) = id.parse::<Ulid>() {
			RecordIdPart::Ulid(value)
		} else if let Ok(value) = id.parse::<i64>() {
			RecordIdPart::Int(value)
		} else {
			RecordIdPart::String(id.into())
		};

		Self::new(class, id)
	}
}

/// Returns whether the string id needs quotes condition holds.
fn string_id_needs_quotes(value: &str) -> bool {
	value.parse::<i64>().is_ok()
		|| value.parse::<Uuid>().is_ok()
		|| value.parse::<Ulid>().is_ok()
		|| value.chars().next().is_some_and(|value| matches!(value, '"' | '[' | '{'))
		|| value.chars().any(char::is_whitespace)
}

/// Validates value.
fn validate_value(value: &Value, depth: usize) -> FerrosGrynnResult<()> {
	if depth > MAX_RECORD_ID_DEPTH {
		return Err(FerrosGrynnError::record_id_nesting_too_deep(MAX_RECORD_ID_DEPTH as u64));
	}
	match value {
		Value::Number(Number::Int(_)) | Value::String(_) | Value::Uuid(_) | Value::Ulid(_) => {
			Ok(())
		}
		Value::Array(values) => {
			for value in values {
				validate_value(value, depth + 1)?;
			}
			Ok(())
		}
		Value::Object(values) => {
			for value in values.values() {
				validate_value(value, depth + 1)?;
			}
			Ok(())
		}
		_ => Err(FerrosGrynnError::invalid_record_id_part(value.kind().name())),
	}
}

/// Returns the value from json result.
fn value_from_json(value: JsonValue, depth: usize) -> FerrosGrynnResult<Value> {
	if depth > MAX_RECORD_ID_DEPTH {
		return Err(FerrosGrynnError::record_id_nesting_too_deep(MAX_RECORD_ID_DEPTH as u64));
	}
	match value {
		JsonValue::Number(value) => value
			.as_i64()
			.map(Value::from)
			.ok_or_else(|| FerrosGrynnError::invalid_record_id_part("non-integer number")),
		JsonValue::String(value) => Ok(Value::from(value)),
		JsonValue::Array(values) => values
			.into_iter()
			.map(|value| value_from_json(value, depth + 1))
			.collect::<Result<Array, _>>()
			.map(Value::Array),
		JsonValue::Object(values) => values
			.into_iter()
			.map(|(key, value)| value_from_json(value, depth + 1).map(|value| (key, value)))
			.collect::<Result<Object, _>>()
			.map(Value::Object),
		JsonValue::Null => Err(FerrosGrynnError::invalid_record_id_part("null")),
		JsonValue::Bool(_) => Err(FerrosGrynnError::invalid_record_id_part("bool")),
	}
}

#[cfg(test)]
/// Regression tests for record id behavior.
mod tests {
	use super::{RecordId, RecordIdPart};
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::{
		gql_status::Domain,
		values::{Array, Object, Value},
	};

	/// Verifies parses scalar and composite ids.
	#[test]
	fn parses_scalar_and_composite_ids() {
		let integer = "person:42".parse::<RecordId>().assert_ok();
		assert_eq!(integer.id(), &RecordIdPart::Int(42));

		let composite = r#"person:[42,{"region":"eu"}]"#.parse::<RecordId>().assert_ok();
		assert!(matches!(composite.id(), RecordIdPart::Array(_)));
	}

	/// Verifies recursively rejects unsupported parts.
	#[test]
	fn recursively_rejects_unsupported_parts() {
		let mut object = Object::new();
		object.insert("invalid", true);
		let array = Array::new(vec![Value::Object(object)]);
		let error = RecordId::new("person", RecordIdPart::Array(array)).assert_err();
		assert_status(&error, "FG-22F14");
	}

	/// Verifies rejects empty components.
	#[test]
	fn rejects_empty_components() {
		let empty_class = ":id".parse::<RecordId>().assert_err();
		assert_status(&empty_class, "FG-22F12");
		let empty_part = "person:".parse::<RecordId>().assert_err();
		assert_status(&empty_part, "FG-22F13");
		let missing_separator = "person".parse::<RecordId>().assert_err();
		assert_status(&missing_separator, "FG-22F11");
	}

	/// Verifies wraps composite parser errors as technical sources.
	#[test]
	fn wraps_composite_parser_errors_as_technical_sources() {
		let error = r#"person:["unterminated]"#.parse::<RecordId>().assert_err();
		assert_status(&error, "FG-22F15");
		assert!(error.technical_source().is_some());
		assert!(!error.to_string().contains("EOF"));
	}

	/// Verifies enforces the composite nesting limit.
	#[test]
	fn enforces_the_composite_nesting_limit() {
		let mut value = Value::from(1_i64);
		for _ in 0..=super::MAX_RECORD_ID_DEPTH {
			value = Value::Array(Array::new(vec![value]));
		}

		let error = RecordIdPart::try_from_value(value).assert_err();
		assert_status(&error, "FG-54F02");
	}

	/// Verifies quotes ambiguous string ids for round trip.
	#[test]
	fn quotes_ambiguous_string_ids_for_round_trip() {
		let value = RecordId::new("person", RecordIdPart::String("42".into())).assert_ok();
		assert_eq!(value.to_string(), r#"person:"42""#);
		assert_eq!(value.to_string().parse::<RecordId>().assert_ok(), value);
	}

	/// Returns assert status for this module.
	fn assert_status(error: &crate::gql_status::FerrosGrynnError, code: &str) {
		assert_eq!(error.status().code().to_string(), code);
		assert_eq!(error.status().domain(), Domain::Value);
	}
}
