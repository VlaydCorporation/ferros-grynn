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
mod dag;
mod forest;
mod general;
mod hypergraph;
mod id;
mod live;
mod materialized;
mod metagraph;
mod path;
mod read;
mod storage;
mod subgraph;
mod topology;
mod traversal;
mod view;
mod workspace;

#[derive(Debug, Default, Clone, Copy, PartialOrd, PartialEq, Eq, Ord, Hash)]
#[non_exhaustive]
pub struct Mutable;

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
pub(crate) use general::{merge_transaction_graph, remap_topology};
pub use hypergraph::{FrozenHypergraph, Hypergraph, MutableHypergraph};
pub(crate) use id::SlotAllocator;
pub use id::{AtomId, AtomKind, GraphId};
pub use live::{LiveEvent, LiveGraph, LiveReadGuard, LiveUpdate};
pub use materialized::MaterializedGraph;
pub(crate) use materialized::{estimated_nested_graph_heap_bytes, estimated_topology_heap_bytes};
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
pub use view::{DagView, ForestView, GraphView, TreeView};
pub use workspace::{GraphWorkspace, IntoGraphWorkspace};
