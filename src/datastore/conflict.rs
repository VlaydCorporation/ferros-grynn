use super::{AnalyzerId, ClassId, EventId, FunctionId, GlobalAtomId, SchemaId};
use crate::graph::{AtomId, AtomKind, GraphId};

/// Independently versioned logical targets used by FCW and SSI validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConflictKey {
	/// Registered external database namespace and alias lookups.
	BridgeDatabases,
	/// Exact reservation/confirmation domain; no lossy uniqueness-key hashes.
	BridgeMappings(
		/// Exact reservation/confirmation domain; no lossy uniqueness-key
		/// hashes.
		crate::bridge::BridgeDatabaseId,
		/// Exact reservation/confirmation domain; no lossy uniqueness-key
		/// hashes.
		crate::bridge::BridgeMappingRole,
	),
	/// Append-only property-name dictionary revision.
	PropertyDictionary,
	/// One named graph definition, including catalog-owned properties.
	GraphDefinition(
		/// One named graph definition, including catalog-owned properties.
		GraphId,
	),
	/// Set of graph definitions observed by a catalog scan.
	GraphCatalogRange,
	/// One class definition, independent of its record contents.
	SchemaClass(
		/// One class definition, independent of its record contents.
		ClassId,
	),
	/// Class namespace including absent-name and graph-wide source reads.
	ClassCatalogRange,
	/// One durable database parameter definition.
	SchemaParameter(
		/// One durable database parameter definition.
		SchemaId,
	),
	/// Set of durable database parameters observed by a catalog scan.
	ParameterCatalogRange,
	/// One durable named-function overload.
	SchemaFunction(
		/// One durable named-function overload.
		FunctionId,
	),
	/// Set of durable named-function overloads observed by a catalog scan.
	FunctionCatalogRange,
	/// One versioned class or graph event definition.
	SchemaEvent(
		/// One versioned class or graph event definition.
		EventId,
	),
	/// Event namespace, including absent owner/name lookups.
	EventCatalogRange,
	/// One accepted invocation or atomic completion/cancellation marker.
	EventInvocation(
		/// One accepted invocation or atomic completion/cancellation marker.
		super::JobId,
	),
	/// Invocation namespace observed by bounded job administration.
	EventOutboxRange,
	/// One durable text-analyzer definition.
	SchemaAnalyzer(
		/// One durable text-analyzer definition.
		AnalyzerId,
	),
	/// Set of durable text analyzers observed by a catalog scan.
	AnalyzerCatalogRange,
	/// Database-global identity or base properties.
	BaseAtom(
		/// Database-global identity or base properties.
		GlobalAtomId,
	),
	/// Membership or value set observed by a database-global class scan.
	BaseClassRange(
		/// Membership or value set observed by a database-global class scan.
		ClassId,
	),
	/// Presence and local address of one atom in one graph.
	GraphMembership(
		/// Graph identity addressed by this form.
		GraphId,
		/// Atom identity addressed by this form.
		GlobalAtomId,
	),
	/// Sparse effective-property decisions for one occurrence.
	GraphOverlay(
		/// Graph identity addressed by this form.
		GraphId,
		/// Atom identity addressed by this form.
		GlobalAtomId,
	),
	/// Graph-local slot and, for edges, topology, weight, and transition
	/// binding.
	GraphIncidence(
		/// Graph identity addressed by this form.
		GraphId,
		/// Atom identity addressed by this form.
		AtomId,
	),
	/// Membership or effective-value set observed by a graph-scoped class scan.
	GraphClassRange(
		/// Graph identity addressed by this form.
		GraphId,
		/// Membership or effective-value set observed by a graph-scoped class
		/// scan.
		ClassId,
	),
	/// MATCH candidate/topology set for one graph-local atom kind.
	GraphKindRange(
		/// Graph identity addressed by this form.
		GraphId,
		/// MATCH candidate/topology set for one graph-local atom kind.
		AtomKind,
	),
	/// Capture policy of one class or graph source.
	ChangefeedDefinition(
		/// Capture policy of one class or graph source.
		crate::changefeed::ChangefeedSource,
	),
	/// Retained source namespace observed by historical name/id lookup.
	ChangefeedCatalogRange,
	/// One subscription incarnation, independent of source data changes.
	LiveDefinition(
		/// One subscription incarnation, independent of source data changes.
		crate::live::LiveId,
	),
	/// One reactive identity, expression revision or committed value.
	LiveBinding(
		/// One reactive identity, expression revision or committed value.
		crate::live::LiveBindingId,
	),
	/// Registry namespace observed by metadata lookup.
	LiveCatalogRange,
}
