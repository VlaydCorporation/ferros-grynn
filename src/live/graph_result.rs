//! Owned logical graph terminal; execution stays in the ordinary SELECT
//! pipeline.

use super::LiveExpr;
use crate::graph::{GraphKind, GraphTopologyConstraint, GraphTopologyConstraints};

/// Graph-result selectors over the projected SELECT rows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct LiveGraphResult {
	/// Retains the concrete result family selected by the shared graph
	/// terminal.
	pub(crate) family: GraphKind,
	/// Retains the orthogonal topology contract for this scope.
	pub(crate) validators: GraphTopologyConstraints,
	/// Owns the root selector evaluated over ordinary SELECT projection rows.
	pub(crate) root: Option<LiveExpr>,
	/// Owns the nodes selector evaluated over ordinary SELECT projection rows.
	pub(crate) nodes: LiveExpr,
	/// Owns the edges selector evaluated over ordinary SELECT projection rows.
	pub(crate) edges: LiveExpr,
}

impl LiveGraphResult {
	/// Creates the default Subgraph result, matching unannotated RETURN GRAPH.
	pub const fn new(nodes: LiveExpr, edges: LiveExpr) -> Self {
		Self {
			family: GraphKind::Subgraph,
			validators: GraphTopologyConstraints::empty(),
			root: None,
			nodes,
			edges,
		}
	}

	/// Selects the exact frozen result family; registration validates its
	/// topology.
	pub const fn with_family(mut self, family: GraphKind) -> Self {
		self.family = family;
		self
	}

	/// Adds orthogonal validators in canonical order, deduplicating repeated
	/// inputs.
	pub fn with_validators(
		mut self,
		validators: impl IntoIterator<Item = GraphTopologyConstraint>,
	) -> Self {
		for validator in validators {
			self.validators.insert(validator);
		}
		self
	}

	/// Sets the authoritative ROOT expression for a RootedTree result.
	pub fn with_root(mut self, root: LiveExpr) -> Self {
		self.root = Some(root);
		self
	}

	/// Returns the selected family without evaluating selectors.
	pub const fn family(&self) -> GraphKind {
		self.family
	}

	/// Returns the canonical orthogonal contract.
	pub const fn validators(&self) -> GraphTopologyConstraints {
		self.validators
	}

	/// Borrows the optional ROOT selector.
	pub const fn root(&self) -> Option<&LiveExpr> {
		self.root.as_ref()
	}

	/// Borrows the node selector over projection aliases.
	pub const fn nodes(&self) -> &LiveExpr {
		&self.nodes
	}

	/// Borrows the edge selector over projection aliases.
	pub const fn edges(&self) -> &LiveExpr {
		&self.edges
	}

	/// Every selector participates in capture, dependency and bounded codec
	/// traversal.
	pub(crate) fn expressions(&self) -> impl Iterator<Item = &LiveExpr> {
		std::iter::once(&self.nodes).chain(std::iter::once(&self.edges)).chain(self.root.iter())
	}
}
