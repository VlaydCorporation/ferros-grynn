//! Containment-aware graph-result closure and owned assembly.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::*;
use crate::graph::{
	AtomRef,
	EdgeRead,
	Graph,
	GraphId,
	GraphRead,
	Mutable,
	NodeRead,
	QualifiedAtomId,
};

#[derive(Clone)]
struct ContainmentLink {
	parent: GraphId,
	owner: AtomId,
	owner_ref: AtomRef,
	child: GraphRef,
}

#[derive(Default)]
struct ContainmentIndex {
	graphs: BTreeMap<GraphId, GraphRef>,
	links: BTreeMap<(GraphId, AtomId), ContainmentLink>,
	children: BTreeMap<GraphId, Vec<ContainmentLink>>,
	parents: BTreeMap<GraphId, Vec<ContainmentLink>>,
}

impl ContainmentIndex {
	fn insert(&mut self, link: ContainmentLink) -> FerrosGrynnResult<()> {
		if let Some(existing) = self.links.get(&(link.parent, link.owner)) {
			if existing.child.id() != link.child.id() {
				return Err(invalid("one meta owner refers to conflicting child graphs"));
			}
			return Ok(());
		}
		self.graphs.entry(link.child.id()).or_insert_with(|| link.child.clone());
		self.children.entry(link.parent).or_default().push(link.clone());
		self.parents.entry(link.child.id()).or_default().push(link.clone());
		self.links.insert((link.parent, link.owner), link);
		Ok(())
	}

	fn child(&self, parent: GraphId, owner: AtomId) -> FerrosGrynnResult<&ContainmentLink> {
		self.links
			.get(&(parent, owner))
			.ok_or_else(|| invalid("qualified endpoint scope does not name a meta owner"))
	}
}

struct BuiltGraph {
	graph: Graph,
	ids: BTreeMap<AtomId, AtomId>,
}

#[derive(Clone, Copy)]
enum SelectionOrigin {
	Selected,
	Closure,
}

pub(super) fn resolve_root<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	root_hint: Option<&GraphRef>,
	selected: &BTreeMap<GraphId, GraphRef>,
	control: &ExecutionControl,
) -> FerrosGrynnResult<GraphRef> {
	if let Some(root) = root_hint {
		return Ok(root.clone());
	}
	if selected.len() == 1 {
		return selected
			.values()
			.next()
			.cloned()
			.ok_or_else(|| invalid("graph assembly root disappeared"));
	}
	if selected.is_empty() {
		return Err(invalid("graph assembly has no graph-scoped values"));
	}
	let selected_ids = selected.keys().copied().collect::<BTreeSet<_>>();
	let mut candidates = Vec::new();
	for candidate in read.graph_references(snapshot)? {
		control.check()?;
		let index = load_index(read, snapshot, &candidate, control)?;
		if selected_ids.iter().all(|id| index.graphs.contains_key(id)) {
			let reachable = index.graphs.keys().copied().collect::<BTreeSet<_>>();
			candidates.push((candidate, reachable));
		}
	}
	let candidate_ids = candidates.iter().map(|(graph, _)| graph.id()).collect::<BTreeSet<_>>();
	candidates.retain(|(graph, reachable)| {
		!reachable.iter().any(|id| *id != graph.id() && candidate_ids.contains(id))
	});
	if candidates.len() != 1 {
		return Err(invalid("graph assembly contains unrelated or ambiguous roots"));
	}
	candidates
		.pop()
		.map(|(graph, _)| graph)
		.ok_or_else(|| invalid("graph assembly root disappeared"))
}

pub(super) fn materialize_hierarchy<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	root: &GraphRef,
	graphs: &BTreeMap<GraphId, GraphRef>,
	selections: &mut BTreeMap<GraphId, GraphSelection>,
	node_limit: u64,
	edge_limit: u64,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<Graph<Mutable>> {
	let index = load_index(read, snapshot, root, control)?;
	for (id, graph) in graphs.iter() {
		let Some(known) = index.graphs.get(id) else {
			return Err(invalid("selected graph is not contained by the result root"));
		};
		if known != graph {
			return Err(invalid("selected graph reference disagrees with the result snapshot"));
		}
	}
	close_selections(read, snapshot, root, &index, selections, memory, control)?;
	let node_count = selections.values().map(|selection| selection.nodes.len()).sum::<usize>();
	let edge_count = selections.values().map(|selection| selection.edges.len()).sum::<usize>();
	check_limit(node_count, node_limit, "nodes")?;
	check_limit(edge_count, edge_limit, "edges")?;
	let mut built = BTreeMap::new();
	let mut ancestry = Vec::new();
	build_graph(
		read,
		snapshot,
		root,
		&index,
		selections,
		&mut built,
		&mut ancestry,
		memory,
		control,
	)?;
	control.record_graph_assembly_result(node_count, edge_count, built.len().saturating_sub(1));
	built
		.remove(&root.id())
		.map(|built| built.graph.thaw())
		.ok_or_else(|| invalid("assembled root graph is absent"))
}

fn load_index<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	root: &GraphRef,
	control: &ExecutionControl,
) -> FerrosGrynnResult<ContainmentIndex> {
	let mut index = ContainmentIndex::default();
	index.graphs.insert(root.id(), root.clone());
	let mut queue = VecDeque::from([(root.clone(), vec![root.id()])]);
	let mut scanned = BTreeSet::new();
	while let Some((graph, ancestry)) = queue.pop_front() {
		control.check()?;
		if !scanned.insert(graph.id()) {
			continue;
		}
		let source = read.graph(snapshot, &graph)?;
		for id in source.node_ids()? {
			let id = id?;
			let node = source.node(id)?.ok_or_else(|| invalid("meta node is absent"))?;
			let Some(child) = node.inner_graph_ref()?.map(std::borrow::Cow::into_owned) else {
				continue;
			};
			queue_child(&mut index, &mut queue, &ancestry, ContainmentLink {
				parent: graph.id(),
				owner: id,
				owner_ref: node.atom_ref()?,
				child,
			})?;
		}
		for id in source.edge_ids()? {
			let id = id?;
			let edge = source.edge(id)?.ok_or_else(|| invalid("meta edge is absent"))?;
			let Some(child) = edge.transition_graph_ref()?.map(std::borrow::Cow::into_owned) else {
				continue;
			};
			queue_child(&mut index, &mut queue, &ancestry, ContainmentLink {
				parent: graph.id(),
				owner: id,
				owner_ref: edge.atom_ref()?,
				child,
			})?;
		}
	}
	validate_containment_acyclic(&index)?;
	Ok(index)
}

fn validate_containment_acyclic(index: &ContainmentIndex) -> FerrosGrynnResult<()> {
	let mut indegree =
		index.graphs.keys().map(|graph| (*graph, 0_usize)).collect::<BTreeMap<_, _>>();
	for links in index.children.values() {
		for link in links {
			let degree = indegree
				.get_mut(&link.child.id())
				.ok_or_else(|| invalid("containment child is absent from the graph index"))?;
			*degree = degree.saturating_add(1);
		}
	}
	let mut ready = indegree
		.iter()
		.filter_map(|(graph, degree)| (*degree == 0).then_some(*graph))
		.collect::<VecDeque<_>>();
	let mut visited = 0_usize;
	while let Some(graph) = ready.pop_front() {
		visited = visited.saturating_add(1);
		for link in index.children.get(&graph).into_iter().flatten() {
			let degree = indegree
				.get_mut(&link.child.id())
				.ok_or_else(|| invalid("containment child is absent from the graph index"))?;
			*degree = degree.saturating_sub(1);
			if *degree == 0 {
				ready.push_back(link.child.id());
			}
		}
	}
	if visited != indegree.len() {
		return Err(invalid("graph containment contains a cycle"));
	}
	Ok(())
}

fn queue_child(
	index: &mut ContainmentIndex,
	queue: &mut VecDeque<(GraphRef, Vec<GraphId>)>,
	ancestry: &[GraphId],
	link: ContainmentLink,
) -> FerrosGrynnResult<()> {
	if ancestry.contains(&link.child.id()) {
		return Err(invalid("graph containment contains a cycle"));
	}
	let mut child_ancestry = ancestry.to_vec();
	child_ancestry.push(link.child.id());
	queue.push_back((link.child.clone(), child_ancestry));
	index.insert(link)
}

fn close_selections<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	root: &GraphRef,
	index: &ContainmentIndex,
	selections: &mut BTreeMap<GraphId, GraphSelection>,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	for (graph, selection) in selections.iter_mut() {
		let reference = index
			.graphs
			.get(graph)
			.ok_or_else(|| invalid("selected graph is outside the result root"))?;
		let source = read.graph(snapshot, reference)?;
		for record in std::mem::take(&mut selection.records) {
			let node = AtomRef::new(AtomKind::Node, record.clone());
			if source.atom_id(&node)?.is_some() {
				insert_selection_atom(selection, node, SelectionOrigin::Selected, memory, control)?;
				continue;
			}
			let edge = AtomRef::new(AtomKind::Edge, record.clone());
			if source.atom_id(&edge)?.is_some() {
				insert_selection_atom(selection, edge, SelectionOrigin::Selected, memory, control)?;
				continue;
			}
			return Err(FerrosGrynnError::atom_does_not_exist("atom", record.to_string()));
		}
		if selection.all_nodes {
			for id in source.node_ids()? {
				let node = source.node(id?)?.ok_or_else(|| invalid("selected node is absent"))?;
				insert_selection_atom(
					selection,
					node.atom_ref()?,
					SelectionOrigin::Selected,
					memory,
					control,
				)?;
			}
		}
		if selection.all_edges {
			for id in source.edge_ids()? {
				let edge = source.edge(id?)?.ok_or_else(|| invalid("selected edge is absent"))?;
				insert_selection_atom(
					selection,
					edge.atom_ref()?,
					SelectionOrigin::Selected,
					memory,
					control,
				)?;
			}
		}
	}

	loop {
		let before = selection_size(selections);
		let targets = selections.keys().copied().collect::<Vec<_>>();
		for target in targets {
			include_minimal_owners(root.id(), target, index, selections, memory, control)?;
		}
		let edges = selections
			.iter()
			.flat_map(|(graph, selection)| {
				selection.edges.iter().cloned().map(|edge| (*graph, edge)).collect::<Vec<_>>()
			})
			.collect::<Vec<_>>();
		for (graph, edge) in edges {
			control.check()?;
			close_edge(read, snapshot, graph, &edge, index, selections, memory, control)?;
		}
		if selection_size(selections) == before {
			break;
		}
	}
	Ok(())
}

fn include_minimal_owners(
	root: GraphId,
	target: GraphId,
	index: &ContainmentIndex,
	selections: &mut BTreeMap<GraphId, GraphSelection>,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	if root == target {
		return Ok(());
	}
	let mut distance = BTreeMap::from([(root, 0_usize)]);
	let mut queue = VecDeque::from([root]);
	while let Some(parent) = queue.pop_front() {
		let next = distance[&parent].saturating_add(1);
		for link in index.children.get(&parent).into_iter().flatten() {
			if distance.get(&link.child.id()).is_none_or(|known| next < *known) {
				distance.insert(link.child.id(), next);
				queue.push_back(link.child.id());
			}
		}
	}
	if !distance.contains_key(&target) {
		return Err(invalid("selected graph is not reachable from the result root"));
	}
	let mut pending = vec![target];
	let mut seen = BTreeSet::new();
	while let Some(child) = pending.pop() {
		if child == root || !seen.insert(child) {
			continue;
		}
		let child_distance = distance[&child];
		for link in index.parents.get(&child).into_iter().flatten() {
			if distance
				.get(&link.parent)
				.is_some_and(|value| value.saturating_add(1) == child_distance)
			{
				insert_atom(selections, link.parent, link.owner_ref.clone(), memory, control)?;
				pending.push(link.parent);
			}
		}
	}
	Ok(())
}

fn close_edge<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	graph: GraphId,
	edge: &AtomRef,
	index: &ContainmentIndex,
	selections: &mut BTreeMap<GraphId, GraphSelection>,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let reference = index.graphs.get(&graph).ok_or_else(|| invalid("edge graph is absent"))?;
	let source = read.graph(snapshot, reference)?;
	let id = source.atom_id(edge)?.ok_or_else(|| missing(edge))?;
	let edge = source.edge(id)?.ok_or_else(|| invalid("selected edge is absent"))?;
	for endpoint in edge.topology()?.endpoints() {
		match endpoint {
			EdgeEndpoint::Local(id) => {
				insert_atom(selections, graph, atom_ref(&source, *id)?, memory, control)?;
			}
			EdgeEndpoint::Nested(qualified) => {
				let mut scope_graph = graph;
				for owner in qualified.scope() {
					let reference = index
						.graphs
						.get(&scope_graph)
						.ok_or_else(|| invalid("qualified endpoint graph is absent"))?;
					let scope = read.graph(snapshot, reference)?;
					insert_atom(
						selections,
						scope_graph,
						atom_ref(&scope, *owner)?,
						memory,
						control,
					)?;
					scope_graph = index.child(scope_graph, *owner)?.child.id();
				}
				let reference = index
					.graphs
					.get(&scope_graph)
					.ok_or_else(|| invalid("qualified endpoint target graph is absent"))?;
				let scope = read.graph(snapshot, reference)?;
				insert_atom(
					selections,
					scope_graph,
					atom_ref(&scope, qualified.atom())?,
					memory,
					control,
				)?;
			}
		}
	}
	Ok(())
}

fn atom_ref<Reader: GraphRead>(graph: &Reader, id: AtomId) -> FerrosGrynnResult<AtomRef> {
	match id.kind() {
		Some(AtomKind::Node) => {
			graph.node(id)?.ok_or_else(|| invalid("edge endpoint node is absent"))?.atom_ref()
		}
		Some(AtomKind::Edge) => {
			graph.edge(id)?.ok_or_else(|| invalid("edge endpoint edge is absent"))?.atom_ref()
		}
		None => Err(invalid("edge topology contains a null endpoint")),
	}
}

fn insert_atom(
	selections: &mut BTreeMap<GraphId, GraphSelection>,
	graph: GraphId,
	atom: AtomRef,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let selection = selections.entry(graph).or_default();
	insert_selection_atom(selection, atom, SelectionOrigin::Closure, memory, control)
}

fn insert_selection_atom(
	selection: &mut GraphSelection,
	atom: AtomRef,
	origin: SelectionOrigin,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	let values = match atom.kind() {
		AtomKind::Node => &mut selection.nodes,
		AtomKind::Edge => &mut selection.edges,
	};
	if values.contains(&atom) {
		control.record_graph_deduplicated_value();
		return Ok(());
	}
	reserve_accounted(
		memory,
		control,
		Value::Atom(Box::new(atom.clone()))
			.estimated_heap_bytes()
			.saturating_add(std::mem::size_of::<AtomRef>()),
	)?;
	let kind = atom.kind();
	values.insert(atom);
	match origin {
		SelectionOrigin::Selected => control.record_graph_selected_atom(kind),
		SelectionOrigin::Closure => control.record_graph_closure_atom(kind),
	}
	Ok(())
}

fn selection_size(selections: &BTreeMap<GraphId, GraphSelection>) -> usize {
	selections
		.values()
		.map(|selection| {
			selection
				.nodes
				.len()
				.saturating_add(selection.edges.len())
				.saturating_add(selection.records.len())
		})
		.sum()
}

#[allow(
	clippy::too_many_arguments,
	reason = "recursive assembly carries one explicit bounded context"
)]
fn build_graph<Read: QueryRead>(
	read: &Read,
	snapshot: Snapshot,
	reference: &GraphRef,
	index: &ContainmentIndex,
	selections: &BTreeMap<GraphId, GraphSelection>,
	built: &mut BTreeMap<GraphId, BuiltGraph>,
	ancestry: &mut Vec<GraphId>,
	memory: &mut BarrierAccount,
	control: &ExecutionControl,
) -> FerrosGrynnResult<()> {
	if built.contains_key(&reference.id()) {
		return Ok(());
	}
	if ancestry.contains(&reference.id()) {
		return Err(invalid("graph containment contains a cycle"));
	}
	ancestry.push(reference.id());
	let source = read.graph(snapshot, reference)?;
	let selection = selections.get(&reference.id()).cloned().unwrap_or_default();
	let mut nodes = Vec::new();
	let mut edges = Vec::new();
	for atom in &selection.nodes {
		let id = source.atom_id(atom)?.ok_or_else(|| missing(atom))?;
		if id.kind() != Some(AtomKind::Node) {
			return Err(invalid("NODES selector resolved an edge atom"));
		}
		nodes.push(id);
	}
	for atom in &selection.edges {
		let id = source.atom_id(atom)?.ok_or_else(|| missing(atom))?;
		if id.kind() != Some(AtomKind::Edge) {
			return Err(invalid("EDGES selector resolved a node atom"));
		}
		edges.push(id);
	}
	for owner in nodes.iter().chain(&edges).copied() {
		if let Some(link) = index.links.get(&(reference.id(), owner)) {
			build_graph(
				read,
				snapshot,
				&link.child,
				index,
				selections,
				built,
				ancestry,
				memory,
				control,
			)?;
		}
	}
	control.check()?;
	let graph_properties = source.properties()?.into_owned();
	reserve_accounted(
		memory,
		control,
		graph_properties
			.estimated_heap_bytes()
			.saturating_add(std::mem::size_of::<Graph<Mutable>>()),
	)?;
	let mut result =
		Graph::<Mutable>::begin_query_materialization(graph_properties, nodes.len(), edges.len());
	result.set_topology_constraints(source.topology_constraints());
	let mut ids = BTreeMap::new();
	for source_id in nodes {
		let node = source.node(source_id)?.ok_or_else(|| invalid("selected node is absent"))?;
		let child = index
			.links
			.get(&(reference.id(), source_id))
			.map(|link| {
				built
					.get(&link.child.id())
					.map(|child| child.graph.clone())
					.ok_or_else(|| invalid("assembled child graph is absent"))
			})
			.transpose()?;
		if child.is_some() {
			control.record_graph_scope_slots(1);
		}
		let record = node.record_id()?.into_owned();
		let label = node.label_text()?.map(std::borrow::Cow::into_owned);
		let properties = node.properties()?.into_owned();
		reserve_accounted(
			memory,
			control,
			record
				.estimated_heap_bytes()
				.saturating_add(label.as_ref().map_or(0, |value| value.len()))
				.saturating_add(properties.estimated_heap_bytes())
				.saturating_add(std::mem::size_of::<AtomId>() * 8),
		)?;
		let target = result.materialize_query_node(record, label, properties, child)?;
		ids.insert(source_id, target);
	}
	for source_id in &edges {
		let edge = source.edge(*source_id)?.ok_or_else(|| invalid("selected edge is absent"))?;
		let child = index
			.links
			.get(&(reference.id(), *source_id))
			.map(|link| {
				built
					.get(&link.child.id())
					.map(|child| child.graph.clone())
					.ok_or_else(|| invalid("assembled transition graph is absent"))
			})
			.transpose()?;
		if child.is_some() {
			control.record_graph_scope_slots(1);
		}
		let record = edge.record_id()?.into_owned();
		let label = edge.label_text()?.map(std::borrow::Cow::into_owned);
		let properties = edge.properties()?.into_owned();
		reserve_accounted(
			memory,
			control,
			record
				.estimated_heap_bytes()
				.saturating_add(label.as_ref().map_or(0, |value| value.len()))
				.saturating_add(properties.estimated_heap_bytes())
				.saturating_add(std::mem::size_of::<AtomId>() * 10),
		)?;
		let target = result.reserve_query_edge(record, label, properties, edge.weight(), child)?;
		ids.insert(*source_id, target);
	}
	for source_id in edges {
		let edge = source.edge(source_id)?.ok_or_else(|| invalid("selected edge is absent"))?;
		let topology = edge.topology()?.try_map_endpoints(|endpoint| {
			remap_endpoint(reference.id(), endpoint, &ids, index, built)
		})?;
		reserve_accounted(memory, control, crate::graph::estimated_topology_heap_bytes(&topology))?;
		let target = ids
			.get(&source_id)
			.copied()
			.ok_or_else(|| invalid("reserved edge mapping is absent"))?;
		result.materialize_query_edge_topology(target, topology)?;
	}
	ancestry.pop();
	built.insert(reference.id(), BuiltGraph {
		graph: result.freeze(),
		ids,
	});
	Ok(())
}

fn remap_endpoint(
	root: GraphId,
	endpoint: &EdgeEndpoint,
	root_ids: &BTreeMap<AtomId, AtomId>,
	index: &ContainmentIndex,
	built: &BTreeMap<GraphId, BuiltGraph>,
) -> FerrosGrynnResult<EdgeEndpoint> {
	match endpoint {
		EdgeEndpoint::Local(id) => root_ids
			.get(id)
			.copied()
			.map(EdgeEndpoint::Local)
			.ok_or_else(|| invalid("selected edge endpoint was not closed")),
		EdgeEndpoint::Nested(qualified) => {
			let mut graph = root;
			let mut scope = Vec::with_capacity(qualified.scope().len());
			for (depth, owner) in qualified.scope().iter().enumerate() {
				let ids = if depth == 0 {
					root_ids
				} else {
					&built
						.get(&graph)
						.ok_or_else(|| invalid("qualified endpoint child graph is absent"))?
						.ids
				};
				scope.push(
					ids.get(owner)
						.copied()
						.ok_or_else(|| invalid("qualified endpoint owner was not closed"))?,
				);
				graph = index.child(graph, *owner)?.child.id();
			}
			let atom = built
				.get(&graph)
				.ok_or_else(|| invalid("qualified endpoint target graph is absent"))?
				.ids
				.get(&qualified.atom())
				.copied()
				.ok_or_else(|| invalid("qualified endpoint target atom was not closed"))?;
			QualifiedAtomId::new(scope, atom).map(EdgeEndpoint::Nested)
		}
	}
}
