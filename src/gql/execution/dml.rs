#![allow(
	clippy::wildcard_imports,
	reason = "the DML executor shares typed IR and runtime helpers with its parent module"
)]

use super::*;
use crate::{
	datastore::{ClassSchema, ClassSchemaMode},
	values::{PropertyMap, RecordIdPart, Text, Ulid},
};

mod graph;
mod scoped;

pub(super) use graph::execute_graph_dml;

#[derive(Clone)]
struct DmlCandidate {
	target: usize,
	record_id: RecordId,
	kind: Option<AtomKind>,
	local_id: Option<AtomId>,
	base_properties: PropertyMap,
	properties: PropertyMap,
	before: Object,
	row: Row,
}

impl DmlCandidate {
	const fn exists(&self) -> bool {
		self.kind.is_some()
	}
}

pub(super) fn execute_dml(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	statement: &LogicalDmlStatement,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let dictionary = transaction.query_property_dictionary();
	let mut candidates = Vec::new();
	for (target_index, target) in statement.targets.iter().enumerate() {
		control.check()?;
		collect_dml_candidates(
			transaction,
			snapshot,
			statement.kind,
			statement.graph.as_ref(),
			target_index,
			target,
			slots,
			source,
			&dictionary,
			&mut candidates,
			control,
		)?;
	}
	let mut unique_targets = BTreeSet::new();
	for candidate in &candidates {
		control.check()?;
		if !unique_targets.insert(candidate.record_id.clone()) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"DML target '{}' resolves more than once in one statement.",
				candidate.record_id
			)));
		}
	}
	for candidate in &candidates {
		let target = &statement.targets[candidate.target];
		if !expect_matches(target.expect.as_ref(), candidate, slots, source, control)? {
			return Err(FerrosGrynnError::condition_failed(candidate.record_id.to_string()));
		}
	}

	let returns_none = statement
		.targets
		.first()
		.is_some_and(|target| matches!(target.returning, LogicalDmlReturn::None));
	let mut output = Array::new(Vec::new());
	for candidate in candidates {
		control.check()?;
		let target = &statement.targets[candidate.target];
		let schema = transaction.query_class(snapshot, &target.class)?;
		let affected = candidate.exists()
			|| matches!(statement.kind, LogicalDmlKind::Create | LogicalDmlKind::Upsert);
		let after = apply_dml_candidate(
			transaction,
			statement,
			target,
			&schema,
			&candidate,
			slots,
			source,
			&dictionary,
			control,
		)?;
		if affected
			&& !returns_none
			&& let Some(value) = dml_return_value(
				&target.returning,
				&target.schema,
				&candidate.before,
				after.as_ref(),
				slots,
				source,
				control,
			)? {
			output.push(value);
		}
	}
	if returns_none {
		Ok(Value::None)
	} else {
		Ok(Value::Array(output))
	}
}

#[allow(
	clippy::too_many_arguments,
	reason = "DML preflight keeps snapshot, target, and expression state explicit"
)]
fn collect_dml_candidates(
	transaction: &Transaction,
	snapshot: &QuerySnapshot,
	kind: LogicalDmlKind,
	graph: Option<&GraphRef>,
	target_index: usize,
	target: &LogicalDmlTarget,
	slots: &[Value],
	source: &str,
	dictionary: &PropertyDictionary,
	output: &mut Vec<DmlCandidate>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	match &target.selector {
		LogicalDmlSelector::Class if kind == LogicalDmlKind::Create => {
			output.push(missing_candidate(
				target_index,
				generated_record_id(&target.class)?,
				&target.schema,
			));
		}
		LogicalDmlSelector::Range(count) => {
			for _ in 0..*count {
				control.check()?;
				output.push(missing_candidate(
					target_index,
					generated_record_id(&target.class)?,
					&target.schema,
				));
			}
		}
		LogicalDmlSelector::Record(key) => {
			let record_id = RecordId::from_value(
				target.class.as_ref(),
				eval(key, None, slots, source, None, Some(control))?,
			)?;
			let row = transaction.query_point(snapshot, &record_id, graph)?;
			if graph.is_some()
				&& row.is_none()
				&& transaction.query_point(snapshot, &record_id, None)?.is_some()
			{
				return Err(FerrosGrynnError::atom_does_not_exist(
					"occurrence",
					record_id.to_string(),
				));
			}
			let candidate = match row {
				Some(row) => storage_candidate(target_index, row, &target.schema, dictionary)?,
				None => missing_candidate(target_index, record_id, &target.schema),
			};
			if candidate.exists() {
				if filter_matches(target.filter.as_ref(), &candidate.row, slots, source, control)? {
					output.push(candidate);
				}
			} else if target.filter.is_none() {
				output.push(candidate);
			}
		}
		LogicalDmlSelector::Class => {
			let mut cursor = transaction.query_row_cursor(snapshot, &target.class, &[], graph)?;
			let start_len = output.len();
			while let Some(row) = cursor.next_row()? {
				control.check()?;
				let candidate = storage_candidate(target_index, row, &target.schema, dictionary)?;
				if filter_matches(target.filter.as_ref(), &candidate.row, slots, source, control)? {
					output.push(candidate);
				}
			}
			if kind == LogicalDmlKind::Upsert && output.len() == start_len {
				output.push(missing_candidate(
					target_index,
					generated_record_id(&target.class)?,
					&target.schema,
				));
			}
		}
	}
	Ok(())
}

fn generated_record_id(class: &str) -> FerrosGrynnResult<RecordId> {
	RecordId::new(class, RecordIdPart::Ulid(Ulid::new()))
}

fn missing_candidate(target: usize, record_id: RecordId, schema: &Schema) -> DmlCandidate {
	DmlCandidate {
		target,
		record_id,
		kind: None,
		local_id: None,
		base_properties: PropertyMap::new(),
		properties: PropertyMap::new(),
		before: Object::new(),
		row: Row::new(vec![Value::None; schema.columns.len()]),
	}
}

fn storage_candidate(
	target: usize,
	row: QueryStorageRow,
	schema: &Schema,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<DmlCandidate> {
	let mut before = Object::new();
	before.insert("id", Value::Record(Box::new(row.record_id.clone())));
	for (key, value) in &row.properties {
		let name = dictionary.resolve(key).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(format!(
				"property dictionary does not resolve key {key}"
			))
		})?;
		before.insert(name.clone(), value.clone());
	}
	let values = schema
		.columns
		.iter()
		.map(|column| before.get(&column.name).cloned().unwrap_or(Value::None))
		.collect();
	Ok(DmlCandidate {
		target,
		record_id: row.record_id,
		kind: Some(row.kind),
		local_id: row.local_id,
		base_properties: row.base_properties,
		properties: row.properties,
		before,
		row: Row::new(values),
	})
}

fn filter_matches(
	filter: Option<&TypedExpr>,
	row: &Row,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<bool> {
	filter.map_or(Ok(true), |filter| {
		eval(filter, Some(row), slots, source, None, Some(control)).map(|value| value.is_truthy())
	})
}

fn expect_matches(
	expect: Option<&LogicalExpect>,
	candidate: &DmlCandidate,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<bool> {
	match expect {
		None => Ok(true),
		Some(LogicalExpect::Exists(expected)) => Ok(candidate.exists() == *expected),
		Some(LogicalExpect::Predicate(predicate)) => {
			eval(predicate, Some(&candidate.row), slots, source, None, Some(control))
				.map(|value| value.is_truthy())
		}
	}
}

#[allow(
	clippy::too_many_arguments,
	reason = "the mutation boundary keeps pre-image and runtime context explicit"
)]
fn apply_dml_candidate(
	transaction: &mut Transaction,
	statement: &LogicalDmlStatement,
	target: &LogicalDmlTarget,
	schema: &ClassSchema,
	candidate: &DmlCandidate,
	slots: &[Value],
	source: &str,
	dictionary: &PropertyDictionary,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<Object>> {
	let kind = statement.kind;
	let exists = candidate.exists();
	if matches!(kind, LogicalDmlKind::Update | LogicalDmlKind::Delete) && !exists {
		return Ok(None);
	}
	if kind == LogicalDmlKind::Create && exists {
		return Err(FerrosGrynnError::graph_constraint_violation(
			"record id",
			format!("duplicate RecordId '{}'", candidate.record_id),
		));
	}
	if kind == LogicalDmlKind::Delete {
		remove_dml_atom(transaction, schema, candidate, statement.cascade)?;
		return Ok(None);
	}
	if let Some(graph) = &statement.graph {
		return scoped::apply_graph_update(
			transaction,
			graph,
			statement.label.as_ref(),
			target.data.as_ref(),
			schema,
			candidate,
			slots,
			source,
			dictionary,
			control,
		)
		.map(Some);
	}

	let mut after = if exists {
		candidate.before.clone()
	} else {
		Object::new()
	};
	apply_dml_data(target.data.as_ref(), &candidate.row, slots, source, &mut after, control)?;
	after.insert("id", Value::Record(Box::new(candidate.record_id.clone())));
	validate_dml_object(schema, &after)?;
	if exists {
		if after != candidate.before {
			update_dml_atom(transaction, schema, candidate, &after, dictionary)?;
		}
	} else {
		create_dml_atom(transaction, schema, candidate, &after)?;
	}
	Ok(Some(after))
}

pub(super) fn apply_dml_data(
	data: Option<&LogicalDmlData>,
	row: &Row,
	slots: &[Value],
	source: &str,
	image: &mut Object,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let Some(data) = data else {
		return Ok(());
	};
	match data {
		LogicalDmlData::Content(value) => {
			*image = eval_object(value, row, slots, source, control)?;
			image.remove("id");
		}
		LogicalDmlData::Replace(value) => {
			let replacement = eval_object(value, row, slots, source, control)?;
			if replacement.contains_key("id") {
				return Err(FerrosGrynnError::invalid_argument(
					"REPLACE cannot change the implicit read-only `id` field.",
				));
			}
			*image = replacement;
		}
		LogicalDmlData::Merge(value) => {
			for (name, value) in eval_object(value, row, slots, source, control)? {
				control.check()?;
				if name.as_str().eq_ignore_ascii_case("id") {
					return Err(FerrosGrynnError::invalid_argument(
						"MERGE cannot change the implicit read-only `id` field.",
					));
				}
				image.insert(name.as_ref(), value);
			}
		}
		LogicalDmlData::Patch(value) => {
			for (name, value) in eval_object(value, row, slots, source, control)? {
				control.check()?;
				if name.as_str().eq_ignore_ascii_case("id") {
					return Err(FerrosGrynnError::invalid_argument(
						"PATCH cannot change the implicit read-only `id` field.",
					));
				}
				if value == Value::None {
					image.remove(name.as_str());
				} else {
					image.insert(name, value);
				}
			}
		}
		LogicalDmlData::Set(assignments) => {
			let values = assignments
				.iter()
				.map(|(name, value)| {
					eval(value, Some(row), slots, source, None, Some(control))
						.map(|value| (name.clone(), value))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			for (name, value) in values {
				image.insert(name.as_ref(), value);
			}
		}
		LogicalDmlData::Unset(fields) => {
			for field in fields {
				control.check()?;
				image.remove(field);
			}
		}
		LogicalDmlData::Reset(_) => {
			return Err(FerrosGrynnError::invalid_argument(
				"RESET is only valid for a graph-scoped update.",
			));
		}
	}
	Ok(())
}

fn eval_object(
	expr: &TypedExpr,
	row: &Row,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Object> {
	match eval(expr, Some(row), slots, source, None, Some(control))? {
		Value::Object(value) => Ok(value),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"DML object mode received {}",
			value.value_type()
		))),
	}
}

pub(super) fn validate_dml_object(schema: &ClassSchema, image: &Object) -> FerrosGrynnResult<()> {
	for (name, value) in image {
		if name.as_str() == "id" {
			continue;
		}
		let field = schema.fields().find(|field| field.name().as_str() == name.as_str());
		if schema.schema_mode() == ClassSchemaMode::Schemafull && field.is_none() {
			return Err(FerrosGrynnError::unknown_field(name.as_str()));
		}
		if let Some(field) = field
			&& !compatible(field.value_type(), &value.value_type())
		{
			return Err(FerrosGrynnError::invalid_argument(format!(
				"field '{}' expects {}, found {}",
				name,
				field.value_type(),
				value.value_type()
			)));
		}
	}
	Ok(())
}

fn create_dml_atom(
	transaction: &mut Transaction,
	_schema: &ClassSchema,
	candidate: &DmlCandidate,
	after: &Object,
) -> FerrosGrynnResult<()> {
	let properties = dml_property_map(transaction, after)?;
	transaction.create_atom(candidate.record_id.clone(), properties).map(|_atom| ())
}

fn update_dml_atom(
	transaction: &mut Transaction,
	_schema: &ClassSchema,
	candidate: &DmlCandidate,
	after: &Object,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<()> {
	let removals = candidate
		.properties
		.iter()
		.filter_map(|(key, _value)| {
			dictionary
				.resolve(key)
				.filter(|name| !after.contains_key(name.as_str()))
				.map(|_name| key)
		})
		.collect::<Vec<_>>();
	let mut assignments = Vec::new();
	for (name, value) in after {
		if name.as_str() == "id" {
			continue;
		}
		let key = transaction.intern_property(name.as_str())?;
		if candidate.properties.get(key) != Some(value) {
			assignments.push((key, value.clone()));
		}
	}
	for key in removals {
		transaction.remove_base_property(&candidate.record_id, key)?;
	}
	for (key, value) in assignments {
		transaction.set_base_property(&candidate.record_id, key, value)?;
	}
	Ok(())
}

fn remove_dml_atom(
	transaction: &mut Transaction,
	_schema: &ClassSchema,
	candidate: &DmlCandidate,
	cascade: bool,
) -> FerrosGrynnResult<()> {
	transaction.delete_atom_cascade(&candidate.record_id, cascade)
}

fn dml_property_map(
	transaction: &mut Transaction,
	image: &Object,
) -> FerrosGrynnResult<PropertyMap> {
	image
		.iter()
		.filter(|(name, _value)| name.as_str() != "id")
		.map(|(name, value)| {
			transaction.intern_property(name.as_str()).map(|key| (key, value.clone()))
		})
		.collect()
}

fn dml_return_value(
	returning: &LogicalDmlReturn,
	schema: &Schema,
	before: &Object,
	after: Option<&Object>,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<Value>> {
	let selected = after.unwrap_or(before);
	match returning {
		LogicalDmlReturn::None => Ok(None),
		LogicalDmlReturn::Before => Ok(Some(Value::Object(before.clone()))),
		LogicalDmlReturn::After => Ok(after.cloned().map(Value::Object)),
		LogicalDmlReturn::Diff => Ok(Some(Value::Object(dml_diff(before, after)))),
		LogicalDmlReturn::Value(value) => {
			let row = object_row(schema, selected);
			eval(value, Some(&row), slots, source, None, Some(control)).map(Some)
		}
		LogicalDmlReturn::Fields(items) => {
			let row = object_row(schema, selected);
			let mut object = Object::new();
			for item in items {
				control.check()?;
				match item {
					LogicalDmlReturnItem::Wildcard => {
						for (name, value) in selected {
							object.insert(name.clone(), value.clone());
						}
					}
					LogicalDmlReturnItem::Expr {
						name,
						value,
					} => {
						object.insert(
							name.as_ref(),
							eval(value, Some(&row), slots, source, None, Some(control))?,
						);
					}
				}
			}
			Ok(Some(Value::Object(object)))
		}
	}
}

fn object_row(schema: &Schema, object: &Object) -> Row {
	Row::new(
		schema
			.columns
			.iter()
			.map(|column| object.get(&column.name).cloned().unwrap_or(Value::None))
			.collect(),
	)
}

pub(super) fn dml_diff(before: &Object, after: Option<&Object>) -> Object {
	let empty = Object::new();
	let after = after.unwrap_or(&empty);
	before
		.keys()
		.chain(after.keys())
		.map(Text::as_str)
		.collect::<BTreeSet<_>>()
		.into_iter()
		.filter(|name| before.get(name) != after.get(name))
		.map(|name| {
			(
				name,
				Value::Object(Object::from_iter([
					("before", before.get(name).cloned().unwrap_or(Value::None)),
					("after", after.get(name).cloned().unwrap_or(Value::None)),
				])),
			)
		})
		.collect()
}
