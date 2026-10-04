//! Common public SDK setup for independent changefeed backend targets.

use ferros_grynn::{
	changefeed::{
		ChangefeedConfig,
		ChangefeedCursor,
		ChangefeedId,
		ChangefeedInfo,
		ChangefeedReadMode,
		ChangefeedSource,
		ChangefeedState,
	},
	datastore::{ClassField, ClassId, ClassSchema, Datastore},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphId, GraphKind},
	values::{Duration, PropertyKeyId, PropertyMap, RecordId, Type},
};

/// Creates and stops source configurations exclusively through the Rust API.
pub fn configure(store: &Datastore) -> FerrosGrynnResult<ChangefeedInfo> {
	let mut transaction = store.begin();
	let class = transaction.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let graph = transaction.define_graph("scope", GraphKind::Metagraph)?;
	let config = ChangefeedConfig::new(Duration::new(jiff::Span::new().days(7)))?
		.with_full(true)
		.with_include_original(true);
	let id = transaction.configure_changefeed(ChangefeedSource::Class(class), config)?;
	let graph_id = transaction.configure_changefeed(ChangefeedSource::Graph(graph.id()), config)?;
	assert!(store.begin().changefeed_info(id).is_err());
	let commit = transaction.commit()?;
	let mut old = store.begin();
	let original = old.changefeed_info(id)?;
	assert_eq!(original.defined_at(), commit.versionstamp());
	let cursor = ChangefeedCursor::new(
		original.database_id(),
		id,
		ChangefeedReadMode::Full,
		commit.versionstamp(),
	)?;
	assert_eq!(ChangefeedCursor::decode(cursor.encode().as_slice())?, cursor);
	let mut dropper = store.begin();
	dropper.drop_changefeed(ChangefeedSource::Class(class))?;
	dropper.remove_graph("scope")?;
	dropper.commit()?;
	assert_eq!(old.changefeed_info(id)?, original);
	assert_eq!(store.begin().changefeed_info(graph_id)?.state(), ChangefeedState::Retired);
	let stopped = store.begin().changefeed_info(id)?;
	assert_eq!(stopped.state(), ChangefeedState::Stopped);
	assert_eq!(stopped.configuration(), config);
	Ok(stopped)
}

/// SDK-only setup shared by independent RAM, Sync and NativeAsync reader
/// targets.
pub struct HistoryFixture {
	/// Global feed owner.
	pub class: ClassId,
	/// Qualified occurrence scope.
	pub graph: GraphId,
	/// Global retained feed.
	pub feed: ChangefeedId,
	/// Qualified retained feed.
	pub graph_feed: ChangefeedId,
	/// First record with a numeric property.
	pub first: RecordId,
	/// Second independent record.
	pub second: RecordId,
	/// Numeric property dictionary key.
	pub key: PropertyKeyId,
}

/// Publishes two records and occurrences under one historically FULL policy.
pub fn history_fixture(store: &Datastore) -> FerrosGrynnResult<HistoryFixture> {
	let mut transaction = store.begin();
	let class = transaction.define_class(ClassSchema::new("history", AtomKind::Node, [
		ClassField::new("value", Type::Int),
	])?)?;
	let graph = transaction.define_graph("history_scope", GraphKind::Graph)?.id();
	let config = ChangefeedConfig::new(Duration::new(jiff::Span::new().days(1)))?.with_full(true);
	let feed = transaction.configure_changefeed(ChangefeedSource::Class(class), config)?;
	let graph_feed = transaction.configure_changefeed(ChangefeedSource::Graph(graph), config)?;
	transaction.commit()?;
	let key = store.property_dictionary().get("value").ok_or_else(|| {
		FerrosGrynnError::invalid_argument("published value field has no dictionary key")
	})?;
	let mut transaction = store.begin();
	let first: RecordId = "history:a".parse()?;
	let second: RecordId = "history:b".parse()?;
	for record in [&first, &second] {
		let mut properties = PropertyMap::new();
		properties.insert(key, 1);
		transaction.create_atom(record.clone(), properties)?;
		transaction.attach_node(graph, record, None, None)?;
	}
	transaction.commit()?;
	Ok(HistoryFixture {
		class,
		graph,
		feed,
		graph_feed,
		first,
		second,
		key,
	})
}

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
