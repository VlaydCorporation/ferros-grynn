//! Lexical type contracts shared by session storage and both evaluators.

use std::{collections::BTreeMap, ops::Bound, sync::Arc};

use super::{ExecutionControl, execution::AtomFieldRead};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	values::{Type, Value},
};

/// One lexical declaration carries both type and reactive provenance.
#[derive(Clone)]
struct BindingType {
	/// Shared declaration, independent of the current frame value.
	ty: Arc<Type>,
	/// Identity lookup rather than an ordinary value capture.
	reactive: bool,
}

/// Immutable lexical metadata; rich types remain shared across savepoints.
#[derive(Clone, Default)]
pub struct BindingTypes {
	/// Empty scopes allocate no map; declarations and types remain shared
	/// after admission.
	entries: Option<Arc<BTreeMap<Box<str>, BindingType>>>,
	/// Conservative retained representation, including rich type payloads.
	bytes: usize,
}

impl BindingTypes {
	/// Resolves the declaration independently of its current runtime value.
	pub(crate) fn get(&self, name: &str) -> Option<&Type> {
		self.entry(name).map(|entry| entry.ty.as_ref())
	}

	/// A nearest ordinary declaration masks a reactive outer declaration.
	pub(crate) fn is_reactive(&self, name: &str) -> bool {
		self.entry(name).is_some_and(|entry| entry.reactive)
	}

	/// Admits lexical provenance together with its retained type contract.
	pub(crate) fn with_reactive_binding(
		&self,
		name: &str,
		control: &ExecutionControl,
		outer: &Self,
	) -> FerrosGrynnResult<Self> {
		self.with_declaration(name, &Type::Any, true, control, outer)
	}

	/// Admits the replacement map and type before copying either
	/// representation.
	pub(crate) fn with_binding(
		&self,
		name: &str,
		ty: &Type,
		control: &ExecutionControl,
		outer: &Self,
	) -> FerrosGrynnResult<Self> {
		self.with_declaration(name, ty, false, control, outer)
	}

	/// Borrows one immutable declaration without exposing map ownership.
	fn entry(&self, name: &str) -> Option<&BindingType> {
		self.entries.as_deref()?.get(name)
	}

	/// Copies admitted declaration entries for the new immutable scope.
	fn cloned_entries(&self) -> BTreeMap<Box<str>, BindingType> {
		self.entries.as_deref().map_or_else(BTreeMap::new, Clone::clone)
	}

	/// Keeps the declaration and provenance in one authoritative entry.
	fn with_declaration(
		&self,
		name: &str,
		ty: &Type,
		reactive: bool,
		control: &ExecutionControl,
		outer: &Self,
	) -> FerrosGrynnResult<Self> {
		let type_bytes = type_bytes(ty, control)?;
		let added = name
			.len()
			.checked_add(256)
			.and_then(|n| n.checked_add(type_bytes))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("lexical type metadata"))?;
		// Declarations do not replace a name within one lexical scope.
		let bytes = self
			.bytes
			.checked_add(added)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("lexical type metadata"))?;
		let retained = bytes
			.checked_add(outer.bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("lexical type metadata"))?;
		let _admission = Self::admit_copy(retained, control)?;
		let mut entries = self.cloned_entries();
		entries.insert(name.into(), BindingType {
			ty: Arc::new(ty.clone()),
			reactive,
		});
		Ok(Self {
			entries: Some(Arc::new(entries)),
			bytes,
		})
	}

	/// Exports an autocommit scope without losing declarations from its
	/// session.
	pub(crate) fn overlay(
		&self,
		outer: &Self,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Self> {
		let Some(local) = self.entries.as_deref() else {
			return Ok(outer.clone());
		};
		if local.is_empty() {
			return Ok(outer.clone());
		}
		let bytes = self
			.bytes
			.checked_add(outer.bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("session type metadata"))?;
		let _admission = Self::admit_copy(bytes, control)?;
		let mut entries = outer.cloned_entries();
		for (name, ty) in local {
			entries.insert(name.clone(), ty.clone());
		}
		Ok(Self {
			entries: Some(Arc::new(entries)),
			bytes,
		})
	}

	/// Caps retained metadata and its temporary copy before any allocation.
	fn admit_copy(
		bytes: usize,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<super::options::BarrierAccount> {
		control.check()?;
		let cap = 256 * 1024 * 1024;
		let mut account = control.barrier_account();
		if bytes > cap || !account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"lexical type metadata",
				cap as u64,
			));
		}
		Ok(account)
	}
}

/// Uses the existing checked rich-type walker before cloning a declaration.
fn type_bytes(ty: &Type, control: &ExecutionControl) -> FerrosGrynnResult<usize> {
	let mut total = 0_usize;
	super::live_lowering::admit_type_shape(ty, 0, 1, &mut |bytes, _work| {
		control.check()?;
		total = total
			.checked_add(bytes)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("lexical type size"))?;
		Ok(())
	})?;
	Ok(total)
}

/// Checks dynamic values before assignment, including every container member.
pub(super) fn validate_value(
	expected: &Type,
	value: &Value,
	control: &ExecutionControl,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<()> {
	if value_matches(expected, value, control, fields, 0)? {
		Ok(())
	} else {
		Err(FerrosGrynnError::invalid_argument(format!(
			"Lexical binding value of kind {:?} violates its type contract.",
			value.kind()
		)))
	}
}

/// Recurses only through the expected type, without constructing owned types.
fn value_matches(
	expected: &Type,
	value: &Value,
	control: &ExecutionControl,
	fields: Option<&dyn AtomFieldRead>,
	depth: u32,
) -> FerrosGrynnResult<bool> {
	control.check()?;
	if depth > control.max_control_depth {
		return Err(FerrosGrynnError::control_limit_exceeded(
			"lexical type depth",
			u64::from(control.max_control_depth),
		));
	}
	if let Some(fields) = fields {
		fields.work(1)?;
	}
	let matches = match (expected, value) {
		(Type::Any, _) => true,
		(Type::Number, Value::Number(_)) => true,
		(Type::Option(_), Value::None | Value::Null) => true,
		(Type::Option(inner), value) => value_matches(inner, value, control, fields, depth + 1)?,
		(Type::Union(types), value) => {
			let mut found = false;
			for ty in types {
				if value_matches(ty, value, control, fields, depth + 1)? {
					found = true;
					break;
				}
			}
			found
		}
		(Type::Array(inner), Value::Array(values)) => {
			for value in values.iter() {
				if !value_matches(inner, value, control, fields, depth + 1)? {
					return Ok(false);
				}
			}
			true
		}
		(Type::Set(inner), Value::Set(values)) => {
			for value in values.iter() {
				if !value_matches(inner, value, control, fields, depth + 1)? {
					return Ok(false);
				}
			}
			true
		}
		(Type::Range(inner), Value::Range(range)) => {
			for bound in [range.start(), range.end()] {
				if let Bound::Included(value) | Bound::Excluded(value) = bound
					&& !value_matches(inner, value, control, fields, depth + 1)?
				{
					return Ok(false);
				}
			}
			true
		}
		(Type::RecordShape(types), Value::Object(object)) => {
			for (name, ty) in types {
				if !value_matches(
					ty,
					object.get(name).unwrap_or(&Value::None),
					control,
					fields,
					depth + 1,
				)? {
					return Ok(false);
				}
			}
			true
		}
		(
			Type::Closure {
				parameters,
				result,
			},
			Value::Closure(closure),
		) => {
			parameters.as_slice() == closure.parameters()
				&& result.as_ref() == closure.result_type()
		}
		// A closure signature can own rich types; mismatches never clone it.
		(_, Value::Closure(_) | Value::Array(_) | Value::Set(_) | Value::Range(_)) => false,
		_ => super::analysis::compatible(expected, &value.value_type()),
	};
	Ok(matches)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		gql::QueryExecutionOptions,
		values::{Range, Set},
	};

	/// Erased public container types cannot hide incompatible members.
	#[test]
	fn lexical_type_validation_checks_sets_ranges_shapes_and_unions() -> FerrosGrynnResult<()> {
		let control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
		let set = Value::Set(Set::from_iter([Value::from(1), Value::from("bad")]));
		assert!(validate_value(&Type::Set(Box::new(Type::Int)), &set, &control, None).is_err());
		let range = Value::Range(Range::closed("a", "z")?);
		assert!(validate_value(&Type::Range(Box::new(Type::Int)), &range, &control, None).is_err());
		assert!(
			validate_value(&Type::Range(Box::new(Type::String)), &range, &control, None).is_ok()
		);
		let shape = Type::RecordShape(vec![("items".into(), Type::Array(Box::new(Type::Int)))]);
		let object = Value::Object(crate::values::Object::from_iter([(
			"items",
			Value::from(vec![Value::from("bad")]),
		)]));
		assert!(validate_value(&shape, &object, &control, None).is_err());
		let union = Type::Union(vec![Type::Int, Type::String]);
		assert!(validate_value(&union, &Value::from("ok"), &control, None).is_ok());
		assert!(validate_value(&union, &Value::Bool(true), &control, None).is_err());
		Ok(())
	}

	/// Metadata admission and validator interruption precede payload copies.
	#[test]
	fn lexical_types_observe_admission_and_cancellation() {
		let control = ExecutionControl::new(
			&QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::MIN,
				..QueryExecutionOptions::default()
			},
			None,
		);
		let types = BindingTypes::default();
		assert!(types.with_binding("x", &Type::Int, &control, &types).is_err());
		let cancellation = crate::gql::QueryCancellation::default();
		let control = ExecutionControl::new(
			&QueryExecutionOptions {
				cancellation: Some(cancellation.clone()),
				..QueryExecutionOptions::default()
			},
			None,
		);
		cancellation.cancel();
		assert!(validate_value(&Type::Int, &Value::from(1), &control, None).is_err());
	}
	/// Empty scopes, snapshots and lexical shadowing retain exact provenance.
	#[test]
	fn lazy_lexical_scopes_preserve_snapshots_and_reactive_shadowing() -> FerrosGrynnResult<()> {
		let control = ExecutionControl::new(&QueryExecutionOptions::default(), None);
		let empty = BindingTypes::default();
		assert!(empty.get("missing").is_none());
		assert!(!empty.is_reactive("missing"));
		let outer = empty.with_reactive_binding("tracked", &control, &empty)?;
		let snapshot = outer.clone();
		let exported = empty.overlay(&outer, &control)?;
		assert!(exported.is_reactive("tracked"));
		assert_eq!(exported.get("tracked"), Some(&Type::Any));
		let local = empty.with_binding("tracked", &Type::Int, &control, &outer)?;
		let shadowed = local.overlay(&outer, &control)?;
		assert_eq!(shadowed.get("tracked"), Some(&Type::Int));
		assert!(!shadowed.is_reactive("tracked"));
		assert_eq!(snapshot.get("tracked"), Some(&Type::Any));
		assert!(snapshot.is_reactive("tracked"));
		let rich = Type::Array(Box::new(Type::Int));
		let declared = local.with_binding("rich", &rich, &control, &outer)?;
		assert_eq!(declared.get("rich"), Some(&rich));
		assert!(local.get("rich").is_none());
		Ok(())
	}
}
