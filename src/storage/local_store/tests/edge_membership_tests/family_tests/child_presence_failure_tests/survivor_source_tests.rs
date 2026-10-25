//! Valid codecs still require canonical owners and complete survivor sources.

use super::{
	source_tests::{hot_fault_at, stage},
	*,
};
use crate::{
	datastore::{DatabaseRoot, PublicationDelta},
	storage::{
		graph_pages::{decode_subgraph_directory, encode_subgraph_directory},
		page::PageLsn,
	},
};

/// Sync rejects each injected authority between passing source controls.
#[test]
fn child_scope_remap_sync_malformed_sources_preserve_boundary() {
	sources(IoMode::Sync);
}

/// NativeAsync exercises the same complete selected/survivor identities.
#[cfg(feature = "native-async-io")]
#[test]
fn child_scope_remap_native_async_malformed_sources_preserve_boundary() {
	sources(IoMode::NativeAsync);
}

/// Each fault changes a single semantic authority while keeping valid bytes.
fn inject(
	case: &Case,
	accepted: &GraphArenaBindings,
	pages: &[(PageKey, Box<AlignedPage>)],
	indices: (usize, usize),
	fault: usize,
) -> &'static str {
	let store = local(&case.fixture.store);
	let (owners, children) = case.survivors.as_ref().assert_some();
	match fault {
		0..=3 => {
			hot_fault_at(case, case.owner, pages[0].0, &pages[0].1, fault);
			if case.kind == AtomKind::Node {
				"leaf rebind source NODE identity or child slot disagrees"
			} else {
				"EDGE Hot source disagrees with accepted root"
			}
		}
		4 => {
			store.state.lock().bindings.graphs[indices.0].arena = GraphArenaBindings::new(
				accepted.graph_by_slot().clone(),
				accepted.scope_by_slot().clone(),
			);
			"child remap source catalog arena owner disagrees"
		}
		5..=7 => {
			let number = pages[1].0.page_no();
			let (_, mut entries) = decode_subgraph_directory(&pages[1].1, number).assert_ok();
			let next = (fault == 5).then_some(number);
			if fault == 6 {
				entries.remove(0);
			} else if fault == 7 {
				entries.last_mut().assert_some().generation += 1;
			}
			let page = encode_subgraph_directory(number, PageLsn::ZERO, next, &entries).assert_ok();
			stage(store, pages[1].0, &page);
			match fault {
				5 => "parent remap source chain exceeds its ceiling or cycles",
				6 => "parent remap source has unreachable pages or missing entries",
				_ => "parent directory direct scope or generation disagrees",
			}
		}
		8 => {
			store
				.state
				.lock()
				.graph_superblocks
				.get_mut(&case.fixture.graph.id())
				.assert_some()
				.subgraphs_pages = 2;
			"parent remap directory source layout disagrees"
		}
		9 => {
			store.state.lock().graph_superblocks.get_mut(&children[0]).assert_some().node_count = 0;
			if owners[0].child_graph().is_none() {
				"parent directory replacement child counters disagree"
			} else {
				"parent directory source child counters disagree"
			}
		}
		10 => {
			let mut graphs = accepted.graph_by_slot().clone();
			let slot = accepted
				.direct_child_slot(
					&case.fixture.store.inner.current_root(),
					case.fixture.graph.id(),
					owners[1].local_id(),
				)
				.assert_ok();
			assert_eq!(graphs.insert(slot, children[0]), Some(children[1]));
			assert_ne!(children[0], children[1]);
			let forged = GraphArenaBindings::new(graphs, accepted.scope_by_slot().clone());
			let mut state = store.state.lock();
			state.bindings.graphs[indices.0].arena = forged.clone();
			state._database_catalog.graphs[indices.1].arena = forged;
			"child remap source arena disagrees with canonical scopes"
		}
		11..=14 => {
			let index = if case.kind == AtomKind::Node {
				2
			} else {
				0
			};
			let owner = (owners[1].global_id(), owners[1].local_id());
			hot_fault_at(case, owner, pages[index].0, &pages[index].1, fault - 11);
			"EDGE Hot source disagrees with accepted root"
		}
		_ => {
			assert_eq!(fault, 15);
			store.state.lock().graph_superblocks.get_mut(&children[1]).assert_some().node_count = 0;
			"parent directory source child counters disagree"
		}
	}
}

/// No failure may add frames, rotate files, or replace even a forged source
/// owner.
pub(super) fn rejected(
	case: &Case,
	roots: (&DatabaseRoot, &DatabaseRoot),
	delta: &PublicationDelta,
	reason: &str,
) {
	let store = local(&case.fixture.store);
	let root = case.fixture.store.inner.current_root();
	let boundary = store.wal.snapshot().visible_lsn();
	let state = store.state.lock();
	let files = state.file_catalog.files.clone();
	let binding = state
		.bindings
		.graphs
		.iter()
		.find(|entry| entry.graph_id == case.fixture.graph.id())
		.assert_some()
		.arena
		.clone();
	let catalog = state
		._database_catalog
		.graphs
		.iter()
		.find(|entry| entry.id == case.fixture.graph.id())
		.assert_some()
		.arena
		.clone();
	drop(state);
	let paths = super::super::super::super::generation_directory_paths(&case.fixture.directory.0);
	let pages = case.all_pages();
	let error = prepare_leaf(store, roots, delta).err().assert_some();
	assert!(
		matches!(error.status().status(), crate::gql_status::Status::InvalidStorageFile { reason: actual }
		if actual.as_str() == reason),
		"expected={reason}, error={error}"
	);
	assert!(Arc::ptr_eq(&root, &case.fixture.store.inner.current_root()));
	assert_eq!(store.wal.snapshot().visible_lsn(), boundary);
	let state = store.state.lock();
	assert_eq!(state.file_catalog.files, files);
	assert!(
		state
			.bindings
			.graphs
			.iter()
			.find(|entry| entry.graph_id == case.fixture.graph.id())
			.assert_some()
			.arena
			.same_owner(&binding)
	);
	assert!(
		state
			._database_catalog
			.graphs
			.iter()
			.find(|entry| entry.id == case.fixture.graph.id())
			.assert_some()
			.arena
			.same_owner(&catalog)
	);
	drop(state);
	assert_eq!(
		super::super::super::super::generation_directory_paths(&case.fixture.directory.0),
		paths
	);
	let current = case.all_pages();
	assert_eq!(current.len(), pages.len());
	for ((key, page), (old_key, old)) in current.iter().zip(&pages) {
		assert_eq!(key, old_key);
		assert_eq!(page.as_bytes(), old.as_bytes());
	}
}

/// Raw test frames use their actual WAL clock without advancing the MVCC root.
fn sources(mode: IoMode) {
	sources_with_family(mode, GraphKind::Metagraph);
}

/// Runs the same fixture contract for the selected public parent family.
pub(super) fn sources_with_family(mode: IoMode, family: GraphKind) {
	for kind in [AtomKind::Node, AtomKind::Edge] {
		for attach in [false, true] {
			let case = Case::surviving_with_family(mode, kind, attach, family);
			case.check(false);
			let published = case.fixture.store.inner.current_root();
			let store = local(&case.fixture.store);
			let (before, after, delta) =
				leaf_roots(&case.fixture, case.owner.1, case.states.1.clone());
			let (owners, children) = case.survivors.as_ref().assert_some();
			let state = store.state.lock();
			let indices = (
				state
					.bindings
					.graphs
					.iter()
					.position(|entry| entry.graph_id == case.fixture.graph.id())
					.assert_some(),
				state
					._database_catalog
					.graphs
					.iter()
					.position(|entry| entry.id == case.fixture.graph.id())
					.assert_some(),
			);
			let accepted = state.bindings.graphs[indices.0].arena.clone();
			assert!(accepted.same_owner(&state._database_catalog.graphs[indices.1].arena));
			let header = *state.graph_superblocks.get(&case.fixture.graph.id()).assert_some();
			let child_headers =
				children.map(|child| (child, *state.graph_superblocks.get(&child).assert_some()));
			drop(state);
			let pages = case.all_pages();
			assert_eq!(
				pages.len(),
				if kind == AtomKind::Node {
					3
				} else {
					2
				}
			);
			for fault in 0..16 {
				prepare_leaf(store, (&before, &after), &delta).assert_ok();
				let reason = inject(&case, &accepted, &pages, indices, fault);
				rejected(&case, (&before, &after), &delta, reason);
				let mut state = store.state.lock();
				state.bindings.graphs[indices.0].arena = accepted.clone();
				state._database_catalog.graphs[indices.1].arena = accepted.clone();
				*state.graph_superblocks.get_mut(&case.fixture.graph.id()).assert_some() = header;
				for (child, saved) in child_headers {
					*state.graph_superblocks.get_mut(&child).assert_some() = saved;
				}
				drop(state);
				for (key, page) in &pages {
					stage(store, *key, page);
				}
				prepare_leaf(store, (&before, &after), &delta).assert_ok();
				case.assert_owners(&accepted);
			}
			assert!(Arc::ptr_eq(&published, &case.fixture.store.inner.current_root()));
			let stamp = store.wal.snapshot().commit_ts().get();
			assert!(stamp > published.commit_ts);
			assert!(store.capture_query_snapshot(published.commit_ts).assert_ok().is_none());
			let physical = store.capture_query_snapshot(stamp).assert_ok().assert_some();
			for owner in owners {
				let actual = store
					.physical_traversal_inner_graph(
						&physical,
						case.fixture.graph.id(),
						owner.local_id(),
					)
					.assert_ok()
					.assert_some();
				assert_eq!(actual.graph, owner.child_graph());
			}
			drop(physical);
			assert_bulk_image(&case.fixture);
		}
	}
}
