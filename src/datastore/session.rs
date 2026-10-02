//! Persistent GrynnQL session and the shared statement transaction driver.

use std::{num::NonZeroUsize, sync::Arc, time::Instant};

use parking_lot::Mutex;

use super::{
	ClassIndexState,
	Datastore,
	JobId,
	MaintenanceAnalyzeMode,
	MaintenanceJobKind,
	MaintenanceOptions,
	MaintenanceTarget,
	Transaction,
	TransactionAccess,
	TransactionOptions,
};
use crate::{
	gql::{
		ExecutionControl,
		ExecutionMode,
		QueryCursor,
		QueryExecutionOptions,
		QueryResult,
		TransactionCommand,
		closure::LanguageClosureState,
		cursor::CursorRuntime,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Outcome},
	syn::{
		AnalyzeModeName,
		InfoTarget,
		MaintenanceIndexOwner,
		MaintenanceOperationName,
		MaintenanceTargetName,
		Program,
		Statement,
		StatementKind,
	},
	values::{Object, Value},
};

/// Persistent bindings and optional explicit transaction for GrynnQL calls.
pub struct GqlSession {
	store: Datastore,
	bindings: Object,
	transaction: Option<Transaction>,
	execution_mode: ExecutionMode,
	cursors: Option<Arc<Mutex<CursorRuntime>>>,
	closures: Arc<LanguageClosureState>,
}

impl GqlSession {
	pub(crate) fn new(store: Datastore) -> Self {
		let max_snapshot_age = match store.options() {
			super::DatastoreOptions::InMemory(options) => options.max_snapshot_age,
			super::DatastoreOptions::Local(options) => options.max_snapshot_age,
		};
		Self {
			store,
			bindings: Object::new(),
			transaction: None,
			execution_mode: ExecutionMode::BestEffort,
			cursors: Some(Arc::new(Mutex::new(CursorRuntime::new(max_snapshot_age)))),
			closures: Arc::new(LanguageClosureState::default()),
		}
	}

	pub(crate) fn new_transient(store: Datastore) -> Self {
		Self {
			store,
			bindings: Object::new(),
			transaction: None,
			execution_mode: ExecutionMode::BestEffort,
			cursors: None,
			closures: Arc::new(LanguageClosureState::default()),
		}
	}

	/// Returns whether this session currently owns an explicit transaction.
	pub const fn has_active_transaction(&self) -> bool {
		self.transaction.is_some()
	}

	/// Returns closure-registry memory retained by persistent session values.
	#[cfg(test)]
	pub(crate) fn retained_closure_bytes(&self) -> usize {
		self.closures.retained_bytes()
	}

	/// Releases the retained result spool associated with `cursor`.
	///
	/// # Errors
	///
	/// Returns an invalid-cursor status when the token belongs to another
	/// session or has already been closed.
	pub fn close_cursor(&mut self, cursor: &QueryCursor) -> FerrosGrynnResult<()> {
		self.cursors
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_query_cursor("session cursors are disabled"))?
			.lock()
			.close(*cursor)
	}

	/// Parses and executes one call while preserving session and transaction
	/// bindings for later calls.
	///
	/// # Errors
	///
	/// Returns parse, structural-validation, import, persistence, or
	/// publication failures. Statement-local failures are retained in
	/// `QueryResult`.
	pub fn execute(
		&mut self,
		source: &str,
		variables: &Object,
	) -> FerrosGrynnResult<Vec<QueryResult>> {
		self.execute_with_options(source, variables, QueryExecutionOptions::default())
	}

	/// Executes with explicit parser settings and default runtime budgets.
	///
	/// # Errors
	///
	/// Returns the same failures as [`Self::execute`].
	pub fn execute_with_settings(
		&mut self,
		source: &str,
		variables: &Object,
		settings: crate::gql::ParserSettings,
	) -> FerrosGrynnResult<Vec<QueryResult>> {
		self.execute_with_options(source, variables, QueryExecutionOptions {
			parser_settings: settings,
			..QueryExecutionOptions::default()
		})
	}

	/// Executes with explicit parser, timeout, budget, and cancellation
	/// options.
	///
	/// # Errors
	///
	/// Returns parse, structural-validation, import, persistence, or
	/// publication failures. Statement-local failures are retained in
	/// `QueryResult`.
	pub fn execute_with_options(
		&mut self,
		source: &str,
		variables: &Object,
		options: QueryExecutionOptions,
	) -> FerrosGrynnResult<Vec<QueryResult>> {
		self.execute_internal(source, variables, &options, false)
	}

	pub(crate) fn execute_internal(
		&mut self,
		source: &str,
		variables: &Object,
		options: &QueryExecutionOptions,
		require_closed: bool,
	) -> FerrosGrynnResult<Vec<QueryResult>> {
		options.validate()?;
		let transient = self.cursors.is_none();
		let program = match crate::gql::parse(source, options.parser_settings) {
			Ok(program) => program,
			Err(error) => {
				self.rollback_explicit_transaction();
				return Err(error);
			}
		};
		let validation = if transient {
			self.validate_structure(&program, require_closed)
		} else {
			self.ensure_parameter_names_available(variables)
				.and_then(|()| self.validate_structure(&program, require_closed))
		};
		if let Err(error) = validation {
			self.rollback_explicit_transaction();
			return Err(error);
		}
		let mut bindings = self.merged_bindings(variables)?;
		if let [
			statement @ Statement {
				kind: StatementKind::Import(import),
				..
			},
		] = program.statements.as_slice()
		{
			let timeout = crate::gql::resolve_timeout(statement.timeout.as_ref(), &bindings)?;
			let control = ExecutionControl::with_runtime(
				options,
				timeout,
				self.cursors.clone(),
				self.closures.clone(),
			)
			.with_execution_mode(ExecutionMode::BestEffort, &bindings);
			return crate::gql::import::execute_import(
				&self.store,
				source,
				import,
				&bindings,
				&control,
			)
			.map(|result| vec![result]);
		}

		let mut results = Vec::with_capacity(program.statements.len());
		for (index, statement) in program.statements.iter().enumerate() {
			let started = Instant::now();
			if matches!(
				statement.kind,
				StatementKind::Begin(_) | StatementKind::Commit(_) | StatementKind::Cancel
			) {
				let command =
					crate::gql::analyze_transaction_statement(statement)?.ok_or_else(|| {
						FerrosGrynnError::invalid_argument("Transaction statement has no command.")
					})?;
				let result = self.execute_transaction_command(command);
				let failed = result.is_err();
				results.push(QueryResult {
					kind: crate::gql::statement_kind(&statement.kind),
					elapsed: started.elapsed(),
					result: result.map(Outcome::success),
					continuation: None,
				});
				if failed {
					break;
				}
				if index + 1 < program.statements.len() {
					bindings = self.merged_bindings(variables)?;
				}
				continue;
			}
			if let Some(result) = self.execute_admin_statement(statement, &bindings, started) {
				let failed = result.result.is_err();
				results.push(result);
				if failed {
					self.rollback_explicit_transaction();
					break;
				}
				if index + 1 < program.statements.len() {
					bindings = self.merged_bindings(variables)?;
				}
				continue;
			}

			let clause_timeout =
				match crate::gql::resolve_timeout(statement.timeout.as_ref(), &bindings) {
					Ok(value) => value,
					Err(error) => {
						self.rollback_explicit_transaction();
						results.push(error_result(statement, started, error));
						break;
					}
				};
			let control = ExecutionControl::with_runtime(
				options,
				clause_timeout,
				self.cursors.clone(),
				self.closures.clone(),
			);
			let control = if transient {
				control
			} else {
				control
					.with_execution_mode(self.execution_mode, &bindings)
					.with_publication_deferred(self.transaction.is_some())
			};
			let let_name = match &statement.kind {
				StatementKind::Let(statement) if statement.name.value.as_ref() != "_" => {
					Some(statement.name.value.as_ref())
				}
				_ => None,
			};
			if let Some(name) = let_name
				&& self.binding_declared_in_current_scope(name, variables)
			{
				let error = FerrosGrynnError::invalid_argument(format!(
					"Binding `${name}` is already declared in this scope."
				));
				self.rollback_explicit_transaction();
				results.push(error_result(statement, started, error));
				break;
			}

			let mutable_bindings = self.mutable_binding_names();
			let mut result = if self.transaction.is_some() {
				self.execute_in_explicit_transaction(
					statement,
					source,
					&mut bindings,
					&mutable_bindings,
					&control,
					started,
				)
			} else {
				self.execute_autocommit(
					statement,
					source,
					&mut bindings,
					&mutable_bindings,
					&control,
					started,
				)
			};
			if result.result.is_ok()
				&& let StatementKind::DefineIndex(definition) = &statement.kind
				&& definition.concurrently
			{
				match self.enqueue_concurrent_index(&definition.class.value, &definition.name.value)
				{
					Ok(Some(id)) => {
						result.result = result
							.result
							.clone()
							.map(|outcome| outcome.map(|_value| Value::from(id.as_uuid())));
					}
					Ok(None) => {}
					Err(error) => result.result = Err(error),
				}
			}
			if result.result.is_ok()
				&& let StatementKind::DefineGraphIndex(definition) = &statement.kind
				&& definition.concurrently
			{
				let ready =
					self.store.schema_catalog().named_graph(&definition.graph.value).is_some_and(
						|graph| {
							graph.indexes().any(|index| {
								index.name().as_str() == definition.name.value.as_ref()
									&& index.state() == ClassIndexState::Ready
							})
						},
					);
				if !ready {
					let owner = MaintenanceIndexOwner::Graph(definition.graph.clone());
					match self.resolve_index_owner(&owner, &definition.name.value).and_then(
						|target| {
							self.store.enqueue_maintenance_coalesced(
								MaintenanceJobKind::BuildIndex,
								target,
								None,
							)
						},
					) {
						Ok(id) => {
							result.result = result
								.result
								.clone()
								.map(|outcome| outcome.map(|_value| Value::from(id.as_uuid())));
						}
						Err(error) => result.result = Err(error),
					}
				}
			}
			if require_closed
				&& (self.closures.has_entries() || !variables.is_empty())
				&& let Some(closure) = result
					.result
					.as_ref()
					.ok()
					.and_then(|outcome| outcome.value().session_closure_id())
			{
				result.result = Err(FerrosGrynnError::closure_requires_session(closure.get()));
				result.continuation = None;
			}
			let failed = result.result.is_err();
			if !failed {
				if let Some(transaction) = &mut self.transaction {
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
				} else {
					for name in &mutable_bindings {
						if let Some(value) = bindings.get(name).cloned() {
							self.bindings.insert(name.as_ref(), value);
						}
					}
					if let Some(name) = let_name {
						self.bindings
							.insert(name, bindings.get(name).cloned().unwrap_or(Value::None));
					}
				}
			} else {
				self.rollback_explicit_transaction();
			}
			let statement_nonce = control.statement_nonce();
			let result_value = result.result.as_ref().ok().map(Outcome::value);
			let transaction_bindings = self.transaction.as_ref().map(Transaction::gql_bindings);
			if !transient || index + 1 < program.statements.len() {
				self.closures.finish_statement(
					statement_nonce,
					self.bindings
						.values()
						.chain(transaction_bindings.into_iter().flat_map(Object::values))
						.chain(result_value),
				);
			}
			result.elapsed = started.elapsed();
			results.push(result);
			if failed {
				break;
			}
			if index + 1 < program.statements.len() {
				bindings = self.merged_bindings(variables)?;
			}
		}
		Ok(results)
	}

	fn execute_admin_statement(
		&self,
		statement: &Statement,
		bindings: &Object,
		started: Instant,
	) -> Option<QueryResult> {
		let result = match &statement.kind {
			StatementKind::DefineIndex(definition)
				if definition.concurrently && self.transaction.is_some() =>
			{
				Err(FerrosGrynnError::query_feature_not_supported(
					"DEFINE INDEX CONCURRENTLY inside an explicit transaction",
				))
			}
			StatementKind::DefineGraphIndex(definition)
				if definition.concurrently && self.transaction.is_some() =>
			{
				Err(FerrosGrynnError::query_feature_not_supported(
					"DEFINE INDEX CONCURRENTLY inside an explicit transaction",
				))
			}
			StatementKind::Maintenance(maintenance) => {
				if self.transaction.is_some() {
					Err(FerrosGrynnError::query_feature_not_supported(
						"maintenance statement inside an explicit transaction",
					))
				} else {
					self.execute_maintenance(maintenance)
				}
			}
			StatementKind::Kill(kill) => {
				if self.transaction.is_some() {
					Err(FerrosGrynnError::query_feature_not_supported(
						"KILL inside an explicit transaction",
					))
				} else {
					resolve_job_id(bindings, &kill.job)
						.and_then(|id| self.store.kill_job(id))
						.map(|job| job.value())
				}
			}
			StatementKind::Info(info) => match &info.target {
				InfoTarget::Job(job) => resolve_job_id(bindings, job).and_then(|id| {
					self.store.job(id)?.map_or_else(
						|| Err(FerrosGrynnError::maintenance_job_not_found(id.to_string())),
						|job| Ok(job.value()),
					)
				}),
				InfoTarget::Jobs => self.store.jobs().map(|jobs| {
					Value::from(
						jobs.iter().map(super::MaintenanceJobInfo::value).collect::<Vec<_>>(),
					)
				}),
				_ => return None,
			},
			_ => return None,
		};
		Some(QueryResult {
			kind: crate::gql::statement_kind(&statement.kind),
			elapsed: started.elapsed(),
			result: result.map(Outcome::success),
			continuation: None,
		})
	}

	fn execute_maintenance(
		&self,
		statement: &crate::syn::MaintenanceStatement,
	) -> FerrosGrynnResult<Value> {
		let options = self.maintenance_options(statement)?;
		let (kind, target, analyze_mode) = match &statement.operation {
			MaintenanceOperationName::Analyze {
				target,
				mode,
			} => (
				MaintenanceJobKind::Analyze,
				self.resolve_maintenance_target(target)?,
				Some(resolve_analyze_mode(*mode)?),
			),
			MaintenanceOperationName::RebuildIndex {
				if_exists,
				name,
				owner,
			} => {
				let target = match self.resolve_index_owner(owner, &name.value) {
					Ok(target) => target,
					Err(_) if *if_exists => return Ok(Value::None),
					Err(error) => return Err(error),
				};
				(MaintenanceJobKind::RebuildIndex, target, None)
			}
			MaintenanceOperationName::Compact(target) => {
				let target = self.resolve_maintenance_target(target)?;
				let kind = match target {
					MaintenanceTarget::Database => MaintenanceJobKind::CompactDatabase,
					MaintenanceTarget::Graph(_) => MaintenanceJobKind::CompactGraph,
					MaintenanceTarget::Index {
						..
					}
					| MaintenanceTarget::GraphIndex {
						..
					} => MaintenanceJobKind::CompactIndex,
					MaintenanceTarget::BaseClass(_)
					| MaintenanceTarget::EffectiveClass {
						..
					} => {
						return Err(FerrosGrynnError::invalid_argument(
							"COMPACT does not accept class statistics targets",
						));
					}
				};
				(kind, target, None)
			}
		};
		if statement.concurrently {
			self.store
				.enqueue_maintenance_with_analysis(kind, target, Some(options), analyze_mode)
				.map(|id| Value::from(id.as_uuid()))
		} else {
			self.store.run_foreground_maintenance_with_analysis(
				kind,
				target,
				options,
				analyze_mode,
			)?;
			Ok(Value::None)
		}
	}

	fn maintenance_options(
		&self,
		statement: &crate::syn::MaintenanceStatement,
	) -> FerrosGrynnResult<MaintenanceOptions> {
		let configured = self.store.inner.transaction_limits().maintenance;
		let max_memory_bytes = statement
			.sort_memory_bytes
			.map(|value| {
				usize::try_from(value).ok().and_then(NonZeroUsize::new).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"maintenance SORT_MEM is outside this platform domain",
					)
				})
			})
			.transpose()?
			.unwrap_or(configured.max_memory_bytes);
		if max_memory_bytes > configured.max_memory_bytes {
			return Err(FerrosGrynnError::invalid_argument(
				"maintenance SORT_MEM exceeds the configured datastore limit",
			));
		}
		let max_parallelism = statement
			.parallelism
			.map(|value| {
				usize::try_from(value).ok().and_then(NonZeroUsize::new).ok_or_else(|| {
					FerrosGrynnError::invalid_argument(
						"maintenance PARALLELISM is outside this platform domain",
					)
				})
			})
			.transpose()?
			.unwrap_or(NonZeroUsize::MIN);
		let available = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN);
		if max_parallelism > configured.max_parallelism || max_parallelism > available {
			return Err(FerrosGrynnError::invalid_argument(
				"maintenance PARALLELISM exceeds the configured or available limit",
			));
		}
		Ok(MaintenanceOptions {
			max_memory_bytes,
			max_parallelism,
		})
	}

	fn resolve_maintenance_target(
		&self,
		target: &MaintenanceTargetName,
	) -> FerrosGrynnResult<MaintenanceTarget> {
		match target {
			MaintenanceTargetName::Database => Ok(MaintenanceTarget::Database),
			MaintenanceTargetName::ClassBase(class) => {
				let class = self
					.store
					.class(&class.value)
					.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class.value.as_ref()))?;
				Ok(MaintenanceTarget::BaseClass(class.id().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
				})?))
			}
			MaintenanceTargetName::ClassEffective {
				class,
				graph,
			} => {
				let class = self
					.store
					.class(&class.value)
					.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class.value.as_ref()))?;
				let graph =
					self.store.schema_catalog().named_graph(&graph.value).ok_or_else(|| {
						FerrosGrynnError::graph_does_not_exist(graph.value.as_ref())
					})?;
				Ok(MaintenanceTarget::EffectiveClass {
					class: class.id().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
					})?,
					graph: graph.id(),
				})
			}
			MaintenanceTargetName::Graph(name) => self
				.store
				.schema_catalog()
				.named_graph(&name.value)
				.map(|graph| MaintenanceTarget::Graph(graph.id()))
				.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name.value.as_ref())),
			MaintenanceTargetName::Index {
				name,
				owner,
			} => self.resolve_index_owner(owner, &name.value),
		}
	}

	fn resolve_index_owner(
		&self,
		owner: &MaintenanceIndexOwner,
		index_name: &str,
	) -> FerrosGrynnResult<MaintenanceTarget> {
		match owner {
			MaintenanceIndexOwner::Class(class) => {
				self.resolve_index_target(&class.value, index_name)
			}
			MaintenanceIndexOwner::Graph(graph) => {
				let catalog = self.store.schema_catalog();
				let graph = catalog
					.named_graph(&graph.value)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.value.as_ref()))?;
				if index_name == "edge_endpoint"
					&& let Some(index) = graph.edge_endpoint_index()
				{
					return Ok(MaintenanceTarget::GraphIndex {
						graph: graph.id(),
						index: index.id(),
						version: index.version(),
					});
				}
				if let Some(index) =
					graph.indexes().find(|index| index.name().as_str() == index_name)
				{
					return Ok(MaintenanceTarget::GraphIndex {
						graph: graph.id(),
						index: index.id().ok_or_else(|| {
							FerrosGrynnError::invalid_storage_file(
								"published graph index has no identity",
							)
						})?,
						version: index.version(),
					});
				}
				let mut matches = catalog.classes().filter_map(|class| {
					class
						.indexes()
						.find(|index| {
							index.name().as_str() == index_name
								&& matches!(
									index.scope(),
									super::IndexScope::Effective { graph: scope } if scope == graph.id()
								)
						})
						.map(|index| (class, index))
				});
				let Some((class, index)) = matches.next() else {
					return Err(FerrosGrynnError::invalid_argument(format!(
						"index '{}.{}' does not exist",
						graph.name(),
						index_name
					)));
				};
				if matches.next().is_some() {
					return Err(FerrosGrynnError::invalid_argument(format!(
						"index '{}' is ambiguous in graph '{}'",
						index_name,
						graph.name()
					)));
				}
				Ok(MaintenanceTarget::Index {
					class: class.id().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
					})?,
					index: index.id().ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file("published index is missing IndexId")
					})?,
					version: index.version(),
				})
			}
		}
	}

	fn resolve_index_target(
		&self,
		class_name: &str,
		index_name: &str,
	) -> FerrosGrynnResult<MaintenanceTarget> {
		let class = self
			.store
			.class(class_name)
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class_name))?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let index =
			class.indexes().find(|index| index.name().as_str() == index_name).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"index '{class_name}.{index_name}' does not exist"
				))
			})?;
		let index_id = index.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published index is missing IndexId")
		})?;
		Ok(MaintenanceTarget::Index {
			class: class_id,
			index: index_id,
			version: index.version(),
		})
	}

	fn enqueue_concurrent_index(
		&self,
		class_name: &str,
		index_name: &str,
	) -> FerrosGrynnResult<Option<JobId>> {
		let class = self
			.store
			.class(class_name)
			.ok_or_else(|| FerrosGrynnError::class_does_not_exist(class_name))?;
		let index =
			class.indexes().find(|index| index.name().as_str() == index_name).ok_or_else(|| {
				FerrosGrynnError::invalid_argument(format!(
					"index '{class_name}.{index_name}' does not exist"
				))
			})?;
		if index.state() != super::ClassIndexState::Stale {
			return Ok(None);
		}
		let target = self.resolve_index_target(class_name, index_name)?;
		self.store
			.enqueue_maintenance_coalesced(MaintenanceJobKind::BuildIndex, target, None)
			.map(Some)
	}

	fn execute_transaction_command(
		&mut self,
		command: TransactionCommand,
	) -> FerrosGrynnResult<Value> {
		match command {
			TransactionCommand::Begin {
				options,
				execution_mode,
			} => {
				debug_assert!(self.transaction.is_none());
				self.transaction = Some(self.store.begin_with(options));
				self.execution_mode = execution_mode;
				Ok(Value::None)
			}
			TransactionCommand::Commit {
				full_validation,
			} => {
				let transaction =
					self.transaction.take().ok_or_else(FerrosGrynnError::transaction_not_active)?;
				if full_validation {
					transaction.commit_with_validation()?;
				} else {
					transaction.commit()?;
				}
				self.execution_mode = ExecutionMode::BestEffort;
				Ok(Value::None)
			}
			TransactionCommand::Cancel => {
				self.transaction.take().ok_or_else(FerrosGrynnError::transaction_not_active)?;
				self.execution_mode = ExecutionMode::BestEffort;
				Ok(Value::None)
			}
		}
	}

	fn execute_in_explicit_transaction(
		&mut self,
		statement: &Statement,
		source: &str,
		bindings: &mut Object,
		mutable_bindings: &std::collections::BTreeSet<Box<str>>,
		control: &ExecutionControl,
		started: Instant,
	) -> QueryResult {
		let Some(transaction) = self.transaction.as_mut() else {
			return error_result(statement, started, FerrosGrynnError::transaction_not_active());
		};
		match transaction.run_gql_statement(|transaction| {
			crate::gql::execute_parsed_statement_with_bindings(
				transaction,
				source,
				statement,
				bindings,
				mutable_bindings,
				control,
			)
		}) {
			Ok(result) => result,
			Err(error) => error_result(statement, started, error),
		}
	}

	fn execute_autocommit(
		&self,
		statement: &Statement,
		source: &str,
		bindings: &mut Object,
		mutable_bindings: &std::collections::BTreeSet<Box<str>>,
		control: &ExecutionControl,
		started: Instant,
	) -> QueryResult {
		let access = if crate::gql::statement_mutates_state(&statement.kind) {
			TransactionAccess::ReadWrite
		} else {
			TransactionAccess::ReadOnly
		};
		let mut transaction = self.store.begin_with(TransactionOptions::new(
			crate::datastore::IsolationLevel::Serializable,
			access,
		));
		let execution = transaction.run_gql_statement(|transaction| {
			crate::gql::execute_parsed_statement_with_bindings(
				transaction,
				source,
				statement,
				bindings,
				mutable_bindings,
				control,
			)
		});
		let mut result = match execution {
			Ok(result) => result,
			Err(error) => return error_result(statement, started, error),
		};
		if result.result.is_ok() {
			match control.attach_pending_index_seek(transaction) {
				Ok(Some(transaction)) => {
					let commit_started = matches!(
						statement.kind,
						StatementKind::Explain(ref explain)
							if explain.mode == crate::syn::ExplainMode::Analyze
					)
					.then(Instant::now);
					match transaction.commit() {
						Ok(_) => {
							if let Some(started) = commit_started {
								finalize_explain_publication(&mut result, started.elapsed());
							}
						}
						Err(error) => result.result = Err(error),
					}
				}
				Ok(None) => {}
				Err(error) => result.result = Err(error),
			}
		}
		result
	}

	fn validate_structure(&self, program: &Program, require_closed: bool) -> FerrosGrynnResult<()> {
		let needs_structural_walk = program.statements.iter().any(|statement| {
			matches!(
				statement.kind,
				StatementKind::Begin(_)
					| StatementKind::Commit(_)
					| StatementKind::Cancel
					| StatementKind::Import(_)
					| StatementKind::If(_)
					| StatementKind::For(_)
					| StatementKind::While(_)
					| StatementKind::Break
					| StatementKind::Continue
					| StatementKind::Try(_)
			)
		});
		if !needs_structural_walk {
			if require_closed && self.transaction.is_some() {
				return Err(FerrosGrynnError::invalid_argument(
					"Datastore::execute requires COMMIT or CANCEL before the program ends.",
				));
			}
			return Ok(());
		}
		let import_count = program
			.statements
			.iter()
			.filter(|statement| matches!(statement.kind, StatementKind::Import(_)))
			.count();
		if import_count != 0 && (import_count != 1 || program.statements.len() != 1) {
			return Err(FerrosGrynnError::invalid_argument(
				"BEGIN IMPORT must be the only top-level statement in an execute call.",
			));
		}
		let mut active = self.transaction.is_some();
		for statement in &program.statements {
			validate_control_structure(statement, 0, false)?;
			match crate::gql::analyze_transaction_statement(statement)? {
				Some(TransactionCommand::Begin {
					..
				}) if active => {
					return Err(FerrosGrynnError::transaction_already_active());
				}
				Some(TransactionCommand::Begin {
					..
				}) => active = true,
				Some(
					TransactionCommand::Commit {
						..
					}
					| TransactionCommand::Cancel,
				) if !active => {
					return Err(FerrosGrynnError::transaction_not_active());
				}
				Some(
					TransactionCommand::Commit {
						..
					}
					| TransactionCommand::Cancel,
				) => active = false,
				None if matches!(statement.kind, StatementKind::Import(_)) && active => {
					return Err(FerrosGrynnError::query_feature_not_supported(
						"BEGIN IMPORT inside an active transaction",
					));
				}
				None => {}
			}
		}
		if require_closed && active {
			return Err(FerrosGrynnError::invalid_argument(
				"Datastore::execute requires COMMIT or CANCEL before the program ends.",
			));
		}
		Ok(())
	}

	fn ensure_parameter_names_available(&self, variables: &Object) -> FerrosGrynnResult<()> {
		for name in variables.keys() {
			if self.bindings.contains_key(name)
				|| self
					.transaction
					.as_ref()
					.is_some_and(|transaction| transaction.gql_bindings().contains_key(name))
			{
				return Err(FerrosGrynnError::invalid_argument(format!(
					"Execute parameter `${name}` conflicts with a persistent binding."
				)));
			}
		}
		Ok(())
	}

	fn merged_bindings(&mut self, variables: &Object) -> FerrosGrynnResult<Object> {
		let mut bindings = if let Some(transaction) = &mut self.transaction {
			transaction.parameter_bindings()?
		} else {
			let catalog = self.store.schema_catalog();
			let mut parameters = catalog.parameters();
			let Some(first) = parameters.next() else {
				if self.bindings.is_empty() {
					return Ok(variables.clone());
				}
				let mut bindings = variables.clone();
				for (name, value) in &self.bindings {
					bindings.insert(name.clone(), value.clone());
				}
				return Ok(bindings);
			};
			std::iter::once(first)
				.chain(parameters)
				.map(|parameter| (parameter.name().clone(), parameter.value().clone()))
				.collect()
		};
		for (name, value) in variables {
			bindings.insert(name.clone(), value.clone());
		}
		for (name, value) in &self.bindings {
			bindings.insert(name.clone(), value.clone());
		}
		if let Some(transaction) = &self.transaction {
			for (name, value) in transaction.gql_bindings() {
				bindings.insert(name.clone(), value.clone());
			}
		}
		Ok(bindings)
	}

	fn binding_declared_in_current_scope(&self, name: &str, variables: &Object) -> bool {
		variables.contains_key(name)
			|| self.transaction.as_ref().map_or_else(
				|| self.bindings.contains_key(name),
				|transaction| transaction.gql_bindings().contains_key(name),
			)
	}

	fn mutable_binding_names(&self) -> std::collections::BTreeSet<Box<str>> {
		self.transaction.as_ref().map_or_else(
			|| self.bindings.keys().map(|name| Box::<str>::from(name.as_str())).collect(),
			|transaction| {
				transaction
					.gql_bindings()
					.keys()
					.map(|name| Box::<str>::from(name.as_str()))
					.collect()
			},
		)
	}

	fn rollback_explicit_transaction(&mut self) {
		self.transaction.take();
		self.execution_mode = ExecutionMode::BestEffort;
	}
}

fn finalize_explain_publication(result: &mut QueryResult, elapsed: std::time::Duration) {
	let Ok(outcome) = &result.result else {
		return;
	};
	let (mut value, statuses) = outcome.clone().into_parts();
	let elapsed = u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX);
	match &mut value {
		Value::Object(root) => {
			let Some(Value::Object(analyze)) = root.get_mut("analyze") else {
				return;
			};
			if matches!(analyze.get("publication_state"), Some(Value::String(value)) if value.as_str() == "pending")
			{
				analyze.insert("publication_state", "published");
				analyze.insert("commit_elapsed_ns", elapsed);
			}
		}
		Value::String(text) if text.as_str().contains("publication=pending") => {
			value = Value::from(format!(
				"{} commit_ns={elapsed}",
				text.as_str().replace("publication=pending", "publication=published")
			));
		}
		_ => return,
	}
	result.result = Ok(Outcome::new(value, statuses));
}

fn resolve_job_id(bindings: &Object, job: &crate::syn::Identifier) -> FerrosGrynnResult<JobId> {
	match bindings.get(job.value.as_ref()) {
		Some(Value::Uuid(id)) => Ok(JobId::from_uuid(*id)),
		Some(_) => Err(FerrosGrynnError::invalid_argument(format!(
			"Job parameter `${}` must contain a UUID.",
			job.value
		))),
		None => Err(FerrosGrynnError::invalid_argument(format!(
			"Job parameter `${}` is not bound.",
			job.value
		))),
	}
}

fn resolve_analyze_mode(mode: AnalyzeModeName) -> FerrosGrynnResult<MaintenanceAnalyzeMode> {
	const DEFAULT_SAMPLE_COUNT: u32 = 30_000;
	const MIN_SAMPLE_COUNT: u64 = 1_024;
	const MAX_SAMPLE_COUNT: u64 = 1_000_000;

	match mode {
		AnalyzeModeName::Automatic => Ok(MaintenanceAnalyzeMode::Automatic),
		AnalyzeModeName::Full => Ok(MaintenanceAnalyzeMode::Full),
		AnalyzeModeName::Approximate {
			sample_count,
		} => {
			let sample_count = sample_count.unwrap_or(u64::from(DEFAULT_SAMPLE_COUNT));
			if !(MIN_SAMPLE_COUNT..=MAX_SAMPLE_COUNT).contains(&sample_count) {
				return Err(FerrosGrynnError::invalid_argument(
					"ANALYZE SAMPLE must be in the range 1024..=1000000",
				));
			}
			Ok(MaintenanceAnalyzeMode::Approximate {
				sample_count: u32::try_from(sample_count).map_err(|_error| {
					FerrosGrynnError::invalid_argument("ANALYZE SAMPLE exceeds u32")
				})?,
			})
		}
	}
}

pub(crate) fn validate_control_structure(
	statement: &Statement,
	loop_depth: u32,
	nested: bool,
) -> FerrosGrynnResult<()> {
	if nested
		&& matches!(
			statement.kind,
			StatementKind::Begin(_)
				| StatementKind::Commit(_)
				| StatementKind::Cancel
				| StatementKind::Import(_)
		) {
		return Err(FerrosGrynnError::invalid_argument(
			"Transaction and import statements are only allowed at program top level.",
		));
	}
	match &statement.kind {
		StatementKind::Break | StatementKind::Continue if loop_depth == 0 => {
			Err(FerrosGrynnError::invalid_argument("BREAK and CONTINUE require an enclosing loop."))
		}
		StatementKind::If(statement) => {
			for (_condition, block) in &statement.branches {
				validate_block_structure(block, loop_depth)?;
			}
			if let Some(block) = &statement.else_block {
				validate_block_structure(block, loop_depth)?;
			}
			Ok(())
		}
		StatementKind::For(statement) => {
			validate_block_structure(&statement.body, loop_depth.saturating_add(1))
		}
		StatementKind::While(statement) => {
			validate_block_structure(&statement.body, loop_depth.saturating_add(1))
		}
		StatementKind::Try(statement) => {
			validate_block_structure(&statement.body, loop_depth)?;
			if let Some(catch) = &statement.catch {
				validate_block_structure(&catch.body, loop_depth)?;
			}
			Ok(())
		}
		_ => Ok(()),
	}
}

fn validate_block_structure(block: &crate::syn::Block, loop_depth: u32) -> FerrosGrynnResult<()> {
	for statement in &block.statements {
		validate_control_structure(statement, loop_depth, true)?;
	}
	Ok(())
}

fn error_result(statement: &Statement, started: Instant, error: FerrosGrynnError) -> QueryResult {
	QueryResult {
		kind: crate::gql::statement_kind(&statement.kind),
		elapsed: started.elapsed(),
		result: Err(error),
		continuation: None,
	}
}
