//! Selected endpoint trees and retained headers share one publication barrier.

use std::{collections::BTreeMap, path::Path};

use fg_runtime::__internal::EdgeTopologyInternal as _;

use super::{
	LocalState,
	LocalStore,
	layout::{LayoutBuilder, invalid},
	point_namespace::PublicationBudget,
};
use crate::{
	datastore::{DataSpaceId, DatabaseRoot},
	gql_status::FerrosGrynnResult,
	graph::GraphId,
	storage::{
		catalog::PhysicalIndexBinding,
		file_registry::DataFileRole,
		graph_pages::GraphArenaBindings,
		page::{AlignedPage, PAGE_SIZE, PageNo},
		types::{DataFileKind, PageKey},
		wal::TxHandle,
	},
};

/// Prepared revisions never mutate the source registry or its pinned snapshots.
pub(super) struct EndpointRevisionPlan {
	/// One heap-stable header per existing physical endpoint capability.
	pages: Vec<(PageKey, Box<AlignedPage>)>,
}

impl EndpointRevisionPlan {
	/// Applies to sparse data and catalog-only commits retaining the same
	/// trees.
	///
	/// # Errors
	/// Rejects absent capabilities, stale source metadata and resource/I/O
	/// failures.
	pub(super) fn build(
		store: &LocalStore,
		state: &LocalState,
		after: &DatabaseRoot,
		budget: &mut PublicationBudget,
		replacements: Option<
			&std::collections::BTreeMap<crate::graph::GraphId, PhysicalIndexBinding>,
		>,
	) -> FerrosGrynnResult<Self> {
		let previous = store.wal.snapshot().commit_ts().get();
		let mut pages = Vec::new();
		for graph in &state.bindings.graphs {
			// Selected replacements already validated this source header before
			// output.
			if replacements.is_some_and(|files| files.contains_key(&graph.graph_id)) {
				continue;
			}
			let Some(binding) = graph.edge_endpoint else {
				continue;
			};
			let capability = after
				.schema
				.graphs
				.get(&graph.graph_id)
				.and_then(|schema| schema.edge_endpoint_index())
				.ok_or_else(|| invalid("retained endpoint file has no schema capability"))?;
			if binding.root != PageNo::new(0) {
				return Err(invalid("retained endpoint binding does not name its header"));
			}
			budget.retain(PAGE_SIZE + 4 * size_of::<(PageKey, Box<AlignedPage>)>())?;
			budget.stage_pages(1)?;
			let space = DataSpaceId::Graph(graph.graph_id);
			let key = PageKey::new(space, binding.file_id, binding.root);
			let mut page = store.read_update_page(state, key)?;
			crate::storage::edge_endpoint_index::rewrite_header_revision(
				&mut page,
				graph.graph_id,
				capability,
				previous,
				after.commit_ts,
				store.visible_pages(state, space, binding.file_id)?,
			)?;
			pages.push((key, page));
		}
		Ok(Self {
			pages,
		})
	}

	/// Joins header versions to the same catalog/data transaction.
	///
	/// # Errors
	/// Propagates private WAL staging failure before publication.
	pub(super) fn stage(&self, transaction: &mut TxHandle) -> FerrosGrynnResult<()> {
		for (key, page) in &self.pages {
			transaction.stage_page(*key, page)?;
		}
		Ok(())
	}
}

/// Publishes synchronous endpoint postings in the same generation as topology.
///
/// # Errors
/// Propagates capability, sorting, codec, allocation and unreachable-file I/O
/// failures.
pub(super) fn add_edge_endpoint(
	layout: &mut LayoutBuilder,
	prefix: &str,
	database: &DatabaseRoot,
	graph: crate::graph::GraphId,
	arena: &crate::storage::graph_pages::GraphArenaBindings,
	stream_root: Option<&Path>,
) -> FerrosGrynnResult<Option<PhysicalIndexBinding>> {
	let Some(capability) =
		database.schema.graph(graph).and_then(|schema| schema.edge_endpoint_index())
	else {
		return Ok(None);
	};
	let temporary = std::env::temp_dir();
	let scratch = stream_root.unwrap_or(&temporary);
	let (records, posting_count) =
		crate::storage::edge_endpoint_index::prepare(database, graph, arena, scratch)?;
	let header = crate::storage::edge_endpoint_index::EndpointHeader {
		graph,
		capability,
		revision: database.commit_ts,
		root: PageNo::NULL,
		page_count: 0,
		posting_count,
	};
	let path = format!("{prefix}/idx/ee_endpoint.fgidx");
	let space = DataSpaceId::Graph(graph);
	let file_id = if let Some(root) = stream_root {
		let built = crate::storage::scoped_index_image::write_streamed_file(root, &path, |file| {
			crate::storage::edge_endpoint_index::stream(
				&records,
				header,
				scratch,
				|number, page| file.write_page(number, page),
			)
		})?;
		layout.add_prewritten(
			space,
			DataFileKind::Index,
			DataFileRole::WalBackfilled,
			path,
			built.page_count,
		)?
	} else {
		let pages = crate::storage::edge_endpoint_index::buffered(&records, header, scratch)?;
		layout.add(space, DataFileKind::Index, DataFileRole::WalBackfilled, path, None, pages)?
	};
	Ok(Some(PhysicalIndexBinding {
		file_id,
		root: PageNo::new(0),
	}))
}

/// Selects logical changes and physical keys moved by prepared child scopes.
///
/// # Errors
/// Rejects absent or inconsistent source/final scopes and selected-set
/// admission failures.
pub(super) fn changed_graphs(
	state: &LocalState,
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &crate::datastore::PublicationDelta,
	arenas: Option<&BTreeMap<GraphId, GraphArenaBindings>>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<std::collections::BTreeSet<crate::graph::GraphId>> {
	let mut selected = changed_logical_graphs(before, after, delta, budget)?;
	for (&graph, final_arena) in arenas.into_iter().flatten() {
		if selected.contains(&graph) {
			continue;
		}
		let source = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph)
			.ok_or_else(|| invalid("endpoint remap source graph binding is absent"))?;
		if final_arena == &source.arena {
			continue;
		}
		let old =
			before.graph(graph).ok_or_else(|| invalid("endpoint remap source graph is absent"))?;
		// Logical posting equality does not preserve a dense numeric scope key.
		for (endpoint, _, _) in old.edge_endpoint_entries() {
			let crate::graph::EdgeEndpoint::Nested(route) = endpoint else {
				continue;
			};
			if source.arena.qualified(before, graph, route)?
				!= final_arena.qualified(after, graph, route)?
			{
				budget.tree_entry::<GraphId>(selected.len())?;
				selected.insert(graph);
				break;
			}
		}
	}
	Ok(selected)
}

/// Selects changed canonical postings once per explicit EDGE-bearing graph.
///
/// # Errors
/// Rejects missing accepted graphs and admits the selected owner set first.
fn changed_logical_graphs(
	before: &DatabaseRoot,
	after: &DatabaseRoot,
	delta: &crate::datastore::PublicationDelta,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<std::collections::BTreeSet<crate::graph::GraphId>> {
	let mut selected = std::collections::BTreeSet::new();
	if !std::sync::Arc::ptr_eq(&before.schema, &after.schema) {
		// A transient first E-E role still leaves a durable empty capability.
		for (&graph, schema) in &after.schema.graphs {
			if schema.edge_endpoint_index().is_some()
				&& before
					.schema
					.graphs
					.get(&graph)
					.is_some_and(|old| old.edge_endpoint_index().is_none())
			{
				budget.tree_entry::<crate::graph::GraphId>(selected.len())?;
				selected.insert(graph);
			}
		}
	}
	let mut examined = None;
	for &(graph, local) in &delta.incidence {
		if local.kind() != Some(crate::graph::AtomKind::Edge)
			|| examined == Some(graph)
			|| selected.contains(&graph)
		{
			continue;
		}
		let has_postings = [before, after].into_iter().any(|root| {
			root.occurrence(graph, local).is_some_and(|occurrence| {
				matches!(occurrence.state().view(), crate::datastore::OccurrenceStateRef::Edge { topology, .. }
					if topology.edge_endpoint_sides().next().is_some())
			})
		});
		if !has_postings {
			continue;
		}
		examined = Some(graph);
		let old = before.graph(graph).ok_or_else(|| invalid("endpoint source graph is absent"))?;
		let new = after.graph(graph).ok_or_else(|| invalid("endpoint final graph is absent"))?;
		if !old.edge_endpoint_entries().eq(new.edge_endpoint_entries()) {
			budget.tree_entry::<crate::graph::GraphId>(selected.len())?;
			selected.insert(graph);
		}
	}
	Ok(selected)
}

/// Streams changed trees using the final root and its prepared scope authority.
///
/// # Errors
/// Rejects invalid source headers and propagates admission, codec and I/O
/// errors.
pub(super) fn append_selected_indexes(
	store: &LocalStore,
	state: &LocalState,
	target: (&DatabaseRoot, Option<&BTreeMap<GraphId, GraphArenaBindings>>),
	selected: &std::collections::BTreeSet<crate::graph::GraphId>,
	generation: &str,
	layout: &mut LayoutBuilder,
	cleanup: &mut Option<super::cleanup::UnpublishedGeneration>,
	retired: &mut std::collections::BTreeSet<(DataSpaceId, crate::storage::types::DataFileId)>,
	budget: &mut PublicationBudget,
) -> FerrosGrynnResult<std::collections::BTreeMap<crate::graph::GraphId, PhysicalIndexBinding>> {
	let (after, arenas) = target;
	let mut replacements = std::collections::BTreeMap::new();
	for &graph in selected {
		let source = state
			.bindings
			.graphs
			.iter()
			.find(|binding| binding.graph_id == graph)
			.ok_or_else(|| invalid("selected endpoint graph binding is absent"))?;
		let previous = checked_source_binding(store, state, after, source, budget)?;
		// Accepted bindings validate the source; prepared arenas encode final
		// postings.
		let arena = arenas.and_then(|prepared| prepared.get(&graph)).unwrap_or(&source.arena);
		// Admit temporary path construction before allocating either String.
		budget.scratch(
			generation
				.len()
				.checked_add(128)
				.and_then(|bytes| bytes.checked_mul(4))
				.ok_or_else(|| invalid("endpoint path scratch overflows usize"))?,
		)?;
		let prefix = super::layout::graph_prefix(graph, generation);
		let path = format!("{prefix}/idx/ee_endpoint.fgidx");
		budget.retain(super::layout::selected_file_metadata_bytes(&path)?)?;
		budget.tree_entry::<(crate::graph::GraphId, PhysicalIndexBinding)>(replacements.len())?;
		if previous.is_some() {
			budget.tree_entry::<(DataSpaceId, crate::storage::types::DataFileId)>(retired.len())?;
		}
		if cleanup.is_none() {
			admit_cleanup_paths(budget, &store.root, after, generation)?;
			*cleanup = Some(super::cleanup::UnpublishedGeneration::for_database(
				&store.root,
				after,
				generation,
			));
		}
		let run_budget = selected_stream_budget(budget, &store.root, &path)?;
		// Serial sorting avoids auxiliary worker stacks in the point subtree.
		let replacement = crate::storage::graph_index::with_external_sort_options(
			run_budget,
			std::num::NonZeroUsize::MIN,
			|| add_edge_endpoint(layout, &prefix, after, graph, arena, Some(&store.root)),
		)?
		.ok_or_else(|| invalid("selected endpoint graph lost its capability"))?;
		if let Some(previous) = previous {
			retired.insert((DataSpaceId::Graph(graph), previous.file_id));
		}
		replacements.insert(graph, replacement);
	}
	Ok(replacements)
}

/// Checks accepted catalog authority before addition or replacement output.
///
/// # Errors
/// Rejects wrong ownership, revision, checksum, extent and source read
/// failures.
fn checked_source_binding(
	store: &LocalStore,
	state: &LocalState,
	after: &DatabaseRoot,
	source: &super::GraphBinding,
	budget: &PublicationBudget,
) -> FerrosGrynnResult<Option<PhysicalIndexBinding>> {
	let catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|graph| graph.id == source.graph_id)
		.ok_or_else(|| invalid("selected endpoint accepted graph is absent"))?;
	if catalog.edge_endpoint != source.edge_endpoint {
		return Err(invalid("selected endpoint catalog and runtime bindings disagree"));
	}
	let final_capability = after
		.schema
		.graphs
		.get(&source.graph_id)
		.and_then(|schema| schema.edge_endpoint_index())
		.ok_or_else(|| invalid("selected endpoint final capability is absent"))?;
	let (capability, previous) = match (catalog.edge_endpoint_index, source.edge_endpoint) {
		(None, None) if final_capability.version() == crate::datastore::SchemaVersion::INITIAL => {
			return Ok(None);
		}
		(Some((id, version)), Some(previous)) => (
			crate::datastore::SystemIndex::from_storage(
				id,
				version,
				crate::datastore::SystemIndexKind::EdgeEndpoint,
			),
			previous,
		),
		_ => return Err(invalid("selected endpoint source capability and binding disagree")),
	};
	if capability != final_capability {
		return Err(invalid("selected endpoint capability changed identity"));
	}
	if previous.root != PageNo::new(0) {
		return Err(invalid("selected endpoint binding does not name its header"));
	}
	budget.scratch(PAGE_SIZE)?;
	let space = DataSpaceId::Graph(source.graph_id);
	let page =
		store.read_update_page(state, PageKey::new(space, previous.file_id, previous.root))?;
	crate::storage::edge_endpoint_index::decode_header(
		&page,
		source.graph_id,
		capability,
		store.wal.snapshot().commit_ts().get(),
		store.visible_pages(state, space, previous.file_id)?,
	)?;
	Ok(Some(previous))
}

/// Bounds nested merge readers, keys, metadata and record Vec growth
/// separately.
///
/// # Errors
/// Rejects overflow and insufficient scratch before any unreachable file
/// exists.
fn selected_stream_budget(
	budget: &PublicationBudget,
	root: &Path,
	path: &str,
) -> FerrosGrynnResult<std::num::NonZeroUsize> {
	use crate::storage::graph_index::{INDEX_SORT_FAN_IN, INDEX_SORT_METADATA_BYTES};
	let paths = root
		.as_os_str()
		.as_encoded_bytes()
		.len()
		.checked_add(path.len())
		.and_then(|bytes| bytes.checked_add(128))
		.and_then(|bytes| bytes.checked_mul(32))
		.ok_or_else(|| invalid("endpoint streaming paths overflow usize"))?;
	// Two merges own up to 2F reader pages and 2F PAGE-bounded decoded keys.
	let fixed = ((4 * INDEX_SORT_FAN_IN + 32) * PAGE_SIZE + 4 * INDEX_SORT_METADATA_BYTES)
		.checked_add(paths)
		.ok_or_else(|| invalid("endpoint fixed scratch overflows usize"))?;
	let minimum = 8 * (PAGE_SIZE + size_of::<(Vec<u8>, u64)>());
	budget.scratch(
		fixed
			.checked_add(minimum)
			.ok_or_else(|| invalid("endpoint minimum scratch overflows usize"))?,
	)?;
	// At most three record Vecs coexist; one may grow, so their peak is <7q.
	// The builder divides this half by four: q <= usable/8, leaving fixed
	// apart.
	let run = (budget.remaining()? - fixed) / 2;
	std::num::NonZeroUsize::new(run).ok_or_else(|| invalid("endpoint run budget is empty"))
}

/// A selected-only rewire must admit the common all-graph cleanup vector too.
///
/// # Errors
/// Rejects count/path overflow or insufficient retained budget before
/// allocation.
fn admit_cleanup_paths(
	budget: &mut PublicationBudget,
	root: &Path,
	after: &DatabaseRoot,
	generation: &str,
) -> FerrosGrynnResult<()> {
	let count = after
		.schema
		.all_graphs()
		.count()
		.checked_add(1)
		.ok_or_else(|| invalid("endpoint cleanup directory count overflows usize"))?;
	let path = root
		.as_os_str()
		.as_encoded_bytes()
		.len()
		.checked_add(generation.len())
		.and_then(|bytes| bytes.checked_add(128))
		.ok_or_else(|| invalid("endpoint cleanup path bytes overflow usize"))?;
	// Chained joins and verbatim-path rebuilding precede guard ownership.
	let bytes = path
		.checked_add(size_of::<std::path::PathBuf>())
		.and_then(|bytes| bytes.checked_mul(8))
		.and_then(|bytes| bytes.checked_mul(count))
		.ok_or_else(|| invalid("endpoint cleanup metadata overflows usize"))?;

	let components = root
		.components()
		.count()
		.checked_add(8)
		.and_then(|count| count.checked_mul(4))
		.and_then(|count| count.checked_mul(size_of::<std::path::Component<'_>>()))
		.ok_or_else(|| invalid("endpoint cleanup component scratch overflows usize"))?;
	budget.scratch(
		bytes
			.checked_add(components)
			.ok_or_else(|| invalid("endpoint cleanup aggregate bytes overflow usize"))?,
	)?;
	budget.retain(bytes)
}

#[cfg(test)]
/// Shared cleanup metadata participates in the selected endpoint envelope.
mod tests {
	use super::*;
	use crate::{
		datastore::Datastore,
		graph::GraphKind,
		test_assertions::{AssertOption as _, AssertResult as _},
	};

	/// One selected file cannot hide cleanup allocations for many unrelated
	/// graphs.
	#[test]
	fn endpoint_cleanup_many_graphs_is_admitted_before_guard_allocation() {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_graph("one", GraphKind::Graph).assert_ok();
		seed.commit().assert_ok();
		let one = store.inner.current_root();
		let mut seed = store.begin();
		for index in 0..63 {
			seed.define_graph(format!("other_{index}"), GraphKind::Graph).assert_ok();
		}
		seed.commit().assert_ok();
		let many = store.inner.current_root();
		let root = std::env::temp_dir()
			.join(format!("fg-endpoint-cleanup-{}", crate::values::Uuid::now_v7()));
		let generation = crate::values::Uuid::now_v7().to_string();
		crate::storage::graph_index::with_external_sort_options(
			std::num::NonZeroUsize::new(8 * 1024).assert_some(),
			std::num::NonZeroUsize::MIN,
			|| {
				let mut small = PublicationBudget::new();
				admit_cleanup_paths(&mut small, Path::new("r"), &one, &generation).assert_ok();
				assert!(small.remaining().assert_ok() < 8 * 1024);
				let mut budget = PublicationBudget::new();
				let before = budget.remaining().assert_ok();
				let error = admit_cleanup_paths(&mut budget, Path::new("r"), &many, &generation)
					.assert_err();
				assert!(matches!(
					error.status().status(),
					crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
				));
				assert_eq!(budget.remaining().assert_ok(), before);
				Ok(())
			},
		)
		.assert_ok();
		let mut normal = PublicationBudget::new();
		admit_cleanup_paths(&mut normal, &root, &many, &generation).assert_ok();
		let _guard =
			super::super::cleanup::UnpublishedGeneration::for_database(&root, &many, &generation);
	}

	/// Long and verbatim inputs must reject before any cleanup path is built.
	#[test]
	fn endpoint_cleanup_long_paths_include_join_and_component_scratch() {
		let store = Datastore::in_memory();
		let root = store.inner.current_root();
		assert_eq!(root.schema.all_graphs().count(), 0);
		let paths = [
			std::path::PathBuf::from("r".repeat(4096)),
			#[cfg(windows)]
			std::path::PathBuf::from(format!(r"\\?\C:\{}", r"a\".repeat(512))),
		];
		crate::storage::graph_index::with_external_sort_options(
			std::num::NonZeroUsize::new(24 * 1024).assert_some(),
			std::num::NonZeroUsize::MIN,
			|| {
				for path in &paths {
					let mut budget = PublicationBudget::new();
					let error =
						admit_cleanup_paths(&mut budget, path, &root, "generation").assert_err();
					assert!(matches!(
						error.status().status(),
						crate::gql_status::Status::StorageDecodeLimitExceeded { .. }
					));
					assert_eq!(budget.remaining().assert_ok(), 24 * 1024);
				}
				Ok(())
			},
		)
		.assert_ok();
	}
}
