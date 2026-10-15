#![allow(
	clippy::wildcard_imports,
	reason = "move-only statement split keeps the existing parser imports during stage 1"
)]

use super::*;

/// Returned expression and optional name assigned to its result.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReturnStatement {
	/// Payload retained for the operation or its expected result.
	pub value: Expr,
	/// Optional name assigned to the produced value or source.
	pub alias: Option<Identifier>,
}

impl Parser<'_> {
	/// Parses the return statement syntax.
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
