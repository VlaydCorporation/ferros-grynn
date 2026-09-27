//! MATCH outer-row, DISTINCT, projection, and ordering adapters.

use super::*;
use crate::gql::{
	cursor::{CursorAnchor, CursorRecord},
	spill::{QuerySorter, RecordStream, SortField},
};

/// Preserves the standalone input row when the complete optional clause has no
/// match.
pub(in crate::gql::execution) struct OptionalCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) fallback: Option<Row>,
	pub(in crate::gql::execution) yielded: bool,
}

impl RowCursor for OptionalCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		if let Some(row) = self.input.next(control)? {
			self.yielded = true;
			return Ok(Some(row));
		}
		if self.yielded {
			return Ok(None);
		}
		self.yielded = true;
		Ok(self.fallback.take())
	}
}

/// Projects MATCH bindings while retaining ordering keys and stable public
/// identities for the result barriers.
pub(in crate::gql::execution) struct MatchProjectCursor<'a> {
	pub(in crate::gql::execution) input: Box<dyn RowCursor + 'a>,
	pub(in crate::gql::execution) expressions: &'a [TypedExpr],
	pub(in crate::gql::execution) order: &'a [LogicalOrder],
	pub(in crate::gql::execution) slots: &'a [Value],
	pub(in crate::gql::execution) source: &'a str,
	pub(in crate::gql::execution) fields: &'a dyn AtomFieldRead,
	pub(in crate::gql::execution) random_seed: Option<u64>,
	pub(in crate::gql::execution) ordinal: usize,
}

impl RowCursor for MatchProjectCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.input.next(control)? else {
			return Ok(None);
		};
		let projected = self
			.expressions
			.iter()
			.map(|expression| {
				eval(
					expression,
					Some(&row),
					self.slots,
					self.source,
					Some(self.fields),
					Some(control),
				)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		if self.order.is_empty() && !control.deterministic() {
			return Ok(Some(Row::new(projected)));
		}
		let mut order_row = row.clone();
		order_row.values.extend(projected.iter().cloned());
		let binding_key = row.values.clone();
		let order = self
			.order
			.iter()
			.map(|order| {
				if order.random {
					let seed = self.random_seed.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("RAND ordering has no statement seed")
					})?;
					return Ok(Value::from(crate::gql::optimizer::canonical_hash(
						crate::gql::optimizer::PLAN_ENCODING_VERSION,
						b"match-order-rand-v3",
						&(seed, &binding_key),
					)));
				}
				eval(
					&order.expression,
					Some(&order_row),
					self.slots,
					self.source,
					Some(self.fields),
					Some(control),
				)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		let mut output = Row::new(projected);
		output.order = order;
		output.stable = binding_key;
		output.stable.push(Value::from(self.ordinal));
		output.sequence.push(self.ordinal);
		self.ordinal = self
			.ordinal
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH output ordinal overflow"))?;
		Ok(Some(output))
	}
}

/// Materializes a stable DISTINCT barrier using the shared bounded query
/// sorter. The first sort groups equal rows; the second restores the first
/// occurrence order.
pub(in crate::gql::execution) fn distinct_rows<'a, Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	mut input: Box<dyn RowCursor + 'a>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
	let root = control
		.temporary_directory
		.as_deref()
		.map(ToOwned::to_owned)
		.unwrap_or_else(|| read.query_scratch_root());
	let mut grouped = QuerySorter::new(
		&root,
		read.spill_context(snapshot)?,
		vec![ascending()],
		control.barrier_account(),
		false,
	)?;
	let mut ordinal = 0_usize;
	while let Some(row) = input.next(control)? {
		let key = Value::Array(row.values.clone().into());
		grouped.push(match_record(row, vec![key], vec![Value::from(ordinal)]))?;
		ordinal = ordinal
			.checked_add(1)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("MATCH DISTINCT ordinal overflow"))?;
	}

	let mut grouped = grouped.finish()?.into_stream()?;
	let mut ordered = QuerySorter::new(
		&root,
		read.spill_context(snapshot)?,
		vec![ascending()],
		control.barrier_account(),
		false,
	)?;
	let mut previous = None;
	while let Some(record) = grouped.next()? {
		control.check()?;
		let key = record.anchor.order_keys.first().cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("MATCH DISTINCT row has no grouping key")
		})?;
		if previous.as_ref() == Some(&key) {
			continue;
		}
		previous = Some(key);
		let ordinal = record.anchor.stable_key.first().cloned().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("MATCH DISTINCT row has no ordinal")
		})?;
		ordered.push(CursorRecord {
			value: record.value,
			anchor: CursorAnchor {
				order_keys: vec![ordinal.clone()],
				stable_key: vec![ordinal],
			},
		})?;
	}
	Ok(Box::new(RecordRowCursor {
		records: ordered.finish()?.into_stream()?,
	}))
}

/// Orders projected MATCH rows through the shared bounded query sorter.
pub(in crate::gql::execution) fn sort_match_rows<'a, Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	mut input: Box<dyn RowCursor + 'a>,
	order: &[LogicalOrder],
	control: &ExecutionControl,
) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
	let fields = if order.is_empty() {
		vec![ascending()]
	} else {
		order
			.iter()
			.map(|item| SortField {
				descending: item.direction == crate::syn::SelectOrderDirection::Descending,
				nulls_first: item.nulls == crate::syn::SelectNulls::First,
			})
			.collect()
	};
	let root = control
		.temporary_directory
		.as_deref()
		.map(ToOwned::to_owned)
		.unwrap_or_else(|| read.query_scratch_root());
	let mut sorter = QuerySorter::new(
		&root,
		read.spill_context(snapshot)?,
		fields,
		control.barrier_account(),
		false,
	)?;
	while let Some(row) = input.next(control)? {
		let keys = if order.is_empty() {
			vec![Value::Array(row.stable.clone().into())]
		} else {
			row.order.clone()
		};
		sorter.push(match_record(row.clone(), keys, row.stable.clone()))?;
	}
	Ok(Box::new(RecordRowCursor {
		records: sorter.finish()?.into_stream()?,
	}))
}

fn ascending() -> SortField {
	SortField {
		descending: false,
		nulls_first: false,
	}
}

struct RecordRowCursor {
	records: RecordStream,
}

impl RowCursor for RecordRowCursor {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		self.records.next()?.map(match_row).transpose()
	}
}

fn match_record(row: Row, order_keys: Vec<Value>, stable_key: Vec<Value>) -> CursorRecord {
	let atoms = row
		.atoms
		.into_iter()
		.map(|atom| atom.map_or(Value::None, bound_atom_value))
		.collect::<Vec<_>>();
	let used_edges = row.used_edges.into_iter().map(bound_atom_value).collect();
	let used_atoms = row.used_atoms.into_iter().map(bound_atom_value).collect();
	let current = row.current.map_or(Value::None, bound_atom_value);
	let graph = row.graph.map(Value::from).unwrap_or(Value::None);
	let value = Value::Array(
		vec![
			Value::Array(row.values.into()),
			Value::Array(atoms.into()),
			Value::Array(used_edges),
			Value::Array(used_atoms),
			current,
			graph,
			Value::Array(row.order.into()),
			Value::Array(row.stable.into()),
			Value::Array(row.sequence.into_iter().map(Value::from).collect::<Vec<_>>().into()),
		]
		.into(),
	);
	CursorRecord {
		value,
		anchor: CursorAnchor {
			order_keys,
			stable_key,
		},
	}
}

fn match_row(record: CursorRecord) -> FerrosGrynnResult<Row> {
	let Value::Array(fields) = record.value else {
		return Err(invalid_match_run("record is not an array"));
	};
	let mut fields = fields.into_iter();
	let values = take_array(&mut fields, "values")?;
	let atoms = take_array(&mut fields, "atoms")?
		.into_iter()
		.map(optional_atom)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let used_edges = take_array(&mut fields, "used edges")?
		.into_iter()
		.map(required_atom)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let used_atoms = take_array(&mut fields, "used atoms")?
		.into_iter()
		.map(required_atom)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let current = fields.next().ok_or_else(|| invalid_match_run("missing current atom"))?;
	let current = optional_atom(current)?;
	let graph = match fields.next() {
		Some(Value::GraphRef(graph)) => Some(*graph),
		Some(Value::None) => None,
		_ => return Err(invalid_match_run("invalid graph reference")),
	};
	let order = take_array(&mut fields, "order keys")?.into_iter().collect();
	let stable = take_array(&mut fields, "stable key")?.into_iter().collect();
	let sequence = take_array(&mut fields, "sequence")?
		.into_iter()
		.map(|value| match value {
			Value::Number(number) => usize::try_from(number)
				.map_err(|_error| invalid_match_run("sequence does not fit usize")),
			_ => Err(invalid_match_run("sequence is not an integer")),
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if fields.next().is_some() {
		return Err(invalid_match_run("record has trailing fields"));
	}
	Ok(Row {
		values: values.into_iter().collect(),
		compact_atom: false,
		sequence,
		stable,
		order,
		graph,
		atoms,
		used_edges,
		used_atoms,
		current,
		trace: None,
		trace_cost: Number::Int(0),
	})
}

fn take_array(
	fields: &mut impl Iterator<Item = Value>,
	name: &'static str,
) -> FerrosGrynnResult<Array> {
	match fields.next() {
		Some(Value::Array(values)) => Ok(values),
		_ => Err(invalid_match_run(&format!("invalid {name}"))),
	}
}

fn bound_atom_value(atom: BoundAtom) -> Value {
	Value::Array(vec![Value::from(atom.graph.get()), Value::from(atom.atom.raw())].into())
}

fn optional_atom(value: Value) -> FerrosGrynnResult<Option<BoundAtom>> {
	match value {
		Value::None => Ok(None),
		value => required_atom(value).map(Some),
	}
}

fn required_atom(value: Value) -> FerrosGrynnResult<BoundAtom> {
	let Value::Array(parts) = value else {
		return Err(invalid_match_run("bound atom is not a pair"));
	};
	let mut parts = parts.into_iter();
	let graph = required_u64(
		parts.next().ok_or_else(|| invalid_match_run("bound atom has no graph"))?,
		"graph identity",
	)?;
	let raw = required_u64(
		parts.next().ok_or_else(|| invalid_match_run("bound atom has no handle"))?,
		"atom handle",
	)?;
	if parts.next().is_some() {
		return Err(invalid_match_run("bound atom has trailing fields"));
	}
	let graph = crate::graph::GraphId::new(graph)
		.ok_or_else(|| invalid_match_run("graph identity is invalid"))?;
	let atom = AtomId::from_raw(raw);
	if atom.kind().is_none() {
		return Err(invalid_match_run("atom handle is null"));
	}
	Ok(BoundAtom::new(graph, atom))
}

fn required_u64(value: Value, name: &'static str) -> FerrosGrynnResult<u64> {
	let Value::Number(number) = value else {
		return Err(invalid_match_run(&format!("{name} is not an integer")));
	};
	u64::try_from(number).map_err(|_error| invalid_match_run(&format!("{name} does not fit u64")))
}

fn invalid_match_run(message: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(format!("invalid MATCH query run: {message}"))
}
