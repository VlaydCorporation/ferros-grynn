//! Syntax model for StandardPath, AtomWalk, hyper, and containment MATCH.

use super::super::*;

/// One canonical MATCH statement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStatement {
	pub optional: bool,
	pub binding_distinct: bool,
	pub atom: bool,
	pub mode: MatchMode,
	pub hyper: bool,
	pub meta_scope: MatchMetaScope,
	pub constraints: MatchConstraints,
	pub patterns: Vec<MatchPattern>,
	pub source: Expr,
	pub filter: Option<Expr>,
	pub max_depth: Option<Expr>,
	pub timeout: Option<TimeoutValue>,
	pub projection_distinct: bool,
	pub projection: Vec<MatchReturnItem>,
	pub order: Vec<SelectOrder>,
}

/// Clause-wide element uniqueness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchMode {
	RepeatableElements,
	#[default]
	DifferentEdges,
	DifferentAtoms,
}

/// Clause-wide meta requirement, independent of hyper-edge arity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchMetaScope {
	#[default]
	None,
	Meta,
	MetaVertex,
	MetaEdge,
}

/// One StandardPath/AtomWalk chain or a graph-first relation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MatchPattern {
	Linear(MatchLinearPattern),
	Hyper(MatchHyperPattern),
	Within(MatchWithinPattern),
}

impl MatchPattern {
	/// Returns the complete source span of this pattern.
	pub const fn span(&self) -> TextSpan {
		match self {
			Self::Linear(pattern) => pattern.span,
			Self::Hyper(pattern) => pattern.span,
			Self::Within(pattern) => pattern.span,
		}
	}
}

/// One linear StandardPath or AtomWalk chain.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchLinearPattern {
	pub path_alias: Option<Identifier>,
	pub start: MatchEndpoint,
	pub steps: Vec<MatchStep>,
	pub span: TextSpan,
}

/// One hyperedge and its endpoint assignments.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchHyperPattern {
	pub edge: MatchEndpoint,
	pub contains: Vec<MatchContains>,
	pub span: TextSpan,
}

/// One HEAD, TAIL, or unqualified hyperedge endpoint group.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchContains {
	pub side: MatchHyperSide,
	pub endpoints: Vec<MatchEndpoint>,
	pub span: TextSpan,
}

/// Qualified side of a hyperedge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchHyperSide {
	Any,
	Head,
	Tail,
}

/// One direct or bounded transitive containment join.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchWithinPattern {
	pub child: MatchEndpoint,
	pub owner: MatchEndpoint,
	pub depth: MatchWithinDepth,
	pub span: TextSpan,
}

/// Inclusive containment depth bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchWithinDepth {
	pub min: u32,
	pub max: Option<u32>,
	pub span: TextSpan,
}

/// One endpoint position in a graph pattern.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEndpoint {
	pub kind: MatchEndpointKind,
	pub selector: MatchSelector,
	pub alias: Option<Identifier>,
	pub label: Option<Identifier>,
	pub properties: Option<Expr>,
	pub predicate: Option<Expr>,
	pub meta: bool,
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

/// Domain encoded by an endpoint delimiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchEndpointKind {
	Node,
	Edge,
}

/// One connector expansion and its target endpoint.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStep {
	pub direction: MatchDirection,
	pub edge: MatchEdge,
	pub target: MatchEndpoint,
	pub span: TextSpan,
}

/// Connector constraints for one fixed or variable expansion.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEdge {
	pub selector: MatchSelector,
	pub alias: Option<Identifier>,
	pub label: Option<Identifier>,
	pub properties: Option<Expr>,
	pub predicate: Option<Expr>,
	pub length: Option<MatchLength>,
	pub constraints: MatchConstraints,
	pub custom_cost: Option<Identifier>,
	pub span: TextSpan,
}

/// Physical traversal direction requested by a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchDirection {
	Forward,
	Backward,
	Undirected,
	Bidirectional,
}

/// Inclusive path-length bounds. A missing maximum uses statement MAXDEPTH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchLength {
	pub min: u32,
	pub max: Option<u32>,
	pub span: TextSpan,
}

/// Independent path mode, objective, and filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchConstraints {
	pub mode: Option<MatchPathMode>,
	pub objective: Option<MatchPathObjective>,
	pub filters: Vec<MatchPathFilter>,
}

/// Uniqueness semantics of a variable path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchPathMode {
	Walk,
	Trail,
	Acyclic,
	Simple,
}

/// Search objective of a variable path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchPathObjective {
	Shortest,
	WeightedShortest,
}

/// Boolean constraint applied to candidate path branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum MatchPathFilter {
	Weighted,
	Meta,
	MetaVertices,
	MetaEdges,
	HyperEdges,
	CrossLevel,
}

/// One projected MATCH expression.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchReturnItem {
	pub expr: Expr,
	pub alias: Option<Identifier>,
	pub span: TextSpan,
}
