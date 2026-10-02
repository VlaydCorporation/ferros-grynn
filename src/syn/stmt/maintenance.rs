#![allow(
	clippy::wildcard_imports,
	reason = "maintenance statements share parser helpers with their parent module"
)]

//! Administrative maintenance statement syntax.

use super::*;

/// Schema owner used to resolve one logical index target.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceIndexOwner {
	Class(Identifier),
	Graph(Identifier),
}

/// Logical target selected by an administrative maintenance statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceTargetName {
	Database,
	ClassBase(Identifier),
	ClassEffective {
		class: Identifier,
		graph: Identifier,
	},
	Graph(Identifier),
	Index {
		name: Identifier,
		owner: MaintenanceIndexOwner,
	},
}

/// Statistics build policy selected by `ANALYZE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzeModeName {
	Automatic,
	Full,
	Approximate {
		sample_count: Option<u64>,
	},
}

/// Supported foreground or background maintenance operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaintenanceOperationName {
	Analyze {
		target: MaintenanceTargetName,
		mode: AnalyzeModeName,
	},
	RebuildIndex {
		if_exists: bool,
		name: Identifier,
		owner: MaintenanceIndexOwner,
	},
	Compact(MaintenanceTargetName),
}

/// Parsed maintenance operation and its execution mode.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MaintenanceStatement {
	pub operation: MaintenanceOperationName,
	pub concurrently: bool,
	/// Optional aggregate sort-memory override in bytes.
	pub sort_memory_bytes: Option<u64>,
	/// Optional upper bound for scoped builder threads.
	pub parallelism: Option<u64>,
}

/// Parsed `KILL $job` command.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct KillStatement {
	pub job: Identifier,
}

impl Parser<'_> {
	pub(in crate::syn) fn analyze_statement(&mut self) -> FerrosGrynnResult<MaintenanceStatement> {
		let target = if self.consume_keyword(keywords::CLASS).is_some() {
			let class = self.identifier()?;
			if self.consume_keyword(keywords::BASE).is_some() {
				MaintenanceTargetName::ClassBase(class)
			} else if self.consume_keyword(keywords::EFFECTIVE).is_some() {
				self.expect_keyword(keywords::IN, "Expected `IN GRAPH` after `EFFECTIVE`.")?;
				self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `EFFECTIVE IN`.")?;
				MaintenanceTargetName::ClassEffective {
					class,
					graph: self.identifier()?,
				}
			} else {
				return Err(self.syntax(
					"Expected `BASE` or `EFFECTIVE IN GRAPH` after ANALYZE CLASS target.",
					self.current().span,
				));
			}
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			MaintenanceTargetName::Graph(self.identifier()?)
		} else {
			return Err(
				self.syntax("Expected `CLASS` or `GRAPH` after `ANALYZE`.", self.current().span)
			);
		};
		let mode = if self.consume_keyword(keywords::FULL).is_some() {
			AnalyzeModeName::Full
		} else if self.consume_keyword("APPROXIMATE").is_some() {
			let sample_count = if self.consume_keyword("SAMPLE").is_some() {
				Some(self.maintenance_positive_integer("ANALYZE SAMPLE")?)
			} else {
				None
			};
			AnalyzeModeName::Approximate {
				sample_count,
			}
		} else {
			AnalyzeModeName::Automatic
		};
		self.finish_maintenance(MaintenanceOperationName::Analyze {
			target,
			mode,
		})
	}

	pub(in crate::syn) fn rebuild_index_statement(
		&mut self,
	) -> FerrosGrynnResult<MaintenanceStatement> {
		self.expect_keyword(keywords::INDEX, "Expected `INDEX` after `REBUILD`.")?;
		let if_exists = self.if_exists()?;
		let name = self.identifier()?;
		let owner = self.maintenance_index_owner()?;
		self.finish_maintenance(MaintenanceOperationName::RebuildIndex {
			if_exists,
			name,
			owner,
		})
	}

	pub(in crate::syn) fn compact_statement(&mut self) -> FerrosGrynnResult<MaintenanceStatement> {
		let target = if self.consume_keyword(keywords::DATABASE).is_some()
			|| self.consume_keyword(keywords::DB).is_some()
		{
			MaintenanceTargetName::Database
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
			MaintenanceTargetName::Graph(self.identifier()?)
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let name = self.identifier()?;
			MaintenanceTargetName::Index {
				name,
				owner: self.maintenance_index_owner()?,
			}
		} else {
			return Err(self.syntax(
				"Expected `DATABASE`, `GRAPH`, or `INDEX` after `COMPACT`.",
				self.current().span,
			));
		};
		self.finish_maintenance(MaintenanceOperationName::Compact(target))
	}

	pub(in crate::syn) fn alter_database_compact_statement(
		&mut self,
	) -> FerrosGrynnResult<MaintenanceStatement> {
		self.expect_keyword(keywords::COMPACT, "Expected `COMPACT` after `ALTER DATABASE`.")?;
		self.finish_maintenance(MaintenanceOperationName::Compact(MaintenanceTargetName::Database))
	}

	pub(in crate::syn) fn kill_statement(&mut self) -> FerrosGrynnResult<KillStatement> {
		let job = self.parameter()?;
		self.expect_statement_end("KILL")?;
		Ok(KillStatement {
			job,
		})
	}

	fn finish_maintenance(
		&mut self,
		operation: MaintenanceOperationName,
	) -> FerrosGrynnResult<MaintenanceStatement> {
		let concurrently = self.consume_keyword(keywords::CONCURRENTLY).is_some();
		let (sort_memory_bytes, parallelism) = if self.consume_keyword(keywords::WITH).is_some() {
			self.maintenance_options()?
		} else {
			(None, None)
		};
		self.expect_statement_end("maintenance")?;
		Ok(MaintenanceStatement {
			operation,
			concurrently,
			sort_memory_bytes,
			parallelism,
		})
	}

	fn maintenance_options(&mut self) -> FerrosGrynnResult<(Option<u64>, Option<u64>)> {
		self.expect_symbol(Symbol::LeftParen, "Expected `(` after maintenance `WITH`.")?;
		let mut sort_memory_bytes = None;
		let mut parallelism = None;
		loop {
			if self.consume_keyword("SORT_MEM").is_some() {
				if sort_memory_bytes.replace(self.maintenance_unsigned_integer()?).is_some() {
					return Err(self.syntax(
						"SORT_MEM may appear only once in maintenance options.",
						self.previous_span(),
					));
				}
			} else if self.consume_keyword("PARALLELISM").is_some() {
				if parallelism.replace(self.maintenance_unsigned_integer()?).is_some() {
					return Err(self.syntax(
						"PARALLELISM may appear only once in maintenance options.",
						self.previous_span(),
					));
				}
			} else if self.consume_keyword("FILL_FACTOR").is_some() {
				return Err(self.unsupported("maintenance FILL_FACTOR", self.previous_span()));
			} else {
				return Err(self.syntax(
					"Expected SORT_MEM or PARALLELISM in maintenance options.",
					self.current().span,
				));
			}
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		self.expect_symbol(Symbol::RightParen, "Expected `)` after maintenance options.")?;
		Ok((sort_memory_bytes, parallelism))
	}

	fn maintenance_unsigned_integer(&mut self) -> FerrosGrynnResult<u64> {
		self.expect_symbol(Symbol::Equal, "Expected `=` after maintenance option name.")?;
		self.maintenance_positive_integer("maintenance option")
	}

	pub(in crate::syn) fn maintenance_positive_integer(
		&mut self,
		field: &str,
	) -> FerrosGrynnResult<u64> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			let message = format!("Expected a positive integer {field}.");
			return Err(self.syntax(&message, token.span));
		};
		if value.contains(['.', 'e', 'E', '+', '-']) {
			let message = format!("Expected a positive integer {field}.");
			return Err(self.syntax(&message, token.span));
		}
		let value = value.parse::<u64>().map_err(|_error| {
			let message = format!("{field} is outside the u64 range.");
			self.syntax(&message, token.span)
		})?;
		if value == 0 {
			let message = format!("{field} must be greater than zero.");
			return Err(self.syntax(&message, token.span));
		}
		Ok(value)
	}

	pub(in crate::syn) fn maintenance_index_owner(
		&mut self,
	) -> FerrosGrynnResult<MaintenanceIndexOwner> {
		self.expect_keyword(keywords::ON, "Expected `ON CLASS` or `ON GRAPH` after index name.")?;
		if self.consume_keyword(keywords::CLASS).is_some() {
			return self.identifier().map(MaintenanceIndexOwner::Class);
		}
		if self.consume_keyword(keywords::GRAPH).is_some() {
			return self.identifier().map(MaintenanceIndexOwner::Graph);
		}
		Err(self.syntax("Expected `CLASS` or `GRAPH` after `ON`.", self.current().span))
	}
}
