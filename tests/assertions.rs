#![cfg(test)]

#[path = "common/assertions.rs"]
pub mod test_assertions;

use test_assertions::{AssertComparison, AssertCondition, AssertOption, AssertResult};

/// Verifies that the assertions retain values without requiring irrelevant
/// Debug bounds.
#[test]
fn expected_sides_preserve_values_and_allow_opaque_values() {
	let opaque = || 7;
	assert_eq!(Ok::<_, &str>(opaque).assert_ok()(), 7);
	assert_eq!(Err::<u8, _>(opaque).assert_err()(), 7);
	assert_eq!(Some(opaque).assert_some()(), 7);
	assert_eq!(Ok::<_, &str>(3).assert_ok_with("fixture"), 3);
	assert_eq!(Err::<u8, _>("error").assert_err_with("fixture"), "error");
	assert_eq!(Some(5).assert_some_with("fixture"), 5);
}

/// Verifies that unexpected sides trigger assertion unwinding instead of
/// fallback values.
#[test]
fn unexpected_sides_unwind() {
	assert!(std::panic::catch_unwind(|| Err::<(), _>("error").assert_ok()).is_err());
	assert!(std::panic::catch_unwind(|| Ok::<_, ()>(4).assert_err()).is_err());
	assert!(std::panic::catch_unwind(|| None::<u8>.assert_some()).is_err());
}

/// Verifies borrowed comparisons and lazy messages match ordinary assertions.
#[test]
fn fixture_assertions_preserve_operands_and_lazy_messages() {
	let left = String::from("graph");
	let right = String::from("graph");
	(&left, &right).assert_equal();
	(&left, &"different").assert_different();
	true.assert_true();
	let messages = std::cell::Cell::new(0);
	let message = || {
		messages.set(messages.get() + 1);
		String::from("fixture failure")
	};
	true.assert_true_with(message);
	(&left, &right).assert_equal_with(message);
	(&left, &"different").assert_different_with(message);
	assert_eq!(messages.get(), 0);
	assert_eq!((left.as_str(), right.as_str()), ("graph", "graph"));
	assert!(std::panic::catch_unwind(|| false.assert_true()).is_err());
	assert!(std::panic::catch_unwind(|| (&1, &2).assert_equal()).is_err());
	assert!(std::panic::catch_unwind(|| (&1, &1).assert_different()).is_err());
	assert!(
		std::panic::catch_unwind(|| false.assert_true_with(|| String::from("failure"))).is_err()
	);
	assert!(
		std::panic::catch_unwind(|| (&1, &2).assert_equal_with(|| String::from("failure")))
			.is_err()
	);
	assert!(
		std::panic::catch_unwind(|| (&1, &1).assert_different_with(|| String::from("failure")))
			.is_err()
	);
}
