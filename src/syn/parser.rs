use reblessive::{Stack, Stk};

use super::{
	ParserSettings,
	ast::{
		BinaryOperator,
		ClosureBody,
		ClosureExpr,
		ClosureParameter,
		Expr,
		ExprKind,
		FulltextOperator,
		Identifier,
		KnnDistance,
		KnnSearchArgument,
		Literal,
		ObjectField,
		Program,
		TypeName,
		UnaryOperator,
		cover,
	},
	lexer::{keywords, lex},
	stmt::{RecordKey, Statement, StatementKind, TimeoutValue},
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

/// Parses the current state.
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
	// Reblessive drives recursive parsing without using the native call stack
	// for every level.
	let mut stack = Stack::new();
	stack.enter(|stk| parser.program(stk)).finish()
}

/// Token buffer state.
pub(in crate::syn) struct TokenBuffer<const LOOKAHEAD: usize> {
	/// Tokens associated with this token buffer.
	tokens: Vec<Token>,
	/// Cursor associated with this token buffer.
	cursor: usize,
}

impl<const LOOKAHEAD: usize> TokenBuffer<LOOKAHEAD> {
	/// Creates a new `TokenBuffer<LOOKAHEAD>` value.
	const fn new(tokens: Vec<Token>) -> Self {
		Self {
			tokens,
			cursor: 0,
		}
	}

	/// Borrows a lookahead token, clamping positions beyond input to EOF.
	pub(in crate::syn) fn peek(&self, offset: usize) -> &Token {
		debug_assert!(offset < LOOKAHEAD);
		let index = self.cursor.saturating_add(offset).min(self.tokens.len() - 1);
		&self.tokens[index]
	}

	/// Returns the current token and advances unless it is the end of input.
	pub(in crate::syn) fn next(&mut self) -> Token {
		let token = self.peek(0).clone();
		if !matches!(token.kind, TokenKind::Eof) {
			self.cursor += 1;
		}
		token
	}
}

/// Parser state.
pub(in crate::syn) struct Parser<'a> {
	/// Source associated with this parser.
	pub(in crate::syn) source: &'a str,
	/// Settings associated with this parser.
	pub(in crate::syn) settings: ParserSettings,
	/// Tokens associated with this parser.
	pub(in crate::syn) tokens: TokenBuffer<4>,
	/// In import associated with this parser.
	pub(in crate::syn) in_import: bool,
}

impl Parser<'_> {
	/// Parses the program syntax.
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

	/// Parses the statement syntax.
	pub(in crate::syn) async fn statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Statement> {
		let start = self.current().span;
		let mut kind = if let Some(kind) = self.query_statement_kind(context).await? {
			kind
		} else if let Some(kind) = self.control_statement_kind(context).await? {
			kind
		} else if let Some(kind) = self.catalog_statement_kind(context).await? {
			kind
		} else if let Some(kind) = self.mutation_statement_kind(context).await? {
			kind
		} else if let Some(kind) = self.graph_statement_kind(context).await? {
			kind
		} else if let Some(feature) = self.unsupported_statement() {
			return Err(self.unsupported(feature, start));
		} else {
			StatementKind::Expression(
				context.run(|context| self.expression(context, 0, 1, 0)).await?,
			)
		};
		let timeout = self.statement_timeout(&mut kind)?;
		let end = self.previous_span();
		Ok(Statement {
			kind,
			span: cover(start, end),
			timeout,
		})
	}

	/// Recognizes query or EXPLAIN statements, consuming nothing when none
	/// matches.
	///
	/// # Errors
	/// Returns the existing parsing error from the first recognized statement.
	async fn query_statement_kind(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<StatementKind>> {
		let kind = if self.consume_keyword(keywords::EXPLAIN).is_some() {
			StatementKind::Explain(context.run(|context| self.explain_statement(context)).await?)
		} else if self.consume_keyword(keywords::RETURN).is_some() {
			if self.consume_keyword(keywords::GRAPH).is_some() {
				StatementKind::ReturnGraph(context.run(|context| self.return_graph(context)).await?)
			} else {
				StatementKind::Return(context.run(|context| self.return_statement(context)).await?)
			}
		} else if self.consume_keyword(keywords::SELECT).is_some() {
			StatementKind::Select(context.run(|context| self.select_statement(context)).await?)
		} else {
			return Ok(None);
		};
		Ok(Some(kind))
	}

	/// Recognizes transaction and control-flow statements, consuming nothing
	/// when none matches.
	///
	/// # Errors
	/// Returns the existing parsing error from the first recognized statement.
	async fn control_statement_kind(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<StatementKind>> {
		let kind = if self.consume_keyword(keywords::BEGIN).is_some() {
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
		} else if self.consume_keyword(keywords::SLEEP).is_some() {
			StatementKind::Sleep(self.timeout_value()?)
		} else {
			return Ok(None);
		};
		Ok(Some(kind))
	}

	/// Recognizes catalog, maintenance and consumer commands, consuming nothing
	/// when none matches.
	///
	/// # Errors
	/// Returns the existing parsing error from the first recognized statement.
	async fn catalog_statement_kind(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<StatementKind>> {
		let kind = if self.consume_keyword(keywords::ASSERT).is_some() {
			StatementKind::AssertInvariants(self.assert_invariants_statement()?)
		} else if self.consume_keyword(keywords::ANALYZE).is_some() {
			StatementKind::Maintenance(self.analyze_statement()?)
		} else if self.consume_keyword(keywords::REBUILD).is_some() {
			StatementKind::Maintenance(self.rebuild_index_statement()?)
		} else if self.consume_keyword(keywords::COMPACT).is_some() {
			StatementKind::Maintenance(self.compact_statement()?)
		} else if self.at_consumer_control() {
			context.run(|context| self.consumer_control_statement(context)).await?
		} else if self.consume_keyword(keywords::BUILD).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `BUILD`.")?;
			StatementKind::BuildGraph(
				context.run(|context| self.build_graph_statement(context)).await?,
			)
		} else if self.consume_keyword(keywords::DEFINE).is_some() {
			context.run(|context| self.definition_statement(context)).await?
		} else {
			return Ok(None);
		};
		Ok(Some(kind))
	}

	/// Recognizes mutation and schema-alteration commands, consuming nothing
	/// when none matches.
	///
	/// # Errors
	/// Returns the existing parsing error from the first recognized statement.
	async fn mutation_statement_kind(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<StatementKind>> {
		let kind = if self.consume_keyword(keywords::CREATE).is_some() {
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
			if self.consume_keyword(keywords::DATABASE).is_some()
				|| self.consume_keyword(keywords::DB).is_some()
			{
				StatementKind::Maintenance(self.alter_database_compact_statement()?)
			} else {
				StatementKind::Alter(context.run(|context| self.alter_statement(context)).await?)
			}
		} else {
			return Ok(None);
		};
		Ok(Some(kind))
	}

	/// Recognizes inspection, binding and graph-query statements, consuming
	/// nothing when none matches.
	///
	/// # Errors
	/// Returns the existing parsing error from the first recognized statement.
	async fn graph_statement_kind(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Option<StatementKind>> {
		let kind = if self.consume_keyword(keywords::INFO).is_some() {
			StatementKind::Info(self.info_statement()?)
		} else if self.consume_keyword(keywords::SHOW).is_some() {
			StatementKind::ShowChanges(
				context.run(|context| self.show_changes_statement(context)).await?,
			)
		} else if self.at_binding_statement() {
			context.run(|context| self.binding_statement(context)).await?
		} else if self.consume_keyword(keywords::OPTIONAL).is_some() {
			self.expect_keyword(keywords::MATCH, "Expected `MATCH` after `OPTIONAL`.")?;
			StatementKind::Match(context.run(|context| self.match_statement(context, true)).await?)
		} else if self.consume_keyword(keywords::MATCH).is_some() {
			StatementKind::Match(context.run(|context| self.match_statement(context, false)).await?)
		} else if self.consume_keyword(keywords::TRAVERSE).is_some() {
			StatementKind::Traverse(context.run(|context| self.traverse_statement(context)).await?)
		} else {
			return Ok(None);
		};
		Ok(Some(kind))
	}

	/// Resolves embedded and trailing timeout clauses after the statement body.
	fn statement_timeout(
		&mut self,
		kind: &mut StatementKind,
	) -> FerrosGrynnResult<Option<TimeoutValue>> {
		let embedded_timeout = match kind {
			StatementKind::Explain(statement) => statement.statement.timeout.take(),
			StatementKind::Match(statement) => statement.timeout.take(),
			StatementKind::BuildGraph(statement) => statement.timeout.take(),
			StatementKind::Let(crate::syn::LetStatement {
				value: crate::syn::LetValue::BuildGraph(statement),
				..
			}) => statement.timeout.take(),
			_ => None,
		};
		Ok(if embedded_timeout.is_some() {
			embedded_timeout
		} else if self.consume_keyword(keywords::TIMEOUT).is_some() {
			if matches!(kind, StatementKind::Match(_)) {
				return Err(
					self.syntax("MATCH TIMEOUT belongs before RETURN.", self.previous_span())
				);
			}
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
					| StatementKind::Traverse(_)
					| StatementKind::ShowChanges(_)
			) {
				return Err(
					self.syntax("TIMEOUT is not valid for this statement.", self.previous_span())
				);
			}
			Some(self.timeout_value()?)
		} else {
			None
		})
	}

	/// Parses the timeout value syntax.
	pub(in crate::syn) fn timeout_value(&mut self) -> FerrosGrynnResult<TimeoutValue> {
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

	/// Parses the expression syntax.
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
			if minimum_binding_power <= 9
				&& let TokenKind::FullText(operator) = &self.current().kind
			{
				let operator = self.fulltext_operator(operator, self.current().span)?;
				self.tokens.next();
				let right = context
					.run(|context| self.expression(context, 10, depth + 1, object_depth))
					.await?;
				let span = cover(left.span, right.span);
				left = Expr {
					kind: ExprKind::Fulltext {
						left: Box::new(left),
						op: operator,
						right: Box::new(right),
					},
					span,
				};
				continue;
			}
			if minimum_binding_power <= 9 && self.at_symbol(Symbol::ReversePipeline) {
				left = self.knn_expression(context, left, depth, object_depth).await?;
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

	/// Parses the knn expression syntax.
	async fn knn_expression(
		&mut self,
		context: &mut Stk,
		left: Expr,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		self.tokens.next();
		let k = context.run(|context| self.expression(context, 2, depth + 1, object_depth)).await?;
		let search = if self.consume_symbol(Symbol::Comma).is_some() {
			if self.consume_keyword(keywords::EF).is_some() {
				Some(KnnSearchArgument::Ef(Box::new(
					context
						.run(|context| self.expression(context, 2, depth + 1, object_depth))
						.await?,
				)))
			} else {
				Some(KnnSearchArgument::Distance(
					self.knn_distance(context, depth, object_depth).await?,
				))
			}
		} else {
			None
		};
		self.expect_symbol(Symbol::Pipeline, "Expected `|>` after KNN search options.")?;
		let right =
			context.run(|context| self.expression(context, 10, depth + 1, object_depth)).await?;
		Ok(Expr {
			span: cover(left.span, right.span),
			kind: ExprKind::Knn {
				left: Box::new(left),
				k: Box::new(k),
				search,
				right: Box::new(right),
			},
		})
	}

	/// Parses the knn distance syntax.
	async fn knn_distance(
		&mut self,
		context: &mut Stk,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<KnnDistance> {
		for (keyword, distance) in [
			(keywords::EUCLIDEAN, KnnDistance::Euclidean),
			(keywords::COSINE, KnnDistance::Cosine),
			(keywords::MANHATTAN, KnnDistance::Manhattan),
			(keywords::INNER_PRODUCT, KnnDistance::InnerProduct),
			(keywords::COSINE_NORMALIZED, KnnDistance::CosineNormalized),
		] {
			if self.consume_keyword(keyword).is_some() {
				return Ok(distance);
			}
		}
		if let Some(start) = self.consume_keyword(keywords::MINKOWSKI) {
			let open = self.expect_symbol(Symbol::LeftParen, "Expected `(` after MINKOWSKI.")?;
			let exponent =
				context.run(|context| self.expression(context, 0, depth + 1, object_depth)).await?;
			self.close(Symbol::RightParen, open.span, "`)`")?;
			if exponent.span.offset < start.span.offset {
				return Err(self.syntax("Invalid MINKOWSKI exponent.", exponent.span));
			}
			return Ok(KnnDistance::Minkowski(Box::new(exponent)));
		}
		let distance =
			context.run(|context| self.expression(context, 2, depth + 1, object_depth)).await?;
		Ok(KnnDistance::Custom(Box::new(distance)))
	}

	/// Parses the prefix syntax.
	pub(in crate::syn) async fn prefix(
		&mut self,
		context: &mut Stk,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let token = self.tokens.next();
		match token.kind {
			TokenKind::Symbol(Symbol::Range) => {
				self.prefix_range(context, token.span, depth, object_depth).await
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
			} if !quoted && value.eq_ignore_ascii_case(keywords::MATCH) => {
				let statement = context.run(|context| self.match_statement(context, false)).await?;
				let end = self.previous_span();
				Ok(Expr {
					kind: ExprKind::MatchSubquery(Box::new(statement)),
					span: cover(token.span, end),
				})
			}
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
			} => {
				if self.at_symbol(Symbol::Colon)
					&& !self.current().gap_before
					&& !self.tokens.peek(1).gap_before
				{
					self.tokens.next();
					let key = self.tokens.next();
					let record_key = match key.kind {
						TokenKind::Number(value) => RecordKey::Integer(value, key.span),
						TokenKind::String(value)
						| TokenKind::Word {
							value,
							..
						} => RecordKey::String(value, key.span),
						_ => {
							return Err(self.syntax("Expected a record ID after `:`.", key.span));
						}
					};
					Ok(Expr {
						kind: ExprKind::Literal(Literal::Record {
							class: value,
							key: record_key,
						}),
						span: cover(token.span, key.span),
					})
				} else {
					Ok(Expr {
						kind: ExprKind::Name(Identifier {
							value,
							quoted,
							span: token.span,
						}),
						span: token.span,
					})
				}
			}
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
			TokenKind::Symbol(Symbol::Pipe) => {
				self.closure(context, token.span, depth, object_depth).await
			}
			_ => Err(self.syntax("Expected an expression.", token.span)),
		}
	}

	/// Parses an open-start range after its leading range token was consumed.
	///
	/// # Errors
	/// Returns the existing expression or depth-limit error for the range end.
	async fn prefix_range(
		&mut self,
		context: &mut Stk,
		span: TextSpan,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
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
		let end_span = end.as_ref().map_or(span, |end| end.span);
		Ok(Expr {
			kind: ExprKind::Range {
				start: None,
				end,
				inclusive_end,
			},
			span: cover(span, end_span),
		})
	}

	/// Parses the closure syntax.
	async fn closure(
		&mut self,
		context: &mut Stk,
		start: TextSpan,
		depth: u32,
		object_depth: u32,
	) -> FerrosGrynnResult<Expr> {
		let mut parameters = Vec::new();
		if !self.at_symbol(Symbol::Pipe) {
			loop {
				let name = match self.current().kind {
					TokenKind::Parameter(_) => self.parameter()?,
					_ => self.identifier()?,
				};
				let declared_type = if self.consume_symbol(Symbol::Colon).is_some() {
					Some(self.type_name()?)
				} else {
					None
				};
				parameters.push(ClosureParameter {
					name,
					declared_type,
				});
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
		}
		self.expect_symbol(Symbol::Pipe, "Expected `|` after closure parameters.")?;
		let result_type = if self.consume_symbol(Symbol::ArrowRight).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		let body = if self.at_symbol(Symbol::LeftBrace) {
			ClosureBody::Block(context.run(|context| self.block(context)).await?)
		} else {
			// Pipeline has lower binding power and therefore remains outside
			// the closure body.
			ClosureBody::Expression(Box::new(
				context.run(|context| self.expression(context, 2, depth + 1, object_depth)).await?,
			))
		};
		let end = match &body {
			ClosureBody::Expression(body) => body.span,
			ClosureBody::Block(body) => body.span,
		};
		Ok(Expr {
			kind: ExprKind::Closure(ClosureExpr {
				parameters,
				result_type,
				body,
			}),
			span: cover(start, end),
		})
	}

	/// Parses the unary syntax.
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

	/// Parses the array syntax.
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

	/// Parses the object syntax.
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

	/// Parses the postfix syntax.
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
				let mut argument_names = Vec::new();
				let mut arguments = Vec::new();
				let mut saw_named_argument = false;
				if !self.at_symbol(Symbol::RightParen) {
					loop {
						let argument_name = if matches!(self.current().kind, TokenKind::Word { .. })
							&& matches!(self.tokens.peek(1).kind, TokenKind::Symbol(Symbol::Equal))
						{
							let name = self.identifier()?;
							self.tokens.next();
							if argument_names.iter().flatten().any(|existing: &Identifier| {
								existing.value.eq_ignore_ascii_case(name.value.as_ref())
							}) {
								return Err(self.syntax("Duplicate named argument.", name.span));
							}
							saw_named_argument = true;
							Some(name)
						} else {
							if saw_named_argument {
								return Err(self.syntax(
									"Positional arguments must precede named arguments.",
									self.current().span,
								));
							}
							None
						};
						argument_names.push(argument_name);
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
						argument_names,
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

	/// Parses the binary operator syntax.
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
			_ if self.at_keyword(keywords::INSIDE) => (BinaryOperator::Inside, 11, 12, 1),
			_ if self.at_keyword(keywords::NOTINSIDE) => (BinaryOperator::NotInside, 11, 12, 1),
			_ if self.at_keyword(keywords::ALLINSIDE) => (BinaryOperator::AllInside, 11, 12, 1),
			_ if self.at_keyword(keywords::ANYINSIDE) => (BinaryOperator::AnyInside, 11, 12, 1),
			_ if self.at_keyword(keywords::NONEINSIDE) => (BinaryOperator::NoneInside, 11, 12, 1),
			_ if self.at_keyword(keywords::OUTSIDE) => (BinaryOperator::Outside, 11, 12, 1),
			_ if self.at_keyword(keywords::INTERSECTS) => (BinaryOperator::Intersects, 11, 12, 1),
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

	/// Parses the fulltext operator syntax.
	fn fulltext_operator(
		&self,
		operator: &str,
		span: TextSpan,
	) -> FerrosGrynnResult<FulltextOperator> {
		if operator == "@@" {
			return Ok(FulltextOperator::Any);
		}
		if operator.eq_ignore_ascii_case("@AND@") {
			return Ok(FulltextOperator::All);
		}
		if operator.eq_ignore_ascii_case("@OR@") {
			return Ok(FulltextOperator::AnyOf);
		}
		let slot = operator
			.strip_prefix('@')
			.and_then(|value| value.strip_suffix('@'))
			.and_then(|value| value.parse::<u16>().ok())
			.filter(|value| *value != 0)
			.ok_or_else(|| {
				self.syntax("Fulltext match slots must be integers in 1..=65535.", span)
			})?;
		Ok(FulltextOperator::Slot(slot))
	}

	/// Parses the identifier syntax.
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

	/// Parses the parameter syntax.
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

	/// Parses the type name syntax.
	pub(in crate::syn) fn type_name(&mut self) -> FerrosGrynnResult<TypeName> {
		let identifier = self.identifier()?;
		let mut value = identifier.value;
		let mut span = identifier.span;
		if let Some(open) = self.consume_symbol(Symbol::Less) {
			let item = self.type_name()?;
			let close = self.close(Symbol::Greater, open.span, "`>`")?;
			value = format!("{value}<{}>", item.value).into_boxed_str();
			span = cover(span, close.span);
		}
		Ok(TypeName {
			value,
			span,
		})
	}

	/// Closes parser<' >.
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

	/// Parses the expect symbol syntax.
	pub(in crate::syn) fn expect_symbol(
		&mut self,
		symbol: Symbol,
		message: &str,
	) -> FerrosGrynnResult<Token> {
		self.consume_symbol(symbol).ok_or_else(|| self.syntax(message, self.current().span))
	}

	/// Parses the expect keyword syntax.
	pub(in crate::syn) fn expect_keyword(
		&mut self,
		keyword: &str,
		message: &str,
	) -> FerrosGrynnResult<Token> {
		self.consume_keyword(keyword).ok_or_else(|| self.syntax(message, self.current().span))
	}

	/// Parses the consume symbol syntax.
	pub(in crate::syn) fn consume_symbol(&mut self, symbol: Symbol) -> Option<Token> {
		self.at_symbol(symbol).then(|| self.tokens.next())
	}

	/// Parses the consume keyword syntax.
	pub(in crate::syn) fn consume_keyword(&mut self, keyword: &str) -> Option<Token> {
		self.at_keyword(keyword).then(|| self.tokens.next())
	}

	/// Returns whether the at symbol condition holds.
	pub(in crate::syn) fn at_symbol(&self, symbol: Symbol) -> bool {
		matches!(self.current().kind, TokenKind::Symbol(current) if current == symbol)
	}

	/// Returns whether the at keyword condition holds.
	pub(in crate::syn) fn at_keyword(&self, keyword: &str) -> bool {
		self.peek_keyword(0, keyword)
	}

	/// Returns whether the peek keyword condition holds.
	pub(in crate::syn) fn peek_keyword(&self, offset: usize, keyword: &str) -> bool {
		matches!(
			&self.tokens.peek(offset).kind,
			TokenKind::Word { value, quoted: false } if value.eq_ignore_ascii_case(keyword)
		)
	}

	/// Returns whether the at eof condition holds.
	pub(in crate::syn) fn at_eof(&self) -> bool {
		matches!(self.current().kind, TokenKind::Eof)
	}

	/// Returns whether the expression terminator condition holds.
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

	/// Parses the current syntax.
	pub(in crate::syn) fn current(&self) -> &Token {
		self.tokens.peek(0)
	}

	/// Parses the previous span syntax.
	pub(in crate::syn) fn previous_span(&self) -> TextSpan {
		self.tokens.tokens[self.tokens.cursor.saturating_sub(1)].span
	}

	/// Ensures depth.
	pub(in crate::syn) fn ensure_depth(&self, depth: u32) -> FerrosGrynnResult<()> {
		if depth > self.settings.max_query_parsing_depth {
			Err(self.nesting_error(self.settings.max_query_parsing_depth, self.current().span))
		} else {
			Ok(())
		}
	}

	/// Parses the unsupported statement syntax.
	pub(in crate::syn) fn unsupported_statement(&self) -> Option<&'static str> {
		keywords::UNSUPPORTED_STATEMENTS.iter().copied().find(|keyword| self.at_keyword(keyword))
	}

	/// Parses the syntax syntax.
	pub(in crate::syn) fn syntax(&self, message: &str, span: TextSpan) -> FerrosGrynnError {
		let error =
			FerrosGrynnError::query_parse_error(message).with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}

	/// Parses the unsupported syntax.
	pub(in crate::syn) fn unsupported(&self, feature: &str, span: TextSpan) -> FerrosGrynnError {
		let error = FerrosGrynnError::query_feature_not_supported(feature)
			.with_phase(ExecutionPhase::Parsing);
		Location::from_source(None, self.source, span)
			.map_or(error.clone(), |location| error.with_location(location))
	}

	/// Parses the nesting error syntax.
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
/// Regression tests for parser behavior.
mod tests {
	use super::parse;
	#[cfg(test)]
	use crate::test_assertions::{AssertOption as _, AssertResult as _};
	use crate::{
		gql_status::{FerrosGrynnError, FerrosGrynnResult},
		syn::{
			BasicIndexDefinition,
			BinaryOperator,
			ExecutionModeName,
			ExprKind,
			FulltextOperator,
			KnnDistance,
			KnnSearchArgument,
			MatchDirection,
			MatchLinearPattern,
			MatchPattern,
			MatchSelector,
			ParserSettings,
			StatementKind,
			TimeoutValue,
			TransactionIsolationName,
		},
	};

	/// Reads the linear pattern syntax from the parser input.
	fn linear_pattern(pattern: &MatchPattern) -> &MatchLinearPattern {
		match pattern {
			MatchPattern::Linear(pattern) => Some(pattern),
			_ => None,
		}
		.assert_some_with("expected a linear MATCH pattern")
	}

	/// Verifies respects precedence and record adjacency.
	#[test]
	fn respects_precedence_and_record_adjacency() {
		let program = parse(
			"RETURN 1 + 2 * 3; SELECT VALUE name FROM person:alice",
			ParserSettings::default(),
		)
		.assert_ok_with("valid query");
		let statement = match &program.statements[0].kind {
			StatementKind::Return(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected RETURN");
		let right = match &statement.value.kind {
			ExprKind::Binary {
				op: BinaryOperator::Add,
				right,
				..
			} => Some(right),
			_ => None,
		}
		.assert_some_with("expected addition");
		assert!(matches!(right.kind, ExprKind::Binary {
			op: BinaryOperator::Multiply,
			..
		}));
		let statement = match &program.statements[1].kind {
			StatementKind::Select(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected SELECT");
		assert!(matches!(
			statement.source.kind,
			crate::syn::SelectSourceKind::Atoms(crate::syn::SelectTarget::Record { .. })
		));
	}

	/// Verifies changefeed clauses and history reads have checked statement
	/// boundaries.
	#[test]
	fn changefeed_clauses_and_history_reads_have_checked_statement_boundaries() {
		use crate::test_assertions::AssertResult as _;

		let program = parse(
			"DEFINE CLASS item TYPE NODE CHANGEFEED 7d INCLUDE ORIGINAL FULL; DEFINE GRAPH scope \
			 TYPE GRAPH CHANGEFEED 1h FULL; ALTER CLASS item DROP CHANGEFEED; ALTER GRAPH scope \
			 CHANGEFEED 2d; SHOW CHANGES FOR GRAPH $graph SINCE $cursor FULL LIMIT 2; INFO FOR \
			 CHANGEFEED $id;",
			ParserSettings::default(),
		)
		.assert_ok();
		assert_eq!(program.statements.len(), 6);
		assert!(matches!(program.statements[4].kind, StatementKind::ShowChanges(_)));
		for query in [
			"DEFINE CLASS item TYPE NODE CHANGEFEED $ttl;",
			"DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL FULL;",
			"DEFINE GRAPH scope TYPE GRAPH CHANGEFEED 7d CHANGEFEED 1d;",
			"ALTER GRAPH scope CHANGEFEED 1d INCLUDE ORIGINAL INCLUDE ORIGINAL;",
			"SHOW CHANGES FOR CLASS item;",
			"SHOW CHANGES FOR CLASS item SINCE 0 FULL FULL;",
			"SHOW CHANGES FOR CLASS item SINCE 0 LIMIT 1 FULL;",
			"SHOW CHANGES FOR CLASS item SINCE 0 LIMIT;",
		] {
			assert!(parse(query, ParserSettings::default()).is_err(), "{query}");
		}
	}

	/// Verifies reports unsupported statement and unclosed delimiter.
	#[test]
	fn reports_unsupported_statement_and_unclosed_delimiter() {
		let transaction = parse("BEGIN; COMMIT;", ParserSettings::default())
			.assert_ok_with("transaction statements belong to the executable stage-11 profile");
		assert!(matches!(transaction.statements[0].kind, StatementKind::Begin(_)));
		assert!(matches!(transaction.statements[1].kind, StatementKind::Commit(_)));
		let relate = parse(
			"RELATE (person:alice)-[friendship:known LABEL Friend]->(person:bob) IN GRAPH social",
			ParserSettings::default(),
		)
		.assert_ok_with("RELATE has an exact edge identity contract");
		assert!(matches!(relate.statements[0].kind, StatementKind::Relate(_)));

		let unclosed =
			parse("RETURN [1, 2", ParserSettings::default()).assert_err_with("array is not closed");
		assert_eq!(unclosed.status().code().to_string(), "FG-42001");
		assert_eq!(unclosed.status().diagnostic().related_locations().len(), 1);
	}

	/// Verifies parses transaction options and statement timeout.
	#[test]
	fn parses_transaction_options_and_statement_timeout() {
		let program = parse(
			"BEGIN ISOLATION LEVEL SNAPSHOT READ ONLY EXECUTION MODE BEST_EFFORT; SELECT * FROM \
			 person TIMEOUT 25ms; COMMIT WITH VALIDATION;",
			ParserSettings::default(),
		)
		.assert_ok_with("stage-11 transaction syntax parses");
		let begin = match &program.statements[0].kind {
			StatementKind::Begin(begin) => Some(begin),
			_ => None,
		}
		.assert_some_with("expected BEGIN");
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
		let commit = match &program.statements[2].kind {
			StatementKind::Commit(commit) => Some(commit),
			_ => None,
		}
		.assert_some_with("expected COMMIT");
		assert!(commit.full_validation);
	}

	/// Verifies parses control flow and assertion surface.
	#[test]
	fn parses_control_flow_and_assertion_surface() {
		let program = parse(
			"IF true { 1; } ELSE { 2; }; FOR $x IN 1..=2 { $x; }; WHILE (false) { BREAK; }; TRY { \
			 THROW 'boom'; } CATCH |$error| { $error.code; }; ASSERT INVARIANTS FOR GRAPH social \
			 REPORT 3;",
			ParserSettings::default(),
		)
		.assert_ok_with("stage-12 control syntax parses");
		assert!(matches!(program.statements[0].kind, StatementKind::If(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::For(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::While(_)));
		assert!(matches!(program.statements[3].kind, StatementKind::Try(_)));
		let assertion = match &program.statements[4].kind {
			StatementKind::AssertInvariants(assertion) => Some(assertion),
			_ => None,
		}
		.assert_some_with("expected ASSERT INVARIANTS");
		assert_eq!(assertion.graph.as_ref().map(|graph| graph.value.as_ref()), Some("social"));
		assert_eq!(assertion.report, Some(3));
	}

	/// Verifies parses core dml and rejects unbounded upsert.
	#[test]
	fn parses_core_dml_and_rejects_unbounded_upsert() {
		let program = parse(
			"CREATE person:alice CONTENT {name: 'Alice'} RETURN *; UPDATE person SET active = \
			 true WHERE age >= 18 RETURN DIFF EXPECT active = false; DELETE person:alice RETURN \
			 BEFORE EXPECT EXISTS",
			ParserSettings::default(),
		)
		.assert_ok_with("core DML parses");
		assert!(matches!(program.statements[0].kind, StatementKind::Create(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::Update(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::Delete(_)));

		let error = parse("UPSERT person SET active = true", ParserSettings::default())
			.assert_err_with("UPSERT must be bounded by ID or WHERE");
		assert_eq!(error.status().code().to_string(), "FG-42001");
		for source in [
			"CREATE person:alice MERGE {name: 'Alice'}",
			"CREATE person:alice RETURN BEFORE",
			"DELETE person:alice RETURN VALUE name",
		] {
			let error = parse(source, ParserSettings::default())
				.assert_err_with("statement-specific DML grammar must be enforced");
			assert_eq!(error.status().code().to_string(), "FG-42001", "{source}");
		}
	}

	/// Verifies parses keep levelpath only on graph local mutations.
	#[test]
	fn parses_keep_levelpath_only_on_graph_local_mutations() {
		use crate::test_assertions::AssertResult as _;

		let program = parse(
			"ATTACH person:alice TO GRAPH social KEEP LEVELPATH; UPDATE person:alice IN GRAPH \
			 social KEEP LEVELPATH; RELATE (person:alice)-[friendship:known AS \
			 edge]->(person:bob) IN GRAPH social KEEP LEVELPATH FOR [edge]",
			ParserSettings::default(),
		)
		.assert_ok();
		let StatementKind::Attach(attach) = &program.statements[0].kind else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("expected ATTACH"));
				test_result.assert_ok();
			};
		};
		assert!(attach.keep_levelpath.is_some());
		let StatementKind::Update(update) = &program.statements[1].kind else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("expected UPDATE"));
				test_result.assert_ok();
			};
		};
		assert!(update.keep_levelpath.is_some());
		let StatementKind::Relate(relate) = &program.statements[2].kind else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("expected RELATE"));
				test_result.assert_ok();
			};
		};
		assert_eq!(
			relate
				.keep_levelpath
				.as_ref()
				.and_then(|keep| keep.aliases.first())
				.map(|alias| alias.value.as_ref()),
			Some("edge"),
		);
		assert!(
			parse(
				"MATCH (* AS a) IN GRAPH social KEEP LEVELPATH RETURN a",
				ParserSettings::default()
			)
			.is_err()
		);
		assert!(
			parse("TRAVERSE FROM $values |> tv::fold() KEEP LEVELPATH", ParserSettings::default())
				.is_err()
		);
	}

	/// Verifies parses atomic import as the only top level statement.
	#[test]
	fn parses_atomic_import_as_the_only_top_level_statement() {
		let program = parse(
			"BEGIN IMPORT INTO GRAPH social WITH (INDEXES = DEFERRED, CONSTRAINTS = IMMEDIATE, ON \
			 VIOLATION = SKIP, REJECT LIMIT = 2, SEGMENT SIZE = 1, EVENTS = OFF, CHANGEFEED = \
			 OFF); ATTACH person:alice LABEL Person; UPDATE person:alice LABEL Friend; RELATE \
			 (person:alice)-[friendship:known]->(person:bob); END IMPORT WITH VALIDATION;",
			ParserSettings::default(),
		)
		.assert_ok_with("stage-10 import syntax parses");
		let statement = match &program.statements[0].kind {
			StatementKind::Import(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected BEGIN IMPORT");
		assert_eq!(program.statements.len(), 1);
		assert_eq!(statement.statements.len(), 3);
		assert_eq!(statement.options.len(), 7);
		assert!(statement.full_validation);
		let attach = match &statement.statements[0].kind {
			StatementKind::Attach(attach) => Some(attach),
			_ => None,
		}
		.assert_some_with("expected ATTACH");
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

	/// Verifies enforces query depth.
	#[test]
	fn enforces_query_depth() {
		let settings = ParserSettings {
			max_query_parsing_depth: 2,
			..ParserSettings::default()
		};
		let error = parse("RETURN (((1)))", settings).assert_err_with("depth is limited");
		assert_eq!(error.status().code().to_string(), "FG-54F06");
	}

	/// Verifies distinguishes spaced record separator.
	#[test]
	fn distinguishes_spaced_record_separator() {
		let error = parse("SELECT VALUE name FROM person :alice", ParserSettings::default())
			.assert_err_with("a spaced colon is not a record ID");
		assert_eq!(error.status().code().to_string(), "FG-42001");
	}

	/// Verifies parses standard atomwalk chains and ranges.
	#[test]
	fn parses_standard_atomwalk_chains_and_ranges() {
		let standard = parse(
			"MATCH (Человек AS a LABEL Person)-[ЗНАЕТ AS e LABEL Friend]->(* AS b)<-[LIKES]-(* AS \
			 c), path = (* AS c)-[NEXT*0..3]->(* AS d), (* AS u)->(* AS v)<-(* AS w)-(* AS \
			 x)<->(* AS y), (* AS p)-[E*2]->(* AS q), (* AS r)-[E*..4]->(* AS s) IN GRAPH social \
			 RETURN a, path",
			ParserSettings::default(),
		)
		.assert_ok_with("valid StandardPath MATCH");
		let statement = match &standard.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		let patterns = statement.patterns.iter().map(linear_pattern).collect::<Vec<_>>();
		assert_eq!(statement.patterns.len(), 5);
		assert_eq!(patterns[0].steps.len(), 2);
		assert!(matches!(patterns[0].start.selector, MatchSelector::Class(_)));
		assert_eq!(patterns[0].start.alias.as_ref().map(|alias| alias.value.as_ref()), Some("a"));
		assert_eq!(
			patterns[0].start.label.as_ref().map(|label| label.value.as_ref()),
			Some("Person")
		);
		assert_eq!(patterns[0].steps[0].direction, MatchDirection::Forward);
		assert_eq!(patterns[0].steps[1].direction, MatchDirection::Backward);
		assert_eq!(
			patterns[1].steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((0, Some(3)))
		);
		assert_eq!(patterns[2].steps.iter().map(|step| step.direction).collect::<Vec<_>>(), [
			MatchDirection::Forward,
			MatchDirection::Backward,
			MatchDirection::Undirected,
			MatchDirection::Bidirectional,
		]);
		assert_eq!(
			patterns[3].steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((2, Some(2)))
		);
		assert_eq!(
			patterns[4].steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((1, Some(4)))
		);

		let atom = parse(
			"MATCH ATOM <(Person AS v)>~[ANNOTATES AS link]~><[FRIEND AS e]><~[BACK AS \
			 back]~<(Person AS w)> ~[UNDIRECTED AS u]~<(Person AS x)><~[BOTH AS bi]~><(Person AS \
			 y)> IN GRAPH social RETURN v, e, w",
			ParserSettings::default(),
		)
		.assert_ok_with("valid AtomWalk MATCH");
		let statement = match &atom.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH ATOM");
		assert!(statement.atom);
		let pattern = linear_pattern(&statement.patterns[0]);
		assert_eq!(pattern.steps.iter().map(|step| step.direction).collect::<Vec<_>>(), [
			MatchDirection::Forward,
			MatchDirection::Backward,
			MatchDirection::Undirected,
			MatchDirection::Bidirectional,
		]);
	}

	/// Verifies parses open match range for maxdepth normalization.
	#[test]
	fn parses_open_match_range_for_maxdepth_normalization() {
		let source = "MATCH (* AS a)-[E*1..]->(* AS b) IN GRAPH social RETURN b";
		let program =
			parse(source, ParserSettings::default()).assert_ok_with("open path range is canonical");
		let statement = match &program.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		let pattern = linear_pattern(&statement.patterns[0]);
		assert_eq!(
			pattern.steps[0].edge.length.map(|length| (length.min, length.max)),
			Some((1, None))
		);
	}

	/// Verifies parses global match cost before predicates and preserves
	/// unicode span.
	#[test]
	fn parses_global_match_cost_before_predicates_and_preserves_unicode_span() {
		let source = "MATCH path = (person AS a)-[link*1..3 WEIGHTED SHORTEST]->(person AS b) IN \
		              GRAPH routes WITH COST fn::`тариф` WHERE b.active RETURN path;";
		let program = parse(source, ParserSettings::default()).assert_ok();
		let statement = match &program.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		let function = statement.custom_cost.as_ref().assert_some();
		assert_eq!(function.name.value.as_ref(), "тариф");
		assert!(source.is_char_boundary(function.name.span.offset as usize));
		assert!(
			source.is_char_boundary((function.name.span.offset + function.name.span.len) as usize)
		);
		assert_eq!(
			&source[function.name.span.offset as usize
				..(function.name.span.offset + function.name.span.len) as usize],
			"`тариф`"
		);
		assert!(statement.filter.is_some());
		assert!(linear_pattern(&statement.patterns[0]).steps[0].edge.custom_cost.is_none());
		for invalid in [
			"MATCH (person)-[link]->(person) IN GRAPH routes WITH COST RETURN 1;",
			"MATCH (person)-[link]->(person) IN GRAPH routes WITH COST fn::fee WITH COST \
			 fn::other RETURN 1;",
			"MATCH (person)-[link]->(person) IN GRAPH routes WITH fn::fee RETURN 1;",
			"MATCH (person)-[link]->(person) IN GRAPH routes WHERE true WITH COST fn::fee RETURN \
			 1;",
		] {
			assert!(parse(invalid, ParserSettings::default()).is_err(), "{invalid}");
		}
	}

	/// Verifies parses global traverse cost and unicode span.
	#[test]
	fn parses_global_traverse_cost_and_unicode_span() {
		let source = "TRAVERSE ATOM FROM [person:a] IN GRAPH routes WITH COST fn::`тариф` |> \
		              tv::weightedShortestPath(person:b);";
		let program = parse(source, ParserSettings::default()).assert_ok();
		let statement = match &program.statements[0].kind {
			StatementKind::Traverse(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected TRAVERSE");
		let function = statement.custom_cost.as_ref().assert_some();
		assert_eq!(function.name.value.as_ref(), "тариф");
		assert_eq!(
			&source[function.name.span.offset as usize
				..(function.name.span.offset + function.name.span.len) as usize],
			"`тариф`"
		);
		for invalid in [
			"TRAVERSE FROM [] WITH COST |> tv::limit(1);",
			"TRAVERSE FROM [] WITH fn::cost |> tv::limit(1);",
			"TRAVERSE FROM [] WITH COST fn::a WITH COST fn::b |> tv::limit(1);",
			"TRAVERSE FROM [] |> tv::limit(1) WITH COST fn::a;",
		] {
			assert!(parse(invalid, ParserSettings::default()).is_err(), "{invalid}");
		}
	}

	/// Verifies parses full match and graph first surface.
	#[test]
	fn parses_full_match_and_graph_first_surface() {
		let standard = parse(
			"OPTIONAL MATCH DISTINCT DIFFERENT ATOMS CONSTRAINTS <TRAIL SHORTEST WEIGHTED> \
			 (person AS p { active: true } WHERE p.active)-[friend*1.. SIMPLE]->(person AS q) IN \
			 GRAPH $graph WHERE q.active MAXDEPTH 8 TIMEOUT 1s RETURN DISTINCT p AS person ORDER \
			 BY person DESC NULLS FIRST",
			ParserSettings::default(),
		)
		.assert_ok_with("full StandardPath surface parses");
		let statement = match &standard.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		assert!(statement.optional);
		assert!(statement.distinctness.bindings);
		assert!(statement.distinctness.projection);
		assert_eq!(statement.order.len(), 1);
		assert!(matches!(statement.source.kind, ExprKind::Parameter(_)));

		let hyper = parse(
			"MATCH HYPER [meeting AS e] CONTAINS HEAD [(person AS a), (person AS b)], TAIL \
			 [(document AS d)] IN GRAPH knowledge RETURN e, a, b, d",
			ParserSettings::default(),
		)
		.assert_ok_with("hyper endpoint assignments parse");
		let statement = match &hyper.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected HYPER MATCH");
		assert!(matches!(statement.patterns[0], MatchPattern::Hyper(_)));

		let within = parse(
			"MATCH (child AS c) WITHIN*[1..4] <(module AS m)> IN GRAPH knowledge RETURN c, m",
			ParserSettings::default(),
		)
		.assert_ok_with("transitive containment parses");
		let statement = match &within.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected WITHIN MATCH");
		assert!(matches!(statement.patterns[0], MatchPattern::Within(_)));

		let returned = parse(
			"MATCH (* AS a) IN GRAPH social RETURN GRAPH <DAG SIMPLE> { nodes: a, edges: [] }",
			ParserSettings::default(),
		)
		.assert_ok_with("terminal RETURN GRAPH parses");
		let statement = match &returned.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		assert!(statement.return_graph.is_some());

		let built = parse(
			"BUILD GRAPH OVERWRITE <ROOTED_TREE BIPARTITE> social FROM (SELECT VALUE id FROM \
			 person) ROOT person:root LIMIT NODES 10 LIMIT EDGES 9",
			ParserSettings::default(),
		)
		.assert_ok_with("named BUILD GRAPH parses");
		assert!(matches!(built.statements[0].kind, StatementKind::BuildGraph(_)));
		parse("BUILD GRAPH <GRAPH DAG> FROM []", ParserSettings::default())
			.assert_err_with("one graph contract cannot contain two families");
		parse("BUILD GRAPH <SIMPLE SIMPLE> FROM []", ParserSettings::default())
			.assert_err_with("one graph contract cannot repeat a validator");

		parse(
			"MATCH CONSTRAINTS <TRAIL WALK> (* AS a)-[*]->(* AS b) IN GRAPH social RETURN b",
			ParserSettings::default(),
		)
		.assert_err_with("one constraint scope cannot contain two path modes");
		parse(
			"MATCH CONSTRAINTS <SHORTEST WEIGHTED SHORTEST> (* AS a)-[*]->(* AS b) IN GRAPH \
			 social RETURN b",
			ParserSettings::default(),
		)
		.assert_err_with("one constraint scope cannot contain two objectives");
	}

	/// Verifies atomwalk path bounds share grammar and preserve unicode spans.
	#[test]
	fn atomwalk_path_bounds_share_grammar_and_preserve_unicode_spans() {
		/// Unicode AtomWalk fixture used to check shared path grammar and UTF-8
		/// byte spans.
		const SOURCE: &str = "MATCH ATOM путь = <(person AS а)>~[link*0..3 TRAIL \
		                      SHORTEST]~><[link AS е]> IN GRAPH routes RETURN путь;";
		let bounded = parse(SOURCE, ParserSettings::default())
			.assert_ok_with("bounded AtomWalk uses the shared path grammar");
		let statement = match &bounded.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		let step = &linear_pattern(&statement.patterns[0]).steps[0];
		assert_eq!(step.edge.length.assert_some().min, 0);
		assert_eq!(step.edge.length.assert_some().max, Some(3));
		assert!(SOURCE.is_char_boundary(step.edge.span.offset as usize));
		assert!(SOURCE.is_char_boundary((step.edge.span.offset + step.edge.span.len) as usize));
		for query in [
			"MATCH ATOM <(person)>~[link*1..4294967296]~><(person)> IN GRAPH routes RETURN 1;",
			"MATCH ATOM <(person)>~[link*-1]~><(person)> IN GRAPH routes RETURN 1;",
		] {
			assert!(parse(query, ParserSettings::default()).is_err());
		}
	}

	/// Verifies rejects legacy match source and treats colon as record id.
	#[test]
	fn rejects_legacy_match_source_and_treats_colon_as_record_id() {
		let program =
			parse("MATCH (person:alice) IN GRAPH social RETURN person", ParserSettings::default())
				.assert_ok_with("adjacent colon is the normative exact RecordId selector");
		let statement = match &program.statements[0].kind {
			StatementKind::Match(statement) => Some(statement),
			_ => None,
		}
		.assert_some_with("expected MATCH");
		let pattern = linear_pattern(&statement.patterns[0]);
		assert!(matches!(pattern.start.selector, MatchSelector::Record { .. }));
		assert!(pattern.start.alias.is_none());

		parse("MATCH (Person AS a) FROM social RETURN a", ParserSettings::default())
			.assert_err_with("legacy FROM source syntax must not remain available");
	}

	/// Verifies parses class schema statements and requires explicit index
	/// scope.
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
		.assert_ok_with("core schema syntax");
		assert_eq!(program.statements.len(), 14);
		assert!(matches!(program.statements[0].kind, StatementKind::DefineGraph(_)));
		assert!(matches!(program.statements[3].kind, StatementKind::DefineIndex(_)));
		assert!(matches!(program.statements[6].kind, StatementKind::Alter(_)));
		assert!(matches!(program.statements[9].kind, StatementKind::Info(_)));
		assert!(matches!(program.statements[13].kind, StatementKind::Remove(_)));

		let specialized = parse(
			"DEFINE INDEX title_search ON CLASS user FIELDS title FULLTEXT ANALYZER english \
			 BM25(1.4, 0.6) HIGHLIGHTS SCOPE BASE DEFER; DEFINE INDEX embedding ON CLASS user \
			 FIELDS vector HNSW DIMENSION 128 TYPE F32 DIST MINKOWSKI(3) EFC 180 M 16 SCOPE BASE; \
			 DEFINE INDEX disk_embedding ON CLASS user FIELDS compact DISKANN DIMENSION 768 TYPE \
			 F16 DIST COSINE_NORMALIZED DEGREE 48 L_BUILD 120 ALPHA 1.3 HASHED_VECTOR SCOPE BASE \
			 CONCURRENTLY",
			ParserSettings::default(),
		)
		.assert_ok_with("specialized index definitions");
		let fulltext = match &specialized.statements[0].kind {
			StatementKind::DefineIndex(fulltext) => Some(fulltext),
			_ => None,
		}
		.assert_some_with("expected Fulltext definition");
		assert!(matches!(fulltext.definition, BasicIndexDefinition::Fulltext { .. }));
		let hnsw = match &specialized.statements[1].kind {
			StatementKind::DefineIndex(hnsw) => Some(hnsw),
			_ => None,
		}
		.assert_some_with("expected HNSW definition");
		assert!(matches!(hnsw.definition, BasicIndexDefinition::Hnsw { .. }));
		let disk_ann = match &specialized.statements[2].kind {
			StatementKind::DefineIndex(disk_ann) => Some(disk_ann),
			_ => None,
		}
		.assert_some_with("expected DiskANN definition");
		assert!(matches!(disk_ann.definition, BasicIndexDefinition::DiskAnn { .. }));
		parse(
			"DEFINE INDEX embedding ON CLASS user FIELDS vector HNSW SCOPE BASE",
			ParserSettings::default(),
		)
		.assert_err_with("HNSW requires a dimension");
		let alter_graph = parse("ALTER GRAPH social", ParserSettings::default())
			.assert_err_with("ALTER GRAPH requires a capture action");
		assert_eq!(alter_graph.status().code().to_string(), "FG-42001");
		assert!(parse("ALTER GRAPH social CHANGEFEED 7d FULL", ParserSettings::default()).is_ok());
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
		.assert_ok_with("effective scope");
		assert!(matches!(effective.statements[0].kind, StatementKind::DefineIndex(_)));
	}

	/// Verifies parses analyzer schema statements and inline mapper.
	#[test]
	fn parses_analyzer_schema_statements_and_inline_mapper() {
		let program = parse(
			"DEFINE ANALYZER english TOKENIZERS blank, punct FILTERS lowercase, \
			 snowball(english), mapper({driven: 'drive', stop_word: null}) COMMENT 'search'; \
			 ALTER ANALYZER english DROP TOKENIZERS FUNCTION fn::tokens FILTERS nfkc; INFO FOR \
			 ANALYZER english; REMOVE ANALYZER IF EXISTS english;",
			ParserSettings::default(),
		)
		.assert_ok_with("analyzer DDL parses");
		assert!(matches!(program.statements[0].kind, StatementKind::DefineAnalyzer(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::Alter(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::Info(_)));
		assert!(matches!(program.statements[3].kind, StatementKind::Remove(_)));

		parse(
			"DEFINE ANALYZER invalid FUNCTION fn::tokens TOKENIZERS blank;",
			ParserSettings::default(),
		)
		.assert_err_with("FUNCTION and TOKENIZERS are mutually exclusive");
		parse(
			"DEFINE ANALYZER invalid FILTERS mapper('dictionary.txt');",
			ParserSettings::default(),
		)
		.assert_ok_with("mapper type is rejected during semantic analysis");
	}

	/// Verifies parses fulltext and knn expression operators.
	#[test]
	fn parses_fulltext_and_knn_expression_operators() {
		let program = parse(
			"SELECT title FROM document WHERE title @7@ 'rust graph' AND embedding <| 10, \
			 MINKOWSKI(3) |> $query; SELECT title FROM document WHERE title @@ 'rust'; SELECT \
			 title FROM document WHERE title @AND@ ['rust', 'graph']; SELECT title FROM document \
			 WHERE title @OR@ ['rust', 'graph']; SELECT id FROM document WHERE embedding <| 5, EF \
			 32 |> $query;",
			ParserSettings::default(),
		)
		.assert_ok_with("stage-22/23 search operators parse");
		let select = match &program.statements[0].kind {
			StatementKind::Select(select) => Some(select),
			_ => None,
		}
		.assert_some_with("expected SELECT");
		let (left, right) = match &select.filter.as_ref().assert_some_with("WHERE expression").kind
		{
			ExprKind::Binary {
				left,
				op: BinaryOperator::And,
				right,
			} => Some((left, right)),
			_ => None,
		}
		.assert_some_with("expected conjunction");
		assert!(matches!(left.kind, ExprKind::Fulltext {
			op: FulltextOperator::Slot(7),
			..
		}));
		assert!(matches!(right.kind, ExprKind::Knn {
			search: Some(KnnSearchArgument::Distance(KnnDistance::Minkowski(_))),
			..
		}));
		let ef = match &program.statements[4].kind {
			StatementKind::Select(ef) => Some(ef),
			_ => None,
		}
		.assert_some_with("expected EF SELECT");
		assert!(matches!(
			ef.filter.as_ref().map(|filter| &filter.kind),
			Some(ExprKind::Knn {
				search: Some(KnnSearchArgument::Ef(_)),
				..
			})
		));
		parse("SELECT title FROM document WHERE title @0@ 'rust';", ParserSettings::default())
			.assert_err_with("Fulltext slot zero is reserved");
	}

	/// Verifies parses graph content and property updates.
	#[test]
	fn parses_graph_content_and_property_updates() {
		let program = parse(
			"DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics', region: 'eu'}; UPDATE GRAPH \
			 sales SET properties.owner = 'risk', properties.address.city = 'Berlin' RETURN \
			 AFTER; UPDATE GRAPH sales UNSET properties.region RETURN DIFF",
			ParserSettings::default(),
		)
		.assert_ok_with("stage-13 graph property syntax");
		let definition = match &program.statements[0].kind {
			StatementKind::DefineGraph(definition) => Some(definition),
			_ => None,
		}
		.assert_some_with("expected DEFINE GRAPH");
		assert!(definition.content.is_some());
		assert!(matches!(program.statements[1].kind, StatementKind::UpdateGraph(_)));
		assert!(matches!(program.statements[2].kind, StatementKind::UpdateGraph(_)));
		assert!(
			parse("UPDATE GRAPH sales SET owner = 'risk'", ParserSettings::default(),).is_err()
		);
	}

	/// Verifies duration explain normalizes timeout.
	#[test]
	fn duration_explain_normalizes_timeout() {
		use crate::test_assertions::AssertResult as _;

		for source in [
			"EXPLAIN SELECT id FROM person TIMEOUT $limit;",
			"EXPLAIN FULL CREATE person:planned TIMEOUT 1s;",
			"EXPLAIN ANALYZE MATCH (* AS n) IN GRAPH social TIMEOUT $limit RETURN n;",
		] {
			let program = parse(source, ParserSettings::default()).assert_ok();
			let statement = &program.statements[0];
			let StatementKind::Explain(explain) = &statement.kind else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("test EXPLAIN missing"));
					test_result.assert_ok();
				};
			};
			assert!(statement.timeout.is_some(), "{source}");
			assert!(explain.statement.timeout.is_none(), "{source}");
		}
		for source in [
			"EXPLAIN SELECT id FROM person TIMEOUT 1s TIMEOUT 2s;",
			"EXPLAIN EXPLAIN SELECT id FROM person TIMEOUT 1s;",
			"EXPLAIN DEFINE CLASS person TYPE NODE TIMEOUT 1s;",
		] {
			assert!(parse(source, ParserSettings::default()).is_err(), "{source}");
		}
	}

	/// Verifies parses optimizer hints and prefix explain only.
	#[test]
	fn parses_optimizer_hints_and_prefix_explain_only() {
		let program = parse(
			"EXPLAIN FULL FORMAT JSON SELECT p.id FROM person AS p JOIN department AS d ON \
			 p.department = d.id WITH INDEX p.by_department, d.primary WHERE p.active = true; \
			 EXPLAIN ANALYZE CREATE person:alice; SELECT id FROM person WITH NOINDEX;",
			ParserSettings::default(),
		)
		.assert_ok_with("stage-14 optimizer syntax parses");
		assert!(matches!(program.statements[0].kind, StatementKind::Explain(_)));
		assert!(matches!(program.statements[1].kind, StatementKind::Explain(_)));
		let select = match &program.statements[2].kind {
			StatementKind::Select(select) => Some(select),
			_ => None,
		}
		.assert_some_with("expected SELECT");
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
