//! Owned graph-result assembly shared by RETURN GRAPH and BUILD GRAPH.

use std::{
	collections::{BTreeMap, BTreeSet},
	mem::size_of,
};

mod cursor;
mod hierarchy;

use hierarchy::{materialize_hierarchy, resolve_root};

use super::{
	AtomId,
	AtomKind,
	BarrierAccount,
	ControlRuntime,
	EdgeEndpoint,
	ExecutionControl,
	FerrosGrynnError,
	FerrosGrynnResult,
	GraphRead,
	GraphRef,
	Instant,
	LogicalSelect,
	LogicalSelectSourceKind,
	Object,
	PropertyMap,
	QueryRead,
	RecordFields,
	RecordId,
	RootRead,
	Row,
	Snapshot,
	TypedExpr,
	Value,
	eval,
	eval_query_expression,
	eval_usize,
	resolve_match_graph,
	select,
	traversal,
	visit_match_with_environment,
};
use crate::{
	gql::ir::{
		LogicalBuildGraph,
		LogicalBuildGraphSource,
		LogicalGraphContract,
		LogicalReturnGraph,
		Schema,
	},
	graph::{Graph, GraphId, MaterializedGraph, Mutable, QueryMaterialize},
};

#[derive(Clone, Default)]
/// Graph selection state.
struct GraphSelection {
	/// Nodes associated with this graph selection.
	nodes: BTreeSet<crate::graph::AtomRef>,
	/// Edges associated with this graph selection.
	edges: BTreeSet<crate::graph::AtomRef>,
	/// Records associated with this graph selection.
	records: BTreeSet<RecordId>,
	/// All nodes associated with this graph selection.
	all_nodes: bool,
	/// All edges associated with this graph selection.
	all_edges: bool,
}

/// One statement-local, non-spilling owned graph builder.
pub(in crate::gql) struct GraphAssemblyAccumulator<'a> {
	/// Root hint associated with this graph assembly accumulator.
	root_hint: Option<GraphRef>,
	/// Graphs associated with this graph assembly accumulator.
	graphs: BTreeMap<GraphId, GraphRef>,
	/// Selections associated with this graph assembly accumulator.
	selections: BTreeMap<GraphId, GraphSelection>,
	/// Graphless nodes associated with this graph assembly accumulator.
	graphless_nodes: BTreeSet<RecordId>,
	/// Direct associated with this graph assembly accumulator.
	direct: Option<MaterializedGraph>,
	/// Root values associated with this graph assembly accumulator.
	root_values: BTreeSet<Value>,
	/// Node limit associated with this graph assembly accumulator.
	node_limit: u64,
	/// Edge limit associated with this graph assembly accumulator.
	edge_limit: u64,
	/// Memory associated with this graph assembly accumulator.
	memory: BarrierAccount,
	/// Control associated with this graph assembly accumulator.
	control: &'a ExecutionControl,
}

impl<'a> GraphAssemblyAccumulator<'a> {
	/// Uses the same source scope as an ordinary SELECT graph terminal.
	pub(in crate::gql) fn for_select(
		read: &RootRead<'_>,
		plan: &LogicalSelect,
		slots: &[Value],
		control: &'a ExecutionControl,
	) -> FerrosGrynnResult<Self> {
		let (_, root_hint) = select::bind_terminal_graph_scope(
			read,
			read.snapshot()?,
			plan,
			slots,
			"",
			control,
			None,
		)?;
		Ok(Self::new(root_hint, None, None, control))
	}

	/// Root evaluation keeps the generic transaction adapter inside execution.
	pub(in crate::gql) fn add_root_row(
		&mut self,
		read: &RootRead<'_>,
		plan: &LogicalSelect,
		value: Value,
		slots: &[Value],
	) -> FerrosGrynnResult<()> {
		self.add_projected_row(read, read.snapshot()?, plan, value, slots, "")
	}

	/// Completes the same family validation and closure on the supplied root.
	pub(in crate::gql) fn finish_root(
		self,
		read: &RootRead<'_>,
		contract: LogicalGraphContract,
	) -> FerrosGrynnResult<MaterializedGraph> {
		let graph = self.finish(read, read.snapshot()?, contract)?;
		read.observe_graph_result(&graph)?;
		Ok(graph)
	}

	/// Evaluates selectors over an ordinary projected row with copy admission.
	#[allow(clippy::too_many_arguments, reason = "one shared projected SELECT row invocation")]
	pub(super) fn add_projected_row<Read: QueryRead>(
		&mut self,
		read: &Read,
		snapshot: Snapshot,
		plan: &LogicalSelect,
		value: Value,
		slots: &[Value],
		source: &str,
	) -> FerrosGrynnResult<()> {
		let graph =
			plan.return_graph.as_ref().ok_or_else(|| invalid("SELECT graph terminal is absent"))?;
		self.reserve(
			value
				.estimated_heap_bytes()
				.checked_add(
					plan.result
						.columns
						.len()
						.checked_mul(size_of::<Value>())
						.ok_or_else(|| invalid("graph row size overflows"))?,
				)
				.and_then(|bytes| bytes.checked_add(size_of::<Row>()))
				.ok_or_else(|| invalid("graph row size overflows"))?,
		)?;
		self.record_source_row();
		let mut row = result_row(value, &plan.result, plan.value)?;
		row.graph = self.root_hint.clone();
		self.add_selectors(
			Some(&graph.nodes),
			Some(&graph.edges),
			graph.root.as_ref(),
			|expression| {
				let value = select::eval_row_expression(
					read,
					snapshot,
					expression,
					&row,
					slots,
					source,
					self.control,
				)?;
				match graph.bridge.as_deref() {
					Some(alias) => {
						super::bridge::recipes(read, snapshot, value, alias, self.control)
					}
					None => Ok(value),
				}
			},
		)
	}

	/// Creates a new `GraphAssemblyAccumulator<'a>` value.
	pub(super) fn new(
		root_hint: Option<GraphRef>,
		node_limit: Option<u64>,
		edge_limit: Option<u64>,
		control: &'a ExecutionControl,
	) -> Self {
		Self {
			root_hint,
			graphs: BTreeMap::new(),
			selections: BTreeMap::new(),
			graphless_nodes: BTreeSet::new(),
			direct: None,
			root_values: BTreeSet::new(),
			node_limit: node_limit.map_or_else(
				|| control.max_graph_nodes(),
				|limit| limit.min(control.max_graph_nodes()),
			),
			edge_limit: edge_limit.map_or_else(
				|| control.max_graph_edges(),
				|limit| limit.min(control.max_graph_edges()),
			),
			memory: control.barrier_account(),
			control,
		}
	}

	/// Returns the add root result.
	pub(super) fn add_root(&mut self, value: Value) -> FerrosGrynnResult<()> {
		if matches!(value, Value::None) {
			return Err(invalid("ROOT must resolve to one node occurrence"));
		}
		if !matches!(value, Value::None) && self.root_values.contains(&value) {
			self.control.record_graph_deduplicated_value();
		} else if !matches!(value, Value::None) {
			self.reserve(value.estimated_heap_bytes().saturating_add(size_of::<Value>()))?;
			self.root_values.insert(value);
		}
		Ok(())
	}

	/// Records source row.
	pub(super) fn record_source_row(&self) {
		self.control.record_graph_source_row();
	}

	/// Applies explicit selectors using the caller's ordinary row evaluator.
	pub(super) fn add_selectors(
		&mut self,
		nodes: Option<&TypedExpr>,
		edges: Option<&TypedExpr>,
		root: Option<&TypedExpr>,
		mut evaluate: impl FnMut(&TypedExpr) -> FerrosGrynnResult<Value>,
	) -> FerrosGrynnResult<()> {
		if let Some(nodes) = nodes {
			self.add_nodes(evaluate(nodes)?)?;
		}
		if let Some(edges) = edges {
			self.add_edges(evaluate(edges)?)?;
		}
		if let Some(root) = root {
			self.add_root(evaluate(root)?)?;
		}
		Ok(())
	}

	/// Returns the add nodes result.
	pub(super) fn add_nodes(&mut self, value: Value) -> FerrosGrynnResult<()> {
		self.add(value, Some(AtomKind::Node), false)
	}

	/// Returns the add edges result.
	pub(super) fn add_edges(&mut self, value: Value) -> FerrosGrynnResult<()> {
		self.add(value, Some(AtomKind::Edge), false)
	}

	/// Returns the add auto result.
	pub(super) fn add_auto(&mut self, value: Value) -> FerrosGrynnResult<()> {
		self.add(value, None, true)
	}

	/// Returns the add result.
	fn add(
		&mut self,
		value: Value,
		expected: Option<AtomKind>,
		auto_object: bool,
	) -> FerrosGrynnResult<()> {
		self.control.check()?;
		match value {
			Value::None => Ok(()),
			Value::Null => Err(invalid("null is not a graph assembly value")),
			Value::Array(values) => {
				for value in values {
					self.add(value, expected, auto_object)?;
				}
				Ok(())
			}
			Value::Set(values) => {
				for value in values {
					self.add(value, expected, auto_object)?;
				}
				Ok(())
			}
			Value::Object(values) if auto_object => {
				for (_name, value) in values {
					self.add(value, None, true)?;
				}
				Ok(())
			}
			Value::Record(record) => {
				if expected == Some(AtomKind::Edge) {
					return Err(invalid(
						"a database-global edge has no topology; select an edge occurrence",
					));
				}
				if self.root_hint.is_none() {
					self.insert_graphless(*record)?;
					Ok(())
				} else if expected.is_none() {
					self.add_record(
						*record,
						self.root_hint
							.clone()
							.ok_or_else(|| invalid("graph-scoped record has no graph context"))?,
					)
				} else {
					self.add_atom(
						crate::graph::AtomRef::new(expected.unwrap_or(AtomKind::Node), *record),
						None,
					)
				}
			}
			Value::Atom(atom) => {
				ensure_kind(atom.kind(), expected)?;
				self.add_atom(*atom, None)
			}
			Value::AtomOccurrence(occurrence) => {
				ensure_kind(occurrence.atom().kind(), expected)?;
				let (atom, graph) = occurrence.into_parts();
				self.add_atom(atom, Some(graph.as_ref().clone()))
			}
			Value::Path(path) => {
				match expected {
					Some(AtomKind::Node) => {
						for occurrence in path.nodes() {
							self.add_atom(
								occurrence.atom().clone(),
								Some(occurrence.graph().clone()),
							)?;
						}
					}
					Some(AtomKind::Edge) => {
						for occurrence in path
							.edges()
							.chain(path.atoms().filter(|atom| atom.atom().kind() == AtomKind::Edge))
						{
							self.add_atom(
								occurrence.atom().clone(),
								Some(occurrence.graph().clone()),
							)?;
						}
					}
					None => {
						for occurrence in path.atoms() {
							self.add_atom(
								occurrence.atom().clone(),
								Some(occurrence.graph().clone()),
							)?;
						}
					}
				}
				Ok(())
			}
			Value::GraphRef(graph) => {
				let graph = self.selection(graph.as_ref().clone())?;
				let selection = self
					.selections
					.get_mut(&graph)
					.ok_or_else(|| invalid("graph selection disappeared"))?;
				match expected {
					Some(AtomKind::Node) => selection.all_nodes = true,
					Some(AtomKind::Edge) => selection.all_edges = true,
					None => {
						selection.all_nodes = true;
						selection.all_edges = true;
					}
				}
				Ok(())
			}
			Value::Graph(graph) if expected.is_none() => {
				if self.direct.is_some() || !self.selections.is_empty() {
					return Err(invalid("graph assembly accepts one direct graph value"));
				}
				self.reserve(graph.estimated_heap_bytes())?;
				self.direct = Some(*graph);
				Ok(())
			}
			other => {
				Err(invalid(format!("{} is not valid in a graph assembly selector", other.kind())))
			}
		}
	}

	/// Returns the add atom result.
	fn add_atom(
		&mut self,
		atom: crate::graph::AtomRef,
		graph: Option<GraphRef>,
	) -> FerrosGrynnResult<()> {
		let Some(graph) = graph.or_else(|| self.root_hint.clone()) else {
			if atom.kind() == AtomKind::Edge {
				return Err(invalid(
					"a database-global edge has no topology; select an edge occurrence",
				));
			}
			self.insert_graphless(atom.id().clone())?;
			return Ok(());
		};
		let graph = self.selection(graph)?;
		let duplicate = self.selections.get(&graph).is_some_and(|selection| match atom.kind() {
			AtomKind::Node => selection.nodes.contains(&atom),
			AtomKind::Edge => selection.edges.contains(&atom),
		});
		if duplicate {
			self.control.record_graph_deduplicated_value();
			return Ok(());
		}
		self.reserve(
			Value::Atom(Box::new(atom.clone()))
				.estimated_heap_bytes()
				.saturating_add(size_of::<crate::graph::AtomRef>()),
		)?;
		let selection = self
			.selections
			.get_mut(&graph)
			.ok_or_else(|| invalid("graph selection disappeared"))?;
		match atom.kind() {
			AtomKind::Node => {
				selection.nodes.insert(atom);
				self.control.record_graph_selected_atom(AtomKind::Node);
			}
			AtomKind::Edge => {
				selection.edges.insert(atom);
				self.control.record_graph_selected_atom(AtomKind::Edge);
			}
		}
		Ok(())
	}

	/// Returns the add record result.
	fn add_record(&mut self, record: RecordId, graph: GraphRef) -> FerrosGrynnResult<()> {
		let graph = self.selection(graph)?;
		if self.selections.get(&graph).is_some_and(|selection| selection.records.contains(&record))
		{
			self.control.record_graph_deduplicated_value();
			return Ok(());
		}
		self.reserve(record.estimated_heap_bytes().saturating_add(size_of::<RecordId>()))?;
		self.selections
			.get_mut(&graph)
			.ok_or_else(|| invalid("graph selection disappeared"))?
			.records
			.insert(record);
		Ok(())
	}

	/// Returns the selection result.
	fn selection(&mut self, graph: GraphRef) -> FerrosGrynnResult<GraphId> {
		let id = graph.id();
		if !self.graphs.contains_key(&id) {
			self.reserve(
				Value::GraphRef(Box::new(graph.clone()))
					.estimated_heap_bytes()
					.saturating_add(size_of::<(GraphId, GraphRef)>()),
			)?;
			self.graphs.insert(id, graph);
		}
		if !self.selections.contains_key(&id) {
			self.reserve(size_of::<(GraphId, GraphSelection)>())?;
			self.selections.insert(id, GraphSelection::default());
		}
		Ok(id)
	}

	/// Inserts graphless.
	fn insert_graphless(&mut self, record: RecordId) -> FerrosGrynnResult<()> {
		if self.graphless_nodes.contains(&record) {
			self.control.record_graph_deduplicated_value();
		} else {
			self.reserve(record.estimated_heap_bytes().saturating_add(size_of::<RecordId>()))?;
			self.graphless_nodes.insert(record);
			self.control.record_graph_selected_atom(AtomKind::Node);
		}
		Ok(())
	}

	/// Returns the full graph reference result.
	fn full_graph_reference(&self) -> Option<GraphRef> {
		if !self.graphless_nodes.is_empty() || self.selections.len() != 1 {
			return None;
		}
		let (id, selection) = self.selections.first_key_value()?;
		if !selection.all_nodes
			|| !selection.all_edges
			|| !selection.nodes.is_empty()
			|| !selection.edges.is_empty()
			|| !selection.records.is_empty()
			|| self.root_hint.as_ref().is_some_and(|root| root.id() != *id)
		{
			return None;
		}
		self.graphs.get(id).cloned()
	}

	/// Reserves graph assembly accumulator<'a>.
	fn reserve(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		if self.memory.reserve(bytes) {
			return Ok(());
		}
		Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"graph assembly memory".to_owned(),
			self.control.barrier_memory_limit() as u64,
		))
	}

	/// Returns the enforce result memory result.
	fn enforce_result_memory(&mut self, graph: &MaterializedGraph) -> FerrosGrynnResult<()> {
		self.memory.release_all();
		self.reserve(graph.estimated_heap_bytes())
	}

	/// Finishes graph assembly accumulator<'a>.
	pub(super) fn finish<Read: QueryRead>(
		mut self,
		read: &Read,
		snapshot: Snapshot,
		contract: LogicalGraphContract,
	) -> FerrosGrynnResult<MaterializedGraph> {
		if let Some(graph) = self.direct.take() {
			if !self.selections.is_empty() || !self.graphless_nodes.is_empty() {
				return Err(invalid("a direct graph cannot be combined with scoped atoms"));
			}
			let graph = Graph::<Mutable>::query_from(&graph)?;
			let validation_started = Instant::now();
			let graph =
				finish_family(graph, contract, std::mem::take(&mut self.root_values), None)?;
			self.control.record_graph_validation(validation_started.elapsed());
			self.control.record_graph_assembly_result(graph.node_count(), graph.edge_count(), 0);
			self.enforce_result_memory(&graph)?;
			return Ok(graph);
		}
		if let Some(reference) = self.full_graph_reference() {
			let source = read.graph(snapshot, &reference)?;
			if let Some((graph, estimated_bytes)) =
				Graph::<Mutable>::query_flat_with_heap_estimate(&source)?
			{
				let nodes = graph.node_count();
				let edges = graph.edge_count();
				check_limit(nodes, self.node_limit, "nodes")?;
				check_limit(edges, self.edge_limit, "edges")?;
				self.control.record_graph_selected_counts(nodes, edges);
				let validation_started = Instant::now();
				let graph = finish_family(
					graph,
					contract,
					std::mem::take(&mut self.root_values),
					Some(reference.id()),
				)?;
				self.control.record_graph_validation(validation_started.elapsed());
				self.control.record_graph_assembly_result(nodes, edges, 0);
				if contract.family == Some(crate::graph::GraphKind::Graph) {
					self.memory.release_all();
					self.reserve(estimated_bytes)?;
				} else {
					self.enforce_result_memory(&graph)?;
				}
				return Ok(graph);
			}
		}
		let mut root_scope = None;
		let mut graph = if self.selections.is_empty() {
			Graph::<Mutable>::begin_query_materialization(
				PropertyMap::new(),
				self.graphless_nodes.len(),
				0,
			)
		} else {
			let root =
				resolve_root(read, snapshot, self.root_hint.as_ref(), &self.graphs, self.control)?;
			root_scope = Some(root.id());
			materialize_hierarchy(
				read,
				snapshot,
				&root,
				&self.graphs,
				&mut self.selections,
				self.node_limit,
				self.edge_limit,
				&mut self.memory,
				self.control,
			)?
		};
		let mut global_source_atoms = BTreeMap::new();
		for record in std::mem::take(&mut self.graphless_nodes) {
			self.control.check()?;
			let Some((kind, properties)) = read.base_atom(snapshot, &record)? else {
				return Err(FerrosGrynnError::atom_does_not_exist("global", record.to_string()));
			};
			if kind != AtomKind::Node {
				return Err(invalid(
					"a database-global edge has no topology; select an edge occurrence",
				));
			}
			self.reserve(
				record
					.estimated_heap_bytes()
					.saturating_add(properties.estimated_heap_bytes())
					.saturating_add(size_of::<crate::graph::AtomId>() * 8),
			)?;
			let atom = crate::graph::AtomRef::new(kind, record.clone());
			if graph.atom_id(&atom)?.is_none() {
				let local = graph.materialize_query_node(record, None, properties, None)?;
				if root_scope.is_none() {
					self.reserve(128 + size_of::<(crate::graph::AtomId, crate::graph::AtomRef)>())?;
					global_source_atoms.insert(local, atom);
				}
			}
		}
		if root_scope.is_none() {
			graph.finish_query_materialization_parts(
				graph.identity(),
				crate::graph::SnapshotId::new(snapshot.0, snapshot.0),
				None,
				global_source_atoms,
			);
		}
		check_limit(graph.node_count(), self.node_limit, "nodes")?;
		check_limit(graph.edge_count(), self.edge_limit, "edges")?;
		let validation_started = Instant::now();
		let graph =
			finish_family(graph, contract, std::mem::take(&mut self.root_values), root_scope)?;
		self.control.record_graph_validation(validation_started.elapsed());
		if root_scope.is_none() {
			self.control.record_graph_assembly_result(graph.node_count(), graph.edge_count(), 0);
		}
		self.enforce_result_memory(&graph)?;
		Ok(graph)
	}
}

/// Executes return graph.
pub(super) fn execute_return_graph<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalReturnGraph,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let mut assembly = GraphAssemblyAccumulator::new(None, None, None, control);
	assembly.add_selectors(
		Some(&plan.nodes),
		Some(&plan.edges),
		plan.root.as_ref(),
		|expression| {
			let value = eval(expression, None, slots, source, None, Some(control))?;
			match plan.bridge.as_deref() {
				Some(alias) => super::bridge::recipes(read, snapshot, value, alias, control),
				None => Ok(value),
			}
		},
	)?;
	assembly.finish(read, snapshot, plan.contract).map(|graph| Value::Graph(Box::new(graph)))
}

/// Uses the ordinary SELECT projection/window before the shared graph assembly.
#[allow(
	clippy::too_many_arguments,
	reason = "graph terminal retains the ordinary SELECT invocation"
)]
pub(super) fn execute_select_graph<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	retain_cursor: bool,
) -> FerrosGrynnResult<Value> {
	if retain_cursor {
		match cursor::resume(read, snapshot, plan, slots, source, control) {
			Ok(Some(value)) => return Ok(value),
			Ok(None) => {}
			Err(error) => {
				control.discard_continuation();
				return Err(error);
			}
		}
	}
	let result = execute_select_graph_from_root(
		read,
		snapshot,
		plan,
		slots,
		source,
		control,
		outer,
		retain_cursor,
		None,
	);
	if result.is_err() && retain_cursor {
		control.discard_continuation();
	}
	result
}

/// One fixed root owns row selection, selector evaluation and endpoint closure.
#[allow(clippy::too_many_arguments, reason = "one direct or embedded SELECT graph invocation")]
fn execute_select_graph_from_root<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	template: &LogicalSelect,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
	outer: Option<&Row>,
	retain_cursor: bool,
	captured_page: Option<Vec<Value>>,
) -> FerrosGrynnResult<Value> {
	let plan = template;
	let graph =
		plan.return_graph.as_ref().ok_or_else(|| invalid("SELECT graph terminal is absent"))?;
	if let Some(alias) = &graph.bridge {
		super::bridge::resolve(read, snapshot, alias, control, false)?;
	}
	let (bound, root_hint) =
		select::bind_terminal_graph_scope(read, snapshot, plan, slots, source, control, outer)?;
	let plan = bound.as_ref();
	let mut assembly = GraphAssemblyAccumulator::new(root_hint, None, None, control);
	let mut consume =
		|value| assembly.add_projected_row(read, snapshot, plan, value, slots, source);
	if let Some(values) = captured_page {
		for value in values {
			consume(value)?;
		}
	} else if retain_cursor {
		let values =
			select::execute_select_projected_page(read, snapshot, plan, slots, source, control)?;
		for value in values {
			consume(value)?;
		}
	} else {
		select::visit_select_projected_values(
			read,
			snapshot,
			plan,
			slots,
			source,
			control,
			outer,
			None,
			&mut consume,
		)?;
	}
	let graph = assembly.finish(read, snapshot, graph.contract)?;
	if retain_cursor {
		cursor::capture(read, snapshot, template, plan, slots, source, control)?;
	}
	Ok(Value::Graph(Box::new(graph)))
}

#[allow(clippy::too_many_arguments, reason = "BUILD GRAPH reuses every existing query executor")]
/// Executes build graph.
pub(super) fn execute_build_graph<Read: QueryRead>(
	read: &mut Read,
	snapshot: Snapshot,
	plan: &LogicalBuildGraph,
	slots: &mut [Value],
	source: &str,
	control: &ExecutionControl,
	runtime: &mut ControlRuntime,
) -> FerrosGrynnResult<Value> {
	let existing = plan
		.name
		.as_deref()
		.map(|name| read.named_graph_contract(snapshot, name))
		.transpose()?
		.flatten();
	if plan.mode == crate::syn::DefinitionMode::IfNotExists
		&& let Some((reference, _constraints)) = existing
	{
		return Ok(Value::GraphRef(Box::new(reference)));
	}
	let mut contract = plan.contract;
	if let Some((reference, constraints)) = &existing {
		if let Some(family) = contract.family
			&& family != reference.kind()
		{
			return Err(invalid("OVERWRITE graph family must match the existing graph"));
		}
		contract.family = Some(reference.kind());
		if !plan.contract_explicit {
			contract.validators = *constraints;
		}
	} else if plan.name.is_some() && contract.family.is_none() {
		contract.family = Some(crate::graph::GraphKind::Graph);
	}
	let root_hint = match &plan.source {
		LogicalBuildGraphSource::Match(statement) => {
			Some(resolve_match_graph(read, snapshot, statement, slots, source, control)?)
		}
		LogicalBuildGraphSource::Select(statement) => match &statement.source.kind {
			LogicalSelectSourceKind::Atoms {
				graph: Some(reference),
				..
			}
			| LogicalSelectSourceKind::AtomUnion {
				graph: Some(reference),
				..
			} => Some(select::resolve_graph_reference(
				read, snapshot, reference, None, slots, source, control,
			)?),
			LogicalSelectSourceKind::Atoms {
				graph: None,
				..
			}
			| LogicalSelectSourceKind::Graphs {
				..
			}
			| LogicalSelectSourceKind::Empty {
				..
			}
			| LogicalSelectSourceKind::AtomUnion {
				graph: None,
				..
			}
			| LogicalSelectSourceKind::Derived(_) => None,
		},
		LogicalBuildGraphSource::Traverse(statement) => statement
			.graph
			.as_ref()
			.map(|reference| {
				select::resolve_graph_reference(
					read, snapshot, reference, None, slots, source, control,
				)
			})
			.transpose()?,
		LogicalBuildGraphSource::Expression(_) => None,
	};
	let mut assembly =
		GraphAssemblyAccumulator::new(root_hint, plan.limit_nodes, plan.limit_edges, control);
	let recipe_root = if plan.bridge.is_some() {
		read.bridge_recipe_reads(snapshot)?;
		Some(read.snapshot_root(snapshot)?)
	} else {
		None
	};
	let observe_recipe = |_key| {};
	let recipe_work = |_amount| control.check();
	let recipe_read = recipe_root.as_ref().map(|root| {
		RootRead::new(root, control, &observe_recipe, &recipe_work, read.query_scratch_root())
	});
	match &plan.source {
		LogicalBuildGraphSource::Expression(expression) => {
			assembly.record_source_row();
			let value = eval_query_expression(read, expression, slots, source, control, runtime)?;
			let value = match plan.bridge.as_deref() {
				Some(alias) => super::bridge::recipes(read, snapshot, value, alias, control)?,
				None => value,
			};
			assembly.add_auto(value)?;
			if let Some(root) = &plan.root {
				assembly.add_root(eval_query_expression(
					read, root, slots, source, control, runtime,
				)?)?;
			}
		}
		LogicalBuildGraphSource::Match(statement) => {
			let mut consume = |value| {
				consume_query_value(
					Some(read),
					snapshot,
					&mut assembly,
					value,
					&statement.result,
					plan,
					slots,
					source,
					control,
				)
			};
			if let Some(value) = visit_match_with_environment(
				read,
				snapshot,
				statement,
				slots,
				source,
				control,
				&Object::new(),
				&mut consume,
			)? {
				consume(value)?;
			}
		}
		LogicalBuildGraphSource::Select(statement) => {
			let schema = statement.output_schema();
			select::visit_select_values(
				read,
				snapshot,
				statement,
				slots,
				source,
				control,
				None,
				None,
				|value| {
					consume_query_value(
						Some(read),
						snapshot,
						&mut assembly,
						value,
						&schema,
						plan,
						slots,
						source,
						control,
					)
				},
			)?;
		}
		LogicalBuildGraphSource::Traverse(statement) => {
			let mut consume = |value| {
				consume_query_value(
					recipe_read.as_ref(),
					snapshot,
					&mut assembly,
					value,
					&statement.result,
					plan,
					slots,
					source,
					control,
				)
			};
			if let Some(value) = traversal::visit_traverse_values(
				read,
				snapshot,
				statement,
				slots,
				source,
				control,
				runtime,
				&mut consume,
			)? {
				consume(value)?;
			}
		}
	}
	let graph = assembly.finish(read, snapshot, contract)?;
	let Some(name) = &plan.name else {
		return Ok(Value::Graph(Box::new(graph)));
	};
	read.install_built_graph(name, plan.mode, plan.contract_explicit, graph, control)
		.map(|reference| Value::GraphRef(Box::new(reference)))
}

/// Resolves optional external recipes before using the shared graph assembler.
fn consume_query_value<Read: QueryRead>(
	read: Option<&Read>,
	snapshot: Snapshot,
	assembly: &mut GraphAssemblyAccumulator<'_>,
	value: Value,
	schema: &Schema,
	plan: &LogicalBuildGraph,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	control.check()?;
	assembly.record_source_row();
	if plan.nodes.is_none() && plan.edges.is_none() {
		let value = match plan.bridge.as_deref() {
			Some(alias) => super::bridge::recipes(
				read.ok_or_else(|| invalid("bridge recipe reader is absent"))?,
				snapshot,
				value.clone(),
				alias,
				control,
			)?,
			None => value.clone(),
		};
		assembly.add_auto(value)?;
	}
	let value_mode =
		matches!(&plan.source, LogicalBuildGraphSource::Select(select) if select.value);
	let row = result_row(value, schema, value_mode)?;
	assembly.add_selectors(
		plan.nodes.as_ref(),
		plan.edges.as_ref(),
		plan.root.as_ref(),
		|expression| {
			let value = eval(expression, Some(&row), slots, source, None, Some(control))?;
			match plan.bridge.as_deref() {
				Some(alias) => super::bridge::recipes(
					read.ok_or_else(|| invalid("bridge recipe reader is absent"))?,
					snapshot,
					value,
					alias,
					control,
				),
				None => Ok(value),
			}
		},
	)
}

/// Projects the owned graph terminal into the existing result-row surface.
fn result_row(value: Value, schema: &Schema, value_mode: bool) -> FerrosGrynnResult<Row> {
	if !value_mode && let Value::Object(object) = &value {
		return Ok(Row::new(
			schema
				.columns
				.iter()
				.map(|column| object.get(column.name.as_ref()).cloned().unwrap_or(Value::None))
				.collect(),
		));
	}
	if schema.columns.len() == 1 {
		return Ok(Row::new(vec![value]));
	}
	Err(invalid("query-source graph assembly requires an object row or one VALUE column"))
}

/// Finishes family.
fn finish_family(
	mut graph: Graph<Mutable>,
	contract: LogicalGraphContract,
	root_values: BTreeSet<Value>,
	root_scope: Option<GraphId>,
) -> FerrosGrynnResult<MaterializedGraph> {
	graph.set_topology_constraints(contract.validators);
	let root = root_record(root_values, root_scope)?;
	let root_id = root
		.as_ref()
		.map(|record| graph.atom_id(&crate::graph::AtomRef::new(AtomKind::Node, record.clone())))
		.transpose()?
		.flatten();
	if root.is_some() && root_id.is_none() {
		return Err(invalid("ROOT must identify a selected root-scope node"));
	}
	let graph = graph.freeze();
	if contract.family == Some(crate::graph::GraphKind::Subgraph) {
		return Err(invalid("Subgraph is not a named graph contract family"));
	}
	MaterializedGraph::from_general(
		graph,
		contract.family.unwrap_or(crate::graph::GraphKind::Subgraph),
		root_id,
	)
}

/// Returns the root record result.
fn root_record(
	values: BTreeSet<Value>,
	root_scope: Option<GraphId>,
) -> FerrosGrynnResult<Option<RecordId>> {
	if values.len() > 1 {
		return Err(invalid("ROOT must resolve to exactly one node occurrence"));
	}
	match values.into_iter().next() {
		None | Some(Value::None) => Ok(None),
		Some(Value::Record(record)) => Ok(Some(*record)),
		Some(Value::Atom(atom)) if atom.kind() == AtomKind::Node => Ok(Some(atom.id().clone())),
		Some(Value::AtomOccurrence(occurrence))
			if occurrence.atom().kind() == AtomKind::Node
				&& root_scope.is_none_or(|root| occurrence.graph().id() == root) =>
		{
			Ok(Some(occurrence.atom().id().clone()))
		}
		Some(Value::AtomOccurrence(_)) => Err(invalid("ROOT must be a root-scope node occurrence")),
		Some(_) => Err(invalid("ROOT must resolve to one node RecordId or occurrence")),
	}
}

/// Ensures kind.
fn ensure_kind(actual: AtomKind, expected: Option<AtomKind>) -> FerrosGrynnResult<()> {
	if expected.is_some_and(|expected| expected != actual) {
		return Err(invalid("graph selector atom kind does not match its target"));
	}
	Ok(())
}

/// Reserves accounted.
fn reserve_accounted(
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
	bytes: usize,
) -> FerrosGrynnResult<()> {
	if memory.reserve(bytes) {
		return Ok(());
	}
	Err(FerrosGrynnError::traversal_resource_limit_exceeded(
		"graph assembly memory".to_owned(),
		control.barrier_memory_limit() as u64,
	))
}

/// Checks limit.
fn check_limit(actual: usize, limit: u64, subject: &str) -> FerrosGrynnResult<()> {
	if u64::try_from(actual).unwrap_or(u64::MAX) > limit {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			format!("graph result {subject}"),
			limit,
		));
	}
	Ok(())
}

/// Returns the missing result.
fn missing(atom: &crate::graph::AtomRef) -> FerrosGrynnError {
	FerrosGrynnError::atom_does_not_exist(atom.kind().to_string(), atom.id().to_string())
}

/// Returns the invalid result.
fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("graph assembly", reason.into())
}
