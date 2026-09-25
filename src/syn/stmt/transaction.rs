#![allow(
	clippy::wildcard_imports,
	reason = "transaction syntax shares the small parser helper surface"
)]

use super::*;

/// Isolation level written in a `BEGIN` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransactionIsolationName {
	/// Refresh the transaction root once per top-level statement.
	ReadCommitted,
	/// Keep the root captured at `BEGIN` until completion.
	Snapshot,
	/// Keep one root and validate FCW/SSI dependencies at commit.
	Serializable,
}

/// Execution mode requested by a `BEGIN` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecutionModeName {
	/// Current executable mode without deterministic ordering guarantees.
	BestEffort,
	/// Deterministic mode reserved for stage 14.
	Deterministic,
	/// Performance-first mode reserved for stage 17.
	Fast,
}

/// Options attached to one explicit transaction start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BeginStatement {
	/// Requested isolation level.
	pub isolation: TransactionIsolationName,
	/// Whether mutations are forbidden.
	pub read_only: bool,
	/// Requested execution mode.
	pub execution_mode: ExecutionModeName,
}

/// Options attached to one explicit transaction commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommitStatement {
	/// Whether the complete private root must be revalidated.
	pub full_validation: bool,
}

impl Parser<'_> {
	pub(in crate::syn) fn begin_statement(&mut self) -> FerrosGrynnResult<BeginStatement> {
		let isolation = if self.consume_keyword(keywords::ISOLATION).is_some() {
			self.expect_keyword(keywords::LEVEL, "Expected `LEVEL` after `ISOLATION`.")?;
			if self.consume_keyword(keywords::READ).is_some() {
				self.expect_keyword(
					keywords::COMMITTED,
					"Expected `COMMITTED` after `READ` in an isolation level.",
				)?;
				TransactionIsolationName::ReadCommitted
			} else if self.consume_keyword(keywords::SNAPSHOT).is_some() {
				TransactionIsolationName::Snapshot
			} else if self.consume_keyword(keywords::SERIALIZABLE).is_some() {
				TransactionIsolationName::Serializable
			} else {
				return Err(
					self.syntax("Expected a supported isolation level.", self.current().span)
				);
			}
		} else {
			TransactionIsolationName::Serializable
		};
		let read_only = if self.consume_keyword(keywords::READ).is_some() {
			self.expect_keyword(keywords::ONLY, "Expected `ONLY` after `READ`.")?;
			true
		} else {
			false
		};
		let execution_mode = if self.consume_keyword(keywords::EXECUTION).is_some() {
			self.expect_keyword(keywords::MODE, "Expected `MODE` after `EXECUTION`.")?;
			if self.consume_keyword(keywords::BEST_EFFORT).is_some() {
				ExecutionModeName::BestEffort
			} else if self.consume_keyword(keywords::DETERMINISTIC).is_some() {
				ExecutionModeName::Deterministic
			} else if self.consume_keyword(keywords::FAST).is_some() {
				ExecutionModeName::Fast
			} else {
				return Err(
					self.syntax("Expected a supported execution mode.", self.current().span)
				);
			}
		} else {
			ExecutionModeName::BestEffort
		};
		Ok(BeginStatement {
			isolation,
			read_only,
			execution_mode,
		})
	}

	pub(in crate::syn) fn commit_statement(&mut self) -> FerrosGrynnResult<CommitStatement> {
		let full_validation = if self.consume_keyword(keywords::WITH).is_some() {
			self.expect_keyword(keywords::VALIDATION, "Expected `VALIDATION` after `WITH`.")?;
			true
		} else {
			false
		};
		Ok(CommitStatement {
			full_validation,
		})
	}
}
