//! Canonical content changes with self-contained checkpoint images.

use std::{collections::BTreeSet, sync::Arc};

use fg_runtime::__internal::RecordIdInternal as _;

use super::{
	AtomRef,
	FerrosGrynnError,
	FerrosGrynnResult,
	LiveGraphAtomId,
	LiveGraphEdge,
	LiveGraphImage,
	LiveGraphNode,
	LiveGraphScope,
	LiveGraphScopeId,
	Object,
	Versionstamp,
};
use crate::values::{Value, runtime::same_value};

/// One changed logical scope or qualified atom, independent of slot handles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LiveGraphTarget {
	/// Family, validators, ROOT or properties of one scope.
	Graph(
		/// Family, validators, ROOT or properties of one scope.
		LiveGraphScopeId,
	),
	/// An effective atom, its incidence, child binding or write-back source.
	Atom(
		/// An effective atom, its incidence, child binding or write-back
		/// source.
		LiveGraphAtomId,
	),
}

/// A complete graph revision; retained images also anchor replay intervals.
#[derive(Debug, Clone)]
pub struct LiveGraphDelta {
	/// Pins the immutable remote base used for local dirty comparison.
	pub(crate) base: Versionstamp,
	/// Stores the complete accepted durable graph watermark.
	pub(crate) revision: Versionstamp,
	/// Pins the complete predecessor checkpoint for replay and canonical diff.
	pub(crate) before: Option<Arc<LiveGraphImage>>,
	/// Pins the complete candidate checkpoint for whole-delta acceptance.
	pub(crate) after: Arc<LiveGraphImage>,
	/// Owns the canonical logical targets changed by this complete delta.
	pub(crate) changes: Vec<LiveGraphTarget>,
}

/// Compares named content using the runtime semantic value equality.
fn properties_equal(left: &Object, right: &Object) -> bool {
	left.len() == right.len()
		&& left.iter().zip(right.iter()).all(|((ln, lv), (rn, rv))| ln == rn && same_value(lv, rv))
}

/// Compares node content while ignoring transport-only source watermarks.
fn node_equal(left: &LiveGraphNode, right: &LiveGraphNode) -> bool {
	left.label == right.label
		&& left.child == right.child
		&& left.source.map(|source| source.class) == right.source.map(|source| source.class)
		&& properties_equal(&left.properties, &right.properties)
}

/// Compares qualified topology and properties while ignoring source watermarks.
fn edge_equal(left: &LiveGraphEdge, right: &LiveGraphEdge) -> bool {
	let weight = match (left.weight, right.weight) {
		(None, None) => true,
		(Some(left), Some(right)) => same_value(&Value::from(left), &Value::from(right)),
		_ => false,
	};
	left.label == right.label
		&& left.child == right.child
		&& left.source.map(|source| source.class) == right.source.map(|source| source.class)
		&& left.direction == right.direction
		&& left.inputs == right.inputs
		&& left.outputs == right.outputs
		&& weight
		&& properties_equal(&left.properties, &right.properties)
}

/// Duplicate owned views of one logical scope must agree on content.
pub(super) fn scope_equal(left: &LiveGraphScope, right: &LiveGraphScope) -> bool {
	left.source == right.source
		&& left.validators == right.validators
		&& properties_equal(&left.properties, &right.properties)
		&& left.nodes.len() == right.nodes.len()
		&& left
			.nodes
			.iter()
			.zip(&right.nodes)
			.all(|((lk, lv), (rk, rv))| lk == rk && node_equal(lv, rv))
		&& left.edges.len() == right.edges.len()
		&& left
			.edges
			.iter()
			.zip(&right.edges)
			.all(|((lk, lv), (rk, rv))| lk == rk && edge_equal(lv, rv))
}

impl LiveGraphDelta {
	/// Borrows the complete incoming image; acceptance is always atomic.
	pub fn after(&self) -> &LiveGraphImage {
		&self.after
	}

	/// Borrows the preceding checkpoint when this is a commit delta.
	pub fn before(&self) -> Option<&LiveGraphImage> {
		self.before.as_deref()
	}

	/// Returns the prior graph watermark, independent of quiet database
	/// commits.
	pub const fn base(&self) -> Versionstamp {
		self.base
	}

	/// Returns the complete accepted remote commit watermark.
	pub const fn revision(&self) -> Versionstamp {
		self.revision
	}

	/// Borrows changed targets in canonical logical order.
	pub fn changes(&self) -> &[LiveGraphTarget] {
		&self.changes
	}

	/// Diffs owned logical images; runtime graph identity never substitutes for
	/// content.
	#[allow(
		clippy::mutable_key_type,
		reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash \
		          identities"
	)]
	pub(crate) fn between(
		before: Option<Arc<LiveGraphImage>>,
		after: Arc<LiveGraphImage>,
		base: Versionstamp,
		revision: Versionstamp,
		admit: &dyn Fn(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		if revision <= base {
			return Err(FerrosGrynnError::invalid_argument("graph delta revision must advance"));
		}
		let mut changes = BTreeSet::new();
		let mut insert = |scope: &LiveGraphScopeId,
		                  atom: Option<&AtomRef>|
		 -> FerrosGrynnResult<()> {
			let heap = scope
				.estimated_heap_bytes()
				.checked_add(atom.map_or(0, |atom| atom.id().estimated_heap_bytes()))
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE graph target bytes"))?;
			admit(
				heap.checked_add(256 + std::mem::size_of::<LiveGraphTarget>()).ok_or_else(
					|| FerrosGrynnError::operation_overflow("LIVE graph delta bytes"),
				)?,
				1,
			)?;
			let target = match atom {
				Some(atom) => LiveGraphTarget::Atom(LiveGraphAtomId {
					scope: scope.clone(),
					atom: atom.clone(),
				}),
				None => LiveGraphTarget::Graph(scope.clone()),
			};
			changes.insert(target);
			Ok(())
		};
		if before.as_ref().is_none_or(|before| {
			before.family != after.family
				|| before.validators != after.validators
				|| before.root != after.root
		}) {
			insert(&LiveGraphScopeId::Root, None)?;
		}
		for (scope, image) in &after.scopes {
			admit(0, 1)?;
			let old = before.as_ref().and_then(|before| before.scopes.get(scope));
			if old.is_none_or(|old| {
				old.source != image.source
					|| old.validators != image.validators
					|| !properties_equal(&old.properties, &image.properties)
			}) {
				insert(scope, None)?;
			}
			for (atom, node) in &image.nodes {
				admit(0, 1)?;
				if old.and_then(|old| old.nodes.get(atom)).is_none_or(|old| !node_equal(old, node))
				{
					insert(scope, Some(atom))?;
				}
			}
			for (atom, edge) in &image.edges {
				admit(0, 1)?;
				if old.and_then(|old| old.edges.get(atom)).is_none_or(|old| !edge_equal(old, edge))
				{
					insert(scope, Some(atom))?;
				}
			}
		}
		if let Some(before) = &before {
			for (scope, image) in &before.scopes {
				admit(0, 1)?;
				let next = after.scopes.get(scope);
				if next.is_none() {
					insert(scope, None)?;
				}
				for atom in image.nodes.keys().chain(image.edges.keys()) {
					admit(0, 1)?;
					if next.is_none_or(|next| {
						!next.nodes.contains_key(atom) && !next.edges.contains_key(atom)
					}) {
						insert(scope, Some(atom))?;
					}
				}
			}
		}
		admit(
			changes
				.len()
				.checked_mul(std::mem::size_of::<LiveGraphTarget>())
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE graph targets vector"))?,
			changes.len() as u64,
		)?;
		Ok(Self {
			base,
			revision,
			before,
			after,
			changes: changes.into_iter().collect(),
		})
	}
}
