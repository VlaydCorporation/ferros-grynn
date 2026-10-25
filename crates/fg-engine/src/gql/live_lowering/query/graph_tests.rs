//! SDK graph recipes use ordinary SELECT projection and graph closure.

use fg_runtime::__internal::{DagMutableInternal as _, GraphTopologyConstraintsInternal as _};

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

/// Owned node rows and ordered edge endpoints used by the graph replay oracle.
type FixtureGraphRows = (Vec<(RecordId, PropertyMap)>, Vec<(RecordId, PropertyMap, Vec<RecordId>)>);

/// Returns evaluate for this module.
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

/// Builds the fixture fixture used by nearby scenarios.
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

/// Returns signature for this module.
fn signature(graph: &MaterializedGraph) -> FerrosGrynnResult<FixtureGraphRows> {
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

/// Verifies sdk graph result oracle all families and effective closure.
#[test]
fn sdk_graph_result_oracle_all_families_and_effective_closure() {
	use crate::test_assertions::AssertResult as _;

	let (store, source, root, edge) = fixture().assert_ok();
	let base = LiveQuery::new(source).with_projection(LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::field("id").assert_ok())
			.with_alias(LivePath::field("picked").assert_ok()),
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
		let mut recipe = LiveGraphResult::new(
			LiveExpr::field("picked").assert_ok(),
			LiveExpr::Literal(edge.clone()),
		)
		.with_family(family);
		if family == GraphKind::RootedTree {
			recipe = recipe.with_root(LiveExpr::Literal(root.clone()));
		}
		let actual = evaluate(&store, &base.clone().with_graph_result(recipe)).assert_ok();
		assert!(actual.rows.is_empty());
		let image = actual
			.graph_image
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("logical graph image absent"))
			.assert_ok();
		assert_eq!(image.family(), family);
		let image_root = image
			.scope(&crate::live::LiveGraphScopeId::Root)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("logical root scope absent"))
			.assert_ok();
		assert_eq!(image_root.nodes().len(), 2);
		assert_eq!(image_root.edges().len(), 1);
		assert!(image_root.nodes().all(|(_, node)| node.source().is_some()));
		assert!(image_root.edges().all(|(_, edge)| edge.source().is_some()));
		assert!(image.property_key("score").is_some());
		assert_eq!(image.root().is_some(), family == GraphKind::RootedTree);
		let encoded = crate::storage::live_graph_image::encode(image, 1 << 24).assert_ok();
		assert_eq!(&encoded[..8], b"FGLG\x01\x00\x00\x00");
		assert_eq!(crate::storage::live_graph_image::decode(&encoded, 1 << 24).assert_ok(), *image);
		assert!(crate::storage::live_graph_image::decode(&encoded, 1).is_err());
		for offset in [0, 4, 6, 8, 16, 24, encoded.len() - 1] {
			let mut bad = encoded.clone();
			bad[offset] ^= 255;
			assert!(crate::storage::live_graph_image::decode(&bad, 1 << 24).is_err());
		}
		let actual_graph = actual
			.graph
			.ok_or_else(|| FerrosGrynnError::invalid_argument("SDK graph absent"))
			.assert_ok();
		let variables = Object::from_iter([("root", root.clone()), ("edge", edge.clone())]);
		let result = store
			.execute(
				&format!(
					"SELECT id AS picked FROM person IN GRAPH source RETURN GRAPH {annotation} \
					 {{nodes: picked, edges: $edge}}"
				),
				&variables,
			)
			.assert_ok()
			.remove(0)
			.result
			.assert_ok()
			.into_parts()
			.0;
		let Value::Graph(expected) = result else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("GQL graph absent"));
				test_result.assert_ok();
			};
		};
		let expected_image = crate::live::LiveGraphImage::capture(
			&expected,
			&store.inner.current_root().dictionary,
			&|_, _| Ok(()),
			&|_, _| Ok(None),
		)
		.assert_ok();
		let incarnation = crate::live::LiveId::from_uuid(crate::values::Uuid::now_v7()).assert_ok();
		let restored = crate::storage::live_graph_image::decode(&encoded, 1 << 24)
			.assert_ok()
			.restore(incarnation, 99.into(), &mut |_, _| Ok(()))
			.assert_ok();
		assert_eq!(restored.identity(), incarnation.as_uuid());
		assert_eq!(restored.revision(), 99);
		assert_eq!(restored.kind(), family);
		assert_eq!(signature(&restored).assert_ok(), signature(&expected).assert_ok());
		assert!(
			image
				.restore(incarnation, 99.into(), &mut |_, _| Err(
					FerrosGrynnError::traversal_resource_limit_exceeded("restore", 0)
				))
				.is_err()
		);
		let expected_scope = expected_image
			.scope(&crate::live::LiveGraphScopeId::Root)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle logical scope absent"))
			.assert_ok();
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
		assert_eq!(signature(&actual_graph).assert_ok(), signature(&expected).assert_ok());
		assert!(actual.reads.iter().any(|key| matches!(key, ConflictKey::GraphIncidence(_, _))));
	}
}

/// Verifies sdk graph result join split group share the projected domain.
#[test]
fn sdk_graph_result_join_split_group_share_the_projected_domain() {
	use crate::test_assertions::AssertResult as _;

	let (store, source, _root, _edge) = fixture().assert_ok();
	let nodes = LiveExpr::field("picked").assert_ok();
	let base = LiveQuery::new(source.clone()).with_projection(LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::field("id").assert_ok())
			.with_alias(LivePath::field("picked").assert_ok()),
	]));
	let joined = base
		.clone()
		.with_alias("p")
		.assert_ok()
		.with_joins(vec![LiveJoin::new(source, "q", LiveJoinKind::Cross, None).assert_ok()])
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::Field(
				LivePath::new(vec!["p".into(), "id".into()]).assert_ok(),
			))
			.with_alias(LivePath::field("picked").assert_ok()),
		]));
	let split = base.clone().with_split(vec![LiveExpr::Field(
		LivePath::new(vec!["properties".into(), "tags".into()]).assert_ok(),
	)]);
	let group = base.with_group_all().with_projection(LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::Call {
			name: "array::group".into(),
			arguments: vec![LiveExpr::field("id").assert_ok()],
		})
		.with_alias(LivePath::field("picked").assert_ok()),
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
		})
		.assert_ok();
		let graph = result
			.graph
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph result absent"))
			.assert_ok();
		assert_eq!(graph.node_count(), 2);
		assert_eq!(graph.edge_count(), 0);
	}
}

/// Verifies sdk graph result closure base fanout and registration reads.
#[test]
fn sdk_graph_result_closure_base_fanout_and_registration_reads() {
	use crate::test_assertions::AssertResult as _;

	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("id").assert_ok())
				.with_alias(LivePath::field("picked").assert_ok()),
		]))
		.with_graph_result(LiveGraphResult::new(
			LiveExpr::field("picked").assert_ok(),
			LiveExpr::Literal(edge),
		));
	let first = evaluate(&store, &query).assert_ok();
	let mut registration = store.begin();
	registration.register_live(query.clone(), LiveOptions::default()).assert_ok();
	let mut write = store.begin();
	let key = write.intern_property("score").assert_ok();
	let edge: RecordId = "link:edge".parse().assert_ok();
	let global = store
		.inner
		.current_root()
		.atom_by_record(&edge)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("edge absent"))
		.assert_ok()
		.id();
	assert!(first.reads.contains(&ConflictKey::BaseAtom(global)));
	write.set_base_property(&edge, key, Value::from(77)).assert_ok();
	write.commit().assert_ok();
	assert!(registration.commit().is_err());
	let second = evaluate(&store, &query).assert_ok();
	let before = first
		.graph
		.ok_or_else(|| FerrosGrynnError::invalid_argument("first graph absent"))
		.assert_ok();
	let after = second
		.graph
		.ok_or_else(|| FerrosGrynnError::invalid_argument("second graph absent"))
		.assert_ok();
	assert_ne!(signature(&before).assert_ok(), signature(&after).assert_ok());
	assert_eq!(before.node_count(), after.node_count());
	assert_eq!(before.edge_count(), after.edge_count());
}

/// Verifies graph result logical delta tracks properties topology and deleted
/// targets.
#[test]
fn graph_result_logical_delta_tracks_properties_topology_and_deleted_targets() {
	use crate::test_assertions::AssertResult as _;

	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("id").assert_ok())
				.with_alias(LivePath::field("picked").assert_ok()),
		]))
		.with_graph_result(LiveGraphResult::new(
			LiveExpr::field("picked").assert_ok(),
			LiveExpr::Literal(edge),
		));
	let image = Arc::new(
		evaluate(&store, &query)
			.assert_ok()
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("graph image absent"))
			.assert_ok(),
	);
	assert!(image.estimated_heap_bytes().assert_ok() > 0);
	let unchanged = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(&image)),
		Arc::clone(&image),
		1.into(),
		2.into(),
		&|_, _| Ok(()),
	)
	.assert_ok();
	assert!(unchanged.changes().is_empty());
	let mut changed = image.as_ref().clone();
	let scope = changed
		.scopes
		.get_mut(&crate::live::LiveGraphScopeId::Root)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("root absent"))
		.assert_ok();
	let node = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	let edge = AtomRef::new(AtomKind::Edge, "link:edge".parse().assert_ok());
	scope
		.nodes
		.get_mut(&node)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("node absent"))
		.assert_ok()
		.properties
		.insert("score", Value::from(99));
	scope
		.edges
		.get_mut(&edge)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("edge absent"))
		.assert_ok()
		.direction = crate::graph::EdgeDirection::Undirected;
	let delta = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(&image)),
		Arc::new(changed),
		2.into(),
		3.into(),
		&|_, _| Ok(()),
	)
	.assert_ok();
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
		.ok_or_else(|| FerrosGrynnError::invalid_argument("root absent"))
		.assert_ok()
		.edges
		.remove(&edge);
	let deletion = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(&image)),
		Arc::new(removed),
		3.into(),
		4.into(),
		&|_, _| Ok(()),
	)
	.assert_ok();
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
}

/// Activation is immutable even when a tail replaces the root before SDK use.
#[test]
fn graph_result_publication_keeps_initial_checkpoint_and_whole_commit_ack() {
	use crate::{
		live::{LiveNext, LivePayload},
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("id").assert_ok())
				.with_alias(LivePath::field("picked").assert_ok()),
		]))
		.with_graph_result(LiveGraphResult::new(
			LiveExpr::field("picked").assert_ok(),
			LiveExpr::Literal(edge),
		));
	let mut subscription = store.subscribe_live(query.clone(), LiveOptions::default()).assert_ok();
	let registered = subscription.cursor().versionstamp();
	let oracle = evaluate(&store, &query)
		.assert_ok()
		.graph_image
		.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle image absent"))
		.assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveNext::Idle));
	let mut write = store.begin();
	let key = write.intern_property("score").assert_ok();
	write.set_base_property(&"person:left".parse().assert_ok(), key, Value::from(77)).assert_ok();
	let changed = write.commit().assert_ok().versionstamp();
	let initial = subscription
		.inner
		.take_graph_checkpoint()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("activation checkpoint absent"))
		.assert_ok();
	assert_eq!(initial.versionstamp(), registered);
	assert_eq!(initial.delta().after(), &oracle);
	let LiveNext::Delivery(delivery) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("graph delta absent"));
			test_result.assert_ok();
		};
	};
	let LivePayload::Graph(batch) = delivery.payload() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("graph publication faulted"));
			test_result.assert_ok();
		};
	};
	assert_eq!(batch.versionstamp(), changed);
	assert_eq!(batch.delta().base(), registered);
	assert_eq!(batch.delta().before(), Some(&oracle));
	let after = evaluate(&store, &query)
		.assert_ok()
		.graph_image
		.ok_or_else(|| FerrosGrynnError::invalid_argument("new oracle absent"))
		.assert_ok();
	assert_eq!(batch.delta().after(), &after);
	let changed_node = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	let node = batch
		.delta()
		.after()
		.scopes
		.get(&crate::live::LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get(&changed_node))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("changed node absent"))
		.assert_ok();
	assert_eq!(
		node.source()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source mapping absent"))
			.assert_ok()
			.revision(),
		changed
	);
	let repeated = subscription.next().assert_ok();
	assert!(
		matches!(repeated, LiveNext::Delivery(repeated) if repeated.token() == delivery.token())
	);
	subscription.ack(delivery.token()).assert_ok();
	assert_eq!(subscription.cursor().versionstamp(), changed);
	assert!(matches!(subscription.next().assert_ok(), LiveNext::Idle));
	subscription.close().assert_ok();
}

/// Verifies graph result nested scope validators are owned and diffed.
#[test]
fn graph_result_nested_scope_validators_are_owned_and_diffed() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("doc", AtomKind::Node, []).assert_ok()).assert_ok();
	let root = seed.define_graph("root", GraphKind::Metagraph).assert_ok();
	let child = seed.define_graph("child", GraphKind::Graph).assert_ok();
	let owner: RecordId = "module:owner".parse().assert_ok();
	let document: RecordId = "doc:one".parse().assert_ok();
	seed.create_atom(owner.clone(), PropertyMap::new()).assert_ok();
	seed.create_atom(document.clone(), PropertyMap::new()).assert_ok();
	seed.attach_node(child.id(), &document, None, None).assert_ok();
	seed.attach_node(root.id(), &owner, None, Some(child.id())).assert_ok();
	seed.commit().assert_ok();
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
		evaluate(&store, &query)
			.assert_ok()
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("first image absent"))
			.assert_ok(),
	);
	let mut constraints = crate::graph::GraphTopologyConstraints::empty();
	constraints.insert(crate::graph::GraphTopologyConstraint::Simple);
	let mut alter = store.begin();
	alter.replace_graph_definition("child", None, constraints, None).assert_ok();
	alter.commit().assert_ok();
	let after = Arc::new(
		evaluate(&store, &query)
			.assert_ok()
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("second image absent"))
			.assert_ok(),
	);
	let child_key = crate::live::LiveGraphScopeId::Source(child.id());
	assert_ne!(
		before
			.scope(&child_key)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("first child absent"))
			.assert_ok()
			.validators(),
		after
			.scope(&child_key)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("second child absent"))
			.assert_ok()
			.validators()
	);
	let delta =
		crate::live::LiveGraphDelta::between(Some(before), after, 1.into(), 2.into(), &|_, _| {
			Ok(())
		})
		.assert_ok();
	assert!(delta.changes().contains(&crate::live::LiveGraphTarget::Graph(child_key)));
}

/// Retained images share the same aggregate cap in publication and recovery.
#[test]
fn graph_result_checkpoint_retained_admission_rejects_without_losing_authority() {
	use crate::{
		live::{LiveGraphBatch, LiveGraphDelta, LiveGraphScopeId, LiveId, LiveVersion},
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, _edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(LiveGraphResult::new(
		LiveExpr::field("id").assert_ok(),
		LiveExpr::Literal(Value::None),
	));
	let mut image = evaluate(&store, &query)
		.assert_ok()
		.graph_image
		.ok_or_else(|| FerrosGrynnError::invalid_argument("image absent"))
		.assert_ok();
	let scope = image
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("root scope absent"))
		.assert_ok();
	let node = scope
		.nodes
		.values_mut()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("node absent"))
		.assert_ok();
	node.properties.insert("payload", Value::from("x".repeat(1 << 20)));
	let image = Arc::new(image);
	let mut catalog = crate::datastore::LiveCatalog::default();
	let mut rejected = false;
	for ordinal in 1..=512 {
		let id = LiveId::from_uuid(uuid::Uuid::from_u128(ordinal).into()).assert_ok();
		let batch = Arc::new(LiveGraphBatch {
			id,
			version: LiveVersion::default(),
			committed_at: DateTime::from(
				jiff::Timestamp::from_nanosecond(100).assert_ok().to_zoned(jiff::tz::TimeZone::UTC),
			),
			delta: LiveGraphDelta::between(
				None,
				Arc::clone(&image),
				0.into(),
				1.into(),
				&|_, _| Ok(()),
			)
			.assert_ok(),
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
		assert_eq!(catalog.graph_bytes, bytes + batch.estimated_heap_bytes().assert_ok());
		catalog.replace_graph_checkpoint(id, batch).assert_ok();
		assert_eq!(catalog.graph_checkpoints.len(), count + 1);
	}
	assert!(rejected);
	assert!(catalog.graph_bytes <= 256 * 1024 * 1024);
}

/// Every sealed family accepts whole revisions and keeps immutable old guards.
fn automatic_graph_subscription<G>() -> FerrosGrynnResult<()>
where
	G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
{
	use crate::{
		live::LiveGraphNext,
		test_assertions::{AssertComparison as _, AssertCondition as _},
	};
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
	(&(original.node_count()?), &(2)).assert_equal();
	(&(original.edge_count()?), &(1)).assert_equal();
	(&(original.identity()), &(subscription.id().as_uuid())).assert_equal();
	let mut write = store.begin();
	let key = write.intern_property("score")?;
	write.set_base_property(&"person:left".parse()?, key, Value::from(99))?;
	let stamp = write.commit()?.versionstamp();
	(matches!(subscription.next()?, LiveGraphNext::Updated { revision } if revision == stamp))
		.assert_true();
	(&(subscription.cursor().versionstamp()), &(stamp)).assert_equal();
	(subscription.applied_unconfirmed().is_none()).assert_true();
	(&(subscription.materialization()?.read()?.revision()), &(stamp.get())).assert_equal();
	(&(original.revision()), &(first.get())).assert_equal();
	(&(original.identity()), &(subscription.materialization()?.read()?.identity())).assert_equal();
	(matches!(subscription.next()?, LiveGraphNext::Idle)).assert_true();
	let saved = subscription.cursor().clone();
	drop(subscription);
	let resumed = store.resume_live_graph::<G>(&saved)?;
	(&(resumed.revision()), &(Some(stamp))).assert_equal();
	(&(resumed.materialization()?.read()?.node_count()?), &(2)).assert_equal();
	let mut mutable = resumed.into_mutable()?;
	(&(mutable.materialization()?.static_snapshot()?.node_count()?), &(2)).assert_equal();
	mutable.close()?;
	(&(original.revision()), &(first.get())).assert_equal();
	Ok(())
}

/// Verifies automatic live graph sdk all families update resume and old guards.
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
fn graph_local_field_replay_preserves_remote_fields_and_checks_admission() {
	use crate::{
		live::{LiveGraphDelta, LiveGraphScopeId},
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(LiveGraphResult::new(
		LiveExpr::field("id").assert_ok(),
		LiveExpr::Literal(edge),
	));
	let base = Arc::new(
		evaluate(&store, &query)
			.assert_ok()
			.graph_image
			.ok_or_else(|| FerrosGrynnError::invalid_argument("base image absent"))
			.assert_ok(),
	);
	let atom = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	let mut local = base.as_ref().clone();
	local
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))
		.assert_ok()
		.properties
		.insert("score", Value::from(99));
	let local = Arc::new(local);
	let changes = LiveGraphDelta::between(
		Some(Arc::clone(&base)),
		Arc::clone(&local),
		1.into(),
		2.into(),
		&|_, _| Ok(()),
	)
	.assert_ok();
	let mut remote = base.as_ref().clone();
	let node = remote
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("remote node absent"))
		.assert_ok();
	node.properties.insert("score", Value::from(10));
	node.properties.insert("tags", Value::Array(vec![Value::from(7)].into()));
	let merged = crate::live::LiveGraphImage::merge_local(
		&base,
		&remote,
		&local,
		changes.changes(),
		&mut |_, _| Ok(()),
	)
	.assert_ok();
	let node = merged
		.scopes
		.get(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("merged node absent"))
		.assert_ok();
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
		)
		.assert_ok()
		.changes()
		.is_empty()
	);
	let mut mapping = base.as_ref().clone();
	mapping
		.scopes
		.get_mut(&LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&atom))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("mapped node absent"))
		.assert_ok()
		.source = None;
	assert_eq!(
		LiveGraphDelta::between(Some(base), Arc::new(mapping), 1.into(), 2.into(), &|_, _| Ok(()))
			.assert_ok()
			.changes()
			.len(),
		1
	);
}

/// Verifies mutable graph sdk conflict branch keeps decisions and blocks ack.
#[test]
fn mutable_graph_sdk_conflict_branch_keeps_decisions_and_blocks_ack() {
	use crate::{
		graph::{DirtyTarget, Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score key absent"))
		.assert_ok();
	subscription
		.materialization()
		.assert_ok()
		.mutate_local(Vec::<DirtyTarget>::new(), |graph| {
			let local = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?;
			graph.node_properties_mut(local)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let mut write = store.begin();
	write.set_base_property(&"person:right".parse().assert_ok(), key, Value::from(20)).assert_ok();
	let disjoint = write.commit().assert_ok().versionstamp();
	assert!(
		matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { revision } if revision == disjoint)
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
	assert_eq!(value(&subscription).assert_ok(), Some(Value::from(99)));
	let mut write = store.begin();
	write.set_base_property(&"person:left".parse().assert_ok(), key, Value::from(10)).assert_ok();
	let incoming = write.commit().assert_ok().versionstamp();
	let LiveGraphNext::Conflict(conflict) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("conflict branch absent"));
			test_result.assert_ok();
		};
	};
	assert_eq!(conflict.remote_revision(), incoming);
	assert_eq!(conflict.conflicts().len(), 1);
	assert_eq!(subscription.cursor().versionstamp(), disjoint);
	assert_eq!(value(&subscription).assert_ok(), Some(Value::from(99)));
	assert!(subscription.resume().is_err());
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Idle));
	assert!(
		subscription
			.rebase(|_, _| Err(FerrosGrynnError::invalid_argument("invalid explicit merge")))
			.is_err()
	);
	assert!(subscription.conflict().is_some());
	assert_eq!(value(&subscription).assert_ok(), Some(Value::from(99)));
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let local = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(local)?.insert(key, 120);
			Ok(())
		})
		.assert_ok();
	assert!(
		matches!(subscription.keep_local().assert_ok(), LiveGraphNext::Updated { revision } if revision == incoming)
	);
	assert_eq!(subscription.cursor().versionstamp(), incoming);
	assert_eq!(value(&subscription).assert_ok(), Some(Value::from(120)));
	let mut write = store.begin();
	write.set_base_property(&"person:left".parse().assert_ok(), key, Value::from(60)).assert_ok();
	write.commit().assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Conflict(_)));
	subscription.accept_remote().assert_ok();
	assert_eq!(value(&subscription).assert_ok(), Some(Value::from(60)));
	assert!(subscription.conflict().is_none());
	subscription.close().assert_ok();
}

/// Verifies mutable graph closed resolution preserves local state and
/// unconfirmed token.
#[test]
fn mutable_graph_closed_resolution_preserves_local_state_and_unconfirmed_token() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let mut write = store.begin();
	write.set_base_property(left.id(), key, Value::from(10)).assert_ok();
	write.commit().assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Conflict(_)));
	let before = subscription.materialization().assert_ok().static_snapshot().assert_ok();
	let revision = subscription.revision();
	subscription.close().assert_ok();
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
	assert_eq!(subscription.materialization().assert_ok().static_snapshot().assert_ok(), before);
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Closed));
	// An applied-but-unconfirmed token remains available when its consumer
	// closes.
	subscription.unconfirmed = Some(subscription.cursor().clone());
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Closed));
	assert!(subscription.applied_unconfirmed().is_some());
}

/// Verifies mutable graph event wait cancel preserves pending conflict and ack.
#[test]
fn mutable_graph_event_wait_cancel_preserves_pending_conflict_and_ack() {
	use std::{
		future::Future,
		pin::pin,
		task::{Context, Poll, Waker},
	};

	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let mut context = Context::from_waker(Waker::noop());
	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let cursor = subscription.cursor().clone();
	{
		let mut waiting = pin!(subscription.next_event());
		assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));
	}
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let mut write = store.begin();
	write.set_base_property(left.id(), key, Value::from(10)).assert_ok();
	let incoming = write.commit().assert_ok().versionstamp();
	{
		let mut waiting = pin!(subscription.next_event());
		let Poll::Ready(Ok(LiveGraphNext::Conflict(conflict))) =
			waiting.as_mut().poll(&mut context)
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("typed conflict branch absent"));
				test_result.assert_ok();
			};
		};
		assert_eq!(conflict.remote_revision(), incoming);
	}
	{
		let mut waiting = pin!(subscription.next_event());
		assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));
	}
	assert_eq!(subscription.cursor(), &cursor);
	assert!(subscription.conflict().is_some());
	subscription.discard_local().assert_ok();
	assert_eq!(subscription.cursor().versionstamp(), incoming);
	subscription.close().assert_ok();
}

/// Verifies mutable graph interned key survives remote updates before use.
#[test]
fn mutable_graph_interned_key_survives_remote_updates_before_use() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let draft = subscription.intern_property("draft").assert_ok();
	let score = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
		.assert_ok();
	let right: RecordId = "person:right".parse().assert_ok();
	let mut write = store.begin();
	write.set_base_property(&right, score, Value::from(20)).assert_ok();
	write.commit().assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { .. }));
	assert_eq!(subscription.property_key("draft"), Some(draft));
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(draft, "saved");
			Ok(())
		})
		.assert_ok();
	let mut write = store.begin();
	write.set_base_property(&right, score, Value::from(21)).assert_ok();
	write.commit().assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { .. }));
	subscription
		.materialization()
		.assert_ok()
		.with_graph(|graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			assert_eq!(
				graph.node(id).and_then(|node| node.properties().get(draft)),
				Some(&Value::from("saved"))
			);
			Ok::<(), FerrosGrynnError>(())
		})
		.assert_ok()
		.assert_ok();
	subscription.close().assert_ok();
}

/// Verifies mutable graph disjoint topology changes produce structural
/// conflict.
#[test]
fn mutable_graph_disjoint_topology_changes_produce_structural_conflict() {
	use crate::{
		datastore::OccurrenceState,
		graph::{Dag, Frozen, GraphRefReader},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _root, first_edge) = fixture().assert_ok();
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("graph source absent"));
			test_result.assert_ok();
		};
	};
	let graph = graph.clone();
	let mut seed = store.begin();
	let c: RecordId = "person:c".parse().assert_ok();
	let d: RecordId = "person:d".parse().assert_ok();
	let second: RecordId = "link:second".parse().assert_ok();
	for record in [&c, &d, &second] {
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
	}
	let c_id = seed.attach_node(graph.id(), &c, None, None).assert_ok();
	let d_id = seed.attach_node(graph.id(), &d, None, None).assert_ok();
	seed.attach_edge(graph.id(), &second, None, EdgeTopology::directed([c_id], [d_id]), None, None)
		.assert_ok();
	seed.commit().assert_ok();
	let second_edge = Value::from(AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Edge, second.clone()),
		Arc::new(graph.clone()),
	));
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(
			LiveExpr::field("id").assert_ok(),
			LiveExpr::Array(vec![LiveExpr::Literal(first_edge), LiveExpr::Literal(second_edge)]),
		)
		.with_family(GraphKind::Dag),
	);
	let mut subscription = store
		.subscribe_live_graph::<Dag<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let cursor = subscription.cursor().clone();
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |local| {
			let lookup = |kind, record: RecordId| {
				local
					.atom_id(&AtomRef::new(kind, record))?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("local target absent"))
			};
			let edge = lookup(AtomKind::Edge, "link:edge".parse()?)?;
			let b = lookup(AtomKind::Node, "person:right".parse()?)?;
			let d = lookup(AtomKind::Node, d.clone())?;
			local.replace_edge_incidence(edge, d, b)
		})
		.assert_ok();
	let mut write = store.begin();
	let b = write
		.read_graph(&graph)
		.assert_ok()
		.atom_id(&AtomRef::new(AtomKind::Node, "person:right".parse().assert_ok()))
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source node absent"))
		.assert_ok();
	write
		.replace_occurrence_state(
			graph.id(),
			&second,
			OccurrenceState::edge(EdgeTopology::directed([b], [d_id]), None, None),
		)
		.assert_ok();
	let incoming = write.commit().assert_ok().versionstamp();
	let LiveGraphNext::Conflict(conflict) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("structural conflict branch absent"));
			test_result.assert_ok();
		};
	};
	assert!(conflict.diagnostic().is_some());
	assert_eq!(conflict.remote_revision(), incoming);
	assert_eq!(subscription.cursor(), &cursor);
	assert!(subscription.keep_local().is_err());
	assert!(subscription.conflict().is_some());
	subscription.accept_remote().assert_ok();
	assert_eq!(subscription.cursor().versionstamp(), incoming);
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush uses occurrence mapping and preserves later
/// edits.
#[test]
fn mutable_graph_flush_uses_occurrence_mapping_and_preserves_later_edits() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let LiveSource::GraphClass {
		graph: source_graph,
		..
	} = &source
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("source absent"));
			test_result.assert_ok();
		};
	};
	let source_graph = source_graph.clone();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	let edit = |value| {
		subscription.materialization()?.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, value);
			Ok(())
		})
	};
	edit(99).assert_ok();
	let old_cursor = subscription.cursor().clone();
	let mut disjoint = store.begin();
	disjoint.set_base_property(&"person:right".parse().assert_ok(), key, 20.into()).assert_ok();
	disjoint.commit().assert_ok();
	let committed = subscription.flush(store.begin()).assert_ok();
	assert_eq!(subscription.cursor(), &old_cursor);
	assert_eq!(
		store
			.begin()
			.atom(left.id())
			.assert_ok()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("base absent"))
			.assert_ok()
			.properties()
			.get(key),
		Some(&Value::from(1))
	);
	assert_eq!(
		store.begin().effective_property(source_graph.id(), left.id(), key).assert_ok(),
		Some(Value::from(99))
	);
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 120);
			Ok(())
		})
		.assert_ok();
	for _ in 0..3 {
		if matches!(subscription.next().assert_ok(), LiveGraphNext::Idle) {
			break;
		}
	}
	assert_eq!(subscription.cursor().versionstamp(), committed.versionstamp());
	subscription
		.materialization()
		.assert_ok()
		.with_graph(|graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			assert_eq!(
				graph.node(id).and_then(|node| node.properties().get(key)),
				Some(&Value::from(120))
			);
			Ok::<(), FerrosGrynnError>(())
		})
		.assert_ok()
		.assert_ok();
	subscription.flush(store.begin()).assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { .. }));
	assert_eq!(
		store.begin().effective_property(source_graph.id(), left.id(), key).assert_ok(),
		Some(Value::from(120))
	);
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush rejects stale and foreign sources before
/// publication.
#[test]
fn mutable_graph_flush_rejects_stale_and_foreign_sources_before_publication() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let old = subscription.materialization().assert_ok().static_snapshot().assert_ok();
	let foreign = Datastore::in_memory();
	assert!(subscription.flush(foreign.begin()).is_err());
	let mut update = store.begin();
	update.set_base_property(left.id(), key, 10.into()).assert_ok();
	update.commit().assert_ok();
	let stamp = store.inner.current_root().commit_ts;
	assert!(subscription.flush(store.begin()).is_err());
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	assert_eq!(subscription.materialization().assert_ok().static_snapshot().assert_ok(), old);
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush source prerequisites survive every isolation
/// commit race.
#[test]
fn mutable_graph_flush_source_prerequisites_survive_every_isolation_commit_race() {
	use crate::{
		datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
		graph::{Frozen, Graph},
		live::{LiveGraphDelta, LiveGraphScopeId},
		test_assertions::AssertResult as _,
	};
	for isolation in
		[IsolationLevel::ReadCommitted, IsolationLevel::Snapshot, IsolationLevel::Serializable]
	{
		let (store, source, _, edge) = fixture().assert_ok();
		let query = LiveQuery::new(source).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
				.with_family(GraphKind::Graph),
		);
		let mut subscription = store
			.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
			.assert_ok()
			.into_mutable()
			.assert_ok();
		let before = subscription
			.remote
			.as_ref()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("remote absent"))
			.assert_ok();
		let key = subscription
			.property_key("score")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))
			.assert_ok();
		let record: RecordId = "person:left".parse().assert_ok();
		let mut local = before.as_ref().clone();
		local
			.scopes
			.get_mut(&LiveGraphScopeId::Root)
			.and_then(|scope| scope.nodes.get_mut(&AtomRef::new(AtomKind::Node, record.clone())))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))
			.assert_ok()
			.properties
			.insert("score", Value::from(99));
		let local = Arc::new(local);
		let changes = LiveGraphDelta::between(
			Some(Arc::clone(before)),
			Arc::clone(&local),
			0.into(),
			1.into(),
			&|_, _| Ok(()),
		)
		.assert_ok();
		let mut write =
			store.begin_with(TransactionOptions::new(isolation, TransactionAccess::ReadWrite));
		write
			.flush_live_graph(
				&subscription.subscription.inner,
				subscription.cursor(),
				before,
				&local,
				changes.changes(),
				&LiveOptions::default(),
			)
			.assert_ok();
		let mut racing = store.begin();
		racing.set_base_property(&record, key, 20.into()).assert_ok();
		let stamp = racing.commit().assert_ok().versionstamp();
		assert!(
			write.commit_live_graph(subscription.cursor()).is_err(),
			"isolation {isolation:?} lost source prerequisite"
		);
		assert_eq!(store.inner.current_root().commit_ts, stamp.get());
		assert_eq!(
			store
				.begin()
				.atom(&record)
				.assert_ok()
				.and_then(|atom| atom.properties().get(key).cloned()),
			Some(Value::from(20))
		);
		subscription.close().assert_ok();
	}
}

/// Verifies graph resnapshot replaces incarnation and preserves old guards.
#[test]
fn graph_resnapshot_replaces_incarnation_and_preserves_old_guards() {
	use crate::{
		graph::{Frozen, Graph},
		live::{LiveGraphNext, LiveState},
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut old =
		store.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default()).assert_ok();
	let guard = old.materialization().assert_ok().read().assert_ok();
	let old_id = old.id();
	let old_revision = guard.revision();
	let mut update = store.begin();
	let key = update.intern_property("score").assert_ok();
	update.set_base_property(&"person:left".parse().assert_ok(), key, 10.into()).assert_ok();
	update.commit().assert_ok();
	let mut replacement = old.resnapshot(&store).assert_ok();
	assert_ne!(replacement.id(), old_id);
	assert_eq!(store.begin().live_info(old_id).assert_ok().state(), LiveState::Closed);
	assert_eq!(guard.revision(), old_revision);
	assert!(matches!(old.next().assert_ok(), LiveGraphNext::Closed));
	assert_eq!(
		replacement.materialization().assert_ok().read().assert_ok().identity(),
		replacement.id().as_uuid()
	);
	replacement.close().assert_ok();
}

/// Verifies mutable graph flush does not replay earlier remote values over
/// event effects.
#[test]
fn mutable_graph_flush_does_not_replay_earlier_remote_values_over_event_effects() {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("source absent"));
			test_result.assert_ok();
		};
	};
	let mut definition = store.begin();
	definition
		.define_event(
			EventDefinition::new(
				"after_flush",
				EventTarget::Graph(graph.id()),
				"$event = 'UPDATE' AND $after.score = 99",
				"{ UPDATE person:right SET score = 22; }",
			)
			.assert_ok(),
		)
		.assert_ok();
	definition.commit().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let mut prior = store.begin();
	prior.set_base_property(&"person:right".parse().assert_ok(), key, 20.into()).assert_ok();
	prior.commit().assert_ok();
	subscription.flush(store.begin()).assert_ok();
	for _ in 0..3 {
		if matches!(subscription.next().assert_ok(), LiveGraphNext::Idle) {
			break;
		}
	}
	subscription
		.materialization()
		.assert_ok()
		.with_graph(|graph| {
			let id = graph
				.atom_id(&AtomRef::new(AtomKind::Node, "person:right".parse()?))?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("right absent"))?;
			assert_eq!(
				graph.node(id).and_then(|node| node.properties().get(key)),
				Some(&Value::from(22))
			);
			Ok::<(), FerrosGrynnError>(())
		})
		.assert_ok()
		.assert_ok();
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush resolution of earlier delivery discards old
/// local decisions.
#[test]
fn mutable_graph_flush_resolution_of_earlier_delivery_discards_old_local_decisions() {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	for resolution in 0..3 {
		let (store, source, _, edge) = fixture().assert_ok();
		let LiveSource::GraphClass {
			graph,
			..
		} = &source
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("source absent"));
				test_result.assert_ok();
			};
		};
		let mut definition = store.begin();
		definition
			.define_event(
				EventDefinition::new(
					"after_flush",
					EventTarget::Graph(graph.id()),
					"$event = 'UPDATE' AND $after.score = 99",
					"{ UPDATE person:right SET score = 22; }",
				)
				.assert_ok(),
			)
			.assert_ok();
		definition.commit().assert_ok();
		let query = LiveQuery::new(source).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
				.with_family(GraphKind::Graph),
		);
		let mut subscription = store
			.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
			.assert_ok()
			.into_mutable()
			.assert_ok();
		let key = subscription
			.property_key("score")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
			.assert_ok();
		let edit = |record: &str, score| -> FerrosGrynnResult<()> {
			subscription.materialization()?.mutate_local([], |graph| {
				let id = graph
					.atom_id(&AtomRef::new(AtomKind::Node, record.parse()?))?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("local node absent"))?;
				graph.node_properties_mut(id)?.insert(key, score);
				Ok(())
			})
		};
		edit("person:left", 99).assert_ok();
		let mut prior = store.begin();
		prior.set_base_property(&"person:right".parse().assert_ok(), key, 20.into()).assert_ok();
		prior.commit().assert_ok();
		subscription.flush(store.begin()).assert_ok();
		subscription
			.materialization()
			.assert_ok()
			.mutate_local([], |graph| {
				let id = graph
					.atom_id(&AtomRef::new(AtomKind::Node, "person:right".parse()?))?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("right absent"))?;
				graph.node_properties_mut(id)?.insert(key, 30);
				Ok(())
			})
			.assert_ok();
		assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Conflict(_)));
		match resolution {
			0 => subscription.accept_remote().assert_ok(),
			1 => subscription.discard_local().assert_ok(),
			_ => subscription.rebase(|_, _| Ok(())).assert_ok(),
		};
		for _ in 0..3 {
			if matches!(subscription.next().assert_ok(), LiveGraphNext::Idle) {
				break;
			}
		}
		subscription
			.materialization()
			.assert_ok()
			.with_graph(|graph| {
				for (record, score) in [("person:left", 99), ("person:right", 22)] {
					let id =
						graph.atom_id(&AtomRef::new(AtomKind::Node, record.parse()?))?.ok_or_else(
							|| FerrosGrynnError::invalid_argument("accepted node absent"),
						)?;
					assert_eq!(
						graph.node(id).and_then(|node| node.properties().get(key)),
						Some(&Value::from(score))
					);
				}
				Ok::<(), FerrosGrynnError>(())
			})
			.assert_ok()
			.assert_ok();
		subscription.close().assert_ok();
	}
}

/// Verifies mutable graph flush structural conflict uses manual resolution
/// branch.
#[test]
fn mutable_graph_flush_structural_conflict_uses_manual_resolution_branch() {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Dag, Frozen, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _, first_edge) = fixture().assert_ok();
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("source absent"));
			test_result.assert_ok();
		};
	};
	let graph = graph.clone();
	let mut seed = store.begin();
	for name in ["c", "d"] {
		let record: RecordId = format!("person:{name}").parse().assert_ok();
		seed.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		seed.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	seed.define_event(
		EventDefinition::new(
			"flush_topology",
			EventTarget::Graph(graph.id()),
			"$event = 'UPDATE' AND $after.score = 99",
			"{ DETACH link:edge FROM GRAPH source; RELATE (person:d)-[link:edge]->(person:c) IN \
			 GRAPH source; }",
		)
		.assert_ok(),
	)
	.assert_ok();
	seed.commit().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(first_edge))
			.with_family(GraphKind::Dag),
	);
	let mut subscription = store
		.subscribe_live_graph::<Dag<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("score absent"))
		.assert_ok();
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&AtomRef::new(AtomKind::Node, "person:left".parse()?))?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let committed = subscription.flush(store.begin()).assert_ok().versionstamp();
	let cursor = subscription.cursor().clone();
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let lookup = |name: &str| {
				graph
					.atom_id(&AtomRef::new(AtomKind::Node, name.parse()?))?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("local endpoint absent"))
			};
			graph.add_edge(lookup("person:c")?, lookup("person:d")?, None, PropertyMap::new())?;
			Ok(())
		})
		.assert_ok();
	let LiveGraphNext::Conflict(conflict) = subscription.next().assert_ok() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("flush conflict branch absent"));
			test_result.assert_ok();
		};
	};
	assert_eq!(conflict.remote_revision(), committed);
	assert!(conflict.diagnostic().is_some());
	assert_eq!(subscription.cursor(), &cursor);
	assert!(subscription.keep_local().is_err());
	assert!(subscription.resume().is_err());
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Idle));
	subscription.accept_remote().assert_ok();
	assert_eq!(subscription.cursor().versionstamp(), committed);
	assert!(subscription.conflict().is_none());
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Idle));
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush charges private source work before capture.
#[test]
fn mutable_graph_flush_charges_private_source_work_before_capture() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))
		.assert_ok();
	let record: RecordId = "person:left".parse().assert_ok();
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&AtomRef::new(AtomKind::Node, record.clone()))?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let mut transaction = store.begin();
	for id in 0..512 {
		let name = transaction.intern_property(&format!("extra_{id}")).assert_ok();
		transaction.set_base_property(&record, name, 1.into()).assert_ok();
	}
	let limits = LiveOptions::default().with_budgets(8 << 20, 250).assert_ok();
	let remote = subscription
		.remote
		.as_ref()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("remote absent"))
		.assert_ok();
	let mut local = remote.as_ref().clone();
	local
		.scopes
		.get_mut(&crate::live::LiveGraphScopeId::Root)
		.and_then(|scope| scope.nodes.get_mut(&AtomRef::new(AtomKind::Node, record.clone())))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("local image absent"))
		.assert_ok()
		.properties
		.insert("score", Value::from(99));
	let local = Arc::new(local);
	let dirty = crate::live::LiveGraphDelta::between(
		Some(Arc::clone(remote)),
		Arc::clone(&local),
		0.into(),
		1.into(),
		&|_, _| Ok(()),
	)
	.assert_ok();
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
		.ok_or_else(|| FerrosGrynnError::invalid_argument("work limit ignored"))
		.assert_ok();
	assert!(matches!(
		error.status().status(),
		crate::gql_status::Status::TraversalResourceLimitExceeded { .. }
	));
	assert_eq!(store.inner.current_root().commit_ts, stamp);
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush rebinds edge owned child before connector.
#[test]
fn mutable_graph_flush_rebinds_edge_owned_child_before_connector() {
	use crate::{
		graph::{Frozen, Graph, QualifiedAtomId},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	let mut seed = store.begin();
	let class =
		seed.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).assert_ok()).assert_ok();
	let root = seed.define_graph("root", GraphKind::Metagraph).assert_ok();
	let a = seed.define_graph("a", GraphKind::Graph).assert_ok();
	let b = seed.define_graph("b", GraphKind::Graph).assert_ok();
	for record in ["item:root", "item:one", "item:two", "link:a", "link:y", "link:z"] {
		seed.create_atom(record.parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	let root_node =
		seed.attach_node(root.id(), &"item:root".parse().assert_ok(), None, None).assert_ok();
	let terminal =
		seed.attach_node(a.id(), &"item:one".parse().assert_ok(), None, None).assert_ok();
	seed.attach_node(b.id(), &"item:two".parse().assert_ok(), None, None).assert_ok();
	let owner = seed
		.attach_edge(
			root.id(),
			&"link:z".parse().assert_ok(),
			None,
			EdgeTopology::undirected([root_node]),
			None,
			Some(a.id()),
		)
		.assert_ok();
	seed.attach_edge(
		root.id(),
		&"link:y".parse().assert_ok(),
		None,
		EdgeTopology::undirected([root_node]),
		None,
		Some(b.id()),
	)
	.assert_ok();
	seed.attach_edge(
		root.id(),
		&"link:a".parse().assert_ok(),
		None,
		EdgeTopology::directed([QualifiedAtomId::new(vec![owner], terminal).assert_ok()], [
			root_node,
		]),
		None,
		None,
	)
	.assert_ok();
	seed.commit().assert_ok();
	let edges = ["link:a", "link:y", "link:z"]
		.into_iter()
		.map(|record| {
			Ok(LiveExpr::Literal(Value::from(AtomOccurrenceRef::new(
				AtomRef::new(AtomKind::Edge, record.parse()?),
				Arc::new(root.clone()),
			))))
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()
		.assert_ok();
	let query = LiveQuery::new(LiveSource::GraphClass {
		graph: root.clone(),
		class,
	})
	.with_graph_result(
		LiveGraphResult::new(
			LiveExpr::Array(vec![
				LiveExpr::field("id").assert_ok(),
				LiveExpr::Literal(Value::from(AtomOccurrenceRef::new(
					AtomRef::new(AtomKind::Node, "item:two".parse().assert_ok()),
					Arc::new(b.clone()),
				))),
			]),
			LiveExpr::Array(edges),
		)
		.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
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
		})
		.assert_ok();
	subscription.flush(store.begin()).assert_ok();
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { .. }));
	let root_image = store.inner.current_root();
	let global = root_image
		.resolve_atom(&"link:z".parse().assert_ok())
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source owner absent"))
		.assert_ok();
	let local = root_image
		.membership(root.id(), global)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("source occurrence absent"))
		.assert_ok();
	assert_eq!(
		root_image.occurrence(root.id(), local).and_then(|owner| owner.child_graph()),
		Some(b.id())
	);
	assert!(
		root_image.resolve_atom(&"item:one".parse().assert_ok()).is_some(),
		"closure loss must preserve child source data"
	);
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush preserves cached synthetic property handle.
#[test]
fn mutable_graph_flush_preserves_cached_synthetic_property_handle() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let key = subscription.intern_property("draft").assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	subscription.flush(store.begin()).assert_ok();
	subscription.next().assert_ok();
	assert_eq!(subscription.property_key("draft"), Some(key));
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			assert_eq!(graph.node_properties_mut(id)?.remove(key), Some(Value::from(99)));
			Ok(())
		})
		.assert_ok();
	subscription.flush(store.begin()).assert_ok();
	subscription.next().assert_ok();
	subscription.close().assert_ok();
}

/// One global source mapping works through every sealed family layout.
fn flush_global_family<G>() -> FerrosGrynnResult<()>
where
	G: crate::graph::LiveFamily<State = crate::graph::Frozen, Frozen = G> + Clone,
{
	use crate::{
		graph::GraphMut,
		live::LiveGraphNext,
		test_assertions::{AssertComparison as _, AssertCondition as _},
	};
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
	(matches!(subscription.next()?, LiveGraphNext::Updated { revision } if revision == committed.versionstamp())).assert_true();
	(
		&(store
			.begin()
			.atom(&record)?
			.ok_or_else(|| FerrosGrynnError::invalid_argument("source absent"))?
			.properties()
			.get(key)),
		&(Some(&Value::from(99))),
	)
		.assert_equal();
	subscription.close()?;
	Ok(())
}

/// Verifies mutable graph flush all sealed families global source.
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

/// Verifies mutable graph flush unchanged result keeps checkpoint watermark.
#[test]
fn mutable_graph_flush_unchanged_result_keeps_checkpoint_watermark() {
	use crate::{
		datastore::{EventDefinition, EventTarget},
		graph::{Frozen, Graph, GraphMut},
		live::LiveGraphNext,
		test_assertions::AssertResult as _,
	};
	let (store, source, _, edge) = fixture().assert_ok();
	let LiveSource::GraphClass {
		graph,
		..
	} = &source
	else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("source absent"));
			test_result.assert_ok();
		};
	};
	let mut definition = store.begin();
	definition
		.define_event(
			EventDefinition::new(
				"reset",
				EventTarget::Graph(graph.id()),
				"$event = 'UPDATE' AND $after.score = 99",
				"{ UPDATE person:left IN GRAPH source SET score = 1; }",
			)
			.assert_ok(),
		)
		.assert_ok();
	definition.commit().assert_ok();
	let query = LiveQuery::new(source).with_graph_result(
		LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
			.with_family(GraphKind::Graph),
	);
	let mut subscription = store
		.subscribe_live_graph::<Graph<Frozen>>(query, LiveOptions::default())
		.assert_ok()
		.into_mutable()
		.assert_ok();
	let original = subscription.revision();
	let key = subscription
		.property_key("score")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))
		.assert_ok();
	let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
	subscription
		.materialization()
		.assert_ok()
		.mutate_local([], |graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			graph.node_properties_mut(id)?.insert(key, 99);
			Ok(())
		})
		.assert_ok();
	let committed = subscription.flush(store.begin()).assert_ok();
	assert!(Some(committed.versionstamp()) > original);
	assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { .. }));
	assert_eq!(subscription.revision(), original);
	subscription
		.materialization()
		.assert_ok()
		.with_graph(|graph| {
			let id = graph
				.atom_id(&left)?
				.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
			assert_eq!(
				graph.node(id).and_then(|node| node.properties().get(key)),
				Some(&Value::from(1))
			);
			Ok::<(), FerrosGrynnError>(())
		})
		.assert_ok()
		.assert_ok();
	subscription.close().assert_ok();
}

/// Verifies mutable graph flush overflow keeps gap explicit and block atomic.
#[test]
fn mutable_graph_flush_overflow_keeps_gap_explicit_and_block_atomic() {
	use crate::{
		graph::{Frozen, Graph, GraphMut},
		live::{LiveGraphNext, LiveOverflowPolicy},
		test_assertions::AssertResult as _,
	};
	for policy in [LiveOverflowPolicy::Gap, LiveOverflowPolicy::Block] {
		let (store, source, _, edge) = fixture().assert_ok();
		let query = LiveQuery::new(source).with_graph_result(
			LiveGraphResult::new(LiveExpr::field("id").assert_ok(), LiveExpr::Literal(edge))
				.with_family(GraphKind::Graph),
		);
		let options = LiveOptions::default()
			.with_queue(4, 64 << 20)
			.assert_ok()
			.with_overflow(policy)
			.with_block_timeout(std::time::Duration::from_millis(10))
			.assert_ok();
		let mut subscription = store
			.subscribe_live_graph::<Graph<Frozen>>(query, options)
			.assert_ok()
			.into_mutable()
			.assert_ok();
		let key = subscription
			.property_key("score")
			.ok_or_else(|| FerrosGrynnError::invalid_argument("key absent"))
			.assert_ok();
		let left = AtomRef::new(AtomKind::Node, "person:left".parse().assert_ok());
		subscription
			.materialization()
			.assert_ok()
			.mutate_local([], |graph| {
				let id = graph
					.atom_id(&left)?
					.ok_or_else(|| FerrosGrynnError::invalid_argument("left absent"))?;
				graph.node_properties_mut(id)?.insert(key, 99);
				Ok(())
			})
			.assert_ok();
		let mut prior = subscription.cursor().versionstamp();
		for value in [20, 21, 22, 23] {
			let mut write = store.begin();
			write
				.set_base_property(&"person:right".parse().assert_ok(), key, value.into())
				.assert_ok();
			prior = write.commit().assert_ok().versionstamp();
		}
		if policy == LiveOverflowPolicy::Block {
			assert!(subscription.flush(store.begin()).is_err());
			assert_eq!(store.inner.current_root().commit_ts, prior.get());
		} else {
			subscription.flush(store.begin()).assert_ok();
			assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Updated { .. }));
			let revision = subscription.revision();
			assert!(matches!(subscription.next().assert_ok(), LiveGraphNext::Gap(_)));
			assert_eq!(subscription.revision(), revision);
			let mut fresh = subscription.resnapshot(&store).assert_ok();
			fresh.close().assert_ok();
		}
		subscription.close().assert_ok();
	}
}
