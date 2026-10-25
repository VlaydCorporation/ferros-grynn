//! Stable containment keeps one directory image per immediate parent.

use std::collections::BTreeSet;

use super::{super::GraphBinding, GraphUpdatePlan, LocalState, LocalStore};
use crate::{
	datastore::{DatabaseRoot, OccurrenceStateRef, PublicationDelta},
	gql_status::FerrosGrynnResult,
	graph::{AtomId, AtomKind, GraphId},
	storage::{
		graph_pages::{
			SUBGRAPHS_PER_PAGE,
			SubgraphEntry,
			decode_subgraph_directory,
			encode_subgraph_directory,
			encode_subgraph_directory_into,
		},
		graph_superblock::GraphSuperblock,
		local_store::{layout::invalid, point_namespace::PublicationBudget},
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		types::PageKey,
	},
};

impl GraphUpdatePlan {
	/// Patches copied child counters without changing containment or file
	/// identities.
	///
	/// # Errors
	/// Rejects malformed source chains, bindings, counters, admission and I/O
	/// before WAL.
	pub(super) fn prepare_subgraphs(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		delta: &PublicationDelta,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<()> {
		if delta.memberships.is_empty() && self.arenas.is_empty() {
			return Ok(());
		}
		let mut scratch_reserved = false;
		for binding in &state.bindings.graphs {
			// Descendants appear in the arena but only direct owners have
			// directory entries.
			let selected = self.arenas.contains_key(&binding.graph_id)
				|| binding.arena.graph_by_slot().iter().any(|(slot, child)| {
					*slot != 0
						&& binding
							.arena
							.scope_by_slot()
							.get(slot)
							.is_some_and(|scope| scope.len() == 1)
						&& delta.memberships.iter().any(|(graph, _)| graph == child)
				});
			if !selected {
				continue;
			}
			if !scratch_reserved {
				// Keep reusable page/decoder/encoder backing charged during
				// nested admissions.
				budget.retain(2 * PAGE_SIZE + SUBGRAPHS_PER_PAGE * size_of::<SubgraphEntry>())?;
				scratch_reserved = true;
			}
			self.prepare_directory(store, state, roots, binding, delta, budget)?;
		}
		Ok(())
	}

	/// Validates and patches one existing direct-parent directory chain.
	///
	/// # Errors
	/// Preserves source chain, owner, counter, admission, codec and I/O
	/// failures.
	fn prepare_directory(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		binding: &GraphBinding,
		delta: &PublicationDelta,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<()> {
		if let Some(final_arena) = self.arenas.get(&binding.graph_id) {
			let direct_count = |arena: &crate::storage::graph_pages::GraphArenaBindings| {
				arena
					.scope_by_slot()
					.iter()
					.filter(|(slot, scope)| **slot != 0 && scope.len() == 1)
					.count()
			};
			if direct_count(&binding.arena) != direct_count(final_arena) {
				let extended = if let Some(header) = state.graph_superblocks.get(&binding.graph_id)
				{
					header.subgraphs_pages > 1
						&& store.visible_pages(
							state,
							binding.files.space(),
							binding.files.subgraphs,
						)? == header.subgraphs_pages
				} else {
					false
				};
				// A previously shrunk directory can keep empty pinned tails
				// after its logical count falls back to zero or one.
				if extended || direct_count(&binding.arena).max(direct_count(final_arena)) > 1 {
					return self.prepare_remap_directory(store, state, roots, binding, budget);
				}
				return self.prepare_presence_directory(store, state, roots, binding, budget);
			}
		}
		let header = state
			.graph_superblocks
			.get(&binding.graph_id)
			.ok_or_else(|| invalid("parent directory superblock is absent"))?;
		let ceiling = header.subgraphs_pages;
		if ceiling == 0
			|| store.visible_pages(state, binding.files.space(), binding.files.subgraphs)?
				!= ceiling
		{
			return Err(invalid("parent directory source ceiling disagrees"));
		}
		let mut visited = BTreeSet::new();
		let mut seen = BTreeSet::new();
		let mut current = Some(PageNo::new(0));
		while let Some(number) = current {
			if number.get() >= ceiling || visited.contains(&number) {
				return Err(invalid(
					"parent directory chain exceeds its ceiling or contains a cycle",
				));
			}
			budget.tree_entry::<PageNo>(visited.len())?;
			visited.insert(number);
			let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
			let mut page = store.read_update_page(state, key)?;
			let (next, mut entries) = decode_subgraph_directory(&page, number)?;
			let mut changed = false;
			for entry in &mut entries {
				if entry.slot == 0 || seen.contains(&entry.slot) {
					return Err(invalid("parent directory has a reserved or duplicate slot"));
				}
				budget.tree_entry::<u32>(seen.len())?;
				seen.insert(entry.slot);
				let (owner, child) = accepted_entry(binding, *entry)?;
				let final_child = self
					.arenas
					.get(&binding.graph_id)
					.unwrap_or(&binding.arena)
					.graph_by_slot()
					.get(&entry.slot)
					.copied()
					.ok_or_else(|| invalid("parent directory final child binding is absent"))?;
				for (root, expected_child) in [(roots.0, child), (roots.1, final_child)] {
					validate_container_binding(root, binding.graph_id, owner, expected_child)?;
				}
				validate_source_counters(roots.0, state, child, *entry)?;
				if child != final_child {
					let final_header = replacement_child_header(state, roots.1, final_child)?;
					let replacement = counters(*entry, final_header)?;
					changed |= replacement != *entry;
					*entry = replacement;
				} else if delta.memberships.iter().any(|(graph, _)| *graph == child) {
					let final_header = self.superblocks.get(&child).ok_or_else(|| {
						invalid("parent directory final child header was not prepared")
					})?;
					let replacement = counters(*entry, final_header)?;
					changed |= replacement != *entry;
					*entry = replacement;
				}
			}
			if changed {
				budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
				budget.stage_pages(1)?;
				// Directory and Hot files have distinct keys; each source
				// page is visited once.
				self.pages.try_reserve_exact(1).map_err(|_error| {
					invalid("parent directory prepared page allocation failed")
				})?;
				*page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries)?;
				self.pages.push((key, page));
			}
			current = next;
		}
		let direct = binding
			.arena
			.scope_by_slot()
			.iter()
			.filter(|(slot, scope)| **slot != 0 && scope.len() == 1)
			.count();
		if u64::try_from(visited.len()).ok() != Some(ceiling)
			|| u64::try_from(seen.len()).ok() != Some(header.subgraph_count)
			|| seen.len() != direct
		{
			return Err(invalid(
				"parent directory has unreachable pages or missing direct entries",
			));
		}
		Ok(())
	}

	/// Rewrites changed direct entries while preserving allocated chain pages.
	///
	/// # Errors
	/// Rejects source coverage, owners/counters, overflow, admission and I/O
	/// before publication; shrinking never truncates pinned physical pages.
	fn prepare_remap_directory(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		binding: &GraphBinding,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<()> {
		let header = state
			.graph_superblocks
			.get(&binding.graph_id)
			.ok_or_else(|| invalid("parent remap source superblock is absent"))?;
		let ceiling = header.subgraphs_pages;
		if ceiling == 0
			|| header.subgraph_count > header.subgraph_capacity
			|| store.visible_pages(state, binding.files.space(), binding.files.subgraphs)?
				!= ceiling
		{
			return Err(invalid("parent remap directory source layout disagrees"));
		}
		let old_pages = usize::try_from(ceiling)
			.map_err(|_error| invalid("parent remap page count exceeds usize"))?;
		let bytes = old_pages
			.checked_mul(size_of::<PageNo>())
			.ok_or_else(|| invalid("parent remap chain bytes overflow usize"))?;
		budget.retain(bytes)?;
		let mut order = Vec::new();
		order
			.try_reserve_exact(old_pages)
			.map_err(|_error| invalid("parent remap chain allocation failed"))?;
		let mut visited = BTreeSet::new();
		let mut seen = BTreeSet::new();
		let mut current = Some(PageNo::new(0));
		while let Some(number) = current {
			if number.get() >= ceiling || visited.contains(&number) {
				return Err(invalid("parent remap source chain exceeds its ceiling or cycles"));
			}
			budget.tree_entry::<PageNo>(visited.len())?;
			visited.insert(number);
			order.push(number);
			let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
			let page = store.read_update_page(state, key)?;
			let (next, entries) = decode_subgraph_directory(&page, number)?;
			for entry in entries {
				if entry.slot == 0 || seen.contains(&entry.slot) {
					return Err(invalid("parent remap source has a reserved or duplicate slot"));
				}
				budget.tree_entry::<u32>(seen.len())?;
				seen.insert(entry.slot);
				let (owner, child) = accepted_entry(binding, entry)?;
				validate_container_binding(roots.0, binding.graph_id, owner, child)?;
				validate_source_counters(roots.0, state, child, entry)?;
			}
			current = next;
		}
		let direct = binding
			.arena
			.scope_by_slot()
			.iter()
			.filter(|(slot, scope)| **slot != 0 && scope.len() == 1)
			.count();
		if order.len() != old_pages
			|| u64::try_from(seen.len()).ok() != Some(header.subgraph_count)
			|| seen.len() != direct
		{
			return Err(invalid("parent remap source has unreachable pages or missing entries"));
		}
		let arena = self
			.arenas
			.get(&binding.graph_id)
			.ok_or_else(|| invalid("parent remap final arena is absent"))?;
		let count = arena
			.scope_by_slot()
			.iter()
			.filter(|(slot, scope)| **slot != 0 && scope.len() == 1)
			.count();
		budget.retain(
			count
				.checked_mul(size_of::<SubgraphEntry>())
				.ok_or_else(|| invalid("parent remap final entry bytes overflow usize"))?,
		)?;
		let mut entries = Vec::new();
		entries
			.try_reserve_exact(count)
			.map_err(|_error| invalid("parent remap final entry allocation failed"))?;
		for (&slot, scope) in arena.scope_by_slot() {
			if slot == 0 || scope.len() != 1 {
				continue;
			}
			let child = *arena
				.graph_by_slot()
				.get(&slot)
				.ok_or_else(|| invalid("parent remap final child is absent"))?;
			let owner = scope[0];
			validate_container_binding(roots.1, binding.graph_id, owner, child)?;
			let child_header = replacement_child_header(state, roots.1, child)?;
			entries.push(counters(
				SubgraphEntry {
					slot,
					generation: owner.generation(),
					node_capacity: 0,
					edge_capacity: 0,
					node_count: 0,
					edge_count: 0,
				},
				child_header,
			)?);
		}
		let required = entries.len().div_ceil(SUBGRAPHS_PER_PAGE).max(1);
		if required > order.len() {
			let final_bytes = required
				.checked_mul(size_of::<PageNo>())
				.ok_or_else(|| invalid("parent remap grown chain bytes overflow usize"))?;
			budget.scratch(final_bytes)?;
			budget.retain(
				final_bytes
					.checked_sub(bytes)
					.ok_or_else(|| invalid("parent remap chain reserve decreases"))?,
			)?;
			order
				.try_reserve_exact(required - order.len())
				.map_err(|_error| invalid("parent remap grown chain allocation failed"))?;
			while order.len() < required {
				// Existing pages may be linked out of numeric order; only new
				// pages start at the exact WAL-visible EOF.
				let number = u64::try_from(order.len())
					.map_err(|_error| invalid("parent remap appended page exceeds u64"))?;
				order.push(PageNo::new(number));
			}
		}
		for (index, &number) in order.iter().enumerate() {
			let start = index
				.checked_mul(SUBGRAPHS_PER_PAGE)
				.ok_or_else(|| invalid("parent remap entry offset overflows usize"))?
				.min(entries.len());
			let end = start
				.checked_add(SUBGRAPHS_PER_PAGE)
				.ok_or_else(|| invalid("parent remap entry end overflows usize"))?
				.min(entries.len());
			let next = order.get(index + 1).copied();
			let chunk = &entries[start..end];
			let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
			let source = if index < old_pages {
				let page = store.read_update_page(state, key)?;
				let (old_next, old_entries) = decode_subgraph_directory(&page, number)?;
				if old_next == next && old_entries.as_slice() == chunk {
					continue;
				}
				Some(page)
			} else {
				None
			};
			budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
			budget.stage_pages(1)?;
			self.pages
				.try_reserve_exact(1)
				.map_err(|_error| invalid("parent remap prepared page allocation failed"))?;
			let mut page = source.unwrap_or_else(|| Box::new(AlignedPage::zeroed()));
			encode_subgraph_directory_into(&mut page, number, PageLsn::ZERO, next, chunk)?;
			self.pages.push((key, page));
		}
		let final_header = self
			.superblocks
			.get_mut(&binding.graph_id)
			.ok_or_else(|| invalid("parent remap prepared header is absent"))?;
		final_header.subgraphs_pages = u64::try_from(order.len())
			.map_err(|_error| invalid("parent remap final page count exceeds u64"))?;
		final_header.subgraph_count = u64::try_from(entries.len())
			.map_err(|_error| invalid("parent remap final count exceeds u64"))?;
		final_header.subgraph_capacity =
			final_header.subgraph_capacity.max(final_header.subgraph_count);
		let key = PageKey::new(binding.files.space(), binding.files.superblock, PageNo::new(0));
		let (_, page) = self
			.pages
			.iter_mut()
			.find(|(prepared, _)| *prepared == key)
			.ok_or_else(|| invalid("parent remap prepared header page is absent"))?;
		**page = final_header.encode()?;
		Ok(())
	}

	/// Publishes the zero/one direct-entry case without moving surviving
	/// scopes.
	///
	/// # Errors
	/// Rejects source coverage, child identity/counters, admission and I/O
	/// errors before changing any published arena or page.
	fn prepare_presence_directory(
		&mut self,
		store: &LocalStore,
		state: &LocalState,
		roots: (&DatabaseRoot, &DatabaseRoot),
		binding: &GraphBinding,
		budget: &mut PublicationBudget,
	) -> FerrosGrynnResult<()> {
		let header = state
			.graph_superblocks
			.get(&binding.graph_id)
			.ok_or_else(|| invalid("child presence source header is absent"))?;
		let final_arena = self
			.arenas
			.get(&binding.graph_id)
			.ok_or_else(|| invalid("child presence final arena is absent"))?;
		if header.subgraphs_pages != 1
			|| header.subgraph_count > 1
			|| binding.arena.graph_by_slot().len() > 2
			|| final_arena.graph_by_slot().len() > 2
			|| store.visible_pages(state, binding.files.space(), binding.files.subgraphs)? != 1
		{
			return Err(invalid("child presence directory exceeds its zero/one layout"));
		}
		let number = PageNo::new(0);
		let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
		let mut page = store.read_update_page(state, key)?;
		let (next, mut entries) = decode_subgraph_directory(&page, number)?;
		if next.is_some()
			|| u64::try_from(entries.len()).ok() != Some(header.subgraph_count)
			|| entries.len() + 1 != binding.arena.graph_by_slot().len()
		{
			return Err(invalid("child presence directory source coverage disagrees"));
		}
		for entry in &entries {
			if entry.slot != 1 {
				return Err(invalid("child presence directory has a noncanonical slot"));
			}
			let (owner, child) = accepted_entry(binding, *entry)?;
			validate_container_binding(roots.0, binding.graph_id, owner, child)?;
			validate_source_counters(roots.0, state, child, *entry)?;
		}
		entries.clear();
		if let Some(&child) = final_arena.graph_by_slot().get(&1) {
			let scope = final_arena
				.scope_by_slot()
				.get(&1)
				.ok_or_else(|| invalid("child presence final scope is absent"))?;
			if scope.len() != 1 {
				return Err(invalid("child presence final scope is not direct"));
			}
			let owner = scope[0];
			validate_container_binding(roots.1, binding.graph_id, owner, child)?;
			let final_header = replacement_child_header(state, roots.1, child)?;
			let entry = counters(
				SubgraphEntry {
					slot: 1,
					generation: owner.generation(),
					node_capacity: 0,
					edge_capacity: 0,
					node_count: 0,
					edge_count: 0,
				},
				final_header,
			)?;
			if entries.capacity() == 0 {
				budget.retain(size_of::<SubgraphEntry>())?;
				entries.try_reserve_exact(1).map_err(|_error| {
					invalid("child presence directory entry allocation failed")
				})?;
			}
			entries.push(entry);
		}
		budget.retain(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())?;
		budget.stage_pages(1)?;
		self.pages
			.try_reserve_exact(1)
			.map_err(|_error| invalid("child presence directory page allocation failed"))?;
		*page = encode_subgraph_directory(number, PageLsn::ZERO, None, &entries)?;
		self.pages.push((key, page));
		let final_header = self
			.superblocks
			.get_mut(&binding.graph_id)
			.ok_or_else(|| invalid("child presence final header was not prepared"))?;
		final_header.subgraph_count = u64::try_from(entries.len())
			.map_err(|_error| invalid("child presence count exceeds u64"))?;
		final_header.subgraph_capacity =
			final_header.subgraph_capacity.max(final_header.subgraph_count);
		let header_key = PageKey::new(binding.files.space(), binding.files.superblock, number);
		let (_, page) = self
			.pages
			.iter_mut()
			.find(|(key, _)| *key == header_key)
			.ok_or_else(|| invalid("child presence prepared header page is absent"))?;
		**page = final_header.encode()?;
		Ok(())
	}
}

/// Borrows the accepted direct scope before consulting any final mapping.
///
/// # Errors
/// Rejects missing scopes/graphs, non-direct paths and stale generations.
fn accepted_entry(
	binding: &GraphBinding,
	entry: SubgraphEntry,
) -> FerrosGrynnResult<(AtomId, GraphId)> {
	let scope = binding
		.arena
		.scope_by_slot()
		.get(&entry.slot)
		.ok_or_else(|| invalid("parent directory slot has no scope binding"))?;
	if scope.len() != 1 || scope[0].generation() != entry.generation {
		return Err(invalid("parent directory direct scope or generation disagrees"));
	}
	let child = *binding
		.arena
		.graph_by_slot()
		.get(&entry.slot)
		.ok_or_else(|| invalid("parent directory slot has no graph binding"))?;
	Ok((scope[0], child))
}

/// A full container identity must bind the expected child in this exact root.
///
/// # Errors
/// Rejects missing occurrences or a different NODE/EDGE child binding.
fn validate_container_binding(
	root: &DatabaseRoot,
	graph: GraphId,
	owner: AtomId,
	expected_child: GraphId,
) -> FerrosGrynnResult<()> {
	let container = root
		.occurrence(graph, owner)
		.ok_or_else(|| invalid("parent directory container is absent"))?;
	let bound = match container.state().view() {
		OccurrenceStateRef::Node {
			nested_graph,
		} => *nested_graph,
		OccurrenceStateRef::Edge {
			transition_graph,
			..
		} => *transition_graph,
	};
	if bound != Some(expected_child) {
		return Err(invalid("parent directory container binding disagrees"));
	}
	Ok(())
}

/// The copied entry must agree with the accepted child allocator and header.
///
/// # Errors
/// Preserves absent-root/header, counter overflow and source disagreement
/// errors.
fn validate_source_counters(
	root: &DatabaseRoot,
	state: &LocalState,
	child: GraphId,
	entry: SubgraphEntry,
) -> FerrosGrynnResult<()> {
	let source = state
		.graph_superblocks
		.get(&child)
		.ok_or_else(|| invalid("parent directory child superblock is absent"))?;
	let old_graph =
		root.graph(child).ok_or_else(|| invalid("parent directory source child is absent"))?;
	if u64::try_from(old_graph.slot_capacity(AtomKind::Node)).ok() != Some(source.node_capacity)
		|| u64::try_from(old_graph.slot_capacity(AtomKind::Edge)).ok() != Some(source.edge_capacity)
		|| entry != counters(entry, source)?
	{
		return Err(invalid("parent directory source child counters disagree"));
	}
	Ok(())
}

/// Preserves container identity while replacing only the copied child counters.
///
/// # Errors
/// Rejects child counts that exceed the directory's u32 fields.
fn counters(
	mut entry: SubgraphEntry,
	header: &GraphSuperblock,
) -> FerrosGrynnResult<SubgraphEntry> {
	entry.node_capacity = header.node_capacity;
	entry.edge_capacity = header.edge_capacity;
	entry.node_count = u32::try_from(header.node_count)
		.map_err(|_error| invalid("parent directory NODE count exceeds u32"))?;
	entry.edge_count = u32::try_from(header.edge_count)
		.map_err(|_error| invalid("parent directory EDGE count exceeds u32"))?;
	Ok(entry)
}

/// Validates the unchanged replacement child's physical counters before reuse.
///
/// # Errors
/// Rejects absent roots/headers, count overflow and logical/physical
/// disagreement.
fn replacement_child_header<'a>(
	state: &'a LocalState,
	root: &DatabaseRoot,
	child: crate::graph::GraphId,
) -> FerrosGrynnResult<&'a GraphSuperblock> {
	let final_header = state
		.graph_superblocks
		.get(&child)
		.ok_or_else(|| invalid("parent directory replacement child header is absent"))?;
	let final_graph =
		root.graph(child).ok_or_else(|| invalid("parent directory replacement child is absent"))?;
	let nodes = final_graph.occurrences().filter(|owner| owner.local_id().is_node()).count();
	let edges = final_graph
		.occurrences()
		.len()
		.checked_sub(nodes)
		.ok_or_else(|| invalid("parent directory replacement child count underflows"))?;
	if u64::try_from(final_graph.slot_capacity(AtomKind::Node)).ok()
		!= Some(final_header.node_capacity)
		|| u64::try_from(final_graph.slot_capacity(AtomKind::Edge)).ok()
			!= Some(final_header.edge_capacity)
		|| u64::try_from(nodes).ok() != Some(final_header.node_count)
		|| u64::try_from(edges).ok() != Some(final_header.edge_count)
	{
		return Err(invalid("parent directory replacement child counters disagree"));
	}
	Ok(final_header)
}
