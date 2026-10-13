//! Field-level local decisions replay over a new complete remote base.

use std::collections::BTreeSet;

use super::{
	FerrosGrynnError,
	FerrosGrynnResult,
	LiveGraphImage,
	LiveGraphScopeId,
	LiveGraphTarget,
	Number,
	Object,
	PropertyKeyId,
};
use crate::values::runtime::same_value;

/// Replays only changed local fields over independent incoming properties.
fn properties(base: Option<&Object>, local: &Object, target: &mut Object) {
	for (name, value) in local.iter() {
		if base.and_then(|base| base.get(name)).is_none_or(|old| !same_value(old, value)) {
			target.insert(name.clone(), value.clone());
		}
	}
	if let Some(base) = base {
		for (name, _) in base.iter() {
			if !local.contains_key(name) {
				target.remove(name);
			}
		}
	}
}

/// Uses numeric semantic equality for optional stored edge weights.
fn weight_equal(left: Option<Number>, right: Option<Number>) -> bool {
	match (left, right) {
		(None, None) => true,
		(Some(left), Some(right)) => {
			same_value(&crate::values::Value::from(left), &crate::values::Value::from(right))
		}
		_ => false,
	}
}

impl LiveGraphImage {
	/// Replays only local field changes, retaining independent incoming fields.
	pub(crate) fn merge_local(
		base: &Self,
		remote: &Self,
		local: &Self,
		changes: &[LiveGraphTarget],
		admit: &mut dyn FnMut(usize, u64) -> FerrosGrynnResult<()>,
	) -> FerrosGrynnResult<Self> {
		admit(remote.estimated_heap_bytes()?, remote.scopes.len() as u64)?;
		admit(
			local
				.estimated_heap_bytes()?
				.checked_mul(2)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE local replay bytes"))?,
			changes.len() as u64,
		)?;
		let mut merged = remote.clone();
		for change in changes {
			admit(0, 1)?;
			match change {
				LiveGraphTarget::Graph(key) => {
					let old = base.scopes.get(key);
					let Some(local_scope) = local.scopes.get(key) else {
						merged.scopes.remove(key);
						continue;
					};
					if let Some(target) = merged.scopes.get_mut(key) {
						properties(
							old.map(|old| &old.properties),
							&local_scope.properties,
							&mut target.properties,
						);
						if old.is_none_or(|old| old.validators != local_scope.validators) {
							target.validators = local_scope.validators;
						}
					} else {
						merged.scopes.insert(key.clone(), local_scope.clone());
					}
					if *key == LiveGraphScopeId::Root {
						if base.family != local.family {
							merged.family = local.family;
						}
						if base.validators != local.validators {
							merged.validators = local.validators;
						}
						if base.root != local.root {
							merged.root = local.root.clone();
						}
					}
				}
				LiveGraphTarget::Atom(id) => {
					let local_scope = local.scopes.get(&id.scope);
					let target = merged.scopes.get_mut(&id.scope).ok_or_else(|| {
						FerrosGrynnError::invalid_argument(
							"local target's remote graph scope no longer exists",
						)
					})?;
					let old = base.scopes.get(&id.scope);
					match id.atom.kind() {
						crate::graph::AtomKind::Node => {
							let Some(local_node) =
								local_scope.and_then(|scope| scope.nodes.get(&id.atom))
							else {
								target.nodes.remove(&id.atom);
								continue;
							};
							let previous = old.and_then(|scope| scope.nodes.get(&id.atom));
							if let Some(node) = target.nodes.get_mut(&id.atom) {
								properties(
									previous.map(|node| &node.properties),
									&local_node.properties,
									&mut node.properties,
								);
								if previous.is_none_or(|node| node.label != local_node.label) {
									node.label = local_node.label.clone();
								}
								if previous.is_none_or(|node| node.child != local_node.child) {
									node.child = local_node.child.clone();
								}
							} else {
								target.nodes.insert(id.atom.clone(), local_node.clone());
							}
						}
						crate::graph::AtomKind::Edge => {
							let Some(local_edge) =
								local_scope.and_then(|scope| scope.edges.get(&id.atom))
							else {
								target.edges.remove(&id.atom);
								continue;
							};
							let previous = old.and_then(|scope| scope.edges.get(&id.atom));
							if let Some(edge) = target.edges.get_mut(&id.atom) {
								properties(
									previous.map(|edge| &edge.properties),
									&local_edge.properties,
									&mut edge.properties,
								);
								if previous.is_none_or(|edge| edge.label != local_edge.label) {
									edge.label = local_edge.label.clone();
								}
								if previous.is_none_or(|edge| edge.child != local_edge.child) {
									edge.child = local_edge.child.clone();
								}
								if previous.is_none_or(|edge| {
									!weight_equal(edge.weight, local_edge.weight)
								}) {
									edge.weight = local_edge.weight;
								}
								if previous.is_none_or(|edge| {
									edge.direction != local_edge.direction
										|| edge.inputs != local_edge.inputs
										|| edge.outputs != local_edge.outputs
								}) {
									edge.direction = local_edge.direction;
									edge.inputs = local_edge.inputs.clone();
									edge.outputs = local_edge.outputs.clone();
								}
							} else {
								target.edges.insert(id.atom.clone(), local_edge.clone());
							}
						}
					}
				}
			}
		}
		admit(
			merged
				.property_keys
				.len()
				.checked_mul(64)
				.ok_or_else(|| FerrosGrynnError::operation_overflow("LIVE merged key admission"))?,
			merged.property_keys.len() as u64,
		)?;
		let mut used = merged.property_keys.values().copied().collect::<BTreeSet<_>>();
		for (name, local_key) in &local.property_keys {
			if merged.property_keys.contains_key(name) {
				continue;
			}
			let mut key = *local_key;
			if used.contains(&key) {
				key = PropertyKeyId::new(u32::MAX);
			}
			while used.contains(&key) {
				admit(0, 1)?;
				key = PropertyKeyId::new(key.get().checked_sub(1).ok_or_else(|| {
					FerrosGrynnError::invalid_argument("LIVE local property key space is exhausted")
				})?);
			}
			admit(256 + name.len(), 1)?;
			used.insert(key);
			merged.property_keys.insert(name.clone(), key);
		}
		merged.validate_structure(admit)?;
		Ok(merged)
	}
}
