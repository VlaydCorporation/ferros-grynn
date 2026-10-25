//! Assertions shared by unit tests, integration fixtures and benchmark setup.

use std::fmt::Debug;

/// Reports a failed result at the assertion's caller and returns the expected
/// side.
pub trait AssertResult<T, E>: Sized {
	/// Returns the successful value and reports an unexpected error.
	///
	/// # Panics
	///
	/// Panics when this result contains an error.
	#[track_caller]
	fn assert_ok(self) -> T
	where
		E: Debug,
	{
		self.assert_ok_with("expected a successful result")
	}

	/// Returns the successful value, adding fixture context on failure.
	///
	/// # Panics
	///
	/// Panics with the supplied context when this result contains an error.
	#[track_caller]
	fn assert_ok_with(self, context: &str) -> T
	where
		E: Debug;

	/// Returns the expected error and reports an unexpected successful value.
	///
	/// # Panics
	///
	/// Panics when this result contains a successful value.
	#[track_caller]
	fn assert_err(self) -> E
	where
		T: Debug,
	{
		self.assert_err_with("expected an error result")
	}

	/// Returns the expected error, adding fixture context on failure.
	///
	/// # Panics
	///
	/// Panics with the supplied context when this result is successful.
	#[track_caller]
	fn assert_err_with(self, context: &str) -> E
	where
		T: Debug;
}

impl<T, E> AssertResult<T, E> for Result<T, E> {
	/// Asserts success before extracting the owned result value.
	#[track_caller]
	fn assert_ok_with(self, context: &str) -> T
	where
		E: Debug,
	{
		assert!(self.is_ok(), "{context}; actual error: {:?}", self.as_ref().err());
		match self {
			Ok(value) => value,
			Err(_) => unreachable!("success was asserted before extracting the value"),
		}
	}

	/// Asserts failure before extracting the owned error value.
	#[track_caller]
	fn assert_err_with(self, context: &str) -> E
	where
		T: Debug,
	{
		assert!(self.is_err(), "{context}; actual value: {:?}", self.as_ref().ok());
		match self {
			Err(error) => error,
			Ok(_) => unreachable!("failure was asserted before extracting the error"),
		}
	}
}

/// Reports an absent fixture value at the assertion's caller.
pub trait AssertOption<T>: Sized {
	/// Returns the present value without requiring it to implement `Debug`.
	///
	/// # Panics
	///
	/// Panics when this option is absent.
	#[track_caller]
	fn assert_some(self) -> T {
		self.assert_some_with("expected a present value")
	}

	/// Returns the present value, adding fixture context on failure.
	///
	/// # Panics
	///
	/// Panics with the supplied context when this option is absent.
	#[track_caller]
	fn assert_some_with<Message: std::fmt::Display>(self, context: Message) -> T;
}

impl<T> AssertOption<T> for Option<T> {
	/// Asserts presence before extracting the owned option value.
	#[track_caller]
	fn assert_some_with<Message: std::fmt::Display>(self, context: Message) -> T {
		assert!(self.is_some(), "{context}");
		match self {
			Some(value) => value,
			None => unreachable!("presence was asserted before extracting the value"),
		}
	}
}

/// Boolean assertions usable inside fallible test fixtures and callbacks.
pub trait AssertCondition {
	/// Requires a true condition.
	///
	/// # Panics
	/// Panics when the condition is false.
	#[track_caller]
	fn assert_true(self);

	/// Requires a true condition, evaluating the failure message only on
	/// failure.
	///
	/// # Panics
	/// Panics when the condition is false.
	#[track_caller]
	fn assert_true_with<Message: FnOnce() -> String>(self, message: Message);
}

impl AssertCondition for bool {
	/// Checks the condition at the fixture's call site.
	#[track_caller]
	fn assert_true(self) {
		assert!(self);
	}

	/// Checks the condition with a lazily formatted failure message.
	#[track_caller]
	fn assert_true_with<Message: FnOnce() -> String>(self, message: Message) {
		assert!(self, "{}", message());
	}
}

/// Borrowing comparisons that preserve values owned by a fallible fixture.
pub trait AssertComparison {
	/// Requires the two borrowed values to be equal.
	///
	/// # Panics
	/// Panics when the values differ.
	#[track_caller]
	fn assert_equal(self);

	/// Requires equality with a failure message evaluated only on failure.
	///
	/// # Panics
	/// Panics when the values differ.
	#[track_caller]
	fn assert_equal_with<Message: FnOnce() -> String>(self, message: Message);

	/// Requires the two borrowed values to differ.
	///
	/// # Panics
	/// Panics when the values are equal.
	#[track_caller]
	fn assert_different(self);

	/// Requires inequality with a failure message evaluated only on failure.
	///
	/// # Panics
	/// Panics when the values are equal.
	#[track_caller]
	fn assert_different_with<Message: FnOnce() -> String>(self, message: Message);
}

impl<Left: std::fmt::Debug + PartialEq<Right> + ?Sized, Right: std::fmt::Debug + ?Sized>
	AssertComparison for (&Left, &Right)
{
	/// Compares borrowed values without consuming either operand.
	#[track_caller]
	fn assert_equal(self) {
		assert_eq!(self.0, self.1);
	}

	/// Compares borrowed values with a lazily formatted equality failure.
	#[track_caller]
	fn assert_equal_with<Message: FnOnce() -> String>(self, message: Message) {
		assert_eq!(self.0, self.1, "{}", message());
	}

	/// Checks borrowed values for inequality without consuming either operand.
	#[track_caller]
	fn assert_different(self) {
		assert_ne!(self.0, self.1);
	}

	/// Checks inequality with a lazily formatted failure message.
	#[track_caller]
	fn assert_different_with<Message: FnOnce() -> String>(self, message: Message) {
		assert_ne!(self.0, self.1, "{}", message());
	}
}
