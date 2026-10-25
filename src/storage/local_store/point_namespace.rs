//! One admitted label namespace shared by Hot, arrays and metadata publication.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	LocalState,
	LocalStore,
	cleanup::UnpublishedGeneration,
	layout::{LayoutBuilder, graph_prefix, invalid, selected_file_metadata_bytes},
	point_label::LabelTargets,
	point_occurrence::{SlotTransition, incidence_slots, structural_target},
};
use crate::{
	datastore::{ClassId, DataSpaceId, DatabaseRoot, GlobalAtomId, PublicationDelta},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId},
	memory::tree_retained_bytes,
	storage::{
		file_registry::DataFileRole,
		graph_index::{
			LabelDictionary,
			LabelDictionaryLayout,
			LabelKind,
			encode_label_dictionary,
			external_sort_run_budget,
		},
		graph_pages::{
			GraphLabelStatisticsLayout,
			GraphStatisticsLayout,
			decode_class_statistics_bounded,
			decode_statistics_root,
			encode_graph_label_statistics,
			encode_graph_statistics,
		},
		page::{PAGE_SIZE, PageLsn, PageNo},
		types::{DataFileId, DataFileKind, PageKey},
	},
	values::Text,
};

/// Conservative logical envelope; temporary scratch is checked above retained
/// data.
pub(super) struct PublicationBudget {
	/// Caller-selected ceiling, without changing production defaults.
	maximum: usize,
	/// Retained allocations and conservative reservations through publication.
	retained: usize,
	/// Namespace-owned page copies simultaneously retained by the WAL handle.
	staged_pages: usize,
}

impl PublicationBudget {
	/// Starts an empty publication envelope under the current builder policy.
	pub(super) fn new() -> Self {
		Self {
			maximum: external_sort_run_budget(),
			retained: 0,
			staged_pages: 0,
		}
	}

	/// Checks temporary work before allocation or output creation.
	pub(super) fn scratch(&self, bytes: usize) -> FerrosGrynnResult<()> {
		let total = self
			.retained
			.checked_add(bytes)
			.ok_or_else(|| invalid("namespace publication bytes overflow usize"))?;
		if total > self.maximum {
			return Err(FerrosGrynnError::storage_decode_limit_exceeded(
				"namespace publication",
				total as u64,
				self.maximum as u64,
			));
		}
		Ok(())
	}

	/// Admits a retained allocation before changing its owning collection.
	pub(super) fn retain(&mut self, bytes: usize) -> FerrosGrynnResult<()> {
		self.scratch(bytes)?;
		self.retained += bytes;
		Ok(())
	}

	/// Returns a zero-capable ceiling for a preparation that may allocate
	/// nothing.
	pub(super) fn available_bytes(&self) -> FerrosGrynnResult<usize> {
		self.maximum
			.checked_sub(self.retained)
			.ok_or_else(|| invalid("namespace available bytes underflow usize"))
	}

	/// Keeps an already admitted temporary live beside nested retained results.
	///
	/// # Errors
	/// Preserves admission and nested failures; releases only this scope's
	/// bytes.
	pub(super) fn with_scratch<T>(
		&mut self,
		bytes: usize,
		action: impl FnOnce(&mut Self) -> FerrosGrynnResult<T>,
	) -> FerrosGrynnResult<T> {
		self.retain(bytes)?;
		let result = action(self);
		self.retained = self
			.retained
			.checked_sub(bytes)
			.ok_or_else(|| invalid("namespace scratch reservation underflows usize"))?;
		result
	}

	/// Charges only a vacant tree entry; a newly empty tree owns no allocation.
	pub(super) fn tree_entry<Entry>(&mut self, old_count: usize) -> FerrosGrynnResult<()> {
		let old = if old_count == 0 {
			0
		} else {
			tree_retained_bytes::<Entry>(old_count)?
		};
		let next = old_count
			.checked_add(1)
			.ok_or_else(|| invalid("namespace tree count overflows usize"))?;
		let additional = tree_retained_bytes::<Entry>(next)?
			.checked_sub(old)
			.ok_or_else(|| invalid("namespace tree reserve decreases"))?;
		self.scratch(
			additional
				.checked_add(4096)
				.ok_or_else(|| invalid("namespace tree split scratch overflows usize"))?,
		)?;
		self.retain(additional)
	}

	/// Remaining logical run budget for a nested scalar streaming subtree.
	pub(super) fn remaining(&self) -> FerrosGrynnResult<usize> {
		self.scratch(1)?;
		self.maximum
			.checked_sub(self.retained)
			.filter(|bytes| *bytes != 0)
			.ok_or_else(|| invalid("namespace publication leaves no scalar run budget"))
	}

	/// Admits the WAL copy before allocating its prepared source image.
	pub(super) fn stage_pages(&mut self, count: usize) -> FerrosGrynnResult<()> {
		let (next, bytes) = self.staging_reservation(count)?;
		self.scratch(
			bytes
				.checked_add(4096)
				.ok_or_else(|| invalid("namespace WAL tree scratch overflows usize"))?,
		)?;
		self.retain(bytes)?;
		self.staged_pages = next;
		Ok(())
	}

	/// Computes the existing WAL reservation without advancing its account.
	///
	/// # Errors
	/// Preserves the staged-count overflow and WAL sizing errors.
	pub(super) fn staging_reservation(&self, count: usize) -> FerrosGrynnResult<(usize, usize)> {
		let next = self
			.staged_pages
			.checked_add(count)
			.ok_or_else(|| invalid("namespace staged page count overflows usize"))?;
		let bytes = crate::storage::wal::TxHandle::staging_bytes(next)?
			.checked_sub(crate::storage::wal::TxHandle::staging_bytes(self.staged_pages)?)
			.ok_or_else(|| invalid("namespace staging reserve decreases"))?;
		Ok((next, bytes))
	}
}

/// Dense IDs are derived once from the source and final sorted logical names.
pub(super) struct NamespaceGraph {
	/// Source names resolve physical source filenames and Hot expectations.
	pub(super) before: LabelDictionary,
	/// Final names resolve target filenames, Hot values and statistics
	/// references.
	pub(super) after: LabelDictionary,
	/// Exact dictionary page count and encoder scratch reservation.
	layout: LabelDictionaryLayout,
}

/// All selected names and namespace changes for one final replay candidate.
pub(super) struct LabelPlan {
	/// Explicit replay targets bound insertion scratch independently of
	/// population.
	pub(super) incidence: Vec<SlotTransition>,
	/// Explicit owners also retain transient empty membership buckets.
	pub(super) memberships: Vec<(GraphId, GlobalAtomId)>,
	/// Changed memberships or names whose numeric ID changes.
	pub(super) targets: LabelTargets,
	/// Only graphs with a different final set of names.
	pub(super) graphs: BTreeMap<GraphId, NamespaceGraph>,
	/// New class-key layouts replace only graph metadata, retaining
	/// auxiliaries.
	pub(super) class_graphs: BTreeSet<GraphId>,
	/// Shared retained/scratch account used by downstream physical builders.
	pub(super) budget: PublicationBudget,
}

impl LabelPlan {
	/// Starts a standalone envelope for isolated physical-plan tests.
	#[cfg(test)]
	pub(super) fn build(
		store: &LocalStore,
		state: &LocalState,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
	) -> FerrosGrynnResult<Self> {
		Self::build_with_budget(store, state, before, after, delta, PublicationBudget::new())
	}

	/// Carries already retained context bytes into every physical builder.
	pub(super) fn build_with_budget(
		store: &LocalStore,
		state: &LocalState,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
		mut budget: PublicationBudget,
	) -> FerrosGrynnResult<Self> {
		// Canonical paths and fixed builder metadata coexist with every phase.
		budget.retain(4096)?;
		let incidence = incidence_slots(before, after, delta, &mut budget)?;
		budget.retain(
			delta
				.memberships
				.len()
				.checked_mul(size_of::<(GraphId, GlobalAtomId)>())
				.ok_or_else(|| invalid("membership targets overflow usize"))?,
		)?;
		let mut result = Self {
			incidence,
			memberships: delta.memberships.iter().copied().collect(),
			targets: LabelTargets::new(),
			graphs: BTreeMap::new(),
			class_graphs: BTreeSet::new(),
			budget,
		};
		for target in &result.incidence {
			let old = target
				.before
				.and_then(|state| state.occurrence)
				.and_then(|(local, _)| before.occurrence(target.graph, local))
				.and_then(|occurrence| occurrence.label());
			let new = target
				.after
				.and_then(|state| state.occurrence)
				.and_then(|(local, _)| after.occurrence(target.graph, local))
				.and_then(|occurrence| occurrence.label());
			if old != new {
				for name in old.into_iter().chain(new) {
					insert_target(
						&mut result.targets,
						&mut result.budget,
						target.graph,
						target.kind,
						name,
					)?;
				}
			}
		}
		result.budget.retain(
			result
				.targets
				.len()
				.checked_mul(size_of::<GraphId>())
				.ok_or_else(|| invalid("namespace graph targets overflow usize"))?,
		)?;
		let graphs = result.targets.keys().copied().collect::<Vec<_>>();
		for graph in graphs {
			let old = &before
				.indexes
				.graphs
				.get(&graph)
				.ok_or_else(|| invalid("source label indexes are absent"))?
				.labels;
			let new = &after
				.indexes
				.graphs
				.get(&graph)
				.ok_or_else(|| invalid("final label indexes are absent"))?
				.labels;
			if old.keys().eq(new.keys()) {
				continue;
			}
			let source = copy_dictionary(old.keys(), &mut result.budget)?;
			let accepted = copy_dictionary(new.keys(), &mut result.budget)?;
			let layout = LabelDictionaryLayout::measure(&accepted)?;
			for (kind, name) in old.keys() {
				let kind_id = label_kind(*kind);
				if source.id(kind_id, name.as_str()) != accepted.id(kind_id, name.as_str()) {
					insert_target(&mut result.targets, &mut result.budget, graph, *kind, name)?;
				}
			}
			result.budget.tree_entry::<(GraphId, NamespaceGraph)>(result.graphs.len())?;
			result.graphs.insert(graph, NamespaceGraph {
				before: source,
				after: accepted,
				layout,
			});
		}
		let mut previous = None;
		for &(graph, _) in &delta.memberships {
			if previous == Some(graph) {
				continue;
			}
			previous = Some(graph);
			if class_layout_changes(store, state, before, after, graph, &result.budget)? {
				result.budget.tree_entry::<GraphId>(result.class_graphs.len())?;
				result.class_graphs.insert(graph);
			}
		}
		Ok(result)
	}

	/// Reused replay generations share one physical slot and one final
	/// renderer.
	pub(super) fn is_structural(
		&self,
		roots: (&DatabaseRoot, &DatabaseRoot),
		graph: GraphId,
		local: crate::graph::AtomId,
	) -> bool {
		let Some(kind) = local.kind() else {
			return false;
		};
		self.incidence
			.binary_search_by_key(&(graph, kind, local.slot()), |target| {
				(target.graph, target.kind, target.slot)
			})
			.is_ok_and(|index| structural_target(&self.incidence[index], &self.memberships, roots))
	}

	/// Supplies the exact dictionary ceiling before graph superblock encoding.
	pub(super) fn dictionary_pages(&self, graph: GraphId) -> FerrosGrynnResult<Option<u64>> {
		self.graphs
			.get(&graph)
			.map(|namespace| {
				u64::try_from(namespace.layout.pages)
					.map_err(|_error| invalid("namespace dictionary page count exceeds u64"))
			})
			.transpose()
	}

	/// Prepares selected metadata; property statistics keep their current
	/// identity.
	pub(super) fn append_metadata(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		after: &DatabaseRoot,
		generation: &str,
		layout: &mut LayoutBuilder,
		cleanup: &mut Option<UnpublishedGeneration>,
		retired: &mut BTreeSet<(DataSpaceId, DataFileId)>,
	) -> FerrosGrynnResult<BTreeMap<GraphId, DataFileId>> {
		let mut files = BTreeMap::new();
		if (!self.graphs.is_empty() || !self.class_graphs.is_empty())
			&& after.statistics.auxiliary_deferred()
		{
			return Err(invalid("namespace candidate statistics are deferred"));
		}
		for (&graph, namespace) in &self.graphs {
			let binding = state
				.bindings
				.graphs
				.iter()
				.find(|binding| binding.graph_id == graph)
				.ok_or_else(|| invalid("namespace graph binding is absent"))?;
			let space = DataSpaceId::Graph(graph);
			let property =
				LocalStore::statistics_file(state, space, DataFileKind::PropertyStatistics)?;
			let property_pages = usize::try_from(store.visible_pages(state, space, property)?)
				.map_err(|_error| invalid("retained property statistics pages exceed usize"))?;
			let statistics = after
				.statistics
				.graph(graph)
				.ok_or_else(|| invalid("namespace statistics are absent"))?;
			let measured = GraphLabelStatisticsLayout::measure(statistics, property_pages)?;
			let prefix = graph_prefix(graph, generation);
			let dictionary_bytes = namespace
				.layout
				.pages
				.checked_mul(PAGE_SIZE + size_of::<PageNo>())
				.ok_or_else(|| invalid("namespace dictionary output overflows usize"))?;
			self.budget.retain(dictionary_bytes)?;
			self.budget.stage_pages(namespace.layout.pages)?;
			self.budget.scratch(namespace.layout.scratch_bytes)?;
			let dictionary = encode_label_dictionary(&namespace.after, PageLsn::ZERO)?;
			if dictionary.len() != namespace.layout.pages {
				return Err(invalid("namespace dictionary sizing disagrees with encoding"));
			}
			self.budget.retain(measured.output_bytes)?;
			self.budget.stage_pages(measured.output_pages)?;
			self.budget.scratch(measured.scratch_bytes)?;
			let statistics = encode_graph_label_statistics(
				statistics,
				&namespace.after,
				&measured,
				PageLsn::ZERO,
			)?;
			for name in ["labels.fgb", "stats/graph_stats.fgb", "stats/label_stats.fgb"] {
				self.budget.retain(selected_file_metadata_bytes(&format!("{prefix}/{name}"))?)?;
			}
			self.budget.tree_entry::<(GraphId, DataFileId)>(files.len())?;
			cleanup.get_or_insert_with(|| {
				UnpublishedGeneration::for_database(&store.root, after, generation)
			});
			let file = layout.add(
				space,
				DataFileKind::Labels,
				DataFileRole::WalBackfilled,
				format!("{prefix}/labels.fgb"),
				None,
				dictionary,
			)?;
			self.budget.tree_entry::<(DataSpaceId, DataFileId)>(retired.len())?;
			retired.insert((space, binding.files.labels));
			files.insert(graph, file);
			for (kind, name, pages) in [
				(DataFileKind::Statistics, "stats/graph_stats.fgb", statistics.graph),
				(DataFileKind::LabelStatistics, "stats/label_stats.fgb", statistics.labels),
			] {
				let previous = LocalStore::statistics_file(state, space, kind)?;
				self.budget.tree_entry::<(DataSpaceId, DataFileId)>(retired.len())?;
				layout.add(
					space,
					kind,
					DataFileRole::WalBackfilled,
					format!("{prefix}/{name}"),
					None,
					pages,
				)?;
				retired.insert((space, previous));
			}
		}
		self.append_class_metadata(store, state, after, generation, layout, cleanup, retired)?;
		Ok(files)
	}

	/// Carries stale degree payloads into a changed class layout without
	/// ANALYZE.
	fn append_class_metadata(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		after: &DatabaseRoot,
		generation: &str,
		layout: &mut LayoutBuilder,
		cleanup: &mut Option<UnpublishedGeneration>,
		retired: &mut BTreeSet<(DataSpaceId, DataFileId)>,
	) -> FerrosGrynnResult<()> {
		for &graph in &self.class_graphs {
			if self.graphs.contains_key(&graph) {
				continue;
			}
			let space = DataSpaceId::Graph(graph);
			let previous = LocalStore::statistics_file(state, space, DataFileKind::Statistics)?;
			let labels = LocalStore::statistics_file(state, space, DataFileKind::LabelStatistics)?;
			let properties =
				LocalStore::statistics_file(state, space, DataFileKind::PropertyStatistics)?;
			let label_pages = usize::try_from(store.visible_pages(state, space, labels)?)
				.map_err(|_error| invalid("retained label statistics pages exceed usize"))?;
			let property_pages = usize::try_from(store.visible_pages(state, space, properties)?)
				.map_err(|_error| invalid("retained property statistics pages exceed usize"))?;
			let statistics = after
				.statistics
				.graph(graph)
				.ok_or_else(|| invalid("class-layout graph statistics are absent"))?;
			let measured = GraphStatisticsLayout::measure(statistics, label_pages, property_pages)?;
			let names = &after
				.indexes
				.graphs
				.get(&graph)
				.ok_or_else(|| invalid("class-layout graph indexes are absent"))?
				.labels;
			let dictionary = copy_dictionary(names.keys(), &mut self.budget)?;
			self.budget.retain(measured.output_bytes)?;
			self.budget.stage_pages(measured.output_pages)?;
			self.budget.scratch(measured.scratch_bytes)?;
			let pages = encode_graph_statistics(statistics, &dictionary, &measured, PageLsn::ZERO)?;
			let path = format!("{}/stats/graph_stats.fgb", graph_prefix(graph, generation));
			self.budget.retain(selected_file_metadata_bytes(&path)?)?;
			self.budget.tree_entry::<(DataSpaceId, DataFileId)>(retired.len())?;
			cleanup.get_or_insert_with(|| {
				UnpublishedGeneration::for_database(&store.root, after, generation)
			});
			layout.add(
				space,
				DataFileKind::Statistics,
				DataFileRole::WalBackfilled,
				path,
				None,
				pages,
			)?;
			retired.insert((space, previous));
		}
		Ok(())
	}
}

/// Deferred source maps are read from validated class pages, never lookup keys.
fn class_layout_changes(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	graph: GraphId,
	budget: &PublicationBudget,
) -> FerrosGrynnResult<bool> {
	if after.statistics.auxiliary_deferred() {
		return Err(invalid("membership candidate statistics are deferred"));
	}
	let accepted = &after
		.statistics
		.graph(graph)
		.ok_or_else(|| invalid("membership graph statistics are absent"))?
		.classes;
	if !before.statistics.auxiliary_deferred() {
		let source = &before
			.statistics
			.graph(graph)
			.ok_or_else(|| invalid("source graph statistics are absent"))?
			.classes;
		return Ok(!source.keys().eq(accepted.keys()));
	}
	let maximum = before.schema.classes.len();
	budget.scratch(
		tree_retained_bytes::<(ClassId, u64)>(maximum)?
			.checked_add(4 * PAGE_SIZE)
			.ok_or_else(|| invalid("source class statistics scratch overflows usize"))?,
	)?;
	let space = DataSpaceId::Graph(graph);
	let file = LocalStore::statistics_file(state, space, DataFileKind::Statistics)?;
	let labels = LocalStore::statistics_file(state, space, DataFileKind::LabelStatistics)?;
	let properties = LocalStore::statistics_file(state, space, DataFileKind::PropertyStatistics)?;
	let root = store.read_update_page(state, PageKey::new(space, file, PageNo::new(0)))?;
	let (_, layout) = decode_statistics_root(
		&root,
		store.visible_pages(state, space, file)?,
		store.visible_pages(state, space, labels)?,
		store.visible_pages(state, space, properties)?,
	)?;
	let source = decode_class_statistics_bounded(
		|number, output| {
			store.read_update_page_into(state, PageKey::new(space, file, number), output)
		},
		layout,
		maximum,
	)?;
	if source.keys().any(|class| before.schema.class_by_id(*class).is_none()) {
		return Err(invalid("source statistics reference an absent class"));
	}
	Ok(!source.keys().eq(accepted.keys()))
}

/// Adds one name only after admitting its owning graph tree and name storage.
fn insert_target(
	targets: &mut LabelTargets,
	budget: &mut PublicationBudget,
	graph: GraphId,
	kind: AtomKind,
	name: &Text,
) -> FerrosGrynnResult<()> {
	if !targets.contains_key(&graph) {
		budget.tree_entry::<(GraphId, BTreeSet<(AtomKind, Text)>)>(targets.len())?;
		targets.insert(graph, BTreeSet::new());
	}
	let labels =
		targets.get_mut(&graph).ok_or_else(|| invalid("namespace target graph is absent"))?;
	budget.scratch(name.estimated_clone_heap_bytes())?;
	if !labels.contains(&(kind, name.clone())) {
		budget.tree_entry::<(AtomKind, Text)>(labels.len())?;
		budget.retain(name.estimated_clone_heap_bytes())?;
		labels.insert((kind, name.clone()));
	}
	Ok(())
}

/// Copies already distinct sorted names without a second owned sorting tree.
fn copy_dictionary<'name>(
	names: impl Clone + Iterator<Item = &'name (AtomKind, Text)>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<LabelDictionary> {
	let mut counts = [0_usize; 2];
	let mut bytes = 0_usize;
	for (kind, name) in names.clone() {
		counts[label_kind(*kind) as usize] += 1;
		bytes = bytes
			.checked_add(size_of::<String>())
			.and_then(|bytes| bytes.checked_add(name.len()))
			.ok_or_else(|| invalid("namespace owned names overflow usize"))?;
	}
	budget.retain(bytes)?;
	let mut dictionary = LabelDictionary::default();
	dictionary
		.node_labels
		.try_reserve_exact(counts[0])
		.map_err(|_error| invalid("namespace node names allocation failed"))?;
	dictionary
		.edge_labels
		.try_reserve_exact(counts[1])
		.map_err(|_error| invalid("namespace edge names allocation failed"))?;
	for (kind, name) in names {
		match kind {
			AtomKind::Node => dictionary.node_labels.push(name.as_str().to_owned()),
			AtomKind::Edge => dictionary.edge_labels.push(name.as_str().to_owned()),
		}
	}
	Ok(dictionary)
}

/// Maps runtime kinds to the two independent durable label namespaces.
pub(super) const fn label_kind(kind: AtomKind) -> LabelKind {
	match kind {
		AtomKind::Node => LabelKind::Node,
		AtomKind::Edge => LabelKind::Edge,
	}
}

#[cfg(test)]
/// Budget failures leave the retained envelope unchanged.
mod tests {
	use super::*;
	use crate::test_assertions::AssertResult as _;

	/// Scratch and retained admission reject before advancing the account.
	#[test]
	fn namespace_budget_rejection_preserves_retained_state() {
		let mut budget = PublicationBudget {
			maximum: 8192,
			retained: 0,
			staged_pages: 0,
		};
		budget.tree_entry::<u64>(0).assert_ok();
		let retained = budget.retained;
		assert!(retained > 0);
		assert!(budget.scratch(8192).is_err());
		assert!(budget.retain(8192).is_err());
		assert_eq!(budget.retained, retained);
		assert!(budget.retain(usize::MAX).is_err());
		assert_eq!(budget.retained, retained);
	}

	/// A failed next staging reservation preserves the accepted page
	/// population.
	#[test]
	fn namespace_staging_budget_rejects_before_advancing_copies() {
		let mut budget = PublicationBudget {
			maximum: crate::storage::wal::TxHandle::staging_bytes(2).assert_ok() + 4096,
			retained: 0,
			staged_pages: 0,
		};
		budget.stage_pages(2).assert_ok();
		let retained = budget.retained;
		assert!(budget.stage_pages(1).is_err());
		assert_eq!(budget.staged_pages, 2);
		assert_eq!(budget.retained, retained);
	}
	/// Nested retained results survive while only the temporary scope is
	/// released.
	#[test]
	fn namespace_scratch_scope_restores_after_success_and_nested_failure() {
		let mut budget = PublicationBudget {
			maximum: 128,
			retained: 0,
			staged_pages: 0,
		};
		budget
			.with_scratch(64, |budget| {
				assert_eq!(budget.available_bytes()?, 64);
				budget.retain(16)
			})
			.assert_ok();
		assert_eq!(budget.retained, 16);
		let error = budget
			.with_scratch(64, |budget| {
				budget.retain(8)?;
				budget.retain(128)
			})
			.assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
		));
		assert_eq!(budget.retained, 24);
		assert_eq!(budget.available_bytes().assert_ok(), 104);
	}

	/// A zero-capable preparation ceiling does not admit any nonempty scratch.
	#[test]
	fn namespace_empty_preparation_accepts_zero_and_rejects_before_entering_scope() {
		let mut budget = PublicationBudget {
			maximum: 128,
			retained: 128,
			staged_pages: 0,
		};
		assert_eq!(budget.available_bytes().assert_ok(), 0);
		assert!(budget.remaining().is_err());
		let mut entered = false;
		assert!(
			budget
				.with_scratch(1, |_| {
					entered = true;
					Ok(())
				})
				.is_err()
		);
		assert!(!entered);
		assert_eq!(budget.retained, 128);
	}
}
