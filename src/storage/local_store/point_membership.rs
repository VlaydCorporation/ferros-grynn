//! Selected forward/reverse membership trees share one publication barrier.

use std::collections::{BTreeMap, BTreeSet};

use super::{
	LocalState,
	LocalStore,
	cleanup::UnpublishedGeneration,
	layout::{LayoutBuilder, atom_prefix, graph_prefix, invalid, selected_file_metadata_bytes},
	point_namespace::LabelPlan,
};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot, GlobalAtomId},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	memory::tree_retained_bytes,
	storage::{
		catalog::PhysicalIndexBinding,
		file_registry::DataFileRole,
		graph_index::{
			ExternalIndexSorter,
			PrimaryIndexMutation,
			SortedIndexRecords,
			leftmost_primary_leaf,
			mutate_primary_index_leaves,
			visit_primary_leaf_page,
			with_external_sort_budget,
		},
		membership_index::reverse_key,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		scoped_index_image::{local_address, write_streamed_unique_file},
		types::{DataFileId, DataFileKind, PageKey},
	},
	values::storage::DiskAtomRef,
};

/// Graph owner selects a forward tree; None selects the database reverse tree.
type TreeChanges = BTreeMap<Option<GraphId>, Vec<PrimaryIndexMutation<DiskAtomRef>>>;

/// Prepared bindings are installed only after the common WAL publication.
pub(super) struct MembershipUpdate {
	/// Existing graph bindings receive these already allocated final values.
	pub(super) graphs: BTreeMap<GraphId, PhysicalIndexBinding>,
	/// The reverse tree participates in the same physical identity clock.
	pub(super) reverse: PhysicalIndexBinding,
	/// Stable leaves retain their physical identity and join ordinary staging.
	pub(super) pages: Vec<(PageKey, Box<AlignedPage>)>,
}

/// Publishes only the trees selected by explicit final membership targets.
///
/// # Errors
/// Rejects inconsistent sources and propagates admission, codec and I/O errors.
pub(super) fn append_membership_indexes(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	plan: &mut LabelPlan,
	generation: &str,
	layout: &mut LayoutBuilder,
	cleanup: &mut Option<UnpublishedGeneration>,
	retired: &mut BTreeSet<(DataSpaceId, DataFileId)>,
) -> FerrosGrynnResult<MembershipUpdate> {
	let changes = membership_changes(before, after, plan)?;
	let mut result = MembershipUpdate {
		graphs: BTreeMap::new(),
		reverse: state._database_catalog.atom_store.reverse_membership,
		pages: Vec::new(),
	};
	for (owner, changes) in changes {
		let (space, previous, path) = match owner {
			Some(graph) => (
				DataSpaceId::Graph(graph),
				state
					.bindings
					.graphs
					.iter()
					.find(|binding| binding.graph_id == graph)
					.ok_or_else(|| invalid("membership graph binding is absent"))?
					.membership,
				format!("{}/membership.fgidx", graph_prefix(graph, generation)),
			),
			None => (
				DataSpaceId::Database,
				result.reverse,
				format!("{}/memberships.fgidx", atom_prefix(generation)),
			),
		};
		let source_pages = store.visible_pages(state, space, previous.file_id)?;
		let touched = changes.len().min(
			usize::try_from(source_pages)
				.map_err(|_error| invalid("membership page ceiling exceeds usize"))?,
		);
		plan.budget.retain(
			touched
				.checked_mul(PAGE_SIZE + 3 * size_of::<(PageKey, Box<AlignedPage>)>())
				.ok_or_else(|| invalid("membership leaf output bytes overflow usize"))?,
		)?;
		plan.budget.stage_pages(touched)?;
		plan.budget.scratch(
			touched
				.checked_mul(8 * PAGE_SIZE)
				.ok_or_else(|| invalid("membership leaf scratch overflows usize"))?,
		)?;
		let leaves = mutate_primary_index_leaves(
			|number, output| {
				read_source_page(
					store,
					state,
					space,
					previous.file_id,
					source_pages,
					number,
					output,
				)
			},
			previous.root,
			&changes,
			PageLsn::ZERO,
		)?;
		let binding = if let Some(leaves) = leaves {
			result.pages.extend(
				leaves
					.into_iter()
					.map(|(number, page)| (PageKey::new(space, previous.file_id, number), page)),
			);
			previous
		} else {
			plan.budget.scratch(
				changes
					.len()
					.checked_mul(size_of::<usize>() + 1)
					.and_then(|bytes| bytes.checked_add(2 * PAGE_SIZE + 4096))
					.ok_or_else(|| invalid("membership merge scratch overflows usize"))?,
			)?;
			plan.budget.retain(selected_file_metadata_bytes(&path)?)?;
			cleanup.get_or_insert_with(|| {
				UnpublishedGeneration::for_database(&store.root, after, generation)
			});
			let image = with_external_sort_budget(plan.budget.remaining()?, || {
				let records =
					sort_final_memberships(store, state, before, owner, previous, &changes)?;
				write_streamed_unique_file::<DiskAtomRef>(&records, &store.root, &path)
			})?;
			let file_id = layout.add_prewritten(
				space,
				DataFileKind::Index,
				DataFileRole::WalBackfilled,
				path,
				image.page_count,
			)?;
			plan.budget.tree_entry::<(DataSpaceId, DataFileId)>(retired.len())?;
			retired.insert((space, previous.file_id));
			PhysicalIndexBinding {
				file_id,
				root: image.root,
			}
		};
		if let Some(graph) = owner {
			plan.budget.tree_entry::<(GraphId, PhysicalIndexBinding)>(result.graphs.len())?;
			result.graphs.insert(graph, binding);
		} else {
			result.reverse = binding;
		}
	}
	Ok(result)
}

/// Normalized disk addresses can stay unchanged across a local generation.
fn membership_changes(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	plan: &mut LabelPlan,
) -> FerrosGrynnResult<TreeChanges> {
	if plan.memberships.is_empty() {
		return Ok(TreeChanges::new());
	}
	let maximum = plan
		.memberships
		.len()
		.checked_add(1)
		.ok_or_else(|| invalid("membership scope count overflows usize"))?;
	let entries =
		tree_retained_bytes::<(Option<GraphId>, Vec<PrimaryIndexMutation<DiskAtomRef>>)>(maximum)?;
	let values = plan
		.memberships
		.len()
		.checked_mul(4 * (3 * size_of::<PrimaryIndexMutation<DiskAtomRef>>() + 16))
		.and_then(|bytes| bytes.checked_add(entries))
		.ok_or_else(|| invalid("membership mutations overflow usize"))?;
	plan.budget.retain(values)?;
	let mut changes = TreeChanges::new();
	for &(graph, global) in &plan.memberships {
		let old = before.membership(graph, global).map(local_address).transpose()?;
		let new = after.membership(graph, global).map(local_address).transpose()?;
		if old == new {
			continue;
		}
		for (address, insert) in [(old, false), (new, true)] {
			if let Some(atom) = address {
				changes.entry(Some(graph)).or_default().push(PrimaryIndexMutation {
					key: global.raw().to_be_bytes().to_vec(),
					atom,
					insert,
				});
				changes.entry(None).or_default().push(PrimaryIndexMutation {
					key: reverse_key(global, graph),
					atom,
					insert,
				});
			}
		}
	}
	Ok(changes)
}

/// A source page ceiling bounds malformed links before issuing the read.
fn read_source_page(
	store: &LocalStore,
	state: &LocalState,
	space: DataSpaceId,
	file: DataFileId,
	ceiling: u64,
	number: PageNo,
	output: &mut AlignedPage,
) -> FerrosGrynnResult<()> {
	if number.get() >= ceiling {
		return Err(invalid("membership source page exceeds its visible ceiling"));
	}
	store.read_update_page_into(state, PageKey::new(space, file, number), output)
}

/// Streams the validated physical source; no graph population is materialized.
fn sort_final_memberships(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	owner: Option<GraphId>,
	binding: PhysicalIndexBinding,
	changes: &[PrimaryIndexMutation<DiskAtomRef>],
) -> FerrosGrynnResult<SortedIndexRecords> {
	let mut removals = changes.iter().filter(|change| !change.insert).collect::<Vec<_>>();
	removals.sort_unstable_by(|left, right| left.key.cmp(&right.key));
	let mut seen = vec![false; removals.len()];
	let mut sorter = ExternalIndexSorter::new(&store.root);
	visit_source_tree(store, state, before, owner, binding, |key, address| {
		if let Ok(index) = removals.binary_search_by(|change| change.key.as_slice().cmp(key)) {
			if removals[index].atom != address || seen[index] {
				return Err(invalid("membership removal disagrees with the physical source"));
			}
			seen[index] = true;
		} else {
			sorter.push(key.to_vec(), address)?;
		}
		Ok(())
	})?;
	if seen.iter().any(|found| !found) {
		return Err(invalid("membership source omits an explicit removal"));
	}
	for change in changes.iter().filter(|change| change.insert) {
		sorter.push(change.key.clone(), change.atom)?;
	}
	sorter.finish()
}

/// Ordered leaves and a finite page ceiling bound malformed empty cycles.
fn visit_source_tree(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	owner: Option<GraphId>,
	binding: PhysicalIndexBinding,
	mut visit: impl FnMut(&[u8], DiskAtomRef) -> FerrosGrynnResult<()>,
) -> FerrosGrynnResult<()> {
	let space = owner.map_or(DataSpaceId::Database, DataSpaceId::Graph);
	let ceiling = store.visible_pages(state, space, binding.file_id)?;
	let expected = if let Some(graph) = owner {
		before
			.graph(graph)
			.ok_or_else(|| invalid("membership source graph is absent"))?
			.occurrences()
			.len()
	} else {
		before.graphs.values().try_fold(0_usize, |total, graph| {
			total
				.checked_add(graph.occurrences().len())
				.ok_or_else(|| invalid("reverse membership population overflows usize"))
		})?
	};
	let mut number = leftmost_primary_leaf(
		|number, output| {
			read_source_page(store, state, space, binding.file_id, ceiling, number, output)
		},
		binding.root,
	)?;
	let mut previous = None;
	let mut visited = 0_u64;
	let mut count = 0_usize;
	let mut page = Box::new(AlignedPage::zeroed());
	while number != PageNo::NULL {
		if visited >= ceiling {
			return Err(invalid("membership leaf chain exceeds its visible page ceiling"));
		}
		visited += 1;
		read_source_page(store, state, space, binding.file_id, ceiling, number, &mut page)?;
		number = visit_primary_leaf_page(number, &page, &mut previous, &mut |key, address| {
			validate_source_entry(before, owner, key, address)?;
			count = count
				.checked_add(1)
				.ok_or_else(|| invalid("membership source population overflows usize"))?;
			if count > expected {
				return Err(invalid("membership source has excess entries"));
			}
			visit(key, address)
		})?;
	}
	if count != expected {
		return Err(invalid("membership source omits unchanged entries"));
	}
	Ok(())
}

/// Source keys must resolve through the generation-aware runtime membership.
fn validate_source_entry(
	before: &DatabaseRoot,
	owner: Option<GraphId>,
	key: &[u8],
	address: DiskAtomRef,
) -> FerrosGrynnResult<()> {
	let expected = if owner.is_some() {
		8
	} else {
		16
	};
	if key.len() != expected {
		return Err(invalid("membership source key has the wrong width"));
	}
	let raw = u64::from_be_bytes(
		key[..8].try_into().map_err(|_error| invalid("membership global key is truncated"))?,
	);
	let global = GlobalAtomId::from_storage(raw)?;
	let graph = match owner {
		Some(graph) => graph,
		None => GraphId::new(u64::from_be_bytes(
			key[8..].try_into().map_err(|_error| invalid("membership graph key is truncated"))?,
		))
		.ok_or_else(|| invalid("membership source graph is null"))?,
	};
	let local = before
		.membership(graph, global)
		.ok_or_else(|| invalid("membership source owner is absent"))?;
	if before.atom(global).is_none() || local_address(local)? != address {
		return Err(invalid("membership source address disagrees with its owner"));
	}
	Ok(())
}
