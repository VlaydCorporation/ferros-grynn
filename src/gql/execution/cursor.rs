#![allow(
	clippy::wildcard_imports,
	reason = "move-only executor split keeps the existing imports during stage 1"
)]

use std::mem::size_of;

use super::*;

/// Bounded window for a physically ordered scalar-index source.
#[derive(Clone, Copy)]
pub(super) struct IndexScanWindow<'a> {
	pub(super) after: Option<&'a crate::gql::cursor::CursorAnchor>,
	pub(super) maximum: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Snapshot(pub(super) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Snapshot-local atom handle qualified by its graph occurrence.
pub(in crate::gql) struct BoundAtom {
	pub(super) graph: crate::graph::GraphId,
	pub(super) atom: AtomId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FulltextMatch {
	pub(super) predicate: u32,
	pub(super) slot: Option<u16>,
	pub(super) matched: bool,
	pub(super) highlights: bool,
	pub(super) score_bits: u64,
	pub(super) text: Box<str>,
	pub(super) spans: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KnnMatch {
	pub(super) predicate: u32,
	pub(super) distance_bits: u64,
}

impl BoundAtom {
	pub(super) const fn new(graph: crate::graph::GraphId, atom: AtomId) -> Self {
		Self {
			graph,
			atom,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
	pub(super) values: Vec<Value>,
	/// Whether atom metadata columns 1..=4 are omitted from this direct-pull
	/// row.
	compact_atom: bool,
	/// Source-local ordinals used to preserve left-major relational order.
	pub(super) sequence: Vec<usize>,
	/// Deterministic public identities used to break ORDER ties.
	pub(super) stable: Vec<Value>,
	/// Evaluated MATCH ORDER keys.
	pub(super) order: Vec<Value>,
	/// Graph scope used when dereferencing RecordId properties.
	pub(super) graph: Option<GraphRef>,
	/// Internal atom identities aligned with MATCH binding columns.
	pub(super) atoms: Vec<Option<BoundAtom>>,
	/// Connector edges already consumed by DIFFERENT EDGES semantics.
	used_edges: Vec<BoundAtom>,
	/// Endpoint and connector atoms consumed by DIFFERENT ATOMS semantics.
	used_atoms: Vec<BoundAtom>,
	/// Endpoint from which the next linear expansion starts.
	current: Option<BoundAtom>,
	/// Terminal trace node for the currently active named pattern.
	pub(super) trace: Option<PathTraceId>,
	/// Exact cumulative cost stored in the active trace.
	pub(super) trace_cost: Number,
	/// Fulltext outcomes materialized before relational filtering.
	pub(super) fulltext: Vec<FulltextMatch>,
	/// Exact or ANN KNN membership materialized before filtering.
	pub(super) knn: Vec<KnnMatch>,
}

impl Row {
	pub(super) const fn new(values: Vec<Value>) -> Self {
		let atoms = Vec::new();
		Self {
			values,
			compact_atom: false,
			sequence: Vec::new(),
			stable: Vec::new(),
			order: Vec::new(),
			graph: None,
			atoms,
			used_edges: Vec::new(),
			used_atoms: Vec::new(),
			current: None,
			trace: None,
			trace_cost: Number::Int(0),
			fulltext: Vec::new(),
			knn: Vec::new(),
		}
	}

	/// Keeps the datastore's `id + declared fields` layout for direct SELECT.
	pub(super) const fn compact_atom(values: Vec<Value>) -> Self {
		let mut row = Self::new(values);
		row.compact_atom = true;
		row
	}

	/// Resolves a logical schema column without expanding unused atom metadata.
	pub(super) fn value(&self, column: usize) -> Option<&Value> {
		let physical = if self.compact_atom {
			match column {
				0 => 0,
				5.. => column - 4,
				_ => return None,
			}
		} else {
			column
		};
		self.values.get(physical)
	}

	/// Creates an empty MATCH binding row with `count` columns.
	pub(super) fn bindings(count: usize) -> Self {
		Self {
			values: vec![Value::None; count],
			compact_atom: false,
			sequence: Vec::new(),
			stable: Vec::new(),
			order: Vec::new(),
			graph: None,
			atoms: vec![None; count],
			used_edges: Vec::new(),
			used_atoms: Vec::new(),
			current: None,
			trace: None,
			trace_cost: Number::Int(0),
			fulltext: Vec::new(),
			knn: Vec::new(),
		}
	}

	/// Seeds one MATCH binding from an outer correlated environment.
	///
	/// Returns `false` when the seeded occurrence violates the clause match
	/// mode.
	pub(super) fn seed_binding(
		&mut self,
		column: usize,
		value: Value,
		occurrence: BoundAtom,
		mode: crate::syn::MatchMode,
	) -> bool {
		let repeated = match mode {
			crate::syn::MatchMode::RepeatableElements => false,
			crate::syn::MatchMode::DifferentEdges => {
				occurrence.atom.kind() == Some(AtomKind::Edge)
					&& self.used_edges.contains(&occurrence)
			}
			crate::syn::MatchMode::DifferentAtoms => self.used_atoms.contains(&occurrence),
		};
		if repeated {
			return false;
		}
		self.values[column] = value;
		self.atoms[column] = Some(occurrence);
		match mode {
			crate::syn::MatchMode::RepeatableElements => {}
			crate::syn::MatchMode::DifferentEdges
				if occurrence.atom.kind() == Some(AtomKind::Edge) =>
			{
				self.used_edges.push(occurrence);
			}
			crate::syn::MatchMode::DifferentAtoms => self.used_atoms.push(occurrence),
			crate::syn::MatchMode::DifferentEdges => {}
		}
		true
	}

	pub(super) fn with_graph(mut self, graph: Option<GraphRef>) -> Self {
		self.graph = graph;
		self
	}

	pub(super) fn with_stable(mut self, stable: Vec<Value>) -> Self {
		self.stable = stable;
		self
	}

	pub(super) fn with_sequence(mut self, sequence: Vec<usize>) -> Self {
		self.sequence = sequence;
		self
	}

	pub(super) fn fulltext_match(&self, predicate: u32) -> Option<&FulltextMatch> {
		self.fulltext.iter().find(|entry| entry.predicate == predicate)
	}

	pub(super) fn fulltext_slot(&self, slot: u16) -> Option<&FulltextMatch> {
		self.fulltext.iter().find(|entry| entry.slot == Some(slot))
	}

	pub(super) fn knn_match(&self, predicate: u32) -> Option<KnnMatch> {
		self.knn.iter().copied().find(|entry| entry.predicate == predicate)
	}

	pub(super) fn estimated_heap_bytes(&self) -> usize {
		size_of::<Self>()
			.saturating_add(self.values.capacity().saturating_mul(size_of::<Value>()))
			.saturating_add(
				self.values
					.iter()
					.map(Value::estimated_heap_bytes)
					.fold(0_usize, usize::saturating_add),
			)
			.saturating_add(self.sequence.capacity().saturating_mul(size_of::<usize>()))
			.saturating_add(self.stable.capacity().saturating_mul(size_of::<Value>()))
			.saturating_add(self.order.capacity().saturating_mul(size_of::<Value>()))
			.saturating_add(
				self.stable
					.iter()
					.map(Value::estimated_heap_bytes)
					.fold(0_usize, usize::saturating_add),
			)
			.saturating_add(
				self.order
					.iter()
					.map(Value::estimated_heap_bytes)
					.fold(0_usize, usize::saturating_add),
			)
			.saturating_add(self.graph.as_ref().map_or(0, |graph| graph.name().len()))
			.saturating_add(self.atoms.capacity().saturating_mul(size_of::<Option<BoundAtom>>()))
			.saturating_add(self.used_edges.capacity().saturating_mul(size_of::<BoundAtom>()))
			.saturating_add(self.used_atoms.capacity().saturating_mul(size_of::<BoundAtom>()))
			.saturating_add(self.fulltext.iter().fold(0_usize, |bytes, entry| {
				bytes
					.saturating_add(size_of::<FulltextMatch>())
					.saturating_add(entry.text.len())
					.saturating_add(entry.spans.capacity().saturating_mul(size_of::<(u32, u32)>()))
			}))
			.saturating_add(self.knn.capacity().saturating_mul(size_of::<KnnMatch>()))
	}
}

pub(super) trait RowCursor {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>>;
}

/// Datastore-owned logical output of one snapshot-exact physical frontier.
pub(super) struct PhysicalTraversalBatch {
	pub(super) candidates: Vec<ScopedTraversalCandidate>,
	pub(super) resident_pages: u64,
	pub(super) wal_pages: u64,
	pub(super) sync_home_pages: u64,
	pub(super) ioring_home_pages: u64,
}

/// Datastore-owned result of one physical nested-graph lookup.
pub(super) struct PhysicalInnerGraphBatch {
	pub(super) graph: Option<crate::graph::GraphId>,
	pub(super) resident_pages: u64,
	pub(super) wal_pages: u64,
	pub(super) sync_home_pages: u64,
	pub(super) ioring_home_pages: u64,
}

/// Owned public fields decoded through the local physical traversal cursor.
pub(super) struct PhysicalOccurrenceFieldsBatch {
	pub(super) record_id: RecordId,
	pub(super) properties: crate::values::PropertyMap,
	pub(super) resident_pages: u64,
	pub(super) wal_pages: u64,
	pub(super) sync_home_pages: u64,
	pub(super) ioring_home_pages: u64,
}

/// One query-term contribution read from an immutable Fulltext binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhysicalFulltextTerm {
	pub(super) document_frequency: u64,
	pub(super) term_frequency: u32,
	pub(super) spans: Vec<(u32, u32)>,
}

/// One matching document addressed only by its public record identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhysicalFulltextDocument {
	pub(super) record_id: RecordId,
	pub(super) document_length: u32,
	pub(super) terms: std::collections::BTreeMap<Box<str>, PhysicalFulltextTerm>,
}

/// Exact statistics and matching documents from one snapshot-bound index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhysicalFulltextBatch {
	pub(super) document_count: u64,
	pub(super) total_document_length: u64,
	pub(super) documents: Vec<PhysicalFulltextDocument>,
}

/// One ANN candidate addressed only by its public record identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PhysicalVectorHit {
	pub(super) record_id: RecordId,
	pub(super) distance_bits: u64,
}

pub(super) trait QueryRead {
	/// Graph view tied to the requested query snapshot.
	type Graph<'a>: GraphRead + 'a
	where
		Self: 'a;
	type Savepoint;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot>;

	/// Recaptures the transaction-private root after an explicit traversal
	/// side effect and returns its logical snapshot identity.
	fn refresh_snapshot(&mut self) -> FerrosGrynnResult<Snapshot>;

	/// Returns the number of logical mutations currently staged by the
	/// transaction.
	fn mutation_count(&self) -> usize;

	fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog;

	/// Captures immutable qualified incidence from this statement's read root.
	fn endpoint_tensor(
		&self,
		snapshot: Snapshot,
		graph: &GraphRef,
		options: crate::graph::tensor::TensorOptions,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<crate::graph::tensor::EndpointTensor> {
		let view = self.graph(snapshot, graph)?;
		crate::graph::tensor::EndpointTensor::from_graph_with_check(&view, options, &mut || check())
	}

	/// Reads the selected logical generation, reporting exact on-demand fallback.
	fn endpoint_tensor_selected(
		&self, snapshot: Snapshot, graph: &GraphRef,
		options: crate::graph::tensor::TensorOptions,
		_selected: Option<&crate::datastore::QueryTensorCache>,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<(crate::graph::tensor::EndpointTensor, Option<crate::datastore::QueryTensorCache>)> {
		self.endpoint_tensor(snapshot, graph, options, check).map(|tensor| (tensor, None))
	}

	/// Reuses the bounded PURE evaluator against this exact immutable root.
	fn tensor_cost_evaluator(
		&self,
		_snapshot: Snapshot,
		_bytes: usize,
	) -> FerrosGrynnResult<crate::gql::PureFunctionEvaluator<'_>> {
		Err(FerrosGrynnError::invalid_argument("this reader has no published PURE COST catalog"))
	}

	/// Records dependencies consulted by bounded tensor COST evaluation.
	fn observe_tensor_cost(&self, _key: crate::datastore::ConflictKey) {}

	/// Captures the catalog, index, exact-counter, and demanded-histogram
	/// projection used by physical planning.
	fn planner_snapshot(
		&self,
		snapshot: Snapshot,
		demand: &crate::gql::optimizer::PlannerDemand,
	) -> FerrosGrynnResult<crate::gql::optimizer::PlannerSnapshot>;

	fn scan<'a>(
		&'a self,
		snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		predicate: Option<&TypedExpr>,
		index: Option<(crate::datastore::IndexId, bool)>,
		window: Option<IndexScanWindow<'_>>,
		slots: &[Value],
		source: &str,
		full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>>;

	fn get(
		&self,
		snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<Option<Row>>;

	/// Resolves one database-global atom without inventing graph topology.
	fn base_atom(
		&self,
		snapshot: Snapshot,
		record: &RecordId,
	) -> FerrosGrynnResult<Option<(AtomKind, PropertyMap)>>;

	/// Reads one exact transaction-visible Count index.
	fn count_index(
		&self,
		snapshot: Snapshot,
		index: crate::datastore::IndexId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64>;

	/// Reads one immutable Fulltext binding without exposing storage addresses.
	/// `None` requests the exact analyzer scan fallback.
	fn fulltext_index(
		&self,
		snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		terms: &std::collections::BTreeSet<Box<str>>,
	) -> FerrosGrynnResult<Option<PhysicalFulltextBatch>>;

	/// Searches one immutable spatial binding without exposing storage
	/// addresses.
	fn geometry_index(
		&self,
		snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		query: &crate::values::Geometry,
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Option<crate::datastore::QueryGeometryIndexRead>>;

	/// Reads a logical bound from the selected immutable generation.
	fn path_lower_bound(
		&self,
		snapshot: Snapshot,
		binding: &crate::datastore::QueryLandmarkPath,
		source: AtomId,
		target: AtomId,
	) -> FerrosGrynnResult<Option<Number>>;

	/// Executes only the selected sketch binding or bounded exact fallback.
	#[allow(
		clippy::too_many_arguments,
		reason = "the logical estimate keeps projection, binding and resource guards explicit"
	)]
	fn k_hop_estimate(
		&self,
		snapshot: Snapshot,
		occurrence: &AtomOccurrenceRef,
		mode: crate::graph::TraversalSemantics,
		radius: u32,
		index: Option<crate::datastore::IndexId>,
		scratch_bytes: usize,
		max_work: u64,
		admit: &mut dyn FnMut(usize) -> FerrosGrynnResult<()>,
		check: &mut dyn FnMut() -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<crate::datastore::QueryNeighbourhoodEstimate>;

	/// Searches one immutable ANN binding without exposing storage addresses.
	/// `None` requests the bounded exact-scan fallback.
	fn vector_index(
		&self,
		snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		query: &crate::values::Vector,
		k: usize,
		breadth: usize,
	) -> FerrosGrynnResult<Option<Vec<PhysicalVectorHit>>>;

	/// Opens an exact full-key probe of one transaction-visible scalar index.
	fn probe_index<'a>(
		&'a self,
		snapshot: Snapshot,
		class: &str,
		graph: Option<&GraphRef>,
		index: crate::datastore::IndexId,
		values: &[Value],
		full_row: bool,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>>;

	/// Produces graph-catalog rows in explicit-list or GraphId order.
	fn graph_rows(
		&self,
		snapshot: Snapshot,
		references: Option<&[GraphRef]>,
	) -> FerrosGrynnResult<Vec<Row>>;

	/// Returns graph-local seed candidates from primary, class, and label
	/// indexes.
	fn match_candidates(
		&self,
		snapshot: Snapshot,
		graph: &GraphRef,
		binding: &ResolvedMatchBinding,
	) -> FerrosGrynnResult<Vec<AtomId>>;

	/// Attempts a bounded local hot-page expansion. `None` requests the
	/// occurrence-aware materialized fallback.
	fn physical_traversal_candidates(
		&self,
		snapshot: Snapshot,
		graph: &GraphRef,
		source: AtomId,
		directions: &[crate::syn::MatchDirection],
	) -> FerrosGrynnResult<Option<PhysicalTraversalBatch>>;

	/// Attempts a snapshot-exact local nested-graph lookup. `None` requests
	/// the materialized occurrence fallback.
	fn physical_inner_graph(
		&self,
		snapshot: Snapshot,
		graph: &GraphRef,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalInnerGraphBatch>>;

	/// Attempts a snapshot-exact local base/effective property read. `None`
	/// requests the ordinary materialized occurrence fallback.
	fn physical_occurrence_fields(
		&self,
		snapshot: Snapshot,
		graph: &GraphRef,
		owner: AtomId,
	) -> FerrosGrynnResult<Option<PhysicalOccurrenceFieldsBatch>>;

	/// Dereferences one record field in the statement's optional graph scope.
	fn record_field(
		&self,
		snapshot: Snapshot,
		record: &RecordId,
		graph: Option<&GraphRef>,
		field: &str,
	) -> FerrosGrynnResult<Value>;

	/// Opens a graph without refreshing the statement snapshot.
	fn graph<'a>(
		&'a self,
		snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'a>>;

	/// Resolves one graph identity in the same immutable statement snapshot.
	fn graph_reference(
		&self,
		snapshot: Snapshot,
		graph: crate::graph::GraphId,
	) -> FerrosGrynnResult<GraphRef>;

	/// Lists public and internal graph references in stable GraphId order.
	fn graph_references(&self, snapshot: Snapshot) -> FerrosGrynnResult<Vec<GraphRef>>;

	/// Resolves one public graph definition without opening graph data.
	fn named_graph_contract(
		&self,
		snapshot: Snapshot,
		name: &str,
	) -> FerrosGrynnResult<Option<(GraphRef, crate::graph::GraphTopologyConstraints)>>;

	/// Computes the canonical read-only LevelPath for one occurrence.
	fn level_path(
		&self,
		snapshot: Snapshot,
		occurrence: &AtomOccurrenceRef,
	) -> FerrosGrynnResult<Value>;

	/// Returns the property dictionary captured by the statement snapshot.
	fn property_dictionary(&self, snapshot: Snapshot) -> Arc<PropertyDictionary>;

	/// Builds the exact dictionary/reference context for transient query runs.
	fn spill_context(
		&self,
		snapshot: Snapshot,
	) -> FerrosGrynnResult<crate::values::storage::EncodeContext>;

	/// Returns the database-owned root below which query scratch is created.
	fn query_scratch_root(&self) -> std::path::PathBuf;

	fn savepoint(&self) -> Self::Savepoint;

	fn restore_savepoint(&mut self, savepoint: Self::Savepoint) -> FerrosGrynnResult<()>;

	fn assert_invariants(
		&mut self,
		graph: Option<&GraphRef>,
		detail_limit: usize,
	) -> FerrosGrynnResult<usize>;

	/// Applies one typed schema command to the transaction that owns this view.
	fn execute_schema(
		&mut self,
		statement: &LogicalSchemaStatement,
		slots: &[Value],
		source: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value>;

	/// Applies one typed data-mutation statement to the owning transaction.
	fn execute_dml(
		&mut self,
		statement: &LogicalDmlStatement,
		slots: &[Value],
		source: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value>;

	/// Applies one typed graph-membership or graph-incidence mutation.
	fn execute_graph_dml(
		&mut self,
		statement: &LogicalGraphDmlStatement,
		slots: &[Value],
		source: &str,
		control: &ExecutionControl,
	) -> FerrosGrynnResult<Value>;

	/// Atomically installs one already validated owned graph into the
	/// transaction.
	fn install_built_graph(
		&mut self,
		name: &str,
		mode: crate::syn::DefinitionMode,
		contract_explicit: bool,
		graph: crate::graph::MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef>;
}

#[derive(Debug, Clone)]
/// One traversable connector candidate.
pub(super) struct TraversalCandidate {
	/// Candidate target endpoint.
	target: AtomId,
	/// Connector edge used by the hop.
	edge: AtomId,
	/// Direction recorded in the public path step.
	direction: PathDirection,
	/// Incidence role recorded in the public path step.
	role: EndpointRole,
}

/// Single-hop cursor that scans graph edge identifiers once.
pub(super) struct EdgeExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared public graph identity for emitted occurrences.
	graph_ref: Arc<GraphRef>,
	/// Fixed expansion contract.
	expand: ResolvedExpand,
	/// Evaluator for inline property and WHERE constraints.
	predicate: MatchPredicateContext<'a>,
	/// Named-path trace arena, absent when the path is unobservable.
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	/// Connector identifiers scanned for each source row.
	edges: Vec<AtomId>,
	/// Current upstream row.
	row: Option<Row>,
	/// Candidates for the current source endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next candidate position.
	next_candidate: usize,
}

impl<'a, Graph: GraphRead> EdgeExpandCursor<'a, Graph> {
	/// Captures the edge-id scan and creates a fixed-hop cursor.
	pub(super) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		graph_ref: Arc<GraphRef>,
		expand: ResolvedExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
	) -> FerrosGrynnResult<Self> {
		let edges = graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Self {
			input,
			graph,
			graph_ref,
			expand,
			predicate,
			trace,
			edges,
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		})
	}
}

impl<Graph: GraphRead> RowCursor for EdgeExpandCursor<'_, Graph> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				control.record_path_expansion()?;
				if !fixed_candidate_allowed(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
				)? {
					continue;
				}
				if let Some(mut row) = expand_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
					&self.graph_ref,
					self.predicate,
					control,
				)? {
					let contribution = fixed_hop_cost(
						self.graph.as_ref(),
						&self.graph_ref,
						&self.expand,
						candidate.edge,
						self.predicate,
						control,
					)?;
					append_trace_hop(
						&mut row,
						&candidate,
						self.graph.as_ref(),
						&self.graph_ref,
						self.trace.as_ref(),
						contribution,
						control,
					)?;
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next(control)? else {
				return Ok(None);
			};
			let source = row
				.current
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH expand has no source endpoint.")
				})?
				.atom;
			self.candidates = edge_candidates(
				self.graph.as_ref(),
				&self.edges,
				source,
				self.expand.direction,
				&self.expand.edge,
			)?;
			self.next_candidate = 0;
			self.row = Some(row);
		}
	}
}

/// Fixed-hop cursor backed by a shared traversal index.
pub(super) struct IndexExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared public graph identity for emitted occurrences.
	graph_ref: Arc<GraphRef>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Fixed expansion contract.
	expand: ResolvedExpand,
	/// Evaluator for inline property and WHERE constraints.
	predicate: MatchPredicateContext<'a>,
	/// Named-path trace arena, absent when the path is unobservable.
	trace: Option<Rc<RefCell<MatchPathTrace>>>,
	/// Current upstream row.
	row: Option<Row>,
	/// Candidates for the current source endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next candidate position.
	next_candidate: usize,
}

impl<'a, Graph> IndexExpandCursor<'a, Graph> {
	/// Creates a fixed-hop cursor over an existing index.
	pub(super) fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		graph_ref: Arc<GraphRef>,
		index: Rc<TraversalIndex>,
		expand: ResolvedExpand,
		predicate: MatchPredicateContext<'a>,
		trace: Option<Rc<RefCell<MatchPathTrace>>>,
	) -> Self {
		Self {
			input,
			graph,
			graph_ref,
			index,
			expand,
			predicate,
			trace,
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		}
	}
}

impl<Graph: GraphRead> RowCursor for IndexExpandCursor<'_, Graph> {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>> {
		loop {
			control.check()?;
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				control.record_path_expansion()?;
				if !fixed_candidate_allowed(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
				)? {
					continue;
				}
				if let Some(mut row) = expand_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
					&self.graph_ref,
					self.predicate,
					control,
				)? {
					let contribution = fixed_hop_cost(
						self.graph.as_ref(),
						&self.graph_ref,
						&self.expand,
						candidate.edge,
						self.predicate,
						control,
					)?;
					append_trace_hop(
						&mut row,
						&candidate,
						self.graph.as_ref(),
						&self.graph_ref,
						self.trace.as_ref(),
						contribution,
						control,
					)?;
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next(control)? else {
				return Ok(None);
			};
			let source = row
				.current
				.ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH expand has no source endpoint.")
				})?
				.atom;
			self.candidates = index_candidates(
				self.graph.as_ref(),
				self.index.as_ref(),
				source,
				self.expand.direction,
			)?;
			self.next_candidate = 0;
			self.row = Some(row);
		}
	}
}

/// Applies bindings and DIFFERENT EDGES checks to one hop.
pub(super) fn expand_row<Graph: GraphRead>(
	input: &Row,
	expand: &ResolvedExpand,
	candidate: &TraversalCandidate,
	graph: &Graph,
	graph_ref: &Arc<GraphRef>,
	predicate: MatchPredicateContext<'_>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Option<Row>> {
	if !atom_matches(graph, candidate.target, &expand.target)? {
		return Ok(None);
	}
	let mut row = input.clone();
	if !bind_match_position(
		&mut row,
		&expand.edge,
		candidate.edge,
		graph,
		graph_ref,
		expand.mode,
		predicate,
		control,
	)? || !bind_match_position(
		&mut row,
		&expand.target,
		candidate.target,
		graph,
		graph_ref,
		expand.mode,
		predicate,
		control,
	)? {
		return Ok(None);
	}
	row.current = Some(BoundAtom::new(graph_ref.id(), candidate.target));
	Ok(Some(row))
}

/// Applies edge-local path filters to one fixed expansion candidate.
pub(super) fn fixed_candidate_allowed<Graph: GraphRead>(
	row: &Row,
	expand: &ResolvedExpand,
	candidate: &TraversalCandidate,
	graph: &Graph,
) -> FerrosGrynnResult<bool> {
	if !connector_matches(graph, candidate.edge, &expand.edge)? {
		return Ok(false);
	}
	let edge = graph
		.edge(candidate.edge)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", candidate.edge.to_string()))?;
	let topology = edge.topology()?;
	for filter in &expand.filters {
		let accepted = match filter {
			crate::syn::MatchPathFilter::Weighted => edge.weight().is_some(),
			crate::syn::MatchPathFilter::HyperEdges => topology.is_hyper(),
			crate::syn::MatchPathFilter::MetaEdges => edge.is_meta()?,
			crate::syn::MatchPathFilter::Meta => {
				edge.is_meta()?
					&& endpoint_is_meta(graph, row.current.map(|atom| atom.atom))?
					&& endpoint_is_meta(graph, Some(candidate.target))?
			}
			crate::syn::MatchPathFilter::MetaVertices => {
				endpoint_is_meta(graph, row.current.map(|atom| atom.atom))?
					&& endpoint_is_meta(graph, Some(candidate.target))?
			}
			crate::syn::MatchPathFilter::CrossLevel => false,
		};
		if !accepted {
			return Ok(false);
		}
	}
	Ok(true)
}

fn endpoint_is_meta<Graph: GraphRead>(
	graph: &Graph,
	endpoint: Option<AtomId>,
) -> FerrosGrynnResult<bool> {
	let Some(endpoint) = endpoint else {
		return Ok(false);
	};
	match endpoint.kind() {
		Some(AtomKind::Node) => graph.node(endpoint)?.map_or(Ok(false), |node| node.is_meta()),
		Some(AtomKind::Edge) => graph.edge(endpoint)?.map_or(Ok(false), |edge| edge.is_meta()),
		None => Ok(false),
	}
}

/// Applies the connector class, exact-record, and graph-label predicates.
pub(super) fn connector_matches<Graph: GraphRead>(
	graph: &Graph,
	edge: AtomId,
	binding: &ResolvedMatchBinding,
) -> FerrosGrynnResult<bool> {
	atom_matches(graph, edge, binding)
}

/// Finds one-hop candidates by scanning connector edges.
pub(super) fn edge_candidates<Graph: GraphRead>(
	graph: &Graph,
	edges: &[AtomId],
	source: AtomId,
	direction: crate::syn::MatchDirection,
	binding: &ResolvedMatchBinding,
) -> FerrosGrynnResult<Vec<TraversalCandidate>> {
	let mut candidates = Vec::new();
	for edge_id in edges {
		if !connector_matches(graph, *edge_id, binding)? {
			continue;
		}
		let edge = graph
			.edge(*edge_id)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge_id.to_string()))?;
		let topology = edge.topology()?;
		match topology.as_ref() {
			EdgeTopology::Directed {
				source: from,
				target: to,
			} => match direction {
				crate::syn::MatchDirection::Forward => push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					from,
					to,
					PathDirection::Forward,
					EndpointRole::Target,
				),
				crate::syn::MatchDirection::Backward => push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					to,
					from,
					PathDirection::Reverse,
					EndpointRole::Source,
				),
				crate::syn::MatchDirection::Undirected
				| crate::syn::MatchDirection::Bidirectional => {}
			},
			EdgeTopology::Bidirectional {
				source: from,
				target: to,
			} if direction != crate::syn::MatchDirection::Undirected => {
				push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					from,
					to,
					PathDirection::Forward,
					EndpointRole::Target,
				);
				push_endpoint_pairs(
					&mut candidates,
					source,
					*edge_id,
					to,
					from,
					PathDirection::Reverse,
					EndpointRole::Source,
				);
			}
			EdgeTopology::Undirected {
				endpoints,
			} if direction == crate::syn::MatchDirection::Undirected => {
				for (index, endpoint) in endpoints.iter().enumerate() {
					if endpoint.local_id() != Some(source) {
						continue;
					}
					for target in endpoints
						.iter()
						.enumerate()
						.filter(|(target_index, _target)| *target_index != index)
						.filter_map(|(_target_index, target)| target.local_id())
					{
						candidates.push(TraversalCandidate {
							target,
							edge: *edge_id,
							direction: PathDirection::Undirected,
							role: EndpointRole::Undirected,
						});
					}
				}
			}
			EdgeTopology::Bidirectional {
				..
			}
			| EdgeTopology::Undirected {
				..
			} => {}
		}
	}
	Ok(candidates)
}

/// Appends all local target endpoints when the source occurs in `from`.
pub(super) fn push_endpoint_pairs(
	candidates: &mut Vec<TraversalCandidate>,
	source: AtomId,
	edge: AtomId,
	from: &[EdgeEndpoint],
	to: &[EdgeEndpoint],
	direction: PathDirection,
	role: EndpointRole,
) {
	if !from.iter().any(|endpoint| endpoint.local_id() == Some(source)) {
		return;
	}
	for target in to.iter().filter_map(EdgeEndpoint::local_id) {
		candidates.push(TraversalCandidate {
			target,
			edge,
			direction,
			role,
		});
	}
}

/// Converts traversal-index steps into MATCH candidates.
pub(super) fn index_candidates<Graph: GraphRead>(
	graph: &Graph,
	index: &TraversalIndex,
	source: AtomId,
	direction: crate::syn::MatchDirection,
) -> FerrosGrynnResult<Vec<TraversalCandidate>> {
	let traversal = match direction {
		crate::syn::MatchDirection::Forward => TraversalDirection::Forward,
		crate::syn::MatchDirection::Backward => TraversalDirection::Backward,
		crate::syn::MatchDirection::Undirected | crate::syn::MatchDirection::Bidirectional => {
			TraversalDirection::Both
		}
	};
	let endpoint = EdgeEndpoint::Local(source);
	let mut candidates = Vec::new();
	let endpoint_index = if index.semantics() == TraversalSemantics::Atom {
		TraversalIndex::for_edge_endpoint(graph, &endpoint)?
	} else {
		None
	};
	let index = endpoint_index.as_ref().unwrap_or(index);
	for step in index.steps(&endpoint, traversal) {
		let Some(target) = step.target().local_id() else {
			continue;
		};
		let edge = graph.edge(step.via_edge())?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("edge", step.via_edge().to_string())
		})?;
		if !direction_accepts(direction, edge.topology()?.direction()) {
			continue;
		}
		candidates.push(TraversalCandidate {
			target,
			edge: step.via_edge(),
			direction: step.direction(),
			role: step.role(),
		});
	}
	Ok(candidates)
}

/// Checks whether an edge topology satisfies the requested MATCH direction.
pub(super) const fn direction_accepts(
	requested: crate::syn::MatchDirection,
	actual: EdgeDirection,
) -> bool {
	match requested {
		crate::syn::MatchDirection::Forward | crate::syn::MatchDirection::Backward => {
			matches!(actual, EdgeDirection::Directed | EdgeDirection::Bidirectional)
		}
		crate::syn::MatchDirection::Undirected => matches!(actual, EdgeDirection::Undirected),
		crate::syn::MatchDirection::Bidirectional => {
			matches!(actual, EdgeDirection::Bidirectional)
		}
	}
}

mod barrier;
mod binding;
mod cross_level;
mod hyper;
mod nesting;
mod path;
mod pipeline;
mod scoped;
mod scoped_path;
mod shortest;
mod tensor_shortest;

pub(super) use barrier::{MatchProjectCursor, OptionalCursor, distinct_rows, sort_match_rows};
pub(super) use binding::{
	MatchPredicateContext,
	ResolvedExpand,
	ResolvedMatchBinding,
	ResolvedMatchSelector,
	ResolvedPathExpand,
	SeedCursor,
	atom_matches,
	bind_match_position,
	resolve_expand,
	resolve_match_binding,
	resolve_path_expand,
};
pub(super) use cross_level::{
	CrossLevelExpandCursor,
	append_scoped_trace_hop,
	scoped_filters_match,
};
pub(super) use hyper::{HyperContainsCursor, ResolvedHyperContains};
pub(super) use nesting::{ResolvedWithin, WithinCursor, nested_graph_reference};
pub(super) use path::{
	MatchPathTrace,
	PathBindCursor,
	PathExpandCursor,
	PathTraceStartCursor,
	add_path_cost,
	append_trace_hop,
	custom_path_contribution,
	custom_path_cost,
	fixed_hop_cost,
	path_candidate_allowed,
	path_edge_weight,
	path_hop_cost,
	path_row,
};
pub(in crate::gql) use pipeline::{AtomFieldRead, RootFields};
pub(super) use pipeline::{
	FilterCursor,
	MatchGraphFields,
	OneRowCursor,
	RecordFields,
	properties_object,
};
pub(super) use scoped::{ScopedTraversalCandidate, ScopedTraversalIndex, resolve_scoped_endpoint};
pub(super) use scoped_path::{ScopedPathExpandCursor, ScopedShortestPathCursor};
pub(super) use shortest::{
	CompletedPath,
	ShortestPathCursor,
	canonical_atom_bytes,
	completion_bound,
	extend_path_buffers,
	monotone_path_prefix,
};
pub(super) use tensor_shortest::TensorShortestPathCursor;
