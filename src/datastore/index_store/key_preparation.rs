//! Logical key construction and optional publication preparation admission.

use super::{IndexBinding, mutation::PreparedIndexChange};
use crate::{
	datastore::{BaseAtom, ClassIndexKind, GraphOccurrence},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::AtomId,
	values::{
		PropertyKeyId,
		Type,
		Value,
		storage::{EncodeContext, StorageValue},
	},
};

/// Logical callers retain legacy conversion; publication supplies a ceiling.
pub(super) struct KeyPreparation {
	/// No storage-owned policy or DTO escapes this private datastore seam.
	maximum: Option<usize>,
	/// Keys, new context and the prepared changes vector coexist until
	/// transfer.
	retained: usize,
}

impl KeyPreparation {
	/// Keeps existing logical setters independent of physical encoding limits.
	pub(super) const fn logical() -> Self {
		Self {
			maximum: None,
			retained: 0,
		}
	}

	/// Starts one preparation within the outer publication's remaining budget.
	pub(super) const fn bounded(maximum: usize) -> Self {
		Self {
			maximum: Some(maximum),
			retained: 0,
		}
	}

	/// Counts components before allocating; earlier keys reduce later
	/// allowance.
	pub(super) fn key<'value, Address>(
		&mut self,
		binding: &IndexBinding<Address>,
		mut value: impl FnMut(PropertyKeyId) -> Option<&'value Value>,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<Option<Vec<u8>>> {
		if self.maximum.is_none() {
			return index_key(binding, value, context);
		}
		if binding.kind == ClassIndexKind::Count {
			return Ok(None);
		}
		let bytes = array_bytes::<(&Value, &Type)>(binding.fields.len())?;
		self.scratch(bytes)?;
		let mut components = Vec::new();
		components.try_reserve_exact(binding.fields.len()).map_err(|source| {
			invalid("index component allocation failed").with_technical_source(source)
		})?;
		let missing = Value::None;
		for field in &binding.fields {
			components.push((value(field.key).unwrap_or(&missing), &field.value_type));
		}
		let (key, context_bytes) = StorageValue::encode_index_key_bounded(
			&components,
			context,
			self.remaining(array_bytes::<(&Value, &Type)>(components.capacity())?)?,
		)?;
		self.retain(add(context_bytes, key.capacity())?)?;
		Ok(Some(key))
	}

	/// Effective reads preserve the existing overlay/state inheritance rule.
	pub(super) fn effective_key(
		&mut self,
		binding: &IndexBinding<AtomId>,
		atom: &BaseAtom,
		occurrence: &GraphOccurrence,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<Option<Vec<u8>>> {
		self.key(
			binding,
			|key| occurrence.effective_value(atom.properties().get(key), key),
			context,
		)
	}

	/// Equal keys are dropped before releasing copies; context growth survives.
	pub(super) fn discard_equal(
		&mut self,
		before: Option<Vec<u8>>,
		after: Option<Vec<u8>>,
	) -> FerrosGrynnResult<()> {
		if self.maximum.is_none() {
			return Ok(());
		}
		let bytes =
			add(before.as_ref().map_or(0, Vec::capacity), after.as_ref().map_or(0, Vec::capacity))?;
		drop(before);
		drop(after);
		self.retained = self
			.retained
			.checked_sub(bytes)
			.ok_or_else(|| invalid("index key reservation underflows"))?;
		Ok(())
	}

	/// Both old and new vector backing are admitted before geometric growth.
	pub(super) fn push(
		&mut self,
		changes: &mut Vec<PreparedIndexChange>,
		change: PreparedIndexChange,
	) -> FerrosGrynnResult<()> {
		if self.maximum.is_some() && changes.len() == changes.capacity() {
			let old = changes.capacity();
			let next = old
				.checked_mul(2)
				.ok_or_else(|| invalid("index changes capacity overflows"))?
				.max(1);
			self.scratch(array_bytes::<PreparedIndexChange>(next)?)?;
			changes.try_reserve_exact(next - changes.len()).map_err(|source| {
				invalid("index changes allocation failed").with_technical_source(source)
			})?;
			self.retain(array_bytes::<PreparedIndexChange>(changes.capacity() - old)?)?;
		}
		changes.push(change);
		Ok(())
	}

	/// Reports persistent key/context ownership separately from transfer
	/// scratch.
	pub(super) fn finish(
		self,
		changes: Vec<PreparedIndexChange>,
	) -> FerrosGrynnResult<(Vec<PreparedIndexChange>, usize)> {
		let backing = array_bytes::<PreparedIndexChange>(changes.capacity())?;
		let persistent = self
			.retained
			.checked_sub(backing)
			.ok_or_else(|| invalid("index preparation accounting underflows"))?;
		Ok((changes, persistent))
	}

	/// Checks a live temporary beside all previously prepared keys and context.
	fn scratch(&self, bytes: usize) -> FerrosGrynnResult<()> {
		let actual = add(self.retained, bytes)?;
		if let Some(maximum) = self.maximum
			&& actual > maximum
		{
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"scalar index preparation",
				actual as u64,
				maximum as u64,
			));
		}
		Ok(())
	}

	/// Only owned results remain charged after a nested encoder returns.
	fn retain(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		self.scratch(bytes)?;
		self.retained = add(self.retained, bytes)?;
		Ok(())
	}

	/// Component backing is still live during the values-owned operation.
	fn remaining(&self, scratch: usize) -> FerrosGrynnResult<usize> {
		self.scratch(scratch)?;
		self.maximum
			.ok_or_else(|| invalid("bounded preparation has no ceiling"))?
			.checked_sub(add(self.retained, scratch)?)
			.ok_or_else(|| invalid("index preparation remaining bytes underflow"))
	}
}

/// Typed backing uses checked lengths before any reservation.
fn array_bytes<T>(count: usize) -> FerrosGrynnResult<usize> {
	count.checked_mul(size_of::<T>()).ok_or_else(|| invalid("index preparation backing overflows"))
}
/// Overflow never saturates into an apparently admissible allocation.
fn add(left: usize, right: usize) -> FerrosGrynnResult<usize> {
	left.checked_add(right).ok_or_else(|| invalid("index preparation bytes overflow"))
}
/// Preparation errors use the project's domain status boundary.
fn invalid(message: &'static str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_value_encoding(0, message)
}

/// Returns the index key result.
pub(super) fn index_key<'value, Address>(
	binding: &IndexBinding<Address>,
	mut value: impl FnMut(crate::values::PropertyKeyId) -> Option<&'value Value>,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Option<Vec<u8>>> {
	if binding.kind == ClassIndexKind::Count {
		return Ok(None);
	}
	let missing = Value::None;
	let components = binding
		.fields
		.iter()
		.map(|field| (value(field.key).unwrap_or(&missing), &field.value_type))
		.collect::<Vec<_>>();
	crate::values::storage::encode_index_key(&components, context).map(Some)
}

/// Returns the effective index key result.
pub(super) fn effective_index_key(
	binding: &IndexBinding<AtomId>,
	atom: &BaseAtom,
	occurrence: &GraphOccurrence,
	context: &mut EncodeContext,
) -> FerrosGrynnResult<Option<Vec<u8>>> {
	index_key(binding, |key| occurrence.effective_value(atom.properties().get(key), key), context)
}
