//! Canonical graph-result and graph-contract syntax.

#![allow(
	clippy::wildcard_imports,
	reason = "graph-result syntax uses the shared statement parser vocabulary"
)]

use super::*;

/// Graph family named by a query contract before semantic lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphFamilyName {
	Graph,
	Tree,
	RootedTree,
	Forest,
	Dag,
	Hyper,
	Meta,
}

/// Orthogonal topology validator named by a query contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum GraphValidatorName {
	Simple,
	Bipartite,
	Complete,
}

/// One normalized graph family and validator declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraphContract {
	pub family: Option<GraphFamilyName>,
	pub validators: Vec<GraphValidatorName>,
	pub span: TextSpan,
}

/// Terminal owned graph projection.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReturnGraph {
	pub contract: Option<GraphContract>,
	pub root: Option<Expr>,
	pub nodes: Expr,
	pub edges: Expr,
	pub span: TextSpan,
}

/// Source consumed by `BUILD GRAPH`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BuildGraphSource {
	Match(Box<MatchStatement>),
	Select(Box<SelectStatement>),
	Traverse(Box<TraverseStatement>),
	Expression(Expr),
}

/// Atomic named build or anonymous owned graph result.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BuildGraphStatement {
	pub mode: DefinitionMode,
	pub contract: Option<GraphContract>,
	pub name: Option<Identifier>,
	pub source: BuildGraphSource,
	pub root: Option<Expr>,
	pub nodes: Option<Expr>,
	pub edges: Option<Expr>,
	pub limit_nodes: Option<u64>,
	pub limit_edges: Option<u64>,
	pub timeout: Option<TimeoutValue>,
}

impl Parser<'_> {
	pub(in crate::syn) async fn return_graph(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<ReturnGraph> {
		let start = self.previous_span();
		let contract = self.optional_angle_graph_contract()?;
		let root = if self.consume_keyword(keywords::ROOT).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let open = self.expect_symbol(Symbol::LeftBrace, "Expected `{` in RETURN GRAPH.")?;
		self.expect_keyword(keywords::NODES, "Expected `nodes` in RETURN GRAPH.")?;
		self.expect_symbol(Symbol::Colon, "Expected `:` after RETURN GRAPH nodes.")?;
		let nodes = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		self.expect_symbol(Symbol::Comma, "Expected `,` after RETURN GRAPH nodes.")?;
		self.expect_keyword(keywords::EDGES, "Expected `edges` in RETURN GRAPH.")?;
		self.expect_symbol(Symbol::Colon, "Expected `:` after RETURN GRAPH edges.")?;
		let edges = context.run(|context| self.expression(context, 0, 1, 0)).await?;
		let close = self.close(Symbol::RightBrace, open.span, "`}`")?;
		Ok(ReturnGraph {
			contract,
			root,
			nodes,
			edges,
			span: cover(start, close.span),
		})
	}

	pub(in crate::syn) async fn build_graph_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<BuildGraphStatement> {
		let mode = self.definition_mode()?;
		let contract = self.optional_angle_graph_contract()?;
		let name = if self.at_keyword(keywords::FROM) {
			None
		} else {
			Some(self.identifier()?)
		};
		self.expect_keyword(keywords::FROM, "Expected `FROM` in BUILD GRAPH.")?;
		let source = context.run(|context| self.build_graph_source(context)).await?;
		let mut root = None;
		let mut nodes = None;
		let mut edges = None;
		let mut limit_nodes = None;
		let mut limit_edges = None;
		let mut timeout = None;
		while !self.at_statement_end() && !self.at_symbol(Symbol::RightParen) {
			if self.consume_keyword(keywords::ROOT).is_some() {
				if root.is_some() {
					return Err(
						self.syntax("BUILD GRAPH ROOT may appear only once.", self.previous_span())
					);
				}
				root = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else if self.consume_keyword(keywords::NODES).is_some() {
				if nodes.is_some() {
					return Err(self
						.syntax("BUILD GRAPH NODES may appear only once.", self.previous_span()));
				}
				nodes = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else if self.consume_keyword(keywords::EDGES).is_some() {
				if edges.is_some() {
					return Err(self
						.syntax("BUILD GRAPH EDGES may appear only once.", self.previous_span()));
				}
				edges = Some(context.run(|context| self.expression(context, 0, 1, 0)).await?);
			} else if self.consume_keyword(keywords::LIMIT).is_some() {
				if self.consume_keyword(keywords::NODES).is_some() {
					if limit_nodes.replace(self.unsigned_integer()?).is_some() {
						return Err(self.syntax(
							"BUILD GRAPH LIMIT NODES may appear only once.",
							self.previous_span(),
						));
					}
				} else if self.consume_keyword(keywords::EDGES).is_some() {
					if limit_edges.replace(self.unsigned_integer()?).is_some() {
						return Err(self.syntax(
							"BUILD GRAPH LIMIT EDGES may appear only once.",
							self.previous_span(),
						));
					}
				} else {
					return Err(self.syntax(
						"Expected `NODES` or `EDGES` after BUILD GRAPH LIMIT.",
						self.current().span,
					));
				}
			} else if self.consume_keyword(keywords::TIMEOUT).is_some() {
				if timeout.replace(self.timeout_value()?).is_some() {
					return Err(self.syntax(
						"BUILD GRAPH TIMEOUT may appear only once.",
						self.previous_span(),
					));
				}
			} else {
				return Err(self.syntax("Unexpected BUILD GRAPH clause.", self.current().span));
			}
		}
		Ok(BuildGraphStatement {
			mode,
			contract,
			name,
			source,
			root,
			nodes,
			edges,
			limit_nodes,
			limit_edges,
			timeout,
		})
	}

	async fn build_graph_source(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<BuildGraphSource> {
		let parenthesized = self.consume_symbol(Symbol::LeftParen);
		let source = if self.consume_keyword(keywords::MATCH).is_some() {
			BuildGraphSource::Match(Box::new(
				context.run(|context| self.match_statement(context, false)).await?,
			))
		} else if self.consume_keyword(keywords::SELECT).is_some() {
			BuildGraphSource::Select(Box::new(
				context.run(|context| self.select_statement(context)).await?,
			))
		} else if self.consume_keyword(keywords::TRAVERSE).is_some() {
			BuildGraphSource::Traverse(Box::new(
				context.run(|context| self.traverse_statement(context)).await?,
			))
		} else {
			BuildGraphSource::Expression(
				context.run(|context| self.expression(context, 0, 1, 0)).await?,
			)
		};
		if let Some(open) = parenthesized {
			self.close(Symbol::RightParen, open.span, "`)`")?;
		}
		Ok(source)
	}

	pub(in crate::syn) fn optional_angle_graph_contract(
		&mut self,
	) -> FerrosGrynnResult<Option<GraphContract>> {
		let Some(open) = self.consume_symbol(Symbol::Less) else {
			return Ok(None);
		};
		let contract = self.graph_contract_until(Some(Symbol::Greater), open.span)?;
		let close = self.close(Symbol::Greater, open.span, "`>`")?;
		Ok(Some(GraphContract {
			span: cover(open.span, close.span),
			..contract
		}))
	}

	pub(in crate::syn) fn graph_contract_until(
		&mut self,
		terminator: Option<Symbol>,
		start: TextSpan,
	) -> FerrosGrynnResult<GraphContract> {
		let mut family = None;
		let mut validators = Vec::new();
		while terminator.is_none_or(|symbol| !self.at_symbol(symbol))
			&& !self.at_statement_end()
			&& !self.at_keyword(keywords::ROOT)
			&& !self.at_keyword(keywords::CONTENT)
			&& !self.at_keyword(keywords::CHANGEFEED)
		{
			let token = self.tokens.next();
			let TokenKind::Word {
				value,
				quoted: false,
			} = token.kind
			else {
				return Err(self.syntax("Expected a graph family or validator.", token.span));
			};
			let value = value.as_ref();
			let parsed_family = if value.eq_ignore_ascii_case(keywords::GRAPH) {
				Some(GraphFamilyName::Graph)
			} else if value.eq_ignore_ascii_case(keywords::TREE) {
				Some(GraphFamilyName::Tree)
			} else if value.eq_ignore_ascii_case(keywords::ROOTED_TREE) {
				Some(GraphFamilyName::RootedTree)
			} else if value.eq_ignore_ascii_case(keywords::FOREST) {
				Some(GraphFamilyName::Forest)
			} else if value.eq_ignore_ascii_case(keywords::DAG) {
				Some(GraphFamilyName::Dag)
			} else if value.eq_ignore_ascii_case(keywords::HYPER) {
				Some(GraphFamilyName::Hyper)
			} else if value.eq_ignore_ascii_case(keywords::META) {
				Some(GraphFamilyName::Meta)
			} else {
				None
			};
			if let Some(parsed) = parsed_family {
				if family.replace(parsed).is_some() {
					return Err(
						self.syntax("A graph contract accepts at most one family.", token.span)
					);
				}
				continue;
			}
			let validator = if value.eq_ignore_ascii_case(keywords::SIMPLE) {
				GraphValidatorName::Simple
			} else if value.eq_ignore_ascii_case(keywords::BIPARTITE) {
				GraphValidatorName::Bipartite
			} else if value.eq_ignore_ascii_case(keywords::COMPLETE) {
				GraphValidatorName::Complete
			} else {
				return Err(self.syntax("Unknown graph family or validator.", token.span));
			};
			if validators.contains(&validator) {
				return Err(self.syntax("A graph validator may appear only once.", token.span));
			}
			validators.push(validator);
		}
		validators.sort_unstable();
		Ok(GraphContract {
			family,
			validators,
			span: cover(start, self.previous_span()),
		})
	}

	fn unsigned_integer(&mut self) -> FerrosGrynnResult<u64> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax("Expected an unsigned integer.", token.span));
		};
		if value.contains(['.', 'e', 'E', '+', '-']) {
			return Err(self.syntax("Expected an unsigned integer.", token.span));
		}
		value
			.parse::<u64>()
			.map_err(|_error| self.syntax("Unsigned integer is out of range.", token.span))
	}
}
