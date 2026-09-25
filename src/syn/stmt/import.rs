#![allow(
	clippy::wildcard_imports,
	reason = "the import parser shares the statement parser vocabulary with sibling modules"
)]

use super::*;

/// Graph scope declared by an atomic import header.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportGraphRef {
	/// Case-sensitive graph name.
	Name(Identifier),
	/// Runtime parameter containing a graph reference or graph name.
	Parameter(Identifier),
}

/// Canonical import option name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ImportOptionName {
	Indexes,
	Constraints,
	OnViolation,
	RejectLimit,
	SegmentSize,
	Events,
	Changefeed,
	FillFactor,
	SortMem,
	Parallelism,
}

/// One syntactic import option value, validated before staging is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportOptionValue {
	Word(Identifier),
	Number {
		value: Box<str>,
		span: TextSpan,
	},
}

/// One option assignment from `BEGIN IMPORT WITH (...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportOption {
	pub name: ImportOptionName,
	pub value: ImportOptionValue,
	pub span: TextSpan,
}

/// One atomic import program.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ImportStatement {
	pub graph: Option<ImportGraphRef>,
	pub options: Vec<ImportOption>,
	pub statements: Vec<Statement>,
	pub full_validation: bool,
}

impl Parser<'_> {
	/// Parses a complete top-level `BEGIN IMPORT ... END IMPORT` program.
	///
	/// # Errors
	///
	/// Returns syntax or unsupported-feature statuses for malformed headers,
	/// nested imports, invalid body statements, or an incomplete terminator.
	pub(in crate::syn) async fn import_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<ImportStatement> {
		if self.in_import {
			return Err(self.unsupported("nested BEGIN IMPORT", self.previous_span()));
		}
		let graph = if self.consume_keyword("INTO").is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `INTO`.")?;
			Some(match &self.current().kind {
				TokenKind::Parameter(_) => ImportGraphRef::Parameter(self.parameter()?),
				_ => ImportGraphRef::Name(self.identifier()?),
			})
		} else {
			None
		};
		let options = if self.consume_keyword(keywords::WITH).is_some() {
			self.import_options()?
		} else {
			Vec::new()
		};
		self.expect_symbol(Symbol::Semicolon, "Expected `;` after BEGIN IMPORT header.")?;

		self.in_import = true;
		let mut statements = Vec::new();
		while !self.at_keyword(keywords::END) {
			while self.consume_symbol(Symbol::Semicolon).is_some() {}
			if self.at_keyword(keywords::END) {
				break;
			}
			if self.at_eof() {
				return Err(self.syntax("Expected `END IMPORT`.", self.current().span));
			}
			let statement = context.run(|context| self.statement(context)).await?;
			if !matches!(
				statement.kind,
				StatementKind::Create(_)
					| StatementKind::Update(_)
					| StatementKind::Upsert(_)
					| StatementKind::Delete(_)
					| StatementKind::Attach(_)
					| StatementKind::Detach(_)
					| StatementKind::Relate(_)
					| StatementKind::If(_)
					| StatementKind::For(_)
					| StatementKind::Try(_)
					| StatementKind::Throw(_)
			) {
				return Err(self.unsupported("statement inside BEGIN IMPORT", statement.span));
			}
			statements.push(statement);
			self.expect_symbol(Symbol::Semicolon, "Expected `;` after import item.")?;
		}
		self.in_import = false;
		self.expect_keyword(keywords::END, "Expected `END IMPORT`.")?;
		self.expect_keyword(keywords::IMPORT, "Expected `IMPORT` after `END`.")?;
		let full_validation = if self.consume_keyword(keywords::WITH).is_some() {
			self.expect_keyword(
				keywords::VALIDATION,
				"Expected `VALIDATION` after `END IMPORT WITH`.",
			)?;
			true
		} else {
			false
		};
		Ok(ImportStatement {
			graph,
			options,
			statements,
			full_validation,
		})
	}

	/// Parses the parenthesized option assignment list without semantic
	/// validation.
	///
	/// # Errors
	///
	/// Returns a syntax status for an unknown name, missing delimiter, or
	/// invalid lexical value form.
	fn import_options(&mut self) -> FerrosGrynnResult<Vec<ImportOption>> {
		let open = self.expect_symbol(Symbol::LeftParen, "Expected `(` after import `WITH`.")?;
		let mut options = Vec::new();
		loop {
			let start = self.current().span;
			let name = if self.consume_keyword(keywords::INDEXES).is_some() {
				ImportOptionName::Indexes
			} else if self.consume_keyword("CONSTRAINTS").is_some() {
				ImportOptionName::Constraints
			} else if self.consume_keyword(keywords::ON).is_some() {
				self.expect_keyword(keywords::VIOLATION, "Expected `VIOLATION` after `ON`.")?;
				ImportOptionName::OnViolation
			} else if self.consume_keyword(keywords::REJECT).is_some() {
				self.expect_keyword(keywords::LIMIT, "Expected `LIMIT` after `REJECT`.")?;
				ImportOptionName::RejectLimit
			} else if self.consume_keyword(keywords::SEGMENT).is_some() {
				self.expect_keyword(keywords::SIZE, "Expected `SIZE` after `SEGMENT`.")?;
				ImportOptionName::SegmentSize
			} else if self.consume_keyword(keywords::EVENTS).is_some() {
				ImportOptionName::Events
			} else if self.consume_keyword(keywords::CHANGEFEED).is_some() {
				ImportOptionName::Changefeed
			} else if self.consume_keyword("FILL_FACTOR").is_some() {
				ImportOptionName::FillFactor
			} else if self.consume_keyword("SORT_MEM").is_some() {
				ImportOptionName::SortMem
			} else if self.consume_keyword("PARALLELISM").is_some() {
				ImportOptionName::Parallelism
			} else {
				return Err(self.syntax("Expected a supported import option.", start));
			};
			self.expect_symbol(Symbol::Equal, "Expected `=` after import option name.")?;
			let token = self.tokens.next();
			let value = match token.kind {
				TokenKind::Word {
					value,
					quoted,
				} => ImportOptionValue::Word(Identifier {
					value,
					quoted,
					span: token.span,
				}),
				TokenKind::Number(value) => ImportOptionValue::Number {
					value,
					span: token.span,
				},
				_ => {
					return Err(self.syntax("Expected an import option value.", token.span));
				}
			};
			let span = cover(start, self.previous_span());
			options.push(ImportOption {
				name,
				value,
				span,
			});
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		self.close(Symbol::RightParen, open.span, "`)`")?;
		Ok(options)
	}
}
