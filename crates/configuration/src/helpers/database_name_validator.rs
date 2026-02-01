use crate::{Error, Result};
use once_cell::sync::Lazy;
use regex::Regex;

pub const MINIMUM_DATABASE_NAME_LENGTH: usize = 3;
pub const MAXIMUM_DATABASE_NAME_LENGTH: usize = 63;
static DATABASE_NAME_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new("^[a-z0-9-.]+$").unwrap());
static DATABASE_NAME_GLOBBING_PATTERN: Lazy<Regex> =
	Lazy::new(|| Regex::new("^[a-z0-9-.*?]+$").unwrap());

pub fn validate_external_database_name(name: &str) -> Result<()> {
	if name.starts_with("system") {
		Err(Error::Validation(format!(
			"Database name '{name}' is invalid, due to the prefix 'system'."
		)))
	} else {
		Ok(())
	}
}

pub fn validate_internal_database_name(name: &str) -> Result<()> {
	if name.is_empty() {
		Err(Error::Validation("The provided database name is empty.".to_string()))
	} else {
		if name.len() < MINIMUM_DATABASE_NAME_LENGTH || name.len() > MAXIMUM_DATABASE_NAME_LENGTH {
			return Err(Error::Validation(format!(
				"The provided database name must have a length between {MINIMUM_DATABASE_NAME_LENGTH} and {MAXIMUM_DATABASE_NAME_LENGTH} characters."
			)));
		}

		if !name.chars().peekable().peek().unwrap().is_ascii_lowercase() {
			return Err(Error::Validation(format!(
				"Database name '{name}' is not starting with an ASCII alphabetic character."
			)));
		}

		if !DATABASE_NAME_PATTERN.is_match(name) {
			return Err(Error::Validation(format!(
				"Database name '{name}' contains illegal characters. Use simple ascii characters, numbers, dots and dashes."
			)));
		}

		Ok(())
	}
}

pub fn validate_database_name_pattern(name: &str) -> Result<()> {
	if name.is_empty() {
		Err(Error::Validation("The provided database name is empty.".to_string()))
	} else {
		if name.trim().is_empty() {
			return Err(Error::Validation("The provided database name is empty.".to_string()));
		}

		let name = name.to_lowercase();

		if name.len() > MAXIMUM_DATABASE_NAME_LENGTH {
			return Err(Error::Validation(format!(
				"The provided database name must have a length between 1 and {MAXIMUM_DATABASE_NAME_LENGTH} characters."
			)));
		}

		if !DATABASE_NAME_GLOBBING_PATTERN.is_match(&name) {
			return Err(Error::Validation(format!(
				"Database name '{name}' contains illegal characters. Use simple ascii characters, numbers, dots, question marks, asterisk and dashes."
			)));
		}

		Ok(())
	}
}
