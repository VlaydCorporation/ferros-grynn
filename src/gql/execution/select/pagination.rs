//! SELECT result ordering, paging, and retained-cursor output.

use super::*;

#[allow(
	clippy::too_many_arguments,
	reason = "pagination needs the complete analyzed query identity and statement runtime"
)]
/// Returns the page output result.
pub(super) fn page_output(
	output: SortedRecords,
	start: usize,
	limit: Option<usize>,
	plan: &LogicalSelect,
	snapshot: Snapshot,
	control: &ExecutionControl,
	fingerprint: u64,
	order_fingerprint: u64,
) -> FerrosGrynnResult<Vec<Value>> {
	match output {
		SortedRecords::Memory {
			records,
			account,
		} => {
			let records = records.into_iter().skip(start).collect::<Vec<_>>();
			let end = limit.unwrap_or(usize::MAX).min(records.len());
			let values = records[..end].iter().map(|row| row.value.clone()).collect();
			if !should_retain_spool(plan, limit, end, records.len()) {
				return Ok(values);
			}
			let Some(runtime) = control.cursors() else {
				return Ok(values);
			};
			let anchor = records[end - 1].anchor.clone();
			let cursor = runtime.lock().register(
				records,
				account,
				end,
				fingerprint,
				order_fingerprint,
				snapshot.0,
				anchor,
			)?;
			control.set_continuation(cursor);
			Ok(values)
		}
		SortedRecords::Disk {
			spool,
			offset,
		} => {
			let page = spool.page_after_rows(offset, start, limit.unwrap_or(usize::MAX))?;
			let values = page.records.iter().map(|row| row.value.clone()).collect();
			if page.records.is_empty()
				|| page.eof
				|| limit.is_none()
				|| plan.order.is_empty()
				|| plan.only
			{
				return Ok(values);
			}
			let Some(runtime) = control.cursors() else {
				return Ok(values);
			};
			let anchor = page
				.records
				.last()
				.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("query page is empty"))?
				.anchor
				.clone();
			let cursor = runtime.lock().register_disk(
				spool,
				page.next_offset,
				fingerprint,
				order_fingerprint,
				snapshot.0,
				anchor,
			)?;
			control.set_continuation(cursor);
			Ok(values)
		}
	}
}

/// Returns whether the should retain spool condition holds.
fn should_retain_spool(
	plan: &LogicalSelect,
	limit: Option<usize>,
	returned: usize,
	total: usize,
) -> bool {
	returned != 0 && returned != total && limit.is_some() && !plan.order.is_empty() && !plan.only
}

/// Returns the select limit result.
pub(super) fn select_limit(
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	fields: &dyn AtomFieldRead,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<usize>> {
	plan.limit.as_ref().map(|value| eval_usize(value, slots, source, fields, control)).transpose()
}

/// Resolves an entire result window before its source pulls any rows.
pub(super) fn select_window(
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	fields: &dyn AtomFieldRead,
	control: &ExecutionControl,
) -> FerrosGrynnResult<(usize, usize)> {
	let start = match &plan.start {
		None => 0,
		Some(LogicalSelectStart::Offset(value)) => {
			eval_usize(value, slots, source, fields, control)?
		}
		Some(LogicalSelectStart::After {
			..
		}) => {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"START AFTER requires a retained query-session cursor",
			));
		}
	};
	Ok((start, select_limit(plan, slots, source, fields, control)?.unwrap_or(usize::MAX)))
}

/// Streaming ONLY needs two fully evaluated payloads to prove a violation.
pub(super) fn select_pull_limit(only: bool, limit: usize) -> usize {
	if only {
		limit.min(2)
	} else {
		limit
	}
}

/// Cardinality is checked after the selected window and payload evaluation.
pub(in crate::gql::execution) fn check_select_cardinality(
	only: bool,
	count: usize,
) -> FerrosGrynnResult<()> {
	if only && count > 1 {
		let count = u64::try_from(count)
			.map_err(|_error| FerrosGrynnError::operation_overflow("SELECT cardinality"))?;
		return Err(FerrosGrynnError::select_cardinality_violation(count)
			.with_phase(ExecutionPhase::Execution));
	}
	Ok(())
}

/// Output row state.
pub(super) struct OutputRow {
	/// Value associated with this output row.
	pub(super) value: Value,
	/// Keys associated with this output row.
	pub(super) keys: Vec<Value>,
	/// Stable associated with this output row.
	pub(super) stable: Vec<Value>,
	/// Sequence associated with this output row.
	pub(super) sequence: Vec<Value>,
	/// Ordinal associated with this output row.
	pub(super) ordinal: usize,
}

/// Output collector alternatives.
pub(super) enum OutputCollector {
	/// Memory case of output collector.
	Memory(
		/// Memory case of output collector.
		Vec<CursorRecord>,
	),
	/// Sorter case of output collector.
	Sorter {
		/// Sorter associated with this output collector.
		sorter: QuerySorter,
		/// Preserve input order associated with this output collector.
		preserve_input_order: bool,
	},
}

impl OutputCollector {
	/// Creates a new `OutputCollector` value.
	pub(super) fn new<Read: QueryRead>(
		read: &Read,
		snapshot: Snapshot,
		plan: &LogicalSelect,
		requires_sort: bool,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Self> {
		if !requires_sort && !plan.tempfiles && plan.joins.is_empty() && !control.deterministic() {
			return Ok(Self::Memory(Vec::new()));
		}
		let root = control
			.temporary_directory
			.as_deref()
			.map(ToOwned::to_owned)
			.unwrap_or_else(|| read.query_scratch_root());
		let fields = if plan.order.is_empty() {
			(0..1 + plan.joins.len() + plan.split.len())
				.map(|_index| SortField {
					descending: false,
					nulls_first: false,
				})
				.collect()
		} else {
			sort_fields(plan)
		};
		Ok(Self::Sorter {
			sorter: QuerySorter::new(
				&root,
				read.spill_context(snapshot)?,
				fields,
				control.barrier_account(),
				plan.tempfiles,
			),
			preserve_input_order: plan.order.is_empty() && !control.deterministic(),
		})
	}

	/// Pushes output collector.
	pub(super) fn push(&mut self, mut row: OutputRow) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory(records) => {
				records.push(output_record(row));
				Ok(())
			}
			Self::Sorter {
				sorter,
				preserve_input_order,
			} => {
				if *preserve_input_order {
					row.keys = row.sequence.clone();
					if row.keys.is_empty() {
						row.keys.push(Value::from(row.ordinal));
					}
				}
				sorter.push(output_record(row))
			}
		}
	}

	/// Finishes output collector.
	pub(super) fn finish(self) -> FerrosGrynnResult<SortedRecords> {
		match self {
			Self::Memory(records) => Ok(SortedRecords::Memory {
				records,
				account: BarrierAccount::new(0),
			}),
			Self::Sorter {
				sorter,
				..
			} => sorter.finish(),
		}
	}

	/// Moved class outputs keep anchors; unsorted unions retain concatenation
	/// order.
	pub(super) fn push_record(
		&mut self,
		mut record: CursorRecord,
		ordinal: usize,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory(records) => {
				records.try_reserve(1).map_err(|error| {
					FerrosGrynnError::traversal_resource_limit_exceeded(
						"LIVE union output",
						ordinal as u64,
					)
					.with_technical_source(error)
				})?;
				records.push(record);
				Ok(())
			}
			Self::Sorter {
				sorter,
				preserve_input_order,
			} => {
				if *preserve_input_order {
					record.anchor.order_keys = vec![Value::from(ordinal)];
				}
				sorter.push(record)
			}
		}
	}
}

/// Returns the output record result.
pub(super) fn output_record(row: OutputRow) -> CursorRecord {
	CursorRecord {
		value: row.value,
		anchor: CursorAnchor {
			order_keys: row.keys,
			stable_key: row.stable,
		},
	}
}

/// Sorts fields.
fn sort_fields(plan: &LogicalSelect) -> Vec<SortField> {
	if plan.order.is_empty() {
		return vec![SortField {
			descending: false,
			nulls_first: false,
		}];
	}
	plan.order
		.iter()
		.map(|order| SortField {
			descending: order.direction == crate::syn::SelectOrderDirection::Descending,
			nulls_first: order.nulls == crate::syn::SelectNulls::First,
		})
		.collect()
}

/// Returns the output after anchor result.
pub(super) fn output_after_anchor(
	output: SortedRecords,
	anchor: &CursorAnchor,
	plan: &LogicalSelect,
) -> FerrosGrynnResult<SortedRecords> {
	let fields = sort_fields(plan);
	match output {
		SortedRecords::Memory {
			mut records,
			account,
		} => {
			records.retain(|record| compare_record_anchor(record, anchor, &fields).is_gt());
			Ok(SortedRecords::Memory {
				records,
				account,
			})
		}
		SortedRecords::Disk {
			spool,
			offset,
		} => {
			let offset = spool.offset_after(offset, anchor, &fields)?;
			Ok(SortedRecords::Disk {
				spool,
				offset,
			})
		}
	}
}

/// Preserves source/group anchors while applying one bounded result window.
pub(super) fn visit_records<Read: QueryRead>(
	read: &Read,
	output: SortedRecords,
	start: usize,
	limit: usize,
	control: &ExecutionControl,
	mut visit: impl FnMut(CursorRecord) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	let check = || {
		control.check()?;
		if let Some(fields) = read.bounded_fields() {
			fields.work(1)?;
		}
		control.check()
	};
	control.check()?;
	if limit == 0 {
		return Ok(0);
	}
	let mut stream = output.into_stream()?;
	for _index in 0..start {
		check()?;
		if stream.next()?.is_none() {
			return Ok(0);
		}
	}
	let mut visited = 0;
	while visited < limit {
		check()?;
		let Some(record) = stream.next()? else {
			break;
		};
		visit(record)?;
		visited += 1;
	}
	Ok(visited)
}

/// Value-only consumers use the same window and resource checks.
pub(super) fn visit_record_values<Read: QueryRead>(
	read: &Read,
	output: SortedRecords,
	start: usize,
	limit: usize,
	control: &ExecutionControl,
	mut visit: impl FnMut(Value) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<usize> {
	visit_records(read, output, start, limit, control, |record| visit(record.value))
}
