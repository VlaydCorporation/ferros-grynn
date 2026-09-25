#![allow(
	clippy::wildcard_imports,
	reason = "control syntax shares the parser helper surface with sibling statement modules"
)]

use super::*;

/// A lexical block whose value is the last executed statement or `none`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Block {
	/// Statements executed in lexical order.
	pub statements: Vec<Statement>,
	/// Source range including the braces.
	pub span: TextSpan,
}

/// Statement-form conditional with zero or more `ELSE IF` branches.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct IfStatement {
	/// Ordered condition/body pairs, including `ELSE IF` branches.
	pub branches: Vec<(Expr, Block)>,
	/// Optional final `ELSE` body.
	pub else_block: Option<Block>,
}

/// Bounded iteration over an array, set, or finite integer range.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ForStatement {
	/// Iteration binding local to the loop body.
	pub binding: Identifier,
	/// Array, set, or finite integer range expression.
	pub iterable: Expr,
	/// Loop body.
	pub body: Block,
}

/// Condition-controlled loop.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct WhileStatement {
	/// Condition evaluated before each iteration.
	pub condition: Expr,
	/// Loop body.
	pub body: Block,
}

/// Optional runtime-error handler attached to a `TRY` block.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CatchClause {
	/// Binding that receives the projected runtime error.
	pub binding: Identifier,
	/// Handler body.
	pub body: Block,
}

/// Runtime-error boundary with transaction savepoint semantics.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TryStatement {
	/// Body executed behind a transaction savepoint.
	pub body: Block,
	/// Optional catch handler.
	pub catch: Option<CatchClause>,
}

/// Full or graph-scoped invariant validation request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssertInvariantsStatement {
	/// Optional graph scope; `None` means every visible graph.
	pub graph: Option<Identifier>,
	/// Maximum number of detailed violations to attach.
	pub report: Option<u32>,
}

impl Parser<'_> {
	pub(in crate::syn) async fn block(&mut self, context: &mut Stk) -> FerrosGrynnResult<Block> {
		let open = self.expect_symbol(Symbol::LeftBrace, "Expected `{` to open a block.")?;
		let mut statements = Vec::new();
		loop {
			while self.consume_symbol(Symbol::Semicolon).is_some() {}
			if self.at_symbol(Symbol::RightBrace) {
				let close = self.close(Symbol::RightBrace, open.span, "`}`")?;
				return Ok(Block {
					statements,
					span: cover(open.span, close.span),
				});
			}
			if self.at_eof() {
				return Err(self.syntax("Expected `}` to close block.", self.current().span));
			}
			statements.push(context.run(|context| self.statement(context)).await?);
			if !self.at_symbol(Symbol::RightBrace)
				&& self.consume_symbol(Symbol::Semicolon).is_none()
			{
				return Err(
					self.syntax("Expected `;` between block statements.", self.current().span)
				);
			}
		}
	}

	pub(in crate::syn) async fn if_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<IfStatement> {
		let mut branches = Vec::new();
		loop {
			let condition = context.run(|context| self.expression(context, 0, 1, 0)).await?;
			let body = context.run(|context| self.block(context)).await?;
			branches.push((condition, body));
			if self.consume_keyword(keywords::ELSE).is_none() {
				return Ok(IfStatement {
					branches,
					else_block: None,
				});
			}
			if self.consume_keyword(keywords::IF).is_none() {
				let else_block = context.run(|context| self.block(context)).await?;
				return Ok(IfStatement {
					branches,
					else_block: Some(else_block),
				});
			}
		}
	}

	pub(in crate::syn) async fn for_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<ForStatement> {
		let binding = match self.current().kind {
			TokenKind::Parameter(_) => self.parameter()?,
			_ => self.identifier()?,
		};
		self.expect_keyword(keywords::IN, "Expected `IN` after FOR binding.")?;
		let iterable = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let body = context.run(|context| self.block(context)).await?;
		Ok(ForStatement {
			binding,
			iterable,
			body,
		})
	}

	pub(in crate::syn) async fn while_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<WhileStatement> {
		let open = self.expect_symbol(Symbol::LeftParen, "Expected `(` after WHILE.")?;
		let condition = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		self.close(Symbol::RightParen, open.span, "`)`")?;
		let body = context.run(|context| self.block(context)).await?;
		Ok(WhileStatement {
			condition,
			body,
		})
	}

	pub(in crate::syn) async fn try_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<TryStatement> {
		let body = context.run(|context| self.block(context)).await?;
		let catch = if self.consume_keyword(keywords::CATCH).is_some() {
			self.expect_symbol(Symbol::Pipe, "Expected `|` after CATCH.")?;
			let binding = match self.current().kind {
				TokenKind::Parameter(_) => self.parameter()?,
				_ => self.identifier()?,
			};
			self.expect_symbol(Symbol::Pipe, "Expected `|` after CATCH binding.")?;
			Some(CatchClause {
				binding,
				body: context.run(|context| self.block(context)).await?,
			})
		} else {
			None
		};
		Ok(TryStatement {
			body,
			catch,
		})
	}

	pub(in crate::syn) fn assert_invariants_statement(
		&mut self,
	) -> FerrosGrynnResult<AssertInvariantsStatement> {
		self.expect_keyword(keywords::INVARIANTS, "Expected `INVARIANTS` after ASSERT.")?;
		let graph = if self.consume_keyword(keywords::FOR).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after ASSERT INVARIANTS FOR.")?;
			Some(self.identifier()?)
		} else {
			None
		};
		let report = if self.consume_keyword(keywords::REPORT).is_some() {
			let token = self.tokens.next();
			let TokenKind::Number(value) = token.kind else {
				return Err(self.syntax("Expected an integer after REPORT.", token.span));
			};
			Some(value.replace('_', "").parse::<u32>().map_err(|_error| {
				self.syntax("REPORT value is outside the supported range.", token.span)
			})?)
		} else {
			None
		};
		Ok(AssertInvariantsStatement {
			graph,
			report,
		})
	}

	pub(in crate::syn) async fn if_expression(
		&mut self,
		context: &mut Stk,
		start: TextSpan,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let condition =
			context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?;
		self.expect_keyword(keywords::THEN, "Expected `THEN` in IF expression.")?;
		let then_value =
			context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?;
		self.expect_keyword(keywords::ELSE, "Expected `ELSE` in IF expression.")?;
		let else_value =
			context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?;
		let end = self.expect_keyword(keywords::END, "Expected `END` after IF expression.")?;
		Ok(Expr {
			kind: crate::syn::ExprKind::Conditional {
				condition: Box::new(condition),
				then_value: Box::new(then_value),
				else_value: Box::new(else_value),
			},
			span: cover(start, end.span),
		})
	}

	pub(in crate::syn) async fn case_expression(
		&mut self,
		context: &mut Stk,
		start: TextSpan,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let operand = if self.at_keyword(keywords::WHEN) {
			None
		} else {
			Some(Box::new(
				context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?,
			))
		};
		let mut branches = Vec::new();
		while self.consume_keyword(keywords::WHEN).is_some() {
			let condition =
				context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?;
			self.expect_keyword(keywords::THEN, "Expected `THEN` after CASE condition.")?;
			let value =
				context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?;
			branches.push((condition, value));
		}
		if branches.is_empty() {
			return Err(self.syntax("CASE requires at least one WHEN branch.", self.current().span));
		}
		let else_value = if self.consume_keyword(keywords::ELSE).is_some() {
			Some(Box::new(
				context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?,
			))
		} else {
			None
		};
		let end = self.expect_keyword(keywords::END, "Expected `END` after CASE expression.")?;
		Ok(Expr {
			kind: crate::syn::ExprKind::Case {
				operand,
				branches,
				else_value,
			},
			span: cover(start, end.span),
		})
	}
}
