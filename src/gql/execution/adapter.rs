#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;
use crate::{
	datastore::{
		GraphSchema,
		SystemIndexKind,
		TableField,
		TableIndex,
		TableIndexKind,
		TableIndexState,
		TableKind,
		TableSchemaMode,
	},
	graph::{AtomRef, Dag, Graph, GraphKind, Hypergraph, MaterializedGraph, Metagraph, Mutable},
	syn::DefinitionMode,
	values::{PropertyMap, RecordIdPart, Text, Ulid},
};

/// Parses, analyzes, and executes query text within an existing transaction.
///
/// # Errors
///
/// Returns syntax, semantic, storage, or execution errors produced by the
/// query.
pub fn execute_transaction(
	transaction: &mut Transaction,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let program = crate::gql::parse(source, settings)?;
	execute_parsed_transaction(transaction, source, &program, variables)
}

/// Analyzes and executes an already parsed program within a transaction.
///
/// # Errors
///
/// Returns semantic, storage, or execution errors produced while analyzing or
/// executing the program.
pub fn execute_parsed_transaction(
	transaction: &mut Transaction,
	source: &str,
	program: &Program,
	variables: &Object,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = transaction.query_snapshot()?;
	let mut read = TransactionRead {
		transaction,
		snapshot,
	};
	execute_program(&mut read, source, program, variables)
}

pub(super) struct TransactionRead<'a> {
	pub(super) transaction: &'a mut Transaction,
	pub(super) snapshot: QuerySnapshot,
}

impl QueryCatalog for TransactionRead<'_> {
	fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_table(&self.snapshot, table).map(|schema| query_schema(&schema))
	}

	fn schema_mode(&self, table: &str) -> FerrosGrynnResult<TableSchemaMode> {
		self.transaction.query_table(&self.snapshot, table).map(|schema| schema.schema_mode())
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		self.transaction.query_named_graph(&self.snapshot, name)
	}
}

impl QueryRead for TransactionRead<'_> {
	type Graph<'a>
		= TransactionGraphView<'a>
	where
		Self: 'a;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(self.snapshot.commit_ts()))
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn scan<'a>(
		&'a self,
		_snapshot: Snapshot,
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let mut constraints = Vec::new();
		if let Some(predicate) = predicate {
			collect_scalar_constraints(predicate, slots, &mut constraints);
		}
		let evaluator: Arc<dyn PredicateEvaluator> = Arc::new(QueryPredicate {
			predicate: predicate.cloned(),
			slots: slots.to_vec(),
			source: source.to_owned(),
		});

		let rows = self.transaction.query_row_cursor(&self.snapshot, table, &constraints)?;
		let predicate_graph = self.transaction.register_streaming_query_predicate(
			&self.snapshot,
			table,
			evaluator.clone(),
		)?;
		Ok(Box::new(StorageCursor {
			rows,
			transaction: self.transaction,
			snapshot: &self.snapshot,
			predicate: predicate_graph.map(|graph| (graph, evaluator)),
		}))
	}

	fn get(&self, _snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
		let row = self.transaction.query_point(&self.snapshot, record)?;
		self.transaction.register_query_point(
			&self.snapshot,
			record.table(),
			record,
			row.as_ref(),
		)?;
		Ok(row.map(|row| Row::new(row.values)))
	}

	fn graph(
		&self,
		_snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'_>> {
		self.transaction.query_graph(&self.snapshot, reference)
	}

	fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
		self.transaction.query_property_dictionary()
	}

	fn execute_schema(&mut self, statement: &LogicalSchemaStatement) -> FerrosGrynnResult<Value> {
		execute_schema(self.transaction, &self.snapshot, statement)
	}

	fn execute_dml(
		&mut self,
		statement: &LogicalDmlStatement,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Value> {
		execute_dml(self.transaction, &self.snapshot, statement, slots, source)
	}
}

fn execute_schema(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	statement: &LogicalSchemaStatement,
) -> FerrosGrynnResult<Value> {
	match statement {
		LogicalSchemaStatement::DefineGraph {
			mode,
			name,
			kind,
		} => define_graph(transaction, snapshot, *mode, name, *kind),
		LogicalSchemaStatement::DefineTable {
			mode,
			name,
			kind,
			schema_mode,
		} => define_table(transaction, snapshot, *mode, name, *kind, *schema_mode),
		LogicalSchemaStatement::DefineField {
			mode,
			table,
			name,
			value_type,
		} => define_field(transaction, snapshot, *mode, table, name, value_type.clone()),
		LogicalSchemaStatement::DefineIndex {
			mode,
			table,
			name,
			kind,
			fields,
		} => define_index(transaction, snapshot, *mode, table, name, *kind, fields),
		LogicalSchemaStatement::RemoveGraph {
			if_exists,
			name,
		} => remove_graph(transaction, snapshot, *if_exists, name),
		LogicalSchemaStatement::RemoveTable {
			if_exists,
			name,
		} => remove_table(transaction, snapshot, *if_exists, name),
		LogicalSchemaStatement::RemoveField {
			if_exists,
			table,
			name,
		} => remove_field(transaction, snapshot, *if_exists, table, name),
		LogicalSchemaStatement::RemoveIndex {
			if_exists,
			table,
			name,
		} => remove_index(transaction, snapshot, *if_exists, table, name),
		LogicalSchemaStatement::AlterTableMode {
			if_exists,
			name,
			mode,
		} => alter_table(transaction, snapshot, *if_exists, name, *mode),
		LogicalSchemaStatement::AlterFieldType {
			if_exists,
			table,
			name,
			value_type,
		} => alter_field(transaction, snapshot, *if_exists, table, name, value_type.clone()),
		LogicalSchemaStatement::PrepareIndexRemoval {
			table,
			name,
		} => transaction.prepare_index_removal(table, name).map(|()| Value::None),
		LogicalSchemaStatement::InfoDatabase => info_database(transaction, snapshot),
		LogicalSchemaStatement::InfoTable {
			name,
		} => info_table(transaction, snapshot, name),
		LogicalSchemaStatement::InfoIndex {
			table,
			name,
		} => info_index(transaction, snapshot, table, name),
	}
}

#[derive(Clone)]
struct DmlCandidate {
	target: usize,
	record_id: RecordId,
	kind: Option<AtomKind>,
	properties: PropertyMap,
	before: Object,
	row: Row,
}

impl DmlCandidate {
	const fn exists(&self) -> bool {
		self.kind.is_some()
	}
}

fn execute_dml(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	statement: &LogicalDmlStatement,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	let dictionary = transaction.query_property_dictionary();
	let mut candidates = Vec::new();
	for (target_index, target) in statement.targets.iter().enumerate() {
		collect_dml_candidates(
			transaction,
			snapshot,
			statement.kind,
			target_index,
			target,
			slots,
			source,
			&dictionary,
			&mut candidates,
		)?;
	}
	let mut unique_targets = BTreeSet::new();
	for candidate in &candidates {
		if !unique_targets.insert(candidate.record_id.clone()) {
			return Err(FerrosGrynnError::invalid_argument(format!(
				"DML target '{}' resolves more than once in one statement.",
				candidate.record_id
			)));
		}
	}
	for candidate in &candidates {
		let target = &statement.targets[candidate.target];
		if !expect_matches(target.expect.as_ref(), candidate, slots, source)? {
			return Err(FerrosGrynnError::condition_failed(candidate.record_id.to_string()));
		}
	}

	let returns_none = statement
		.targets
		.first()
		.is_some_and(|target| matches!(target.returning, LogicalDmlReturn::None));
	let mut output = Array::new(Vec::new());
	for candidate in candidates {
		let target = &statement.targets[candidate.target];
		let schema = transaction.query_table(snapshot, &target.table)?;
		let affected = candidate.exists()
			|| matches!(statement.kind, LogicalDmlKind::Create | LogicalDmlKind::Upsert);
		let after = apply_dml_candidate(
			transaction,
			statement.kind,
			target,
			&schema,
			&candidate,
			slots,
			source,
			&dictionary,
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
	target_index: usize,
	target: &LogicalDmlTarget,
	slots: &[Value],
	source: &str,
	dictionary: &PropertyDictionary,
	output: &mut Vec<DmlCandidate>,
) -> FerrosGrynnResult<()> {
	match &target.selector {
		LogicalDmlSelector::Table if kind == LogicalDmlKind::Create => {
			output.push(missing_candidate(
				target_index,
				generated_record_id(&target.table)?,
				&target.schema,
			));
		}
		LogicalDmlSelector::Range(count) => {
			for _ in 0..*count {
				output.push(missing_candidate(
					target_index,
					generated_record_id(&target.table)?,
					&target.schema,
				));
			}
		}
		LogicalDmlSelector::Record(key) => {
			let record_id =
				RecordId::from_value(target.table.as_ref(), eval(key, None, slots, source, None)?)?;
			let row = transaction.query_point(snapshot, &record_id)?;
			transaction.register_query_point(snapshot, &target.table, &record_id, row.as_ref())?;
			let candidate = match row {
				Some(row) => storage_candidate(target_index, row, &target.schema, dictionary)?,
				None => missing_candidate(target_index, record_id, &target.schema),
			};
			if candidate.exists() {
				if filter_matches(target.filter.as_ref(), &candidate.row, slots, source)? {
					output.push(candidate);
				}
			} else if target.filter.is_none() {
				output.push(candidate);
			}
		}
		LogicalDmlSelector::Table => {
			let evaluator: Arc<dyn PredicateEvaluator> = Arc::new(AllRowsPredicate);
			let predicate_graph = transaction.register_streaming_query_predicate(
				snapshot,
				&target.table,
				evaluator.clone(),
			)?;
			let mut cursor = transaction.query_row_cursor(snapshot, &target.table, &[])?;
			let start_len = output.len();
			while let Some(row) = cursor.next_row()? {
				if let Some(graph) = &predicate_graph {
					transaction.register_streamed_query_row(
						snapshot,
						graph,
						&row,
						evaluator.as_ref(),
					)?;
				}
				let candidate = storage_candidate(target_index, row, &target.schema, dictionary)?;
				if filter_matches(target.filter.as_ref(), &candidate.row, slots, source)? {
					output.push(candidate);
				}
			}
			if kind == LogicalDmlKind::Upsert && output.len() == start_len {
				output.push(missing_candidate(
					target_index,
					generated_record_id(&target.table)?,
					&target.schema,
				));
			}
		}
	}
	Ok(())
}

fn generated_record_id(table: &str) -> FerrosGrynnResult<RecordId> {
	RecordId::new(table, RecordIdPart::Ulid(Ulid::new()))
}

fn missing_candidate(target: usize, record_id: RecordId, schema: &Schema) -> DmlCandidate {
	DmlCandidate {
		target,
		record_id,
		kind: None,
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
		kind: row.atom_id.kind(),
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
) -> FerrosGrynnResult<bool> {
	filter.map_or(Ok(true), |filter| {
		eval(filter, Some(row), slots, source, None).map(|value| value.is_truthy())
	})
}

fn expect_matches(
	expect: Option<&LogicalExpect>,
	candidate: &DmlCandidate,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<bool> {
	match expect {
		None => Ok(true),
		Some(LogicalExpect::Exists(expected)) => Ok(candidate.exists() == *expected),
		Some(LogicalExpect::Predicate(predicate)) => {
			eval(predicate, Some(&candidate.row), slots, source, None)
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
	kind: LogicalDmlKind,
	target: &LogicalDmlTarget,
	schema: &TableSchema,
	candidate: &DmlCandidate,
	slots: &[Value],
	source: &str,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<Option<Object>> {
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
		remove_dml_atom(transaction, schema, candidate)?;
		return Ok(None);
	}

	let mut after = if exists {
		candidate.before.clone()
	} else {
		Object::new()
	};
	apply_dml_data(target.data.as_ref(), &candidate.row, slots, source, &mut after)?;
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

fn apply_dml_data(
	data: Option<&LogicalDmlData>,
	row: &Row,
	slots: &[Value],
	source: &str,
	image: &mut Object,
) -> FerrosGrynnResult<()> {
	let Some(data) = data else {
		return Ok(());
	};
	match data {
		LogicalDmlData::Content(value) => {
			*image = eval_object(value, row, slots, source)?;
			image.remove("id");
		}
		LogicalDmlData::Replace(value) => {
			let replacement = eval_object(value, row, slots, source)?;
			if replacement.contains_key("id") {
				return Err(FerrosGrynnError::invalid_argument(
					"REPLACE cannot change the implicit read-only `id` field.",
				));
			}
			*image = replacement;
		}
		LogicalDmlData::Merge(value) => {
			for (name, value) in eval_object(value, row, slots, source)? {
				if name.as_str().eq_ignore_ascii_case("id") {
					return Err(FerrosGrynnError::invalid_argument(
						"MERGE cannot change the implicit read-only `id` field.",
					));
				}
				image.insert(name.as_ref(), value);
			}
		}
		LogicalDmlData::Patch(value) => {
			for (name, value) in eval_object(value, row, slots, source)? {
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
					eval(value, Some(row), slots, source, None).map(|value| (name.clone(), value))
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			for (name, value) in values {
				image.insert(name.as_ref(), value);
			}
		}
		LogicalDmlData::Unset(fields) => {
			for field in fields {
				image.remove(field);
			}
		}
	}
	Ok(())
}

fn eval_object(
	expr: &TypedExpr,
	row: &Row,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Object> {
	match eval(expr, Some(row), slots, source, None)? {
		Value::Object(value) => Ok(value),
		value => Err(FerrosGrynnError::invalid_argument(format!(
			"DML object mode received {}",
			value.value_type()
		))),
	}
}

fn validate_dml_object(schema: &TableSchema, image: &Object) -> FerrosGrynnResult<()> {
	for (name, value) in image {
		if name.as_str() == "id" {
			continue;
		}
		let field = schema.fields().find(|field| field.name().as_str() == name.as_str());
		if schema.schema_mode() == TableSchemaMode::Schemafull && field.is_none() {
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

fn dml_graph(schema: &TableSchema) -> FerrosGrynnResult<GraphRef> {
	schema.graph().cloned().ok_or_else(|| {
		FerrosGrynnError::query_feature_not_supported(
			"DML storage for tables in the system graph (GraphId=0)",
		)
	})
}

fn create_dml_atom(
	transaction: &mut Transaction,
	schema: &TableSchema,
	candidate: &DmlCandidate,
	after: &Object,
) -> FerrosGrynnResult<()> {
	if schema.kind() == TableKind::Relation {
		return Err(FerrosGrynnError::invalid_argument(
			"CREATE cannot create a relation atom without endpoints; use RELATE.",
		));
	}
	let graph = dml_graph(schema)?;
	let properties = dml_property_map(transaction, after)?;
	let atom = transaction.mutate_graph(&graph)?.create_node(
		candidate.record_id.clone(),
		None,
		properties,
	)?;
	transaction.assign_atom_to_table(&graph, &atom, schema.name().as_str())
}

fn update_dml_atom(
	transaction: &mut Transaction,
	schema: &TableSchema,
	candidate: &DmlCandidate,
	after: &Object,
	dictionary: &PropertyDictionary,
) -> FerrosGrynnResult<()> {
	let graph = dml_graph(schema)?;
	let atom = AtomRef::new(
		candidate.kind.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("DML update candidate has no atom kind")
		})?,
		candidate.record_id.clone(),
	);
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
	let mut mutation = transaction.mutate_graph(&graph)?;
	for key in removals {
		mutation.remove_property(&atom, key)?;
	}
	for (key, value) in assignments {
		mutation.set_property(&atom, key, value)?;
	}
	Ok(())
}

fn remove_dml_atom(
	transaction: &mut Transaction,
	schema: &TableSchema,
	candidate: &DmlCandidate,
) -> FerrosGrynnResult<()> {
	let graph = dml_graph(schema)?;
	let atom = AtomRef::new(
		candidate.kind.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("DML delete candidate has no atom kind")
		})?,
		candidate.record_id.clone(),
	);
	transaction.mutate_graph(&graph)?.remove_atom(&atom)
}

fn dml_property_map(transaction: &Transaction, image: &Object) -> FerrosGrynnResult<PropertyMap> {
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
) -> FerrosGrynnResult<Option<Value>> {
	let selected = after.unwrap_or(before);
	match returning {
		LogicalDmlReturn::None => Ok(None),
		LogicalDmlReturn::Before => Ok(Some(Value::Object(before.clone()))),
		LogicalDmlReturn::After => Ok(after.cloned().map(Value::Object)),
		LogicalDmlReturn::Diff => Ok(Some(Value::Object(dml_diff(before, after)))),
		LogicalDmlReturn::Value(value) => {
			let row = object_row(schema, selected);
			eval(value, Some(&row), slots, source, None).map(Some)
		}
		LogicalDmlReturn::Fields(items) => {
			let row = object_row(schema, selected);
			let mut object = Object::new();
			for item in items {
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
						object.insert(name.as_ref(), eval(value, Some(&row), slots, source, None)?);
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

fn dml_diff(before: &Object, after: Option<&Object>) -> Object {
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

struct AllRowsPredicate;

impl PredicateEvaluator for AllRowsPredicate {
	fn matches(&self, _row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		Ok(true)
	}
}

fn define_graph(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	mode: DefinitionMode,
	name: &str,
	kind: GraphKind,
) -> FerrosGrynnResult<Value> {
	let existing = visible_graph(transaction, snapshot, name)?;
	match (mode, existing) {
		(DefinitionMode::IfNotExists, Some(_)) => return Ok(Value::None),
		(DefinitionMode::Overwrite, Some(schema)) if schema.reference().kind() == kind => {
			return Ok(Value::None);
		}
		(DefinitionMode::Overwrite, Some(_)) => {
			return Err(FerrosGrynnError::query_feature_not_supported(
				"changing a graph family with DEFINE GRAPH OVERWRITE",
			));
		}
		_ => {}
	}
	let id = RecordId::new("graph", RecordIdPart::String(name.into()))?;
	transaction.create_named_graph(name, id, empty_graph(kind)?)?;
	Ok(Value::None)
}

fn empty_graph(kind: GraphKind) -> FerrosGrynnResult<MaterializedGraph> {
	match kind {
		GraphKind::Graph => Ok(Graph::<Mutable>::new().freeze().into()),
		GraphKind::Dag => Ok(Dag::<Mutable>::new().freeze().into()),
		GraphKind::Hypergraph => Ok(Hypergraph::<Mutable>::new().freeze().into()),
		GraphKind::Metagraph => Ok(Metagraph::<Mutable>::new().freeze().into()),
		_ => Err(FerrosGrynnError::query_feature_not_supported(format!(
			"empty {} graph construction",
			kind.name()
		))),
	}
}

fn define_table(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	mode: DefinitionMode,
	name: &str,
	kind: TableKind,
	schema_mode: TableSchemaMode,
) -> FerrosGrynnResult<Value> {
	let existing = visible_table(transaction, snapshot, name)?;
	if mode == DefinitionMode::IfNotExists && existing.is_some() {
		return Ok(Value::None);
	}
	if mode == DefinitionMode::Overwrite
		&& let Some(mut schema) = existing
	{
		schema.kind = kind;
		schema.mode = schema_mode;
		transaction.overwrite_table(schema)?;
		return Ok(Value::None);
	}
	let schema = TableSchema::new_system(name, kind, [])?.with_schema_mode(schema_mode);
	transaction.define_table(schema)?;
	Ok(Value::None)
}

fn define_field(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	mode: DefinitionMode,
	table: &str,
	name: &str,
	value_type: Type,
) -> FerrosGrynnResult<Value> {
	let schema = transaction.query_table(snapshot, table)?;
	let exists = schema.fields().any(|field| field.name().as_str() == name);
	if mode == DefinitionMode::IfNotExists && exists {
		return Ok(Value::None);
	}
	let field = TableField::new(name, value_type);
	if mode == DefinitionMode::Overwrite && exists {
		transaction.overwrite_field(table, field)?;
	} else {
		transaction.define_field(table, field)?;
	}
	Ok(Value::None)
}

fn define_index(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	mode: DefinitionMode,
	table: &str,
	name: &str,
	kind: TableIndexKind,
	fields: &[Box<str>],
) -> FerrosGrynnResult<Value> {
	let schema = transaction.query_table(snapshot, table)?;
	let exists = schema.indexes().any(|index| index.name().as_str() == name);
	if mode == DefinitionMode::IfNotExists && exists {
		return Ok(Value::None);
	}
	let mut index = TableIndex::new(name, kind, fields.iter().map(Box::as_ref))?;
	if schema.graph().is_none() {
		index.state = TableIndexState::Stale;
	}
	if mode == DefinitionMode::Overwrite && exists {
		transaction.overwrite_index(table, index)?;
	} else {
		transaction.define_index(table, index)?;
	}
	Ok(Value::None)
}

fn remove_graph(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	if_exists: bool,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = visible_graph(transaction, snapshot, name)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::graph_does_not_exist(name));
	};
	transaction.remove_graph(schema.reference())?;
	Ok(Value::None)
}

fn remove_table(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	if_exists: bool,
	name: &str,
) -> FerrosGrynnResult<Value> {
	if visible_table(transaction, snapshot, name)?.is_none() {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::table_does_not_exist(name));
	}
	transaction.remove_table(name)?;
	Ok(Value::None)
}

fn remove_field(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	if_exists: bool,
	table: &str,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = visible_table(transaction, snapshot, table)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::table_does_not_exist(table));
	};
	if !schema.fields().any(|field| field.name().as_str() == name) && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_field(table, name)?;
	Ok(Value::None)
}

fn remove_index(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	if_exists: bool,
	table: &str,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = visible_table(transaction, snapshot, table)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::table_does_not_exist(table));
	};
	if !schema.indexes().any(|index| index.name().as_str() == name) && if_exists {
		return Ok(Value::None);
	}
	transaction.remove_index(table, name)?;
	Ok(Value::None)
}

fn alter_table(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	if_exists: bool,
	name: &str,
	mode: TableSchemaMode,
) -> FerrosGrynnResult<Value> {
	if visible_table(transaction, snapshot, name)?.is_none() && if_exists {
		return Ok(Value::None);
	}
	transaction.set_table_schema_mode(name, mode)?;
	Ok(Value::None)
}

fn alter_field(
	transaction: &mut Transaction,
	snapshot: &QuerySnapshot,
	if_exists: bool,
	table: &str,
	name: &str,
	value_type: Type,
) -> FerrosGrynnResult<Value> {
	let Some(schema) = visible_table(transaction, snapshot, table)? else {
		if if_exists {
			return Ok(Value::None);
		}
		return Err(FerrosGrynnError::table_does_not_exist(table));
	};
	if !schema.fields().any(|field| field.name().as_str() == name) && if_exists {
		return Ok(Value::None);
	}
	transaction.overwrite_field(table, TableField::new(name, value_type))?;
	Ok(Value::None)
}

fn visible_graph(
	transaction: &Transaction,
	snapshot: &QuerySnapshot,
	name: &str,
) -> FerrosGrynnResult<Option<GraphSchema>> {
	Ok(transaction
		.query_schema_graphs(snapshot)?
		.into_iter()
		.find(|schema| schema.name().as_str() == name))
}

fn visible_table(
	transaction: &Transaction,
	snapshot: &QuerySnapshot,
	name: &str,
) -> FerrosGrynnResult<Option<TableSchema>> {
	Ok(transaction
		.query_schema_tables(snapshot)?
		.into_iter()
		.find(|schema| schema.name().as_str() == name))
}

fn info_database(transaction: &Transaction, snapshot: &QuerySnapshot) -> FerrosGrynnResult<Value> {
	let graphs = transaction
		.query_schema_graphs(snapshot)?
		.into_iter()
		.map(|schema| (schema.name().clone(), graph_definition(&schema)))
		.collect::<Object>();
	let tables = transaction
		.query_schema_tables(snapshot)?
		.into_iter()
		.map(|schema| (schema.name().clone(), table_definition(&schema)))
		.collect::<Object>();
	Ok(Value::from(Object::from_iter([
		("analyzers", Value::from(Object::new())),
		("configs", Value::from(Object::new())),
		("functions", Value::from(Object::new())),
		("graphs", Value::from(graphs)),
		("models", Value::from(Object::new())),
		("params", Value::from(Object::new())),
		("tables", Value::from(tables)),
	])))
}

fn info_table(
	transaction: &Transaction,
	snapshot: &QuerySnapshot,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let schema = transaction.query_table(snapshot, name)?;
	let fields = schema
		.fields()
		.map(|field| (field.name().clone(), field_definition(name, field)))
		.collect::<Object>();
	let mut indexes = schema
		.indexes()
		.map(|index| (index.name().clone(), index_info(name, index)))
		.collect::<Object>();
	if schema.primary_index().is_some() {
		indexes.insert("primary", system_index_info(name, SystemIndexKind::Primary));
	}
	Ok(Value::from(Object::from_iter([
		("definition", table_definition(&schema)),
		("events", Value::from(Object::new())),
		("fields", Value::from(fields)),
		("indexes", Value::from(indexes)),
		("lives", Value::from(Object::new())),
		("tables", Value::from(Object::new())),
	])))
}

fn info_index(
	transaction: &Transaction,
	snapshot: &QuerySnapshot,
	table: &str,
	name: &str,
) -> FerrosGrynnResult<Value> {
	let schema = transaction.query_table(snapshot, table)?;
	if name == "primary" && schema.primary_index().is_some() {
		return Ok(system_index_info(table, SystemIndexKind::Primary));
	}
	schema
		.indexes()
		.find(|index| index.name().as_str() == name)
		.map(|index| index_info(table, index))
		.ok_or_else(|| {
			FerrosGrynnError::invalid_argument(format!("index '{table}.{name}' does not exist"))
		})
}

fn graph_definition(schema: &GraphSchema) -> Value {
	let name = quote_name(schema.name().as_str());
	let suffix = match schema.reference().kind() {
		GraphKind::Graph => String::new(),
		GraphKind::Dag => " TYPE DAG".to_owned(),
		GraphKind::Hypergraph => " TYPE HYPER".to_owned(),
		GraphKind::Metagraph => " TYPE META".to_owned(),
		kind => format!(" TYPE {}", kind.name().to_ascii_uppercase()),
	};
	Value::from(format!("DEFINE GRAPH {name}{suffix}"))
}

fn table_definition(schema: &TableSchema) -> Value {
	let kind = match schema.kind() {
		TableKind::Any => "ANY",
		TableKind::Normal => "NORMAL",
		TableKind::Relation => "RELATION",
	};
	let mode = match schema.schema_mode() {
		TableSchemaMode::Schemaless => "SCHEMALESS",
		TableSchemaMode::Schemafull => "SCHEMAFULL",
	};
	Value::from(format!("DEFINE TABLE {} TYPE {kind} {mode}", quote_name(schema.name().as_str())))
}

fn field_definition(table: &str, field: &TableField) -> Value {
	Value::from(format!(
		"DEFINE FIELD {} ON TABLE {} TYPE {}",
		quote_name(field.name().as_str()),
		quote_name(table),
		field.value_type()
	))
}

fn index_info(table: &str, index: &TableIndex) -> Value {
	let state = index_state_name(index.state());
	Value::from(Object::from_iter([
		("building", Value::from(Object::from_iter([("status", Value::from(state))]))),
		("definition", index_definition(table, index)),
		("state", Value::from(state)),
		("system", Value::Bool(false)),
	]))
}

fn system_index_info(table: &str, kind: SystemIndexKind) -> Value {
	let name = match kind {
		SystemIndexKind::Primary => "primary",
		SystemIndexKind::Label => "label",
		SystemIndexKind::Adjacency => "adjacency",
	};
	Value::from(Object::from_iter([
		("building", Value::from(Object::from_iter([("status", Value::from("ready"))]))),
		("definition", Value::from(format!("SYSTEM INDEX {name} ON TABLE {}", quote_name(table)))),
		("state", Value::from("ready")),
		("system", Value::Bool(true)),
	]))
}

fn index_definition(table: &str, index: &TableIndex) -> Value {
	let body = match index.kind() {
		TableIndexKind::Count => "COUNT".to_owned(),
		TableIndexKind::Standard | TableIndexKind::Unique => {
			let fields = index.fields().map(|field| quote_name(field.as_str())).collect::<Vec<_>>();
			let unique = if index.kind() == TableIndexKind::Unique {
				" UNIQUE"
			} else {
				""
			};
			format!("FIELDS {}{unique}", fields.join(", "))
		}
	};
	Value::from(format!(
		"DEFINE INDEX {} ON TABLE {} {body}",
		quote_name(index.name().as_str()),
		quote_name(table)
	))
}

const fn index_state_name(state: TableIndexState) -> &'static str {
	match state {
		TableIndexState::Ready => "ready",
		TableIndexState::Stale => "stale",
		TableIndexState::PreparedForRemoval => "prepared_for_removal",
	}
}

fn quote_name(name: &str) -> String {
	format!("`{}`", name.replace('`', "``"))
}

fn collect_scalar_constraints(
	expr: &TypedExpr,
	slots: &[Value],
	constraints: &mut Vec<ScalarIndexConstraint>,
) {
	let TypedExprKind::Binary {
		left,
		op,
		right,
	} = &expr.kind
	else {
		return;
	};
	if matches!(op, BinaryOperator::And) {
		collect_scalar_constraints(left, slots, constraints);
		collect_scalar_constraints(right, slots, constraints);
		return;
	}
	let comparison = match op {
		BinaryOperator::Equal | BinaryOperator::Is => ScalarComparison::Equal,
		BinaryOperator::Less => ScalarComparison::Less,
		BinaryOperator::LessOrEqual => ScalarComparison::LessOrEqual,
		BinaryOperator::Greater => ScalarComparison::Greater,
		BinaryOperator::GreaterOrEqual => ScalarComparison::GreaterOrEqual,
		_ => return,
	};
	if let (TypedExprKind::Column(column), Some(value)) =
		(&left.kind, scalar_constraint_value(right, slots))
	{
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison,
			value,
		});
	} else if let (Some(value), TypedExprKind::Column(column)) =
		(scalar_constraint_value(left, slots), &right.kind)
	{
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison: match comparison {
				ScalarComparison::Equal => ScalarComparison::Equal,
				ScalarComparison::Less => ScalarComparison::Greater,
				ScalarComparison::LessOrEqual => ScalarComparison::GreaterOrEqual,
				ScalarComparison::Greater => ScalarComparison::Less,
				ScalarComparison::GreaterOrEqual => ScalarComparison::LessOrEqual,
			},
			value,
		});
	}
}

fn scalar_constraint_value(expr: &TypedExpr, slots: &[Value]) -> Option<Value> {
	match &expr.kind {
		TypedExprKind::Constant(value) => Some(value.clone()),
		TypedExprKind::Slot(slot) => slots.get(usize::try_from(*slot).ok()?).cloned(),
		_ => None,
	}
}

fn query_schema(schema: &TableSchema) -> Schema {
	let mut columns = Vec::with_capacity(schema.fields().len() + 1);
	columns.push(Column {
		name: "id".into(),
		ty: Type::Record,
	});
	columns.extend(schema.fields().map(|field| Column {
		name: field.name().as_str().into(),
		ty: field.value_type().clone(),
	}));
	Schema {
		columns,
	}
}

struct QueryPredicate {
	predicate: Option<TypedExpr>,
	slots: Vec<Value>,
	source: String,
}

impl PredicateEvaluator for QueryPredicate {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		self.predicate.as_ref().map_or(Ok(true), |predicate| {
			eval(predicate, Some(&Row::new(row.values.clone())), &self.slots, &self.source, None)
				.map(|value| value.is_truthy())
		})
	}
}

/// Streaming adapter that keeps predicate registration alive after an early
/// LIMIT or cursor drop.
struct StorageCursor<'a> {
	rows: QueryStorageCursor<'a>,
	transaction: &'a Transaction,
	snapshot: &'a QuerySnapshot,
	predicate: Option<(RecordId, Arc<dyn PredicateEvaluator>)>,
}

impl RowCursor for StorageCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.rows.next_row()? else {
			return Ok(None);
		};
		if let Some((graph, evaluator)) = &self.predicate {
			self.transaction.register_streamed_query_row(
				self.snapshot,
				graph,
				&row,
				evaluator.as_ref(),
			)?;
		}
		Ok(Some(Row::new(row.values)))
	}
}
