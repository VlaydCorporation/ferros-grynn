//! Selected immutable arrays merge canonical sources with explicit insertions.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	LocalState,
	LocalStore,
	cleanup::UnpublishedGeneration,
	layout::{LayoutBuilder, graph_prefix, invalid, selected_file_metadata_bytes},
	point_array::{stream_membership_array, visit_source},
	point_namespace::{LabelPlan, label_kind},
};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphId},
	storage::{
		file_registry::DataFileRole,
		page::{AlignedPage, PAGE_SIZE},
		scoped_index_image::{label_filename, write_streamed_file},
		types::{DataFileId, DataFileKind},
	},
	values::Text,
};

/// Distinct source/final names selected by membership or numeric-ID changes.
pub(super) type LabelTargets = BTreeMap<GraphId, BTreeSet<(AtomKind, Text)>>;

/// Resolves the same independently sorted dense namespace as full builders.
///
/// # Errors
/// Rejects a missing graph, unknown name or unrepresentable dense ID.
pub(super) fn label_id(
	root: &DatabaseRoot,
	graph: GraphId,
	kind: AtomKind,
	name: Option<&Text>,
) -> FerrosGrynnResult<Option<u32>> {
	let Some(name) = name else {
		return Ok(None);
	};
	let indexes = root
		.indexes
		.graphs
		.get(&graph)
		.ok_or_else(|| invalid("point label graph indexes are absent"))?;
	let id = indexes
		.labels
		.keys()
		.filter(|(candidate, _)| *candidate == kind)
		.position(|(_, candidate)| candidate == name)
		.ok_or_else(|| invalid("point label name is absent"))?;
	u32::try_from(id).map(Some).map_err(|_error| invalid("point label identity exceeds u32"))
}

/// Streams selected memberships without sorting the existing posting
/// population.
///
/// # Errors
/// Rejects incomplete/inconsistent sources and propagates admission, codec and
/// I/O failures.
pub(super) fn append_selected_indexes(
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
	for (&graph, labels) in &plan.targets {
		let source = before.graph(graph).ok_or_else(|| invalid("point label graph is absent"))?;
		let old = &before
			.indexes
			.graphs
			.get(&graph)
			.ok_or_else(|| invalid("point label source indexes are absent"))?
			.labels;
		let new = &after
			.indexes
			.graphs
			.get(&graph)
			.ok_or_else(|| invalid("point label target indexes are absent"))?
			.labels;
		for key in labels {
			let old_atoms = old.get(key);
			let atoms = new.get(key);
			let (old_id, id) = if let Some(namespace) = plan.graphs.get(&graph) {
				(
					namespace.before.id(label_kind(key.0), key.1.as_str()),
					namespace.after.id(label_kind(key.0), key.1.as_str()),
				)
			} else {
				(
					label_id(before, graph, key.0, Some(&key.1))?,
					label_id(after, graph, key.0, Some(&key.1))?,
				)
			};
			if old_id == id && old_atoms == atoms {
				continue;
			}
			let space = DataSpaceId::Graph(graph);
			let previous = old_id
				.map(|id| {
					let suffix = format!("/{}", label_filename(key.0, id));
					state
						.file_catalog
						.files
						.iter()
						.find(|entry| {
							entry.space_id == space
								&& entry.file_id.kind() == Some(DataFileKind::Index)
								&& entry.path.as_str().ends_with(&suffix)
						})
						.map(|entry| entry.file_id)
						.ok_or_else(|| invalid("point label physical source is absent"))
				})
				.transpose()?;
			if let Some(previous) = previous {
				plan.budget.tree_entry::<(DataSpaceId, DataFileId)>(retired.len())?;
				retired.insert((space, previous));
			}
			let Some(id) = id else {
				visit_source(store, state, source, previous, key.0, old_atoms, |_id, _address| {
					Ok(())
				})?;
				continue;
			};
			let atoms = atoms.ok_or_else(|| invalid("point label target name is absent"))?;
			let is_insertion = |target: &&super::point_occurrence::SlotTransition| {
				target.graph == graph
					&& target.kind == key.0
					&& target
						.after
						.and_then(|state| state.occurrence)
						.and_then(|(local, _)| after.occurrence(graph, local))
						.is_some_and(|occurrence| occurrence.label() == Some(&key.1))
					&& target
						.before
						.and_then(|state| state.occurrence)
						.and_then(|(local, _)| before.occurrence(graph, local))
						.is_none_or(|occurrence| occurrence.label() != Some(&key.1))
			};
			let count = plan.incidence.iter().filter(|target| is_insertion(target)).count();
			let scratch = count
				.checked_mul(size_of::<AtomId>())
				.and_then(|bytes| bytes.checked_add(3 * PAGE_SIZE + 4096))
				.ok_or_else(|| invalid("point label merge scratch overflows usize"))?;
			plan.budget.scratch(scratch)?;
			let mut insertions = Vec::new();
			insertions
				.try_reserve_exact(count)
				.map_err(|_error| invalid("point label insertion allocation failed"))?;
			insertions.extend(
				plan.incidence
					.iter()
					.filter(|target| is_insertion(target))
					.filter_map(|target| target.after.and_then(|state| state.occurrence))
					.map(|(local, _)| local),
			);
			insertions.sort_unstable_by_key(|local| local.slot());
			let path = format!("{}/{}", graph_prefix(graph, generation), label_filename(key.0, id));
			plan.budget.retain(selected_file_metadata_bytes(&path)?)?;
			// Retained file metadata coexists with the merge's fixed scratch.
			plan.budget.scratch(scratch)?;
			cleanup.get_or_insert_with(|| {
				UnpublishedGeneration::for_database(&store.root, after, generation)
			});
			let built = write_streamed_file(&store.root, &path, |file| {
				stream_membership_array(
					store,
					state,
					(
						source,
						after
							.graph(graph)
							.ok_or_else(|| invalid("point label target graph is absent"))?,
					),
					previous,
					key.0,
					old_atoms,
					atoms,
					&insertions,
					|no, page: &AlignedPage| {
						if no.get() != file.page_count() {
							return Err(invalid("label merge wrote non-sequential pages"));
						}
						file.write_page(no, page)
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
	}
	Ok(())
}
