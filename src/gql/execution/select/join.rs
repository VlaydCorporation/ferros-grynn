//! SELECT join algorithms and bounded spill paths.

use std::mem::size_of;

use rapidhash::RapidHashMap;

use super::*;
use crate::gql::spill::RecordStream;

const JOIN_PARTITIONS: u64 = 64;

fn combine_rows(left: &Row, right: &Row, right_layout: (usize, usize)) -> Row {
	let mut values = left.values.clone();
	let (offset, width) = right_layout;
	values[offset..offset.saturating_add(width)]
		.clone_from_slice(&right.values[offset..offset.saturating_add(width)]);
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	columns: &[(usize, usize)],
	operator: crate::gql::optimizer::JoinOperator,
	build_side: crate::gql::optimizer::JoinBuildSide,
	right_layout: (usize, usize),
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let (left, right) = match (operator, left, right, !columns.is_empty()) {
		(
			crate::gql::optimizer::JoinOperator::Hash,
			RowSet::Memory {
				rows: left,
				_account: left_account,
			},
			RowSet::Memory {
				rows: right,
				_account: right_account,
			},
			true,
		) => {
			return hash_join_rows(
				read,
				snapshot,
				left,
				right,
				left_account,
				right_account,
				kind,
				predicates,
				columns,
				build_side,
				right_layout,
				slots,
				source,
				control,
			);
		}
		(
			crate::gql::optimizer::JoinOperator::Hash
			| crate::gql::optimizer::JoinOperator::PartitionedHash,
			left,
			right,
			true,
		) => {
			return partitioned_hash_join_rows(
				read,
				snapshot,
				left,
				right,
				kind,
				predicates,
				columns,
				build_side,
				right_layout,
				slots,
				source,
				control,
			);
		}
		(crate::gql::optimizer::JoinOperator::BlockNestedLoop, left, right, _)
		| (crate::gql::optimizer::JoinOperator::IndexNestedLoop, left, right, _) => (left, right),
		(
			crate::gql::optimizer::JoinOperator::Hash
			| crate::gql::optimizer::JoinOperator::PartitionedHash,
			left,
			right,
			false,
		) => (left, right),
	};
	block_nested_join_rows(
		read,
		snapshot,
		left,
		&right,
		kind,
		predicates,
		right_layout,
		slots,
		source,
		control,
	)
}

#[allow(
	clippy::too_many_arguments,
	reason = "an index nested-loop retains the exact source and statement context"
)]
pub(super) fn index_nested_join_rows<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	left: RowSet,
	right_source: &LogicalSelectSource,
	probe: &crate::gql::optimizer::IndexJoinProbe,
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	right_layout: (usize, usize),
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	let LogicalSelectSourceKind::Atoms {
		target: ScanTarget::Class(class),
		graph,
	} = &right_source.kind
	else {
		return Err(FerrosGrynnError::invalid_argument(
			"IndexNestedLoop requires an atom-class right source",
		));
	};
	let mut output = RowCollector::new(read, control, false);
	left.consume(|left| {
		control.check()?;
		let key = probe
			.left_columns
			.iter()
			.map(|column| left.values.get(*column).cloned().unwrap_or(Value::None))
			.collect::<Vec<_>>();
		let mut matched = false;
		if !key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
			let graph = graph
				.as_ref()
				.map(|reference| {
					resolve_graph_reference(
						read,
						snapshot,
						reference,
						Some(&left),
						slots,
						source,
						control,
					)
				})
				.transpose()?;
			let mut right =
				read.probe_index(snapshot, class, graph.as_ref(), probe.index, &key, true)?;
			let mut ordinal = 0_usize;
			while let Some(right) = right.next(control)? {
				let right = layout_row(
					right.with_graph(graph.clone()).with_sequence(vec![ordinal]),
					Some((right_layout.0, right_layout.1, left.values.len())),
				);
				let row = combine_rows(&left, &right, right_layout);
				if join_predicates_match(read, snapshot, predicates, &row, slots, source, control)?
				{
					matched = true;
					output.push(read, snapshot, row)?;
				}
				ordinal = ordinal.checked_add(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("index join row ordinal overflow")
				})?;
			}
		}
		if !matched && kind == crate::syn::SelectJoinKind::Left {
			output.push(read, snapshot, left)?;
		}
		Ok(())
	})?;
	output.finish()
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	right_layout: (usize, usize),
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
				kind,
				predicates,
				right_layout,
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
				kind,
				predicates,
				right_layout,
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
		kind,
		predicates,
		right_layout,
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	right_layout: (usize, usize),
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
			let row = combine_rows(left, &right, right_layout);
			if join_predicates_match(read, snapshot, predicates, &row, slots, source, control)? {
				matched[index] = true;
				joined.push(read, snapshot, row)?;
			}
		}
		Ok(())
	})?;
	for (row, matched) in left.drain(..).zip(matched) {
		if !matched && kind == crate::syn::SelectJoinKind::Left {
			joined.push(read, snapshot, row)?;
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	columns: &[(usize, usize)],
	build_side: crate::gql::optimizer::JoinBuildSide,
	right_layout: (usize, usize),
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<RowSet> {
	debug_assert!(
		kind != crate::syn::SelectJoinKind::Left
			|| build_side == crate::gql::optimizer::JoinBuildSide::Right
	);
	let mut index_account = control.barrier_account();
	let build_rows = match build_side {
		crate::gql::optimizer::JoinBuildSide::Left => &left,
		crate::gql::optimizer::JoinBuildSide::Right => &right,
	};
	let build_columns = match build_side {
		crate::gql::optimizer::JoinBuildSide::Left => {
			columns.iter().map(|(left, _right)| *left).collect::<Vec<_>>()
		}
		crate::gql::optimizer::JoinBuildSide::Right => {
			columns.iter().map(|(_left, right)| *right).collect::<Vec<_>>()
		}
	};
	let index_bytes = build_rows
		.iter()
		.map(|row| {
			let key = join_key(row, build_columns.iter().copied());
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
			kind,
			predicates,
			columns,
			build_side,
			right_layout,
			slots,
			source,
			control,
		);
	}
	let mut index = RapidHashMap::<Vec<Value>, Vec<usize>>::default();
	for (position, row) in build_rows.iter().enumerate() {
		let key = join_key(row, build_columns.iter().copied());
		if key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
			continue;
		}
		index.entry(key).or_default().push(position);
	}
	let mut joined = RowCollector::new(read, control, false);
	match build_side {
		crate::gql::optimizer::JoinBuildSide::Right => {
			for left in left {
				control.check()?;
				let key = join_key(&left, columns.iter().map(|(left, _right)| *left));
				let candidates =
					if key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
						None
					} else {
						index.get(&key)
					};
				let mut matched = false;
				if let Some(candidates) = candidates {
					for position in candidates {
						let row = combine_rows(&left, &right[*position], right_layout);
						if join_predicates_match(
							read, snapshot, predicates, &row, slots, source, control,
						)? {
							matched = true;
							joined.push(read, snapshot, row)?;
						}
					}
				}
				if !matched && kind == crate::syn::SelectJoinKind::Left {
					joined.push(read, snapshot, left)?;
				}
			}
		}
		crate::gql::optimizer::JoinBuildSide::Left => {
			for right in right {
				control.check()?;
				let key = join_key(&right, columns.iter().map(|(_left, right)| *right));
				if key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
					continue;
				}
				if let Some(candidates) = index.get(&key) {
					for position in candidates {
						let row = combine_rows(&left[*position], &right, right_layout);
						if join_predicates_match(
							read, snapshot, predicates, &row, slots, source, control,
						)? {
							joined.push(read, snapshot, row)?;
						}
					}
				}
			}
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	columns: &[(usize, usize)],
	build_side: crate::gql::optimizer::JoinBuildSide,
	right_layout: (usize, usize),
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
				kind,
				predicates,
				columns,
				build_side,
				right_layout,
				slots,
				source,
				control,
				&mut joined,
			)?;
			right_partition = right.next_partition(read, snapshot, control)?;
		} else if kind == crate::syn::SelectJoinKind::Left {
			left_rows.consume(|row| joined.push(read, snapshot, row))?;
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
	use std::hash::{Hash, Hasher};

	let mut hasher = crate::gql::optimizer::CanonicalHasher::versioned(
		crate::gql::optimizer::PLAN_ENCODING_VERSION,
		b"join-partition",
	);
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	columns: &[(usize, usize)],
	build_side: crate::gql::optimizer::JoinBuildSide,
	right_layout: (usize, usize),
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	joined: &mut RowCollector,
) -> FerrosGrynnResult<()> {
	match build_side {
		crate::gql::optimizer::JoinBuildSide::Right => {
			let RowSet::Memory {
				rows: right_rows,
				_account: right_account,
			} = right
			else {
				return nested_join_partition(
					read,
					snapshot,
					left,
					&right,
					kind,
					predicates,
					right_layout,
					slots,
					source,
					control,
					joined,
				);
			};
			let mut index_account = control.barrier_account();
			let index_bytes = right_rows
				.iter()
				.map(|row| {
					let key = join_key(row, columns.iter().map(|(_left, right)| *right));
					estimated_hash_entry_bytes(&key)
				})
				.fold(0_usize, usize::saturating_add);
			if !index_account.reserve(index_bytes) {
				let right = RowSet::Memory {
					rows: right_rows,
					_account: right_account,
				};
				return nested_join_partition(
					read,
					snapshot,
					left,
					&right,
					kind,
					predicates,
					right_layout,
					slots,
					source,
					control,
					joined,
				);
			}
			let mut index = RapidHashMap::<Vec<Value>, Vec<Row>>::default();
			for row in right_rows {
				let key = join_key(&row, columns.iter().map(|(_left, right)| *right));
				if !key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
					index.entry(key).or_default().push(row);
				}
			}
			left.consume(|left| {
				let key = join_key(&left, columns.iter().map(|(left, _right)| *left));
				let mut matched = false;
				if !key.iter().any(|value| matches!(value, Value::Null | Value::None))
					&& let Some(candidates) = index.get(&key)
				{
					for right in candidates {
						let row = combine_rows(&left, right, right_layout);
						if join_predicates_match(
							read, snapshot, predicates, &row, slots, source, control,
						)? {
							matched = true;
							joined.push(read, snapshot, row)?;
						}
					}
				}
				if !matched && kind == crate::syn::SelectJoinKind::Left {
					joined.push(read, snapshot, left)?;
				}
				Ok(())
			})
		}
		crate::gql::optimizer::JoinBuildSide::Left => {
			let RowSet::Memory {
				rows: left_rows,
				_account: left_account,
			} = left
			else {
				return nested_join_partition(
					read,
					snapshot,
					left,
					&right,
					kind,
					predicates,
					right_layout,
					slots,
					source,
					control,
					joined,
				);
			};
			let mut index_account = control.barrier_account();
			let index_bytes = left_rows
				.iter()
				.map(|row| {
					let key = join_key(row, columns.iter().map(|(left, _right)| *left));
					estimated_hash_entry_bytes(&key)
				})
				.fold(0_usize, usize::saturating_add);
			if !index_account.reserve(index_bytes) {
				let left = RowSet::Memory {
					rows: left_rows,
					_account: left_account,
				};
				return nested_join_partition(
					read,
					snapshot,
					left,
					&right,
					kind,
					predicates,
					right_layout,
					slots,
					source,
					control,
					joined,
				);
			}
			let mut index = RapidHashMap::<Vec<Value>, Vec<Row>>::default();
			for row in left_rows {
				let key = join_key(&row, columns.iter().map(|(left, _right)| *left));
				if !key.iter().any(|value| matches!(value, Value::Null | Value::None)) {
					index.entry(key).or_default().push(row);
				}
			}
			right.consume(|right| {
				let key = join_key(&right, columns.iter().map(|(_left, right)| *right));
				if !key.iter().any(|value| matches!(value, Value::Null | Value::None))
					&& let Some(candidates) = index.get(&key)
				{
					for left in candidates {
						let row = combine_rows(left, &right, right_layout);
						if join_predicates_match(
							read, snapshot, predicates, &row, slots, source, control,
						)? {
							joined.push(read, snapshot, row)?;
						}
					}
				}
				Ok(())
			})
		}
	}
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
	kind: crate::syn::SelectJoinKind,
	predicates: &[&TypedExpr],
	right_layout: (usize, usize),
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
			let row = combine_rows(&left, &right, right_layout);
			if join_predicates_match(read, snapshot, predicates, &row, slots, source, control)? {
				matched = true;
				joined.push(read, snapshot, row)?;
			}
			Ok(())
		})?;
		if !matched && kind == crate::syn::SelectJoinKind::Left {
			joined.push(read, snapshot, left)?;
		}
		Ok(())
	})
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

#[allow(clippy::too_many_arguments, reason = "physical joins may activate several predicates")]
fn join_predicates_match<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	predicates: &[&TypedExpr],
	row: &Row,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<bool> {
	for predicate in predicates {
		if !join_predicate_matches(read, snapshot, Some(predicate), row, slots, source, control)? {
			return Ok(false);
		}
	}
	Ok(true)
}
