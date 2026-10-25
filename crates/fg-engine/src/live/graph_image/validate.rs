//! Logical closure, containment and identity checks before restoration or
//! persistence.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	AtomRef,
	EdgeDirection,
	FerrosGrynnError,
	FerrosGrynnResult,
	GraphKind,
	LiveGraphEndpoint,
	LiveGraphImage,
	LiveGraphScope,
	LiveGraphScopeId,
	Object,
};
use crate::graph::AtomKind;

/// Reports malformed graph state through the existing typed storage diagnostic.
fn invalid(message: &str) -> FerrosGrynnError {
	FerrosGrynnError::invalid_storage_file(message)
}

impl LiveGraphScope {
	/// Checks stable kind and record membership within this logical scope.
	fn contains(&self, atom: &AtomRef) -> bool {
		if fg_runtime::__internal::is_node(&(atom.kind())) {
			self.nodes.contains_key(atom)
		} else {
			self.edges.contains_key(atom)
		}
	}

	/// Resolves the shared child identity for either kind of containment owner.
	fn child_of(&self, atom: &AtomRef) -> Option<&LiveGraphScopeId> {
		if fg_runtime::__internal::is_node(&(atom.kind())) {
			self.nodes.get(atom).and_then(|node| node.child.as_ref())
		} else {
			self.edges.get(atom).and_then(|edge| edge.child.as_ref())
		}
	}
}

impl LiveGraphImage {
	/// Checks the complete logical graph before any family-specific runtime
	/// allocation.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash \
		          identities"
	)]
	pub(crate) fn validate_structure(
		&self,
		admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		let root = self
			.scopes
			.get(&LiveGraphScopeId::Root)
			.ok_or_else(|| invalid("graph image has no root scope"))?;
		if self.validators != root.validators
			|| (self.family == GraphKind::RootedTree) != self.root.is_some()
			|| self
				.root
				.as_ref()
				.is_some_and(|atom| atom.kind() != AtomKind::Node || !root.nodes.contains_key(atom))
		{
			return Err(invalid("graph image family/root contract is invalid"));
		}
		admit(
			self.scopes
				.len()
				.checked_mul(768)
				.ok_or_else(|| invalid("graph scope validation bytes overflow"))?,
			self.scopes.len() as u64,
		)?;
		let mut incoming = BTreeMap::new();
		let mut links = BTreeMap::<&LiveGraphScopeId, BTreeSet<&LiveGraphScopeId>>::new();
		let mut names = BTreeSet::new();
		for key in self.property_keys.values() {
			admit(128, 1)?;
			if !names.insert(*key) {
				return Err(invalid("graph image property handles are not unique"));
			}
		}
		for key in self.scopes.keys() {
			incoming.insert(key, 0usize);
		}
		for (key, scope) in &self.scopes {
			admit(0, 1)?;
			match key {
				LiveGraphScopeId::Root => {}
				LiveGraphScopeId::Source(id)
					if scope.source.as_ref().is_some_and(|source| source.id() == *id) => {}
				LiveGraphScopeId::Derived(path)
					if !path.is_empty() && path.len() <= 64 && scope.source.is_none() => {}
				_ => return Err(invalid("graph image scope/source identity is invalid")),
			}
			self.validate_properties(&scope.properties, admit)?;
			for (atom, node) in &scope.nodes {
				admit(0, 1)?;
				if atom.kind() != AtomKind::Node {
					return Err(invalid("graph image node has another kind"));
				}
				self.validate_properties(&node.properties, admit)?;
			}
			for (atom, edge) in &scope.edges {
				admit(0, 1)?;
				if atom.kind() != AtomKind::Edge
					|| edge.direction == EdgeDirection::Undirected && !edge.outputs.is_empty()
				{
					return Err(invalid("graph image edge kind/direction is invalid"));
				}
				self.validate_properties(&edge.properties, admit)?;
				for endpoint in edge.inputs.iter().chain(&edge.outputs) {
					self.validate_endpoint(key, endpoint, admit)?;
				}
			}
			for child in scope
				.nodes
				.values()
				.filter_map(|node| node.child.as_ref())
				.chain(scope.edges.values().filter_map(|edge| edge.child.as_ref()))
			{
				admit(128 + child.estimated_heap_bytes(), 1)?;
				let target = incoming
					.get_mut(child)
					.ok_or_else(|| invalid("graph image child scope is absent"))?;
				if links.entry(key).or_default().insert(child) {
					*target = target
						.checked_add(1)
						.ok_or_else(|| invalid("graph image indegree overflows"))?;
				}
			}
		}
		if incoming.iter().any(|(key, count)| **key != LiveGraphScopeId::Root && *count == 0) {
			return Err(invalid("graph image has an orphan scope"));
		}
		let mut ready = incoming
			.iter()
			.filter_map(|(key, count)| (*count == 0).then_some(*key))
			.collect::<Vec<_>>();
		let mut visited = 0usize;
		while let Some(key) = ready.pop() {
			admit(0, 1)?;
			visited += 1;
			if let Some(children) = links.get(key) {
				for child in children {
					let count = incoming
						.get_mut(child)
						.ok_or_else(|| invalid("graph image containment scope disappeared"))?;
					*count = count
						.checked_sub(1)
						.ok_or_else(|| invalid("graph image indegree underflows"))?;
					if *count == 0 {
						ready.push(child);
					}
				}
			}
		}
		if visited != self.scopes.len() {
			return Err(invalid("graph image containment has a cycle"));
		}
		Ok(())
	}

	/// Rejects properties that have no name in the image dictionary.
	fn validate_properties(
		&self,
		properties: &Object,
		admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		for name in properties.keys() {
			admit(0, 1)?;
			if !self.property_keys.contains_key(name) {
				return Err(invalid("graph image property has no logical handle"));
			}
		}
		Ok(())
	}

	/// Checks every qualified owner binding and terminal occurrence against the
	/// image.
	fn validate_endpoint(
		&self,
		edge_scope: &LiveGraphScopeId,
		endpoint: &LiveGraphEndpoint,
		admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<()> {
		if endpoint.owners.len() > 64 {
			return Err(invalid("graph image endpoint route exceeds 64"));
		}
		let mut current = edge_scope;
		for owner in &endpoint.owners {
			admit(0, 1)?;
			if &owner.scope != current {
				return Err(invalid("graph image owner scope is disconnected"));
			}
			let graph = self
				.scopes
				.get(current)
				.ok_or_else(|| invalid("graph image owner scope is absent"))?;
			if !graph.contains(&owner.atom) {
				return Err(invalid("graph image endpoint owner is absent"));
			}
			current = graph
				.child_of(&owner.atom)
				.ok_or_else(|| invalid("graph image endpoint owner has no child"))?;
		}
		admit(0, 1)?;
		if current != &endpoint.atom.scope
			|| self.scopes.get(current).is_none_or(|scope| !scope.contains(&endpoint.atom.atom))
		{
			return Err(invalid("graph image terminal endpoint is absent or in another scope"));
		}
		Ok(())
	}
}
