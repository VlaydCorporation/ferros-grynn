use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	str::FromStr,
};

use super::Text;

#[derive(Debug, Clone)]
pub struct Regex {
	pattern: Text,
	compiled: regex::Regex,
}

impl Regex {
	pub fn new(pattern: impl Into<Text>) -> Result<Self, regex::Error> {
		let pattern = pattern.into();
		let compiled = regex::Regex::new(pattern.as_str())?;
		Ok(Self {
			pattern,
			compiled,
		})
	}

	pub fn as_str(&self) -> &str {
		self.pattern.as_str()
	}

	pub const fn pattern(&self) -> &Text {
		&self.pattern
	}

	pub fn is_match(&self, value: &str) -> bool {
		self.compiled.is_match(value)
	}
}

impl FromStr for Regex {
	type Err = regex::Error;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		Self::new(value)
	}
}

impl Display for Regex {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		write!(formatter, "/{}/", self.pattern)
	}
}

impl PartialEq for Regex {
	fn eq(&self, other: &Self) -> bool {
		self.pattern == other.pattern
	}
}

impl Eq for Regex {}

impl PartialOrd for Regex {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Regex {
	fn cmp(&self, other: &Self) -> Ordering {
		self.pattern.cmp(&other.pattern)
	}
}

impl Hash for Regex {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.pattern.hash(state);
	}
}

#[cfg(test)]
mod tests {
	use super::Regex;

	#[test]
	fn compiles_and_matches_pattern() {
		let regex = Regex::new(r"^gr(a|y)ph$").unwrap();
		assert!(regex.is_match("graph"));
		assert!(!regex.is_match("graphs"));
	}
}
