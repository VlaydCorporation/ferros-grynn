//! Mixed commits update every admitted ancestor map in the same physical plan.

use super::*;
use crate::storage::{
	graph_pages::{GraphArenaBindings, build_graph_image, decode_subgraph_directory},
	page::PageLsn,
};

/// Alternating public families establish a real containment chain before
/// capture.
fn ancestors(mut case: Case, depth: usize) -> (Case, Vec<(GraphRef, AtomId)>) {
	let mut setup = case.fixture.store.begin();
	let mut child = case.fixture.graph.id();
	let mut parents = Vec::new();
	for level in 0..depth {
		let family = if level % 2 == 0 {
			GraphKind::Graph
		} else {
			GraphKind::Metagraph
		};
		let graph = setup.define_graph(format!("mixed_parent_{level}"), family).assert_ok();
		let record = format!("node:mixed_parent_{level}").parse::<RecordId>().assert_ok();
		setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		let owner = setup.attach_node(graph.id(), &record, None, Some(child)).assert_ok();
		child = graph.id();
		parents.push((graph, owner));
	}
	setup.commit().assert_ok();
	case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
	case.fixture = reopen(case.fixture);
	let mut control = case.fixture.store.begin();
	control
		.set_occurrence_label(
			case.fixture.graph.id(),
			&child_presence_tests::record(case.selected.kind().assert_some()),
			Some("mixed-ancestor-source".into()),
		)
		.assert_ok();
	control.commit().assert_ok();
	assert_old(&case);
	(case, parents)
}

/// Every accepted directory agrees with a freshly built independent graph
/// image.
fn assert_current_arenas(case: &Case, sources: &[(GraphId, GraphArenaBindings)], retained: bool) {
	let root = case.fixture.store.inner.current_root();
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	for (graph, old) in sources {
		let binding =
			state.bindings.graphs.iter().find(|binding| binding.graph_id == *graph).assert_some();
		let catalog =
			state._database_catalog.graphs.iter().find(|entry| entry.id == *graph).assert_some();
		assert!(binding.arena.same_owner(&catalog.arena));
		if retained {
			let mut expected = old.graph_by_slot().clone();
			let mut changed = 0;
			for child in expected.values_mut() {
				if *child == case.children[0] {
					*child = case.children[1];
					changed += 1;
				}
			}
			assert_eq!(changed, 1);
			assert_eq!(binding.arena.graph_by_slot(), &expected);
			assert_eq!(binding.arena.scope_by_slot(), old.scope_by_slot());
		}
		let mut context =
			crate::values::storage::EncodeContext::new(root.dictionary.as_ref().clone());
		let image = build_graph_image(&root, *graph, &mut context, PageLsn::ZERO).assert_ok();
		assert_eq!(binding.arena, image.arena);
		assert!(!image.subgraphs.is_empty());
		for (index, expected) in image.subgraphs.iter().enumerate() {
			let number = PageNo::new(index as u64);
			let key = PageKey::new(binding.files.space(), binding.files.subgraphs, number);
			assert_eq!(
				decode_subgraph_directory(&store.read_update_page(&state, key).assert_ok(), number)
					.assert_ok(),
				decode_subgraph_directory(expected, number).assert_ok()
			);
		}
	}
}

/// Existing graph pages outside directories and headers must keep their
/// versions.
fn watched(case: &Case, sources: &[(GraphId, GraphArenaBindings)]) -> (Vec<PageKey>, PageKey) {
	let store = local(&case.fixture.store);
	let state = store.state.lock();
	let mut keys = Vec::new();
	for entry in &state.file_catalog.files {
		let DataSpaceId::Graph(graph) = entry.space_id else {
			continue;
		};
		if sources.iter().any(|(id, _)| *id == graph)
			&& matches!(
				entry.file_id.kind(),
				Some(DataFileKind::GraphSuperblock | DataFileKind::Subgraphs)
			) {
			continue;
		}
		if let Some(count) = super::super::super::watched_page_count(store, &state, entry) {
			keys.extend(
				(0..count)
					.map(|number| PageKey::new(entry.space_id, entry.file_id, PageNo::new(number))),
			);
		}
	}
	let statistics = LocalStore::statistics_file(
		&state,
		DataSpaceId::Graph(case.fixture.graph.id()),
		DataFileKind::Statistics,
	)
	.assert_ok();
	let statistics =
		PageKey::new(DataSpaceId::Graph(case.fixture.graph.id()), statistics, PageNo::new(0));
	assert!(!keys.is_empty() && keys.contains(&statistics));
	(keys, statistics)
}

/// Depth three is a deliberate full fallback; one and two retain all
/// identities.
fn matrix(mode: IoMode) {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for kind in [AtomKind::Node, AtomKind::Edge] {
			for depth in 1..=3 {
				let (mut case, parents) = ancestors(case_fixture(mode, family, kind), depth);
				let source = case.fixture.store.inner.current_root();
				let store = local(&case.fixture.store);
				let (files, sources, home) = {
					let state = store.state.lock();
					let sources = std::iter::once(case.fixture.graph.id())
						.chain(parents.iter().map(|(graph, _)| graph.id()))
						.map(|id| {
							(
								id,
								state
									.bindings
									.graphs
									.iter()
									.find(|binding| binding.graph_id == id)
									.assert_some()
									.arena
									.clone(),
							)
						})
						.collect::<Vec<_>>();
					(
						state.file_catalog.files.clone(),
						sources,
						state.database_superblock.checkpoint_lsn,
					)
				};
				let (keys, statistics) = watched(&case, &sources);
				let boundary = store.wal.snapshot().visible_lsn();
				assert!(boundary > home);
				let prefix = property_prefix(&case);
				let mut old = case.fixture.store.begin();
				let old_query = old.query_snapshot().assert_ok();
				change(&case, 0).commit().assert_ok();
				assert_current(&case, 0);
				assert_current_arenas(&case, &sources, depth <= 2);
				if depth <= 2 {
					assert_files(&case, &files);
					assert_prefix(&case, &prefix);
					let frames = store.wal.visible_frames(&keys, store.wal.snapshot());
					let changed = keys
						.iter()
						.zip(frames.frames())
						.filter_map(|(key, frame)| {
							frame
								.as_ref()
								.filter(|frame| frame.frame_lsn() > boundary)
								.map(|_| *key)
						})
						.collect::<Vec<_>>();
					assert_eq!(changed, vec![statistics]);
					drop(frames);
				} else {
					assert_ne!(store.state.lock().file_catalog.files, files);
				}
				assert!(case.fixture.store.checkpoint(CheckpointMode::Passive).assert_ok());
				if depth <= 2 {
					let advanced = store.state.lock().database_superblock.checkpoint_lsn;
					assert!(advanced > home && advanced <= boundary);
				}
				assert_eq!(
					old.query_physical_inner_graph(&old_query, &case.fixture.graph, case.selected)
						.assert_ok()
						.assert_some()
						.graph,
					Some(case.children[0])
				);
				assert_eq!(
					old.atom(&case.record).assert_ok().assert_some().properties().get(case.key),
					Some(&Value::from(16_i64))
				);
				let mut child = case.fixture.graph.id();
				for (graph, owner) in &parents {
					assert_eq!(
						old.query_physical_inner_graph(&old_query, graph, *owner)
							.assert_ok()
							.assert_some()
							.graph,
						Some(child)
					);
					child = graph.id();
				}
				assert!(sources.iter().all(|(_, arena)| {
					arena.graph_by_slot().values().any(|child| *child == case.children[0])
				}));
				drop(old_query);
				drop(old);
				drop(source);
				case.fixture = reopen(case.fixture);
				assert_current(&case, 0);
				assert_current_arenas(&case, &sources, depth <= 2);
				if depth <= 2 {
					assert_files(&case, &files);
					assert_prefix(&case, &prefix);
				}
				case.fixture.store.checkpoint(CheckpointMode::Restart).assert_ok();
				case.fixture = reopen(case.fixture);
				assert_current(&case, 0);
				assert_current_arenas(&case, &sources, depth <= 2);
				if depth <= 2 {
					assert_files(&case, &files);
					assert_prefix(&case, &prefix);
				}
				eprintln!(
					"MIXED_LEAF_BASE_ANCESTOR_PASS {mode:?} {family:?} {kind:?} depth={depth}"
				);
			}
		}
	}
}

/// Sync covers both admitted depths and the deeper fallback after cold reopen.
#[test]
fn mixed_leaf_base_sync_ancestors_preserve_complete_scope_maps() {
	matrix(IoMode::Sync);
}

/// NativeAsync checks exact physical identities, WAL slices and old pins.
#[cfg(feature = "native-async-io")]
#[test]
fn mixed_leaf_base_native_async_ancestors_preserve_complete_scope_maps() {
	matrix(IoMode::NativeAsync);
}
