#![allow(
	clippy::wildcard_imports,
	reason = "the DML parser shares the statement facade's compact parser vocabulary"
)]

use super::*;
use crate::syn::{ExprKind, Literal};

/// A `CREATE` statement with targets evaluated from left to right.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CreateStatement {
	pub targets: Vec<DmlTarget>,
	pub data: Option<DmlData>,
	pub returning: Option<DmlReturn>,
	pub expect: Option<ExpectClause>,
}

/// Shared syntax of `UPDATE` and `UPSERT`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MutateStatement {
	pub targets: Vec<DmlTarget>,
	pub graph: Option<Identifier>,
	pub label: Option<Option<Identifier>>,
	pub data: Option<DmlData>,
	pub filter: Option<Expr>,
	pub returning: Option<DmlReturn>,
	pub expect: Option<ExpectClause>,
}

/// A `DELETE` statement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DeleteStatement {
	pub targets: Vec<DmlTarget>,
	pub cascade: bool,
	pub filter: Option<Expr>,
	pub returning: Option<DmlReturn>,
	pub expect: Option<ExpectClause>,
}

/// A class, exact record, or bounded record generator targeted by DML.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DmlTarget {
	Class(Identifier),
	Record {
		class: Identifier,
		key: Expr,
		span: TextSpan,
	},
	Range {
		class: Identifier,
		count: u32,
		span: TextSpan,
	},
}

impl DmlTarget {
	pub(crate) fn class(&self) -> &Identifier {
		match self {
			Self::Class(class)
			| Self::Record {
				class,
				..
			}
			| Self::Range {
				class,
				..
			} => class,
		}
	}

	pub(crate) const fn is_exact(&self) -> bool {
		matches!(self, Self::Record { .. })
	}
}

/// One supported record mutation mode.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DmlData {
	Content(Expr),
	Merge(Expr),
	Patch(Expr),
	Replace(Expr),
	Set(Vec<Assignment>),
	Unset(Vec<Identifier>),
	Reset(Vec<Identifier>),
}

/// One top-level field assignment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Assignment {
	pub field: Identifier,
	pub value: Expr,
}

/// A DML result projection.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DmlReturn {
	None,
	Before,
	After,
	Diff,
	Fields(Vec<DmlReturnItem>),
	Value(Expr),
}

/// One item in an object-shaped DML return.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DmlReturnItem {
	Wildcard(TextSpan),
	Expr(Expr),
}

/// A pre-image condition evaluated before any target is changed.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ExpectClause {
	Exists {
		expected: bool,
		span: TextSpan,
	},
	Predicate(Expr),
}

impl Parser<'_> {
	pub(in crate::syn) async fn create_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<CreateStatement> {
		let targets = self.dml_targets(context, true).await?;
		let data_span = self.current().span;
		let data = self.dml_data(context).await?;
		if matches!(
			data,
			Some(DmlData::Merge(_) | DmlData::Patch(_) | DmlData::Replace(_) | DmlData::Unset(_))
		) {
			return Err(self.syntax("CREATE supports only CONTENT or SET.", data_span));
		}
		let returning = self.dml_return(context, true).await?;
		let expect = self.expect_clause(context).await?;
		self.finish_dml("CREATE")?;
		Ok(CreateStatement {
			targets,
			data,
			returning,
			expect,
		})
	}

	pub(in crate::syn) async fn mutate_statement(
		&mut self,
		context: &mut Stk,
		upsert: bool,
	) -> FerrosGrynnResult<MutateStatement> {
		let targets = self.dml_targets(context, false).await?;
		let graph = if self.consume_keyword(keywords::IN).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN`.")?;
			Some(self.identifier()?)
		} else {
			None
		};
		if upsert && graph.is_some() {
			return Err(self.syntax("UPSERT does not support `IN GRAPH`.", self.previous_span()));
		}
		let label = if self.consume_keyword(keywords::LABEL).is_some() {
			if self.consume_keyword(keywords::NONE).is_some() {
				Some(None)
			} else {
				Some(Some(self.identifier()?))
			}
		} else {
			None
		};
		if label.is_some() && graph.is_none() && !self.in_import {
			return Err(self.syntax("LABEL update requires `IN GRAPH`.", self.previous_span()));
		}
		let data = self.dml_data(context).await?;
		if matches!(data, Some(DmlData::Reset(_))) && graph.is_none() && !self.in_import {
			return Err(self.syntax("RESET requires `IN GRAPH`.", self.previous_span()));
		}
		let filter = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		if upsert && filter.is_none() && !targets.iter().all(DmlTarget::is_exact) {
			return Err(self.syntax(
				"UPSERT requires an explicit record ID or a WHERE clause.",
				targets[0].class().span,
			));
		}
		let returning = self.dml_return(context, false).await?;
		let expect = self.expect_clause(context).await?;
		self.finish_dml(if upsert {
			"UPSERT"
		} else {
			"UPDATE"
		})?;
		Ok(MutateStatement {
			targets,
			graph,
			label,
			data,
			filter,
			returning,
			expect,
		})
	}

	pub(in crate::syn) async fn delete_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DeleteStatement> {
		let targets = self.dml_targets(context, false).await?;
		let cascade = self.consume_keyword(keywords::CASCADE).is_some();
		let filter = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let return_span = self.current().span;
		let returning = self.dml_return(context, false).await?;
		if matches!(returning, Some(DmlReturn::Value(_))) {
			return Err(self.syntax("DELETE does not support RETURN VALUE.", return_span));
		}
		let expect = self.expect_clause(context).await?;
		self.finish_dml("DELETE")?;
		Ok(DeleteStatement {
			targets,
			cascade,
			filter,
			returning,
			expect,
		})
	}

	async fn dml_targets(
		&mut self,
		context: &mut Stk,
		allow_range: bool,
	) -> FerrosGrynnResult<Vec<DmlTarget>> {
		let mut targets = vec![self.dml_target(context, allow_range).await?];
		while self.consume_symbol(Symbol::Comma).is_some() {
			targets.push(self.dml_target(context, allow_range).await?);
		}
		Ok(targets)
	}

	pub(super) async fn dml_target(
		&mut self,
		context: &mut Stk,
		allow_range: bool,
	) -> FerrosGrynnResult<DmlTarget> {
		if let Some(open) = self.consume_symbol(Symbol::Pipe) {
			if !allow_range {
				return Err(self.unsupported("record generators outside CREATE", open.span));
			}
			let class = self.identifier()?;
			self.expect_symbol(Symbol::Colon, "Expected `:` in record generator.")?;
			let token = self.tokens.next();
			let TokenKind::Number(value) = token.kind else {
				return Err(self.syntax("Expected an integer record count.", token.span));
			};
			let count = value.replace('_', "").parse::<u32>().map_err(|_error| {
				self.syntax("Record generator count is out of range.", token.span)
			})?;
			let close = self.expect_symbol(Symbol::Pipe, "Expected `|` after record generator.")?;
			return Ok(DmlTarget::Range {
				class,
				count,
				span: cover(open.span, close.span),
			});
		}
		let parenthesized = self.consume_symbol(Symbol::LeftParen);
		let target = self.class_or_record_target(context).await?;
		if let Some(open) = parenthesized {
			if !matches!(target, DmlTarget::Record { .. }) {
				return Err(
					self.syntax("A parenthesized DML target must be a record ID.", open.span)
				);
			}
			self.close(Symbol::RightParen, open.span, "`)`")?;
		}
		Ok(target)
	}

	/// Parses the class or exact-record subset shared by DML and graph
	/// patterns.
	pub(super) async fn class_or_record_target(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DmlTarget> {
		let class = self.identifier()?;
		if self.at_symbol(Symbol::Colon)
			&& !self.current().gap_before
			&& !self.tokens.peek(1).gap_before
		{
			self.tokens.next();
			let key = if matches!(&self.current().kind, TokenKind::Word { .. }) {
				let token = self.tokens.next();
				let TokenKind::Word {
					value,
					..
				} = token.kind
				else {
					return Err(self.syntax("Expected a record ID after `:`.", token.span));
				};
				Expr {
					kind: ExprKind::Literal(Literal::String(value)),
					span: token.span,
				}
			} else {
				context.run(|context| self.expression(context, 0, 1, 0)).await?
			};
			let span = cover(class.span, key.span);
			Ok(DmlTarget::Record {
				class,
				key,
				span,
			})
		} else {
			Ok(DmlTarget::Class(class))
		}
	}

	pub(super) async fn dml_data(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<DmlData>> {
		let mode = if self.consume_keyword(keywords::CONTENT).is_some() {
			Some(0)
		} else if self.consume_keyword(keywords::MERGE).is_some() {
			Some(1)
		} else if self.consume_keyword(keywords::PATCH).is_some() {
			Some(2)
		} else if self.consume_keyword(keywords::REPLACE).is_some() {
			Some(3)
		} else {
			None
		};
		if let Some(mode) = mode {
			let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
			return Ok(Some(match mode {
				0 => DmlData::Content(value),
				1 => DmlData::Merge(value),
				2 => DmlData::Patch(value),
				_ => DmlData::Replace(value),
			}));
		}
		if self.consume_keyword(keywords::SET).is_some() {
			let mut assignments = Vec::new();
			loop {
				let field = self.identifier()?;
				self.expect_symbol(Symbol::Equal, "Expected `=` in field assignment.")?;
				let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
				assignments.push(Assignment {
					field,
					value,
				});
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
			return Ok(Some(DmlData::Set(assignments)));
		}
		if self.consume_keyword(keywords::UNSET).is_some() {
			let mut fields = vec![self.identifier()?];
			while self.consume_symbol(Symbol::Comma).is_some() {
				fields.push(self.identifier()?);
			}
			return Ok(Some(DmlData::Unset(fields)));
		}
		if self.consume_keyword(keywords::RESET).is_some() {
			let mut fields = vec![self.identifier()?];
			while self.consume_symbol(Symbol::Comma).is_some() {
				fields.push(self.identifier()?);
			}
			return Ok(Some(DmlData::Reset(fields)));
		}
		Ok(None)
	}

	pub(super) async fn dml_return(
		&mut self,
		context: &mut Stk,
		create: bool,
	) -> FerrosGrynnResult<Option<DmlReturn>> {
		if self.consume_keyword(keywords::RETURN).is_none() {
			return Ok(None);
		}
		if self.consume_keyword(keywords::NONE).is_some() {
			return Ok(Some(DmlReturn::None));
		}
		if create
			&& (self.at_keyword(keywords::BEFORE)
				|| self.at_keyword(keywords::AFTER)
				|| self.at_keyword(keywords::DIFF))
		{
			return Err(self.syntax(
				"CREATE does not support RETURN BEFORE, AFTER, or DIFF.",
				self.current().span,
			));
		}
		if !create && self.consume_keyword(keywords::BEFORE).is_some() {
			return Ok(Some(DmlReturn::Before));
		}
		if !create && self.consume_keyword(keywords::AFTER).is_some() {
			return Ok(Some(DmlReturn::After));
		}
		if !create && self.consume_keyword(keywords::DIFF).is_some() {
			return Ok(Some(DmlReturn::Diff));
		}
		if self.consume_keyword(keywords::VALUE).is_some() {
			return context
				.run(|context| self.expression(context, 0, 1, 0))
				.await
				.map(DmlReturn::Value)
				.map(Some);
		}
		let mut items = Vec::new();
		loop {
			if let Some(star) = self.consume_symbol(Symbol::Star) {
				items.push(DmlReturnItem::Wildcard(star.span));
			} else {
				items.push(DmlReturnItem::Expr(
					context.run(|context| self.expression(context, 0, 1, 0)).await?,
				));
			}
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(Some(DmlReturn::Fields(items)))
	}

	pub(super) async fn expect_clause(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<ExpectClause>> {
		if self.consume_keyword(keywords::EXPECT).is_none() {
			return Ok(None);
		}
		let start = self.current().span;
		let expected = self.consume_keyword(keywords::NOT).is_none();
		if let Some(exists) = self.consume_keyword(keywords::EXISTS) {
			return Ok(Some(ExpectClause::Exists {
				expected,
				span: cover(start, exists.span),
			}));
		}
		if !expected {
			return Err(self.syntax("Expected `EXISTS` after `EXPECT NOT`.", self.current().span));
		}
		context
			.run(|context| self.expression(context, 0, 1, 0))
			.await
			.map(ExpectClause::Predicate)
			.map(Some)
	}

	pub(super) fn finish_dml(&self, statement: &str) -> FerrosGrynnResult<()> {
		if self.at_keyword(keywords::TIMEOUT) || self.at_eof() || self.at_symbol(Symbol::Semicolon)
		{
			Ok(())
		} else {
			Err(self.syntax(&format!("Unexpected {statement} clause."), self.current().span))
		}
	}
}
