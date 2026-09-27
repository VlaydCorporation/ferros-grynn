#![allow(
	clippy::wildcard_imports,
	reason = "MATCH syntax shares the parser vocabulary while remaining isolated from analysis"
)]

use super::*;

mod constraints;
mod model;

pub use model::*;

impl Parser<'_> {
	/// Parses the canonical StandardPath and graph-first MATCH syntax.
	pub(in crate::syn) async fn match_statement(
		&mut self,
		context: &mut Stk,
		optional: bool,
	) -> FerrosGrynnResult<MatchStatement> {
		if self.at_keyword(keywords::OPTIONAL) {
			return Err(self.syntax(
				"Use `OPTIONAL MATCH`; `MATCH OPTIONAL` is not valid.",
				self.current().span,
			));
		}
		let binding_distinct = self.consume_keyword(keywords::DISTINCT).is_some();
		let atom = self.consume_keyword(keywords::ATOM).is_some();
		let mode = self.match_mode()?;
		let hyper = self.consume_keyword(keywords::HYPER).is_some();
		let meta_scope = self.match_meta_scope();
		let constraints = self.global_match_constraints()?;

		let mut patterns = Vec::new();
		loop {
			patterns.push(if hyper {
				MatchPattern::Hyper(self.match_hyper_pattern(context, atom).await?)
			} else {
				self.match_linear_or_within_pattern(context, atom).await?
			});
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
			if self.at_keyword(keywords::IN) {
				return Err(self.syntax("Expected a MATCH pattern after `,`.", self.current().span));
			}
		}
		self.expect_keyword(keywords::IN, "Expected `IN GRAPH graph_ref` after MATCH patterns.")?;
		self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN` in MATCH statement.")?;
		let source = self.graph_reference_expr()?;
		let filter = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let max_depth = if self.consume_keyword(keywords::MAXDEPTH).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let timeout = if self.consume_keyword(keywords::TIMEOUT).is_some() {
			Some(self.timeout_value()?)
		} else {
			None
		};
		self.expect_keyword(keywords::RETURN, "Expected `RETURN` in MATCH statement.")?;
		if self.at_keyword(keywords::GRAPH) {
			return Err(self.unsupported("graph-return", self.current().span));
		}
		let projection_distinct = self.consume_keyword(keywords::DISTINCT).is_some();
		let projection = self.match_projection(context).await?;
		let order = if self.consume_keyword(keywords::ORDER).is_some() {
			self.expect_keyword(keywords::BY, "Expected `BY` after MATCH ORDER.")?;
			self.select_order_list(context).await?
		} else {
			Vec::new()
		};
		Ok(MatchStatement {
			optional,
			binding_distinct,
			atom,
			mode,
			hyper,
			meta_scope,
			constraints,
			patterns,
			source,
			filter,
			max_depth,
			timeout,
			projection_distinct,
			projection,
			order,
		})
	}

	fn match_mode(&mut self) -> FerrosGrynnResult<MatchMode> {
		if self.consume_keyword(keywords::REPEATABLE).is_some() {
			self.expect_keyword(keywords::ELEMENTS, "Expected `ELEMENTS` after `REPEATABLE`.")?;
			Ok(MatchMode::RepeatableElements)
		} else if self.consume_keyword(keywords::DIFFERENT).is_some() {
			if self.consume_keyword(keywords::EDGES).is_some() {
				Ok(MatchMode::DifferentEdges)
			} else if self.consume_keyword(keywords::ATOMS).is_some() {
				Ok(MatchMode::DifferentAtoms)
			} else {
				Err(self
					.syntax("Expected `EDGES` or `ATOMS` after `DIFFERENT`.", self.current().span))
			}
		} else {
			Ok(MatchMode::DifferentEdges)
		}
	}

	fn match_meta_scope(&mut self) -> MatchMetaScope {
		if self.consume_keyword(keywords::META).is_some() {
			MatchMetaScope::Meta
		} else if self.consume_keyword(keywords::METAVERTEX).is_some() {
			MatchMetaScope::MetaVertex
		} else if self.consume_keyword(keywords::METAEDGE).is_some() {
			MatchMetaScope::MetaEdge
		} else {
			MatchMetaScope::None
		}
	}

	async fn match_projection(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<Vec<MatchReturnItem>> {
		let mut projection = Vec::new();
		loop {
			let expr = context.run(|context| self.expression(context, 0, 1, 0)).await?;
			let alias = if self.consume_keyword(keywords::AS).is_some() {
				Some(self.identifier()?)
			} else {
				None
			};
			let span = alias.as_ref().map_or(expr.span, |alias| cover(expr.span, alias.span));
			projection.push(MatchReturnItem {
				expr,
				alias,
				span,
			});
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
		}
		Ok(projection)
	}

	async fn match_linear_or_within_pattern(
		&mut self,
		context: &mut Stk,
		atom: bool,
	) -> FerrosGrynnResult<MatchPattern> {
		let path_alias = if matches!(&self.current().kind, TokenKind::Word { .. })
			&& matches!(&self.tokens.peek(1).kind, TokenKind::Symbol(Symbol::Equal))
		{
			let alias = self.identifier()?;
			self.expect_symbol(Symbol::Equal, "Expected `=` after path alias.")?;
			Some(alias)
		} else {
			None
		};
		let start = self.match_endpoint(context, atom).await?;
		let pattern_start = path_alias.as_ref().map_or(start.span, |alias| alias.span);
		if self.consume_keyword(keywords::WITHIN).is_some() {
			if path_alias.is_some() {
				return Err(self.syntax("WITHIN is not a traversal Path.", pattern_start));
			}
			let depth = self.match_within_depth()?;
			let owner = self.match_endpoint(context, atom).await?;
			return Ok(MatchPattern::Within(MatchWithinPattern {
				span: cover(pattern_start, owner.span),
				child: start,
				owner,
				depth,
			}));
		}
		let mut steps = Vec::new();
		while self.at_match_step(atom) {
			steps.push(if atom {
				self.atom_match_step(context).await?
			} else {
				self.standard_match_step(context).await?
			});
		}
		let end = steps.last().map_or(start.span, |step| step.span);
		Ok(MatchPattern::Linear(MatchLinearPattern {
			path_alias,
			start,
			steps,
			span: cover(pattern_start, end),
		}))
	}

	async fn match_hyper_pattern(
		&mut self,
		context: &mut Stk,
		atom: bool,
	) -> FerrosGrynnResult<MatchHyperPattern> {
		let edge = self.match_bare_edge_endpoint(context).await?;
		self.expect_keyword(keywords::CONTAINS, "Expected `CONTAINS` after a HYPER edge.")?;
		let mut contains = Vec::new();
		loop {
			let start = self.current().span;
			let side = if self.consume_keyword(keywords::HEAD).is_some() {
				MatchHyperSide::Head
			} else if self.consume_keyword(keywords::TAIL).is_some() {
				MatchHyperSide::Tail
			} else {
				MatchHyperSide::Any
			};
			let open =
				self.expect_symbol(Symbol::LeftBracket, "Expected `[` after HYPER CONTAINS.")?;
			let mut endpoints = Vec::new();
			loop {
				endpoints.push(self.match_endpoint(context, atom).await?);
				if self.consume_symbol(Symbol::Comma).is_none() {
					break;
				}
			}
			let close = self.close(Symbol::RightBracket, open.span, "`]`")?;
			contains.push(MatchContains {
				side,
				endpoints,
				span: cover(start, close.span),
			});
			if self.consume_symbol(Symbol::Comma).is_none()
				|| !(self.at_keyword(keywords::HEAD)
					|| self.at_keyword(keywords::TAIL)
					|| self.at_symbol(Symbol::LeftBracket))
			{
				break;
			}
		}
		let end = contains.last().map_or(edge.span, |group| group.span);
		Ok(MatchHyperPattern {
			span: cover(edge.span, end),
			edge,
			contains,
		})
	}

	fn match_within_depth(&mut self) -> FerrosGrynnResult<MatchWithinDepth> {
		let start = self.previous_span();
		if self.consume_symbol(Symbol::Star).is_none() {
			return Ok(MatchWithinDepth {
				min: 1,
				max: Some(1),
				span: start,
			});
		}
		let open = self.expect_symbol(Symbol::LeftBracket, "Expected `[` after `WITHIN*`.")?;
		let (min, _) = self.match_length_bound()?;
		self.expect_symbol(Symbol::Range, "Expected `..` in a WITHIN depth range.")?;
		let max = if matches!(&self.current().kind, TokenKind::Number(_)) {
			Some(self.match_length_bound()?.0)
		} else {
			None
		};
		let close = self.close(Symbol::RightBracket, open.span, "`]`")?;
		Ok(MatchWithinDepth {
			min,
			max,
			span: cover(start, close.span),
		})
	}

	async fn match_endpoint(
		&mut self,
		context: &mut Stk,
		atom: bool,
	) -> FerrosGrynnResult<MatchEndpoint> {
		let outer = if atom {
			Some(self.expect_symbol(Symbol::Less, "Expected `<` before AtomWalk endpoint.")?)
		} else {
			self.consume_symbol(Symbol::Less)
		};
		let meta = outer.is_some() && !atom;
		let open = self.tokens.next();
		let (kind, close_symbol, close_name) = match open.kind {
			TokenKind::Symbol(Symbol::LeftParen) => {
				(MatchEndpointKind::Node, Symbol::RightParen, "`)`")
			}
			TokenKind::Symbol(Symbol::LeftBracket) if atom || meta => {
				(MatchEndpointKind::Edge, Symbol::RightBracket, "`]`")
			}
			_ if atom || meta => {
				return Err(self.syntax("Expected `(` or `[` inside MATCH endpoint.", open.span));
			}
			_ => return Err(self.syntax("Expected `(` to start MATCH endpoint.", open.span)),
		};
		let (selector, alias, label) = self.match_binding(context).await?;
		let (properties, predicate) = self.match_inline_filters(context).await?;
		let close = self.close(close_symbol, open.span, close_name)?;
		let end = if outer.is_some() {
			self.expect_symbol(Symbol::Greater, "Expected `>` after MATCH endpoint.")?
		} else {
			close
		};
		Ok(MatchEndpoint {
			kind,
			selector,
			alias,
			label,
			properties,
			predicate,
			meta,
			span: cover(outer.as_ref().map_or(open.span, |token| token.span), end.span),
		})
	}

	async fn match_bare_edge_endpoint(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<MatchEndpoint> {
		let open =
			self.expect_symbol(Symbol::LeftBracket, "Expected `[` to start a HYPER edge.")?;
		let (selector, alias, label) = self.match_binding(context).await?;
		let (properties, predicate) = self.match_inline_filters(context).await?;
		let close = self.close(Symbol::RightBracket, open.span, "`]`")?;
		Ok(MatchEndpoint {
			kind: MatchEndpointKind::Edge,
			selector,
			alias,
			label,
			properties,
			predicate,
			meta: false,
			span: cover(open.span, close.span),
		})
	}

	async fn match_inline_filters(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<(Option<Expr>, Option<Expr>)> {
		let properties = if self.at_symbol(Symbol::LeftBrace) {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		let predicate = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		Ok((properties, predicate))
	}

	async fn match_binding(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<(MatchSelector, Option<Identifier>, Option<Identifier>)> {
		let selector = self.match_selector(context).await?;
		let alias = if self.consume_keyword(keywords::AS).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		let label =
			self.consume_keyword(keywords::LABEL).map(|_token| self.identifier()).transpose()?;
		Ok((selector, alias, label))
	}

	async fn match_selector(&mut self, context: &mut Stk) -> FerrosGrynnResult<MatchSelector> {
		if let Some(star) = self.consume_symbol(Symbol::Star) {
			return Ok(MatchSelector::Any(star.span));
		}
		match self.class_or_record_target(context).await? {
			DmlTarget::Class(class) => Ok(MatchSelector::Class(class)),
			DmlTarget::Record {
				class,
				key,
				span,
			} => Ok(MatchSelector::Record {
				class,
				key,
				span,
			}),
			DmlTarget::Range {
				span,
				..
			} => Err(self.syntax("Record generators are not valid MATCH selectors.", span)),
		}
	}

	async fn standard_match_step(&mut self, context: &mut Stk) -> FerrosGrynnResult<MatchStep> {
		let prefix = self.tokens.next();
		let TokenKind::Symbol(prefix_symbol) = prefix.kind else {
			return Err(self.syntax("Expected a StandardPath edge.", prefix.span));
		};
		let bracketed = matches!(prefix_symbol, Symbol::Minus | Symbol::ArrowLeft)
			&& self.at_symbol(Symbol::LeftBracket);
		let (direction, edge) = if bracketed {
			let open = self.expect_symbol(Symbol::LeftBracket, "Expected `[` in MATCH edge.")?;
			let (selector, alias, label, length) = self.match_edge_binding(context, true).await?;
			let (properties, predicate) = self.match_inline_filters(context).await?;
			let (constraints, custom_cost) = self.local_match_constraints()?;
			self.close(Symbol::RightBracket, open.span, "`]`")?;
			let suffix = self.tokens.next();
			let suffix_symbol = match suffix.kind {
				TokenKind::Symbol(Symbol::Minus) => Symbol::Minus,
				TokenKind::Symbol(Symbol::ArrowRight) => Symbol::ArrowRight,
				_ => return Err(self.syntax("Expected `-` or `->` after MATCH edge.", suffix.span)),
			};
			let direction = match (prefix_symbol, suffix_symbol) {
				(Symbol::Minus, Symbol::ArrowRight) => MatchDirection::Forward,
				(Symbol::ArrowLeft, Symbol::Minus) => MatchDirection::Backward,
				(Symbol::Minus, Symbol::Minus) => MatchDirection::Undirected,
				(Symbol::ArrowLeft, Symbol::ArrowRight) => MatchDirection::Bidirectional,
				_ => return Err(self.syntax("Invalid StandardPath direction.", suffix.span)),
			};
			(direction, MatchEdge {
				selector,
				alias,
				label,
				properties,
				predicate,
				length,
				constraints,
				custom_cost,
				span: cover(prefix.span, suffix.span),
			})
		} else {
			let direction = match prefix_symbol {
				Symbol::ArrowRight => MatchDirection::Forward,
				Symbol::ArrowLeft => MatchDirection::Backward,
				Symbol::Minus => MatchDirection::Undirected,
				Symbol::ArrowBoth => MatchDirection::Bidirectional,
				_ => return Err(self.syntax("Expected a StandardPath edge.", prefix.span)),
			};
			(direction, self.anonymous_match_edge(prefix.span))
		};
		let target = self.match_endpoint(context, false).await?;
		Ok(MatchStep {
			direction,
			span: cover(edge.span, target.span),
			edge,
			target,
		})
	}

	async fn atom_match_step(&mut self, context: &mut Stk) -> FerrosGrynnResult<MatchStep> {
		let prefix = self.tokens.next();
		let left = match prefix.kind {
			TokenKind::Symbol(Symbol::TildeLeftBracket) => false,
			TokenKind::Symbol(Symbol::LeftTildeLeftBracket) => true,
			_ => return Err(self.syntax("Expected an AtomWalk `~[...]~` edge.", prefix.span)),
		};
		let (selector, alias, label, length) = self.match_edge_binding(context, false).await?;
		if length.is_some() {
			return Err(self.syntax("AtomWalk connectors do not accept path ranges.", prefix.span));
		}
		let (properties, predicate) = self.match_inline_filters(context).await?;
		let (constraints, custom_cost) = self.local_match_constraints()?;
		let suffix = self.tokens.next();
		let right = match suffix.kind {
			TokenKind::Symbol(Symbol::RightBracketTilde) => false,
			TokenKind::Symbol(Symbol::RightBracketTildeRight) => true,
			_ => {
				return Err(self.syntax("Expected `]~` or `]~>` after AtomWalk edge.", suffix.span));
			}
		};
		let direction = match (left, right) {
			(false, true) => MatchDirection::Forward,
			(true, false) => MatchDirection::Backward,
			(false, false) => MatchDirection::Undirected,
			(true, true) => MatchDirection::Bidirectional,
		};
		let edge = MatchEdge {
			selector,
			alias,
			label,
			properties,
			predicate,
			length: None,
			constraints,
			custom_cost,
			span: cover(prefix.span, suffix.span),
		};
		let target = self.match_endpoint(context, true).await?;
		Ok(MatchStep {
			direction,
			span: cover(edge.span, target.span),
			edge,
			target,
		})
	}

	async fn match_edge_binding(
		&mut self,
		context: &mut Stk,
		allow_length: bool,
	) -> FerrosGrynnResult<(
		MatchSelector,
		Option<Identifier>,
		Option<Identifier>,
		Option<MatchLength>,
	)> {
		let (selector, alias, label) = self.match_binding(context).await?;
		let length = if let Some(star) = self.consume_symbol(Symbol::Star) {
			if !allow_length {
				return Err(
					self.syntax("AtomWalk connectors do not accept path ranges.", star.span)
				);
			}
			Some(self.match_length(star.span)?)
		} else {
			None
		};
		Ok((selector, alias, label, length))
	}

	fn match_length(&mut self, star: TextSpan) -> FerrosGrynnResult<MatchLength> {
		let first = if matches!(&self.current().kind, TokenKind::Number(_)) {
			Some(self.match_length_bound()?)
		} else {
			None
		};
		if self.consume_symbol(Symbol::Range).is_some() {
			let maximum = if matches!(&self.current().kind, TokenKind::Number(_)) {
				Some(self.match_length_bound()?)
			} else {
				None
			};
			let min = first.map_or(1, |(value, _span)| value);
			let end = maximum.map_or_else(|| self.previous_span(), |(_value, span)| span);
			return Ok(MatchLength {
				min,
				max: maximum.map(|(value, _span)| value),
				span: cover(star, end),
			});
		}
		if let Some((value, value_span)) = first {
			return Ok(MatchLength {
				min: value,
				max: Some(value),
				span: cover(star, value_span),
			});
		}
		Ok(MatchLength {
			min: 1,
			max: None,
			span: star,
		})
	}

	fn match_length_bound(&mut self) -> FerrosGrynnResult<(u32, TextSpan)> {
		let token = self.tokens.next();
		let TokenKind::Number(value) = token.kind else {
			return Err(self.syntax("Expected a path length.", token.span));
		};
		value
			.replace('_', "")
			.parse::<u32>()
			.map(|value| (value, token.span))
			.map_err(|_error| self.syntax("Path length is out of range.", token.span))
	}

	fn anonymous_match_edge(&self, span: TextSpan) -> MatchEdge {
		MatchEdge {
			selector: MatchSelector::Any(span),
			alias: None,
			label: None,
			properties: None,
			predicate: None,
			length: None,
			constraints: MatchConstraints::default(),
			custom_cost: None,
			span,
		}
	}

	fn at_match_step(&self, atom: bool) -> bool {
		if atom {
			self.at_symbol(Symbol::TildeLeftBracket) || self.at_symbol(Symbol::LeftTildeLeftBracket)
		} else {
			self.at_symbol(Symbol::Minus)
				|| self.at_symbol(Symbol::ArrowLeft)
				|| self.at_symbol(Symbol::ArrowRight)
				|| self.at_symbol(Symbol::ArrowBoth)
		}
	}
}
