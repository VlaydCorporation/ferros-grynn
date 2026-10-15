//! Qualified endpoint history setup used by local backend integration targets.

use ferros_grynn::{
	changefeed::{ChangefeedConfig, ChangefeedId, ChangefeedSource},
	datastore::{ClassSchema, Datastore},
	gql_status::FerrosGrynnResult,
	graph::{AtomKind, GraphId, GraphKind},
	values::{Duration, PropertyMap, RecordId},
};

/// Public inputs for a durable qualified endpoint history fixture.
pub struct QualifiedHistoryFixture {
	/// Parent graph owning the qualified route.
	pub parent: GraphId,
	/// Child graph containing the terminal endpoint.
	pub child: GraphId,
	/// Feed owned by the parent graph.
	pub feed: ChangefeedId,
	/// Exclusive starting commit.
	pub since: ferros_grynn::changefeed::Versionstamp,
	/// Nested edge endpoint identity.
	pub endpoint: RecordId,
	/// Edge owner of the nested child scope.
	pub owner: RecordId,
}

/// Creates real cross-level incidence through a metaedge, then captures one
/// parent connector independently of the local backend's page representation.
///
/// # Errors
/// Returns public schema, graph, incidence, changefeed or transaction failures.
pub fn qualified_history_fixture(store: &Datastore) -> FerrosGrynnResult<QualifiedHistoryFixture> {
	use ferros_grynn::graph::{EdgeEndpoint, EdgeTopology, QualifiedAtomId};
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("qualified_node", AtomKind::Node, [])?)?;
	tx.define_class(ClassSchema::new("qualified_edge", AtomKind::Edge, [])?)?;
	let parent = tx.define_graph("qualified_parent", GraphKind::Metagraph)?;
	let child = tx.define_graph("qualified_child", GraphKind::Graph)?;
	let records = [
		"qualified_node:a",
		"qualified_node:b",
		"qualified_edge:base",
		"qualified_edge:owner",
		"qualified_edge:cross",
	]
	.into_iter()
	.map(str::parse::<RecordId>)
	.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new())?;
	}
	let a = tx.attach_node(child.id(), &records[0], None, None)?;
	let b = tx.attach_node(child.id(), &records[1], None, None)?;
	let endpoint = tx.attach_edge(
		child.id(),
		&records[2],
		None,
		EdgeTopology::directed([a], [b]),
		None,
		None,
	)?;
	let a = tx.attach_node(parent.id(), &records[0], None, None)?;
	let b = tx.attach_node(parent.id(), &records[1], None, None)?;
	let owner = tx.attach_edge(
		parent.id(),
		&records[3],
		None,
		EdgeTopology::directed([a], [b]),
		None,
		Some(child.id()),
	)?;
	let config = ChangefeedConfig::new(Duration::new(jiff::Span::new().days(1)))?.with_full(true);
	let feed = tx.configure_changefeed(ChangefeedSource::Graph(parent.id()), config)?;
	let since = tx.commit()?.versionstamp();
	let mut tx = store.begin();
	tx.attach_edge(
		parent.id(),
		&records[4],
		None,
		EdgeTopology::directed([EdgeEndpoint::Nested(QualifiedAtomId::new([owner], endpoint)?)], [
			EdgeEndpoint::Local(b),
		]),
		Some(0.into()),
		None,
	)?;
	tx.commit()?;
	Ok(QualifiedHistoryFixture {
		parent: parent.id(),
		child: child.id(),
		feed,
		since,
		endpoint: records[2].clone(),
		owner: records[3].clone(),
	})
}
