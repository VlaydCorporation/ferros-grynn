#![allow(
	clippy::wildcard_imports,
	reason = "move-only statement split keeps the existing parser imports during stage 1"
)]

use super::*;

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectStatement {
	pub only: bool,
	pub value: bool,
	pub projection: Vec<SelectItem>,
	pub target: SelectTarget,
	pub graph: Option<Identifier>,
	pub filter: Option<Expr>,
	pub limit: Option<Expr>,
	pub start: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectItem {
	Wildcard(TextSpan),
	Expr {
		expr: Expr,
		alias: Option<Identifier>,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectTarget {
	Class(Identifier),
	Record {
		class: Identifier,
		key: RecordKey,
		span: TextSpan,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordKey {
	Integer(Box<str>, TextSpan),
	String(Box<str>, TextSpan),
}

impl RecordKey {
	pub(crate) const fn span(&self) -> TextSpan {
		match self {
			Self::Integer(_, span) | Self::String(_, span) => *span,
		}
	}
}

impl Parser<'_> {
	pub(in crate::syn) async fn select_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<SelectStatement> {
		let only = self.consume_keyword(keywords::ONLY).is_some();
		let value = self.consume_keyword(keywords::VALUE).is_some();
		let mut projection = Vec::new();
		loop {
			if let Some(token) = self.consume_symbol(Symbol::Star) {
				projection.push(SelectItem::Wildcard(token.span));
			} else {
				let expr = context.run(|context| self.expression(context, 0, 1, 0)).await?;
				let alias = if self.consume_keyword(keywords::AS).is_some() {
					Some(self.identifier()?)
				} else {
					None
				};
				projection.push(SelectItem::Expr {
					expr,
					alias,
				});
			}
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		if value && projection.len() != 1 {
			return Err(
				self.syntax("SELECT VALUE requires exactly one expression.", self.current().span)
			);
		}
		self.expect_keyword(keywords::FROM, "Expected `FROM` after SELECT projection.")?;
		let target = self.select_target()?;
		let graph = if self.consume_keyword(keywords::IN).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN`.")?;
			Some(self.identifier()?)
		} else {
			None
		};
		let filter = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let limit = if self.consume_keyword(keywords::LIMIT).is_some() {
			self.consume_keyword(keywords::BY);
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let start = if self.consume_keyword(keywords::START).is_some() {
			if self.consume_keyword(keywords::AFTER).is_some() {
				return Err(self.unsupported("START AFTER", self.previous_span()));
			}
			self.consume_keyword(keywords::AT);
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		if let Some(clause) = self.unsupported_select_clause() {
			return Err(self.unsupported(clause, self.current().span));
		}
		Ok(SelectStatement {
			only,
			value,
			projection,
			target,
			graph,
			filter,
			limit,
			start,
		})
	}

	fn select_target(&mut self) -> FerrosGrynnResult<SelectTarget> {
		let class = self.identifier()?;
		if !self.at_symbol(Symbol::Colon)
			|| self.current().gap_before
			|| self.tokens.peek(1).gap_before
		{
			return Ok(SelectTarget::Class(class));
		}
		self.tokens.next();
		let token = self.tokens.next();
		let key = match token.kind {
			TokenKind::Number(value) => RecordKey::Integer(value, token.span),
			TokenKind::String(value) => RecordKey::String(value, token.span),
			TokenKind::Word {
				value,
				..
			} => RecordKey::String(value, token.span),
			_ => return Err(self.syntax("Expected a record ID after `:`.", token.span)),
		};
		Ok(SelectTarget::Record {
			span: cover(class.span, key.span()),
			class,
			key,
		})
	}

	fn unsupported_select_clause(&self) -> Option<&'static str> {
		keywords::UNSUPPORTED_SELECT_CLAUSES
			.iter()
			.copied()
			.find(|keyword| self.at_keyword(keyword))
	}
}
