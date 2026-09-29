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

/// Core `DEFINE GRAPH` syntax.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineGraphStatement {
	pub mode: DefinitionMode,
	pub name: Identifier,
	pub contract: GraphContract,
	pub root: Option<Expr>,
	pub content: Option<Expr>,
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
	pub concurrently: bool,
	pub deferred: bool,
}

/// Durable database-parameter definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineParameterStatement {
	pub mode: DefinitionMode,
	pub name: Identifier,
	pub value: Expr,
	pub comment: Option<Box<str>>,
}

/// Parsed effect annotation of a named function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FunctionEffectName {
	Pure,
	Stable,
	Impure,
}

/// One named argument in a function definition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionParameterDefinition {
	pub name: Identifier,
	pub value_type: TypeName,
}

/// Qualified function name with an optional exact overload signature.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FunctionSignatureName {
	pub namespace: Identifier,
	pub name: Identifier,
	pub parameters: Option<Vec<TypeName>>,
}

/// Durable named-function definition.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DefineFunctionStatement {
	pub mode: DefinitionMode,
	pub effect: FunctionEffectName,
	pub cost: bool,
	pub namespace: Identifier,
	pub name: Identifier,
	pub parameters: Vec<FunctionParameterDefinition>,
	pub result_type: Option<TypeName>,
	pub max_depth: Option<u8>,
	pub body: Block,
	pub comment: Option<Box<str>>,
}

/// Optional durable comment update for `ALTER PARAM`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParameterCommentChange {
	Keep,
	Set(Box<str>),
	Drop,
}

/// Core schema object accepted by `REMOVE`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoveTarget {
	Function {
		if_exists: bool,
		signature: FunctionSignatureName,
	},
	Parameter {
		if_exists: bool,
		name: Identifier,
	},
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
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AlterTarget {
	Function {
		if_exists: bool,
		signature: FunctionSignatureName,
		effect: Option<FunctionEffectName>,
		cost: Option<bool>,
		max_depth: Option<Option<u8>>,
		body: Option<Block>,
		comment: ParameterCommentChange,
	},
	Parameter {
		if_exists: bool,
		name: Identifier,
		value: Option<Expr>,
		comment: ParameterCommentChange,
	},
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
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AlterStatement {
	pub target: AlterTarget,
}

/// Metadata scope accepted by `INFO FOR`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InfoTarget {
	Database,
	Function(FunctionSignatureName),
	Parameter(Identifier),
	Job(Identifier),
	Jobs,
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
	pub(in crate::syn) async fn define_function_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineFunctionStatement> {
		let effect = self.function_effect().unwrap_or(FunctionEffectName::Impure);
		let cost = self.consume_keyword(keywords::COST).is_some();
		self.expect_keyword(keywords::FUNCTION, "Expected `FUNCTION` after function modifiers.")?;
		let mode = self.definition_mode()?;
		let (namespace, name) = self.qualified_function_name()?;
		let open = self.expect_symbol(Symbol::LeftParen, "Expected `(` after function name.")?;
		let mut parameters = Vec::new();
		if !self.at_symbol(Symbol::RightParen) {
			loop {
				let name = match self.current().kind {
					TokenKind::Parameter(_) => self.parameter()?,
					_ => self.identifier()?,
				};
				self.expect_symbol(Symbol::Colon, "Expected `:` after function parameter.")?;
				parameters.push(FunctionParameterDefinition {
					name,
					value_type: self.type_name()?,
				});
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
		}
		self.close(Symbol::RightParen, open.span, "`)`")?;
		let result_type = if self.consume_symbol(Symbol::ArrowRight).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		let max_depth = if self.consume_keyword(keywords::MAXDEPTH).is_some() {
			Some(self.function_max_depth()?)
		} else {
			None
		};
		let body = context.run(|context| self.block(context)).await?;
		let comment = if self.consume_keyword(keywords::COMMENT).is_some() {
			Some(self.string_value("Expected a string after `COMMENT`.")?)
		} else {
			None
		};
		self.expect_statement_end("DEFINE FUNCTION")?;
		Ok(DefineFunctionStatement {
			mode,
			effect,
			cost,
			namespace,
			name,
			parameters,
			result_type,
			max_depth,
			body,
			comment,
		})
	}

	pub(in crate::syn) async fn define_parameter_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineParameterStatement> {
		let mode = self.definition_mode()?;
		let name = self.parameter()?;
		self.expect_keyword(keywords::VALUE, "Expected `VALUE` after parameter name.")?;
		let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let comment = if self.consume_keyword(keywords::COMMENT).is_some() {
			Some(self.string_value("Expected a string after `COMMENT`.")?)
		} else {
			None
		};
		self.expect_statement_end("DEFINE PARAM")?;
		Ok(DefineParameterStatement {
			mode,
			name,
			value,
			comment,
		})
	}

	pub(in crate::syn) async fn define_graph_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DefineGraphStatement> {
		let mode = self.definition_mode()?;
		let name = self.identifier()?;
		let contract = if self.consume_keyword(keywords::TYPE).is_some() {
			let start = self.current().span;
			let contract = self.graph_contract_until(None, start)?;
			if contract.family.is_none() && contract.validators.is_empty() {
				return Err(self.syntax(
					"Expected a graph family or validator after `TYPE`.",
					self.current().span,
				));
			}
			contract
		} else {
			GraphContract {
				family: None,
				validators: Vec::new(),
				span: name.span,
			}
		};
		let mut root = None;
		let mut content = None;
		loop {
			if self.consume_keyword(keywords::ROOT).is_some() {
				if root.is_some() {
					return Err(self
						.syntax("DEFINE GRAPH ROOT may appear only once.", self.previous_span()));
				}
				root = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else if self.consume_keyword(keywords::CONTENT).is_some() {
				if content.is_some() {
					return Err(self.syntax(
						"DEFINE GRAPH CONTENT may appear only once.",
						self.previous_span(),
					));
				}
				content = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else {
				break;
			}
		}
		if let Some(feature) = [keywords::ROOTS, keywords::VALIDATE, keywords::COMMENT]
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
			contract,
			root,
			content,
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
		let mut concurrently = false;
		let mut deferred = false;
		while !self.at_statement_end() {
			if self.consume_keyword(keywords::CONCURRENTLY).is_some() {
				if concurrently {
					return Err(
						self.syntax("CONCURRENTLY may appear only once.", self.previous_span())
					);
				}
				concurrently = true;
			} else if self.consume_keyword(keywords::DEFER).is_some() {
				if deferred {
					return Err(self.syntax("DEFER may appear only once.", self.previous_span()));
				}
				deferred = true;
			} else {
				let feature = self.current_word().unwrap_or("DEFINE INDEX clause");
				return Err(self.unsupported(feature, self.current().span));
			}
		}
		Ok(DefineIndexStatement {
			mode,
			name,
			class,
			definition,
			scope,
			concurrently,
			deferred,
		})
	}

	pub(in crate::syn) fn remove_statement(&mut self) -> FerrosGrynnResult<RemoveStatement> {
		let target = if self.consume_keyword(keywords::FUNCTION).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Function {
				if_exists,
				signature: self.function_signature_name()?,
			}
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			let if_exists = self.if_exists()?;
			RemoveTarget::Parameter {
				if_exists,
				name: self.parameter()?,
			}
		} else if self.consume_keyword(keywords::GRAPH).is_some() {
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

	pub(in crate::syn) async fn alter_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<AlterStatement> {
		let target = if self.consume_keyword(keywords::FUNCTION).is_some() {
			let if_exists = self.if_exists()?;
			let signature = self.function_signature_name()?;
			let effect = self.function_effect();
			let cost = if self.consume_keyword(keywords::COST).is_some() {
				Some(true)
			} else if self.at_keyword(keywords::DROP) && self.peek_keyword(1, keywords::COST) {
				self.tokens.next();
				self.expect_keyword(keywords::COST, "Expected `COST` after `DROP`.")?;
				Some(false)
			} else {
				None
			};
			let max_depth = if self.consume_keyword(keywords::MAXDEPTH).is_some() {
				Some(Some(self.function_max_depth()?))
			} else if self.at_keyword(keywords::DROP) && self.peek_keyword(1, keywords::MAXDEPTH) {
				self.tokens.next();
				self.expect_keyword(keywords::MAXDEPTH, "Expected `MAXDEPTH` after `DROP`.")?;
				Some(None)
			} else {
				None
			};
			let body = if self.at_symbol(Symbol::LeftBrace) {
				Some(context.run(|context| self.block(context)).await?)
			} else {
				None
			};
			let comment = if self.consume_keyword(keywords::COMMENT).is_some() {
				ParameterCommentChange::Set(
					self.string_value("Expected a string after `COMMENT`.")?,
				)
			} else if self.at_keyword(keywords::DROP) && self.peek_keyword(1, keywords::COMMENT) {
				self.tokens.next();
				self.expect_keyword(keywords::COMMENT, "Expected `COMMENT` after `DROP`.")?;
				ParameterCommentChange::Drop
			} else {
				ParameterCommentChange::Keep
			};
			if effect.is_none()
				&& cost.is_none()
				&& max_depth.is_none()
				&& body.is_none()
				&& comment == ParameterCommentChange::Keep
			{
				return Err(self.syntax(
					"ALTER FUNCTION requires an effect, COST, MAXDEPTH, body, or COMMENT change.",
					self.current().span,
				));
			}
			AlterTarget::Function {
				if_exists,
				signature,
				effect,
				cost,
				max_depth,
				body,
				comment,
			}
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			let if_exists = self.if_exists()?;
			let name = self.parameter()?;
			let mut value = None;
			let mut comment = ParameterCommentChange::Keep;
			while !self.at_statement_end() {
				if self.consume_keyword(keywords::VALUE).is_some() {
					if value.is_some() {
						return Err(self.syntax(
							"ALTER PARAM VALUE may appear only once.",
							self.previous_span(),
						));
					}
					value = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
				} else if self.consume_keyword(keywords::COMMENT).is_some() {
					if comment != ParameterCommentChange::Keep {
						return Err(self.syntax(
							"ALTER PARAM COMMENT may appear only once.",
							self.previous_span(),
						));
					}
					comment = ParameterCommentChange::Set(
						self.string_value("Expected a string after `COMMENT`.")?,
					);
				} else if self.consume_keyword(keywords::DROP).is_some() {
					self.expect_keyword(keywords::COMMENT, "Expected `COMMENT` after `DROP`.")?;
					if comment != ParameterCommentChange::Keep {
						return Err(self.syntax(
							"ALTER PARAM COMMENT may appear only once.",
							self.previous_span(),
						));
					}
					comment = ParameterCommentChange::Drop;
				} else {
					return Err(self.syntax("Unexpected ALTER PARAM clause.", self.current().span));
				}
			}
			if value.is_none() && comment == ParameterCommentChange::Keep {
				return Err(self.syntax(
					"ALTER PARAM requires VALUE, COMMENT, or DROP COMMENT.",
					self.current().span,
				));
			}
			AlterTarget::Parameter {
				if_exists,
				name,
				value,
				comment,
			}
		} else if self.consume_keyword(keywords::CLASS).is_some() {
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
		} else if self.consume_keyword(keywords::FUNCTION).is_some() {
			InfoTarget::Function(self.function_signature_name()?)
		} else if self.consume_keyword(keywords::PARAM).is_some() {
			InfoTarget::Parameter(self.parameter()?)
		} else if self.consume_keyword(keywords::JOB).is_some() {
			InfoTarget::Job(self.parameter()?)
		} else if self.consume_keyword(keywords::JOBS).is_some() {
			InfoTarget::Jobs
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

	pub(in crate::syn) fn definition_mode(&mut self) -> FerrosGrynnResult<DefinitionMode> {
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

	pub(in crate::syn) fn if_exists(&mut self) -> FerrosGrynnResult<bool> {
		if self.consume_keyword(keywords::IF).is_none() {
			return Ok(false);
		}
		self.expect_keyword(keywords::EXISTS, "Expected `EXISTS` after `IF`.")?;
		Ok(true)
	}

	pub(in crate::syn) fn on_class(&mut self) -> FerrosGrynnResult<Identifier> {
		self.expect_keyword(keywords::ON, "Expected `ON CLASS` before class name.")?;
		self.expect_keyword(keywords::CLASS, "Expected `CLASS` after `ON`.")?;
		self.identifier()
	}

	pub(in crate::syn) fn at_statement_end(&self) -> bool {
		self.at_eof() || self.at_symbol(Symbol::Semicolon)
	}

	pub(in crate::syn) fn expect_statement_end(&self, statement: &str) -> FerrosGrynnResult<()> {
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

	fn string_value(&mut self, message: &str) -> FerrosGrynnResult<Box<str>> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::String(value) => Ok(value),
			_ => Err(self.syntax(message, token.span)),
		}
	}

	pub(in crate::syn) fn at_function_definition(&self) -> bool {
		[keywords::PURE, keywords::STABLE, keywords::IMPURE, keywords::COST, keywords::FUNCTION]
			.iter()
			.any(|keyword| self.at_keyword(keyword))
	}

	fn function_effect(&mut self) -> Option<FunctionEffectName> {
		if self.consume_keyword(keywords::PURE).is_some() {
			Some(FunctionEffectName::Pure)
		} else if self.consume_keyword(keywords::STABLE).is_some() {
			Some(FunctionEffectName::Stable)
		} else if self.consume_keyword(keywords::IMPURE).is_some() {
			Some(FunctionEffectName::Impure)
		} else {
			None
		}
	}

	fn qualified_function_name(&mut self) -> FerrosGrynnResult<(Identifier, Identifier)> {
		let namespace = self.identifier()?;
		self.expect_symbol(Symbol::DoubleColon, "Expected `::` in a function name.")?;
		let name = self.identifier()?;
		Ok((namespace, name))
	}

	fn function_signature_name(&mut self) -> FerrosGrynnResult<FunctionSignatureName> {
		let (namespace, name) = self.qualified_function_name()?;
		let parameters = if let Some(open) = self.consume_symbol(Symbol::LeftParen) {
			let mut parameters = Vec::new();
			if !self.at_symbol(Symbol::RightParen) {
				loop {
					parameters.push(self.type_name()?);
					if self.consume_symbol(Symbol::Comma).is_none() {
						break;
					}
				}
			}
			self.close(Symbol::RightParen, open.span, "`)`")?;
			Some(parameters)
		} else {
			None
		};
		Ok(FunctionSignatureName {
			namespace,
			name,
			parameters,
		})
	}

	fn function_max_depth(&mut self) -> FerrosGrynnResult<u8> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax("Expected MAXDEPTH in 1..=64.", token.span));
		};
		let depth = value
			.replace('_', "")
			.parse::<u8>()
			.map_err(|_error| self.syntax("Expected MAXDEPTH in 1..=64.", token.span))?;
		if !(1..=64).contains(&depth) {
			return Err(self.syntax("Expected MAXDEPTH in 1..=64.", token.span));
		}
		Ok(depth)
	}
}
