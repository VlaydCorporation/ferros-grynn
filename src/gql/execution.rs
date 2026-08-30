#![allow(
	clippy::pattern_type_mismatch,
	reason = "the executor intentionally uses match ergonomics for borrowed IR and runtime values"
)]

use std::{collections::BTreeSet, rc::Rc, sync::Arc, time::Instant};

use rust_decimal::Decimal;

use super::{
	QueryResult,
	QueryStatementKind,
	analysis::{QueryCatalog, analyze},
	ir::{
		Column,
		EndpointDomain,
		Expand,
		LogicalMatch,
		LogicalMatchPattern,
		LogicalNodeKind,
		LogicalProgram,
		LogicalSelect,
		LogicalStatement,
		MatchBinding,
		MatchOp,
		NodeId,
		PathExpand,
		ScanTarget,
		Schema,
		TypedExpr,
		TypedExprKind,
	},
};
use crate::{
	datastore::{
		PredicateEvaluator,
		QuerySnapshot,
		QueryStorageRow,
		ScalarComparison,
		ScalarIndexConstraint,
		TableSchema,
		Transaction,
		TransactionGraphView,
	},
	gql_status::{ExecutionPhase, FerrosGrynnError, FerrosGrynnResult, Location, Outcome},
	graph::{
		AtomId,
		EdgeDirection,
		EdgeEndpoint,
		EdgeRead,
		EdgeTopology,
		EndpointRole,
		GraphRead,
		NodeRead,
		PathDirection,
		PathStep,
		TraversalDirection,
		TraversalIndex,
		TraversalSemantics,
	},
	math::{add_exact, divide_signed_exact, multiply_exact, subtract_exact},
	syn::{BinaryOperator, ParserSettings, Program, UnaryOperator},
	values::{
		Array,
		AtomKind,
		GraphRef,
		Number,
		Object,
		Path,
		PropertyDictionary,
		RecordId,
		Type,
		Value,
	},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Snapshot(pub(super) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
	pub(super) values: Vec<Value>,
	/// Internal atom identities aligned with MATCH binding columns.
	atoms: Vec<Option<AtomId>>,
	/// Connector edges already consumed by DIFFERENT EDGES semantics.
	used_edges: Vec<AtomId>,
	/// Endpoint from which the next linear expansion starts.
	current: Option<AtomId>,
}

impl Row {
	pub(super) const fn new(values: Vec<Value>) -> Self {
		let atoms = Vec::new();
		Self {
			values,
			atoms,
			used_edges: Vec::new(),
			current: None,
		}
	}

	/// Creates an empty MATCH binding row with `count` columns.
	fn bindings(count: usize) -> Self {
		Self {
			values: vec![Value::None; count],
			atoms: vec![None; count],
			used_edges: Vec::new(),
			current: None,
		}
	}
}

pub(super) trait RowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>>;
}

pub(super) trait QueryRead {
	/// Graph view tied to the requested query snapshot.
	type Graph<'a>: GraphRead + 'a
	where
		Self: 'a;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot>;

	fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog;

	fn scan<'a>(
		&'a self,
		snapshot: Snapshot,
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>>;

	fn get(&self, snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>>;

	/// Opens a graph without refreshing the statement snapshot.
	fn graph<'a>(
		&'a self,
		snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'a>>;

	/// Returns the property dictionary captured by the statement snapshot.
	fn property_dictionary(&self, snapshot: Snapshot) -> Arc<PropertyDictionary>;
}

/// Parses, analyzes, and executes query text within an existing transaction.
///
/// # Errors
///
/// Returns syntax, semantic, storage, or execution errors produced by the
/// query.
pub fn execute_transaction(
	transaction: &Transaction,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = transaction.query_snapshot()?;
	execute_text(
		&TransactionRead {
			transaction,
			snapshot,
		},
		source,
		variables,
		settings,
	)
}

struct TransactionRead<'a> {
	transaction: &'a Transaction,
	snapshot: QuerySnapshot,
}

impl QueryCatalog for TransactionRead<'_> {
	fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
		self.transaction.query_table(&self.snapshot, table).map(|schema| query_schema(&schema))
	}

	fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
		self.transaction.query_named_graph(&self.snapshot, name)
	}
}

impl QueryRead for TransactionRead<'_> {
	type Graph<'a>
		= TransactionGraphView<'a>
	where
		Self: 'a;

	fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
		Ok(Snapshot(self.snapshot.commit_ts()))
	}

	fn catalog(&self, _snapshot: Snapshot) -> &dyn QueryCatalog {
		self
	}

	fn scan<'a>(
		&'a self,
		_snapshot: Snapshot,
		table: &str,
		predicate: Option<&TypedExpr>,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
		let mut constraints = Vec::new();
		if let Some(predicate) = predicate {
			collect_scalar_constraints(predicate, slots, &mut constraints);
		}
		let rows = self.transaction.query_rows(&self.snapshot, table, &constraints)?;
		let evaluator = Arc::new(QueryPredicate {
			predicate: predicate.cloned(),
			slots: slots.to_vec(),
			source: source.to_owned(),
		});
		self.transaction.register_query_predicate(&self.snapshot, table, &rows, evaluator)?;
		Ok(Box::new(StorageCursor {
			rows: rows.into_iter(),
		}))
	}

	fn get(&self, _snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
		let row = self.transaction.query_point(&self.snapshot, record)?;
		self.transaction.register_query_point(
			&self.snapshot,
			record.table(),
			record,
			row.as_ref(),
		)?;
		Ok(row.map(|row| Row::new(row.values)))
	}

	fn graph<'a>(
		&'a self,
		_snapshot: Snapshot,
		reference: &GraphRef,
	) -> FerrosGrynnResult<Self::Graph<'a>> {
		self.transaction.query_graph(&self.snapshot, reference)
	}

	fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
		self.snapshot.property_dictionary()
	}
}

fn collect_scalar_constraints(
	expr: &TypedExpr,
	slots: &[Value],
	constraints: &mut Vec<ScalarIndexConstraint>,
) {
	let TypedExprKind::Binary {
		left,
		op,
		right,
	} = &expr.kind
	else {
		return;
	};
	if matches!(op, BinaryOperator::And) {
		collect_scalar_constraints(left, slots, constraints);
		collect_scalar_constraints(right, slots, constraints);
		return;
	}
	let comparison = match op {
		BinaryOperator::Equal | BinaryOperator::Is => ScalarComparison::Equal,
		BinaryOperator::Less => ScalarComparison::Less,
		BinaryOperator::LessOrEqual => ScalarComparison::LessOrEqual,
		BinaryOperator::Greater => ScalarComparison::Greater,
		BinaryOperator::GreaterOrEqual => ScalarComparison::GreaterOrEqual,
		_ => return,
	};
	if let (TypedExprKind::Column(column), Some(value)) =
		(&left.kind, scalar_constraint_value(right, slots))
	{
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison,
			value,
		});
	} else if let (Some(value), TypedExprKind::Column(column)) =
		(scalar_constraint_value(left, slots), &right.kind)
	{
		constraints.push(ScalarIndexConstraint {
			column: *column,
			comparison: match comparison {
				ScalarComparison::Equal => ScalarComparison::Equal,
				ScalarComparison::Less => ScalarComparison::Greater,
				ScalarComparison::LessOrEqual => ScalarComparison::GreaterOrEqual,
				ScalarComparison::Greater => ScalarComparison::Less,
				ScalarComparison::GreaterOrEqual => ScalarComparison::LessOrEqual,
			},
			value,
		});
	}
}

fn scalar_constraint_value(expr: &TypedExpr, slots: &[Value]) -> Option<Value> {
	match &expr.kind {
		TypedExprKind::Constant(value) => Some(value.clone()),
		TypedExprKind::Slot(slot) => slots.get(usize::try_from(*slot).ok()?).cloned(),
		_ => None,
	}
}

fn query_schema(schema: &TableSchema) -> Schema {
	let mut columns = Vec::with_capacity(schema.fields().len() + 1);
	columns.push(Column {
		name: "id".into(),
		ty: Type::Record,
	});
	columns.extend(schema.fields().map(|field| Column {
		name: field.name().as_str().into(),
		ty: field.value_type().clone(),
	}));
	Schema {
		columns,
	}
}

struct QueryPredicate {
	predicate: Option<TypedExpr>,
	slots: Vec<Value>,
	source: String,
}

impl PredicateEvaluator for QueryPredicate {
	fn matches(&self, row: &QueryStorageRow) -> FerrosGrynnResult<bool> {
		self.predicate.as_ref().map_or(Ok(true), |predicate| {
			eval(predicate, Some(&Row::new(row.values.clone())), &self.slots, &self.source, None)
				.map(|value| value.is_truthy())
		})
	}
}

struct StorageCursor {
	rows: std::vec::IntoIter<QueryStorageRow>,
}

impl RowCursor for StorageCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.rows.next().map(|row| Row::new(row.values)))
	}
}

/// Parses and executes text through a statically dispatched read adapter.
pub(super) fn execute_text<Read: QueryRead>(
	read: &Read,
	source: &str,
	variables: &Object,
	settings: ParserSettings,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let program = crate::gql::parse(source, settings)?;
	execute_program(read, source, &program, variables)
}

/// Executes an already parsed program against one captured snapshot.
fn execute_program<Read: QueryRead>(
	read: &Read,
	source: &str,
	program: &Program,
	variables: &Object,
) -> FerrosGrynnResult<Vec<QueryResult>> {
	let snapshot = read.snapshot()?;
	let logical = analyze(program, source, read.catalog(snapshot), variables)?;
	let mut slots = initialize_slots(&logical, variables);
	let mut results = Vec::with_capacity(logical.statements.len());
	for statement in &logical.statements {
		let start = Instant::now();
		let (kind, result) = match statement {
			LogicalStatement::Let {
				slot,
				value,
			} => {
				let result = eval(value, None, &slots, source, None).map(|value| {
					slots[*slot as usize] = value;
					Outcome::success(Value::None)
				});
				(QueryStatementKind::Let, result)
			}
			LogicalStatement::Return {
				value,
			} => (
				QueryStatementKind::Return,
				eval(value, None, &slots, source, None).map(Outcome::success),
			),
			LogicalStatement::Match(plan) => (
				QueryStatementKind::Match,
				execute_match(read, snapshot, plan, &slots, source).map(Outcome::success),
			),
			LogicalStatement::Select(select) => (
				QueryStatementKind::Select,
				execute_select(read, snapshot, select, &slots, source).map(Outcome::success),
			),
		};
		let failed = result.is_err();
		results.push(QueryResult {
			kind,
			elapsed: start.elapsed(),
			result,
		});
		if failed {
			break;
		}
	}
	Ok(results)
}

fn initialize_slots(program: &LogicalProgram, variables: &Object) -> Vec<Value> {
	let mut slots = vec![Value::None; program.slot_count];
	for (name, slot) in &program.initial_slots {
		if let Some(value) = variables.get(name) {
			slots[*slot as usize] = value.clone();
		}
	}
	slots
}

/// Executes a logical MATCH as a synchronous pull pipeline.
fn execute_match<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalMatch,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	let graph = Rc::new(read.graph(snapshot, &plan.graph)?);
	let ordered = optimize_match_patterns(graph.as_ref(), &plan.patterns)?;
	let has_chain = plan.patterns.iter().any(|pattern| pattern.ops.len() > 1);
	let has_path = plan
		.patterns
		.iter()
		.flat_map(|pattern| &pattern.ops)
		.any(|op| matches!(op, MatchOp::PathExpand(_)));
	let semantics = if plan.patterns.iter().any(pattern_uses_edge_endpoint) {
		TraversalSemantics::Atom
	} else {
		TraversalSemantics::Node
	};
	let index = (has_chain || has_path)
		.then(|| TraversalIndex::new(graph.as_ref(), semantics).map(Rc::new))
		.transpose()?;
	let fields = GraphFields {
		graph: graph.clone(),
		dictionary: read.property_dictionary(snapshot),
	};
	let mut cursor: Box<dyn RowCursor + '_> = Box::new(OneRowCursor {
		row: Some(Row::bindings(plan.bindings.columns.len())),
	});
	for pattern in ordered {
		cursor = Box::new(SeedCursor::new(cursor, graph.clone(), pattern.start.clone())?);
		for op in &pattern.ops {
			cursor = match op {
				MatchOp::Expand(expand) => {
					if let Some(index) = &index {
						Box::new(IndexExpandCursor::new(
							cursor,
							graph.clone(),
							index.clone(),
							expand.clone(),
						))
					} else {
						Box::new(EdgeExpandCursor::new(cursor, graph.clone(), expand.clone())?)
					}
				}
				MatchOp::PathExpand(expand) => Box::new(PathExpandCursor::new(
					cursor,
					graph.clone(),
					index.clone().ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"MATCH path expansion requires a traversal index.",
						)
					})?,
					expand.clone(),
				)),
			};
		}
	}
	if let Some(predicate) = &plan.filter {
		cursor = Box::new(FilterCursor {
			input: cursor,
			predicate,
			slots,
			source,
			fields: Some(&fields),
		});
	}
	cursor = Box::new(ProjectCursor {
		input: cursor,
		expressions: &plan.projection,
		slots,
		source,
		fields: Some(&fields),
	});

	let mut result = Array::new(Vec::new());
	while let Some(row) = cursor.next().map_err(execution_phase)? {
		let object = plan
			.result
			.columns
			.iter()
			.zip(row.values)
			.map(|(column, value)| (column.name.as_ref(), value))
			.collect::<Object>();
		result.push(Value::Object(object));
	}
	Ok(Value::Array(result))
}

/// Orders patterns by connectivity, estimated cardinality, and source order.
fn optimize_match_patterns<'a, Graph: GraphRead>(
	graph: &Graph,
	patterns: &'a [LogicalMatchPattern],
) -> FerrosGrynnResult<Vec<&'a LogicalMatchPattern>> {
	let nodes = graph.node_count()?;
	let edges = graph.edge_count()?;
	let average_degree =
		edges.saturating_mul(2).saturating_add(nodes.max(1).saturating_sub(1)) / nodes.max(1);
	let mut remaining = patterns.iter().collect::<Vec<_>>();
	let mut known = BTreeSet::new();
	let mut ordered = Vec::with_capacity(patterns.len());
	while !remaining.is_empty() {
		let mut best = 0;
		for index in 1..remaining.len() {
			let candidate = remaining[index];
			let current = remaining[best];
			let candidate_connected =
				pattern_columns(candidate).any(|column| known.contains(&column));
			let current_connected = pattern_columns(current).any(|column| known.contains(&column));
			let candidate_key = (
				!candidate_connected,
				pattern_estimate(candidate, nodes, edges, average_degree),
				candidate.original_index,
			);
			let current_key = (
				!current_connected,
				pattern_estimate(current, nodes, edges, average_degree),
				current.original_index,
			);
			if candidate_key < current_key {
				best = index;
			}
		}
		let pattern = remaining.remove(best);
		known.extend(pattern_columns(pattern));
		ordered.push(pattern);
	}
	Ok(ordered)
}

/// Estimates one pattern using exact graph counts and average degree.
fn pattern_estimate(
	pattern: &LogicalMatchPattern,
	nodes: usize,
	edges: usize,
	average_degree: usize,
) -> usize {
	let hops = pattern.ops.iter().fold(0_usize, |total, op| {
		total.saturating_add(match op {
			MatchOp::Expand(_) => 1,
			MatchOp::PathExpand(expand) => expand.max as usize,
		})
	});
	if hops == 0 {
		return match pattern.start.domain {
			EndpointDomain::Node => nodes,
			EndpointDomain::Edge => edges,
		};
	}
	(1..hops).fold(edges, |estimate, _hop| estimate.saturating_mul(average_degree))
}

/// Iterates all binding columns mentioned by a pattern.
fn pattern_columns(pattern: &LogicalMatchPattern) -> impl Iterator<Item = usize> + '_ {
	pattern.start.column.into_iter().chain(pattern.ops.iter().flat_map(|op| {
		match op {
			MatchOp::Expand(expand) => {
				[expand.source.column, expand.edge.column, expand.target.column]
					.into_iter()
					.flatten()
					.collect::<Vec<_>>()
			}
			MatchOp::PathExpand(expand) => {
				[expand.source.column, expand.target.column, expand.path_column]
					.into_iter()
					.flatten()
					.collect::<Vec<_>>()
			}
		}
	}))
}

/// Reports whether a pattern requires AtomWalk traversal semantics.
fn pattern_uses_edge_endpoint(pattern: &LogicalMatchPattern) -> bool {
	pattern.start.domain == EndpointDomain::Edge
		|| pattern.ops.iter().any(|op| match op {
			MatchOp::Expand(expand) => {
				expand.source.domain == EndpointDomain::Edge
					|| expand.target.domain == EndpointDomain::Edge
			}
			MatchOp::PathExpand(expand) => {
				expand.source.domain == EndpointDomain::Edge
					|| expand.target.domain == EndpointDomain::Edge
			}
		})
}

/// Pull cursor that binds the first endpoint of a pattern.
struct SeedCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Endpoint constraints and optional alias column.
	binding: MatchBinding,
	/// Candidate atom identifiers for an unbound endpoint.
	ids: Vec<AtomId>,
	/// Current upstream row.
	row: Option<Row>,
	/// Alias-constrained atom, when already bound by another pattern.
	bound: Option<AtomId>,
	/// Next candidate position.
	next_id: usize,
}

impl<'a, Graph: GraphRead> SeedCursor<'a, Graph> {
	/// Collects the endpoint domain once and creates a seed cursor.
	fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		binding: MatchBinding,
	) -> FerrosGrynnResult<Self> {
		let ids = match binding.domain {
			EndpointDomain::Node => graph.node_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?,
			EndpointDomain::Edge => graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?,
		};
		Ok(Self {
			input,
			graph,
			binding,
			ids,
			row: None,
			bound: None,
			next_id: 0,
		})
	}
}

impl<Graph: GraphRead> RowCursor for SeedCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if self.row.is_none() {
				let Some(row) = self.input.next()? else {
					return Ok(None);
				};
				self.bound =
					self.binding.column.and_then(|column| row.atoms.get(column).copied().flatten());
				self.row = Some(row);
				self.next_id = 0;
			}
			let candidate = if let Some(bound) = self.bound {
				(self.next_id == 0).then_some(bound)
			} else {
				self.ids.get(self.next_id).copied()
			};
			let Some(candidate) = candidate else {
				self.row = None;
				continue;
			};
			self.next_id += 1;
			if !atom_matches(self.graph.as_ref(), candidate, &self.binding)? {
				continue;
			}
			let mut row = self.row.as_ref().cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH seed lost its input row.")
			})?;
			if !bind_atom(&mut row, &self.binding, candidate, self.graph.as_ref())? {
				continue;
			}
			row.current = Some(candidate);
			return Ok(Some(row));
		}
	}
}

/// Applies endpoint-domain and alternative-label constraints.
fn atom_matches<Graph: GraphRead>(
	graph: &Graph,
	atom: AtomId,
	binding: &MatchBinding,
) -> FerrosGrynnResult<bool> {
	if atom.kind()
		!= Some(match binding.domain {
			EndpointDomain::Node => AtomKind::Node,
			EndpointDomain::Edge => AtomKind::Edge,
		}) {
		return Ok(false);
	}
	if binding.labels.is_empty() {
		return Ok(true);
	}
	match binding.domain {
		EndpointDomain::Node => {
			let Some(node) = graph.node(atom)? else {
				return Ok(false);
			};
			let label = node.label_text()?;
			Ok(label.is_some_and(|label| {
				binding.labels.iter().any(|candidate| candidate.as_ref() == label.as_str())
			}))
		}
		EndpointDomain::Edge => {
			let Some(edge) = graph.edge(atom)? else {
				return Ok(false);
			};
			let label = edge.label_text()?;
			Ok(label.is_some_and(|label| {
				binding.labels.iter().any(|candidate| candidate.as_ref() == label.as_str())
			}))
		}
	}
}

/// Binds an atom or checks equality with an existing alias binding.
fn bind_atom<Graph: GraphRead>(
	row: &mut Row,
	binding: &MatchBinding,
	atom: AtomId,
	graph: &Graph,
) -> FerrosGrynnResult<bool> {
	let Some(column) = binding.column else {
		return Ok(true);
	};
	if let Some(existing) = row.atoms.get(column).copied().flatten() {
		return Ok(existing == atom);
	}
	if !matches!(row.values.get(column), Some(Value::None)) {
		return Ok(false);
	}
	let reference = graph.atom_ref(atom)?.ok_or_else(|| {
		let kind = match atom.kind() {
			Some(AtomKind::Node) => "node",
			Some(AtomKind::Edge) => "edge",
			None => "atom",
		};
		FerrosGrynnError::atom_does_not_exist(kind, atom.to_string())
	})?;
	row.values[column] = Value::Atom(Box::new(reference));
	row.atoms[column] = Some(atom);
	Ok(true)
}

#[derive(Clone)]
/// One traversable connector candidate.
struct TraversalCandidate {
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
struct EdgeExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Fixed expansion contract.
	expand: Expand,
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
	fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		expand: Expand,
	) -> FerrosGrynnResult<Self> {
		let edges = graph.edge_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Self {
			input,
			graph,
			expand,
			edges,
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		})
	}
}

impl<Graph: GraphRead> RowCursor for EdgeExpandCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				if let Some(row) = expand_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
				)? {
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next()? else {
				return Ok(None);
			};
			let source = row.current.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH expand has no source endpoint.")
			})?;
			self.candidates = edge_candidates(
				self.graph.as_ref(),
				&self.edges,
				source,
				self.expand.direction,
				&self.expand.edge.labels,
			)?;
			self.next_candidate = 0;
			self.row = Some(row);
		}
	}
}

/// Fixed-hop cursor backed by a shared traversal index.
struct IndexExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Fixed expansion contract.
	expand: Expand,
	/// Current upstream row.
	row: Option<Row>,
	/// Candidates for the current source endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next candidate position.
	next_candidate: usize,
}

impl<'a, Graph> IndexExpandCursor<'a, Graph> {
	/// Creates a fixed-hop cursor over an existing index.
	fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		index: Rc<TraversalIndex>,
		expand: Expand,
	) -> Self {
		Self {
			input,
			graph,
			index,
			expand,
			row: None,
			candidates: Vec::new(),
			next_candidate: 0,
		}
	}
}

impl<Graph: GraphRead> RowCursor for IndexExpandCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if let Some(candidate) = self.candidates.get(self.next_candidate).cloned() {
				self.next_candidate += 1;
				if !connector_matches(
					self.graph.as_ref(),
					candidate.edge,
					&self.expand.edge.labels,
				)? {
					continue;
				}
				if let Some(row) = expand_row(
					self.row.as_ref().ok_or_else(|| {
						FerrosGrynnError::invalid_argument("MATCH expand lost its input row.")
					})?,
					&self.expand,
					&candidate,
					self.graph.as_ref(),
				)? {
					return Ok(Some(row));
				}
				continue;
			}
			let Some(row) = self.input.next()? else {
				return Ok(None);
			};
			let source = row.current.ok_or_else(|| {
				FerrosGrynnError::invalid_argument("MATCH expand has no source endpoint.")
			})?;
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
fn expand_row<Graph: GraphRead>(
	input: &Row,
	expand: &Expand,
	candidate: &TraversalCandidate,
	graph: &Graph,
) -> FerrosGrynnResult<Option<Row>> {
	if !atom_matches(graph, candidate.target, &expand.target)? {
		return Ok(None);
	}
	let edge_bound =
		expand.edge.column.and_then(|column| input.atoms.get(column).copied().flatten());
	if edge_bound.is_some_and(|edge| edge != candidate.edge)
		|| edge_bound.is_none() && input.used_edges.contains(&candidate.edge)
	{
		return Ok(None);
	}
	let mut row = input.clone();
	if !bind_atom(&mut row, &expand.edge, candidate.edge, graph)?
		|| !bind_atom(&mut row, &expand.target, candidate.target, graph)?
	{
		return Ok(None);
	}
	if !row.used_edges.contains(&candidate.edge) {
		row.used_edges.push(candidate.edge);
	}
	row.current = Some(candidate.target);
	Ok(Some(row))
}

/// Applies connector-label alternatives lazily.
fn connector_matches<Graph: GraphRead>(
	graph: &Graph,
	edge: AtomId,
	labels: &[Box<str>],
) -> FerrosGrynnResult<bool> {
	if labels.is_empty() {
		return Ok(true);
	}
	let label = graph
		.edge(edge)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", edge.to_string()))?
		.label_text()?
		.map(|label| label.into_owned());
	Ok(label
		.is_some_and(|label| labels.iter().any(|candidate| candidate.as_ref() == label.as_str())))
}

/// Finds one-hop candidates by scanning connector edges.
fn edge_candidates<Graph: GraphRead>(
	graph: &Graph,
	edges: &[AtomId],
	source: AtomId,
	direction: crate::syn::MatchDirection,
	labels: &[Box<str>],
) -> FerrosGrynnResult<Vec<TraversalCandidate>> {
	let mut candidates = Vec::new();
	for edge_id in edges {
		if !connector_matches(graph, *edge_id, labels)? {
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
fn push_endpoint_pairs(
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
fn index_candidates<Graph: GraphRead>(
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
const fn direction_accepts(requested: crate::syn::MatchDirection, actual: EdgeDirection) -> bool {
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

/// Incremental depth-first traversal state for one endpoint.
struct DfsFrame {
	/// Endpoint represented by this stack frame.
	endpoint: AtomId,
	/// Outgoing candidates computed for the endpoint.
	candidates: Vec<TraversalCandidate>,
	/// Next outgoing candidate position.
	next_candidate: usize,
	/// Whether the path ending at this endpoint was already emitted.
	yielded: bool,
}

/// Bounded TRAIL cursor that retains only its active DFS path.
struct PathExpandCursor<'a, Graph> {
	/// Upstream rows to extend.
	input: Box<dyn RowCursor + 'a>,
	/// Shared snapshot graph view.
	graph: Rc<Graph>,
	/// Shared traversal index.
	index: Rc<TraversalIndex>,
	/// Variable-length expansion contract.
	expand: PathExpand,
	/// Current upstream row.
	row: Option<Row>,
	/// Active DFS stack.
	frames: Vec<DfsFrame>,
	/// Active connector path aligned below the frame stack.
	hops: Vec<TraversalCandidate>,
}

impl<'a, Graph> PathExpandCursor<'a, Graph> {
	/// Creates an incremental path-expansion cursor.
	fn new(
		input: Box<dyn RowCursor + 'a>,
		graph: Rc<Graph>,
		index: Rc<TraversalIndex>,
		expand: PathExpand,
	) -> Self {
		Self {
			input,
			graph,
			index,
			expand,
			row: None,
			frames: Vec::new(),
			hops: Vec::new(),
		}
	}
}

impl<Graph: GraphRead> PathExpandCursor<'_, Graph> {
	/// Initializes traversal state for the next upstream row.
	fn start_row(&mut self) -> FerrosGrynnResult<bool> {
		let Some(row) = self.input.next()? else {
			return Ok(false);
		};
		let source = row.current.ok_or_else(|| {
			FerrosGrynnError::invalid_argument("MATCH path expansion has no source endpoint.")
		})?;
		let candidates = index_candidates(
			self.graph.as_ref(),
			self.index.as_ref(),
			source,
			self.expand.direction,
		)?;
		self.frames.clear();
		self.frames.push(DfsFrame {
			endpoint: source,
			candidates,
			next_candidate: 0,
			yielded: false,
		});
		self.hops.clear();
		self.row = Some(row);
		Ok(true)
	}

	/// Removes the deepest DFS frame and its incoming hop.
	fn backtrack(&mut self) {
		self.frames.pop();
		if self.frames.is_empty() {
			self.row = None;
		} else {
			self.hops.pop();
		}
	}
}

impl<Graph: GraphRead> RowCursor for PathExpandCursor<'_, Graph> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		loop {
			if self.row.is_none() && !self.start_row()? {
				return Ok(None);
			}
			if self.frames.is_empty() {
				self.row = None;
				continue;
			}

			let depth = self.hops.len();
			let emit = {
				let frame = self.frames.last_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH DFS stack is empty.")
				})?;
				let emit = !frame.yielded
					&& depth >= self.expand.min as usize
					&& depth <= self.expand.max as usize;
				frame.yielded = true;
				emit
			};
			if emit {
				let endpoint = self.frames.last().map(|frame| frame.endpoint).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH DFS stack is empty.")
				})?;
				if atom_matches(self.graph.as_ref(), endpoint, &self.expand.target)?
					&& let Some(row) = path_row(
						self.row.as_ref().ok_or_else(|| {
							FerrosGrynnError::invalid_argument("MATCH path lost its input row.")
						})?,
						&self.expand,
						&self.frames,
						&self.hops,
						self.graph.as_ref(),
					)? {
					return Ok(Some(row));
				}
			}

			if depth >= self.expand.max as usize {
				self.backtrack();
				continue;
			}
			let candidate = {
				let frame = self.frames.last_mut().ok_or_else(|| {
					FerrosGrynnError::invalid_argument("MATCH DFS stack is empty.")
				})?;
				let candidate = frame.candidates.get(frame.next_candidate).cloned();
				frame.next_candidate += usize::from(candidate.is_some());
				candidate
			};
			let Some(candidate) = candidate else {
				self.backtrack();
				continue;
			};
			if candidate.target.kind()
				!= Some(match self.expand.target.domain {
					EndpointDomain::Node => AtomKind::Node,
					EndpointDomain::Edge => AtomKind::Edge,
				}) || self.row.as_ref().is_some_and(|row| row.used_edges.contains(&candidate.edge))
				|| self.hops.iter().any(|hop| hop.edge == candidate.edge)
				|| !connector_matches(
					self.graph.as_ref(),
					candidate.edge,
					&self.expand.edge_labels,
				)? {
				continue;
			}
			let candidates = index_candidates(
				self.graph.as_ref(),
				self.index.as_ref(),
				candidate.target,
				self.expand.direction,
			)?;
			self.hops.push(candidate.clone());
			self.frames.push(DfsFrame {
				endpoint: candidate.target,
				candidates,
				next_candidate: 0,
				yielded: false,
			});
		}
	}
}

/// Materializes one public path value from the active DFS stack.
fn path_row<Graph: GraphRead>(
	input: &Row,
	expand: &PathExpand,
	frames: &[DfsFrame],
	hops: &[TraversalCandidate],
	graph: &Graph,
) -> FerrosGrynnResult<Option<Row>> {
	let Some(first) = frames.first().map(|frame| frame.endpoint) else {
		return Ok(None);
	};
	let Some(target) = frames.last().map(|frame| frame.endpoint) else {
		return Ok(None);
	};
	let mut row = input.clone();
	if !bind_atom(&mut row, &expand.target, target, graph)? {
		return Ok(None);
	}
	let first_ref = graph
		.atom_ref(first)?
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", first.to_string()))?;
	let mut steps = Vec::with_capacity(hops.len() + 1);
	steps.push(PathStep::new(first_ref, None, PathDirection::Forward, EndpointRole::Source));
	for hop in hops {
		let atom = graph
			.atom_ref(hop.target)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("atom", hop.target.to_string()))?;
		let edge = graph
			.atom_ref(hop.edge)?
			.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("edge", hop.edge.to_string()))?;
		steps.push(PathStep::new(atom, Some(edge), hop.direction, hop.role));
	}
	let path = Value::Path(Box::new(Path::new(graph.identity(), graph.snapshot_id(), steps, None)));
	if let Some(column) = expand.path_column {
		match row.values.get(column) {
			Some(Value::None) => row.values[column] = path,
			Some(existing) if existing == &path => {}
			_ => return Ok(None),
		}
	}
	for hop in hops {
		if !row.used_edges.contains(&hop.edge) {
			row.used_edges.push(hop.edge);
		}
	}
	row.current = Some(target);
	Ok(Some(row))
}

/// Executes the existing SELECT pull pipeline.
fn execute_select<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
) -> FerrosGrynnResult<Value> {
	let predicate = plan.nodes.iter().find_map(|node| match &node.kind {
		LogicalNodeKind::Filter {
			predicate,
			..
		} => Some(predicate),
		_ => None,
	});
	let mut cursor = build_cursor(read, snapshot, plan, plan.root, slots, source, predicate)?;
	let schema = &plan.node(plan.root).schema;
	let mut result = Array::new(Vec::new());
	while let Some(row) = cursor.next().map_err(execution_phase)? {
		let value = if plan.value {
			row.values.into_iter().next().unwrap_or(Value::None)
		} else {
			let object = schema
				.columns
				.iter()
				.zip(row.values)
				.map(|(column, value)| (column.name.as_ref(), value))
				.collect::<Object>();
			Value::Object(object)
		};
		result.push(value);
		if plan.only && result.len() > 1 {
			return Err(FerrosGrynnError::invalid_argument(
				"SELECT ONLY returned more than one row.",
			)
			.with_phase(ExecutionPhase::Execution));
		}
	}
	Ok(Value::Array(result))
}

/// Builds the existing SELECT cursor chain for one logical node.
fn build_cursor<'a, Read: QueryRead>(
	read: &'a Read,
	snapshot: Snapshot,
	plan: &'a LogicalSelect,
	node_id: NodeId,
	slots: &'a [Value],
	source: &'a str,
	scan_predicate: Option<&'a TypedExpr>,
) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
	let node = plan.node(node_id);
	match &node.kind {
		LogicalNodeKind::Scan {
			target,
		} => {
			let inner: Box<dyn RowCursor + 'a> = match target {
				ScanTarget::Table(table) => {
					read.scan(snapshot, table, scan_predicate, slots, source)?
				}
				ScanTarget::Record(record) => Box::new(OneRowCursor {
					row: read.get(snapshot, record)?,
				}),
			};
			Ok(Box::new(ScanCursor {
				inner,
			}))
		}
		LogicalNodeKind::Filter {
			input,
			predicate,
		} => Ok(Box::new(FilterCursor {
			input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
			predicate,
			slots,
			source,
			fields: None,
		})),
		LogicalNodeKind::Project {
			input,
			expressions,
		} => Ok(Box::new(ProjectCursor {
			input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
			expressions,
			slots,
			source,
			fields: None,
		})),
		LogicalNodeKind::Limit {
			input,
			limit,
			start,
		} => {
			let limit = limit.as_ref().map(|value| eval_usize(value, slots, source)).transpose()?;
			let start = start
				.as_ref()
				.map(|value| eval_usize(value, slots, source))
				.transpose()?
				.unwrap_or(0);
			Ok(Box::new(LimitCursor {
				input: build_cursor(read, snapshot, plan, *input, slots, source, scan_predicate)?,
				remaining: limit,
				skip: start,
			}))
		}
	}
}

struct ScanCursor<'a> {
	inner: Box<dyn RowCursor + 'a>,
}

impl RowCursor for ScanCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		self.inner.next()
	}
}

struct OneRowCursor {
	row: Option<Row>,
}

impl RowCursor for OneRowCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.row.take())
	}
}

/// Resolves public fields for an atom bound in a MATCH row.
trait AtomFieldRead {
	/// Reads `id`, `label`, or a dictionary-backed property.
	fn field(&self, atom: AtomId, field: &str) -> FerrosGrynnResult<Value>;
}

/// Atom field resolver sharing the current graph and property dictionary.
struct GraphFields<Graph> {
	/// Snapshot graph view.
	graph: Rc<Graph>,
	/// Snapshot property dictionary.
	dictionary: Arc<PropertyDictionary>,
}

impl<Graph: GraphRead> AtomFieldRead for GraphFields<Graph> {
	fn field(&self, atom: AtomId, field: &str) -> FerrosGrynnResult<Value> {
		match atom.kind() {
			Some(AtomKind::Node) => {
				let node = self.graph.node(atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("node", atom.to_string())
				})?;
				match field {
					"id" => Ok(Value::Record(Box::new(node.record_id()?.into_owned()))),
					"label" => Ok(node
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(node.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			Some(AtomKind::Edge) => {
				let edge = self.graph.edge(atom)?.ok_or_else(|| {
					FerrosGrynnError::atom_does_not_exist("edge", atom.to_string())
				})?;
				match field {
					"id" => Ok(Value::Record(Box::new(edge.record_id()?.into_owned()))),
					"label" => Ok(edge
						.label_text()?
						.map(|label| Value::String(label.into_owned()))
						.unwrap_or(Value::None)),
					_ => {
						let Some(key) = self.dictionary.get(field) else {
							return Ok(Value::None);
						};
						Ok(edge.properties()?.get(key).cloned().unwrap_or(Value::None))
					}
				}
			}
			None => Err(FerrosGrynnError::atom_does_not_exist("atom", atom.to_string())),
		}
	}
}

struct FilterCursor<'a> {
	input: Box<dyn RowCursor + 'a>,
	predicate: &'a TypedExpr,
	slots: &'a [Value],
	source: &'a str,
	/// Optional MATCH atom-field resolver.
	fields: Option<&'a dyn AtomFieldRead>,
}

impl RowCursor for FilterCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		while let Some(row) = self.input.next()? {
			if eval(self.predicate, Some(&row), self.slots, self.source, self.fields)?.is_truthy() {
				return Ok(Some(row));
			}
		}
		Ok(None)
	}
}

struct ProjectCursor<'a> {
	input: Box<dyn RowCursor + 'a>,
	expressions: &'a [TypedExpr],
	slots: &'a [Value],
	source: &'a str,
	/// Optional MATCH atom-field resolver.
	fields: Option<&'a dyn AtomFieldRead>,
}

impl RowCursor for ProjectCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		let Some(row) = self.input.next()? else {
			return Ok(None);
		};
		let values = self
			.expressions
			.iter()
			.map(|expression| eval(expression, Some(&row), self.slots, self.source, self.fields))
			.collect::<FerrosGrynnResult<Vec<_>>>()?;
		Ok(Some(Row::new(values)))
	}
}

struct LimitCursor<'a> {
	input: Box<dyn RowCursor + 'a>,
	remaining: Option<usize>,
	skip: usize,
}

impl RowCursor for LimitCursor<'_> {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		while self.skip > 0 {
			if self.input.next()?.is_none() {
				return Ok(None);
			}
			self.skip -= 1;
		}
		if self.remaining == Some(0) {
			return Ok(None);
		}
		let row = self.input.next()?;
		if row.is_some()
			&& let Some(remaining) = &mut self.remaining
		{
			*remaining -= 1;
		}
		Ok(row)
	}
}

fn eval(
	expr: &TypedExpr,
	row: Option<&Row>,
	slots: &[Value],
	source: &str,
	fields: Option<&dyn AtomFieldRead>,
) -> FerrosGrynnResult<Value> {
	let result = match &expr.kind {
		TypedExprKind::Constant(value) => Ok(value.clone()),
		TypedExprKind::Slot(slot) => Ok(slots[*slot as usize].clone()),
		TypedExprKind::Column(column) => {
			row.and_then(|row| row.values.get(*column)).cloned().ok_or_else(|| {
				FerrosGrynnError::invalid_argument("Column is missing from the input row.")
			})
		}
		TypedExprKind::Array(values) => values
			.iter()
			.map(|value| eval(value, row, slots, source, fields))
			.collect::<FerrosGrynnResult<Array>>()
			.map(Value::Array),
		TypedExprKind::Object(object_fields) => object_fields
			.iter()
			.map(|(name, value)| {
				eval(value, row, slots, source, fields).map(|value| (name.as_ref(), value))
			})
			.collect::<FerrosGrynnResult<Object>>()
			.map(Value::Object),
		TypedExprKind::Unary {
			op,
			expr,
		} => unary(*op, eval(expr, row, slots, source, fields)?),
		TypedExprKind::Binary {
			left,
			op,
			right,
		} => {
			let left = eval(left, row, slots, source, fields)?;
			match op {
				BinaryOperator::And if !left.is_truthy() => Ok(Value::Bool(false)),
				BinaryOperator::Or if left.is_truthy() => Ok(Value::Bool(true)),
				BinaryOperator::Coalesce if !matches!(left, Value::None | Value::Null) => Ok(left),
				BinaryOperator::Elvis if left.is_truthy() => Ok(left),
				_ => binary(*op, left, eval(right, row, slots, source, fields)?),
			}
		}
		TypedExprKind::Conditional {
			condition,
			then_value,
			else_value,
		} => {
			if eval(condition, row, slots, source, fields)?.is_truthy() {
				eval(then_value, row, slots, source, fields)
			} else {
				eval(else_value, row, slots, source, fields)
			}
		}
		TypedExprKind::Field {
			base,
			field,
			null_safe,
		} => {
			if let TypedExprKind::Column(column) = base.kind
				&& let Some(atom) = row.and_then(|row| row.atoms.get(column)).and_then(|atom| *atom)
				&& let Some(fields) = fields
			{
				return fields
					.field(atom, field)
					.map_err(|error| locate_execution(error, expr, source));
			}
			let base = eval(base, row, slots, source, fields)?;
			match base {
				Value::Object(object) => Ok(object.get(field).cloned().unwrap_or(Value::None)),
				Value::None | Value::Null if *null_safe => Ok(Value::None),
				Value::None | Value::Null => Ok(Value::None),
				value => Err(FerrosGrynnError::invalid_argument(format!(
					"Cannot access field `{field}` on {}.",
					value.kind()
				))),
			}
		}
		TypedExprKind::Index {
			base,
			index,
		} => index_value(
			eval(base, row, slots, source, fields)?,
			eval(index, row, slots, source, fields)?,
		),
		TypedExprKind::Cast {
			target,
			expr,
		} => cast(eval(expr, row, slots, source, fields)?, target),
	};
	result.map_err(|error| locate_execution(error, expr, source))
}

fn unary(operator: UnaryOperator, value: Value) -> FerrosGrynnResult<Value> {
	match operator {
		UnaryOperator::Not => Ok(Value::Bool(!value.is_truthy())),
		UnaryOperator::Assert if matches!(value, Value::None | Value::Null) => {
			Err(FerrosGrynnError::invalid_argument("Assertion operator received none or null."))
		}
		UnaryOperator::Assert | UnaryOperator::Positive => Ok(value),
		UnaryOperator::Negate => match value {
			Value::Number(Number::Int(value)) => value
				.checked_neg()
				.map(Value::from)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("unary -")),
			Value::Number(Number::Float(value)) => Ok(Value::from(-value)),
			Value::Number(Number::Decimal(value)) => Decimal::ZERO
				.checked_sub(value)
				.map(|value| Value::Number(Number::Decimal(value)))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("unary -")),
			value => Err(FerrosGrynnError::invalid_argument(format!(
				"Unary minus requires a number, found {}.",
				value.kind()
			))),
		},
	}
}

fn binary(operator: BinaryOperator, left: Value, right: Value) -> FerrosGrynnResult<Value> {
	match operator {
		BinaryOperator::Power
		| BinaryOperator::Multiply
		| BinaryOperator::Divide
		| BinaryOperator::Remainder
		| BinaryOperator::Add
		| BinaryOperator::Subtract => arithmetic(operator, left, right),
		BinaryOperator::Less => Ok(Value::Bool(left < right)),
		BinaryOperator::LessOrEqual => Ok(Value::Bool(left <= right)),
		BinaryOperator::Greater => Ok(Value::Bool(left > right)),
		BinaryOperator::GreaterOrEqual => Ok(Value::Bool(left >= right)),
		BinaryOperator::Equal | BinaryOperator::Is => Ok(Value::Bool(left == right)),
		BinaryOperator::NotEqual | BinaryOperator::NotIs => Ok(Value::Bool(left != right)),
		BinaryOperator::In | BinaryOperator::NotIn => {
			let contains =
				matches!(&right, Value::Array(values) if values.iter().any(|value| value == &left));
			Ok(Value::Bool(if matches!(operator, BinaryOperator::NotIn) {
				!contains
			} else {
				contains
			}))
		}
		BinaryOperator::And => Ok(Value::Bool(left.is_truthy() && right.is_truthy())),
		BinaryOperator::Or => Ok(Value::Bool(left.is_truthy() || right.is_truthy())),
		BinaryOperator::Coalesce => Ok(if matches!(left, Value::None | Value::Null) {
			right
		} else {
			left
		}),
		BinaryOperator::Elvis => Ok(if left.is_truthy() {
			left
		} else {
			right
		}),
		BinaryOperator::Pipeline => {
			Err(FerrosGrynnError::query_feature_not_supported("expression pipeline"))
		}
	}
}

fn arithmetic(operator: BinaryOperator, left: Value, right: Value) -> FerrosGrynnResult<Value> {
	let (Value::Number(left), Value::Number(right)) = (left, right) else {
		return Err(FerrosGrynnError::invalid_argument("Arithmetic operators require numbers."));
	};
	match (left, right) {
		(Number::Int(left), Number::Int(right)) => integer_arithmetic(operator, left, right),
		(Number::Float(left), Number::Float(right)) => float_arithmetic(operator, left, right),
		(Number::Float(left), Number::Int(right)) => float_arithmetic(operator, left, right as f64),
		(Number::Int(left), Number::Float(right)) => float_arithmetic(operator, left as f64, right),
		(Number::Decimal(left), Number::Decimal(right)) => {
			decimal_arithmetic(operator, left, right)
		}
		(Number::Decimal(left), Number::Int(right)) => {
			decimal_arithmetic(operator, left, Decimal::from(right))
		}
		(Number::Int(left), Number::Decimal(right)) => {
			decimal_arithmetic(operator, Decimal::from(left), right)
		}
		(Number::Float(_), Number::Decimal(_)) | (Number::Decimal(_), Number::Float(_)) => {
			Err(FerrosGrynnError::invalid_argument(
				"Float and decimal arithmetic requires an explicit cast.",
			))
		}
	}
}

fn integer_arithmetic(operator: BinaryOperator, left: i64, right: i64) -> FerrosGrynnResult<Value> {
	let value = match operator {
		BinaryOperator::Add => add_exact(&left, &right)?,
		BinaryOperator::Subtract => subtract_exact(&left, &right)?,
		BinaryOperator::Multiply => multiply_exact(&left, &right)?,
		BinaryOperator::Divide => divide_signed_exact(&left, &right)?,
		BinaryOperator::Remainder if right == 0 => return Err(FerrosGrynnError::division_by_zero()),
		BinaryOperator::Remainder => {
			left.checked_rem(right).ok_or_else(|| FerrosGrynnError::operation_overflow("%"))?
		}
		BinaryOperator::Power => {
			let exponent = u32::try_from(right).map_err(|_conversion_error| {
				FerrosGrynnError::invalid_argument("Integer exponent must be non-negative.")
			})?;
			left.checked_pow(exponent).ok_or_else(|| FerrosGrynnError::operation_overflow("**"))?
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	};
	Ok(Value::from(value))
}

fn float_arithmetic(operator: BinaryOperator, left: f64, right: f64) -> FerrosGrynnResult<Value> {
	if matches!(operator, BinaryOperator::Divide | BinaryOperator::Remainder) && right == 0.0 {
		return Err(FerrosGrynnError::division_by_zero());
	}
	let value = match operator {
		BinaryOperator::Add => left + right,
		BinaryOperator::Subtract => left - right,
		BinaryOperator::Multiply => left * right,
		BinaryOperator::Divide => left / right,
		BinaryOperator::Remainder => left % right,
		BinaryOperator::Power => left.powf(right),
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	};
	if value.is_infinite() {
		return Err(FerrosGrynnError::operation_overflow(operator_name(operator)));
	}
	Ok(Value::from(value))
}

fn decimal_arithmetic(
	operator: BinaryOperator,
	left: Decimal,
	right: Decimal,
) -> FerrosGrynnResult<Value> {
	if matches!(operator, BinaryOperator::Divide | BinaryOperator::Remainder) && right.is_zero() {
		return Err(FerrosGrynnError::division_by_zero());
	}
	let value = match operator {
		BinaryOperator::Add => left.checked_add(right),
		BinaryOperator::Subtract => left.checked_sub(right),
		BinaryOperator::Multiply => left.checked_mul(right),
		BinaryOperator::Divide => left.checked_div(right),
		BinaryOperator::Remainder => left.checked_rem(right),
		BinaryOperator::Power => {
			return Err(FerrosGrynnError::query_feature_not_supported("decimal exponentiation"));
		}
		_ => return Err(FerrosGrynnError::invalid_argument("Invalid arithmetic operator.")),
	}
	.ok_or_else(|| FerrosGrynnError::operation_overflow(operator_name(operator)))?;
	Ok(Value::Number(Number::Decimal(value)))
}

const fn operator_name(operator: BinaryOperator) -> &'static str {
	match operator {
		BinaryOperator::Power => "**",
		BinaryOperator::Multiply => "*",
		BinaryOperator::Divide => "/",
		BinaryOperator::Remainder => "%",
		BinaryOperator::Add => "+",
		BinaryOperator::Subtract => "-",
		_ => "?",
	}
}

fn index_value(base: Value, index: Value) -> FerrosGrynnResult<Value> {
	match (base, index) {
		(Value::Array(values), Value::Number(number)) => usize::try_from(number)
			.ok()
			.and_then(|index| values.get(index).cloned())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("Array index is out of bounds.")),
		(Value::Object(values), Value::String(key)) => {
			Ok(values.get(key.as_str()).cloned().unwrap_or(Value::None))
		}
		(value, _) => {
			Err(FerrosGrynnError::invalid_argument(format!("Cannot index {}.", value.kind())))
		}
	}
}

fn cast(value: Value, target: &Type) -> FerrosGrynnResult<Value> {
	match target {
		Type::Any => Ok(value),
		Type::Bool => Ok(Value::Bool(value.is_truthy())),
		Type::String => Ok(Value::from(value.to_string())),
		Type::Number => match value {
			Value::Number(_) => Ok(value),
			Value::String(value) => value.parse::<Number>().map(Value::Number),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "number")),
		},
		Type::Int => match value {
			Value::Number(value) => i64::try_from(value).map(Value::from),
			Value::String(value) => value
				.parse::<i64>()
				.map(Value::from)
				.map_err(|_parse_error| FerrosGrynnError::conversion_error(value.as_str(), "int")),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "int")),
		},
		Type::Float => match value {
			Value::Number(value) => f64::try_from(value).map(Value::from),
			Value::String(value) => value.parse::<f64>().map(Value::from).map_err(|_parse_error| {
				FerrosGrynnError::conversion_error(value.as_str(), "float")
			}),
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "float")),
		},
		Type::Decimal => match value {
			Value::Number(value) => {
				Decimal::try_from(value).map(|value| Value::Number(Number::Decimal(value)))
			}
			Value::String(value) => {
				value.parse::<Decimal>().map(|value| Value::Number(Number::Decimal(value))).map_err(
					|_parse_error| FerrosGrynnError::conversion_error(value.as_str(), "decimal"),
				)
			}
			_ => Err(FerrosGrynnError::conversion_error(value.to_string(), "decimal")),
		},
		Type::None if matches!(value, Value::None) => Ok(value),
		Type::Null if matches!(value, Value::Null) => Ok(value),
		_ => Err(FerrosGrynnError::conversion_error(value.to_string(), target.to_string())),
	}
}

fn eval_usize(expr: &TypedExpr, slots: &[Value], source: &str) -> FerrosGrynnResult<usize> {
	let value = eval(expr, None, slots, source, None)?;
	let Value::Number(number) = value else {
		return Err(locate_execution(
			FerrosGrynnError::invalid_argument("Expected a non-negative integer."),
			expr,
			source,
		));
	};
	usize::try_from(number).map_err(|error| locate_execution(error, expr, source))
}

fn locate_execution(error: FerrosGrynnError, expr: &TypedExpr, source: &str) -> FerrosGrynnError {
	let error = error.with_phase(ExecutionPhase::Execution);
	Location::from_source(None, source, expr.span)
		.map_or(error.clone(), |location| error.with_location(location))
}

fn execution_phase(error: FerrosGrynnError) -> FerrosGrynnError {
	error.with_phase(ExecutionPhase::Execution)
}

#[cfg(test)]
struct VecCursor {
	rows: std::vec::IntoIter<Row>,
}

#[cfg(test)]
impl VecCursor {
	fn new(rows: Vec<Row>) -> Self {
		Self {
			rows: rows.into_iter(),
		}
	}
}

#[cfg(test)]
impl RowCursor for VecCursor {
	fn next(&mut self) -> FerrosGrynnResult<Option<Row>> {
		Ok(self.rows.next())
	}
}

#[cfg(test)]
mod tests {
	use std::{error::Error, io};

	use super::*;
	use crate::{
		datastore::{
			Datastore,
			GraphWriteView,
			IsolationLevel,
			TransactionAccess,
			TransactionOptions,
		},
		gql::ir::{Column, Schema},
		gql_status::ExecutionPhase,
		graph::{EdgeTopology, Graph, Mutable},
		values::{PropertyMap, RecordIdPart},
	};

	struct TestStore {
		schema: Schema,
		rows: Vec<(RecordId, Row)>,
		fail_scan: bool,
	}

	impl TestStore {
		fn people() -> Self {
			Self {
				schema: Schema {
					columns: vec![
						Column {
							name: "name".into(),
							ty: Type::String,
						},
						Column {
							name: "age".into(),
							ty: Type::Int,
						},
					],
				},
				rows: vec![
					person("alice", "Alice", 30),
					person("bob", "Bob", 20),
					person("carol", "Carol", 17),
				],
				fail_scan: false,
			}
		}
	}

	impl QueryCatalog for TestStore {
		fn schema(&self, table: &str) -> FerrosGrynnResult<Schema> {
			if table.eq_ignore_ascii_case("person") {
				Ok(self.schema.clone())
			} else {
				Err(FerrosGrynnError::invalid_argument(format!("Unknown table `{table}`.")))
			}
		}

		fn graph(&self, name: &str) -> FerrosGrynnResult<GraphRef> {
			Err(FerrosGrynnError::graph_does_not_exist(name))
		}
	}

	impl QueryRead for TestStore {
		type Graph<'a> = crate::values::MaterializedGraph;

		fn snapshot(&self) -> FerrosGrynnResult<Snapshot> {
			Ok(Snapshot(7))
		}

		fn catalog(&self, snapshot: Snapshot) -> &dyn QueryCatalog {
			assert_eq!(snapshot, Snapshot(7));
			self
		}

		fn scan<'a>(
			&'a self,
			snapshot: Snapshot,
			table: &str,
			_predicate: Option<&TypedExpr>,
			_slots: &[Value],
			_source: &str,
		) -> FerrosGrynnResult<Box<dyn RowCursor + 'a>> {
			assert_eq!(snapshot, Snapshot(7));
			assert_eq!(table, "person");
			if self.fail_scan {
				return Err(FerrosGrynnError::unexpected_external_failure("test-store", "scan")
					.with_technical_source(io::Error::other("secret storage detail")));
			}
			Ok(Box::new(VecCursor::new(self.rows.iter().map(|(_, row)| row.clone()).collect())))
		}

		fn get(&self, snapshot: Snapshot, record: &RecordId) -> FerrosGrynnResult<Option<Row>> {
			assert_eq!(snapshot, Snapshot(7));
			Ok(self
				.rows
				.iter()
				.find(|(candidate, _)| candidate == record)
				.map(|(_, row)| row.clone()))
		}

		fn graph<'a>(
			&'a self,
			_snapshot: Snapshot,
			reference: &GraphRef,
		) -> FerrosGrynnResult<Self::Graph<'a>> {
			Err(FerrosGrynnError::graph_does_not_exist(reference.id().to_string()))
		}

		fn property_dictionary(&self, _snapshot: Snapshot) -> Arc<PropertyDictionary> {
			Arc::new(PropertyDictionary::new())
		}
	}

	fn person(id: &str, name: &str, age: i64) -> (RecordId, Row) {
		(
			RecordId::new("person", RecordIdPart::String(id.into())).expect("valid record ID"),
			Row::new(vec![Value::from(name), Value::from(age)]),
		)
	}

	fn match_store() -> Datastore {
		let store = Datastore::in_memory();
		let name = store.intern_property("name").expect("property key");
		let mut graph = Graph::<Mutable>::new();
		let mut alice_properties = PropertyMap::new();
		alice_properties.insert(name, "Alice");
		let alice = graph
			.add_node_with_id(
				"person:alice".parse().expect("record"),
				Some("Person".into()),
				alice_properties,
			)
			.expect("alice");
		let mut bob_properties = PropertyMap::new();
		bob_properties.insert(name, "Bob");
		let bob = graph
			.add_node_with_id(
				"person:bob".parse().expect("record"),
				Some("Person".into()),
				bob_properties,
			)
			.expect("bob");
		let carol = graph
			.add_node_with_id(
				"person:carol".parse().expect("record"),
				Some("Person".into()),
				PropertyMap::new(),
			)
			.expect("carol");
		let topic = graph
			.add_node_with_id(
				"topic:rust".parse().expect("record"),
				Some("Topic".into()),
				PropertyMap::new(),
			)
			.expect("topic");
		let first = graph
			.add_edge_with_id(
				"knows:first".parse().expect("record"),
				EdgeTopology::directed([alice], [bob]),
				Some("KNOWS".into()),
				PropertyMap::new(),
			)
			.expect("first edge");
		let second = graph
			.add_edge_with_id(
				"knows:second".parse().expect("record"),
				EdgeTopology::directed([bob], [carol]),
				Some("KNOWS".into()),
				PropertyMap::new(),
			)
			.expect("second edge");
		graph
			.add_edge_with_id(
				"annotation:first".parse().expect("record"),
				EdgeTopology::directed([alice], [first]),
				Some("ANNOTATES".into()),
				PropertyMap::new(),
			)
			.expect("V-E edge");
		graph
			.add_edge_with_id(
				"cause:first".parse().expect("record"),
				EdgeTopology::directed([first], [second]),
				Some("CAUSES".into()),
				PropertyMap::new(),
			)
			.expect("E-E edge");
		graph
			.add_edge_with_id(
				"group:team".parse().expect("record"),
				EdgeTopology::directed([alice, bob], [carol, topic]),
				Some("GROUP".into()),
				PropertyMap::new(),
			)
			.expect("hyperedge");
		let mut create = store.begin();
		create
			.create_named_graph(
				"social",
				"graph:internal".parse().expect("graph record"),
				graph.freeze().into(),
			)
			.expect("named graph");
		create.commit().expect("commit");
		store
	}

	fn run(store: &TestStore, source: &str) -> FerrosGrynnResult<Vec<QueryResult>> {
		execute_text(store, source, &Object::new(), ParserSettings::default())
	}

	#[test]
	fn executes_return_with_pratt_precedence() {
		let results = run(&TestStore::people(), "RETURN 1 + 2 * 3").expect("query succeeds");
		assert_eq!(
			results[0].result.as_ref().expect("statement succeeds").value(),
			&Value::from(7_i64)
		);
	}

	#[test]
	fn binds_let_filters_projects_and_limits() {
		let results = run(
			&TestStore::people(),
			"LET $min = 18; SELECT name FROM person WHERE age >= $min LIMIT 2",
		)
		.expect("query succeeds");
		assert_eq!(results[0].result.as_ref().expect("LET succeeds").value(), &Value::None);
		let Value::Array(rows) = results[1].result.as_ref().expect("SELECT succeeds").value()
		else {
			panic!("expected an array result");
		};
		assert_eq!(rows.len(), 2);
		assert_eq!(
			rows.get(0).and_then(Value::as_object).and_then(|row| row.get("name")),
			Some(&Value::from("Alice"))
		);
		assert_eq!(
			rows.get(1).and_then(Value::as_object).and_then(|row| row.get("name")),
			Some(&Value::from("Bob"))
		);
	}

	#[test]
	fn gets_record_and_materializes_select_value() {
		let results = run(&TestStore::people(), "SELECT VALUE name FROM person:alice")
			.expect("query succeeds");
		let Value::Array(values) = results[0].result.as_ref().expect("SELECT succeeds").value()
		else {
			panic!("expected an array result");
		};
		assert_eq!(values.as_slice(), &[Value::from("Alice")]);
	}

	#[test]
	fn rejects_unknown_field_during_analysis() {
		let error = run(&TestStore::people(), "SELECT missing FROM person")
			.expect_err("unknown field must fail before execution");
		assert_eq!(error.status().diagnostic().phase(), Some(ExecutionPhase::SemanticAnalysis));
	}

	#[test]
	fn keeps_none_and_null_distinct() {
		let results = run(
			&TestStore::people(),
			"RETURN none = null; RETURN [none, null]; SELECT VALUE name FROM person WHERE none = \
			 null; SELECT VALUE name FROM person WHERE none IS none LIMIT 1",
		)
		.expect("query succeeds");
		assert_eq!(
			results[0].result.as_ref().expect("comparison succeeds").value(),
			&Value::Bool(false)
		);
		let Value::Array(values) = results[1].result.as_ref().expect("array succeeds").value()
		else {
			panic!("expected array");
		};
		assert_eq!(values.as_slice(), &[Value::None, Value::Null]);
		let Value::Array(filtered_out) =
			results[2].result.as_ref().expect("false filter succeeds").value()
		else {
			panic!("expected array");
		};
		assert!(filtered_out.is_empty());
		let Value::Array(filtered_in) =
			results[3].result.as_ref().expect("true filter succeeds").value()
		else {
			panic!("expected array");
		};
		assert_eq!(filtered_in.as_slice(), &[Value::from("Alice")]);
	}

	#[test]
	fn preserves_domain_error_and_redacts_storage_source() {
		let mut store = TestStore::people();
		store.fail_scan = true;
		let results = run(&store, "SELECT name FROM person").expect("planning succeeds");
		let error = results[0].result.as_ref().expect_err("scan fails");
		assert_eq!(error.status().code().to_string(), "FG-50F00");
		assert!(error.technical_source().is_some());
		assert!(!format!("{error:?}").contains("secret storage detail"));
		assert!(Error::source(error).is_none());
	}

	#[test]
	fn executes_named_graph_standard_match_with_fields() {
		let store = Datastore::in_memory();
		let name = store.intern_property("name").expect("property key");
		let mut graph = Graph::<Mutable>::new();
		let mut alice_properties = PropertyMap::new();
		alice_properties.insert(name, "Alice");
		let alice = graph
			.add_node_with_id(
				"person:alice".parse().expect("record"),
				Some("Person".into()),
				alice_properties,
			)
			.expect("alice");
		let bob = graph
			.add_node_with_id(
				"person:bob".parse().expect("record"),
				Some("Person".into()),
				PropertyMap::new(),
			)
			.expect("bob");
		graph
			.add_edge_with_id(
				"knows:alice-bob".parse().expect("record"),
				EdgeTopology::directed([alice], [bob]),
				Some("KNOWS".into()),
				PropertyMap::new(),
			)
			.expect("edge");
		let mut create = store.begin();
		create
			.create_named_graph(
				"social",
				"graph:internal".parse().expect("graph record"),
				graph.freeze().into(),
			)
			.expect("named graph");
		create.commit().expect("commit");

		let results = store
			.execute(
				"MATCH (a:Person)-[e:KNOWS]->(b:Person) FROM social WHERE a.name = 'Alice' RETURN \
				 a.id AS source, b.id AS target, e.label AS kind, b.name AS missing",
				&Object::new(),
			)
			.expect("MATCH executes");
		let Value::Array(rows) = results[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 1);
		let row = rows.get(0).and_then(Value::as_object).expect("object row");
		assert_eq!(
			row.get("source"),
			Some(&Value::Record(Box::new("person:alice".parse().expect("record"))))
		);
		assert_eq!(row.get("kind"), Some(&Value::from("KNOWS")));
		assert_eq!(row.get("missing"), Some(&Value::None));
	}

	#[test]
	fn executes_atomwalk_hyper_pairs_shared_alias_and_cartesian_patterns() {
		let store = match_store();
		let atomwalk = store
			.execute(
				"MATCH ATOM <(v:Person)>~[link:ANNOTATES]~><[edge:KNOWS]> FROM social RETURN v.id \
				 AS v, edge.id AS edge; MATCH ATOM <[left:KNOWS]>~[cause:CAUSES]~><[right:KNOWS]> \
				 FROM social RETURN left.id AS left, right.id AS right",
				&Object::new(),
			)
			.expect("AtomWalk executes");
		for result in atomwalk {
			let Value::Array(rows) = result.result.expect("MATCH succeeds").into_parts().0 else {
				panic!("expected rows");
			};
			assert_eq!(rows.len(), 1);
		}

		let joined = store
			.execute(
				"MATCH (a:Person)-[:KNOWS]->(b:Person), (b)-[:KNOWS]->(c:Person) FROM social \
				 RETURN a.id AS a, c.id AS c",
				&Object::new(),
			)
			.expect("joined patterns execute");
		let Value::Array(rows) = joined[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 1);

		let cartesian = store
			.execute(
				"MATCH (topic:Topic), (person:Person) FROM social RETURN topic.id, person.id",
				&Object::new(),
			)
			.expect("cartesian patterns execute");
		let Value::Array(rows) = cartesian[0].result.as_ref().expect("MATCH succeeds").value()
		else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 3);

		let hyper = store
			.execute(
				"MATCH (source:Person)-[:GROUP]->(target) FROM social RETURN source.id, target.id",
				&Object::new(),
			)
			.expect("hyperedge pair expansion executes");
		let Value::Array(rows) = hyper[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 4);

		let distinct_connectors = store
			.execute(
				"MATCH (a:Person)-[:KNOWS]->(b:Person), (a)-[:KNOWS]->(b) FROM social RETURN \
				 a.id, b.id",
				&Object::new(),
			)
			.expect("edge-isomorphic patterns execute");
		let Value::Array(rows) =
			distinct_connectors[0].result.as_ref().expect("MATCH succeeds").value()
		else {
			panic!("expected rows");
		};
		assert!(rows.is_empty());
	}

	#[test]
	fn expands_finite_trail_paths_and_returns_public_path_values() {
		let store = match_store();
		let results = store
			.execute(
				"MATCH path = (a:Person)-[:KNOWS*0..2]->(b:Person) FROM social WHERE a.name = \
				 'Alice' RETURN path AS path, b.id AS target",
				&Object::new(),
			)
			.expect("path MATCH executes");
		let Value::Array(rows) = results[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 3);
		let lengths = rows
			.iter()
			.filter_map(Value::as_object)
			.filter_map(|row| row.get("path"))
			.map(|value| match value {
				Value::Path(path) => path.length(),
				_ => panic!("expected public path"),
			})
			.collect::<BTreeSet<_>>();
		assert_eq!(lengths, BTreeSet::from([0, 1, 2]));

		let backward = store
			.execute(
				"MATCH path = (c:Person)<-[:KNOWS*2]-(a:Person) FROM social RETURN a.id AS start, \
				 path",
				&Object::new(),
			)
			.expect("backward path executes");
		let Value::Array(rows) = backward[0].result.as_ref().expect("MATCH succeeds").value()
		else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 1);

		let mut loop_graph = Graph::<Mutable>::new();
		let node = loop_graph
			.add_node_with_id(
				"node:loop".parse().expect("record"),
				Some("Loop".into()),
				PropertyMap::new(),
			)
			.expect("loop node");
		loop_graph
			.add_edge_with_id(
				"edge:loop".parse().expect("record"),
				EdgeTopology::directed([node], [node]),
				Some("LOOP".into()),
				PropertyMap::new(),
			)
			.expect("loop edge");
		let mut create = store.begin();
		create
			.create_named_graph(
				"loops",
				"graph:loops".parse().expect("record"),
				loop_graph.freeze().into(),
			)
			.expect("loop graph");
		create.commit().expect("commit");
		let trail = store
			.execute(
				"MATCH path = (a:Loop)-[:LOOP*0..2]->(b:Loop) FROM loops RETURN path",
				&Object::new(),
			)
			.expect("TRAIL path executes");
		let Value::Array(rows) = trail[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 2, "the same connector edge cannot be reused");
	}

	#[test]
	fn rejects_match_semantic_errors_before_execution() {
		let store = match_store();
		for source in [
			"MATCH (a) FROM missing RETURN a",
			"MATCH (a) FROM Social RETURN a",
			"MATCH ATOM <(same)>, <[same]> FROM social RETURN same",
			"MATCH (a) FROM social RETURN unknown",
			"MATCH (a)-[:E*3..2]->(b) FROM social RETURN b",
			"MATCH (a)-[:E*1..31]->(b) FROM social RETURN b",
		] {
			let error = store.execute(source, &Object::new()).expect_err("semantic error expected");
			assert_eq!(
				error.status().diagnostic().phase(),
				Some(ExecutionPhase::SemanticAnalysis),
				"source: {source}"
			);
			assert!(error.status().diagnostic().primary_location().is_some());
		}
	}

	#[test]
	fn match_observes_transaction_private_graphs_and_one_read_committed_snapshot() {
		let store = Datastore::in_memory();
		let mut graph = Graph::<Mutable>::new();
		graph
			.add_node_with_id(
				"person:private".parse().expect("record"),
				Some("Person".into()),
				PropertyMap::new(),
			)
			.expect("node");
		let mut transaction = store.begin();
		let reference = transaction
			.create_named_graph(
				"private",
				"graph:private".parse().expect("record"),
				graph.freeze().into(),
			)
			.expect("private graph");
		let visible = transaction
			.execute("MATCH (n:Person) FROM private RETURN n.id", &Object::new())
			.expect("private graph is visible");
		let Value::Array(rows) = visible[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 1);
		transaction.remove_graph(&reference).expect("remove private graph");
		assert!(transaction.execute("MATCH (n) FROM private RETURN n", &Object::new()).is_err());

		let store = match_store();
		let read = store.begin_with(TransactionOptions {
			isolation: IsolationLevel::ReadCommitted,
			access: TransactionAccess::ReadOnly,
		});
		let snapshot = read.query_snapshot().expect("statement snapshot");
		let reference = read.query_named_graph(&snapshot, "social").expect("graph ref");
		let adapter = TransactionRead {
			transaction: &read,
			snapshot,
		};
		let mut concurrent = store.begin();
		if let GraphWriteView::Graph(graph) = concurrent.edit_graph(&reference).expect("edit graph")
		{
			graph
				.add_node_with_id(
					"person:late".parse().expect("record"),
					Some("Person".into()),
					PropertyMap::new(),
				)
				.expect("late node");
		}
		concurrent.commit().expect("concurrent commit");
		let results = execute_text(
			&adapter,
			"MATCH (n:Person) FROM social RETURN n.id",
			&Object::new(),
			ParserSettings::default(),
		)
		.expect("captured snapshot executes");
		let Value::Array(rows) = results[0].result.as_ref().expect("MATCH succeeds").value() else {
			panic!("expected rows");
		};
		assert_eq!(rows.len(), 3, "one MATCH must not refresh Read Committed mid-expression");
	}

	#[test]
	fn match_reads_participate_in_serializable_validation() {
		let store = match_store();
		let reader = store.begin();
		let snapshot = reader.query_snapshot().expect("snapshot");
		let reference = reader.query_named_graph(&snapshot, "social").expect("graph ref");
		reader
			.execute(
				"MATCH (a:Person)-[:KNOWS]->(b:Person) FROM social RETURN a.id, b.id",
				&Object::new(),
			)
			.expect("MATCH reads graph");
		let mut concurrent = store.begin();
		if let GraphWriteView::Graph(graph) = concurrent.edit_graph(&reference).expect("edit graph")
		{
			graph
				.add_node_with_id(
					"person:phantom".parse().expect("record"),
					Some("Person".into()),
					PropertyMap::new(),
				)
				.expect("phantom node");
		}
		concurrent.commit().expect("concurrent commit");
		assert!(reader.commit().is_err(), "registered MATCH reads must conflict");
	}
}
