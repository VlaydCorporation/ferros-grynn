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
		let control = crate::gql::options::ExecutionControl::new(&options, timeout)
			.with_publication_deferred(true);
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

	fn mutation_count(&self) -> usize {
		self.transaction.mutation_count()
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn planner_snapshot(
		&self,
		snapshot: Snapshot,
		demand: &crate::gql::optimizer::PlannerDemand,
	) -> FerrosGrynnResult<crate::gql::optimizer::PlannerSnapshot> {
		use crate::{
			gql::optimizer::{
				PlannerClass,
				PlannerField,
				PlannerGraph,
				PlannerIndex,
				PlannerPropertyHistogram,
				PlannerSnapshot,
				PlannerStatisticsMeta,
			},
			stats::{PropertyHistogramKey, StatisticsScope},
		};

		let class_schemas = self.transaction.query_schema_classes(&self.snapshot)?;
		let graph_schemas = if demand.all_graphs || !demand.graphs.is_empty() {
			self.transaction.query_schema_graphs(&self.snapshot)?
		} else {
			Vec::new()
		};
		let statistics = self.transaction.query_statistics(&self.snapshot)?;
		let base = statistics.base();
		let graph_ids = if demand.all_graphs {
			graph_schemas.iter().map(crate::datastore::GraphSchema::id).collect()
		} else {
			demand.graphs.clone()
		};
		let mut classes = std::collections::BTreeMap::new();
		for class in
			class_schemas.into_iter().filter(|class| demand.classes.contains(class.name().as_str()))
		{
			let class_id = class.id().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
			})?;
			let fields = class
				.fields()
				.map(|field| {
					Ok(PlannerField {
						id: field.id().ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"published field is missing SchemaId",
							)
						})?,
						name: field.name().clone(),
						value_type: field.value_type().clone(),
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let indexes = class
				.indexes()
				.map(|index| {
					let (physical_binding, exact_count) =
						self.transaction.query_index_binding(&self.snapshot, &class, index)?;
					Ok(PlannerIndex {
						id: index.id().ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"published index is missing IndexId",
							)
						})?,
						version: index.version(),
						name: index.name().clone(),
						kind: index.kind(),
						state: index.state(),
						scope: index.scope(),
						fields: index.fields().cloned().collect(),
						physical_binding,
						exact_count,
					})
				})
				.collect::<FerrosGrynnResult<Vec<_>>>()?;
			let graph_rows = graph_ids
				.iter()
				.map(|graph| {
					(
						*graph,
						statistics
							.graph(*graph)
							.and_then(|statistics| statistics.classes.get(&class_id).copied())
							.unwrap_or(0),
					)
				})
				.collect();
			classes.insert(class.name().clone(), PlannerClass {
				id: class_id,
				version: class.version(),
				name: class.name().clone(),
				fields,
				indexes,
				base_rows: base.classes.get(&class_id).copied().unwrap_or(0),
				graph_rows,
			});
		}
		let mut graphs = std::collections::BTreeMap::new();
		for graph in graph_schemas.into_iter().filter(|graph| graph_ids.contains(&graph.id())) {
			let graph_statistics = statistics.graph(graph.id());
			graphs.insert(graph.id(), PlannerGraph {
				reference: graph.reference().clone(),
				version: graph.version(),
				statistics: graph_statistics.map_or_else(
					PlannerStatisticsMeta::default,
					|statistics| PlannerStatisticsMeta {
						revision: statistics.revision,
						analyzed_revision: statistics.analyzed_revision,
						dirty_rows: statistics.dirty_rows,
					},
				),
				node_rows: graph_statistics.map_or(0, |statistics| statistics.node_count),
				edge_rows: graph_statistics.map_or(0, |statistics| statistics.edge_count),
				total_out_degree: graph_statistics
					.map_or(0, |statistics| statistics.total_out_degree),
				total_in_degree: graph_statistics
					.map_or(0, |statistics| statistics.total_in_degree),
				total_undirected_degree: graph_statistics
					.map_or(0, |statistics| statistics.total_undirected_degree),
				maximum_out_degree: graph_statistics
					.map_or(0, |statistics| statistics.maximum_out_degree),
				maximum_in_degree: graph_statistics
					.map_or(0, |statistics| statistics.maximum_in_degree),
				labels: graph_statistics
					.map_or_else(std::collections::BTreeMap::new, |statistics| {
						statistics.labels.clone()
					}),
			});
		}
		let mut property_histograms = std::collections::BTreeMap::new();
		for requested in &demand.properties {
			let class = classes.get(requested.class.as_ref()).ok_or_else(|| {
				FerrosGrynnError::class_does_not_exist(requested.class.to_string())
			})?;
			let Some(field) =
				class.fields.iter().find(|field| field.name.as_str() == requested.field.as_ref())
			else {
				// Schemaless properties have no durable field identity and
				// therefore cannot own a persisted property
				// histogram.
				continue;
			};
			let key = PropertyHistogramKey {
				class_id: class.id,
				field_id: field.id,
				scope: requested.scope,
			};
			let histogram = match requested.scope {
				StatisticsScope::Base => base.property_histograms.get(&key),
				StatisticsScope::Effective {
					graph,
				} => statistics
					.graph(graph)
					.and_then(|statistics| statistics.property_histograms.get(&key)),
			};
			if let Some(histogram) = histogram {
				property_histograms.insert(key, PlannerPropertyHistogram {
					key,
					histogram: histogram.clone(),
					method: histogram.method,
				});
			}
		}
		let mut degree_histograms = std::collections::BTreeMap::new();
		for (graph, keys) in &demand.degrees {
			let Some(graph_statistics) = statistics.graph(*graph) else {
				continue;
			};
			for key in keys {
				if let Some(histogram) = graph_statistics.degree_histograms.get(key) {
					degree_histograms.insert((*graph, key.clone()), histogram.clone());
				}
			}
		}
		Ok(PlannerSnapshot {
			revision: snapshot.0,
			base_statistics: PlannerStatisticsMeta {
				revision: base.revision,
				analyzed_revision: base.analyzed_revision,
				dirty_rows: base.dirty_rows,
			},
			classes,
			graphs,
			property_histograms,
			degree_histograms,
		})
	}

	fn scan<'a>(
		&'a self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		predicate: Option<&TypedExpr>,
		index: Option<(crate::datastore::IndexId, bool)>,
		window: Option<IndexScanWindow<'_>>,
		slots: &[Value],
		_source: &str,
		full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let mut constraints = Vec::new();
		if let Some(predicate) = predicate {
			collect_scalar_constraints(predicate, slots, &mut constraints);
		}
		let window = window
			.map(|window| -> FerrosGrynnResult<_> {
				let after = window
					.after
					.map(|anchor| -> FerrosGrynnResult<_> {
						let record = anchor
							.stable_key
							.iter()
							.find_map(|value| match value {
								Value::Record(record) => Some(record.as_ref().clone()),
								_ => None,
							})
							.ok_or_else(|| {
								FerrosGrynnError::invalid_query_cursor(
									"index continuation has no public RecordId tie key",
								)
							})?;
						Ok((anchor.order_keys.clone(), record))
					})
					.transpose()?;
				Ok(ScalarIndexWindow {
					after,
					maximum: window.maximum,
				})
			})
			.transpose()?;
		let rows = self.transaction.query_row_cursor(
			&self.snapshot,
			class,
			&constraints,
			graph,
			index,
			window.as_ref(),
		)?;
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

	fn count_index(
		&self,
		_snapshot: Snapshot,
		index: crate::datastore::IndexId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64> {
		self.transaction.query_count_index(&self.snapshot, index, graph)
	}

	fn probe_index<'a>(
		&'a self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		values: &[Value],
		full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let rows = self.transaction.query_index_probe_cursor(
			&self.snapshot,
			class,
			graph,
			index,
			values,
		)?;
		Ok(Box::new(StorageCursor {
			rows,
			dictionary: self.transaction.query_property_dictionary(),
			full_row,
		}))
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
	if *op == BinaryOperator::In
		&& let TypedExprKind::Column(column) = &left.kind
		&& let Some(value) = scalar_constraint_value(right, slots)
	{
		let values = match value {
			Value::Array(values) => values.into_iter().collect(),
			Value::Set(values) => values.into_iter().collect(),
			_ => return,
		};
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison: ScalarComparison::Equal,
			values,
		});
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
			values: vec![value],
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
			values: vec![value],
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
		control.record_pull();
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
