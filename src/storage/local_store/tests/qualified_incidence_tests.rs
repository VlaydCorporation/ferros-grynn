//! Cold local roots preserve qualified logical identities and reject aliasing.

use super::TestDirectory;
use crate::{
	datastore::{ClassSchema, Datastore, IoMode, LocalOptions},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, EdgeEndpoint, EdgeTopology, GraphKind, QualifiedAtomId},
	values::{Object, PropertyMap},
};

/// Backend parity includes a recovered external connector and BUILD overwrite.
fn reopen(mode: IoMode) -> FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions {
		io_mode: mode,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options)?;
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let parent = seed.define_graph("parent", GraphKind::Metagraph)?;
	let child = seed.define_graph("child", GraphKind::Graph)?;
	let replacement = seed.define_graph("replacement", GraphKind::Graph)?;
	for record in ["item:owner", "item:leaf", "item:other", "link:connector"] {
		seed.create_atom(record.parse()?, PropertyMap::new())?;
	}
	let owner = seed.attach_node(parent.id(), &"item:owner".parse()?, None, Some(child.id()))?;
	let terminal = seed.attach_node(child.id(), &"item:leaf".parse()?, None, None)?;
	seed.attach_node(replacement.id(), &"item:other".parse()?, None, None)?;
	seed.attach_edge(
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
	seed.commit()?;
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options)?;
	let mut remove = reopened.begin();
	remove.detach_occurrence(child.id(), &"item:leaf".parse()?, false)?;
	assert!(remove.commit().is_err());
	let result = reopened.execute(
		"BUILD GRAPH OVERWRITE <GRAPH> child FROM (MATCH (item:other AS p) IN GRAPH replacement \
		 RETURN p) NODES p;",
		&Object::new(),
	)?;
	assert!(result[0].result.is_err(), "{result:?}");
	assert!(reopened.begin().membership(child.id(), &"item:leaf".parse()?)?.is_some());
	assert!(reopened.begin().membership(child.id(), &"item:other".parse()?)?.is_none());
	drop(reopened);
	let recovered = Datastore::open_local(&directory.0, options)?;
	assert!(recovered.begin().membership(parent.id(), &"link:connector".parse()?)?.is_some());
	assert!(recovered.begin().membership(child.id(), &"item:leaf".parse()?)?.is_some());
	Ok(())
}

#[test]
fn qualified_incidence_local_sync_reopen_rejects_dangling_and_overwrite_alias()
-> FerrosGrynnResult<()> {
	reopen(IoMode::Sync)
}

#[cfg(feature = "native-async-io")]
#[test]
fn qualified_incidence_local_native_reopen_rejects_dangling_and_overwrite_alias()
-> FerrosGrynnResult<()> {
	reopen(IoMode::NativeAsync)
}
