//! Fixed-root image preflight and explicit source mutation for mutable LIVE
//! graphs.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	Transaction,
	operation_image::{self, CaptureBudget, same_number, same_object, same_value},
};
use crate::{
	datastore::{
		ConflictKey,
		GlobalAtomId,
		OccurrenceState,
		OccurrenceStateRef,
		SubscriptionHandle,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomKind,
		AtomRef,
		EdgeDirection,
		EdgeEndpoint,
		EdgeTopology,
		GraphId,
		QualifiedAtomId,
	},
	live::{
		LiveCursor,
		LiveGraphAtomId,
		LiveGraphEdge,
		LiveGraphEndpoint,
		LiveGraphImage,
		LiveGraphNode,
		LiveGraphScope,
		LiveGraphScopeId,
		LiveGraphTarget,
		LiveState,
	},
	values::{Object, PropertyMap, Text},
};

/// Borrowed effective data avoids a second owned source representation.
#[derive(Clone, Copy)]
enum Atom<'a> {
	/// One mapped node.
	Node(
		/// One mapped node.
		&'a LiveGraphNode,
	),
	/// One mapped connector with qualified topology.
	Edge(
		/// One mapped connector with qualified topology.
		&'a LiveGraphEdge,
	),
}

impl<'a> Atom<'a> {
	/// Selects the exact kind rather than guessing from a display name.
	fn get(scope: &'a LiveGraphScope, atom: &AtomRef) -> Option<Self> {
		match atom.kind() {
			AtomKind::Node => scope.nodes.get(atom).map(Self::Node),
			AtomKind::Edge => scope.edges.get(atom).map(Self::Edge),
		}
	}

	/// Borrows named effective properties.
	fn properties(self) -> &'a Object {
		match self {
			Self::Node(node) => &node.properties,
			Self::Edge(edge) => &edge.properties,
		}
	}

	/// Borrows a graph-local label.
	fn label(self) -> Option<&'a Text> {
		match self {
			Self::Node(node) => node.label.as_ref(),
			Self::Edge(edge) => edge.label.as_ref(),
		}
	}

	/// Requires the class incarnation accepted with the source image.
	fn source(self) -> FerrosGrynnResult<crate::live::LiveGraphSource> {
		match self {
			Self::Node(node) => node.source,
			Self::Edge(edge) => edge.source,
		}
		.ok_or_else(unmapped)
	}
}

/// One prepared write; owned structural IDs belong only to this transaction.
struct Write<'a> {
	/// Accepted qualified target.
	id: &'a LiveGraphAtomId,
	/// Occurrence owner, or database-global base namespace.
	graph: Option<GraphId>,
	/// Checked stable global identity.
	global: GlobalAtomId,
	/// Accepted old properties for exact name-level changes.
	before: Atom<'a>,
	/// None means an explicit removal from a retained result scope.
	after: Option<Atom<'a>>,
	/// Prepared desired graph-local topology or child binding.
	state: Option<OccurrenceState>,
}

/// Shared scratch and work admission for the complete preflight.
struct Budget {
	/// Existing operation-image copy authority.
	capture: CaptureBudget,
	/// Remaining logical work before any source publication.
	remaining: u64,
}

impl Budget {
	/// Checks work and bytes before copies or growth.
	fn admit(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		self.take(1)?;
		self.capture.admit(bytes)
	}

	/// Charges scans and validation without allocating.
	fn take(&mut self, work: u64) -> FerrosGrynnResult<()> {
		self.remaining = self.remaining.checked_sub(work).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded("LIVE flush work", work)
		})?;
		Ok(())
	}
}

/// A missing mapping never creates or implicitly changes a source atom.
fn unmapped() -> FerrosGrynnError {
	FerrosGrynnError::graph_write_back_unavailable()
}

/// Source changes after the accepted image require explicit conflict
/// resolution.
fn stale(tx: &Transaction) -> FerrosGrynnError {
	FerrosGrynnError::serialization_failure(
		tx.id,
		"LIVE graph source differs from its accepted image",
	)
}

/// Weights use the engine's numeric equality, including representation changes.
fn state_equal(left: &OccurrenceState, right: &OccurrenceState) -> bool {
	match (left.view(), right.view()) {
		(
			OccurrenceStateRef::Node {
				nested_graph: left,
			},
			OccurrenceStateRef::Node {
				nested_graph: right,
			},
		) => left == right,
		(
			OccurrenceStateRef::Edge {
				topology: lt,
				weight: lw,
				transition_graph: lg,
			},
			OccurrenceStateRef::Edge {
				topology: rt,
				weight: rw,
				transition_graph: rg,
			},
		) => {
			lt == rt
				&& lg == rg
				&& match (lw, rw) {
					(None, None) => true,
					(Some(left), Some(right)) => same_number(*left, *right),
					_ => false,
				}
		}
		_ => false,
	}
}

impl Transaction {
	/// Checks owner, incarnation and every target before the first mutation.
	pub(crate) fn flush_live_graph(
		&mut self,
		handle: &SubscriptionHandle,
		cursor: &LiveCursor,
		before: &LiveGraphImage,
		after: &LiveGraphImage,
		changes: &[LiveGraphTarget],
		limits: &crate::live::LiveOptions,
	) -> FerrosGrynnResult<()> {
		handle.validate_store(&self.store)?;
		self.run_gql_statement(|tx| {
			tx.atomic_mutation(|tx| {
				let definition = tx
					.working
					.live_catalog()
					.queries
					.get(&cursor.live_id())
					.ok_or_else(unmapped)?;
				cursor.validate(
					tx.working.database_uuid.into(),
					definition.id,
					definition.version,
				)?;
				if definition.state != LiveState::Active {
					return Err(unmapped());
				}
				tx.require_live_graph_key(ConflictKey::LiveDefinition(cursor.live_id()))?;
				let mut budget = Budget {
					capture: CaptureBudget {
						bytes: 0,
						maximum: limits
							.scratch_bytes()
							.min(tx.store.inner.transaction_limits().max_write_set_bytes),
					},
					remaining: limits.work(),
				};
				after.validate_structure(&mut |bytes, work| {
					budget.take(work)?;
					budget.capture.admit(bytes)
				})?;
				if before.family != after.family
					|| before.validators != after.validators
					|| before.root != after.root
				{
					return Err(unmapped());
				}
				budget.admit(changes.len().checked_mul(512).ok_or_else(unmapped)?)?;
				let mut writes = Vec::new();
				let mut graphs = Vec::new();
				let mut removals = BTreeMap::<GraphId, BTreeSet<GlobalAtomId>>::new();
				let mut mapped = BTreeSet::new();
				for target in changes {
					budget.admit(0)?;
					let scope = match target {
						LiveGraphTarget::Graph(scope) => scope,
						LiveGraphTarget::Atom(atom) => atom.scope(),
					};
					let Some(new_scope) = after.scopes.get(scope) else {
						// Losing child closure never deletes the catalog child
						// or its atoms.
						continue;
					};
					let old_scope = before.scopes.get(scope).ok_or_else(unmapped)?;
					let graph = old_scope.source.as_ref().map(|source| source.id());
					if graph.is_none() && *scope != LiveGraphScopeId::Root {
						return Err(unmapped());
					}
					if old_scope.source != new_scope.source
						|| old_scope.validators != new_scope.validators
					{
						return Err(unmapped());
					}
					if let Some(graph) = graph {
						tx.require_live_graph_definition(graph)?;
					}
					match target {
						LiveGraphTarget::Graph(_) => {
							let graph = graph.ok_or_else(unmapped)?;
							let schema = tx.working.schema.graph(graph).ok_or_else(unmapped)?;
							let mut actual = Object::new();
							for (key, value) in schema.properties() {
								let name =
									tx.working.dictionary.resolve(key).ok_or_else(unmapped)?;
								budget.admit(256 + name.len() + value.estimated_heap_bytes())?;
								actual.insert(name.clone(), value.clone());
							}
							if !same_object(&actual, &old_scope.properties) {
								return Err(stale(tx));
							}
							graphs.push((graph, &new_scope.properties));
						}
						LiveGraphTarget::Atom(id) => {
							let old = Atom::get(old_scope, id.atom()).ok_or_else(unmapped)?;
							let source = old.source()?;
							budget.take(old.properties().len() as u64)?;
							if source.revision().get() > tx.snapshot.commit_ts {
								return Err(stale(tx));
							}
							let global =
								tx.working.resolve_atom(id.atom().id()).ok_or_else(|| stale(tx))?;
							let atom = tx.working.atom(global).ok_or_else(|| stale(tx))?;
							if atom.class_id() != source.class() || atom.kind() != id.atom().kind()
							{
								return Err(stale(tx));
							}
							if !mapped.insert((graph, global)) {
								return Err(unmapped());
							}
							budget.take(
								tx.working.atom(global).ok_or_else(unmapped)?.properties().len()
									as u64,
							)?;
							if let Some(graph) = graph
								&& let Some(local) = tx.working.membership(graph, global)
								&& let Some(overlay) = tx
									.working
									.occurrence(graph, local)
									.and_then(|value| value.overlay())
							{
								budget.take(overlay.iter().len() as u64)?;
							}
							tx.require_live_graph_atom(global, graph)?;
							let actual = operation_image::image(
								&tx.working,
								global,
								graph,
								&mut budget.capture,
							)?
							.ok_or_else(|| stale(tx))?;
							if !same_object(actual.properties(), old.properties())
								|| actual.label() != old.label()
							{
								return Err(stale(tx));
							}
							let new = Atom::get(new_scope, id.atom());
							if let Some(new) = new {
								budget.take(new.properties().len() as u64)?;
								if new.source()?.class() != source.class() {
									return Err(unmapped());
								}
							}
							let state = if let Some(graph) = graph {
								let expected =
									tx.live_graph_state(before, old, graph, &mut budget, true)?;
								let local = tx
									.working
									.membership(graph, global)
									.ok_or_else(|| stale(tx))?;
								if !state_equal(
									&expected,
									tx.working
										.occurrence(graph, local)
										.ok_or_else(|| stale(tx))?
										.state(),
								) {
									return Err(stale(tx));
								}
								new.map(|new| {
									tx.live_graph_state(after, new, graph, &mut budget, false)
								})
								.transpose()?
							} else {
								if let Some(new) = new {
									if new.label() != old.label()
										|| !Self::live_graph_global_state_equal(old, new)
									{
										return Err(unmapped());
									}
								} else if tx
									.working
									.memberships(global)
									.is_some_and(|members| !members.is_empty())
								{
									return Err(unmapped());
								}
								None
							};
							if new.is_none()
								&& let Some(graph) = graph
							{
								removals.entry(graph).or_default().insert(global);
							}
							writes.push(Write {
								id,
								graph,
								global,
								before: old,
								after: new,
								state,
							});
						}
					}
				}
				budget.admit(writes.len() * 128)?;
				let desired_states = writes
					.iter()
					.filter_map(|write| {
						write
							.graph
							.zip(write.state.as_ref())
							.map(|(graph, state)| ((graph, write.global), state))
					})
					.collect::<BTreeMap<_, _>>();
				// Every dependency must be an explicitly selected removal,
				// including E-E cycles.
				for (graph, removed) in &removals {
					tx.require_live_graph_key(ConflictKey::GraphKindRange(*graph, AtomKind::Edge))?;
					for occurrence in tx.working.graph(*graph).ok_or_else(unmapped)?.occurrences() {
						budget.admit(0)?;
						if removed.contains(&occurrence.global_id()) {
							continue;
						}
						let desired = desired_states
							.get(&(*graph, occurrence.global_id()))
							.copied()
							.unwrap_or_else(|| occurrence.state());
						if let OccurrenceStateRef::Edge {
							topology,
							..
						} = desired.view()
						{
							for endpoint in topology.endpoints() {
								budget.admit(0)?;
								let local = match endpoint {
									EdgeEndpoint::Local(local) => *local,
									EdgeEndpoint::Nested(path) => path.scope()[0],
								};
								if tx
									.working
									.occurrence(*graph, local)
									.is_some_and(|target| removed.contains(&target.global_id()))
								{
									return Err(unmapped());
								}
							}
						}
					}
				}
				// Bind containers before recording final qualified connector
				// anchors.
				for write in &writes {
					let (Some(graph), Some(desired)) = (write.graph, write.state.as_ref()) else {
						continue;
					};
					let local = tx.working.membership(graph, write.global).ok_or_else(unmapped)?;
					let current = tx.working.occurrence(graph, local).ok_or_else(unmapped)?.state();
					let binding = match (current.view(), desired.view()) {
						(
							OccurrenceStateRef::Node {
								nested_graph: old,
							},
							OccurrenceStateRef::Node {
								nested_graph: new,
							},
						) if old != new => Some(OccurrenceState::node(*new)),
						(
							OccurrenceStateRef::Edge {
								topology,
								weight,
								transition_graph: old,
							},
							OccurrenceStateRef::Edge {
								transition_graph: new,
								..
							},
						) if old != new => {
							budget.admit(crate::graph::estimated_topology_heap_bytes(topology))?;
							Some(OccurrenceState::edge(topology.clone(), *weight, *new))
						}
						_ => None,
					};
					if let Some(binding) = binding {
						tx.replace_occurrence_state(graph, write.id.atom().id(), binding)?;
					}
				}
				for write in &writes {
					let Some(new) = write.after else {
						continue;
					};
					if let Some(graph) = write.graph {
						tx.set_occurrence_label(graph, write.id.atom().id(), new.label().cloned())?;
						if let Some(state) = &write.state {
							tx.replace_occurrence_state(
								graph,
								write.id.atom().id(),
								state.clone(),
							)?;
						}
					}
					tx.write_live_graph_properties(
						write.graph,
						write.id.atom(),
						write.before.properties(),
						new.properties(),
					)?;
				}
				for (graph, removed) in removals {
					tx.detach_occurrence_plan(graph, removed.into_iter().collect())?;
				}
				for write in &writes {
					if write.graph.is_none() && write.after.is_none() {
						tx.delete_atom(write.id.atom().id())?;
					}
				}
				for (graph, properties) in graphs {
					let schema = tx.working.schema.graph(graph).ok_or_else(unmapped)?;
					let mut mapped = PropertyMap::new();
					for (name, value) in properties {
						mapped.insert(tx.intern_property(name.as_str())?, value.clone());
					}
					tx.replace_graph_properties(schema.name().as_str(), mapped)?;
				}
				Ok(())
			})
		})
	}

	/// Makes preflight reads FCW prerequisites at every supported isolation.
	fn require_live_graph_key(&mut self, key: ConflictKey) -> FerrosGrynnResult<()> {
		let bytes = usize::from(!self.requirements.contains_key(&key)) * 128
			+ usize::from(!self.reads.lock().contains_key(&key)) * 64;
		self.ensure_capacity(bytes)?;
		self.record_requirement(key);
		self.record_read(key);
		self.write_bytes += bytes;
		Ok(())
	}

	/// Resolves graph identities before names can participate in a write.
	fn require_live_graph_definition(&mut self, graph: GraphId) -> FerrosGrynnResult<()> {
		if self.working.schema.graph(graph).is_none() {
			return Err(stale(self));
		}
		self.require_live_graph_key(ConflictKey::GraphDefinition(graph))
	}

	/// Pins base, class and occurrence metadata before overlay/topology writes.
	fn require_live_graph_atom(
		&mut self,
		global: GlobalAtomId,
		graph: Option<GraphId>,
	) -> FerrosGrynnResult<()> {
		let class = self.working.atom(global).ok_or_else(unmapped)?.class_id();
		self.require_live_graph_key(ConflictKey::SchemaClass(class))?;
		self.require_live_graph_key(ConflictKey::BaseAtom(global))?;
		if let Some(graph) = graph {
			self.require_live_graph_definition(graph)?;
			let local = self.working.membership(graph, global).ok_or_else(|| stale(self))?;
			for key in [
				ConflictKey::GraphMembership(graph, global),
				ConflictKey::GraphOverlay(graph, global),
				ConflictKey::GraphIncidence(graph, local),
			] {
				self.require_live_graph_key(key)?;
			}
		}
		Ok(())
	}

	/// Resolves only already accepted catalog child bindings.
	fn live_graph_child(
		&mut self,
		image: &LiveGraphImage,
		child: Option<&LiveGraphScopeId>,
	) -> FerrosGrynnResult<Option<GraphId>> {
		let Some(child) = child else {
			return Ok(None);
		};
		let graph = image
			.scopes
			.get(child)
			.and_then(|scope| scope.source.as_ref())
			.ok_or_else(unmapped)?
			.id();
		self.require_live_graph_definition(graph)?;
		Ok(Some(graph))
	}

	/// Converts a qualified route using logical owners, not snapshot-local
	/// slots.
	fn live_graph_endpoint(
		&mut self,
		image: &LiveGraphImage,
		endpoint: &LiveGraphEndpoint,
		origin: GraphId,
		budget: &mut Budget,
		check_binding: bool,
	) -> FerrosGrynnResult<EdgeEndpoint> {
		budget.admit(256 + endpoint.owners.len() * 128)?;
		let mut graph = origin;
		let mut owners = Vec::new();
		for owner in &endpoint.owners {
			budget.admit(0)?;
			let scope = image.scopes.get(owner.scope()).ok_or_else(unmapped)?;
			if scope.source.as_ref().map(|source| source.id()) != Some(graph) {
				return Err(unmapped());
			}
			let global = self.working.resolve_atom(owner.atom().id()).ok_or_else(unmapped)?;
			let atom = self.working.atom(global).ok_or_else(unmapped)?;
			let image_owner = Atom::get(scope, owner.atom()).ok_or_else(unmapped)?;
			if atom.kind() != owner.atom().kind()
				|| atom.class_id() != image_owner.source()?.class()
			{
				return Err(stale(self));
			}
			self.require_live_graph_atom(global, Some(graph))?;
			owners.push(self.working.membership(graph, global).ok_or_else(unmapped)?);
			let child = match Atom::get(scope, owner.atom()).ok_or_else(unmapped)? {
				Atom::Node(node) => node.child.as_ref(),
				Atom::Edge(edge) => edge.child.as_ref(),
			};
			let child = self.live_graph_child(image, child)?.ok_or_else(unmapped)?;
			if check_binding {
				let local = self.working.membership(graph, global).ok_or_else(unmapped)?;
				if self.working.occurrence(graph, local).and_then(|owner| owner.child_graph())
					!= Some(child)
				{
					return Err(stale(self));
				}
			}
			graph = child;
		}
		let scope = image.scopes.get(endpoint.atom.scope()).ok_or_else(unmapped)?;
		if scope.source.as_ref().map(|source| source.id()) != Some(graph) {
			return Err(unmapped());
		}
		let global = self.working.resolve_atom(endpoint.atom.atom().id()).ok_or_else(unmapped)?;
		if self.working.atom(global).is_none_or(|atom| atom.kind() != endpoint.atom.atom().kind()) {
			return Err(unmapped());
		}
		let source = Atom::get(scope, endpoint.atom.atom()).ok_or_else(unmapped)?.source()?;
		if self.working.atom(global).is_none_or(|atom| atom.class_id() != source.class()) {
			return Err(stale(self));
		}
		self.require_live_graph_atom(global, Some(graph))?;
		let local = self.working.membership(graph, global).ok_or_else(unmapped)?;
		if owners.is_empty() {
			Ok(EdgeEndpoint::Local(local))
		} else {
			Ok(EdgeEndpoint::Nested(QualifiedAtomId::new(owners, local)?))
		}
	}

	/// Builds one source state with the same incidence roles as common
	/// assembly.
	fn live_graph_state(
		&mut self,
		image: &LiveGraphImage,
		atom: Atom<'_>,
		graph: GraphId,
		budget: &mut Budget,
		check_binding: bool,
	) -> FerrosGrynnResult<OccurrenceState> {
		match atom {
			Atom::Node(node) => {
				Ok(OccurrenceState::node(self.live_graph_child(image, node.child.as_ref())?))
			}
			Atom::Edge(edge) => {
				let source = edge
					.inputs
					.iter()
					.map(|endpoint| {
						self.live_graph_endpoint(image, endpoint, graph, budget, check_binding)
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let target = edge
					.outputs
					.iter()
					.map(|endpoint| {
						self.live_graph_endpoint(image, endpoint, graph, budget, check_binding)
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let topology = match edge.direction {
					EdgeDirection::Directed => EdgeTopology::directed(source, target),
					EdgeDirection::Bidirectional => EdgeTopology::bidirectional(source, target),
					EdgeDirection::Undirected => EdgeTopology::undirected(source),
				};
				Ok(OccurrenceState::edge(
					topology,
					edge.weight,
					self.live_graph_child(image, edge.child.as_ref())?,
				))
			}
		}
	}

	/// Global atoms have no occurrence-only label, child, topology or weight
	/// edits.
	fn live_graph_global_state_equal(before: Atom<'_>, after: Atom<'_>) -> bool {
		match (before, after) {
			(Atom::Node(before), Atom::Node(after)) => before.child == after.child,
			(Atom::Edge(before), Atom::Edge(after)) => {
				before.child == after.child
					&& before.direction == after.direction
					&& before.inputs == after.inputs
					&& before.outputs == after.outputs
					&& match (before.weight, after.weight) {
						(None, None) => true,
						(Some(left), Some(right)) => same_number(left, right),
						_ => false,
					}
			}
			_ => false,
		}
	}

	/// Writes only changed names; occurrence removals hide inherited
	/// properties.
	fn write_live_graph_properties(
		&mut self,
		graph: Option<GraphId>,
		atom: &AtomRef,
		before: &Object,
		after: &Object,
	) -> FerrosGrynnResult<()> {
		for (name, value) in after {
			if before.get(name).is_some_and(|old| same_value(old, value)) {
				continue;
			}
			let key = self.intern_property(name.as_str())?;
			if let Some(graph) = graph {
				self.set_overlay(graph, atom.id(), key, value.clone())?;
			} else {
				self.set_base_property(atom.id(), key, value.clone())?;
			}
		}
		for (name, _) in before {
			if after.contains_key(name) {
				continue;
			}
			let key = self.working.dictionary.get(name.as_str()).ok_or_else(unmapped)?;
			if let Some(graph) = graph {
				self.hide_property(graph, atom.id(), key)?;
			} else {
				self.remove_base_property(atom.id(), key)?;
			}
		}
		Ok(())
	}
}
