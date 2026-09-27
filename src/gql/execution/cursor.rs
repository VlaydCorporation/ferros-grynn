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
pub(super) struct BoundAtom {
	pub(super) graph: crate::graph::GraphId,
	pub(super) atom: AtomId,
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
	trace: Option<PathTraceId>,
	/// Exact cumulative cost stored in the active trace.
	trace_cost: Number,
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
		}
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
	}
}

pub(super) trait RowCursor {
	fn next(&mut self, control: &ExecutionControl) -> FerrosGrynnResult<Option<Row>>;
}

pub(super) trait QueryRead {
	/// Graph view tied to the requested query snapshot.
	type Graph<'a>: GraphRead + 'a
	where
		Self: 'a;
	type Savepoint;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot>;

	/// Returns the number of logical mutations currently staged by the
	/// transaction.
	fn mutation_count(&self) -> usize;

	fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog;

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

	/// Reads one exact transaction-visible Count index.
	fn count_index(
		&self,
		snapshot: Snapshot,
		index: crate::datastore::IndexId,
		graph: Option<&GraphRef>,
	) -> FerrosGrynnResult<u64>;

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
					append_trace_hop(
						&mut row,
						&candidate,
						self.graph.as_ref(),
						&self.graph_ref,
						self.trace.as_ref(),
						Number::Int(1),
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
					append_trace_hop(
						&mut row,
						&candidate,
						self.graph.as_ref(),
						&self.graph_ref,
						self.trace.as_ref(),
						Number::Int(1),
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
	path_candidate_allowed,
	path_hop_weight,
	path_row,
};
pub(super) use pipeline::{
	AtomFieldRead,
	FilterCursor,
	MatchGraphFields,
	OneRowCursor,
	RecordFields,
};
pub(super) use scoped::{ScopedTraversalCandidate, ScopedTraversalIndex, resolve_scoped_endpoint};
pub(super) use scoped_path::{ScopedPathExpandCursor, ScopedShortestPathCursor};
pub(super) use shortest::ShortestPathCursor;
