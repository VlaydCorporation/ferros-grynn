//! Persistent GrynnQL session and the shared statement transaction driver.

use std::time::Instant;

use super::{Datastore, Transaction, TransactionAccess, TransactionOptions};
use crate::{
	gql::{ExecutionControl, QueryExecutionOptions, QueryResult, TransactionCommand},
	gql_status::{FerrosGrynnError, FerrosGrynnResult, Outcome},
	syn::{Program, Statement, StatementKind},
	values::{Object, Value},
};

/// Persistent bindings and optional explicit transaction for GrynnQL calls.
pub struct GqlSession {
	store: Datastore,
	bindings: Object,
	transaction: Option<Transaction>,
}

impl GqlSession {
	pub(crate) fn new(store: Datastore) -> Self {
		Self {
			store,
			bindings: Object::new(),
			transaction: None,
		}
	}

	/// Returns whether this session currently owns an explicit transaction.
	pub const fn has_active_transaction(&self) -> bool {
		self.transaction.is_some()
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
		let program = match crate::gql::parse(source, options.parser_settings) {
			Ok(program) => program,
			Err(error) => {
				self.rollback_explicit_transaction();
				return Err(error);
			}
		};
		if let Err(error) = self
			.ensure_parameter_names_available(variables)
			.and_then(|()| self.validate_structure(&program, require_closed))
		{
			self.rollback_explicit_transaction();
			return Err(error);
		}
		let mut bindings = self.merged_bindings(variables);
		if let [
			statement @ Statement {
				kind: StatementKind::Import(import),
				..
			},
		] = program.statements.as_slice()
		{
			let timeout = crate::gql::resolve_timeout(statement.timeout.as_ref(), &bindings)?;
			let control = ExecutionControl::new(options, timeout);
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
				});
				if failed {
					break;
				}
				if index + 1 < program.statements.len() {
					bindings = self.merged_bindings(variables);
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
			let control = ExecutionControl::new(options, clause_timeout);
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

			let mut result = if self.transaction.is_some() {
				self.execute_in_explicit_transaction(
					statement,
					source,
					&mut bindings,
					&control,
					started,
				)
			} else {
				self.execute_autocommit(statement, source, &mut bindings, &control, started)
			};
			let failed = result.result.is_err();
			if !failed {
				if let Some(name) = let_name {
					let value = bindings.get(name).cloned().unwrap_or(Value::None);
					if let Some(transaction) = &mut self.transaction {
						let mut transaction_bindings = transaction.gql_bindings().clone();
						transaction_bindings.insert(name, value);
						transaction.replace_gql_bindings(transaction_bindings);
					} else {
						self.bindings.insert(name, value);
					}
				}
			} else {
				self.rollback_explicit_transaction();
			}
			result.elapsed = started.elapsed();
			results.push(result);
			if failed {
				break;
			}
			if index + 1 < program.statements.len() {
				bindings = self.merged_bindings(variables);
			}
		}
		Ok(results)
	}

	fn execute_transaction_command(
		&mut self,
		command: TransactionCommand,
	) -> FerrosGrynnResult<Value> {
		match command {
			TransactionCommand::Begin(options) => {
				debug_assert!(self.transaction.is_none());
				self.transaction = Some(self.store.begin_with(options));
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
				Ok(Value::None)
			}
			TransactionCommand::Cancel => {
				self.transaction.take().ok_or_else(FerrosGrynnError::transaction_not_active)?;
				Ok(Value::None)
			}
		}
	}

	fn execute_in_explicit_transaction(
		&mut self,
		statement: &Statement,
		source: &str,
		bindings: &mut Object,
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
				control,
			)
		});
		let mut result = match execution {
			Ok(result) => result,
			Err(error) => return error_result(statement, started, error),
		};
		if result.result.is_ok()
			&& let Err(error) = transaction.commit()
		{
			result.result = Err(error);
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
				Some(TransactionCommand::Begin(_)) if active => {
					return Err(FerrosGrynnError::transaction_already_active());
				}
				Some(TransactionCommand::Begin(_)) => active = true,
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

	fn merged_bindings(&self, variables: &Object) -> Object {
		if self.bindings.is_empty() && self.transaction.is_none() {
			return variables.clone();
		}
		let mut bindings = self.bindings.clone();
		if let Some(transaction) = &self.transaction {
			for (name, value) in transaction.gql_bindings() {
				bindings.insert(name.clone(), value.clone());
			}
		}
		for (name, value) in variables {
			bindings.insert(name.clone(), value.clone());
		}
		bindings
	}

	fn binding_declared_in_current_scope(&self, name: &str, variables: &Object) -> bool {
		variables.contains_key(name)
			|| self.transaction.as_ref().map_or_else(
				|| self.bindings.contains_key(name),
				|transaction| transaction.gql_bindings().contains_key(name),
			)
	}

	fn rollback_explicit_transaction(&mut self) {
		self.transaction.take();
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
	}
}
