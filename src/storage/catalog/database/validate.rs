//! Cross-entry validation for one decoded `FGDC v9` payload.

use std::collections::{BTreeMap, BTreeSet};

use super::{DatabaseCatalog, PhysicalIndexBinding, codec::invalid};
use crate::{
	datastore::{ClassIndexKind, ClassIndexState, IndexScope, SchemaId},
	gql_status::FerrosGrynnResult,
	graph::{GraphKind, GraphStorageMode},
	storage::{page::PageNo, types::DataFileKind},
};

pub(super) fn validate(catalog: &DatabaseCatalog) -> FerrosGrynnResult<()> {
	if catalog.next_graph_id == 0 {
		return Err(invalid("next graph identity is zero"));
	}
	if catalog.next_schema_id == 0 || catalog.next_schema_id > u64::from(u32::MAX) {
		return Err(invalid("next schema identity is outside its u32 domain"));
	}
	validate_binding(catalog.atom_store.primary)?;
	validate_binding(catalog.atom_store.reverse_membership)?;
	if catalog.atom_store.primary.file_id == catalog.atom_store.reverse_membership.file_id {
		return Err(invalid("global primary and reverse membership share one file identity"));
	}

	let mut time_zones = BTreeSet::new();
	for name in &catalog.time_zones {
		if name.is_empty() || name.as_str().contains('\0') || !time_zones.insert(name.as_str()) {
			return Err(invalid("time-zone table contains an invalid or duplicate name"));
		}
	}

	let graph_by_id =
		catalog.graphs.iter().map(|graph| (graph.id, graph)).collect::<BTreeMap<_, _>>();
	let graph_ids = graph_by_id.keys().copied().collect::<BTreeSet<_>>();
	let mut schema_ids = BTreeSet::new();
	let mut class_ids = BTreeSet::new();
	let mut class_names = BTreeSet::new();
	let mut previous_class = None;
	for class in &catalog.classes {
		if previous_class.is_some_and(|previous| previous >= class.id)
			|| u64::from(class.id.get()) >= catalog.next_schema_id
			|| class.published_at == 0
			|| class.name.is_empty()
			|| class.name.as_str().contains('\0')
			|| !class_ids.insert(class.id)
			|| !class_names.insert(class.name.as_str())
		{
			return Err(invalid("class catalog ordering or identity is invalid"));
		}
		insert_schema(&mut schema_ids, class.id.into(), catalog.next_schema_id)?;
		let mut field_ids = BTreeSet::new();
		let mut field_names = BTreeSet::new();
		let mut field_keys = BTreeSet::new();
		for field in &class.fields {
			insert_schema(&mut schema_ids, field.id, catalog.next_schema_id)?;
			if !field_ids.insert(field.id)
				|| !field_names.insert(field.name.as_str())
				|| !field_keys.insert(field.property_key)
				|| field.name.is_empty()
				|| field.name.as_str().contains('\0')
			{
				return Err(invalid("class fields contain duplicate or invalid metadata"));
			}
		}
		let mut index_ids = BTreeSet::new();
		let mut index_names = BTreeSet::new();
		for index in &class.indexes {
			insert_schema(&mut schema_ids, index.id.into(), catalog.next_schema_id)?;
			if !index_ids.insert(index.id)
				|| !index_names.insert(index.name.as_str())
				|| index.name.is_empty()
				|| index.name.as_str().contains('\0')
			{
				return Err(invalid("class indexes contain duplicate or invalid metadata"));
			}
			match index.kind {
				ClassIndexKind::Count if !index.fields.is_empty() => {
					return Err(invalid("Count index contains fields"));
				}
				ClassIndexKind::Standard | ClassIndexKind::Unique if index.fields.is_empty() => {
					return Err(invalid("scalar index contains no fields"));
				}
				_ => {}
			}
			let mut keys = BTreeSet::new();
			if index.fields.iter().any(|key| !field_keys.contains(key) || !keys.insert(*key)) {
				return Err(invalid("class index references an absent or duplicate field"));
			}
			if let IndexScope::Effective {
				graph,
			} = index.scope
				&& !graph_ids.contains(&graph)
			{
				return Err(invalid("effective index references an absent graph"));
			}
			if index.state == ClassIndexState::Ready && index.physical.is_none() {
				return Err(invalid("ready class index has no physical binding"));
			}
			if let Some(binding) = index.physical {
				validate_binding(binding)?;
			}
		}
		previous_class = Some(class.id);
	}

	let mut previous_graph = None;
	let mut graph_names = BTreeSet::new();
	for graph in &catalog.graphs {
		if previous_graph.is_some_and(|previous| previous >= graph.id)
			|| graph.id.get() >= catalog.next_graph_id
			|| graph.published_at == 0
			|| graph.name.is_empty()
			|| graph.name.as_str().contains('\0')
			|| !graph_names.insert(graph.name.as_str())
			|| graph.storage_mode != GraphStorageMode::DiskBacked
			|| graph.kind == GraphKind::Subgraph
		{
			return Err(invalid("graph catalog ordering, identity, or storage mode is invalid"));
		}
		match graph.owner_root {
			None if graph.name.as_str().starts_with("$fg$") => {
				return Err(invalid("public graph uses the reserved internal-name prefix"));
			}
			Some(owner) => {
				let owner_entry = graph_by_id
					.get(&owner)
					.ok_or_else(|| invalid("internal graph owner root is absent"))?;
				if owner == graph.id
					|| owner_entry.owner_root.is_some()
					|| graph.kind != GraphKind::Graph
					|| graph.name.as_str() != internal_graph_name(owner, graph.id)
				{
					return Err(invalid("internal graph ownership or name is not canonical"));
				}
			}
			None => {}
		}
		if graph.root.as_ref().is_some_and(Vec::is_empty)
			|| (graph.root.is_some() && graph.kind != GraphKind::RootedTree)
		{
			return Err(invalid("graph root value is empty or belongs to a non-rooted family"));
		}
		insert_schema(&mut schema_ids, graph.label_index.0.into(), catalog.next_schema_id)?;
		insert_schema(&mut schema_ids, graph.adjacency_index.0.into(), catalog.next_schema_id)?;
		validate_binding(graph.membership)?;
		let mut previous_property = None;
		for property in &graph.properties {
			if previous_property.is_some_and(|previous| previous >= property.key)
				|| property.value.is_empty()
			{
				return Err(invalid("graph properties are not canonical"));
			}
			previous_property = Some(property.key);
		}
		if graph.arena.graph_by_slot.keys().ne(graph.arena.scope_by_slot.keys())
			|| graph.arena.graph_by_slot.get(&0) != Some(&graph.id)
			|| graph.arena.scope_by_slot.get(&0).is_none_or(|scope| !scope.is_empty())
		{
			return Err(invalid("graph arena root or slot maps are inconsistent"));
		}
		let mut scope_slots = BTreeMap::new();
		let mut scoped_children = BTreeSet::new();
		for (expected, (slot, child)) in graph.arena.graph_by_slot.iter().enumerate() {
			if usize::try_from(*slot).ok() != Some(expected) || !graph_ids.contains(child) {
				return Err(invalid(
					"graph arena slots are not dense or reference an absent graph",
				));
			}
			let scope = graph
				.arena
				.scope_by_slot
				.get(slot)
				.ok_or_else(|| invalid("graph arena slot has no scope"))?;
			if (*slot != 0 && scope.is_empty())
				|| scope_slots.insert(scope.as_ref(), *slot).is_some()
				|| !scoped_children.insert((scope.as_ref(), *child))
			{
				return Err(invalid("graph arena contains a duplicate or empty scope"));
			}
			if let Some(owner) = graph_by_id.get(child).and_then(|entry| entry.owner_root) {
				let expected_owner = graph.owner_root.unwrap_or(graph.id);
				if owner != expected_owner {
					return Err(invalid("internal graph is reachable from a foreign root arena"));
				}
			}
		}
		for (slot, scope) in &graph.arena.scope_by_slot {
			if *slot == 0 {
				continue;
			}
			let parent_scope = &scope[..scope.len() - 1];
			if !scope_slots.contains_key(parent_scope) {
				return Err(invalid("graph arena scope has no parent scope"));
			}
			let child = graph.arena.graph_by_slot[slot];
			for depth in 0..scope.len() {
				let ancestor_slot = scope_slots
					.get(&scope[..depth])
					.ok_or_else(|| invalid("graph arena ancestry is incomplete"))?;
				if graph.arena.graph_by_slot[ancestor_slot] == child {
					return Err(invalid("graph arena contains a containment cycle"));
				}
			}
		}
		previous_graph = Some(graph.id);
	}
	for graph in &catalog.graphs {
		let Some(owner) = graph.owner_root else {
			continue;
		};
		let owner_entry = graph_by_id
			.get(&owner)
			.ok_or_else(|| invalid("internal graph owner root is absent"))?;
		if !owner_entry.arena.graph_by_slot.values().any(|child| *child == graph.id) {
			return Err(invalid("internal graph is not reachable from its owner root arena"));
		}
	}

	let mut previous_parameter = None;
	for parameter in &catalog.parameters {
		insert_schema(&mut schema_ids, parameter.id, catalog.next_schema_id)?;
		if previous_parameter.is_some_and(|previous: &str| previous >= parameter.name.as_str())
			|| parameter.published_at == 0
			|| parameter.name.is_empty()
			|| parameter.name.as_str().contains('\0')
			|| parameter.value.is_empty()
		{
			return Err(invalid("parameter catalog ordering or value is invalid"));
		}
		previous_parameter = Some(parameter.name.as_str());
	}
	Ok(())
}

fn internal_graph_name(owner: crate::graph::GraphId, child: crate::graph::GraphId) -> String {
	format!("$fg$build_{:016x}_{:016x}", owner.get(), child.get())
}

fn validate_binding(binding: PhysicalIndexBinding) -> FerrosGrynnResult<()> {
	if binding.file_id.kind() != Some(DataFileKind::Index) || binding.root == PageNo::NULL {
		return Err(invalid("physical index binding is invalid"));
	}
	Ok(())
}

fn insert_schema(
	seen: &mut BTreeSet<SchemaId>,
	id: SchemaId,
	next_schema_id: u64,
) -> FerrosGrynnResult<()> {
	if u64::from(id.get()) >= next_schema_id || !seen.insert(id) {
		return Err(invalid("schema identity is duplicated or beyond its clock"));
	}
	Ok(())
}
