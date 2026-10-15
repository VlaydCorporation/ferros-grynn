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
	graph::{AtomKind, GraphKind},
	values::{Duration, PropertyKeyId, PropertyMap, RecordId, Type},
};

/// Creates and stops source configurations exclusively through the Rust API.
pub fn configure(store: &Datastore) -> FerrosGrynnResult<ChangefeedInfo> {
	use crate::test_assertions::{AssertComparison as _, AssertCondition as _};

	let mut transaction = store.begin();
	let class = transaction.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let graph = transaction.define_graph("scope", GraphKind::Metagraph)?;
	let config = ChangefeedConfig::new(Duration::new(jiff::Span::new().days(7)))?
		.with_full(true)
		.with_include_original(true);
	let id = transaction.configure_changefeed(ChangefeedSource::Class(class), config)?;
	let graph_id = transaction.configure_changefeed(ChangefeedSource::Graph(graph.id()), config)?;
	(store.begin().changefeed_info(id).is_err()).assert_true();
	let commit = transaction.commit()?;
	let mut old = store.begin();
	let original = old.changefeed_info(id)?;
	(&(original.defined_at()), &(commit.versionstamp())).assert_equal();
	let cursor = ChangefeedCursor::new(
		original.database_id(),
		id,
		ChangefeedReadMode::Full,
		commit.versionstamp(),
	)?;
	(&(ChangefeedCursor::decode(cursor.encode().as_slice())?), &(cursor)).assert_equal();
	let mut dropper = store.begin();
	dropper.drop_changefeed(ChangefeedSource::Class(class))?;
	dropper.remove_graph("scope")?;
	dropper.commit()?;
	(&(old.changefeed_info(id)?), &(original)).assert_equal();
	(&(store.begin().changefeed_info(graph_id)?.state()), &(ChangefeedState::Retired))
		.assert_equal();
	let stopped = store.begin().changefeed_info(id)?;
	(&(stopped.state()), &(ChangefeedState::Stopped)).assert_equal();
	(&(stopped.configuration()), &(config)).assert_equal();
	Ok(stopped)
}

/// SDK-only setup shared by independent RAM, Sync and NativeAsync reader
/// targets.
pub struct HistoryFixture {
	/// Global retained feed.
	pub feed: ChangefeedId,
	/// First record with a numeric property.
	pub first: RecordId,
	/// Numeric property dictionary key.
	pub key: PropertyKeyId,
}

/// Setup identities needed by metadata-specific API tests: global class owner,
/// qualified feed, and the second independent record, in that order.
/// Local readers retain this tuple until fixture teardown without naming fields
/// that their test target does not inspect.
pub type HistorySourceMetadata = (ClassId, ChangefeedId, RecordId);

/// Publishes two records and occurrences under one historically FULL policy.
///
/// # Errors
/// Returns public schema, graph, record, changefeed or transaction failures.
pub fn history_fixture(
	store: &Datastore,
) -> FerrosGrynnResult<(HistoryFixture, HistorySourceMetadata)> {
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
	Ok((
		HistoryFixture {
			feed,
			first,
			key,
		},
		(class, graph_feed, second),
	))
}
