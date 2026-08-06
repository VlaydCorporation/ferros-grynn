mod ast;
mod lexer;
mod parser;

pub use ast::{
	BinaryOperator,
	Expr,
	ExprKind,
	Identifier,
	LetStatement,
	Literal,
	ObjectField,
	Program,
	RecordKey,
	ReturnStatement,
	SelectItem,
	SelectStatement,
	SelectTarget,
	Statement,
	StatementKind,
	TypeName,
	UnaryOperator,
};

use crate::{config::Config, gql_status::FerrosGrynnResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserSettings {
	pub max_object_parsing_depth: u32,
	pub max_query_parsing_depth: u32,
}

impl Default for ParserSettings {
	fn default() -> Self {
		Self::from(&Config::default())
	}
}

impl From<&Config> for ParserSettings {
	fn from(config: &Config) -> Self {
		Self {
			max_object_parsing_depth: config.max_object_parsing_depth,
			max_query_parsing_depth: config.max_query_parsing_depth,
		}
	}
}

pub fn parse(source: &str, settings: ParserSettings) -> FerrosGrynnResult<Program> {
	parser::parse(source, settings)
}

#[cfg(test)]
mod property_tests {
	use proptest::prelude::*;

	use super::{ParserSettings, parse};

	proptest! {
		#[test]
		fn parser_never_panics(source in ".{0,512}") {
			let _ = parse(&source, ParserSettings::default());
		}
	}
}
