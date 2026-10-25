//! Stable graph identities share Hot patches, label remaps and overlay appends.

use std::{
	collections::{BTreeMap, BTreeSet},
	sync::Arc,
};

use super::{
	LocalState,
	LocalStore,
	layout::invalid,
	point_namespace::{LabelPlan, NamespaceGraph, PublicationBudget, label_kind},
	point_update::supports_point_indexes,
};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot, GlobalAtomId, GraphRoot, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphId, GraphKind},
	storage::{
		atom_page::PropertyRef,
		graph_pages::{
			EdgeSlotFlags,
			GraphArenaBindings,
			NodeSlotFlags,
			hot_slot_address,
			read_edge_hot_slot,
			read_node_hot_slot,
			write_edge_hot_slot,
			write_node_hot_slot,
		},
		graph_superblock::GraphSuperblock,
		page::{AlignedPage, PageLsn, PageNo},
		types::{DataFileId, PageKey},
	},
	values::storage::EncodeContext,
};

/// Overlay payload ownership is separate from Hot-slot identity validation.
mod overlays;

/// Metadata admission and physical weight encoding share one stable-state
/// contract.
mod stable;

/// Existing direct-parent directory pages share the same publication plan.
mod subgraphs;

/// Leaf changes share eligibility and the stable-scope fast publisher.
mod leaf_rebind;

/// Changed dense direct scopes share source validation and private Hot frames.
mod child_remap;

/// Stable leaf replacement updates supported ancestor arenas by full path.
mod ancestor_leaf;

/// T canonical maps share cumulative admission with the earlier M/R/S maps.
#[cfg(test)]
mod depth_resource_tests;

/// Keeps lower-level canonical planning independent of publication policy.
///
/// # Errors
/// Preserves source/cycle failures and the shared logical envelope's rejection.
fn admitted_arena(
	root: &DatabaseRoot,
	graph: GraphId,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<GraphArenaBindings> {
	GraphArenaBindings::build_admitted(root, graph, |growth, scratch| {
		let total = growth
			.checked_add(scratch)
			.ok_or_else(|| invalid("child remap admission bytes overflow usize"))?;
		budget.scratch(total)?;
		budget.retain(growth)
	})
}

pub(super) use overlays::GraphOverlays;
use stable::{stable_layout, stable_target, stable_weights};

/// One source identity and its changed fields, combined before sealing.
struct HotSlotChange {
	/// Generation-aware local address validated against the source Hot slot.
	local: AtomId,
	/// Global owner must agree with the physical source.
	global: GlobalAtomId,
	/// None preserves the source; Some may explicitly remove the overlay.
	overlay: Option<PropertyRef>,
	/// A checked source/replacement pair; None preserves the physical weight.
	weight: Option<(Option<f64>, Option<f64>)>,
	/// Source and final namespace-local IDs, checked against the physical
	/// source.
	label: Option<(Option<u32>, Option<u32>)>,
}

/// Heap-stable frames and prepared runtime ceilings for existing graphs.
pub(super) struct GraphUpdatePlan {
	/// Complete changed pages, without any new file identity.
	pub(super) pages: Vec<(PageKey, Box<AlignedPage>)>,
	/// Every key already exists under the same LocalState publication lock.
	pub(super) superblocks: BTreeMap<GraphId, GraphSuperblock>,
	/// Final canonical owners are installed only after the same WAL commit.
	pub(super) arenas: BTreeMap<GraphId, GraphArenaBindings>,
}

/// Accepts a closed journal for metadata, memberships and same-owner rewires.
pub(super) fn eligible(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
) -> bool {
	if leaf_rebind::candidate(before, after, delta)
		.or_else(|| leaf_rebind::remap_candidate(before, after, delta))
		.is_some()
	{
		return true;
	}
	if !delta.memberships.is_empty()
		|| delta.incidence.iter().any(|&(graph, local)| {
			super::point_occurrence::incidence_rewire(before, after, graph, local)
		}) {
		return super::point_incidence::eligible(before, after, delta);
	}
	for &(graph, global) in &delta.overlays {
		if !stable_target(before, after, graph, global) {
			return false;
		}
	}
	for &(graph, local) in &delta.incidence {
		let Some(old) = before.occurrence(graph, local) else {
			return false;
		};
		if !stable_target(before, after, graph, old.global_id()) {
			return false;
		}
	}
	for (id, graph) in &before.graphs {
		let Some(next) = after.graphs.get(id) else {
			return false;
		};
		if Arc::ptr_eq(graph, next) {
			continue;
		}
		if !stable_graph(*id, graph, next, delta) {
			return false;
		}
	}
	true
}

/// Metadata-only graphs retain the original stable-layout admission contract.
pub(super) fn stable_graph(
	id: GraphId,
	old: &GraphRoot,
	new: &GraphRoot,
	delta: &PublicationDelta,
) -> bool {
	let incidence = delta.incidence.iter().any(|(target, _)| *target == id);
	delta.point_graph_only
		&& (incidence || delta.overlays.iter().any(|(target, _)| *target == id))
		&& old.reference() == new.reference()
		&& (incidence || old.tensor_revision == new.tensor_revision)
		&& old.occurrences().len() == new.occurrences().len()
		&& [AtomKind::Node, AtomKind::Edge]
			.into_iter()
			.all(|kind| old.slot_capacity(kind) == new.slot_capacity(kind))
}

/// Keeps the supported public family predicate shared by both target kinds.
pub(super) fn plain_membership_graph(root: &DatabaseRoot, graph: GraphId) -> bool {
	root.schema.graphs.get(&graph).is_some_and(|schema| {
		!schema.is_internal()
			&& matches!(
				schema.reference().kind(),
				GraphKind::Graph
					| GraphKind::Dag
					| GraphKind::Forest
					| GraphKind::Tree
					| GraphKind::RootedTree
					| GraphKind::Metagraph
					| GraphKind::Hypergraph
			)
	})
}

/// Mixed metadata preserves full owner identities and the complete occurrence
/// layout.
pub(super) fn stable_metadata_target(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	graph: GraphId,
	global: GlobalAtomId,
) -> bool {
	if delta.memberships.contains(&(graph, global)) || !stable_target(before, after, graph, global)
	{
		return false;
	}
	plain_membership_graph(after, graph)
}

/// Rewires share plain-family and specialized-class admission with memberships.
pub(super) fn supported_rewire(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	graph: GraphId,
	global: GlobalAtomId,
) -> bool {
	let Some(local) = after.membership(graph, global) else {
		return false;
	};
	plain_membership_graph(after, graph)
		&& after.atom(global).is_some_and(|owner| supports_point_indexes(after, owner.class_id()))
		&& super::point_occurrence::incidence_rewire(before, after, graph, local)
}

impl GraphUpdatePlan {
	/// Prepares Hot patches, append pages and graph checkpoint metadata.
	///
	/// # Errors
	/// Propagates missing bindings, source identity, codec, overflow and I/O
	/// errors.
	pub(super) fn build(
		store: &LocalStore,
		state: &LocalState,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
		labels: &mut LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<Self> {
		let mut result = Self {
			pages: Vec::new(),
			superblocks: BTreeMap::new(),
			arenas: BTreeMap::new(),
		};
		if let Some(rebind) = leaf_rebind::candidate(before, after, delta) {
			let graph = rebind.graph;
			result.prepare_leaf_rebind(store, state, (before, after), rebind, labels, context)?;
			result.prepare_ancestor_arenas(
				store,
				state,
				(before, after),
				graph,
				labels,
				context,
			)?;
			result.prepare_subgraphs(store, state, (before, after), delta, &mut labels.budget)?;
			return Ok(result);
		}
		if let Some(rebind) = leaf_rebind::remap_candidate(before, after, delta) {
			result.prepare_child_remap(store, state, (before, after), rebind, labels, context)?;
			result.prepare_subgraphs(store, state, (before, after), delta, &mut labels.budget)?;
			return Ok(result);
		}
		let mut metadata = collect_targets(before, after, delta, labels)?.into_iter().peekable();
		let mut start = 0;
		while start < labels.incidence.len() || metadata.peek().is_some() {
			let structural_graph = labels.incidence.get(start).map(|target| target.graph);
			let metadata_graph = metadata.peek().map(|(graph, _)| *graph);
			let graph = match (structural_graph, metadata_graph) {
				(Some(left), Some(right)) => left.min(right),
				(Some(graph), None) | (None, Some(graph)) => graph,
				(None, None) => return Err(invalid("graph publication target disappeared")),
			};
			let mut end = start;
			while end < labels.incidence.len() && labels.incidence[end].graph == graph {
				end += 1;
			}
			let targets = if metadata_graph == Some(graph) {
				metadata.next().ok_or_else(|| invalid("graph metadata target disappeared"))?.1
			} else {
				BTreeSet::new()
			};
			// Only live source/final overlays or stable metadata need a tail
			// and value-context borrow.
			let mut overlays = if targets.is_empty()
				&& !super::point_incidence::has_overlays(before, after, labels, start..end)
			{
				None
			} else {
				let files = state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == graph)
					.ok_or_else(|| invalid("point overlay graph binding is absent"))?
					.files;
				let offset = store.visible_pages(state, files.space(), files.overlays)?;
				Some((files, offset, GraphOverlays::new(offset, context)))
			};
			let first_frame = result.pages.len();
			if start != end {
				super::point_incidence::build_graph(
					store,
					state,
					(before, after),
					start..end,
					labels,
					&mut result,
					overlays.as_mut().map(|(_, _, overlays)| overlays),
				)?;
			}
			let incidence_frames = first_frame..result.pages.len();
			if let Some((files, offset, mut overlays)) = overlays {
				let changed = result.prepare_graph_metadata(
					store,
					state,
					(before, after),
					(graph, targets, incidence_frames.clone()),
					labels,
					&mut overlays,
				)?;
				result.finish_graph(
					state,
					(files, offset, incidence_frames, changed),
					overlays,
					labels,
				)?;
			}
			start = end;
		}
		result.prepare_subgraphs(store, state, (before, after), delta, &mut labels.budget)?;
		Ok(result)
	}

	/// Patches stable fields only within the current graph's incidence frame
	/// range.
	///
	/// # Errors
	/// Preserves identity, admission, codec, overflow and I/O failures.
	fn prepare_graph_metadata(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		target: (GraphId, BTreeSet<GlobalAtomId>, std::ops::Range<usize>),
		labels: &mut LabelPlan,
		overlays: &mut GraphOverlays<'_>,
	) -> FerrosGrynnResult<bool> {
		let (before, after) = roots;
		let (graph, targets, incidence_pages) = target;
		let files = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph)
			.ok_or_else(|| invalid("point overlay graph binding is absent"))?
			.files;
		let space = DataSpaceId::Graph(graph);
		let mut hot = BTreeMap::<(DataFileId, PageNo), Vec<HotSlotChange>>::new();
		for global in targets {
			if let Some(change) = prepare_hot_change(
				before,
				after,
				graph,
				global,
				labels.graphs.get(&graph),
				overlays,
				&mut labels.budget,
			)? {
				let file = match change.local.kind() {
					Some(AtomKind::Node) => files.nodes,
					Some(AtomKind::Edge) => files.edges,
					None => return Err(invalid("point graph local kind is null")),
				};
				let number = hot_slot_address(change.local.slot())?.0;
				let prepared = self.pages[incidence_pages.clone()]
					.iter()
					.any(|(key, _)| *key == PageKey::new(space, file, number));
				push_hot_change(&mut hot, file, change, prepared, &mut labels.budget)?;
			}
		}
		if hot.is_empty() && !labels.graphs.contains_key(&graph) {
			return Ok(false);
		}
		for ((file, number), changes) in hot {
			labels.budget.scratch(
				crate::storage::graph_pages::HOT_SLOTS_PER_PAGE
					.checked_mul(
						size_of::<crate::storage::graph_pages::NodeHotSlot>()
							+ size_of::<crate::storage::graph_pages::EdgeHotSlot>(),
					)
					.ok_or_else(|| invalid("Hot patch scratch overflows usize"))?,
			)?;
			let key = PageKey::new(space, file, number);
			if let Some((_, page)) = self.pages[incidence_pages.clone()]
				.iter_mut()
				.find(|(prepared, _)| *prepared == key)
			{
				patch_hot_page(page, number, &changes)?;
			} else {
				let mut page = store.read_update_page(state, key)?;
				patch_hot_page(&mut page, number, &changes)?;
				self.pages.push((key, page));
			}
		}
		Ok(true)
	}

	/// Seals one shared append range and replaces an existing incidence header
	/// once.
	///
	/// # Errors
	/// Preserves append, admission, source metadata and checkpoint-authority
	/// errors.
	fn finish_graph(
		&mut self,
		state: &LocalState,
		target: (crate::storage::graph_store::GraphFileSet, u64, std::ops::Range<usize>, bool),
		overlays: GraphOverlays<'_>,
		labels: &mut LabelPlan,
	) -> FerrosGrynnResult<()> {
		let (files, start, incidence_pages, metadata_changed) = target;
		let graph = files.graph_id;
		let space = files.space();
		let pages = overlays.finish()?;
		if pages.is_empty() && !metadata_changed {
			return Ok(());
		}
		let count = u64::try_from(pages.len())
			.map_err(|_error| invalid("overlay append length exceeds u64"))?;
		let end =
			start.checked_add(count).ok_or_else(|| invalid("overlay append ceiling overflows"))?;
		for (offset, page) in pages.into_iter().enumerate() {
			let offset =
				u64::try_from(offset).map_err(|_error| invalid("overlay offset exceeds u64"))?;
			let number = start
				.checked_add(offset)
				.ok_or_else(|| invalid("overlay append address overflows"))?;
			self.pages
				.push((PageKey::new(space, files.overlays, PageNo::new(number)), Box::new(page)));
		}
		let prepared_header = self.superblocks.get(&graph);
		let mut header = *prepared_header
			.or_else(|| state.graph_superblocks.get(&graph))
			.ok_or_else(|| invalid("point overlay graph superblock is absent"))?;
		if prepared_header.is_some() {
			labels.budget.scratch(crate::storage::page::PAGE_SIZE)?;
		} else {
			labels.budget.retain(
				crate::storage::page::PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>(),
			)?;
			labels.budget.stage_pages(1)?;
			labels.budget.tree_entry::<(GraphId, GraphSuperblock)>(self.superblocks.len())?;
		}
		header.page_lsn = PageLsn::ZERO;
		header.overlays_pages = end;
		if let Some(pages) = labels.dictionary_pages(graph)? {
			header.labels_pages = pages;
		}
		// Subordinate home metadata cannot authorize a newer checkpoint
		// tuple.
		header.checkpoint_generation = state.database_superblock.generation;
		header.backfilled_lsn = state.database_superblock.checkpoint_lsn;
		header.checkpoint_record_lsn = state.database_superblock.checkpoint_record_lsn;
		let key = PageKey::new(space, files.superblock, PageNo::new(0));
		if let Some((_, page)) =
			self.pages[incidence_pages].iter_mut().find(|(prepared, _)| *prepared == key)
		{
			**page = header.encode()?;
		} else {
			self.pages.push((key, Box::new(header.encode()?)));
		}
		self.superblocks.insert(graph, header);
		Ok(())
	}
}

/// Collects explicit targets and only the postings whose numeric label ID
/// shifts.
fn collect_targets(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	labels: &mut LabelPlan,
) -> FerrosGrynnResult<BTreeMap<GraphId, BTreeSet<GlobalAtomId>>> {
	let mut targets = BTreeMap::new();
	for &(graph, global) in &delta.overlays {
		// Explicit lifecycle owners already receive their final overlay in
		// incidence.
		if delta.memberships.contains(&(graph, global))
			|| after
				.membership(graph, global)
				.is_some_and(|local| labels.is_structural((before, after), graph, local))
		{
			continue;
		}
		insert_hot_target(&mut targets, &mut labels.budget, graph, global)?;
	}
	for &(graph, local) in &delta.incidence {
		if labels.is_structural((before, after), graph, local) {
			continue;
		}
		let occurrence = after
			.occurrence(graph, local)
			.ok_or_else(|| invalid("point graph incidence target is absent"))?;
		insert_hot_target(&mut targets, &mut labels.budget, graph, occurrence.global_id())?;
	}
	for (&graph, namespace) in &labels.graphs {
		let old = &before
			.indexes
			.graphs
			.get(&graph)
			.ok_or_else(|| invalid("namespace source indexes are absent"))?
			.labels;
		for ((kind, name), members) in old {
			if namespace.before.id(label_kind(*kind), name.as_str())
				== namespace.after.id(label_kind(*kind), name.as_str())
			{
				continue;
			}
			for &local in members {
				let occurrence = before
					.occurrence(graph, local)
					.ok_or_else(|| invalid("namespace source member is absent"))?;
				// Incidence already rendered explicit owners with final label
				// IDs.
				if delta.memberships.contains(&(graph, occurrence.global_id()))
					|| labels.is_structural((before, after), graph, local)
				{
					continue;
				}
				if !after.occurrence(graph, local).is_some_and(|new| {
					new.global_id() == occurrence.global_id()
						&& after.membership(graph, new.global_id()) == Some(local)
				}) {
					continue;
				}
				insert_hot_target(&mut targets, &mut labels.budget, graph, occurrence.global_id())?;
			}
		}
	}
	Ok(targets)
}

/// Admits each distinct target before inserting tree metadata.
fn insert_hot_target(
	targets: &mut BTreeMap<GraphId, BTreeSet<GlobalAtomId>>,
	budget: &mut PublicationBudget,
	graph: GraphId,
	global: GlobalAtomId,
) -> FerrosGrynnResult<()> {
	if !targets.contains_key(&graph) {
		budget.tree_entry::<(GraphId, BTreeSet<GlobalAtomId>)>(targets.len())?;
		targets.insert(graph, BTreeSet::new());
	}
	let members = targets.get_mut(&graph).ok_or_else(|| invalid("Hot target graph is absent"))?;
	if !members.contains(&global) {
		budget.tree_entry::<GlobalAtomId>(members.len())?;
		members.insert(global);
	}
	Ok(())
}

/// Prepares one final slot without copying unchanged topology or properties.
fn prepare_hot_change(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	graph: GraphId,
	global: GlobalAtomId,
	namespace: Option<&NamespaceGraph>,
	overlays: &mut GraphOverlays<'_>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<Option<HotSlotChange>> {
	let local = after
		.membership(graph, global)
		.ok_or_else(|| invalid("point overlay membership is absent"))?;
	let old =
		before.occurrence(graph, local).ok_or_else(|| invalid("point overlay source is absent"))?;
	let new =
		after.occurrence(graph, local).ok_or_else(|| invalid("point overlay target is absent"))?;
	if old.global_id() != global || new.global_id() != global {
		return Err(invalid("point graph occurrence identity changed"));
	}
	// A scalar patch must never silently discard a caller's structural change.
	if !stable_layout(old.state(), new.state()) {
		return Err(invalid("point graph metadata layout changed during preparation"));
	}
	let weights = stable_weights(old.state(), new.state()).filter(|(old, new)| old != new);
	let kind = local.kind().ok_or_else(|| invalid("point graph local kind is null"))?;
	let (old_label, new_label) = if let Some(namespace) = namespace {
		let resolve = |dictionary: &crate::storage::graph_index::LabelDictionary,
		               name: Option<&crate::values::Text>| {
			name.map(|name| {
				dictionary
					.id(label_kind(kind), name.as_str())
					.ok_or_else(|| invalid("namespace Hot label is absent"))
			})
			.transpose()
		};
		(resolve(&namespace.before, old.label())?, resolve(&namespace.after, new.label())?)
	} else if old.label() == new.label() {
		(None, None)
	} else {
		(
			super::point_label::label_id(before, graph, kind, old.label())?,
			super::point_label::label_id(after, graph, kind, new.label())?,
		)
	};
	let label = (old_label != new_label).then_some((old_label, new_label));
	if old.overlay() == new.overlay() && weights.is_none() && label.is_none() {
		return Ok(None);
	}
	let weight = match weights {
		Some((old, new)) => {
			Some((old.map(f64::try_from).transpose()?, new.map(f64::try_from).transpose()?))
		}
		None => None,
	};
	let overlay = if old.overlay() == new.overlay() {
		None
	} else {
		Some(match new.overlay() {
			Some(overlay) => overlays.insert(overlay, budget)?,
			None => PropertyRef::None,
		})
	};
	Ok(Some(HotSlotChange {
		local,
		global,
		overlay,
		weight,
		label,
	}))
}

/// Charges one staged page per vacant key and grows each bounded patch vector
/// explicitly.
fn push_hot_change(
	hot: &mut BTreeMap<(DataFileId, PageNo), Vec<HotSlotChange>>,
	file: DataFileId,
	change: HotSlotChange,
	prepared: bool,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<()> {
	let (number, _) = hot_slot_address(change.local.slot())?;
	let key = (file, number);
	if !hot.contains_key(&key) {
		budget.tree_entry::<((DataFileId, PageNo), Vec<HotSlotChange>)>(hot.len())?;
		if !prepared {
			budget.retain(
				crate::storage::page::PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>(),
			)?;
			budget.stage_pages(1)?;
		}
		hot.insert(key, Vec::new());
	}
	let changes = hot.get_mut(&key).ok_or_else(|| invalid("Hot patch page is absent"))?;
	if changes.len() == changes.capacity() {
		let capacity = changes
			.capacity()
			.saturating_mul(2)
			.clamp(4, crate::storage::graph_pages::HOT_SLOTS_PER_PAGE);
		let additional = capacity
			.checked_sub(changes.capacity())
			.filter(|slots| *slots != 0)
			.ok_or_else(|| invalid("Hot patch count exceeds page capacity"))?;
		budget.retain(
			additional
				.checked_mul(size_of::<HotSlotChange>())
				.ok_or_else(|| invalid("Hot patch vector bytes overflow usize"))?,
		)?;
		budget.scratch(
			changes
				.capacity()
				.checked_mul(size_of::<HotSlotChange>())
				.ok_or_else(|| invalid("Hot patch reallocation scratch overflows usize"))?,
		)?;
		changes
			.try_reserve_exact(additional)
			.map_err(|_error| invalid("Hot patch allocation failed"))?;
	}
	changes.push(change);
	Ok(())
}

/// Decodes every source slot before the first write invalidates the checksum.
fn patch_hot_page(
	page: &mut AlignedPage,
	number: PageNo,
	changes: &[HotSlotChange],
) -> FerrosGrynnResult<()> {
	let Some(first) = changes.first() else {
		return Ok(());
	};
	match first.local.kind() {
		Some(AtomKind::Node) => {
			let mut slots = Vec::with_capacity(changes.len());
			for change in changes {
				let (_, within) = hot_slot_address(change.local.slot())?;
				let mut slot = read_node_hot_slot(page, number, within)?;
				if slot.generation != change.local.generation()
					|| slot.global_atom != Some(change.global)
					|| change.weight.is_some()
				{
					return Err(invalid("point overlay node source identity disagrees"));
				}
				if let Some(overlay) = change.overlay {
					slot.overlay = overlay;
					slot.flags
						.set(NodeSlotFlags::HAS_OVERLAY, !matches!(overlay, PropertyRef::None));
				}
				if let Some((old, new)) = change.label {
					slot.replace_label(number, old, new)?;
				}
				slots.push((within, slot));
			}
			for (within, slot) in slots {
				write_node_hot_slot(page, within, &slot)?;
			}
		}
		Some(AtomKind::Edge) => {
			let mut slots = Vec::with_capacity(changes.len());
			for change in changes {
				let (_, within) = hot_slot_address(change.local.slot())?;
				let mut slot = read_edge_hot_slot(page, number, within)?;
				if slot.generation != change.local.generation()
					|| slot.global_atom != Some(change.global)
				{
					return Err(invalid("point overlay edge source identity disagrees"));
				}
				if let Some(overlay) = change.overlay {
					slot.overlay = overlay;
					slot.flags
						.set(EdgeSlotFlags::HAS_OVERLAY, !matches!(overlay, PropertyRef::None));
				}
				if let Some((old, new)) = change.weight {
					slot.replace_weight(number, old, new)?;
				}
				if let Some((old, new)) = change.label {
					slot.replace_label(number, old, new)?;
				}
				slots.push((within, slot));
			}
			for (within, slot) in slots {
				write_edge_hot_slot(page, within, &slot)?;
			}
		}
		None => return Err(invalid("point overlay source kind is null")),
	}
	page.seal()?;
	Ok(())
}

#[cfg(test)]
/// State admission keeps endpoint arrays and containment outside scalar
/// patches.
mod tests {
	use super::*;
	use crate::{
		datastore::{ClassSchema, Datastore, OccurrenceState},
		graph::EdgeTopology,
		storage::graph_index::with_external_sort_budget,
		test_assertions::{AssertOption as _, AssertResult as _},
		values::{PropertyMap, RecordId},
	};

	/// Weight changes alone can reuse Hot layout; child/topology changes
	/// cannot.
	#[test]
	fn state_point_admission_rejects_topology_and_child_changes() {
		let a = AtomId::from_parts(AtomKind::Node, 0, 1).assert_ok();
		let b = AtomId::from_parts(AtomKind::Node, 1, 1).assert_ok();
		let graph = GraphId::new(1).assert_some();
		let source = OccurrenceState::edge(EdgeTopology::directed([a], [b]), None, None);
		let weight =
			OccurrenceState::edge(EdgeTopology::directed([a], [b]), Some(1_i64.into()), None);
		assert!(stable_weights(&source, &weight).is_some());
		for target in [
			OccurrenceState::edge(EdgeTopology::directed([b], [a]), Some(1_i64.into()), None),
			OccurrenceState::edge(EdgeTopology::directed([a], [b]), None, Some(graph)),
			OccurrenceState::node(None),
			OccurrenceState::node(Some(graph)),
		] {
			assert!(stable_weights(&source, &target).is_none());
		}
		assert!(
			stable_weights(&OccurrenceState::node(None), &OccurrenceState::node(Some(graph)))
				.is_none()
		);
	}

	/// Individually admitted canonical arenas must share one cumulative
	/// envelope.
	#[test]
	fn canonical_ancestor_arenas_reject_a_cumulative_budget_overrun() {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		let graphs = ["middle", "outer_r", "outer_s"]
			.map(|name| seed.define_graph(name, GraphKind::Metagraph).assert_ok().id());
		let leaves =
			["old", "new"].map(|name| seed.define_graph(name, GraphKind::Graph).assert_ok().id());
		let selected = "item:selected".parse::<RecordId>().assert_ok();
		seed.create_atom(selected.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graphs[0], &selected, None, Some(leaves[0])).assert_ok();
		for index in 0..128 {
			let record = format!("item:owner_{index}").parse::<RecordId>().assert_ok();
			seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
			for graph in [graphs[1], graphs[2]] {
				seed.attach_node(graph, &record, None, Some(graphs[0])).assert_ok();
			}
		}
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		let owner = before
			.membership(graphs[0], before.resolve_atom(&selected).assert_some())
			.assert_some();
		let mut after = before.as_ref().clone();
		after
			.replace_occurrence_state(graphs[0], owner, OccurrenceState::node(Some(leaves[1])))
			.assert_ok();
		let inputs = [
			(before.as_ref(), graphs[0]),
			(&after, graphs[0]),
			(before.as_ref(), graphs[1]),
			(&after, graphs[1]),
			(before.as_ref(), graphs[2]),
			(&after, graphs[2]),
		];
		let canonical = inputs.map(|(root, graph)| {
			GraphArenaBindings::build_admitted(root, graph, |_growth, _scratch| Ok(())).assert_ok()
		});
		assert_eq!(canonical.each_ref().map(|arena| arena.graph_by_slot().len()), [
			2, 2, 257, 257, 257, 257
		]);
		let maximum = canonical
			.iter()
			.map(|arena| arena.retained_copy_bytes().assert_ok())
			.max()
			.assert_some()
			.checked_mul(4)
			.assert_some();
		assert!(maximum < PublicationBudget::new().remaining().assert_ok());
		// Builder admission includes temporary structures beyond the final
		// maps.
		with_external_sort_budget(maximum, || {
			for (index, (root, graph)) in inputs.iter().enumerate() {
				let mut budget = PublicationBudget::new();
				assert_eq!(admitted_arena(root, *graph, &mut budget)?, canonical[index]);
			}
			Ok(())
		})
		.assert_ok();
		let mut completed = 0;
		let error = with_external_sort_budget(maximum, || {
			let mut budget = PublicationBudget::new();
			let mut retained = Vec::new();
			for (root, graph) in inputs {
				retained.push(admitted_arena(root, graph, &mut budget)?);
				completed += 1;
			}
			Ok(())
		})
		.assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
		));
		assert!(completed > 0 && completed < inputs.len());
		let mut restored = PublicationBudget::new();
		assert_eq!(admitted_arena(&after, graphs[2], &mut restored).assert_ok(), canonical[5]);
		assert!(Arc::ptr_eq(&before, &store.inner.current_root()));
		eprintln!("ANCESTOR_CUMULATIVE_REJECT_PASS maximum={maximum} completed={completed}");
	}
}
