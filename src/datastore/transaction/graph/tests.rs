//! Combined qualified metadata and topology admission precedes source writes.

use crate::{
	datastore::{ClassSchema, Datastore, OccurrenceState},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, EdgeEndpoint, EdgeTopology, GraphKind, QualifiedAtomId},
	values::PropertyMap,
};

#[test]
fn qualified_incidence_replacement_admits_metadata_and_source_as_one_total() -> FerrosGrynnResult<()>
{
	for edge_endpoint in [false, true] {
		let store = Datastore::in_memory();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
		let parent = seed.define_graph("parent", GraphKind::Metagraph)?;
		let child = seed.define_graph("child", GraphKind::Graph)?;
		for record in ["item:owner", "item:local", "item:leaf", "link:base", "link:connector"] {
			seed.create_atom(record.parse()?, PropertyMap::new())?;
		}
		let owner =
			seed.attach_node(parent.id(), &"item:owner".parse()?, None, Some(child.id()))?;
		let local = seed.attach_node(parent.id(), &"item:local".parse()?, None, None)?;
		let node = seed.attach_node(child.id(), &"item:leaf".parse()?, None, None)?;
		let terminal = if edge_endpoint {
			seed.attach_edge(
				child.id(),
				&"link:base".parse()?,
				None,
				EdgeTopology::directed([node], [node]),
				None,
				None,
			)?
		} else {
			node
		};
		let connector = seed.attach_edge(
			parent.id(),
			&"link:connector".parse()?,
			None,
			EdgeTopology::directed([owner], [local]),
			None,
			None,
		)?;
		seed.commit()?;
		let mut write = store.begin();
		let before = write
			.working
			.occurrence(parent.id(), connector)
			.map(|occurrence| occurrence.state().clone())
			.ok_or_else(|| FerrosGrynnError::invalid_argument("missing connector"))?;
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
					&"link:connector".parse()?,
					OccurrenceState::Edge {
						topology: EdgeTopology::directed(
							[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner], terminal)?)],
							[EdgeEndpoint::Local(local)]
						),
						weight: None,
						transition_graph: None,
					}
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
	Ok(())
}

#[test]
fn qualified_incidence_savepoint_removes_pending_anchor_authority() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let parent = seed.define_graph("parent", GraphKind::Metagraph)?;
	let child = seed.define_graph("child", GraphKind::Graph)?;
	for record in ["item:owner", "item:leaf", "link:connector"] {
		seed.create_atom(record.parse()?, PropertyMap::new())?;
	}
	let owner = seed.attach_node(parent.id(), &"item:owner".parse()?, None, Some(child.id()))?;
	let terminal = seed.attach_node(child.id(), &"item:leaf".parse()?, None, None)?;
	seed.commit()?;
	let mut write = store.begin();
	let saved = write.gql_savepoint();
	write.attach_edge(
		parent.id(),
		&"link:connector".parse()?,
		None,
		EdgeTopology::directed(
			[EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner], terminal)?)],
			[EdgeEndpoint::Local(owner)],
		),
		None,
		None,
	)?;
	assert!(write.qualified_incidence.is_some());
	write.restore_gql_savepoint(saved);
	assert!(write.qualified_incidence.is_none());
	write.replace_occurrence_state(parent.id(), &"item:owner".parse()?, OccurrenceState::Node {
		nested_graph: None,
	})?;
	write.commit()?;
	assert!(store.begin().membership(parent.id(), &"link:connector".parse()?)?.is_none());
	Ok(())
}
