//! Bounded intermediate SELECT rows and their transient spill representation.

use std::path::PathBuf;

use super::*;

pub(super) enum RowSet {
	Memory {
		rows: Vec<Row>,
		_account: BarrierAccount,
	},
	Disk {
		spool: crate::gql::spill::QuerySpool,
		offset: u64,
	},
}

impl RowSet {
	pub(super) fn first(&self) -> FerrosGrynnResult<Option<Row>> {
		match self {
			Self::Memory {
				rows,
				_account: _,
			} => Ok(rows.first().cloned()),
			Self::Disk {
				spool,
				offset,
			} => spool.page(*offset, 1)?.records.into_iter().next().map(row_from_record).transpose(),
		}
	}

	pub(super) fn visit(
		&self,
		mut visit: impl FnMut(Row) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory {
				rows,
				_account: _,
			} => rows.iter().cloned().try_for_each(visit),
			Self::Disk {
				spool,
				offset,
			} => spool.for_each(*offset, |record| visit(row_from_record(record)?)),
		}
	}

	pub(super) fn consume(
		self,
		mut visit: impl FnMut(Row) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory {
				rows,
				_account: mut account,
			} => {
				for row in rows {
					account.release(estimated_row_bytes(&row));
					visit(row)?;
				}
				Ok(())
			}
			Self::Disk {
				spool,
				offset,
			} => spool.for_each(offset, |record| visit(row_from_record(record)?)),
		}
	}
}

pub(super) struct RowCollector {
	memory: Vec<Row>,
	memory_bytes: usize,
	account: BarrierAccount,
	root: PathBuf,
	sorter: Option<QuerySorter>,
	next_ordinal: usize,
	force_disk: bool,
}

impl RowCollector {
	pub(super) fn new<Read: QueryRead>(
		read: &Read,
		control: &ExecutionControl,
		force_disk: bool,
	) -> Self {
		Self {
			memory: Vec::new(),
			memory_bytes: 0,
			account: control.barrier_account(),
			root: control.temporary_directory.clone().unwrap_or_else(|| read.query_scratch_root()),
			sorter: None,
			next_ordinal: 0,
			force_disk,
		}
	}

	pub(super) fn push<Read: QueryRead>(
		&mut self,
		read: &Read,
		snapshot: Snapshot,
		row: Row,
	) -> FerrosGrynnResult<()> {
		if let Some(sorter) = &mut self.sorter {
			sorter.push(row_record(row, self.next_ordinal))?;
			self.next_ordinal += 1;
			return Ok(());
		}
		let bytes = estimated_row_bytes(&row);
		let must_spill = self.force_disk || !self.account.reserve(bytes);
		if must_spill {
			self.account.release_all();
			let mut sorter = QuerySorter::new(
				&self.root,
				read.spill_context(snapshot)?,
				vec![SortField {
					descending: false,
					nulls_first: false,
				}],
				std::mem::replace(&mut self.account, BarrierAccount::new(0)),
				true,
			)?;
			for row in self.memory.drain(..) {
				sorter.push(row_record(row, self.next_ordinal))?;
				self.next_ordinal += 1;
			}
			self.memory_bytes = 0;
			self.sorter = Some(sorter);
		}
		if let Some(sorter) = &mut self.sorter {
			sorter.push(row_record(row, self.next_ordinal))?;
			self.next_ordinal += 1;
		} else {
			self.memory_bytes = self.memory_bytes.checked_add(bytes).ok_or_else(|| {
				FerrosGrynnError::invalid_argument("query row accounting overflow")
			})?;
			self.memory.push(row);
		}
		Ok(())
	}

	pub(super) fn finish(self) -> FerrosGrynnResult<RowSet> {
		let Some(sorter) = self.sorter else {
			return Ok(RowSet::Memory {
				rows: self.memory,
				_account: self.account,
			});
		};
		match sorter.finish()? {
			SortedRecords::Memory {
				records,
				account,
			} => {
				records.into_iter().map(row_from_record).collect::<FerrosGrynnResult<Vec<_>>>().map(
					|rows| RowSet::Memory {
						rows,
						_account: account,
					},
				)
			}
			SortedRecords::Disk {
				spool,
				offset,
			} => Ok(RowSet::Disk {
				spool,
				offset,
			}),
		}
	}
}

pub(super) fn consume_records(
	records: SortedRecords,
	visit: impl FnMut(CursorRecord) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	match records {
		SortedRecords::Memory {
			records,
			account: _account,
		} => records.into_iter().try_for_each(visit),
		SortedRecords::Disk {
			spool,
			offset,
		} => spool.for_each(offset, visit),
	}
}

pub(super) fn estimated_row_bytes(row: &Row) -> usize {
	row.estimated_heap_bytes()
}

pub(super) fn row_record(row: Row, ordinal: usize) -> CursorRecord {
	let graph = row.graph.map(Value::from).unwrap_or(Value::None);
	CursorRecord {
		value: Value::Array(
			vec![
				Value::Array(row.values.into()),
				graph,
				Value::Array(row.stable.into()),
				Value::Array(row.sequence.into_iter().map(Value::from).collect::<Vec<_>>().into()),
			]
			.into(),
		),
		anchor: CursorAnchor {
			order_keys: vec![Value::from(ordinal)],
			stable_key: vec![Value::from(ordinal)],
		},
	}
}

pub(super) fn row_from_record(record: CursorRecord) -> FerrosGrynnResult<Row> {
	let Value::Array(values) = record.value else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row run does not contain an array",
		));
	};
	let mut values = values.into_iter();
	let Some(Value::Array(columns)) = values.next() else {
		return Err(FerrosGrynnError::invalid_storage_file("query row run has invalid columns"));
	};
	let graph = match values.next() {
		Some(Value::GraphRef(graph)) => Some(*graph),
		Some(Value::None) => None,
		_ => {
			return Err(FerrosGrynnError::invalid_storage_file(
				"query row run has invalid graph scope",
			));
		}
	};
	let Some(Value::Array(stable)) = values.next() else {
		return Err(FerrosGrynnError::invalid_storage_file("query row run has invalid stable key"));
	};
	let Some(Value::Array(sequence)) = values.next() else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row run has invalid sequence key",
		));
	};
	let sequence = sequence
		.into_iter()
		.map(|value| match value {
			Value::Number(number) => usize::try_from(number).map_err(|_error| {
				FerrosGrynnError::invalid_storage_file("query row sequence does not fit usize")
			}),
			_ => {
				Err(FerrosGrynnError::invalid_storage_file("query row sequence is not an integer"))
			}
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if values.next().is_some() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row run contains trailing fields",
		));
	}
	Ok(Row::new(columns.into_iter().collect())
		.with_graph(graph)
		.with_stable(stable.into_iter().collect())
		.with_sequence(sequence))
}
