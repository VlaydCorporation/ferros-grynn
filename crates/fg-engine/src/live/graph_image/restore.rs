//! Family-preserving restoration from logical images, without current primary
//! reads.

use std::collections::{BTreeMap, BTreeSet};

use fg_runtime::__internal::{
	GraphMutableInternal as _,
	GraphStateInternal as _,
	MaterializedGraphInternal as _,
	PropertyMapInternal as _,
	RecordIdInternal as _,
	ValueInternal as _,
};

use super::{
	AtomRef,
	EdgeDirection,
	FerrosGrynnError,
	FerrosGrynnResult,
	GraphRef,
	LiveGraphAtomId,
	LiveGraphEndpoint,
	LiveGraphImage,
	LiveGraphScopeId,
	Object,
	PropertyKeyId,
	Text,
	Versionstamp,
};
use crate::{
	graph::{
		AtomId,
		EdgeEndpoint,
		EdgeTopology,
		Frozen,
		Graph,
		GraphRead,
		MaterializedGraph,
		Mutable,
		QualifiedAtomId,
		SnapshotId,
		validate_topology_constraints,
	},
	live::LiveId,
	values::{PropertyMap, Uuid},
};

/// The existing checked scratch and work authority for owned graph copies.
type Admission<'a> = dyn FnMut(usize, u64) -> FerrosGrynnResult<()> + 'a;

/// Retains one restored child layout and its materialization-local identity
/// map.
struct Built<'a> {
	/// Retains the agreed graph revision independently of future publications.
	graph: Graph<Frozen>,
	/// Maps stable logical atoms to handles only within the restored child
	/// layout.
	ids: BTreeMap<&'a AtomRef, AtomId>,
}

/// Reports malformed graph state through the existing typed storage diagnostic.
fn invalid(message: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

/// Restores properties through the admitted SDK or image key registry.
fn properties(
	image: &LiveGraphImage,
	keys: Option<&BTreeMap<&str, PropertyKeyId>>,
	value: &Object,
	admit: &mut Admission<'_>,
) -> FerrosGrynnResult<PropertyMap> {
	admit(
		value
			.len()
			.checked_mul(std::mem::size_of::<(PropertyKeyId, crate::values::Value)>())
			.ok_or_else(|| invalid("restored properties size overflows"))?,
		value.len() as u64,
	)?;
	let mut result = PropertyMap::new();
	for (name, value) in value {
		let key = keys
			.map_or_else(
				|| image.property_key(name.as_str()),
				|keys| keys.get(name.as_str()).copied(),
			)
			.ok_or_else(|| invalid("restored property handle is absent"))?;
		admit(value.estimated_heap_bytes(), 1)?;
		result.insert(key, value.clone());
	}
	Ok(result)
}

/// Reuses a built child while admitting each additional owned COW handle.
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
fn clone_child(
	key: Option<&LiveGraphScopeId>,
	built: &BTreeMap<&LiveGraphScopeId, Built<'_>>,
	admit: &mut Admission<'_>,
) -> FerrosGrynnResult<Option<Graph<Frozen>>> {
	let Some(key) = key else {
		return Ok(None);
	};
	let child = built.get(key).ok_or_else(|| invalid("restored child scope is not ready"))?;
	admit(
		child
			.graph
			.properties()
			.estimated_heap_bytes()
			.checked_add(std::mem::size_of::<Graph<Frozen>>())
			.and_then(|bytes| {
				bytes.checked_add(
					child
						.graph
						.graph_ref()
						.map_or(0, |source| source.name().len() + std::mem::size_of::<GraphRef>()),
				)
			})
			.ok_or_else(|| invalid("restored child metadata size overflows"))?,
		1,
	)?;
	Ok(Some(child.graph.clone()))
}

/// Resolves every qualified owner and terminal through the checked logical
/// scope map.
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
fn endpoint(
	value: &LiveGraphEndpoint,
	scope: &LiveGraphScopeId,
	local: &BTreeMap<&AtomRef, AtomId>,
	built: &BTreeMap<&LiveGraphScopeId, Built<'_>>,
	admit: &mut Admission<'_>,
) -> FerrosGrynnResult<EdgeEndpoint> {
	let id = |target: &LiveGraphAtomId| -> FerrosGrynnResult<AtomId> {
		if &target.scope == scope {
			local.get(&target.atom).copied()
		} else {
			built.get(&target.scope).and_then(|graph| graph.ids.get(&target.atom)).copied()
		}
		.ok_or_else(|| invalid("restored endpoint identity is absent"))
	};
	let terminal = id(&value.atom)?;
	if value.owners.is_empty() {
		return Ok(EdgeEndpoint::Local(terminal));
	}
	admit(
		value
			.owners
			.len()
			.checked_mul(std::mem::size_of::<AtomId>())
			.ok_or_else(|| invalid("restored endpoint owners size overflows"))?,
		value.owners.len() as u64,
	)?;
	let route = value.owners.iter().map(&id).collect::<FerrosGrynnResult<Vec<_>>>()?;
	Ok(EdgeEndpoint::Nested(QualifiedAtomId::new(route, terminal)?))
}

impl LiveGraphImage {
	/// Restores a complete candidate using existing layouts and all family
	/// validators.
	#[cfg(test)]
	pub(crate) fn restore(
		&self,
		id: LiveId,
		revision: Versionstamp,
		admit: &mut Admission<'_>,
	) -> FerrosGrynnResult<MaterializedGraph> {
		self.restore_with_keys(id, revision, None, admit)
	}

	/// SDK mutable handles keep their admitted local key namespace across
	/// commits.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash \
		          identities"
	)]
	pub(crate) fn restore_with_keys(
		&self,
		id: LiveId,
		revision: Versionstamp,
		names: Option<&BTreeMap<PropertyKeyId, Text>>,
		admit: &mut Admission<'_>,
	) -> FerrosGrynnResult<MaterializedGraph> {
		let keys = names
			.map(|names| -> FerrosGrynnResult<BTreeMap<&str, PropertyKeyId>> {
				admit(
					names
						.len()
						.checked_mul(128)
						.ok_or_else(|| invalid("SDK name index overflows"))?,
					names.len() as u64,
				)?;
				Ok(names.iter().map(|(key, name)| (name.as_str(), *key)).collect())
			})
			.transpose()?;
		self.validate_structure(admit)?;
		admit(
			self.scopes
				.len()
				.checked_mul(768)
				.ok_or_else(|| invalid("restore scope index size overflows"))?,
			self.scopes.len() as u64,
		)?;
		let mut remaining = BTreeMap::new();
		let mut parents = BTreeMap::<&LiveGraphScopeId, BTreeSet<&LiveGraphScopeId>>::new();
		for (key, scope) in &self.scopes {
			let mut children = BTreeSet::new();
			for child in scope
				.nodes
				.values()
				.filter_map(|node| node.child.as_ref())
				.chain(scope.edges.values().filter_map(|edge| edge.child.as_ref()))
			{
				admit(128, 1)?;
				children.insert(child);
				parents.entry(child).or_default().insert(key);
			}
			remaining.insert(key, children.len());
		}
		let mut ready = remaining
			.iter()
			.filter_map(|(key, count)| (*count == 0).then_some(*key))
			.collect::<Vec<_>>();
		let mut built = BTreeMap::new();
		while let Some(key) = ready.pop() {
			let scope = self.scopes.get(key).ok_or_else(|| invalid("restore scope disappeared"))?;
			admit(
				scope
					.nodes
					.len()
					.checked_add(scope.edges.len())
					.and_then(|count| count.checked_mul(512))
					.ok_or_else(|| invalid("restored layout size overflows"))?,
				1,
			)?;
			let identity = if key == &LiveGraphScopeId::Root {
				id.as_uuid()
			} else {
				Uuid::now_v7()
			};
			let graph_properties = properties(self, keys.as_ref(), &scope.properties, admit)?;
			let mut graph = Graph::<Mutable>::begin_live_materialization(
				identity,
				revision.get(),
				graph_properties,
				scope.nodes.len(),
				scope.edges.len(),
			);
			graph.set_topology_constraints(scope.validators);
			let mut ids = BTreeMap::new();
			let mut source_atoms = BTreeMap::new();
			for (atom, node) in &scope.nodes {
				admit(
					atom.id()
						.estimated_heap_bytes()
						.checked_mul(2)
						.and_then(|bytes| {
							bytes.checked_add(node.label.as_ref().map_or(0, |label| label.len()))
						})
						.and_then(|bytes| bytes.checked_add(256))
						.ok_or_else(|| invalid("restored node size overflows"))?,
					1,
				)?;
				let child = clone_child(node.child.as_ref(), &built, admit)?;
				let values = properties(self, keys.as_ref(), &node.properties, admit)?;
				let local = graph.materialize_query_node(
					atom.id().clone(),
					node.label.clone(),
					values,
					child,
				)?;
				ids.insert(atom, local);
				if node.source.is_some() {
					source_atoms.insert(local, atom.clone());
				}
			}
			for (atom, edge) in &scope.edges {
				admit(
					atom.id()
						.estimated_heap_bytes()
						.checked_mul(2)
						.and_then(|bytes| {
							bytes.checked_add(edge.label.as_ref().map_or(0, |label| label.len()))
						})
						.and_then(|bytes| bytes.checked_add(256))
						.ok_or_else(|| invalid("restored edge size overflows"))?,
					1,
				)?;
				let child = clone_child(edge.child.as_ref(), &built, admit)?;
				let values = properties(self, keys.as_ref(), &edge.properties, admit)?;
				let local = graph.reserve_query_edge(
					atom.id().clone(),
					edge.label.clone(),
					values,
					edge.weight,
					child,
				)?;
				ids.insert(atom, local);
				if edge.source.is_some() {
					source_atoms.insert(local, atom.clone());
				}
			}
			for (atom, edge) in &scope.edges {
				admit(
					edge.inputs
						.len()
						.checked_add(edge.outputs.len())
						.and_then(|count| {
							count.checked_mul(2 * std::mem::size_of::<EdgeEndpoint>())
						})
						.ok_or_else(|| invalid("restored topology size overflows"))?,
					1,
				)?;
				let inputs = edge
					.inputs
					.iter()
					.map(|value| endpoint(value, key, &ids, &built, admit))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let outputs = edge
					.outputs
					.iter()
					.map(|value| endpoint(value, key, &ids, &built, admit))
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
				let topology = match edge.direction {
					EdgeDirection::Directed => EdgeTopology::directed(inputs, outputs),
					EdgeDirection::Bidirectional => EdgeTopology::bidirectional(inputs, outputs),
					EdgeDirection::Undirected => EdgeTopology::undirected(inputs),
					_ => return Err(invalid("unsupported restored edge direction")),
				};
				graph.materialize_query_edge_topology(
					*ids.get(atom).ok_or_else(|| invalid("reserved edge disappeared"))?,
					topology,
				)?;
			}
			let provenance = scope.source.as_ref().map_or(identity, |source| {
				uuid::Uuid::from_u128(u128::from(source.id().get())).into()
			});
			admit(
				scope
					.source
					.as_ref()
					.map_or(0, |source| source.name().len() + std::mem::size_of::<GraphRef>()),
				1,
			)?;
			graph.finish_query_materialization_parts(
				provenance,
				SnapshotId::in_memory(revision.get()),
				scope.source.clone(),
				source_atoms,
			);
			graph.validate_owned_topology()?;
			validate_topology_constraints(&graph, scope.validators)?;
			built.insert(key, Built {
				graph: graph.freeze(),
				ids,
			});
			if let Some(parents) = parents.get(key) {
				for parent in parents {
					let count = remaining
						.get_mut(parent)
						.ok_or_else(|| invalid("restore parent disappeared"))?;
					*count = count
						.checked_sub(1)
						.ok_or_else(|| invalid("restore dependency count underflows"))?;
					if *count == 0 {
						ready.push(parent);
					}
				}
			}
		}
		let root = built
			.remove(&LiveGraphScopeId::Root)
			.ok_or_else(|| invalid("restored root scope is absent"))?;
		let root_id = self
			.root
			.as_ref()
			.map(|atom| {
				root.ids.get(atom).copied().ok_or_else(|| invalid("restored ROOT node is absent"))
			})
			.transpose()?;
		admit(self.estimated_heap_bytes()?, 1)?;
		MaterializedGraph::from_general(root.graph, self.family, root_id)
	}
}
