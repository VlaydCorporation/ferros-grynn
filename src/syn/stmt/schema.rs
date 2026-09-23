#![allow(
	clippy::wildcard_imports,
	reason = "schema statements share the parser helpers owned by their parent module"
)]

use super::*;

/// Conflict behavior shared by core `DEFINE` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DefinitionMode {
	Normal,
	IfNotExists,
	Overwrite,
}

/// Parsed atom kind before semantic lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassKindName {
	Node,
	Edge,
}

/// Parsed class schema-validation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassSchemaModeName {
	Schemaless,
	Schemafull,
}

/// Core `DEFINE GRAPH` syntax. Unsupported constraints remain visible to the
/// semantic analyzer so they produce an explicit feature status.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineGraphStatement {
	pub mode: DefinitionMode,
	pub name: Identifier,
	pub constraints: Vec<Identifier>,
}

/// Core `DEFINE CLASS` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineClassStatement {
	pub mode: DefinitionMode,
	pub name: Identifier,
	pub kind: ClassKindName,
	pub schema_mode: ClassSchemaModeName,
}

/// Core `DEFINE FIELD` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineFieldStatement {
	pub mode: DefinitionMode,
	pub name: Identifier,
	pub class: Identifier,
	pub value_type: Option<TypeName>,
}

/// Basic class-index kind accepted by stage 8.1.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BasicIndexDefinition {
	Standard(Vec<Identifier>),
	Unique(Vec<Identifier>),
	Count,
}

/// Explicit logical scope of a class index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IndexScopeName {
	Base,
	Effective {
		graph: Identifier,
	},
}

/// Core `DEFINE INDEX` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DefineIndexStatement {
	pub mode: DefinitionMode,
	pub name: Identifier,
	pub class: Identifier,
	pub definition: BasicIndexDefinition,
	pub scope: IndexScopeName,
}

/// Core schema object accepted by `REMOVE`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoveTarget {
	Graph {
		if_exists: bool,
		name: Identifier,
	},
	Class {
		if_exists: bool,
		name: Identifier,
	},
	Field {
		if_exists: bool,
		name: Identifier,
		class: Identifier,
	},
	Index {
		if_exists: bool,
		name: Identifier,
		class: Identifier,
	},
}

/// Core `REMOVE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoveStatement {
	pub target: RemoveTarget,
}

/// Supported `ALTER` operation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AlterTarget {
	Class {
		if_exists: bool,
		name: Identifier,
		schema_mode: ClassSchemaModeName,
	},
	Field {
		if_exists: bool,
		name: Identifier,
		class: Identifier,
		value_type: Option<TypeName>,
	},
	IndexPrepareRemove {
		name: Identifier,
		class: Identifier,
	},
}

/// Core `ALTER` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AlterStatement {
	pub target: AlterTarget,
}

/// Metadata scope accepted by `INFO FOR`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InfoTarget {
	Database,
	Class(Identifier),
	Index {
		name: Identifier,
		class: Identifier,
	},
}

/// Core `INFO FOR` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct InfoStatement {
	pub target: InfoTarget,
}

impl Parser<'_> {
	pub(in crate::syn) fn define_graph_statement(
		&mut self,
	) -> FerrosGrynnResult<DefineGraphStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		let mut constraints = Vec::new();
		if self.consume_keyword(keywords::TYPE).is_some() {
			while !self.at_statement_end()
				&& ![keywords::ROOT, keywords::ROOTS, keywords::VALIDATE, keywords::COMMENT]
					.iter()
					.any(|keyword| self.at_keyword(keyword))
			{
				constraints.push(self.identifier()?);
			}
			if constraints.is_empty() {
				return Err(
					self.syntax("Expected a graph constraint after `TYPE`.", self.current().span)
				);
			}
		}
		if let Some(feature) =
			[keywords::ROOT, keywords::ROOTS, keywords::VALIDATE, keywords::COMMENT]
				.iter()
				.copied()
				.find(|keyword| self.at_keyword(keyword))
		{
			return Err(self.unsupported(feature, self.current().span));
		}
		self.expect_statement_end("DEFINE GRAPH")?;
		Ok(DefineGraphStatement {
			mode,
			name,
			constraints,
		})
	}

	pub(in crate::syn) fn define_class_statement(
		&mut self,
	) -> FerrosGrynnResult<DefineClassStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		self.expect_keyword(
			keywords::TYPE,
			"Expected `TYPE NODE` or `TYPE EDGE` after class name.",
		)?;
		let kind = if self.consume_keyword(keywords::NODE).is_some() {
			ClassKindName::Node
		} else if self.consume_keyword(keywords::EDGE).is_some() {
			ClassKindName::Edge
		} else {
			return Err(self.syntax("Expected `NODE` or `EDGE` after `TYPE`.", self.current().span));
		};
		let mut schema_mode = ClassSchemaModeName::Schemaless;
		while !self.at_statement_end() {
			if self.consume_keyword(keywords::SCHEMALESS).is_some() {
				schema_mode = ClassSchemaModeName::Schemaless;
			} else if self.consume_keyword(keywords::SCHEMAFULL).is_some() {
				schema_mode = ClassSchemaModeName::Schemafull;
			} else if let Some(feature) =
				[keywords::DROP, keywords::AS, keywords::CHANGEFEED, keywords::COMMENT]
					.iter()
					.copied()
					.find(|keyword| self.at_keyword(keyword))
			{
				return Err(self.unsupported(feature, self.current().span));
			} else {
				return Err(self.syntax("Unexpected DEFINE CLASS clause.", self.current().span));
			}
		}
		Ok(DefineClassStatement {
			mode,
			name,
			kind,
			schema_mode,
		})
	}

	pub(in crate::syn) fn define_field_statement(
		&mut self,
	) -> FerrosGrynnResult<DefineFieldStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		self.expect_keyword(keywords::ON, "Expected `ON` after field name.")?;
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		let class = self.identifier()?;
		let value_type = if self.consume_keyword(keywords::TYPE).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		if !self.at_statement_end() {
			let feature = self.current_word().unwrap_or("DEFINE FIELD clause");
			return Err(self.unsupported(feature, self.current().span));
		}
		Ok(DefineFieldStatement {
			mode,
			name,
			class,
			value_type,
		})
	}

	pub(in crate::syn) fn define_index_statement(
		&mut self,
	) -> FerrosGrynnResult<DefineIndexStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		self.expect_keyword(keywords::ON, "Expected `ON` after index name.")?;
		if self.consume_keyword(keywords::GRAPH).is_some() {
			return Err(self.unsupported("graph index", self.previous_span()));
		}
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		let class = self.identifier()?;
		let definition = if self.consume_keyword(keywords::COUNT).is_some() {
			BasicIndexDefinition::Count
		} else if self.consume_keyword(keywords::FIELDS).is_some()
			|| self.consume_keyword(keywords::COLUMNS).is_some()
		{
			let mut fields = vec![self.identifier()?];
			while self.consume_symbol(Symbol::Comma).is_some() {
				fields.push(self.identifier()?);
			}
			if self.consume_keyword(keywords::UNIQUE).is_some() {
				BasicIndexDefinition::Unique(fields)
			} else if let Some(feature) =
				[keywords::FULLTEXT, keywords::HNSW, keywords::DISKANN, keywords::GEOMETRY]
					.iter()
					.copied()
					.find(|keyword| self.at_keyword(keyword))
			{
				return Err(self.unsupported(feature, self.current().span));
			} else {
				BasicIndexDefinition::Standard(fields)
			}
		} else {
			return Err(
				self.syntax("Expected `COUNT`, `FIELDS`, or `COLUMNS`.", self.current().span)
			);
		};
		self.expect_keyword(
			keywords::SCOPE,
			"Expected explicit `SCOPE BASE` or `SCOPE EFFECTIVE IN GRAPH`.",
		)?;
		let scope = if self.consume_keyword(keywords::BASE).is_some() {
			IndexScopeName::Base
		} else if self.consume_keyword(keywords::EFFECTIVE).is_some() {
			self.expect_keyword(keywords::IN, "Expected `IN GRAPH` after `SCOPE EFFECTIVE`.")?;
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `SCOPE EFFECTIVE IN`.")?;
			IndexScopeName::Effective {
				graph: self.identifier()?,
			}
		} else {
			return Err(
				self.syntax("Expected `BASE` or `EFFECTIVE` after `SCOPE`.", self.current().span)
			);
		};
		if !self.at_statement_end() {
			let feature = self.current_word().unwrap_or("DEFINE INDEX clause");
			return Err(self.unsupported(feature, self.current().span));
		}
		Ok(DefineIndexStatement {
			mode,
			name,
			class,
			definition,
			scope,
		})
	}

	pub(in crate::syn) fn remove_statement(&mut self) -> FerrosGrynnResult<RemoveStatement> {
		let target = if self.consume_keyword(keywords::GRAPH).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Graph {
				if_exists,
				name: self.identifier()?,
			}
		} else if self.consume_keyword(keywords::CLASS).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Class {
				if_exists,
				name: self.identifier()?,
			}
		} else if self.consume_keyword(keywords::FIELD).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let class = self.on_class()?;
			RemoveTarget::Field {
				if_exists,
				name,
				class,
			}
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let class = self.on_class()?;
			RemoveTarget::Index {
				if_exists,
				name,
				class,
			}
		} else {
			let feature = self.current_word().unwrap_or("REMOVE target");
			return Err(self.unsupported(feature, self.current().span));
		};
		self.expect_statement_end("REMOVE")?;
		Ok(RemoveStatement {
			target,
		})
	}

	pub(in crate::syn) fn alter_statement(&mut self) -> FerrosGrynnResult<AlterStatement> {
		let target = if self.consume_keyword(keywords::CLASS).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let schema_mode = if self.consume_keyword(keywords::SCHEMAFULL).is_some() {
				ClassSchemaModeName::Schemafull
			} else if self.consume_keyword(keywords::SCHEMALESS).is_some() {
				ClassSchemaModeName::Schemaless
			} else {
				let feature = self.current_word().unwrap_or("ALTER CLASS action");
				return Err(self.unsupported(feature, self.current().span));
			};
			AlterTarget::Class {
				if_exists,
				name,
				schema_mode,
			}
		} else if self.consume_keyword(keywords::FIELD).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.identifier()?;
			let class = self.on_class()?;
			let value_type = if self.consume_keyword(keywords::TYPE).is_some() {
				Some(self.type_name()?)
			} else if self.consume_keyword(keywords::DROP).is_some() {
				self.expect_keyword(keywords::TYPE, "Expected `TYPE` after `DROP`.")?;
				None
			} else {
				let feature = self.current_word().unwrap_or("ALTER FIELD action");
				return Err(self.unsupported(feature, self.current().span));
			};
			AlterTarget::Field {
				if_exists,
				name,
				class,
				value_type,
			}
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let name = self.identifier()?;
			let class = self.on_class()?;
			self.expect_keyword(keywords::PREPARE, "Expected `PREPARE REMOVE` for ALTER INDEX.")?;
			self.expect_keyword(keywords::REMOVE, "Expected `REMOVE` after `PREPARE`.")?;
			AlterTarget::IndexPrepareRemove {
				name,
				class,
			}
		} else if self.at_keyword(keywords::GRAPH) {
			return Err(self.unsupported("ALTER GRAPH", self.current().span));
		} else {
			let feature = self.current_word().unwrap_or("ALTER target");
			return Err(self.unsupported(feature, self.current().span));
		};
		self.expect_statement_end("ALTER")?;
		Ok(AlterStatement {
			target,
		})
	}

	pub(in crate::syn) fn info_statement(&mut self) -> FerrosGrynnResult<InfoStatement> {
		self.expect_keyword(keywords::FOR, "Expected `FOR` after `INFO`.")?;
		let target = if self.consume_keyword(keywords::DB).is_some() {
			InfoTarget::Database
		} else if self.consume_keyword(keywords::CLASS).is_some() {
			InfoTarget::Class(self.identifier()?)
		} else if self.consume_keyword(keywords::INDEX).is_some() {
			let name = self.identifier()?;
			let class = self.on_class()?;
			InfoTarget::Index {
				name,
				class,
			}
		} else {
			let feature = self.current_word().unwrap_or("INFO target");
			return Err(self.unsupported(feature, self.current().span));
		};
		self.expect_statement_end("INFO")?;
		Ok(InfoStatement {
			target,
		})
	}

	fn definition_mode(&mut self) -> FerrosGrynnResult<DefinitionMode> {
		if self.consume_keyword(keywords::OVERWRITE).is_some() {
			return Ok(DefinitionMode::Overwrite);
		}
		if self.consume_keyword(keywords::IF).is_some() {
			self.expect_keyword(keywords::NOT, "Expected `NOT EXISTS` after `IF`.")?;
			self.expect_keyword(keywords::EXISTS, "Expected `EXISTS` after `IF NOT`.")?;
			return Ok(DefinitionMode::IfNotExists);
		}
		Ok(DefinitionMode::Normal)
	}

	fn if_exists(&mut self) -> FerrosGrynnResult<bool> {
		if self.consume_keyword(keywords::IF).is_none() {
			return Ok(false);
		}
		self.expect_keyword(keywords::EXISTS, "Expected `EXISTS` after `IF`.")?;
		Ok(true)
	}

	fn on_class(&mut self) -> FerrosGrynnResult<Identifier> {
		self.expect_keyword(keywords::ON, "Expected `ON CLASS` before class name.")?;
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		self.identifier()
	}

	fn at_statement_end(&self) -> bool {
		self.at_eof() || self.at_symbol(Symbol::Semicolon)
	}

	fn expect_statement_end(&self, statement: &str) -> FerrosGrynnResult<()> {
		if self.at_statement_end() {
			Ok(())
		} else {
			Err(self.syntax(&format!("Unexpected {statement} clause."), self.current().span))
		}
	}

	pub(in crate::syn) fn current_word(&self) -> Option<&str> {
		match &self.current().kind {
			TokenKind::Word {
				value,
				..
			} => Some(value),
			_ => None,
		}
	}
}
