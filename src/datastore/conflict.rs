use super::{AnalyzerId, ClassId, FunctionId, GlobalAtomId, SchemaId};
use crate::graph::{AtomId, AtomKind, GraphId};

/// Independently versioned logical targets used by FCW and SSI validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ConflictKey {
	/// Append-only property-name dictionary revision.
	PropertyDictionary,
	/// One named graph definition, including catalog-owned properties.
	GraphDefinition(GraphId),
	/// Set of graph definitions observed by a catalog scan.
	GraphCatalogRange,
	/// One durable database parameter definition.
	SchemaParameter(SchemaId),
	/// Set of durable database parameters observed by a catalog scan.
	ParameterCatalogRange,
	/// One durable named-function overload.
	SchemaFunction(FunctionId),
	/// Set of durable named-function overloads observed by a catalog scan.
	FunctionCatalogRange,
	/// One durable text-analyzer definition.
	SchemaAnalyzer(AnalyzerId),
	/// Set of durable text analyzers observed by a catalog scan.
	AnalyzerCatalogRange,
	/// Database-global identity or base properties.
	BaseAtom(GlobalAtomId),
	/// Membership or value set observed by a database-global class scan.
	BaseClassRange(ClassId),
	/// Presence and local address of one atom in one graph.
	GraphMembership(GraphId, GlobalAtomId),
	/// Sparse effective-property decisions for one occurrence.
	GraphOverlay(GraphId, GlobalAtomId),
	/// Graph-local slot and, for edges, topology, weight, and transition
	/// binding.
	GraphIncidence(GraphId, AtomId),
	/// Membership or effective-value set observed by a graph-scoped class scan.
	GraphClassRange(GraphId, ClassId),
	/// MATCH candidate/topology set for one graph-local atom kind.
	GraphKindRange(GraphId, AtomKind),
}
