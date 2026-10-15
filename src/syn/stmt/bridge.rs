//! Explicit operation-level runtime adapter selection.

use super::{Identifier, Parser, cover, keywords};
use crate::gql_status::{FerrosGrynnResult, TextSpan};

/// Owned alias and its diagnostic location, without a client or storage DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BridgeClause {
	/// Runtime adapter alias; connection state is not syntax data.
	pub alias: Identifier,
	/// Exact clause location for typed diagnostics.
	pub span: TextSpan,
}

impl Parser<'_> {
	/// Parses the clause only at an operation boundary that owns bridge I/O.
	pub(in crate::syn) fn bridge_clause(&mut self) -> FerrosGrynnResult<Option<BridgeClause>> {
		let Some(with) = self.consume_keyword(keywords::WITH) else {
			return Ok(None);
		};
		self.expect_keyword("BRIDGE", "Expected BRIDGE after WITH.")?;
		let alias = self.identifier()?;
		Ok(Some(BridgeClause {
			span: cover(with.span, alias.span),
			alias,
		}))
	}
}
