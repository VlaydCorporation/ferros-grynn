//! Owned logical graph images; materialization handles never cross this
//! boundary.

use std::collections::BTreeMap;

use crate::{
	changefeed::{ChangeIdentity, Versionstamp},
	datastore::ClassId,
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomRef, EdgeDirection, GraphId, GraphKind, GraphRef, GraphTopologyConstraints},
	values::{Number, Object, PropertyKeyId, Text},
};

mod capture;
mod delta;
mod merge;
mod restore;
mod validate;
pub use delta::{LiveGraphDelta, LiveGraphTarget};

/// One logical scope in a result, independent of materialization-local AtomId.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LiveGraphScopeId {
	/// The result's top-level graph, including global-node selections.
	Root,
	/// One catalog child shared by all of its containment owners.
	Source(
		/// One catalog child shared by all of its containment owners.
		GraphId,
	),
	/// A computed child identified by its canonical owner route.
	Derived(
		/// A computed child identified by its canonical owner route.
		Vec<ChangeIdentity>,
	),
}

impl LiveGraphScopeId {
	/// Conservative deep-copy admission for computed owner routes.
	pub(crate) fn estimated_heap_bytes(&self) -> usize {
		match self {
			Self::Root | Self::Source(_) => 0,
			Self::Derived(path) => path.iter().fold(
				path.len().saturating_mul(std::mem::size_of::<ChangeIdentity>()),
				|bytes, owner| {
					bytes.saturating_add(match owner {
						ChangeIdentity::Record(atom) => atom.id().estimated_heap_bytes(),
						ChangeIdentity::Occurrence(atom) => atom
							.atom()
							.id()
							.estimated_heap_bytes()
							.saturating_add(atom.graph().name().len())
							.saturating_add(std::mem::size_of::<GraphRef>() + 32),
					})
				},
			),
		}
	}
}

/// Qualified logical target used by graph deltas and mutable conflicts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LiveGraphAtomId {
	/// Identifies the logical graph namespace independently of physical slots.
	pub(crate) scope: LiveGraphScopeId,
	/// Owns the stable atom kind and record identity.
	pub(crate) atom: AtomRef,
}

impl LiveGraphAtomId {
	/// Borrows the graph scope containing this atom.
	pub const fn scope(&self) -> &LiveGraphScopeId {
		&self.scope
	}

	/// Borrows the stable kind and record identity.
	pub const fn atom(&self) -> &AtomRef {
		&self.atom
	}
}

/// Exact source namespace and accepted snapshot watermark for write-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveGraphSource {
	/// Retains the accepted stable class incarnation for source validation.
	pub(crate) class: ClassId,
	/// Stores the complete accepted durable graph watermark.
	pub(crate) revision: Versionstamp,
}

impl LiveGraphSource {
	/// Returns the captured class incarnation, independent of its display name.
	pub const fn class(&self) -> ClassId {
		self.class
	}

	/// Returns the durable source snapshot used by the accepted image.
	/// Write-back validates effective images and transaction predicates; this
	/// clock never substitutes for process-local FCW/SSI point revisions.
	pub const fn revision(&self) -> Versionstamp {
		self.revision
	}
}

/// An endpoint and its ordered node/edge containment route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGraphEndpoint {
	/// Owns the stable atom kind and record identity.
	pub(crate) atom: LiveGraphAtomId,
	/// Owns the ordered qualified containment route to the terminal endpoint.
	pub(crate) owners: Vec<LiveGraphAtomId>,
}

impl LiveGraphEndpoint {
	/// Borrows the terminal qualified atom.
	pub const fn atom(&self) -> &LiveGraphAtomId {
		&self.atom
	}

	/// Borrows every owner from connector scope to terminal scope.
	pub fn owners(&self) -> &[LiveGraphAtomId] {
		&self.owners
	}
}

/// Fully owned effective node image with an optional shared child scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGraphNode {
	/// Owns the effective occurrence label.
	pub(crate) label: Option<Text>,
	/// Owns effective properties by stable logical name.
	pub(crate) properties: Object,
	/// Links an owner to one shared logical child scope.
	pub(crate) child: Option<LiveGraphScopeId>,
	/// Retains the exact qualified write-back source accepted with this image.
	pub(crate) source: Option<LiveGraphSource>,
}

impl LiveGraphNode {
	/// Borrows the exact effective properties by logical name.
	pub const fn properties(&self) -> &Object {
		&self.properties
	}

	/// Borrows the occurrence label.
	pub const fn label(&self) -> Option<&Text> {
		self.label.as_ref()
	}

	/// Borrows the optional contained graph identity.
	pub const fn child(&self) -> Option<&LiveGraphScopeId> {
		self.child.as_ref()
	}

	/// Returns None when this result has no unambiguous write-back source.
	pub const fn source(&self) -> Option<LiveGraphSource> {
		self.source
	}
}

/// Fully owned incidence, including V–V, V–E, E–V and E–E endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGraphEdge {
	/// Owns the effective occurrence label.
	pub(crate) label: Option<Text>,
	/// Owns effective properties by stable logical name.
	pub(crate) properties: Object,
	/// Retains the original directed, bidirectional or undirected incidence.
	pub(crate) direction: EdgeDirection,
	/// Owns source endpoints, or every endpoint of an undirected connector.
	pub(crate) inputs: Vec<LiveGraphEndpoint>,
	/// Owns target endpoints in their original order.
	pub(crate) outputs: Vec<LiveGraphEndpoint>,
	/// Retains the exact optional stored connector weight.
	pub(crate) weight: Option<Number>,
	/// Links an owner to one shared logical child scope.
	pub(crate) child: Option<LiveGraphScopeId>,
	/// Retains the exact qualified write-back source accepted with this image.
	pub(crate) source: Option<LiveGraphSource>,
}

impl LiveGraphEdge {
	/// Borrows the exact effective properties by logical name.
	pub const fn properties(&self) -> &Object {
		&self.properties
	}

	/// Borrows the occurrence label.
	pub const fn label(&self) -> Option<&Text> {
		self.label.as_ref()
	}

	/// Returns the original incidence direction.
	pub const fn direction(&self) -> EdgeDirection {
		self.direction
	}

	/// Borrows source endpoints, or all endpoints of an undirected edge.
	pub fn inputs(&self) -> &[LiveGraphEndpoint] {
		&self.inputs
	}

	/// Borrows target endpoints; undirected edges have none.
	pub fn outputs(&self) -> &[LiveGraphEndpoint] {
		&self.outputs
	}

	/// Returns the exact stored weight.
	pub const fn weight(&self) -> Option<Number> {
		self.weight
	}

	/// Borrows the optional transition graph identity.
	pub const fn child(&self) -> Option<&LiveGraphScopeId> {
		self.child.as_ref()
	}

	/// Returns None when this result has no unambiguous write-back source.
	pub const fn source(&self) -> Option<LiveGraphSource> {
		self.source
	}
}

/// One owned scope; child scopes are stored once and linked by identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGraphScope {
	/// Retains the exact qualified write-back source accepted with this image.
	pub(crate) source: Option<GraphRef>,
	/// Retains the orthogonal topology contract for this scope.
	pub(crate) validators: GraphTopologyConstraints,
	/// Owns effective properties by stable logical name.
	pub(crate) properties: Object,
	/// Owns nodes in canonical stable-identity order.
	pub(crate) nodes: BTreeMap<AtomRef, LiveGraphNode>,
	/// Owns connectors in canonical stable-identity order.
	pub(crate) edges: BTreeMap<AtomRef, LiveGraphEdge>,
}

impl LiveGraphScope {
	/// Returns this scope's own topology contract, including nested scopes.
	pub const fn validators(&self) -> GraphTopologyConstraints {
		self.validators
	}

	/// Borrows its catalog source when one exists.
	pub const fn source(&self) -> Option<&GraphRef> {
		self.source.as_ref()
	}

	/// Borrows graph properties without consulting current primary data.
	pub const fn properties(&self) -> &Object {
		&self.properties
	}

	/// Iterates nodes in canonical stable-identity order.
	pub fn nodes(&self) -> impl ExactSizeIterator<Item = (&AtomRef, &LiveGraphNode)> {
		self.nodes.iter()
	}

	/// Iterates edges in canonical stable-identity order.
	pub fn edges(&self) -> impl ExactSizeIterator<Item = (&AtomRef, &LiveGraphEdge)> {
		self.edges.iter()
	}
}

/// Self-contained logical image used by checkpoints and complete graph deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGraphImage {
	/// Retains the concrete result family selected by the shared graph
	/// terminal.
	pub(crate) family: GraphKind,
	/// Retains the orthogonal topology contract for this scope.
	pub(crate) validators: GraphTopologyConstraints,
	/// Retains the authoritative logical ROOT node required by the selected
	/// family.
	pub(crate) root: Option<AtomRef>,
	/// Owns each logical scope once, including shared child scopes.
	pub(crate) scopes: BTreeMap<LiveGraphScopeId, LiveGraphScope>,
	/// Retains the self-contained property name and handle dictionary.
	pub(crate) property_keys: BTreeMap<Text, PropertyKeyId>,
}

impl LiveGraphImage {
	/// Accounts each logical scope once, independently of shared runtime
	/// layouts.
	pub(crate) fn estimated_heap_bytes(&self) -> FerrosGrynnResult<usize> {
		/// Returns add for `LiveGraphImage`.
		fn add(bytes: &mut usize, amount: usize) -> FerrosGrynnResult<()> {
			*bytes = bytes
				.checked_add(amount)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE graph image bytes"))?;
			Ok(())
		}
		/// Returns object for `LiveGraphImage`.
		fn object(value: &Object) -> FerrosGrynnResult<usize> {
			value.iter().try_fold(0usize, |bytes, (name, value)| {
				bytes
					.checked_add(256)
					.and_then(|bytes| bytes.checked_add(name.len()))
					.and_then(|bytes| bytes.checked_add(value.estimated_heap_bytes()))
					.ok_or_else(|| {
						FerrosGrynnError::operation_overflow("LIVE graph properties bytes")
					})
			})
		}
		/// Returns endpoint for `LiveGraphImage`.
		fn endpoint(value: &LiveGraphEndpoint) -> FerrosGrynnResult<usize> {
			value.owners.iter().chain(std::iter::once(&value.atom)).try_fold(
				128usize,
				|bytes, atom| {
					bytes
						.checked_add(std::mem::size_of::<LiveGraphAtomId>())
						.and_then(|bytes| bytes.checked_add(atom.scope.estimated_heap_bytes()))
						.and_then(|bytes| bytes.checked_add(atom.atom.id().estimated_heap_bytes()))
						.ok_or_else(|| {
							FerrosGrynnError::operation_overflow("LIVE graph endpoint bytes")
						})
				},
			)
		}
		let mut bytes = std::mem::size_of::<Self>();
		for name in self.property_keys.keys() {
			add(&mut bytes, 256)?;
			add(&mut bytes, name.len())?;
		}
		if let Some(root) = &self.root {
			add(&mut bytes, root.id().estimated_heap_bytes())?;
		}
		for (id, scope) in &self.scopes {
			add(&mut bytes, 256 + std::mem::size_of::<LiveGraphScope>())?;
			add(&mut bytes, id.estimated_heap_bytes())?;
			if let Some(source) = &scope.source {
				add(&mut bytes, source.name().len() + std::mem::size_of::<GraphRef>())?;
			}
			add(&mut bytes, object(&scope.properties)?)?;
			for (atom, node) in &scope.nodes {
				add(&mut bytes, 256 + std::mem::size_of::<LiveGraphNode>())?;
				add(&mut bytes, atom.id().estimated_heap_bytes())?;
				add(&mut bytes, node.label.as_ref().map_or(0, |label| label.len()))?;
				add(&mut bytes, object(&node.properties)?)?;
				if let Some(child) = &node.child {
					add(&mut bytes, child.estimated_heap_bytes())?;
				}
			}
			for (atom, edge) in &scope.edges {
				add(&mut bytes, 256 + std::mem::size_of::<LiveGraphEdge>())?;
				add(&mut bytes, atom.id().estimated_heap_bytes())?;
				add(&mut bytes, edge.label.as_ref().map_or(0, |label| label.len()))?;
				add(&mut bytes, object(&edge.properties)?)?;
				if let Some(child) = &edge.child {
					add(&mut bytes, child.estimated_heap_bytes())?;
				}
				for value in edge.inputs.iter().chain(&edge.outputs) {
					add(&mut bytes, endpoint(value)?)?;
				}
			}
		}
		Ok(bytes)
	}

	/// Returns the exact result family.
	pub const fn family(&self) -> GraphKind {
		self.family
	}

	/// Returns orthogonal topology validators.
	pub const fn validators(&self) -> GraphTopologyConstraints {
		self.validators
	}

	/// Borrows the authoritative ROOT node, when required by the family.
	pub const fn root(&self) -> Option<&AtomRef> {
		self.root.as_ref()
	}

	/// Borrows a checked logical scope.
	pub fn scope(&self, scope: &LiveGraphScopeId) -> Option<&LiveGraphScope> {
		self.scopes.get(scope)
	}

	/// Iterates scopes in canonical order.
	pub fn scopes(&self) -> impl ExactSizeIterator<Item = (&LiveGraphScopeId, &LiveGraphScope)> {
		self.scopes.iter()
	}

	/// Resolves the snapshot's public property handle by owned logical name.
	pub fn property_key(&self, name: &str) -> Option<PropertyKeyId> {
		self.property_keys.get(name).copied()
	}
}
