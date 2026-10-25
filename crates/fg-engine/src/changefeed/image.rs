//! Owned historical records and qualified graph topology.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
	graph::{AtomOccurrenceRef, AtomRef, EdgeDirection, GraphRef},
	values::{Number, Object, Text},
};

/// Identity of one global record or graph-local occurrence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ChangeIdentity {
	/// A database-global class record.
	Record(
		/// A database-global class record.
		AtomRef,
	),
	/// A qualified occurrence in a catalog graph.
	Occurrence(
		/// A qualified occurrence in a catalog graph.
		AtomOccurrenceRef,
	),
}

/// Explicit property decision; absent keys inherit their base value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeOverlay {
	/// An explicit graph-local replacement.
	Value(
		/// An explicit graph-local replacement.
		crate::values::Value,
	),
	/// An explicit suppression of the base value.
	Hidden,
}

/// One historical endpoint, preserving its complete containment route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEndpoint {
	/// Qualified terminal occurrence.
	pub(crate) occurrence: AtomOccurrenceRef,
	/// Ordered qualified owners from the connector graph to the terminal graph.
	pub(crate) owners: Vec<AtomOccurrenceRef>,
}

impl ChangeEndpoint {
	/// Returns the qualified terminal occurrence.
	pub const fn occurrence(&self) -> &AtomOccurrenceRef {
		&self.occurrence
	}

	/// Returns the ordered containment route, empty for a local endpoint.
	pub fn owners(&self) -> &[AtomOccurrenceRef] {
		&self.owners
	}
}

/// Graph-local structure captured independently of current graph records.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChangeGraphState {
	/// Node containment binding.
	Node {
		/// Referenced child graph, without recursively capturing it.
		nested_graph: Option<GraphRef>,
	},
	/// Connector topology, including hyperedges and edge endpoints.
	Edge {
		/// Original directed, bidirectional, or undirected topology.
		direction: EdgeDirection,
		/// Ordered source endpoints; undirected endpoints occupy this side.
		source: Vec<ChangeEndpoint>,
		/// Ordered target endpoints, empty for undirected connectors.
		target: Vec<ChangeEndpoint>,
		/// Exact stored numeric weight.
		weight: Option<Number>,
		/// Optional transition graph.
		transition_graph: Option<GraphRef>,
	},
}

/// Complete owned image at a logical operation boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeImage {
	/// Global or graph-local qualified identity.
	pub(crate) identity: ChangeIdentity,
	/// Base properties for CLASS, effective properties for GRAPH, with owned
	/// names.
	pub(crate) properties: Object,
	/// Optional graph-local label.
	pub(crate) label: Option<Text>,
	/// Sparse explicit overlay decisions, including hidden fields.
	pub(crate) overlays: BTreeMap<Text, ChangeOverlay>,
	/// Optional graph-local structural state.
	pub(crate) graph_state: Option<ChangeGraphState>,
}

impl ChangeImage {
	/// Returns the captured qualified identity.
	pub const fn identity(&self) -> &ChangeIdentity {
		&self.identity
	}

	/// Returns exact properties with owned names, independent of current
	/// dictionaries.
	pub const fn properties(&self) -> &Object {
		&self.properties
	}

	/// Returns the historical graph-local label.
	pub const fn label(&self) -> Option<&Text> {
		self.label.as_ref()
	}

	/// Returns explicit graph-local decisions; absent entries mean inheritance.
	pub const fn overlays(&self) -> &BTreeMap<Text, ChangeOverlay> {
		&self.overlays
	}

	/// Returns the historical graph-local topology and graph references.
	pub const fn graph_state(&self) -> Option<&ChangeGraphState> {
		self.graph_state.as_ref()
	}
}

/// One NET identity change or one FULL operation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
	/// Checked ordinal within the selected feed and read mode of one commit.
	pub(crate) ordinal: u64,
	/// Original logical operation order, preserved by FULL reads.
	pub(crate) operation: u64,
	/// Image immediately before the operation, or absent for creation.
	pub(crate) before: Option<Arc<ChangeImage>>,
	/// Image immediately after the operation, or absent for deletion.
	pub(crate) after: Option<Arc<ChangeImage>>,
}

impl ChangeEntry {
	/// Returns the deduplication ordinal for this selected read mode.
	pub const fn ordinal(&self) -> u64 {
		self.ordinal
	}

	/// Returns the transaction's logical operation ordinal.
	pub const fn operation(&self) -> u64 {
		self.operation
	}

	/// Returns the exact historical image, absent for creation.
	pub fn before(&self) -> Option<&ChangeImage> {
		self.before.as_deref()
	}

	/// Returns the exact historical image, absent for deletion.
	pub fn after(&self) -> Option<&ChangeImage> {
		self.after.as_deref()
	}
}
