#![allow(
	clippy::wildcard_imports,
	reason = "move-only statement split keeps the existing parser imports during stage 1"
)]

use super::*;

/// Value-producing form accepted by one lexical binding.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LetValue {
	Expression(Expr),
	BuildGraph(BuildGraphStatement),
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LetStatement {
	pub name: Identifier,
	pub declared_type: Option<TypeName>,
	pub value: LetValue,
}

impl Parser<'_> {
	pub(in crate::syn) async fn let_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<LetStatement> {
		let name = self.parameter()?;
		let declared_type = if self.consume_symbol(Symbol::Colon).is_some() {
			Some(self.type_name()?)
		} else {
			None
		};
		self.expect_symbol(Symbol::Equal, "Expected `=` in LET statement.")?;
		let value = if self.consume_keyword(keywords::BUILD).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `BUILD`.")?;
			LetValue::BuildGraph(context.run(|context| self.build_graph_statement(context)).await?)
		} else {
			LetValue::Expression(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		};
		Ok(LetStatement {
			name,
			declared_type,
			value,
		})
	}
}
