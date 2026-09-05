use std::{
	fmt::{self, Display, Formatter},
	str::FromStr,
};

use serde_json::Value as JsonValue;

use super::{Array, Number, Object, Text, Ulid, Uuid, Value};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

const MAX_RECORD_ID_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub enum RecordIdPart {
	Int(i64),
	String(Text),
	Uuid(Uuid),
	Ulid(Ulid),
	Array(Array),
	Object(Object),
}

impl RecordIdPart {
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

#[derive(Debug, Clone, PartialOrd, PartialEq, Eq, Ord, Hash)]
pub struct RecordId {
	table: Text,
	id: RecordIdPart,
}

impl RecordId {
	pub fn new<IntoText: Into<Text>>(table: IntoText, id: RecordIdPart) -> FerrosGrynnResult<Self> {
		let table = table.into();
		if table.is_empty() {
			return Err(FerrosGrynnError::empty_record_id_table());
		}
		id.validate()?;
		Ok(Self {
			table,
			id,
		})
	}

	pub fn from_value<IntoText: Into<Text>>(table: IntoText, id: Value) -> FerrosGrynnResult<Self> {
		Self::new(table, RecordIdPart::try_from_value(id)?)
	}

	pub fn table(&self) -> &str {
		self.table.as_str()
	}

	pub const fn table_text(&self) -> &Text {
		&self.table
	}

	pub const fn id(&self) -> &RecordIdPart {
		&self.id
	}

	pub fn into_parts(self) -> (Text, RecordIdPart) {
		(self.table, self.id)
	}

	/// Estimates heap bytes owned by the table and composite identifier.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		super::value::estimated_record_id_heap_bytes(self)
	}
}

impl Display for RecordIdPart {
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
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "{}:{}", self.table, self.id)
	}
}

impl FromStr for RecordId {
	type Err = FerrosGrynnError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let (table, id) =
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

		Self::new(table, id)
	}
}

fn string_id_needs_quotes(value: &str) -> bool {
	value.parse::<i64>().is_ok()
		|| value.parse::<Uuid>().is_ok()
		|| value.parse::<Ulid>().is_ok()
		|| value.chars().next().is_some_and(|value| matches!(value, '"' | '[' | '{'))
		|| value.chars().any(char::is_whitespace)
}

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
mod tests {
	use super::{RecordId, RecordIdPart};
	use crate::{
		gql_status::Domain,
		values::{Array, Object, Value},
	};

	#[test]
	fn parses_scalar_and_composite_ids() {
		let integer = "person:42".parse::<RecordId>().unwrap();
		assert_eq!(integer.id(), &RecordIdPart::Int(42));

		let composite = r#"person:[42,{"region":"eu"}]"#.parse::<RecordId>().unwrap();
		assert!(matches!(composite.id(), RecordIdPart::Array(_)));
	}

	#[test]
	fn recursively_rejects_unsupported_parts() {
		let mut object = Object::new();
		object.insert("invalid", true);
		let array = Array::new(vec![Value::Object(object)]);
		let error = RecordId::new("person", RecordIdPart::Array(array)).unwrap_err();
		assert_status(&error, "FG-22F14");
	}

	#[test]
	fn rejects_empty_components() {
		let empty_table = ":id".parse::<RecordId>().unwrap_err();
		assert_status(&empty_table, "FG-22F12");
		let empty_part = "person:".parse::<RecordId>().unwrap_err();
		assert_status(&empty_part, "FG-22F13");
		let missing_separator = "person".parse::<RecordId>().unwrap_err();
		assert_status(&missing_separator, "FG-22F11");
	}

	#[test]
	fn wraps_composite_parser_errors_as_technical_sources() {
		let error = r#"person:["unterminated]"#.parse::<RecordId>().unwrap_err();
		assert_status(&error, "FG-22F15");
		assert!(error.technical_source().is_some());
		assert!(!error.to_string().contains("EOF"));
	}

	#[test]
	fn enforces_the_composite_nesting_limit() {
		let mut value = Value::from(1_i64);
		for _ in 0..=super::MAX_RECORD_ID_DEPTH {
			value = Value::Array(Array::new(vec![value]));
		}

		let error = RecordIdPart::try_from_value(value).unwrap_err();
		assert_status(&error, "FG-54F02");
	}

	#[test]
	fn quotes_ambiguous_string_ids_for_round_trip() {
		let value = RecordId::new("person", RecordIdPart::String("42".into())).unwrap();
		assert_eq!(value.to_string(), r#"person:"42""#);
		assert_eq!(value.to_string().parse::<RecordId>().unwrap(), value);
	}

	fn assert_status(error: &crate::gql_status::FerrosGrynnError, code: &str) {
		assert_eq!(error.status().code().to_string(), code);
		assert_eq!(error.status().domain(), Domain::Value);
	}
}
