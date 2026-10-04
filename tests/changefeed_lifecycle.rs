//! Committed reader evidence for graph extraction and external operation
//! boundaries.

use ferros_grynn::{
	changefeed::{
		ChangeBatch,
		ChangeGraphState,
		ChangeIdentity,
		ChangeOverlay,
		ChangefeedConfig,
		ChangefeedId,
		ChangefeedReadItem,
		ChangefeedReadMode,
		ChangefeedReadOptions,
		ChangefeedSince,
		ChangefeedSource,
		Versionstamp,
	},
	datastore::{
		ClassField,
		ClassSchema,
		Datastore,
		IsolationLevel,
		Transaction,
		TransactionAccess,
		TransactionOptions,
	},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{
		AtomKind,
		EdgeDirection,
		EdgeEndpoint,
		EdgeTopology,
		Frozen,
		Graph,
		GraphKind,
		GraphMut,
		GraphRead,
		QualifiedAtomId,
	},
	values::{Duration, Number, Object, PropertyMap, RecordId, Value},
};

/// Fixed FULL policy without query text.
fn configuration() -> FerrosGrynnResult<ChangefeedConfig> {
	Ok(ChangefeedConfig::new(Duration::new(jiff::Span::new().days(1)))?.with_full(true))
}

/// Returns one complete commit and rejects unexpected GAPs or extra commits.
fn batch(
	store: &Datastore,
	feed: ChangefeedId,
	since: Versionstamp,
	mode: ChangefeedReadMode,
) -> FerrosGrynnResult<Option<ChangeBatch>> {
	let mut tx = store.begin();
	let mut reader = tx.changes(
		feed,
		ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(since)).with_mode(mode),
	)?;
	let result = match reader.next_batch()? {
		Some(ChangefeedReadItem::Commit(batch)) => Some(batch),
		Some(_) => return Err(FerrosGrynnError::invalid_argument("unexpected test GAP")),
		None => None,
	};
	assert!(reader.next_batch()?.is_none());
	Ok(result)
}

/// Preserves typed execution failures while allowing handled TRY failures.
fn execute(tx: &mut Transaction, query: &str) -> FerrosGrynnResult<()> {
	for result in tx.execute(query, &Object::new())? {
		let _outcome = result.result?;
	}
	Ok(())
}

/// Projects qualified identity independently of datastore-local IDs.
fn record(image: &ferros_grynn::changefeed::ChangeImage) -> Option<&RecordId> {
	match image.identity() {
		ChangeIdentity::Record(atom) => Some(atom.id()),
		ChangeIdentity::Occurrence(atom) => Some(atom.atom().id()),
		_ => None,
	}
}

/// Captures actual ordered hyperedge routes through node and edge owners.
#[test]
fn changefeed_captures_shared_nested_hyperedge_and_edge_owned_routes() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let nodes = tx.define_class(ClassSchema::new("n", AtomKind::Node, [ClassField::new(
		"value",
		ferros_grynn::values::Type::Int,
	)])?)?;
	tx.define_class(ClassSchema::new("e", AtomKind::Edge, [])?)?;
	let parent = tx.define_graph("parent", GraphKind::Metagraph)?;
	let child = tx.define_graph("child", GraphKind::Metagraph)?;
	let leaf = tx.define_graph("leaf", GraphKind::Graph)?;
	let names = ["n:a", "n:b", "n:mid", "n:c1", "n:c2", "e:base", "e:owner", "e:cross"];
	let records =
		names.into_iter().map(str::parse::<RecordId>).collect::<FerrosGrynnResult<Vec<_>>>()?;
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new())?;
	}
	let a = tx.attach_node(leaf.id(), &records[0], None, None)?;
	let b = tx.attach_node(leaf.id(), &records[1], None, None)?;
	let base = tx.attach_edge(
		leaf.id(),
		&records[5],
		None,
		EdgeTopology::directed([a], [b]),
		Some(Number::Int(0)),
		None,
	)?;
	let mid = tx.attach_node(child.id(), &records[2], None, Some(leaf.id()))?;
	let c1 = tx.attach_node(parent.id(), &records[3], None, Some(child.id()))?;
	let c2 = tx.attach_node(parent.id(), &records[4], None, Some(child.id()))?;
	tx.attach_node(parent.id(), &records[0], None, None)?;
	let owner = tx.attach_edge(
		parent.id(),
		&records[6],
		None,
		EdgeTopology::directed([c1], [c2]),
		None,
		Some(child.id()),
	)?;
	let parent_feed =
		tx.configure_changefeed(ChangefeedSource::Graph(parent.id()), configuration()?)?;
	let child_feed =
		tx.configure_changefeed(ChangefeedSource::Graph(child.id()), configuration()?)?;
	let leaf_feed =
		tx.configure_changefeed(ChangefeedSource::Graph(leaf.id()), configuration()?)?;
	tx.configure_changefeed(ChangefeedSource::Class(nodes), configuration()?)?;
	let since = tx.commit()?.versionstamp();
	let first = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![c1, mid], base)?);
	let second = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![c2, mid], base)?);
	let edge_owned = EdgeEndpoint::Nested(QualifiedAtomId::new(vec![owner, mid], base)?);
	let mut tx = store.begin();
	tx.attach_edge(
		parent.id(),
		&records[7],
		Some("cross".into()),
		EdgeTopology::bidirectional([first.clone(), second, first], [
			edge_owned,
			EdgeEndpoint::Local(c2),
		]),
		Some(Number::Int(0)),
		None,
	)?;
	let stamp = tx.commit()?.versionstamp();
	let captured = batch(&store, parent_feed, since, ChangefeedReadMode::Full)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("cross batch absent"))?;
	assert_eq!(captured.entries().len(), 1);
	assert!(captured.entries()[0].before().is_none());
	assert_eq!(batch(&store, parent_feed, since, ChangefeedReadMode::Net)?, Some(captured.clone()));
	assert!(batch(&store, child_feed, since, ChangefeedReadMode::Full)?.is_none());
	assert!(batch(&store, leaf_feed, since, ChangefeedReadMode::Full)?.is_none());
	let image = captured.entries()[0]
		.after()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("cross image absent"))?;
	let Some(ChangeGraphState::Edge {
		direction,
		source,
		target,
		weight,
		..
	}) = image.graph_state()
	else {
		return Err(FerrosGrynnError::invalid_argument("edge topology absent"));
	};
	assert_eq!(*direction, EdgeDirection::Bidirectional);
	assert_eq!(*weight, Some(Number::Int(0)));
	assert_eq!(source.len(), 3);
	assert_eq!(target.len(), 2);
	for (endpoint, first_owner) in source.iter().zip([&records[3], &records[4], &records[3]]) {
		assert_eq!(endpoint.occurrence().graph().id(), leaf.id());
		assert_eq!(endpoint.occurrence().atom().id(), &records[5]);
		assert_eq!(endpoint.occurrence().atom().kind(), AtomKind::Edge);
		assert_eq!(endpoint.owners().len(), 2);
		assert_eq!(endpoint.owners()[0].graph().id(), parent.id());
		assert_eq!(endpoint.owners()[0].atom().id(), first_owner);
		assert_eq!(endpoint.owners()[1].graph().id(), child.id());
		assert_eq!(endpoint.owners()[1].atom().id(), &records[2]);
	}
	assert_eq!(target[0].owners()[0].atom().kind(), AtomKind::Edge);
	assert_eq!(target[0].owners()[0].atom().id(), &records[6]);
	assert_eq!(target[1].occurrence().atom().id(), &records[4]);
	assert!(target[1].owners().is_empty());
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("value key absent"))?;
	let mut tx = store.begin();
	tx.set_base_property(&records[0], key, 1.into())?;
	tx.commit()?;
	for feed in [parent_feed, leaf_feed] {
		let fanout = batch(&store, feed, stamp, ChangefeedReadMode::Full)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("fanout absent"))?;
		assert_eq!(fanout.entries().len(), 1);
		assert_eq!(
			record(
				fanout.entries()[0]
					.after()
					.ok_or_else(|| FerrosGrynnError::invalid_argument("fanout image absent"))?
			),
			Some(&records[0])
		);
	}
	assert!(batch(&store, child_feed, stamp, ChangefeedReadMode::Full)?.is_none());
	let mut tx = store.begin();
	tx.detach_occurrence(parent.id(), &records[7], false)?;
	tx.commit()?;
	drop(store);
	assert_eq!(source[0].owners()[0].atom().id(), &records[3]);
	Ok(())
}

/// One flush groups changed and explicitly dirty no-op nodes into one
/// operation.
#[test]
fn changefeed_successful_writeback_keeps_graph_images_and_outer_group() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("n", AtomKind::Node, [ClassField::new(
		"value",
		ferros_grynn::values::Type::Int,
	)])?)?;
	let graph = tx.define_graph("scope", GraphKind::Graph)?;
	let class_feed = tx.configure_changefeed(ChangefeedSource::Class(class), configuration()?)?;
	let feed = tx.configure_changefeed(ChangefeedSource::Graph(graph.id()), configuration()?)?;
	tx.commit()?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("value key absent"))?;
	let records = ["n:a".parse::<RecordId>()?, "n:b".parse()?];
	let mut tx = store.begin();
	for record in &records {
		let mut properties = PropertyMap::new();
		properties.insert(key, 1);
		tx.create_atom(record.clone(), properties)?;
		tx.attach_node(graph.id(), record, None, None)?;
	}
	let since = tx.commit()?.versionstamp();
	let mut tx = store.begin();
	let mut materialized =
		ferros_grynn::graph::query_to_memory::<Graph<Frozen>, _>(&graph.read(&tx)?)?.thaw();
	let first = materialized
		.atom_id(&ferros_grynn::graph::AtomRef::new(AtomKind::Node, records[0].clone()))?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("first node absent"))?;
	let second = materialized
		.atom_id(&ferros_grynn::graph::AtomRef::new(AtomKind::Node, records[1].clone()))?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("second node absent"))?;
	materialized.set_node_label(first, Some("changed".into()))?;
	materialized.node_properties_mut(first)?.insert(key, 2);
	materialized.set_node_label(second, None)?;
	materialized.flush(&mut tx)?;
	tx.commit()?;
	let full = batch(&store, feed, since, ChangefeedReadMode::Full)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("flush batch absent"))?;
	assert_eq!(full.entries().len(), 2);
	assert_eq!(full.entries()[0].operation(), full.entries()[1].operation());
	let changed = full
		.entries()
		.iter()
		.find(|entry| entry.after().is_some_and(|image| record(image) == Some(&records[0])))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("changed node absent"))?;
	let after =
		changed.after().ok_or_else(|| FerrosGrynnError::invalid_argument("after absent"))?;
	assert_eq!(after.label().map(|label| label.as_str()), Some("changed"));
	assert_eq!(after.properties().get("value"), Some(&Value::from(2)));
	assert_eq!(after.overlays().get("value"), Some(&ChangeOverlay::Value(2.into())));
	let unchanged = full
		.entries()
		.iter()
		.find(|entry| entry.after().is_some_and(|image| record(image) == Some(&records[1])))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("unchanged node absent"))?;
	assert_eq!(unchanged.before(), unchanged.after());
	assert_eq!(
		batch(&store, feed, since, ChangefeedReadMode::Net)?.map(|batch| batch.entries().len()),
		Some(1)
	);
	assert!(batch(&store, class_feed, since, ChangefeedReadMode::Full)?.is_none());
	assert_eq!(
		store
			.begin()
			.atom(&records[0])?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("base absent"))?
			.properties()
			.get(key),
		Some(&Value::from(1))
	);
	Ok(())
}

/// Failed operations, handled TRY and zero matches never survive in FULL
/// history.
#[test]
fn changefeed_handled_rollback_zero_matches_and_abort_leave_no_history() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	execute(&mut tx, "DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL; CREATE item:a SET value=1;")?;
	let since = tx.commit()?.versionstamp();
	let feed = store
		.begin()
		.changefeed_info(ferros_grynn::changefeed::ChangefeedSelector::ClassName("item".into()))?
		.id();
	let mut tx = store.begin();
	execute(&mut tx, "UPDATE item:missing SET value=999;")?;
	assert_eq!(tx.commit()?.versionstamp(), since);
	let mut tx = store.begin();
	execute(&mut tx, "UPDATE item:a SET value=7;")?;
	drop(tx);
	assert!(batch(&store, feed, since, ChangefeedReadMode::Full)?.is_none());
	let mut tx = store.begin();
	execute(
		&mut tx,
		"UPDATE item:missing SET value=999; TRY { UPDATE item:a SET value=9; THROW 'rollback'; } \
		 CATCH |$e| { none; }; UPDATE item:a SET value=2 RETURN NONE;",
	)?;
	tx.commit()?;
	let full = batch(&store, feed, since, ChangefeedReadMode::Full)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("successful update absent"))?;
	assert_eq!(full.entries().len(), 1);
	assert_eq!(full.entries()[0].operation(), 0);
	assert_eq!(
		full.entries()[0].before().and_then(|image| image.properties().get("value")),
		Some(&Value::from(1))
	);
	assert_eq!(
		full.entries()[0].after().and_then(|image| image.properties().get("value")),
		Some(&Value::from(2))
	);
	assert_eq!(batch(&store, feed, since, ChangefeedReadMode::Net)?, Some(full));
	Ok(())
}

/// ReadCommitted refreshes the next reader while an existing reader keeps its
/// bound.
#[test]
fn changefeed_read_committed_reader_keeps_whole_snapshot_upper_bound() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	execute(&mut tx, "DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL; CREATE item:a SET value=1;")?;
	let first = tx.commit()?.versionstamp();
	let feed = store
		.begin()
		.changefeed_info(ferros_grynn::changefeed::ChangefeedSelector::ClassName("item".into()))?
		.id();
	let mut reader_tx = store.begin_with(TransactionOptions::new(
		IsolationLevel::ReadCommitted,
		TransactionAccess::ReadOnly,
	));
	let mut reader = reader_tx.changes(feed, ChangefeedReadOptions::default())?;
	let old_binding =
		reader.binding().ok_or_else(|| FerrosGrynnError::invalid_argument("old binding absent"))?;
	let mut writer = store.begin();
	execute(&mut writer, "UPDATE item:a SET value=2;")?;
	let second = writer.commit()?.versionstamp();
	assert!(
		matches!(reader.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == first)
	);
	assert!(reader.next_batch()?.is_none());
	assert_eq!(reader.binding(), Some(old_binding));
	drop(reader);
	let mut refreshed = reader_tx
		.changes(feed, ChangefeedReadOptions::new(ChangefeedSince::Versionstamp(first)))?;
	assert!(
		matches!(refreshed.next_batch()?, Some(ChangefeedReadItem::Commit(batch)) if batch.versionstamp() == second)
	);
	assert!(refreshed.next_batch()?.is_none());
	Ok(())
}

/// BUILD GRAPH exposes one logical operation for creations, replacement and
/// explicitly retained no-op occurrences.
#[test]
fn changefeed_build_graph_preserves_outer_operation_and_noop_images() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	execute(
		&mut tx,
		"DEFINE CLASS person TYPE NODE; DEFINE CLASS link TYPE EDGE; DEFINE GRAPH source TYPE \
		 GRAPH; DEFINE GRAPH target TYPE GRAPH;",
	)?;
	let target = tx
		.named_graph("target")?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("target absent"))?;
	let source = tx
		.named_graph("source")?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source absent"))?;
	let records = ["person:left", "person:right", "link:edge"]
		.into_iter()
		.map(str::parse::<RecordId>)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new())?;
	}
	let left = tx.attach_node(source.id(), &records[0], None, None)?;
	let right = tx.attach_node(source.id(), &records[1], None, None)?;
	tx.attach_edge(
		source.id(),
		&records[2],
		None,
		EdgeTopology::directed([left], [right]),
		None,
		None,
	)?;
	let feed = tx.configure_changefeed(ChangefeedSource::Graph(target.id()), configuration()?)?;
	let source_feed =
		tx.configure_changefeed(ChangefeedSource::Graph(source.id()), configuration()?)?;
	let since = tx.commit()?.versionstamp();
	let query = "BUILD GRAPH OVERWRITE <GRAPH> target FROM (MATCH (person:left AS a)-[link:edge \
	             AS e]->(person:right AS b) IN GRAPH source RETURN a,e,b) NODES [a,b] EDGES e";
	let mut tx = store.begin();
	execute(&mut tx, query)?;
	let created_at = tx.commit()?.versionstamp();
	let created = batch(&store, feed, since, ChangefeedReadMode::Full)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("BUILD creation absent"))?;
	assert_eq!(created.entries().len(), 3);
	assert!(created.entries().iter().all(|entry| entry.before().is_none()
		&& entry.after().is_some()
		&& entry.operation() == created.entries()[0].operation()));
	assert!(batch(&store, source_feed, since, ChangefeedReadMode::Full)?.is_none());
	let mut tx = store.begin();
	execute(&mut tx, query)?;
	let retained_at = tx.commit()?.versionstamp();
	let retained = batch(&store, feed, created_at, ChangefeedReadMode::Full)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("BUILD no-op absent"))?;
	assert_eq!(retained.entries().len(), 3);
	assert!(retained.entries().iter().all(|entry| entry.before() == entry.after()
		&& entry.operation() == retained.entries()[0].operation()));
	assert!(batch(&store, feed, created_at, ChangefeedReadMode::Net)?.is_none());
	let mut tx = store.begin();
	execute(&mut tx, "BUILD GRAPH IF NOT EXISTS target FROM 1 / 0")?;
	assert_eq!(tx.commit()?.versionstamp(), retained_at);
	let mut tx = store.begin();
	execute(
		&mut tx,
		"BUILD GRAPH OVERWRITE <GRAPH> target FROM (MATCH (person:left AS a) IN GRAPH source \
		 RETURN a) NODES a",
	)?;
	tx.commit()?;
	let removed = batch(&store, feed, retained_at, ChangefeedReadMode::Net)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("BUILD removals absent"))?;
	assert_eq!(removed.entries().len(), 2);
	assert!(
		removed.entries().iter().all(|entry| entry.before().is_some() && entry.after().is_none())
	);
	Ok(())
}

/// Global deletion records graph-local cascade effects without deleting
/// otherwise independent global edge records.
#[test]
fn changefeed_cascade_groups_edge_endpoint_deletions() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let nodes = tx.define_class(ClassSchema::new("n", AtomKind::Node, [])?)?;
	let edges = tx.define_class(ClassSchema::new("e", AtomKind::Edge, [])?)?;
	let first = tx.define_graph("first", GraphKind::Metagraph)?;
	let second = tx.define_graph("second", GraphKind::Metagraph)?;
	let records = ["n:a", "n:b", "e:one", "e:two", "e:three"]
		.into_iter()
		.map(str::parse::<RecordId>)
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	for record in &records {
		tx.create_atom(record.clone(), PropertyMap::new())?;
	}
	let a = tx.attach_node(first.id(), &records[0], None, None)?;
	let b = tx.attach_node(first.id(), &records[1], None, None)?;
	let edge = tx.attach_edge(
		first.id(),
		&records[2],
		None,
		EdgeTopology::directed([a], [b]),
		None,
		None,
	)?;
	tx.attach_edge(first.id(), &records[3], None, EdgeTopology::directed([edge], [b]), None, None)?;
	let a = tx.attach_node(second.id(), &records[0], None, None)?;
	let b = tx.attach_node(second.id(), &records[1], None, None)?;
	tx.attach_edge(second.id(), &records[4], None, EdgeTopology::directed([a], [b]), None, None)?;
	let node_feed = tx.configure_changefeed(ChangefeedSource::Class(nodes), configuration()?)?;
	let edge_feed = tx.configure_changefeed(ChangefeedSource::Class(edges), configuration()?)?;
	let first_feed =
		tx.configure_changefeed(ChangefeedSource::Graph(first.id()), configuration()?)?;
	let second_feed =
		tx.configure_changefeed(ChangefeedSource::Graph(second.id()), configuration()?)?;
	let since = tx.commit()?.versionstamp();
	let mut tx = store.begin();
	assert!(tx.delete_atom_cascade(&records[0], false).is_err());
	tx.delete_atom_cascade(&records[0], true)?;
	tx.commit()?;
	for (feed, count) in [(node_feed, 1), (first_feed, 3), (second_feed, 2)] {
		let deleted = batch(&store, feed, since, ChangefeedReadMode::Full)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("cascade batch absent"))?;
		assert_eq!(deleted.entries().len(), count);
		assert!(deleted.entries().iter().all(|entry| entry.before().is_some()
			&& entry.after().is_none()
			&& entry.operation() == deleted.entries()[0].operation()));
	}
	assert!(batch(&store, edge_feed, since, ChangefeedReadMode::Full)?.is_none());
	let mut tx = store.begin();
	for record in &records[2..] {
		assert!(tx.atom(record)?.is_some());
	}
	Ok(())
}

/// Rebased base-property capture uses memberships of the validated commit root.
#[test]
fn changefeed_rebased_fanout_uses_current_memberships() -> FerrosGrynnResult<()> {
	for attach in [true, false] {
		let store = Datastore::in_memory();
		let mut tx = store.begin();
		tx.define_class(ClassSchema::new("n", AtomKind::Node, [ClassField::new(
			"value",
			ferros_grynn::values::Type::Int,
		)])?)?;
		let first = tx.define_graph("first", GraphKind::Graph)?;
		let second = tx.define_graph("second", GraphKind::Graph)?;
		let record: RecordId = "n:a".parse()?;
		tx.create_atom(record.clone(), PropertyMap::new())?;
		tx.attach_node(first.id(), &record, None, None)?;
		if !attach {
			tx.attach_node(second.id(), &record, None, None)?;
		}
		let first_feed =
			tx.configure_changefeed(ChangefeedSource::Graph(first.id()), configuration()?)?;
		let second_feed =
			tx.configure_changefeed(ChangefeedSource::Graph(second.id()), configuration()?)?;
		tx.commit()?;
		let key = store
			.property_dictionary()
			.get("value")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
		let mut writer = store.begin_with(TransactionOptions::new(
			IsolationLevel::Snapshot,
			TransactionAccess::ReadWrite,
		));
		writer.set_base_property(&record, key, 2.into())?;
		let mut membership = store.begin();
		if attach {
			membership.attach_node(second.id(), &record, None, None)?;
		} else {
			membership.detach_occurrence(second.id(), &record, false)?;
		}
		let since = membership.commit()?.versionstamp();
		writer.commit()?;
		for (feed, expected) in [(first_feed, true), (second_feed, attach)] {
			let captured = batch(&store, feed, since, ChangefeedReadMode::Full)?;
			assert_eq!(captured.is_some(), expected);
			if let Some(captured) = captured {
				assert_eq!(captured.entries().len(), 1);
				assert_eq!(
					captured.entries()[0].after().and_then(|image| image.properties().get("value")),
					Some(&Value::from(2))
				);
			}
		}
	}
	Ok(())
}

/// FULL-only no-op targets participate in Serializable write-skew validation.
#[test]
fn changefeed_full_noop_rejects_serializable_cycle() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	execute(
		&mut tx,
		"DEFINE CLASS item TYPE NODE CHANGEFEED 7d FULL; CREATE item:a SET value=1; CREATE item:b \
		 SET value=1;",
	)?;
	let since = tx.commit()?.versionstamp();
	let a: RecordId = "item:a".parse()?;
	let b: RecordId = "item:b".parse()?;
	let key = store
		.property_dictionary()
		.get("value")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
	let feed = store
		.begin()
		.changefeed_info(ferros_grynn::changefeed::ChangefeedSelector::ClassName("item".into()))?
		.id();
	let options =
		TransactionOptions::new(IsolationLevel::Serializable, TransactionAccess::ReadWrite);
	let mut first = store.begin_with(options);
	let mut second = store.begin_with(options);
	assert!(first.atom(&b)?.is_some());
	assert!(second.atom(&a)?.is_some());
	first.set_base_property(&a, key, 1.into())?;
	second.set_base_property(&b, key, 2.into())?;
	let committed = second.commit()?.versionstamp();
	assert!(first.commit().is_err());
	let history = batch(&store, feed, since, ChangefeedReadMode::Full)?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("writer history absent"))?;
	assert_eq!(history.versionstamp(), committed);
	assert_eq!(history.entries().len(), 1);
	assert_eq!(history.entries()[0].after().and_then(record), Some(&b));
	Ok(())
}
