//! Sparse Primary/scalar leaves and selected fallbacks for Base/Effective DML.

use std::collections::{BTreeMap, BTreeSet};

use self::{primary_keys::primary_changes, scalar_keys::scalar_changes};
use super::{
	LocalState,
	LocalStore,
	cleanup::UnpublishedGeneration,
	layout::{FileImage, LayoutBuilder, atom_prefix, graph_prefix, invalid},
};
use crate::{
	datastore::{
		BaseAtom,
		ClassIndexKind,
		DataSpaceId,
		DatabaseRoot,
		GlobalAtomId,
		GraphOccurrence,
		IndexAddress,
		IndexBinding,
		IndexEntries,
		IndexId,
		IndexScope,
		PreparedIndexChange,
		PublicationDelta,
	},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::{
		catalog::{FileCatalog, PhysicalIndexBinding},
		file_registry::DataFileRole,
		graph_index::{
			CountIndexScope,
			ExternalIndexSorter,
			IndexLeafPages,
			PrimaryAddress,
			PrimaryIndexMutation,
			ScalarIndexMode,
			ScalarIndexMutation,
			decode_count_index,
			mutate_primary_index_leaves,
			mutate_scalar_index_leaves,
			rewrite_count_index_population,
			with_external_sort_budget,
		},
		graph_pages::GraphArenaBindings,
		page::{AlignedPage, PAGE_SIZE, PageLsn, PageNo},
		property_page::encode_record_id,
		scoped_index_image::{
			StreamedScopedIndexFile,
			explicit_index_paths,
			local_address,
			write_streamed_binding,
			write_streamed_unique_file,
		},
		types::{DataFileId, DataFileKind, PageKey},
		wal::TxHandle,
	},
	values::{Uuid, storage::EncodeContext},
};

/// Shared admission for retained index frames and their WAL copies.
mod output;

/// Canonical Primary key preparation is separate from physical leaf output.
mod primary_keys;

/// Canonical scalar preparation is separate from physical index output.
mod scalar_keys;

/// Prepared physical addresses keep filled aligned pages in heap slots.
type PointPages = Vec<(PageKey, Box<AlignedPage>)>;

/// Prepared leaves and optional streamed images for one WAL publication.
pub(super) struct PointIndexPlan {
	/// Prepared graph membership roots share the common publication barrier.
	pub(super) memberships: BTreeMap<GraphId, PhysicalIndexBinding>,
	/// Prepared database reverse root is installed with the graph roots.
	pub(super) reverse: PhysicalIndexBinding,
	/// Prepared dictionary bindings installed without fallible work after WAL.
	pub(super) label_files: BTreeMap<GraphId, DataFileId>,
	/// Only changed engine-owned endpoint trees receive new file bindings.
	pub(super) endpoints: BTreeMap<GraphId, PhysicalIndexBinding>,
	/// Present only when a streamed fallback changes a physical identity.
	pub(super) catalog: Option<FileCatalog>,
	/// New selected files; their pages were streamed before catalog staging.
	pub(super) files: Vec<FileImage>,
	/// Final explicit bindings, including reused unrelated index generations.
	pub(super) bindings: BTreeMap<IndexId, PhysicalIndexBinding>,
	/// Changed bindings require fresh snapshot-owned decoded caches.
	pub(super) changed: BTreeSet<IndexId>,
	/// Primary participates in the same file-identity allocation clock.
	pub(super) primary: PhysicalIndexBinding,
	/// Heap slots stay stable from encoding through staging.
	pages: PointPages,
	/// Deletes unreachable images on known failure, defers uncertain WAL to
	/// recovery.
	pub(super) cleanup: Option<UnpublishedGeneration>,
}

impl PointIndexPlan {
	/// Uses final replay keys for stable leaves; streams only unstable
	/// bindings. Prepared graph arenas also authorize final endpoint scopes.
	///
	/// # Errors
	/// Rejects absent or inconsistent bindings and shared output-budget excess;
	/// propagates codec, allocation and I/O failures before publication.
	pub(super) fn build(
		store: &LocalStore,
		state: &LocalState,
		before: &DatabaseRoot,
		after: &DatabaseRoot,
		delta: &PublicationDelta,
		arenas: Option<&BTreeMap<GraphId, GraphArenaBindings>>,
		labels: &mut super::point_namespace::LabelPlan,
		context: &mut EncodeContext,
	) -> FerrosGrynnResult<Option<Self>> {
		let affected = scalar_changes(before, after, delta, context, &mut labels.budget)?;
		let mutations = primary_changes(before, after, delta, context, &mut labels.budget)?;
		let endpoint_targets = super::point_endpoint::changed_graphs(
			state,
			before,
			after,
			delta,
			arenas,
			&mut labels.budget,
		)?;
		let mut changed = BTreeSet::new();
		let mut pages = Vec::new();
		prepare_count_pages(
			store,
			state,
			before,
			after,
			delta,
			&mut changed,
			&mut pages,
			&mut labels.budget,
		)?;
		if affected.is_empty()
			&& mutations.is_empty()
			&& pages.is_empty()
			&& labels.targets.is_empty()
			&& labels.class_graphs.is_empty()
			&& labels.memberships.is_empty()
			&& endpoint_targets.is_empty()
		{
			return Ok(None);
		}
		let generation = Uuid::now_v7().to_string();
		let mut cleanup = None;
		let mut layout = LayoutBuilder::new(state.file_catalog.next_file_instance_id);
		let mut bindings = state.bindings.physical_indexes.clone();
		let mut retired = BTreeSet::new();
		let label_files = labels.append_metadata(
			store,
			state,
			after,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
		)?;
		let membership = super::point_membership::append_membership_indexes(
			store,
			state,
			before,
			after,
			labels,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
		)?;
		output::reserve_keys(&mut pages, membership.pages.len())?;
		pages.extend(membership.pages);
		let (primary, primary_pages) = prepare_primary(
			store,
			state,
			after,
			&mutations,
			context,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut labels.budget,
		)?;
		if primary != state._database_catalog.atom_store.primary {
			retired.insert((DataSpaceId::Database, state.bindings.atoms.primary));
		}
		output::reserve_keys(&mut pages, primary_pages.len())?;
		pages.extend(primary_pages);
		super::point_class::append_class_indexes(
			store,
			state,
			before,
			after,
			labels,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
		)?;
		super::point_label::append_selected_indexes(
			store,
			state,
			before,
			after,
			labels,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
		)?;
		for (graph, mut selected) in affected {
			let (scope, space, prefix) = match graph {
				Some(graph) => (
					IndexScope::Effective {
						graph,
					},
					DataSpaceId::Graph(graph),
					graph_prefix(graph, &generation),
				),
				None => (IndexScope::Base, DataSpaceId::Database, atom_prefix(&generation)),
			};
			let names = match graph {
				Some(graph) => selected_paths(
					&after
						.indexes
						.graphs
						.get(&graph)
						.ok_or_else(|| invalid("point index scope is absent"))?
						.explicit,
					&selected,
				)?,
				None => selected_paths(&after.indexes.base, &selected)?,
			};
			for (index, name) in names {
				let previous = *bindings
					.get(&index)
					.ok_or_else(|| invalid("point source physical binding is absent"))?;
				let changes =
					selected.remove(&index).ok_or_else(|| invalid("point keys are absent"))?;
				let leaves = prepare_scoped_leaves(
					store,
					state,
					after,
					graph,
					index,
					previous,
					changes,
					&mut labels.budget,
				)?;
				changed.insert(index);
				if let Some(leaves) = leaves {
					output::retain(&mut pages, leaves.len(), &mut labels.budget)?;
					pages.extend(leaves.into_iter().map(|(number, page)| {
						(PageKey::new(space, previous.file_id, number), page)
					}));
					continue;
				}
				cleanup.get_or_insert_with(|| {
					UnpublishedGeneration::for_database(&store.root, after, &generation)
				});
				let path = format!("{prefix}/{name}");
				let image = with_external_sort_budget(labels.budget.remaining()?, || {
					stream_scoped_binding(store, after, scope, index, path)
				})?;
				retired.insert((space, previous.file_id));
				let file_id = layout.add_prewritten(
					space,
					DataFileKind::Index,
					DataFileRole::WalBackfilled,
					image.path,
					image.page_count,
				)?;
				bindings.insert(index, PhysicalIndexBinding {
					file_id,
					root: image.root,
				});
			}
		}
		let endpoints = super::point_endpoint::append_selected_indexes(
			store,
			state,
			(after, arenas),
			&endpoint_targets,
			&generation,
			&mut layout,
			&mut cleanup,
			&mut retired,
			&mut labels.budget,
		)?;
		let (catalog, files) = finish_selected_files(state, layout, &retired)?;
		Ok(Some(Self {
			memberships: membership.graphs,
			reverse: membership.reverse,
			label_files,
			endpoints,
			catalog,
			files,
			bindings,
			changed,
			primary,
			pages,
			cleanup,
		}))
	}

	/// Joins all stable leaves to the same source/catalog/history barrier.
	///
	/// # Errors
	/// Propagates WAL page-staging failures before publication.
	pub(super) fn stage(&self, tx: &mut TxHandle) -> FerrosGrynnResult<()> {
		for (key, page) in &self.pages {
			tx.stage_page(*key, page)?;
		}
		Ok(())
	}
}

/// Retains unrelated identities while finalizing only selected replacements.
fn finish_selected_files(
	state: &LocalState,
	mut layout: LayoutBuilder,
	retired: &BTreeSet<(DataSpaceId, crate::storage::types::DataFileId)>,
) -> FerrosGrynnResult<(Option<FileCatalog>, Vec<FileImage>)> {
	if retired.is_empty() && !layout.has_new_files() {
		return Ok((None, Vec::new()));
	}
	for entry in &state.file_catalog.files {
		// LayoutBuilder supplies the fixed catalog identity itself.
		if !matches!(
			entry.file_id.kind(),
			Some(DataFileKind::SystemCatalog | DataFileKind::JobCatalog)
		) && !retired.contains(&(entry.space_id, entry.file_id))
		{
			layout.retain(entry.clone())?;
		}
	}
	let (catalog, files) = layout.finish()?;
	Ok((Some(catalog), files))
}

/// Encodes Primary separately while sharing the selected-file identity clock.
///
/// # Errors
/// Preserves source validation, output admission and streamed-fallback errors.
fn prepare_primary(
	store: &LocalStore,
	state: &LocalState,
	after: &DatabaseRoot,
	mutations: &[PrimaryIndexMutation<GlobalAtomId>],
	context: &mut EncodeContext,
	generation: &str,
	layout: &mut LayoutBuilder,
	cleanup: &mut Option<UnpublishedGeneration>,
	budget: &mut super::point_namespace::PublicationBudget,
) -> FerrosGrynnResult<(PhysicalIndexBinding, PointPages)> {
	let previous = state._database_catalog.atom_store.primary;
	if mutations.is_empty() {
		return Ok((previous, Vec::new()));
	}
	output::admit_leaves(
		store,
		state,
		DataSpaceId::Database,
		previous.file_id,
		mutations.len(),
		budget,
	)?;
	let leaves = mutate_primary_index_leaves(
		|number, output| {
			store.read_update_page_into(
				state,
				PageKey::new(DataSpaceId::Database, previous.file_id, number),
				output,
			)
		},
		previous.root,
		mutations,
		PageLsn::ZERO,
	)?;
	if let Some(leaves) = leaves {
		let mut pages = Vec::new();
		output::retain(&mut pages, leaves.len(), budget)?;
		pages.extend(leaves.into_iter().map(|(number, page)| {
			(PageKey::new(DataSpaceId::Database, previous.file_id, number), page)
		}));
		return Ok((previous, pages));
	}
	cleanup
		.get_or_insert_with(|| UnpublishedGeneration::for_database(&store.root, after, generation));
	let path = format!("{}/primary.fgidx", atom_prefix(generation));
	let image = with_external_sort_budget(budget.remaining()?, || {
		let mut sorter = ExternalIndexSorter::new(&store.root);
		for atom in after.atoms.atoms() {
			sorter.push(encode_record_id(atom.record_id(), context)?, atom.id())?;
		}
		write_streamed_unique_file::<GlobalAtomId>(&sorter.finish()?, &store.root, &path)
	})?;
	let file_id = layout.add_prewritten(
		DataSpaceId::Database,
		DataFileKind::Index,
		DataFileRole::WalBackfilled,
		path,
		image.page_count,
	)?;
	Ok((
		PhysicalIndexBinding {
			file_id,
			root: image.root,
		},
		Vec::new(),
	))
}

/// Scope selects the persisted address codec before a leaf is decoded.
///
/// # Errors
/// Propagates missing scopes, address conversion, admission and leaf-codec
/// errors.
fn prepare_scoped_leaves(
	store: &LocalStore,
	state: &LocalState,
	after: &DatabaseRoot,
	graph: Option<GraphId>,
	index: IndexId,
	previous: PhysicalIndexBinding,
	changes: Vec<PreparedIndexChange>,
	budget: &mut super::point_namespace::PublicationBudget,
) -> FerrosGrynnResult<Option<IndexLeafPages>> {
	match graph {
		Some(graph) => prepare_scalar_leaves(
			store,
			state,
			DataSpaceId::Graph(graph),
			previous,
			after
				.indexes
				.graph_index(graph, index)
				.ok_or_else(|| invalid("point effective binding is absent"))?
				.kind,
			changes,
			|address| match address {
				IndexAddress::Local(atom) => local_address(atom),
				IndexAddress::Global(_) => Err(invalid("effective index has a global address")),
			},
			budget,
		),
		None => prepare_scalar_leaves(
			store,
			state,
			DataSpaceId::Database,
			previous,
			after
				.indexes
				.base_index(index)
				.ok_or_else(|| invalid("point base binding is absent"))?
				.kind,
			changes,
			|address| match address {
				IndexAddress::Global(atom) => Ok(atom),
				IndexAddress::Local(_) => Err(invalid("base index has a local address")),
			},
			budget,
		),
	}
}

/// Streams only the binding whose leaf layout requires replacement.
fn stream_scoped_binding(
	store: &LocalStore,
	after: &DatabaseRoot,
	scope: IndexScope,
	index: IndexId,
	path: String,
) -> FerrosGrynnResult<StreamedScopedIndexFile> {
	match scope {
		IndexScope::Effective {
			graph,
		} => write_streamed_binding(
			after
				.indexes
				.graph_index(graph, index)
				.ok_or_else(|| invalid("point effective binding is absent"))?,
			scope,
			&store.root,
			path,
			local_address,
		),
		IndexScope::Base => write_streamed_binding(
			after
				.indexes
				.base_index(index)
				.ok_or_else(|| invalid("point base binding is absent"))?,
			scope,
			&store.root,
			path,
			Ok,
		),
	}
}

/// Count has a fixed page layout and shares the source publication barrier.
///
/// # Errors
/// Preserves scope lookup, source validation and cumulative output-budget
/// errors.
fn prepare_count_pages(
	store: &LocalStore,
	state: &LocalState,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &PublicationDelta,
	changed: &mut BTreeSet<IndexId>,
	pages: &mut PointPages,
	budget: &mut super::point_namespace::PublicationBudget,
) -> FerrosGrynnResult<()> {
	let mut classes = BTreeSet::new();
	for slot in &delta.global_slots {
		let old = before.atoms.atom_by_slot(*slot)?;
		let new = after.atoms.atom_by_slot(*slot)?;
		if old.map(BaseAtom::class_id) != new.map(BaseAtom::class_id) {
			classes.extend(old.into_iter().chain(new).map(BaseAtom::class_id));
		}
	}
	for binding in after.indexes.base.values().filter(|binding| {
		binding.kind == ClassIndexKind::Count && classes.contains(&binding.class_id)
	}) {
		append_count_page(store, state, None, binding, changed, pages, budget)?;
	}
	let mut effective = BTreeSet::new();
	for &(graph, global) in &delta.memberships {
		for atom in before.atom(global).into_iter().chain(after.atom(global)) {
			effective.insert((graph, atom.class_id()));
		}
	}
	for (graph, class) in effective {
		let indexes = after
			.indexes
			.graphs
			.get(&graph)
			.ok_or_else(|| invalid("point Count graph indexes are absent"))?;
		for binding in indexes
			.explicit
			.values()
			.filter(|binding| binding.kind == ClassIndexKind::Count && binding.class_id == class)
		{
			append_count_page(store, state, Some(graph), binding, changed, pages, budget)?;
		}
	}
	Ok(())
}

/// The Count codec validates the selected scope before changing its population.
///
/// # Errors
/// Rejects invalid sources and source/output excess before retaining a frame.
fn append_count_page<Address>(
	store: &LocalStore,
	state: &LocalState,
	graph: Option<GraphId>,
	binding: &IndexBinding<Address>,
	changed: &mut BTreeSet<IndexId>,
	pages: &mut PointPages,
	budget: &mut super::point_namespace::PublicationBudget,
) -> FerrosGrynnResult<()> {
	let IndexEntries::Count(count) = binding.entries else {
		return Err(invalid("point Count binding has incompatible entries"));
	};
	let physical = state
		.bindings
		.physical_indexes
		.get(&binding.id)
		.ok_or_else(|| invalid("point Count physical binding is absent"))?;
	if physical.root != PageNo::new(0) {
		return Err(invalid("point Count root is not zero"));
	}
	let (space, scope) = match graph {
		Some(graph) => (DataSpaceId::Graph(graph), CountIndexScope::Effective(graph)),
		None => (DataSpaceId::Database, CountIndexScope::Base),
	};
	let key = PageKey::new(space, physical.file_id, physical.root);
	budget.scratch(PAGE_SIZE)?;
	let mut page = store.read_update_page(state, key)?;
	let previous = decode_count_index(&page)?.atom_count;
	rewrite_count_index_population(&mut page, binding.class_id.get(), scope, count)?;
	if previous != count {
		// The validated source box becomes the output; an unchanged Count keeps
		// no frame.
		output::retain(pages, 1, budget)?;
		changed.insert(binding.id);
		pages.push((key, page));
	}
	Ok(())
}

/// Moves canonical net keys into the scope-specific bounded leaf codec.
///
/// # Errors
/// Preserves address/codec failures and checks output admission before leaf
/// allocation.
fn prepare_scalar_leaves<Address: PrimaryAddress + Ord>(
	store: &LocalStore,
	state: &LocalState,
	space: DataSpaceId,
	binding: PhysicalIndexBinding,
	kind: ClassIndexKind,
	changes: Vec<PreparedIndexChange>,
	address: impl Fn(IndexAddress) -> FerrosGrynnResult<Address>,
	budget: &mut super::point_namespace::PublicationBudget,
) -> FerrosGrynnResult<Option<IndexLeafPages>> {
	let mode = match kind {
		ClassIndexKind::Standard => ScalarIndexMode::Standard,
		ClassIndexKind::Unique => ScalarIndexMode::Unique,
		_ => return Ok(None),
	};
	let count = changes.iter().try_fold(0usize, |count, change| {
		count
			.checked_add(usize::from(change.before.is_some()))
			.and_then(|count| count.checked_add(usize::from(change.after.is_some())))
			.ok_or_else(|| invalid("scalar mutation count overflows usize"))
	})?;
	let bytes = count
		.checked_mul(size_of::<ScalarIndexMutation<Address>>())
		.ok_or_else(|| invalid("scalar mutation backing overflows usize"))?;
	budget.with_scratch(bytes, |budget| {
		let mut mutations = Vec::new();
		mutations.try_reserve_exact(count).map_err(|source| {
			invalid("scalar mutation allocation failed").with_technical_source(source)
		})?;
		for change in changes {
			let atom = address(change.address)?;
			for (key, insert) in [(change.before, false), (change.after, true)] {
				if let Some(key) = key {
					mutations.push(ScalarIndexMutation {
						key,
						atom,
						insert,
					});
				}
			}
		}
		output::admit_leaves(store, state, space, binding.file_id, mutations.len(), budget)?;
		mutate_scalar_index_leaves(
			|number, output| {
				store.read_update_page_into(
					state,
					PageKey::new(space, binding.file_id, number),
					output,
				)
			},
			binding.root,
			mode,
			&mutations,
			PageLsn::ZERO,
		)
	})
}

/// Visits scope metadata once, without cloning or decoding unrelated entries.
fn selected_paths<Address>(
	bindings: &BTreeMap<IndexId, IndexBinding<Address>>,
	selected: &BTreeMap<IndexId, Vec<PreparedIndexChange>>,
) -> FerrosGrynnResult<BTreeMap<IndexId, String>> {
	let mut paths = BTreeMap::new();
	for planned in explicit_index_paths(bindings) {
		let (binding, path) = planned?;
		if selected.contains_key(&binding.id) {
			paths.insert(binding.id, path);
		}
	}
	if paths.len() != selected.len() {
		return Err(invalid("point index selection is absent from its scope"));
	}
	Ok(paths)
}

#[cfg(test)]
/// Independent output admission uses real transaction-private roots.
mod output_tests;

#[cfg(test)]
/// Aggregate canonical-key admission isolated from physical builders.
mod key_tests;

#[cfg(test)]
/// Scalar required-key admission is isolated from physical builders.
mod scalar_key_tests;
