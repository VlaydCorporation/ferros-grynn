//! Graph owners share all closed-journal classifier assertions.

use super::*;

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn node_leaf_rebind_requires_closed_delta_and_full_owner_generation() {
	check_node_leaf_rebind_requires_closed_delta_and_full_owner_generation(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn leaf_rebind_accepts_shared_direct_ancestors_only_for_present_replacement() {
	check_leaf_rebind_accepts_shared_direct_ancestors_only_for_present_replacement(
		GraphKind::Graph,
	);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn leaf_rebind_rejects_deeper_ancestors_and_other_graph_mutations() {
	check_leaf_rebind_rejects_deeper_ancestors_and_other_graph_mutations(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn node_leaf_rebind_rejects_attach_detach_descendants_and_ancestors() {
	check_node_leaf_rebind_rejects_attach_detach_descendants_and_ancestors(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn child_presence_with_survivors_selects_remap_without_widening_fast_path() {
	check_child_presence_with_survivors_selects_remap_without_widening_fast_path(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn child_presence_remap_accepts_qualified_node_roles_through_survivors() {
	check_child_presence_remap_accepts_qualified_node_roles_through_survivors(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn leaf_child_presence_accepts_node_and_edge_without_surviving_children() {
	check_leaf_child_presence_accepts_node_and_edge_without_surviving_children(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn node_leaf_rebind_preserves_scope_with_selected_owner_routes() {
	check_node_leaf_rebind_preserves_scope_with_selected_owner_routes(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn node_leaf_rebind_keeps_foreign_qualified_handle_collisions_independent() {
	check_node_leaf_rebind_keeps_foreign_qualified_handle_collisions_independent(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn edge_leaf_rebind_requires_unchanged_roles_and_weight() {
	check_edge_leaf_rebind_requires_unchanged_roles_and_weight(GraphKind::Graph);
}

/// Graph uses the unchanged closed-journal positive and negative checks.
#[test]
fn edge_leaf_rebind_preserves_scope_with_qualified_container_routes() {
	check_edge_leaf_rebind_preserves_scope_with_qualified_container_routes(GraphKind::Graph);
}

/// Other public families keep the existing full-publication eligibility.
#[test]
fn other_parent_families_remain_on_full_publication() {
	for family in [
		GraphKind::Metagraph,
		GraphKind::Tree,
		GraphKind::RootedTree,
		GraphKind::Forest,
		GraphKind::Dag,
		GraphKind::Hypergraph,
	] {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		let graph = seed.define_graph("guard_control", family).assert_ok().id();
		// Trees require a singleton; SUBGRAPH cannot be a durable catalog
		// family.
		seed.define_class(ClassSchema::new("guard_item", AtomKind::Node, []).assert_ok())
			.assert_ok();
		let record = "guard_item:one".parse::<RecordId>().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		let owner = seed.attach_node(graph, &record, None, None).assert_ok();
		seed.commit().assert_ok();
		let before = store.inner.current_root();
		let delta = PublicationDelta {
			records: BTreeSet::new(),
			global_slots: BTreeSet::new(),
			overlays: BTreeSet::new(),
			incidence: BTreeSet::from([(graph, owner)]),
			memberships: BTreeSet::new(),
			point_graph_only: true,
		};
		assert_eq!(
			stable_parent(&before, &before, &delta, graph),
			family == GraphKind::Metagraph,
			"{family:?}"
		);
	}
}

/// Internal ownership excludes both containment families despite stable layout.
#[test]
fn internal_containment_parents_remain_excluded() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		let fixture = fixture(family);
		let mut before = fixture.before.clone();
		let journal = delta(&fixture);
		let root = before.graph(fixture.parent).assert_some();
		assert!(stable_graph(fixture.parent, root, root, &journal));
		let schema = before.schema.graph(fixture.parent).assert_some();
		let label = *schema
			.system_indexes()
			.find(|item| item.kind() == crate::datastore::SystemIndexKind::Label)
			.assert_some();
		let adjacency = *schema
			.system_indexes()
			.find(|item| item.kind() == crate::datastore::SystemIndexKind::Adjacency)
			.assert_some();
		let internal = crate::datastore::GraphSchema::new(
			schema.version(),
			schema.reference().clone(),
			label,
			adjacency,
			schema.properties().clone(),
			schema.topology_constraints(),
			schema.root().cloned(),
			Some(fixture.graphs[4]),
			0,
		);
		Arc::make_mut(&mut before.schema).graphs.insert(fixture.parent, Arc::new(internal));
		assert!(before.schema.graph(fixture.parent).assert_some().is_internal());
		assert!(!stable_parent(&before, &before, &journal, fixture.parent), "{family:?}");
	}
}

/// Missing roots, graph counts and schema owners cannot enter retained
/// publication.
#[test]
fn parent_metadata_and_root_identity_drift_remain_excluded() {
	for family in [GraphKind::Graph, GraphKind::Metagraph] {
		for fault in 0..6 {
			let fixture = fixture(family);
			let mut before = fixture.before.clone();
			let mut after = rebound(&fixture);
			let journal = delta(&fixture);
			assert!(stable_graph(
				fixture.parent,
				before.graph(fixture.parent).assert_some(),
				after.graph(fixture.parent).assert_some(),
				&journal
			));
			if family == GraphKind::Metagraph {
				assert!(stable_parent(&before, &after, &journal, fixture.parent));
			}
			match fault {
				0 => {
					Arc::make_mut(&mut before.schema).graphs.remove(&fixture.parent);
				}
				1 => {
					Arc::make_mut(&mut after.schema).graphs.remove(&fixture.parent);
				}
				2 => {
					before.graphs.remove(&fixture.parent);
				}
				3 => {
					after.graphs.remove(&fixture.parent);
				}
				4 => {
					after.graphs.remove(&fixture.graphs[4]);
				}
				_ => {
					assert_eq!(fault, 5);
					let schema =
						after.schema.graphs.get(&fixture.parent).assert_some().as_ref().clone();
					Arc::make_mut(&mut after.schema)
						.graphs
						.insert(fixture.parent, Arc::new(schema));
				}
			}
			assert!(
				!stable_parent(&before, &after, &journal, fixture.parent),
				"{family:?} fault={fault}"
			);
		}
	}
}
