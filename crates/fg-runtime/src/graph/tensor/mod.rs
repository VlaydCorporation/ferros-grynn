//! Snapshot-bound endpoint tensors and logical matrix views.
//!
//! Sparse membership is independent of connector weights. Qualified identities
//! remain runtime values and never expose physical pages or global atom slots.
#![doc = include_str!("../../../docs/rustdoc/graph-tensor-example-01.md")]

pub(crate) mod build;
pub(crate) mod control;
pub(crate) mod matrix;
mod projection;
mod views;

use std::sync::Arc;

pub use control::GraphAlgorithmLimits;
pub use matrix::{CsrMatrix, DenseMatrix};
pub(crate) use projection::ProjectedRelation;
pub use projection::{
	AdjacencyBlock,
	BinaryAdjacencyMatrix,
	ProjectionOptions,
	TensorArc,
	TensorProjection,
	WeightSelection,
	WeightedAdjacencyMatrix,
};
pub use views::{IncidenceMatrix, IncidenceSide, IncidenceTensor};

use crate::{
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, AtomOccurrenceRef, AtomRef, EdgeDirection, GraphRead, GraphRef, SnapshotId},
	values::{Number, Text, Uuid},
};

/// Stable logical identity of a tensor axis in a runtime or catalog graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum TensorAtom {
	/// Standalone graph without a catalog GraphId; no fabricated catalog ID.
	Runtime {
		/// Actual standalone runtime graph origin.
		graph: Uuid,
		/// Stable qualified atom identity.
		atom: AtomRef,
	},
	/// Existing datastore occurrence, qualified by its real GraphId.
	Occurrence(
		/// Existing datastore occurrence, qualified by its real GraphId.
		AtomOccurrenceRef,
	),
}

impl TensorAtom {
	/// Returns the owned public atom identity.
	pub const fn atom(&self) -> &AtomRef {
		match self {
			Self::Runtime {
				atom,
				..
			} => atom,
			Self::Occurrence(value) => value.atom(),
		}
	}

	/// Returns a catalog occurrence when this axis belongs to a datastore.
	pub const fn occurrence(&self) -> Option<&AtomOccurrenceRef> {
		match self {
			Self::Runtime {
				..
			} => None,
			Self::Occurrence(value) => Some(value),
		}
	}

	/// Converts reader metadata to owned canonical tensor data.
	pub(crate) fn from_graph<G: GraphRead>(graph: &G, atom: AtomRef) -> Self {
		match graph.graph_ref() {
			Some(reference) => {
				Self::Occurrence(AtomOccurrenceRef::new(atom, Arc::new(reference.clone())))
			}
			None => Self::Runtime {
				graph: graph.identity(),
				atom,
			},
		}
	}

	/// Estimates owned identity payload, including catalog metadata.
	pub(crate) fn heap_bytes(&self) -> usize {
		self.atom()
			.id()
			.estimated_heap_bytes()
			.saturating_add(self.occurrence().map_or(0, |value| value.graph().name().len()))
	}
}

/// Selects the connector domain; containment itself does not create arcs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TensorScope {
	/// Tensor entries address the local graph scope.
	#[default]
	Local,
	/// Tensor entries include the selected rooted graph scope.
	Rooted,
}

/// Checked at the operation boundary; dense materialization is always explicit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TensorOptions {
	/// Local or rooted connector domain.
	pub scope: TensorScope,
	/// Per-operation hard resource ceilings.
	pub limits: GraphAlgorithmLimits,
}

/// Origin and snapshot retained for one graph contributing tensor data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorBinding {
	/// Runtime graph origin captured at collection.
	pub(crate) origin: Uuid,
	/// Exact source snapshot identity.
	pub(crate) snapshot: SnapshotId,
	/// Real catalog metadata when the reader belongs to a datastore.
	pub(crate) graph: Option<GraphRef>,
}

/// Containment metadata retained independently of analytical connectivity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorContainment {
	/// Qualified containment owner.
	pub(crate) owner: TensorAtom,
	/// Captured child graph binding.
	pub(crate) child: TensorBinding,
}

impl TensorContainment {
	/// Returns the qualified container occurrence.
	pub const fn owner(&self) -> &TensorAtom {
		&self.owner
	}

	/// Returns the captured child binding, shared children retaining each
	/// owner.
	pub const fn child(&self) -> &TensorBinding {
		&self.child
	}
}

impl TensorBinding {
	/// Returns runtime graph origin.
	pub const fn origin(&self) -> Uuid {
		self.origin
	}

	/// Returns captured snapshot identity.
	pub const fn snapshot(&self) -> SnapshotId {
		self.snapshot
	}

	/// Returns real catalog metadata when available.
	pub const fn graph(&self) -> Option<&GraphRef> {
		self.graph.as_ref()
	}
}

/// One original endpoint descriptor; different containment routes are retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorEndpoint {
	/// Canonical endpoint row ordinal.
	pub(crate) row: usize,
	/// Ordered qualified containment route.
	pub(crate) owners: Vec<TensorAtom>,
}

impl TensorEndpoint {
	/// Returns the endpoint's tensor row ordinal.
	pub const fn row(&self) -> usize {
		self.row
	}

	/// Returns the qualified owner chain from the connector graph.
	pub fn owners(&self) -> &[TensorAtom] {
		&self.owners
	}
}

/// Connector topology and captured weight, independent of membership planes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorConnector {
	/// Stable qualified atom identity.
	pub(crate) atom: TensorAtom,
	/// Original topology or selected traversal direction.
	pub(crate) direction: EdgeDirection,
	/// Original source incidence, independent of weight.
	pub(crate) source: Vec<TensorEndpoint>,
	/// Original target incidence, independent of weight.
	pub(crate) target: Vec<TensorEndpoint>,
	/// Captured optional connector label.
	pub(crate) label: Option<Text>,
	/// Captured connector weight, defaulting to one.
	pub(crate) weight: Number,
}

impl TensorConnector {
	/// Returns connector identity, including its owning graph.
	pub const fn atom(&self) -> &TensorAtom {
		&self.atom
	}

	/// Returns the original directed/bidirectional/undirected topology.
	pub const fn direction(&self) -> EdgeDirection {
		self.direction
	}

	/// Returns original ordered source endpoints, retaining repetitions.
	pub fn source(&self) -> &[TensorEndpoint] {
		&self.source
	}

	/// Returns original target endpoints, independently of logical membership.
	pub fn target(&self) -> &[TensorEndpoint] {
		&self.target
	}

	/// Selects endpoint lists for one logical membership plane without copying.
	pub(crate) fn plane_endpoints(&self, source: bool) -> [&[TensorEndpoint]; 2] {
		let undirected = self.direction == EdgeDirection::Undirected;
		[
			if source || undirected {
				&self.source
			} else {
				&[]
			},
			if !source || undirected {
				&self.target
			} else {
				&[]
			},
		]
	}

	/// Returns captured connector label.
	pub const fn label(&self) -> Option<&Text> {
		self.label.as_ref()
	}

	/// Returns captured weight; missing original weight is one.
	pub const fn weight(&self) -> Number {
		self.weight
	}
}

/// Immutable owned sparse tensor. Borrowed views cannot outlive this snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointTensor {
	/// Canonical qualified atom dictionary.
	pub(crate) atoms: Vec<TensorAtom>,
	/// Induced-domain membership for each atom row.
	pub(crate) domain: Vec<bool>,
	/// Sorted node-kind row ordinals.
	pub(crate) nodes: Vec<usize>,
	/// Canonical connectors with original topology.
	pub(crate) connectors: Vec<TensorConnector>,
	/// Logical source membership, independent of weight and original roles.
	pub(crate) source: CsrMatrix<bool>,
	/// Logical target membership, independent of weight and original roles.
	pub(crate) target: CsrMatrix<bool>,
	/// Captured source graph bindings.
	pub(crate) bindings: Vec<TensorBinding>,
	/// Primary requested binding ordinal, checked by capture and cache restore.
	pub(crate) root: usize,
	/// Caller-selected scope, projection and resource options.
	pub(crate) options: TensorOptions,
	/// Conservative aggregate build admission.
	pub(crate) admitted_bytes: usize,
	/// Conservative capture work, including empty-row and containment scans.
	pub(crate) work: u64,
	/// Owner-to-child metadata, without implicit adjacency.
	pub(crate) containment: Vec<TensorContainment>,
}

impl EndpointTensor {
	/// Collects owned runtime graph data; ROOTED traverses stored child graphs.
	///
	/// # Errors
	/// Returns malformed incidence, missing nested content or resource errors.
	pub fn from_graph<G: GraphRead>(graph: &G, options: TensorOptions) -> FerrosGrynnResult<Self> {
		Self::from_graph_with_check(graph, options, &mut || Ok(()))
	}

	/// Collects data with cooperative cancellation/deadline checks.
	///
	/// # Errors
	/// Propagates reader/check errors and rejects malformed or excessive data.
	pub fn from_graph_with_check<G: GraphRead, CheckArg: FnMut() -> FerrosGrynnResult<()>>(
		graph: &G,
		options: TensorOptions,
		check: &mut CheckArg,
	) -> FerrosGrynnResult<Self> {
		build::from_graph(graph, options, check)
	}

	/// Returns atoms × connectors × two logical sides.
	pub fn shape(&self) -> (usize, usize, usize) {
		(self.atoms.len(), self.connectors.len(), 2)
	}

	/// Borrows the canonically ordered atom dictionary.
	pub fn atoms(&self) -> &[TensorAtom] {
		&self.atoms
	}

	/// Checks whether a row belongs to the induced analytical domain.
	/// Boundary endpoints are retained in LOCAL incidence but excluded here.
	///
	/// # Errors
	/// Rejects an ordinal outside the atom dictionary.
	pub fn in_domain(&self, row: usize) -> FerrosGrynnResult<bool> {
		self.domain
			.get(row)
			.copied()
			.ok_or_else(|| control::invalid("tensor row is outside its shape"))
	}

	/// Iterates canonical induced-domain axis identities without allocation.
	pub fn domain_atoms(&self) -> impl Iterator<Item = &TensorAtom> {
		self.atoms.iter().zip(&self.domain).filter_map(|(atom, included)| included.then_some(atom))
	}

	/// Borrows the canonically ordered connector dictionary and topology.
	pub fn connectors(&self) -> &[TensorConnector] {
		&self.connectors
	}

	/// Borrows all contributing origin/snapshot bindings.
	pub fn bindings(&self) -> &[TensorBinding] {
		&self.bindings
	}

	/// Borrows the primary requested graph independently of canonical axis
	/// order.
	pub fn root_binding(&self) -> &TensorBinding {
		&self.bindings[self.root]
	}

	/// Borrows containment descriptors; these never become analytical edges.
	pub fn containment(&self) -> &[TensorContainment] {
		&self.containment
	}

	/// Returns original scope/options.
	pub const fn options(&self) -> TensorOptions {
		self.options
	}

	/// Returns conservatively admitted build bytes.
	pub const fn admitted_bytes(&self) -> usize {
		self.admitted_bytes
	}

	/// Returns conservatively charged capture operations.
	pub const fn work(&self) -> u64 {
		self.work
	}

	/// Finds a stable logical identity without scanning storage.
	pub fn atom_ordinal(&self, atom: &TensorAtom) -> Option<usize> {
		self.atoms.binary_search(atom).ok()
	}

	/// Finds a stable connector identity.
	pub fn connector_ordinal(&self, atom: &TensorAtom) -> Option<usize> {
		self.connectors.binary_search_by(|connector| connector.atom.cmp(atom)).ok()
	}

	/// Iterates true atom/connector/side cells in canonical plane order.
	pub fn nonzeros(&self) -> impl Iterator<Item = (usize, usize, IncidenceSide)> + '_ {
		self.source.entries().map(|(row, column, _)| (row, column, IncidenceSide::Source)).chain(
			self.target.entries().map(|(row, column, _)| (row, column, IncidenceSide::Target)),
		)
	}

	/// Looks up membership with checked logical ordinals.
	///
	/// # Errors
	/// Rejects out-of-range cells; Both means source OR target.
	pub fn get(
		&self,
		row: usize,
		connector: usize,
		side: IncidenceSide,
	) -> FerrosGrynnResult<bool> {
		match side {
			IncidenceSide::Source => Ok(self.source.get(row, connector)?.is_some()),
			IncidenceSide::Target => Ok(self.target.get(row, connector)?.is_some()),
			IncidenceSide::Both => Ok(self.source.get(row, connector)?.is_some()
				|| self.target.get(row, connector)?.is_some()),
		}
	}

	/// Borrows one full atom × connector plane.
	pub const fn matrix(&self, side: IncidenceSide) -> IncidenceMatrix<'_> {
		IncidenceMatrix::new(self, None, side)
	}

	/// Borrows node-only axes without copying membership.
	pub const fn incidence_tensor(&self) -> IncidenceTensor<'_> {
		IncidenceTensor::new(self)
	}

	/// Checks whether a runtime reader still identifies the captured snapshot.
	///
	/// # Errors
	/// Rejects foreign origin and changed revisions/snapshots.
	pub fn validate_binding<G: GraphRead>(&self, graph: &G) -> FerrosGrynnResult<()> {
		let binding = self
			.bindings
			.iter()
			.find(|binding| binding.origin == graph.identity())
			.ok_or_else(|| control::invalid("tensor belongs to a different graph origin"))?;
		if binding.snapshot != graph.snapshot_id() {
			return Err(control::invalid("tensor belongs to a different graph snapshot"));
		}
		Ok(())
	}
}

/// Workspace-only tensor owner operations.
pub(crate) mod interop {
	use super::{
		CsrMatrix,
		EndpointTensor,
		FerrosGrynnResult,
		SnapshotId,
		TensorAtom,
		TensorConnector,
		TensorEndpoint,
		TensorOptions,
		control,
	};
	/// Borrowed sparse planes and checked capture updates used by persistence.
	pub trait EndpointTensorInternal {
		/// Borrows the existing source membership plane.
		fn source_plane(&self) -> &CsrMatrix<bool>;
		/// Borrows the existing target membership plane.
		fn target_plane(&self) -> &CsrMatrix<bool>;
		/// Returns the validated primary binding ordinal.
		fn root_ordinal(&self) -> usize;
		/// Retains the caller's original capture accounting.
		fn set_capture_accounting(&mut self, options: TensorOptions, bytes: usize, work: u64);
		/// Restamps a cloned catalog capture while preserving cancellation and
		/// read tracking.
		///
		/// # Errors
		/// Propagates validation, resource-admission or callback errors from
		/// the operation.
		fn restamp_catalog_bindings(
			&mut self,
			snapshot: SnapshotId,
			step: &mut dyn FnMut() -> FerrosGrynnResult<()>,
			record_graph: &mut dyn FnMut(crate::graph::GraphId),
		) -> FerrosGrynnResult<()>;
	}
	impl EndpointTensorInternal for EndpointTensor {
		fn source_plane(&self) -> &CsrMatrix<bool> {
			&self.source
		}

		fn target_plane(&self) -> &CsrMatrix<bool> {
			&self.target
		}

		fn root_ordinal(&self) -> usize {
			self.root
		}

		fn set_capture_accounting(&mut self, options: TensorOptions, bytes: usize, work: u64) {
			self.options = options;
			self.admitted_bytes = bytes;
			self.work = work;
		}

		fn restamp_catalog_bindings(
			&mut self,
			snapshot: SnapshotId,
			step: &mut dyn FnMut() -> FerrosGrynnResult<()>,
			record_graph: &mut dyn FnMut(crate::graph::GraphId),
		) -> FerrosGrynnResult<()> {
			for binding in &mut self.bindings {
				step()?;
				let reference = binding.graph.as_ref().ok_or_else(|| {
					control::invalid("cached tensor binding lacks catalog identity")
				})?;
				binding.snapshot = snapshot;
				record_graph(reference.id());
			}
			for containment in &mut self.containment {
				containment.child.snapshot = snapshot;
			}
			Ok(())
		}
	}
	/// Owner-side selection of original endpoint lists without copying.
	pub trait TensorConnectorInternal {
		/// Borrows the two endpoint lists of one logical membership plane.
		fn plane_endpoints(&self, source: bool) -> [&[TensorEndpoint]; 2];
	}
	impl TensorConnectorInternal for TensorConnector {
		fn plane_endpoints(&self, source: bool) -> [&[TensorEndpoint]; 2] {
			TensorConnector::plane_endpoints(self, source)
		}
	}

	/// Operations shared with the engine and value codecs.
	pub trait TensorAtomInternal: Sized {
		/// Estimates owned identity payload, including catalog metadata.
		fn heap_bytes(&self) -> usize;
	}
	impl TensorAtomInternal for TensorAtom {
		fn heap_bytes(&self) -> usize {
			<TensorAtom>::heap_bytes(self)
		}
	}
}
