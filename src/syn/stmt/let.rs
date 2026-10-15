#![allow(
	clippy::wildcard_imports,
	reason = "move-only statement split keeps the existing parser imports during stage 1"
)]

use super::*;

/// Value-producing form accepted by one lexical binding.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LetValue {
	/// Variable initialized from an expression.
	Expression(
		/// Variable initialized from an expression.
		Expr,
	),
	/// Cold graph construction owned by this lexical binding.
	BuildGraph(
		/// Cold graph construction owned by this lexical binding.
		Box<BuildGraphStatement>,
	),
	/// A LIVE registration returns its owned UUID to the lexical binding.
	LiveSelect(
		/// A LIVE registration returns its owned UUID to the lexical binding.
		Box<LiveSelectStatement>,
	),
}

/// Variable binding with an optional declared type and its initializer.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct LetStatement {
	/// Declared name used when resolving or displaying this element.
	pub name: Identifier,
	/// Optional type annotation applied to the variable initializer.
	pub declared_type: Option<TypeName>,
	/// Payload retained for the operation or its expected result.
	pub value: LetValue,
}

impl Parser<'_> {
	/// Ordinary and reactive bindings share one frontend dispatch boundary.
	pub(in crate::syn) fn at_binding_statement(&self) -> bool {
		self.at_keyword(keywords::LET) || self.at_keyword(keywords::LIVE)
	}

	/// Consumes the common lexical registration prefixes.
	pub(in crate::syn) async fn binding_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<StatementKind> {
		if self.consume_keyword(keywords::LET).is_some() {
			Ok(StatementKind::Let(context.run(|context| self.let_statement(context)).await?))
		} else {
			self.expect_keyword(keywords::LIVE, "Expected LIVE registration.")?;
			if self.consume_keyword(keywords::LET).is_some() {
				Ok(StatementKind::LiveLet(
					context.run(|context| self.live_let_statement(context)).await?,
				))
			} else {
				Ok(StatementKind::LiveSelect(
					context.run(|context| self.live_select_statement(context)).await?,
				))
			}
		}
	}

	/// Parses the let statement syntax.
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
		let value = if self.consume_keyword(keywords::LIVE).is_some() {
			LetValue::LiveSelect(Box::new(
				context.run(|context| self.live_select_statement(context)).await?,
			))
		} else if self.consume_keyword(keywords::BUILD).is_some() {
			self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `BUILD`.")?;
			LetValue::BuildGraph(Box::new(
				context.run(|context| self.build_graph_statement(context)).await?,
			))
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
