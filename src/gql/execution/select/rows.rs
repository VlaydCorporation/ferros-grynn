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

fn take_array_value(value: Option<Value>, name: &str) -> FerrosGrynnResult<Array> {
	match value {
		Some(Value::Array(values)) => Ok(values),
		_ => {
			Err(FerrosGrynnError::invalid_storage_file(format!("query row run has invalid {name}")))
		}
	}
}

fn decode_fulltext_match(value: Value) -> FerrosGrynnResult<FulltextMatch> {
	let Value::Array(fields) = value else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query row fulltext result is not an array",
		));
	};
	let mut fields = fields.into_iter();
	let predicate = decode_u64(fields.next(), "fulltext predicate").and_then(|value| {
		u32::try_from(value).map_err(|_| invalid_row_number("fulltext predicate"))
	})?;
	let slot = match fields.next() {
		Some(Value::None) => None,
		value => Some(
			u16::try_from(decode_u64(value, "fulltext slot")?)
				.map_err(|_| invalid_row_number("fulltext slot"))?,
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
				.map_err(|_| invalid_row_number("fulltext span start"))?;
			let end = u32::try_from(decode_u64(span.next(), "fulltext span end")?)
				.map_err(|_| invalid_row_number("fulltext span end"))?;
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

fn decode_knn_match(value: Value) -> FerrosGrynnResult<KnnMatch> {
	let Value::Array(fields) = value else {
		return Err(FerrosGrynnError::invalid_storage_file("query row KNN result is not an array"));
	};
	let mut fields = fields.into_iter();
	let predicate = u32::try_from(decode_u64(fields.next(), "KNN predicate")?)
		.map_err(|_| invalid_row_number("KNN predicate"))?;
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

fn decode_u64(value: Option<Value>, name: &str) -> FerrosGrynnResult<u64> {
	let Some(Value::Number(number)) = value else {
		return Err(invalid_row_number(name));
	};
	u64::try_from(number).map_err(|_| invalid_row_number(name))
}

fn invalid_row_number(name: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(format!("query row run has invalid {name}"))
}
