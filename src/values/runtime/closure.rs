use std::{
	cmp::Ordering,
	fmt::{self, Debug, Formatter},
	hash::{Hash, Hasher},
	num::NonZeroU64,
	ops::Fn,
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering as AtomicOrdering},
	},
};

use super::{Text, Type, Value};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// Shared next runtime id value.
static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local identity assigned to runtime-only callable values.
#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct RuntimeId(
	/// Numeric identity assigned within the current process.
	NonZeroU64,
);

impl RuntimeId {
	/// Returns the next runtime id.
	pub(crate) fn next() -> Self {
		loop {
			let value = NEXT_RUNTIME_ID.fetch_add(1, AtomicOrdering::Relaxed);
			if let Some(value) = NonZeroU64::new(value) {
				return Self(value);
			}
		}
	}

	/// Looks up the requested entry without modifying the collection.
	pub const fn get(self) -> u64 {
		self.0.get()
	}
}

/// Type used to represent closure function.
type ClosureFunction = dyn Fn(&[Value]) -> Result<Value, FerrosGrynnError> + Send + Sync + 'static;

#[derive(Clone)]
/// Closure implementation alternatives.
enum ClosureImplementation {
	/// Native case of closure implementation.
	Native(
		/// Native case of closure implementation.
		Arc<ClosureFunction>,
	),
	/// Session case of closure implementation.
	Session {
		/// Owner associated with this closure implementation.
		owner: RuntimeId,
	},
}

/// Runtime callable identity, signature and implementation.
#[derive(Clone)]
pub struct Closure {
	/// Identifier used to locate this value within its owning subsystem.
	id: RuntimeId,
	/// Name associated with this closure.
	name: Option<Text>,
	/// Parameters associated with this closure.
	parameters: Vec<Type>,
	/// Result associated with this closure.
	result: Type,
	/// Implementation associated with this closure.
	implementation: ClosureImplementation,
}

impl Closure {
	/// Constructs `Closure` from the supplied initial state.
	pub fn new<Func>(
		name: Option<Text>,
		parameters: Vec<Type>,
		result: Type,
		function: Func,
	) -> Self
	where
		Func: Fn(&[Value]) -> FerrosGrynnResult<Value> + Send + Sync + 'static,
	{
		Self {
			id: RuntimeId::next(),
			name,
			parameters,
			result,
			implementation: ClosureImplementation::Native(Arc::new(function)),
		}
	}

	/// Returns the session result.
	pub(crate) fn session(owner: RuntimeId, parameters: Vec<Type>, result: Type) -> Self {
		Self {
			id: RuntimeId::next(),
			name: None,
			parameters,
			result,
			implementation: ClosureImplementation::Session {
				owner,
			},
		}
	}

	/// Returns the stored id of this `Closure`.
	pub const fn id(&self) -> RuntimeId {
		self.id
	}

	/// Returns name for `Closure`.
	pub const fn name(&self) -> Option<&Text> {
		self.name.as_ref()
	}

	/// Returns name str for `Closure`.
	pub fn name_str(&self) -> Option<&str> {
		self.name.as_ref().map(Text::as_str)
	}

	/// Borrows the stored parameters of this `Closure`.
	pub fn parameters(&self) -> &[Type] {
		&self.parameters
	}

	/// Borrows the stored result of this `Closure`.
	pub fn result_type(&self) -> &Type {
		&self.result
	}

	/// Returns call for `Closure`.
	///
	/// # Errors
	///
	/// Returns `conversion_error`, `closure_requires_session` diagnostics.
	pub fn call(&self, arguments: &[Value]) -> Result<Value, FerrosGrynnError> {
		if arguments.len() != self.parameters.len() {
			return Err(FerrosGrynnError::conversion_error(
				format!("{} closure arguments", arguments.len()),
				format!("{} arguments", self.parameters.len()),
			));
		}
		match &self.implementation {
			ClosureImplementation::Native(function) => function(arguments),
			ClosureImplementation::Session {
				..
			} => Err(FerrosGrynnError::closure_requires_session(self.id.get())),
		}
	}

	/// Returns the session owner result.
	pub(crate) const fn session_owner(&self) -> Option<RuntimeId> {
		match &self.implementation {
			ClosureImplementation::Session {
				owner,
			} => Some(*owner),
			ClosureImplementation::Native(_) => None,
		}
	}

	/// Returns signature for `Closure`.
	pub fn signature(&self) -> Type {
		Type::Closure {
			parameters: self.parameters.clone(),
			result: Box::new(self.result.clone()),
		}
	}
}

impl Debug for Closure {
	/// Formats `Closure` using the requested formatter.
	fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
		f.debug_struct("Closure")
			.field("id", &self.id)
			.field("name", &self.name)
			.field("parameters", &self.parameters)
			.field("result", &self.result)
			.finish_non_exhaustive()
	}
}

impl PartialEq for Closure {
	/// Compares this `Closure` with another value for equality.
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}

impl Eq for Closure {}

impl PartialOrd for Closure {
	/// Compares this `Closure` with another value when they are ordered.
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Closure {
	/// Compares this `Closure` with another value using its ordering contract.
	fn cmp(&self, other: &Self) -> Ordering {
		self.id.cmp(&other.id)
	}
}

impl Hash for Closure {
	/// Feeds this `Closure` into the supplied hasher.
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.id.hash(state);
	}
}

#[cfg(test)]
/// Regression tests for closure behavior.
mod tests {
	use super::Closure;
	#[cfg(test)]
	use crate::test_assertions::AssertResult as _;
	use crate::values::{Type, Value};

	/// Verifies calls function and keeps process identity.
	#[test]
	fn calls_function_and_keeps_process_identity() {
		let closure =
			Closure::new(Some("identity".into()), vec![Type::Any], Type::Any, |arguments| {
				Ok(arguments.first().cloned().unwrap_or(Value::None))
			});
		assert_eq!(closure.call(&[Value::from(42_i64)]).assert_ok(), Value::from(42_i64));
		assert_eq!(closure, closure.clone());
	}

	/// Verifies different closures have different identity.
	#[test]
	fn different_closures_have_different_identity() {
		let left = Closure::new(None, vec![], Type::None, |_| Ok(Value::None));
		let right = Closure::new(None, vec![], Type::None, |_| Ok(Value::None));
		assert_ne!(left, right);
	}

	/// Verifies rejects wrong arity before invocation.
	#[test]
	fn rejects_wrong_arity_before_invocation() {
		let closure = Closure::new(None, vec![Type::Int], Type::Int, |_| Ok(Value::from(1_i64)));
		assert!(closure.call(&[]).is_err());
	}
}
