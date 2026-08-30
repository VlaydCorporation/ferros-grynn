#![allow(
	clippy::wildcard_imports,
	reason = "move-only statement split keeps the existing parser imports during stage 1"
)]

use super::*;

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReturnStatement {
	pub value: Expr,
	pub alias: Option<Identifier>,
}

impl Parser<'_> {
	pub(in crate::syn) async fn return_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<ReturnStatement> {
		let value = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let alias = if self.consume_keyword(keywords::AS).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		Ok(ReturnStatement {
			value,
			alias,
		})
	}
}
