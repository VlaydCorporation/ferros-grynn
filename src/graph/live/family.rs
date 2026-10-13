//! Closed mappings between each family's frozen and mutable COW layouts.

use super::super::{
	Dag,
	Forest,
	Frozen,
	Graph,
	GraphKind,
	GraphMut,
	GraphRead,
	GraphWriteBack,
	Hypergraph,
	MaterializedGraph,
	Metagraph,
	Mutable,
	RootedTree,
	Subgraph,
	Tree,
	validate_topology_constraints,
};
use crate::gql_status::{FerrosGrynnError, FerrosGrynnResult};

/// A runtime graph family supported by [`super::LiveGraph`].
///
/// This trait is sealed: only the engine's graph families can supply the
/// matching mutable/frozen layouts and their validation contract.
pub trait LiveFamily: sealed::Family + GraphRead + Sized {
	/// Returns the fixed family selected by this concrete Rust type.
	fn family_kind() -> GraphKind;
	/// Takes a checked owned result while preserving its concrete layout.
	///
	/// # Errors
	/// Returns a family mismatch without changing any live materialization.
	#[doc(hidden)]
	fn from_materialized(graph: MaterializedGraph) -> FerrosGrynnResult<Self::Frozen>;
	/// Converts a frozen family into the ordinary owned graph transport.
	#[doc(hidden)]
	fn into_materialized(graph: Self::Frozen) -> MaterializedGraph;
	/// Thaws the exact family through its checked COW state conversion.
	///
	/// # Errors
	/// Returns the concrete family's topology conversion failure.
	#[doc(hidden)]
	fn thaw_frozen(graph: Self::Frozen) -> FerrosGrynnResult<Self::Mutable> {
		Self::thaw(graph)
	}
	/// The graph's current mutability state.
	type State;
	/// The same family with immutable revision-pinned storage.
	type Frozen: LiveFamily<State = Frozen, Frozen = Self::Frozen, Mutable = Self::Mutable> + Clone;
	/// The same family with mutable COW storage.
	type Mutable: LiveFamily<State = Mutable, Frozen = Self::Frozen, Mutable = Self::Mutable>
		+ GraphMut;
}

/// Keeps state conversions and family invariants owned by the runtime graph
/// module.
pub(super) mod sealed {
	use super::{FerrosGrynnResult, GraphWriteBack, LiveFamily};

	/// Private operations preserve concrete layouts and mutable dirty metadata.
	pub trait Family {
		/// Admits the complete retained layout before cloning a mutable
		/// snapshot.
		fn admit_snapshot(
			&self,
			admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
		) -> FerrosGrynnResult<()>;
		/// Freezes the concrete family without changing identity or topology.
		fn freeze(graph: <Self as LiveFamily>::Mutable) -> <Self as LiveFamily>::Frozen
		where
			Self: LiveFamily;
		/// Checks the concrete family while changing its mutability state.
		fn thaw(
			graph: <Self as LiveFamily>::Frozen,
		) -> FerrosGrynnResult<<Self as LiveFamily>::Mutable>
		where
			Self: LiveFamily;
		/// Retains local decisions in a COW candidate without publication.
		fn clone_mutable(graph: &<Self as LiveFamily>::Mutable) -> <Self as LiveFamily>::Mutable
		where
			Self: LiveFamily;
		/// Adopts the complete accepted remote watermark.
		fn adopt_revision(graph: &mut <Self as LiveFamily>::Mutable, revision: u64)
		where
			Self: LiveFamily;
		/// Clears local mutation metadata after an accepted remote boundary.
		fn clear_dirty(graph: &mut <Self as LiveFamily>::Mutable)
		where
			Self: LiveFamily;
		/// Checks the concrete family invariants before publication.
		fn validate(&self) -> FerrosGrynnResult<()>;
		/// Passes a validated private candidate to the existing graph
		/// write-back contract.
		fn flush<Writer: GraphWriteBack<<Self as LiveFamily>::Mutable>>(
			graph: &mut <Self as LiveFamily>::Mutable,
			writer: &mut Writer,
		) -> FerrosGrynnResult<()>
		where
			Self: LiveFamily;
	}
}

/// Connects one concrete mutability state to the closed live-family contract.
macro_rules! family_state {
	($family:ident, $state:ident, $thaw:expr, $validate:expr) => {
		impl LiveFamily for $family<$state> {
			type Frozen = $family<Frozen>;
			type Mutable = $family<Mutable>;
			type State = $state;

			fn family_kind() -> GraphKind {
				GraphKind::$family
			}

			fn from_materialized(graph: MaterializedGraph) -> FerrosGrynnResult<Self::Frozen> {
				match graph {
					MaterializedGraph::$family(graph) => Ok(graph),
					other => Err(FerrosGrynnError::invalid_argument(format!(
						"LIVE graph family {:?} does not match {:?}",
						other.kind(),
						Self::family_kind()
					))),
				}
			}

			fn into_materialized(graph: Self::Frozen) -> MaterializedGraph {
				MaterializedGraph::$family(graph)
			}
		}
		impl sealed::Family for $family<$state> {
			/// Freezes the concrete family without changing identity or topology.
			fn freeze(graph: <Self as LiveFamily>::Mutable) -> <Self as LiveFamily>::Frozen
			where
				Self: LiveFamily,
			{
				graph.freeze()
			}

			/// Checks the concrete family while changing its mutability state.
			fn thaw(
				graph: <Self as LiveFamily>::Frozen,
			) -> FerrosGrynnResult<<Self as LiveFamily>::Mutable>
			where
				Self: LiveFamily,
			{
				($thaw)(graph)
			}

			/// Retains local decisions in a COW candidate without publication.
			fn clone_mutable(graph: &<Self as LiveFamily>::Mutable) -> <Self as LiveFamily>::Mutable
			where
				Self: LiveFamily,
			{
				graph.clone_preserving_state()
			}

			/// Admits the complete retained layout before cloning a mutable snapshot.
			fn admit_snapshot(
				&self,
				admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
			) -> FerrosGrynnResult<()> {
				super::super::materialized::admit_graph_snapshot(self, admit)
			}

			/// Adopts the complete accepted remote watermark.
			fn adopt_revision(graph: &mut <Self as LiveFamily>::Mutable, revision: u64)
			where
				Self: LiveFamily,
			{
				graph.adopt_external_revision(revision);
			}

			/// Clears local mutation metadata after an accepted remote boundary.
			fn clear_dirty(graph: &mut <Self as LiveFamily>::Mutable)
			where
				Self: LiveFamily,
			{
				graph.clear_live_dirty();
			}

			fn validate(&self) -> FerrosGrynnResult<()> {
				validate_topology_constraints(self, self.topology_constraints())?;
				($validate)(self)
			}

			/// Passes a validated private candidate to the existing graph write-back
			/// contract.
			fn flush<Writer: GraphWriteBack<<Self as LiveFamily>::Mutable>>(
				graph: &mut <Self as LiveFamily>::Mutable,
				writer: &mut Writer,
			) -> FerrosGrynnResult<()>
			where
				Self: LiveFamily,
			{
				graph.flush(writer)
			}
		}
	};
}

/// Installs the concrete family's existing COW and validation operations.
macro_rules! family {
	($family:ident, $thaw:expr, $validate:expr) => {
		family_state!($family, Frozen, $thaw, $validate);
		family_state!($family, Mutable, $thaw, $validate);
	};
}

family!(Graph, |graph: Graph<Frozen>| Ok(graph.thaw()), |graph: &Graph<_>| graph
	.validate_owned_topology());
family!(Forest, |graph: Forest<Frozen>| Ok(graph.thaw()), |_graph: &Forest<_>| Ok(()));
family!(Tree, |graph: Tree<Frozen>| Ok(graph.thaw()), |_graph: &Tree<_>| Ok(()));
family!(RootedTree, |graph: RootedTree<Frozen>| Ok(graph.thaw()), |_graph: &RootedTree<_>| Ok(()));
family!(Dag, Dag::<Frozen>::thaw, |_graph: &Dag<_>| Ok(()));
family!(Hypergraph, |graph: Hypergraph<Frozen>| Ok(graph.thaw()), |_graph: &Hypergraph<_>| Ok(()));
family!(Metagraph, |graph: Metagraph<Frozen>| Ok(graph.thaw()), |graph: &Metagraph<_>| graph
	.as_graph()
	.validate_owned_topology());
family!(Subgraph, |graph: Subgraph<Frozen>| Ok(graph.thaw()), |graph: &Subgraph<_>| graph
	.as_graph()
	.validate_owned_topology());
