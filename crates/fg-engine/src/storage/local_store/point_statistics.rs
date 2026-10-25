//! Sparse eager roots and exact child counters share one publication barrier.

use std::collections::{BTreeMap, BTreeSet};

use fg_runtime::__internal::TextInternal as _;

use super::{LocalState, LocalStore, layout::invalid, point_namespace::PublicationBudget};
use crate::{
	datastore::{ClassId, DataSpaceId, DatabaseRoot, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphId},
	stats::{LabelStatistic, LabelStatisticKey, effective_property_count},
	storage::{
		base_statistics::rewrite_base_statistics_root,
		graph_index::LabelKind,
		graph_pages::{
			LabelStatisticsChanges,
			rewrite_class_statistics_page,
			rewrite_label_statistics_page,
			rewrite_statistics_root,
		},
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{DataFileKind, PageKey},
	},
};

/// Heap-stable page images shared with the existing sparse publisher.
type PointPages = Vec<(PageKey, Box<AlignedPage>)>;

/// Logical counter targets are selected before physical pages are borrowed.
struct StatisticsTargets {
	/// Base lifecycle counters and an optional eager-root rewrite.
	classes: Option<BTreeMap<ClassId, u64>>,
	/// Exact effective class counters for changed memberships.
	graph_classes: BTreeMap<GraphId, BTreeMap<ClassId, u64>>,
	/// Distinct before/after occurrence contributions in logical label names.
	contributions: BTreeMap<GraphId, LabelContributions>,
	/// Graph roots affected by the complete replay delta.
	graphs: BTreeSet<GraphId>,
}

impl StatisticsTargets {
	/// Keeps logical target selection independent of physical page rewriting.
	///
	/// # Errors
	/// Preserves missing identities, counter overflow and shared admission
	/// failures.
	fn build(
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<Self> {
		let classes = class_populations(before, after, delta, budget)?;
		let graph_classes = graph_class_populations(before, after, delta, budget)?;
		let contributions = label_contributions(before, after, delta, budget)?;
		let mut graphs = BTreeSet::new();
		for (graph, _) in &delta.overlays {
			insert_target(&mut graphs, *graph, budget)?;
		}
		for (graph, _) in &delta.incidence {
			insert_target(&mut graphs, *graph, budget)?;
		}
		for slot in &delta.global_slots {
			if let Some(memberships) =
				after.atoms.atom_by_slot(*slot)?.and_then(|atom| after.memberships(atom.id()))
			{
				for graph in memberships.keys() {
					insert_target(&mut graphs, *graph, budget)?;
				}
			}
		}
		Ok(Self {
			classes,
			graph_classes,
			contributions,
			graphs,
		})
	}
}

/// Stages eager roots and changed exact counters; reports lazy-owner
/// replacement.
///
/// # Errors
/// Propagates source, counter, codec and shared resource admission failures
/// before any WAL publication.
pub(super) fn append_statistics_pages(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	labels: &mut super::point_namespace::LabelPlan,
	pages: &mut PointPages,
) -> FerrosGrynnResult<bool> {
	let StatisticsTargets {
		classes,
		mut graph_classes,
		mut contributions,
		graphs,
	} = StatisticsTargets::build(before, after, delta, &mut labels.budget)?;
	let mut replaces_statistics = !labels.graphs.is_empty() || !labels.class_graphs.is_empty();
	if let Some(classes) = classes {
		append_base_statistics_pages(store, state, after, classes, pages, &mut labels.budget)?;
	}
	for graph in graphs {
		// Namespace metadata is encoded with final IDs in selected new files.
		if labels.graphs.contains_key(&graph) || labels.class_graphs.contains(&graph) {
			continue;
		}
		if before.statistics.graph(graph) == after.statistics.graph(graph) {
			continue;
		}
		let space = DataSpaceId::Graph(graph);
		let graph_file = LocalStore::statistics_file(state, space, DataFileKind::Statistics)?;
		let label_file = LocalStore::statistics_file(state, space, DataFileKind::LabelStatistics)?;
		let property_file =
			LocalStore::statistics_file(state, space, DataFileKind::PropertyStatistics)?;
		if let Some(changes) = contributions.remove(&graph) {
			replaces_statistics |= append_label_statistics_pages(
				store,
				state,
				after,
				graph,
				changes,
				pages,
				&mut labels.budget,
			)?;
		}
		let key = PageKey::new(space, graph_file, PageNo::new(0));
		let mut page = read_statistics_page(store, state, key, 0, &labels.budget)?;
		let statistics = after
			.statistics
			.graph(graph)
			.ok_or_else(|| invalid("point update graph statistics are absent"))?;
		let layout = rewrite_statistics_root(
			&mut page,
			statistics,
			store.visible_pages(state, space, graph_file)?,
			store.visible_pages(state, space, label_file)?,
			store.visible_pages(state, space, property_file)?,
		)?;
		retain_statistics_page(pages, key, page, &mut labels.budget)?;
		if let Some(classes) = graph_classes.remove(&graph) {
			replaces_statistics = true;
			append_class_pages(
				store,
				state,
				space,
				graph_file,
				layout.class_range(),
				classes,
				pages,
				&mut labels.budget,
			)?;
		}
	}
	Ok(replaces_statistics)
}

/// Membership deltas select exact final class counters, including zero.
///
/// # Errors
/// Rejects missing counters and target growth outside the shared budget.
fn graph_class_populations(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<BTreeMap<GraphId, BTreeMap<ClassId, u64>>> {
	let mut result = BTreeMap::<GraphId, BTreeMap<ClassId, u64>>::new();
	for &(graph, global) in &delta.memberships {
		for atom in before.atom(global).into_iter().chain(after.atom(global)) {
			let class = atom.class_id();
			let count = after
				.statistics
				.graph(graph)
				.and_then(|statistics| statistics.classes.get(&class))
				.copied()
				.ok_or_else(|| invalid("point graph class statistics are absent"))?;
			admit_entry(&result, &graph, budget)?;
			let classes = result.entry(graph).or_default();
			admit_entry(classes, &class, budget)?;
			classes.insert(class, count);
		}
	}
	Ok(result)
}

/// Keeps Base population selection with its physical counter publisher.
///
/// # Errors
/// Rejects invalid Base sources, missing counters and shared admission
/// failures.
fn class_populations(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<Option<BTreeMap<ClassId, u64>>> {
	if delta.global_slots.is_empty() {
		return Ok(None);
	}
	let mut classes = BTreeMap::new();
	if super::point_base::has_lifecycle(before, after, delta)? {
		for slot in &delta.global_slots {
			for atom in before
				.atoms
				.atom_by_slot(*slot)?
				.into_iter()
				.chain(after.atoms.atom_by_slot(*slot)?)
			{
				let class = atom.class_id();
				admit_entry(&classes, &class, budget)?;
				classes.insert(
					class,
					after
						.statistics
						.base()
						.classes
						.get(&class)
						.copied()
						.ok_or_else(|| invalid("point class statistics are absent"))?,
				);
			}
		}
	}
	Ok(Some(classes))
}

/// Before/after contributions from distinct changed occurrences, by label.
type LabelContributions = BTreeMap<LabelStatisticKey, (LabelStatistic, LabelStatistic)>;

/// Accounts for mixed Base and overlay edits exactly once per occurrence.
///
/// # Errors
/// Rejects source/contribution failures and admits target/Text copies before
/// allocation.
fn label_contributions(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<BTreeMap<GraphId, LabelContributions>> {
	let mut targets = BTreeSet::new();
	for target in delta.overlays.iter().chain(&delta.memberships) {
		insert_target(&mut targets, *target, budget)?;
	}
	for &(graph, local) in &delta.incidence {
		for root in [before, after] {
			if let Some(occurrence) = root.occurrence(graph, local) {
				insert_target(&mut targets, (graph, occurrence.global_id()), budget)?;
			}
		}
	}
	for slot in &delta.global_slots {
		for (root, atom) in
			[(before, before.atoms.atom_by_slot(*slot)?), (after, after.atoms.atom_by_slot(*slot)?)]
		{
			if let Some(atom) = atom
				&& let Some(memberships) = root.memberships(atom.id())
			{
				for graph in memberships.keys() {
					insert_target(&mut targets, (*graph, atom.id()), budget)?;
				}
			}
		}
	}
	let mut result = BTreeMap::<GraphId, LabelContributions>::new();
	for (graph, global) in targets {
		for (root, source) in [(before, true), (after, false)] {
			let Some(local) = root.membership(graph, global) else {
				continue;
			};
			let atom = root.atom(global).ok_or_else(|| invalid("point label atom is absent"))?;
			let occurrence = root
				.occurrence(graph, local)
				.ok_or_else(|| invalid("point label occurrence is absent"))?;
			let Some(label) = occurrence.label() else {
				continue;
			};
			admit_entry(&result, &graph, budget)?;
			let labels = result.entry(graph).or_default();
			let text_bytes = label.estimated_clone_heap_bytes();
			budget.scratch(text_bytes)?;
			let key = LabelStatisticKey {
				kind: global.kind().ok_or_else(|| invalid("point label atom kind is null"))?,
				label: label.clone(),
			};
			if !labels.contains_key(&key) {
				budget.retain(text_bytes)?;
			}
			admit_entry(labels, &key, budget)?;
			let contribution = labels.entry(key).or_default();
			let contribution = if source {
				&mut contribution.0
			} else {
				&mut contribution.1
			};
			contribution.atom_count = contribution
				.atom_count
				.checked_add(1)
				.ok_or_else(|| invalid("label atom contributions overflow"))?;
			let count = u64::try_from(effective_property_count(atom.properties(), occurrence)?)
				.map_err(|_error| invalid("label property contribution exceeds u64"))?;
			contribution.property_count = contribution
				.property_count
				.checked_add(count)
				.ok_or_else(|| invalid("label property contributions overflow"))?;
		}
	}
	Ok(result)
}

/// Uses complete logical label keys to resolve unchanged dense namespaces.
///
/// # Errors
/// Rejects deferred candidates, missing counters, invalid pages, mismatches and
/// cumulative admission failures.
fn append_label_statistics_pages(
	store: &LocalStore,
	state: &LocalState,
	after: &DatabaseRoot,
	graph: GraphId,
	mut contributions: LabelContributions,
	pages: &mut PointPages,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<bool> {
	contributions.retain(|_, (old, new)| old != new);
	if contributions.is_empty() {
		return Ok(false);
	}
	if after.statistics.auxiliary_deferred() {
		return Err(invalid("point label candidate statistics are deferred"));
	}
	let statistics = after
		.statistics
		.graph(graph)
		.ok_or_else(|| invalid("point label statistics are absent"))?;
	let mut counts = [0_usize; 2];
	let mut targets = LabelStatisticsChanges::new();
	for (key, accepted) in &statistics.labels {
		let kind = match key.kind {
			AtomKind::Node => LabelKind::Node,
			AtomKind::Edge => LabelKind::Edge,

			_ => {
				return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
					"Unsupported runtime variant at the workspace boundary.",
				));
			}
		};
		let namespace = &mut counts[kind as usize];
		if let Some((old, new)) = contributions.remove(key) {
			// The publisher's original root can still be lazy after reopen.
			// Invert all changed contributions against the hydrated candidate.
			let mut expected = accepted.clone();
			expected.atom_count = accepted
				.atom_count
				.checked_sub(new.atom_count)
				.and_then(|count| count.checked_add(old.atom_count))
				.ok_or_else(|| invalid("point label source population overflows"))?;
			expected.property_count = accepted
				.property_count
				.checked_sub(new.property_count)
				.and_then(|count| count.checked_add(old.property_count))
				.ok_or_else(|| invalid("point label source counter overflows"))?;
			let id = u32::try_from(*namespace)
				.map_err(|_error| invalid("point label id exceeds u32"))?;
			let key = (kind, id);
			admit_entry(&targets, &key, budget)?;
			targets.insert(key, (expected, accepted.clone()));
		}
		*namespace = namespace
			.checked_add(1)
			.ok_or_else(|| invalid("point label namespace size overflows"))?;
	}
	if !contributions.is_empty() {
		return Err(invalid("point label target has no accepted counter"));
	}
	let space = DataSpaceId::Graph(graph);
	let file = LocalStore::statistics_file(state, space, DataFileKind::LabelStatistics)?;
	let length = store.visible_pages(state, space, file)?;
	for number in 0..length {
		if targets.is_empty() {
			break;
		}
		let next = (number + 1 < length).then(|| PageNo::new(number + 1));
		let key = PageKey::new(space, file, PageNo::new(number));
		let mut page = read_statistics_page(store, state, key, 0, budget)?;
		if rewrite_label_statistics_page(&mut page, key.page_no(), next, counts, &mut targets)? {
			retain_statistics_page(pages, key, page, budget)?;
		}
	}
	if !targets.is_empty() {
		return Err(invalid("point label statistics target is absent"));
	}
	Ok(true)
}

/// Keeps Base population encoding independent of graph overlay publication.
///
/// # Errors
/// Rejects missing classes, invalid ranges, source pages and admission
/// failures.
fn append_base_statistics_pages(
	store: &LocalStore,
	state: &LocalState,
	after: &DatabaseRoot,
	classes: BTreeMap<ClassId, u64>,
	pages: &mut PointPages,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	let base_file =
		LocalStore::statistics_file(state, DataSpaceId::Database, DataFileKind::ClassStatistics)?;
	let property_file = LocalStore::statistics_file(
		state,
		DataSpaceId::Database,
		DataFileKind::PropertyStatistics,
	)?;
	let key = PageKey::new(DataSpaceId::Database, base_file, PageNo::new(0));
	let mut page = read_statistics_page(store, state, key, 0, budget)?;
	let layout = rewrite_base_statistics_root(
		&mut page,
		after.statistics.base(),
		store.visible_pages(state, DataSpaceId::Database, base_file)?,
		store.visible_pages(state, DataSpaceId::Database, property_file)?,
	)?;
	retain_statistics_page(pages, key, page, budget)?;
	append_class_pages(
		store,
		state,
		DataSpaceId::Database,
		base_file,
		(layout.class_first, layout.class_pages),
		classes,
		pages,
		budget,
	)
}

/// Patches selected counters in a validated fixed class-page range.
///
/// # Errors
/// Rejects source pages, missing class targets and shared admission failures.
fn append_class_pages(
	store: &LocalStore,
	state: &LocalState,
	space: DataSpaceId,
	file: crate::storage::types::DataFileId,
	range: (Option<PageNo>, u32),
	mut classes: BTreeMap<ClassId, u64>,
	pages: &mut PointPages,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	if let Some(first) = range.0 {
		for index in 0..u64::from(range.1) {
			if classes.is_empty() {
				break;
			}
			let number = PageNo::new(
				first
					.get()
					.checked_add(index)
					.ok_or_else(|| invalid("class statistics address overflows"))?,
			);
			let next = (index + 1 < u64::from(range.1)).then(|| PageNo::new(number.get() + 1));
			let key = PageKey::new(space, file, number);
			// The fixed-width decoded entries fit within one additional page.
			let mut page = read_statistics_page(store, state, key, PAGE_SIZE, budget)?;
			if rewrite_class_statistics_page(&mut page, number, next, &mut classes)? {
				retain_statistics_page(pages, key, page, budget)?;
			}
		}
	}
	if !classes.is_empty() {
		return Err(invalid("point class statistics target is absent"));
	}
	Ok(())
}

/// Admits each distinct logical target before its set can grow.
///
/// # Errors
/// Preserves shared-envelope and tree-size overflow failures.
fn insert_target<Key: Ord>(
	targets: &mut BTreeSet<Key>,
	key: Key,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	if !targets.contains(&key) {
		budget.tree_entry::<Key>(targets.len())?;
		targets.insert(key);
	}
	Ok(())
}

/// Repeated counter updates reuse the existing admitted tree entry.
///
/// # Errors
/// Rejects vacant-entry growth before the caller inserts its key or value.
fn admit_entry<Key: Ord, Value>(
	values: &BTreeMap<Key, Value>,
	key: &Key,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	if !values.contains_key(key) {
		budget.tree_entry::<(Key, Value)>(values.len())?;
	}
	Ok(())
}

/// Read-only source frames and codec scratch do not survive page selection.
///
/// # Errors
/// Rejects the complete temporary peak before reading and allocating a frame,
/// then preserves the existing source validation and I/O errors.
fn read_statistics_page(
	store: &LocalStore,
	state: &LocalState,
	key: PageKey,
	decoder_scratch: usize,
	budget: &PublicationBudget,
) -> FerrosGrynnResult<Box<AlignedPage>> {
	budget.scratch(
		PAGE_SIZE
			.checked_add(decoder_scratch)
			.ok_or_else(|| invalid("statistics source scratch overflows usize"))?,
	)?;
	store.read_update_page(state, key)
}

/// Selected frames and their private WAL copies share the Base/graph account.
///
/// # Errors
/// Preserves cumulative admission and page-list allocation failures before the
/// prepared page can enter the unpublished output.
fn retain_statistics_page(
	pages: &mut PointPages,
	key: PageKey,
	page: Box<AlignedPage>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	// Three key slots cover the old and new backing during vector growth.
	budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
	budget.stage_pages(1)?;
	pages.try_reserve_exact(1).map_err(|source| {
		invalid("statistics page key allocation failed").with_technical_source(source)
	})?;
	pages.push((key, page));
	Ok(())
}
