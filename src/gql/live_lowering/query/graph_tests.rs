//! SDK graph recipes use ordinary SELECT projection and graph closure.

use super::*;
use crate::{
	datastore::{ClassSchema, Datastore},
	graph::{
		AtomKind,
		AtomOccurrenceRef,
		AtomRef,
		EdgeTopology,
		GraphKind,
		GraphRead,
		MaterializedGraph,
	},
	live::{
		LiveExpr,
		LiveField,
		LiveGraphResult,
		LiveJoin,
		LiveJoinKind,
		LivePath,
		LiveProjection,
	},
	values::{DateTime, PropertyMap, RecordId},
};

fn evaluate(store: &Datastore, query: &LiveQuery) -> FerrosGrynnResult<QueryEvaluation> {
	evaluate_query(
		&store.inner.current_root(),
		query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveOptions::default(),
		std::env::temp_dir(),
		&|_| {},
	)
}

fn fixture() -> FerrosGrynnResult<(Datastore, LiveSource, Value, Value)> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let source = seed.define_graph("source", GraphKind::Graph)?;
	let score = seed.intern_property("score")?;
	let tags = seed.intern_property("tags")?;
	let left: RecordId = "person:left".parse()?;
	let right: RecordId = "person:right".parse()?;
	let edge: RecordId = "link:edge".parse()?;
	for (record, score_value) in [(&left, 1), (&right, 2), (&edge, 3)] {
		seed.create_atom(
			record.clone(),
			PropertyMap::from_iter([
				(score, Value::from(score_value)),
				(tags, Value::Array(vec![Value::from(1), Value::from(1)].into())),
			]),
		)?;
	}
	let left_id = seed.attach_node(source.id(), &left, None, None)?;
	let right_id = seed.attach_node(source.id(), &right, None, None)?;
	seed.attach_edge(
		source.id(),
		&edge,
		None,
		EdgeTopology::directed([left_id], [right_id]),
		None,
		None,
	)?;
	seed.commit()?;
	let root = Value::from(AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Node, left),
		Arc::new(source.clone()),
	));
	let edge = Value::from(AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Edge, edge),
		Arc::new(source.clone()),
	));
	Ok((
		store,
		LiveSource::GraphClass {
			graph: source,
			class,
		},
		root,
		edge,
	))
}

fn signature(
	graph: &MaterializedGraph,
) -> FerrosGrynnResult<(Vec<(RecordId, PropertyMap)>, Vec<(RecordId, PropertyMap, Vec<RecordId>)>)>
{
	let nodes = graph
		.node_ids()?
		.map(|id| {
			let node = graph
				.node(id?)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle node absent"))?;
			Ok((node.record_id().clone(), node.properties().clone()))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let edges = graph
		.edge_ids()?
		.map(|id| {
			let edge = graph
				.edge(id?)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle edge absent"))?;
			let endpoints =
				edge.topology()
					.endpoints()
					.map(|endpoint| {
						let id = endpoint.local_id().ok_or_else(|| {
							FerrosGrynnError::invalid_argument("fixture has nonlocal endpoint")
						})?;
						graph.node(id).map(|node| node.record_id().clone()).ok_or_else(|| {
							FerrosGrynnError::invalid_argument("oracle endpoint absent")
						})
					})
					.collect::<FerrosGrynnResult<Vec<_>>>()?;
			Ok((edge.record_id().clone(), edge.properties().clone(), endpoints))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	Ok((nodes, edges))
}

#[test]
fn sdk_graph_result_oracle_all_families_and_effective_closure() -> FerrosGrynnResult<()> {
	let (store, source, root, edge) = fixture()?;
	let base = LiveQuery::new(source).with_projection(LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::field("id")?).with_alias(LivePath::field("picked")?),
	]));
	for (family, annotation) in [
		(GraphKind::Subgraph, ""),
		(GraphKind::Graph, "<GRAPH>"),
		(GraphKind::Tree, "<TREE>"),
		(GraphKind::RootedTree, "<ROOTED_TREE> ROOT $root"),
		(GraphKind::Forest, "<FOREST>"),
		(GraphKind::Dag, "<DAG>"),
		(GraphKind::Hypergraph, "<HYPER>"),
		(GraphKind::Metagraph, "<META>"),
	] {
		let mut recipe =
			LiveGraphResult::new(LiveExpr::field("picked")?, LiveExpr::Literal(edge.clone()))
				.with_family(family);
		if family == GraphKind::RootedTree {
			recipe = recipe.with_root(LiveExpr::Literal(root.clone()));
		}
		let actual = evaluate(&store, &base.clone().with_graph_result(recipe))?;
		assert!(actual.rows.is_empty());
		let image = actual
			.graph_image
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("logical graph image absent"))?;
		assert_eq!(image.family(), family);
		let image_root = image
			.scope(&crate::live::LiveGraphScopeId::Root)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("logical root scope absent"))?;
		assert_eq!(image_root.nodes().len(), 2);
		assert_eq!(image_root.edges().len(), 1);
		assert!(image_root.nodes().all(|(_, node)| node.source().is_some()));
		assert!(image_root.edges().all(|(_, edge)| edge.source().is_some()));
		assert!(image.property_key("score").is_some());
		assert_eq!(image.root().is_some(), family == GraphKind::RootedTree);
		let encoded = crate::storage::live_graph_image::encode(image, 1 << 24)?;
		assert_eq!(&encoded[..8], b"FGLG\x01\x00\x00\x00");
		assert_eq!(crate::storage::live_graph_image::decode(&encoded, 1 << 24)?, *image);
		assert!(crate::storage::live_graph_image::decode(&encoded, 1).is_err());
		for offset in [0, 4, 6, 8, 16, 24, encoded.len() - 1] {
			let mut bad = encoded.clone();
			bad[offset] ^= 255;
			assert!(crate::storage::live_graph_image::decode(&bad, 1 << 24).is_err());
		}
		let actual_graph =
			actual.graph.ok_or_else(|| FerrosGrynnError::invalid_argument("SDK graph absent"))?;
		let variables = Object::from_iter([("root", root.clone()), ("edge", edge.clone())]);
		let result = store
			.execute(
				&format!(
					"SELECT id AS picked FROM person IN GRAPH source RETURN GRAPH {annotation} \
					 {{nodes: picked, edges: $edge}}"
				),
				&variables,
			)?
			.remove(0)
			.result?
			.into_parts()
			.0;
		let Value::Graph(expected) = result else {
			return Err(FerrosGrynnError::invalid_argument("GQL graph absent"));
		};
		let expected_image = crate::live::LiveGraphImage::capture(
			&expected,
			&store.inner.current_root().dictionary,
			&|_, _| Ok(()),
			&|_, _| Ok(None),
		)?;
		let incarnation = crate::live::LiveId::from_uuid(crate::values::Uuid::now_v7())?;
		let restored = crate::storage::live_graph_image::decode(&encoded, 1 << 24)?.restore(
			incarnation,
			99.into(),
			&mut |_, _| Ok(()),
		)?;
		assert_eq!(restored.identity(), incarnation.as_uuid());
		assert_eq!(restored.revision(), 99);
		assert_eq!(restored.kind(), family);
		assert_eq!(signature(&restored)?, signature(&expected)?);
		assert!(
			image
				.restore(incarnation, 99.into(), &mut |_, _| Err(
					FerrosGrynnError::traversal_resource_limit_exceeded("restore", 0)
				))
				.is_err()
		);
		let expected_scope = expected_image
			.scope(&crate::live::LiveGraphScopeId::Root)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle logical scope absent"))?;
		assert_eq!(
			image_root.nodes().map(|(atom, node)| (atom, node.properties())).collect::<Vec<_>>(),
			expected_scope
				.nodes()
				.map(|(atom, node)| (atom, node.properties()))
				.collect::<Vec<_>>()
		);
		assert_eq!(
			image_root
				.edges()
				.map(|(atom, edge)| (
					atom,
					edge.properties(),
					edge.direction(),
					edge.inputs(),
					edge.outputs()
				))
				.collect::<Vec<_>>(),
			expected_scope
				.edges()
				.map(|(atom, edge)| (
					atom,
					edge.properties(),
					edge.direction(),
					edge.inputs(),
					edge.outputs()
				))
				.collect::<Vec<_>>()
		);
		assert!(
			crate::live::LiveGraphImage::capture(
				&expected,
				&store.inner.current_root().dictionary,
				&|_, _| Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"image capture",
					0
				)),
				&|_, _| Ok(None)
			)
			.is_err()
		);
		assert_eq!(actual_graph.kind(), family);
		assert_eq!(signature(&actual_graph)?, signature(&expected)?);
		assert!(actual.reads.iter().any(|key| matches!(key, ConflictKey::GraphIncidence(_, _))));
	}
	Ok(())
}

#[test]
fn sdk_graph_result_join_split_group_share_the_projected_domain() -> FerrosGrynnResult<()> {
	let (store, source, _root, _edge) = fixture()?;
	let nodes = LiveExpr::field("picked")?;
	let base = LiveQuery::new(source.clone()).with_projection(LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::field("id")?).with_alias(LivePath::field("picked")?),
	]));
	let joined = base
		.clone()
		.with_alias("p")?
		.with_joins(vec![LiveJoin::new(source, "q", LiveJoinKind::Cross, None)?])
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::Field(LivePath::new(vec!["p".into(), "id".into()])?))
				.with_alias(LivePath::field("picked")?),
		]));
	let split = base.clone().with_split(vec![LiveExpr::Field(LivePath::new(vec![
		"properties".into(),
		"tags".into(),
	])?)]);
	let group = base.with_group_all().with_projection(LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::Call {
			name: "array::group".into(),
			arguments: vec![LiveExpr::field("id")?],
		})
		.with_alias(LivePath::field("picked")?),
	]));
	for (clause, query) in [joined, split, group].into_iter().enumerate() {
		let result = evaluate(
			&store,
			&query.with_graph_result(LiveGraphResult::new(
				nodes.clone(),
				LiveExpr::Array(Vec::new()),
			)),
		)
		.map_err(|error| {
			FerrosGrynnError::invalid_argument(format!("SDK graph clause {clause} failed"))
				.with_cause(error)
		})?;
		let graph = result
			.graph
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph result absent"))?;
		assert_eq!(graph.node_count(), 2);
		assert_eq!(graph.edge_count(), 0);
	}
	Ok(())
}

#[test]
fn sdk_graph_result_closure_base_fanout_and_registration_reads() -> FerrosGrynnResult<()> {
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("id")?).with_alias(LivePath::field("picked")?),
		]))
		.with_graph_result(LiveGraphResult::new(
			LiveExpr::field("picked")?,
			LiveExpr::Literal(edge),
		));
	let first = evaluate(&store, &query)?;
	let mut registration = store.begin();
	registration.register_live(query.clone(), LiveOptions::default())?;
	let mut write = store.begin();
	let key = write.intern_property("score")?;
	let edge: RecordId = "link:edge".parse()?;
	let global = store
		.inner
		.current_root()
		.atom_by_record(&edge)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("edge absent"))?
		.id();
	assert!(first.reads.contains(&ConflictKey::BaseAtom(global)));
	write.set_base_property(&edge, key, Value::from(77))?;
	write.commit()?;
	assert!(registration.commit().is_err());
	let second = evaluate(&store, &query)?;
	let before =
		first.graph.ok_or_else(|| FerrosGrynnError::invalid_argument("first graph absent"))?;
	let after =
		second.graph.ok_or_else(|| FerrosGrynnError::invalid_argument("second graph absent"))?;
	assert_ne!(signature(&before)?, signature(&after)?);
	assert_eq!(before.node_count(), after.node_count());
	assert_eq!(before.edge_count(), after.edge_count());
	Ok(())
}

#[test]
fn graph_result_logical_delta_tracks_properties_topology_and_deleted_targets()
-> FerrosGrynnResult<()> {
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("id")?).with_alias(LivePath::field("picked")?),
		]))
		.with_graph_result(LiveGraphResult::new(
			LiveExpr::field("picked")?,
			LiveExpr::Literal(edge),
		));
	let image = Arc::new(
		evaluate(&store, &query)?
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph image absent"))?,
	);
	assert!(image.estimated_heap_bytes()? > 0);
	let unchanged = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(&image)),
		Arc::clone(&image),
		1.into(),
		2.into(),
		&|_, _| Ok(()),
	)?;
	assert!(unchanged.changes().is_empty());
	let mut changed = image.as_ref().clone();
	let scope = changed
		.scopes
		.get_mut(&crate::live::LiveGraphScopeId::Root)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("root absent"))?;
	let node: AtomRef = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	let edge = AtomRef::new(AtomKind::Edge, "link:edge".parse()?);
	scope
		.nodes
		.get_mut(&node)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("node absent"))?
		.properties
		.insert("score", Value::from(99));
	scope
		.edges
		.get_mut(&edge)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("edge absent"))?
		.direction = crate::graph::EdgeDirection::Undirected;
	let delta = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(&image)),
		Arc::new(changed),
		2.into(),
		3.into(),
		&|_, _| Ok(()),
	)?;
	assert_eq!(delta.changes().len(), 2);
	assert!(
		delta
			.changes()
			.iter()
			.all(|target| matches!(target, crate::live::LiveGraphTarget::Atom(_)))
	);
	let mut removed = image.as_ref().clone();
	removed
		.scopes
		.get_mut(&crate::live::LiveGraphScopeId::Root)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("root absent"))?
		.edges
		.remove(&edge);
	let deletion = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(&image)),
		Arc::new(removed),
		3.into(),
		4.into(),
		&|_, _| Ok(()),
	)?;
	assert_eq!(deletion.changes().len(), 1);
	assert!(
		matches!(&deletion.changes()[0], crate::live::LiveGraphTarget::Atom(atom) if atom.atom() == &edge)
	);
	assert!(
		crate::live::LiveGraphDelta::between(None, image, 0.into(), 1.into(), &|_, _| Err(
			FerrosGrynnError::traversal_resource_limit_exceeded("delta", 0)
		))
		.is_err()
	);
	Ok(())
}

/// Activation is immutable even when a tail replaces the root before SDK use.
#[test]
fn graph_result_publication_keeps_initial_checkpoint_and_whole_commit_ack() -> FerrosGrynnResult<()>
{
	use crate::live::{LiveNext, LivePayload};
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("id")?).with_alias(LivePath::field("picked")?),
		]))
		.with_graph_result(LiveGraphResult::new(
			LiveExpr::field("picked")?,
			LiveExpr::Literal(edge),
		));
	let mut subscription = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let registered = subscription.cursor().versionstamp();
	let oracle = evaluate(&store, &query)?
		.graph_image
		.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle image absent"))?;
	assert!(matches!(subscription.next()?, LiveNext::Idle));
	let mut write = store.begin();
	let key = write.intern_property("score")?;
	write.set_base_property(&"person:left".parse()?, key, Value::from(77))?;
	let changed = write.commit()?.versionstamp();
	let initial = subscription
		.inner
		.take_graph_checkpoint()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("activation checkpoint absent"))?;
	assert_eq!(initial.versionstamp(), registered);
	assert_eq!(initial.delta().after(), &oracle);
	let LiveNext::Delivery(delivery) = subscription.next()? else {
		return Err(FerrosGrynnError::invalid_argument("graph delta absent"));
	};
	let LivePayload::Graph(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("graph publication faulted"));
	};
	assert_eq!(batch.versionstamp(), changed);
	assert_eq!(batch.delta().base(), registered);
	assert_eq!(batch.delta().before(), Some(&oracle));
	let after = evaluate(&store, &query)?
		.graph_image
		.ok_or_else(|| FerrosGrynnError::invalid_argument("new oracle absent"))?;
	assert_eq!(batch.delta().after(), &after);
	let changed_node = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	let node = batch
		.delta()
		.after()
		.scopes
		.get(&crate::live::LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get(&changed_node))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("changed node absent"))?;
	assert_eq!(
		node.source()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source mapping absent"))?
			.revision(),
		changed
	);
	let repeated = subscription.next()?;
	assert!(
		matches!(repeated, LiveNext::Delivery(repeated) if repeated.token() == delivery.token())
	);
	subscription.ack(delivery.token())?;
	assert_eq!(subscription.cursor().versionstamp(), changed);
	assert!(matches!(subscription.next()?, LiveNext::Idle));
	subscription.close()?;
	Ok(())
}

#[test]
fn graph_result_nested_scope_validators_are_owned_and_diffed() -> FerrosGrynnResult<()> {
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("doc", AtomKind::Node, [])?)?;
	let root = seed.define_graph("root", GraphKind::Metagraph)?;
	let child = seed.define_graph("child", GraphKind::Graph)?;
	let owner: RecordId = "module:owner".parse()?;
	let document: RecordId = "doc:one".parse()?;
	seed.create_atom(owner.clone(), PropertyMap::new())?;
	seed.create_atom(document.clone(), PropertyMap::new())?;
	seed.attach_node(child.id(), &document, None, None)?;
	seed.attach_node(root.id(), &owner, None, Some(child.id()))?;
	seed.commit()?;
	let selected = Value::from(AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Node, document),
		Arc::new(child.clone()),
	));
	let query = LiveQuery::new(LiveSource::GraphRecord {
		graph: root,
		record: owner,
	})
	.with_graph_result(LiveGraphResult::new(
		LiveExpr::Literal(selected),
		LiveExpr::Array(Vec::new()),
	));
	let before = Arc::new(
		evaluate(&store, &query)?
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("first image absent"))?,
	);
	let mut constraints = crate::graph::GraphTopologyConstraints::empty();
	constraints.insert(crate::graph::GraphTopologyConstraint::Simple);
	let mut alter = store.begin();
	alter.replace_graph_definition("child", None, constraints, None)?;
	alter.commit()?;
	let after = Arc::new(
		evaluate(&store, &query)?
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("second image absent"))?,
	);
	let child_key = crate::live::LiveGraphScopeId::Source(child.id());
	assert_ne!(
		before
			.scope(&child_key)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("first child absent"))?
			.validators(),
		after
			.scope(&child_key)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("second child absent"))?
			.validators()
	);
	let delta =
		crate::live::LiveGraphDelta::between(Some(before), after, 1.into(), 2.into(), &|_, _| {
			Ok(())
		})?;
	assert!(delta.changes().contains(&crate::live::LiveGraphTarget::Graph(child_key)));
	Ok(())
}

/// Retained images share the same aggregate cap in publication and recovery.
#[test]
fn graph_result_checkpoint_retained_admission_rejects_without_losing_authority()
-> FerrosGrynnResult<()> {
	use crate::live::{LiveGraphBatch, LiveGraphDelta, LiveGraphScopeId, LiveId, LiveVersion};
	let (store, source, _root, _edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(LiveGraphResult::new(
		LiveExpr::field("id")?,
		LiveExpr::Literal(Value::None),
	));
	let mut image = evaluate(&store, &query)?
		.graph_image
		.ok_or_else(|| FerrosGrynnError::invalid_argument("image absent"))?;
	let scope = image
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("root scope absent"))?;
	let node = scope
		.nodes
		.values_mut()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("node absent"))?;
	node.properties.insert("payload", Value::from("x".repeat(1 << 20)));
	let image = Arc::new(image);
	let mut catalog = crate::datastore::LiveCatalog::default();
	let mut rejected = false;
	for ordinal in 1..=512 {
		let id = LiveId::from_uuid(uuid::Uuid::from_u128(ordinal).into())?;
		let batch = Arc::new(LiveGraphBatch {
			id,
			version: LiveVersion::default(),
			committed_at: DateTime::from(
				jiff::Timestamp::from_nanosecond(100)?.to_zoned(jiff::tz::TimeZone::UTC),
			),
			delta: LiveGraphDelta::between(
				None,
				Arc::clone(&image),
				0.into(),
				1.into(),
				&|_, _| Ok(()),
			)?,
			bindings: Vec::new(),
		});
		let bytes = catalog.graph_bytes;
		let count = catalog.graph_checkpoints.len();
		if catalog.replace_graph_checkpoint(id, Arc::clone(&batch)).is_err() {
			assert_eq!(catalog.graph_bytes, bytes);
			assert_eq!(catalog.graph_checkpoints.len(), count);
			assert!(!catalog.graph_checkpoints.contains_key(&id));
			rejected = true;
			break;
		}
		assert_eq!(catalog.graph_bytes, bytes + batch.estimated_heap_bytes()?);
		catalog.replace_graph_checkpoint(id, batch)?;
		assert_eq!(catalog.graph_checkpoints.len(), count + 1);
	}
	assert!(rejected);
	assert!(catalog.graph_bytes <= 256 * 1024 * 1024);
	Ok(())
}

/// Every sealed family accepts whole revisions and keeps immutable old guards.
fn automatic_graph_subscription<G>() -> FerrosGrynnResult<()>
where
	G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
{
	use crate::live::LiveGraphNext;
	let (store, source, root, edge) = fixture()?;
	let mut recipe = LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
		.with_family(G::family_kind());
	if G::family_kind() == GraphKind::RootedTree {
		recipe = recipe.with_root(LiveExpr::Literal(root));
	}
	let query = LiveQuery::new(source).with_graph_result(recipe);
	let mut subscription = store.subscribe_live_graph::<G>(query, LiveOptions::default())?;
	let original = subscription.materialization()?.read()?;
	let first = subscription
		.revision()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("initial SDK revision absent"))?;
	assert_eq!(original.node_count()?, 2);
	assert_eq!(original.edge_count()?, 1);
	assert_eq!(original.identity(), subscription.id().as_uuid());
	let mut write = store.begin();
	let key = write.intern_property("score")?;
	write.set_base_property(&"person:left".parse()?, key, Value::from(99))?;
	let stamp = write.commit()?.versionstamp();
	assert!(
		matches!(subscription.next()?, LiveGraphNext::Updated { revision } if revision == stamp)
	);
	assert_eq!(subscription.cursor().versionstamp(), stamp);
	assert!(subscription.applied_unconfirmed().is_none());
	assert_eq!(subscription.materialization()?.read()?.revision(), stamp.get());
	assert_eq!(original.revision(), first.get());
	assert_eq!(original.identity(), subscription.materialization()?.read()?.identity());
	assert!(matches!(subscription.next()?, LiveGraphNext::Idle));
	let saved = subscription.cursor().clone();
	drop(subscription);
	let resumed = store.resume_live_graph::<G>(&saved)?;
	assert_eq!(resumed.revision(), Some(stamp));
	assert_eq!(resumed.materialization()?.read()?.node_count()?, 2);
	let mut mutable = resumed.into_mutable()?;
	assert_eq!(mutable.materialization()?.static_snapshot()?.node_count()?, 2);
	mutable.close()?;
	assert_eq!(original.revision(), first.get());
	Ok(())
}

#[test]
fn automatic_live_graph_sdk_all_families_update_resume_and_old_guards() -> FerrosGrynnResult<()> {
	use crate::graph::{
		Dag,
		Forest,
		Frozen,
		Graph,
		Hypergraph,
		Metagraph,
		RootedTree,
		Subgraph,
		Tree,
	};
	automatic_graph_subscription::<Graph<Frozen>>()?;
	automatic_graph_subscription::<Forest<Frozen>>()?;
	automatic_graph_subscription::<Tree<Frozen>>()?;
	automatic_graph_subscription::<RootedTree<Frozen>>()?;
	automatic_graph_subscription::<Dag<Frozen>>()?;
	automatic_graph_subscription::<Hypergraph<Frozen>>()?;
	automatic_graph_subscription::<Metagraph<Frozen>>()?;
	automatic_graph_subscription::<Subgraph<Frozen>>()?;
	Ok(())
}

/// Keep-local replays changed fields and preserves independent remote fields.
#[test]
fn graph_local_field_replay_preserves_remote_fields_and_checks_admission() -> FerrosGrynnResult<()>
{
	use crate::live::{LiveGraphDelta, LiveGraphScopeId};
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source)
		.with_graph_result(LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge)));
	let base = Arc::new(
		evaluate(&store, &query)?
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("base image absent"))?,
	);
	let atom = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	let mut local = base.as_ref().clone();
	local
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?
		.properties
		.insert("score", Value::from(99));
	let local = Arc::new(local);
	let changes = LiveGraphDelta::between(
		Some(Arc::clone(&base)),
		Arc::clone(&local),
		1.into(),
		2.into(),
		&|_, _| Ok(()),
	)?;
	let mut remote = base.as_ref().clone();
	let node = remote
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("remote node absent"))?;
	node.properties.insert("score", Value::from(10));
	node.properties.insert("tags", Value::Array(vec![Value::from(7)].into()));
	let merged = crate::live::LiveGraphImage::merge_local(
		&base,
		&remote,
		&local,
		changes.changes(),
		&mut |_, _| Ok(()),
	)?;
	let node = merged
		.scopes
		.get(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("merged node absent"))?;
	assert_eq!(node.properties.get("score"), Some(&Value::from(99)));
	assert_eq!(node.properties.get("tags"), Some(&Value::Array(vec![Value::from(7)].into())));
	assert!(
		crate::live::LiveGraphImage::merge_local(
			&base,
			&remote,
			&local,
			changes.changes(),
			&mut |_, _| Err(FerrosGrynnError::traversal_resource_limit_exceeded("replay", 0))
		)
		.is_err()
	);
	let mut transport_only = base.as_ref().clone();
	for key in transport_only.property_keys.values_mut() {
		*key = crate::values::PropertyKeyId::new(key.get() + 100);
	}
	assert!(
		LiveGraphDelta::between(
			Some(Arc::clone(&base)),
			Arc::new(transport_only),
			1.into(),
			2.into(),
			&|_, _| Ok(())
		)?
		.changes()
		.is_empty()
	);
	let mut mapping = base.as_ref().clone();
	mapping
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("mapped node absent"))?
		.source = None;
	assert_eq!(
		LiveGraphDelta::between(Some(base), Arc::new(mapping), 1.into(), 2.into(), &|_, _| Ok(()))?
			.changes()
			.len(),
		1
	);
	Ok(())
}

#[test]
fn mutable_graph_sdk_conflict_branch_keeps_decisions_and_blocks_ack() -> FerrosGrynnResult<()> {
	use crate::{
		graph::{DirtyTarget, Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score key absent"))?;
	subscription.materialization()?.mutate_local(Vec::<DirtyTarget>::new(), |graph| {
		let local = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?;
		graph.node_properties_mut(local)?.insert(key, 99);
		Ok(())
	})?;
	let mut write = store.begin();
	write.set_base_property(&"person:right".parse()?, key, Value::from(20))?;
	let disjoint = write.commit()?.versionstamp();
	assert!(
		matches!(subscription.next()?, LiveGraphNext::Updated { revision } if revision == disjoint)
	);
	let value = |subscription: &crate::live::LiveGraphSubscription<
		crate::graph::Mutable,
		Graph<crate::graph::Mutable>,
	>|
	 -> FerrosGrynnResult<Option<Value>> {
		subscription.materialization()?.with_graph(|graph| {
			let local = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			Ok(graph.node(local).and_then(|node| node.properties().get(key)).cloned())
		})?
	};
	assert_eq!(value(&subscription)?, Some(Value::from(99)));
	let mut write = store.begin();
	write.set_base_property(&"person:left".parse()?, key, Value::from(10))?;
	let incoming = write.commit()?.versionstamp();
	let LiveGraphNext::Conflict(conflict) = subscription.next()? else {
		return Err(FerrosGrynnError::invalid_argument("conflict branch absent"));
	};
	assert_eq!(conflict.remote_revision(), incoming);
	assert_eq!(conflict.conflicts().len(), 1);
	assert_eq!(subscription.cursor().versionstamp(), disjoint);
	assert_eq!(value(&subscription)?, Some(Value::from(99)));
	assert!(subscription.resume().is_err());
	assert!(matches!(subscription.next()?, LiveGraphNext::Idle));
	assert!(
		subscription
			.rebase(|_, _| Err(FerrosGrynnError::invalid_argument("invalid explicit merge")))
			.is_err()
	);
	assert!(subscription.conflict().is_some());
	assert_eq!(value(&subscription)?, Some(Value::from(99)));
	subscription.materialization()?.mutate_local([], |graph| {
		let local = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(local)?.insert(key, 120);
		Ok(())
	})?;
	assert!(
		matches!(subscription.keep_local()?, LiveGraphNext::Updated { revision } if revision == incoming)
	);
	assert_eq!(subscription.cursor().versionstamp(), incoming);
	assert_eq!(value(&subscription)?, Some(Value::from(120)));
	let mut write = store.begin();
	write.set_base_property(&"person:left".parse()?, key, Value::from(60))?;
	write.commit()?;
	assert!(matches!(subscription.next()?, LiveGraphNext::Conflict(_)));
	subscription.accept_remote()?;
	assert_eq!(value(&subscription)?, Some(Value::from(60)));
	assert!(subscription.conflict().is_none());
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_closed_resolution_preserves_local_state_and_unconfirmed_token()
-> FerrosGrynnResult<()> {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let mut write = store.begin();
	write.set_base_property(left.id(), key, Value::from(10))?;
	write.commit()?;
	assert!(matches!(subscription.next()?, LiveGraphNext::Conflict(_)));
	let before = subscription.materialization()?.static_snapshot()?;
	let revision = subscription.revision();
	subscription.close()?;
	assert!(subscription.accept_remote().is_err());
	assert!(subscription.keep_local().is_err());
	let mut called = false;
	assert!(
		subscription
			.rebase(|_, _| {
				called = true;
				Ok(())
			})
			.is_err()
	);
	assert!(!called);
	assert!(subscription.conflict().is_some());
	assert_eq!(subscription.revision(), revision);
	assert_eq!(subscription.materialization()?.static_snapshot()?, before);
	assert!(matches!(subscription.next()?, LiveGraphNext::Closed));
	// An applied-but-unconfirmed token remains available when its consumer
	// closes.
	subscription.unconfirmed = Some(subscription.cursor().clone());
	assert!(matches!(subscription.next()?, LiveGraphNext::Closed));
	assert!(subscription.applied_unconfirmed().is_some());
	Ok(())
}

#[test]
fn mutable_graph_event_wait_cancel_preserves_pending_conflict_and_ack() -> FerrosGrynnResult<()> {
	use std::{
		future::Future,
		pin::pin,
		task::{Context, Poll, Wake, Waker},
	};

	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	struct Noop;
	impl Wake for Noop {
		fn wake(self: Arc<Self>) {}
	}
	let waker = Waker::from(Arc::new(Noop));
	let mut context = Context::from_waker(&waker);
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let cursor = subscription.cursor().clone();
	{
		let mut waiting = pin!(subscription.next_event());
		assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));
	}
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let mut write = store.begin();
	write.set_base_property(left.id(), key, Value::from(10))?;
	let incoming = write.commit()?.versionstamp();
	{
		let mut waiting = pin!(subscription.next_event());
		let Poll::Ready(Ok(LiveGraphNext::Conflict(conflict))) =
			waiting.as_mut().poll(&mut context)
		else {
			return Err(FerrosGrynnError::invalid_argument("typed conflict branch absent"));
		};
		assert_eq!(conflict.remote_revision(), incoming);
	}
	{
		let mut waiting = pin!(subscription.next_event());
		assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));
	}
	assert_eq!(subscription.cursor(), &cursor);
	assert!(subscription.conflict().is_some());
	subscription.discard_local()?;
	assert_eq!(subscription.cursor().versionstamp(), incoming);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_interned_key_survives_remote_updates_before_use() -> FerrosGrynnResult<()> {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _root, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let draft = subscription.intern_property("draft")?;
	let score = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
	let right: RecordId = "person:right".parse()?;
	let mut write = store.begin();
	write.set_base_property(&right, score, Value::from(20))?;
	write.commit()?;
	assert!(matches!(subscription.next()?, LiveGraphNext::Updated { .. }));
	assert_eq!(subscription.property_key("draft"), Some(draft));
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(draft, "saved");
		Ok(())
	})?;
	let mut write = store.begin();
	write.set_base_property(&right, score, Value::from(21))?;
	write.commit()?;
	assert!(matches!(subscription.next()?, LiveGraphNext::Updated { .. }));
	subscription.materialization()?.with_graph(|graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		assert_eq!(
			graph.node(id).and_then(|node| node.properties().get(draft)),
			Some(&Value::from("saved"))
		);
		Ok::<(), FerrosGrynnError>(())
	})??;
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_disjoint_topology_changes_produce_structural_conflict() -> FerrosGrynnResult<()> {
	use crate::{
		datastore::OccurrenceState,
		graph::{Dag, Frozen, GraphRefReader},
		live::LiveGraphNext,
	};
	let (store, source, _root, first_edge) = fixture()?;
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return Err(FerrosGrynnError::invalid_argument("graph source absent"));
	};
	let graph = graph.clone();
	let mut seed = store.begin();
	let c: RecordId = "person:c".parse()?;
	let d: RecordId = "person:d".parse()?;
	let second: RecordId = "link:second".parse()?;
	for record in [&c, &d, &second] {
		seed.create_atom(record.clone(), PropertyMap::new())?;
	}
	let c_id = seed.attach_node(graph.id(), &c, None, None)?;
	let d_id = seed.attach_node(graph.id(), &d, None, None)?;
	seed.attach_edge(
		graph.id(),
		&second,
		None,
		EdgeTopology::directed([c_id], [d_id]),
		None,
		None,
	)?;
	seed.commit()?;
	let second_edge = Value::from(AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Edge, second.clone()),
		Arc::new(graph.clone()),
	));
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(
			LiveExpr::field("id")?,
			LiveExpr::Array(vec![LiveExpr::Literal(first_edge), LiveExpr::Literal(second_edge)]),
		)
		.with_family(GraphKind::Dag),
	);
	let mut subscription =
		store.subscribe_live_graph::<Dag<Frozen>>(query, LiveOptions::default())?.into_mutable()?;
	let cursor = subscription.cursor().clone();
	subscription.materialization()?.mutate_local([], |local| {
		let lookup = |kind, record: RecordId| {
			local
				.atom_id(&AtomRef::new(kind, record))?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local target absent"))
		};
		let edge = lookup(AtomKind::Edge, "link:edge".parse()?)?;
		let b = lookup(AtomKind::Node, "person:right".parse()?)?;
		let d = lookup(AtomKind::Node, d.clone())?;
		local.replace_edge_incidence(edge, d, b)
	})?;
	let mut write = store.begin();
	let b = write
		.read_graph(&graph)?
		.atom_id(&AtomRef::new(AtomKind::Node, "person:right".parse()?))?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source node absent"))?;
	write.replace_occurrence_state(graph.id(), &second, OccurrenceState::Edge {
		topology: EdgeTopology::directed([b], [d_id]),
		weight: None,
		transition_graph: None,
	})?;
	let incoming = write.commit()?.versionstamp();
	let LiveGraphNext::Conflict(conflict) = subscription.next()? else {
		return Err(FerrosGrynnError::invalid_argument("structural conflict branch absent"));
	};
	assert!(conflict.diagnostic().is_some());
	assert_eq!(conflict.remote_revision(), incoming);
	assert_eq!(subscription.cursor(), &cursor);
	assert!(subscription.keep_local().is_err());
	assert!(subscription.conflict().is_some());
	subscription.accept_remote()?;
	assert_eq!(subscription.cursor().versionstamp(), incoming);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_uses_occurrence_mapping_and_preserves_later_edits() -> FerrosGrynnResult<()>
{
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _, edge) = fixture()?;
	let LiveSource::GraphClass {
		graph: source_graph,
		..
	} = &source
	else {
		return Err(FerrosGrynnError::invalid_argument("source absent"));
	};
	let source_graph = source_graph.clone();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	let edit = |value| {
		subscription.materialization()?.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, value);
			Ok(())
		})
	};
	edit(99)?;
	let old_cursor = subscription.cursor().clone();
	let mut disjoint = store.begin();
	disjoint.set_base_property(&"person:right".parse()?, key, 20.into())?;
	disjoint.commit()?;
	let committed = subscription.flush(store.begin())?;
	assert_eq!(subscription.cursor(), &old_cursor);
	assert_eq!(
		store
			.begin()
			.atom(left.id())?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("base absent"))?
			.properties()
			.get(key),
		Some(&Value::from(1))
	);
	assert_eq!(
		store.begin().effective_property(source_graph.id(), left.id(), key)?,
		Some(Value::from(99))
	);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 120);
		Ok(())
	})?;
	for _ in 0..3 {
		if matches!(subscription.next()?, LiveGraphNext::Idle) {
			break;
		}
	}
	assert_eq!(subscription.cursor().versionstamp(), committed.versionstamp());
	subscription.materialization()?.with_graph(|graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		assert_eq!(
			graph.node(id).and_then(|node| node.properties().get(key)),
			Some(&Value::from(120))
		);
		Ok::<(), FerrosGrynnError>(())
	})??;
	subscription.flush(store.begin())?;
	assert!(matches!(subscription.next()?, LiveGraphNext::Updated { .. }));
	assert_eq!(
		store.begin().effective_property(source_graph.id(), left.id(), key)?,
		Some(Value::from(120))
	);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_rejects_stale_and_foreign_sources_before_publication()
-> FerrosGrynnResult<()> {
	use crate::graph::{Frozen, Graph, GraphMut};
	let (store, source, _, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let old = subscription.materialization()?.static_snapshot()?;
	let foreign = Datastore::in_memory();
	assert!(subscription.flush(foreign.begin()).is_err());
	let mut update = store.begin();
	update.set_base_property(left.id(), key, 10.into())?;
	update.commit()?;
	let stamp = store.inner.current_root().commit_ts;
	assert!(subscription.flush(store.begin()).is_err());
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	assert_eq!(subscription.materialization()?.static_snapshot()?, old);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_source_prerequisites_survive_every_isolation_commit_race()
-> FerrosGrynnResult<()> {
	use crate::{
		datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
		graph::{Frozen, Graph},
		live::{LiveGraphDelta, LiveGraphScopeId},
	};
	for isolation in
		[IsolationLevel::ReadCommitted, IsolationLevel::Snapshot, IsolationLevel::Serializable]
	{
		let (store, source, _, edge) = fixture()?;
		let query = LiveQuery::new(source).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
				.with_family(GraphKind::Graph),
		);
		let mut subscription = store
			.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
			.into_mutable()?;
		let before = subscription
			.remote
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("remote absent"))?;
		let key = subscription
			.property_key("score")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
		let record: RecordId = "person:left".parse()?;
		let mut local = before.as_ref().clone();
		local
			.scopes
			.get_mut(&LiveGraphScopeId::Root)
			.and_then(|scope| scope.nodes.get_mut(&AtomRef::new(AtomKind::Node, record.clone())))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?
			.properties
			.insert("score", Value::from(99));
		let local = Arc::new(local);
		let changes = LiveGraphDelta::between(
			Some(Arc::clone(before)),
			Arc::clone(&local),
			0.into(),
			1.into(),
			&|_, _| Ok(()),
		)?;
		let mut write =
			store.begin_with(TransactionOptions::new(isolation, TransactionAccess::ReadWrite));
		write.flush_live_graph(
			&subscription.subscription.inner,
			subscription.cursor(),
			before,
			&local,
			changes.changes(),
			&LiveOptions::default(),
		)?;
		let mut racing = store.begin();
		racing.set_base_property(&record, key, 20.into())?;
		let stamp = racing.commit()?.versionstamp();
		assert!(
			write.commit_live_graph(subscription.cursor()).is_err(),
			"isolation {isolation:?} lost source prerequisite"
		);
		assert_eq!(store.inner.current_root().commit_ts, stamp.get());
		assert_eq!(
			store.begin().atom(&record)?.and_then(|atom| atom.properties().get(key).cloned()),
			Some(Value::from(20))
		);
		subscription.close()?;
	}
	Ok(())
}

#[test]
fn graph_resnapshot_replaces_incarnation_and_preserves_old_guards() -> FerrosGrynnResult<()> {
	use crate::{
		graph::{Frozen, Graph},
		live::{LiveGraphNext, LiveState},
	};
	let (store, source, _, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut old = store.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?;
	let guard = old.materialization()?.read()?;
	let old_id = old.id();
	let old_revision = guard.revision();
	let mut update = store.begin();
	let key = update.intern_property("score")?;
	update.set_base_property(&"person:left".parse()?, key, 10.into())?;
	update.commit()?;
	let mut replacement = old.resnapshot(&store)?;
	assert_ne!(replacement.id(), old_id);
	assert_eq!(store.begin().live_info(old_id)?.state(), LiveState::Closed);
	assert_eq!(guard.revision(), old_revision);
	assert!(matches!(old.next()?, LiveGraphNext::Closed));
	assert_eq!(replacement.materialization()?.read()?.identity(), replacement.id().as_uuid());
	replacement.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_does_not_replay_earlier_remote_values_over_event_effects()
-> FerrosGrynnResult<()> {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _, edge) = fixture()?;
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return Err(FerrosGrynnError::invalid_argument("source absent"));
	};
	let mut definition = store.begin();
	definition.define_event(EventDefinition::new(
		"after_flush",
		EventTarget::Graph(graph.id()),
		"$event = 'UPDATE' AND $after.score = 99",
		"{ UPDATE person:right SET score = 22; }",
	)?)?;
	definition.commit()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let mut prior = store.begin();
	prior.set_base_property(&"person:right".parse()?, key, 20.into())?;
	prior.commit()?;
	subscription.flush(store.begin())?;
	for _ in 0..3 {
		if matches!(subscription.next()?, LiveGraphNext::Idle) {
			break;
		}
	}
	subscription.materialization()?.with_graph(|graph| {
		let id = graph
			.atom_id(&AtomRef::new(AtomKind::Node, "person:right".parse()?))?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("right absent"))?;
		assert_eq!(
			graph.node(id).and_then(|node| node.properties().get(key)),
			Some(&Value::from(22))
		);
		Ok::<(), FerrosGrynnError>(())
	})??;
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_resolution_of_earlier_delivery_discards_old_local_decisions()
-> FerrosGrynnResult<()> {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	for resolution in 0..3 {
		let (store, source, _, edge) = fixture()?;
		let LiveSource::GraphClass {
			graph,
			..
		} = &source
		else {
			return Err(FerrosGrynnError::invalid_argument("source absent"));
		};
		let mut definition = store.begin();
		definition.define_event(EventDefinition::new(
			"after_flush",
			EventTarget::Graph(graph.id()),
			"$event = 'UPDATE' AND $after.score = 99",
			"{ UPDATE person:right SET score = 22; }",
		)?)?;
		definition.commit()?;
		let query = LiveQuery::new(source).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
				.with_family(GraphKind::Graph),
		);
		let mut subscription = store
			.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
			.into_mutable()?;
		let key = subscription
			.property_key("score")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
		let edit = |record: &str, score| -> FerrosGrynnResult<()> {
			subscription.materialization()?.mutate_local([], |graph| {
				let id = graph
					.atom_id(&AtomRef::new(AtomKind::Node, record.parse()?))?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?;
				graph.node_properties_mut(id)?.insert(key, score);
				Ok(())
			})
		};
		edit("person:left", 99)?;
		let mut prior = store.begin();
		prior.set_base_property(&"person:right".parse()?, key, 20.into())?;
		prior.commit()?;
		subscription.flush(store.begin())?;
		subscription.materialization()?.mutate_local([], |graph| {
			let id = graph
				.atom_id(&AtomRef::new(AtomKind::Node, "person:right".parse()?))?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("right absent"))?;
			graph.node_properties_mut(id)?.insert(key, 30);
			Ok(())
		})?;
		assert!(matches!(subscription.next()?, LiveGraphNext::Conflict(_)));
		match resolution {
			0 => subscription.accept_remote()?,
			1 => subscription.discard_local()?,
			_ => subscription.rebase(|_, _| Ok(()))?,
		};
		for _ in 0..3 {
			if matches!(subscription.next()?, LiveGraphNext::Idle) {
				break;
			}
		}
		subscription.materialization()?.with_graph(|graph| {
			for (record, score) in [("person:left", 99), ("person:right", 22)] {
				let id = graph
					.atom_id(&AtomRef::new(AtomKind::Node, record.parse()?))?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("accepted node absent"))?;
				assert_eq!(
					graph.node(id).and_then(|node| node.properties().get(key)),
					Some(&Value::from(score))
				);
			}
			Ok::<(), FerrosGrynnError>(())
		})??;
		subscription.close()?;
	}
	Ok(())
}

#[test]
fn mutable_graph_flush_structural_conflict_uses_manual_resolution_branch() -> FerrosGrynnResult<()>
{
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Dag, Frozen, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _, first_edge) = fixture()?;
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return Err(FerrosGrynnError::invalid_argument("source absent"));
	};
	let graph = graph.clone();
	let mut seed = store.begin();
	for name in ["c", "d"] {
		let record: RecordId = format!("person:{name}").parse()?;
		seed.create_atom(record.clone(), PropertyMap::new())?;
		seed.attach_node(graph.id(), &record, None, None)?;
	}
	seed.define_event(EventDefinition::new(
		"flush_topology",
		EventTarget::Graph(graph.id()),
		"$event = 'UPDATE' AND $after.score = 99",
		"{ DETACH link:edge FROM GRAPH source; RELATE (person:d)-[link:edge]->(person:c) IN GRAPH \
		 source; }",
	)?)?;
	seed.commit()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(first_edge))
			.with_family(GraphKind::Dag),
	);
	let mut subscription =
		store.subscribe_live_graph::<Dag<Frozen>>(query, LiveOptions::default())?.into_mutable()?;
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))?;
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&AtomRef::new(AtomKind::Node, "person:left".parse()?))?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let committed = subscription.flush(store.begin())?.versionstamp();
	let cursor = subscription.cursor().clone();
	subscription.materialization()?.mutate_local([], |graph| {
		let lookup = |name: &str| {
			graph
				.atom_id(&AtomRef::new(AtomKind::Node, name.parse()?))?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local endpoint absent"))
		};
		graph.add_edge(lookup("person:c")?, lookup("person:d")?, None, PropertyMap::new())?;
		Ok(())
	})?;
	let LiveGraphNext::Conflict(conflict) = subscription.next()? else {
		return Err(FerrosGrynnError::invalid_argument("flush conflict branch absent"));
	};
	assert_eq!(conflict.remote_revision(), committed);
	assert!(conflict.diagnostic().is_some());
	assert_eq!(subscription.cursor(), &cursor);
	assert!(subscription.keep_local().is_err());
	assert!(subscription.resume().is_err());
	assert!(matches!(subscription.next()?, LiveGraphNext::Idle));
	subscription.accept_remote()?;
	assert_eq!(subscription.cursor().versionstamp(), committed);
	assert!(subscription.conflict().is_none());
	assert!(matches!(subscription.next()?, LiveGraphNext::Idle));
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_charges_private_source_work_before_capture() -> FerrosGrynnResult<()> {
	use crate::graph::{Frozen, Graph, GraphMut};
	let (store, source, _, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
	let record: RecordId = "person:left".parse()?;
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&AtomRef::new(AtomKind::Node, record.clone()))?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let mut transaction = store.begin();
	for id in 0..512 {
		let name = transaction.intern_property(&format!("extra_{id}"))?;
		transaction.set_base_property(&record, name, 1.into())?;
	}
	let limits = LiveOptions::default().with_budgets(8 << 20, 250)?;
	let remote = subscription
		.remote
		.as_ref()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("remote absent"))?;
	let mut local = remote.as_ref().clone();
	local
		.scopes
		.get_mut(&crate::live::LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&AtomRef::new(AtomKind::Node, record.clone())))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local image absent"))?
		.properties
		.insert("score", Value::from(99));
	let local = Arc::new(local);
	let dirty = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(remote)),
		Arc::clone(&local),
		0.into(),
		1.into(),
		&|_, _| Ok(()),
	)?;
	let stamp = store.inner.current_root().commit_ts;
	let error = transaction
		.flush_live_graph(
			&subscription.subscription.inner,
			subscription.cursor(),
			remote,
			&local,
			dirty.changes(),
			&limits,
		)
		.err()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("work limit ignored"))?;
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::TraversalResourceLimitExceeded { .. }
	));
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_rebinds_edge_owned_child_before_connector() -> FerrosGrynnResult<()> {
	use crate::{
		graph::{Frozen, Graph, QualifiedAtomId},
		live::LiveGraphNext,
	};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, [])?)?;
	let root = seed.define_graph("root", GraphKind::Metagraph)?;
	let a = seed.define_graph("a", GraphKind::Graph)?;
	let b = seed.define_graph("b", GraphKind::Graph)?;
	for record in ["item:root", "item:one", "item:two", "link:a", "link:y", "link:z"] {
		seed.create_atom(record.parse()?, PropertyMap::new())?;
	}
	let root_node = seed.attach_node(root.id(), &"item:root".parse()?, None, None)?;
	let terminal = seed.attach_node(a.id(), &"item:one".parse()?, None, None)?;
	seed.attach_node(b.id(), &"item:two".parse()?, None, None)?;
	let owner = seed.attach_edge(
		root.id(),
		&"link:z".parse()?,
		None,
		EdgeTopology::undirected([root_node]),
		None,
		Some(a.id()),
	)?;
	seed.attach_edge(
		root.id(),
		&"link:y".parse()?,
		None,
		EdgeTopology::undirected([root_node]),
		None,
		Some(b.id()),
	)?;
	seed.attach_edge(
		root.id(),
		&"link:a".parse()?,
		None,
		EdgeTopology::directed([QualifiedAtomId::new(vec![owner], terminal)?], [root_node]),
		None,
		None,
	)?;
	seed.commit()?;
	let edges = ["link:a", "link:y", "link:z"]
		.into_iter()
		.map(|record| {
			Ok(LiveExpr::Literal(Value::from(AtomOccurrenceRef::new(
				AtomRef::new(AtomKind::Edge, record.parse()?),
				Arc::new(root.clone()),
			))))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let query = LiveQuery::new(LiveSource::GraphClass {
		graph: root.clone(),
		class,
	})
	.with_graph_result(
		LiveGraphResult::new(
			LiveExpr::Array(vec![
				LiveExpr::field("id")?,
				LiveExpr::Literal(Value::from(AtomOccurrenceRef::new(
					AtomRef::new(AtomKind::Node, "item:two".parse()?),
					Arc::new(b.clone()),
				))),
			]),
			LiveExpr::Array(edges),
		)
		.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	subscription.materialization()?.mutate_local([], |graph| {
		let z = graph
			.atom_id(&AtomRef::new(AtomKind::Edge, "link:z".parse()?))?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("owner absent"))?;
		let y = graph
			.atom_id(&AtomRef::new(AtomKind::Edge, "link:y".parse()?))?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("owner absent"))?;
		let child = graph
			.edge(y)
			.and_then(|edge| edge.transition_graph())
			.cloned()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("child absent"))?;
		graph.replace_transition_graph(z, Some(child))?;
		Ok(())
	})?;
	subscription.flush(store.begin())?;
	assert!(matches!(subscription.next()?, LiveGraphNext::Updated { .. }));
	let root_image = store.inner.current_root();
	let global = root_image
		.resolve_atom(&"link:z".parse()?)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source owner absent"))?;
	let local = root_image
		.membership(root.id(), global)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source occurrence absent"))?;
	assert_eq!(
		root_image.occurrence(root.id(), local).and_then(|owner| owner.child_graph()),
		Some(b.id())
	);
	assert!(
		root_image.resolve_atom(&"item:one".parse()?).is_some(),
		"closure loss must preserve child source data"
	);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_preserves_cached_synthetic_property_handle() -> FerrosGrynnResult<()> {
	use crate::graph::{Frozen, Graph, GraphMut};
	let (store, source, _, edge) = fixture()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let key = subscription.intern_property("draft")?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	subscription.flush(store.begin())?;
	subscription.next()?;
	assert_eq!(subscription.property_key("draft"), Some(key));
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		assert_eq!(graph.node_properties_mut(id)?.remove(key), Some(Value::from(99)));
		Ok(())
	})?;
	subscription.flush(store.begin())?;
	subscription.next()?;
	subscription.close()?;
	Ok(())
}

/// One global source mapping works through every sealed family layout.
fn flush_global_family<G>() -> FerrosGrynnResult<()>
where
	G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
{
	use crate::{graph::GraphMut, live::LiveGraphNext};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let class = seed.define_class(ClassSchema::new("item", AtomKind::Node, [])?)?;
	let key = seed.intern_property("score")?;
	let record: RecordId = "item:one".parse()?;
	seed.create_atom(record.clone(), PropertyMap::from_iter([(key, Value::from(1))]))?;
	seed.commit()?;
	let mut recipe = LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(Value::None))
		.with_family(G::family_kind());
	if G::family_kind() == GraphKind::RootedTree {
		recipe = recipe.with_root(LiveExpr::Literal(Value::from(record.clone())));
	}
	let mut subscription = store
		.subscribe_live_graph::<G>(
			LiveQuery::new(LiveSource::Class(class)).with_graph_result(recipe),
			LiveOptions::default(),
		)?
		.into_mutable()?;
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&AtomRef::new(AtomKind::Node, record.clone()))?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("global node absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let committed = subscription.flush(store.begin())?;
	assert!(
		matches!(subscription.next()?, LiveGraphNext::Updated { revision } if revision == committed.versionstamp())
	);
	assert_eq!(
		store
			.begin()
			.atom(&record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source absent"))?
			.properties()
			.get(key),
		Some(&Value::from(99))
	);
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_all_sealed_families_global_source() -> FerrosGrynnResult<()> {
	use crate::graph::{
		Dag,
		Forest,
		Frozen,
		Graph,
		Hypergraph,
		Metagraph,
		RootedTree,
		Subgraph,
		Tree,
	};
	flush_global_family::<Graph<Frozen>>()?;
	flush_global_family::<Dag<Frozen>>()?;
	flush_global_family::<Forest<Frozen>>()?;
	flush_global_family::<Tree<Frozen>>()?;
	flush_global_family::<RootedTree<Frozen>>()?;
	flush_global_family::<Hypergraph<Frozen>>()?;
	flush_global_family::<Metagraph<Frozen>>()?;
	flush_global_family::<Subgraph<Frozen>>()
}

#[test]
fn mutable_graph_flush_unchanged_result_keeps_checkpoint_watermark() -> FerrosGrynnResult<()> {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
	};
	let (store, source, _, edge) = fixture()?;
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return Err(FerrosGrynnError::invalid_argument("source absent"));
	};
	let mut definition = store.begin();
	definition.define_event(EventDefinition::new(
		"reset",
		EventTarget::Graph(graph.id()),
		"$event = 'UPDATE' AND $after.score = 99",
		"{ UPDATE person:left IN GRAPH source SET score = 1; }",
	)?)?;
	definition.commit()?;
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())?
		.into_mutable()?;
	let original = subscription.revision();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
	let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
	subscription.materialization()?.mutate_local([], |graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		graph.node_properties_mut(id)?.insert(key, 99);
		Ok(())
	})?;
	let committed = subscription.flush(store.begin())?;
	assert!(Some(committed.versionstamp()) > original);
	assert!(matches!(subscription.next()?, LiveGraphNext::Updated { .. }));
	assert_eq!(subscription.revision(), original);
	subscription.materialization()?.with_graph(|graph| {
		let id = graph
			.atom_id(&left)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
		assert_eq!(
			graph.node(id).and_then(|node| node.properties().get(key)),
			Some(&Value::from(1))
		);
		Ok::<(), FerrosGrynnError>(())
	})??;
	subscription.close()?;
	Ok(())
}

#[test]
fn mutable_graph_flush_overflow_keeps_gap_explicit_and_block_atomic() -> FerrosGrynnResult<()> {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::{LiveGraphNext, LiveOverflowPolicy},
	};
	for policy in [LiveOverflowPolicy::Gap, LiveOverflowPolicy::Block] {
		let (store, source, _, edge) = fixture()?;
		let query = LiveQuery::new(source).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id")?, LiveExpr::Literal(edge))
				.with_family(GraphKind::Graph),
		);
		let options = LiveOptions::default()
			.with_queue(4, 64 << 20)?
			.with_overflow(policy)
			.with_block_timeout(std::time::Duration::from_millis(10))?;
		let mut subscription =
			store.subscribe_live_graph::<Graph<Frozen>>(query, options)?.into_mutable()?;
		let key = subscription
			.property_key("score")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))?;
		let left = AtomRef::new(AtomKind::Node, "person:left".parse()?);
		subscription.materialization()?.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})?;
		let mut prior = subscription.cursor().versionstamp();
		for value in [20, 21, 22, 23] {
			let mut write = store.begin();
			write.set_base_property(&"person:right".parse()?, key, value.into())?;
			prior = write.commit()?.versionstamp();
		}
		if policy == LiveOverflowPolicy::Block {
			assert!(subscription.flush(store.begin()).is_err());
			assert_eq!(store.inner.current_root().commit_ts, prior.get());
		} else {
			subscription.flush(store.begin())?;
			assert!(matches!(subscription.next()?, LiveGraphNext::Updated { .. }));
			let revision = subscription.revision();
			assert!(matches!(subscription.next()?, LiveGraphNext::Gap(_)));
			assert_eq!(subscription.revision(), revision);
			let mut fresh = subscription.resnapshot(&store)?;
			fresh.close()?;
		}
		subscription.close()?;
	}
	Ok(())
}
