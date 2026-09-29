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
	id: RuntimeId,
	state: Arc<Mutex<MatcherState>>,
}

struct MatcherState {
	text: Text,
	pattern: Regex,
	byte_cursor: usize,
	scalar_cursor: usize,
	last: Option<(usize, usize)>,
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

	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		let state = self.state.lock();
		state.text.len().saturating_add(state.pattern.as_str().len())
	}
}

impl Debug for Matcher {
	fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
		formatter
			.debug_struct("Matcher")
			.field("id", &self.id)
			.field("start", &self.start())
			.field("end", &self.end())
			.finish()
	}
}

impl PartialEq for Matcher {
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}

impl Eq for Matcher {}

impl PartialOrd for Matcher {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Matcher {
	fn cmp(&self, other: &Self) -> Ordering {
		self.id.cmp(&other.id)
	}
}

impl Hash for Matcher {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.id.hash(state);
	}
}

#[cfg(test)]
mod tests {
	use super::Matcher;
	use crate::values::{Regex, Text, Value};

	#[test]
	fn clones_share_unicode_offsets_and_zero_length_patterns_terminate() {
		let matcher = Matcher::new(Text::from("åβ"), Regex::new("").unwrap());
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
