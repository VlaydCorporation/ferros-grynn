//! Exact trust-boundary regressions use valid roots and deliberately false
//! bindings.

use std::sync::Arc;

use super::{super::arena::GraphArena, *};
use crate::{
	datastore::{ClassSchema, Datastore},
	graph::{AtomKind, EdgeTopology, GraphKind},
	test_assertions::{AssertOption as _, AssertResult as _},
	values::PropertyMap,
};

/// A foreign terminal deliberately collides with the legitimate local address.
struct Fixture {
	/// Fully validated immutable runtime state.
	database: Arc<DatabaseRoot>,
	/// Root owning both a real and a plain container candidate.
	graph: GraphId,
	/// Actual child selected by the live container.
	child: GraphId,
	/// Another graph also has terminal NODE slot zero/generation zero.
	foreign: GraphId,
	/// Live NODE with a child graph.
	owner: AtomId,
	/// Live NODE without any child graph.
	plain: AtomId,
	/// Full terminal identity in the actual child.
	leaf: AtomId,
}

/// Matching terminal slots make a wrong graph visible as an acceptance defect.
fn fixture() -> Fixture {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let parent = seed.define_graph("parent", GraphKind::Metagraph).assert_ok().id();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok().id();
	let foreign = seed.define_graph("foreign", GraphKind::Graph).assert_ok().id();
	for record in ["item:owner", "item:plain", "item:leaf", "item:foreign"] {
		seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	let owner =
		seed.attach_node(parent, &"item:owner".parse().assert_ok(), None, Some(child)).assert_ok();
	let plain = seed.attach_node(parent, &"item:plain".parse().assert_ok(), None, None).assert_ok();
	let leaf = seed.attach_node(child, &"item:leaf".parse().assert_ok(), None, None).assert_ok();
	let collision =
		seed.attach_node(foreign, &"item:foreign".parse().assert_ok(), None, None).assert_ok();
	assert_eq!(leaf, collision);
	seed.commit().assert_ok();
	Fixture {
		database: store.inner.current_root(),
		graph: parent,
		child,
		foreign,
		owner,
		plain,
		leaf,
	}
}

/// A plausible binding still needs the runtime parent checked before
/// conversion.
fn assert_invalid(fixture: &Fixture, owner: AtomId, slot: u32, child: GraphId) {
	let qualified = QualifiedAtomId::new([owner], fixture.leaf).assert_ok();
	let result = resolve_qualified(
		&fixture.database,
		fixture.graph,
		&qualified,
		|_parent, _container, _prefix| Ok((slot, child)),
	);
	assert!(result.is_err(), "QUALIFIED_ROUTE_ACCEPTED_INVALID_PARENT");
}

/// A second graph with the same terminal address must not impersonate the
/// child.
#[test]
fn qualified_route_rejects_wrong_child_with_colliding_terminal() {
	let fixture = fixture();
	assert_invalid(&fixture, fixture.owner, 1, fixture.foreign);
}

/// A stale container generation cannot borrow the current owner's child.
#[test]
fn qualified_route_rejects_stale_container_generation() {
	let fixture = fixture();
	let stale =
		AtomId::from_parts(AtomKind::Node, fixture.owner.slot(), fixture.owner.generation() + 1)
			.assert_ok();
	assert_invalid(&fixture, stale, 1, fixture.child);
}

/// A live plain atom cannot acquire a child from the binding callback alone.
#[test]
fn qualified_route_rejects_plain_owner_without_child() {
	let fixture = fixture();
	assert_invalid(&fixture, fixture.plain, 1, fixture.child);
}

/// Slot zero denotes the root and cannot represent a nonempty child path.
#[test]
fn qualified_route_rejects_reserved_root_slot_for_child() {
	let fixture = fixture();
	assert_invalid(&fixture, fixture.owner, 0, fixture.child);
}

/// Real bulk children preserve valid resolution and the old terminal boundary.
#[test]
fn qualified_bulk_route_preserves_terminal_generation_checks() {
	let fixture = fixture();
	let arena = GraphArena::build(&fixture.database, fixture.graph).assert_ok();
	let qualified = QualifiedAtomId::new([fixture.owner], fixture.leaf).assert_ok();
	assert_eq!(
		arena.qualified(&qualified).assert_ok(),
		(1, DiskAtomRef::from_parts(AtomKind::Node, fixture.leaf.slot()).assert_ok())
	);
	let stale =
		AtomId::from_parts(AtomKind::Node, fixture.leaf.slot(), fixture.leaf.generation() + 1)
			.assert_ok();
	arena.qualified(&QualifiedAtomId::new([fixture.owner], stale).assert_ok()).assert_err();
	arena.qualified(&QualifiedAtomId::new([fixture.plain], fixture.leaf).assert_ok()).assert_err();
}

/// Full-generation occurrence lookup cannot replace reverse membership
/// validation.
#[test]
fn qualified_route_rejects_missing_owner_or_terminal_reverse_membership() {
	let fixture = fixture();
	let qualified = QualifiedAtomId::new([fixture.owner], fixture.leaf).assert_ok();
	for (graph, local) in [(fixture.graph, fixture.owner), (fixture.child, fixture.leaf)] {
		let mut database = fixture.database.as_ref().clone();
		let global = database.occurrence(graph, local).assert_some().global_id();
		Arc::make_mut(&mut database.atoms).remove_membership(global, graph);
		resolve_qualified(&database, fixture.graph, &qualified, |_parent, _container, _prefix| {
			Ok((1, fixture.child))
		})
		.assert_err();
	}
}

/// Two owners of one child keep distinct deep routes through a real EDGE
/// container.
#[test]
fn qualified_bulk_route_preserves_shared_child_paths_and_deep_edge_owner() {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let root = seed.define_graph("root", GraphKind::Metagraph).assert_ok().id();
	let middle = seed.define_graph("middle", GraphKind::Metagraph).assert_ok().id();
	let leaf = seed.define_graph("leaf", GraphKind::Graph).assert_ok().id();
	for name in ["item:a", "item:b", "item:middle", "item:leaf", "link:owner", "link:leaf"] {
		seed.create_atom(name.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	let owners = ["item:a", "item:b"].map(|name| {
		seed.attach_node(root, &name.parse().assert_ok(), None, Some(middle)).assert_ok()
	});
	let mid = seed.attach_node(middle, &"item:middle".parse().assert_ok(), None, None).assert_ok();
	let portal = seed
		.attach_edge(
			middle,
			&"link:owner".parse().assert_ok(),
			None,
			EdgeTopology::directed([mid], [mid]),
			None,
			Some(leaf),
		)
		.assert_ok();
	let node = seed.attach_node(leaf, &"item:leaf".parse().assert_ok(), None, None).assert_ok();
	let edge = seed
		.attach_edge(
			leaf,
			&"link:leaf".parse().assert_ok(),
			None,
			EdgeTopology::directed([node], [node]),
			None,
			None,
		)
		.assert_ok();
	seed.commit().assert_ok();
	let database = store.inner.current_root();
	let arena = GraphArena::build(&database, root).assert_ok();
	for terminal in [node, edge] {
		let results = owners.map(|owner| {
			arena
				.qualified(&QualifiedAtomId::new([owner, portal], terminal).assert_ok())
				.assert_ok()
		});
		assert_ne!(results[0].0, results[1].0);
		assert_eq!(results[0].1, results[1].1);
		for (owner, (slot, atom)) in owners.into_iter().zip(results) {
			assert_eq!(
				arena
					.bindings()
					.qualified(
						&database,
						root,
						&QualifiedAtomId::new([owner, portal], terminal).assert_ok()
					)
					.assert_ok(),
				(slot, atom)
			);
			assert_eq!(arena.bindings().graph_by_slot().get(&slot), Some(&leaf));
			assert_eq!(arena.bindings().scope_by_slot()[&slot].as_ref(), &[owner, portal]);
			assert_eq!(
				atom,
				DiskAtomRef::from_parts(terminal.kind().assert_some(), terminal.slot()).assert_ok()
			);
		}
	}
}

/// A selected scope must be unique and preserve its full accepted root/child
/// identity.
#[test]
fn qualified_point_route_rejects_missing_duplicate_wrong_and_foreign_bindings() {
	use super::super::GraphArenaBindings;
	let fixture = fixture();
	let accepted = GraphArena::build(&fixture.database, fixture.graph).assert_ok().bindings();
	let qualified = QualifiedAtomId::new([fixture.owner], fixture.leaf).assert_ok();
	assert_eq!(accepted.qualified(&fixture.database, fixture.graph, &qualified).assert_ok().0, 1);
	accepted.qualified(&fixture.database, fixture.foreign, &qualified).assert_err();
	let mut scopes = accepted.scope_by_slot().clone();
	scopes.remove(&1);
	GraphArenaBindings::new(accepted.graph_by_slot().clone(), scopes)
		.qualified(&fixture.database, fixture.graph, &qualified)
		.assert_err();
	let mut graphs = accepted.graph_by_slot().clone();
	graphs.insert(2, fixture.child);
	let mut scopes = accepted.scope_by_slot().clone();
	scopes.insert(2, vec![fixture.owner].into_boxed_slice());
	GraphArenaBindings::new(graphs, scopes)
		.qualified(&fixture.database, fixture.graph, &qualified)
		.assert_err();
	let mut graphs = accepted.graph_by_slot().clone();
	graphs.insert(1, fixture.foreign);
	GraphArenaBindings::new(graphs, accepted.scope_by_slot().clone())
		.qualified(&fixture.database, fixture.graph, &qualified)
		.assert_err();
	let mut graphs = accepted.graph_by_slot().clone();
	graphs.remove(&1);
	GraphArenaBindings::new(graphs, accepted.scope_by_slot().clone())
		.qualified(&fixture.database, fixture.graph, &qualified)
		.assert_err();
	let stale =
		AtomId::from_parts(AtomKind::Node, fixture.owner.slot(), fixture.owner.generation() + 1)
			.assert_ok();
	accepted
		.qualified(
			&fixture.database,
			fixture.graph,
			&QualifiedAtomId::new([stale], fixture.leaf).assert_ok(),
		)
		.assert_err();
}

/// Direct META bindings validate full owners without requiring a terminal atom.
#[test]
fn direct_child_route_rejects_bad_binding_and_membership() {
	use super::super::GraphArenaBindings;
	let fixture = fixture();
	let accepted = GraphArena::build(&fixture.database, fixture.graph).assert_ok().bindings();
	assert_eq!(
		accepted.direct_child_slot(&fixture.database, fixture.graph, fixture.owner).assert_ok(),
		1
	);
	for fault in 0..7 {
		let mut graphs = accepted.graph_by_slot().clone();
		let mut scopes = accepted.scope_by_slot().clone();
		match fault {
			0 => {
				scopes.remove(&1);
			}
			1 => {
				graphs.insert(2, fixture.child);
				scopes.insert(2, vec![fixture.owner].into_boxed_slice());
			}
			2 => {
				graphs.insert(1, fixture.foreign);
			}
			3 => {
				graphs.remove(&1);
			}
			4 => {
				graphs.insert(0, fixture.foreign);
			}
			5 => {
				scopes.insert(0, vec![fixture.owner].into_boxed_slice());
			}
			_ => {
				scopes.insert(0, vec![fixture.owner].into_boxed_slice());
				scopes.remove(&1);
			}
		}
		let error = GraphArenaBindings::new(graphs, scopes)
			.direct_child_slot(&fixture.database, fixture.graph, fixture.owner)
			.assert_err();
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::InvalidStorageFile { .. }
		));
	}
	let stale =
		AtomId::from_parts(AtomKind::Node, fixture.owner.slot(), fixture.owner.generation() + 1)
			.assert_ok();
	accepted.direct_child_slot(&fixture.database, fixture.graph, stale).assert_err();
	accepted.direct_child_slot(&fixture.database, fixture.foreign, fixture.owner).assert_err();
	let mut database = fixture.database.as_ref().clone();
	let global = database.occurrence(fixture.graph, fixture.owner).assert_some().global_id();
	Arc::make_mut(&mut database.atoms).remove_membership(global, fixture.graph);
	accepted.direct_child_slot(&database, fixture.graph, fixture.owner).assert_err();
}
