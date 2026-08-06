use reblessive::{Stack, Stk};

use super::{
	ParserSettings,
	ast::{
		BinaryOperator,
		Expr,
		ExprKind,
		Identifier,
		LetStatement,
		Literal,
		ObjectField,
		Program,
		RecordKey,
		ReturnStatement,
		SelectItem,
		SelectStatement,
		SelectTarget,
		Statement,
		StatementKind,
		TypeName,
		UnaryOperator,
		cover,
	},
	lexer::{Symbol, Token, TokenKind, lex},
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
	};
	let mut stack = Stack::new();
	stack.enter(|context| parser.program(context)).finish()
}

struct TokenBuffer<const LOOKAHEAD: usize> {
	tokens: Vec<Token>,
	cursor: usize,
}

impl<const LOOKAHEAD: usize> TokenBuffer<LOOKAHEAD> {
	fn new(tokens: Vec<Token>) -> Self {
		Self {
			tokens,
			cursor: 0,
		}
	}

	fn peek(&self, offset: usize) -> &Token {
		debug_assert!(offset < LOOKAHEAD);
		let index = self.cursor.saturating_add(offset).min(self.tokens.len() - 1);
		&self.tokens[index]
	}

	fn next(&mut self) -> Token {
		let token = self.peek(0).clone();
		if !matches!(token.kind, TokenKind::Eof) {
			self.cursor += 1;
		}
		token
	}
}

struct Parser<'a> {
	source: &'a str,
	settings: ParserSettings,
	tokens: TokenBuffer<4>,
}

impl Parser<'_> {
	async fn program(&mut self, context: &mut Stk) -> FerrosGrynnResult<Program> {
		let mut statements = Vec::new();
		while !self.at_eof() {
			while self.consume_symbol(Symbol::Semicolon).is_some() {}
			if self.at_eof() {
				break;
			}
			statements.push(context.run(|context| self.statement(context)).await?);
			if !self.at_eof() && self.consume_symbol(Symbol::Semicolon).is_none() {
				return Err(self.syntax("Expected `;` between statements.", self.current().span));
			}
		}
		Ok(Program {
			statements,
		})
	}

	async fn statement(&mut self, context: &mut Stk) -> FerrosGrynnResult<Statement> {
		let start = self.current().span;
		let kind = if self.consume_keyword("LET").is_some() {
			StatementKind::Let(context.run(|context| self.let_statement(context)).await?)
		} else if self.consume_keyword("RETURN").is_some() {
			StatementKind::Return(context.run(|context| self.return_statement(context)).await?)
		} else if self.consume_keyword("SELECT").is_some() {
			StatementKind::Select(context.run(|context| self.select_statement(context)).await?)
		} else if let Some(feature) = self.unsupported_statement() {
			return Err(self.unsupported(feature, start));
		} else {
			return Err(self.syntax("Expected `LET`, `RETURN`, or `SELECT`.", start));
		};
		let end = self.previous_span();
		Ok(Statement {
			kind,
			span: cover(start, end),
		})
	}

	async fn let_statement(&mut self, context: &mut Stk) -> FerrosGrynnResult<LetStatement> {
		let name = self.parameter()?;
		let declared_type = if self.consume_symbol(Symbol::Colon).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		self.expect_symbol(Symbol::Equal, "Expected `=` in LET statement.")?;
		let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		Ok(LetStatement {
			name,
			declared_type,
			value,
		})
	}

	async fn return_statement(&mut self, context: &mut Stk) -> FerrosGrynnResult<ReturnStatement> {
		let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let alias = if self.consume_keyword("AS").is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		Ok(ReturnStatement {
			value,
			alias,
		})
	}

	async fn select_statement(&mut self, context: &mut Stk) -> FerrosGrynnResult<SelectStatement> {
		let only = self.consume_keyword("ONLY").is_some();
		let value = self.consume_keyword("VALUE").is_some();
		let mut projection = Vec::new();
		loop {
			if let Some(token) = self.consume_symbol(Symbol::Star) {
				projection.push(SelectItem::Wildcard(token.span));
			} else {
				let expr = context.run(|context| self.expression(context, 0, 1, 0)).await?;
				let alias = if self.consume_keyword("AS").is_some() {
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
		self.expect_keyword("FROM", "Expected `FROM` after SELECT projection.")?;
		let target = self.select_target()?;
		let filter = if self.consume_keyword("WHERE").is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let limit = if self.consume_keyword("LIMIT").is_some() {
			self.consume_keyword("BY");
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let start = if self.consume_keyword("START").is_some() {
			if self.consume_keyword("AFTER").is_some() {
				return Err(self.unsupported("START AFTER", self.previous_span()));
			}
			self.consume_keyword("AT");
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
			filter,
			limit,
			start,
		})
	}

	fn select_target(&mut self) -> FerrosGrynnResult<SelectTarget> {
		let table = self.identifier()?;
		if !self.at_symbol(Symbol::Colon)
			|| self.current().gap_before
			|| self.tokens.peek(1).gap_before
		{
			return Ok(SelectTarget::Table(table));
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
		let key_span = match &key {
			RecordKey::Integer(_, span) | RecordKey::String(_, span) => *span,
		};
		Ok(SelectTarget::Record {
			span: cover(table.span, key_span),
			table,
			key,
		})
	}

	async fn expression(
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

	async fn prefix(
		&mut self,
		context: &mut Stk,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let token = self.tokens.next();
		match token.kind {
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
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case("NONE") => Ok(Expr {
				kind: ExprKind::Literal(Literal::None),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case("NULL") => Ok(Expr {
				kind: ExprKind::Literal(Literal::Null),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case("TRUE") => Ok(Expr {
				kind: ExprKind::Literal(Literal::Bool(true)),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case("FALSE") => Ok(Expr {
				kind: ExprKind::Literal(Literal::Bool(false)),
				span: token.span,
			}),
			TokenKind::Word {
				value,
				quoted,
			} if !quoted && value.eq_ignore_ascii_case("NOT") => {
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
				self.unary(context, token.span, UnaryOperator::Assert, depth, object_depth).await
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

	async fn unary(
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

	async fn array(
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

	async fn object(
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

	async fn postfix(
		&mut self,
		context: &mut Stk,
		base: Expr,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Option<Expr>> {
		let null_safe = if self.consume_symbol(Symbol::QuestionDot).is_some() {
			true
		} else if self.consume_symbol(Symbol::Dot).is_some() {
			false
		} else {
			if let Some(open) = self.consume_symbol(Symbol::LeftBracket) {
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
			if let Some(open) = self.consume_symbol(Symbol::LeftParen) {
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
						arguments,
					},
				}));
			}
			return Ok(None);
		};
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

	fn binary_operator(&self) -> Option<(BinaryOperator, u8, u8, usize)> {
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
			Some(Symbol::Equal | Symbol::QuestionEqual | Symbol::StarEqual) => {
				(BinaryOperator::Equal, 9, 10, 1)
			}
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
			_ if self.at_keyword("OR") => (BinaryOperator::Or, 5, 6, 1),
			_ if self.at_keyword("AND") => (BinaryOperator::And, 7, 8, 1),
			_ if self.at_keyword("IS") => (BinaryOperator::Is, 9, 10, 1),
			_ if self.at_keyword("IN") => (BinaryOperator::In, 9, 10, 1),
			_ if self.at_keyword("NOT") && self.peek_keyword(1, "IS") => {
				(BinaryOperator::NotIs, 9, 10, 2)
			}
			_ if self.at_keyword("NOT") && self.peek_keyword(1, "IN") => {
				(BinaryOperator::NotIn, 9, 10, 2)
			}
			_ => return None,
		};
		Some(operator)
	}

	fn identifier(&mut self) -> FerrosGrynnResult<Identifier> {
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
			_ => Err(self.syntax("Expected an identifier.", token.span)),
		}
	}

	fn parameter(&mut self) -> FerrosGrynnResult<Identifier> {
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

	fn type_name(&mut self) -> FerrosGrynnResult<TypeName> {
		let identifier = self.identifier()?;
		Ok(TypeName {
			value: identifier.value,
			span: identifier.span,
		})
	}

	fn close(
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

	fn expect_symbol(&mut self, symbol: Symbol, message: &str) -> FerrosGrynnResult<Token> {
		self.consume_symbol(symbol).ok_or_else(|| self.syntax(message, self.current().span))
	}

	fn expect_keyword(&mut self, keyword: &str, message: &str) -> FerrosGrynnResult<Token> {
		self.consume_keyword(keyword).ok_or_else(|| self.syntax(message, self.current().span))
	}

	fn consume_symbol(&mut self, symbol: Symbol) -> Option<Token> {
		self.at_symbol(symbol).then(|| self.tokens.next())
	}

	fn consume_keyword(&mut self, keyword: &str) -> Option<Token> {
		self.at_keyword(keyword).then(|| self.tokens.next())
	}

	fn at_symbol(&self, symbol: Symbol) -> bool {
		matches!(self.current().kind, TokenKind::Symbol(current) if current == symbol)
	}

	fn at_keyword(&self, keyword: &str) -> bool {
		self.peek_keyword(0, keyword)
	}

	fn peek_keyword(&self, offset: usize, keyword: &str) -> bool {
		matches!(
			&self.tokens.peek(offset).kind,
			TokenKind::Word { value, quoted: false } if value.eq_ignore_ascii_case(keyword)
		)
	}

	fn at_eof(&self) -> bool {
		matches!(self.current().kind, TokenKind::Eof)
	}

	fn current(&self) -> &Token {
		self.tokens.peek(0)
	}

	fn previous_span(&self) -> TextSpan {
		self.tokens.tokens[self.tokens.cursor.saturating_sub(1)].span
	}

	fn ensure_depth(&self, depth: u32) -> FerrosGrynnResult<()> {
		if depth > self.settings.max_query_parsing_depth {
			Err(self.nesting_error(self.settings.max_query_parsing_depth, self.current().span))
		} else {
			Ok(())
		}
	}

	fn unsupported_statement(&self) -> Option<&'static str> {
		[
			"IF", "CASE", "FOR", "WHILE", "BREAK", "CONTINUE", "THROW", "TRY", "BEGIN", "COMMIT",
			"CANCEL", "EXPLAIN", "CREATE", "UPDATE", "UPSERT", "DELETE", "RELATE", "MATCH",
			"TRAVERSE", "BUILD", "DEFINE", "REMOVE", "INFO", "SHOW", "KILL", "REBUILD", "ALTER",
			"COMPACT", "LIVE",
		]
		.into_iter()
		.find(|keyword| self.at_keyword(keyword))
	}

	fn unsupported_select_clause(&self) -> Option<&'static str> {
		["WITH", "SPLIT", "GROUP", "ORDER", "FETCH", "OMIT", "TIMEOUT", "TEMPFILES"]
			.into_iter()
			.find(|keyword| self.at_keyword(keyword))
	}

	fn syntax(&self, message: &str, span: TextSpan) -> FerrosGrynnError {
		let error =
			FerrosGrynnError::query_parse_error(message).with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}

	fn unsupported(&self, feature: &str, span: TextSpan) -> FerrosGrynnError {
		let error = FerrosGrynnError::query_feature_not_supported(feature)
			.with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}

	fn nesting_error(&self, maximum_depth: u32, span: TextSpan) -> FerrosGrynnError {
		let error = FerrosGrynnError::query_nesting_too_deep(u64::from(maximum_depth))
			.with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}
}

#[cfg(test)]
mod tests {
	use super::parse;
	use crate::syn::{BinaryOperator, ExprKind, ParserSettings, StatementKind};

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
		assert!(matches!(statement.target, crate::syn::SelectTarget::Record { .. }));
	}

	#[test]
	fn reports_unsupported_statement_and_unclosed_delimiter() {
		let unsupported = parse("MATCH (n) RETURN n", ParserSettings::default())
			.expect_err("MATCH is not in the first slice");
		assert_eq!(unsupported.status().code().to_string(), "FG-50F03");

		let unclosed =
			parse("RETURN [1, 2", ParserSettings::default()).expect_err("array is not closed");
		assert_eq!(unclosed.status().code().to_string(), "FG-42001");
		assert_eq!(unclosed.status().diagnostic().related_locations().len(), 1);
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
}
