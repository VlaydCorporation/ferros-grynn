#![allow(
	clippy::wildcard_imports,
	reason = "SELECT syntax shares the parser vocabulary while remaining isolated from analysis"
)]

use super::*;

/// Ordered field paths and their optional per-path bridge clauses.
type ParsedSelectPaths = (Vec<SelectPath>, Option<Box<[Option<BridgeClause>]>>);

/// Internal SDK unions separate row concatenation from JOIN typing states.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(
	clippy::exhaustive_enums,
	reason = "Internal syntax/engine protocol is versioned with the workspace"
)]
pub enum SelectUnionPurpose {
	/// Independent class outputs share one global result window/reduction.
	#[default]
	IndependentOutputs,
	/// Class and missing-row states collapse before one relational JOIN.
	JoinedTypingVariants,
}

/// SELECT clauses retained for semantic analysis and logical planning.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
#[allow(
	clippy::partial_pub_fields,
	reason = "SDK lowering metadata is internal while text syntax fields retain their public API"
)]
pub struct SelectStatement {
	/// Independent SDK source plans; textual SELECT leaves this empty.
	pub(crate) union_inputs: Vec<Self>,
	/// Typing alternatives collapse before JOIN; they are not independent rows.
	pub(crate) union_purpose: SelectUnionPurpose,
	/// Canonical lexical origin within one SDK lowering, never a pointer.
	pub(crate) lexical_id: Option<u32>,
	/// Whether SELECT requests its single-target ONLY form.
	pub only: bool,
	/// Payload retained for the operation or its expected result.
	pub value: bool,
	/// Expressions or fields selected for output.
	pub projection: Vec<SelectItem>,
	/// Input source or initial endpoint selected by this syntax element.
	pub source: SelectSource,
	/// Joined sources retained in evaluation order.
	pub joins: Vec<SelectJoin>,
	/// Requested access mode or explicit query access-path selection.
	pub access: Option<SelectAccessHint>,
	/// Predicate or adapter deciding which input values continue downstream.
	pub filter: Option<Expr>,
	/// Expressions selecting collection values expanded by SPLIT.
	pub split: Vec<Expr>,
	/// Optional whole-input or key-based grouping specification.
	pub group: Option<SelectGroup>,
	/// Property paths excluded from the selected output.
	pub omit: Vec<SelectPath>,
	/// Property paths whose references are fetched for the output.
	pub fetch: Vec<SelectPath>,
	/// Cold aliases correspond positionally to FETCH paths; absent for local
	/// FETCH.
	pub fetch_bridges: Option<Box<[Option<BridgeClause>]>>,
	/// Ordering expressions applied to the produced rows.
	pub order: Vec<SelectOrder>,
	/// Optional expression bounding the number of output rows.
	pub limit: Option<Expr>,
	/// Initial endpoint or lower range bound selected by the syntax element.
	pub start: Option<SelectStart>,
	/// Whether the query permits temporary-file execution paths.
	pub tempfiles: bool,
	/// Cold graph selectors stay outside ordinary statement storage.
	pub return_graph: Option<Box<ReturnGraph>>,
}

impl SelectStatement {
	/// Wraps admitted independent inputs without cloning their syntax trees.
	pub(crate) fn independent_union(inputs: Vec<Self>, lexical_id: u32) -> Self {
		let value = inputs.first().is_some_and(|input| input.value);
		Self {
			union_inputs: inputs,
			union_purpose: SelectUnionPurpose::IndependentOutputs,
			lexical_id: Some(lexical_id),
			only: false,
			value,
			projection: Vec::new(),
			source: SelectSource {
				kind: SelectSourceKind::Empty,
				graph: None,
				alias: None,
				span: TextSpan::new(0, 0),
			},
			joins: Vec::new(),
			access: None,
			filter: None,
			split: Vec::new(),
			group: None,
			omit: Vec::new(),
			fetch: Vec::new(),
			fetch_bridges: None,
			order: Vec::new(),
			limit: None,
			start: None,
			tempfiles: false,
			return_graph: None,
		}
	}
}

/// Optional user restriction over secondary-index candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectAccessHint {
	/// Only the listed source/index candidates may be selected.
	Indexes(
		/// Only the listed source/index candidates may be selected.
		Vec<SelectIndexHint>,
	),
	/// User secondary indexes are excluded for this SELECT.
	NoIndex(
		/// User secondary indexes are excluded for this SELECT.
		TextSpan,
	),
}

/// One optionally source-qualified index name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SelectIndexHint {
	/// Optional source alias to which the index hint applies.
	pub source: Option<Identifier>,
	/// Index expression or named index selected by this syntax element.
	pub index: Identifier,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Wildcard or expression projected by a SELECT clause.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectItem {
	/// Project every available source field.
	Wildcard(
		/// Project every available source field.
		TextSpan,
	),
	/// Project an expression with an optional identifier alias.
	Expr {
		/// Expression retained for later semantic analysis and execution.
		expr: Expr,
		/// Optional name assigned to the produced value or source.
		alias: Option<Identifier>,
	},
	/// A checked logical output path supplied by the Rust LIVE builder.
	PathExpr {
		/// Expression evaluated in the original projection order.
		expr: Expr,
		/// Nested output shape applied before FETCH.
		alias: SelectPath,
	},
}

/// SELECT input with graph scope, optional alias and source location.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectSource {
	/// Selected operation or representation discriminator.
	pub kind: SelectSourceKind,
	/// Graph materialization or physical graph files retained by the operation.
	pub graph: Option<Expr>,
	/// Optional name assigned to the produced value or source.
	pub alias: Option<Identifier>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Atom, graph or derived-query input selected by a SELECT source.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectSourceKind {
	/// Explicit zero-domain SDK source, without a synthetic class.
	Empty,
	/// Read atoms selected by a class or record target.
	Atoms(
		/// Read atoms selected by a class or record target.
		SelectTarget,
	),
	/// SDK-only class typing state over one raw atom relation. Text parsing
	/// never creates this metadata; None describes an unmatched LEFT slot.
	#[doc(hidden)]
	AtomUnion {
		/// Canonical source classes, resolved against the same snapshot.
		classes: Vec<Identifier>,
		/// Selected class ordinal, or the explicit missing-row typing state.
		typing: Option<usize>,
	},
	/// Read atom contents from one graph source.
	Graph(
		/// Read atom contents from one graph source.
		Expr,
	),
	/// Read graph-reference values from the selected graph source.
	Graphs(
		/// Read graph-reference values from the selected graph source.
		Option<Vec<Expr>>,
	),
	/// Read rows produced by a nested SELECT source.
	Derived(
		/// Read rows produced by a nested SELECT source.
		Box<SelectStatement>,
	),
}

/// Joined source and optional predicate connecting it to the preceding input.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectJoin {
	/// Selected operation or representation discriminator.
	pub kind: SelectJoinKind,
	/// Input source or initial endpoint selected by this syntax element.
	pub source: SelectSource,
	/// Condition restricting the values or incidences accepted by this element.
	pub predicate: Option<Expr>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Inner, left-preserving or Cartesian semantics of a SELECT join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SelectJoinKind {
	/// Retain rows satisfying the join predicate on both inputs.
	Inner,
	/// Retain left input rows even when no right row matches.
	Left,
	/// Produce the Cartesian product of both inputs.
	Cross,
}

/// Whole-input or expression-key grouping requested by SELECT.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectGroup {
	/// Treat the entire selected population as one group.
	All(
		/// Treat the entire selected population as one group.
		TextSpan,
	),
	/// Group rows by the supplied expressions.
	By(
		/// Group rows by the supplied expressions.
		Vec<Expr>,
	),
}

/// Expression and comparison modifiers used to order SELECT output.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct SelectOrder {
	/// Expression evaluated to produce this clause or step value.
	pub expression: Expr,
	/// Special versioned stable-row hash ordering requested by `RAND()`.
	pub random: bool,
	/// Root Unicode collation with primary strength.
	pub collate: bool,
	/// Numeric comparison of digit sequences in strings.
	pub numeric: bool,
	/// Traversal orientation or requested sort direction selected by this
	/// element.
	pub direction: SelectOrderDirection,
	/// Requested placement of null values during ordering.
	pub nulls: SelectNulls,
}

/// Ascending or descending comparison order for a SELECT key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectOrderDirection {
	/// Sort smaller values before larger values.
	Ascending,
	/// Sort larger values before smaller values.
	Descending,
}

/// Placement of null values relative to other values during ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectNulls {
	/// Place null values before non-null values.
	First,
	/// Place null values after non-null values.
	Last,
}

/// Offset or cursor boundary from which SELECT output begins.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SelectStart {
	/// Start after skipping the evaluated number of rows.
	Offset(
		/// Start after skipping the evaluated number of rows.
		Expr,
	),
	/// Continue after the supplied cursor boundary.
	After {
		/// Expression resolving the continuation boundary.
		cursor: Expr,
		/// Whether cursor continuation requests read-committed behavior.
		read_committed: bool,
	},
}

/// Property-name segments and source span of a projected or addressed path.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SelectPath {
	/// Property-name segments composing the addressed path.
	pub segments: Vec<Identifier>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Class, record or parameter-bound target retained by SELECT syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectTarget {
	/// Select records from the named class.
	Class(
		/// Select records from the named class.
		Identifier,
	),
	/// SDK-only exact identity supplied through an immutable literal parameter.
	/// Text parsing never creates this metadata.
	#[doc(hidden)]
	BoundRecord {
		/// Declared class spelling, checked against the captured identity.
		class: Identifier,
		/// Supplied literal slot; never evaluated as user code during analysis.
		parameter: Identifier,
	},
	/// Select one explicitly identified record.
	Record {
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
		/// Record key expression retained for target resolution.
		key: RecordKey,
		/// Source byte range used to report diagnostics for this syntax
		/// element.
		span: TextSpan,
	},
}

/// Integer or text record key written directly in query syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecordKey {
	/// Signed integer key written directly in the query.
	Integer(
		/// Parsed key text retained for record resolution and diagnostics.
		Box<str>,
		/// Source byte range used for diagnostics about this syntax form.
		TextSpan,
	),
	/// Text key written directly in the query.
	String(
		/// Parsed key text retained for record resolution and diagnostics.
		Box<str>,
		/// Source byte range used for diagnostics about this syntax form.
		TextSpan,
	),
}

impl RecordKey {
	/// Parses the span syntax.
	pub(crate) const fn span(&self) -> TextSpan {
		match self {
			Self::Integer(_, span) | Self::String(_, span) => *span,
		}
	}
}

impl Parser<'_> {
	/// Parses the select statement syntax.
	pub(in crate::syn) async fn select_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<SelectStatement> {
		self.select_with_projection(context, None).await
	}

	/// Shares source/clauses with LIVE DIFF/PATCH, including implicit wildcard
	/// shorthand and ordinary explicit projections.
	pub(in crate::syn) async fn select_with_projection(
		&mut self,
		context: &mut Stk,
		implicit: Option<SelectItem>,
	) -> FerrosGrynnResult<SelectStatement> {
		let only = self.consume_keyword(keywords::ONLY).is_some();
		let value = self.consume_keyword(keywords::VALUE).is_some();
		let projection = context.run(|context| self.select_projection(context, implicit)).await?;
		if value && projection.len() != 1 {
			return Err(
				self.syntax("SELECT VALUE requires exactly one expression.", self.current().span)
			);
		}
		self.expect_keyword(keywords::FROM, "Expected `FROM` after SELECT projection.")?;
		let source = context.run(|context| self.select_source(context)).await?;
		let joins = self.select_joins(context).await?;
		let access = if self.consume_keyword(keywords::WITH).is_some() {
			if let Some(token) = self.consume_keyword(keywords::NOINDEX) {
				Some(SelectAccessHint::NoIndex(token.span))
			} else {
				self.expect_keyword(keywords::INDEX, "Expected INDEX or NOINDEX after WITH.")?;
				let mut indexes = Vec::new();
				loop {
					let first = self.identifier()?;
					let start = first.span;
					let (source, index) = if self.consume_symbol(Symbol::Dot).is_some() {
						(Some(first), self.identifier()?)
					} else {
						(None, first)
					};
					indexes.push(SelectIndexHint {
						span: cover(start, index.span),
						source,
						index,
					});
					if self.consume_symbol(Symbol::Comma).is_none() {
						break;
					}
				}
				Some(SelectAccessHint::Indexes(indexes))
			}
		} else {
			None
		};
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
		let (fetch, fetch_bridges) = if self.consume_keyword(keywords::FETCH).is_some() {
			self.select_fetch_list()?
		} else {
			(Vec::new(), None)
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
		let return_graph =
			if self.at_keyword(keywords::RETURN) && self.peek_keyword(1, keywords::GRAPH) {
				self.tokens.next();
				self.tokens.next();
				Some(Box::new(context.run(|context| self.return_graph(context)).await?))
			} else {
				None
			};
		Ok(SelectStatement {
			union_inputs: Vec::new(),
			union_purpose: SelectUnionPurpose::IndependentOutputs,
			lexical_id: None,
			only,
			value,
			projection,
			source,
			joins,
			access,
			filter,
			split,
			group,
			omit,
			fetch,
			fetch_bridges,
			order,
			limit,
			start,
			tempfiles,
			return_graph,
		})
	}

	/// Parses the JOIN clauses immediately following one SELECT source.
	///
	/// # Errors
	/// Returns the existing source, ON-clause or predicate parsing error.
	async fn select_joins(&mut self, context: &mut Stk) -> FerrosGrynnResult<Vec<SelectJoin>> {
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
		Ok(joins)
	}

	/// Parses output items before the shared FROM/clauses continuation.
	async fn select_projection(
		&mut self,
		context: &mut Stk,
		implicit: Option<SelectItem>,
	) -> FerrosGrynnResult<Vec<SelectItem>> {
		if let Some(item) = implicit {
			return Ok(vec![item]);
		}
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
		Ok(projection)
	}

	/// Parses the select source syntax.
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

	/// Parses the select join kind syntax.
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

	/// Parses the select expression list syntax.
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

	/// Parses ordinary ORDER BY expressions and modifiers.
	pub(in crate::syn) async fn select_order_list(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Vec<SelectOrder>> {
		self.select_order_list_with_power(context, 0).await
	}

	/// Preserves an enclosing traversal pipeline after an embedded ORDER BY.
	pub(in crate::syn) async fn select_order_list_with_power(
		&mut self,
		context: &mut Stk,
		minimum_binding_power: u8,
	) -> FerrosGrynnResult<Vec<SelectOrder>> {
		let mut order = Vec::new();
		loop {
			let expression = context
				.run(|context| self.expression(context, minimum_binding_power, 1, 0))
				.await?;
			let random = select_order_is_rand(&expression);
			let mut collate = false;
			let mut numeric = false;
			while self.at_keyword("COLLATE") || self.at_keyword("NUMERIC") {
				if let Some(token) = self.consume_keyword("COLLATE") {
					if std::mem::replace(&mut collate, true) {
						return Err(
							self.syntax("COLLATE may appear only once per ORDER item.", token.span)
						);
					}
				} else if let Some(token) = self.consume_keyword("NUMERIC")
					&& std::mem::replace(&mut numeric, true)
				{
					return Err(
						self.syntax("NUMERIC may appear only once per ORDER item.", token.span)
					);
				}
			}
			if random && (collate || numeric) {
				return Err(
					self.syntax("RAND() ordering cannot use COLLATE or NUMERIC.", expression.span)
				);
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
				random,
				collate,
				numeric,
				direction,
				nulls,
			});
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(order)
	}

	/// Parses the select path list syntax.
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

	/// FETCH owns individual adapter selections without changing OMIT path
	/// semantics.
	fn select_fetch_list(&mut self) -> FerrosGrynnResult<ParsedSelectPaths> {
		let mut paths = Vec::new();
		let mut bridges = Vec::new();
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
			bridges.push(self.bridge_clause()?);
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		let bridges = bridges.iter().any(Option::is_some).then(|| bridges.into_boxed_slice());
		Ok((paths, bridges))
	}

	/// Parses the graph reference expr syntax.
	pub(in crate::syn) fn graph_reference_expr(&mut self) -> FerrosGrynnResult<Expr> {
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

	/// Returns whether the select source boundary condition holds.
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
				keywords::WITH,
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

	/// Parses the select target syntax.
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

/// Returns whether the select order is rand condition holds.
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
