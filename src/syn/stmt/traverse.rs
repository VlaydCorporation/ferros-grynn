//! Canonical Stage-17 TRAVERSE syntax.

#![allow(
	clippy::wildcard_imports,
	reason = "traversal syntax uses the shared statement parser vocabulary"
)]

use super::*;

/// Source consumed by a traversal pipeline.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TraverseSource {
	Expression(Expr),
	Match(Box<MatchStatement>),
	Select(Box<SelectStatement>),
}

/// One `tv::` pipeline call in source order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TraverseStep {
	pub expression: Expr,
	pub span: TextSpan,
}

/// Streaming graph-first traversal statement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TraverseStatement {
	pub atom: bool,
	pub constraints: MatchConstraints,
	pub source: TraverseSource,
	pub graph: Option<Expr>,
	pub steps: Vec<TraverseStep>,
	pub returning: Option<ReturnStatement>,
	pub return_graph: Option<ReturnGraph>,
}

impl Parser<'_> {
	/// Parses one traversal source, its optional graph scope, and ordered
	/// steps.
	pub(in crate::syn) async fn traverse_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<TraverseStatement> {
		let atom = self.consume_keyword(keywords::ATOM).is_some();
		let constraints = self.global_match_constraints()?;
		self.expect_keyword(keywords::FROM, "Expected `FROM` in TRAVERSE statement.")?;
		let source = if self.consume_keyword(keywords::MATCH).is_some() {
			TraverseSource::Match(Box::new(
				context.run(|context| self.match_statement(context, false)).await?,
			))
		} else if self.consume_keyword(keywords::SELECT).is_some() {
			TraverseSource::Select(Box::new(
				context.run(|context| self.select_statement(context)).await?,
			))
		} else {
			TraverseSource::Expression(
				context.run(|context| self.expression(context, 10, 1, 0)).await?,
			)
		};
		let graph = if self.consume_keyword(keywords::IN).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN` in TRAVERSE.")?;
			Some(self.graph_reference_expr()?)
		} else {
			None
		};
		let mut steps = Vec::new();
		while let Some(pipeline) = self.consume_symbol(Symbol::Pipeline) {
			let expression = context.run(|context| self.expression(context, 2, 1, 0)).await?;
			steps.push(TraverseStep {
				span: cover(pipeline.span, expression.span),
				expression,
			});
		}
		if steps.is_empty() {
			return Err(self.syntax(
				"TRAVERSE requires at least one `|> tv::step(...)`.",
				self.current().span,
			));
		}
		let (returning, return_graph) = if self.consume_keyword(keywords::RETURN).is_some() {
			if self.consume_keyword(keywords::GRAPH).is_some() {
				(None, Some(context.run(|context| self.return_graph(context)).await?))
			} else {
				(Some(context.run(|context| self.return_statement(context)).await?), None)
			}
		} else {
			(None, None)
		};
		Ok(TraverseStatement {
			atom,
			constraints,
			source,
			graph,
			steps,
			returning,
			return_graph,
		})
	}
}
