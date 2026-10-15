use std::{
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	sync::Arc,
};

use parking_lot::Mutex;

use super::{Regex, RuntimeId, Text};

/// Stateful regular-expression cursor shared by cloned runtime values.
#[derive(Clone)]
pub struct Matcher {
	/// Identifier used to locate this value within its owning subsystem.
	id: RuntimeId,
	/// State associated with this matcher.
	state: Arc<Mutex<MatcherState>>,
}

/// Regex search input, byte and scalar cursors, and the most recently observed
/// match.
struct MatcherState {
	/// Text associated with this matcher state.
	text: Text,
	/// Pattern associated with this matcher state.
	pattern: Regex,
	/// Byte cursor associated with this matcher state.
	byte_cursor: usize,
	/// Scalar cursor associated with this matcher state.
	scalar_cursor: usize,
	/// Last associated with this matcher state.
	last: Option<(usize, usize)>,
	/// Exhausted associated with this matcher state.
	exhausted: bool,
}

impl Matcher {
	/// Creates a matcher positioned before the first match.
	pub fn new(text: Text, pattern: Regex) -> Self {
		Self {
			id: RuntimeId::next(),
			state: Arc::new(Mutex::new(MatcherState {
				text,
				pattern,
				byte_cursor: 0,
				scalar_cursor: 0,
				last: None,
				exhausted: false,
			})),
		}
	}

	/// Returns the stable runtime identity of this matcher.
	pub const fn id(&self) -> RuntimeId {
		self.id
	}

	/// Advances to the next match.
	///
	/// Zero-length matches advance over one Unicode scalar when possible, so a
	/// caller cannot loop forever at one byte position.
	pub fn advance(&self) -> bool {
		let mut state = self.state.lock();
		if state.exhausted {
			return false;
		}
		let Some((start, end)) = state.pattern.find_at(state.text.as_str(), state.byte_cursor)
		else {
			state.exhausted = true;
			return false;
		};
		let start_scalar = state
			.scalar_cursor
			.saturating_add(state.text.as_str()[state.byte_cursor..start].chars().count());
		let end_scalar =
			start_scalar.saturating_add(state.text.as_str()[start..end].chars().count());
		state.last = Some((start_scalar, end_scalar));
		if start != end {
			state.byte_cursor = end;
			state.scalar_cursor = end_scalar;
			return true;
		}
		let Some(character) = state.text.as_str()[end..].chars().next() else {
			state.exhausted = true;
			return true;
		};
		state.byte_cursor = end.saturating_add(character.len_utf8());
		state.scalar_cursor = end_scalar.saturating_add(1);
		true
	}

	/// Returns the Unicode-scalar start offset of the last successful match.
	pub fn start(&self) -> Option<usize> {
		self.state.lock().last.map(|(start, _)| start)
	}

	/// Returns the Unicode-scalar end offset of the last successful match.
	pub fn end(&self) -> Option<usize> {
		self.state.lock().last.map(|(_, end)| end)
	}

	/// Returns the estimated heap bytes result.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		let state = self.state.lock();
		state.text.len().saturating_add(state.pattern.as_str().len())
	}
}

impl Debug for Matcher {
	/// Formats `Matcher` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		let last = self.state.lock().last;
		f.debug_struct("Matcher")
			.field("id", &self.id)
			.field("start", &last.map(|(start, _)| start))
			.field("end", &last.map(|(_, end)| end))
			.finish()
	}
}

// Cursor mutation must not move an existing map key: equality, ordering and
// hashing use only id.
impl PartialEq for Matcher {
	/// Compares this `Matcher` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}

impl Eq for Matcher {}

impl PartialOrd for Matcher {
	/// Compares this `Matcher` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Matcher {
	/// Compares this `Matcher` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.id.cmp(&other.id)
	}
}

impl Hash for Matcher {
	/// Feeds this `Matcher` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.id.hash(state);
	}
}

#[cfg(test)]
/// Regression tests for matcher behavior.
mod tests {
	use super::Matcher;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{Regex, Text, Value};

	/// Verifies clones share unicode offsets and zero length patterns
	/// terminate.
	#[test]
	fn clones_share_unicode_offsets_and_zero_length_patterns_terminate() {
		let matcher = Matcher::new(Text::from("åβ"), Regex::new("").assert_ok());
		assert!(!Value::Matcher(matcher.clone()).is_storable());
		let clone = matcher.clone();
		assert!(matcher.advance());
		assert_eq!((clone.start(), clone.end()), (Some(0), Some(0)));
		assert!(clone.advance());
		assert_eq!((matcher.start(), matcher.end()), (Some(1), Some(1)));
		assert!(matcher.advance());
		assert_eq!((clone.start(), clone.end()), (Some(2), Some(2)));
		assert!(!clone.advance());
	}
}
