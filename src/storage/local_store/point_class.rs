//! Selected implicit class arrays share the checked canonical posting merge.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	LocalState,
	LocalStore,
	cleanup::UnpublishedGeneration,
	layout::{LayoutBuilder, graph_prefix, invalid, selected_file_metadata_bytes},
	point_array::{stream_membership_array, visit_source},
	point_namespace::LabelPlan,
};
use crate::{
	datastore::{ClassId, DataSpaceId, DatabaseRoot, GlobalAtomId},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, GraphId},
	storage::{
		file_registry::DataFileRole,
		page::PAGE_SIZE,
		scoped_index_image::write_streamed_file,
		types::{DataFileId, DataFileKind},
	},
};

/// A generation change alone does not change a physical class posting.
#[derive(Default)]
struct ClassChange {
	/// At least one slot enters or leaves this class.
	changed: bool,
	/// Only new slots need sorting; the persisted population is already sorted.
	insertions: Vec<AtomId>,
}

/// Publishes only class arrays whose normalized slot membership changes.
///
/// # Errors
/// Rejects absent owners, inconsistent sources and resource/codec/I/O failures.
pub(super) fn append_class_indexes(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	plan: &mut LabelPlan,
	generation: &str,
	layout: &mut LayoutBuilder,
	cleanup: &mut Option<UnpublishedGeneration>,
	retired: &mut BTreeSet<(DataSpaceId, DataFileId)>,
) -> FerrosGrynnResult<()> {
	let targets = class_changes(before, after, plan)?;
	for ((graph, class), mut change) in targets {
		let source = before.graph(graph).ok_or_else(|| invalid("class source graph is absent"))?;
		let target = after.graph(graph).ok_or_else(|| invalid("class target graph is absent"))?;
		let old_atoms = before
			.indexes
			.graphs
			.get(&graph)
			.ok_or_else(|| invalid("class source indexes are absent"))?
			.classes
			.get(&class);
		let atoms = after
			.indexes
			.graphs
			.get(&graph)
			.ok_or_else(|| invalid("class target indexes are absent"))?
			.classes
			.get(&class);
		let kind = after
			.schema
			.class_by_id(class)
			.ok_or_else(|| invalid("class schema is absent"))?
			.kind();
		let space = DataSpaceId::Graph(graph);
		let suffix = format!("/idx/cls_{}.fgidx", class.get());
		let previous = source_file(state, space, &suffix, old_atoms.is_some())?;
		// Cold lookup roots omit empty buckets; their physical arrays remain
		// authoritative and must be validated before retention or replacement.
		let empty = BTreeSet::new();
		let source_atoms = old_atoms.or_else(|| previous.map(|_| &empty));
		plan.budget.scratch(3 * PAGE_SIZE + 4096)?;
		if !change.changed && previous.is_some() == atoms.is_some() {
			if old_atoms.is_none() && previous.is_some() {
				visit_source(
					store,
					state,
					source,
					previous,
					kind,
					source_atoms,
					|_id, _address| Ok(()),
				)?;
			}
			continue;
		}
		let atoms = atoms.ok_or_else(|| invalid("class target bucket is absent"))?;
		if let Some(previous) = previous
			&& !retired.contains(&(space, previous))
		{
			plan.budget.tree_entry::<(DataSpaceId, DataFileId)>(retired.len())?;
			retired.insert((space, previous));
		}
		change.insertions.sort_unstable_by_key(|local| local.slot());
		let path = format!("{}{suffix}", graph_prefix(graph, generation));
		plan.budget.retain(selected_file_metadata_bytes(&path)?)?;
		plan.budget.scratch(3 * PAGE_SIZE + 4096)?;
		cleanup.get_or_insert_with(|| {
			UnpublishedGeneration::for_database(&store.root, after, generation)
		});
		let built = write_streamed_file(&store.root, &path, |file| {
			stream_membership_array(
				store,
				state,
				(source, target),
				previous,
				kind,
				source_atoms,
				atoms,
				&change.insertions,
				|number, page| {
					if number.get() != file.page_count() {
						return Err(invalid("class merge wrote non-sequential pages"));
					}
					file.write_page(number, page)
				},
			)
		})?;
		layout.add_prewritten(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			path,
			built.page_count,
		)?;
	}
	Ok(())
}

/// Resolves physical ownership independently of decoded lookup bucket presence.
fn source_file(
	state: &LocalState,
	space: DataSpaceId,
	suffix: &str,
	required: bool,
) -> FerrosGrynnResult<Option<DataFileId>> {
	let mut candidates = state.file_catalog.files.iter().filter(|entry| {
		entry.space_id == space
			&& entry.file_id.kind() == Some(DataFileKind::Index)
			&& entry.path.as_str().ends_with(suffix)
	});
	let previous = candidates.next().map(|entry| entry.file_id);
	if candidates.next().is_some() {
		return Err(invalid("class physical source is ambiguous"));
	}
	if required && previous.is_none() {
		return Err(invalid("class physical source is absent"));
	}
	Ok(previous)
}

/// Selects from explicit owners and slot transitions without a population scan.
fn class_changes(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	plan: &mut LabelPlan,
) -> FerrosGrynnResult<BTreeMap<(GraphId, ClassId), ClassChange>> {
	let mut changes = BTreeMap::new();
	let mut ownerless_graph = None;
	for &(graph, global) in &plan.memberships {
		if let Some(class) = membership_class(before, after, graph, global)? {
			insert_target(&mut changes, &mut plan.budget, (graph, class))?;
		} else if ownerless_graph != Some(graph) {
			let source = before
				.indexes
				.graphs
				.get(&graph)
				.ok_or_else(|| invalid("class source indexes are absent"))?;
			let target = after
				.indexes
				.graphs
				.get(&graph)
				.ok_or_else(|| invalid("class target indexes are absent"))?;
			// DETACH retains a new empty bucket; the next warm ATTACH requires
			// its physical array even though neither root has a Base owner.
			for (&class, atoms) in &target.classes {
				if atoms.is_empty() && !source.classes.contains_key(&class) {
					insert_target(&mut changes, &mut plan.budget, (graph, class))?;
				}
			}
			ownerless_graph = Some(graph);
		}
	}
	for transition in &plan.incidence {
		let old = transition
			.before
			.and_then(|state| state.occurrence)
			.map(|(local, global)| {
				before
					.atom(global)
					.map(|atom| (atom.class_id(), local))
					.ok_or_else(|| invalid("class source atom is absent"))
			})
			.transpose()?;
		let new = transition
			.after
			.and_then(|state| state.occurrence)
			.map(|(local, global)| {
				after
					.atom(global)
					.map(|atom| (atom.class_id(), local))
					.ok_or_else(|| invalid("class target atom is absent"))
			})
			.transpose()?;
		if old.map(|(class, _)| class) == new.map(|(class, _)| class) {
			continue;
		}
		for (entry, insert) in [(old, false), (new, true)] {
			if let Some((class, local)) = entry {
				let key = (transition.graph, class);
				insert_target(&mut changes, &mut plan.budget, key)?;
				let change = changes
					.get_mut(&key)
					.ok_or_else(|| invalid("class change target disappeared"))?;
				change.changed = true;
				if insert {
					reserve_insertion(&mut change.insertions, &mut plan.budget)?;
					change.insertions.push(local);
				}
			}
		}
	}
	Ok(changes)
}

/// Resolves a live Base owner or an ownerless transient membership.
///
/// # Errors
/// Rejects a dangling membership before any class-file preparation.
fn membership_class(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	graph: GraphId,
	global: GlobalAtomId,
) -> FerrosGrynnResult<Option<ClassId>> {
	if let Some(atom) = after.atom(global).or_else(|| before.atom(global)) {
		Ok(Some(atom.class_id()))
	} else if before.membership(graph, global).is_none()
		&& after.membership(graph, global).is_none()
	{
		Ok(None)
	} else {
		Err(invalid("class membership owner is absent"))
	}
}

/// Admits retained growth and the old buffer during geometric reallocation.
fn reserve_insertion(
	insertions: &mut Vec<AtomId>,
	budget: &mut super::point_namespace::PublicationBudget,
) -> FerrosGrynnResult<()> {
	if insertions.len() == insertions.capacity() {
		let next = insertions
			.capacity()
			.max(2)
			.checked_mul(2)
			.ok_or_else(|| invalid("class insertion capacity overflows usize"))?;
		let additional = (next - insertions.capacity())
			.checked_mul(size_of::<AtomId>())
			.ok_or_else(|| invalid("class insertion bytes overflow usize"))?;
		budget.retain(additional)?;
		budget.scratch(
			insertions
				.capacity()
				.checked_mul(size_of::<AtomId>())
				.ok_or_else(|| invalid("class insertion old buffer overflows usize"))?,
		)?;
		insertions
			.try_reserve_exact(next - insertions.len())
			.map_err(|_error| invalid("class insertion allocation failed"))?;
	}
	Ok(())
}

/// Admits a vacant map entry before its first allocation.
fn insert_target(
	changes: &mut BTreeMap<(GraphId, ClassId), ClassChange>,
	budget: &mut super::point_namespace::PublicationBudget,
	key: (GraphId, ClassId),
) -> FerrosGrynnResult<()> {
	if !changes.contains_key(&key) {
		budget.tree_entry::<((GraphId, ClassId), ClassChange)>(changes.len())?;
		changes.insert(key, ClassChange::default());
	}
	Ok(())
}

#[cfg(test)]
/// Class evidence can outlive both transient Base and occurrence owners.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, PublicationDelta},
		graph::{AtomKind, GraphKind},
		storage::local_store::{
			point_namespace::PublicationBudget,
			point_occurrence::incidence_slots,
		},
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyMap, RecordId},
	};

	/// The final empty lookup bucket is authoritative even without a live
	/// owner.
	#[test]
	fn global_node_transient_class_target_retains_new_empty_lookup_authority() {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		let graph = seed.define_graph("g", GraphKind::Graph).assert_ok();
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		let mut after = before.as_ref().clone();
		let class = before.schema.class("item").assert_some();
		let global = after
			.insert_atom(&class, "item:gone".parse::<RecordId>().assert_ok(), PropertyMap::new())
			.assert_ok();
		let node = after.attach_node(graph.id(), global, None, None).assert_ok();
		after.detach(graph.id(), global).assert_ok().assert_some();
		after.remove_atom(global).assert_ok();
		let delta = PublicationDelta {
			records: BTreeSet::new(),
			global_slots: BTreeSet::from([global.slot()]),
			overlays: BTreeSet::new(),
			incidence: BTreeSet::from([(graph.id(), node)]),
			memberships: BTreeSet::from([(graph.id(), global)]),
			point_graph_only: true,
		};
		let mut budget = PublicationBudget::new();
		budget.retain(4096).assert_ok();
		let incidence = incidence_slots(&before, &after, &delta, &mut budget).assert_ok();
		let mut plan = LabelPlan {
			incidence,
			memberships: delta.memberships.iter().copied().collect(),
			targets: BTreeMap::new(),
			graphs: BTreeMap::new(),
			class_graphs: BTreeSet::new(),
			budget,
		};
		let changes = class_changes(&before, &after, &mut plan).assert_ok();
		assert_eq!(changes.len(), 1);
		let change = changes.get(&(graph.id(), class.id().assert_some())).assert_some();
		assert!(!change.changed);
		assert!(change.insertions.is_empty());
	}
}
