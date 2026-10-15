//! Syntax model for StandardPath, AtomWalk, hyper, and containment MATCH.

use super::super::*;

/// One canonical MATCH statement.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStatement {
	/// Whether unmatched inputs are retained through OPTIONAL MATCH semantics.
	pub optional: bool,
	/// Independent binding-tuple and projected-row deduplication.
	pub distinctness: MatchDistinctness,
	/// Whether graph traversal uses atom endpoints rather than node-only
	/// endpoints.
	pub atom: bool,
	/// Execution or declaration policy selected by this clause.
	pub mode: MatchMode,
	/// Whether MATCH is using the hyperedge pattern form.
	pub hyper: bool,
	/// Additional metagraph containment scope selected for matching.
	pub meta_scope: MatchMetaScope,
	/// Structural or path constraints applied to the selected graph operation.
	pub constraints: MatchConstraints,
	/// Patterns that must be satisfied in the selected graph scope.
	pub patterns: Vec<MatchPattern>,
	/// Input source or initial endpoint selected by this syntax element.
	pub source: Expr,
	/// Statement-wide default for connector path cost; local COST overrides it.
	pub custom_cost: Option<FunctionSignatureName>,
	/// Predicate or adapter deciding which input values continue downstream.
	pub filter: Option<Expr>,
	/// Maximum nested depth accepted by the operation or declaration.
	pub max_depth: Option<Expr>,
	/// Optional time limit attached to this operation.
	pub timeout: Option<TimeoutValue>,
	/// Optional graph projection replacing the ordinary value projection.
	pub return_graph: Option<ReturnGraph>,
	/// Expressions or fields selected for output.
	pub projection: Vec<MatchReturnItem>,
	/// Ordering expressions applied to the produced rows.
	pub order: Vec<SelectOrder>,
}

/// Independent DISTINCT stages in a MATCH statement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchDistinctness {
	/// Deduplicate complete binding tuples before projection.
	pub bindings: bool,
	/// Deduplicate values after projection.
	pub projection: bool,
}

impl MatchDistinctness {
	/// Selects binding-tuple and projected-row deduplication independently.
	pub const fn new(bindings: bool, projection: bool) -> Self {
		Self {
			bindings,
			projection,
		}
	}
}

/// Clause-wide element uniqueness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchMode {
	/// Permit repeated elements under the selected path constraints.
	RepeatableElements,
	/// Require edge bindings to be distinct within the match scope.
	#[default]
	DifferentEdges,
	/// Require atom bindings to be distinct within the match scope.
	DifferentAtoms,
}

/// Clause-wide meta requirement, independent of hyper-edge arity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchMetaScope {
	/// Apply no additional metagraph scope restriction.
	#[default]
	None,
	/// Select the general metagraph matching scope.
	Meta,
	/// Restrict metagraph matching to vertex containment.
	MetaVertex,
	/// Restrict metagraph matching to edge containment.
	MetaEdge,
}

/// One StandardPath/AtomWalk chain or a graph-first relation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MatchPattern {
	/// Linear endpoint-and-edge path pattern.
	Linear(
		/// Linear endpoint-and-edge path pattern.
		MatchLinearPattern,
	),
	/// Hyperedge pattern with endpoint-side constraints.
	Hyper(
		/// Hyperedge pattern with endpoint-side constraints.
		MatchHyperPattern,
	),
	/// Nested-containment pattern with a bounded depth.
	Within(
		/// Nested-containment pattern with a bounded depth.
		MatchWithinPattern,
	),
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
	/// Optional binding name assigned to the matched path.
	pub path_alias: Option<Identifier>,
	/// Initial endpoint or lower range bound selected by the syntax element.
	pub start: MatchEndpoint,
	/// Traversal or pattern steps retained in execution order.
	pub steps: Vec<MatchStep>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// One hyperedge and its endpoint assignments.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchHyperPattern {
	/// Edge target or edge pattern defining the relation incidence.
	pub edge: MatchEndpoint,
	/// Endpoint containment requirements of the hyperedge pattern.
	pub contains: Vec<MatchContains>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// One HEAD, TAIL, or unqualified hyperedge endpoint group.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchContains {
	/// Hyperedge side on which the endpoint must occur.
	pub side: MatchHyperSide,
	/// Endpoint collection defining the edge or hyperedge incidence.
	pub endpoints: Vec<MatchEndpoint>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Qualified side of a hyperedge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchHyperSide {
	/// Accept an endpoint on either hyperedge side.
	Any,
	/// Select hyperedge head endpoints.
	Head,
	/// Select hyperedge tail endpoints.
	Tail,
}

/// One direct or bounded transitive containment join.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchWithinPattern {
	/// Nested endpoint selected inside the containment pattern.
	pub child: MatchEndpoint,
	/// Cold owner endpoint, retained independently of the child position.
	pub owner: Box<MatchEndpoint>,
	/// Depth bound used by the containment pattern or graph fixture.
	pub depth: MatchWithinDepth,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Inclusive containment depth bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchWithinDepth {
	/// Inclusive lower bound accepted by the pattern or filter.
	pub min: u32,
	/// Upper bound accepted by the pattern or filter, when specified.
	pub max: Option<u32>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// One endpoint position in a graph pattern.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEndpoint {
	/// Selected operation or representation discriminator.
	pub kind: MatchEndpointKind,
	/// Unrestricted, class-based or record-based atom selection.
	pub selector: MatchSelector,
	/// Optional name assigned to the produced value or source.
	pub alias: Option<Identifier>,
	/// Optional label supplied for the created graph occurrence.
	pub label: Option<Identifier>,
	/// Optional property-pattern expression restricting the matched atom.
	pub properties: Option<Expr>,
	/// Condition restricting the values or incidences accepted by this element.
	pub predicate: Option<Expr>,
	/// Whether the endpoint participates through metagraph containment.
	pub meta: bool,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Class, exact record, or wildcard selected by one graph-pattern position.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum MatchSelector {
	/// Accept any atom satisfying the remaining pattern constraints.
	Any(
		/// Accept any atom satisfying the remaining pattern constraints.
		TextSpan,
	),
	/// Restrict the atom to a named class.
	Class(
		/// Restrict the atom to a named class.
		Identifier,
	),
	/// Restrict the atom to one class-qualified record.
	Record {
		/// Class selected by the schema declaration, query target or fixture.
		class: Identifier,
		/// Record key expression retained for target resolution.
		key: Expr,
		/// Source byte range used to report diagnostics for this syntax
		/// element.
		span: TextSpan,
	},
}

/// Domain encoded by an endpoint delimiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchEndpointKind {
	/// Endpoint pattern addresses a node atom.
	Node,
	/// Endpoint pattern addresses an edge atom.
	Edge,
}

/// One connector expansion and its target endpoint.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchStep {
	/// Traversal orientation or requested sort direction selected by this
	/// element.
	pub direction: MatchDirection,
	/// Edge target or edge pattern defining the relation incidence.
	pub edge: MatchEdge,
	/// Target endpoint or schema object selected by the operation.
	pub target: MatchEndpoint,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Connector constraints for one fixed or variable expansion.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchEdge {
	/// Unrestricted, class-based or record-based atom selection.
	pub selector: MatchSelector,
	/// Optional name assigned to the produced value or source.
	pub alias: Option<Identifier>,
	/// Optional label supplied for the created graph occurrence.
	pub label: Option<Identifier>,
	/// Optional property-pattern expression restricting the matched atom.
	pub properties: Option<Expr>,
	/// Condition restricting the values or incidences accepted by this element.
	pub predicate: Option<Expr>,
	/// Optional permitted number of hops in the matched edge pattern.
	pub length: Option<MatchLength>,
	/// Structural or path constraints applied to the selected graph operation.
	pub constraints: MatchConstraints,
	/// Optional resolved cost function used when comparing weighted paths.
	pub custom_cost: Option<FunctionSignatureName>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Physical traversal direction requested by a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchDirection {
	/// Match an incidence in its forward direction.
	Forward,
	/// Match an incidence in its reverse direction.
	Backward,
	/// Match without requiring a directed orientation.
	Undirected,
	/// Match the bidirectional incidence form.
	Bidirectional,
}

/// Inclusive path-length bounds. A missing maximum uses statement MAXDEPTH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchLength {
	/// Inclusive lower bound accepted by the pattern or filter.
	pub min: u32,
	/// Upper bound accepted by the pattern or filter, when specified.
	pub max: Option<u32>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}

/// Independent path mode, objective, and filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatchConstraints {
	/// Execution or declaration policy selected by this clause.
	pub mode: Option<MatchPathMode>,
	/// Optional hop-count or weighted shortest-path objective.
	pub objective: Option<MatchPathObjective>,
	/// Filters retained in the order required by the selected declaration.
	pub filters: Vec<MatchPathFilter>,
}

/// Uniqueness semantics of a variable path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchPathMode {
	/// Permit repeated nodes and edges.
	Walk,
	/// Reject repeated edges within a path.
	Trail,
	/// Reject repeated nodes within a path.
	Acyclic,
	/// Reject repeated nodes while permitting the final step to close a cycle.
	Simple,
}

/// Search objective of a variable path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MatchPathObjective {
	/// Select paths with the minimum hop count.
	Shortest,
	/// Select paths with the minimum accumulated weight.
	WeightedShortest,
	/// Retain all paths sharing the minimum hop count.
	AllShortest,
	/// Retain all paths sharing the minimum accumulated weight.
	AllWeightedShortest,
}

impl MatchPathObjective {
	/// Returns whether this objective preserves every minimum-cost alternative.
	pub const fn is_all(self) -> bool {
		matches!(self, Self::AllShortest | Self::AllWeightedShortest)
	}

	/// Returns whether the objective selects stored or custom numerical costs.
	pub const fn is_weighted(self) -> bool {
		matches!(self, Self::WeightedShortest | Self::AllWeightedShortest)
	}
}

/// Boolean constraint applied to candidate path branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum MatchPathFilter {
	/// Select weighted path traversal.
	Weighted,
	/// Permit metagraph containment incidences.
	Meta,
	/// Permit nested-graph incidences through vertices.
	MetaVertices,
	/// Permit nested-graph incidences through edges.
	MetaEdges,
	/// Permit hyperedge incidences.
	HyperEdges,
	/// Permit incidences crossing graph containment levels.
	CrossLevel,
}

/// One projected MATCH expression.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MatchReturnItem {
	/// Expression retained for later semantic analysis and execution.
	pub expr: Expr,
	/// Optional name assigned to the produced value or source.
	pub alias: Option<Identifier>,
	/// Source byte range used to report diagnostics for this syntax element.
	pub span: TextSpan,
}
