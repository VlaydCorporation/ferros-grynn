#![allow(
	clippy::wildcard_imports,
	reason = "SELECT syntax shares the parser vocabulary while remaining isolated from analysis"
)]

use super::*;

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectStatement {
	pub only: bool,
	pub value: bool,
	pub projection: Vec<SelectItem>,
	pub source: SelectSource,
	pub joins: Vec<SelectJoin>,
	pub filter: Option<Expr>,
	pub split: Vec<Expr>,
	pub group: Option<SelectGroup>,
	pub omit: Vec<SelectPath>,
	pub fetch: Vec<SelectPath>,
	pub order: Vec<SelectOrder>,
	pub limit: Option<Expr>,
	pub start: Option<SelectStart>,
	pub tempfiles: bool,
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

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectSource {
	pub kind: SelectSourceKind,
	pub graph: Option<Expr>,
	pub alias: Option<Identifier>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectSourceKind {
	Atoms(SelectTarget),
	Graph(Expr),
	Graphs(Option<Vec<Expr>>),
	Derived(Box<SelectStatement>),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectJoin {
	pub kind: SelectJoinKind,
	pub source: SelectSource,
	pub predicate: Option<Expr>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectJoinKind {
	Inner,
	Left,
	Cross,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectGroup {
	All(TextSpan),
	By(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectOrder {
	pub expression: Expr,
	pub direction: SelectOrderDirection,
	pub nulls: SelectNulls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectOrderDirection {
	Ascending,
	Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectNulls {
	First,
	Last,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectStart {
	Offset(Expr),
	After {
		cursor: Expr,
		read_committed: bool,
	},
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SelectPath {
	pub segments: Vec<Identifier>,
	pub span: TextSpan,
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
		let source = context.run(|context| self.select_source(context)).await?;
		let mut joins = Vec::new();
		while let Some(kind) = self.select_join_kind()? {
			let start = self.previous_span();
			let source = context.run(|context| self.select_source(context)).await?;
			let predicate = if kind == SelectJoinKind::Cross {
				if self.at_keyword(keywords::ON) {
					return Err(self.syntax("CROSS JOIN does not accept ON.", self.current().span));
				}
				None
			} else {
				self.expect_keyword(keywords::ON, "INNER and LEFT JOIN require ON.")?;
				Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
			};
			joins.push(SelectJoin {
				kind,
				span: cover(start, self.previous_span()),
				source,
				predicate,
			});
		}
		let filter = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let split = if self.consume_keyword(keywords::SPLIT).is_some() {
			self.consume_keyword(keywords::ON);
			self.select_expression_list(context).await?
		} else {
			Vec::new()
		};
		let group = if let Some(token) = self.consume_keyword(keywords::GROUP) {
			if self.consume_keyword(keywords::ALL).is_some() {
				Some(SelectGroup::All(token.span))
			} else {
				self.consume_keyword(keywords::BY);
				Some(SelectGroup::By(self.select_expression_list(context).await?))
			}
		} else {
			None
		};
		let omit = if self.consume_keyword(keywords::OMIT).is_some() {
			self.select_path_list()?
		} else {
			Vec::new()
		};
		let fetch = if self.consume_keyword(keywords::FETCH).is_some() {
			self.select_path_list()?
		} else {
			Vec::new()
		};
		let order = if self.consume_keyword(keywords::ORDER).is_some() {
			self.expect_keyword(keywords::BY, "Expected BY after ORDER.")?;
			self.select_order_list(context).await?
		} else {
			Vec::new()
		};
		let mut start = None;
		let mut limit = None;
		loop {
			if self.consume_keyword(keywords::LIMIT).is_some() {
				if limit.is_some() {
					return Err(self.syntax("LIMIT may appear only once.", self.previous_span()));
				}
				self.consume_keyword(keywords::BY);
				limit = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
				continue;
			}
			if self.consume_keyword(keywords::START).is_some() {
				if start.is_some() {
					return Err(self.syntax("START may appear only once.", self.previous_span()));
				}
				start = if self.consume_keyword(keywords::AFTER).is_some() {
					let cursor = context.run(|context| self.expression(context, 0, 1, 0)).await?;
					let read_committed = if self.consume_keyword(keywords::READ).is_some() {
						self.expect_keyword(
							keywords::COMMITTED,
							"Expected COMMITTED after START AFTER ... READ.",
						)?;
						true
					} else {
						false
					};
					Some(SelectStart::After {
						cursor,
						read_committed,
					})
				} else {
					self.consume_keyword(keywords::AT);
					Some(SelectStart::Offset(
						context.run(|context| self.expression(context, 0, 1, 0)).await?,
					))
				};
				continue;
			}
			break;
		}
		let tempfiles = self.consume_keyword(keywords::TEMPFILES).is_some();
		if self.at_keyword(keywords::WITH) {
			return Err(self.unsupported("SELECT WITH index hints", self.current().span));
		}
		Ok(SelectStatement {
			only,
			value,
			projection,
			source,
			joins,
			filter,
			split,
			group,
			omit,
			fetch,
			order,
			limit,
			start,
			tempfiles,
		})
	}

	async fn select_source(&mut self, context: &mut Stk) -> FerrosGrynnResult<SelectSource> {
		let start = self.current().span;
		let (kind, graph, alias_required) = if self.consume_keyword(keywords::GRAPH).is_some() {
			(SelectSourceKind::Graph(self.graph_reference_expr()?), None, false)
		} else if self.consume_keyword(keywords::GRAPHS).is_some() {
			let graphs = if self.consume_symbol(Symbol::LeftBracket).is_some() {
				let mut values = Vec::new();
				if !self.at_symbol(Symbol::RightBracket) {
					loop {
						values.push(self.graph_reference_expr()?);
						if self.consume_symbol(Symbol::Comma).is_none() {
							break;
						}
					}
				}
				self.expect_symbol(Symbol::RightBracket, "Expected `]` after GRAPHS list.")?;
				Some(values)
			} else if self.select_source_boundary() {
				None
			} else {
				let mut values = vec![self.graph_reference_expr()?];
				while self.consume_symbol(Symbol::Comma).is_some() {
					values.push(self.graph_reference_expr()?);
				}
				Some(values)
			};
			(SelectSourceKind::Graphs(graphs), None, false)
		} else if self.at_symbol(Symbol::LeftParen) && self.peek_keyword(1, keywords::SELECT) {
			let open = self.tokens.next();
			self.tokens.next();
			let select = context.run(|context| self.select_statement(context)).await?;
			self.close(Symbol::RightParen, open.span, "`)`")?;
			(SelectSourceKind::Derived(Box::new(select)), None, true)
		} else {
			let target = self.select_target()?;
			let graph = if self.consume_keyword(keywords::IN).is_some() {
				self.expect_keyword(keywords::GRAPH, "Expected GRAPH after IN.")?;
				Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
			} else {
				None
			};
			(SelectSourceKind::Atoms(target), graph, false)
		};
		let alias = if self.consume_keyword(keywords::AS).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		if alias_required && alias.is_none() {
			return Err(self.syntax("A derived SELECT source requires AS alias.", start));
		}
		Ok(SelectSource {
			kind,
			graph,
			alias,
			span: cover(start, self.previous_span()),
		})
	}

	fn select_join_kind(&mut self) -> FerrosGrynnResult<Option<SelectJoinKind>> {
		if self.consume_keyword(keywords::JOIN).is_some() {
			return Ok(Some(SelectJoinKind::Inner));
		}
		let kind = if self.consume_keyword(keywords::INNER).is_some() {
			Some(SelectJoinKind::Inner)
		} else if self.consume_keyword(keywords::LEFT).is_some() {
			Some(SelectJoinKind::Left)
		} else if self.consume_keyword(keywords::CROSS).is_some() {
			Some(SelectJoinKind::Cross)
		} else {
			None
		};
		if kind.is_some() {
			self.expect_keyword(keywords::JOIN, "Expected JOIN after join kind.")?;
		}
		Ok(kind)
	}

	async fn select_expression_list(&mut self, context: &mut Stk) -> FerrosGrynnResult<Vec<Expr>> {
		let mut expressions = Vec::new();
		loop {
			expressions.push(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(expressions)
	}

	async fn select_order_list(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Vec<SelectOrder>> {
		let mut order = Vec::new();
		loop {
			let expression = context.run(|context| self.expression(context, 0, 1, 0)).await?;
			if select_order_is_rand(&expression) {
				return Err(self.unsupported("ORDER BY RAND()", expression.span));
			}
			if self.at_keyword("COLLATE") {
				return Err(self.unsupported("ORDER BY COLLATE", self.current().span));
			}
			if self.at_keyword("NUMERIC") {
				return Err(self.unsupported("ORDER BY NUMERIC", self.current().span));
			}
			let direction = if self.consume_keyword(keywords::DESC).is_some() {
				SelectOrderDirection::Descending
			} else {
				self.consume_keyword(keywords::ASC);
				SelectOrderDirection::Ascending
			};
			let nulls = if self.consume_keyword(keywords::NULLS).is_some() {
				if self.consume_keyword(keywords::FIRST).is_some() {
					SelectNulls::First
				} else {
					self.expect_keyword(keywords::LAST, "Expected FIRST or LAST after NULLS.")?;
					SelectNulls::Last
				}
			} else {
				SelectNulls::Last
			};
			order.push(SelectOrder {
				expression,
				direction,
				nulls,
			});
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(order)
	}

	fn select_path_list(&mut self) -> FerrosGrynnResult<Vec<SelectPath>> {
		let mut paths = Vec::new();
		loop {
			let first = self.identifier()?;
			let start = first.span;
			let mut segments = vec![first];
			while self.consume_symbol(Symbol::Dot).is_some() {
				segments.push(self.identifier()?);
			}
			paths.push(SelectPath {
				span: cover(start, self.previous_span()),
				segments,
			});
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(paths)
	}

	fn graph_reference_expr(&mut self) -> FerrosGrynnResult<Expr> {
		let token = self.tokens.next();
		match token.kind {
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
			} => Ok(Expr {
				kind: ExprKind::Name(Identifier {
					value,
					quoted,
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
			_ => Err(self.syntax("Expected a graph reference.", token.span)),
		}
	}

	fn select_source_boundary(&self) -> bool {
		self.at_eof()
			|| matches!(
				self.current().kind,
				TokenKind::Symbol(Symbol::Semicolon | Symbol::RightParen)
			)
			|| [
				keywords::AS,
				keywords::JOIN,
				keywords::INNER,
				keywords::LEFT,
				keywords::CROSS,
				keywords::WHERE,
				keywords::SPLIT,
				keywords::GROUP,
				keywords::OMIT,
				keywords::FETCH,
				keywords::ORDER,
				keywords::START,
				keywords::LIMIT,
				keywords::TIMEOUT,
				keywords::TEMPFILES,
			]
			.into_iter()
			.any(|keyword| self.at_keyword(keyword))
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
}

fn select_order_is_rand(expression: &Expr) -> bool {
	let ExprKind::Call {
		callee,
		arguments,
		..
	} = &expression.kind
	else {
		return false;
	};
	matches!(
		&callee.kind,
		ExprKind::Name(name) if name.value.eq_ignore_ascii_case("rand") && arguments.is_empty()
	)
}
