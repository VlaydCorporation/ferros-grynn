#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use std::collections::{BTreeMap, BTreeSet};

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
	let closures = std::sync::Arc::new(crate::gql::closure::LanguageClosureState::default());
	for statement in &program.statements {
		let started = std::time::Instant::now();
		let let_name = match &statement.kind {
			crate::syn::StatementKind::LiveLet(statement)
				if statement.name.value.as_ref() != "_" =>
			{
				Some(statement.name.value.as_ref())
			}
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
		let mut control = crate::gql::options::ExecutionControl::with_runtime(
			&options,
			None,
			None,
			closures.clone(),
		)
		.with_publication_deferred(true);
		let mutable_bindings = transaction
			.gql_bindings()
			.keys()
			.map(|name| Box::<str>::from(name.as_str()))
			.collect::<BTreeSet<_>>();
		let execution = transaction.run_gql_statement(|transaction| {
			execute_parsed_statement_with_bindings(
				transaction,
				source,
				statement,
				&mut bindings,
				&mutable_bindings,
				&mut control,
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
		if !failed {
			let mut transaction_bindings = transaction.gql_bindings().clone();
			for name in &mutable_bindings {
				if let Some(value) = bindings.get(name).cloned() {
					transaction_bindings.insert(name.as_ref(), value);
				}
			}
			if let Some(name) = let_name {
				transaction_bindings
					.insert(name, bindings.get(name).cloned().unwrap_or(Value::None));
			}
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
	function_cache: Option<&crate::gql::closure::LanguageClosureState>,
) -> FerrosGrynnResult<(LogicalProgram, u64)> {
	let snapshot = transaction.query_snapshot()?;
	let read = TransactionRead {
		transaction,
		snapshot,
		metadata_only: false,
	};
	let logical = match function_cache {
		None => analyze(program, source, &read, variables)?,
		Some(cache) => {
			crate::gql::analysis::analyze_with_runtime(program, source, &read, variables, cache)?
		}
	};
	Ok((logical, read.snapshot.commit_ts()))
}

/// Analyzes and freezes item deadlines against the same preflight root.
/// The caller retains admission for both timeout vectors through import END.
pub(in crate::gql) fn analyze_import_preflight(
	transaction: &mut Transaction,
	source: &str,
	program: &Program,
	variables: &Object,
	control: &ExecutionControl,
) -> FerrosGrynnResult<(LogicalProgram, Vec<Option<Duration>>, u64)> {
	let snapshot = transaction.query_snapshot()?;
	let read = TransactionRead {
		transaction,
		snapshot,
		metadata_only: false,
	};
	let (logical, typed_timeouts) = crate::gql::analysis::analyze_import_with_runtime(
		program, source, &read, variables, control,
	)?;
	let fields = cursor::RecordFields {
		read: &read,
		snapshot: read.snapshot()?,
		graph: None,
	};
	let mut timeouts = vec![None; program.statements.len()];
	for (index, timeout) in typed_timeouts {
		let duration = eval::eval_duration_with_lookup(
			&timeout,
			|slot| {
				logical
					.initial_slots
					.iter()
					.find(|(_, id, _)| *id == slot)
					.and_then(|(name, _, _)| variables.get(name.as_ref()))
			},
			source,
			Some(&fields),
			control,
			"TIMEOUT",
		)?;
		// Validate platform range before obtaining the writer lease. Zero
		// remains an item-local deadline, handled by the ordinary
		// execution checks.
		control.checked_child(Some(duration))?;
		let item = timeouts.get_mut(index).ok_or_else(|| {
			FerrosGrynnError::invalid_argument("Import timeout ordinal is invalid.")
		})?;
		*item = Some(duration);
	}
	control.check()?;
	Ok((logical, timeouts, read.snapshot.commit_ts()))
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
		metadata_only: false,
	};
	execute_program(&mut read, source, program, bindings, control)
}

pub(crate) fn execute_parsed_statement_with_bindings(
	transaction: &mut Transaction,
	source: &str,
	statement: &crate::syn::Statement,
	bindings: &mut Object,
	mutable_bindings: &BTreeSet<Box<str>>,
	control: &mut crate::gql::options::ExecutionControl,
) -> FerrosGrynnResult<QueryResult> {
	let metadata_only = matches!(
		statement.kind,
		crate::syn::StatementKind::Info(crate::syn::InfoStatement {
			target: crate::syn::InfoTarget::Live(_) | crate::syn::InfoTarget::LiveLet(_),
		}) | crate::syn::StatementKind::Kill(_)
			| crate::syn::StatementKind::AckLive(_)
	);
	let mut metadata_account = metadata_only.then(|| control.barrier_account());
	let snapshot = transaction.query_snapshot_admitted(metadata_only, |bytes| {
		if metadata_only {
			control.check()?;
			let bytes = bytes.checked_mul(2).ok_or_else(|| {
				FerrosGrynnError::operation_overflow("LIVE metadata snapshot overlap")
			})?;
			if !metadata_account.as_mut().is_some_and(|account| account.reserve(bytes)) {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"LIVE metadata snapshot",
					control.barrier_memory_limit() as u64,
				));
			}
		}
		Ok(())
	})?;
	let mut read = TransactionRead {
		transaction,
		snapshot,
		metadata_only,
	};
	execute_parsed_statement(&mut read, source, statement, bindings, mutable_bindings, control)
}

pub(super) struct TransactionRead<'a> {
	pub(super) transaction: &'a mut Transaction,
	pub(super) snapshot: QuerySnapshot,
	/// Catalog-only commands neither bind physical indexes nor pull records.
	pub(super) metadata_only: bool,
}

impl TransactionRead<'_> {
	fn refresh_snapshot(&mut self) -> FerrosGrynnResult<()> {
		self.snapshot =
			self.transaction.query_snapshot_admitted(self.metadata_only, |_bytes| Ok(()))?;
		Ok(())
	}
}

impl QueryCatalog for TransactionRead<'_> {
	fn lexical_binding_type(&self, name: &str) -> Option<&Type> {
		self.transaction.gql_binding_type(name)
	}

	fn lexical_binding_is_reactive(&self, name: &str) -> bool {
		self.transaction.gql_binding_is_reactive(name)
	}

	fn schema_epoch(&self) -> u64 {
		self.snapshot.commit_ts()
	}

	fn schema(&self, class: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_class(&self.snapshot, class).map(|schema| query_schema(&schema))
	}

	fn class_definition(&self, class: &str) -> FerrosGrynnResult<Arc<ClassSchema>> {
		self.transaction.query_class(&self.snapshot, class)
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

	fn graph_if_exists(&self, name: &str) -> FerrosGrynnResult<Option<GraphRef>> {
		self.transaction
			.query_named_graph_schema(&self.snapshot, name)
			.map(|schema| schema.map(|schema| schema.reference().clone()))
	}

	fn functions(
		&self,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<crate::datastore::SchemaFunction>>> {
		self.transaction.query_schema_functions(&self.snapshot, namespace, name)
	}

	fn function(
		&self,
		id: crate::datastore::FunctionId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaFunction>>> {
		self.transaction.query_schema_function(&self.snapshot, id)
	}

	fn analyzer(
		&self,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>> {
		self.transaction.query_schema_analyzer(&self.snapshot, name)
	}

	fn analyzer_by_id(
		&self,
		id: crate::datastore::AnalyzerId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>> {
		self.transaction.query_schema_analyzer_by_id(&self.snapshot, id)
	}
}

impl QueryRead for TransactionRead<'_> {
	type Graph<'a>
		= TransactionGraphView<'a>
	where
		Self: 'a;
	type Savepoint = crate::datastore::TransactionSavepoint;

	fn live_info(
		&self,
		snapshot: Snapshot,
		id: crate::live::LiveId,
		maximum: usize,
	) -> FerrosGrynnResult<crate::live::LiveInfo> {
		if snapshot.0 != self.snapshot.commit_ts() {
			return Err(FerrosGrynnError::invalid_argument("LIVE metadata snapshot mismatch"));
		}
		self.transaction.query_live_info(&self.snapshot, id, maximum)
	}

	fn live_binding_info(
		&self,
		snapshot: Snapshot,
		id: crate::live::LiveBindingId,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<(crate::live::LiveBindingInfo, crate::gql::options::BarrierAccount)>
	{
		if snapshot.0 != self.snapshot.commit_ts() {
			return Err(FerrosGrynnError::invalid_argument("LIVE LET metadata snapshot mismatch"));
		}
		self.transaction.query_live_binding_info(&self.snapshot, id, control)
	}

	fn live_binding_value(
		&self,
		snapshot: Snapshot,
		id: crate::live::LiveBindingId,
	) -> FerrosGrynnResult<&Value> {
		if snapshot.0 != self.snapshot.commit_ts() {
			return Err(FerrosGrynnError::invalid_argument("LIVE LET read snapshot mismatch"));
		}
		self.transaction.query_live_binding_value(&self.snapshot, id)
	}

	fn endpoint_tensor(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		options: crate::graph::tensor::TensorOptions,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<crate::graph::tensor::EndpointTensor> {
		self.transaction.endpoint_tensor_at(&self.snapshot, graph, options, &mut || check())
	}

	fn endpoint_tensor_selected(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		options: crate::graph::tensor::TensorOptions,
		selected: Option<&crate::datastore::QueryTensorCache>,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<(
		crate::graph::tensor::EndpointTensor,
		Option<crate::datastore::QueryTensorCache>,
	)> {
		self.transaction.endpoint_tensor_selected_at(
			&self.snapshot,
			graph,
			options,
			selected,
			&mut || check(),
		)
	}

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(self.snapshot.commit_ts()))
	}

	fn prepare_mutating_statement(&mut self) -> FerrosGrynnResult<()> {
		self.transaction.infer_gql_write_access()
	}

	fn tensor_cost_evaluator(
		&self,
		_snapshot: Snapshot,
		bytes: usize,
	) -> FerrosGrynnResult<crate::gql::PureFunctionEvaluator<'_>> {
		self.transaction.tensor_cost_evaluator_at(&self.snapshot, bytes)
	}

	fn changefeed_binding(
		&self,
		selector: &crate::changefeed::ChangefeedSelector,
	) -> FerrosGrynnResult<Option<crate::changefeed::ChangefeedReadBinding>> {
		self.transaction.query_changefeed_binding(selector)
	}

	fn changefeed_context(
		&self,
	) -> FerrosGrynnResult<(crate::values::Uuid, crate::changefeed::Versionstamp)> {
		self.transaction.query_changefeed_context()
	}

	fn visit_changefeed_functions(
		&self,
		visitor: &mut dyn FnMut(&crate::datastore::SchemaFunction) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		self.transaction.query_visit_schema_functions(&self.snapshot, visitor)
	}

	fn observe_tensor_cost(&self, key: crate::datastore::ConflictKey) {
		self.transaction.observe_tensor_cost(key);
	}

	fn refresh_snapshot(&mut self) -> FerrosGrynnResult<Snapshot> {
		TransactionRead::refresh_snapshot(self)?;
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
		let graph_ids = if demand.all_graphs {
			graph_schemas.iter().map(crate::datastore::GraphSchema::id).collect()
		} else {
			demand.graphs.clone()
		};
		let statistics_demand = crate::datastore::QueryStatisticsDemand {
			base_classes: !demand.classes.is_empty(),
			base_properties: demand
				.properties
				.iter()
				.any(|property| property.scope == StatisticsScope::Base),
			graph_classes: if demand.classes.is_empty() {
				std::collections::BTreeSet::new()
			} else {
				graph_ids.clone()
			},
			graph_labels: graph_ids.clone(),
			graph_degrees: demand.degrees.keys().copied().collect(),
			graph_properties: demand
				.properties
				.iter()
				.filter_map(|property| match property.scope {
					StatisticsScope::Base => None,
					StatisticsScope::Effective {
						graph,
					} => Some(graph),
				})
				.collect(),
		};
		let statistics = self.transaction.query_statistics(&self.snapshot, &statistics_demand)?;
		let base = statistics.base();
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
				neighbourhood_sketches: if demand.neighbourhood_sketches {
					self.transaction
						.query_neighbourhood_sketches(&self.snapshot, graph.reference())?
				} else {
					Vec::new()
				},
				landmark_paths: if demand.landmark_paths {
					self.transaction.query_landmark_paths(&self.snapshot, graph.reference())?
				} else {
					Vec::new()
				},
				tensor_caches: if demand.tensor_caches {
					self.transaction.query_tensor_caches(&self.snapshot, graph.reference())?
				} else {
					Vec::new()
				},
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

	fn base_atom(
		&self,
		_snapshot: Snapshot,
		record: &RecordId,
	) -> FerrosGrynnResult<Option<(AtomKind, PropertyMap)>> {
		Ok(self
			.transaction
			.query_point(&self.snapshot, record, None)?
			.map(|row| (row.kind, row.base_properties)))
	}

	fn count_index(
		&self,
		_snapshot: Snapshot,
		index: crate::datastore::IndexId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64> {
		self.transaction.query_count_index(&self.snapshot, index, graph)
	}

	fn fulltext_index(
		&self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		terms: &std::collections::BTreeSet<Box<str>>,
	) -> FerrosGrynnResult<Option<PhysicalFulltextBatch>> {
		let class = self.transaction.query_class(&self.snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let scope = graph.map_or(crate::datastore::IndexScope::Base, |graph| {
			crate::datastore::IndexScope::Effective {
				graph: graph.id(),
			}
		});
		self.transaction.query_fulltext_index(&self.snapshot, index, class_id, scope, terms).map(
			|batch| {
				batch.map(|batch| PhysicalFulltextBatch {
					document_count: batch.document_count,
					total_document_length: batch.total_document_length,
					documents: batch
						.documents
						.into_iter()
						.map(|document| PhysicalFulltextDocument {
							record_id: document.record_id,
							document_length: document.document_length,
							terms: document
								.terms
								.into_iter()
								.map(|(term, value)| {
									(term, PhysicalFulltextTerm {
										document_frequency: value.document_frequency,
										term_frequency: value.term_frequency,
										spans: value.spans,
									})
								})
								.collect(),
						})
						.collect(),
				})
			},
		)
	}

	fn geometry_index(
		&self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		query: &crate::values::Geometry,
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Option<crate::datastore::QueryGeometryIndexRead>> {
		let class = self.transaction.query_class(&self.snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class has no identity")
		})?;
		let scope = graph.map_or(crate::datastore::IndexScope::Base, |graph| {
			crate::datastore::IndexScope::Effective {
				graph: graph.id(),
			}
		});
		self.transaction.query_geometry_index(&self.snapshot, index, class_id, scope, query, admit)
	}

	fn path_lower_bound(
		&self,
		_snapshot: Snapshot,
		binding: &crate::datastore::QueryLandmarkPath,
		source: AtomId,
		target: AtomId,
	) -> FerrosGrynnResult<Option<Number>> {
		self.transaction.query_path_lower_bound(&self.snapshot, binding, source, target)
	}

	fn k_hop_estimate(
		&self,
		_snapshot: Snapshot,
		occurrence: &AtomOccurrenceRef,
		mode: crate::graph::TraversalSemantics,
		radius: u32,
		index: Option<crate::datastore::IndexId>,
		scratch_bytes: usize,
		max_work: u64,
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<crate::datastore::QueryNeighbourhoodEstimate> {
		self.transaction.query_k_hop_estimate(
			&self.snapshot,
			occurrence,
			mode,
			radius,
			index,
			scratch_bytes,
			max_work,
			admit,
			check,
		)
	}

	fn vector_index(
		&self,
		_snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		query: &crate::values::Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<PhysicalVectorHit>>> {
		let class = self.transaction.query_class(&self.snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let scope = graph.map_or(crate::datastore::IndexScope::Base, |graph| {
			crate::datastore::IndexScope::Effective {
				graph: graph.id(),
			}
		});
		self.transaction
			.query_vector_index(&self.snapshot, index, class_id, scope, query, k, breadth)
			.map(|hits| {
				hits.map(|hits| {
					hits.into_iter()
						.map(|hit| PhysicalVectorHit {
							record_id: hit.record_id,
							distance_bits: hit.distance.to_bits(),
						})
						.collect()
				})
			})
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

	fn physical_traversal_candidates(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		source: AtomId,
		directions: &[crate::syn::MatchDirection],
	) -> FerrosGrynnResult<Option<PhysicalTraversalBatch>> {
		let forward = directions.contains(&crate::syn::MatchDirection::Forward);
		let backward = directions.contains(&crate::syn::MatchDirection::Backward);
		let undirected = directions.contains(&crate::syn::MatchDirection::Undirected);
		self.transaction
			.query_physical_traversal(&self.snapshot, graph, source, forward, backward, undirected)
			.map(|read| {
				read.map(|read| PhysicalTraversalBatch {
					candidates: read
						.candidates
						.into_iter()
						.map(|candidate| ScopedTraversalCandidate {
							target: BoundAtom::new(graph.id(), candidate.target),
							edge: BoundAtom::new(graph.id(), candidate.edge),
							direction: candidate.direction,
							role: candidate.role,
							topology: candidate.topology,
							cross_level: false,
						})
						.collect(),
					resident_pages: read.stats.resident_pages,
					wal_pages: read.stats.wal_pages,
					sync_home_pages: read.stats.sync_home_pages,
					ioring_home_pages: read.stats.ioring_home_pages,
				})
			})
	}

	fn physical_inner_graph(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalInnerGraphBatch>> {
		self.transaction.query_physical_inner_graph(&self.snapshot, graph, owner).map(|read| {
			read.map(|read| PhysicalInnerGraphBatch {
				graph: read.graph,
				resident_pages: read.stats.resident_pages,
				wal_pages: read.stats.wal_pages,
				sync_home_pages: read.stats.sync_home_pages,
				ioring_home_pages: read.stats.ioring_home_pages,
			})
		})
	}

	fn physical_occurrence_fields(
		&self,
		_snapshot: Snapshot,
		graph: &GraphRef,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalOccurrenceFieldsBatch>> {
		self.transaction.query_physical_fields(&self.snapshot, graph, owner).map(|read| {
			read.map(|read| PhysicalOccurrenceFieldsBatch {
				record_id: read.record_id,
				properties: read.properties,
				resident_pages: read.stats.resident_pages,
				wal_pages: read.stats.wal_pages,
				sync_home_pages: read.stats.sync_home_pages,
				ioring_home_pages: read.stats.ioring_home_pages,
			})
		})
	}

	fn record_field(
		&self,
		_snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
		control: Option<&ExecutionControl>,
	) -> FerrosGrynnResult<Value> {
		let control = control.filter(|control| control.has_reactive_copies());
		self.transaction.query_record_field(
			&self.snapshot,
			record,
			graph,
			field,
			|bytes| {
				control.map_or(Ok(()), |control| {
					control.admit_reactive_work(1)?;
					control.admit_reactive_bytes(bytes)
				})
			},
			|value| {
				control
					.map_or_else(|| Ok(value.clone()), |control| control.copy_reactive_value(value))
			},
		)
	}

	fn graph(
		&self,
		_snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'_>> {
		self.transaction.query_graph(&self.snapshot, reference)
	}

	fn graph_reference(
		&self,
		_snapshot: Snapshot,
		graph: crate::graph::GraphId,
	) -> FerrosGrynnResult<GraphRef> {
		self.transaction
			.query_schema_all_graphs(&self.snapshot)?
			.into_iter()
			.find(|schema| schema.id() == graph)
			.map(|schema| schema.reference().clone())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))
	}

	fn graph_references(&self, _snapshot: Snapshot) -> FerrosGrynnResult<Vec<GraphRef>> {
		self.transaction
			.query_schema_all_graphs(&self.snapshot)
			.map(|graphs| graphs.into_iter().map(|graph| graph.reference().clone()).collect())
	}

	fn named_graph_contract(
		&self,
		_snapshot: Snapshot,
		name: &str,
	) -> FerrosGrynnResult<Option<(GraphRef, crate::graph::GraphTopologyConstraints)>> {
		self.transaction.query_named_graph_schema(&self.snapshot, name).map(|schema| {
			schema.map(|schema| (schema.reference().clone(), schema.topology_constraints()))
		})
	}

	fn level_path(
		&self,
		_snapshot: Snapshot,
		occurrence: &crate::graph::AtomOccurrenceRef,
	) -> FerrosGrynnResult<Value> {
		self.transaction.query_level_path(&self.snapshot, occurrence)
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

	fn note_binding_type(
		&mut self,
		name: &str,
		ty: &Type,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		self.transaction.note_gql_binding_type(name, ty, control)
	}

	fn note_reactive_binding(
		&mut self,
		name: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<()> {
		self.transaction.note_gql_reactive_binding(name, control)
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
		let value = if statement.is_read() {
			execute_schema(self.transaction, &snapshot, statement, slots, source, control)?
		} else if matches!(
			statement,
			LogicalSchemaStatement::CloseLive { .. } | LogicalSchemaStatement::AckLive { .. }
		) {
			self.transaction.atomic_live_control(|transaction| {
				execute_schema(transaction, &snapshot, statement, slots, source, control)
			})?
		} else {
			self.transaction.atomic_mutation(|transaction| {
				execute_schema(transaction, &snapshot, statement, slots, source, control)
			})?
		};
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

	fn install_built_graph(
		&mut self,
		name: &str,
		mode: crate::syn::DefinitionMode,
		contract_explicit: bool,
		graph: crate::graph::MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		let reference = self.transaction.atomic_mutation(|transaction| {
			transaction.install_built_graph(name, mode, contract_explicit, &graph)
		})?;
		self.refresh_snapshot()?;
		Ok(reference)
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

/// Universal atom columns also type-check an empty graph occurrence domain.
pub(in crate::gql) fn atom_source_schema(additional: usize) -> Schema {
	let mut columns = Vec::with_capacity(additional + 5);
	columns.extend([
		Column {
			name: "id".into(),
			qualifier: None,
			ty: Type::Record,
		},
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
	Schema {
		columns,
		origins: BTreeMap::new(),
	}
}

pub(in crate::gql) fn query_schema(schema: &ClassSchema) -> Schema {
	let mut columns = atom_source_schema(schema.fields().len()).columns;
	columns.extend(schema.fields().map(|field| Column {
		name: field.name().as_str().into(),
		qualifier: None,
		ty: field.value_type().clone(),
	}));
	let origins = schema
		.fields()
		.enumerate()
		.map(|(offset, field)| {
			(offset + 5, crate::gql::ir::ColumnOrigin {
				source: schema.name().as_str().into(),
				class: schema.name().as_str().into(),
				field: field.name().as_str().into(),
				scope: Some(crate::datastore::IndexScope::Base),
			})
		})
		.collect();
	Schema {
		columns,
		origins,
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

pub(super) fn storage_row(
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
