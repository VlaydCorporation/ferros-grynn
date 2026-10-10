//! Owned window recipes compared with ordinary SELECT and result history.

use std::collections::BTreeMap;

use super::{QueryEvaluation, evaluate_query};
use crate::{
	changefeed::{ChangefeedConfig, ChangefeedSource},
	datastore::{ClassField, ClassId, ClassSchema, Datastore},
	gql_status::{FerrosGrynnError, FerrosGrynnResult},
	graph::{AtomKind, GraphKind},
	live::{
		LiveExpr,
		LiveField,
		LiveIdentity,
		LiveNext,
		LiveNullsOrder,
		LiveOptions,
		LiveOrder,
		LiveOrderDirection,
		LivePath,
		LivePayload,
		LiveProjection,
		LiveQuery,
		LiveSince,
		LiveSource,
	},
	values::{Object, PropertyMap, RecordId, Type, Value},
};

/// Direct owned SPLIT recipes share SELECT binding and exact bag semantics.
#[test]
fn live_window_split_sdk_bags_groups_and_nested() -> FerrosGrynnResult<()> {
	use crate::live::LiveIdentityPart;
	let store = Datastore::in_memory();
	for result in store.execute(
		"DEFINE GRAPH split_scope; DEFINE CLASS item TYPE NODE SCHEMALESS; CREATE item:a SET tags \
		 = [7, 7], left = [1, 1], right = ['x', 'y']; ATTACH item:a TO GRAPH split_scope;",
		&Object::new(),
	)? {
		result.result?;
	}
	let record: RecordId = "item:a".parse()?;
	let mut root = store.begin();
	let graph = root
		.named_graph("split_scope")?
		.ok_or_else(|| FerrosGrynnError::invalid_argument("SPLIT graph absent"))?;
	drop(root);
	for source in [LiveSource::Record(record.clone()), LiveSource::GraphRecord {
		graph: graph.clone(),
		record: record.clone(),
	}] {
		for paths in [vec!["properties.tags"], vec!["properties.left", "properties.right"]] {
			let split = paths
				.iter()
				.map(|path| {
					Ok(LiveExpr::Field(LivePath::new(
						path.split('.').map(|segment| segment.into()).collect(),
					)?))
				})
				.collect::<FerrosGrynnResult<_>>()?;
			let query = LiveQuery::new(source.clone())
				.with_split(split)
				.with_projection(LiveProjection::Value(LiveExpr::Literal(7.into())));
			let actual = evaluate(&store, &query)
				.map_err(|error| {
					FerrosGrynnError::invalid_argument(format!(
						"SPLIT SDK row source={source:?} paths={paths:?}"
					))
					.with_cause(error)
				})?
				.rows;
			assert_eq!(
				actual.len(),
				if paths.len() == 1 {
					2
				} else {
					4
				}
			);
			assert!(actual.keys().all(|identity|matches!(identity,LiveIdentity::Composite(parts) if parts.len()==paths.len()+1 && matches!(parts.last(),Some(LiveIdentityPart::Split(_))))));
			let scope = if matches!(source, LiveSource::GraphRecord { .. }) {
				"IN GRAPH split_scope"
			} else {
				""
			};
			let expected = oracle(
				&store,
				&format!("SELECT VALUE 7 FROM item:a {scope} SPLIT {};", paths.join(",")),
			)?;
			assert_eq!(actual.into_values().collect::<Vec<_>>(), expected);
			let group = query.clone().with_group_all().with_projection(LiveProjection::Value(
				LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				},
			));
			assert_eq!(
				evaluate(&store, &group)
					.map_err(|error| FerrosGrynnError::invalid_argument(format!(
						"SPLIT SDK group source={source:?} paths={paths:?}"
					))
					.with_cause(error))?
					.rows,
				BTreeMap::from([(
					LiveIdentity::Aggregate(Vec::new()),
					Value::from(if paths.len() == 1 {
						2
					} else {
						4
					})
				)])
			);
			let inner = query.with_only(true).with_limit(LiveExpr::Literal(1.into()));
			let outer = LiveQuery::new(LiveSource::Record(record.clone()))
				.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(inner))));
			assert_eq!(
				evaluate(&store, &outer)
					.map_err(|error| FerrosGrynnError::invalid_argument(format!(
						"SPLIT SDK nested source={source:?} paths={paths:?}"
					))
					.with_cause(error))?
					.rows,
				BTreeMap::from([(LiveIdentity::Record(record.clone()), 7.into())])
			);
		}
	}
	for value in [
		Value::None,
		Value::Null,
		Value::Array(Vec::<Value>::new().into()),
		Value::Set([1.into(), 2.into()].into_iter().collect()),
		Value::from(1),
	] {
		let mut tx = store.begin();
		let tags = tx.intern_property("tags")?;
		tx.set_base_property(&record, tags, value.clone())?;
		tx.commit()?;
		let query =
			LiveQuery::new(LiveSource::Record(record.clone())).with_split(vec![LiveExpr::Field(
				LivePath::new(vec!["properties".into(), "tags".into()])?,
			)]);
		match value {
			Value::Set(_) => assert_eq!(evaluate(&store, &query)?.rows.len(), 2),
			Value::Number(_) => assert!(evaluate(&store, &query).is_err()),
			_ => assert!(evaluate(&store, &query)?.rows.is_empty()),
		}
	}
	for value in [
		LiveExpr::Literal(Value::Array(vec![Value::from(1)].into())),
		LiveExpr::Parameter("tags".into()),
	] {
		let query = LiveQuery::new(LiveSource::Record(record.clone()))
			.with_parameters(Object::from_iter([(
				"tags",
				Value::Array(vec![Value::from(1)].into()),
			)]))
			.with_split(vec![value]);
		assert!(evaluate(&store, &query).is_err());
	}
	Ok(())
}

/// Exact numeric ties, nulls and string comparison keys in a tiny domain.
fn fixture() -> FerrosGrynnResult<(Datastore, ClassId)> {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("age", Type::Option(Box::new(Type::Int))),
		ClassField::new("name", Type::String),
	])?)?;
	let age = tx.intern_property("age")?;
	let name = tx.intern_property("name")?;
	tx.configure_changefeed(
		ChangefeedSource::Class(class),
		ChangefeedConfig::new("P1D".parse()?)?,
	)?;
	let wide = 9_007_199_254_740_992_i64;
	for (id, score, text) in [
		("a", Value::from(wide + 1), "item10"),
		("b", Value::from(wide), "item2"),
		("c", Value::from(wide + 1), "Item1"),
		("d", Value::Null, "item20"),
	] {
		tx.create_atom(
			format!("item:{id}").parse()?,
			PropertyMap::from_iter([(age, score), (name, text.into())]),
		)?;
	}
	tx.commit()?;
	Ok((store, class))
}

/// Runs the same owned recipe through the shared root-bound pipeline.
fn evaluate(store: &Datastore, query: &LiveQuery) -> FerrosGrynnResult<QueryEvaluation> {
	evaluate_query(
		&store.inner.current_root(),
		query,
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected test binding")),
		&LiveOptions::default(),
		std::env::temp_dir(),
		&|_| {},
	)
}

/// Independent ordinary SELECT returns a bag of projected values.
fn oracle(store: &Datastore, text: &str) -> FerrosGrynnResult<Vec<Value>> {
	let outcome = store
		.execute(text, &Object::new())?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("window SELECT missing"))?
		.result?;
	let Value::Array(rows) = outcome.value() else {
		return Err(FerrosGrynnError::invalid_argument("window SELECT is not rows"));
	};
	Ok(rows.iter().cloned().collect())
}

/// Public JOIN recipes share ordinary matching, exact targets and identities.
#[test]
fn live_join_sdk_gql_exact_sources_and_aliases() -> FerrosGrynnResult<()> {
	use crate::live::{LiveBinaryOperator, LiveIdentityPart, LiveJoin, LiveJoinKind};
	let (store, class) = fixture()?;
	let field = |source: &str, name: &str| -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Field(LivePath::new(vec![source.into(), name.into()])?))
	};
	let left: RecordId = "item:a".parse()?;
	let projection =
		LiveProjection::Value(LiveExpr::Array(vec![field("l", "id")?, field("r", "id")?]));
	for (kind, source, predicate, sql) in [
		(
			LiveJoinKind::Inner,
			LiveSource::Class(class),
			Some(field("l", "age")?.binary(LiveBinaryOperator::Equal, field("r", "age")?)),
			"SELECT VALUE [l.id, r.id] FROM item:a AS l INNER JOIN item AS r ON l.age = r.age;",
		),
		(
			LiveJoinKind::Cross,
			LiveSource::Record("item:b".parse()?),
			None,
			"SELECT VALUE [l.id, r.id] FROM item:a AS l CROSS JOIN item:b AS r;",
		),
		(
			LiveJoinKind::Left,
			LiveSource::Record("item:missing".parse()?),
			Some(LiveExpr::Literal(true.into())),
			"SELECT VALUE [l.id, r.id] FROM item:a AS l LEFT JOIN item:missing AS r ON true;",
		),
	] {
		let query = LiveQuery::new(LiveSource::Record(left.clone()))
			.with_alias("l")?
			.with_joins(vec![LiveJoin::new(source, "r", kind, predicate)?])
			.with_projection(projection.clone());
		let actual = evaluate(&store, &query)?;
		assert_eq!(actual.rows.values().cloned().collect::<Vec<_>>(), oracle(&store, sql)?);
		for identity in actual.rows.keys() {
			assert!(matches!(identity, LiveIdentity::Composite(parts)
				if matches!(parts.first(), Some(LiveIdentityPart::Record(record)) if record == &left)
				&& parts.len() == 2));
		}
		if kind == LiveJoinKind::Left {
			assert_eq!(actual.rows.len(), 1);
			assert!(actual.rows.keys().all(|id| matches!(id, LiveIdentity::Composite(parts)
				if matches!(parts[1], LiveIdentityPart::Unmatched))));
		}
		let outcome = store
			.execute(&sql.replacen("SELECT", "LIVE SELECT", 1), &Object::new())?
			.into_iter()
			.next()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("LIVE JOIN result missing"))?
			.result?;
		let Value::Uuid(uuid) = outcome.value() else {
			return Err(FerrosGrynnError::invalid_argument("LIVE JOIN identity missing"));
		};
		let id = crate::live::LiveId::from_uuid(*uuid)?;
		let info = store.begin().live_info(id)?;
		assert_eq!(info.query().joins().len(), 1);
		assert_eq!(evaluate(&store, info.query())?.rows, actual.rows);
	}
	let joined = LiveQuery::new(LiveSource::Record(left))
		.with_alias("l")?
		.with_joins(vec![LiveJoin::new(LiveSource::Class(class), "r", LiveJoinKind::Cross, None)?]);
	assert!(
		evaluate(
			&store,
			&joined.clone().with_projection(LiveProjection::Value(LiveExpr::field("age")?))
		)
		.is_err()
	);
	let duplicate = LiveQuery::new(LiveSource::Class(class))
		.with_alias("l")?
		.with_joins(vec![LiveJoin::new(LiveSource::Class(class), "L", LiveJoinKind::Cross, None)?]);
	assert!(evaluate(&store, &duplicate).is_err());
	Ok(())
}

/// A polymorphic RHS is one LEFT relation, with one aggregate and window
/// domain.
#[test]
fn live_join_polymorphic_relation_and_nested_domain() -> FerrosGrynnResult<()> {
	use crate::live::{LiveBinaryOperator, LiveIdentityPart, LiveJoin, LiveJoinKind};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for (class, fields) in [
		("left", vec![ClassField::new("value", Type::Int)]),
		("a", vec![ClassField::new("value", Type::Int)]),
		("b", vec![ClassField::new("padding", Type::String), ClassField::new("value", Type::Int)]),
	] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, fields)?)?;
	}
	let value = tx.intern_property("value")?;
	let padding = tx.intern_property("padding")?;
	tx.create_atom("left:one".parse()?, PropertyMap::from_iter([(value, 1.into())]))?;
	let graph = tx.define_graph("rhs", GraphKind::Graph)?;
	for (name, number) in [("a:one", 1), ("b:two", 2)] {
		let mut properties = PropertyMap::from_iter([(value, number.into())]);
		if name.starts_with('b') {
			properties.insert(padding, "wide");
		}
		let record: RecordId = name.parse()?;
		tx.create_atom(record.clone(), properties)?;
		tx.attach_node(graph.id(), &record, None, None)?;
	}
	tx.commit()?;
	let field = |alias: &str, name: &str| -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Field(LivePath::new(vec![alias.into(), name.into()])?))
	};
	let base = LiveQuery::new(LiveSource::Record("left:one".parse()?))
		.with_alias("l")?
		.with_joins(vec![LiveJoin::new(
			LiveSource::Graph(graph.clone()),
			"r",
			LiveJoinKind::Left,
			Some(field("l", "value")?.binary(LiveBinaryOperator::Equal, field("r", "value")?)),
		)?])
		.with_projection(LiveProjection::Value(field("r", "id")?));
	let rows = evaluate(&store, &base)?.rows;
	assert_eq!(rows.len(), 1);
	assert_eq!(rows.values().next(), Some(&Value::Record(Box::new("a:one".parse()?))));
	assert!(
		evaluate(
			&store,
			&base.clone().with_filter(
				field("r", "value")?.binary(LiveBinaryOperator::Equal, LiveExpr::Literal(2.into()))
			)
		)?
		.rows
		.is_empty()
	);
	let missing = base.clone().with_joins(vec![LiveJoin::new(
		LiveSource::Graph(graph.clone()),
		"r",
		LiveJoinKind::Left,
		Some(LiveExpr::Literal(false.into())),
	)?]);
	let rows = evaluate(&store, &missing)?.rows;
	assert_eq!(rows.len(), 1);
	assert!(rows.keys().all(|id| matches!(id, LiveIdentity::Composite(parts) if matches!(parts.last(), Some(LiveIdentityPart::Unmatched)))));
	assert_eq!(rows.values().next(), Some(&Value::None));
	let records = evaluate(&store, &base.clone().with_projection(LiveProjection::Records))?;
	assert!(
		records
			.rows
			.values()
			.all(|value| matches!(value, Value::Object(object) if object.get("padding").is_none()))
	);
	let cross = base.clone().with_joins(vec![LiveJoin::new(
		LiveSource::Graph(graph),
		"r",
		LiveJoinKind::Cross,
		None,
	)?]);
	let sum = cross.clone().with_projection(LiveProjection::Value(LiveExpr::Call {
		name: "sum".into(),
		arguments: vec![field("r", "value")?],
	}));
	assert_eq!(evaluate(&store, &sum)?.rows.values().next(), Some(&Value::from(3)));
	let aggregate_alias = cross
		.clone()
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			})
			.with_alias(LivePath::field("n")?),
		]))
		.with_order(vec![LiveOrder::by_name("n")?]);
	assert_eq!(
		evaluate(&store, &aggregate_alias)?.rows.values().next(),
		Some(&Value::Object(Object::from_iter([("n", Value::from(2))])))
	);
	let compound_order = aggregate_alias.with_order(vec![LiveOrder::new(
		LiveExpr::field("n")?.binary(LiveBinaryOperator::Add, LiveExpr::Literal(1.into())),
	)]);
	assert!(evaluate(&store, &compound_order).is_err());
	assert!(oracle(&store, "SELECT count() AS n FROM a ORDER BY n + 1;").is_err());
	let rhs = cross.joins()[0].source().clone();
	let twice = missing.clone().with_joins(vec![
		missing.joins()[0].clone(),
		LiveJoin::new(rhs.clone(), "s", LiveJoinKind::Left, Some(LiveExpr::Literal(false.into())))?,
	]);
	let result = evaluate(&store, &twice)?;
	assert_eq!(result.rows.len(), 1);
	assert!(result.rows.keys().all(|id| matches!(id, LiveIdentity::Composite(parts) if parts.len() == 3 && matches!(parts[1], LiveIdentityPart::Unmatched) && matches!(parts[2], LiveIdentityPart::Unmatched))));
	let nested_union = LiveQuery::new(rhs.clone())
		.with_alias("l")?
		.with_joins(vec![LiveJoin::new(rhs, "r", LiveJoinKind::Cross, None)?])
		.with_projection(LiveProjection::Value(LiveExpr::Call {
			name: "sum".into(),
			arguments: vec![field("r", "value")?],
		}));
	let nested_union = LiveQuery::new(LiveSource::Record("left:one".parse()?))
		.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(nested_union))));
	assert_eq!(evaluate(&store, &nested_union)?.rows.values().next(), Some(&Value::from(6)));
	let nested = LiveQuery::new(LiveSource::Record("left:one".parse()?))
		.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(sum))));
	assert_eq!(evaluate(&store, &nested)?.rows.values().next(), Some(&Value::from(3)));
	let limited = cross
		.with_order(vec![LiveOrder::new(field("r", "value")?)])
		.with_start_offset(LiveExpr::Literal(1.into()))
		.with_limit(LiveExpr::Literal(1.into()));
	assert_eq!(
		evaluate(&store, &limited)?.rows.values().next(),
		Some(&Value::Record(Box::new("b:two".parse()?)))
	);
	Ok(())
}

/// Overload typing preserves real classes and rejects unsafe NONE substitution.
#[test]
fn live_join_polymorphic_overloads_and_admission() -> FerrosGrynnResult<()> {
	use crate::{
		datastore::{FunctionEffect, SchemaFunction, SchemaFunctionParameter},
		live::{LiveJoin, LiveJoinKind},
	};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("rhs", GraphKind::Graph)?;
	let v = tx.intern_property("v")?;
	for (class, ty, number, result) in
		[("ints", Type::Int, Value::from(1), 1), ("floats", Type::Float, Value::from(2.0_f64), 10)]
	{
		tx.define_class(ClassSchema::new(class, AtomKind::Node, [ClassField::new(
			"v",
			ty.clone(),
		)])?)?;
		tx.define_function(SchemaFunction::new(
			"fn",
			"score",
			vec![SchemaFunctionParameter::new("v", ty)?],
			Type::Int,
			FunctionEffect::Pure,
			false,
			None,
			format!("{{ RETURN {result}; }}"),
		)?)?;
		let record: RecordId = format!("{class}:one").parse()?;
		tx.create_atom(record.clone(), PropertyMap::from_iter([(v, number)]))?;
		tx.attach_node(graph.id(), &record, None, None)?;
	}
	tx.commit()?;
	let score = LiveExpr::Call {
		name: "fn::score".into(),
		arguments: vec![LiveExpr::Field(LivePath::new(vec!["r".into(), "v".into()])?)],
	};
	let base = LiveQuery::new(LiveSource::Record("ints:one".parse()?))
		.with_alias("l")?
		.with_joins(vec![LiveJoin::new(
			LiveSource::Graph(graph.clone()),
			"r",
			LiveJoinKind::Cross,
			None,
		)?])
		.with_projection(LiveProjection::Value(score.clone()));
	let mut actual = evaluate(&store, &base)?.rows.into_values().collect::<Vec<_>>();
	actual.sort();
	assert_eq!(actual, vec![Value::from(1), Value::from(10)]);
	let sum = base.clone().with_projection(LiveProjection::Value(LiveExpr::Call {
		name: "sum".into(),
		arguments: vec![score],
	}));
	assert_eq!(evaluate(&store, &sum)?.rows.values().next(), Some(&Value::from(11)));
	let missing = base.clone().with_joins(vec![LiveJoin::new(
		LiveSource::Graph(graph.clone()),
		"r",
		LiveJoinKind::Left,
		Some(LiveExpr::Literal(false.into())),
	)?]);
	assert!(store.begin().register_live(missing.clone(), LiveOptions::default()).is_err());
	let mut tx = store.begin();
	tx.define_function(SchemaFunction::new(
		"fn",
		"score",
		vec![SchemaFunctionParameter::new("v", Type::Any)?],
		Type::Int,
		FunctionEffect::Pure,
		false,
		None,
		"{ RETURN 99; }",
	)?)?;
	tx.commit()?;
	assert_eq!(evaluate(&store, &missing)?.rows.values().next(), Some(&Value::from(99)));
	let huge = base.with_joins(
		(0..25)
			.map(|index| {
				LiveJoin::new(
					LiveSource::Graph(graph.clone()),
					format!("r{index}"),
					LiveJoinKind::Cross,
					None,
				)
			})
			.collect::<FerrosGrynnResult<Vec<_>>>()?,
	);
	assert!(store.begin().register_live(huge, LiveOptions::default()).is_err());
	assert!(store.inner.current_root().live_catalog().queries.is_empty());
	Ok(())
}

/// Qualified JOIN/SPLIT identities survive right-only data and binding changes.
#[test]
fn live_join_scoped_split_bindings_and_frozen_sources() -> FerrosGrynnResult<()> {
	use crate::live::{LiveBindingOptions, LiveIdentityPart, LiveJoin, LiveJoinKind};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, [
		ClassField::new("score", Type::Int),
		ClassField::new("tags", Type::Array(Box::new(Type::Int))),
	])?)?;
	let score = setup.intern_property("score")?;
	let tags = setup.intern_property("tags")?;
	let record: RecordId = "item:a".parse()?;
	setup.create_atom(
		record.clone(),
		PropertyMap::from_iter([
			(score, Value::from(0)),
			(tags, Value::from(vec![Value::from(7), Value::from(7)])),
		]),
	)?;
	let left = setup.define_graph("left", GraphKind::Graph)?;
	let right = setup.define_graph("right", GraphKind::Graph)?;
	for (graph, value) in [(&left, 11), (&right, 22)] {
		setup.attach_node(graph.id(), &record, None, None)?;
		setup.set_overlay(graph.id(), &record, score, value.into())?;
	}
	let enabled = setup.define_live_binding(
		LiveExpr::Literal(true.into()),
		Object::new(),
		LiveBindingOptions::default(),
	)?;
	setup.commit()?;
	let field = |source: &str, name: &str| -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Field(LivePath::new(vec![source.into(), name.into()])?))
	};
	let query = LiveQuery::new(LiveSource::GraphRecord {
		graph: left.clone(),
		record: record.clone(),
	})
	.with_alias("l")?
	.with_joins(vec![LiveJoin::new(
		LiveSource::GraphRecord {
			graph: right.clone(),
			record: record.clone(),
		},
		"r",
		LiveJoinKind::Inner,
		Some(LiveExpr::Binding(enabled)),
	)?])
	.with_split(vec![field("l", "tags")?])
	.with_projection(LiveProjection::Value(LiveExpr::Array(vec![
		field("l", "score")?,
		field("r", "score")?,
		field("l", "tags")?,
	])));
	let root = store.inner.current_root();
	let rows = evaluate_query(
		&root,
		&query,
		&|id| root.live_catalog().binding_value(id),
		&LiveOptions::default(),
		std::env::temp_dir(),
		&|_| {},
	)?
	.rows;
	assert_eq!(rows.len(), 2);
	for id in rows.keys() {
		assert!(matches!(id, LiveIdentity::Composite(parts)
			if matches!(parts.as_slice(), [LiveIdentityPart::Occurrence(l), LiveIdentityPart::Occurrence(r), LiveIdentityPart::Split(_)]
				if l.graph().id() == left.id() && r.graph().id() == right.id()
				&& l.atom().id() == &record && r.atom().id() == &record)));
	}
	let mut handle = store.subscribe_live(query, LiveOptions::default())?;
	let mut update = store.begin();
	update.set_overlay(right.id(), &record, score, 33.into())?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("right-only JOIN delivery missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 2
		&& batch.events().iter().all(|event| event.after() == Some(&Value::from(vec![Value::from(11), Value::from(33), Value::from(7)])))));
	handle.ack(delivery.token())?;
	let mut update = store.begin();
	update.replace_live_binding_value(enabled, false.into())?;
	update.commit()?;
	let LiveNext::Delivery(delivery) = handle.next()? else {
		return Err(FerrosGrynnError::invalid_argument("JOIN ON binding delivery missing"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Batch(batch) if batch.events().len() == 2
		&& batch.events().iter().all(|event| event.before().is_some() && event.after().is_none())));
	handle.ack(delivery.token())?;
	let text = "LIVE SELECT VALUE [l.score, r.score] FROM item:a IN GRAPH $first AS l CROSS JOIN \
	            item:a IN GRAPH $second AS r;";
	let parameters = Object::from_iter([
		("first", Value::from(left.clone())),
		("second", Value::from(right.clone())),
	]);
	let result = store
		.execute(text, &parameters)?
		.into_iter()
		.next()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("frozen JOIN result missing"))?
		.result?;
	let Value::Uuid(id) = result.value() else {
		return Err(FerrosGrynnError::invalid_argument("frozen JOIN id missing"));
	};
	let info = store.begin().live_info(crate::live::LiveId::from_uuid(*id)?)?;
	assert!(
		matches!(info.query().source(), LiveSource::GraphRecord { graph, .. } if graph.id() == left.id())
	);
	assert!(
		matches!(info.query().joins()[0].source(), LiveSource::GraphRecord { graph, .. } if graph.id() == right.id())
	);
	assert_eq!(
		evaluate(&store, info.query())?.rows.into_values().next(),
		Some(Value::from(vec![Value::from(11), Value::from(33)]))
	);
	Ok(())
}

/// Public qualifiers preserve exact values, identities and lexical captures.
#[test]
fn live_window_source_alias_sdk_and_nested_captures() -> FerrosGrynnResult<()> {
	let (store, class) = fixture()?;
	let path = || -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Field(LivePath::new(vec!["P".into(), "age".into()])?))
	};
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_alias("p")?
		.with_projection(LiveProjection::Value(path()?));
	assert_eq!(
		evaluate(&store, &query)?.rows,
		evaluate(
			&store,
			&LiveQuery::new(LiveSource::Class(class))
				.with_projection(LiveProjection::Value(LiveExpr::field("age")?))
		)?
		.rows,
	);
	assert_eq!(
		evaluate(&store, &query)?.rows.into_values().collect::<Vec<_>>(),
		oracle(&store, "SELECT VALUE P.age FROM item AS p;")?,
	);
	let nested = LiveQuery::new(LiveSource::Record("item:b".parse()?))
		.with_alias("p")?
		.with_only(true)
		.with_parameters(Object::from_iter([("v", 22)]))
		.with_projection(LiveProjection::Value(LiveExpr::Array(vec![
			path()?,
			LiveExpr::Parameter("v".into()),
		])));
	let outer = LiveQuery::new(LiveSource::Record("item:a".parse()?))
		.with_alias("p")?
		.with_parameters(Object::from_iter([("v", 11)]))
		.with_projection(LiveProjection::Value(LiveExpr::Array(vec![
			path()?,
			LiveExpr::Parameter("v".into()),
			LiveExpr::Select(Box::new(nested)),
		])));
	let wide = 9_007_199_254_740_992_i64;
	assert_eq!(
		evaluate(&store, &outer)?.rows.into_values().next(),
		Some(Value::from(vec![
			Value::from(wide + 1),
			Value::from(11),
			Value::from(vec![Value::from(wide), Value::from(22)]),
		]))
	);
	for alias in ["", "bad\0alias"] {
		assert!(LiveQuery::new(LiveSource::Class(class)).with_alias(alias).is_err());
	}
	Ok(())
}

/// Comparison modes and global output bounds reuse ordinary semantics.
#[test]
fn live_window_sdk_matches_select() -> FerrosGrynnResult<()> {
	let (store, class) = fixture()?;
	let base = LiveQuery::new(LiveSource::Class(class));
	let asc = LiveOrder::by_name("age")?;
	let desc = asc
		.clone()
		.with_direction(LiveOrderDirection::Descending)
		.with_nulls(LiveNullsOrder::First);
	let names = LiveOrder::by_name("name")?.with_collation(true).with_numeric(true);
	for (query, sql) in [
		(base.clone().with_order(vec![asc.clone()]), "SELECT * FROM item ORDER BY age;"),
		(base.clone().with_order(vec![desc]), "SELECT * FROM item ORDER BY age DESC NULLS FIRST;"),
		(base.clone().with_order(vec![names]), "SELECT * FROM item ORDER BY name COLLATE NUMERIC;"),
		(
			base.clone()
				.with_order(vec![asc.clone(), LiveOrder::new(LiveExpr::field("name")?)])
				.with_start_offset(LiveExpr::Literal(1.into()))
				.with_limit(LiveExpr::Literal(2.into())),
			"SELECT * FROM item ORDER BY age, name LIMIT 2 START 1;",
		),
		(base.clone().with_limit(LiveExpr::Literal(0.into())), "SELECT * FROM item LIMIT 0;"),
		(
			base.with_order(vec![asc]).with_start_offset(LiveExpr::Literal(100.into())),
			"SELECT * FROM item ORDER BY age START 100;",
		),
	] {
		let mut actual = evaluate(&store, &query)?.rows.into_values().collect::<Vec<_>>();
		let mut expected = oracle(&store, sql)?;
		actual.sort();
		expected.sort();
		assert_eq!(actual, expected, "{sql}");
	}
	assert!(LiveOrder::by_name("").is_err());
	assert!(LiveOrder::by_name("x\0y").is_err());
	Ok(())
}

/// Bare projected names and explicitly qualified source expressions differ.
#[test]
fn live_window_sdk_alias_and_group_order() -> FerrosGrynnResult<()> {
	let (store, grouped_class) = fixture()?;
	let mut tx = store.begin();
	let class = tx.define_class(ClassSchema::new("plain", AtomKind::Node, [ClassField::new(
		"age",
		Type::Int,
	)])?)?;
	let age = tx.intern_property("age")?;
	for (id, value) in [
		("a", 9_007_199_254_740_993_i64),
		("b", 9_007_199_254_740_992),
		("c", 9_007_199_254_740_993),
	] {
		tx.create_atom(
			format!("plain:{id}").parse()?,
			PropertyMap::from_iter([(age, value.into())]),
		)?;
	}
	tx.commit()?;
	let projection = LiveProjection::Fields(vec![
		LiveField::new(LiveExpr::Negate(Box::new(LiveExpr::field("age")?)))
			.with_alias(LivePath::field("age")?),
	]);
	let base = LiveQuery::new(LiveSource::Class(class))
		.with_projection(projection)
		.with_limit(LiveExpr::Literal(1.into()));
	for (order, sql, id) in [
		(
			LiveOrder::by_name("age")?,
			"SELECT -age AS age FROM plain ORDER BY age LIMIT 1;",
			"plain:a",
		),
		(
			LiveOrder::new(LiveExpr::field("age")?),
			"SELECT -age AS age FROM plain ORDER BY plain.age LIMIT 1;",
			"plain:b",
		),
	] {
		let result = evaluate(&store, &base.clone().with_order(vec![order]))?;
		assert_eq!(result.rows.len(), 1);
		assert!(result.rows.contains_key(&LiveIdentity::Record(id.parse()?)));
		assert_eq!(result.rows.into_values().collect::<Vec<_>>(), oracle(&store, sql)?);
	}
	let count = LiveExpr::Call {
		name: "count".into(),
		arguments: Vec::new(),
	};
	let query = LiveQuery::new(LiveSource::Class(grouped_class))
		.with_group_by(vec![LiveExpr::field("age")?])?
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(count).with_alias(LivePath::field("n")?),
		]))
		.with_order(vec![LiveOrder::by_name("n")?.with_direction(LiveOrderDirection::Descending)])
		.with_limit(LiveExpr::Literal(1.into()));
	let result = evaluate(&store, &query)?;
	assert_eq!(
		result.rows.into_values().collect::<Vec<_>>(),
		oracle(&store, "SELECT count() AS n FROM item GROUP BY age ORDER BY n DESC LIMIT 1;")?
	);
	Ok(())
}

/// Graph class unions apply the single window after global sorting.
#[test]
fn live_window_sdk_graph_union_is_global() -> FerrosGrynnResult<()> {
	let (store, class) = fixture()?;
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph)?;
	tx.define_class(ClassSchema::new("other", AtomKind::Node, [
		ClassField::new("extra", Type::String),
		ClassField::new("age", Type::Int),
	])?)?;
	let age = tx.intern_property("age")?;
	let record: RecordId = "other:a".parse()?;
	tx.create_atom(record.clone(), PropertyMap::from_iter([(age, 1.into())]))?;
	for record in [&record, &"item:b".parse()?] {
		tx.attach_node(graph.id(), record, None, None)?;
	}
	tx.commit()?;
	let query = LiveQuery::new(LiveSource::Graph(graph.clone()))
		.with_order(vec![LiveOrder::new(LiveExpr::field("age")?)])
		.with_start_offset(LiveExpr::Literal(1.into()))
		.with_limit(LiveExpr::Literal(1.into()));
	let result = evaluate(&store, &query)?;
	assert_eq!(result.rows.len(), 1);
	assert!(matches!(result.rows.keys().next(), Some(LiveIdentity::Occurrence(value))
		if value.atom().id() == &"item:b".parse::<RecordId>()? && value.graph().id() == graph.id()));
	let class_query = LiveQuery::new(LiveSource::GraphClass {
		graph,
		class,
	})
	.with_limit(LiveExpr::Literal(0.into()));
	assert!(evaluate(&store, &class_query)?.rows.is_empty());
	Ok(())
}

/// Window changes include unchanged competitors in both active and historical
/// reads.
#[test]
fn live_window_net_history_uses_complete_domain() -> FerrosGrynnResult<()> {
	let (store, class) = fixture()?;
	let floor = store.inner.current_root().commit_ts.into();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_projection(LiveProjection::Value(LiveExpr::field("name")?))
		.with_order(vec![LiveOrder::by_name("age")?])
		.with_limit(LiveExpr::Literal(1.into()));
	let mut active = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let age = store
		.property_dictionary()
		.get("age")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("test age missing"))?;
	let a: RecordId = "item:a".parse()?;
	let b: RecordId = "item:b".parse()?;
	let mut tx = store.begin();
	tx.set_base_property(&a, age, 0.into())?;
	let stamp = tx.commit()?.versionstamp();
	let LiveNext::Delivery(delivery) = active.next()? else {
		return Err(FerrosGrynnError::invalid_argument("window net delivery missing"));
	};
	let LivePayload::Batch(batch) = delivery.payload() else {
		return Err(FerrosGrynnError::invalid_argument("window net batch missing"));
	};
	assert_eq!(batch.versionstamp(), stamp);
	assert_eq!(batch.events().len(), 2);
	let expected = BTreeMap::from([
		(LiveIdentity::Record(a.clone()), (None, Some(Value::from("item10")))),
		(LiveIdentity::Record(b), (Some(Value::from("item2")), None)),
	]);
	for event in batch.events() {
		assert_eq!((event.before().cloned(), event.after().cloned()), expected[event.identity()]);
	}
	active.ack(delivery.token())?;
	let mut replay = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))?;
	let LiveNext::Delivery(replayed) = replay.next()? else {
		return Err(FerrosGrynnError::invalid_argument("window history missing"));
	};
	assert!(matches!(replayed.payload(), LivePayload::Batch(saved)
		if saved.events() == batch.events() && saved.versionstamp() == stamp));
	replay.ack(replayed.token())?;
	let mut tx = store.begin();
	tx.set_base_property(&a, age, (-1).into())?;
	tx.commit()?;
	assert!(
		matches!(active.next()?, LiveNext::Idle),
		"unchanged projected membership emits no rank event"
	);
	assert!(matches!(replay.next()?, LiveNext::Idle));
	Ok(())
}

/// Bounds are exact and errors reject registration, including zero-row sources.
#[test]
fn live_window_invalid_bounds_are_not_published() -> FerrosGrynnResult<()> {
	let (store, class) = fixture()?;
	for value in [Value::from(-1), Value::from(0.5_f64), Value::from("1"), Value::from(f64::MAX)] {
		for query in [
			LiveQuery::new(LiveSource::Class(class)).with_limit(LiveExpr::Literal(value.clone())),
			LiveQuery::new(LiveSource::Record("item:missing".parse()?))
				.with_start_offset(LiveExpr::Literal(value.clone()))
				.with_limit(LiveExpr::Literal(0.into())),
		] {
			let mut tx = store.begin();
			assert!(tx.register_live(query, LiveOptions::default()).is_err(), "{value:?}");
		}
	}
	Ok(())
}

/// SDK ONLY keeps ordinary scalar shape without replacing logical identities.
#[test]
fn live_window_only_sdk_matches_select() -> FerrosGrynnResult<()> {
	let (store, class) = fixture()?;
	let base = LiveQuery::new(LiveSource::Class(class))
		.with_only(true)
		.with_projection(LiveProjection::Value(LiveExpr::field("name")?));
	assert!(base.only());
	assert!(!base.clone().with_only(false).only());
	for (query, sql) in [
		(base.clone(), "SELECT ONLY VALUE name FROM item;"),
		(
			base.clone().with_limit(LiveExpr::Literal(0.into())),
			"SELECT ONLY VALUE name FROM item LIMIT 0;",
		),
		(
			base.clone().with_limit(LiveExpr::Literal(1.into())),
			"SELECT ONLY VALUE name FROM item LIMIT 1;",
		),
		(
			base.clone().with_start_offset(LiveExpr::Literal(3.into())),
			"SELECT ONLY VALUE name FROM item START AT 3;",
		),
		(
			base.with_order(vec![LiveOrder::by_name("name")?])
				.with_start_offset(LiveExpr::Literal(1.into()))
				.with_limit(LiveExpr::Literal(1.into())),
			"SELECT ONLY VALUE name FROM item ORDER BY name START AT 1 LIMIT 1;",
		),
	] {
		let expected = store.execute(sql, &Object::new())?.remove(0).result;
		match (expected, evaluate(&store, &query)) {
			(Ok(expected), Ok(actual)) => {
				assert!(actual.rows.len() <= 1);
				assert!(actual.rows.keys().all(|id| matches!(id, LiveIdentity::Record(_))));
				assert_eq!(
					actual.rows.into_values().next().unwrap_or(Value::None),
					*expected.value()
				);
			}
			(Err(expected), Err(actual)) => {
				assert_eq!(actual.status().code(), expected.status().code())
			}
			_ => return Err(FerrosGrynnError::invalid_argument("SDK ONLY differential mismatch")),
		}
	}
	let missing = LiveQuery::new(LiveSource::Record("item:missing".parse()?)).with_only(true);
	assert!(evaluate(&store, &missing)?.rows.is_empty());
	let aggregate = LiveQuery::new(LiveSource::Class(class))
		.with_only(true)
		.with_filter(LiveExpr::Literal(false.into()))
		.with_group_all()
		.with_projection(LiveProjection::Value(LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		}));
	assert_eq!(
		evaluate(&store, &aggregate)?.rows,
		BTreeMap::from([(LiveIdentity::Aggregate(Vec::new()), Value::from(0))])
	);
	Ok(())
}

/// A graph recipe and nested scalar query check ONLY over the whole class
/// union.
#[test]
fn live_window_only_graph_and_nested_sdk() -> FerrosGrynnResult<()> {
	let (store, _class) = fixture()?;
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("plain", AtomKind::Node, [])?)?;
	let graph = tx.define_graph("only_scope", GraphKind::Graph)?;
	let plain: RecordId = "plain:a".parse()?;
	tx.create_atom(plain.clone(), PropertyMap::new())?;
	tx.attach_node(graph.id(), &plain, None, None)?;
	tx.attach_node(graph.id(), &"item:a".parse()?, None, None)?;
	tx.commit()?;
	let inner = LiveQuery::new(LiveSource::Graph(graph.clone()))
		.with_only(true)
		.with_projection(LiveProjection::Value(LiveExpr::field("id")?));
	let Err(error) = evaluate(&store, &inner) else {
		return Err(FerrosGrynnError::invalid_argument("graph ONLY was applied per class"));
	};
	assert_eq!(error.status().code().to_string(), "FG-22F22");
	let selected = inner.with_start_offset(LiveExpr::Literal(1.into()));
	let rows = evaluate(&store, &selected)?.rows;
	assert_eq!(rows.len(), 1);
	assert!(rows.keys().all(|id| matches!(id, LiveIdentity::Occurrence(occurrence)
		if occurrence.graph().id() == graph.id())));
	assert_eq!(rows.into_values().next(), Some(Value::from(plain.clone())));
	let outer = LiveQuery::new(LiveSource::Record("item:a".parse()?))
		.with_projection(LiveProjection::Value(LiveExpr::Select(Box::new(selected))));
	assert_eq!(
		evaluate(&store, &outer)?.rows,
		BTreeMap::from([(LiveIdentity::Record("item:a".parse()?), Value::from(plain))])
	);
	Ok(())
}

/// Unchanged competitors require complete-domain history and fault active ONLY.
#[test]
fn live_window_only_fault_and_history_domain() -> FerrosGrynnResult<()> {
	use crate::live::{LiveBinaryOperator, LiveState};
	let (store, class) = fixture()?;
	let floor = store.inner.current_root().commit_ts.into();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_only(true)
		.with_filter(
			LiveExpr::field("name")?
				.binary(LiveBinaryOperator::Equal, LiveExpr::Literal("item10".into())),
		)
		.with_projection(LiveProjection::Value(LiveExpr::field("name")?));
	let mut active = store.subscribe_live(query.clone(), LiveOptions::default())?;
	let name = store
		.property_dictionary()
		.get("name")
		.ok_or_else(|| FerrosGrynnError::invalid_argument("ONLY fixture name absent"))?;
	let b: RecordId = "item:b".parse()?;
	let mut tx = store.begin();
	tx.set_base_property(&b, name, "item10".into())?;
	tx.commit()?;
	let info = store.begin().live_info(active.id())?;
	assert_eq!(info.state(), LiveState::Paused);
	let diagnostic = info
		.diagnostic()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("ONLY diagnostic absent"))?;
	assert_eq!(diagnostic.status().code().to_string(), "FG-G1F09");
	assert!(
		diagnostic.cause().is_some_and(|cause| cause.status().code().to_string() == "FG-22F22")
	);
	let LiveNext::Delivery(delivery) = active.next()? else {
		return Err(FerrosGrynnError::invalid_argument("ONLY fault delivery absent"));
	};
	assert!(matches!(delivery.payload(), LivePayload::Fault { diagnostic, .. }
		if diagnostic.status().code().to_string() == "FG-G1F09"
			&& diagnostic.cause().is_some_and(|cause|
				cause.status().code().to_string() == "FG-22F22")));
	active.ack(delivery.token())?;
	let mut tx = store.begin();
	tx.set_base_property(&b, name, "item2".into())?;
	tx.commit()?;
	assert!(evaluate(&store, &query)?.rows.len() == 1);
	let Err(error) = store
		.subscribe_live(query, LiveOptions::default().with_since(LiveSince::Versionstamp(floor)))
	else {
		return Err(FerrosGrynnError::invalid_argument("ONLY history missed unchanged competitor"));
	};
	assert_eq!(error.status().code().to_string(), "FG-22F22");
	Ok(())
}
