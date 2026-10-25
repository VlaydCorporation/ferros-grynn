//! Preparation preserves accepted nonnumeric chains and valid empty tails.

use std::collections::BTreeMap;

use super::{source_tests::stage, survivor_source_tests::rejected, *};
use crate::{
	datastore::{DatabaseRoot, PublicationDelta},
	storage::{
		graph_pages::{
			SubgraphEntry,
			build_graph_image,
			decode_subgraph_directory,
			encode_subgraph_directory,
			open_graph_image,
		},
		graph_superblock::GraphSuperblock,
		page::PageLsn,
	},
};

/// Sync preparation covers both survivor remap and retained zero/one tails.
#[test]
fn child_scope_directory_sync_extended_chain_preserves_order_and_rejects_faults() {
	cases(IoMode::Sync);
}

/// NativeAsync uses the actual opened backend for all source-page reads.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_directory_native_async_extended_chain_preserves_order_and_rejects_faults() {
	cases(IoMode::NativeAsync);
}

/// Literal dense slots derive from the copied inputs, never from prepared maps.
fn expected(case: &Case, root: &DatabaseRoot) -> (Vec<SubgraphEntry>, GraphArenaBindings) {
	let mut direct = Vec::new();
	let selected = root.occurrence(case.fixture.graph.id(), case.owner.1).assert_some();
	if let Some(child) = selected.child_graph() {
		direct.push((case.owner.1, child));
	}
	if let Some((owners, children)) = &case.survivors {
		direct.push((owners[1].local_id(), children[1]));
	}
	let mut graphs = BTreeMap::from([(0, case.fixture.graph.id())]);
	let mut scopes = BTreeMap::from([(0, Box::<[AtomId]>::default())]);
	let entries = direct
		.into_iter()
		.enumerate()
		.map(|(index, (owner, child))| {
			let slot = u32::try_from(index + 1).assert_ok();
			graphs.insert(slot, child);
			scopes.insert(slot, vec![owner].into_boxed_slice());
			// Both fixture leaves have exactly one NODE and no EDGE capacity.
			SubgraphEntry {
				slot,
				generation: owner.generation(),
				node_capacity: 1,
				edge_capacity: 0,
				node_count: 1,
				edge_count: 0,
			}
		})
		.collect();
	(entries, GraphArenaBindings::new(graphs, scopes))
}

/// Trusted numeric keys are kept separate from directory traversal order.
fn read_pages(case: &Case, keys: &[PageKey; 3]) -> Vec<Box<AlignedPage>> {
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	keys.iter().map(|key| store.read_update_page(&state, *key).assert_ok()).collect()
}

/// Exact source links include both empty tail pages after the first full page.
fn assert_source(case: &Case, keys: &[PageKey; 3], entries: &[SubgraphEntry]) {
	let pages = read_pages(case, keys);
	for (index, (key, page)) in keys.iter().zip(pages).enumerate() {
		let next = [Some(PageNo::new(2)), None, Some(PageNo::new(1))][index];
		let expected = if index == 0 {
			entries
		} else {
			&[]
		};
		assert_eq!(
			decode_subgraph_directory(&page, key.page_no()).assert_ok(),
			(next, expected.to_vec())
		);
	}
}

/// The complete-image reader independently admits this preparation-only source.
fn install_source(case: &Case, root: &DatabaseRoot, entries: &[SubgraphEntry]) -> [PageKey; 3] {
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	let files = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == case.fixture.graph.id())
		.assert_some()
		.files;
	assert_eq!(
		state.graph_superblocks.get(&case.fixture.graph.id()).assert_some().subgraphs_pages,
		1
	);
	drop(state);
	let keys =
		[0, 1, 2].map(|number| PageKey::new(files.space(), files.subgraphs, PageNo::new(number)));
	let mut context = crate::storage::atom_store_image::value_context(
		&root.schema,
		root.dictionary.as_ref().clone(),
	)
	.assert_ok();
	let mut image =
		build_graph_image(root, case.fixture.graph.id(), &mut context, PageLsn::ZERO).assert_ok();
	image.subgraphs = keys
		.iter()
		.enumerate()
		.map(|(index, key)| {
			let next = [Some(PageNo::new(2)), None, Some(PageNo::new(1))][index];
			let selected = if index == 0 {
				entries
			} else {
				&[]
			};
			encode_subgraph_directory(key.page_no(), PageLsn::ZERO, next, selected).assert_ok()
		})
		.collect();
	image.superblock.subgraphs_pages = 3;
	for (key, page) in keys.iter().zip(&image.subgraphs) {
		stage(store, *key, page);
	}
	store
		.state
		.lock()
		.graph_superblocks
		.get_mut(&case.fixture.graph.id())
		.assert_some()
		.subgraphs_pages = 3;
	let opened = open_graph_image(image, root, &context.decoder()).assert_ok();
	assert_eq!(
		opened.occurrences().count(),
		root.graph(case.fixture.graph.id()).assert_some().occurrences().count()
	);
	assert_source(case, &keys, entries);
	keys
}

/// One changed directory frame preserves all three accepted physical pages.
fn assert_plan(
	case: &Case,
	roots: (&DatabaseRoot, &DatabaseRoot),
	delta: &PublicationDelta,
	keys: &[PageKey; 3],
) {
	let store = local(&case.fixture.store);
	let boundary = store.wal.snapshot().visible_lsn();
	let source = read_pages(case, keys);
	let hot_and_directory = case.all_pages();
	let plan = prepare_leaf(store, roots, delta).assert_ok();
	let (entries, arena) = expected(case, roots.1);
	assert_eq!(plan.arenas.get(&case.fixture.graph.id()), Some(&arena));
	let header = plan.superblocks.get(&case.fixture.graph.id()).assert_some();
	let state = store.state.lock();
	let old = state.graph_superblocks.get(&case.fixture.graph.id()).assert_some();
	assert_eq!(header.subgraphs_pages, 3);
	assert_eq!(header.subgraph_count, u64::try_from(entries.len()).assert_ok());
	assert_eq!(header.subgraph_capacity, old.subgraph_capacity.max(header.subgraph_count));
	assert_eq!(
		(header.checkpoint_generation, header.backfilled_lsn, header.checkpoint_record_lsn),
		(
			state.database_superblock.generation,
			state.database_superblock.checkpoint_lsn,
			state.database_superblock.checkpoint_record_lsn
		)
	);
	let files = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == case.fixture.graph.id())
		.assert_some()
		.files;
	assert_eq!(store.visible_pages(&state, files.space(), files.subgraphs).assert_ok(), 3);
	let header_key = PageKey::new(files.space(), files.superblock, PageNo::new(0));
	drop(state);
	let changed =
		plan.pages.iter().filter(|(key, _)| key.file_id() == keys[0].file_id()).collect::<Vec<_>>();
	assert_eq!(changed.len(), 1);
	assert_eq!(changed[0].0, keys[0]);
	assert_eq!(
		decode_subgraph_directory(&changed[0].1, PageNo::new(0)).assert_ok(),
		(Some(PageNo::new(2)), entries)
	);
	let prepared_header = &plan.pages.iter().find(|(key, _)| *key == header_key).assert_some().1;
	assert_eq!(GraphSuperblock::decode(prepared_header).assert_ok(), *header);
	let mut expected_keys = hot_and_directory.iter().map(|(key, _)| *key).collect::<Vec<_>>();
	expected_keys.push(header_key);
	expected_keys.sort_unstable();
	let mut actual_keys = plan.pages.iter().map(|(key, _)| *key).collect::<Vec<_>>();
	actual_keys.sort_unstable();
	assert_eq!(actual_keys, expected_keys);
	assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
	let current = case.all_pages();
	assert_eq!(current.len(), hot_and_directory.len());
	for ((key, page), (old_key, old)) in current.iter().zip(&hot_and_directory) {
		assert_eq!(key, old_key);
		assert_eq!(page.as_bytes(), old.as_bytes());
	}
	for (before, after) in source.iter().zip(read_pages(case, keys)) {
		assert_eq!(after.as_bytes(), before.as_bytes());
	}
}

/// Link faults never append page3, so restoring the three-page baseline is
/// valid.
fn inject(
	case: &Case,
	keys: &[PageKey; 3],
	entries: &[SubgraphEntry],
	fault: usize,
) -> &'static str {
	let index = if fault == 0 {
		1
	} else {
		2
	};
	let next = match fault {
		0 => Some(PageNo::new(0)),
		1 => Some(PageNo::new(3)),
		2 => None,
		_ => Some(PageNo::new(1)),
	};
	let selected = if fault == 3 {
		&entries[..1]
	} else {
		&[]
	};
	let page =
		encode_subgraph_directory(keys[index].page_no(), PageLsn::ZERO, next, selected).assert_ok();
	stage(local(&case.fixture.store), keys[index], &page);
	match fault {
		0 | 1 => "parent remap source chain exceeds its ceiling or cycles",
		2 => "parent remap source has unreachable pages or missing entries",
		_ => "parent remap source has a reserved or duplicate slot",
	}
}

/// Raw frames prove source/preparation behavior; no cold recovery claim is
/// made.
fn one(case: Case) {
	case.check(false);
	let root = case.fixture.store.inner.current_root();
	let store = local(&case.fixture.store);
	let accepted = case.arena();
	let (before, after, delta) = leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
	let (entries, arena) = expected(&case, &before);
	assert_eq!(accepted, arena);
	let keys = install_source(&case, &before, &entries);
	assert_plan(&case, (&before, &after), &delta, &keys);
	let baseline = read_pages(&case, &keys);
	if case.survivors.is_some() {
		for fault in 0..4 {
			let reason = inject(&case, &keys, &entries, fault);
			let injected = read_pages(&case, &keys);
			rejected(&case, (&before, &after), &delta, reason);
			for (before, after) in injected.iter().zip(read_pages(&case, &keys)) {
				assert_eq!(after.as_bytes(), before.as_bytes());
			}
			// Keep ceiling3: raw appends cannot be undone by restoring header1.
			for (key, page) in keys.iter().zip(&baseline) {
				stage(store, *key, page);
			}
			assert_source(&case, &keys, &entries);
			assert_plan(&case, (&before, &after), &delta, &keys);
			case.assert_owners(&accepted);
		}
	}
	assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
	let stamp = store.wal.snapshot().commit_ts().get();
	assert!(stamp > root.commit_ts);
	assert!(store.capture_query_snapshot(root.commit_ts).assert_ok().is_none());
	let pin = store.capture_query_snapshot(stamp).assert_ok().assert_some();
	let actual = store
		.physical_traversal_inner_graph(&pin, case.fixture.graph.id(), case.owner.1)
		.assert_ok()
		.assert_some();
	assert_eq!(
		actual.graph,
		before.occurrence(case.fixture.graph.id(), case.owner.1).assert_some().child_graph()
	);
	drop(pin);
	case.assert_owners(&accepted);
	assert_source(&case, &keys, &entries);
}

/// Eight valid shapes per backend include zero/one tails and survivor remaps.
fn cases(mode: IoMode) {
	cases_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn cases_with_family(mode: IoMode, family: GraphKind) {
	for surviving in [false, true] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for attach in [false, true] {
				let case = if surviving {
					Case::surviving_with_family(mode, kind, attach, family)
				} else {
					Case::new_with_family(mode, kind, attach, family)
				};
				one(case);
			}
		}
	}
}
