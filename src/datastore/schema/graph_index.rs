//! Graph-owned snapshot index definitions. Physical bindings remain
//! storage-owned.

use std::collections::BTreeSet;

use super::{ClassIndexState, FunctionId, IndexId, IndexMaintenanceMode, SchemaVersion};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{GraphId, GraphKind, TraversalDirection, TraversalSemantics, tensor::TensorScope},
	values::Text,
};

/// Reachability preprocessing and its bounded label parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ReachabilityAlgorithm {
	/// Selects a fitting algorithm from tree, chain, interval, and Bloom
	/// labels.
	Auto,
	/// Exact intervals, restricted to tree families.
	TreeCover,
	/// Negative filter with one through thirty-two interval dimensions.
	Grail(u8),
	/// Exact/approximate interval cover with one through 255 ranges.
	Ferrari(u8),
	/// Negative Bloom filter with 64 through 4096 bits, divisible by eight.
	Bfl(u16),
	/// Exact chain matrix; requires an explicit artifact budget.
	ChainCover,
	/// Performs exact traversal without a materialized artifact.
	OnDemand,
}

/// Neighbourhood storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum NeighbourhoodAlgorithm {
	/// Chooses an exact materialization only when it fits the budget.
	Auto,
	/// Distinct members at each radius.
	Exact,
	/// HyperLogLog with the supplied register precision, default twelve.
	Sketch(u8),
	/// Exact cached neighbourhoods for the supplied number of landmarks.
	Landmark(u16),
	/// Performs exact traversal without a materialized artifact.
	OnDemand,
}

/// Path acceleration policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PathIndexAlgorithm {
	/// Directed ALT distances for one through 256 deterministic landmarks.
	LandmarkSssp(u16),
	/// Exact bindings for bounded, parameter-free patterns.
	PatternCache,
	/// Executes the existing exact shortest-path or pattern pipeline.
	OnDemand,
}

/// Additive path cost captured by a definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum GraphIndexCost {
	/// Unsigned integral hop count.
	Hop,
	/// Built-in nonnegative finite edge weights.
	Weighted,
	/// Exact named function identity and semantic version.
	Function {
		/// Durable function identity.
		function: FunctionId,
		/// Captured definition version.
		version: SchemaVersion,
	},
}

/// Physical membership representation for a complete ATOM endpoint cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum TensorCacheFormat {
	/// Selects the smaller admitted encoded representation.
	#[default]
	Auto,
	/// Sorted sparse connector ordinals for both membership planes.
	Csr,
	/// Two row-major, LSB-first binary membership planes.
	Dense,
}

/// Kind-specific graph index definition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GraphIndexDefinition {
	/// Unbounded reachability over one selected incidence projection.
	Reachability {
		/// Label-building algorithm.
		algorithm: ReachabilityAlgorithm,
	},
	/// Distinct members or explicitly approximate counts within a radius.
	Neighbourhood {
		/// Maximum query radius; materialization is restricted to four.
		radius: u32,
		/// Exact, sketch, landmark, or traversal policy.
		algorithm: NeighbourhoodAlgorithm,
	},
	/// Shortest-path distances or reusable exact pattern bindings.
	Path {
		/// Path acceleration algorithm.
		algorithm: PathIndexAlgorithm,
		/// Additive cost model.
		cost: GraphIndexCost,
		/// Pattern source; executable IR is never persisted.
		patterns: Vec<Text>,
	},
	/// Complete qualified incidence, topology, containment and captured
	/// weights.
	Tensor {
		/// LOCAL includes boundary endpoints; ROOTED also includes child
		/// domains.
		scope: TensorScope,
		/// Requested membership encoding; AUTO selects a concrete format at
		/// build.
		format: TensorCacheFormat,
	},
}

/// Versioned user definition owned by exactly one graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphIndex {
	/// Stable identity assigned by the shared schema clock.
	pub(crate) id: Option<IndexId>,
	/// Semantic definition version.
	pub(crate) version: SchemaVersion,
	/// Owning graph identity assigned at publication.
	pub(crate) graph: Option<GraphId>,
	/// Case-sensitive graph-local name.
	pub(crate) name: Text,
	/// Kind-specific options.
	pub(crate) definition: GraphIndexDefinition,
	/// NODE or ATOM incidence projection.
	pub(crate) mode: TraversalSemantics,
	/// OUT, IN, or BOTH relation direction.
	pub(crate) direction: TraversalDirection,
	/// Empty means all edge labels; otherwise any listed label matches.
	pub(crate) edge_labels: BTreeSet<Text>,
	/// Artifact pages and metadata admission limit.
	pub(crate) max_bytes: Option<u64>,
	/// Catalog availability; physical headers do not duplicate this state.
	pub(crate) state: ClassIndexState,
	/// Scheduling policy; materialized graph artifacts never receive row
	/// updates.
	pub(crate) maintenance: IndexMaintenanceMode,
	/// Source revision of the last published artifact, zero before initial
	/// build.
	pub(crate) build_revision: u64,
	/// Exact versions of all function dependencies.
	pub(crate) dependencies: BTreeSet<(FunctionId, SchemaVersion)>,
	/// Optional catalog comment.
	pub(crate) comment: Option<Text>,
}

impl GraphIndex {
	/// Creates an unpublished checked definition with NODE/OUT defaults.
	///
	/// # Errors
	/// Returns an invalid-argument error for an invalid name, parameter,
	/// materialization radius, pattern list, or missing required budget.
	pub fn new<IntoText: Into<Text>>(
		name: IntoText,
		definition: GraphIndexDefinition,
		max_bytes: Option<u64>,
	) -> FerrosGrynnResult<Self> {
		let mut index = Self {
			id: None,
			version: SchemaVersion::INITIAL,
			graph: None,
			name: name.into(),
			definition,
			mode: TraversalSemantics::Node,
			direction: TraversalDirection::Forward,
			edge_labels: BTreeSet::new(),
			max_bytes,
			state: ClassIndexState::Stale,
			maintenance: IndexMaintenanceMode::Immediate,
			build_revision: 0,
			dependencies: BTreeSet::new(),
			comment: None,
		};
		if matches!(index.definition, GraphIndexDefinition::Tensor { .. }) {
			index.mode = TraversalSemantics::Atom;
			index.direction = TraversalDirection::Both;
		}
		if let GraphIndexDefinition::Path {
			cost: GraphIndexCost::Function {
				function,
				version,
			},
			..
		} = index.definition
		{
			index.dependencies.insert((function, version));
		}
		index.validate()?;
		if index.is_on_demand() {
			index.state = ClassIndexState::Ready;
		}
		Ok(index)
	}

	/// Creates a complete ATOM tensor cache with an explicit artifact byte cap.
	/// NODE and label/direction projections remain views over the captured
	/// data.
	///
	/// # Errors
	/// Returns invalid name, scope, format or zero-budget errors.
	///
	/// # Examples
	/// ```
	/// use ferros_grynn::{
	/// 	datastore::{Datastore, GraphIndex, MaintenanceOptions, TensorCacheFormat},
	/// 	graph::{
	/// 		GraphKind,
	/// 		tensor::{TensorOptions, TensorScope},
	/// 	},
	/// };
	/// # fn main() -> ferros_grynn::gql_status::FerrosGrynnResult<()> {
	/// let store = Datastore::in_memory();
	/// let mut tx = store.begin();
	/// let graph = tx.define_graph("example", GraphKind::Graph)?;
	/// let id = tx.define_graph_index(
	/// 	"example",
	/// 	GraphIndex::new_tensor("incidence", TensorScope::Local, TensorCacheFormat::Auto, 1 << 20)?,
	/// )?;
	/// tx.commit()?;
	/// store.rebuild_graph_index(graph.id(), id, MaintenanceOptions::default())?;
	/// let mut read = store.begin();
	/// let tensor = read.endpoint_tensor_using_index(&graph, TensorOptions::default(), id)?;
	/// assert_eq!(tensor.shape(), (0, 0, 2));
	/// assert!(read.graph_index("example", "incidence")?.is_some());
	/// read.remove_graph_index("example", "incidence")?;
	/// read.commit()?;
	/// # Ok(())
	/// # }
	/// ```
	pub fn new_tensor<IntoText: Into<Text>>(
		name: IntoText,
		scope: TensorScope,
		format: TensorCacheFormat,
		max_bytes: u64,
	) -> FerrosGrynnResult<Self> {
		Self::new(
			name,
			GraphIndexDefinition::Tensor {
				scope,
				format,
			},
			Some(max_bytes),
		)
	}

	/// Selects the incidence projection stored by this definition.
	#[must_use]
	pub const fn with_mode(mut self, mode: TraversalSemantics) -> Self {
		self.mode = mode;
		self
	}

	/// Selects the direction of the stored relation.
	#[must_use]
	pub const fn with_direction(mut self, direction: TraversalDirection) -> Self {
		self.direction = direction;
		self
	}

	/// Restricts connectors to the supplied exact labels in canonical order.
	///
	/// # Errors
	/// Returns an invalid-argument error for invalid labels or more than 1024
	/// labels.
	pub fn with_edge_labels<IntoText: Into<Text>>(
		mut self,
		labels: impl IntoIterator<Item = IntoText>,
	) -> FerrosGrynnResult<Self> {
		for label in labels {
			let label = label.into();
			super::catalog::validate_catalog_name(label.as_str(), "edge label")?;
			if self.edge_labels.len() == 1024 && !self.edge_labels.contains(&label) {
				return Err(FerrosGrynnError::invalid_argument(
					"graph index supports at most 1024 edge labels",
				));
			}
			self.edge_labels.insert(label);
		}
		Ok(self)
	}

	/// Selects the existing maintenance scheduling policy.
	#[must_use]
	pub const fn with_maintenance_mode(mut self, maintenance: IndexMaintenanceMode) -> Self {
		self.maintenance = maintenance;
		self
	}

	/// Returns the assigned stable identity, if published.
	pub const fn id(&self) -> Option<IndexId> {
		self.id
	}

	/// Returns the definition version.
	pub const fn version(&self) -> SchemaVersion {
		self.version
	}

	/// Returns the owning graph identity, if published.
	pub const fn graph(&self) -> Option<GraphId> {
		self.graph
	}

	/// Returns the graph-local name.
	pub const fn name(&self) -> &Text {
		&self.name
	}

	/// Returns the kind-specific options.
	pub const fn definition(&self) -> &GraphIndexDefinition {
		&self.definition
	}

	/// Returns the incidence projection.
	pub const fn mode(&self) -> TraversalSemantics {
		self.mode
	}

	/// Returns the relation direction.
	pub const fn direction(&self) -> TraversalDirection {
		self.direction
	}

	/// Returns the exact connector label filter.
	pub fn edge_labels(&self) -> impl ExactSizeIterator<Item = &Text> {
		self.edge_labels.iter()
	}

	/// Returns the artifact admission limit.
	pub const fn max_bytes(&self) -> Option<u64> {
		self.max_bytes
	}

	/// Returns catalog availability.
	pub const fn state(&self) -> ClassIndexState {
		self.state
	}

	/// Returns maintenance scheduling policy.
	pub const fn maintenance_mode(&self) -> IndexMaintenanceMode {
		self.maintenance
	}

	/// Returns the source revision of the published artifact.
	pub const fn build_revision(&self) -> u64 {
		self.build_revision
	}

	/// Returns exact function dependency identities and versions.
	pub fn dependencies(&self) -> impl ExactSizeIterator<Item = &(FunctionId, SchemaVersion)> {
		self.dependencies.iter()
	}

	/// Returns the optional catalog comment.
	pub const fn comment(&self) -> Option<&Text> {
		self.comment.as_ref()
	}

	/// Returns the stable fingerprint of the complete semantic definition.
	///
	/// # Errors
	/// Returns a definition validation or checked-length error.
	pub fn fingerprint(&self) -> FerrosGrynnResult<u64> {
		Ok(rapidhash::v3::rapidhash_v3(&self.descriptor()?))
	}

	/// Encodes canonical semantic bytes shared by catalog and binding
	/// validation.
	pub(crate) fn descriptor(&self) -> FerrosGrynnResult<Vec<u8>> {
		self.descriptor_with_limit(8 << 20)
	}

	/// Encodes semantic bytes only after checked aggregate allocation
	/// admission.
	///
	/// # Errors
	/// Returns validation or resource errors before allocating the descriptor.
	pub(crate) fn descriptor_with_limit(&self, max_bytes: usize) -> FerrosGrynnResult<Vec<u8>> {
		self.validate()?;
		let (kind, algorithm, parameter, radius, cost, patterns) = match &self.definition {
			GraphIndexDefinition::Reachability {
				algorithm,
			} => {
				let (tag, parameter) = match algorithm {
					ReachabilityAlgorithm::Auto => (0, 0),
					ReachabilityAlgorithm::TreeCover => (1, 0),
					ReachabilityAlgorithm::Grail(value) => (2, u32::from(*value)),
					ReachabilityAlgorithm::Ferrari(value) => (3, u32::from(*value)),
					ReachabilityAlgorithm::Bfl(value) => (4, u32::from(*value)),
					ReachabilityAlgorithm::ChainCover => (5, 0),
					ReachabilityAlgorithm::OnDemand => (6, 0),
				};
				(0, tag, parameter, 0, GraphIndexCost::Hop, &[][..])
			}
			GraphIndexDefinition::Neighbourhood {
				radius,
				algorithm,
			} => {
				let (tag, parameter) = match algorithm {
					NeighbourhoodAlgorithm::Auto => (0, 0),
					NeighbourhoodAlgorithm::Exact => (1, 0),
					NeighbourhoodAlgorithm::Sketch(value) => (2, u32::from(*value)),
					NeighbourhoodAlgorithm::Landmark(value) => (3, u32::from(*value)),
					NeighbourhoodAlgorithm::OnDemand => (4, 0),
				};
				(1, tag, parameter, *radius, GraphIndexCost::Hop, &[][..])
			}
			GraphIndexDefinition::Path {
				algorithm,
				cost,
				patterns,
			} => {
				let (tag, parameter) = match algorithm {
					PathIndexAlgorithm::LandmarkSssp(value) => (0, u32::from(*value)),
					PathIndexAlgorithm::PatternCache => (1, 0),
					PathIndexAlgorithm::OnDemand => (2, 0),
				};
				(2, tag, parameter, 0, *cost, patterns.as_slice())
			}
			GraphIndexDefinition::Tensor {
				scope,
				format,
			} => (
				3,
				match format {
					TensorCacheFormat::Auto => 0,
					TensorCacheFormat::Csr => 1,
					TensorCacheFormat::Dense => 2,
				},
				u32::from(*scope == TensorScope::Rooted),
				0,
				GraphIndexCost::Hop,
				&[][..],
			),
		};
		let resource = || {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"graph index descriptor bytes",
				max_bytes as u64,
			)
		};
		let length = self.edge_labels.iter().chain(patterns).try_fold(
			68_usize
				.checked_add(self.name.len())
				.and_then(|bytes| {
					self.dependencies
						.len()
						.checked_mul(8)
						.and_then(|dependencies| bytes.checked_add(dependencies))
				})
				.ok_or_else(resource)?,
			|bytes, text| {
				bytes
					.checked_add(4)
					.and_then(|bytes| bytes.checked_add(text.len()))
					.ok_or_else(resource)
			},
		)?;
		if length > max_bytes {
			return Err(resource());
		}
		let mut bytes = Vec::new();
		bytes.try_reserve_exact(length).map_err(|cause| resource().with_technical_source(cause))?;
		bytes.extend_from_slice(&self.id.map_or(0, IndexId::get).to_le_bytes());
		bytes.extend_from_slice(&self.version.get().to_le_bytes());
		bytes.extend_from_slice(&self.graph.map_or(0, GraphId::get).to_le_bytes());
		bytes.push(match self.mode {
			TraversalSemantics::Node => 0,
			TraversalSemantics::Atom => 1,
		});
		bytes.push(match self.direction {
			TraversalDirection::Forward => 0,
			TraversalDirection::Backward => 1,
			TraversalDirection::Both => 2,
		});
		bytes.push(match self.maintenance {
			IndexMaintenanceMode::Immediate => 0,
			IndexMaintenanceMode::Deferred => 1,
		});
		bytes.extend_from_slice(&[kind, algorithm, 0, 0, 0]);
		bytes.extend_from_slice(&parameter.to_le_bytes());
		bytes.extend_from_slice(&radius.to_le_bytes());
		bytes.extend_from_slice(&self.max_bytes.unwrap_or(0).to_le_bytes());
		let (cost_tag, function, version) = match cost {
			GraphIndexCost::Hop => (0, 0, 0),
			GraphIndexCost::Weighted => (1, 0, 0),
			GraphIndexCost::Function {
				function,
				version,
			} => (2, function.get(), version.get()),
		};
		bytes.extend_from_slice(&[cost_tag, 0, 0, 0]);
		bytes.extend_from_slice(&function.to_le_bytes());
		bytes.extend_from_slice(&version.to_le_bytes());
		append_text(&mut bytes, &self.name)?;
		bytes.extend_from_slice(&(self.edge_labels.len() as u32).to_le_bytes());
		for label in &self.edge_labels {
			append_text(&mut bytes, label)?;
		}
		bytes.extend_from_slice(&(self.dependencies.len() as u32).to_le_bytes());
		for (function, version) in &self.dependencies {
			bytes.extend_from_slice(&function.get().to_le_bytes());
			bytes.extend_from_slice(&version.get().to_le_bytes());
		}
		bytes.extend_from_slice(&(patterns.len() as u32).to_le_bytes());
		for pattern in patterns {
			append_text(&mut bytes, pattern)?;
		}
		Ok(bytes)
	}

	/// Identifies policies which require no materialized side file.
	pub(crate) fn is_on_demand(&self) -> bool {
		matches!(
			self.definition,
			GraphIndexDefinition::Reachability {
				algorithm: ReachabilityAlgorithm::OnDemand
			} | GraphIndexDefinition::Neighbourhood {
				algorithm: NeighbourhoodAlgorithm::OnDemand,
				..
			} | GraphIndexDefinition::Neighbourhood {
				radius: 1,
				..
			} | GraphIndexDefinition::Path {
				algorithm: PathIndexAlgorithm::OnDemand,
				..
			}
		) || matches!(self.definition, GraphIndexDefinition::Neighbourhood {
			radius: 5..,
			algorithm: NeighbourhoodAlgorithm::Auto
		})
	}

	/// Validates definition parameters at SDK and durable decode boundaries.
	pub(crate) fn validate(&self) -> FerrosGrynnResult<()> {
		super::catalog::validate_catalog_name(self.name.as_str(), "graph index")?;
		if matches!(self.name.as_str(), "label" | "adjacency" | "edge_endpoint") {
			return Err(FerrosGrynnError::invalid_argument(
				"engine-owned graph index cannot be defined by a user",
			));
		}
		if self.name.len() > 4096 || self.dependencies.len() > 1024 {
			return Err(FerrosGrynnError::invalid_argument(
				"graph index name or dependency count exceeds its catalog bound",
			));
		}
		if self.max_bytes == Some(0) {
			return Err(FerrosGrynnError::invalid_argument("MAX_BYTES must be positive"));
		}
		let requires_budget = match &self.definition {
			GraphIndexDefinition::Tensor {
				..
			} => {
				if self.mode != TraversalSemantics::Atom
					|| self.direction != TraversalDirection::Both
					|| !self.edge_labels.is_empty()
					|| !self.dependencies.is_empty()
				{
					return Err(FerrosGrynnError::invalid_argument(
						"tensor cache stores complete ATOM incidence without label or direction \
						 filters",
					));
				}
				true
			}
			GraphIndexDefinition::Reachability {
				algorithm,
			} => {
				if matches!(algorithm, ReachabilityAlgorithm::Grail(0 | 33..))
					|| matches!(algorithm, ReachabilityAlgorithm::Ferrari(0))
					|| matches!(algorithm, ReachabilityAlgorithm::Bfl(bits) if !(64..=4096).contains(bits) || bits % 8 != 0)
				{
					return Err(FerrosGrynnError::invalid_argument(
						"invalid reachability algorithm parameter",
					));
				}
				matches!(algorithm, ReachabilityAlgorithm::Auto | ReachabilityAlgorithm::ChainCover)
			}
			GraphIndexDefinition::Neighbourhood {
				radius,
				algorithm,
			} => {
				if *radius == 0
					|| *radius > 4
						&& !matches!(
							algorithm,
							NeighbourhoodAlgorithm::Auto | NeighbourhoodAlgorithm::OnDemand
						)
					|| matches!(algorithm, NeighbourhoodAlgorithm::Sketch(precision) if !(4..=18).contains(precision))
					|| matches!(algorithm, NeighbourhoodAlgorithm::Landmark(0 | 257..))
				{
					return Err(FerrosGrynnError::invalid_argument(
						"invalid neighbourhood radius or algorithm parameter",
					));
				}
				*radius > 1
					&& *radius <= 4
					&& !matches!(algorithm, NeighbourhoodAlgorithm::OnDemand)
			}
			GraphIndexDefinition::Path {
				algorithm,
				patterns,
				..
			} => {
				if matches!(algorithm, PathIndexAlgorithm::LandmarkSssp(0 | 257..)) {
					return Err(FerrosGrynnError::invalid_argument(
						"LANDMARK_SSSP count must be in 1..=256",
					));
				}
				let bytes = patterns
					.iter()
					.try_fold(0_usize, |total, pattern| total.checked_add(pattern.len()));
				if patterns.len() > 64
					|| bytes.is_none_or(|bytes| bytes > 1024 * 1024)
					|| patterns
						.iter()
						.any(|pattern| pattern.is_empty() || pattern.as_str().contains('\0'))
					|| matches!(algorithm, PathIndexAlgorithm::PatternCache) != !patterns.is_empty()
				{
					return Err(FerrosGrynnError::invalid_argument(
						"PATTERN_CACHE requires a nonempty bounded pattern list",
					));
				}
				!matches!(algorithm, PathIndexAlgorithm::OnDemand)
			}
		};
		if requires_budget && self.max_bytes.is_none() {
			return Err(FerrosGrynnError::invalid_argument("this graph index requires MAX_BYTES"));
		}
		if self.edge_labels.len() > 1024 {
			return Err(FerrosGrynnError::invalid_argument("too many graph index labels"));
		}
		for label in &self.edge_labels {
			super::catalog::validate_catalog_name(label.as_str(), "edge label")?;
			if label.len() > 4096 {
				return Err(FerrosGrynnError::invalid_argument(
					"graph index label exceeds its catalog bound",
				));
			}
		}
		if self
			.comment
			.as_ref()
			.is_some_and(|comment| comment.len() > 4096 || comment.as_str().contains('\0'))
		{
			return Err(FerrosGrynnError::invalid_argument(
				"graph index comment exceeds its catalog bound or contains NUL",
			));
		}
		if let GraphIndexDefinition::Path {
			cost: GraphIndexCost::Function {
				function,
				version,
			},
			..
		} = self.definition
			&& !self.dependencies.contains(&(function, version))
		{
			return Err(FerrosGrynnError::invalid_argument(
				"graph index cost is missing its function dependency",
			));
		}
		let mut previous_function = None;
		for &(function, _) in &self.dependencies {
			if previous_function == Some(function) {
				return Err(FerrosGrynnError::invalid_argument(
					"graph index captures multiple versions of one function",
				));
			}
			previous_function = Some(function);
		}
		Ok(())
	}

	/// Rejects tree intervals on graph families which do not promise a tree
	/// relation.
	pub(crate) fn validate_family(&self, family: GraphKind) -> FerrosGrynnResult<()> {
		if matches!(self.definition, GraphIndexDefinition::Reachability {
			algorithm: ReachabilityAlgorithm::TreeCover
		}) && !matches!(family, GraphKind::Tree | GraphKind::RootedTree | GraphKind::Forest)
		{
			return Err(FerrosGrynnError::invalid_argument("TREE_COVER requires a tree family"));
		}
		Ok(())
	}
}

/// Appends length-prefixed UTF-8 without platform-dependent integer widths.
fn append_text(bytes: &mut Vec<u8>, text: &Text) -> FerrosGrynnResult<()> {
	let length = u32::try_from(text.len())
		.map_err(|_error| FerrosGrynnError::invalid_argument("graph index text exceeds u32"))?;
	bytes.extend_from_slice(&length.to_le_bytes());
	bytes.extend_from_slice(text.as_str().as_bytes());
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn graph_definition_bounds_and_fingerprints() {
		let reach = |algorithm, budget| {
			GraphIndex::new(
				"reach",
				GraphIndexDefinition::Reachability {
					algorithm,
				},
				budget,
			)
		};
		assert!(reach(ReachabilityAlgorithm::Grail(0), None).is_err());
		assert!(reach(ReachabilityAlgorithm::Grail(33), None).is_err());
		assert!(reach(ReachabilityAlgorithm::Bfl(159), None).is_err());
		assert!(reach(ReachabilityAlgorithm::ChainCover, None).is_err());
		assert!(reach(ReachabilityAlgorithm::Auto, None).is_err());
		let base = reach(ReachabilityAlgorithm::Grail(2), None).unwrap();
		assert_eq!(base.state(), ClassIndexState::Stale);
		assert_eq!(base.mode(), TraversalSemantics::Node);
		assert_ne!(
			base.fingerprint().unwrap(),
			base.clone().with_mode(TraversalSemantics::Atom).fingerprint().unwrap()
		);
		assert_ne!(
			base.fingerprint().unwrap(),
			base.clone().with_direction(TraversalDirection::Both).fingerprint().unwrap()
		);
		assert_eq!(
			base.clone().with_edge_labels(["b", "a"]).unwrap().fingerprint().unwrap(),
			base.with_edge_labels(["a", "b", "a"]).unwrap().fingerprint().unwrap()
		);
		assert!(
			GraphIndex::new(
				"nh",
				GraphIndexDefinition::Neighbourhood {
					radius: 5,
					algorithm: NeighbourhoodAlgorithm::Exact
				},
				Some(1_000_000)
			)
			.is_err()
		);
		let automatic = GraphIndex::new(
			"nh",
			GraphIndexDefinition::Neighbourhood {
				radius: 5,
				algorithm: NeighbourhoodAlgorithm::Auto,
			},
			None,
		)
		.unwrap();
		assert!(automatic.is_on_demand());
		assert_eq!(automatic.state(), ClassIndexState::Ready);
	}
}
