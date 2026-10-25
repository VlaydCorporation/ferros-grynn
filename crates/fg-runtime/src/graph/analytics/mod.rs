//! Exact qualified topology with floating-point analytical scores.
//!
//! All kernels consume the same incidence projection as tensor views. Results
//! own public identities; cancellation, overflow and nonconvergence are errors.
#![doc = include_str!("../../../docs/rustdoc/graph-analytics-example-01.md")]
//! All-shortest owns complete logical paths; callbacks receive exact prefixes.
#![doc = include_str!("../../../docs/rustdoc/graph-analytics-example-02.md")]

pub(crate) mod all_shortest;
mod community;
mod distance;
mod scalar;
mod structural;

pub use all_shortest::{
	AllShortestOptions,
	AllShortestPaths,
	PathCost,
	PathCostContext,
	PathMode,
	PathTerminalContext,
	TensorPath,
	TensorPathStep,
};
pub use community::{label_propagation, louvain};
pub use scalar::{betweenness, degree, eigenvector, harmonic_closeness, katz, page_rank};
pub use structural::{clustering, triangles};

pub use crate::graph::tensor::WeightSelection;
use crate::{
	gql_status::FerrosGrynnResult,
	graph::tensor::{
		GraphAlgorithmLimits,
		ProjectionOptions,
		TensorAtom,
		TensorProjection,
		TensorScope,
		control::{Control, invalid},
	},
	values::Number,
};

/// Convergence limits, validated before any analytical work.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct IterationOptions {
	/// Positive finite stopping tolerance.
	pub tolerance: f64,
	/// Positive maximum iterations; exhaustion is a typed error.
	pub max_iterations: usize,
}

impl Default for IterationOptions {
	/// Constructs the default initial state of `IterationOptions`.
	fn default() -> Self {
		Self {
			tolerance: 1.0e-10,
			max_iterations: 1000,
		}
	}
}

impl IterationOptions {
	/// Validates convergence controls at the operation boundary.
	pub(crate) fn validate(self) -> FerrosGrynnResult<()> {
		if !self.tolerance.is_finite() || self.tolerance <= 0.0 || self.max_iterations == 0 {
			return Err(invalid("iteration tolerance and limit must be finite and positive"));
		}
		Ok(())
	}
}

/// Complete owned values in canonical qualified axis order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct AnalyticsResult<T, Parameters> {
	/// Canonical identities and their complete analytical values.
	pub values: Vec<(TensorAtom, T)>,
	/// Exact parameters used for this result.
	pub parameters: Parameters,
	/// Projection mode, direction, labels and captured resource selection.
	pub selection: ProjectionOptions,
	/// Original local or rooted domain scope.
	pub scope: TensorScope,
	/// Convergence evidence for iterative algorithms.
	pub convergence: Option<ConvergenceMetadata>,
	/// Admitted operations, including scans and checked arithmetic.
	pub work: u64,
	/// Conservative aggregate retained/scratch admission.
	pub admitted_bytes: usize,
}

/// Residual evidence for an algorithm that actually converged.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct ConvergenceMetadata {
	/// Number of completed iterations.
	pub iterations: usize,
	/// Stopping residual: PageRank iterate L1 change, or original spectral
	/// equation.
	pub residual: f64,
}

/// Degree values retain exact raw counts and Number strengths.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct DegreeValues {
	/// Incoming connector incidences.
	pub incoming: u64,
	/// Outgoing connector incidences.
	pub outgoing: u64,
	/// Total degree; undirected connections contribute once, loops twice.
	pub total: u64,
	/// Incoming strength with original Number arithmetic.
	pub incoming_strength: Number,
	/// Outgoing strength with original Number arithmetic.
	pub outgoing_strength: Number,
	/// Total strength using the total-degree convention.
	pub total_strength: Number,
	/// Incoming degree divided by n-1, or zero for n<=1.
	pub normalized_incoming: f64,
	/// Outgoing degree divided by n-1, or zero for n<=1.
	pub normalized_outgoing: f64,
	/// Total degree divided by n-1, or zero for n<=1.
	pub normalized_total: f64,
}

/// Sparse PageRank controls.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct PageRankOptions {
	/// Damping in [0,1), default 0.85.
	pub damping: f64,
	/// Unit connector contributions or stored nonnegative strengths.
	pub weights: WeightSelection,
	/// Stopping controls.
	pub iteration: IterationOptions,
	/// Hard operation resource ceilings.
	pub limits: GraphAlgorithmLimits,
}
impl Default for PageRankOptions {
	/// Constructs the default initial state of `PageRankOptions`.
	fn default() -> Self {
		Self {
			damping: 0.85,
			weights: WeightSelection::Unweighted,
			iteration: IterationOptions::default(),
			limits: GraphAlgorithmLimits::default(),
		}
	}
}

/// Exact-distance centrality controls for vertex-acyclic projected paths.
/// A connector may appear in different projected pairs; Path TRAIL constraints
/// apply separately to all-shortest bindings, not these analytical distances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CentralityOptions {
	/// Hop distance or strictly positive stored connector costs.
	pub weights: WeightSelection,
	/// Uses standard domain-size normalization when true.
	pub normalized: bool,
	/// Hard operation resource ceilings.
	pub limits: GraphAlgorithmLimits,
}
impl Default for CentralityOptions {
	/// Constructs the default initial state of `CentralityOptions`.
	fn default() -> Self {
		Self {
			weights: WeightSelection::Unweighted,
			normalized: true,
			limits: GraphAlgorithmLimits::default(),
		}
	}
}

/// Shifted eigenvector iteration controls.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct SpectralOptions {
	/// Unit connector contributions or finite nonnegative strengths.
	pub weights: WeightSelection,
	/// Stopping controls, checked against the original eigenproblem residual.
	pub iteration: IterationOptions,
	/// Hard operation resource ceilings.
	pub limits: GraphAlgorithmLimits,
}

/// Katz affine iteration controls.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct KatzOptions {
	/// None selects 0.5/max(1,kappa); explicit alpha requires alpha*kappa<1.
	pub alpha: Option<f64>,
	/// Finite nonnegative constant source, default one.
	pub beta: f64,
	/// Sparse strengths and stopping controls.
	pub spectral: SpectralOptions,
}
impl Default for KatzOptions {
	/// Constructs the default initial state of `KatzOptions`.
	fn default() -> Self {
		Self {
			alpha: None,
			beta: 1.0,
			spectral: SpectralOptions::default(),
		}
	}
}

/// Selects the mathematical orientation of structural/community metrics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum AnalyticsOrientation {
	/// Requires symmetric projected binary topology and selected strengths.
	#[default]
	Undirected,
	/// Preserves ordered arcs and reciprocal connections.
	Directed,
}

/// Exact triangle count controls; loops and parallel arcs are collapsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TriangleOptions {
	/// Undirected triples, or directed cycles and transitive triples.
	pub orientation: AnalyticsOrientation,
	/// Hard work and allocation limits.
	pub limits: GraphAlgorithmLimits,
}

/// Exact motif counts, either local participation or global distinct motifs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TriangleCounts {
	/// Unordered fully connected undirected triples.
	pub undirected: u64,
	/// Oriented three-cycles, distinct up to cyclic rotation.
	pub cycles: u64,
	/// Ordered source-middle-target transitive triples.
	pub transitive: u64,
}

/// Complete motif participation and global counts.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TriangleResult {
	/// Qualified per-vertex participation, with exact parameters and counters.
	pub local: AnalyticsResult<TriangleCounts, TriangleOptions>,
	/// Global distinct motif counts.
	pub totals: TriangleCounts,
}

/// Local/mean/transitivity clustering controls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ClusteringOptions {
	/// Undirected coefficients or directed Fagiolo coefficients.
	pub orientation: AnalyticsOrientation,
	/// Binary motifs, or cube-root strengths normalized by their maximum.
	pub weights: WeightSelection,
	/// Hard operation budgets.
	pub limits: GraphAlgorithmLimits,
}

/// Complete local coefficients and deterministic global reductions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ClusteringResult {
	/// Qualified coefficients; zero denominators produce zero.
	pub local: AnalyticsResult<f64, ClusteringOptions>,
	/// Arithmetic mean over all vertices, including isolated vertices.
	pub mean: f64,
	/// Sum of motif numerators divided by sum of possible-motif denominators.
	pub transitivity: f64,
}

/// Canonical sequential label propagation controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct LabelPropagationOptions {
	/// Incoming, outgoing, or combined votes relative to projected arcs.
	pub votes: crate::graph::TraversalDirection,
	/// Parallel unit votes or finite nonnegative stored strengths.
	pub weights: WeightSelection,
	/// Positive sweep limit; exhaustion is an error.
	pub max_sweeps: usize,
	/// Hard work and scratch ceilings.
	pub limits: GraphAlgorithmLimits,
}

impl Default for LabelPropagationOptions {
	/// Constructs the default initial state of `LabelPropagationOptions`.
	fn default() -> Self {
		Self {
			votes: crate::graph::TraversalDirection::Both,
			weights: WeightSelection::Unweighted,
			max_sweeps: 1000,
			limits: GraphAlgorithmLimits::default(),
		}
	}
}

/// Deterministic directed/undirected Louvain controls.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct LouvainOptions {
	/// Undirected requires symmetric selected strengths.
	pub orientation: AnalyticsOrientation,
	/// Unit parallel contributions or finite nonnegative strengths.
	pub weights: WeightSelection,
	/// Positive finite resolution gamma, default one.
	pub resolution: f64,
	/// Positive maximum canonical local sweeps per level.
	pub max_sweeps: usize,
	/// Positive maximum aggregation levels.
	pub max_levels: usize,
	/// Hard scratch/work limits.
	pub limits: GraphAlgorithmLimits,
}

impl Default for LouvainOptions {
	/// Constructs the default initial state of `LouvainOptions`.
	fn default() -> Self {
		Self {
			orientation: AnalyticsOrientation::Undirected,
			weights: WeightSelection::Unweighted,
			resolution: 1.0,
			max_sweeps: 100,
			max_levels: 64,
			limits: GraphAlgorithmLimits::default(),
		}
	}
}

/// Complete qualified community assignments and convergence evidence.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct CommunityResult<P> {
	/// Community identities are the minimum canonical member of each group.
	pub assignments: AnalyticsResult<TensorAtom, P>,
	/// Louvain objective, absent for label propagation.
	pub modularity: Option<f64>,
	/// Completed aggregation levels, or one label-propagation level.
	pub levels: usize,
}

/// Admits the complete qualified owned result before copying identity payloads.
pub(super) fn finish<T, P>(
	projection: &TensorProjection,
	values: Vec<T>,
	parameters: P,
	convergence: Option<ConvergenceMetadata>,
	control: &mut Control<'_>,
) -> FerrosGrynnResult<AnalyticsResult<T, P>> {
	if values.len() != projection.atoms.len() {
		return Err(invalid("analytical result does not cover its complete axis"));
	}
	let mut output = control.array(values.len())?;
	for label in &projection.options.labels {
		control.step()?;
		control.admit(
			128_usize
				.checked_add(label.len())
				.ok_or_else(crate::graph::tensor::control::overflow)?,
		)?;
	}
	for (atom, value) in projection.atoms.iter().zip(values) {
		control.step()?;
		control.admit(atom.heap_bytes())?;
		output.push((atom.clone(), value));
	}
	Ok(AnalyticsResult {
		values: output,
		parameters,
		selection: projection.options.clone(),
		scope: projection.scope,
		convergence,
		work: control.work,
		admitted_bytes: control.bytes,
	})
}
