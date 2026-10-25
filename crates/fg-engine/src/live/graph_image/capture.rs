//! Fixed-root capture with one owned copy of every logical child scope.

use std::{mem::size_of, sync::Arc};

use fg_runtime::__internal::{RecordIdInternal as _, ValueInternal as _};

use super::{
	AtomRef,
	BTreeMap,
	ChangeIdentity,
	GraphRef,
	GraphTopologyConstraints,
	LiveGraphAtomId,
	LiveGraphEdge,
	LiveGraphEndpoint,
	LiveGraphImage,
	LiveGraphNode,
	LiveGraphScope,
	LiveGraphScopeId,
	LiveGraphSource,
	Object,
	PropertyKeyId,
	Text,
};
use crate::{
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomId,
		AtomKind,
		EdgeEndpoint,
		EdgeView,
		Frozen,
		Graph,
		GraphRead,
		MaterializationOrigin,
		MaterializedGraph,
		NodeView,
	},
	values::{PropertyDictionary, PropertyMap, Uuid},
};

/// The existing checked scratch and work authority for owned graph copies.
type Admission<'a> = dyn Fn(usize, u64) -> FerrosGrynnResult<()> + 'a;

/// The stable source-mapping contract used during logical graph capture.
type Mapping<'a> =
	dyn Fn(&AtomRef, Option<&GraphRef>) -> FerrosGrynnResult<Option<LiveGraphSource>> + 'a;

/// Borrows either the selected root family or one immutable general child.
#[derive(Clone, Copy)]
enum Reader<'a> {
	/// Borrows the selected top-level concrete result family.
	Root(
		/// Borrows the selected top-level concrete result family.
		&'a MaterializedGraph,
	),
	/// Borrows one immutable child retained by a containment owner.
	Child(
		/// Borrows one immutable child retained by a containment owner.
		&'a Graph<Frozen>,
	),
}

impl<'a> Reader<'a> {
	/// Identifies a retained layout for shared-child traversal deduplication.
	fn physical_identity(self) -> usize {
		match self {
			Self::Root(graph) => std::ptr::from_ref(graph) as usize,
			Self::Child(graph) => std::ptr::from_ref(graph) as usize,
		}
	}

	/// Returns the concrete scope topology contract.
	fn validators(self) -> GraphTopologyConstraints {
		match self {
			Self::Root(graph) => graph.topology_constraints(),
			Self::Child(graph) => graph.topology_constraints(),
		}
	}

	/// Borrows the scope source identity and logical atom mapping.
	fn origin(self) -> Option<&'a MaterializationOrigin> {
		match self {
			Self::Root(graph) => graph.materialization_origin(),
			Self::Child(graph) => graph.materialization_origin(),
		}
	}

	/// Projects admitted property handles into owned names without consulting
	/// current data.
	fn properties(self) -> &'a PropertyMap {
		match self {
			Self::Root(graph) => graph.properties(),
			Self::Child(graph) => graph.properties(),
		}
	}

	/// Visits the concrete scope nodes without copying the runtime layout.
	fn nodes(self) -> FerrosGrynnResult<Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>> {
		match self {
			Self::Root(graph) => graph.node_ids(),
			Self::Child(graph) => Ok(Box::new(graph.node_ids()?)),
		}
	}

	/// Visits the concrete scope connectors without copying their topology.
	fn edges(self) -> FerrosGrynnResult<Box<dyn Iterator<Item = FerrosGrynnResult<AtomId>> + 'a>> {
		match self {
			Self::Root(graph) => graph.edge_ids(),
			Self::Child(graph) => Ok(Box::new(graph.edge_ids()?)),
		}
	}

	/// Borrows one checked node view from the concrete scope.
	fn node(self, id: AtomId) -> Option<NodeView<'a>> {
		match self {
			Self::Root(graph) => graph.node(id),
			Self::Child(graph) => graph.node(id),
		}
	}

	/// Borrows one checked connector view from the concrete scope.
	fn edge(self, id: AtomId) -> Option<EdgeView<'a>> {
		match self {
			Self::Root(graph) => graph.edge(id),
			Self::Child(graph) => graph.edge(id),
		}
	}

	/// Borrows the immutable layout bound to a node or edge owner.
	fn child(self, id: AtomId) -> Option<&'a Graph<Frozen>> {
		match (id.kind()).map(|kind| fg_runtime::__internal::is_node(&kind)) {
			Some(true) => self.node(id).and_then(|node| node.inner_graph()),
			Some(false) => self.edge(id).and_then(|edge| edge.transition_graph()),
			None => None,
		}
	}

	/// Admits and resolves the stable record identity of a
	/// materialization-local handle.
	fn atom(self, id: AtomId, admission: &Admission<'_>) -> FerrosGrynnResult<AtomRef> {
		let (kind, record) = match id.kind() {
			Some(AtomKind::Node) => (AtomKind::Node, self.node(id).map(|node| node.record_id())),
			Some(AtomKind::Edge) => (AtomKind::Edge, self.edge(id).map(|edge| edge.record_id())),
			None => return Err(invalid("graph image atom kind is invalid")),

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported runtime variant at the workspace boundary.",
				));
			}
		};
		let record = record.ok_or_else(|| invalid("graph image atom is absent"))?;
		(admission)(
			record
				.estimated_heap_bytes()
				.checked_add(size_of::<AtomRef>())
				.ok_or_else(|| invalid("graph atom size overflows"))?,
			1,
		)?;
		Ok(AtomRef::new(kind, record.clone()))
	}
}

/// Query capture uses a dense source dictionary; mutable SDK keys are sparse.
enum Names<'a> {
	/// Resolves source handles through the accepted datastore dictionary.
	Dictionary(
		/// Resolves source handles through the accepted datastore dictionary.
		&'a PropertyDictionary,
	),
	/// Resolves stable SDK handles through its private sparse registry.
	Owned(
		/// Resolves stable SDK handles through its private sparse registry.
		&'a BTreeMap<PropertyKeyId, Text>,
	),
}

impl Names<'_> {
	/// Resolves a materialization property handle through its owning name
	/// registry.
	fn resolve(&self, key: PropertyKeyId) -> Option<&Text> {
		match self {
			Self::Dictionary(dictionary) => dictionary.resolve(key),
			Self::Owned(names) => names.get(&key),
		}
	}
}

/// Owns only admitted logical names and scope identities during capture.
struct Capture<'a> {
	/// Owns the stable SDK property handles independently of source dictionary
	/// IDs.
	names: Names<'a>,
	/// Borrows the common pre-copy resource admission authority.
	admission: &'a Admission<'a>,
	/// Resolves stable write-back sources without consulting local slot
	/// identity.
	mapping: &'a Mapping<'a>,
	/// Owns the self-contained logical property-name dictionary.
	keys: BTreeMap<Text, PropertyKeyId>,
	/// Deduplicates computed child identities by their canonical owner routes.
	derived: BTreeMap<Uuid, LiveGraphScopeId>,
}

impl Capture<'_> {
	/// Projects admitted property handles into owned names without consulting
	/// current data.
	fn properties(&mut self, map: &PropertyMap) -> FerrosGrynnResult<Object> {
		let mut output = Object::new();
		for (key, value) in map.iter() {
			let name = self
				.names
				.resolve(key)
				.ok_or_else(|| invalid("graph image property name is absent"))?;
			(self.admission)(
				name.len()
					.checked_mul(2)
					.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
					.and_then(|bytes| bytes.checked_add(512))
					.ok_or_else(|| invalid("graph image property size overflows"))?,
				1,
			)?;
			self.keys.insert(name.clone(), key);
			output.insert(name.clone(), value.clone());
		}
		Ok(output)
	}

	/// Assigns a shared source identity or bounded canonical derived owner
	/// route.
	fn child_id(
		&mut self,
		parent: &LiveGraphScopeId,
		reader: Reader<'_>,
		owner: &AtomRef,
		child: &Graph<Frozen>,
	) -> FerrosGrynnResult<LiveGraphScopeId> {
		if let Some(source) =
			child.materialization_origin().and_then(MaterializationOrigin::graph_ref)
		{
			return Ok(LiveGraphScopeId::Source(source.id()));
		}
		if let Some(key) = self.derived.get(&child.identity()) {
			(self.admission)(key.estimated_heap_bytes(), 1)?;
			return Ok(key.clone());
		}
		(self.admission)(parent.estimated_heap_bytes(), 1)?;
		let mut path = match parent {
			LiveGraphScopeId::Derived(path) => path.clone(),
			_ => Vec::new(),
		};
		(self.admission)(
			owner
				.id()
				.estimated_heap_bytes()
				.checked_mul(2)
				.and_then(|bytes| bytes.checked_add(512 + size_of::<ChangeIdentity>()))
				.ok_or_else(|| invalid("derived graph identity size overflows"))?,
			1,
		)?;
		let identity = match reader.origin().and_then(MaterializationOrigin::graph_ref) {
			Some(graph) => ChangeIdentity::Occurrence(crate::graph::AtomOccurrenceRef::new(
				owner.clone(),
				Arc::new(graph.clone()),
			)),
			None => ChangeIdentity::Record(owner.clone()),
		};
		path.push(identity);
		let key = LiveGraphScopeId::Derived(path);
		(self.admission)(key.estimated_heap_bytes(), 1)?;
		self.derived.insert(child.identity(), key.clone());
		Ok(key)
	}

	/// Captures the stable source mapping independently of local slot handles.
	fn source(
		&self,
		reader: Reader<'_>,
		id: AtomId,
		atom: &AtomRef,
	) -> FerrosGrynnResult<Option<LiveGraphSource>> {
		let Some(origin) = reader.origin() else {
			return Ok(None);
		};
		if origin.source_atom(id) != Some(atom) {
			return Ok(None);
		}
		(self.mapping)(atom, origin.graph_ref())
	}

	/// Resolves every qualified owner and terminal through the checked logical
	/// scope map.
	fn endpoint(
		&mut self,
		reader: Reader<'_>,
		scope: &LiveGraphScopeId,
		endpoint: &EdgeEndpoint,
	) -> FerrosGrynnResult<LiveGraphEndpoint> {
		let (route, terminal) = match endpoint {
			EdgeEndpoint::Local(id) => (&[][..], *id),
			EdgeEndpoint::Nested(qualified) => (qualified.scope(), qualified.atom()),

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported runtime variant at the workspace boundary.",
				));
			}
		};
		if route.len() > 64 {
			return Err(invalid("graph image containment route exceeds 64"));
		}
		(self.admission)(
			route
				.len()
				.checked_mul(2 * size_of::<LiveGraphAtomId>())
				.ok_or_else(|| invalid("graph route size overflows"))?,
			route.len() as u64,
		)?;
		let mut owners = Vec::new();
		let mut current = reader;
		(self.admission)(scope.estimated_heap_bytes(), 1)?;
		let mut current_scope = scope.clone();
		for owner in route {
			let atom = current.atom(*owner, self.admission)?;
			let child = current
				.child(*owner)
				.ok_or_else(|| invalid("graph image endpoint owner has no child"))?;
			let next_scope = self.child_id(&current_scope, current, &atom, child)?;
			owners.push(LiveGraphAtomId {
				scope: current_scope,
				atom,
			});
			current = Reader::Child(child);
			current_scope = next_scope;
		}
		Ok(LiveGraphEndpoint {
			atom: LiveGraphAtomId {
				scope: current_scope,
				atom: current.atom(terminal, self.admission)?,
			},
			owners,
		})
	}
}

impl LiveGraphImage {
	/// Copies only this validated result's logical content and admitted source
	/// mappings.
	pub(crate) fn capture(
		graph: &MaterializedGraph,
		dictionary: &PropertyDictionary,
		admission: &Admission<'_>,
		mapping: &Mapping<'_>,
	) -> FerrosGrynnResult<Self> {
		Self::capture_named(graph, Names::Dictionary(dictionary), admission, mapping)
	}

	/// Uses the SDK's snapshot-local sparse keys without consulting primary
	/// data.
	pub(crate) fn capture_with_names(
		graph: &MaterializedGraph,
		names: &BTreeMap<PropertyKeyId, Text>,
		admission: &Admission<'_>,
		mapping: &Mapping<'_>,
	) -> FerrosGrynnResult<Self> {
		Self::capture_named(graph, Names::Owned(names), admission, mapping)
	}

	/// Captures each logical scope once after admitting its owned
	/// representation.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash \
		          identities"
	)]
	fn capture_named(
		graph: &MaterializedGraph,
		names: Names<'_>,
		admission: &Admission<'_>,
		mapping: &Mapping<'_>,
	) -> FerrosGrynnResult<Self> {
		let mut capture = Capture {
			names,
			admission,
			mapping,
			keys: BTreeMap::new(),
			derived: BTreeMap::new(),
		};
		let root = graph
			.as_rooted_tree()
			.map(|tree| tree.root())
			.map(|id| Reader::Root(graph).atom(id, capture.admission))
			.transpose()?;
		let mut pending = BTreeMap::new();
		(capture.admission)(512, 1)?;
		pending.insert(
			(LiveGraphScopeId::Root, Reader::Root(graph).physical_identity()),
			Reader::Root(graph),
		);
		let mut visited = std::collections::BTreeSet::new();
		let mut scopes = BTreeMap::new();
		while let Some((current, reader)) = pending.pop_first() {
			if visited.contains(&current) {
				continue;
			}
			let (scope, physical) = current;
			(capture.admission)(256 + scope.estimated_heap_bytes(), 1)?;
			visited.insert((scope.clone(), physical));
			(capture.admission)(512 + size_of::<LiveGraphScope>(), 1)?;
			let source = reader.origin().and_then(MaterializationOrigin::graph_ref).cloned();
			let properties = capture.properties(reader.properties())?;
			let mut ids = BTreeMap::new();
			for id in reader.nodes()?.chain(reader.edges()?) {
				let id = id?;
				let atom = reader.atom(id, capture.admission)?;
				(capture.admission)(256, 1)?;
				ids.insert(atom, id);
			}
			let mut nodes = BTreeMap::new();
			let mut edges = BTreeMap::new();
			for (atom, id) in ids {
				let child = reader
					.child(id)
					.map(|child| {
						let key = capture.child_id(&scope, reader, &atom, child)?;
						let child_reader = Reader::Child(child);
						let physical = child_reader.physical_identity();
						(capture.admission)(key.estimated_heap_bytes(), 1)?;
						let pending_key = (key.clone(), physical);
						if !visited.contains(&pending_key) && !pending.contains_key(&pending_key) {
							(capture.admission)(
								512usize
									.checked_add(key.estimated_heap_bytes())
									.ok_or_else(|| invalid("graph scope size overflows"))?,
								1,
							)?;
							pending.insert(pending_key, child_reader);
						}
						Ok::<_, FerrosGrynnError>(key)
					})
					.transpose()?;
				let source = capture.source(reader, id, &atom)?;
				match atom.kind() {
					AtomKind::Node => {
						let node =
							reader.node(id).ok_or_else(|| invalid("graph image node is absent"))?;
						(capture.admission)(256 + node.label().map_or(0, |label| label.len()), 1)?;
						let properties = capture.properties(node.properties())?;
						nodes.insert(atom, LiveGraphNode {
							label: node.label().map(Text::from),
							properties,
							child,
							source,
						});
					}
					AtomKind::Edge => {
						let edge =
							reader.edge(id).ok_or_else(|| invalid("graph image edge is absent"))?;
						(capture.admission)(256 + edge.label().map_or(0, |label| label.len()), 1)?;
						let properties = capture.properties(edge.properties())?;
						let topology = edge.topology();
						(capture.admission)(
							topology
								.endpoint_count()
								.checked_mul(2 * size_of::<LiveGraphEndpoint>())
								.ok_or_else(|| invalid("graph endpoints size overflows"))?,
							topology.endpoint_count() as u64,
						)?;
						let inputs = topology
							.source()
							.iter()
							.map(|endpoint| capture.endpoint(reader, &scope, endpoint))
							.collect::<FerrosGrynnResult<Vec<_>>>()?;
						let outputs = topology
							.target()
							.iter()
							.map(|endpoint| capture.endpoint(reader, &scope, endpoint))
							.collect::<FerrosGrynnResult<Vec<_>>>()?;
						edges.insert(atom, LiveGraphEdge {
							label: edge.label().map(Text::from),
							properties,
							direction: topology.direction(),
							inputs,
							outputs,
							weight: edge.weight(),
							child,
							source,
						});
					}

					_ => {
						return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
							"Unsupported runtime variant at the workspace boundary.",
						));
					}
				}
			}
			let image = LiveGraphScope {
				source,
				validators: reader.validators(),
				properties,
				nodes,
				edges,
			};
			if let Some(previous) = scopes.get(&scope) {
				if !super::delta::scope_equal(previous, &image) {
					return Err(invalid("shared logical graph scope has divergent owned images"));
				}
			} else {
				scopes.insert(scope, image);
			}
		}
		Ok(Self {
			family: graph.kind(),
			validators: graph.topology_constraints(),
			root,
			scopes,
			property_keys: capture.keys,
		})
	}
}

/// Reports malformed graph state through the existing typed storage diagnostic.
fn invalid(message: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

#[cfg(test)]
/// Regression tests for capture behavior.
mod tests {
	use super::*;
	use crate::graph::{GraphMut, Mutable};

	/// Verifies shared scope capture rejects divergent views even at equal
	/// revisions.
	#[test]
	fn shared_scope_capture_rejects_divergent_views_even_at_equal_revisions() {
		use crate::test_assertions::AssertResult as _;

		let key = PropertyKeyId::new(0);
		let names = BTreeMap::from([(key, Text::from("score"))]);
		let mut child = Graph::<Mutable>::new();
		let node = child.add_node(None, PropertyMap::from_iter([(key, 1.into())])).assert_ok();
		let child = child.freeze();
		let mut parent = Graph::<Mutable>::new();
		let first = parent.add_metavertex(child.clone(), None, PropertyMap::new()).assert_ok();
		let second = parent.add_metavertex(child.clone(), None, PropertyMap::new()).assert_ok();
		assert_ne!(first, second);
		let image = LiveGraphImage::capture_with_names(
			&MaterializedGraph::Graph(parent.freeze()),
			&names,
			&|_, _| Ok(()),
			&|_, _| Ok(None),
		)
		.assert_ok();
		assert_eq!(image.scopes.len(), 2);
		let mut left = child.clone().thaw();
		left.node_properties_mut(node).assert_ok().insert(key, 99);
		let mut right = child.thaw();
		right.node_properties_mut(node).assert_ok().insert(key, 100);
		assert_eq!(left.revision(), right.revision());
		let mut parent = Graph::<Mutable>::new();
		let first = parent.add_metavertex(left.freeze(), None, PropertyMap::new()).assert_ok();
		let second = parent.add_metavertex(right.freeze(), None, PropertyMap::new()).assert_ok();
		assert_ne!(first, second);
		assert!(
			LiveGraphImage::capture_with_names(
				&MaterializedGraph::Graph(parent.freeze()),
				&names,
				&|_, _| Ok(()),
				&|_, _| Ok(None)
			)
			.is_err()
		);
	}
}
