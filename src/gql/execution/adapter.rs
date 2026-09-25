#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use super::*;
use crate::datastore::{ClassSchema, ClassSchemaMode};

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
	execute_transaction_with_options(
		transaction,
		source,
		variables,
		crate::gql::QueryExecutionOptions {
			parser_settings: settings,
			..crate::gql::QueryExecutionOptions::default()
		},
	)
}

pub fn execute_transaction_with_options(
	transaction: &mut Transaction,
	source: &str,
	variables: &Object,
	options: crate::gql::QueryExecutionOptions,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	options.validate()?;
	let program = match crate::gql::parse(source, options.parser_settings) {
		Ok(program) => program,
		Err(error) => {
			transaction.abort_gql();
			return Err(error);
		}
	};
	if program.statements.iter().any(|statement| {
		matches!(
			statement.kind,
			crate::syn::StatementKind::Import(_)
				| crate::syn::StatementKind::Begin(_)
				| crate::syn::StatementKind::Commit(_)
				| crate::syn::StatementKind::Cancel
		)
	}) {
		transaction.abort_gql();
		return Err(FerrosGrynnError::query_feature_not_supported(
			"transaction or import statement inside an open Transaction",
		));
	}
	for statement in &program.statements {
		if let Err(error) = crate::datastore::validate_control_structure(statement, 0, false) {
			transaction.abort_gql();
			return Err(error);
		}
	}
	for name in variables.keys() {
		if transaction.gql_bindings().contains_key(name) {
			transaction.abort_gql();
			return Err(FerrosGrynnError::invalid_argument(format!(
				"Execute parameter `${name}` conflicts with a transaction binding."
			)));
		}
	}
	let mut bindings = transaction.gql_bindings().clone();
	for (name, value) in variables {
		bindings.insert(name.clone(), value.clone());
	}
	let mut results = Vec::with_capacity(program.statements.len());
	for statement in &program.statements {
		let started = std::time::Instant::now();
		let let_name = match &statement.kind {
			crate::syn::StatementKind::Let(statement) if statement.name.value.as_ref() != "_" => {
				Some(statement.name.value.as_ref())
			}
			_ => None,
		};
		if let Some(name) = let_name
			&& (transaction.gql_bindings().contains_key(name) || variables.contains_key(name))
		{
			let result = QueryResult {
				kind: crate::gql::statement_kind(&statement.kind),
				elapsed: started.elapsed(),
				result: Err(FerrosGrynnError::invalid_argument(format!(
					"Binding `${name}` is already declared in this scope."
				))),
				continuation: None,
			};
			transaction.abort_gql();
			results.push(result);
			break;
		}
		let timeout = match crate::gql::resolve_timeout(statement.timeout.as_ref(), &bindings) {
			Ok(timeout) => timeout,
			Err(error) => {
				transaction.abort_gql();
				results.push(QueryResult {
					kind: crate::gql::statement_kind(&statement.kind),
					elapsed: started.elapsed(),
					result: Err(error),
					continuation: None,
				});
				break;
			}
		};
		let control = crate::gql::options::ExecutionControl::new(&options, timeout);
		let execution = transaction.run_gql_statement(|transaction| {
			execute_parsed_statement_with_bindings(
				transaction,
				source,
				statement,
				&mut bindings,
				&control,
			)
		});
		let mut result = match execution {
			Ok(result) => result,
			Err(error) => QueryResult {
				kind: crate::gql::statement_kind(&statement.kind),
				elapsed: started.elapsed(),
				result: Err(error),
				continuation: None,
			},
		};
		let failed = result.result.is_err();
		if !failed && let Some(name) = let_name {
			let value = bindings.get(name).cloned().unwrap_or(Value::None);
			let mut transaction_bindings = transaction.gql_bindings().clone();
			transaction_bindings.insert(name, value);
			transaction.replace_gql_bindings(transaction_bindings);
		}
		result.elapsed = started.elapsed();
		results.push(result);
		if failed {
			transaction.abort_gql();
			break;
		}
	}
	Ok(results)
}

/// Analyzes a parsed mutation program without executing it and returns the
/// logical program with its captured database commit timestamp.
pub(crate) fn analyze_parsed_transaction(
	transaction: &mut Transaction,
	source: &str,
	program: &Program,
	variables: &Object,
) -> FerrosGrynnResult<(LogicalProgram, u64)> {
	let snapshot = transaction.query_snapshot()?;
	let read = TransactionRead {
		transaction,
		snapshot,
	};
	let logical = analyze(program, source, &read, variables)?;
	Ok((logical, read.snapshot.commit_ts()))
}

#[cfg(test)]
pub(crate) fn execute_parsed_transaction_with_bindings(
	transaction: &mut Transaction,
	source: &str,
	program: &Program,
	bindings: &mut Object,
	control: &crate::gql::options::ExecutionControl,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = transaction.query_snapshot()?;
	let mut read = TransactionRead {
		transaction,
		snapshot,
	};
	execute_program(&mut read, source, program, bindings, control)
}

pub(crate) fn execute_parsed_statement_with_bindings(
	transaction: &mut Transaction,
	source: &str,
	statement: &crate::syn::Statement,
	bindings: &mut Object,
	control: &crate::gql::options::ExecutionControl,
) -> FerrosGrynnResult<QueryResult> {
	let snapshot = transaction.query_snapshot()?;
	let mut read = TransactionRead {
		transaction,
		snapshot,
	};
	execute_parsed_statement(&mut read, source, statement, bindings, control)
}

pub(super) struct TransactionRead<'a> {
	pub(super) transaction: &'a mut Transaction,
	pub(super) snapshot: QuerySnapshot,
}

impl TransactionRead<'_> {
	fn refresh_snapshot(&mut self) -> FerrosGrynnResult<()> {
		self.snapshot = self.transaction.query_snapshot()?;
		Ok(())
	}
}

impl QueryCatalog for TransactionRead<'_> {
	fn schema(&self, class: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| query_schema(&schema))
	}

	fn schema_mode(&self, class: &str) -> FerrosGrynnResult<ClassSchemaMode> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| schema.schema_mode())
	}

	fn class_kind(&self, class: &str) -> FerrosGrynnResult<AtomKind> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| schema.kind())
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
	type Savepoint = crate::datastore::TransactionSavepoint;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(self.snapshot.commit_ts()))
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn scan<'a>(
		&'a self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		_source: &str,
		full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let mut constraints = Vec::new();
		if let Some(predicate) = predicate {
			collect_scalar_constraints(predicate, slots, &mut constraints);
		}
		let rows = self.transaction.query_row_cursor(&self.snapshot, class, &constraints, graph)?;
		Ok(Box::new(StorageCursor {
			rows,
			dictionary: self.transaction.query_property_dictionary(),
			full_row,
		}))
	}

	fn get(
		&self,
		_snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<Row>> {
		let row = self.transaction.query_point(&self.snapshot, record, graph)?;
		Ok(row
			.map(|row| storage_row(row, &self.transaction.query_property_dictionary(), true))
			.transpose()?)
	}

	fn graph_rows(
		&self,
		_snapshot: Snapshot,
		references: Option<&[GraphRef]>,
	) -> FerrosGrynnResult<Vec<Row>> {
		let schemas = self.transaction.query_schema_graphs(&self.snapshot)?;
		let dictionary = self.transaction.query_property_dictionary();
		let selected = if let Some(references) = references {
			references
				.iter()
				.map(|reference| {
					schemas
						.iter()
						.find(|schema| schema.reference() == reference)
						.cloned()
						.ok_or_else(|| {
							FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
						})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?
		} else {
			let mut schemas = schemas;
			schemas.sort_by_key(crate::datastore::GraphSchema::id);
			schemas
		};
		selected
			.into_iter()
			.map(|schema| {
				let graph = self.transaction.query_graph(&self.snapshot, schema.reference())?;
				let properties = super::graph_properties::property_map_to_object(
					schema.properties(),
					&dictionary,
				)?;
				Ok(Row::new(vec![
					Value::from(schema.reference().clone()),
					Value::from(schema.name().clone()),
					Value::from(schema.reference().kind().name()),
					Value::from(super::graph_properties::storage_mode_name(
						schema.reference().storage_mode(),
					)),
					Value::from(graph.revision()),
					Value::from(graph.node_count()?),
					Value::from(graph.edge_count()?),
					Value::Object(properties),
				])
				.with_stable(vec![Value::from(schema.reference().clone())]))
			})
			.collect()
	}

	fn match_candidates(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		binding: &ResolvedMatchBinding,
	) -> FerrosGrynnResult<Vec<AtomId>> {
		let (class, record) = match &binding.selector {
			ResolvedMatchSelector::Any => (None, None),
			ResolvedMatchSelector::Class(class) => (Some(class.as_ref()), None),
			ResolvedMatchSelector::Record(record) => (None, Some(record)),
		};
		self.transaction.query_match_candidates(
			&self.snapshot,
			graph,
			binding.domain.atom_kind(),
			class,
			record,
			binding.label.as_deref(),
		)
	}

	fn record_field(
		&self,
		_snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		self.transaction.query_record_field(&self.snapshot, record, graph, field)
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

	fn spill_context(
		&self,
		_snapshot: Snapshot,
	) -> FerrosGrynnResult<crate::values::storage::EncodeContext> {
		let mut context = crate::values::storage::EncodeContext::new(
			self.transaction.query_property_dictionary().as_ref().clone(),
		);
		for class in self.transaction.query_schema_classes(&self.snapshot)? {
			let id = class.id().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
			})?;
			context.register_class_name(id.get(), class.name().clone())?;
		}
		for graph in self.transaction.query_schema_graphs(&self.snapshot)? {
			context.register_graph(
				graph.reference().clone(),
				crate::values::storage::DiskGraphRef::new(graph.id()),
			)?;
		}
		Ok(context)
	}

	fn query_scratch_root(&self) -> std::path::PathBuf {
		self.transaction.query_scratch_root()
	}

	fn savepoint(&self) -> Self::Savepoint {
		self.transaction.gql_savepoint()
	}

	fn restore_savepoint(&mut self, savepoint: Self::Savepoint) -> FerrosGrynnResult<()> {
		self.transaction.restore_gql_savepoint(savepoint);
		self.refresh_snapshot()
	}

	fn assert_invariants(
		&mut self,
		graph: Option<&GraphRef>,
		detail_limit: usize,
	) -> FerrosGrynnResult<usize> {
		self.transaction.assert_invariants(graph, detail_limit)
	}

	fn execute_schema(
		&mut self,
		statement: &LogicalSchemaStatement,
		slots: &[Value],
		source: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		let snapshot = self.snapshot.clone();
		let value = self.transaction.atomic_mutation(|transaction| {
			execute_schema(transaction, &snapshot, statement, slots, source, control)
		})?;
		self.refresh_snapshot()?;
		Ok(value)
	}

	fn execute_dml(
		&mut self,
		statement: &LogicalDmlStatement,
		slots: &[Value],
		source: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		let snapshot = self.snapshot.clone();
		let value = self.transaction.atomic_mutation(|transaction| {
			execute_dml(transaction, &snapshot, statement, slots, source, control)
		})?;
		self.refresh_snapshot()?;
		Ok(value)
	}

	fn execute_graph_dml(
		&mut self,
		statement: &LogicalGraphDmlStatement,
		slots: &[Value],
		source: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		let snapshot = self.snapshot.clone();
		let value = self.transaction.atomic_mutation(|transaction| {
			execute_graph_dml(transaction, &snapshot, statement, slots, source, control)
		})?;
		self.refresh_snapshot()?;
		Ok(value)
	}
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

fn query_schema(schema: &ClassSchema) -> Schema {
	let mut columns = Vec::with_capacity(schema.fields().len() + 5);
	columns.push(Column {
		name: "id".into(),
		qualifier: None,
		ty: Type::Record,
	});
	columns.extend([
		Column {
			name: "class".into(),
			qualifier: None,
			ty: Type::String,
		},
		Column {
			name: "kind".into(),
			qualifier: None,
			ty: Type::String,
		},
		Column {
			name: "label".into(),
			qualifier: None,
			ty: Type::Option(Box::new(Type::String)),
		},
		Column {
			name: "properties".into(),
			qualifier: None,
			ty: Type::Object,
		},
	]);
	columns.extend(schema.fields().map(|field| Column {
		name: field.name().as_str().into(),
		qualifier: None,
		ty: field.value_type().clone(),
	}));
	Schema {
		columns,
	}
}

/// Streaming adapter that keeps predicate registration alive after an early
/// LIMIT or cursor drop.
struct StorageCursor<'a> {
	rows: QueryStorageCursor<'a>,
	dictionary: Arc<PropertyDictionary>,
	full_row: bool,
}

impl RowCursor for StorageCursor<'_> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		control.check()?;
		let Some(row) = self.rows.next_row()? else {
			return Ok(None);
		};
		storage_row(row, &self.dictionary, self.full_row).map(Some)
	}
}

fn storage_row(
	row: QueryStorageRow,
	dictionary: &PropertyDictionary,
	full_row: bool,
) -> FerrosGrynnResult<Row> {
	if !full_row {
		return Ok(Row::compact_atom(row.values));
	}
	let record_id = row.record_id.clone();
	let properties = Value::Object(
		row.properties
			.iter()
			.map(|(key, value)| {
				dictionary.resolve(key).map(|name| (name.as_str(), value.clone())).ok_or_else(
					|| {
						FerrosGrynnError::invalid_storage_file(
							"query row property key is absent from the property dictionary",
						)
					},
				)
			})
			.collect::<FerrosGrynnResult<Object>>()?,
	);
	let mut values = Vec::with_capacity(row.values.len() + 4);
	let mut source_values = row.values.into_iter();
	values.push(
		source_values.next().unwrap_or_else(|| Value::Record(Box::new(row.record_id.clone()))),
	);
	values.push(Value::from(row.record_id.class()));
	values.push(Value::from(row.kind.to_string()));
	values.push(row.label.map_or(Value::None, Value::from));
	values.push(properties);
	values.extend(source_values);
	Ok(Row::new(values).with_stable(vec![Value::Record(Box::new(record_id))]))
}
