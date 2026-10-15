//! Regression oracles for family layouts and failed publication boundaries.

use super::{LiveEvent, LiveFamily, LiveGraph, LiveUpdate, Mutable, invalid_state};
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
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

/// Verifies mutable snapshot admits actual properties before copy.
#[test]
fn mutable_snapshot_admits_actual_properties_before_copy() {
	use crate::test_assertions::AssertResult as _;

	let mut graph = Graph::<Mutable>::new();
	graph.properties_mut().assert_ok().insert(1_u32.into(), "x".repeat(2 * 1024 * 1024));
	let live = LiveGraph::new_mutable(graph);
	let mut observed = 0;
	let failed = live.static_snapshot_admitted(&mut |bytes, _work| {
		observed = bytes;
		Err(invalid_state("snapshot refused before copy"))
	});
	assert!(failed.is_err());
	assert!(observed >= 2 * 1024 * 1024);
	assert_eq!(
		live.with_graph(GraphRead::revision).assert_ok(),
		live.static_snapshot().assert_ok().revision()
	);
}

/// Returns exercise family for this module.
fn exercise_family<G: LiveFamily<State = Mutable, Mutable = G> + GraphMut>(
	graph: G,
	dirty: impl Fn(&G) -> Vec<DirtyTarget>,
) {
	let original_dirty = dirty(&graph);
	assert!(!original_dirty.is_empty());
	assert_eq!(dirty(&G::clone_mutable(&graph)), original_dirty);
	let ids = graph.node_ids().assert_ok().collect::<FerrosGrynnResult<Vec<_>>>().assert_ok();
	let first = ids[0];
	let second = ids[1];
	let identity = graph.identity();
	let original_revision = graph.revision();
	let live = LiveGraph::new(G::freeze(graph));
	let old = live.read().assert_ok();
	let update = LiveUpdate::<G>::for_family(
		identity,
		original_revision + 10,
		[DirtyTarget::Atom(first)],
		move |graph| {
			graph.node_properties_mut(first)?.insert(1_u32.into(), 1);
			Ok(())
		},
	);
	live.apply_update(&update).assert_ok();
	assert_eq!(old.revision(), original_revision);
	assert!(
		!GraphRead::node(&*old, first)
			.assert_ok()
			.assert_some()
			.properties()
			.assert_ok()
			.contains_key(1_u32.into())
	);
	assert_eq!(live.read().assert_ok().revision(), original_revision + 10);
	let mutable = live.into_mutable().assert_ok();
	mutable
		.mutate_local([DirtyTarget::Atom(first)], move |graph| {
			graph.node_properties_mut(first)?.insert(2_u32.into(), 2);
			Ok(())
		})
		.assert_ok();
	let remote = LiveUpdate::<G>::for_family(
		identity,
		original_revision + 20,
		[DirtyTarget::Atom(second)],
		move |graph| {
			graph.node_properties_mut(second)?.insert(3_u32.into(), 3);
			Ok(())
		},
	);
	assert!(matches!(mutable.ingest(remote).assert_ok(), LiveEvent::Applied { .. }));
	let state = mutable.mutable_state().assert_ok();
	assert_eq!(state.remote_revision, original_revision + 20);
	assert!(state.local_changes.contains(&DirtyTarget::Atom(first)));
	assert!(
		state
			.graph
			.node(first)
			.assert_ok()
			.assert_some()
			.properties()
			.assert_ok()
			.contains_key(2_u32.into())
	);
	assert!(
		state
			.graph
			.node(second)
			.assert_ok()
			.assert_some()
			.properties()
			.assert_ok()
			.contains_key(3_u32.into())
	);
}

/// Verifies frozen and mutable live graphs keep each concrete family.
#[test]
fn frozen_and_mutable_live_graphs_keep_each_concrete_family() {
	let mut graph = Graph::<Mutable>::new();
	let first = graph.add_node(None, PropertyMap::new()).assert_ok();
	let second = graph.add_node(None, PropertyMap::new()).assert_ok();
	graph.add_edge(EdgeTopology::undirected([first, second]), None, PropertyMap::new()).assert_ok();
	exercise_family(graph, Graph::transaction_dirty);
	let mut forest = Forest::<Mutable>::new(TreeOrientation::OutArborescence);
	let first = forest.add_root(None, PropertyMap::new()).assert_ok();
	let second = forest.add_root(None, PropertyMap::new()).assert_ok();
	forest.connect(first, second, None, PropertyMap::new()).assert_ok();
	exercise_family(forest, Forest::transaction_dirty);
	let mut tree =
		Tree::<Mutable>::new(TreeOrientation::Undirected, None, PropertyMap::new()).assert_ok();
	let root = tree.roots().next().assert_some();
	tree.add_child(root, None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
	exercise_family(tree, Tree::transaction_dirty);
	let mut rooted =
		RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
			.assert_ok();
	rooted.add_child(rooted.root(), None, PropertyMap::new(), None, PropertyMap::new()).assert_ok();
	exercise_family(rooted, RootedTree::transaction_dirty);
	let mut dag = Dag::<Mutable>::new();
	let first = dag.add_node(None, PropertyMap::new()).assert_ok();
	let second = dag.add_node(None, PropertyMap::new()).assert_ok();
	dag.add_edge(first, second, None, PropertyMap::new()).assert_ok();
	exercise_family(dag, Dag::transaction_dirty);
	let mut hypergraph = Hypergraph::<Mutable>::new();
	let first = hypergraph.add_node(None, PropertyMap::new()).assert_ok();
	let second = hypergraph.add_node(None, PropertyMap::new()).assert_ok();
	hypergraph
		.add_edge(EdgeTopology::directed([first], [second]), None, PropertyMap::new())
		.assert_ok();
	exercise_family(hypergraph, Hypergraph::transaction_dirty);
	let mut metagraph = Metagraph::<Mutable>::new();
	metagraph.add_node(None, PropertyMap::new()).assert_ok();
	metagraph.add_node(None, PropertyMap::new()).assert_ok();
	exercise_family(metagraph, Metagraph::transaction_dirty);
	let mut graph = Graph::<Mutable>::new();
	graph.add_node(None, PropertyMap::new()).assert_ok();
	graph.add_node(None, PropertyMap::new()).assert_ok();
	exercise_family(Subgraph::from_graph(graph), |graph| graph.as_graph().transaction_dirty());
}

/// Returns failing conflict for this module.
fn failing_conflict(fail: bool) -> (LiveGraph<Mutable>, AtomId, u64) {
	let mut graph = Graph::<Mutable>::new();
	let node = graph.add_node(None, PropertyMap::new()).assert_ok();
	let revision = graph.revision();
	let identity = graph.identity();
	let live = LiveGraph::<Mutable>::new_mutable(graph);
	live.mutate_local([DirtyTarget::Atom(node)], |graph| {
		graph.node_properties_mut(node)?.insert(1_u32.into(), 1);
		Ok(())
	})
	.assert_ok();
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
		assert!(matches!(accepted.assert_ok(), LiveEvent::Conflict { .. }));
	}
	(live, node, revision)
}

/// Returns assert conflict preserved for this module.
fn assert_conflict_preserved(
	live: &LiveGraph<Mutable>,
	node: AtomId,
	revision: u64,
	queued: usize,
) {
	let state = live.mutable_state().assert_ok();
	assert!(state.paused);
	assert!(state.pending.is_some());
	assert_eq!(state.queue.len(), queued);
	assert_eq!(state.remote_revision, revision);
	assert!(state.local_changes.contains(&DirtyTarget::Atom(node)));
	assert!(state.graph.node(node).assert_some().properties().contains_key(1_u32.into()));
	assert!(!state.graph.node(node).assert_some().properties().contains_key(2_u32.into()));
	assert!(!state.base.node(node).assert_some().properties().contains_key(1_u32.into()));
}

/// Verifies invalid overlapping callback does not poison conflict or revision.
#[test]
fn invalid_overlapping_callback_does_not_poison_conflict_or_revision() {
	let (live, node, revision) = failing_conflict(true);
	let identity = live.with_graph(Graph::identity).assert_ok();
	{
		let state = live.mutable_state().assert_ok();
		assert!(!state.paused);
		assert!(state.pending.is_none());
		assert!(state.queue.is_empty());
		assert_eq!(state.remote_revision, revision);
		assert!(state.local_changes.contains(&DirtyTarget::Atom(node)));
		assert!(!state.graph.node(node).assert_some().properties().contains_key(2_u32.into()));
	}
	let corrected =
		LiveUpdate::new(identity, revision + 10, [DirtyTarget::Atom(node)], move |graph| {
			graph.node_properties_mut(node)?.insert(2_u32.into(), 2);
			Ok(())
		});
	assert!(matches!(live.ingest(corrected).assert_ok(), LiveEvent::Conflict { .. }));
	live.accept_remote().assert_ok();
}

/// Verifies failed rebase preserves the old remote base and local state.
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
	live.accept_remote().assert_ok();
}

/// Verifies invalid queued callback preserves tail and accepts corrected
/// revision.
#[test]
fn invalid_queued_callback_preserves_tail_and_accepts_corrected_revision() {
	let (live, node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).assert_ok();
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
	assert!(matches!(live.ingest(corrected).assert_ok(), LiveEvent::Queued { .. }));
	assert!(
		matches!(live.accept_remote().assert_ok(), LiveEvent::Resumed { remote_revision } if remote_revision == revision + 20)
	);
	assert!(live.mutable_state().assert_ok().queue.is_empty());
}

/// Verifies queued local failure preserves pending base dirty and remote tail.
#[test]
fn queued_local_failure_preserves_pending_base_dirty_and_remote_tail() {
	let (live, node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).assert_ok();
	let update = LiveUpdate::new(identity, revision + 20, [], move |graph| {
		if graph.node(node).is_some_and(|node| node.properties().contains_key(1_u32.into())) {
			return Err(invalid_state("queued remote delta cannot merge into this local copy"));
		}
		graph.node_properties_mut(node)?.insert(3_u32.into(), 3);
		Ok(())
	});
	assert!(matches!(live.ingest(update).assert_ok(), LiveEvent::Queued { .. }));
	assert!(live.keep_local().is_err());
	assert_conflict_preserved(&live, node, revision, 1);
	live.accept_remote().assert_ok();
}

/// Verifies queued revisions must advance the last accepted revision.
#[test]
fn queued_revisions_must_advance_the_last_accepted_revision() {
	let (live, node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).assert_ok();
	let update = || LiveUpdate::new(identity, revision + 10, [], |_| Ok(()));
	assert!(live.ingest(update()).is_err());
	assert_conflict_preserved(&live, node, revision, 0);
}

/// Verifies discard local drains accepted revisions before new delivery.
#[test]
fn discard_local_drains_accepted_revisions_before_new_delivery() {
	let (live, _node, revision) = failing_conflict(false);
	let identity = live.with_graph(Graph::identity).assert_ok();
	let update = |revision| LiveUpdate::new(identity, revision, [], |_| Ok(()));
	live.ingest(update(revision + 20)).assert_ok();
	assert!(
		matches!(live.discard_local().assert_ok(), LiveEvent::Resumed { remote_revision } if remote_revision == revision + 20)
	);
	assert_eq!(live.mutable_state().assert_ok().queue.len(), 0);
	live.ingest(update(revision + 30)).assert_ok();
	assert!(
		matches!(live.resume().assert_ok(), LiveEvent::Resumed { remote_revision } if remote_revision == revision + 30)
	);
}

/// Verifies concurrent frozen updates cannot publish from the same stale base.
#[test]
fn concurrent_frozen_updates_cannot_publish_from_the_same_stale_base() {
	use std::sync::{Arc, Barrier};
	let mut graph = Graph::<Mutable>::new();
	graph.add_node(None, PropertyMap::new()).assert_ok();
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
	let results = handles.map(|handle| handle.join().assert_ok());
	assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
	assert_eq!(live.read().assert_ok().node_count(), 2);
}

/// Verifies callbacks cannot replace the materialization identity.
#[test]
fn callbacks_cannot_replace_the_materialization_identity() {
	let mut graph = Graph::<Mutable>::new();
	let node = graph.add_node(None, PropertyMap::new()).assert_ok();
	let identity = graph.identity();
	let revision = graph.revision();
	let live = LiveGraph::new(graph.freeze());
	let update = LiveUpdate::new(identity, revision + 10, [], |graph| {
		*graph = Graph::<Mutable>::new();
		Ok(())
	});
	assert!(live.apply_update(&update).is_err());
	assert_eq!(live.read().assert_ok().identity(), identity);
	assert_eq!(live.read().assert_ok().revision(), revision);
	let mutable = live.into_mutable().assert_ok();
	assert!(
		mutable
			.mutate_local([DirtyTarget::Atom(node)], |graph| {
				*graph = Graph::<Mutable>::new();
				Ok(())
			})
			.is_err()
	);
	assert_eq!(mutable.static_snapshot().assert_ok().identity(), identity);
}

/// Verifies family and orthogonal invariants are checked before publication.
#[test]
fn family_and_orthogonal_invariants_are_checked_before_publication() {
	let mut graph = Graph::<Mutable>::new();
	let first = graph.add_node(None, PropertyMap::new()).assert_ok();
	let second = graph.add_node(None, PropertyMap::new()).assert_ok();
	graph.add_edge(EdgeTopology::undirected([first, second]), None, PropertyMap::new()).assert_ok();
	let mut constraints = GraphTopologyConstraints::empty();
	constraints.insert(GraphTopologyConstraint::Complete);
	graph.set_topology_constraints(constraints);
	let live = LiveGraph::<Mutable>::new_mutable(graph);
	let revision = live.static_snapshot().assert_ok().revision();
	assert!(
		live.mutate_local([DirtyTarget::Graph], |graph| {
			graph.add_node(None, PropertyMap::new())?;
			Ok(())
		})
		.is_err()
	);
	assert_eq!(live.static_snapshot().assert_ok().revision(), revision);
	let mut dag = Dag::<Mutable>::new();
	let first = dag.add_node(None, PropertyMap::new()).assert_ok();
	let second = dag.add_node(None, PropertyMap::new()).assert_ok();
	dag.add_edge(first, second, None, PropertyMap::new()).assert_ok();
	let live = LiveGraph::<Mutable, Dag<Mutable>>::new_mutable(dag);
	assert!(
		live.mutate_local([DirtyTarget::Graph], |dag| {
			dag.add_edge(second, first, None, PropertyMap::new())?;
			Ok(())
		})
		.is_err()
	);
	assert_eq!(live.static_snapshot().assert_ok().edge_count(), 1);
}

/// Verifies nested incidence resolves edge transition owners.
#[test]
fn nested_incidence_resolves_edge_transition_owners() {
	let mut child = Graph::<Mutable>::new();
	let terminal = child.add_node(None, PropertyMap::new()).assert_ok();
	let mut graph = Graph::<Mutable>::new();
	let first = graph.add_node(None, PropertyMap::new()).assert_ok();
	let second = graph.add_node(None, PropertyMap::new()).assert_ok();
	let owner = graph
		.add_metaedge(
			EdgeTopology::directed([first], [second]),
			child.freeze(),
			None,
			PropertyMap::new(),
		)
		.assert_ok();
	let nested = EdgeEndpoint::Nested(QualifiedAtomId::new([owner], terminal).assert_ok());
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
	live.apply_update(&update).assert_ok();
	assert_eq!(live.read().assert_ok().edge_count(), 2);
}

/// Verifies containment rejects ancestor identity but allows shared siblings.
#[test]
fn containment_rejects_ancestor_identity_but_allows_shared_siblings() {
	for edge_owner in [false, true] {
		let mut graph = Graph::<Mutable>::new();
		let first = graph.add_node(None, PropertyMap::new()).assert_ok();
		let second = graph.add_node(None, PropertyMap::new()).assert_ok();
		let live = LiveGraph::<Mutable>::new_mutable(graph);
		let old = live.static_snapshot().assert_ok();
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
		assert_eq!(live.static_snapshot().assert_ok().revision(), revision);
		assert!(live.mutable_state().assert_ok().local_changes.is_empty());
	}
	let mut child = Graph::<Mutable>::new();
	child.add_node(None, PropertyMap::new()).assert_ok();
	let child = child.freeze();
	let live = LiveGraph::<Mutable>::new_mutable(Graph::<Mutable>::new());
	live.mutate_local([DirtyTarget::Graph], move |graph| {
		graph.add_metavertex(child.clone(), None, PropertyMap::new())?;
		graph.add_metavertex(child, None, PropertyMap::new())?;
		Ok(())
	})
	.assert_ok();
	assert_eq!(live.static_snapshot().assert_ok().node_count(), 2);
}

/// Verifies deeply shared containment validates distinct graphs without
/// expanding routes.
#[test]
fn deeply_shared_containment_validates_distinct_graphs_without_expanding_routes() {
	let mut child = Graph::<Mutable>::new();
	child.add_node(None, PropertyMap::new()).assert_ok();
	let mut child = child.freeze();
	for _level in 0..30 {
		let mut parent = Graph::<Mutable>::new();
		parent.add_metavertex(child.clone(), None, PropertyMap::new()).assert_ok();
		parent.add_metavertex(child, None, PropertyMap::new()).assert_ok();
		child = parent.freeze();
	}
	let identity = child.identity();
	let revision = child.revision();
	let live = LiveGraph::new(child);
	let update = LiveUpdate::new(identity, revision + 10, [DirtyTarget::Graph], |graph| {
		graph.properties_mut()?.insert(1_u32.into(), 1);
		Ok(())
	});
	live.apply_update(&update).assert_ok();
	assert_eq!(live.read().assert_ok().revision(), revision + 10);
	let mutable = live.into_mutable().assert_ok();
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
		.assert_ok();
}
