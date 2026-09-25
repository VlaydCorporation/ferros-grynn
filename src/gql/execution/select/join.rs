//! SELECT join algorithms and bounded spill paths.

use std::{
	hash::{DefaultHasher, Hash, Hasher},
	mem::size_of,
};

use rapidhash::RapidHashMap;

use super::*;
use crate::gql::spill::RecordStream;

const JOIN_PARTITIONS: u64 = 64;

fn combine_rows(left: &Row, right: &Row) -> Row {
	let mut values = Vec::with_capacity(left.values.len() + right.values.len());
	values.extend(left.values.iter().cloned());
	values.extend(right.values.iter().cloned());
	let mut stable = Vec::with_capacity(left.stable.len() + right.stable.len());
	stable.extend(left.stable.iter().cloned());
	stable.extend(right.stable.iter().cloned());
	let mut sequence = Vec::with_capacity(left.sequence.len() + right.sequence.len());
	sequence.extend(left.sequence.iter().copied());
	sequence.extend(right.sequence.iter().copied());
	Row::new(values)
		.with_graph(left.graph.clone().or_else(|| right.graph.clone()))
		.with_sequence(sequence)
		.with_stable(stable)
}

#[allow(clippy::too_many_arguments, reason = "join evaluation uses the complete statement context")]
pub(super) fn join_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: RowSet,
	right: RowSet,
	left_width: usize,
	join: &crate::gql::ir::LogicalSelectJoin,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let right_width = join.source.schema.columns.len();
	let columns =
		join.predicate.as_ref().and_then(|predicate| equality_join_columns(predicate, left_width));
	let (left, right) = match (left, right, columns.as_deref()) {
		(
			RowSet::Memory {
				rows: left,
				_account: left_account,
			},
			RowSet::Memory {
				rows: right,
				_account: right_account,
			},
			Some(columns),
		) => {
			return hash_join_rows(
				read,
				snapshot,
				left,
				right,
				left_account,
				right_account,
				join,
				columns,
				right_width,
				slots,
				source,
				control,
			);
		}
		(left, right, Some(columns)) => {
			return partitioned_hash_join_rows(
				read,
				snapshot,
				left,
				right,
				join,
				columns,
				right_width,
				slots,
				source,
				control,
			);
		}
		(left, right, None) => (left, right),
	};
	block_nested_join_rows(read, snapshot, left, &right, join, right_width, slots, source, control)
}

#[allow(
	clippy::too_many_arguments,
	reason = "block nested-loop keeps statement evaluation and resource controls explicit"
)]
fn block_nested_join_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: RowSet,
	right: &RowSet,
	join: &crate::gql::ir::LogicalSelectJoin,
	right_width: usize,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let mut joined = RowCollector::new(read, control, false);
	let mut block = Vec::new();
	let mut block_account = control.barrier_account();
	left.consume(|row| {
		let bytes = estimated_row_bytes(&row);
		let mut reserved = block_account.reserve(bytes);
		if !reserved && !block.is_empty() {
			join_block(
				read,
				snapshot,
				&mut block,
				right,
				join,
				right_width,
				slots,
				source,
				control,
				&mut joined,
			)?;
			block_account.release_all();
			reserved = block_account.reserve(bytes);
		}
		block.push(row);
		if !reserved {
			// One oversized row remains the only unaccounted buffer and is
			// consumed immediately.
			join_block(
				read,
				snapshot,
				&mut block,
				right,
				join,
				right_width,
				slots,
				source,
				control,
				&mut joined,
			)?;
		}
		Ok(())
	})?;
	join_block(
		read,
		snapshot,
		&mut block,
		right,
		join,
		right_width,
		slots,
		source,
		control,
		&mut joined,
	)?;
	block_account.release_all();
	joined.finish()
}

#[allow(
	clippy::too_many_arguments,
	reason = "one nested-loop block retains the complete join execution context"
)]
fn join_block<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: &mut Vec<Row>,
	right: &RowSet,
	join: &crate::gql::ir::LogicalSelectJoin,
	right_width: usize,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	joined: &mut RowCollector,
) -> FerrosGrynnResult<()> {
	if left.is_empty() {
		return Ok(());
	}
	let mut matched = vec![false; left.len()];
	right.visit(|right| {
		control.check()?;
		for (index, left) in left.iter().enumerate() {
			let row = combine_rows(left, &right);
			if join_predicate_matches(
				read,
				snapshot,
				join.predicate.as_ref(),
				&row,
				slots,
				source,
				control,
			)? {
				matched[index] = true;
				joined.push(read, snapshot, row)?;
			}
		}
		Ok(())
	})?;
	for (row, matched) in left.drain(..).zip(matched) {
		if !matched && join.kind == crate::syn::SelectJoinKind::Left {
			joined.push(read, snapshot, unmatched_left(row, right_width))?;
		}
	}
	left.shrink_to_fit();
	Ok(())
}

#[allow(
	clippy::too_many_arguments,
	reason = "hash join keeps expression evaluation and statement controls explicit"
)]
fn hash_join_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: Vec<Row>,
	right: Vec<Row>,
	left_account: BarrierAccount,
	right_account: BarrierAccount,
	join: &crate::gql::ir::LogicalSelectJoin,
	columns: &[(usize, usize)],
	right_width: usize,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let mut index_account = control.barrier_account();
	let index_bytes = right
		.iter()
		.map(|row| {
			let key = join_key(row, columns.iter().map(|(_left, right)| *right));
			estimated_hash_entry_bytes(&key)
		})
		.fold(0_usize, usize::saturating_add);
	if !index_account.reserve(index_bytes) {
		return partitioned_hash_join_rows(
			read,
			snapshot,
			RowSet::Memory {
				rows: left,
				_account: left_account,
			},
			RowSet::Memory {
				rows: right,
				_account: right_account,
			},
			join,
			columns,
			right_width,
			slots,
			source,
			control,
		);
	}
	let mut index = RapidHashMap::<Vec<Value>, Vec<usize>>::default();
	for (position, row) in right.iter().enumerate() {
		let key = join_key(row, columns.iter().map(|(_left, right)| *right));
		if key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
			continue;
		}
		index.entry(key).or_default().push(position);
	}
	let mut joined = RowCollector::new(read, control, false);
	for left in left {
		control.check()?;
		let key = join_key(&left, columns.iter().map(|(left, _right)| *left));
		let candidates = if key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
			None
		} else {
			index.get(&key)
		};
		let mut matched = false;
		if let Some(candidates) = candidates {
			for position in candidates {
				let row = combine_rows(&left, &right[*position]);
				if join_predicate_matches(
					read,
					snapshot,
					join.predicate.as_ref(),
					&row,
					slots,
					source,
					control,
				)? {
					matched = true;
					joined.push(read, snapshot, row)?;
				}
			}
		}
		if !matched && join.kind == crate::syn::SelectJoinKind::Left {
			joined.push(read, snapshot, unmatched_left(left, right_width))?;
		}
	}
	joined.finish()
}

fn estimated_hash_entry_bytes(key: &[Value]) -> usize {
	key.len()
		.saturating_mul(size_of::<Value>())
		.saturating_add(
			key.iter().map(Value::estimated_heap_bytes).fold(0_usize, usize::saturating_add),
		)
		.saturating_add(size_of::<usize>())
		.saturating_add(size_of::<(Vec<Value>, Vec<usize>)>().saturating_mul(2))
}

#[allow(
	clippy::too_many_arguments,
	reason = "partitioned join keeps statement evaluation and resource controls explicit"
)]
fn partitioned_hash_join_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: RowSet,
	right: RowSet,
	join: &crate::gql::ir::LogicalSelectJoin,
	columns: &[(usize, usize)],
	right_width: usize,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let left_columns = columns.iter().map(|(left, _right)| *left).collect::<Vec<_>>();
	let right_columns = columns.iter().map(|(_left, right)| *right).collect::<Vec<_>>();
	let mut left =
		PartitionReader::new(sort_join_partitions(read, snapshot, left, &left_columns, control)?)?;
	let mut right = PartitionReader::new(sort_join_partitions(
		read,
		snapshot,
		right,
		&right_columns,
		control,
	)?)?;
	let mut left_partition = left.next_partition(read, snapshot, control)?;
	let mut right_partition = right.next_partition(read, snapshot, control)?;
	let mut joined = RowCollector::new(read, control, false);
	while let Some((partition, left_rows)) = left_partition.take() {
		control.check()?;
		while right_partition
			.as_ref()
			.is_some_and(|(right_partition, _rows)| *right_partition < partition)
		{
			right_partition = right.next_partition(read, snapshot, control)?;
		}
		if right_partition
			.as_ref()
			.is_some_and(|(right_partition, _rows)| *right_partition == partition)
		{
			let (_partition, right_rows) = right_partition
				.take()
				.ok_or_else(|| FerrosGrynnError::invalid_argument("join partition disappeared"))?;
			join_partition(
				read,
				snapshot,
				left_rows,
				right_rows,
				join,
				columns,
				right_width,
				slots,
				source,
				control,
				&mut joined,
			)?;
			right_partition = right.next_partition(read, snapshot, control)?;
		} else if join.kind == crate::syn::SelectJoinKind::Left {
			left_rows
				.consume(|row| joined.push(read, snapshot, unmatched_left(row, right_width)))?;
		}
		left_partition = left.next_partition(read, snapshot, control)?;
	}
	joined.finish()
}

fn sort_join_partitions<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	rows: RowSet,
	columns: &[usize],
	control: &ExecutionControl,
) -> FerrosGrynnResult<SortedRecords> {
	let root = control
		.temporary_directory
		.as_deref()
		.map(ToOwned::to_owned)
		.unwrap_or_else(|| read.query_scratch_root());
	let mut sorter = QuerySorter::new(
		&root,
		read.spill_context(snapshot)?,
		vec![SortField {
			descending: false,
			nulls_first: false,
		}],
		control.barrier_account(),
		true,
	)?;
	let mut ordinal = 0_usize;
	rows.consume(|row| {
		let key = join_key(&row, columns.iter().copied());
		let partition = join_partition_number(&key);
		let mut record = row_record(row, ordinal);
		record.anchor.order_keys = vec![Value::from(partition as usize)];
		record.anchor.stable_key = vec![Value::from(ordinal)];
		ordinal = ordinal
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("join row ordinal overflow"))?;
		sorter.push(record)
	})?;
	sorter.finish()
}

fn join_partition_number(key: &[Value]) -> u64 {
	let mut hasher = DefaultHasher::new();
	key.hash(&mut hasher);
	hasher.finish() % JOIN_PARTITIONS
}

struct PartitionReader {
	stream: RecordStream,
	pending: Option<CursorRecord>,
}

impl PartitionReader {
	fn new(records: SortedRecords) -> FerrosGrynnResult<Self> {
		Ok(Self {
			stream: records.into_stream()?,
			pending: None,
		})
	}

	fn next_partition<Read: QueryRead>(
		&mut self,
		read: &Read,
		snapshot: Snapshot,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Option<(u64, RowSet)>> {
		let first = match self.pending.take() {
			Some(record) => Some(record),
			None => self.stream.next()?,
		};
		let Some(first) = first else {
			return Ok(None);
		};
		let partition = record_partition(&first)?;
		let mut rows = RowCollector::new(read, control, false);
		rows.push(read, snapshot, row_from_record(first)?)?;
		loop {
			let Some(record) = self.stream.next()? else {
				break;
			};
			if record_partition(&record)? != partition {
				self.pending = Some(record);
				break;
			}
			rows.push(read, snapshot, row_from_record(record)?)?;
		}
		Ok(Some((partition, rows.finish()?)))
	}
}

fn record_partition(record: &CursorRecord) -> FerrosGrynnResult<u64> {
	let Some(Value::Number(number)) = record.anchor.order_keys.first() else {
		return Err(FerrosGrynnError::invalid_storage_file("join partition key is absent"));
	};
	u64::try_from(*number).map_err(|_error| {
		FerrosGrynnError::invalid_storage_file("join partition key is not an unsigned integer")
	})
}

#[allow(
	clippy::too_many_arguments,
	reason = "one partition retains the complete join execution context"
)]
fn join_partition<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: RowSet,
	right: RowSet,
	join: &crate::gql::ir::LogicalSelectJoin,
	columns: &[(usize, usize)],
	right_width: usize,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	joined: &mut RowCollector,
) -> FerrosGrynnResult<()> {
	if let RowSet::Memory {
		rows: right,
		_account: right_account,
	} = right
	{
		let mut index_account = control.barrier_account();
		let index_bytes = right
			.iter()
			.map(|row| {
				let key = join_key(row, columns.iter().map(|(_left, right)| *right));
				estimated_hash_entry_bytes(&key)
			})
			.fold(0_usize, usize::saturating_add);
		if !index_account.reserve(index_bytes) {
			let right = RowSet::Memory {
				rows: right,
				_account: right_account,
			};
			return nested_join_partition(
				read,
				snapshot,
				left,
				&right,
				join,
				right_width,
				slots,
				source,
				control,
				joined,
			);
		}
		let mut index = RapidHashMap::<Vec<Value>, Vec<Row>>::default();
		for row in right {
			let key = join_key(&row, columns.iter().map(|(_left, right)| *right));
			if !key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
				index.entry(key).or_default().push(row);
			}
		}
		return left.consume(|left| {
			let key = join_key(&left, columns.iter().map(|(left, _right)| *left));
			let mut matched = false;
			if !key.iter().any(|value| matches!(value, Value::Null | Value::None))
				&& let Some(candidates) = index.get(&key)
			{
				for right in candidates {
					let row = combine_rows(&left, right);
					if join_predicate_matches(
						read,
						snapshot,
						join.predicate.as_ref(),
						&row,
						slots,
						source,
						control,
					)? {
						matched = true;
						joined.push(read, snapshot, row)?;
					}
				}
			}
			if !matched && join.kind == crate::syn::SelectJoinKind::Left {
				joined.push(read, snapshot, unmatched_left(left, right_width))?;
			}
			Ok(())
		});
	}
	nested_join_partition(
		read,
		snapshot,
		left,
		&right,
		join,
		right_width,
		slots,
		source,
		control,
		joined,
	)
}

#[allow(
	clippy::too_many_arguments,
	reason = "skew fallback keeps the partition join context explicit"
)]
fn nested_join_partition<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: RowSet,
	right: &RowSet,
	join: &crate::gql::ir::LogicalSelectJoin,
	right_width: usize,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	joined: &mut RowCollector,
) -> FerrosGrynnResult<()> {
	// ponytail: an over-budget single hash partition uses bounded nested-loop;
	// recursively repartition only if measured skew makes this path material.
	left.consume(|left| {
		let mut matched = false;
		right.visit(|right| {
			let row = combine_rows(&left, &right);
			if join_predicate_matches(
				read,
				snapshot,
				join.predicate.as_ref(),
				&row,
				slots,
				source,
				control,
			)? {
				matched = true;
				joined.push(read, snapshot, row)?;
			}
			Ok(())
		})?;
		if !matched && join.kind == crate::syn::SelectJoinKind::Left {
			joined.push(read, snapshot, unmatched_left(left, right_width))?;
		}
		Ok(())
	})
}

fn equality_join_columns(expression: &TypedExpr, left_width: usize) -> Option<Vec<(usize, usize)>> {
	match &expression.kind {
		TypedExprKind::Binary {
			left,
			op: BinaryOperator::And,
			right,
		} => {
			let mut columns = equality_join_columns(left, left_width)?;
			columns.extend(equality_join_columns(right, left_width)?);
			Some(columns)
		}
		TypedExprKind::Binary {
			left,
			op: BinaryOperator::Equal,
			right,
		} => match (&left.kind, &right.kind) {
			(TypedExprKind::Column(left), TypedExprKind::Column(right))
				if *left < left_width && *right >= left_width =>
			{
				Some(vec![(*left, *right - left_width)])
			}
			(TypedExprKind::Column(right), TypedExprKind::Column(left))
				if *left < left_width && *right >= left_width =>
			{
				Some(vec![(*left, *right - left_width)])
			}
			_ => None,
		},
		_ => None,
	}
}

fn join_key(row: &Row, columns: impl Iterator<Item = usize>) -> Vec<Value> {
	columns.map(|column| row.values.get(column).cloned().unwrap_or(Value::None)).collect()
}

#[allow(clippy::too_many_arguments, reason = "join predicate uses the statement evaluator")]
fn join_predicate_matches<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	predicate: Option<&TypedExpr>,
	row: &Row,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<bool> {
	let Some(predicate) = predicate else {
		return Ok(true);
	};
	match &predicate.kind {
		TypedExprKind::Binary {
			left,
			op: BinaryOperator::And,
			right,
		} => Ok(join_predicate_matches(read, snapshot, Some(left), row, slots, source, control)?
			&& join_predicate_matches(read, snapshot, Some(right), row, slots, source, control)?),
		TypedExprKind::Binary {
			left,
			op: BinaryOperator::Or,
			right,
		} => Ok(join_predicate_matches(read, snapshot, Some(left), row, slots, source, control)?
			|| join_predicate_matches(read, snapshot, Some(right), row, slots, source, control)?),
		TypedExprKind::Binary {
			left,
			op: operator @ (BinaryOperator::Equal | BinaryOperator::NotEqual),
			right,
		} => {
			let left = eval_select_expr(read, snapshot, left, row, None, slots, source, control)?;
			let right = eval_select_expr(read, snapshot, right, row, None, slots, source, control)?;
			if matches!(left, Value::Null | Value::None)
				|| matches!(right, Value::Null | Value::None)
			{
				return Ok(false);
			}
			Ok(match operator {
				BinaryOperator::Equal => left == right,
				BinaryOperator::NotEqual => left != right,
				_ => unreachable!(),
			})
		}
		_ => eval_select_expr(read, snapshot, predicate, row, None, slots, source, control)
			.map(|value| value.is_truthy()),
	}
}

fn unmatched_left(mut left: Row, right_width: usize) -> Row {
	left.values.resize(left.values.len() + right_width, Value::None);
	left
}
