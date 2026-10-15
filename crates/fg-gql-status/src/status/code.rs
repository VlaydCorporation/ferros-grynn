use std::{
	error::Error,
	fmt::{self, Display, Formatter},
	str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A validated status code with a five-character body and the textual FG-
/// prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatusCode(
	/// The five-byte ASCII body of the validated status code.
	[u8; 5],
);

impl StatusCode {
	/// Creates a validated five-byte status code body.
	///
	/// # Panics
	///
	/// Panics when `body` contains bytes outside ASCII uppercase letters and
	/// digits.
	pub const fn new(body: [u8; 5]) -> Self {
		let mut index = 0;
		while index < body.len() {
			let byte = body[index];
			assert!(
				(byte >= b'0' && byte <= b'9') || (byte >= b'A' && byte <= b'Z'),
				"status code contains an invalid byte"
			);
			index += 1;
		}
		Self(body)
	}

	/// Returns the five-character code body without the FG- prefix.
	pub const fn body(&self) -> &str {
		// SAFETY: constructors validate that every byte is ASCII.
		unsafe { std::str::from_utf8_unchecked(&self.0) }
	}
}

impl Display for StatusCode {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "FG-{}", self.body())
	}
}

impl FromStr for StatusCode {
	type Err = StatusCodeParseError;

	/// Parses the string representation of a status code.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		let body = value.strip_prefix("FG-").ok_or(StatusCodeParseError::MissingPrefix)?;
		if body.len() != 5 {
			return Err(StatusCodeParseError::InvalidLength);
		}
		let mut bytes = [0_u8; 5];
		bytes.copy_from_slice(body.as_bytes());
		if !bytes.iter().all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase()) {
			return Err(StatusCodeParseError::InvalidCharacter);
		}
		Ok(Self(bytes))
	}
}

impl Serialize for StatusCode {
	/// Writes the status code in its string wire representation.
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: Serializer,
	{
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for StatusCode {
	/// Parses and validates a string code from the wire representation.
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: Deserializer<'de>,
	{
		let value = String::deserialize(deserializer)?;
		value.parse().map_err(serde::de::Error::custom)
	}
}

/// The reason a status-code string representation was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatusCodeParseError {
	/// The string lacks the required FG- prefix.
	MissingPrefix,
	/// The code body does not contain exactly five characters.
	InvalidLength,
	/// The code body contains a disallowed ASCII character.
	InvalidCharacter,
}

impl Display for StatusCodeParseError {
	/// Formats the value using the corresponding text representation.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		match self {
			Self::MissingPrefix => f.write_str("status code must start with FG-"),
			Self::InvalidLength => f.write_str("status code body must contain five characters"),
			Self::InvalidCharacter => {
				f.write_str("status code must contain only ASCII uppercase letters and digits")
			}
		}
	}
}

impl Error for StatusCodeParseError {}
