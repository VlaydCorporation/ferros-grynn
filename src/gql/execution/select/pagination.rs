//! SELECT result ordering, paging, and retained-cursor output.

use super::*;

#[allow(
	clippy::too_many_arguments,
	reason = "pagination needs the complete analyzed query identity and statement runtime"
)]
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

fn should_retain_spool(
	plan: &LogicalSelect,
	limit: Option<usize>,
	returned: usize,
	total: usize,
) -> bool {
	returned != 0 && returned != total && limit.is_some() && !plan.order.is_empty() && !plan.only
}

pub(super) fn select_limit(
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<usize>> {
	plan.limit.as_ref().map(|value| eval_usize(value, slots, source, control)).transpose()
}

pub(super) struct OutputRow {
	pub(super) value: Value,
	pub(super) keys: Vec<Value>,
	pub(super) stable: Vec<Value>,
	pub(super) sequence: Vec<Value>,
	pub(super) ordinal: usize,
}

pub(super) enum OutputCollector {
	Memory(Vec<CursorRecord>),
	Sorter {
		sorter: QuerySorter,
		preserve_input_order: bool,
	},
}

impl OutputCollector {
	pub(super) fn new<Read: QueryRead>(
		read: &Read,
		snapshot: Snapshot,
		plan: &LogicalSelect,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Self> {
		if plan.order.is_empty() && !plan.tempfiles && plan.joins.is_empty() {
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
			)?,
			preserve_input_order: plan.order.is_empty(),
		})
	}

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
}

fn output_record(row: OutputRow) -> CursorRecord {
	CursorRecord {
		value: row.value,
		anchor: CursorAnchor {
			order_keys: row.keys,
			stable_key: row.stable,
		},
	}
}

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

pub(super) fn record_values(
	output: SortedRecords,
	start: usize,
	limit: usize,
) -> FerrosGrynnResult<Vec<Value>> {
	let mut stream = output.into_stream()?;
	for _index in 0..start {
		if stream.next()?.is_none() {
			return Ok(Vec::new());
		}
	}
	let mut values = Vec::with_capacity(limit.min(1024));
	while values.len() < limit {
		let Some(record) = stream.next()? else {
			break;
		};
		values.push(record.value);
	}
	Ok(values)
}
