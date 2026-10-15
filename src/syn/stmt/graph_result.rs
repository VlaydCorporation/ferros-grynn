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
	/// General graph family.
	Graph,
	/// Connected acyclic tree family.
	Tree,
	/// Tree family with an explicitly selected root.
	RootedTree,
	/// Acyclic forest family allowing disconnected components.
	Forest,
	/// Directed acyclic graph family.
	Dag,
	/// Hypergraph family with multi-endpoint incidences.
	Hyper,
	/// Metagraph family retaining graph containment.
	Meta,
}

/// Orthogonal topology validator named by a query contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum GraphValidatorName {
	/// Require the SIMPLE graph validator.
	Simple,
	/// Require the BIPARTITE graph validator.
	Bipartite,
	/// Require the COMPLETE graph validator.
	Complete,
}

/// One normalized graph family and validator declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GraphContract {
	/// Optional graph family required by the graph contract.
	pub family: Option<GraphFamilyName>,
	/// Additional graph invariants required by the contract.
	pub validators: Vec<GraphValidatorName>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Terminal owned graph projection.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ReturnGraph {
	/// Optional external-database bridge clause attached to this graph
	/// operation.
	pub bridge: Option<Box<BridgeClause>>,
	/// Requested graph family and structural validators.
	pub contract: Option<GraphContract>,
	/// Optional expression selecting the root of the produced graph.
	pub root: Option<Expr>,
	/// Selected node identifiers or retained physical node storage.
	pub nodes: Expr,
	/// Selected edge identifiers or retained physical edge storage.
	pub edges: Expr,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Source consumed by `BUILD GRAPH`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BuildGraphSource {
	/// Use MATCH output as the graph-building source.
	Match(
		/// Use MATCH output as the graph-building source.
		Box<MatchStatement>,
	),
	/// Use SELECT output as the graph-building source.
	Select(
		/// Use SELECT output as the graph-building source.
		Box<SelectStatement>,
	),
	/// Use traversal output as the graph-building source.
	Traverse(
		/// Use traversal output as the graph-building source.
		Box<TraverseStatement>,
	),
	/// Use an evaluated expression as the graph-building source.
	Expression(
		/// Use an evaluated expression as the graph-building source.
		Expr,
	),
}

/// Atomic named build or anonymous owned graph result.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BuildGraphStatement {
	/// Optional external-database bridge clause attached to this graph
	/// operation.
	pub bridge: Option<Box<BridgeClause>>,
	/// Execution or declaration policy selected by this clause.
	pub mode: DefinitionMode,
	/// Requested graph family and structural validators.
	pub contract: Option<GraphContract>,
	/// Declared name used when resolving or displaying this element.
	pub name: Option<Identifier>,
	/// Input source or initial endpoint selected by this syntax element.
	pub source: BuildGraphSource,
	/// Optional expression selecting the root of the produced graph.
	pub root: Option<Expr>,
	/// Selected node identifiers or retained physical node storage.
	pub nodes: Option<Expr>,
	/// Selected edge identifiers or retained physical edge storage.
	pub edges: Option<Expr>,
	/// Optional upper bound on nodes admitted to the built graph.
	pub limit_nodes: Option<u64>,
	/// Optional upper bound on edges admitted to the built graph.
	pub limit_edges: Option<u64>,
	/// Optional time limit attached to this operation.
	pub timeout: Option<TimeoutValue>,
}

impl Parser<'_> {
	/// Parses the return graph syntax.
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
		let bridge = self.bridge_clause()?.map(Box::new);
		Ok(ReturnGraph {
			bridge,
			contract,
			root,
			nodes,
			edges,
			span: cover(start, close.span),
		})
	}

	/// Builds graph statement.
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
		let mut bridge = None;
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
			} else if self.at_keyword(keywords::WITH) {
				if bridge.is_some() {
					return Err(self.syntax(
						"BUILD GRAPH WITH BRIDGE may appear only once.",
						self.current().span,
					));
				}
				bridge = self.bridge_clause()?.map(Box::new);
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
			bridge,
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

	/// Builds graph source.
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

	/// Parses the optional angle graph contract syntax.
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

	/// Parses the graph contract until syntax.
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

	/// Parses the unsigned integer syntax.
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
