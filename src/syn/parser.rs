use reblessive::{Stack, Stk};

use super::{
	ParserSettings,
	ast::{
		BinaryOperator,
		Expr,
		ExprKind,
		Identifier,
		Literal,
		ObjectField,
		Program,
		TypeName,
		UnaryOperator,
		cover,
	},
	lexer::{keywords, lex},
	stmt::{Statement, StatementKind, TimeoutValue},
	token::{Symbol, Token, TokenKind},
};
use crate::gql_status::{
	ExecutionPhase,
	FerrosGrynnError,
	FerrosGrynnResult,
	LabeledLocation,
	Location,
	TextSpan,
};

pub(super) fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	if source.len() > u32::MAX as usize {
		return Err(FerrosGrynnError::query_text_too_large(u64::from(u32::MAX))
			.with_phase(ExecutionPhase::Parsing));
	}
	let tokens = lex(source)?;
	let mut parser = Parser {
		source,
		settings,
		tokens: TokenBuffer::new(tokens),
		in_import: false,
	};
	let mut stack = Stack::new();
	stack.enter(|stk| parser.program(stk)).finish()
}

pub(in crate::syn) struct TokenBuffer<const LOOKAHEAD: usize> {
	tokens: Vec<Token>,
	cursor: usize,
}

impl<const LOOKAHEAD: usize> TokenBuffer<LOOKAHEAD> {
	const fn new(tokens: Vec<Token>) -> Self {
		Self {
			tokens,
			cursor: 0,
		}
	}

	pub(in crate::syn) fn peek(&self, offset: usize) -> &Token {
		debug_assert!(offset < LOOKAHEAD);
		let index = self.cursor.saturating_add(offset).min(self.tokens.len() - 1);
		&self.tokens[index]
	}

	pub(in crate::syn) fn next(&mut self) -> Token {
		let token = self.peek(0).clone();
		if !matches!(token.kind, TokenKind::Eof) {
			self.cursor += 1;
		}
		token
	}
}

pub(in crate::syn) struct Parser<'a> {
	pub(in crate::syn) source: &'a str,
	pub(in crate::syn) settings: ParserSettings,
	pub(in crate::syn) tokens: TokenBuffer<4>,
	pub(in crate::syn) in_import: bool,
}

impl Parser<'_> {
	pub(in crate::syn) async fn program(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Program> {
		let mut statements = Vec::new();
		while !self.at_eof() {
			while self.consume_symbol(Symbol::Semicolon).is_some() {}
			if self.at_eof() {
				break;
			}
			let statement = context.run(|context| self.statement(context)).await?;
			if matches!(statement.kind, StatementKind::Import(_)) {
				if !statements.is_empty() {
					return Err(self.syntax(
						"BEGIN IMPORT must be the only top-level statement.",
						statement.span,
					));
				}
				statements.push(statement);
				while self.consume_symbol(Symbol::Semicolon).is_some() {}
				if !self.at_eof() {
					return Err(self.syntax(
						"BEGIN IMPORT must be the only top-level statement.",
						self.current().span,
					));
				}
				break;
			}
			statements.push(statement);
			if !self.at_eof() && self.consume_symbol(Symbol::Semicolon).is_none() {
				return Err(self.syntax("Expected `;` between statements.", self.current().span));
			}
		}
		Ok(Program {
			statements,
		})
	}

	pub(in crate::syn) async fn statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Statement> {
		let start = self.current().span;
		let kind = if self.consume_keyword(keywords::EXPLAIN).is_some() {
			StatementKind::Explain(context.run(|context| self.explain_statement(context)).await?)
		} else if self.consume_keyword(keywords::BEGIN).is_some() {
			if self.consume_keyword(keywords::IMPORT).is_some() {
				StatementKind::Import(context.run(|context| self.import_statement(context)).await?)
			} else {
				StatementKind::Begin(self.begin_statement()?)
			}
		} else if self.consume_keyword(keywords::COMMIT).is_some() {
			StatementKind::Commit(self.commit_statement()?)
		} else if self.consume_keyword(keywords::CANCEL).is_some() {
			StatementKind::Cancel
		} else if self.consume_keyword(keywords::IF).is_some() {
			StatementKind::If(context.run(|context| self.if_statement(context)).await?)
		} else if self.consume_keyword(keywords::FOR).is_some() {
			StatementKind::For(context.run(|context| self.for_statement(context)).await?)
		} else if self.consume_keyword(keywords::WHILE).is_some() {
			StatementKind::While(context.run(|context| self.while_statement(context)).await?)
		} else if self.consume_keyword(keywords::BREAK).is_some() {
			StatementKind::Break
		} else if self.consume_keyword(keywords::CONTINUE).is_some() {
			StatementKind::Continue
		} else if self.consume_keyword(keywords::TRY).is_some() {
			StatementKind::Try(context.run(|context| self.try_statement(context)).await?)
		} else if self.consume_keyword(keywords::THROW).is_some() {
			StatementKind::Throw(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else if self.consume_keyword(keywords::ASSERT).is_some() {
			StatementKind::AssertInvariants(self.assert_invariants_statement()?)
		} else if self.consume_keyword(keywords::DEFINE).is_some() {
			if self.consume_keyword(keywords::GRAPH).is_some() {
				StatementKind::DefineGraph(
					context.run(|context| self.define_graph_statement(context)).await?,
				)
			} else if self.consume_keyword(keywords::CLASS).is_some() {
				StatementKind::DefineClass(self.define_class_statement()?)
			} else if self.consume_keyword(keywords::FIELD).is_some() {
				StatementKind::DefineField(self.define_field_statement()?)
			} else if self.consume_keyword(keywords::INDEX).is_some() {
				StatementKind::DefineIndex(self.define_index_statement()?)
			} else {
				let feature = self.current_word().unwrap_or("DEFINE target");
				return Err(self.unsupported(feature, self.current().span));
			}
		} else if self.consume_keyword(keywords::CREATE).is_some() {
			StatementKind::Create(context.run(|context| self.create_statement(context)).await?)
		} else if self.consume_keyword(keywords::ATTACH).is_some() {
			StatementKind::Attach(context.run(|context| self.attach_statement(context)).await?)
		} else if self.consume_keyword(keywords::UPDATE).is_some() {
			if self.consume_keyword(keywords::GRAPH).is_some() {
				StatementKind::UpdateGraph(
					context.run(|context| self.update_graph_statement(context)).await?,
				)
			} else {
				StatementKind::Update(
					context.run(|context| self.mutate_statement(context, false)).await?,
				)
			}
		} else if self.consume_keyword(keywords::UPSERT).is_some() {
			StatementKind::Upsert(
				context.run(|context| self.mutate_statement(context, true)).await?,
			)
		} else if self.consume_keyword(keywords::DELETE).is_some() {
			StatementKind::Delete(context.run(|context| self.delete_statement(context)).await?)
		} else if self.consume_keyword(keywords::DETACH).is_some() {
			StatementKind::Detach(context.run(|context| self.detach_statement(context)).await?)
		} else if self.consume_keyword(keywords::RELATE).is_some() {
			StatementKind::Relate(context.run(|context| self.relate_statement(context)).await?)
		} else if self.consume_keyword(keywords::REMOVE).is_some() {
			StatementKind::Remove(self.remove_statement()?)
		} else if self.consume_keyword(keywords::ALTER).is_some() {
			StatementKind::Alter(self.alter_statement()?)
		} else if self.consume_keyword(keywords::INFO).is_some() {
			StatementKind::Info(self.info_statement()?)
		} else if self.consume_keyword(keywords::LET).is_some() {
			StatementKind::Let(context.run(|context| self.let_statement(context)).await?)
		} else if self.consume_keyword(keywords::MATCH).is_some() {
			StatementKind::Match(context.run(|context| self.match_statement(context)).await?)
		} else if self.consume_keyword(keywords::RETURN).is_some() {
			StatementKind::Return(context.run(|context| self.return_statement(context)).await?)
		} else if self.consume_keyword(keywords::SELECT).is_some() {
			StatementKind::Select(context.run(|context| self.select_statement(context)).await?)
		} else if let Some(feature) = self.unsupported_statement() {
			return Err(self.unsupported(feature, start));
		} else {
			StatementKind::Expression(
				context.run(|context| self.expression(context, 0, 1, 0)).await?,
			)
		};
		let timeout = if self.consume_keyword(keywords::TIMEOUT).is_some() {
			if !matches!(
				kind,
				StatementKind::Create(_)
					| StatementKind::Update(_)
					| StatementKind::UpdateGraph(_)
					| StatementKind::Upsert(_)
					| StatementKind::Delete(_)
					| StatementKind::Attach(_)
					| StatementKind::Detach(_)
					| StatementKind::Relate(_)
					| StatementKind::Select(_)
					| StatementKind::Match(_)
			) {
				return Err(
					self.syntax("TIMEOUT is not valid for this statement.", self.previous_span())
				);
			}
			Some(self.timeout_value()?)
		} else {
			None
		};
		let end = self.previous_span();
		Ok(Statement {
			kind,
			span: cover(start, end),
			timeout,
		})
	}

	fn timeout_value(&mut self) -> FerrosGrynnResult<TimeoutValue> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::Parameter(value) => Ok(TimeoutValue::Parameter(Identifier {
				value,
				quoted: false,
				span: token.span,
			})),
			TokenKind::Number(number) => {
				let unit = self.tokens.next();
				let Token {
					kind,
					span: unit_span,
					gap_before,
				} = unit;
				let TokenKind::Word {
					value: unit,
					quoted: false,
				} = kind
				else {
					return Err(self.syntax("Expected a duration unit after TIMEOUT.", unit_span));
				};
				if gap_before {
					return Err(
						self.syntax("Duration literals cannot contain whitespace.", unit_span)
					);
				}
				Ok(TimeoutValue::Literal {
					value: format!("{number}{unit}").into_boxed_str(),
					span: cover(token.span, unit_span),
				})
			}
			_ => {
				Err(self
					.syntax("Expected a duration literal or parameter after TIMEOUT.", token.span))
			}
		}
	}

	pub(in crate::syn) async fn expression(
		&mut self,
		context: &mut Stk,
		minimum_binding_power: u8,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		self.ensure_depth(depth)?;
		let mut left = context.run(|context| self.prefix(context, depth, object_depth)).await?;
		loop {
			if let Some(expr) = context
				.run(|context| self.postfix(context, left.clone(), depth, object_depth))
				.await?
			{
				left = expr;
				continue;
			}
			if self.at_symbol(Symbol::Question) && minimum_binding_power <= 2 {
				self.tokens.next();
				let then_value = context
					.run(|context| self.expression(context, 0, depth + 1, object_depth))
					.await?;
				self.expect_symbol(Symbol::Colon, "Expected `:` in conditional expression.")?;
				let else_value = context
					.run(|context| self.expression(context, 2, depth + 1, object_depth))
					.await?;
				let span = cover(left.span, else_value.span);
				left = Expr {
					kind: ExprKind::Conditional {
						condition: Box::new(left),
						then_value: Box::new(then_value),
						else_value: Box::new(else_value),
					},
					span,
				};
				continue;
			}
			if self.at_symbol(Symbol::Range) && minimum_binding_power <= 12 {
				self.tokens.next();
				let inclusive_end = self.consume_symbol(Symbol::Equal).is_some();
				let end = if self.expression_terminator() {
					None
				} else {
					Some(Box::new(
						context
							.run(|context| self.expression(context, 13, depth + 1, object_depth))
							.await?,
					))
				};
				let end_span = end.as_ref().map_or(self.previous_span(), |end| end.span);
				left = Expr {
					span: cover(left.span, end_span),
					kind: ExprKind::Range {
						start: Some(Box::new(left)),
						end,
						inclusive_end,
					},
				};
				continue;
			}
			let Some((operator, left_bp, right_bp, consumed)) = self.binary_operator() else {
				break;
			};
			if left_bp < minimum_binding_power {
				break;
			}
			for _ in 0..consumed {
				self.tokens.next();
			}
			let right = context
				.run(|context| self.expression(context, right_bp, depth + 1, object_depth))
				.await?;
			let span = cover(left.span, right.span);
			left = Expr {
				kind: ExprKind::Binary {
					left: Box::new(left),
					op: operator,
					right: Box::new(right),
				},
				span,
			};
		}
		Ok(left)
	}

	pub(in crate::syn) async fn prefix(
		&mut self,
		context: &mut Stk,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::Symbol(Symbol::Range) => {
				let inclusive_end = self.consume_symbol(Symbol::Equal).is_some();
				let end = if self.expression_terminator() {
					None
				} else {
					Some(Box::new(
						context
							.run(|context| self.expression(context, 13, depth + 1, object_depth))
							.await?,
					))
				};
				let end_span = end.as_ref().map_or(token.span, |end| end.span);
				Ok(Expr {
					kind: ExprKind::Range {
						start: None,
						end,
						inclusive_end,
					},
					span: cover(token.span, end_span),
				})
			}
			TokenKind::Number(value) => Ok(Expr {
				kind: ExprKind::Literal(Literal::Number(value)),
				span: token.span,
			}),
			TokenKind::String(value) => Ok(Expr {
				kind: ExprKind::Literal(Literal::String(value)),
				span: token.span,
			}),
			TokenKind::Parameter(value) => Ok(Expr {
				kind: ExprKind::Parameter(Identifier {
					value,
					quoted: false,
					span: token.span,
				}),
				span: token.span,
			}),
			TokenKind::GraphRef(value) => Ok(Expr {
				kind: ExprKind::Name(Identifier {
					value,
					quoted: false,
					span: token.span,
				}),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::NONE) => Ok(Expr {
				kind: ExprKind::Literal(Literal::None),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::NULL) => Ok(Expr {
				kind: ExprKind::Literal(Literal::Null),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::TRUE) => Ok(Expr {
				kind: ExprKind::Literal(Literal::Bool(true)),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::FALSE) => Ok(Expr {
				kind: ExprKind::Literal(Literal::Bool(false)),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::IF) => {
				self.if_expression(context, token.span, depth, object_depth).await
			}
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::CASE) => {
				self.case_expression(context, token.span, depth, object_depth).await
			}
			TokenKind::Word {
				value,
				quoted,
			} if !quoted
				&& value.eq_ignore_ascii_case(keywords::SET)
				&& self.at_symbol(Symbol::LeftBracket) =>
			{
				let open = self.tokens.next();
				let mut values = Vec::new();
				if !self.at_symbol(Symbol::RightBracket) {
					loop {
						values.push(
							context
								.run(|context| self.expression(context, 0, depth + 1, object_depth))
								.await?,
						);
						if self.consume_symbol(Symbol::Comma).is_none() {
							break;
						}
					}
				}
				let close = self.close(Symbol::RightBracket, open.span, "`]`")?;
				Ok(Expr {
					kind: ExprKind::Set(values),
					span: cover(token.span, close.span),
				})
			}
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case(keywords::NOT) => {
				self.unary(context, token.span, UnaryOperator::Not, depth, object_depth).await
			}
			TokenKind::Word {
				value,
				quoted,
			} => Ok(Expr {
				kind: ExprKind::Name(Identifier {
					value,
					quoted,
					span: token.span,
				}),
				span: token.span,
			}),
			TokenKind::Symbol(Symbol::Bang) => {
				self.unary(context, token.span, UnaryOperator::Not, depth, object_depth).await
			}
			TokenKind::Symbol(Symbol::DoubleBang) => {
				self.unary(context, token.span, UnaryOperator::Truthy, depth, object_depth).await
			}
			TokenKind::Symbol(Symbol::Minus) => {
				self.unary(context, token.span, UnaryOperator::Negate, depth, object_depth).await
			}
			TokenKind::Symbol(Symbol::Plus) => {
				self.unary(context, token.span, UnaryOperator::Positive, depth, object_depth).await
			}
			TokenKind::Symbol(Symbol::Less) => {
				let target = self.type_name()?;
				self.expect_symbol(Symbol::Greater, "Expected `>` after cast type.")?;
				let expr = context
					.run(|context| self.expression(context, 18, depth + 1, object_depth))
					.await?;
				Ok(Expr {
					span: cover(token.span, expr.span),
					kind: ExprKind::Cast {
						target,
						expr: Box::new(expr),
					},
				})
			}
			TokenKind::Symbol(Symbol::LeftParen) => {
				if self.consume_keyword(keywords::SELECT).is_some() {
					let select = context.run(|context| self.select_statement(context)).await?;
					let close = self.close(Symbol::RightParen, token.span, "`)`")?;
					return Ok(Expr {
						kind: ExprKind::Subquery(Box::new(select)),
						span: cover(token.span, close.span),
					});
				}
				let expr = context
					.run(|context| self.expression(context, 0, depth + 1, object_depth))
					.await?;
				let close = self.close(Symbol::RightParen, token.span, "`)`")?;
				Ok(Expr {
					span: cover(token.span, close.span),
					..expr
				})
			}
			TokenKind::Symbol(Symbol::LeftBracket) => {
				self.array(context, token.span, depth, object_depth).await
			}
			TokenKind::Symbol(Symbol::LeftBrace) => {
				self.object(context, token.span, depth, object_depth + 1).await
			}
			_ => Err(self.syntax("Expected an expression.", token.span)),
		}
	}

	pub(in crate::syn) async fn unary(
		&mut self,
		context: &mut Stk,
		start: TextSpan,
		op: UnaryOperator,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let expr =
			context.run(|context| self.expression(context, 18, depth + 1, object_depth)).await?;
		Ok(Expr {
			span: cover(start, expr.span),
			kind: ExprKind::Unary {
				op,
				expr: Box::new(expr),
			},
		})
	}

	pub(in crate::syn) async fn array(
		&mut self,
		context: &mut Stk,
		open: TextSpan,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let mut values = Vec::new();
		if !self.at_symbol(Symbol::RightBracket) {
			loop {
				values.push(
					context
						.run(|context| self.expression(context, 0, depth + 1, object_depth))
						.await?,
				);
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
		}
		let close = self.close(Symbol::RightBracket, open, "`]`")?;
		Ok(Expr {
			kind: ExprKind::Array(values),
			span: cover(open, close.span),
		})
	}

	pub(in crate::syn) async fn object(
		&mut self,
		context: &mut Stk,
		open: TextSpan,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		if object_depth > self.settings.max_object_parsing_depth {
			return Err(self.nesting_error(self.settings.max_object_parsing_depth, open));
		}
		let mut fields = Vec::new();
		if !self.at_symbol(Symbol::RightBrace) {
			loop {
				let name = self.identifier()?;
				self.expect_symbol(Symbol::Colon, "Expected `:` after object field name.")?;
				let value = context
					.run(|context| self.expression(context, 0, depth + 1, object_depth))
					.await?;
				fields.push(ObjectField {
					span: cover(name.span, value.span),
					name,
					value,
				});
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
		}
		let close = self.close(Symbol::RightBrace, open, "`}`")?;
		Ok(Expr {
			kind: ExprKind::Object(fields),
			span: cover(open, close.span),
		})
	}

	pub(in crate::syn) async fn postfix(
		&mut self,
		context: &mut Stk,
		base: Expr,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Option<Expr>> {
		let null_safe = if self.consume_symbol(Symbol::QuestionDot).is_some() {
			true
		} else if self.consume_symbol(Symbol::Dot).is_some()
			|| self.consume_symbol(Symbol::DoubleColon).is_some()
		{
			false
		} else {
			if let Some(open) = self.consume_symbol(Symbol::LeftBracket) {
				if self.consume_keyword(keywords::WHERE).is_some() {
					let predicate = context
						.run(|context| self.expression(context, 0, depth + 1, object_depth))
						.await?;
					let close = self.close(Symbol::RightBracket, open.span, "`]`")?;
					return Ok(Some(Expr {
						span: cover(base.span, close.span),
						kind: ExprKind::Filter {
							base: Box::new(base),
							predicate: Box::new(predicate),
						},
					}));
				}
				let index = context
					.run(|context| self.expression(context, 0, depth + 1, object_depth))
					.await?;
				let close = self.close(Symbol::RightBracket, open.span, "`]`")?;
				return Ok(Some(Expr {
					span: cover(base.span, close.span),
					kind: ExprKind::Index {
						base: Box::new(base),
						index: Box::new(index),
					},
				}));
			}
			let type_argument = if self.at_symbol(Symbol::Less)
				&& matches!(self.tokens.peek(1).kind, TokenKind::Word { .. })
				&& matches!(self.tokens.peek(2).kind, TokenKind::Symbol(Symbol::Greater))
				&& matches!(self.tokens.peek(3).kind, TokenKind::Symbol(Symbol::LeftParen))
			{
				self.tokens.next();
				let argument = self.type_name()?;
				self.tokens.next();
				Some(argument)
			} else {
				None
			};
			if let Some(open) = self.consume_symbol(Symbol::LeftParen) {
				let distinct = self.consume_keyword(keywords::DISTINCT).is_some();
				let mut arguments = Vec::new();
				if !self.at_symbol(Symbol::RightParen) {
					loop {
						arguments.push(
							context
								.run(|context| self.expression(context, 0, depth + 1, object_depth))
								.await?,
						);
						if self.consume_symbol(Symbol::Comma).is_none() {
							break;
						}
					}
				}
				let close = self.close(Symbol::RightParen, open.span, "`)`")?;
				return Ok(Some(Expr {
					span: cover(base.span, close.span),
					kind: ExprKind::Call {
						callee: Box::new(base),
						type_argument,
						distinct,
						arguments,
					},
				}));
			}
			if type_argument.is_some() {
				return Err(self.syntax(
					"A generic argument must be followed by a call.",
					self.current().span,
				));
			}
			return Ok(None);
		};
		if self.at_symbol(Symbol::LeftBrace) {
			if null_safe {
				return Err(self.syntax(
					"Null-safe destructuring is not supported; apply `?.` to each field.",
					self.current().span,
				));
			}
			let open = self.tokens.next();
			let mut fields = Vec::new();
			loop {
				fields.push(self.identifier()?);
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
			let close = self.close(Symbol::RightBrace, open.span, "`}`")?;
			return Ok(Some(Expr {
				span: cover(base.span, close.span),
				kind: ExprKind::Destructure {
					base: Box::new(base),
					fields,
				},
			}));
		}
		let field = self.identifier()?;
		Ok(Some(Expr {
			span: cover(base.span, field.span),
			kind: ExprKind::Field {
				base: Box::new(base),
				field,
				null_safe,
			},
		}))
	}

	pub(in crate::syn) fn binary_operator(&self) -> Option<(BinaryOperator, u8, u8, usize)> {
		let symbol = match self.current().kind {
			TokenKind::Symbol(symbol) => Some(symbol),
			_ => None,
		};
		let operator = match symbol {
			Some(Symbol::Pipeline) => (BinaryOperator::Pipeline, 1, 2, 1),
			Some(Symbol::Coalesce) => (BinaryOperator::Coalesce, 3, 4, 1),
			Some(Symbol::Elvis) => (BinaryOperator::Elvis, 3, 4, 1),
			Some(Symbol::Or) => (BinaryOperator::Or, 5, 6, 1),
			Some(Symbol::And) => (BinaryOperator::And, 7, 8, 1),
			Some(Symbol::Equal) => (BinaryOperator::Equal, 9, 10, 1),
			Some(Symbol::QuestionEqual) => (BinaryOperator::AnyEqual, 9, 10, 1),
			Some(Symbol::StarEqual) => (BinaryOperator::AllEqual, 9, 10, 1),
			Some(Symbol::NotEqual) => (BinaryOperator::NotEqual, 9, 10, 1),
			Some(Symbol::Less) => (BinaryOperator::Less, 11, 12, 1),
			Some(Symbol::LessOrEqual) => (BinaryOperator::LessOrEqual, 11, 12, 1),
			Some(Symbol::Greater) => (BinaryOperator::Greater, 11, 12, 1),
			Some(Symbol::GreaterOrEqual) => (BinaryOperator::GreaterOrEqual, 11, 12, 1),
			Some(Symbol::Plus) => (BinaryOperator::Add, 13, 14, 1),
			Some(Symbol::Minus) => (BinaryOperator::Subtract, 13, 14, 1),
			Some(Symbol::Star) => (BinaryOperator::Multiply, 15, 16, 1),
			Some(Symbol::Slash) => (BinaryOperator::Divide, 15, 16, 1),
			Some(Symbol::Percent) => (BinaryOperator::Remainder, 15, 16, 1),
			Some(Symbol::Power) => (BinaryOperator::Power, 17, 17, 1),
			_ if self.at_keyword(keywords::OR) => (BinaryOperator::Or, 5, 6, 1),
			_ if self.at_keyword(keywords::AND) => (BinaryOperator::And, 7, 8, 1),
			_ if self.at_keyword(keywords::IS) => (BinaryOperator::Is, 9, 10, 1),
			_ if self.at_keyword(keywords::IN) => (BinaryOperator::In, 9, 10, 1),
			_ if self.at_keyword(keywords::ALLIN) => (BinaryOperator::AllIn, 9, 10, 1),
			_ if self.at_keyword(keywords::ANYIN) => (BinaryOperator::AnyIn, 9, 10, 1),
			_ if self.at_keyword(keywords::NONEIN) => (BinaryOperator::NoneIn, 9, 10, 1),
			_ if self.at_keyword(keywords::CONTAINS) => (BinaryOperator::Contains, 11, 12, 1),
			_ if self.at_keyword(keywords::CONTAINSNOT) => (BinaryOperator::ContainsNot, 11, 12, 1),
			_ if self.at_keyword(keywords::CONTAINSANY) => (BinaryOperator::ContainsAny, 11, 12, 1),
			_ if self.at_keyword(keywords::CONTAINSALL) => (BinaryOperator::ContainsAll, 11, 12, 1),
			_ if self.at_keyword(keywords::NOT) && self.peek_keyword(1, keywords::IS) => {
				(BinaryOperator::NotIs, 9, 10, 2)
			}
			_ if self.at_keyword(keywords::NOT) && self.peek_keyword(1, keywords::IN) => {
				(BinaryOperator::NotIn, 9, 10, 2)
			}
			_ => return None,
		};
		Some(operator)
	}

	pub(in crate::syn) fn identifier(&mut self) -> FerrosGrynnResult<Identifier> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::Word {
				value,
				quoted,
			} => Ok(Identifier {
				value,
				quoted,
				span: token.span,
			}),
			TokenKind::GraphRef(value) => Ok(Identifier {
				value,
				quoted: false,
				span: token.span,
			}),
			_ => Err(self.syntax("Expected an identifier.", token.span)),
		}
	}

	pub(in crate::syn) fn parameter(&mut self) -> FerrosGrynnResult<Identifier> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::Parameter(value) => Ok(Identifier {
				value,
				quoted: false,
				span: token.span,
			}),
			_ => Err(self.syntax("Expected a `$parameter`.", token.span)),
		}
	}

	pub(in crate::syn) fn type_name(&mut self) -> FerrosGrynnResult<TypeName> {
		let identifier = self.identifier()?;
		Ok(TypeName {
			value: identifier.value,
			span: identifier.span,
		})
	}

	pub(in crate::syn) fn close(
		&mut self,
		symbol: Symbol,
		open: TextSpan,
		expected: &str,
	) -> FerrosGrynnResult<Token> {
		if let Some(token) = self.consume_symbol(symbol) {
			return Ok(token);
		}
		let mut error =
			self.syntax(&format!("Expected {expected} to close delimiter."), self.current().span);
		if let Ok(location) = Location::from_source(None, self.source, open) {
			error =
				error.with_related_location(LabeledLocation::new(location, Some("opened here")));
		}
		Err(error)
	}

	pub(in crate::syn) fn expect_symbol(
		&mut self,
		symbol: Symbol,
		message: &str,
	) -> FerrosGrynnResult<Token> {
		self.consume_symbol(symbol).ok_or_else(|| self.syntax(message, self.current().span))
	}

	pub(in crate::syn) fn expect_keyword(
		&mut self,
		keyword: &str,
		message: &str,
	) -> FerrosGrynnResult<Token> {
		self.consume_keyword(keyword).ok_or_else(|| self.syntax(message, self.current().span))
	}

	pub(in crate::syn) fn consume_symbol(&mut self, symbol: Symbol) -> Option<Token> {
		self.at_symbol(symbol).then(|| self.tokens.next())
	}

	pub(in crate::syn) fn consume_keyword(&mut self, keyword: &str) -> Option<Token> {
		self.at_keyword(keyword).then(|| self.tokens.next())
	}

	pub(in crate::syn) fn at_symbol(&self, symbol: Symbol) -> bool {
		matches!(self.current().kind, TokenKind::Symbol(current) if current == symbol)
	}

	pub(in crate::syn) fn at_keyword(&self, keyword: &str) -> bool {
		self.peek_keyword(0, keyword)
	}

	pub(in crate::syn) fn peek_keyword(&self, offset: usize, keyword: &str) -> bool {
		matches!(
			&self.tokens.peek(offset).kind,
			TokenKind::Word { value, quoted: false } if value.eq_ignore_ascii_case(keyword)
		)
	}

	pub(in crate::syn) fn at_eof(&self) -> bool {
		matches!(self.current().kind, TokenKind::Eof)
	}

	fn expression_terminator(&self) -> bool {
		self.at_eof()
			|| matches!(
				self.current().kind,
				TokenKind::Symbol(
					Symbol::Comma
						| Symbol::Semicolon
						| Symbol::LeftBrace
						| Symbol::RightParen
						| Symbol::RightBracket
						| Symbol::RightBrace
				)
			)
			|| self.at_keyword(keywords::THEN)
			|| self.at_keyword(keywords::ELSE)
			|| self.at_keyword(keywords::END)
			|| self.at_keyword(keywords::WHEN)
	}

	pub(in crate::syn) fn current(&self) -> &Token {
		self.tokens.peek(0)
	}

	pub(in crate::syn) fn previous_span(&self) -> TextSpan {
		self.tokens.tokens[self.tokens.cursor.saturating_sub(1)].span
	}

	pub(in crate::syn) fn ensure_depth(&self, depth: u32) -> FerrosGrynnResult<()> {
		if depth > self.settings.max_query_parsing_depth {
			Err(self.nesting_error(self.settings.max_query_parsing_depth, self.current().span))
		} else {
			Ok(())
		}
	}

	pub(in crate::syn) fn unsupported_statement(&self) -> Option<&'static str> {
		keywords::UNSUPPORTED_STATEMENTS.iter().copied().find(|keyword| self.at_keyword(keyword))
	}

	pub(in crate::syn) fn syntax(&self, message: &str, span: TextSpan) -> FerrosGrynnError {
		let error =
			FerrosGrynnError::query_parse_error(message).with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}

	pub(in crate::syn) fn unsupported(&self, feature: &str, span: TextSpan) -> FerrosGrynnError {
		let error = FerrosGrynnError::query_feature_not_supported(feature)
			.with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}

	pub(in crate::syn) fn nesting_error(
		&self,
		maximum_depth: u32,
		span: TextSpan,
	) -> FerrosGrynnError {
		let error = FerrosGrynnError::query_nesting_too_deep(u64::from(maximum_depth))
			.with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}
}

#[cfg(test)]
mod tests {
	use super::parse;
	use crate::syn::{
		BinaryOperator,
		ExecutionModeName,
		ExprKind,
		MatchDirection,
		MatchSelector,
		ParserSettings,
		StatementKind,
		TimeoutValue,
		TransactionIsolationName,
	};

	#[test]
	fn respects_precedence_and_record_adjacency() {
		let program = parse(
			"RETURN 1 + 2 * 3; SELECT VALUE name FROM person:alice",
			ParserSettings::default(),
		)
		.expect("valid query");
		let StatementKind::Return(statement) = &program.statements[0].kind else {
			panic!("expected RETURN");
		};
		let ExprKind::Binary {
			op: BinaryOperator::Add,
			right,
			..
		} = &statement.value.kind
		else {
			panic!("expected addition");
		};
		assert!(matches!(right.kind, ExprKind::Binary {
			op: BinaryOperator::Multiply,
			..
		}));
		let StatementKind::Select(statement) = &program.statements[1].kind else {
			panic!("expected SELECT");
		};
		assert!(matches!(
			statement.source.kind,
			crate::syn::SelectSourceKind::Atoms(crate::syn::SelectTarget::Record { .. })
		));
	}

	#[test]
	fn reports_unsupported_statement_and_unclosed_delimiter() {
		let transaction = parse("BEGIN; COMMIT;", ParserSettings::default())
			.expect("transaction statements belong to the executable stage-11 profile");
		assert!(matches!(transaction.statements[0].kind, StatementKind::Begin(_)));
		assert!(matches!(transaction.statements[1].kind, StatementKind::Commit(_)));
		let relate = parse(
			"RELATE (person:alice)-[friendship:known LABEL Friend]->(person:bob) IN GRAPH social",
			ParserSettings::default(),
		)
		.expect("RELATE has an exact edge identity contract");
		assert!(matches!(relate.statements[0].kind, StatementKind::Relate(_)));

		let unclosed =
			parse("RETURN [1, 2", ParserSettings::default()).expect_err("array is not closed");
		assert_eq!(unclosed.status().code().to_string(), "FG-42001");
		assert_eq!(unclosed.status().diagnostic().related_locations().len(), 1);
	}

	#[test]
	fn parses_transaction_options_and_statement_timeout() {
		let program = parse(
			"BEGIN ISOLATION LEVEL SNAPSHOT READ ONLY EXECUTION MODE BEST_EFFORT; SELECT * FROM \
			 person TIMEOUT 25ms; COMMIT WITH VALIDATION;",
			ParserSettings::default(),
		)
		.expect("stage-11 transaction syntax parses");
		let StatementKind::Begin(begin) = &program.statements[0].kind else {
			panic!("expected BEGIN");
		};
		assert_eq!(begin.isolation, TransactionIsolationName::Snapshot);
		assert!(begin.read_only);
		assert_eq!(begin.execution_mode, ExecutionModeName::BestEffort);
		assert!(matches!(
			program.statements[1].timeout,
			Some(TimeoutValue::Literal {
				ref value,
				..
			}) if value.as_ref() == "25ms"
		));
		let StatementKind::Commit(commit) = &program.statements[2].kind else {
			panic!("expected COMMIT");
		};
		assert!(commit.full_validation);
	}

	#[test]
	fn parses_control_flow_and_assertion_surface() {
		let program = parse(
			"IF true { 1; } ELSE { 2; }; FOR $x IN 1..=2 { $x; }; WHILE (false) { BREAK; }; TRY { \
			 THROW 'boom'; } CATCH |$error| { $error.code; }; ASSERT INVARIANTS FOR GRAPH social \
			 REPORT 3;",
			ParserSettings::default(),
		)
		.expect("stage-12 control syntax parses");
		assert!(matches!(program.statements[0].kind, StatementKind::If(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::For(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::While(_)));
		assert!(matches!(program.statements[3].kind, StatementKind::Try(_)));
		let StatementKind::AssertInvariants(assertion) = &program.statements[4].kind else {
			panic!("expected ASSERT INVARIANTS");
		};
		assert_eq!(assertion.graph.as_ref().map(|graph| graph.value.as_ref()), Some("social"));
		assert_eq!(assertion.report, Some(3));
	}

	#[test]
	fn parses_core_dml_and_rejects_unbounded_upsert() {
		let program = parse(
			"CREATE person:alice CONTENT {name: 'Alice'} RETURN *; UPDATE person SET active = \
			 true WHERE age >= 18 RETURN DIFF EXPECT active = false; DELETE person:alice RETURN \
			 BEFORE EXPECT EXISTS",
			ParserSettings::default(),
		)
		.expect("core DML parses");
		assert!(matches!(program.statements[0].kind, StatementKind::Create(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::Update(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::Delete(_)));

		let error = parse("UPSERT person SET active = true", ParserSettings::default())
			.expect_err("UPSERT must be bounded by ID or WHERE");
		assert_eq!(error.status().code().to_string(), "FG-42001");
		for source in [
			"CREATE person:alice MERGE {name: 'Alice'}",
			"CREATE person:alice RETURN BEFORE",
			"DELETE person:alice RETURN VALUE name",
		] {
			let error = parse(source, ParserSettings::default())
				.expect_err("statement-specific DML grammar must be enforced");
			assert_eq!(error.status().code().to_string(), "FG-42001", "{source}");
		}
	}

	#[test]
	fn parses_atomic_import_as_the_only_top_level_statement() {
		let program = parse(
			"BEGIN IMPORT INTO GRAPH social WITH (INDEXES = DEFERRED, CONSTRAINTS = IMMEDIATE, ON \
			 VIOLATION = SKIP, REJECT LIMIT = 2, SEGMENT SIZE = 1, EVENTS = OFF, CHANGEFEED = \
			 OFF); ATTACH person:alice LABEL Person; UPDATE person:alice LABEL Friend; RELATE \
			 (person:alice)-[friendship:known]->(person:bob); END IMPORT WITH VALIDATION;",
			ParserSettings::default(),
		)
		.expect("stage-10 import syntax parses");
		let StatementKind::Import(statement) = &program.statements[0].kind else {
			panic!("expected BEGIN IMPORT");
		};
		assert_eq!(program.statements.len(), 1);
		assert_eq!(statement.statements.len(), 3);
		assert_eq!(statement.options.len(), 7);
		assert!(statement.full_validation);
		let StatementKind::Attach(attach) = &statement.statements[0].kind else {
			panic!("expected ATTACH");
		};
		assert!(attach.graph.is_none());

		for source in [
			"RETURN 1; BEGIN IMPORT; END IMPORT;",
			"BEGIN IMPORT; CREATE person:alice; END IMPORT; RETURN 1;",
			"BEGIN IMPORT; DEFINE CLASS forbidden TYPE NODE; END IMPORT;",
			"BEGIN IMPORT; BEGIN IMPORT; END IMPORT; END IMPORT;",
		] {
			assert!(parse(source, ParserSettings::default()).is_err(), "source: {source}");
		}
	}

	#[test]
	fn enforces_query_depth() {
		let settings = ParserSettings {
			max_query_parsing_depth: 2,
			..ParserSettings::default()
		};
		let error = parse("RETURN (((1)))", settings).expect_err("depth is limited");
		assert_eq!(error.status().code().to_string(), "FG-54F06");
	}

	#[test]
	fn distinguishes_spaced_record_separator() {
		let error = parse("SELECT VALUE name FROM person :alice", ParserSettings::default())
			.expect_err("a spaced colon is not a record ID");
		assert_eq!(error.status().code().to_string(), "FG-42001");
	}

	#[test]
	fn parses_standard_atomwalk_chains_and_ranges() {
		let standard = parse(
			"MATCH (Человек AS a LABEL Person)-[ЗНАЕТ AS e LABEL Friend]->(* AS b)<-[LIKES]-(* AS \
			 c), path = (* AS c)-[NEXT*0..3]->(* AS d), (* AS u)->(* AS v)<-(* AS w)-(* AS \
			 x)<->(* AS y), (* AS p)-[E*2]->(* AS q), (* AS r)-[E*..4]->(* AS s) IN GRAPH social \
			 RETURN a, path",
			ParserSettings::default(),
		)
		.expect("valid StandardPath MATCH");
		let StatementKind::Match(statement) = &standard.statements[0].kind else {
			panic!("expected MATCH");
		};
		assert_eq!(statement.patterns.len(), 5);
		assert_eq!(statement.patterns[0].steps.len(), 2);
		assert!(matches!(statement.patterns[0].start.selector, MatchSelector::Class(_)));
		assert_eq!(
			statement.patterns[0].start.alias.as_ref().map(|alias| alias.value.as_ref()),
			Some("a")
		);
		assert_eq!(
			statement.patterns[0].start.label.as_ref().map(|label| label.value.as_ref()),
			Some("Person")
		);
		assert_eq!(statement.patterns[0].steps[0].direction, MatchDirection::Forward);
		assert_eq!(statement.patterns[0].steps[1].direction, MatchDirection::Backward);
		assert_eq!(
			statement.patterns[1].steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((0, 3))
		);
		assert_eq!(
			statement.patterns[2].steps.iter().map(|step| step.direction).collect::<Vec<_>>(),
			[
				MatchDirection::Forward,
				MatchDirection::Backward,
				MatchDirection::Undirected,
				MatchDirection::Bidirectional,
			]
		);
		assert_eq!(
			statement.patterns[3].steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((2, 2))
		);
		assert_eq!(
			statement.patterns[4].steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((1, 4))
		);

		let atom = parse(
			"MATCH ATOM <(Person AS v)>~[ANNOTATES AS link]~><[FRIEND AS e]><~[BACK AS \
			 back]~<(Person AS w)> ~[UNDIRECTED AS u]~<(Person AS x)><~[BOTH AS bi]~><(Person AS \
			 y)> IN GRAPH social RETURN v, e, w",
			ParserSettings::default(),
		)
		.expect("valid AtomWalk MATCH");
		let StatementKind::Match(statement) = &atom.statements[0].kind else {
			panic!("expected MATCH ATOM");
		};
		assert!(statement.atom);
		assert_eq!(
			statement.patterns[0].steps.iter().map(|step| step.direction).collect::<Vec<_>>(),
			[
				MatchDirection::Forward,
				MatchDirection::Backward,
				MatchDirection::Undirected,
				MatchDirection::Bidirectional,
			]
		);
	}

	#[test]
	fn locates_unbounded_match_range_at_missing_maximum() {
		let source = "MATCH (* AS a)-[E*1..]->(* AS b) IN GRAPH social RETURN b";
		let error =
			parse(source, ParserSettings::default()).expect_err("unbounded paths are rejected");
		let span = error.status().diagnostic().primary_location().expect("located error").span();
		assert_eq!(span.offset as usize, source.find(']').expect("closing bracket"));
		assert_eq!(span.len, 1);
	}

	#[test]
	fn classifies_deferred_match_syntax_as_unsupported() {
		for source in [
			"MATCH (* AS a)-[E TRAIL]->(* AS b) IN GRAPH social RETURN b",
			"MATCH (* AS a) WITHIN (* AS b) IN GRAPH social RETURN a",
			"MATCH (* AS a)-[E CROSS_LEVEL]->(* AS b) IN GRAPH social RETURN b",
			"MATCH (* AS a) IN GRAPH social RETURN GRAPH",
		] {
			let error = parse(source, ParserSettings::default())
				.expect_err("deferred MATCH feature must be rejected");
			assert_eq!(error.status().code().to_string(), "FG-50F03", "source: {source}");
		}
	}

	#[test]
	fn rejects_legacy_match_source_and_treats_colon_as_record_id() {
		let program =
			parse("MATCH (person:alice) IN GRAPH social RETURN person", ParserSettings::default())
				.expect("adjacent colon is the normative exact RecordId selector");
		let StatementKind::Match(statement) = &program.statements[0].kind else {
			panic!("expected MATCH");
		};
		assert!(matches!(statement.patterns[0].start.selector, MatchSelector::Record { .. }));
		assert!(statement.patterns[0].start.alias.is_none());

		parse("MATCH (Person AS a) FROM social RETURN a", ParserSettings::default())
			.expect_err("legacy FROM source syntax must not remain available");
	}

	#[test]
	fn parses_class_schema_statements_and_requires_explicit_index_scope() {
		let program = parse(
			"DEFINE GRAPH social TYPE DAG; DEFINE CLASS user TYPE NODE SCHEMAFULL; DEFINE FIELD \
			 email ON CLASS user TYPE string; DEFINE INDEX by_email ON CLASS user FIELDS email \
			 UNIQUE SCOPE BASE; ALTER CLASS user SCHEMALESS; ALTER FIELD email ON CLASS user DROP \
			 TYPE; ALTER INDEX by_email ON CLASS user PREPARE REMOVE; INFO FOR DB; INFO FOR CLASS \
			 user; INFO FOR INDEX by_email ON CLASS user; REMOVE INDEX by_email ON CLASS user; \
			 REMOVE FIELD email ON CLASS user; REMOVE CLASS user; REMOVE GRAPH social",
			ParserSettings::default(),
		)
		.expect("core schema syntax");
		assert_eq!(program.statements.len(), 14);
		assert!(matches!(program.statements[0].kind, StatementKind::DefineGraph(_)));
		assert!(matches!(program.statements[3].kind, StatementKind::DefineIndex(_)));
		assert!(matches!(program.statements[6].kind, StatementKind::Alter(_)));
		assert!(matches!(program.statements[9].kind, StatementKind::Info(_)));
		assert!(matches!(program.statements[13].kind, StatementKind::Remove(_)));

		let advanced = parse(
			"DEFINE INDEX embedding ON CLASS user FIELDS vector HNSW SCOPE BASE",
			ParserSettings::default(),
		)
		.expect_err("advanced index is explicit");
		assert_eq!(advanced.status().code().to_string(), "FG-50F03");
		let alter_graph = parse("ALTER GRAPH social", ParserSettings::default())
			.expect_err("ALTER GRAPH has no approved semantics");
		assert_eq!(alter_graph.status().code().to_string(), "FG-50F03");
		assert!(parse("DEFINE TABLE user TYPE NODE", ParserSettings::default()).is_err());
		assert!(
			parse(
				"DEFINE INDEX by_email ON CLASS user FIELDS email UNIQUE",
				ParserSettings::default(),
			)
			.is_err()
		);
		let effective = parse(
			"DEFINE INDEX by_email ON CLASS user FIELDS email UNIQUE SCOPE EFFECTIVE IN GRAPH \
			 social",
			ParserSettings::default(),
		)
		.expect("effective scope");
		assert!(matches!(effective.statements[0].kind, StatementKind::DefineIndex(_)));
	}

	#[test]
	fn parses_graph_content_and_property_updates() {
		let program = parse(
			"DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics', region: 'eu'}; UPDATE GRAPH \
			 sales SET properties.owner = 'risk', properties.address.city = 'Berlin' RETURN \
			 AFTER; UPDATE GRAPH sales UNSET properties.region RETURN DIFF",
			ParserSettings::default(),
		)
		.expect("stage-13 graph property syntax");
		let StatementKind::DefineGraph(definition) = &program.statements[0].kind else {
			panic!("expected DEFINE GRAPH");
		};
		assert!(definition.content.is_some());
		assert!(matches!(program.statements[1].kind, StatementKind::UpdateGraph(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::UpdateGraph(_)));
		assert!(
			parse("UPDATE GRAPH sales SET owner = 'risk'", ParserSettings::default(),).is_err()
		);
	}

	#[test]
	fn parses_optimizer_hints_and_prefix_explain_only() {
		let program = parse(
			"EXPLAIN FULL FORMAT JSON SELECT p.id FROM person AS p JOIN department AS d ON \
			 p.department = d.id WITH INDEX p.by_department, d.primary WHERE p.active = true; \
			 EXPLAIN ANALYZE CREATE person:alice; SELECT id FROM person WITH NOINDEX;",
			ParserSettings::default(),
		)
		.expect("stage-14 optimizer syntax parses");
		assert!(matches!(program.statements[0].kind, StatementKind::Explain(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::Explain(_)));
		let StatementKind::Select(select) = &program.statements[2].kind else {
			panic!("expected SELECT");
		};
		assert!(matches!(select.access, Some(crate::syn::SelectAccessHint::NoIndex(_))));
		for source in [
			"EXPLAIN EXPLAIN SELECT id FROM person;",
			"EXPLAIN BEGIN;",
			"EXPLAIN FORMAT YAML SELECT id FROM person;",
			"SELECT id FROM person EXPLAIN;",
		] {
			assert!(parse(source, ParserSettings::default()).is_err(), "source: {source}");
		}
	}
}
