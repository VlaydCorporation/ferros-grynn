//! Parsing of orthogonal MATCH path constraints.

use super::*;

impl Parser<'_> {
	/// Parses an optional angle-delimited global constraint block.
	pub(super) fn global_match_constraints(&mut self) -> FerrosGrynnResult<MatchConstraints> {
		if self.consume_keyword(keywords::CONSTRAINTS).is_none() {
			return Ok(MatchConstraints::default());
		}
		let open = self.expect_symbol(Symbol::Less, "Expected `<` after MATCH CONSTRAINTS.")?;
		let constraints = self.match_constraints_until(Some(Symbol::Greater))?;
		self.close(Symbol::Greater, open.span, "`>`")?;
		Ok(constraints)
	}

	/// Parses local connector constraints until no recognized term remains.
	pub(super) fn local_match_constraints(
		&mut self,
	) -> FerrosGrynnResult<(MatchConstraints, Option<Identifier>)> {
		let constraints = self.match_constraints_until(None)?;
		let custom_cost = if self.consume_keyword(keywords::COST).is_some() {
			Some(self.identifier()?)
		} else {
			None
		};
		Ok((constraints, custom_cost))
	}

	fn match_constraints_until(
		&mut self,
		terminator: Option<Symbol>,
	) -> FerrosGrynnResult<MatchConstraints> {
		let mut constraints = MatchConstraints::default();
		loop {
			if terminator.is_some_and(|symbol| self.at_symbol(symbol)) {
				break;
			}
			let mode = if self.consume_keyword(keywords::WALK).is_some() {
				Some(MatchPathMode::Walk)
			} else if self.consume_keyword(keywords::TRAIL).is_some() {
				Some(MatchPathMode::Trail)
			} else if self.consume_keyword(keywords::ACYCLIC).is_some() {
				Some(MatchPathMode::Acyclic)
			} else if self.consume_keyword(keywords::SIMPLE).is_some() {
				Some(MatchPathMode::Simple)
			} else {
				None
			};
			if let Some(mode) = mode {
				if constraints.mode.replace(mode).is_some() {
					return Err(self.syntax(
						"A MATCH constraint scope accepts only one path mode.",
						self.previous_span(),
					));
				}
				continue;
			}

			let objective = if self.consume_keyword(keywords::SHORTEST).is_some() {
				Some(MatchPathObjective::Shortest)
			} else if self.at_keyword(keywords::WEIGHTED)
				&& matches!(
					&self.tokens.peek(1).kind,
					TokenKind::Word {
						value,
						quoted: false,
					} if value.eq_ignore_ascii_case(keywords::SHORTEST)
				) {
				self.tokens.next();
				self.tokens.next();
				Some(MatchPathObjective::WeightedShortest)
			} else {
				None
			};
			if let Some(objective) = objective {
				if constraints.objective.replace(objective).is_some() {
					return Err(self.syntax(
						"A MATCH constraint scope accepts only one path objective.",
						self.previous_span(),
					));
				}
				continue;
			}

			let filter = if self.consume_keyword(keywords::WEIGHTED).is_some() {
				Some(MatchPathFilter::Weighted)
			} else if self.consume_keyword(keywords::META).is_some() {
				Some(MatchPathFilter::Meta)
			} else if self.consume_keyword(keywords::METAVERTICES).is_some() {
				Some(MatchPathFilter::MetaVertices)
			} else if self.consume_keyword(keywords::METAEDGES).is_some() {
				Some(MatchPathFilter::MetaEdges)
			} else if self.consume_keyword(keywords::HYPEREDGES).is_some() {
				Some(MatchPathFilter::HyperEdges)
			} else if self.consume_keyword(keywords::CROSS_LEVEL).is_some() {
				Some(MatchPathFilter::CrossLevel)
			} else {
				None
			};
			if let Some(filter) = filter {
				if constraints.filters.contains(&filter) {
					return Err(self.syntax(
						"A MATCH constraint filter cannot be repeated in one scope.",
						self.previous_span(),
					));
				}
				constraints.filters.push(filter);
				continue;
			}
			break;
		}
		constraints.filters.sort_unstable();
		Ok(constraints)
	}
}
