//! Bounded intermediate SELECT rows and their transient spill representation.

use std::path::PathBuf;

use super::*;

/// Row set alternatives.
pub(super) enum RowSet {
	/// Memory case of row set.
	Memory {
		/// Rows associated with this row set.
		rows: Vec<Row>,
		/// Memory admission account retained for the lifetime of this value.
		_account: BarrierAccount,
	},
	/// Disk case of row set.
	Disk {
		/// Spool associated with this row set.
		spool: crate::gql::spill::QuerySpool,
		/// Position relative to the beginning of the associated buffer.
		offset: u64,
	},
}

impl RowSet {
	/// Returns the first result.
	pub(super) fn first(&self) -> FerrosGrynnResult<Option<Row>> {
		match self {
			Self::Memory {
				rows,
				..
			} => Ok(rows.first().cloned()),
			Self::Disk {
				spool,
				offset,
			} => spool.page(*offset, 1)?.records.into_iter().next().map(row_from_record).transpose(),
		}
	}

	/// Visits row set.
	pub(super) fn visit(
		&self,
		mut visit: impl FnMut(Row) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory {
				rows,
				..
			} => rows.iter().cloned().try_for_each(visit),
			Self::Disk {
				spool,
				offset,
			} => spool.for_each(*offset, |record| visit(row_from_record(record)?)),
		}
	}

	/// Returns the consume result.
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

	/// Reductions borrow resident rows and one decoded spill row at a time.
	pub(super) fn visit_borrowed(
		&self,
		mut visit: impl FnMut(&Row) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		match self {
			Self::Memory {
				rows,
				..
			} => rows.iter().try_for_each(visit),
			Self::Disk {
				spool,
				offset,
			} => spool.for_each(*offset, |record| visit(&row_from_record(record)?)),
		}
	}
}

/// Row collector state.
pub(super) struct RowCollector {
	/// Memory associated with this row collector.
	memory: Vec<Row>,
	/// Memory bytes associated with this row collector.
	memory_bytes: usize,
	/// Memory admission account retained with the owned data.
	account: BarrierAccount,
	/// Root associated with this row collector.
	root: PathBuf,
	/// Sorter associated with this row collector.
	sorter: Option<QuerySorter>,
	/// Next ordinal associated with this row collector.
	next_ordinal: usize,
	/// Force disk associated with this row collector.
	force_disk: bool,
}

impl RowCollector {
	/// Creates a new `RowCollector` value.
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

	/// Pushes row collector.
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
			let mut sorter = QuerySorter::new(
				&self.root,
				read.spill_context(snapshot)?,
				vec![SortField {
					descending: false,
					nulls_first: false,
				}],
				self.account.empty_shared(),
				true,
			);
			for row in std::mem::take(&mut self.memory) {
				let retained = estimated_row_bytes(&row);
				sorter.push(row_record(row, self.next_ordinal))?;
				// The iterator retains its backing row slots until the loop
				// ends.
				self.account.release(retained.saturating_sub(size_of::<Row>()));
				self.next_ordinal += 1;
			}
			self.account.release_all();
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

	/// Finishes row collector.
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

/// Returns the consume records result.
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

/// Returns the estimated row bytes result.
pub(super) fn estimated_row_bytes(row: &Row) -> usize {
	row.estimated_heap_bytes()
}

/// Returns the row record result.
pub(super) fn row_record(row: Row, ordinal: usize) -> CursorRecord {
	let graph = row.graph.map(Value::from).unwrap_or(Value::None);
	CursorRecord {
		value: Value::Array(
			vec![
				Value::Array(row.values.into()),
				graph,
				Value::Array(row.stable.into()),
				Value::Array(row.sequence.into_iter().map(Value::from).collect::<Vec<_>>().into()),
				Value::Array(
					row.fulltext
						.into_iter()
						.map(|entry| {
							Value::Array(
								vec![
									Value::from(entry.predicate),
									entry.slot.map_or(Value::None, Value::from),
									Value::Bool(entry.matched),
									Value::Bool(entry.highlights),
									Value::from(entry.score_bits),
									Value::from(entry.text.as_ref()),
									Value::Array(
										entry
											.spans
											.into_iter()
											.map(|(start, end)| {
												Value::Array(
													vec![Value::from(start), Value::from(end)]
														.into(),
												)
											})
											.collect::<Vec<_>>()
											.into(),
									),
								]
								.into(),
							)
						})
						.collect::<Vec<_>>()
						.into(),
				),
				Value::Array(
					row.knn
						.into_iter()
						.map(|entry| {
							Value::Array(
								vec![
									Value::from(entry.predicate),
									Value::from(entry.distance_bits),
								]
								.into(),
							)
						})
						.collect::<Vec<_>>()
						.into(),
				),
			]
			.into(),
		),
		anchor: CursorAnchor {
			order_keys: vec![Value::from(ordinal)],
			stable_key: vec![Value::from(ordinal)],
		},
	}
}

/// Returns the row from record result.
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
	let fulltext = take_array_value(values.next(), "fulltext results")?
		.into_iter()
		.map(decode_fulltext_match)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let knn = take_array_value(values.next(), "KNN results")?
		.into_iter()
		.map(decode_knn_match)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if values.next().is_some() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row run contains trailing fields",
		));
	}
	let mut row = Row::new(columns.into_iter().collect())
		.with_graph(graph)
		.with_stable(stable.into_iter().collect())
		.with_sequence(sequence);
	row.fulltext = fulltext;
	row.knn = knn;
	Ok(row)
}

/// Takes array value.
fn take_array_value(value: Option<Value>, name: &str) -> FerrosGrynnResult<Array> {
	match value {
		Some(Value::Array(values)) => Ok(values),
		_ => {
			Err(FerrosGrynnError::invalid_storage_file(format!("query row run has invalid {name}")))
		}
	}
}

/// Decodes fulltext match.
fn decode_fulltext_match(value: Value) -> FerrosGrynnResult<FulltextMatch> {
	let Value::Array(fields) = value else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row fulltext result is not an array",
		));
	};
	let mut fields = fields.into_iter();
	let predicate = decode_u64(fields.next(), "fulltext predicate").and_then(|value| {
		u32::try_from(value).map_err(|_conversion_error| invalid_row_number("fulltext predicate"))
	})?;
	let slot = match fields.next() {
		Some(Value::None) => None,
		value => Some(
			u16::try_from(decode_u64(value, "fulltext slot")?)
				.map_err(|_conversion_error| invalid_row_number("fulltext slot"))?,
		),
	};
	let Some(Value::Bool(matched)) = fields.next() else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row fulltext result has invalid match flag",
		));
	};
	let Some(Value::Bool(highlights)) = fields.next() else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row fulltext result has invalid highlight flag",
		));
	};
	let score_bits = decode_u64(fields.next(), "fulltext score")?;
	let Some(Value::String(text)) = fields.next() else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row fulltext result has invalid text",
		));
	};
	let spans = take_array_value(fields.next(), "fulltext spans")?
		.into_iter()
		.map(|value| {
			let Value::Array(span) = value else {
				return Err(FerrosGrynnError::invalid_storage_file(
					"query row fulltext span is not an array",
				));
			};
			let mut span = span.into_iter();
			let start = u32::try_from(decode_u64(span.next(), "fulltext span start")?)
				.map_err(|_conversion_error| invalid_row_number("fulltext span start"))?;
			let end = u32::try_from(decode_u64(span.next(), "fulltext span end")?)
				.map_err(|_conversion_error| invalid_row_number("fulltext span end"))?;
			if span.next().is_some() {
				return Err(FerrosGrynnError::invalid_storage_file(
					"query row fulltext span has trailing fields",
				));
			}
			Ok((start, end))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if fields.next().is_some() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row fulltext result has trailing fields",
		));
	}
	Ok(FulltextMatch {
		predicate,
		slot,
		matched,
		highlights,
		score_bits,
		text: text.as_str().into(),
		spans,
	})
}

/// Decodes knn match.
fn decode_knn_match(value: Value) -> FerrosGrynnResult<KnnMatch> {
	let Value::Array(fields) = value else {
		return Err(FerrosGrynnError::invalid_storage_file("query row KNN result is not an array"));
	};
	let mut fields = fields.into_iter();
	let predicate = u32::try_from(decode_u64(fields.next(), "KNN predicate")?)
		.map_err(|_conversion_error| invalid_row_number("KNN predicate"))?;
	let distance_bits = decode_u64(fields.next(), "KNN distance")?;
	if fields.next().is_some() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row KNN result has trailing fields",
		));
	}
	Ok(KnnMatch {
		predicate,
		distance_bits,
	})
}

/// Decodes u64.
fn decode_u64(value: Option<Value>, name: &str) -> FerrosGrynnResult<u64> {
	let Some(Value::Number(number)) = value else {
		return Err(invalid_row_number(name));
	};
	u64::try_from(number).map_err(|_conversion_error| invalid_row_number(name))
}

/// Returns the invalid row number result.
fn invalid_row_number(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(format!("query row run has invalid {name}"))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{
		datastore::Datastore,
		test_assertions::{AssertOption as _, AssertResult as _},
	};

	/// Spill context must overlap the still-owned resident input buffer.
	#[test]
	fn collector_spill_keeps_resident_rows_admitted() {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		for ordinal in 0..128 {
			setup.intern_property(&format!("unused_{ordinal}")).assert_ok();
		}
		setup.commit().assert_ok();
		let root = store.inner.current_root();
		let options = crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(100 << 10).assert_some(),
			..Default::default()
		};
		let control = ExecutionControl::new(&options, None);
		let observe = |_| {};
		let work = |_| Ok(());
		let read = RootRead::new(&root, &control, &observe, &work, std::env::temp_dir());
		let snapshot = read.snapshot().assert_ok();
		let mut rows = RowCollector::new(&read, &control, false);
		for _ in 0..2 {
			rows.push(&read, snapshot, Row::new(vec!["x".repeat(40 << 10).into()])).assert_ok();
		}
		assert!(rows.push(&read, snapshot, Row::new(vec!["x".repeat(40 << 10).into()])).is_err());
		assert!(control.barrier_memory_peak() <= options.barrier_memory_bytes.get());
	}

	/// The row scope releases scratch while the sorter keeps its codec context.
	#[test]
	fn collector_forced_spill_retains_context_and_releases_row_scratch() {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		for ordinal in 0..128 {
			setup.intern_property(&format!("unused_{ordinal}")).assert_ok();
		}
		setup.commit().assert_ok();
		let root = store.inner.current_root();
		let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
		let observe = |_| {};
		let work = |_| Ok(());
		let read = RootRead::new(&root, &control, &observe, &work, std::env::temp_dir());
		let snapshot = read.snapshot().assert_ok();
		let fields = read.fields();
		let available = control.barrier_memory_available();
		let mut rows = RowCollector::new(&read, &control, true);
		for ordinal in 0..8 {
			let _scope = fields.transient_scope();
			fields.admit(1 << 20).assert_ok();
			rows.push(&read, snapshot, Row::new(vec![ordinal.into()])).assert_ok();
		}
		assert!(control.barrier_memory_available() < available - (32 << 10));
		assert!(control.barrier_memory_available() > available - (1 << 20));
		let rows = rows.finish().assert_ok();
		assert!(matches!(&rows, RowSet::Disk { .. }));
		let mut count = 0;
		rows.consume(|_row| {
			count += 1;
			Ok(())
		})
		.assert_ok();
		assert_eq!(count, 8);
	}
}
