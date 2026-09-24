#![allow(
	clippy::wildcard_imports,
	reason = "graph DML shares the statement parser vocabulary with sibling modules"
)]

use super::*;

/// Creates one node occurrence in a named graph.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AttachStatement {
	pub record: DmlTarget,
	pub graph: Option<Identifier>,
	pub label: Option<Identifier>,
}

/// Removes one occurrence from a named graph.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct DetachStatement {
	pub record: DmlTarget,
	pub graph: Option<Identifier>,
	pub cascade: bool,
}

/// Creates one binary graph-local edge occurrence.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct RelateStatement {
	pub atom_endpoints: bool,
	pub source: DmlTarget,
	pub edge: DmlTarget,
	pub alias: Option<Identifier>,
	pub label: Option<Identifier>,
	pub direction: MatchDirection,
	pub target: DmlTarget,
	pub graph: Option<Identifier>,
	pub weight: Option<Expr>,
	pub data: Option<DmlData>,
	pub returning: Option<DmlReturn>,
	pub expect: Option<ExpectClause>,
}

impl Parser<'_> {
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
		self.finish_dml("ATTACH")?;
		Ok(AttachStatement {
			record,
			graph,
			label,
		})
	}

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

	pub(in crate::syn) async fn relate_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<RelateStatement> {
		let atom_endpoints = self.consume_keyword(keywords::ATOM).is_some();
		if self.at_keyword("HYPER") {
			return Err(self.unsupported("RELATE HYPER", self.current().span));
		}
		let source = self.exact_graph_target(context, "RELATE endpoint").await?;
		let prefix_token = self.tokens.next();
		let TokenKind::Symbol(prefix) = prefix_token.kind else {
			return Err(self.syntax("Expected a RELATE edge connector.", prefix_token.span));
		};
		if !matches!(prefix, Symbol::Minus | Symbol::ArrowLeft) {
			return Err(self.syntax("Expected `-[` or `<-[` in RELATE.", prefix_token.span));
		}
		let open = self.expect_symbol(Symbol::LeftBracket, "Expected `[` in RELATE edge.")?;
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
		self.close(Symbol::RightBracket, open.span, "`]`")?;
		let suffix_token = self.tokens.next();
		let TokenKind::Symbol(suffix) = suffix_token.kind else {
			return Err(self.syntax("Expected `-` or `->` after RELATE edge.", suffix_token.span));
		};
		let direction = match (prefix, suffix) {
			(Symbol::Minus, Symbol::ArrowRight) => MatchDirection::Forward,
			(Symbol::ArrowLeft, Symbol::Minus) => MatchDirection::Backward,
			(Symbol::Minus, Symbol::Minus) => MatchDirection::Undirected,
			(Symbol::ArrowLeft, Symbol::ArrowRight) => MatchDirection::Bidirectional,
			_ => return Err(self.syntax("Invalid RELATE direction.", suffix_token.span)),
		};
		let target = self.exact_graph_target(context, "RELATE endpoint").await?;
		let graph = if self.consume_keyword(keywords::IN).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN`.")?;
			Some(self.identifier()?)
		} else if self.in_import {
			None
		} else {
			return Err(self.syntax("Expected `IN GRAPH` after RELATE path.", self.current().span));
		};
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
		let returning = self.dml_return(context, true).await?;
		let expect = self.expect_clause(context).await?;
		self.finish_dml("RELATE")?;
		Ok(RelateStatement {
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
			returning,
			expect,
		})
	}

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
}
