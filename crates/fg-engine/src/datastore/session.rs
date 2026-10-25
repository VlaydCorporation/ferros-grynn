//! Persistent GrynnQL session and the shared statement transaction driver.

use std::{num::NonZeroUsize, sync::Arc, time::Instant};

use fg_runtime::__internal::ValueInternal as _;
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
	/// Store associated with this gql session.
	store: Datastore,
	/// Bindings associated with this gql session.
	bindings: Object,
	/// Owned lexical contracts, independent of the current value
	/// representation.
	binding_types: crate::gql::bindings::BindingTypes,
	/// Transaction associated with this gql session.
	transaction: Option<Transaction>,
	/// Execution mode associated with this gql session.
	execution_mode: ExecutionMode,
	/// Cursors associated with this gql session.
	cursors: Option<Arc<Mutex<CursorRuntime>>>,
	/// Closures associated with this gql session.
	closures: Arc<LanguageClosureState>,
	/// Owned admitted consumers detach when the session or extracted handle
	/// drops.
	live_subscriptions: Vec<crate::live::LiveSubscription>,
}

impl GqlSession {
	/// Creates a new `GqlSession` value.
	pub(crate) fn new(store: Datastore) -> Self {
		let max_snapshot_age = match store.options() {
			super::DatastoreOptions::InMemory(options) => options.max_snapshot_age,
			super::DatastoreOptions::Local(options) => options.max_snapshot_age,
		};
		Self {
			store,
			bindings: Object::new(),
			binding_types: crate::gql::bindings::BindingTypes::default(),
			transaction: None,
			execution_mode: ExecutionMode::BestEffort,
			cursors: Some(Arc::new(Mutex::new(CursorRuntime::new(max_snapshot_age)))),
			closures: Arc::new(LanguageClosureState::default()),
			live_subscriptions: Vec::new(),
		}
	}

	/// Returns the new transient result.
	pub(crate) fn new_transient(store: Datastore) -> Self {
		Self {
			store,
			bindings: Object::new(),
			binding_types: crate::gql::bindings::BindingTypes::default(),
			transaction: None,
			execution_mode: ExecutionMode::BestEffort,
			cursors: None,
			closures: Arc::new(LanguageClosureState::default()),
			live_subscriptions: Vec::new(),
		}
	}

	/// Returns whether this session currently owns an explicit transaction.
	pub const fn has_active_transaction(&self) -> bool {
		self.transaction.is_some()
	}

	/// Extracts the owned consumer of a successfully committed LIVE SELECT.
	/// The handle retains no transaction snapshot and survives this session.
	///
	/// # Errors
	///
	/// Returns an invalid-argument status for an unknown or already extracted
	/// ID.
	pub fn take_live_subscription(
		&mut self,
		id: crate::live::LiveId,
	) -> FerrosGrynnResult<crate::live::LiveSubscription> {
		let position =
			self.live_subscriptions.iter().position(|handle| handle.id() == id).ok_or_else(
				|| FerrosGrynnError::invalid_argument("LIVE consumer is absent from this session"),
			)?;
		Ok(self.live_subscriptions.swap_remove(position))
	}

	/// Polls a retained LIVE consumer without transferring ACK authority.
	///
	/// # Errors
	///
	/// Returns a missing attachment or the same checked error as
	/// [`crate::live::LiveSubscription::next`]. Extracted handles must be
	/// polled and acknowledged through their Rust API instead.
	pub fn next_live(
		&mut self,
		id: crate::live::LiveId,
	) -> FerrosGrynnResult<crate::live::LiveNext> {
		let handle =
			self.live_subscriptions.iter_mut().find(|handle| handle.id() == id).ok_or_else(
				|| FerrosGrynnError::invalid_argument("LIVE consumer is absent from this session"),
			)?;
		handle.next()
	}

	/// Admits immutable authority before attaching it to a statement runtime.
	fn attach_live_authority(
		&self,
		control: ExecutionControl,
	) -> FerrosGrynnResult<ExecutionControl> {
		if self.live_subscriptions.is_empty() {
			return Ok(control);
		}
		let bytes = self
			.live_subscriptions
			.len()
			.checked_mul(std::mem::size_of::<super::GqlLiveAttachment>() * 2)
			.and_then(|bytes| bytes.checked_add(256))
			.ok_or_else(|| FerrosGrynnError::operation_overflow("session LIVE authority"))?;
		let mut account = control.barrier_account();
		if !account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"session LIVE authority",
				control.barrier_memory_limit() as u64,
			));
		}
		let mut attachments = Vec::new();
		attachments.try_reserve_exact(self.live_subscriptions.len()).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"session LIVE authority",
				bytes as u64,
			)
			.with_technical_source(error)
		})?;
		for handle in &self.live_subscriptions {
			attachments.push(handle.inner.gql_attachment());
		}
		Ok(control.with_live_attachments(attachments, account))
	}

	/// Checks current ownership and prepares fixed cursor updates before WAL.
	fn prepare_live_acks(
		&self,
		tx: &Transaction,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<(Vec<super::GqlLiveAck>, Option<crate::gql::BarrierAccount>)> {
		if tx.gql_live_acks().is_empty() {
			return Ok((Vec::new(), None));
		}
		for ack in tx.gql_live_acks() {
			control.check()?;
			if !self
				.live_subscriptions
				.iter()
				.any(|handle| handle.inner.gql_attachment() == ack.attachment)
			{
				return Err(FerrosGrynnError::invalid_argument(
					"GQL ACK attachment was transferred before COMMIT",
				));
			}
		}
		let bytes = tx
			.gql_live_acks()
			.len()
			.checked_mul(std::mem::size_of::<super::GqlLiveAck>())
			.ok_or_else(|| FerrosGrynnError::operation_overflow("GQL ACK cursor updates"))?;
		let mut account = control.barrier_account();
		if !account.reserve(bytes) {
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"GQL ACK cursor updates",
				control.barrier_memory_limit() as u64,
			));
		}
		Ok((tx.gql_live_acks().to_vec(), Some(account)))
	}

	/// Publication already confirmed queues; only owned fixed metadata moves.
	fn publish_live_acks(&mut self, acks: &[super::GqlLiveAck]) {
		for ack in acks {
			for handle in &mut self.live_subscriptions {
				handle.inner.apply_gql_ack(ack);
			}
		}
	}

	/// Pre-admits all surviving GQL registrations and retained handle capacity.
	fn prepare_live_subscriptions(
		&mut self,
		tx: &mut Transaction,
	) -> FerrosGrynnResult<Vec<crate::live::LiveSubscription>> {
		let count = tx.gql_live_registrations().len();
		if count == 0 {
			return Ok(Vec::new());
		}
		let total = self
			.live_subscriptions
			.len()
			.checked_add(count)
			.ok_or_else(|| FerrosGrynnError::operation_overflow("session LIVE handles"))?;
		if total
			.checked_mul(std::mem::size_of::<crate::live::LiveSubscription>() * 2)
			.is_none_or(|bytes| bytes > super::live_queue::SESSION_BYTES)
		{
			return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
				"session LIVE handles",
				super::live_queue::SESSION_BYTES as u64,
			));
		}
		self.live_subscriptions.try_reserve(count).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"session LIVE handles",
				total as u64,
			)
			.with_technical_source(error)
		})?;
		let mut pending = Vec::new();
		pending.try_reserve_exact(count).map_err(|error| {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"pending LIVE handles",
				count as u64,
			)
			.with_technical_source(error)
		})?;
		for index in 0..count {
			let id = tx.gql_live_registrations()[index];
			if tx.live_info(id)?.state() != crate::live::LiveState::Closed {
				pending.push(self.store.prepare_live_attachment(tx, id)?);
			}
		}
		Ok(pending)
	}

	/// Finishes only prepared consumers; vector capacity was reserved before
	/// WAL.
	fn publish_live_subscriptions(
		&mut self,
		pending: Vec<crate::live::LiveSubscription>,
	) -> FerrosGrynnResult<()> {
		for mut subscription in pending {
			subscription.inner.finish_registration()?;
			self.live_subscriptions.push(subscription);
		}
		Ok(())
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

	/// Executes internal.
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
			Statement {
				kind: StatementKind::Import(import),
				..
			},
		] = program.statements.as_slice()
		{
			let control = ExecutionControl::with_runtime(
				options,
				None,
				self.cursors.clone(),
				self.closures.clone(),
			)
			.with_execution_mode(ExecutionMode::BestEffort, &bindings);
			return crate::gql::import::execute_import(
				&self.store,
				source,
				import,
				&bindings,
				&self.binding_types,
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
				let control = ExecutionControl::with_runtime(
					options,
					None,
					self.cursors.clone(),
					self.closures.clone(),
				);
				let result = self.execute_transaction_command(command, &control);
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

			let control = ExecutionControl::with_session_runtime(
				options,
				statement.timeout.is_some(),
				started,
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
			let mut control = match self.attach_live_authority(control) {
				Ok(control) => control,
				Err(error) => {
					self.rollback_explicit_transaction();
					results.push(error_result(statement, started, error));
					break;
				}
			};
			let let_name = match &statement.kind {
				StatementKind::LiveLet(statement) if statement.name.value.as_ref() != "_" => {
					Some(statement.name.value.as_ref())
				}
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
					&mut control,
					started,
				)
			} else {
				control = control.with_inferred_write_access();
				self.execute_autocommit(
					statement,
					source,
					&mut bindings,
					&mutable_bindings,
					&mut control,
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
				self.persist_statement_bindings(&bindings, &mutable_bindings, let_name);
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

	/// Preserves the current lexical scope only after successful statement
	/// execution.
	fn persist_statement_bindings(
		&mut self,
		bindings: &Object,
		mutable: &std::collections::BTreeSet<Box<str>>,
		declared: Option<&str>,
	) {
		if let Some(transaction) = &mut self.transaction {
			let mut retained = transaction.gql_bindings().clone();
			for name in mutable {
				if let Some(value) = bindings.get(name).cloned() {
					retained.insert(name.as_ref(), value);
				}
			}
			if let Some(name) = declared {
				retained.insert(name, bindings.get(name).cloned().unwrap_or(Value::None));
			}
			transaction.replace_gql_bindings(retained);
		} else {
			for name in mutable {
				if let Some(value) = bindings.get(name).cloned() {
					self.bindings.insert(name.as_ref(), value);
				}
			}
			if let Some(name) = declared {
				self.bindings.insert(name, bindings.get(name).cloned().unwrap_or(Value::None));
			}
		}
	}

	/// Executes admin statement.
	fn execute_admin_statement(
		&mut self,
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
				match self.live_kill_target(bindings, &kill.job) {
					Ok(true) => return None,
					Err(error) => return Some(error_result(statement, started, error)),
					Ok(false) => {}
				}
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

	/// Retained LIVE identities, including private registrations and closed
	/// incarnations, take the transactional path; jobs retain admin dispatch.
	fn live_kill_target(
		&mut self,
		bindings: &Object,
		target: &crate::syn::Identifier,
	) -> FerrosGrynnResult<bool> {
		let Some(Value::Uuid(uuid)) = bindings.get(target.value.as_ref()) else {
			return Ok(false);
		};
		let Ok(id) = crate::live::LiveId::from_uuid(*uuid) else {
			return Ok(false);
		};
		match self.transaction.as_mut() {
			Some(tx) => tx.contains_live_query(id),
			None => Ok(self.store.inner.current_root().live_catalog().queries.contains_key(&id)),
		}
	}

	/// Executes maintenance.
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

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported syntax variant at the engine boundary.",
				));
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

	/// Returns the maintenance options result.
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

	/// Resolves maintenance target.
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

			_ => Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported syntax variant at the engine boundary.",
			)),
		}
	}

	/// Resolves index owner.
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

			_ => Err(crate::gql_status::FerrosGrynnError::invalid_argument(
				"Unsupported syntax variant at the engine boundary.",
			)),
		}
	}

	/// Resolves index target.
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

	/// Returns the enqueue concurrent index result.
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

	/// Executes transaction command.
	fn execute_transaction_command(
		&mut self,
		command: TransactionCommand,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value> {
		match command {
			TransactionCommand::Begin {
				options,
				execution_mode,
			} => {
				debug_assert!(self.transaction.is_none());
				self.transaction = Some(self.store.begin_with(options));
				if let Some(transaction) = &mut self.transaction {
					transaction.inherit_gql_binding_types(self.binding_types.clone());
				}
				self.execution_mode = execution_mode;
				Ok(Value::None)
			}
			TransactionCommand::Commit {
				full_validation,
			} => {
				let mut transaction =
					self.transaction.take().ok_or_else(FerrosGrynnError::transaction_not_active)?;
				let (acks, _ack_admission) = self.prepare_live_acks(&transaction, control)?;
				let pending = self.prepare_live_subscriptions(&mut transaction)?;
				if full_validation {
					transaction.commit_with_validation_controlled(control.deadline(), &|| {
						control.check()
					})?;
				} else {
					transaction.commit_controlled(control.deadline(), &|| control.check())?;
				}
				self.publish_live_acks(&acks);
				if let Some(scope) = &control.bridge_scope {
					scope.committed();
				}
				self.publish_live_subscriptions(pending)?;
				self.execution_mode = ExecutionMode::BestEffort;
				Ok(Value::None)
			}
			TransactionCommand::Cancel => {
				self.transaction.take().ok_or_else(FerrosGrynnError::transaction_not_active)?;
				if let Some(scope) = &control.bridge_scope {
					scope.cancelled();
				}
				self.execution_mode = ExecutionMode::BestEffort;
				Ok(Value::None)
			}
		}
	}

	/// Executes in explicit transaction.
	fn execute_in_explicit_transaction(
		&mut self,
		statement: &Statement,
		source: &str,
		bindings: &mut Object,
		mutable_bindings: &std::collections::BTreeSet<Box<str>>,
		control: &mut ExecutionControl,
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

	/// Executes autocommit.
	fn execute_autocommit(
		&mut self,
		statement: &Statement,
		source: &str,
		bindings: &mut Object,
		mutable_bindings: &std::collections::BTreeSet<Box<str>>,
		control: &mut ExecutionControl,
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
		transaction.inherit_gql_binding_types(self.binding_types.clone());
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
			let binding_types = match transaction.export_gql_binding_types(control) {
				Ok(types) => types,
				Err(error) => {
					result.result = Err(error);
					return result;
				}
			};
			match control.attach_pending_index_seek(transaction) {
				Ok(Some(transaction)) => {
					let mut transaction = transaction;
					let (acks, _ack_admission) = match self.prepare_live_acks(&transaction, control)
					{
						Ok(acks) => acks,
						Err(error) => {
							result.result = Err(error);
							return result;
						}
					};
					let pending = match self.prepare_live_subscriptions(&mut transaction) {
						Ok(pending) => pending,
						Err(error) => {
							result.result = Err(error);
							return result;
						}
					};
					let commit_started = matches!(
						statement.kind,
						StatementKind::Explain(ref explain)
							if explain.mode == crate::syn::ExplainMode::Analyze
					)
					.then(Instant::now);
					match transaction.commit_controlled(control.deadline(), &|| control.check()) {
						Ok(_) => {
							self.binding_types = binding_types;
							if let Some(scope) = &control.bridge_scope {
								scope.committed();
							}
							self.publish_live_acks(&acks);
							if let Err(error) = self.publish_live_subscriptions(pending) {
								result.result = Err(error);
							}
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

	/// Validates structure.
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

	/// Ensures parameter names available.
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

	/// Returns the merged bindings result.
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

	/// Returns whether the binding declared in current scope condition holds.
	fn binding_declared_in_current_scope(&self, name: &str, variables: &Object) -> bool {
		variables.contains_key(name)
			|| self.transaction.as_ref().map_or_else(
				|| self.bindings.contains_key(name),
				|transaction| transaction.gql_bindings().contains_key(name),
			)
	}

	/// Returns the mutable binding names result.
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

	/// Performs the rollback explicit transaction operation.
	fn rollback_explicit_transaction(&mut self) {
		self.transaction.take();
		self.execution_mode = ExecutionMode::BestEffort;
	}
}

/// Performs the finalize explain publication operation.
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

/// Resolves job id.
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

/// Resolves analyze mode.
fn resolve_analyze_mode(mode: AnalyzeModeName) -> FerrosGrynnResult<MaintenanceAnalyzeMode> {
	/// Samples used when approximate ANALYZE omits an explicit sample count.
	const DEFAULT_SAMPLE_COUNT: u32 = 30_000;
	/// Minimum sample count accepted by approximate ANALYZE.
	const MIN_SAMPLE_COUNT: u64 = 1_024;
	/// Maximum sample count accepted by approximate ANALYZE.
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

		_ => Err(crate::gql_status::FerrosGrynnError::invalid_argument(
			"Unsupported syntax variant at the engine boundary.",
		)),
	}
}

/// Validates control structure.
pub fn validate_control_structure(
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

/// Validates block structure.
fn validate_block_structure(block: &crate::syn::Block, loop_depth: u32) -> FerrosGrynnResult<()> {
	for statement in &block.statements {
		validate_control_structure(statement, loop_depth, true)?;
	}
	Ok(())
}

/// Returns the error result result.
fn error_result(statement: &Statement, started: Instant, error: FerrosGrynnError) -> QueryResult {
	QueryResult {
		kind: crate::gql::statement_kind(&statement.kind),
		elapsed: started.elapsed(),
		result: Err(error),
		continuation: None,
	}
}
