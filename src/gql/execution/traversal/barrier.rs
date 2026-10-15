//! Bounded traversal barriers backed by the shared transient query-run format.

use std::{collections::VecDeque, num::NonZeroU64};

use super::*;
use crate::gql::{
	cursor::{CursorAnchor, CursorRecord},
	spill::{QuerySorter, RecordStream, SortField},
};

/// Reports whether the value is barrier for this module.
pub(super) const fn is_barrier(kind: TraverseStepKind) -> bool {
	kind.is_barrier()
}

/// Traversal barrier output retained in memory or streamed from sorted records.
pub(super) enum BarrierCursor {
	/// Traversers retained in an admitted in-memory queue.
	Buffered {
		/// Traversers buffered until the barrier can produce output.
		values: VecDeque<Traverser>,
		/// Memory admission retained for the lifetime of the buffered values.
		account: BarrierAccount,
	},
	/// Sorted records streamed from the external record sorter.
	Records(
		/// Sorted records streamed from the external record sorter.
		RecordStream,
	),
	/// Grouped records streamed with one retained lookahead record.
	Groups {
		/// Upstream stream polled by this adapter.
		stream: RecordStream,
		/// Lookahead record retained while detecting the next group boundary.
		pending: Option<CursorRecord>,
	},
}

impl BarrierCursor {
	/// Advances this `BarrierCursor` and returns its next element, if any.
	pub(super) fn next<Read: QueryRead>(
		&mut self,
		machine: &mut TraversalMachine<'_, Read>,
	) -> FerrosGrynnResult<Option<Traverser>> {
		match self {
			Self::Buffered {
				values,
				account,
			} => {
				let value = values.pop_front();
				if let Some(value) = &value {
					account.release(value.estimated_heap_bytes());
				}
				Ok(value)
			}
			Self::Records(stream) => {
				stream.next()?.map(|record| machine.traverser_from_record(record)).transpose()
			}
			Self::Groups {
				stream,
				pending,
			} => next_group(machine, stream, pending),
		}
	}
}

impl<Read: QueryRead> TraversalMachine<'_, Read> {
	/// Prepares barrier for `TraversalMachine<'_, Read>`.
	pub(super) fn prepare_barrier(
		&mut self,
		upstream: usize,
		step: &PreparedStep,
	) -> FerrosGrynnResult<BarrierCursor> {
		match step.kind {
			TraverseStepKind::Order => self.sorted_records(upstream, step, SortPurpose::Order),
			TraverseStepKind::Deduplicate | TraverseStepKind::DeduplicateBy => {
				self.deduplicated_records(upstream, step)
			}
			TraverseStepKind::Group => {
				self.sorted_stream(upstream, step, SortPurpose::Group).map(|stream| {
					BarrierCursor::Groups {
						stream,
						pending: None,
					}
				})
			}
			TraverseStepKind::Fold => {
				self.fold(upstream).and_then(|value| buffered_value(value, self.control))
			}
			TraverseStepKind::Count => {
				self.count(upstream).and_then(|value| buffered_value(value, self.control))
			}
			TraverseStepKind::Sum
			| TraverseStepKind::Minimum
			| TraverseStepKind::Maximum
			| TraverseStepKind::Average => self
				.aggregate_stream(upstream, step.kind)
				.and_then(|value| buffered_value(value, self.control)),
			kind => self.algorithm_barrier(upstream, kind, step),
		}
	}

	/// Returns sorted records for `TraversalMachine<'_, Read>`.
	fn sorted_records(
		&mut self,
		upstream: usize,
		step: &PreparedStep,
		purpose: SortPurpose,
	) -> FerrosGrynnResult<BarrierCursor> {
		self.sorted_stream(upstream, step, purpose).map(BarrierCursor::Records)
	}

	/// Returns sorted stream for `TraversalMachine<'_, Read>`.
	fn sorted_stream(
		&mut self,
		upstream: usize,
		step: &PreparedStep,
		purpose: SortPurpose,
	) -> FerrosGrynnResult<RecordStream> {
		let root = self
			.control
			.temporary_directory
			.clone()
			.unwrap_or_else(|| self.read.query_scratch_root());
		let mut sorter = QuerySorter::new(
			&root,
			self.read.spill_context(self.snapshot)?,
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			self.control.barrier_account(),
			false,
		);
		let mut ordinal = 0_usize;
		while let Some(traverser) = self.pull(upstream)? {
			self.control.check()?;
			let key = match purpose {
				SortPurpose::Order if step.arguments.is_empty() => traverser.current.clone(),
				SortPurpose::Order => {
					self.invoke_argument(step, 0, std::slice::from_ref(&traverser.current), false)?
				}
				SortPurpose::Group if step.arguments.is_empty() => traverser.current.clone(),
				SortPurpose::Group if matches!(step.arguments.first(), Some(Value::Closure(_))) => {
					self.invoke_argument(step, 0, std::slice::from_ref(&traverser.current), false)?
				}
				SortPurpose::Group => {
					let mut fields = Vec::with_capacity(step.arguments.len());
					for field in &step.arguments {
						let field = field.as_str().ok_or_else(|| {
							FerrosGrynnError::traversal_source_type_mismatch(
								"group field string".to_owned(),
								field.kind().to_string(),
							)
						})?;
						fields.push(self.field_value(&traverser.current, field)?);
					}
					if fields.len() == 1 {
						fields.pop().unwrap_or(Value::None)
					} else {
						Value::Array(fields.into())
					}
				}
			};
			sorter.push(self.traverser_record(traverser, key, ordinal)?)?;
			ordinal = ordinal
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal barrier ordinal"))?;
		}
		sorter.finish()?.into_stream()
	}

	/// Returns deduplicated records for `TraversalMachine<'_, Read>`.
	fn deduplicated_records(
		&mut self,
		upstream: usize,
		step: &PreparedStep,
	) -> FerrosGrynnResult<BarrierCursor> {
		let root = self
			.control
			.temporary_directory
			.clone()
			.unwrap_or_else(|| self.read.query_scratch_root());
		let mut keyed = QuerySorter::new(
			&root,
			self.read.spill_context(self.snapshot)?,
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			self.control.barrier_account(),
			false,
		);
		let mut ordinal = 0_usize;
		while let Some(traverser) = self.pull(upstream)? {
			let key = if step.kind == TraverseStepKind::DeduplicateBy {
				self.invoke_argument(step, 0, std::slice::from_ref(&traverser.current), false)?
			} else {
				traverser.current.clone()
			};
			keyed.push(self.traverser_record(traverser.with_single_bulk(), key, ordinal)?)?;
			ordinal = ordinal
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal dedup ordinal"))?;
		}
		let mut encounter = QuerySorter::new(
			&root,
			self.read.spill_context(self.snapshot)?,
			vec![SortField {
				descending: false,
				nulls_first: false,
			}],
			self.control.barrier_account(),
			false,
		);
		let mut stream = keyed.finish()?.into_stream()?;
		let mut previous = None::<Value>;
		while let Some(mut record) = stream.next()? {
			let key = record.anchor.order_keys.first().cloned().unwrap_or(Value::None);
			if previous.as_ref() == Some(&key) {
				continue;
			}
			previous = Some(key);
			let ordinal = record.anchor.stable_key.first().cloned().unwrap_or(Value::None);
			record.anchor.order_keys = vec![ordinal.clone()];
			record.anchor.stable_key = vec![ordinal];
			encounter.push(record)?;
		}
		encounter.finish()?.into_stream().map(BarrierCursor::Records)
	}

	/// Returns fold for `TraversalMachine<'_, Read>`.
	fn fold(&mut self, upstream: usize) -> FerrosGrynnResult<Value> {
		let mut account = self.control.barrier_account();
		let mut values = Vec::new();
		while let Some(traverser) = self.pull(upstream)? {
			for logical in 0..traverser.bulk.get() {
				if logical & 1023 == 0 {
					self.control.check()?;
				}
				let bytes = std::mem::size_of::<Value>()
					.saturating_add(traverser.current.estimated_heap_bytes());
				if !account.reserve(bytes) {
					return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
						"fold result".to_owned(),
						self.control.barrier_memory_limit() as u64,
					));
				}
				reserve_slot(&mut values, &mut account, self.control)?;
				values.push(traverser.current.clone());
			}
		}
		Ok(Value::Array(values.into()))
	}

	/// Returns count for `TraversalMachine<'_, Read>`.
	fn count(&mut self, upstream: usize) -> FerrosGrynnResult<Value> {
		let mut count = 0_u64;
		while let Some(traverser) = self.pull(upstream)? {
			count = count
				.checked_add(traverser.bulk.get())
				.ok_or_else(FerrosGrynnError::traversal_bulk_overflow)?;
		}
		i64::try_from(count)
			.map(Value::from)
			.map_err(|_error| FerrosGrynnError::operation_overflow("tv::count"))
	}

	/// Returns aggregate stream for `TraversalMachine<'_, Read>`.
	fn aggregate_stream(
		&mut self,
		upstream: usize,
		kind: TraverseStepKind,
	) -> FerrosGrynnResult<Value> {
		let mut aggregate = None;
		let mut count = 0_u64;
		while let Some(traverser) = self.pull(upstream)? {
			if matches!(traverser.current, Value::None | Value::Null) {
				continue;
			}
			for logical in 0..traverser.bulk.get() {
				if logical & 1023 == 0 {
					self.control.check()?;
				}
				aggregate = Some(match (kind, aggregate) {
					(TraverseStepKind::Sum | TraverseStepKind::Average, None) => {
						traverser.current.clone()
					}
					(TraverseStepKind::Sum | TraverseStepKind::Average, Some(value)) => {
						binary(BinaryOperator::Add, value, traverser.current.clone())?
					}
					(TraverseStepKind::Minimum, None) => traverser.current.clone(),
					(TraverseStepKind::Minimum, Some(value)) => {
						value.min(traverser.current.clone())
					}
					(TraverseStepKind::Maximum, None) => traverser.current.clone(),
					(TraverseStepKind::Maximum, Some(value)) => {
						value.max(traverser.current.clone())
					}
					_ => {
						return Err(FerrosGrynnError::invalid_argument(
							"Invalid traversal aggregate state.",
						));
					}
				});
				count =
					count.checked_add(1).ok_or_else(FerrosGrynnError::traversal_bulk_overflow)?;
			}
		}
		match (kind, aggregate) {
			(TraverseStepKind::Sum, None) => Ok(Value::from(0_i64)),
			(
				TraverseStepKind::Average | TraverseStepKind::Minimum | TraverseStepKind::Maximum,
				None,
			) => Ok(Value::None),
			(TraverseStepKind::Average, Some(value)) => binary(
				BinaryOperator::Divide,
				value,
				Value::from(
					i64::try_from(count)
						.map_err(|_error| FerrosGrynnError::operation_overflow("tv::avg"))?,
				),
			),
			(_, Some(value)) => Ok(value),
			_ => Err(FerrosGrynnError::invalid_argument("Invalid traversal aggregate operation.")),
		}
	}

	/// Returns algorithm barrier for `TraversalMachine<'_, Read>`.
	fn algorithm_barrier(
		&mut self,
		upstream: usize,
		kind: TraverseStepKind,
		step: &PreparedStep,
	) -> FerrosGrynnResult<BarrierCursor> {
		let mut account = self.control.barrier_account();
		let mut input = Vec::new();
		while let Some(traverser) = self.pull(upstream)? {
			if !account.reserve(traverser.estimated_heap_bytes()) {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"graph algorithm input".to_owned(),
					self.control.barrier_memory_limit() as u64,
				));
			}
			reserve_slot(&mut input, &mut account, self.control)?;
			input.push(traverser);
		}
		let values = self.execute_algorithm(kind, step, input)?;
		drop(account);
		buffered_values(values, self.control)
	}

	/// Returns traverser record for `TraversalMachine<'_, Read>`.
	fn traverser_record(
		&self,
		traverser: Traverser,
		key: Value,
		ordinal: usize,
	) -> FerrosGrynnResult<CursorRecord> {
		let trace = traverser
			.trace
			.map(|_| self.materialize_path(&traverser).map(Value::from))
			.transpose()?
			.unwrap_or(Value::None);
		let bulk = traverser.bulk.get();
		let value = Value::Array(
			vec![
				traverser.current,
				trace,
				traverser.sack,
				Value::Object(traverser.named_sacks),
				Value::Object(traverser.env),
				Value::from(i64::from((bulk >> 32) as u32)),
				Value::from(i64::from(bulk as u32)),
			]
			.into(),
		);
		Ok(CursorRecord {
			value,
			anchor: CursorAnchor {
				order_keys: vec![key],
				stable_key: vec![Value::from(ordinal)],
			},
		})
	}

	/// Returns traverser from record for `TraversalMachine<'_, Read>`.
	fn traverser_from_record(&mut self, record: CursorRecord) -> FerrosGrynnResult<Traverser> {
		let Value::Array(values) = record.value else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"traversal run record is not an array",
			));
		};
		let mut values = values.into_iter();
		let current = values.next().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("traversal run current value is absent")
		})?;
		let trace = values.next().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("traversal run trace value is absent")
		})?;
		let sack = values.next().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("traversal run sack value is absent")
		})?;
		let Some(Value::Object(named_sacks)) = values.next() else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"traversal run named sacks are invalid",
			));
		};
		let Some(Value::Object(env)) = values.next() else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"traversal run environment is invalid",
			));
		};
		let high = spill_u32(values.next(), "bulk high word")?;
		let low = spill_u32(values.next(), "bulk low word")?;
		if values.next().is_some() {
			return Err(FerrosGrynnError::invalid_storage_file(
				"traversal run record contains trailing values",
			));
		}
		let bulk = (u64::from(high) << 32) | u64::from(low);
		let bulk = NonZeroU64::new(bulk)
			.ok_or_else(|| FerrosGrynnError::invalid_storage_file("traversal run bulk is zero"))?;
		let mut traverser = Traverser::new(current);
		traverser.sack = sack;
		traverser.named_sacks = named_sacks;
		traverser.env = env;
		traverser.bulk = bulk;
		match trace {
			Value::None => {}
			Value::Path(path) => {
				let mut previous = None;
				let last = path.steps().len().saturating_sub(1);
				for (index, step) in path.steps().iter().cloned().enumerate() {
					previous = Some(self.push_trace_node(
						previous,
						step,
						(index == last).then(|| path.cost()).flatten(),
					)?);
				}
				traverser.trace = previous;
				traverser.trace_origin = Some(path.origin());
				traverser.trace_snapshot = Some(path.snapshot());
				traverser.trace_cost = path.cost().unwrap_or(Number::Int(0));
			}
			value => {
				return Err(FerrosGrynnError::invalid_storage_file(format!(
					"traversal run trace has type {}",
					value.kind()
				)));
			}
		}
		Ok(traverser)
	}
}

/// Whether a traversal sort is preparing ordered output or grouped aggregation.
#[derive(Clone, Copy)]
enum SortPurpose {
	/// Sort traversal output by its ordering keys.
	Order,
	/// Sort traversal output so equal group keys become adjacent.
	Group,
}

/// Yields the next group for this module.
fn next_group<Read: QueryRead>(
	machine: &mut TraversalMachine<'_, Read>,
	stream: &mut RecordStream,
	pending: &mut Option<CursorRecord>,
) -> FerrosGrynnResult<Option<Traverser>> {
	let Some(first) = pending.take().map_or_else(|| stream.next(), |record| Ok(Some(record)))?
	else {
		return Ok(None);
	};
	let key = first.anchor.order_keys.first().cloned().unwrap_or(Value::None);
	let mut values = Array::new(Vec::new());
	let mut count = 0_u64;
	let mut account = machine.control.barrier_account();
	let mut record = Some(first);
	loop {
		let current = record.take().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("traversal group record is unavailable")
		})?;
		let traverser = machine.traverser_from_record(current)?;
		count = count
			.checked_add(traverser.bulk.get())
			.ok_or_else(FerrosGrynnError::traversal_bulk_overflow)?;
		for _ in 0..traverser.bulk.get() {
			let bytes = std::mem::size_of::<Value>()
				.saturating_add(traverser.current.estimated_heap_bytes());
			if !account.reserve(bytes) {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"group result".to_owned(),
					machine.control.barrier_memory_limit() as u64,
				));
			}
			values.push(traverser.current.clone());
		}
		let Some(next) = stream.next()? else {
			break;
		};
		if next.anchor.order_keys.first() != Some(&key) {
			*pending = Some(next);
			break;
		}
		record = Some(next);
	}
	let mut value = Object::new();
	value.insert("key", key);
	value.insert("values", Value::Array(values));
	value.insert(
		"count",
		i64::try_from(count)
			.map(Value::from)
			.map_err(|_error| FerrosGrynnError::operation_overflow("tv::group"))?,
	);
	Ok(Some(Traverser::new(Value::Object(value))))
}

/// Returns buffered value for this module.
fn buffered_value(value: Value, control: &ExecutionControl) -> FerrosGrynnResult<BarrierCursor> {
	let mut account = control.barrier_account();
	let traverser = Traverser::new(value);
	if !account.reserve(traverser.estimated_heap_bytes()) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"retained traversal result",
			control.barrier_memory_limit() as u64,
		));
	}
	let mut values = Vec::new();
	reserve_slot(&mut values, &mut account, control)?;
	values.push(traverser);
	Ok(BarrierCursor::Buffered {
		values: values.into(),
		account,
	})
}

/// Admits old and new allocations before growing a buffered array.
fn reserve_slot<T>(
	values: &mut Vec<T>,
	account: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	if values.len() < values.capacity() {
		return Ok(());
	}
	let old = values
		.capacity()
		.checked_mul(size_of::<T>())
		.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal buffer capacity"))?;
	let capacity = values
		.capacity()
		.checked_mul(2)
		.map(|value| value.max(4))
		.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal buffer capacity"))?;
	let bytes = capacity
		.checked_mul(size_of::<T>())
		.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal buffer bytes"))?;
	if !account.reserve(bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"traversal buffer capacity",
			control.barrier_memory_limit() as u64,
		));
	}
	values.try_reserve_exact(capacity - values.len()).map_err(|cause| {
		FerrosGrynnError::traversal_resource_limit_exceeded(
			"traversal buffer allocation",
			control.barrier_memory_limit() as u64,
		)
		.with_technical_source(cause)
	})?;
	account.release(old);
	Ok(())
}

/// Holds complete retained output under the shared account until it is pulled.
fn buffered_values(
	values: Vec<Traverser>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<BarrierCursor> {
	let bytes = values.iter().try_fold(
		values
			.capacity()
			.checked_mul(size_of::<Traverser>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal result capacity"))?,
		|bytes, value| {
			bytes
				.checked_add(value.estimated_heap_bytes())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("traversal result bytes"))
		},
	)?;
	let mut account = control.barrier_account();
	if !account.reserve(bytes) {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"retained graph result",
			control.barrier_memory_limit() as u64,
		));
	}
	Ok(BarrierCursor::Buffered {
		values: values.into(),
		account,
	})
}

/// Returns spill u32 for this module.
fn spill_u32(value: Option<Value>, field: &str) -> FerrosGrynnResult<u32> {
	value
		.and_then(|value| value.as_number().and_then(|number| i64::try_from(number).ok()))
		.and_then(|value| u32::try_from(value).ok())
		.ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!("traversal run {field} is invalid"))
		})
}

#[cfg(test)]
/// Regression tests for barrier behavior.
mod tests {
	use std::num::NonZeroUsize;

	use super::*;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};

	/// Verifies capacity is admitted before growth including old allocation.
	#[test]
	fn capacity_is_admitted_before_growth_including_old_allocation() {
		let options = crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: NonZeroUsize::new(64).assert_some(),
			..Default::default()
		};
		let control = ExecutionControl::new(&options, None);
		let mut account = control.barrier_account();
		let mut traversers = Vec::<Traverser>::new();
		assert!(reserve_slot(&mut traversers, &mut account, &control).is_err());
		assert_eq!(traversers.capacity(), 0);
		let mut values = Vec::<u64>::new();
		reserve_slot(&mut values, &mut account, &control).assert_ok();
		values.extend([0, 1, 2, 3]);
		assert!(reserve_slot(&mut values, &mut account, &control).is_err());
		assert_eq!(values.capacity(), 4);
	}
}
