//! Combined qualified metadata and topology admission precedes source writes.

use crate::{
	datastore::{ClassSchema, Datastore, OccurrenceState},
	gql_status::FerrosGrynnError,
	graph::{AtomKind, EdgeEndpoint, EdgeTopology, GraphKind, QualifiedAtomId},
	values::PropertyMap,
};

/// Verifies qualified incidence replacement admits metadata and source as one
/// total.
#[test]
fn qualified_incidence_replacement_admits_metadata_and_source_as_one_total() {
	use crate::test_assertions::AssertResult as _;

	for edge_endpoint in [false, true] {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
		let parent = seed.define_graph("parent", GraphKind::Metagraph).assert_ok();
		let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
		for record in ["item:owner", "item:local", "item:leaf", "link:base", "link:connector"] {
			seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
		}
		let owner = seed
			.attach_node(parent.id(), &"item:owner".parse().assert_ok(), None, Some(child.id()))
			.assert_ok();
		let local = seed
			.attach_node(parent.id(), &"item:local".parse().assert_ok(), None, None)
			.assert_ok();
		let node =
			seed.attach_node(child.id(), &"item:leaf".parse().assert_ok(), None, None).assert_ok();
		let terminal = if edge_endpoint {
			seed.attach_edge(
				child.id(),
				&"link:base".parse().assert_ok(),
				None,
				EdgeTopology::directed([node], [node]),
				None,
				None,
			)
			.assert_ok()
		} else {
			node
		};
		let connector = seed
			.attach_edge(
				parent.id(),
				&"link:connector".parse().assert_ok(),
				None,
				EdgeTopology::directed([owner], [local]),
				None,
				None,
			)
			.assert_ok();
		seed.commit().assert_ok();
		let mut write = store.begin();
		let before = write
			.working
			.occurrence(parent.id(), connector)
			.map(|occurrence| occurrence.state().clone())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing connector"))
			.assert_ok();
		write.write_bytes = store.inner.transaction_limits().max_write_set_bytes
			- if edge_endpoint {
				1300
			} else {
				1250
			};
		let bytes = write.write_set_bytes();
		assert!(
			write
				.replace_occurrence_state(
					parent.id(),
					&"link:connector".parse().assert_ok(),
					OccurrenceState::edge(
						EdgeTopology::directed(
							[EdgeEndpoint::Nested(
								QualifiedAtomId::new(vec![owner], terminal).assert_ok()
							)],
							[EdgeEndpoint::Local(local)]
						),
						None,
						None
					)
				)
				.is_err()
		);
		assert_eq!(
			write.working.occurrence(parent.id(), connector).map(|occurrence| occurrence.state()),
			Some(&before)
		);
		assert!(
			write
				.working
				.schema
				.graph(parent.id())
				.and_then(|graph| graph.edge_endpoint_index())
				.is_none()
		);
		assert_eq!(write.write_set_bytes(), bytes);
	}
}

/// Verifies qualified incidence savepoint removes pending anchor authority.
#[test]
fn qualified_incidence_savepoint_removes_pending_anchor_authority() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let parent = seed.define_graph("parent", GraphKind::Metagraph).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	for record in ["item:owner", "item:leaf", "link:connector"] {
		seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	let owner = seed
		.attach_node(parent.id(), &"item:owner".parse().assert_ok(), None, Some(child.id()))
		.assert_ok();
	let terminal =
		seed.attach_node(child.id(), &"item:leaf".parse().assert_ok(), None, None).assert_ok();
	seed.commit().assert_ok();
	let mut write = store.begin();
	let saved = write.gql_savepoint();
	write
		.attach_edge(
			parent.id(),
			&"link:connector".parse().assert_ok(),
			None,
			EdgeTopology::directed(
				[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner], terminal).assert_ok())],
				[EdgeEndpoint::Local(owner)],
			),
			None,
			None,
		)
		.assert_ok();
	assert!(write.qualified_incidence.is_some());
	write.restore_gql_savepoint(saved);
	assert!(write.qualified_incidence.is_none());
	write
		.replace_occurrence_state(
			parent.id(),
			&"item:owner".parse().assert_ok(),
			OccurrenceState::node(None),
		)
		.assert_ok();
	write.commit().assert_ok();
	assert!(
		store
			.begin()
			.membership(parent.id(), &"link:connector".parse().assert_ok())
			.assert_ok()
			.is_none()
	);
}
