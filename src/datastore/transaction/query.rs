use std::{
	collections::{BTreeMap, BTreeSet, btree_set},
	ops::Bound,
	sync::Arc,
};

use super::Transaction;
use crate::{
	datastore::{
		ClassIndex,
		ClassSchema,
		DatabaseRoot,
		GlobalAtomId,
		GraphOccurrence,
		GraphRoot,
		GraphSchema,
		IndexId,
		OccurrenceState,
		OverlayValue,
		index_context,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomOccurrenceRef, GraphId, GraphRef},
	storage::{
		LocalQuerySnapshot,
		PhysicalIndexAtom,
		PhysicalIndexDelta,
		PhysicalIndexEntries,
		PhysicalIndexRead,
		PhysicalScalarGroup,
	},
	values::{AtomKind, PropertyDictionary, PropertyMap, RecordId, Text, Value, Vector},
};

/// One owned atom row passed from datastore cursors to query execution.
#[derive(Clone)]
pub(crate) struct QueryStorageRow {
	pub(crate) record_id: RecordId,
	pub(crate) kind: AtomKind,
	pub(crate) local_id: Option<AtomId>,
	pub(crate) label: Option<Text>,
	pub(crate) values: Vec<Value>,
	pub(crate) base_properties: PropertyMap,
	pub(crate) properties: PropertyMap,
}

/// One query term retained from a validated immutable Fulltext side-file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryFulltextTermRead {
	pub(crate) document_frequency: u64,
	pub(crate) term_frequency: u32,
	pub(crate) spans: Vec<(u32, u32)>,
}

/// One matching document projected out of storage-owned index addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryFulltextDocumentRead {
	pub(crate) record_id: RecordId,
	pub(crate) document_length: u32,
	pub(crate) terms: BTreeMap<Box<str>, QueryFulltextTermRead>,
}

/// Snapshot-exact Fulltext statistics and matching public identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueryFulltextIndexRead {
	pub(crate) document_count: u64,
	pub(crate) total_document_length: u64,
	pub(crate) documents: Vec<QueryFulltextDocumentRead>,
}

/// One public row identity returned by a physical ANN generation after exact
/// reranking of its full canonical vector.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueryVectorIndexRead {
	pub(crate) record_id: RecordId,
	pub(crate) distance: f64,
}

/// Spatial candidates expressed only through public identities and work
/// counters.
#[derive(Default)]
pub(crate) struct QueryGeometryIndexRead {
	/// Snapshot-stable row identities requiring exact refinement.
	pub(crate) records: Vec<RecordId>,
	/// Tree node pages visited by the lookup.
	pub(crate) page_reads: u64,
	/// Exact replacement records consulted.
	pub(crate) delta_records: u64,
}

/// Logical availability metadata for one Ready unfiltered graph sketch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryNeighbourhoodSketch {
	/// Owning logical graph scope.
	pub(crate) graph: GraphId,
	/// Stable catalog identity.
	pub(crate) index: IndexId,
	/// Captured semantic definition version.
	pub(crate) version: crate::datastore::SchemaVersion,
	/// Snapshot revision used by the immutable artifact.
	pub(crate) revision: u64,
	/// Stored NODE/ATOM incidence projection.
	pub(crate) mode: crate::graph::TraversalSemantics,
	/// Stored relation direction.
	pub(crate) direction: crate::graph::TraversalDirection,
	/// Maximum materialized radius.
	pub(crate) radius: u32,
	/// Register precision, never an exact-count guarantee.
	pub(crate) precision: u8,
	/// Complete artifact pages and metadata bytes.
	pub(crate) bytes: u64,
}

/// Logical binding for one snapshot-exact directed landmark generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryLandmarkPath {
	/// Graph scope and immutable catalog identity.
	pub(crate) graph: GraphId,
	/// Catalog index identity.
	pub(crate) index: IndexId,
	/// Definition version and captured build revision.
	pub(crate) version: crate::datastore::SchemaVersion,
	/// Commit revision captured by the builder.
	pub(crate) revision: u64,
	/// Incidence projection, direction and additive cost.
	pub(crate) mode: crate::graph::TraversalSemantics,
	/// Direction of the projected relation.
	pub(crate) direction: crate::graph::TraversalDirection,
	/// Additive cost captured by this generation.
	pub(crate) cost: crate::datastore::GraphIndexCost,
	/// Canonical connector label filter.
	pub(crate) edge_labels: Vec<Box<str>>,
	/// Actual landmark count and complete artifact footprint.
	pub(crate) landmarks: u32,
	/// Complete artifact pages and metadata bytes.
	pub(crate) bytes: u64,
}

/// Logical snapshot binding for one complete qualified incidence generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(clippy::redundant_pub_crate, reason = "Logical metadata is re-exported to the GQL adapter")]
pub(crate) struct QueryTensorCache {
	/// Owning graph and exact catalog index.
	pub(crate) graph: GraphId,
	/// Catalog index identity.
	pub(crate) index: IndexId,
	/// Captured semantic definition version.
	pub(crate) version: crate::datastore::SchemaVersion,
	/// Snapshot revision captured by the builder.
	pub(crate) revision: u64,
	/// Canonical definition fingerprint, checked with full bytes during lookup.
	pub(crate) fingerprint: u64,
	/// Complete LOCAL or ROOTED incidence scope.
	pub(crate) scope: crate::graph::tensor::TensorScope,
	/// Actual CSR or DENSE encoding; AUTO is never a stored format.
	pub(crate) format: crate::datastore::TensorCacheFormat,
	/// Full ATOM axis length.
	pub(crate) rows: u64,
	/// Qualified connector axis length.
	pub(crate) connectors: u64,
	/// Distinct source membership cells.
	pub(crate) source_cells: u64,
	/// Distinct target membership cells.
	pub(crate) target_cells: u64,
	/// Complete encoded generation footprint.
	pub(crate) bytes: u64,
	/// Durable incidence revisions; sharing avoids planner-to-cursor copies.
	pub(crate) dependencies: Arc<[(GraphId, u64)]>,
}

/// Explicit approximate count or bounded exact fallback, with logical work.
pub struct QueryNeighbourhoodEstimate {
	/// Distinct count including the source occurrence.
	pub(crate) count: u64,
	/// True only when a compatible Ready sketch supplied the estimate.
	pub(crate) approximate: bool,
	/// Admitted artifact bytes used by this lookup, zero for traversal.
	pub(crate) artifact_bytes: u64,
	/// Exact-fallback cooperative work, including relation construction.
	pub(crate) work: u64,
}

enum QueryCursorSource<'snapshot> {
	Empty,
	Base(btree_set::Iter<'snapshot, GlobalAtomId>),
	BaseOwned(std::vec::IntoIter<GlobalAtomId>),
	Graph {
		graph: GraphId,
		ids: btree_set::Iter<'snapshot, AtomId>,
	},
	GraphOwned {
		graph: GraphId,
		ids: std::vec::IntoIter<AtomId>,
	},
}

/// Snapshot-bound cursor over database-global atoms of one class.
pub(crate) struct QueryStorageCursor<'snapshot> {
	root: &'snapshot DatabaseRoot,
	class: Arc<ClassSchema>,
	source: QueryCursorSource<'snapshot>,
}

impl QueryStorageCursor<'_> {
	/// Pulls one owned row without materializing the remaining class.
	pub(crate) fn next_row(&mut self) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		let row = match &mut self.source {
			QueryCursorSource::Empty => return Ok(None),
			QueryCursorSource::Base(ids) => {
				let Some(id) = ids.next().copied() else {
					return Ok(None);
				};
				let atom = self.root.atom(id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"class index references an absent global atom",
					)
				})?;
				row_from_atom(atom, &self.class)
			}
			QueryCursorSource::BaseOwned(ids) => {
				let Some(id) = ids.next() else {
					return Ok(None);
				};
				let atom = self.root.atom(id).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scalar index references an absent global atom",
					)
				})?;
				row_from_atom(atom, &self.class)
			}
			QueryCursorSource::Graph {
				graph,
				ids,
			} => {
				let Some(local) = ids.next().copied() else {
					return Ok(None);
				};
				let graph = self.root.graph(*graph).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph class index references an absent graph",
					)
				})?;
				let occurrence = graph.occurrence(local).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph class index references an absent occurrence",
					)
				})?;
				row_from_occurrence(&self.root, occurrence, &self.class)?
			}
			QueryCursorSource::GraphOwned {
				graph,
				ids,
			} => {
				let Some(local) = ids.next() else {
					return Ok(None);
				};
				let graph = self.root.graph(*graph).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scalar index references an absent graph",
					)
				})?;
				let occurrence = graph.occurrence(local).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"scalar index references an absent occurrence",
					)
				})?;
				row_from_occurrence(&self.root, occurrence, &self.class)?
			}
		};
		Ok(Some(row))
	}
}

/// Scalar comparison available to a class-index probe.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ScalarComparison {
	Equal,
	Less,
	LessOrEqual,
	Greater,
	GreaterOrEqual,
}

/// One scalar predicate that may be answered by a class index.
#[derive(Debug, Clone)]
pub(crate) struct ScalarIndexConstraint {
	pub(crate) column: usize,
	pub(crate) comparison: ScalarComparison,
	pub(crate) values: Vec<Value>,
}

/// Bounded ordered-index window used by LIMIT and keyset continuation.
#[derive(Debug, Clone)]
pub(crate) struct ScalarIndexWindow {
	pub(crate) after: Option<(Vec<Value>, RecordId)>,
	pub(crate) maximum: usize,
}

/// Immutable root captured for one query program.
#[derive(Clone)]
pub(crate) struct QuerySnapshot {
	pub(super) root: Arc<DatabaseRoot>,
	pub(super) physical: Option<LocalQuerySnapshot>,
}

/// Independently stored statistics parts required by one planner snapshot.
#[derive(Debug, Default)]
pub(crate) struct QueryStatisticsDemand {
	pub(crate) base_classes: bool,
	pub(crate) base_properties: bool,
	pub(crate) graph_classes: BTreeSet<GraphId>,
	pub(crate) graph_labels: BTreeSet<GraphId>,
	pub(crate) graph_degrees: BTreeSet<GraphId>,
	pub(crate) graph_properties: BTreeSet<GraphId>,
}

/// One graph-local incidence returned by the datastore physical cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueryPhysicalTraversalCandidate {
	pub(crate) target: AtomId,
	pub(crate) edge: AtomId,
	pub(crate) direction: crate::graph::PathDirection,
	pub(crate) role: crate::graph::EndpointRole,
	pub(crate) topology: crate::graph::EdgeDirection,
}

/// Transport accounting retained without exposing pages or file identities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct QueryPhysicalTraversalStats {
	pub(crate) resident_pages: u64,
	pub(crate) wal_pages: u64,
	pub(crate) sync_home_pages: u64,
	pub(crate) ioring_home_pages: u64,
}

/// Complete owned result of one bounded physical frontier expansion.
pub(crate) struct QueryPhysicalTraversalRead {
	pub(crate) candidates: Vec<QueryPhysicalTraversalCandidate>,
	pub(crate) stats: QueryPhysicalTraversalStats,
}

/// One exact physical nested-graph lookup with transport accounting.
pub(crate) struct QueryPhysicalInnerGraphRead {
	pub(crate) graph: Option<GraphId>,
	pub(crate) stats: QueryPhysicalTraversalStats,
}

/// Snapshot-exact public atom fields decoded from local physical pages.
pub(crate) struct QueryPhysicalFieldsRead {
	pub(crate) record_id: RecordId,
	pub(crate) properties: PropertyMap,
	pub(crate) stats: QueryPhysicalTraversalStats,
}

impl QuerySnapshot {
	pub(crate) fn commit_ts(&self) -> u64 {
		self.root.commit_ts
	}
}

impl Transaction {
	/// Projects admitted Ready path bindings without physical addresses.
	///
	/// # Errors
	/// Rejects foreign graphs, expired transactions or allocation failures.
	pub(crate) fn query_landmark_paths(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Vec<QueryLandmarkPath>> {
		self.ensure_live()?;
		query_graph_root(&snapshot.root, reference)?;
		self.record_read(crate::datastore::ConflictKey::GraphDefinition(reference.id()));
		let schema =
			snapshot.root.schema.graph(reference.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
			})?;
		let mut result = Vec::new();
		result.try_reserve_exact(schema.indexes().len()).map_err(|cause| {
			FerrosGrynnError::invalid_argument("path binding allocation failed")
				.with_technical_source(cause)
		})?;
		for definition in schema
			.indexes()
			.filter(|index| index.state() == crate::datastore::ClassIndexState::Ready)
		{
			let crate::datastore::GraphIndexDefinition::Path {
				algorithm: crate::datastore::PathIndexAlgorithm::LandmarkSssp(_),
				cost,
				..
			} = definition.definition()
			else {
				continue;
			};
			let Some(id) = definition.id() else {
				continue;
			};
			let Some(artifact) = snapshot.root.path_artifacts.get(&id) else {
				continue;
			};
			let header = artifact.layout.header;
			if header.has_cross_level
				|| header.identity.version != definition.version().get()
				|| header.identity.revision != definition.build_revision()
			{
				continue;
			}
			result.push(QueryLandmarkPath {
				graph: reference.id(),
				index: id,
				version: definition.version(),
				revision: definition.build_revision(),
				mode: definition.mode(),
				direction: definition.direction(),
				cost: *cost,
				edge_labels: definition.edge_labels().map(|label| label.as_str().into()).collect(),
				landmarks: artifact.distances.landmarks.len() as u32,
				bytes: artifact.layout.page_count * 16_384,
			});
		}
		Ok(result)
	}

	/// Reads a directed admissible bound from the exact selected generation.
	///
	/// # Errors
	/// Rejects graph identity or corrupt ordinal/distance metadata.
	pub(crate) fn query_path_lower_bound(
		&self,
		snapshot: &QuerySnapshot,
		binding: &QueryLandmarkPath,
		source: AtomId,
		target: AtomId,
	) -> FerrosGrynnResult<Option<crate::values::Number>> {
		self.ensure_live()?;
		let Some(schema) = snapshot.root.schema.graph(binding.graph) else {
			return Ok(None);
		};
		let Some(definition) = schema.indexes().find(|index| index.id() == Some(binding.index))
		else {
			return Ok(None);
		};
		if definition.state() != crate::datastore::ClassIndexState::Ready
			|| definition.version() != binding.version
			|| definition.build_revision() != binding.revision
			|| definition.mode() != binding.mode
			|| definition.direction() != binding.direction
			|| !matches!(definition.definition(), crate::datastore::GraphIndexDefinition::Path { cost, .. } if *cost == binding.cost)
			|| !definition
				.edge_labels()
				.map(|label| label.as_str())
				.eq(binding.edge_labels.iter().map(Box::as_ref))
		{
			return Ok(None);
		}
		let Some(artifact) = snapshot.root.path_artifacts.get(&binding.index) else {
			return Ok(None);
		};
		if artifact.layout.header.has_cross_level
			|| artifact.layout.header.identity.version != binding.version.get()
			|| artifact.layout.header.identity.revision != binding.revision
		{
			return Ok(None);
		}
		self.record_reads([
			crate::datastore::ConflictKey::GraphDefinition(binding.graph),
			crate::datastore::ConflictKey::GraphKindRange(binding.graph, AtomKind::Node),
			crate::datastore::ConflictKey::GraphKindRange(binding.graph, AtomKind::Edge),
		]);
		if matches!(binding.cost, crate::datastore::GraphIndexCost::Function { .. }) {
			// A bound can prune unvisited connectors. SSI must therefore
			// observe every effective-property range captured by
			// this PURE artifact.
			for class in snapshot.root.schema.classes() {
				let id = class.id().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("published class has no identity")
				})?;
				self.record_reads([
					crate::datastore::ConflictKey::BaseClassRange(id),
					crate::datastore::ConflictKey::GraphClassRange(binding.graph, id),
				]);
			}
			self.record_read(crate::datastore::ConflictKey::FunctionCatalogRange);
			for &(function, _) in definition.dependencies() {
				self.record_read(crate::datastore::ConflictKey::SchemaFunction(function));
			}
		}
		let (Some(source), Some(target)) =
			(artifact.ordinal_by_atom.get(&source), artifact.ordinal_by_atom.get(&target))
		else {
			return Ok(None);
		};
		artifact.distances.lower_bound(*source, *target).map(Some)
	}

	/// Projects only logical sketch metadata from the captured snapshot.
	///
	/// # Errors
	/// Rejects a stale transaction, foreign graph or metadata allocation
	/// failure.
	pub(crate) fn query_neighbourhood_sketches(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Vec<QueryNeighbourhoodSketch>> {
		self.ensure_live()?;
		query_graph_root(&snapshot.root, reference)?;
		self.record_read(crate::datastore::ConflictKey::GraphDefinition(reference.id()));
		let schema =
			snapshot.root.schema.graph(reference.id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(reference.name().to_string())
			})?;
		let mut result = Vec::new();
		result.try_reserve_exact(schema.indexes().len()).map_err(|cause| {
			FerrosGrynnError::invalid_argument("neighbourhood metadata allocation failed")
				.with_technical_source(cause)
		})?;
		for definition in schema.indexes().filter(|index| {
			index.state() == crate::datastore::ClassIndexState::Ready
				&& index.edge_labels().len() == 0
		}) {
			let crate::datastore::GraphIndexDefinition::Neighbourhood {
				radius,
				algorithm: crate::datastore::NeighbourhoodAlgorithm::Sketch(precision),
			} = *definition.definition()
			else {
				continue;
			};
			let Some(id) = definition.id() else {
				continue;
			};
			let Some(artifact) = snapshot.root.neighbourhood_artifacts.get(&id) else {
				continue;
			};
			let header = artifact.layout.header;
			if header.has_cross_level
				|| header.identity.version != definition.version().get()
				|| header.identity.revision != definition.build_revision()
			{
				continue;
			}
			result.push(QueryNeighbourhoodSketch {
				graph: reference.id(),
				index: id,
				version: definition.version(),
				revision: definition.build_revision(),
				mode: definition.mode(),
				direction: definition.direction(),
				radius,
				precision,
				bytes: artifact.layout.page_count * 16_384,
			});
		}
		Ok(result)
	}

	/// Reads the planner-selected sketch or completes exact bounded local
	/// traversal.
	///
	/// # Errors
	/// Rejects source/graph/type mismatches, scratch/work overflow or
	/// cancellation.
	#[allow(
		clippy::too_many_arguments,
		reason = "snapshot, projection, selected binding and independent resource guards are \
		          explicit"
	)]
	pub(crate) fn query_k_hop_estimate(
		&self,
		snapshot: &QuerySnapshot,
		occurrence: &AtomOccurrenceRef,
		mode: crate::graph::TraversalSemantics,
		radius: u32,
		index: Option<IndexId>,
		scratch_bytes: usize,
		max_work: u64,
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<QueryNeighbourhoodEstimate> {
		use crate::{
			datastore::graph_neighbourhood::{
				NeighbourhoodBuildLimits,
				NeighbourhoodLabels,
				neighbourhood_members,
			},
			graph::{ProjectedRelation, TraversalDirection, TraversalSemantics},
		};
		self.ensure_live()?;
		check()?;
		let graph = query_graph_root(&snapshot.root, occurrence.graph())?;
		let global = snapshot.root.resolve_atom(occurrence.atom().id()).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("atom", occurrence.atom().id().to_string())
		})?;
		let source = graph.local_id(global).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", occurrence.atom().id().to_string())
		})?;
		if source.kind() != Some(occurrence.atom().kind())
			|| mode == TraversalSemantics::Node && !source.is_node()
		{
			return Err(FerrosGrynnError::invalid_argument(
				"kHopEstimate source is incompatible with its projection",
			));
		}
		self.record_reads([
			crate::datastore::ConflictKey::GraphDefinition(graph.reference().id()),
			crate::datastore::ConflictKey::GraphKindRange(graph.reference().id(), AtomKind::Node),
			crate::datastore::ConflictKey::GraphKindRange(graph.reference().id(), AtomKind::Edge),
		]);
		if radius == 0 {
			return Ok(QueryNeighbourhoodEstimate {
				count: 1,
				approximate: false,
				artifact_bytes: 0,
				work: 0,
			});
		}
		if let Some(id) = index {
			let schema = snapshot.root.schema.graph(graph.reference().id()).ok_or_else(|| {
				FerrosGrynnError::graph_does_not_exist(graph.reference().name().to_string())
			})?;
			let compatible = schema.indexes().find(|definition| definition.id() == Some(id)).is_some_and(|definition| definition.state() == crate::datastore::ClassIndexState::Ready && definition.mode() == mode && definition.direction() == TraversalDirection::Forward && definition.edge_labels().len() == 0 && matches!(definition.definition(), crate::datastore::GraphIndexDefinition::Neighbourhood { radius: maximum, algorithm: crate::datastore::NeighbourhoodAlgorithm::Sketch(_) } if radius <= *maximum));
			if compatible
				&& let Some(artifact) = snapshot.root.neighbourhood_artifacts.get(&id)
				&& !artifact.relation.has_cross_level
				&& let NeighbourhoodLabels::Sketch(sketches) = &artifact.labels
			{
				let ordinal = *artifact.relation.ordinal_by_atom.get(&source).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("neighbourhood sketch source is absent")
				})?;
				let row = (radius as usize - 1)
					.checked_mul(artifact.relation.vertices.len())
					.and_then(|row| row.checked_add(ordinal))
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("neighbourhood sketch row")
					})?;
				let sketch = sketches.get(row).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"neighbourhood sketch radius row is absent",
					)
				})?;
				return Ok(QueryNeighbourhoodEstimate {
					count: sketch.estimate(),
					approximate: true,
					artifact_bytes: artifact.layout.page_count * 16_384,
					work: 0,
				});
			}
		}
		admit(scratch_bytes)?;
		let resource = || {
			FerrosGrynnError::traversal_resource_limit_exceeded(
				"neighbourhood exact scratch bytes",
				scratch_bytes as u64,
			)
		};
		let vertices = graph
			.occurrences()
			.filter(|occurrence| {
				mode == TraversalSemantics::Atom || occurrence.local_id().is_node()
			})
			.count();
		let workspace = vertices
			.checked_mul(32)
			.and_then(|bytes| bytes.checked_add(1024))
			.ok_or_else(resource)?;
		let relation_budget = scratch_bytes.checked_sub(workspace).ok_or_else(resource)?;
		let mut work = 0_u64;
		let mut bounded = || {
			check()?;
			if work >= max_work {
				return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"neighbourhood exact work",
					max_work,
				));
			}
			work += 1;
			Ok(())
		};
		let relation = ProjectedRelation::build(
			graph.occurrences().map(GraphOccurrence::local_id),
			graph.occurrences().filter_map(|occurrence| match occurrence.state() {
				OccurrenceState::Edge {
					topology,
					..
				} => Some((occurrence.local_id(), topology)),
				OccurrenceState::Node {
					..
				} => None,
			}),
			mode,
			TraversalDirection::Forward,
			relation_budget,
			|atom| {
				graph.occurrence(atom).map(|occurrence| occurrence.global_id().raw()).ok_or_else(
					|| {
						FerrosGrynnError::invalid_storage_file(
							"neighbourhood exact vertex is absent",
						)
					},
				)
			},
			&mut bounded,
		)?;
		let ordinal = *relation.ordinal_by_atom.get(&source).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("neighbourhood exact source is absent")
		})?;
		let live = relation
			.vertices
			.len()
			.checked_mul(512)
			.and_then(|bytes| {
				relation
					.forward
					.targets
					.len()
					.checked_mul(128)
					.and_then(|arcs| bytes.checked_add(arcs))
			})
			.and_then(|bytes| bytes.checked_add(1024))
			.ok_or_else(resource)?;
		let members = neighbourhood_members(
			&relation,
			None,
			ordinal,
			radius,
			0,
			max_work,
			NeighbourhoodBuildLimits {
				max_bytes: u64::MAX,
				common_pages: 0,
				scratch_bytes,
				live_scratch_bytes: live,
			},
			&mut bounded,
		)?;
		Ok(QueryNeighbourhoodEstimate {
			count: members.len() as u64,
			approximate: false,
			artifact_bytes: 0,
			work,
		})
	}

	pub(crate) fn query_scratch_root(&self) -> std::path::PathBuf {
		self.store.inner.query_scratch_root()
	}

	pub(crate) fn query_property_dictionary(&self) -> Arc<PropertyDictionary> {
		self.working.dictionary.clone()
	}

	pub(crate) fn query_snapshot(&mut self) -> FerrosGrynnResult<QuerySnapshot> {
		self.prepare_read()?;
		let physical = if self.mutations.is_empty() {
			if self
				.physical_query_snapshot
				.as_ref()
				.is_some_and(|(revision, _)| *revision != self.working.commit_ts)
			{
				self.physical_query_snapshot = None;
			}
			if self.physical_query_snapshot.is_none() {
				self.physical_query_snapshot = Some((
					self.working.commit_ts,
					self.store.inner.capture_physical_query_snapshot(self.working.commit_ts),
				));
			}
			self.physical_query_snapshot
				.as_ref()
				.map(|(_, snapshot)| snapshot.clone())
				.transpose()?
				.flatten()
		} else {
			None
		};
		Ok(QuerySnapshot {
			root: Arc::new(self.working.clone()),
			physical,
		})
	}

	pub(crate) fn query_class(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<Arc<ClassSchema>> {
		self.ensure_live()?;
		snapshot.root.schema.class(name).ok_or_else(|| FerrosGrynnError::class_does_not_exist(name))
	}

	pub(crate) fn query_named_graph(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<GraphRef> {
		self.ensure_live()?;
		snapshot
			.root
			.schema
			.named_graph(name)
			.map(|schema| schema.reference().clone())
			.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(name))
	}

	pub(crate) fn query_named_graph_schema(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<Option<GraphSchema>> {
		self.ensure_live()?;
		let schema = snapshot.root.schema.named_graph(name);
		if let Some(schema) = &schema {
			self.record_read(crate::datastore::ConflictKey::GraphDefinition(schema.id()));
		}
		Ok(schema.map(|schema| schema.as_ref().clone()))
	}

	pub(crate) fn query_schema_graphs(
		&self,
		snapshot: &QuerySnapshot,
	) -> FerrosGrynnResult<Vec<GraphSchema>> {
		self.ensure_live()?;
		self.record_read(crate::datastore::ConflictKey::GraphCatalogRange);
		Ok(snapshot.root.schema.graphs().map(|graph| graph.as_ref().clone()).collect())
	}

	pub(crate) fn query_schema_all_graphs(
		&self,
		snapshot: &QuerySnapshot,
	) -> FerrosGrynnResult<Vec<GraphSchema>> {
		self.ensure_live()?;
		self.record_read(crate::datastore::ConflictKey::GraphCatalogRange);
		Ok(snapshot.root.schema.all_graphs().map(|graph| graph.as_ref().clone()).collect())
	}

	pub(crate) fn query_schema_classes(
		&self,
		snapshot: &QuerySnapshot,
	) -> FerrosGrynnResult<Vec<ClassSchema>> {
		self.ensure_live()?;
		Ok(snapshot.root.schema.classes().map(|class| class.as_ref().clone()).collect())
	}

	pub(crate) fn query_schema_functions(
		&self,
		snapshot: &QuerySnapshot,
		namespace: &str,
		name: &str,
	) -> FerrosGrynnResult<Vec<Arc<crate::datastore::SchemaFunction>>> {
		self.ensure_live()?;
		self.record_read(crate::datastore::ConflictKey::FunctionCatalogRange);
		Ok(snapshot
			.root
			.schema
			.functions()
			.filter(|function| {
				function.namespace().as_str() == namespace && function.name().as_str() == name
			})
			.cloned()
			.collect())
	}

	pub(crate) fn query_schema_function(
		&self,
		snapshot: &QuerySnapshot,
		id: crate::datastore::FunctionId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaFunction>>> {
		self.ensure_live()?;
		self.record_read(crate::datastore::ConflictKey::SchemaFunction(id));
		Ok(snapshot.root.schema.functions.get(&id).cloned())
	}

	pub(crate) fn query_schema_analyzer(
		&self,
		snapshot: &QuerySnapshot,
		name: &str,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>> {
		self.ensure_live()?;
		let analyzer = snapshot.root.schema.analyzer(name);
		match analyzer.as_ref().and_then(|analyzer| analyzer.id()) {
			Some(id) => self.record_read(crate::datastore::ConflictKey::SchemaAnalyzer(id)),
			None => self.record_read(crate::datastore::ConflictKey::AnalyzerCatalogRange),
		}
		Ok(analyzer)
	}

	pub(crate) fn query_schema_analyzer_by_id(
		&self,
		snapshot: &QuerySnapshot,
		id: crate::datastore::AnalyzerId,
	) -> FerrosGrynnResult<Option<Arc<crate::datastore::SchemaAnalyzer>>> {
		self.ensure_live()?;
		self.record_read(crate::datastore::ConflictKey::SchemaAnalyzer(id));
		Ok(snapshot.root.schema.analyzer_by_id(id))
	}

	/// Computes a LevelPath object from one immutable statement snapshot.
	pub(crate) fn query_level_path(
		&self,
		snapshot: &QuerySnapshot,
		occurrence: &AtomOccurrenceRef,
	) -> FerrosGrynnResult<Value> {
		self.ensure_live()?;
		let global = snapshot.root.resolve_atom(occurrence.atom().id()).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(
				occurrence.atom().kind().to_string(),
				occurrence.atom().id().to_string(),
			)
		})?;
		if global.kind() != Some(occurrence.atom().kind()) {
			return Err(FerrosGrynnError::atom_kind_mismatch(
				occurrence.atom().id().to_string(),
				global.kind().map_or_else(|| "null".to_owned(), |kind| kind.to_string()),
				occurrence.atom().kind().to_string(),
			));
		}
		let graph = occurrence.graph().id();
		self.record_read(crate::datastore::ConflictKey::GraphCatalogRange);
		self.record_read(crate::datastore::ConflictKey::BaseAtom(global));
		self.record_read(crate::datastore::ConflictKey::GraphMembership(graph, global));
		for schema in snapshot.root.schema.graphs() {
			self.record_read(crate::datastore::ConflictKey::GraphKindRange(
				schema.id(),
				AtomKind::Node,
			));
			self.record_read(crate::datastore::ConflictKey::GraphKindRange(
				schema.id(),
				AtomKind::Edge,
			));
		}
		snapshot.root.level_path_value(graph, global)
	}

	/// Returns planner statistics projected from the same immutable statement
	/// snapshot. Corrupt optional parts remain absent so planning can use its
	/// conservative fallback without weakening data reads.
	pub(crate) fn query_statistics<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
		demand: &QueryStatisticsDemand,
	) -> FerrosGrynnResult<std::borrow::Cow<'snapshot, crate::stats::StatisticsStore>> {
		self.ensure_live()?;
		let mut statistics = std::borrow::Cow::Borrowed(snapshot.root.statistics.as_ref());
		if statistics.auxiliary_deferred()
			&& let Some(physical) = snapshot.physical.as_ref()
		{
			self.store.inner.load_planner_statistics(
				physical,
				&snapshot.root,
				demand,
				statistics.to_mut(),
			);
		}
		Ok(statistics)
	}

	/// Resolves one transaction-visible physical binding without exposing its
	/// addresses or storage representation to the query layer.
	pub(crate) fn query_index_binding(
		&self,
		snapshot: &QuerySnapshot,
		class: &ClassSchema,
		index: &ClassIndex,
	) -> FerrosGrynnResult<(bool, Option<u64>)> {
		self.ensure_live()?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let index_id = index.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published index is missing IndexId")
		})?;
		if matches!(
			index.kind(),
			crate::datastore::ClassIndexKind::Fulltext
				| crate::datastore::ClassIndexKind::Hnsw
				| crate::datastore::ClassIndexKind::DiskAnn
				| crate::datastore::ClassIndexKind::Geometry
		) {
			if index.state() != crate::datastore::ClassIndexState::Ready {
				return Ok((false, None));
			}
			if index.kind() == crate::datastore::ClassIndexKind::Geometry
				&& self.mutations.is_empty()
				&& snapshot.root.resident_geometry.get(&index_id).is_some_and(|generation| {
					generation.header.spec.class == class_id
						&& generation.header.spec.index_version == index.version()
						&& generation.header.spec.scope == index.scope()
						&& generation.header.spec.build_revision == index.build_revision()
				}) {
				return Ok((true, None));
			}
			let available = snapshot
				.physical
				.as_ref()
				.is_some_and(|physical| self.store.inner.has_physical_index(physical, index_id));
			return Ok((available, None));
		}
		let binding = match index.scope() {
			crate::datastore::IndexScope::Base => planner_binding_summary(
				snapshot.root.indexes.base_index(index_id),
				class_id,
				index_id,
				index,
			),
			crate::datastore::IndexScope::Effective {
				graph,
			} => planner_binding_summary(
				snapshot.root.indexes.graph_index(graph, index_id),
				class_id,
				index_id,
				index,
			),
		}?;
		let deferred = match index.scope() {
			crate::datastore::IndexScope::Base => {
				snapshot.root.indexes.base_index(index_id).is_some_and(|binding| binding.deferred)
			}
			crate::datastore::IndexScope::Effective {
				graph,
			} => snapshot
				.root
				.indexes
				.graph_index(graph, index_id)
				.is_some_and(|binding| binding.deferred),
		};
		if !deferred {
			return Ok(binding);
		}
		let Some(physical) = snapshot.physical.as_ref() else {
			return Ok((false, None));
		};
		if !self.store.inner.has_physical_index(physical, index_id) {
			return Ok((false, None));
		}
		if index.kind() != crate::datastore::ClassIndexKind::Count {
			return Ok((true, None));
		}
		let physical = self
			.store
			.inner
			.physical_index(physical, index_id)?
			.ok_or_else(|| deferred_index_missing(index_id))?;
		validate_physical_index(&physical, class_id, index)?;
		match &physical.entries {
			PhysicalIndexEntries::Count(count) => Ok((true, Some(*count))),
			PhysicalIndexEntries::Scalar(_) => {
				Err(FerrosGrynnError::invalid_storage_file("Count index decoded as scalar"))
			}
			PhysicalIndexEntries::Fulltext(_) => {
				Err(FerrosGrynnError::invalid_storage_file("Count index decoded as Fulltext"))
			}
			PhysicalIndexEntries::Hnsw(_) => {
				Err(FerrosGrynnError::invalid_storage_file("Count index decoded as HNSW"))
			}
			PhysicalIndexEntries::Vector(_) => {
				Err(FerrosGrynnError::invalid_storage_file("Count index decoded as vector"))
			}
			PhysicalIndexEntries::Geometry(_) => {
				Err(FerrosGrynnError::invalid_storage_file("Count index decoded as geometry"))
			}
		}
	}

	/// Reads one immutable Fulltext binding into public record identities.
	///
	/// `None` means the captured snapshot has no compatible physical binding
	/// and the query executor must use its exact analyzer scan.
	pub(crate) fn query_fulltext_index(
		&self,
		snapshot: &QuerySnapshot,
		index: IndexId,
		class_id: crate::datastore::ClassId,
		scope: crate::datastore::IndexScope,
		terms: &BTreeSet<Box<str>>,
	) -> FerrosGrynnResult<Option<QueryFulltextIndexRead>> {
		self.ensure_live()?;
		let Some(physical_snapshot) = snapshot.physical.as_ref() else {
			return Ok(None);
		};
		if !self.store.inner.has_physical_index(physical_snapshot, index) {
			return Ok(None);
		}
		match scope {
			crate::datastore::IndexScope::Base => {
				self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			}
			crate::datastore::IndexScope::Effective {
				graph,
			} => {
				self.record_read(crate::datastore::ConflictKey::GraphClassRange(graph, class_id));
			}
		}
		let physical = self.query_physical_index(
			snapshot,
			index,
			class_id,
			crate::datastore::ClassIndexKind::Fulltext,
			scope,
		)?;
		let PhysicalIndexEntries::Fulltext(fulltext) = &physical.entries else {
			return Err(FerrosGrynnError::invalid_storage_file(
				"Fulltext index decoded as another index kind",
			));
		};
		let mut documents = fulltext
			.documents
			.iter()
			.map(|document| (document.atom, (document.document_length, BTreeMap::new())))
			.collect::<BTreeMap<_, _>>();
		for term in terms {
			let Some(entry) = fulltext
				.terms
				.binary_search_by(|candidate| candidate.term.as_slice().cmp(term.as_bytes()))
				.ok()
				.map(|position| &fulltext.terms[position])
			else {
				continue;
			};
			for posting in &entry.postings {
				let document = documents
					.entry(posting.atom)
					.or_insert_with(|| (posting.document_length, BTreeMap::new()));
				if document.0 != posting.document_length {
					return Err(FerrosGrynnError::invalid_storage_file(
						"Fulltext postings disagree on document length",
					));
				}
				document.1.insert(term.clone(), QueryFulltextTermRead {
					document_frequency: 0,
					term_frequency: posting.term_frequency,
					spans: posting.spans.clone(),
				});
			}
		}
		match physical.delta.as_ref() {
			None => {}
			Some(PhysicalIndexDelta::Fulltext(delta)) => {
				for record in &delta.records {
					match record {
						crate::storage::FulltextDeltaRecord::Delete {
							atom,
						} => {
							documents.remove(atom);
						}
						crate::storage::FulltextDeltaRecord::Upsert {
							atom,
							tokens,
						} => {
							let document_length =
								u32::try_from(tokens.len()).map_err(|_error| {
									FerrosGrynnError::invalid_storage_file(
										"Fulltext delta document length exceeds u32",
									)
								})?;
							let mut matches = BTreeMap::<Box<str>, QueryFulltextTermRead>::new();
							for token in
								tokens.iter().filter(|token| terms.contains(token.term.as_str()))
							{
								let matched = matches
									.entry(token.term.as_str().into())
									.or_insert_with(|| QueryFulltextTermRead {
										document_frequency: 0,
										term_frequency: 0,
										spans: Vec::new(),
									});
								matched.term_frequency =
									matched.term_frequency.checked_add(1).ok_or_else(|| {
										FerrosGrynnError::invalid_storage_file(
											"Fulltext delta term frequency exceeds u32",
										)
									})?;
								if fulltext.spec.options.highlights() {
									matched.spans.push((token.start, token.end));
								}
							}
							documents.insert(*atom, (document_length, matches));
						}
					}
				}
			}
			Some(_) => {
				return Err(FerrosGrynnError::invalid_storage_file(
					"Fulltext index has a vector delta",
				));
			}
		}
		let document_count = u64::try_from(documents.len()).map_err(|_error| {
			FerrosGrynnError::invalid_storage_file("Fulltext document count exceeds u64")
		})?;
		let total_document_length = documents.values().try_fold(0_u64, |total, document| {
			total.checked_add(u64::from(document.0)).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file("Fulltext total document length exceeds u64")
			})
		})?;
		let frequencies = terms
			.iter()
			.map(|term| {
				(
					term.clone(),
					u64::try_from(
						documents.values().filter(|document| document.1.contains_key(term)).count(),
					)
					.unwrap_or(u64::MAX),
				)
			})
			.collect::<BTreeMap<_, _>>();
		for document in documents.values_mut() {
			for (term, matched) in &mut document.1 {
				matched.document_frequency = frequencies.get(term).copied().unwrap_or(0);
			}
		}
		let graph = match scope {
			crate::datastore::IndexScope::Base => None,
			crate::datastore::IndexScope::Effective {
				graph,
			} => Some(
				snapshot
					.root
					.graph(graph)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?,
			),
		};
		let mut output = Vec::with_capacity(documents.len());
		for (address, (document_length, terms)) in
			documents.into_iter().filter(|(_address, (_length, terms))| !terms.is_empty())
		{
			let address = PhysicalIndexAtom {
				kind: address.kind(),
				slot: address.slot(),
			};
			let global = if let Some(graph) = graph {
				let local = physical_local_id(graph, address)?;
				graph
					.occurrence(local)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"Fulltext posting references an absent occurrence",
						)
					})?
					.global_id()
			} else {
				physical_global_id(&snapshot.root, address)?
			};
			let atom = snapshot.root.atom(global).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"Fulltext posting references an absent base atom",
				)
			})?;
			if atom.class_id() != class_id {
				return Err(FerrosGrynnError::invalid_storage_file(
					"Fulltext posting references an atom of another class",
				));
			}
			let record_id = atom.record_id().clone();
			output.push(QueryFulltextDocumentRead {
				record_id,
				document_length,
				terms,
			});
		}
		output.sort_unstable_by(|left, right| left.record_id.cmp(&right.record_id));
		Ok(Some(QueryFulltextIndexRead {
			document_count,
			total_document_length,
			documents: output,
		}))
	}

	/// Searches a snapshot-bound geometry generation and maps candidates to
	/// public identities.
	pub(crate) fn query_geometry_index(
		&self,
		snapshot: &QuerySnapshot,
		index: IndexId,
		class_id: crate::datastore::ClassId,
		scope: crate::datastore::IndexScope,
		geometry: &crate::values::Geometry,
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Option<QueryGeometryIndexRead>> {
		self.ensure_live()?;
		if !self.mutations.is_empty() {
			return Ok(None);
		}
		match scope {
			crate::datastore::IndexScope::Base => {
				self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id))
			}
			crate::datastore::IndexScope::Effective {
				graph,
			} => self.record_read(crate::datastore::ConflictKey::GraphClassRange(graph, class_id)),
		}
		let Some(mbr) = geometry.mbr()? else {
			return Ok(Some(QueryGeometryIndexRead::default()));
		};
		let candidates = if let Some(generation) = snapshot.root.resident_geometry.get(&index) {
			if generation.header.spec.class != class_id || generation.header.spec.scope != scope {
				return Err(FerrosGrynnError::invalid_storage_file(
					"resident geometry binding is incompatible",
				));
			}
			generation.search(mbr, admit)?
		} else {
			let Some(physical_snapshot) = &snapshot.physical else {
				return Ok(None);
			};
			if !self.store.inner.has_physical_index(physical_snapshot, index) {
				return Ok(None);
			}
			// Admit the bounded cold header and delta decode before physical
			// metadata reads.
			admit(512 * 1024)?;
			self.query_physical_index(
				snapshot,
				index,
				class_id,
				crate::datastore::ClassIndexKind::Geometry,
				scope,
			)?;
			let Some(candidates) =
				self.store.inner.search_geometry_index(physical_snapshot, index, mbr, admit)?
			else {
				return Ok(None);
			};
			candidates
		};
		let graph = match scope {
			crate::datastore::IndexScope::Base => None,
			crate::datastore::IndexScope::Effective {
				graph,
			} => Some(
				snapshot
					.root
					.graph(graph)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?,
			),
		};
		admit(candidates.atoms.len().checked_mul(std::mem::size_of::<RecordId>()).ok_or_else(
			|| FerrosGrynnError::invalid_storage_file("geometry candidate allocation overflows"),
		)?)?;
		let mut output = Vec::with_capacity(candidates.atoms.len());
		for address in candidates.atoms {
			let address = PhysicalIndexAtom {
				kind: address.kind(),
				slot: address.slot(),
			};
			let global = if let Some(graph) = graph {
				graph
					.occurrence(physical_local_id(graph, address)?)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"geometry candidate references an absent occurrence",
						)
					})?
					.global_id()
			} else {
				physical_global_id(&snapshot.root, address)?
			};
			let atom =
				snapshot.root.atom(global).filter(|atom| atom.class_id() == class_id).ok_or_else(
					|| {
						FerrosGrynnError::invalid_storage_file(
							"geometry candidate references an absent or incompatible atom",
						)
					},
				)?;
			admit(crate::values::runtime::estimated_record_id_heap_bytes(atom.record_id()))?;
			output.push(atom.record_id().clone());
		}
		output.sort_unstable();
		Ok(Some(QueryGeometryIndexRead {
			records: output,
			page_reads: candidates.page_reads,
			delta_records: candidates.delta_records,
		}))
	}

	/// Searches one compatible immutable vector generation.
	pub(crate) fn query_vector_index(
		&self,
		snapshot: &QuerySnapshot,
		index: IndexId,
		class_id: crate::datastore::ClassId,
		scope: crate::datastore::IndexScope,
		query: &Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<QueryVectorIndexRead>>> {
		self.ensure_live()?;
		let Some(physical_snapshot) = snapshot.physical.as_ref() else {
			return Ok(None);
		};
		if !self.store.inner.has_physical_index(physical_snapshot, index) {
			return Ok(None);
		}
		match scope {
			crate::datastore::IndexScope::Base => {
				self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			}
			crate::datastore::IndexScope::Effective {
				graph,
			} => {
				self.record_read(crate::datastore::ConflictKey::GraphClassRange(graph, class_id));
			}
		}
		let class = snapshot.root.schema.class_by_id(class_id).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"vector physical binding references an absent class",
			)
		})?;
		let definition =
			class.indexes().find(|candidate| candidate.id() == Some(index)).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"vector physical binding has no schema definition",
				)
			})?;
		if !matches!(
			definition.kind(),
			crate::datastore::ClassIndexKind::Hnsw | crate::datastore::ClassIndexKind::DiskAnn
		) {
			return Err(FerrosGrynnError::invalid_storage_file(
				"vector physical binding points to a non-vector definition",
			));
		}
		let graph = match scope {
			crate::datastore::IndexScope::Base => None,
			crate::datastore::IndexScope::Effective {
				graph,
			} => Some(
				snapshot
					.root
					.graph(graph)
					.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?,
			),
		};
		let hits = if definition.kind() == crate::datastore::ClassIndexKind::DiskAnn {
			self.store
				.inner
				.search_diskann_index(physical_snapshot, index, query, k, breadth)?
				.ok_or_else(|| deferred_index_missing(index))?
		} else {
			let evaluator = crate::gql::PureFunctionEvaluator::new(&snapshot.root);
			let mut distance = |function, version, left: &Vector, right: &Vector| {
				evaluator.distance(function, version, left, right)
			};
			self.store
				.inner
				.search_hnsw_index(physical_snapshot, index, query, k, breadth, &mut distance)?
				.ok_or_else(|| deferred_index_missing(index))?
		};
		let mut output = Vec::new();
		for hit in hits {
			let address = PhysicalIndexAtom {
				kind: hit.atom.kind(),
				slot: hit.atom.slot(),
			};
			let global = if let Some(graph) = graph {
				let local = physical_local_id(graph, address)?;
				graph
					.occurrence(local)
					.ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"vector posting references an absent occurrence",
						)
					})?
					.global_id()
			} else {
				physical_global_id(&snapshot.root, address)?
			};
			let atom = snapshot.root.atom(global).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"vector posting references an absent base atom",
				)
			})?;
			if atom.class_id() != class_id {
				return Err(FerrosGrynnError::invalid_storage_file(
					"vector posting references an atom of another class",
				));
			}
			output.push(QueryVectorIndexRead {
				record_id: atom.record_id().clone(),
				distance: hit.distance,
			});
		}
		output.sort_unstable_by(|left, right| {
			left.distance
				.total_cmp(&right.distance)
				.then_with(|| left.record_id.cmp(&right.record_id))
		});
		Ok(Some(output))
	}

	pub(crate) fn query_row_cursor<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
		class: &str,
		constraints: &[ScalarIndexConstraint],
		graph: Option<&GraphRef>,
		index: Option<(IndexId, bool)>,
		window: Option<&ScalarIndexWindow>,
	) -> FerrosGrynnResult<QueryStorageCursor<'snapshot>> {
		let class = self.query_class(snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let source = if let Some(reference) = graph {
			let graph_root = query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				reference.id(),
				class_id,
			));
			if let Some((index, reverse)) = index {
				let binding =
					snapshot.root.indexes.graph_index(reference.id(), index).ok_or_else(|| {
						FerrosGrynnError::invalid_storage_file(
							"planned effective index has no transaction-visible binding",
						)
					})?;
				let ids = if binding.deferred {
					let physical = self.query_physical_index(
						snapshot,
						index,
						class_id,
						binding.kind,
						crate::datastore::IndexScope::Effective {
							graph: reference.id(),
						},
					)?;
					query_index_addresses(
						&snapshot.root,
						&class,
						binding,
						constraints,
						reverse,
						window,
						|lower, upper, reverse, maximum| {
							read_physical_index_groups(
								&physical,
								lower,
								upper,
								reverse,
								maximum,
								|atom| physical_local_id(graph_root, atom),
							)
						},
						|local| graph_record_id(&snapshot.root, graph_root, local),
					)?
				} else {
					query_index_addresses(
						&snapshot.root,
						&class,
						binding,
						constraints,
						reverse,
						window,
						|lower, upper, reverse, maximum| {
							Ok(read_memory_index_groups(
								&binding.entries,
								lower,
								upper,
								reverse,
								maximum,
							))
						},
						|local| graph_record_id(&snapshot.root, graph_root, local),
					)?
				};
				QueryCursorSource::GraphOwned {
					graph: reference.id(),
					ids: ids.into_iter(),
				}
			} else {
				snapshot
					.root
					.indexes
					.graphs
					.get(&reference.id())
					.and_then(|indexes| indexes.classes.get(&class_id))
					.map(|ids| QueryCursorSource::Graph {
						graph: reference.id(),
						ids: ids.iter(),
					})
					.unwrap_or(QueryCursorSource::Empty)
			}
		} else {
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			if let Some((index, reverse)) = index {
				let binding = snapshot.root.indexes.base_index(index).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"planned base index has no transaction-visible binding",
					)
				})?;
				let ids = if binding.deferred {
					let physical = self.query_physical_index(
						snapshot,
						index,
						class_id,
						binding.kind,
						crate::datastore::IndexScope::Base,
					)?;
					query_index_addresses(
						&snapshot.root,
						&class,
						binding,
						constraints,
						reverse,
						window,
						|lower, upper, reverse, maximum| {
							read_physical_index_groups(
								&physical,
								lower,
								upper,
								reverse,
								maximum,
								|atom| physical_global_id(&snapshot.root, atom),
							)
						},
						|global| base_record_id(&snapshot.root, global),
					)?
				} else {
					query_index_addresses(
						&snapshot.root,
						&class,
						binding,
						constraints,
						reverse,
						window,
						|lower, upper, reverse, maximum| {
							Ok(read_memory_index_groups(
								&binding.entries,
								lower,
								upper,
								reverse,
								maximum,
							))
						},
						|global| base_record_id(&snapshot.root, global),
					)?
				};
				QueryCursorSource::BaseOwned(ids.into_iter())
			} else {
				snapshot
					.root
					.indexes
					.classes
					.get(&class_id)
					.map(|ids| QueryCursorSource::Base(ids.iter()))
					.unwrap_or(QueryCursorSource::Empty)
			}
		};
		Ok(QueryStorageCursor {
			root: &snapshot.root,
			class,
			source,
		})
	}

	/// Opens one exact scalar-index probe while preserving scan-equivalent
	/// predicate tracking for SSI.
	pub(crate) fn query_index_probe_cursor<'snapshot>(
		&self,
		snapshot: &'snapshot QuerySnapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: IndexId,
		values: &[Value],
	) -> FerrosGrynnResult<QueryStorageCursor<'snapshot>> {
		let class = self.query_class(snapshot, class)?;
		let class_id = class.id().ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
		})?;
		let mut context =
			crate::datastore::index_context(&snapshot.root.schema, &snapshot.root.dictionary)?;
		let source = if let Some(reference) = graph {
			query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				reference.id(),
				class_id,
			));
			let binding =
				snapshot.root.indexes.graph_index(reference.id(), index).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"planned effective index has no transaction-visible binding",
					)
				})?;
			let key = exact_index_probe_key(binding, values, &mut context)?;
			let ids = if binding.deferred {
				let physical = self.query_physical_index(
					snapshot,
					index,
					class_id,
					binding.kind,
					crate::datastore::IndexScope::Effective {
						graph: reference.id(),
					},
				)?;
				read_physical_index_groups(
					&physical,
					Bound::Included(key.clone()),
					Bound::Included(key.clone()),
					false,
					None,
					|atom| {
						let graph = query_graph_root(&snapshot.root, reference)?;
						physical_local_id(graph, atom)
					},
				)?
				.into_iter()
				.flat_map(|(_key, ids)| ids)
				.collect()
			} else {
				binding.entries.lookup(&key)
			};
			QueryCursorSource::GraphOwned {
				graph: reference.id(),
				ids: ids.into_iter(),
			}
		} else {
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(class_id));
			let binding = snapshot.root.indexes.base_index(index).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned base index has no transaction-visible binding",
				)
			})?;
			let key = exact_index_probe_key(binding, values, &mut context)?;
			let ids = if binding.deferred {
				let physical = self.query_physical_index(
					snapshot,
					index,
					class_id,
					binding.kind,
					crate::datastore::IndexScope::Base,
				)?;
				read_physical_index_groups(
					&physical,
					Bound::Included(key.clone()),
					Bound::Included(key.clone()),
					false,
					None,
					|atom| physical_global_id(&snapshot.root, atom),
				)?
				.into_iter()
				.flat_map(|(_key, ids)| ids)
				.collect()
			} else {
				binding.entries.lookup(&key)
			};
			QueryCursorSource::BaseOwned(ids.into_iter())
		};
		Ok(QueryStorageCursor {
			root: &snapshot.root,
			class,
			source,
		})
	}

	pub(crate) fn query_count_index(
		&self,
		snapshot: &QuerySnapshot,
		index: IndexId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64> {
		self.ensure_live()?;
		if let Some(graph) = graph {
			query_graph_root(&snapshot.root, graph)?;
			let binding =
				snapshot.root.indexes.graph_index(graph.id(), index).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"planned effective Count index has no transaction-visible binding",
					)
				})?;
			self.record_read(crate::datastore::ConflictKey::GraphClassRange(
				graph.id(),
				binding.class_id,
			));
			if binding.deferred {
				let physical = self.query_physical_index(
					snapshot,
					index,
					binding.class_id,
					binding.kind,
					crate::datastore::IndexScope::Effective {
						graph: graph.id(),
					},
				)?;
				return physical_count(&physical);
			}
			return binding.entries.count().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned Count lookup selected a scalar index",
				)
			});
		} else {
			let binding = snapshot.root.indexes.base_index(index).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned base Count index has no transaction-visible binding",
				)
			})?;
			self.record_read(crate::datastore::ConflictKey::BaseClassRange(binding.class_id));
			if binding.deferred {
				let physical = self.query_physical_index(
					snapshot,
					index,
					binding.class_id,
					binding.kind,
					crate::datastore::IndexScope::Base,
				)?;
				return physical_count(&physical);
			}
			binding.entries.count().ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"planned Count lookup selected a scalar index",
				)
			})
		}
	}

	fn query_physical_index(
		&self,
		snapshot: &QuerySnapshot,
		index: IndexId,
		class_id: crate::datastore::ClassId,
		kind: crate::datastore::ClassIndexKind,
		scope: crate::datastore::IndexScope,
	) -> FerrosGrynnResult<Arc<PhysicalIndexRead>> {
		let physical_snapshot =
			snapshot.physical.as_ref().ok_or_else(|| deferred_index_missing(index))?;
		let physical = self
			.store
			.inner
			.physical_index(physical_snapshot, index)?
			.ok_or_else(|| deferred_index_missing(index))?;
		if physical.class_id != class_id || physical.kind != kind || physical.scope != scope {
			return Err(FerrosGrynnError::invalid_storage_file(
				"durable index metadata disagrees with its transaction binding",
			));
		}
		Ok(physical)
	}

	/// Selects graph-local MATCH seeds through exact, class, and label indexes.
	///
	/// Every returned atom satisfies the requested kind, selector, membership,
	/// and label. Meta-position checks remain in the MATCH executor because no
	/// transaction index represents that predicate.
	pub(crate) fn query_match_candidates(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
		kind: AtomKind,
		class: Option<&str>,
		record: Option<&RecordId>,
		label: Option<&str>,
	) -> FerrosGrynnResult<Vec<AtomId>> {
		self.ensure_live()?;
		let graph = query_graph_root(&snapshot.root, reference)?;
		self.record_read(crate::datastore::ConflictKey::GraphKindRange(
			reference.id(),
			AtomKind::Node,
		));
		self.record_read(crate::datastore::ConflictKey::GraphKindRange(
			reference.id(),
			AtomKind::Edge,
		));
		if let Some(record) = record {
			self.track_query_point(snapshot, record, Some(reference))?;
			let Some(atom) = snapshot.root.atom_by_record(record) else {
				return Ok(Vec::new());
			};
			if atom.kind() != kind {
				return Ok(Vec::new());
			}
			let Some(local) = graph.local_id(atom.id()) else {
				return Ok(Vec::new());
			};
			if let Some(expected) = label {
				let occurrence = graph.occurrence(local).ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file(
						"graph membership references an absent occurrence",
					)
				})?;
				if occurrence.label().map(Text::as_str) != Some(expected) {
					return Ok(Vec::new());
				}
			}
			return Ok(vec![local]);
		}

		let class_ids = class
			.map(|class| {
				let schema = self.query_class(snapshot, class)?;
				if schema.kind() != kind {
					return Err(FerrosGrynnError::invalid_argument(
						"MATCH selector class kind disagrees with its pattern delimiter.",
					));
				}
				let class_id = schema.id().ok_or_else(|| {
					FerrosGrynnError::invalid_storage_file("published class is missing ClassId")
				})?;
				Ok(snapshot.root.indexes.graph_class(reference.id(), class_id))
			})
			.transpose()?;
		let label_ids = label.map(|label| {
			snapshot.root.indexes.graph_label(reference.id(), kind, &Text::from(label))
		});
		Ok(match (class_ids, label_ids) {
			(Some(classes), Some(labels)) => intersect_sorted(classes, labels),
			(Some(classes), None) => classes,
			(None, Some(labels)) => labels,
			(None, None) => graph
				.occurrences()
				.filter(|occurrence| occurrence.local_id().kind() == Some(kind))
				.map(GraphOccurrence::local_id)
				.collect(),
		})
	}

	/// Attempts one snapshot-exact local hot-page expansion. `None` selects the
	/// ordinary materialized graph path without changing query semantics.
	#[allow(
		clippy::too_many_arguments,
		reason = "the physical frontier keeps all requested direction axes explicit"
	)]
	pub(crate) fn query_physical_traversal(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
		source: AtomId,
		forward: bool,
		backward: bool,
		undirected: bool,
	) -> FerrosGrynnResult<Option<QueryPhysicalTraversalRead>> {
		self.ensure_live()?;
		let graph = query_graph_root(&snapshot.root, reference)?;
		if graph.occurrence(source).is_none() {
			return Err(FerrosGrynnError::atom_does_not_exist("atom", source.to_string()));
		}
		let Some(physical) = &snapshot.physical else {
			return Ok(None);
		};
		let Some(read) = self.store.inner.physical_traversal_expand(
			physical,
			reference.id(),
			source,
			forward,
			backward,
			undirected,
		)?
		else {
			return Ok(None);
		};
		self.record_reads([
			crate::datastore::ConflictKey::GraphKindRange(reference.id(), AtomKind::Node),
			crate::datastore::ConflictKey::GraphKindRange(reference.id(), AtomKind::Edge),
			crate::datastore::ConflictKey::GraphIncidence(reference.id(), source),
		]);
		let candidates = read
			.incidences
			.into_iter()
			.map(|candidate| {
				if graph.occurrence(candidate.edge).is_none()
					|| graph.occurrence(candidate.target).is_none()
				{
					return Err(FerrosGrynnError::invalid_storage_file(
						"physical traversal hot pages disagree with the captured MVCC root",
					));
				}
				self.record_reads([
					crate::datastore::ConflictKey::GraphIncidence(reference.id(), candidate.edge),
					crate::datastore::ConflictKey::GraphIncidence(reference.id(), candidate.target),
				]);
				Ok(QueryPhysicalTraversalCandidate {
					target: candidate.target,
					edge: candidate.edge,
					direction: candidate.direction,
					role: candidate.role,
					topology: candidate.topology,
				})
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Some(QueryPhysicalTraversalRead {
			candidates,
			stats: QueryPhysicalTraversalStats {
				resident_pages: read.stats.resident_pages,
				wal_pages: read.stats.wal_pages,
				sync_home_pages: read.stats.sync_home_pages,
				ioring_home_pages: read.stats.ioring_home_pages,
			},
		}))
	}

	/// Attempts one snapshot-exact node inner-graph or edge transition-graph
	/// lookup. The physical value is checked against the immutable MVCC root
	/// before it crosses the datastore boundary.
	pub(crate) fn query_physical_inner_graph(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<QueryPhysicalInnerGraphRead>> {
		self.ensure_live()?;
		let graph = query_graph_root(&snapshot.root, reference)?;
		let occurrence = graph
			.occurrence(owner)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", owner.to_string()))?;
		let expected = match occurrence.state() {
			OccurrenceState::Node {
				nested_graph,
			} => *nested_graph,
			OccurrenceState::Edge {
				transition_graph,
				..
			} => *transition_graph,
		};
		let Some(physical) = &snapshot.physical else {
			return Ok(None);
		};
		let Some(read) =
			self.store.inner.physical_traversal_inner_graph(physical, reference.id(), owner)?
		else {
			return Ok(None);
		};
		if read.graph != expected {
			return Err(FerrosGrynnError::invalid_storage_file(
				"physical nested-graph binding disagrees with the captured MVCC root",
			));
		}
		self.record_reads([
			crate::datastore::ConflictKey::BaseAtom(occurrence.global_id()),
			crate::datastore::ConflictKey::GraphMembership(reference.id(), occurrence.global_id()),
			crate::datastore::ConflictKey::GraphOverlay(reference.id(), occurrence.global_id()),
			crate::datastore::ConflictKey::GraphIncidence(reference.id(), owner),
		]);
		Ok(Some(QueryPhysicalInnerGraphRead {
			graph: read.graph,
			stats: QueryPhysicalTraversalStats {
				resident_pages: read.stats.resident_pages,
				wal_pages: read.stats.wal_pages,
				sync_home_pages: read.stats.sync_home_pages,
				ioring_home_pages: read.stats.ioring_home_pages,
			},
		}))
	}

	/// Attempts one snapshot-exact base/effective property read. Physical
	/// identities are checked against the immutable MVCC root before owned
	/// values cross the datastore boundary.
	pub(crate) fn query_physical_fields(
		&self,
		snapshot: &QuerySnapshot,
		reference: &GraphRef,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<QueryPhysicalFieldsRead>> {
		self.ensure_live()?;
		let graph = query_graph_root(&snapshot.root, reference)?;
		let occurrence = graph
			.occurrence(owner)
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", owner.to_string()))?;
		let base = snapshot.root.atom(occurrence.global_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file(
				"graph occurrence references an absent base atom",
			)
		})?;
		let Some(physical) = &snapshot.physical else {
			return Ok(None);
		};
		let context = index_context(&snapshot.root.schema, &snapshot.root.dictionary)?;
		let Some(read) = self.store.inner.physical_traversal_fields(
			physical,
			reference.id(),
			owner,
			&context.decoder(),
		)?
		else {
			return Ok(None);
		};
		if read.global != occurrence.global_id()
			|| read.class_id != base.class_id()
			|| &read.record_id != base.record_id()
			|| &read.base_properties != base.properties()
			|| read.overlay.as_ref() != occurrence.overlay()
		{
			return Err(FerrosGrynnError::invalid_storage_file(
				"physical occurrence fields disagree with the captured MVCC root",
			));
		}
		let mut properties = read.base_properties;
		if let Some(overlay) = &read.overlay {
			for (key, value) in overlay.iter() {
				match value {
					OverlayValue::Value(value) => {
						properties.insert(key, value.clone());
					}
					OverlayValue::Hidden => {
						properties.remove(key);
					}
				}
			}
		}
		self.record_reads([
			crate::datastore::ConflictKey::BaseAtom(occurrence.global_id()),
			crate::datastore::ConflictKey::GraphMembership(reference.id(), occurrence.global_id()),
			crate::datastore::ConflictKey::GraphOverlay(reference.id(), occurrence.global_id()),
		]);
		Ok(Some(QueryPhysicalFieldsRead {
			record_id: read.record_id,
			properties,
			stats: QueryPhysicalTraversalStats {
				resident_pages: read.stats.resident_pages,
				wal_pages: read.stats.wal_pages,
				sync_home_pages: read.stats.sync_home_pages,
				ioring_home_pages: read.stats.ioring_home_pages,
			},
		}))
	}

	pub(crate) fn query_point(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<QueryStorageRow>> {
		self.ensure_live()?;
		self.track_query_point(snapshot, record, graph)?;
		let Some(atom) = snapshot.root.atom_by_record(record) else {
			return Ok(None);
		};
		let class = snapshot.root.schema.class_by_id(atom.class_id()).ok_or_else(|| {
			FerrosGrynnError::invalid_storage_file("global atom references an absent class")
		})?;
		if let Some(reference) = graph {
			let graph = query_graph_root(&snapshot.root, reference)?;
			let Some(local) = graph.local_id(atom.id()) else {
				return Ok(None);
			};
			let occurrence = graph.occurrence(local).ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"graph membership references an absent occurrence",
				)
			})?;
			return row_from_occurrence(&snapshot.root, occurrence, &class).map(Some);
		}
		Ok(Some(row_from_atom(atom, &class)))
	}

	/// Resolves one record field under the normative base/effective reference
	/// rules.
	pub(crate) fn query_record_field(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
	) -> FerrosGrynnResult<Value> {
		let row = match graph {
			Some(graph) => match self.query_point(snapshot, record, Some(graph))? {
				Some(row) => Some(row),
				None => self.query_point(snapshot, record, None)?,
			},
			None => self.query_point(snapshot, record, None)?,
		}
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("record", record.to_string()))?;

		match field {
			"id" => Ok(Value::Record(Box::new(row.record_id))),
			"label" => Ok(row.label.map(Value::String).unwrap_or(Value::None)),
			_ => {
				let Some(key) = snapshot.root.dictionary.get(field) else {
					return Ok(Value::None);
				};
				Ok(row.properties.get(key).cloned().unwrap_or(Value::None))
			}
		}
	}

	fn track_query_point(
		&self,
		snapshot: &QuerySnapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<()> {
		let Some(global) = snapshot.root.resolve_atom(record) else {
			self.record_missing(record);
			return Ok(());
		};
		self.record_read(crate::datastore::ConflictKey::BaseAtom(global));
		if let Some(reference) = graph {
			let graph = query_graph_root(&snapshot.root, reference)?;
			self.record_read(crate::datastore::ConflictKey::GraphMembership(
				reference.id(),
				global,
			));
			self.record_read(crate::datastore::ConflictKey::GraphOverlay(reference.id(), global));
			if let Some(local) = graph.local_id(global) {
				self.record_read(crate::datastore::ConflictKey::GraphIncidence(
					reference.id(),
					local,
				));
			}
		}
		Ok(())
	}
}

fn exact_index_probe_key<Address: Copy + Ord>(
	binding: &crate::datastore::IndexBinding<Address>,
	values: &[Value],
	context: &mut crate::values::storage::EncodeContext,
) -> FerrosGrynnResult<Vec<u8>> {
	if values.len() != binding.fields.len() {
		return Err(FerrosGrynnError::invalid_storage_file(
			"planned index probe does not cover the complete composite key",
		));
	}
	let components = values
		.iter()
		.zip(&binding.fields)
		.map(|(value, field)| (value, &field.value_type))
		.collect::<Vec<_>>();
	crate::values::storage::encode_index_key(&components, context)
}

fn query_index_addresses<Address: Copy + Ord>(
	root: &DatabaseRoot,
	class: &ClassSchema,
	binding: &crate::datastore::IndexBinding<Address>,
	constraints: &[ScalarIndexConstraint],
	reverse: bool,
	window: Option<&ScalarIndexWindow>,
	mut read_groups: impl FnMut(
		Bound<Vec<u8>>,
		Bound<Vec<u8>>,
		bool,
		Option<usize>,
	) -> FerrosGrynnResult<Vec<(Vec<u8>, Vec<Address>)>>,
	mut record_id: impl FnMut(Address) -> FerrosGrynnResult<RecordId>,
) -> FerrosGrynnResult<Vec<Address>> {
	if binding.kind == crate::datastore::ClassIndexKind::Count {
		return Err(FerrosGrynnError::invalid_storage_file("Count index reached the row cursor"));
	}
	let mut columns = Vec::with_capacity(binding.fields.len());
	for field in &binding.fields {
		let position = class
			.fields()
			.position(|candidate| candidate.key == Some(field.key))
			.ok_or_else(|| {
				FerrosGrynnError::invalid_storage_file(
					"scalar index field is absent from its owning class",
				)
			})?;
		columns.push(position.saturating_add(5));
	}
	let mut context = crate::datastore::index_context(&root.schema, &root.dictionary)?;
	let after = window
		.and_then(|window| window.after.as_ref())
		.map(|(values, record)| -> FerrosGrynnResult<_> {
			let key = exact_index_probe_key(binding, values, &mut context)?;
			Ok((key, record))
		})
		.transpose()?;
	let maximum = window.map(|window| window.maximum);
	let mut prefixes = vec![Vec::<Value>::new()];
	let mut equality_fields = 0;
	for column in &columns {
		let Some(constraint) = constraints.iter().find(|constraint| {
			constraint.column == *column && matches!(constraint.comparison, ScalarComparison::Equal)
		}) else {
			break;
		};
		let mut next = Vec::new();
		for prefix in &prefixes {
			for value in &constraint.values {
				let mut key = prefix.clone();
				key.push(value.clone());
				next.push(key);
			}
		}
		prefixes = next;
		equality_fields += 1;
	}
	if prefixes.is_empty() {
		return Ok(Vec::new());
	}
	let mut groups = std::collections::BTreeMap::<Vec<u8>, BTreeSet<Address>>::new();
	if equality_fields == binding.fields.len() {
		for key in prefixes {
			let components = key
				.iter()
				.zip(binding.fields.iter())
				.map(|(value, field)| (value, &field.value_type))
				.collect::<Vec<_>>();
			let key = crate::values::storage::encode_index_key(&components, &mut context)?;
			let after_key = after.as_ref().map(|(key, _record)| key.as_slice());
			if !key_is_after(&key, reverse, after_key) {
				continue;
			}
			let group_maximum = if after_key == Some(key.as_slice()) {
				None
			} else {
				maximum
			};
			for (key, addresses) in read_groups(
				Bound::Included(key.clone()),
				Bound::Included(key),
				reverse,
				group_maximum,
			)? {
				groups.entry(key).or_default().extend(addresses);
			}
		}
		return ordered_index_addresses(
			groups,
			reverse,
			after.as_ref().map(|(key, record)| (key.as_slice(), *record)),
			maximum,
			&mut record_id,
		);
	}
	let range_column = columns[equality_fields];
	let ranges = constraints
		.iter()
		.filter(|constraint| {
			constraint.column == range_column
				&& !matches!(constraint.comparison, ScalarComparison::Equal)
		})
		.collect::<Vec<_>>();
	if equality_fields == 0 && ranges.is_empty() {
		let after_key = after.as_ref().map(|(key, _record)| key);
		if let Some(after_key) = after_key {
			for (key, addresses) in read_groups(
				Bound::Included(after_key.clone()),
				Bound::Included(after_key.clone()),
				reverse,
				None,
			)? {
				groups.entry(key).or_default().extend(addresses);
			}
		}
		let lower = match (reverse, after_key) {
			(false, Some(key)) => Bound::Excluded(key.clone()),
			_ => Bound::Unbounded,
		};
		let upper = match (reverse, after_key) {
			(true, Some(key)) => Bound::Excluded(key.clone()),
			_ => Bound::Unbounded,
		};
		for (key, addresses) in read_groups(lower, upper, reverse, maximum)? {
			groups.entry(key).or_default().extend(addresses);
		}
		return ordered_index_addresses(
			groups,
			reverse,
			after.as_ref().map(|(key, record)| (key.as_slice(), *record)),
			maximum,
			&mut record_id,
		);
	}
	for prefix in prefixes {
		let components = prefix
			.iter()
			.zip(binding.fields[..equality_fields].iter())
			.map(|(value, field)| (value, &field.value_type))
			.collect::<Vec<_>>();
		let encoded_prefix =
			crate::values::storage::encode_index_prefix(&components, &mut context)?;
		let mut lower = if encoded_prefix.is_empty() {
			Bound::Unbounded
		} else {
			Bound::Included(encoded_prefix.clone())
		};
		let mut upper = next_prefix(&encoded_prefix).map_or(Bound::Unbounded, Bound::Excluded);
		for constraint in &ranges {
			let Some(value) = constraint.values.first() else {
				continue;
			};
			let field = &binding.fields[equality_fields];
			let component = [(value, &field.value_type)];
			let encoded = if equality_fields + 1 == binding.fields.len() {
				crate::values::storage::encode_index_key(&component, &mut context)?
			} else {
				crate::values::storage::encode_index_prefix(&component, &mut context)?
			};
			let mut bound = encoded_prefix.clone();
			bound.extend(encoded);
			match constraint.comparison {
				ScalarComparison::Equal => {}
				ScalarComparison::GreaterOrEqual => {
					lower = tighter_lower(lower, Bound::Included(bound));
				}
				ScalarComparison::Greater => {
					let bound = if equality_fields + 1 == binding.fields.len() {
						Bound::Excluded(bound)
					} else {
						next_prefix(&bound).map_or(Bound::Unbounded, Bound::Included)
					};
					lower = tighter_lower(lower, bound);
				}
				ScalarComparison::Less => {
					upper = tighter_upper(upper, Bound::Excluded(bound));
				}
				ScalarComparison::LessOrEqual => {
					let bound = if equality_fields + 1 == binding.fields.len() {
						Bound::Included(bound)
					} else {
						next_prefix(&bound).map_or(Bound::Unbounded, Bound::Excluded)
					};
					upper = tighter_upper(upper, bound);
				}
			}
		}
		if let Some((after_key, _record)) = after.as_ref() {
			if key_in_bounds(after_key, &lower, &upper) {
				for (key, addresses) in read_groups(
					Bound::Included(after_key.clone()),
					Bound::Included(after_key.clone()),
					reverse,
					None,
				)? {
					groups.entry(key).or_default().extend(addresses);
				}
			}
			if reverse {
				upper = tighter_upper(upper, Bound::Excluded(after_key.clone()));
			} else {
				lower = tighter_lower(lower, Bound::Excluded(after_key.clone()));
			}
		}
		for (key, addresses) in read_groups(lower, upper, reverse, maximum)? {
			groups.entry(key).or_default().extend(addresses);
		}
	}
	ordered_index_addresses(
		groups,
		reverse,
		after.as_ref().map(|(key, record)| (key.as_slice(), *record)),
		maximum,
		&mut record_id,
	)
}

fn ordered_index_addresses<Address: Copy + Ord>(
	groups: std::collections::BTreeMap<Vec<u8>, BTreeSet<Address>>,
	reverse: bool,
	after: Option<(&[u8], &RecordId)>,
	maximum: Option<usize>,
	record_id: &mut impl FnMut(Address) -> FerrosGrynnResult<RecordId>,
) -> FerrosGrynnResult<Vec<Address>> {
	let mut groups = groups.into_iter().collect::<Vec<_>>();
	if reverse {
		groups.reverse();
	}
	let mut output = Vec::new();
	let mut seen = BTreeSet::new();
	for (key, addresses) in groups {
		if !key_is_after(&key, reverse, after.map(|(key, _record)| key)) {
			continue;
		}
		let mut posting = addresses
			.into_iter()
			.map(|address| Ok((record_id(address)?, address)))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		posting.sort_unstable();
		for (record, address) in posting {
			if after.is_some_and(|(after_key, after_record)| {
				key.as_slice() == after_key && record <= *after_record
			}) {
				continue;
			}
			if seen.insert(address) {
				output.push(address);
				if maximum.is_some_and(|maximum| output.len() >= maximum) {
					return Ok(output);
				}
			}
		}
	}
	Ok(output)
}

fn read_memory_index_groups<Address: Copy + Ord>(
	entries: &crate::datastore::IndexEntries<Address>,
	lower: Bound<Vec<u8>>,
	upper: Bound<Vec<u8>>,
	reverse: bool,
	maximum: Option<usize>,
) -> Vec<(Vec<u8>, Vec<Address>)> {
	let mut output = Vec::new();
	let mut address_count = 0_usize;
	entries.visit_groups(lower, upper, reverse, |key, posting| {
		address_count = address_count.saturating_add(posting.len());
		output.push((key.to_vec(), posting));
		maximum.is_none_or(|maximum| address_count < maximum)
	});
	output
}

fn read_physical_index_groups<Address: Copy + Ord>(
	index: &PhysicalIndexRead,
	lower: Bound<Vec<u8>>,
	upper: Bound<Vec<u8>>,
	reverse: bool,
	maximum: Option<usize>,
	mut address: impl FnMut(PhysicalIndexAtom) -> FerrosGrynnResult<Address>,
) -> FerrosGrynnResult<Vec<(Vec<u8>, Vec<Address>)>> {
	let PhysicalIndexEntries::Scalar(groups) = &index.entries else {
		return Err(FerrosGrynnError::invalid_storage_file(
			"Count index reached a scalar row cursor",
		));
	};
	let mut output = Vec::new();
	let mut address_count = 0_usize;
	let mut append = |group: &PhysicalScalarGroup| -> FerrosGrynnResult<bool> {
		if !key_in_bounds(&group.key, &lower, &upper) {
			return Ok(true);
		}
		let posting =
			group.atoms.iter().copied().map(&mut address).collect::<FerrosGrynnResult<Vec<_>>>()?;
		address_count = address_count.saturating_add(posting.len());
		output.push((group.key.clone(), posting));
		Ok(maximum.is_none_or(|maximum| address_count < maximum))
	};
	if reverse {
		for group in groups.iter().rev() {
			if !append(group)? {
				break;
			}
		}
	} else {
		for group in groups {
			if !append(group)? {
				break;
			}
		}
	}
	Ok(output)
}

fn key_in_bounds(key: &[u8], lower: &Bound<Vec<u8>>, upper: &Bound<Vec<u8>>) -> bool {
	let above_lower = match lower {
		Bound::Included(lower) => key >= lower.as_slice(),
		Bound::Excluded(lower) => key > lower.as_slice(),
		Bound::Unbounded => true,
	};
	let below_upper = match upper {
		Bound::Included(upper) => key <= upper.as_slice(),
		Bound::Excluded(upper) => key < upper.as_slice(),
		Bound::Unbounded => true,
	};
	above_lower && below_upper
}

fn physical_global_id(
	root: &DatabaseRoot,
	atom: PhysicalIndexAtom,
) -> FerrosGrynnResult<GlobalAtomId> {
	let slot = usize::try_from(atom.slot)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("index slot exceeds usize"))?;
	let generation = root
		.atoms
		.slot_generation(slot)
		.ok_or_else(|| FerrosGrynnError::invalid_storage_file("index slot is outside AtomStore"))?;
	let id = GlobalAtomId::from_raw(AtomId::new(atom.kind, atom.slot, generation).raw());
	root.atom(id).filter(|stored| stored.id().kind() == Some(atom.kind)).map(|_atom| id).ok_or_else(
		|| {
			FerrosGrynnError::invalid_storage_file(
				"durable scalar index references an absent global atom",
			)
		},
	)
}

fn physical_local_id(graph: &GraphRoot, atom: PhysicalIndexAtom) -> FerrosGrynnResult<AtomId> {
	let slot = usize::try_from(atom.slot)
		.map_err(|_error| FerrosGrynnError::invalid_storage_file("index slot exceeds usize"))?;
	let generation = graph.slot_generation(atom.kind, slot).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("index slot is outside graph occurrence storage")
	})?;
	let id = AtomId::new(atom.kind, atom.slot, generation);
	graph.occurrence(id).map(|_occurrence| id).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(
			"durable scalar index references an absent graph occurrence",
		)
	})
}

fn physical_count(index: &PhysicalIndexRead) -> FerrosGrynnResult<u64> {
	match &index.entries {
		PhysicalIndexEntries::Count(count) => Ok(*count),
		PhysicalIndexEntries::Scalar(_) => {
			Err(FerrosGrynnError::invalid_storage_file("Count index decoded as scalar"))
		}
		PhysicalIndexEntries::Fulltext(_) => {
			Err(FerrosGrynnError::invalid_storage_file("Count index decoded as Fulltext"))
		}
		PhysicalIndexEntries::Hnsw(_) => {
			Err(FerrosGrynnError::invalid_storage_file("Count index decoded as HNSW"))
		}
		PhysicalIndexEntries::Vector(_) => {
			Err(FerrosGrynnError::invalid_storage_file("Count index decoded as vector"))
		}
		PhysicalIndexEntries::Geometry(_) => {
			Err(FerrosGrynnError::invalid_storage_file("Count index decoded as geometry"))
		}
	}
}

fn validate_physical_index(
	physical: &PhysicalIndexRead,
	class_id: crate::datastore::ClassId,
	index: &ClassIndex,
) -> FerrosGrynnResult<()> {
	if physical.class_id != class_id
		|| physical.kind != index.kind()
		|| physical.scope != index.scope()
	{
		return Err(FerrosGrynnError::invalid_storage_file(
			"durable index metadata disagrees with the schema catalog",
		));
	}
	Ok(())
}

fn deferred_index_missing(index: IndexId) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(format!(
		"deferred index {} has no exact durable binding",
		index.get()
	))
}

fn key_is_after(key: &[u8], reverse: bool, after: Option<&[u8]>) -> bool {
	after.is_none_or(|after| {
		if reverse {
			key <= after
		} else {
			key >= after
		}
	})
}

fn base_record_id(root: &DatabaseRoot, global: GlobalAtomId) -> FerrosGrynnResult<RecordId> {
	root.atom(global).map(|atom| atom.record_id().clone()).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("scalar index references an absent global atom")
	})
}

fn graph_record_id(
	root: &DatabaseRoot,
	graph: &GraphRoot,
	local: AtomId,
) -> FerrosGrynnResult<RecordId> {
	let global = graph.occurrence(local).map(GraphOccurrence::global_id).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file(
			"effective scalar index references an absent graph occurrence",
		)
	})?;
	base_record_id(root, global)
}

fn next_prefix(prefix: &[u8]) -> Option<Vec<u8>> {
	let mut next = prefix.to_vec();
	for index in (0..next.len()).rev() {
		if next[index] != u8::MAX {
			next[index] += 1;
			next.truncate(index + 1);
			return Some(next);
		}
	}
	None
}

fn tighter_lower(current: Bound<Vec<u8>>, candidate: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
	match (&current, &candidate) {
		(Bound::Unbounded, _) => candidate,
		(_, Bound::Unbounded) => current,
		(Bound::Included(left), Bound::Included(right))
		| (Bound::Included(left), Bound::Excluded(right))
		| (Bound::Excluded(left), Bound::Included(right))
		| (Bound::Excluded(left), Bound::Excluded(right))
			if right > left =>
		{
			candidate
		}
		(Bound::Included(left), Bound::Excluded(right)) if right == left => candidate,
		_ => current,
	}
}

fn tighter_upper(current: Bound<Vec<u8>>, candidate: Bound<Vec<u8>>) -> Bound<Vec<u8>> {
	match (&current, &candidate) {
		(Bound::Unbounded, _) => candidate,
		(_, Bound::Unbounded) => current,
		(Bound::Included(left), Bound::Included(right))
		| (Bound::Included(left), Bound::Excluded(right))
		| (Bound::Excluded(left), Bound::Included(right))
		| (Bound::Excluded(left), Bound::Excluded(right))
			if right < left =>
		{
			candidate
		}
		(Bound::Included(left), Bound::Excluded(right)) if right == left => candidate,
		_ => current,
	}
}

fn planner_binding_summary<Address: Copy + Ord>(
	binding: Option<&crate::datastore::IndexBinding<Address>>,
	class_id: crate::datastore::ClassId,
	index_id: crate::datastore::IndexId,
	index: &ClassIndex,
) -> FerrosGrynnResult<(bool, Option<u64>)> {
	let Some(binding) = binding else {
		return Ok((false, None));
	};
	if binding.id != index_id
		|| binding.class_id != class_id
		|| binding.version != index.version()
		|| binding.kind != index.kind()
		|| binding.fields.len() != index.fields().len()
	{
		return Err(FerrosGrynnError::invalid_storage_file(format!(
			"physical binding for index '{}' disagrees with the schema catalog",
			index.name()
		)));
	}
	Ok((true, (!binding.deferred).then(|| binding.entries.count()).flatten()))
}

fn row_from_atom(atom: &crate::datastore::BaseAtom, class: &ClassSchema) -> QueryStorageRow {
	let properties = atom.properties().clone();
	let mut values = Vec::with_capacity(class.fields().len() + 1);
	values.push(Value::Record(Box::new(atom.record_id().clone())));
	values.extend(class.fields().map(|field| {
		field.key.and_then(|key| properties.get(key).cloned()).unwrap_or(Value::None)
	}));
	QueryStorageRow {
		record_id: atom.record_id().clone(),
		kind: atom.kind(),
		local_id: None,
		label: None,
		values,
		base_properties: properties.clone(),
		properties,
	}
}

fn row_from_occurrence(
	root: &DatabaseRoot,
	occurrence: &GraphOccurrence,
	class: &ClassSchema,
) -> FerrosGrynnResult<QueryStorageRow> {
	let atom = root.atom(occurrence.global_id()).ok_or_else(|| {
		FerrosGrynnError::invalid_storage_file("graph occurrence references an absent global atom")
	})?;
	let base_properties = atom.properties().clone();
	let mut properties = base_properties.clone();
	if let Some(overlay) = occurrence.overlay() {
		for (key, value) in overlay.iter() {
			match value {
				OverlayValue::Value(value) => {
					properties.insert(key, value.clone());
				}
				OverlayValue::Hidden => {
					properties.remove(key);
				}
			}
		}
	}
	let mut values = Vec::with_capacity(class.fields().len() + 1);
	values.push(Value::Record(Box::new(atom.record_id().clone())));
	values.extend(class.fields().map(|field| {
		field.key.and_then(|key| properties.get(key).cloned()).unwrap_or(Value::None)
	}));
	Ok(QueryStorageRow {
		record_id: atom.record_id().clone(),
		kind: atom.kind(),
		local_id: Some(occurrence.local_id()),
		label: occurrence.label().cloned(),
		values,
		base_properties,
		properties,
	})
}

fn query_graph_root<'root>(
	root: &'root DatabaseRoot,
	reference: &GraphRef,
) -> FerrosGrynnResult<&'root GraphRoot> {
	let graph = root
		.graph(reference.id())
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(reference.name().to_string()))?;
	if graph.reference() != reference {
		return Err(FerrosGrynnError::invalid_storage_file(
			"query graph reference disagrees with the snapshot catalog",
		));
	}
	Ok(graph)
}

fn intersect_sorted(left: Vec<AtomId>, right: Vec<AtomId>) -> Vec<AtomId> {
	let (mut left_index, mut right_index) = (0, 0);
	let mut intersection = Vec::with_capacity(left.len().min(right.len()));
	while let (Some(left_id), Some(right_id)) = (left.get(left_index), right.get(right_index)) {
		match left_id.cmp(right_id) {
			std::cmp::Ordering::Less => left_index += 1,
			std::cmp::Ordering::Greater => right_index += 1,
			std::cmp::Ordering::Equal => {
				intersection.push(*left_id);
				left_index += 1;
				right_index += 1;
			}
		}
	}
	intersection
}
