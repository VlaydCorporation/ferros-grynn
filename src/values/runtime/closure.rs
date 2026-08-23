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

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[repr(transparent)]
pub struct RuntimeId(NonZeroU64);

impl RuntimeId {
	fn next() -> Self {
		loop {
			let value = NEXT_RUNTIME_ID.fetch_add(1, AtomicOrdering::Relaxed);
			if let Some(value) = NonZeroU64::new(value) {
				return Self(value);
			}
		}
	}

	pub const fn get(self) -> u64 {
		self.0.get()
	}
}

type ClosureFunction = dyn Fn(&[Value]) -> Result<Value, FerrosGrynnError> + Send + Sync + 'static;

#[derive(Clone)]
pub struct Closure {
	id: RuntimeId,
	name: Option<Text>,
	parameters: Vec<Type>,
	result: Type,
	function: Arc<ClosureFunction>,
}

impl Closure {
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
			function: Arc::new(function),
		}
	}

	pub const fn id(&self) -> RuntimeId {
		self.id
	}

	pub const fn name(&self) -> Option<&Text> {
		self.name.as_ref()
	}

	pub fn name_str(&self) -> Option<&str> {
		self.name.as_ref().map(Text::as_str)
	}

	pub fn parameters(&self) -> &[Type] {
		&self.parameters
	}

	pub fn result_type(&self) -> &Type {
		&self.result
	}

	pub fn call(&self, arguments: &[Value]) -> Result<Value, FerrosGrynnError> {
		if arguments.len() != self.parameters.len() {
			return Err(FerrosGrynnError::conversion_error(
				format!("{} closure arguments", arguments.len()),
				format!("{} arguments", self.parameters.len()),
			));
		}
		(self.function)(arguments)
	}

	pub fn signature(&self) -> Type {
		Type::Closure {
			parameters: self.parameters.clone(),
			result: Box::new(self.result.clone()),
		}
	}
}

impl Debug for Closure {
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
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}

impl Eq for Closure {}

impl PartialOrd for Closure {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for Closure {
	fn cmp(&self, other: &Self) -> Ordering {
		self.id.cmp(&other.id)
	}
}

impl Hash for Closure {
	fn hash<H: Hasher>(&self, state: &mut H) {
		self.id.hash(state);
	}
}

#[cfg(test)]
mod tests {
	use super::Closure;
	use crate::values::{Type, Value};

	#[test]
	fn calls_function_and_keeps_process_identity() {
		let closure =
			Closure::new(Some("identity".into()), vec![Type::Any], Type::Any, |arguments| {
				Ok(arguments.first().cloned().unwrap_or(Value::None))
			});
		assert_eq!(closure.call(&[Value::from(42_i64)]).unwrap(), Value::from(42_i64));
		assert_eq!(closure, closure.clone());
	}

	#[test]
	fn different_closures_have_different_identity() {
		let left = Closure::new(None, vec![], Type::None, |_| Ok(Value::None));
		let right = Closure::new(None, vec![], Type::None, |_| Ok(Value::None));
		assert_ne!(left, right);
	}

	#[test]
	fn rejects_wrong_arity_before_invocation() {
		let closure = Closure::new(None, vec![Type::Int], Type::Int, |_| Ok(Value::from(1_i64)));
		assert!(closure.call(&[]).is_err());
	}
}
