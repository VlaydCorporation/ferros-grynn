//! Atomic installation of validated owned graph results.

use std::{
	collections::{BTreeMap, BTreeSet},
	mem::size_of,
};

use super::{Mutation, Transaction};
use crate::{
	datastore::{
		ConflictKey,
		DatabaseRoot,
		GlobalAtomId,
		GraphSchema,
		consts::{
			GRAPH_INCIDENCE_WRITE_BYTES,
			GRAPH_MEMBERSHIP_WRITE_BYTES,
			GRAPH_OVERLAY_WRITE_BYTES,
			SCHEMA_WRITE_BYTES,
		},
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		EdgeTopology,
		Frozen,
		Graph,
		GraphId,
		GraphKind,
		GraphRef,
		GraphTopologyConstraints,
		MaterializedGraph,
	},
	syn::DefinitionMode,
	values::{Number, PropertyMap, RecordId, Text, Uuid},
};

#[derive(Debug, Clone)]
pub(super) struct InstallBuiltGraphTree {
	root_schema: GraphSchema,
	create_root: bool,
	removed_children: Vec<GraphId>,
	child_schemas: Vec<GraphSchema>,
	graphs: Vec<BuiltGraph>,
}

#[derive(Debug, Clone)]
struct BuiltGraph {
	graph: GraphId,
	nodes: Vec<BuiltNode>,
	edges: Vec<BuiltEdge>,
}

#[derive(Debug, Clone)]
struct BuiltNode {
	local: AtomId,
	global: GlobalAtomId,
	record: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
	nested_graph: Option<GraphId>,
}

#[derive(Debug, Clone)]
struct BuiltEdge {
	local: AtomId,
	global: GlobalAtomId,
	record: RecordId,
	label: Option<Text>,
	properties: PropertyMap,
	topology: EdgeTopology,
	weight: Option<Number>,
	transition_graph: Option<GraphId>,
}

impl InstallBuiltGraphTree {
	pub(super) fn root(&self) -> GraphId {
		self.root_schema.id()
	}

	pub(super) fn graph_ids(&self) -> impl Iterator<Item = GraphId> + '_ {
		std::iter::once(self.root())
			.chain(self.removed_children.iter().copied())
			.chain(self.child_schemas.iter().map(GraphSchema::id))
	}

	pub(super) fn apply(
		&self,
		root: &mut DatabaseRoot,
		published_at: u64,
	) -> FerrosGrynnResult<()> {
		if self.create_root {
			root.install_graph(self.root_schema.clone(), published_at)?;
		} else {
			clear_graph(root, self.root())?;
			for child in &self.removed_children {
				root.remove_graph(*child)?;
			}
			root.replace_graph_schema(self.root_schema.clone(), published_at)?;
		}
		for schema in &self.child_schemas {
			root.install_graph(schema.clone(), published_at)?;
		}
		for graph in &self.graphs {
			for node in &graph.nodes {
				validate_base_atom(root, node.global, &node.record, AtomKind::Node)?;
				root.attach_node_at(
					graph.graph,
					node.local,
					node.global,
					node.label.clone(),
					node.nested_graph,
				)?;
			}
			for edge in &graph.edges {
				validate_base_atom(root, edge.global, &edge.record, AtomKind::Edge)?;
				root.restore_edge_at(
					graph.graph,
					edge.local,
					edge.global,
					edge.label.clone(),
					edge.topology.clone(),
					edge.weight,
					edge.transition_graph,
				)?;
			}
			root.validate_graph_endpoints(graph.graph)?;
		}
		for graph in &self.graphs {
			for node in &graph.nodes {
				apply_effective_properties(
					root,
					graph.graph,
					node.local,
					node.global,
					&node.properties,
				)?;
			}
			for edge in &graph.edges {
				apply_effective_properties(
					root,
					graph.graph,
					edge.local,
					edge.global,
					&edge.properties,
				)?;
			}
		}
		Ok(())
	}

	fn estimated_bytes(&self) -> usize {
		self.graphs.iter().fold(size_of::<Self>(), |bytes, graph| {
			let node_bytes = graph.nodes.iter().fold(0_usize, |bytes, node| {
				bytes
					.saturating_add(size_of::<BuiltNode>())
					.saturating_add(node.record.estimated_heap_bytes())
					.saturating_add(node.label.as_ref().map_or(0, Text::len))
					.saturating_add(node.properties.estimated_heap_bytes())
			});
			let edge_bytes = graph.edges.iter().fold(0_usize, |bytes, edge| {
				bytes
					.saturating_add(size_of::<BuiltEdge>())
					.saturating_add(edge.record.estimated_heap_bytes())
					.saturating_add(edge.label.as_ref().map_or(0, Text::len))
					.saturating_add(edge.properties.estimated_heap_bytes())
					.saturating_add(
						edge.topology.endpoint_count().saturating_mul(size_of::<AtomId>()),
					)
			});
			bytes.saturating_add(node_bytes).saturating_add(edge_bytes)
		})
	}
}

impl Transaction {
	/// Installs one owned graph using existing database-global atoms only.
	pub(crate) fn install_built_graph(
		&mut self,
		name: &str,
		mode: DefinitionMode,
		contract_explicit: bool,
		graph: &MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		self.atomic_mutation(|transaction| {
			transaction.install_built_graph_inner(name, mode, contract_explicit, graph)
		})
	}

	fn install_built_graph_inner(
		&mut self,
		name: &str,
		mode: DefinitionMode,
		contract_explicit: bool,
		graph: &MaterializedGraph,
	) -> FerrosGrynnResult<GraphRef> {
		self.prepare_schema_mutation()?;
		if graph.kind() == GraphKind::Subgraph {
			return Err(invalid("a named graph requires an explicit durable family"));
		}
		if name.starts_with("$fg$") {
			return Err(FerrosGrynnError::invalid_argument(
				"graph names beginning with `$fg$` are reserved by the engine",
			));
		}
		let existing = self.working.schema.named_graph(name);
		if let Some(existing) = &existing {
			match mode {
				DefinitionMode::Normal => {
					return Err(FerrosGrynnError::graph_already_exists(name));
				}
				DefinitionMode::IfNotExists => return Ok(existing.reference().clone()),
				DefinitionMode::Overwrite => {}
			}
			if existing.reference().kind() != graph.kind() {
				return Err(invalid("OVERWRITE cannot change the graph family"));
			}
		}

		let source = graph.clone().into_general()?;
		let root_record = rooted_record(graph)?;
		let constraints = existing.as_ref().map_or_else(
			|| graph.topology_constraints(),
			|schema| {
				if contract_explicit {
					graph.topology_constraints()
				} else {
					schema.topology_constraints()
				}
			},
		);
		let create_root = existing.is_none();
		let mut root_schema = if let Some(existing) = &existing {
			existing.replace_definition(None, constraints, root_record)?
		} else {
			self.allocate_graph_schema(
				Text::from(name),
				graph.kind(),
				graph.properties().clone(),
				constraints,
				root_record,
				None,
			)?
		};
		let root_id = root_schema.id();
		let removed_children = self
			.working
			.schema
			.all_graphs()
			.filter(|schema| schema.owner_root() == Some(root_id))
			.map(|schema| schema.id())
			.collect::<Vec<_>>();

		let nested = collect_nested_graphs(&source)?;
		let mut graph_ids = BTreeMap::from([(source.identity(), root_id)]);
		let mut child_schemas = Vec::with_capacity(nested.len());
		for (identity, child) in &nested {
			let schema = self.allocate_graph_schema(
				Text::from(format!("$fg$pending_{}", identity)),
				GraphKind::Graph,
				child.properties().clone(),
				GraphTopologyConstraints::empty(),
				None,
				Some(root_id),
			)?;
			graph_ids.insert(*identity, schema.id());
			child_schemas.push(schema);
		}
		for schema in &mut child_schemas {
			let indexes = schema.system_indexes().cloned().collect::<Vec<_>>();
			let name =
				Text::from(format!("$fg$build_{:016x}_{:016x}", root_id.get(), schema.id().get()));
			*schema = GraphSchema::new(
				schema.version(),
				GraphRef::new(
					schema.id(),
					name,
					GraphKind::Graph,
					self.store.inner.graph_storage_mode(),
				)?,
				indexes[0].clone(),
				indexes[1].clone(),
				schema.properties().clone(),
				GraphTopologyConstraints::empty(),
				None,
				Some(root_id),
				self.snapshot.commit_ts,
			);
		}

		let mut graphs = Vec::with_capacity(nested.len().saturating_add(1));
		graphs.push(self.prepare_built_graph(root_id, &source, &graph_ids)?);
		for (identity, child) in &nested {
			graphs.push(self.prepare_built_graph(graph_ids[identity], child, &graph_ids)?);
		}
		for built in &graphs {
			if !built.edges.iter().any(|edge| edge.topology.edge_endpoint_sides().next().is_some())
			{
				continue;
			}
			let schema = if built.graph == root_id {
				&mut root_schema
			} else {
				child_schemas
					.iter_mut()
					.find(|schema| schema.id() == built.graph)
					.ok_or_else(|| invalid("built graph has no prepared schema"))?
			};
			if schema.edge_endpoint_index().is_none() {
				let capability = crate::datastore::SystemIndex::new(
					crate::datastore::IndexId::from_schema_id(
						self.store.inner.allocate_schema_id()?,
					),
					crate::datastore::SystemIndexKind::EdgeEndpoint,
				);
				*schema = schema.clone().with_edge_endpoint_index(capability)?;
			}
		}
		let mutation = InstallBuiltGraphTree {
			root_schema,
			create_root,
			removed_children,
			child_schemas,
			graphs,
		};
		for graph in mutation.graph_ids() {
			if let Some(root) = self.working.graphs.get(&graph).cloned() {
				for occurrence in root.occurrences() {
					let global = occurrence.global_id();
					self.note_change_target(
						global,
						Some(graph),
						ConflictKey::GraphMembership(graph, global),
					)?;
				}
			}
		}
		for built in &mutation.graphs {
			for global in built
				.nodes
				.iter()
				.map(|node| node.global)
				.chain(built.edges.iter().map(|edge| edge.global))
			{
				self.note_change_target(
					global,
					Some(built.graph),
					ConflictKey::GraphMembership(built.graph, global),
				)?;
			}
		}
		self.register_built_graph_write_set(&mutation)?;
		mutation.apply(&mut self.working, self.snapshot.commit_ts)?;
		self.retire_missing_changefeeds()?;
		let reference = mutation.root_schema.reference().clone();
		self.mutations.push(Mutation::InstallBuiltGraphTree(mutation));
		Ok(reference)
	}

	fn prepare_built_graph(
		&self,
		graph_id: GraphId,
		source: &Graph<Frozen>,
		graph_ids: &BTreeMap<Uuid, GraphId>,
	) -> FerrosGrynnResult<BuiltGraph> {
		let mut nodes = Vec::with_capacity(source.node_count());
		for node in source.nodes() {
			let record = node.record_id().clone();
			let global = self.require_built_atom(&record, AtomKind::Node)?;
			let nested_graph = node
				.inner_graph()
				.map(Graph::identity)
				.map(|identity| {
					graph_ids
						.get(&identity)
						.copied()
						.ok_or_else(|| invalid("nested graph identity was not prepared"))
				})
				.transpose()?;
			nodes.push(BuiltNode {
				local: node.id(),
				global,
				record,
				label: node.label_text().cloned(),
				properties: node.properties().clone(),
				nested_graph,
			});
		}
		let mut edges = Vec::with_capacity(source.edge_count());
		for edge in source.edges() {
			let record = edge.record_id().clone();
			let global = self.require_built_atom(&record, AtomKind::Edge)?;
			let transition_graph = edge
				.transition_graph()
				.map(Graph::identity)
				.map(|identity| {
					graph_ids
						.get(&identity)
						.copied()
						.ok_or_else(|| invalid("transition graph identity was not prepared"))
				})
				.transpose()?;
			edges.push(BuiltEdge {
				local: edge.id(),
				global,
				record,
				label: edge.label_text().cloned(),
				properties: edge.properties().clone(),
				topology: edge.topology().clone(),
				weight: edge.weight(),
				transition_graph,
			});
		}
		Ok(BuiltGraph {
			graph: graph_id,
			nodes,
			edges,
		})
	}

	fn require_built_atom(
		&self,
		record: &RecordId,
		expected: AtomKind,
	) -> FerrosGrynnResult<GlobalAtomId> {
		let global = self.working.resolve_atom(record).ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist(expected.to_string(), record.to_string())
		})?;
		validate_base_atom(&self.working, global, record, expected)?;
		Ok(global)
	}

	fn register_built_graph_write_set(
		&mut self,
		mutation: &InstallBuiltGraphTree,
	) -> FerrosGrynnResult<()> {
		let mut keys = BTreeMap::new();
		for graph in mutation.graph_ids() {
			keys.insert(ConflictKey::GraphDefinition(graph), SCHEMA_WRITE_BYTES);
			if let Some(root) = self.working.graph(graph) {
				for occurrence in root.occurrences() {
					keys.insert(
						ConflictKey::GraphMembership(graph, occurrence.global_id()),
						GRAPH_MEMBERSHIP_WRITE_BYTES,
					);
					keys.insert(
						ConflictKey::GraphIncidence(graph, occurrence.local_id()),
						GRAPH_INCIDENCE_WRITE_BYTES,
					);
					if occurrence.has_overlay() {
						keys.insert(
							ConflictKey::GraphOverlay(graph, occurrence.global_id()),
							GRAPH_OVERLAY_WRITE_BYTES,
						);
					}
				}
			}
		}
		for graph in &mutation.graphs {
			for (local, global) in graph
				.nodes
				.iter()
				.map(|node| (node.local, node.global))
				.chain(graph.edges.iter().map(|edge| (edge.local, edge.global)))
			{
				keys.insert(
					ConflictKey::GraphMembership(graph.graph, global),
					GRAPH_MEMBERSHIP_WRITE_BYTES,
				);
				keys.insert(
					ConflictKey::GraphIncidence(graph.graph, local),
					GRAPH_INCIDENCE_WRITE_BYTES,
				);
				keys.insert(
					ConflictKey::GraphOverlay(graph.graph, global),
					GRAPH_OVERLAY_WRITE_BYTES,
				);
				self.record_read(ConflictKey::BaseAtom(global));
				let (class, kind) = self.atom_domain(global)?;
				self.mark_range_change(ConflictKey::GraphClassRange(graph.graph, class));
				self.mark_range_change(ConflictKey::GraphKindRange(graph.graph, kind));
			}
		}
		let conflict_bytes = keys
			.iter()
			.filter(|(key, _cost)| !self.writes.contains_key(key))
			.fold(0_usize, |bytes, (_key, cost)| bytes.saturating_add(*cost));
		let posting_bytes = mutation.graphs.iter().flat_map(|graph| &graph.edges).try_fold(
			0_usize,
			|bytes, edge| {
				bytes
					.checked_add(super::graph::endpoint_posting_write_bytes(&edge.topology)?)
					.ok_or_else(|| self.limit_error(usize::MAX))
			},
		)?;
		let payload_bytes = mutation.estimated_bytes().saturating_add(posting_bytes);
		self.ensure_capacity(conflict_bytes.saturating_add(payload_bytes))?;
		self.write_bytes = self.write_bytes.saturating_add(payload_bytes);
		for (key, cost) in keys {
			self.insert_write(key, cost);
		}
		self.mark_range_change(ConflictKey::GraphCatalogRange);
		Ok(())
	}
}

fn collect_nested_graphs(root: &Graph<Frozen>) -> FerrosGrynnResult<BTreeMap<Uuid, Graph<Frozen>>> {
	let mut graphs = BTreeMap::new();
	let mut ancestry = BTreeSet::from([root.identity()]);
	collect_children(root, &mut ancestry, &mut graphs)?;
	Ok(graphs)
}

fn collect_children(
	graph: &Graph<Frozen>,
	ancestry: &mut BTreeSet<Uuid>,
	graphs: &mut BTreeMap<Uuid, Graph<Frozen>>,
) -> FerrosGrynnResult<()> {
	for child in graph
		.nodes()
		.filter_map(|node| node.inner_graph())
		.chain(graph.edges().filter_map(|edge| edge.transition_graph()))
	{
		let identity = child.identity();
		if ancestry.contains(&identity) {
			return Err(invalid("graph containment contains a cycle"));
		}
		if let Some(existing) = graphs.get(&identity) {
			if existing.revision() != child.revision()
				|| existing.node_count() != child.node_count()
				|| existing.edge_count() != child.edge_count()
			{
				return Err(invalid("one nested graph identity has conflicting contents"));
			}
			continue;
		}
		ancestry.insert(identity);
		collect_children(child, ancestry, graphs)?;
		ancestry.remove(&identity);
		graphs.insert(identity, child.clone());
	}
	Ok(())
}

fn clear_graph(root: &mut DatabaseRoot, graph: GraphId) -> FerrosGrynnResult<()> {
	let occurrences = root
		.graph(graph)
		.ok_or_else(|| FerrosGrynnError::graph_does_not_exist(graph.to_string()))?
		.occurrences()
		.map(|occurrence| occurrence.global_id())
		.collect::<Vec<_>>();
	for global in occurrences {
		root.detach(graph, global)?.ok_or_else(|| {
			FerrosGrynnError::atom_does_not_exist("occurrence", global.to_string())
		})?;
	}
	Ok(())
}

fn validate_base_atom(
	root: &DatabaseRoot,
	global: GlobalAtomId,
	record: &RecordId,
	expected: AtomKind,
) -> FerrosGrynnResult<()> {
	let atom = root.atom(global).ok_or_else(|| {
		FerrosGrynnError::atom_does_not_exist(expected.to_string(), record.to_string())
	})?;
	if atom.record_id() != record || atom.kind() != expected {
		return Err(FerrosGrynnError::atom_kind_mismatch(
			record.to_string(),
			atom.kind().to_string(),
			expected.to_string(),
		));
	}
	Ok(())
}

fn apply_effective_properties(
	root: &mut DatabaseRoot,
	graph: GraphId,
	local: AtomId,
	global: GlobalAtomId,
	desired: &PropertyMap,
) -> FerrosGrynnResult<()> {
	let base = root
		.atom(global)
		.ok_or_else(|| FerrosGrynnError::atom_does_not_exist("global", global.to_string()))?
		.properties()
		.clone();
	for (key, value) in desired {
		if base.get(key) != Some(value) {
			root.set_overlay(graph, local, key, value.clone())?;
		}
	}
	for (key, _value) in &base {
		if desired.get(key).is_none() {
			root.hide_property(graph, local, key)?;
		}
	}
	Ok(())
}

fn rooted_record(graph: &MaterializedGraph) -> FerrosGrynnResult<Option<RecordId>> {
	let Some(rooted) = graph.as_rooted_tree() else {
		return Ok(None);
	};
	let root = rooted.node(rooted.root()).ok_or_else(|| invalid("rooted-tree root is absent"))?;
	Ok(Some(root.record_id().clone()))
}

fn invalid(reason: impl Into<String>) -> FerrosGrynnError {
	FerrosGrynnError::graph_constraint_violation("BUILD GRAPH", reason.into())
}
