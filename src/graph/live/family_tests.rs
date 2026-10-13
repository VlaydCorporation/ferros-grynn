//! Regression oracles for family layouts and failed publication boundaries.

use super::{LiveEvent, LiveFamily, LiveGraph, LiveUpdate, Mutable, invalid_state};
use crate::{
	gql_status::FerrosGrynnResult,
	graph::{
		AtomId,
		Dag,
		DirtyTarget,
		EdgeEndpoint,
		EdgeTopology,
		Forest,
		Graph,
		GraphMut,
		GraphRead,
		GraphTopologyConstraint,
		GraphTopologyConstraints,
		Hypergraph,
		Metagraph,
		NodeRead,
		QualifiedAtomId,
		RootedTree,
		Subgraph,
		Tree,
		TreeOrientation,
	},
	values::PropertyMap,
};

#[test]
fn mutable_snapshot_admits_actual_properties_before_copy() -> FerrosGrynnResult<()> {
	let mut graph = Graph::<Mutable>::new();
	graph.properties_mut()?.insert(1_u32.into(), "x".repeat(2 * 1024 * 1024));
	let live = LiveGraph::new_mutable(graph);
	let mut observed = 0;
	let failed = live.static_snapshot_admitted(&mut |bytes, _work| {
		observed = bytes;
		Err(invalid_state("snapshot refused before copy"))
	});
	assert!(failed.is_err());
	assert!(observed >= 2 * 1024 * 1024);
	assert_eq!(live.with_graph(GraphRead::revision)?, live.static_snapshot()?.revision());
	Ok(())
}

fn exercise_family<G: LiveFamily<State = Mutable, Mutable = G> + GraphMut>(
	graph: G,
	dirty: impl Fn(&G) -> Vec<DirtyTarget>,
) {
	let original_dirty = dirty(&graph);
	assert!(!original_dirty.is_empty());
	assert_eq!(dirty(&G::clone_mutable(&graph)), original_dirty);
	let ids = graph.node_ids().unwrap().collect::<FerrosGrynnResult<Vec<_>>>().unwrap();
	let first = ids[0];
	let second = ids[1];
	let identity = graph.identity();
	let original_revision = graph.revision();
	let live = LiveGraph::new(G::freeze(graph));
	let old = live.read().unwrap();
	let update = LiveUpdate::<G>::for_family(
		identity,
		original_revision + 10,
		[DirtyTarget::Atom(first)],
		move |graph| {
			graph.node_properties_mut(first)?.insert(1_u32.into(), 1);
			Ok(())
		},
	);
	live.apply_update(&update).unwrap();
	assert_eq!(old.revision(), original_revision);
	assert!(
		!GraphRead::node(&*old, first)
			.unwrap()
			.unwrap()
			.properties()
			.unwrap()
			.contains_key(1_u32.into())
	);
	assert_eq!(live.read().unwrap().revision(), original_revision + 10);
	let mutable = live.into_mutable().unwrap();
	mutable
		.mutate_local([DirtyTarget::Atom(first)], move |graph| {
			graph.node_properties_mut(first)?.insert(2_u32.into(), 2);
			Ok(())
		})
		.unwrap();
	let remote = LiveUpdate::<G>::for_family(
		identity,
		original_revision + 20,
		[DirtyTarget::Atom(second)],
		move |graph| {
			graph.node_properties_mut(second)?.insert(3_u32.into(), 3);
			Ok(())
		},
	);
	assert!(matches!(mutable.ingest(remote).unwrap(), LiveEvent::Applied { .. }));
	let state = mutable.mutable_state().unwrap();
	assert_eq!(state.remote_revision, original_revision + 20);
	assert!(state.local_changes.contains(&DirtyTarget::Atom(first)));
	assert!(
		state.graph.node(first).unwrap().unwrap().properties().unwrap().contains_key(2_u32.into())
	);
	assert!(
		state.graph.node(second).unwrap().unwrap().properties().unwrap().contains_key(3_u32.into())
	);
}

#[test]
fn frozen_and_mutable_live_graphs_keep_each_concrete_family() {
	let mut graph = Graph::<Mutable>::new();
	let first = graph.add_node(None, PropertyMap::new()).unwrap();
	let second = graph.add_node(None, PropertyMap::new()).unwrap();
	graph.add_edge(EdgeTopology::undirected([first, second]), None, PropertyMap::new()).unwrap();
	exercise_family(graph, Graph::transaction_dirty);
	let mut forest = Forest::<Mutable>::new(TreeOrientation::OutArborescence);
	let first = forest.add_root(None, PropertyMap::new()).unwrap();
	let second = forest.add_root(None, PropertyMap::new()).unwrap();
	forest.connect(first, second, None, PropertyMap::new()).unwrap();
	exercise_family(forest, Forest::transaction_dirty);
	let mut tree =
		Tree::<Mutable>::new(TreeOrientation::Undirected, None, PropertyMap::new()).unwrap();
	let root = tree.roots().next().unwrap();
	tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
	exercise_family(tree, Tree::transaction_dirty);
	let mut rooted =
		RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
			.unwrap();
	rooted.add_child(rooted.root(), None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
	exercise_family(rooted, RootedTree::transaction_dirty);
	let mut dag = Dag::<Mutable>::new();
	let first = dag.add_node(None, PropertyMap::new()).unwrap();
	let second = dag.add_node(None, PropertyMap::new()).unwrap();
	dag.add_edge(first, second, None, PropertyMap::new()).unwrap();
	exercise_family(dag, Dag::transaction_dirty);
	let mut hypergraph = Hypergraph::<Mutable>::new();
	let first = hypergraph.add_node(None, PropertyMap::new()).unwrap();
	let second = hypergraph.add_node(None, PropertyMap::new()).unwrap();
	hypergraph
		.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
		.unwrap();
	exercise_family(hypergraph, Hypergraph::transaction_dirty);
	let mut metagraph = Metagraph::<Mutable>::new();
	metagraph.add_node(None, PropertyMap::new()).unwrap();
	metagraph.add_node(None, PropertyMap::new()).unwrap();
	exercise_family(metagraph, Metagraph::transaction_dirty);
	let mut graph = Graph::<Mutable>::new();
	graph.add_node(None, PropertyMap::new()).unwrap();
	graph.add_node(None, PropertyMap::new()).unwrap();
	exercise_family(Subgraph::from_graph(graph), |graph| graph.as_graph().transaction_dirty());
}

fn failing_conflict(fail: bool) -> (LiveGraph<Mutable>, AtomId, u64) {
	let mut graph = Graph::<Mutable>::new();
	let node = graph.add_node(None, PropertyMap::new()).unwrap();
	let revision = graph.revision();
	let identity = graph.identity();
	let live = LiveGraph::<Mutable>::new_mutable(graph);
	live.mutate_local([DirtyTarget::Atom(node)], |graph| {
		graph.node_properties_mut(node)?.insert(1_u32.into(), 1);
		Ok(())
	})
	.unwrap();
	let update =
		LiveUpdate::new(identity, revision + 10, [DirtyTarget::Atom(node)], move |graph| {
			graph.node_properties_mut(node)?.insert(2_u32.into(), 2);
			if fail {
				return Err(invalid_state("injected candidate failure"));
			}
			Ok(())
		});
	let accepted = live.ingest(update);
	if fail {
		assert!(accepted.is_err());
	} else {
		assert!(matches!(accepted.unwrap(), LiveEvent::Conflict { .. }));
	}
	(live, node, revision)
}

fn assert_conflict_preserved(
	live: &LiveGraph<Mutable>,
	node: AtomId,
	revision: u64,
	queued: usize,
) {
	let state = live.mutable_state().unwrap();
	assert!(state.paused);
	assert!(state.pending.is_some());
	assert_eq!(state.queue.len(), queued);
	assert_eq!(state.remote_revision, revision);
	assert!(state.local_changes.contains(&DirtyTarget::Atom(node)));
	assert!(state.graph.node(node).unwrap().properties().contains_key(1_u32.into()));
	assert!(!state.graph.node(node).unwrap().properties().contains_key(2_u32.into()));
	assert!(!state.base.node(node).unwrap().properties().contains_key(1_u32.into()));
}

#[test]
fn invalid_overlapping_callback_does_not_poison_conflict_or_revision() {
	let (live, node, revision) = failing_conflict(true);
	let identity = live.with_graph(Graph::identity).unwrap();
	{
		let state = live.mutable_state().unwrap();
		assert!(!state.paused);
		assert!(state.pending.is_none());
		assert!(state.queue.is_empty());
		assert_eq!(state.remote_revision, revision);
		assert!(state.local_changes.contains(&DirtyTarget::Atom(node)));
		assert!(!state.graph.node(node).unwrap().properties().contains_key(2_u32.into()));
	}
	let corrected =
		LiveUpdate::new(identity, revision + 10, [DirtyTarget::Atom(node)], move |graph| {
			graph.node_properties_mut(node)?.insert(2_u32.into(), 2);
			Ok(())
		});
	assert!(matches!(live.ingest(corrected).unwrap(), LiveEvent::Conflict { .. }));
	live.accept_remote().unwrap();
}

#[test]
fn failed_rebase_preserves_the_old_remote_base_and_local_state() {
	let (live, node, revision) = failing_conflict(false);
	assert!(
		live.rebase(|graph| {
			graph.node_properties_mut(node)?.insert(4_u32.into(), 4);
			Err(invalid_state("injected replay failure"))
		})
		.is_err()
	);
	assert_conflict_preserved(&live, node, revision, 0);
	assert!(live.resume().is_err());
	live.accept_remote().unwrap();
}

#[test]
fn invalid_queued_callback_preserves_tail_and_accepts_corrected_revision() {
	let (live, node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).unwrap();
	let update = LiveUpdate::new(identity, revision + 20, [], |_| {
		Err(invalid_state("injected queue failure"))
	});
	assert!(live.ingest(update).is_err());
	assert_conflict_preserved(&live, node, revision, 0);
	let corrected = LiveUpdate::new(identity, revision + 20, [], move |graph| {
		if !graph.node(node).is_some_and(|node| node.properties().contains_key(2_u32.into())) {
			return Err(invalid_state("validated pending remote tail was lost"));
		}
		graph.node_properties_mut(node)?.insert(3_u32.into(), 3);
		Ok(())
	});
	assert!(matches!(live.ingest(corrected).unwrap(), LiveEvent::Queued { .. }));
	assert!(
		matches!(live.accept_remote().unwrap(), LiveEvent::Resumed { remote_revision } if remote_revision == revision + 20)
	);
	assert!(live.mutable_state().unwrap().queue.is_empty());
}

#[test]
fn queued_local_failure_preserves_pending_base_dirty_and_remote_tail() {
	let (live, node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).unwrap();
	let update = LiveUpdate::new(identity, revision + 20, [], move |graph| {
		if graph.node(node).is_some_and(|node| node.properties().contains_key(1_u32.into())) {
			return Err(invalid_state("queued remote delta cannot merge into this local copy"));
		}
		graph.node_properties_mut(node)?.insert(3_u32.into(), 3);
		Ok(())
	});
	assert!(matches!(live.ingest(update).unwrap(), LiveEvent::Queued { .. }));
	assert!(live.keep_local().is_err());
	assert_conflict_preserved(&live, node, revision, 1);
	live.accept_remote().unwrap();
}

#[test]
fn queued_revisions_must_advance_the_last_accepted_revision() {
	let (live, node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).unwrap();
	let update = || LiveUpdate::new(identity, revision + 10, [], |_| Ok(()));
	assert!(live.ingest(update()).is_err());
	assert_conflict_preserved(&live, node, revision, 0);
}

#[test]
fn discard_local_drains_accepted_revisions_before_new_delivery() {
	let (live, _node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).unwrap();
	let update = |revision| LiveUpdate::new(identity, revision, [], |_| Ok(()));
	live.ingest(update(revision + 20)).unwrap();
	assert!(
		matches!(live.discard_local().unwrap(), LiveEvent::Resumed { remote_revision } if remote_revision == revision + 20)
	);
	assert_eq!(live.mutable_state().unwrap().queue.len(), 0);
	live.ingest(update(revision + 30)).unwrap();
	assert!(
		matches!(live.resume().unwrap(), LiveEvent::Resumed { remote_revision } if remote_revision == revision + 30)
	);
}

#[test]
fn concurrent_frozen_updates_cannot_publish_from_the_same_stale_base() {
	use std::sync::{Arc, Barrier};
	let mut graph = Graph::<Mutable>::new();
	graph.add_node(None, PropertyMap::new()).unwrap();
	let identity = graph.identity();
	let revision = graph.revision();
	let live = Arc::new(LiveGraph::new(graph.freeze()));
	let barrier = Arc::new(Barrier::new(2));
	let handles = [10, 20].map(|advance| {
		let live = Arc::clone(&live);
		let barrier = Arc::clone(&barrier);
		std::thread::spawn(move || {
			live.apply_update(&LiveUpdate::new(identity, revision + advance, [], move |graph| {
				graph.add_node(None, PropertyMap::new())?;
				barrier.wait();
				Ok(())
			}))
		})
	});
	let results = handles.map(|handle| handle.join().unwrap());
	assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
	assert_eq!(live.read().unwrap().node_count(), 2);
}

#[test]
fn callbacks_cannot_replace_the_materialization_identity() {
	let mut graph = Graph::<Mutable>::new();
	let node = graph.add_node(None, PropertyMap::new()).unwrap();
	let identity = graph.identity();
	let revision = graph.revision();
	let live = LiveGraph::new(graph.freeze());
	let update = LiveUpdate::new(identity, revision + 10, [], |graph| {
		*graph = Graph::<Mutable>::new();
		Ok(())
	});
	assert!(live.apply_update(&update).is_err());
	assert_eq!(live.read().unwrap().identity(), identity);
	assert_eq!(live.read().unwrap().revision(), revision);
	let mutable = live.into_mutable().unwrap();
	assert!(
		mutable
			.mutate_local([DirtyTarget::Atom(node)], |graph| {
				*graph = Graph::<Mutable>::new();
				Ok(())
			})
			.is_err()
	);
	assert_eq!(mutable.static_snapshot().unwrap().identity(), identity);
}

#[test]
fn family_and_orthogonal_invariants_are_checked_before_publication() {
	let mut graph = Graph::<Mutable>::new();
	let first = graph.add_node(None, PropertyMap::new()).unwrap();
	let second = graph.add_node(None, PropertyMap::new()).unwrap();
	graph.add_edge(EdgeTopology::undirected([first, second]), None, PropertyMap::new()).unwrap();
	let mut constraints = GraphTopologyConstraints::empty();
	constraints.insert(GraphTopologyConstraint::Complete);
	graph.set_topology_constraints(constraints);
	let live = LiveGraph::<Mutable>::new_mutable(graph);
	let revision = live.static_snapshot().unwrap().revision();
	assert!(
		live.mutate_local([DirtyTarget::Graph], |graph| {
			graph.add_node(None, PropertyMap::new())?;
			Ok(())
		})
		.is_err()
	);
	assert_eq!(live.static_snapshot().unwrap().revision(), revision);
	let mut dag = Dag::<Mutable>::new();
	let first = dag.add_node(None, PropertyMap::new()).unwrap();
	let second = dag.add_node(None, PropertyMap::new()).unwrap();
	dag.add_edge(first, second, None, PropertyMap::new()).unwrap();
	let live = LiveGraph::<Mutable, Dag<Mutable>>::new_mutable(dag);
	assert!(
		live.mutate_local([DirtyTarget::Graph], |dag| {
			dag.add_edge(second, first, None, PropertyMap::new())?;
			Ok(())
		})
		.is_err()
	);
	assert_eq!(live.static_snapshot().unwrap().edge_count(), 1);
}

#[test]
fn nested_incidence_resolves_edge_transition_owners() {
	let mut child = Graph::<Mutable>::new();
	let terminal = child.add_node(None, PropertyMap::new()).unwrap();
	let mut graph = Graph::<Mutable>::new();
	let first = graph.add_node(None, PropertyMap::new()).unwrap();
	let second = graph.add_node(None, PropertyMap::new()).unwrap();
	let owner = graph
		.add_metaedge(
			EdgeTopology::directed([first], [second]),
			child.freeze(),
			None,
			PropertyMap::new(),
		)
		.unwrap();
	let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([owner], terminal).unwrap());
	let identity = graph.identity();
	let revision = graph.revision();
	let live = LiveGraph::new(graph.freeze());
	let update = LiveUpdate::new(identity, revision + 10, [DirtyTarget::Graph], move |graph| {
		graph.add_edge(
			EdgeTopology::directed([nested.clone()], [second]),
			None,
			PropertyMap::new(),
		)?;
		Ok(())
	});
	live.apply_update(&update).unwrap();
	assert_eq!(live.read().unwrap().edge_count(), 2);
}

#[test]
fn containment_rejects_ancestor_identity_but_allows_shared_siblings() {
	for edge_owner in [false, true] {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).unwrap();
		let second = graph.add_node(None, PropertyMap::new()).unwrap();
		let live = LiveGraph::<Mutable>::new_mutable(graph);
		let old = live.static_snapshot().unwrap();
		let revision = old.revision();
		assert!(
			live.mutate_local([DirtyTarget::Graph], move |graph| {
				if edge_owner {
					graph.add_metaedge(
						EdgeTopology::directed([first], [second]),
						old,
						None,
						PropertyMap::new(),
					)?;
				} else {
					graph.add_metavertex(old, None, PropertyMap::new())?;
				}
				Ok(())
			})
			.is_err()
		);
		assert_eq!(live.static_snapshot().unwrap().revision(), revision);
		assert!(live.mutable_state().unwrap().local_changes.is_empty());
	}
	let mut child = Graph::<Mutable>::new();
	child.add_node(None, PropertyMap::new()).unwrap();
	let child = child.freeze();
	let live = LiveGraph::<Mutable>::new_mutable(Graph::<Mutable>::new());
	live.mutate_local([DirtyTarget::Graph], move |graph| {
		graph.add_metavertex(child.clone(), None, PropertyMap::new())?;
		graph.add_metavertex(child, None, PropertyMap::new())?;
		Ok(())
	})
	.unwrap();
	assert_eq!(live.static_snapshot().unwrap().node_count(), 2);
}

#[test]
fn deeply_shared_containment_validates_distinct_graphs_without_expanding_routes() {
	let mut child = Graph::<Mutable>::new();
	child.add_node(None, PropertyMap::new()).unwrap();
	let mut child = child.freeze();
	for _level in 0..30 {
		let mut parent = Graph::<Mutable>::new();
		parent.add_metavertex(child.clone(), None, PropertyMap::new()).unwrap();
		parent.add_metavertex(child, None, PropertyMap::new()).unwrap();
		child = parent.freeze();
	}
	let identity = child.identity();
	let revision = child.revision();
	let live = LiveGraph::new(child);
	let update = LiveUpdate::new(identity, revision + 10, [DirtyTarget::Graph], |graph| {
		graph.properties_mut()?.insert(1_u32.into(), 1);
		Ok(())
	});
	live.apply_update(&update).unwrap();
	assert_eq!(live.read().unwrap().revision(), revision + 10);
	let mutable = live.into_mutable().unwrap();
	let mut work = 0;
	let mut bytes = 0;
	mutable
		.static_snapshot_admitted(&mut |additional, units| {
			work += units;
			bytes += additional;
			assert!(work < 1_000, "shared containment expanded routes");
			assert!(bytes < 1 << 20, "shared layouts counted repeatedly");
			Ok(())
		})
		.unwrap();
}
