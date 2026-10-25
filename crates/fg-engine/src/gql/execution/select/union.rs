//! Independent class outputs share one ordinary sorter and result window.

use super::*;

/// Only global clauses require a barrier across otherwise streaming classes.
pub(super) fn needs_window(plan: &LogicalSelect) -> bool {
	plan.only || !plan.order.is_empty() || plan.start.is_some() || plan.limit.is_some()
}

/// Sort keys may be class-specific; their comparison policy must agree.
pub(super) fn check_order(first: &LogicalSelect, other: &LogicalSelect) -> FerrosGrynnResult<()> {
	if first.order.len() != other.order.len()
		|| !first.order.iter().zip(&other.order).all(|(left, right)| {
			left.random == right.random
				&& left.collate == right.collate
				&& left.numeric == right.numeric
				&& left.direction == right.direction
				&& left.nulls == right.nulls
		}) {
		return Err(FerrosGrynnError::invalid_argument("LIVE union ORDER layout mismatch"));
	}
	Ok(())
}

/// Preserves each class's projection and comparison keys before global paging.
pub(super) fn visit_rows<Read: QueryRead>(
	read: &Read,
	plans: &[&LogicalSelect],
	slots: &[Value],
	control: &ExecutionControl,
	maximum: Option<usize>,
	root_identities: bool,
	mut visit: impl FnMut(CursorRecord) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	let Some(first) = plans.first() else {
		return Ok(0);
	};
	let fields = read.bounded_fields().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("independent SELECT union requires bounded evaluation")
	})?;
	let snapshot = read.snapshot()?;
	let (first, _physical) = prepare_physical_plan(read, snapshot, first, control)?;
	let row_fields = RecordFields {
		read,
		snapshot,
		graph: None,
	};
	let window = select_window(&first, slots, "", &row_fields, control)?;
	for input in plans {
		fields.work(1)?;
		if input.only != first.only {
			return Err(FerrosGrynnError::invalid_argument("LIVE union ONLY mismatch"));
		}
		check_order(&first, input)?;
		if input.split.len() != first.split.len() {
			return Err(FerrosGrynnError::invalid_argument("LIVE union SPLIT layout mismatch"));
		}
		if input.joins.len() != first.joins.len() {
			return Err(FerrosGrynnError::invalid_argument("LIVE union JOIN layout mismatch"));
		}
		if input.group.is_some() || input.filter.as_ref().is_some_and(contains_search) {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"LIVE row union with relational barriers",
			));
		}
		if select_window(input, slots, "", &row_fields, control)? != window {
			return Err(FerrosGrynnError::invalid_argument("LIVE union window mismatch"));
		}
	}
	let (start, limit) = window;
	let limit = limit.min(maximum.unwrap_or(usize::MAX));
	if limit == 0 {
		for input in plans {
			if root_identities && !input.joins.is_empty() {
				let (mut scoped, _physical) =
					prepare_physical_plan(read, snapshot, input, control)?;
				JoinedSources::prepare(read, snapshot, &mut scoped, slots, control)?;
				validate_zero_window(read, snapshot, &scoped, slots, "", control, None)?;
			} else {
				validate_zero_window(read, snapshot, input, slots, "", control, None)?;
			}
		}
		return Ok(0);
	}
	if first.order.is_empty() && !first.tempfiles {
		let count = visit_unordered(
			read,
			plans,
			slots,
			control,
			start,
			select_pull_limit(first.only, limit),
			root_identities,
			&mut visit,
		)?;
		check_select_cardinality(first.only, count)?;
		return Ok(count);
	}
	let mut output =
		OutputCollector::new(read, snapshot, &first, !first.order.is_empty(), control)?;
	let mut ordinal = 0_usize;
	for input in plans {
		let (mut input, physical) = prepare_physical_plan(read, snapshot, input, control)?;
		let joined_sources = if root_identities && !input.joins.is_empty() {
			Some(JoinedSources::prepare(read, snapshot, &mut input, slots, control)?)
		} else {
			None
		};
		let records = execute_select_output(
			read,
			snapshot,
			&input,
			&physical,
			slots,
			"",
			control,
			None,
			joined_sources.as_ref(),
		)?;
		visit_records(read, records, 0, usize::MAX, control, |mut record| {
			fields.admit(std::mem::size_of::<CursorRecord>())?;
			if joined_sources.is_some() {
				fields.admit(2 * std::mem::size_of::<Value>())?;
				record.anchor.stable_key.try_reserve(1).map_err(|error| {
					FerrosGrynnError::traversal_resource_limit_exceeded(
						"LIVE JOIN union anchor",
						ordinal as u64,
					)
					.with_technical_source(error)
				})?;
				let suffix = record.anchor.stable_key.pop().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("LIVE JOIN union suffix missing")
				})?;
				record.anchor.stable_key.push(Value::from(ordinal));
				record.anchor.stable_key.push(suffix);
			}
			output.push_record(record, ordinal)?;
			ordinal = ordinal
				.checked_add(1)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE union output ordinal"))?;
			Ok(())
		})?;
	}
	let count = visit_records(read, output.finish()?, start, limit, control, visit)?;
	check_select_cardinality(first.only, count)?;
	Ok(count)
}

/// Unordered windows stop projection/FETCH after the global limit, just as
/// SELECT.
fn visit_unordered<Read: QueryRead>(
	read: &Read,
	plans: &[&LogicalSelect],
	slots: &[Value],
	control: &ExecutionControl,
	mut start: usize,
	mut limit: usize,
	root_identities: bool,
	visit: &mut impl FnMut(CursorRecord) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	let snapshot = read.snapshot()?;
	let fields = read.bounded_fields().ok_or_else(|| {
		FerrosGrynnError::invalid_argument("LIVE union requires bounded evaluation")
	})?;
	let mut count = 0_usize;
	for input in plans {
		let (mut input, physical) = prepare_physical_plan(read, snapshot, input, control)?;
		// ONLY belongs to the global window, including rows skipped across
		// classes.
		input.only = false;
		let joined_sources = if root_identities && !input.joins.is_empty() {
			Some(JoinedSources::prepare(read, snapshot, &mut input, slots, control)?)
		} else {
			None
		};
		let maximum = start.saturating_add(limit);
		let mut accept = |record| {
			if start != 0 {
				start -= 1;
			} else {
				count = count.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::operation_overflow("LIVE union window count")
				})?;
				visit(record)?;
				limit -= 1;
			}
			Ok(())
		};
		if visit_streaming_select_values(
			read,
			snapshot,
			&input,
			&physical,
			slots,
			"",
			control,
			None,
			0,
			maximum,
			false,
			&mut |row, value| {
				let row = row.ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("LIVE row union has no source anchor")
				})?;
				let stable = if row.stable.is_empty() {
					std::slice::from_ref(row.value(0).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("LIVE union source identity missing")
					})?)
				} else {
					row.stable.as_slice()
				};
				fields.admit(
					stable.len().checked_mul(2 * std::mem::size_of::<Value>()).ok_or_else(
						|| FerrosGrynnError::operation_overflow("LIVE union source anchor"),
					)?,
				)?;
				for value in stable {
					fields.admit_value(value)?;
				}
				accept(CursorRecord {
					value,
					anchor: CursorAnchor {
						order_keys: Vec::new(),
						stable_key: stable.to_vec(),
					},
				})
			},
		)?
		.is_none()
		{
			let records = execute_select_output(
				read,
				snapshot,
				&input,
				&physical,
				slots,
				"",
				control,
				None,
				joined_sources.as_ref(),
			)?;
			visit_records(read, records, 0, maximum, control, &mut accept)?;
		}
		if limit == 0 {
			break;
		}
	}
	Ok(count)
}
