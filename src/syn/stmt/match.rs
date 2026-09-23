#![allow(
	clippy::wildcard_imports,
	reason = "move-only statement split keeps the existing parser imports during stage 1"
)]

use super::*;

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStatement {
	pub atom: bool,
	pub patterns: Vec<MatchPattern>,
	pub source: Identifier,
	pub filter: Option<Expr>,
	pub projection: Vec<MatchReturnItem>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchPattern {
	pub path_alias: Option<Identifier>,
	pub start: MatchEndpoint,
	pub steps: Vec<MatchStep>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEndpoint {
	pub kind: MatchEndpointKind,
	pub selector: MatchSelector,
	pub alias: Option<Identifier>,
	pub label: Option<Identifier>,
	pub span: TextSpan,
}

/// Class, exact record, or wildcard selected by one graph-pattern position.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MatchSelector {
	Any(TextSpan),
	Class(Identifier),
	Record {
		class: Identifier,
		key: Expr,
		span: TextSpan,
	},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatchEndpointKind {
	Node,
	Edge,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStep {
	pub direction: MatchDirection,
	pub edge: MatchEdge,
	pub target: MatchEndpoint,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEdge {
	pub selector: MatchSelector,
	pub alias: Option<Identifier>,
	pub label: Option<Identifier>,
	pub length: Option<MatchLength>,
	pub span: TextSpan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MatchDirection {
	Forward,
	Backward,
	Undirected,
	Bidirectional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchLength {
	pub min: u32,
	pub max: u32,
	pub span: TextSpan,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchReturnItem {
	pub expr: Expr,
	pub alias: Option<Identifier>,
	pub span: TextSpan,
}

impl Parser<'_> {
	/// Parses the approved read-only MATCH statement profile.
	pub(in crate::syn) async fn match_statement(
		&mut self,
		context: &mut Stk,
	) -> FerrosGrynnResult<MatchStatement> {
		if let Some(feature) = self.unsupported_match_modifier() {
			return Err(self.unsupported(feature, self.current().span));
		}
		let atom = self.consume_keyword(keywords::ATOM).is_some();
		if let Some(feature) = self.unsupported_match_modifier() {
			return Err(self.unsupported(feature, self.current().span));
		}

		let mut patterns = Vec::new();
		loop {
			patterns.push(self.match_pattern(context, atom).await?);
			if self.consume_symbol(Symbol::Comma).is_none() {
				break;
			}
			if self.at_keyword(keywords::IN) {
				return Err(self.syntax("Expected a MATCH pattern after `,`.", self.current().span));
			}
		}
		if let Some(feature) = self.unsupported_match_modifier() {
			return Err(self.unsupported(feature, self.current().span));
		}
		self.expect_keyword(keywords::IN, "Expected `IN GRAPH graph_name` after MATCH patterns.")?;
		self.expect_keyword(keywords::GRAPH, "Expected `GRAPH` after `IN` in MATCH statement.")?;
		let source = self.identifier()?;
		let filter = if self.consume_keyword(keywords::WHERE).is_some() {
			Some(context.run(|context| self.expression(context, 0, 1, 0)).await?)
		} else {
			None
		};
		self.expect_keyword(keywords::RETURN, "Expected `RETURN` in MATCH statement.")?;
		if let Some(token) = self.consume_keyword(keywords::DISTINCT) {
			return Err(self.unsupported("RETURN DISTINCT", token.span));
		}
		if self.at_keyword(keywords::GRAPH) {
			return Err(self.unsupported("graph-return", self.current().span));
		}
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
		Ok(MatchStatement {
			atom,
			patterns,
			source,
			filter,
			projection,
		})
	}

	/// Parses one StandardPath or AtomWalk chain.
	async fn match_pattern(
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
		let mut steps = Vec::new();
		while self.at_match_step(atom) {
			steps.push(if atom {
				self.atom_match_step(context).await?
			} else {
				self.standard_match_step(context).await?
			});
		}
		let end = steps.last().map_or(start.span, |step| step.span);
		Ok(MatchPattern {
			path_alias,
			start,
			steps,
			span: cover(pattern_start, end),
		})
	}

	/// Parses one node or explicit AtomWalk edge endpoint.
	async fn match_endpoint(
		&mut self,
		context: &mut Stk,
		atom: bool,
	) -> FerrosGrynnResult<MatchEndpoint> {
		let outer = atom
			.then(|| self.expect_symbol(Symbol::Less, "Expected `<` before AtomWalk endpoint."))
			.transpose()?;
		let open = self.tokens.next();
		let (kind, close_symbol, close_name) = match open.kind {
			TokenKind::Symbol(Symbol::LeftParen) => {
				(MatchEndpointKind::Node, Symbol::RightParen, "`)`")
			}
			TokenKind::Symbol(Symbol::LeftBracket) if atom => {
				(MatchEndpointKind::Edge, Symbol::RightBracket, "`]`")
			}
			_ if atom => {
				return Err(self.syntax("Expected `(` or `[` inside AtomWalk endpoint.", open.span));
			}
			_ => return Err(self.syntax("Expected `(` to start MATCH endpoint.", open.span)),
		};
		let (selector, alias, label) = self.match_binding(context).await?;
		if self.at_symbol(Symbol::LeftBrace) {
			return Err(self.unsupported("inline MATCH property objects", self.current().span));
		}
		if let Some(feature) = self.unsupported_match_modifier() {
			return Err(self.unsupported(feature, self.current().span));
		}
		let close = self.close(close_symbol, open.span, close_name)?;
		let end = if atom {
			self.expect_symbol(Symbol::Greater, "Expected `>` after AtomWalk endpoint.")?
		} else {
			close
		};
		Ok(MatchEndpoint {
			kind,
			selector,
			alias,
			label,
			span: cover(outer.as_ref().map_or(open.span, |token| token.span), end.span),
		})
	}

	/// Parses a required selector followed by independent alias and label
	/// clauses.
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

	/// Parses the wildcard, class, or exact RecordId at a graph-pattern
	/// position.
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

	/// Parses one StandardPath connector and target endpoint.
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
			if self.at_symbol(Symbol::LeftBrace) {
				return Err(self.unsupported("inline MATCH property objects", self.current().span));
			}
			if let Some(feature) = self.unsupported_match_modifier() {
				return Err(self.unsupported(feature, self.current().span));
			}
			self.close(Symbol::RightBracket, open.span, "`]`")?;
			let suffix = self.tokens.next();
			let suffix_symbol = match suffix.kind {
				TokenKind::Symbol(Symbol::Minus) => Symbol::Minus,
				TokenKind::Symbol(Symbol::ArrowRight) => Symbol::ArrowRight,
				_ => {
					return Err(self.syntax("Expected `-` or `->` after MATCH edge.", suffix.span));
				}
			};
			let direction = match (prefix_symbol, suffix_symbol) {
				(Symbol::Minus, Symbol::ArrowRight) => MatchDirection::Forward,
				(Symbol::ArrowLeft, Symbol::Minus) => MatchDirection::Backward,
				(Symbol::Minus, Symbol::Minus) => MatchDirection::Undirected,
				(Symbol::ArrowLeft, Symbol::ArrowRight) => MatchDirection::Bidirectional,
				_ => {
					return Err(self.syntax("Invalid StandardPath direction.", suffix.span));
				}
			};
			let span = cover(prefix.span, suffix.span);
			(direction, MatchEdge {
				selector,
				alias,
				label,
				length,
				span,
			})
		} else {
			let direction = match prefix_symbol {
				Symbol::ArrowRight => MatchDirection::Forward,
				Symbol::ArrowLeft => MatchDirection::Backward,
				Symbol::Minus => MatchDirection::Undirected,
				Symbol::ArrowBoth => MatchDirection::Bidirectional,
				_ => return Err(self.syntax("Expected a StandardPath edge.", prefix.span)),
			};
			(direction, MatchEdge {
				selector: MatchSelector::Any(prefix.span),
				alias: None,
				label: None,
				length: None,
				span: prefix.span,
			})
		};
		let target = self.match_endpoint(context, false).await?;
		Ok(MatchStep {
			direction,
			span: cover(edge.span, target.span),
			edge,
			target,
		})
	}

	/// Parses one AtomWalk connector and target endpoint.
	async fn atom_match_step(&mut self, context: &mut Stk) -> FerrosGrynnResult<MatchStep> {
		let prefix = self.tokens.next();
		let left = match prefix.kind {
			TokenKind::Symbol(Symbol::TildeLeftBracket) => false,
			TokenKind::Symbol(Symbol::LeftTildeLeftBracket) => true,
			_ => return Err(self.syntax("Expected an AtomWalk `~[...]~` edge.", prefix.span)),
		};
		let (selector, alias, label, _length) = self.match_edge_binding(context, false).await?;
		if self.at_symbol(Symbol::LeftBrace) {
			return Err(self.unsupported("inline MATCH property objects", self.current().span));
		}
		if let Some(feature) = self.unsupported_match_modifier() {
			return Err(self.unsupported(feature, self.current().span));
		}
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
			length: None,
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

	/// Parses connector alias, labels, and an optional finite range.
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
				return Err(self.unsupported("AtomWalk variable paths", star.span));
			}
			Some(self.match_length(star.span)?)
		} else {
			None
		};
		Ok((selector, alias, label, length))
	}

	/// Parses a finite exact or bounded StandardPath length.
	fn match_length(&mut self, star: TextSpan) -> FerrosGrynnResult<MatchLength> {
		let first = if matches!(&self.current().kind, TokenKind::Number(_)) {
			Some(self.match_length_bound()?)
		} else {
			None
		};
		if self.consume_symbol(Symbol::Range).is_some() {
			let Some((max, max_span)) = (matches!(&self.current().kind, TokenKind::Number(_)))
				.then(|| self.match_length_bound())
				.transpose()?
			else {
				return Err(self.unsupported("unbounded MATCH paths", self.current().span));
			};
			let min = first.map_or(1, |(value, _span)| value);
			return Ok(MatchLength {
				min,
				max,
				span: cover(star, max_span),
			});
		}
		let Some((value, value_span)) = first else {
			return Err(self.unsupported("unbounded MATCH paths", star));
		};
		Ok(MatchLength {
			min: value,
			max: value,
			span: cover(star, value_span),
		})
	}

	/// Parses one unsigned path-length bound with its source span.
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

	/// Reports whether the current token starts another MATCH step.
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

	/// Classifies deferred MATCH syntax for typed diagnostics.
	fn unsupported_match_modifier(&self) -> Option<&'static str> {
		keywords::UNSUPPORTED_MATCH_MODIFIERS
			.iter()
			.copied()
			.find(|keyword| self.at_keyword(keyword))
	}
}
