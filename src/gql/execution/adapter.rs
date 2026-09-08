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
	graph::{Dag, Graph, GraphKind, Hypergraph, MaterializedGraph, Metagraph, Mutable},
	syn::DefinitionMode,
	values::RecordIdPart,
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
		self.snapshot.property_dictionary()
	}

	fn execute_schema(&mut self, statement: &LogicalSchemaStatement) -> FerrosGrynnResult<Value> {
		execute_schema(self.transaction, &self.snapshot, statement)
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
