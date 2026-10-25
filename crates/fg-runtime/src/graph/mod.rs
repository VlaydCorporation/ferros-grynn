//! Public runtime graph domain.
//!
//! Storage families keep their structural invariants in distinct layouts while
//! [`GraphRead`] provides one read surface for in-memory, disk-bound and hybrid
//! graphs. Transaction-bound views never enter [`crate::values::Value`].

#![allow(
	clippy::pattern_type_mismatch,
	reason = "graph topology matching intentionally uses Rust match ergonomics for borrowed enums"
)]
#![allow(
	clippy::renamed_function_params,
	reason = "trait implementations use domain-specific formatter and value names"
)]

pub mod algo;
/// Qualified sparse graph analytics and convergence metadata.
pub mod analytics;
/// Directed acyclic graph operations and validation.
pub(crate) mod dag;
/// Tree, rooted-tree and forest structural invariants.
mod forest;
/// General mutable and frozen graph operations.
pub(crate) mod general;
/// Graph storage and validation for hyperedge incidence.
mod hypergraph;
/// Snapshot-local atom handles and their generation layout.
pub(crate) mod id;
pub(crate) mod live;
/// Owned graph-family results and memory estimates.
pub(crate) mod materialized;
/// Graphs with nested graph atoms and cross-level incidence.
pub(crate) mod metagraph;
/// Owned qualified paths and trace materialization.
pub(crate) mod path;
/// Shared graph-read contracts for runtime and storage-backed views.
pub(crate) mod read;
/// Graph metadata and family-specific atom layouts.
mod storage;
/// Scoped subsets of existing graphs.
mod subgraph;
pub mod tensor;
/// Incidence directions, endpoint roles and topology constraints.
pub(crate) mod topology;
/// Graph traversal, connectivity and path algorithms.
pub(crate) mod traversal;
pub(crate) mod validate;
/// Borrowed and filtered graph views.
mod view;
/// Derived component workspaces attached to graph snapshots.
mod workspace;

pub(crate) use traversal::visit_incidence_targets;

/// Type-state marker enabling structural mutation of a graph family.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub struct Mutable;

/// Type-state marker exposing a graph family as an immutable snapshot.
#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub struct Frozen;

pub use dag::{Dag, FrozenDag, MutableDag};
pub use forest::{
	Forest,
	FrozenForest,
	FrozenRootedTree,
	FrozenTree,
	MutableForest,
	MutableRootedTree,
	MutableTree,
	RootedTree,
	Tree,
	TreeOrientation,
};
pub use general::{FrozenGraph, Graph, MutableGraph};
pub use hypergraph::{FrozenHypergraph, Hypergraph, MutableHypergraph};
pub use id::{AtomId, AtomKind, GraphId};
pub use live::{LiveEvent, LiveFamily, LiveGraph, LiveReadGuard, LiveUpdate};
pub use materialized::MaterializedGraph;
pub use metagraph::{FrozenMetagraph, Metagraph, MutableMetagraph};
pub use path::{
	EndpointRole,
	Path,
	PathDirection,
	PathStep,
	ResolvedAtom,
	ResolvedPath,
	ResolvedPathStep,
};
#[allow(
	unused_imports,
	reason = "the query traversal VM consumes the crate-private trace arena in integration stage 6"
)]
pub(crate) use path::{PathTraceArena, PathTraceId};
pub use read::{
	AtomIds,
	AtomOccurrenceRef,
	AtomRef,
	DirtyTarget,
	DiskGraphView,
	EdgeRead,
	EdgeView,
	FallibleAtomIds,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphReadView,
	GraphRef,
	GraphRefReader,
	GraphStorageMode,
	GraphTopologyConstraint,
	GraphTopologyConstraints,
	GraphWriteBack,
	MaterializationOrigin,
	NodeRead,
	NodeView,
	QueryMaterialize,
	SnapshotId,
	query_to_memory,
};
pub use subgraph::{FrozenSubgraph, MutableSubgraph, Subgraph};
pub use topology::{EdgeDirection, EdgeEndpoint, EdgeTopology, QualifiedAtomId};
pub use traversal::{
	BinaryLiftingIndex,
	Condensation,
	CutSet,
	ShortestPaths,
	SpanningForest,
	TraversalDirection,
	TraversalIndex,
	TraversalSemantics,
	TraversalStep,
	bellman_ford,
	bfs,
	condensation,
	dfs,
	dijkstra,
	find_cuts,
	has_cycle_directed,
	has_cycle_undirected,
	is_connected,
	minimum_spanning_forest,
	minimum_spanning_tree,
	strongly_connected_components,
	topological_sort,
	weakly_connected_components,
};
pub(crate) use validate::validate_topology_constraints;
pub use view::{DagView, ForestView, GraphView, TreeView};
pub use workspace::{GraphWorkspace, IntoGraphWorkspace};
