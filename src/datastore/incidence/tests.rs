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

#[test]
fn qualified_incidence_sdk_rejects_missing_owners_and_terminals_before_mutation()
-> FerrosGrynnResult<()> {
	let (store, [parent, _child, _leaf], [owner, inner, terminal]) = fixture()?;
	let absent = AtomId::from_parts(AtomKind::Node, 999, 0)?;
	for (scope, terminal) in [
		(vec![owner, absent], terminal),
		(vec![owner, inner], absent),
		(vec![owner, inner, terminal], terminal),
		(vec![owner; 65], terminal),
	] {
		let mut write = store.begin();
		let endpoint = EdgeEndpoint::Nested(QualifiedAtomId::new(scope, terminal)?);
		assert!(
			write
				.attach_edge(
					parent.id(),
					&"link:connector".parse()?,
					None,
					EdgeTopology::directed([endpoint], [EdgeEndpoint::Local(owner)]),
					None,
					None
				)
				.is_err()
		);
		assert!(write.membership(parent.id(), &"link:connector".parse()?)?.is_none());
	}
	Ok(())
}

#[test]
fn qualified_incidence_snapshot_prerequisite_rejects_owner_rebinding() -> FerrosGrynnResult<()> {
	let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture()?;
	let mut private = store.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	private.attach_edge(
		parent.id(),
		&"link:connector".parse()?,
		None,
		EdgeTopology::directed(
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?)],
			[EdgeEndpoint::Local(owner)],
		),
		None,
		None,
	)?;
	let mut rebind = store.begin();
	rebind.replace_occurrence_state(
		child.id(),
		&"item:inner".parse()?,
		crate::datastore::OccurrenceState::Node {
			nested_graph: None,
		},
	)?;
	rebind.commit()?;
	assert!(private.commit().is_err());
	assert!(store.begin().membership(parent.id(), &"link:connector".parse()?)?.is_none());
	Ok(())
}

#[test]
fn qualified_incidence_final_candidate_rejects_cross_graph_dangling_routes() -> FerrosGrynnResult<()>
{
	for rebind in [false, true] {
		let (store, [parent, child, leaf], [owner, inner, terminal]) = fixture()?;
		let mut seed = store.begin();
		seed.attach_edge(
			parent.id(),
			&"link:connector".parse()?,
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)?;
		seed.commit()?;
		let mut remove = store.begin();
		if rebind {
			remove.replace_occurrence_state(
				child.id(),
				&"item:inner".parse()?,
				crate::datastore::OccurrenceState::Node {
					nested_graph: None,
				},
			)?;
		} else {
			remove.detach_occurrence(leaf.id(), &"item:leaf".parse()?, false)?;
		}
		assert!(remove.commit().is_err());
		assert!(store.begin().membership(leaf.id(), &"item:leaf".parse()?)?.is_some());
		assert!(store.begin().membership(parent.id(), &"link:connector".parse()?)?.is_some());
	}
	Ok(())
}

#[test]
fn qualified_incidence_replay_remaps_each_owner_and_terminal_arena() -> FerrosGrynnResult<()> {
	let (store, [parent, child, leaf], [owner, _inner, _terminal]) = fixture()?;
	let mut private = store.begin_with(TransactionOptions::new(
		IsolationLevel::Snapshot,
		TransactionAccess::ReadWrite,
	));
	for record in ["item:private_owner", "item:private_leaf"] {
		private.create_atom(record.parse()?, PropertyMap::new())?;
	}
	let inner =
		private.attach_node(child.id(), &"item:private_owner".parse()?, None, Some(leaf.id()))?;
	let terminal = private.attach_node(leaf.id(), &"item:private_leaf".parse()?, None, None)?;
	let endpoint = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?);
	private.attach_edge(
		parent.id(),
		&"link:connector".parse()?,
		None,
		EdgeTopology::directed([endpoint], [EdgeEndpoint::Local(owner)]),
		None,
		None,
	)?;
	let mut concurrent = store.begin();
	for record in ["item:rival_owner", "item:rival_leaf"] {
		concurrent.create_atom(record.parse()?, PropertyMap::new())?;
	}
	assert_eq!(
		concurrent.attach_node(child.id(), &"item:rival_owner".parse()?, None, Some(leaf.id()))?,
		inner
	);
	assert_eq!(
		concurrent.attach_node(leaf.id(), &"item:rival_leaf".parse()?, None, None)?,
		terminal
	);
	concurrent.commit()?;
	private.commit()?;
	let root = store.inner.current_root();
	let edge = root
		.resolve_atom(&"link:connector".parse()?)
		.and_then(|global| root.membership(parent.id(), global))
		.and_then(|local| root.occurrence(parent.id(), local))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing connector"))?;
	let crate::datastore::OccurrenceState::Edge {
		topology,
		..
	} = edge.state()
	else {
		return Err(FerrosGrynnError::invalid_argument("connector is not an edge"));
	};
	let (graph, occurrence) =
		root.resolve_incidence_endpoint(parent.id(), &topology.source()[0], |_graph, _owner| {
			Ok(())
		})?;
	assert_eq!(graph, leaf.id());
	assert_eq!(
		root.atom(occurrence.global_id()).map(|atom| atom.record_id().to_string()),
		Some("item:private_leaf".into())
	);
	let EdgeEndpoint::Nested(qualified) = &topology.source()[0] else {
		return Err(FerrosGrynnError::invalid_argument("qualified scope was lost"));
	};
	assert_ne!(qualified.scope()[1], inner);
	assert_ne!(qualified.atom(), terminal);
	Ok(())
}

#[test]
fn qualified_incidence_rebinding_requires_an_explicit_connector_replacement()
-> FerrosGrynnResult<()> {
	for repair in [false, true] {
		let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture()?;
		let mut seed = store.begin();
		let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
		seed.create_atom("item:other".parse()?, PropertyMap::new())?;
		let other = seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?;
		assert_eq!(other, terminal, "different arenas deliberately collide");
		seed.attach_edge(
			parent.id(),
			&"link:connector".parse()?,
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)?;
		seed.commit()?;
		let mut write = store.begin();
		write.replace_occurrence_state(
			child.id(),
			&"item:inner".parse()?,
			crate::datastore::OccurrenceState::Node {
				nested_graph: Some(replacement.id()),
			},
		)?;
		if repair {
			write.detach_occurrence(parent.id(), &"link:connector".parse()?, false)?;
			write.attach_edge(
				parent.id(),
				&"link:connector".parse()?,
				None,
				EdgeTopology::directed(
					[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], other)?)],
					[EdgeEndpoint::Local(owner)],
				),
				None,
				None,
			)?;
		}
		assert_eq!(write.commit().is_ok(), repair);
	}
	Ok(())
}

#[test]
fn qualified_incidence_build_overwrite_rejects_dangling_and_aliased_terminals()
-> FerrosGrynnResult<()> {
	for empty in [false, true] {
		let (store, [parent, _child, leaf], [owner, inner, terminal]) = fixture()?;
		let mut seed = store.begin();
		let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
		seed.create_atom("item:other".parse()?, PropertyMap::new())?;
		seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?;
		seed.attach_edge(
			parent.id(),
			&"link:connector".parse()?,
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?)],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)?;
		seed.commit()?;
		let query = if empty {
			"BUILD GRAPH OVERWRITE <GRAPH> leaf FROM [];"
		} else {
			"BUILD GRAPH OVERWRITE <GRAPH> leaf FROM (MATCH (item:other AS p) IN GRAPH replacement \
			 RETURN p) NODES p;"
		};
		let results = store.execute(query, &crate::values::Object::new())?;
		assert!(results[0].result.is_err(), "{results:?}");
		assert!(store.begin().membership(leaf.id(), &"item:leaf".parse()?)?.is_some());
		assert!(store.begin().membership(leaf.id(), &"item:other".parse()?)?.is_none());
	}
	Ok(())
}

#[test]
fn qualified_incidence_pending_connector_keeps_its_selected_logical_terminal()
-> FerrosGrynnResult<()> {
	let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture()?;
	let mut seed = store.begin();
	let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
	seed.create_atom("item:other".parse()?, PropertyMap::new())?;
	assert_eq!(seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?, terminal);
	seed.commit()?;
	let mut write = store.begin();
	write.attach_edge(
		parent.id(),
		&"link:connector".parse()?,
		None,
		EdgeTopology::directed(
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, inner], terminal)?)],
			[EdgeEndpoint::Local(owner)],
		),
		None,
		None,
	)?;
	write.replace_occurrence_state(
		child.id(),
		&"item:inner".parse()?,
		crate::datastore::OccurrenceState::Node {
			nested_graph: Some(replacement.id()),
		},
	)?;
	assert!(
		write.commit().is_err(),
		"a new connector must also keep the accepted logical endpoint"
	);
	Ok(())
}

#[test]
fn qualified_incidence_pending_import_anchors_survive_private_seals() -> FerrosGrynnResult<()> {
	for rebind in [false, true] {
		let (store, [parent, child, _leaf], [owner, inner, terminal]) = fixture()?;
		let mut seed = store.begin();
		let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
		seed.create_atom("item:other".parse()?, PropertyMap::new())?;
		seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?;
		seed.commit()?;
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
							crate::datastore::OccurrenceState::Node {
								nested_graph: Some(replacement.id()),
							},
						)
					})?;
				}
				Ok(())
			},
		);
		assert_eq!(result.is_ok(), !rebind);
		assert_eq!(
			store.begin().membership(parent.id(), &"link:connector".parse()?)?.is_some(),
			!rebind
		);
	}
	Ok(())
}

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
		crate::datastore::OccurrenceState::Node {
			nested_graph: Some(replacement.id()),
		},
	)?;
	write.replace_occurrence_state(
		parent.id(),
		&"link:connector".parse()?,
		crate::datastore::OccurrenceState::Edge {
			topology: EdgeTopology::directed([owner], [owner]),
			weight: None,
			transition_graph: None,
		},
	)?;
	write.replace_occurrence_state(
		parent.id(),
		&"link:connector".parse()?,
		crate::datastore::OccurrenceState::Edge {
			topology,
			weight: None,
			transition_graph: None,
		},
	)?;
	write.commit()?;
	Ok(())
}

#[test]
fn qualified_incidence_new_build_connectors_keep_their_selected_terminal() -> FerrosGrynnResult<()>
{
	let (store, [parent, _child, _leaf], [owner, inner, _terminal]) = fixture()?;
	let mut seed = store.begin();
	let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
	seed.create_atom("item:other".parse()?, PropertyMap::new())?;
	let other = seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?;
	assert_eq!(other, inner);
	seed.attach_edge(
		parent.id(),
		&"link:connector".parse()?,
		None,
		EdgeTopology::directed(
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner], inner)?)],
			[EdgeEndpoint::Local(owner)],
		),
		None,
		None,
	)?;
	seed.commit().map_err(|error| {
		FerrosGrynnError::invalid_argument("qualified BUILD seed commit").with_cause(error)
	})?;
	let mut write = store.begin();
	for result in write.execute(
		"BUILD GRAPH <META> copied FROM $graph;",
		&crate::values::Object::from_iter([(
			"graph",
			crate::values::Value::GraphRef(Box::new(parent)),
		)]),
	)? {
		result.result.map_err(|error| {
			FerrosGrynnError::invalid_argument("qualified BUILD execution").with_cause(error)
		})?;
	}
	let copied = write
		.named_graph("copied")?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing copied graph"))?;
	write.replace_occurrence_state(
		copied.id(),
		&"item:owner".parse()?,
		crate::datastore::OccurrenceState::Node {
			nested_graph: Some(replacement.id()),
		},
	)?;
	assert!(write.commit().is_err(), "BUILD must capture its new qualified connectors");
	assert!(store.begin().named_graph("copied")?.is_none());
	Ok(())
}
