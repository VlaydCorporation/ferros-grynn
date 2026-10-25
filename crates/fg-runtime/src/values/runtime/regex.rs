use std::{
	cmp::Ordering,
	fmt::{self, Display, Formatter},
	hash::{Hash, Hasher},
	str::FromStr,
};

use super::Text;

/// Compiled regular expression retaining its original pattern text.
#[derive(Debug, Clone)]
pub struct Regex {
	/// Pattern associated with this regex.
	pattern: Text,
	/// Compiled associated with this regex.
	compiled: regex::Regex,
}

impl Regex {
	/// Constructs `Regex` from the supplied initial state.
	///
	/// # Errors
	///
	/// Propagates errors returned by the underlying operation.
	pub fn new<IntoText: Into<Text>>(pattern: IntoText) -> Result<Self, regex::Error> {
		let pattern = pattern.into();
		let compiled = regex::Regex::new(pattern.as_str())?;
		Ok(Self {
			pattern,
			compiled,
		})
	}

	/// Borrows str for `Regex`.
	pub fn as_str(&self) -> &str {
		self.pattern.as_str()
	}

	/// Borrows the stored pattern of this `Regex`.
	pub const fn pattern(&self) -> &Text {
		&self.pattern
	}

	/// Reports whether the value is match for `Regex`.
	pub fn is_match(&self, value: &str) -> bool {
		self.compiled.is_match(value)
	}

	/// Finds at.
	pub(crate) fn find_at(&self, value: &str, start: usize) -> Option<(usize, usize)> {
		self.compiled.find_at(value, start).map(|value| (value.start(), value.end()))
	}
}

impl FromStr for Regex {
	type Err = regex::Error;

	/// Constructs from str for `Regex`.
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		Self::new(value)
	}
}

impl Display for Regex {
	/// Formats `Regex` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		write!(f, "/{}/", self.pattern)
	}
}

// The regex engine may mutate search caches; collection-key identity uses only
// immutable pattern text.
impl PartialEq for Regex {
	/// Compares this `Regex` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.pattern == other.pattern
	}
}

impl Eq for Regex {}

impl PartialOrd for Regex {
	/// Compares this `Regex` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Regex {
	/// Compares this `Regex` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.pattern.cmp(&other.pattern)
	}
}

impl Hash for Regex {
	/// Feeds this `Regex` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.pattern.hash(state);
	}
}

/// Operations shared with the engine and value codecs.
pub trait RegexInternal: Sized {
	/// Finds at.
	fn find_at(&self, value: &str, start: usize) -> Option<(usize, usize)>;
}
impl RegexInternal for Regex {
	fn find_at(&self, value: &str, start: usize) -> Option<(usize, usize)> {
		<Regex>::find_at(self, value, start)
	}
}

#[cfg(test)]
/// Regression tests for regex behavior.
mod tests {
	use super::Regex;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;

	/// Verifies compiles and matches pattern.
	#[test]
	fn compiles_and_matches_pattern() {
		let regex = Regex::new(r"^gr(a|y)ph$").assert_ok();
		assert!(regex.is_match("graph"));
		assert!(!regex.is_match("graphs"));
	}
}
