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
	/// Start traversal from the evaluated expression value.
	Expression(
		/// Start traversal from the evaluated expression value.
		Expr,
	),
	/// Start traversal from MATCH output.
	Match(
		/// Start traversal from MATCH output.
		Box<MatchStatement>,
	),
	/// Start traversal from SELECT output.
	Select(
		/// Start traversal from SELECT output.
		Box<SelectStatement>,
	),
}

/// One `tv::` pipeline call in source order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TraverseStep {
	/// Expression evaluated to produce this clause or step value.
	pub expression: Expr,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Streaming graph-first traversal statement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TraverseStatement {
	/// Whether graph traversal uses atom endpoints rather than node-only
	/// endpoints.
	pub atom: bool,
	/// Structural or path constraints applied to the selected graph operation.
	pub constraints: MatchConstraints,
	/// Input source or initial endpoint selected by this syntax element.
	pub source: TraverseSource,
	/// Graph materialization or physical graph files retained by the operation.
	pub graph: Option<Expr>,
	/// Versioned PURE contribution for path-generating traversal operations.
	pub custom_cost: Option<FunctionSignatureName>,
	/// Traversal or pattern steps retained in execution order.
	pub steps: Vec<TraverseStep>,
	/// Optional projection of values returned after the data mutation.
	pub returning: Option<ReturnStatement>,
	/// Optional graph projection replacing the ordinary value projection.
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
				context
					.run(|context| self.match_statement_with_projection_power(context, false, 2))
					.await?,
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
		let custom_cost = if self.consume_keyword(keywords::WITH).is_some() {
			self.expect_keyword(keywords::COST, "Expected `COST` after `WITH` in TRAVERSE.")?;
			Some(self.function_signature_name()?)
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
			custom_cost,
			steps,
			returning,
			return_graph,
		})
	}
}
