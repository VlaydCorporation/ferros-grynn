//! Owned graph-result assembly shared by RETURN GRAPH and BUILD GRAPH.

use std::{
	collections::{BTreeMap, BTreeSet},
	mem::size_of,
};

mod hierarchy;

use hierarchy::{materialize_hierarchy, resolve_root};

use super::*;
use crate::{
	gql::ir::{
		LogicalBuildGraph,
		LogicalBuildGraphSource,
		LogicalGraphContract,
		LogicalReturnGraph,
		Schema,
	},
	graph::{
		Dag,
		Forest,
		Graph,
		GraphId,
		Hypergraph,
		MaterializedGraph,
		Metagraph,
		Mutable,
		QueryMaterialize,
		RootedTree,
		Subgraph,
		Tree,
		validate_topology_constraints,
	},
};

#[derive(Clone, Default)]
struct GraphSelection {
	nodes: BTreeSet<crate::graph::AtomRef>,
	edges: BTreeSet<crate::graph::AtomRef>,
	records: BTreeSet<RecordId>,
	all_nodes: bool,
	all_edges: bool,
}

/// One statement-local, non-spilling owned graph builder.
pub(super) struct GraphAssemblyAccumulator<'a> {
	root_hint: Option<GraphRef>,
	graphs: BTreeMap<GraphId, GraphRef>,
	selections: BTreeMap<GraphId, GraphSelection>,
	graphless_nodes: BTreeSet<RecordId>,
	direct: Option<MaterializedGraph>,
	root_values: BTreeSet<Value>,
	node_limit: u64,
	edge_limit: u64,
	memory: BarrierAccount,
	control: &'a ExecutionControl,
}

impl<'a> GraphAssemblyAccumulator<'a> {
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

	pub(super) fn add_root(&mut self, value: Value) -> FerrosGrynnResult<()> {
		if !matches!(value, Value::None) && self.root_values.contains(&value) {
			self.control.record_graph_deduplicated_value();
		} else if !matches!(value, Value::None) {
			self.reserve(value.estimated_heap_bytes().saturating_add(size_of::<Value>()))?;
			self.root_values.insert(value);
		}
		Ok(())
	}

	pub(super) fn record_source_row(&self) {
		self.control.record_graph_source_row();
	}

	pub(super) fn add_nodes(&mut self, value: Value) -> FerrosGrynnResult<()> {
		self.add(value, Some(AtomKind::Node), false)
	}

	pub(super) fn add_edges(&mut self, value: Value) -> FerrosGrynnResult<()> {
		self.add(value, Some(AtomKind::Edge), false)
	}

	pub(super) fn add_auto(&mut self, value: Value) -> FerrosGrynnResult<()> {
		self.add(value, None, true)
	}

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

	fn reserve(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		if self.memory.reserve(bytes) {
			return Ok(());
		}
		Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			"graph assembly memory".to_owned(),
			self.control.barrier_memory_limit() as u64,
		))
	}

	fn enforce_result_memory(&mut self, graph: &MaterializedGraph) -> FerrosGrynnResult<()> {
		self.memory.release_all();
		self.reserve(graph.estimated_heap_bytes())
	}

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
				graph.materialize_query_node(record, None, properties, None)?;
			}
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

pub(super) fn execute_return_graph<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	plan: &LogicalReturnGraph,
	slots: &[Value],
	source: &str,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Value> {
	let mut assembly = GraphAssemblyAccumulator::new(None, None, None, control);
	assembly.add_nodes(eval(&plan.nodes, None, slots, source, None, Some(control))?)?;
	assembly.add_edges(eval(&plan.edges, None, slots, source, None, Some(control))?)?;
	if let Some(root) = &plan.root {
		assembly.add_root(eval(root, None, slots, source, None, Some(control))?)?;
	}
	assembly.finish(read, snapshot, plan.contract).map(|graph| Value::Graph(Box::new(graph)))
}

#[allow(clippy::too_many_arguments, reason = "BUILD GRAPH reuses every existing query executor")]
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
		LogicalBuildGraphSource::Match(statement) => Some(statement.graph.clone()),
		LogicalBuildGraphSource::Select(statement) => match &statement.source.kind {
			LogicalSelectSourceKind::Atoms {
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
	match &plan.source {
		LogicalBuildGraphSource::Expression(expression) => {
			assembly.record_source_row();
			assembly.add_auto(eval_query_expression(
				read, expression, slots, source, control, runtime,
			)?)?;
			if let Some(root) = &plan.root {
				assembly.add_root(eval_query_expression(
					read, root, slots, source, control, runtime,
				)?)?;
			}
		}
		LogicalBuildGraphSource::Match(statement) => {
			let mut consume = |value| {
				consume_query_value(
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
						&mut assembly,
						value,
						&statement.result,
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
	read.install_built_graph(name, plan.mode, plan.contract_explicit, graph)
		.map(|reference| Value::GraphRef(Box::new(reference)))
}

fn consume_query_value(
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
		assembly.add_auto(value.clone())?;
	}
	let row = result_row(value, schema)?;
	if let Some(nodes) = &plan.nodes {
		assembly.add_nodes(eval(nodes, Some(&row), slots, source, None, Some(control))?)?;
	}
	if let Some(edges) = &plan.edges {
		assembly.add_edges(eval(edges, Some(&row), slots, source, None, Some(control))?)?;
	}
	if let Some(root) = &plan.root {
		assembly.add_root(eval(root, Some(&row), slots, source, None, Some(control))?)?;
	}
	Ok(())
}

fn result_row(value: Value, schema: &Schema) -> FerrosGrynnResult<Row> {
	if let Value::Object(object) = value {
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
	let graph = graph.freeze();
	let materialized = match contract.family {
		None => MaterializedGraph::from(Subgraph::from_graph(graph)),
		Some(crate::graph::GraphKind::Graph) => MaterializedGraph::from(graph),
		Some(crate::graph::GraphKind::Tree) => MaterializedGraph::from(Tree::try_from(graph)?),
		Some(crate::graph::GraphKind::RootedTree) => {
			let rooted = if let Some(root) = root_id {
				RootedTree::try_from_graph_with_root(graph, root)?
			} else {
				RootedTree::try_from(graph)?
			};
			MaterializedGraph::from(rooted)
		}
		Some(crate::graph::GraphKind::Forest) => MaterializedGraph::from(Forest::try_from(graph)?),
		Some(crate::graph::GraphKind::Dag) => MaterializedGraph::from(Dag::try_from(graph)?),
		Some(crate::graph::GraphKind::Hypergraph) => {
			MaterializedGraph::from(Hypergraph::try_from(graph)?)
		}
		Some(crate::graph::GraphKind::Metagraph) => {
			MaterializedGraph::from(Metagraph::try_from(graph)?)
		}
		Some(crate::graph::GraphKind::Subgraph) => {
			return Err(invalid("Subgraph is not a named graph contract family"));
		}
	};
	validate_topology_constraints(&materialized, contract.validators)?;
	Ok(materialized)
}

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

fn ensure_kind(actual: AtomKind, expected: Option<AtomKind>) -> FerrosGrynnResult<()> {
	if expected.is_some_and(|expected| expected != actual) {
		return Err(invalid("graph selector atom kind does not match its target"));
	}
	Ok(())
}

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

fn check_limit(actual: usize, limit: u64, subject: &str) -> FerrosGrynnResult<()> {
	if u64::try_from(actual).unwrap_or(u64::MAX) > limit {
		return Err(FerrosGrynnError::traversal_resource_limit_exceeded(
			format!("graph result {subject}"),
			limit,
		));
	}
	Ok(())
}

fn missing(atom: &crate::graph::AtomRef) -> FerrosGrynnError {
	FerrosGrynnError::atom_does_not_exist(atom.kind().to_string(), atom.id().to_string())
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("graph assembly", reason.into())
}
