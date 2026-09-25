#![allow(
	clippy::wildcard_imports,
	reason = "graph property syntax shares the statement facade's parser vocabulary"
)]

use super::*;

/// One graph-property path below the reserved `properties` root.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraphPropertyPath {
	pub fields: Vec<Identifier>,
}

/// One graph-property assignment.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct GraphPropertyAssignment {
	pub path: GraphPropertyPath,
	pub value: Expr,
}

/// Mutation applied by `UPDATE GRAPH`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum GraphPropertyMutation {
	Set(Vec<GraphPropertyAssignment>),
	Unset(Vec<GraphPropertyPath>),
}

/// Catalog-owned graph-property update.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct UpdateGraphStatement {
	pub graph: Identifier,
	pub mutation: GraphPropertyMutation,
	pub returning: Option<DmlReturn>,
}

impl Parser<'_> {
	pub(in crate::syn) async fn update_graph_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<UpdateGraphStatement> {
		let graph = self.identifier()?;
		let mutation = if self.consume_keyword(keywords::SET).is_some() {
			let mut assignments = Vec::new();
			loop {
				let path = self.graph_property_path()?;
				self.expect_symbol(Symbol::Equal, "Expected `=` in graph property assignment.")?;
				let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
				assignments.push(GraphPropertyAssignment {
					path,
					value,
				});
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
			GraphPropertyMutation::Set(assignments)
		} else if self.consume_keyword(keywords::UNSET).is_some() {
			let mut paths = vec![self.graph_property_path()?];
			while self.consume_symbol(Symbol::Comma).is_some() {
				paths.push(self.graph_property_path()?);
			}
			GraphPropertyMutation::Unset(paths)
		} else {
			return Err(self.syntax(
				"Expected `SET properties.<path>` or `UNSET properties.<path>`.",
				self.current().span,
			));
		};
		let return_span = self.current().span;
		let returning = self.dml_return(context, false).await?;
		if matches!(returning, Some(DmlReturn::Fields(_) | DmlReturn::Value(_))) {
			return Err(self.syntax(
				"UPDATE GRAPH supports only RETURN NONE, BEFORE, AFTER, or DIFF.",
				return_span,
			));
		}
		self.finish_dml("UPDATE GRAPH")?;
		Ok(UpdateGraphStatement {
			graph,
			mutation,
			returning,
		})
	}

	fn graph_property_path(&mut self) -> FerrosGrynnResult<GraphPropertyPath> {
		let root = self.identifier()?;
		if !root.value.eq_ignore_ascii_case("properties") {
			return Err(self
				.syntax("Graph user properties must be addressed below `properties`.", root.span));
		}
		self.expect_symbol(Symbol::Dot, "Expected a property name after `properties`.")?;
		let mut fields = vec![self.identifier()?];
		while self.consume_symbol(Symbol::Dot).is_some() {
			fields.push(self.identifier()?);
		}
		Ok(GraphPropertyPath {
			fields,
		})
	}
}
