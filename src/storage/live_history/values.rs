//! Bounded logical result values; property storage retains its existing type
//! contract.

use std::{ops::Bound, sync::Arc};

use super::Work;
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{EndpointRole, Path, PathDirection, PathStep, SnapshotId},
	storage::history_codec::{
		CodecBudget,
		Decoder,
		append,
		append_blob,
		invalid,
		qualified,
		values,
	},
	values::{Array, Object, Range, Set, Value, storage::EncodeContext},
};

/// Writes compounds explicitly so nested qualified references remain
/// recoverable.
pub(super) fn encode(
	output: &mut Vec<u8>,
	value: &Value,
	context: &mut EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<()> {
	work.take(depth, 1)?;
	match value {
		Value::Array(items) => {
			append(output, &[1], budget)?;
			values::write_count(output, items.len(), budget)?;
			for item in items {
				encode(output, item, context, budget, work, depth + 1)?;
			}
		}
		Value::Object(items) => {
			append(output, &[2], budget)?;
			values::write_count(output, items.len(), budget)?;
			for (name, item) in items {
				values::append_text(output, name, budget)?;
				encode(output, item, context, budget, work, depth + 1)?;
			}
		}
		Value::Set(items) => {
			append(output, &[3], budget)?;
			values::write_count(output, items.len(), budget)?;
			for item in items {
				encode(output, item, context, budget, work, depth + 1)?;
			}
		}
		Value::Range(range) => {
			append(output, &[4], budget)?;
			for bound in [range.start(), range.end()] {
				let (tag, item) = match bound {
					Bound::Unbounded => (0, None),
					Bound::Included(item) => (1, Some(item)),
					Bound::Excluded(item) => (2, Some(item)),
				};
				append(output, &[tag], budget)?;
				if let Some(item) = item {
					encode(output, item, context, budget, work, depth + 1)?;
				}
			}
		}
		Value::AtomOccurrence(occurrence) => {
			append(output, &[5], budget)?;
			qualified::encode_occurrence(output, occurrence, context, budget)?;
		}
		Value::Path(path) => {
			append(output, &[6], budget)?;
			append(output, path.origin().as_inner().as_bytes(), budget)?;
			append(output, &path.snapshot().timestamp().to_le_bytes(), budget)?;
			append(output, &path.snapshot().transaction().to_le_bytes(), budget)?;
			values::write_count(output, path.steps().len(), budget)?;
			for step in path.steps() {
				work.take(depth + 1, 1)?;
				qualified::encode_occurrence(output, step.atom(), context, budget)?;
				append(output, &[u8::from(step.via_edge().is_some())], budget)?;
				if let Some(edge) = step.via_edge() {
					qualified::encode_occurrence(output, edge, context, budget)?;
				}
				let direction = match step.direction() {
					PathDirection::Forward => 0,
					PathDirection::Reverse => 1,
					PathDirection::Undirected => 2,
				};
				let role = match step.role() {
					EndpointRole::Source => 0,
					EndpointRole::Target => 1,
					EndpointRole::Undirected => 2,
				};
				append(output, &[direction, role, 0], budget)?;
			}
			append(output, &[u8::from(path.cost().is_some())], budget)?;
			if let Some(cost) = path.cost() {
				let bytes = values::encode(&Value::Number(cost), context, budget)?;
				append_blob(output, &bytes, budget)?;
			}
		}
		_ => {
			append(output, &[0], budget)?;
			let bytes = values::encode(value, context, budget)?;
			append_blob(output, &bytes, budget)?;
		}
	}
	Ok(())
}

/// Admits collection nodes before consuming attacker-controlled items.
pub(super) fn reserve<T>(count: usize, budget: &mut CodecBudget) -> FerrosGrynnResult<Vec<T>> {
	budget.admit(
		count
			.checked_mul(std::mem::size_of::<T>())
			.ok_or_else(|| invalid("LIVE history allocation overflows"))?,
	)?;
	let mut output = Vec::new();
	output.try_reserve_exact(count).map_err(|_error| invalid("LIVE history allocation failed"))?;
	Ok(output)
}

/// Decodes an entire logical value without consulting primary or schema state.
pub(super) fn decode(
	decoder: &mut Decoder<'_>,
	context: &EncodeContext,
	budget: &mut CodecBudget,
	work: &mut Work,
	depth: usize,
) -> FerrosGrynnResult<Value> {
	work.take(depth, 1)?;
	budget.admit(std::mem::size_of::<Value>())?;
	Ok(match decoder.u8()? {
		0 => {
			let value = values::decode(decoder.blob()?, context, budget)?;
			if matches!(value, Value::Array(_) | Value::Object(_) | Value::Set(_) | Value::Range(_))
			{
				return Err(invalid("LIVE compound value used a scalar encoding"));
			}
			value
		}
		1 => {
			let count = values::count(decoder, 1)?;
			work.check_count(count)?;
			let mut items = reserve::<Value>(count, budget)?;
			for _ in 0..count {
				items.push(decode(decoder, context, budget, work, depth + 1)?);
			}
			Value::Array(Array::from(items))
		}
		2 => {
			let count = values::count(decoder, 9)?;
			work.check_count(count)?;
			budget.admit(crate::values::storage::tree_allocation_bytes(
				count,
				std::mem::size_of::<(crate::values::Text, Value)>(),
			)?)?;
			let mut items = Object::new();
			for _ in 0..count {
				let name = values::text(decoder, budget)?;
				if items.iter().next_back().is_some_and(|(previous, _)| previous >= &name) {
					return Err(invalid("LIVE object keys are not sorted distinct"));
				}
				items.insert(name, decode(decoder, context, budget, work, depth + 1)?);
			}
			Value::Object(items)
		}
		3 => {
			let count = values::count(decoder, 1)?;
			work.check_count(count)?;
			budget.admit(crate::values::storage::tree_allocation_bytes(
				count,
				std::mem::size_of::<Value>(),
			)?)?;
			let mut items = Set::new();
			for _ in 0..count {
				let item = decode(decoder, context, budget, work, depth + 1)?;
				if items.iter().next_back().is_some_and(|previous| previous >= &item) {
					return Err(invalid("LIVE set members are not sorted distinct"));
				}
				if !items.insert(item) {
					return Err(invalid("LIVE set member is duplicated"));
				}
			}
			Value::Set(items)
		}
		4 => {
			budget.admit(2 * std::mem::size_of::<Value>())?;
			let mut bound = || -> FerrosGrynnResult<Bound<Value>> {
				Ok(match decoder.u8()? {
					0 => Bound::Unbounded,
					1 => Bound::Included(decode(decoder, context, budget, work, depth + 1)?),
					2 => Bound::Excluded(decode(decoder, context, budget, work, depth + 1)?),
					_ => return Err(invalid("LIVE range boundary is invalid")),
				})
			};
			Value::Range(Range::new_with_diagnostic_admission(bound()?, bound()?, |_, _| {
				Err(invalid("LIVE range bounds are invalid"))
			})?)
		}
		5 => {
			budget.admit(std::mem::size_of::<crate::graph::AtomOccurrenceRef>())?;
			Value::from(qualified::decode_occurrence(decoder, context, budget)?)
		}
		6 => {
			let origin = uuid::Uuid::from_slice(decoder.take(16)?)
				.map_err(|_error| invalid("LIVE path origin is malformed"))?
				.into();
			let snapshot = SnapshotId::new(decoder.u64()?, decoder.u64()?);
			let count = values::count(decoder, 20)?;
			work.check_count(count)?;
			// Vec to Arc conversion may temporarily retain both allocations.
			budget.admit(
				count
					.checked_mul(std::mem::size_of::<PathStep>())
					.ok_or_else(|| invalid("LIVE path allocation overflows"))?
					+ 64,
			)?;
			let mut steps = reserve::<PathStep>(count, budget)?;
			for _ in 0..count {
				work.take(depth + 1, 1)?;
				let atom = qualified::decode_occurrence(decoder, context, budget)?;
				let edge = match decoder.u8()? {
					0 => None,
					1 => Some(qualified::decode_occurrence(decoder, context, budget)?),
					_ => return Err(invalid("LIVE path edge presence is invalid")),
				};
				let direction = match decoder.u8()? {
					0 => PathDirection::Forward,
					1 => PathDirection::Reverse,
					2 => PathDirection::Undirected,
					_ => return Err(invalid("LIVE path direction is invalid")),
				};
				let role = match decoder.u8()? {
					0 => EndpointRole::Source,
					1 => EndpointRole::Target,
					2 => EndpointRole::Undirected,
					_ => return Err(invalid("LIVE path role is invalid")),
				};
				decoder.zero(1)?;
				steps.push(PathStep::new(atom, edge, direction, role));
			}
			let cost = match decoder.u8()? {
				0 => None,
				1 => match values::decode(decoder.blob()?, context, budget)? {
					Value::Number(number) => Some(number),
					_ => return Err(invalid("LIVE path cost is not a number")),
				},
				_ => return Err(invalid("LIVE path cost presence is invalid")),
			};
			Value::from(Path::new(origin, snapshot, Arc::<[PathStep]>::from(steps), cost))
		}
		_ => return Err(invalid("LIVE history value tag is unknown")),
	})
}
