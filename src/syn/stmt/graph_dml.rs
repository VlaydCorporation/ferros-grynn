#![allow(
	clippy::wildcard_imports,
	reason = "graph DML shares the statement parser vocabulary with sibling modules"
)]

use super::*;

/// Creates one node occurrence in a named graph.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AttachStatement {
	/// Record target addressed by the attachment or detachment.
	pub record: DmlTarget,
	/// Graph materialization or physical graph files retained by the operation.
	pub graph: Option<Identifier>,
	/// Optional label supplied for the created graph occurrence.
	pub label: Option<Identifier>,
	/// Optional policy retaining named containment levels while mutating
	/// records.
	pub keep_levelpath: Option<KeepLevelPathClause>,
}

/// Removes one occurrence from a named graph.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DetachStatement {
	/// Record target addressed by the attachment or detachment.
	pub record: DmlTarget,
	/// Graph materialization or physical graph files retained by the operation.
	pub graph: Option<Identifier>,
	/// Whether dependent contents may be removed with the selected target.
	pub cascade: bool,
}

/// Creates one binary graph-local edge occurrence.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RelateStatement {
	/// Optional external-database bridge clause attached to this graph
	/// operation.
	pub bridge: Option<Box<BridgeClause>>,
	/// Whether relation endpoints use atom rather than node-only semantics.
	pub atom_endpoints: bool,
	/// Input source or initial endpoint selected by this syntax element.
	pub source: RelateEndpoint,
	/// Edge target or edge pattern defining the relation incidence.
	pub edge: DmlTarget,
	/// Optional name assigned to the produced value or source.
	pub alias: Option<Identifier>,
	/// Optional label supplied for the created graph occurrence.
	pub label: Option<Identifier>,
	/// Traversal orientation or requested sort direction selected by this
	/// element.
	pub direction: MatchDirection,
	/// Target endpoint or schema object selected by the operation.
	pub target: RelateEndpoint,
	/// Graph materialization or physical graph files retained by the operation.
	pub graph: Option<Identifier>,
	/// Optional expression producing the relation weight.
	pub weight: Option<Expr>,
	/// Payload bytes, elements or retained storage backing this representation.
	pub data: Option<DmlData>,
	/// Optional policy retaining named containment levels while mutating
	/// records.
	pub keep_levelpath: Option<KeepLevelPathClause>,
	/// Optional projection of values returned after the data mutation.
	pub returning: Option<DmlReturn>,
	/// Optional existence or predicate requirement checked before the mutation.
	pub expect: Option<ExpectClause>,
}

/// RELATE owns endpoint selection independently of ATTACH/DETACH record
/// targets.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RelateEndpoint {
	/// Endpoint syntax analyzed by the RELATE pipeline.
	pub target: RelateEndpointTarget,
	/// Endpoint diagnostic location.
	pub span: TextSpan,
	/// Explicit AtomWalk node or edge wrapper, when supplied.
	pub kind: Option<MatchEndpointKind>,
}

/// One endpoint selector; exact record keys keep ordinary DML semantics.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RelateEndpointTarget {
	/// An exact class-qualified record with a dynamically evaluated key.
	Record(
		/// An exact class-qualified record with a dynamically evaluated key.
		DmlTarget,
	),
	/// A lexical reference or idiom evaluated by the ordinary expression
	/// kernel.
	Reference(
		/// A lexical reference or idiom evaluated by the ordinary expression
		/// kernel.
		Expr,
	),
}

/// Explicit request to persist the canonical graph-local LevelPath overlay.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct KeepLevelPathClause {
	/// Containment-level names retained by the level-path policy.
	pub aliases: Vec<Identifier>,
}

impl Parser<'_> {
	/// Attaches statement.
	pub(in crate::syn) async fn attach_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<AttachStatement> {
		let record = self.exact_graph_target(context, "ATTACH").await?;
		let graph = if self.consume_keyword(keywords::TO).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `TO`.")?;
			Some(self.identifier()?)
		} else if self.in_import {
			None
		} else {
			return Err(
				self.syntax("Expected `TO GRAPH` after ATTACH target.", self.current().span)
			);
		};
		let label = if self.consume_keyword(keywords::LABEL).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		let keep_levelpath = self.keep_levelpath_clause()?;
		self.finish_dml("ATTACH")?;
		Ok(AttachStatement {
			record,
			graph,
			label,
			keep_levelpath,
		})
	}

	/// Detaches statement.
	pub(in crate::syn) async fn detach_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<DetachStatement> {
		let record = self.exact_graph_target(context, "DETACH").await?;
		let graph = if self.consume_keyword(keywords::FROM).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `FROM`.")?;
			Some(self.identifier()?)
		} else if self.in_import {
			None
		} else {
			return Err(
				self.syntax("Expected `FROM GRAPH` after DETACH target.", self.current().span)
			);
		};
		let cascade = self.consume_keyword(keywords::CASCADE).is_some();
		self.finish_dml("DETACH")?;
		Ok(DetachStatement {
			record,
			graph,
			cascade,
		})
	}

	/// Parses the relate statement syntax.
	pub(in crate::syn) async fn relate_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<RelateStatement> {
		let atom_endpoints = self.consume_keyword(keywords::ATOM).is_some();
		if self.at_keyword("HYPER") {
			return Err(self.unsupported("RELATE HYPER", self.current().span));
		}
		let source = self.relate_endpoint(context, atom_endpoints).await?;
		let prefix_token = self.tokens.next();
		let TokenKind::Symbol(prefix) = prefix_token.kind else {
			return Err(self.syntax("Expected a RELATE edge connector.", prefix_token.span));
		};
		let (left, atom_connector, open_span) = match prefix {
			Symbol::Minus | Symbol::ArrowLeft => (
				prefix == Symbol::ArrowLeft,
				false,
				self.expect_symbol(Symbol::LeftBracket, "Expected `[` in RELATE edge.")?.span,
			),
			Symbol::TildeLeftBracket | Symbol::LeftTildeLeftBracket
				if atom_endpoints && source.kind.is_some() =>
			{
				(prefix == Symbol::LeftTildeLeftBracket, true, prefix_token.span)
			}
			_ => {
				return Err(self.syntax(
					"Expected a RELATE edge connector with matching AtomWalk wrappers.",
					prefix_token.span,
				));
			}
		};
		let edge = self.dml_target(context, false).await?;
		let alias = if self.consume_keyword(keywords::AS).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		let label = if self.consume_keyword(keywords::LABEL).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		if !atom_connector {
			self.close(Symbol::RightBracket, open_span, "`]`")?;
		}
		let suffix_token = self.tokens.next();
		let TokenKind::Symbol(suffix) = suffix_token.kind else {
			return Err(self.syntax("Expected `-` or `->` after RELATE edge.", suffix_token.span));
		};
		let right = match (atom_connector, suffix) {
			(false, Symbol::Minus) | (true, Symbol::RightBracketTilde) => false,
			(false, Symbol::ArrowRight) | (true, Symbol::RightBracketTildeRight) => true,
			_ => return Err(self.syntax("Invalid RELATE direction.", suffix_token.span)),
		};
		let direction = match (left, right) {
			(false, true) => MatchDirection::Forward,
			(true, false) => MatchDirection::Backward,
			(false, false) => MatchDirection::Undirected,
			(true, true) => MatchDirection::Bidirectional,
		};
		let target = self.relate_endpoint(context, atom_endpoints).await?;
		if atom_connector && target.kind.is_none() {
			return Err(self.syntax(
				"AtomWalk RELATE requires an explicit node/edge target wrapper.",
				target.span,
			));
		}
		let graph = if self.consume_keyword(keywords::IN).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN`.")?;
			Some(self.identifier()?)
		} else if self.in_import {
			None
		} else {
			return Err(self.syntax("Expected `IN GRAPH` after RELATE path.", self.current().span));
		};
		let mut bridge = self.bridge_clause()?.map(Box::new);
		let weight = if self.consume_keyword(keywords::WEIGHTED).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let data_span = self.current().span;
		let data = self.dml_data(context).await?;
		if matches!(
			data,
			Some(
				DmlData::Merge(_)
					| DmlData::Patch(_)
					| DmlData::Replace(_)
					| DmlData::Unset(_)
					| DmlData::Reset(_)
			)
		) {
			return Err(self.syntax("RELATE supports only CONTENT or SET.", data_span));
		}
		let keep_levelpath = self.keep_levelpath_clause()?;
		let returning = self.dml_return(context, true).await?;
		let expect = self.expect_clause(context).await?;
		if let Some(trailing) = self.bridge_clause()? {
			if bridge.is_some() {
				return Err(self.syntax("RELATE WITH BRIDGE may appear only once.", trailing.span));
			}
			bridge = Some(Box::new(trailing));
		}
		self.finish_dml("RELATE")?;
		Ok(RelateStatement {
			bridge,
			atom_endpoints,
			source,
			edge,
			alias,
			label,
			direction,
			target,
			graph,
			weight,
			data,
			keep_levelpath,
			returning,
			expect,
		})
	}

	/// Parses the keep levelpath clause syntax.
	pub(super) fn keep_levelpath_clause(
		&mut self,
	) -> FerrosGrynnResult<Option<KeepLevelPathClause>> {
		if self.consume_keyword(keywords::KEEP).is_none() {
			return Ok(None);
		}
		self.expect_keyword(keywords::LEVELPATH, "Expected `LEVELPATH` after `KEEP`.")?;
		let mut aliases = Vec::new();
		if self.consume_keyword(keywords::FOR).is_some() {
			let open = self
				.expect_symbol(Symbol::LeftBracket, "Expected `[` after `KEEP LEVELPATH FOR`.")?;
			aliases.push(self.identifier()?);
			while self.consume_symbol(Symbol::Comma).is_some() {
				aliases.push(self.identifier()?);
			}
			self.close(Symbol::RightBracket, open.span, "`]`")?;
		}
		Ok(Some(KeepLevelPathClause {
			aliases,
		}))
	}

	/// Parses the exact graph target syntax.
	async fn exact_graph_target(
		&mut self,
		context: &mut Stk,
		statement: &str,
	) -> FerrosGrynnResult<DmlTarget> {
		let target = self.dml_target(context, false).await?;
		if !target.is_exact() {
			return Err(self.syntax(
				&format!("{statement} requires an exact class-qualified RecordId."),
				target.class().span,
			));
		}
		Ok(target)
	}

	/// Retains exact record keys and references without consuming connector
	/// operators.
	async fn relate_endpoint(
		&mut self,
		context: &mut Stk,
		atom: bool,
	) -> FerrosGrynnResult<RelateEndpoint> {
		let start = self.current().span;
		let outer = self.consume_symbol(Symbol::Less);
		if outer.is_some() && !atom {
			return Err(self.syntax("AtomWalk wrappers require RELATE ATOM.", start));
		}
		let (open, kind, close) = if outer.is_some() {
			let open = self.tokens.next();
			match open.kind {
				TokenKind::Symbol(Symbol::LeftParen) => {
					(Some(open.span), Some(MatchEndpointKind::Node), Symbol::RightParen)
				}
				TokenKind::Symbol(Symbol::LeftBracket) => {
					(Some(open.span), Some(MatchEndpointKind::Edge), Symbol::RightBracket)
				}
				_ => {
					return Err(
						self.syntax("Expected `(` or `[` inside AtomWalk endpoint.", open.span)
					);
				}
			}
		} else {
			(self.consume_symbol(Symbol::LeftParen).map(|open| open.span), None, Symbol::RightParen)
		};
		let minimum = if open.is_some() {
			0
		} else {
			u8::MAX
		};
		let (target, end) = if matches!(self.current().kind, TokenKind::Word { .. })
			&& matches!(self.tokens.peek(1).kind, TokenKind::Symbol(Symbol::Colon))
		{
			let target = self.class_or_record_target_at(context, minimum).await?;
			if !target.is_exact() {
				return Err(self.syntax("RELATE requires an exact record endpoint.", start));
			}
			let end = match &target {
				DmlTarget::Record {
					span,
					..
				} => *span,
				_ => start,
			};
			(RelateEndpointTarget::Record(target), end)
		} else {
			let value = context.run(|context| self.expression(context, minimum, 1, 0)).await?;
			let end = value.span;
			(RelateEndpointTarget::Reference(value), end)
		};
		let end = if let Some(open) = open {
			self.close(close, open, "endpoint closing delimiter")?.span
		} else {
			end
		};
		let end = if outer.is_some() {
			self.expect_symbol(Symbol::Greater, "Expected `>` after AtomWalk endpoint.")?.span
		} else {
			end
		};
		Ok(RelateEndpoint {
			target,
			kind,
			span: cover(start, end),
		})
	}
}
