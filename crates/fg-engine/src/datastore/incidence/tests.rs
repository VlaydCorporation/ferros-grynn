//! SDK qualified incidence validation and commit-local allocation remapping.

use crate::{
	datastore::{ClassSchema, Datastore, IsolationLevel, TransactionAccess, TransactionOptions},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomId, AtomKind, EdgeEndpoint, EdgeTopology, GraphKind, QualifiedAtomId},
	values::{GraphRef, PropertyMap},
};

/// Independent child and leaf arenas expose collisions in every scope level.
fn fixture() -> FerrosGrynnResult<(Datastore, [GraphRef; 3], [AtomId; 3])> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let parent = seed.define_graph("parent", GraphKind::Metagraph)?;
	let child = seed.define_graph("child", GraphKind::Metagraph)?;
	let leaf = seed.define_graph("leaf", GraphKind::Graph)?;
	for record in ["item:owner", "item:inner", "item:leaf", "link:connector"] {
		seed.create_atom(record.parse()?, PropertyMap::new())?;
	}
	let owner = seed.attach_node(parent.id(), &"item:owner".parse()?, None, Some(child.id()))?;
	let inner = seed.attach_node(child.id(), &"item:inner".parse()?, None, Some(leaf.id()))?;
	let terminal = seed.attach_node(leaf.id(), &"item:leaf".parse()?, None, None)?;
	seed.commit()?;
	Ok((store, [parent, child, leaf], [owner, inner, terminal]))
}

/// Verifies qualified incidence sdk rejects missing owners and terminals before
/// mutation.
#[test]
fn qualified_incidence_sdk_rejects_missing_owners_and_terminals_before_mutation() {
	use crate::test_assertions::AssertResult as _;

	let (store, [parent, _child, _leaf], [owner, inner, terminal]) = fixture().assert_ok();
	let absent = AtomId::from_parts(AtomKind::Node, 999, 0).assert_ok();
	for (scope, terminal) in [
		(vec![owner, absent], terminal),
		(vec![owner, inner], absent),
		(vec![owner, inner, terminal], terminal),
		(vec![owner; 65], terminal),
	] {
		let mut write = store.begin();
		let endpoint = EdgeEndpoint::Nested(QualifiedAtomId::new(scope, terminal).assert_ok());
		assert!(
			write
				.attach_edge(
					parent.id(),
					&"link:connector".parse().assert_ok(),
					None,
					EdgeTopology::directed([endpoint], [EdgeEndpoint::Local(owner)]),
					None,
					None
				)
				.is_err()
		);
		assert!(
			write
				.membership(parent.id(), &"link:connector".parse().assert_ok())
				.assert_ok()
				.is_none()
		);
	}
}

/// Verifies qualified incidence snapshot prerequisite rejects owner rebinding.
#[test]
fn qualified_incidence_snapshot_prerequisite_rejects_owner_rebinding() {
	use crate::test_assertions::AssertResult as _;

	let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture().assert_ok();
	let mut private = store.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	private
		.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(
					QualifiedAtomId::new(vec![owner, inner], terminal).assert_ok(),
				)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)
		.assert_ok();
	let mut rebind = store.begin();
	rebind
		.replace_occurrence_state(
			child.id(),
			&"item:inner".parse().assert_ok(),
			crate::datastore::OccurrenceState::node(None),
		)
		.assert_ok();
	rebind.commit().assert_ok();
	assert!(private.commit().is_err());
	assert!(
		store
			.begin()
			.membership(parent.id(), &"link:connector".parse().assert_ok())
			.assert_ok()
			.is_none()
	);
}

/// Verifies qualified incidence final candidate rejects cross graph dangling
/// routes.
#[test]
fn qualified_incidence_final_candidate_rejects_cross_graph_dangling_routes() {
	use crate::test_assertions::AssertResult as _;

	for rebind in [false, true] {
		let (store, [parent, child, leaf], [owner, inner, terminal]) = fixture().assert_ok();
		let mut seed = store.begin();
		seed.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(
					QualifiedAtomId::new(vec![owner, inner], terminal).assert_ok(),
				)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)
		.assert_ok();
		seed.commit().assert_ok();
		let mut remove = store.begin();
		if rebind {
			remove
				.replace_occurrence_state(
					child.id(),
					&"item:inner".parse().assert_ok(),
					crate::datastore::OccurrenceState::node(None),
				)
				.assert_ok();
		} else {
			remove
				.detach_occurrence(leaf.id(), &"item:leaf".parse().assert_ok(), false)
				.assert_ok();
		}
		assert!(remove.commit().is_err());
		assert!(
			store
				.begin()
				.membership(leaf.id(), &"item:leaf".parse().assert_ok())
				.assert_ok()
				.is_some()
		);
		assert!(
			store
				.begin()
				.membership(parent.id(), &"link:connector".parse().assert_ok())
				.assert_ok()
				.is_some()
		);
	}
}

/// Verifies qualified incidence replay remaps each owner and terminal arena.
#[test]
fn qualified_incidence_replay_remaps_each_owner_and_terminal_arena() {
	use crate::test_assertions::AssertResult as _;

	let (store, [parent, child, leaf], [owner, _inner, _terminal]) = fixture().assert_ok();
	let mut private = store.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	for record in ["item:private_owner", "item:private_leaf"] {
		private.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	let inner = private
		.attach_node(child.id(), &"item:private_owner".parse().assert_ok(), None, Some(leaf.id()))
		.assert_ok();
	let terminal = private
		.attach_node(leaf.id(), &"item:private_leaf".parse().assert_ok(), None, None)
		.assert_ok();
	let endpoint =
		EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal).assert_ok());
	private
		.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed([endpoint], [EdgeEndpoint::Local(owner)]),
			None,
			None,
		)
		.assert_ok();
	let mut concurrent = store.begin();
	for record in ["item:rival_owner", "item:rival_leaf"] {
		concurrent.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	assert_eq!(
		concurrent
			.attach_node(child.id(), &"item:rival_owner".parse().assert_ok(), None, Some(leaf.id()))
			.assert_ok(),
		inner
	);
	assert_eq!(
		concurrent
			.attach_node(leaf.id(), &"item:rival_leaf".parse().assert_ok(), None, None)
			.assert_ok(),
		terminal
	);
	concurrent.commit().assert_ok();
	private.commit().assert_ok();
	let root = store.inner.current_root();
	let edge = root
		.resolve_atom(&"link:connector".parse().assert_ok())
		.and_then(|global| root.membership(parent.id(), global))
		.and_then(|local| root.occurrence(parent.id(), local))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing connector"))
		.assert_ok();
	let crate::datastore::OccurrenceStateRef::Edge {
		topology,
		..
	} = edge.state().view()
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("connector is not an edge"));
			test_result.assert_ok();
		};
	};
	let (graph, occurrence) = root
		.resolve_incidence_endpoint(parent.id(), &topology.source()[0], |_graph, _owner| Ok(()))
		.assert_ok();
	assert_eq!(graph, leaf.id());
	assert_eq!(
		root.atom(occurrence.global_id()).map(|atom| atom.record_id().to_string()),
		Some("item:private_leaf".into())
	);
	let EdgeEndpoint::Nested(qualified) = &topology.source()[0] else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("qualified scope was lost"));
			test_result.assert_ok();
		};
	};
	assert_ne!(qualified.scope()[1], inner);
	assert_ne!(qualified.atom(), terminal);
}

/// Verifies qualified incidence rebinding requires an explicit connector
/// replacement.
#[test]
fn qualified_incidence_rebinding_requires_an_explicit_connector_replacement() {
	use crate::test_assertions::AssertResult as _;

	for repair in [false, true] {
		let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture().assert_ok();
		let mut seed = store.begin();
		let replacement = seed.define_graph("replacement", GraphKind::Graph).assert_ok();
		seed.create_atom("item:other".parse().assert_ok(), PropertyMap::new()).assert_ok();
		let other = seed
			.attach_node(replacement.id(), &"item:other".parse().assert_ok(), None, None)
			.assert_ok();
		assert_eq!(other, terminal, "different arenas deliberately collide");
		seed.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(
					QualifiedAtomId::new(vec![owner, inner], terminal).assert_ok(),
				)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)
		.assert_ok();
		seed.commit().assert_ok();
		let mut write = store.begin();
		write
			.replace_occurrence_state(
				child.id(),
				&"item:inner".parse().assert_ok(),
				crate::datastore::OccurrenceState::node(Some(replacement.id())),
			)
			.assert_ok();
		if repair {
			write
				.detach_occurrence(parent.id(), &"link:connector".parse().assert_ok(), false)
				.assert_ok();
			write
				.attach_edge(
					parent.id(),
					&"link:connector".parse().assert_ok(),
					None,
					EdgeTopology::directed(
						[EdgeEndpoint::Nested(
							QualifiedAtomId::new(vec![owner, inner], other).assert_ok(),
						)],
						[EdgeEndpoint::Local(owner)],
					),
					None,
					None,
				)
				.assert_ok();
		}
		assert_eq!(write.commit().is_ok(), repair);
	}
}

/// Verifies qualified incidence build overwrite rejects dangling and aliased
/// terminals.
#[test]
fn qualified_incidence_build_overwrite_rejects_dangling_and_aliased_terminals() {
	use crate::test_assertions::AssertResult as _;

	for empty in [false, true] {
		let (store, [parent, _child, leaf], [owner, inner, terminal]) = fixture().assert_ok();
		let mut seed = store.begin();
		let replacement = seed.define_graph("replacement", GraphKind::Graph).assert_ok();
		seed.create_atom("item:other".parse().assert_ok(), PropertyMap::new()).assert_ok();
		seed.attach_node(replacement.id(), &"item:other".parse().assert_ok(), None, None)
			.assert_ok();
		seed.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(
					QualifiedAtomId::new(vec![owner, inner], terminal).assert_ok(),
				)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)
		.assert_ok();
		seed.commit().assert_ok();
		let query = if empty {
			"BUILD GRAPH OVERWRITE <GRAPH> leaf FROM [];"
		} else {
			"BUILD GRAPH OVERWRITE <GRAPH> leaf FROM (MATCH (item:other AS p) IN GRAPH replacement \
			 RETURN p) NODES p;"
		};
		let results = store.execute(query, &crate::values::Object::new()).assert_ok();
		assert!(results[0].result.is_err(), "{results:?}");
		assert!(
			store
				.begin()
				.membership(leaf.id(), &"item:leaf".parse().assert_ok())
				.assert_ok()
				.is_some()
		);
		assert!(
			store
				.begin()
				.membership(leaf.id(), &"item:other".parse().assert_ok())
				.assert_ok()
				.is_none()
		);
	}
}

/// Verifies qualified incidence pending connector keeps its selected logical
/// terminal.
#[test]
fn qualified_incidence_pending_connector_keeps_its_selected_logical_terminal() {
	use crate::test_assertions::AssertResult as _;

	let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture().assert_ok();
	let mut seed = store.begin();
	let replacement = seed.define_graph("replacement", GraphKind::Graph).assert_ok();
	seed.create_atom("item:other".parse().assert_ok(), PropertyMap::new()).assert_ok();
	assert_eq!(
		seed.attach_node(replacement.id(), &"item:other".parse().assert_ok(), None, None)
			.assert_ok(),
		terminal
	);
	seed.commit().assert_ok();
	let mut write = store.begin();
	write
		.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(
					QualifiedAtomId::new(vec![owner, inner], terminal).assert_ok(),
				)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)
		.assert_ok();
	write
		.replace_occurrence_state(
			child.id(),
			&"item:inner".parse().assert_ok(),
			crate::datastore::OccurrenceState::node(Some(replacement.id())),
		)
		.assert_ok();
	assert!(
		write.commit().is_err(),
		"a new connector must also keep the accepted logical endpoint"
	);
}

/// Verifies qualified incidence pending import anchors survive private seals.
#[test]
fn qualified_incidence_pending_import_anchors_survive_private_seals() {
	use crate::test_assertions::AssertResult as _;

	for rebind in [false, true] {
		let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture().assert_ok();
		let mut seed = store.begin();
		let replacement = seed.define_graph("replacement", GraphKind::Graph).assert_ok();
		seed.create_atom("item:other".parse().assert_ok(), PropertyMap::new()).assert_ok();
		seed.attach_node(replacement.id(), &"item:other".parse().assert_ok(), None, None)
			.assert_ok();
		seed.commit().assert_ok();
		let result = store.import(
			crate::datastore::ImportOptions {
				segment_size: 1,
				..crate::datastore::ImportOptions::default()
			},
			|session| {
				session.apply_item(|tx| {
					tx.attach_edge(
						parent.id(),
						&"link:connector".parse()?,
						None,
						EdgeTopology::directed(
							[EdgeEndpoint::Nested(QualifiedAtomId::new(
								vec![owner, inner],
								terminal,
							)?)],
							[EdgeEndpoint::Local(owner)],
						),
						None,
						None,
					)
				})?;
				if rebind {
					session.apply_item(|tx| {
						tx.replace_occurrence_state(
							child.id(),
							&"item:inner".parse()?,
							crate::datastore::OccurrenceState::node(Some(replacement.id())),
						)
					})?;
				}
				Ok(())
			},
		);
		assert_eq!(result.is_ok(), !rebind);
		assert_eq!(
			store
				.begin()
				.membership(parent.id(), &"link:connector".parse().assert_ok())
				.assert_ok()
				.is_some(),
			!rebind
		);
	}
}

/// Verifies qualified incidence explicit topology repair can return to the same
/// wire image.
#[test]
fn qualified_incidence_explicit_topology_repair_can_return_to_the_same_wire_image()
-> FerrosGrynnResult<()> {
	let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture()?;
	let mut seed = store.begin();
	let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
	seed.create_atom("item:other".parse()?, PropertyMap::new())?;
	seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?;
	let topology = EdgeTopology::directed(
		[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?)],
		[EdgeEndpoint::Local(owner)],
	);
	seed.attach_edge(parent.id(), &"link:connector".parse()?, None, topology.clone(), None, None)?;
	seed.commit()?;
	let mut write = store.begin();
	write.replace_occurrence_state(
		child.id(),
		&"item:inner".parse()?,
		crate::datastore::OccurrenceState::node(Some(replacement.id())),
	)?;
	write.replace_occurrence_state(
		parent.id(),
		&"link:connector".parse()?,
		crate::datastore::OccurrenceState::edge(
			EdgeTopology::directed([owner], [owner]),
			None,
			None,
		),
	)?;
	write.replace_occurrence_state(
		parent.id(),
		&"link:connector".parse()?,
		crate::datastore::OccurrenceState::edge(topology, None, None),
	)?;
	write.commit()?;
	Ok(())
}

/// Verifies qualified incidence new build connectors keep their selected
/// terminal.
#[test]
fn qualified_incidence_new_build_connectors_keep_their_selected_terminal() {
	use crate::test_assertions::AssertResult as _;

	let (store, [parent, _child, _leaf], [owner, inner, _terminal]) = fixture().assert_ok();
	let mut seed = store.begin();
	let replacement = seed.define_graph("replacement", GraphKind::Graph).assert_ok();
	seed.create_atom("item:other".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let other = seed
		.attach_node(replacement.id(), &"item:other".parse().assert_ok(), None, None)
		.assert_ok();
	assert_eq!(other, inner);
	seed.attach_edge(
		parent.id(),
		&"link:connector".parse().assert_ok(),
		None,
		EdgeTopology::directed(
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner], inner).assert_ok())],
			[EdgeEndpoint::Local(owner)],
		),
		None,
		None,
	)
	.assert_ok();
	seed.commit()
		.map_err(|error| {
			FerrosGrynnError::invalid_argument("qualified BUILD seed commit").with_cause(error)
		})
		.assert_ok();
	let mut write = store.begin();
	for result in write
		.execute(
			"BUILD GRAPH <META> copied FROM $graph;",
			&crate::values::Object::from_iter([(
				"graph",
				crate::values::Value::GraphRef(Box::new(parent)),
			)]),
		)
		.assert_ok()
	{
		result
			.result
			.map_err(|error| {
				FerrosGrynnError::invalid_argument("qualified BUILD execution").with_cause(error)
			})
			.assert_ok();
	}
	let copied = write
		.named_graph("copied")
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing copied graph"))
		.assert_ok();
	write
		.replace_occurrence_state(
			copied.id(),
			&"item:owner".parse().assert_ok(),
			crate::datastore::OccurrenceState::node(Some(replacement.id())),
		)
		.assert_ok();
	assert!(write.commit().is_err(), "BUILD must capture its new qualified connectors");
	assert!(store.begin().named_graph("copied").assert_ok().is_none());
}
