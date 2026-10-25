//! Differential checks for the common root-bound SELECT result collector.

use std::collections::BTreeSet;

use super::*;
use crate::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::{AtomKind, GraphKind},
	live::{LiveBinaryOperator, LiveExpr, LiveField, LivePath, LiveProjection},
	values::{PropertyMap, RecordId, Type},
};

/// Every SDK key stays source-local in root, scoped and nested point queries.
#[test]
fn live_point_sources_all_keys_and_nested_targets() {
	use crate::{
		test_assertions::AssertResult as _,
		values::{RecordIdPart, Ulid, Uuid},
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("score", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
	let score = setup.intern_property("score").assert_ok();
	let graph = setup.define_graph("points", GraphKind::Graph).assert_ok();
	let keys = [
		RecordIdPart::Int(9_007_199_254_740_993),
		RecordIdPart::String("text".into()),
		RecordIdPart::Uuid(Uuid::now_v7()),
		RecordIdPart::Ulid(Ulid::new()),
		RecordIdPart::Array(vec![Value::from(1), Value::from("part")].into()),
		RecordIdPart::Object(Object::from_iter([("part", 1)])),
	];
	let mut records = Vec::new();
	for (ordinal, key) in keys.into_iter().enumerate() {
		let record = RecordId::new("item", key).assert_ok();
		setup
			.create_atom(record.clone(), PropertyMap::from_iter([(score, Value::from(ordinal))]))
			.assert_ok();
		setup.attach_node(graph.id(), &record, None, None).assert_ok();
		setup.set_overlay(graph.id(), &record, score, Value::from(ordinal + 10)).assert_ok();
		records.push(record);
	}
	setup.commit().assert_ok();
	let evaluate = |query: &LiveQuery| {
		evaluate_query(
			&store.inner.current_root(),
			query,
			&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_| {},
		)
	};
	for (ordinal, record) in records.iter().enumerate() {
		for scoped in [false, true] {
			let source = if scoped {
				LiveSource::GraphRecord {
					graph: graph.clone(),
					record: record.clone(),
				}
			} else {
				LiveSource::Record(record.clone())
			};
			let query = LiveQuery::new(source)
				.with_projection(LiveProjection::Value(LiveExpr::field("score").assert_ok()));
			let actual = evaluate(&query).assert_ok();
			assert_eq!(actual.rows.len(), 1);
			assert_eq!(
				actual.rows.into_values().next(),
				Some(Value::from(
					ordinal
						+ if scoped {
							10
						} else {
							0
						}
				))
			);
			let selected = store
				.inner
				.current_root()
				.atom_by_record(record)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("point fixture missing"))
				.assert_ok()
				.id();
			assert!(
				actual
					.reads
					.iter()
					.all(|key| !matches!(key, ConflictKey::BaseAtom(id) if *id != selected))
			);
			let outer = LiveQuery::new(LiveSource::Record(records[0].clone())).with_projection(
				LiveProjection::Value(LiveExpr::Select(Box::new(query.with_only(true)))),
			);
			assert_eq!(
				evaluate(&outer).assert_ok().rows.into_values().next(),
				Some(Value::from(
					ordinal
						+ if scoped {
							10
						} else {
							0
						}
				))
			);
		}
	}
	let missing = LiveQuery::new(LiveSource::Record(
		RecordId::new("item", RecordIdPart::Array(vec![Value::from("missing")].into())).assert_ok(),
	));
	assert!(evaluate(&missing).assert_ok().rows.is_empty());
	let count = LiveExpr::Select(Box::new(missing.with_projection(LiveProjection::Value(
		LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		},
	))));
	let outer = LiveQuery::new(LiveSource::Record(records[0].clone()))
		.with_projection(LiveProjection::Value(count));
	assert_eq!(evaluate(&outer).assert_ok().rows.into_values().next(), Some(Value::from(0)));
}

/// Bound source metadata cannot freeze a mutable slot or a different class.
#[test]
fn live_point_source_metadata_requires_exact_immutable_literal() {
	use crate::{
		gql::root_catalog::RootCatalog,
		syn::{ParserSettings, SelectTarget, StatementKind, parse},
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	let catalog = RootCatalog(&root);
	let text = "SELECT * FROM item;";
	let mut program = parse(text, ParserSettings::default()).assert_ok();
	let StatementKind::Select(select) = &mut program.statements[0].kind else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("point statement missing"));
			test_result.assert_ok();
		};
	};
	select.source.kind = crate::syn::SelectSourceKind::Atoms(SelectTarget::BoundRecord {
		class: super::super::identifier("item"),
		parameter: super::super::identifier("key"),
	});
	for (value, mutable, accepted) in [
		(Value::from("item:a".parse::<RecordId>().assert_ok()), false, true),
		(Value::from("item:a".parse::<RecordId>().assert_ok()), true, false),
		(Value::from("other:a".parse::<RecordId>().assert_ok()), false, false),
		(Value::from(1), false, false),
	] {
		let parameters = Object::from_iter([("key", value)]);
		let mutable = if mutable {
			BTreeSet::from(["key".into()])
		} else {
			BTreeSet::new()
		};
		assert_eq!(
			crate::gql::analysis::analyze_statement_with_mutable(
				&program.statements[0],
				text,
				&catalog,
				&parameters,
				&mutable
			)
			.is_ok(),
			accepted
		);
	}
	assert!(
		crate::gql::analysis::analyze_statement(
			&program.statements[0],
			text,
			&catalog,
			&Object::new()
		)
		.is_err()
	);
}

/// Mid-scan cancellation is inherited by the root reader; partial rows are
/// discarded and interruption is not retained by ordinary reevaluation.
#[test]
fn live_query_initial_scan_inherits_registration_cancellation() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class =
		setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	for key in 0..100 {
		setup
			.create_atom(format!("item:{key}").parse().assert_ok(), PropertyMap::new())
			.assert_ok();
	}
	setup.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class));
	let token = crate::gql::QueryCancellation::default();
	let control = crate::gql::ExecutionControl::new(
		&crate::gql::QueryExecutionOptions {
			cancellation: Some(token.clone()),
			..crate::gql::QueryExecutionOptions::default()
		},
		None,
	);
	let seen = Cell::new(0);
	let scanning = Cell::new(false);
	let result = super::evaluate_query_controlled(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
		&LiveOptions::default(),
		std::env::temp_dir(),
		&|key| {
			if key == ConflictKey::BaseClassRange(class) {
				scanning.set(true);
			}
			if scanning.get() && key == ConflictKey::SchemaClass(class) {
				seen.set(seen.get() + 1);
				token.cancel();
			}
		},
		None,
		&[],
		Some(&control),
	);
	assert!(result.is_err());
	assert!(seen.get() > 0 && seen.get() < 100);
	assert!(token.is_cancelled());
	let later = evaluate_query(
		&store.inner.current_root(),
		&query,
		&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
		&LiveOptions::default(),
		std::env::temp_dir(),
		&|_| {},
	)
	.assert_ok();
	assert_eq!(later.rows.len(), 100);
}

/// Caller checks reach named PURE compilation without another wall clock.
#[test]
fn live_query_initial_compiler_and_deadline_inherit_checks() {
	use crate::test_assertions::AssertResult as _;

	let token = crate::gql::QueryCancellation::default();
	let control = crate::gql::ExecutionControl::new(
		&crate::gql::QueryExecutionOptions {
			cancellation: Some(token.clone()),
			..crate::gql::QueryExecutionOptions::default()
		},
		None,
	);
	let compiler = crate::gql::closure::LanguageClosureState::bounded_with_work(1 << 20, 1_000)
		.with_compile_checks(Some(control.interruption()));
	let _reservation = compiler.admit_compile(1).assert_ok();
	token.cancel();
	assert!(compiler.admit_compile(1).is_err());
	let deadline = crate::gql::ExecutionControl::new(
		&crate::gql::QueryExecutionOptions {
			statement_timeout: Some(std::time::Duration::ZERO),
			..crate::gql::QueryExecutionOptions::default()
		},
		None,
	);
	let child =
		crate::gql::ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None)
			.inherit_checks(&deadline);
	assert_eq!(child.deadline(), deadline.deadline());
	assert!(child.check().is_err());
}

/// Ordinary SELECT and root-bound collection retain exact projection/FETCH
/// values.
#[test]
fn live_query_rows_where_fetch_aliases_and_aggregate() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [
				ClassField::new("age", Type::Int),
				ClassField::new("manager", Type::Option(Box::new(Type::Record))),
			])
			.assert_ok(),
		)
		.assert_ok();
	let age = tx.intern_property("age").assert_ok();
	let manager = tx.intern_property("manager").assert_ok();
	let alice: RecordId = "person:alice".parse().assert_ok();
	let bob: RecordId = "person:bob".parse().assert_ok();
	tx.create_atom(
		alice.clone(),
		PropertyMap::from_iter([(age, 19_i64.into()), (manager, Value::from(bob.clone()))]),
	)
	.assert_ok();
	tx.create_atom(bob.clone(), PropertyMap::from_iter([(age, 40_i64.into())])).assert_ok();
	tx.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_filter(
			LiveExpr::field("age")
				.assert_ok()
				.binary(LiveBinaryOperator::Less, LiveExpr::Literal(30_i64.into())),
		)
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("manager").assert_ok())
				.with_alias(LivePath::new(vec!["nested".into(), "boss".into()]).assert_ok()),
		]))
		.with_fetch(vec![LivePath::new(vec!["nested".into(), "boss".into()]).assert_ok()]);
	let evaluate = |query: &LiveQuery| {
		evaluate_query(
			&store.inner.current_root(),
			query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
	};
	let before = evaluate(&query).assert_ok();
	assert_eq!(before.rows.len(), 1);
	let projected = before
		.rows
		.get(&LiveIdentity::Record(alice.clone()))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("missing test source"))
		.assert_ok();
	let Value::Object(object) = projected else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("expected projected object"));
			test_result.assert_ok();
		};
	};
	let Some(Value::Object(nested)) = object.get("nested") else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("nested alias was lost"));
			test_result.assert_ok();
		};
	};
	let Some(Value::Object(boss)) = nested.get("boss") else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("nested FETCH was skipped"));
			test_result.assert_ok();
		};
	};
	assert_eq!(boss.get("age"), Some(&Value::from(40_i64)));
	assert!(
		before.reads.contains(&ConflictKey::BaseClassRange(class)),
		"class range retains WHERE=false and same-class FETCH dependencies"
	);
	let mut change = store.begin();
	change.set_base_property(&bob, age, 25_i64.into()).assert_ok();
	change.commit().assert_ok();
	let after = evaluate(&query).assert_ok();
	assert_eq!(after.rows.len(), 2);
	assert_ne!(
		before.rows.get(&LiveIdentity::Record(alice.clone())),
		after.rows.get(&LiveIdentity::Record(alice))
	);
	let aggregate = LiveQuery::new(LiveSource::Class(class)).with_projection(
		LiveProjection::Value(LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		}),
	);
	assert_eq!(
		evaluate(&aggregate).assert_ok().rows.get(&LiveIdentity::Aggregate(Vec::new())),
		Some(&Value::from(2_i64))
	);
	let none = LiveQuery::new(LiveSource::Record(bob.clone()))
		.with_projection(LiveProjection::Value(LiveExpr::Literal(Value::None)));
	assert_eq!(
		evaluate(&none).assert_ok().rows.get(&LiveIdentity::Record(bob)),
		Some(&Value::None)
	);
	let complete = evaluate(&query).assert_ok();
	let limit = LiveOptions::default().with_budgets(64 << 20, complete.work_used - 1).assert_ok();
	assert!(
		evaluate_query(
			&store.inner.current_root(),
			&query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&limit,
			std::env::temp_dir(),
			&|_key| {}
		)
		.is_err()
	);
}

/// Literal OMIT paths share ordinary projection/FETCH and keep result
/// identities.
#[test]
fn live_query_omit_paths_and_identities() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx
		.define_class(
			ClassSchema::new("person", AtomKind::Node, [
				ClassField::new("age", Type::Int),
				ClassField::new("manager", Type::Option(Box::new(Type::Record))),
			])
			.assert_ok(),
		)
		.assert_ok();
	let age = tx.intern_property("age").assert_ok();
	let manager = tx.intern_property("manager").assert_ok();
	let alice: RecordId = "person:alice".parse().assert_ok();
	let bob: RecordId = "person:bob".parse().assert_ok();
	tx.create_atom(
		alice.clone(),
		PropertyMap::from_iter([(age, 19.into()), (manager, Value::from(bob.clone()))]),
	)
	.assert_ok();
	tx.create_atom(bob.clone(), PropertyMap::from_iter([(age, 40.into())])).assert_ok();
	tx.commit().assert_ok();
	let evaluate = |query: &LiveQuery| {
		evaluate_query(
			&store.inner.current_root(),
			query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_| {},
		)
	};
	let path =
		|fields: &[&str]| LivePath::new(fields.iter().map(|field| (*field).into()).collect());
	let query = LiveQuery::new(LiveSource::Record(alice.clone()))
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("age").assert_ok())
				.with_alias(path(&["nested", "age"]).assert_ok()),
			LiveField::new(LiveExpr::field("manager").assert_ok())
				.with_alias(path(&["nested", "boss"]).assert_ok()),
			LiveField::new(LiveExpr::Literal(5.into()))
				.with_alias(LivePath::field("literal.dot").assert_ok()),
		]))
		.with_omit(vec![
			LivePath::field("id").assert_ok(),
			path(&["nested", "age"]).assert_ok(),
			LivePath::field("literal.dot").assert_ok(),
			path(&["nested", "age"]).assert_ok(),
			LivePath::field("absent").assert_ok(),
			path(&["nested", "boss", "age"]).assert_ok(),
		])
		.with_fetch(vec![path(&["nested", "boss"]).assert_ok()]);
	assert_eq!(query.omit()[2].segments().len(), 1);
	let result = evaluate(&query).assert_ok();
	let image = result
		.rows
		.get(&LiveIdentity::Record(alice.clone()))
		.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT row missing"))
		.assert_ok();
	let object = image
		.as_object()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT object missing"))
		.assert_ok();
	assert!(object.get("id").is_none() && object.get("literal.dot").is_none());
	let nested = object
		.get("nested")
		.and_then(Value::as_object)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("nested OMIT missing"))
		.assert_ok();
	assert!(nested.get("age").is_none());
	assert!(
		matches!(nested.get("boss"), Some(Value::Object(boss)) if boss.get("age") == Some(&40.into()))
	);
	let bob_global = store
		.inner
		.current_root()
		.atom_by_record(&bob)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("boss missing"))
		.assert_ok()
		.id();
	assert!(result.reads.contains(&ConflictKey::BaseAtom(bob_global)));
	let omitted = query.with_omit(vec![path(&["nested", "boss"]).assert_ok()]);
	assert!(
		!evaluate(&omitted).assert_ok().reads.contains(&ConflictKey::BaseAtom(bob_global)),
		"OMIT precedes FETCH and removes its external read"
	);
	for value in [
		Value::from(7),
		Value::Array(vec![Value::Object(Object::from_iter([("x", Value::from(1))]))].into()),
	] {
		let scalar = LiveQuery::new(LiveSource::Record(alice.clone()))
			.with_projection(LiveProjection::Value(LiveExpr::Literal(value.clone())))
			.with_omit(vec![LivePath::field("x").assert_ok()]);
		assert_eq!(
			evaluate(&scalar).assert_ok().rows.get(&LiveIdentity::Record(alice.clone())),
			Some(&value)
		);
	}
	let grouped = LiveQuery::new(LiveSource::Class(class))
		.with_group_by(vec![LiveExpr::field("age").assert_ok()])
		.assert_ok()
		.with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::field("age").assert_ok())
				.with_alias(LivePath::field("key").assert_ok()),
			LiveField::new(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			})
			.with_alias(LivePath::field("n").assert_ok()),
		]))
		.with_omit(vec![LivePath::field("key").assert_ok()]);
	let result = evaluate(&grouped).assert_ok();
	assert_eq!(result.rows.len(), 2);
	for key in [19, 40] {
		let image = result
			.rows
			.get(&LiveIdentity::Aggregate(vec![Value::from(key)]))
			.ok_or_else(|| FerrosGrynnError::invalid_argument("OMIT group missing"))
			.assert_ok();
		assert!(
			matches!(image, Value::Object(object) if object.get("key").is_none() && object.get("n") == Some(&1.into()))
		);
	}
}

/// Qualified identities and unknown FETCH dependencies preserve exact scope.
#[test]
fn live_query_qualified_identity_absent_fetch_and_resource_errors() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("link", Type::Record)])
				.assert_ok(),
		)
		.assert_ok();
	let link = tx.intern_property("link").assert_ok();
	let record: RecordId = "item:a".parse().assert_ok();
	tx.create_atom(
		record.clone(),
		PropertyMap::from_iter([(link, Value::from("missing:no".parse::<RecordId>().assert_ok()))]),
	)
	.assert_ok();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	tx.attach_node(graph.id(), &record, None, None).assert_ok();
	tx.commit().assert_ok();
	for source in [
		LiveSource::GraphClass {
			graph: graph.clone(),
			class,
		},
		LiveSource::GraphRecord {
			graph: graph.clone(),
			record: record.clone(),
		},
	] {
		let query = LiveQuery::new(source)
			.with_projection(LiveProjection::Fields(vec![LiveField::new(
				LiveExpr::field("link").assert_ok(),
			)]))
			.with_fetch(vec![LivePath::field("link").assert_ok()]);
		let evaluation = evaluate_query(
			&store.inner.current_root(),
			&query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
		.assert_ok();
		assert!(
			matches!(evaluation.rows.keys().next(), Some(LiveIdentity::Occurrence(occurrence)) if occurrence.graph().id() == graph.id() && occurrence.atom().id() == &record)
		);
		let limited = LiveOptions::default().with_budgets(1024, 1).assert_ok();
		assert!(
			evaluate_query(
				&store.inner.current_root(),
				&query,
				&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
				&limited,
				std::env::temp_dir(),
				&|_key| {}
			)
			.is_err()
		);
	}
}

/// A whole graph has one aggregate across classes, including an empty catalog.
#[test]
fn live_query_graph_wide_aggregate_unifies_classes_and_empty_domain() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Metagraph).assert_ok();
	tx.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Graph(graph.clone())).with_projection(
		LiveProjection::Value(LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		}),
	);
	let evaluate = || {
		evaluate_query(
			&store.inner.current_root(),
			&query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
	};
	assert_eq!(
		evaluate().assert_ok().rows.get(&LiveIdentity::Aggregate(Vec::new())),
		Some(&Value::from(0_i64))
	);
	let mut create = store.begin();
	create
		.define_class(
			ClassSchema::new("other", AtomKind::Node, [ClassField::new("extra", Type::String)])
				.assert_ok(),
		)
		.assert_ok();
	create.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	for spelling in ["other:a", "item:b", "item:c"] {
		let record: RecordId = spelling.parse().assert_ok();
		let properties = if spelling.starts_with("other") {
			PropertyMap::from_iter([(
				create.intern_property("extra").assert_ok(),
				Value::from("value"),
			)])
		} else {
			PropertyMap::new()
		};
		create.create_atom(record.clone(), properties).assert_ok();
		create.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	create.commit().assert_ok();
	let result = evaluate().assert_ok();
	assert_eq!(result.rows.len(), 1);
	assert_eq!(result.rows.get(&LiveIdentity::Aggregate(Vec::new())), Some(&Value::from(3_i64)));
}

/// Standard fields and VALUE eligibility do not depend on class count.
#[test]
fn live_query_empty_atom_columns_and_collection_modes_are_consistent() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Metagraph).assert_ok();
	tx.commit().assert_ok();
	for count in 0..3 {
		let mut tx = store.begin();
		let source = LiveSource::Graph(graph.clone());
		let count_id =
			LiveQuery::new(source.clone()).with_projection(LiveProjection::Value(LiveExpr::Call {
				name: "count".into(),
				arguments: vec![LiveExpr::field("id").assert_ok()],
			}));
		let result = evaluate_query(
			&store.inner.current_root(),
			&count_id,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
		.assert_ok();
		assert_eq!(
			result.rows.get(&LiveIdentity::Aggregate(Vec::new())),
			Some(&Value::from(count))
		);
		let filtered = LiveQuery::new(source.clone())
			.with_projection(LiveProjection::Value(LiveExpr::field("id").assert_ok()))
			.with_filter(LiveExpr::field("id").assert_ok().binary(
				LiveBinaryOperator::Equal,
				LiveExpr::Literal(Value::from("item0:a".parse::<RecordId>().assert_ok())),
			));
		tx.register_live(filtered, LiveOptions::default()).assert_ok();
		for projection in [
			LiveProjection::Records,
			LiveProjection::Fields(vec![LiveField::new(LiveExpr::field("id").assert_ok())]),
		] {
			let expression = LiveExpr::Literal(Value::Null).binary(
				LiveBinaryOperator::In,
				LiveExpr::Select(Box::new(
					LiveQuery::new(source.clone()).with_projection(projection),
				)),
			);
			let invalid =
				LiveQuery::new(source.clone()).with_projection(LiveProjection::Value(expression));
			assert!(tx.register_live(invalid, LiveOptions::default()).is_err(), "{count} classes");
		}
		let expression = LiveExpr::Literal(Value::from("item0:a".parse::<RecordId>().assert_ok()))
			.binary(
				LiveBinaryOperator::In,
				LiveExpr::Select(Box::new(
					LiveQuery::new(source)
						.with_projection(LiveProjection::Value(LiveExpr::field("id").assert_ok())),
				)),
			);
		let result = crate::gql::live_lowering::evaluate_binding(
			&store.inner.current_root(),
			&expression,
			&crate::values::Object::new(),
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&crate::live::LiveBindingOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
		.assert_ok();
		assert_eq!(result.value, Value::from(count != 0));
		let spelling = format!("item{count}");
		tx.define_class(ClassSchema::new(spelling.clone(), AtomKind::Node, []).assert_ok())
			.assert_ok();
		let record: RecordId = format!("{spelling}:a").parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_node(graph.id(), &record, None, None).assert_ok();
		tx.commit().assert_ok();
	}
}

/// Root and nested whole-graph scopes are read once independently of class
/// count, including the empty domain; class namespace reads remain recorded.
#[test]
fn live_query_graph_expression_scope_is_single() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup
		.define_class(
			ClassSchema::new("policy", AtomKind::Node, [ClassField::new("scope", Type::String)])
				.assert_ok(),
		)
		.assert_ok();
	let key = setup.intern_property("scope").assert_ok();
	let policy: RecordId = "policy:a".parse().assert_ok();
	setup.create_atom(policy.clone(), PropertyMap::from_iter([(key, "scope".into())])).assert_ok();
	let graph = setup.define_graph("scope", GraphKind::Metagraph).assert_ok();
	setup.commit().assert_ok();
	let scope = LiveExpr::Access {
		base: Box::new(LiveExpr::Literal(Value::from(policy.clone()))),
		field: "scope".into(),
		null_safe: false,
	};
	let mut expected_reads = [None; 3];
	for count in 0..4_i64 {
		let root = store.inner.current_root();
		let policy_id = root
			.atom_by_record(&policy)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("scope policy missing"))
			.assert_ok()
			.id();
		let query = LiveQuery::new(LiveSource::GraphExpr(Box::new(scope.clone()))).with_projection(
			LiveProjection::Value(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			}),
		);
		for (mode, expected_reads) in expected_reads.iter_mut().enumerate() {
			let seen = Cell::new(0);
			let namespace = Cell::new(false);
			let observe = |key| {
				if key == ConflictKey::ClassCatalogRange {
					namespace.set(true);
				}
				if key == ConflictKey::BaseAtom(policy_id) {
					seen.set(seen.get() + 1);
				}
			};
			if mode == 0 {
				let result = evaluate_query(
					&root,
					&query,
					&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
					&LiveOptions::default(),
					std::env::temp_dir(),
					&observe,
				)
				.assert_ok();
				assert_eq!(
					result.capture_source,
					crate::changefeed::ChangefeedSource::Graph(graph.id())
				);
				assert_eq!(
					result.rows.get(&LiveIdentity::Aggregate(Vec::new())),
					Some(&Value::from(count))
				);
			} else {
				let expression = if mode == 1 {
					LiveExpr::Select(Box::new(query.clone()))
				} else {
					LiveExpr::Literal(Value::from("item0:a".parse::<RecordId>().assert_ok()))
						.binary(
							LiveBinaryOperator::In,
							LiveExpr::Select(Box::new(
								LiveQuery::new(query.source().clone()).with_projection(
									LiveProjection::Value(LiveExpr::field("id").assert_ok()),
								),
							)),
						)
				};
				let result = crate::gql::live_lowering::evaluate_binding(
					&root,
					&expression,
					&Object::new(),
					&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
					&LiveBindingOptions::default(),
					std::env::temp_dir(),
					&observe,
				)
				.assert_ok();
				assert_eq!(
					result.value,
					if mode == 1 {
						Value::from(count)
					} else {
						Value::from(count > 0)
					}
				);
			}
			assert!(seen.get() > 0, "scope dependency missing");
			assert!(namespace.get(), "class namespace dependency missing");
			if let Some(expected) = expected_reads {
				assert_eq!(seen.get(), *expected, "mode {mode}, {count} attached classes");
			} else {
				*expected_reads = Some(seen.get());
			}
		}
		let mut add = store.begin();
		add.define_class(ClassSchema::new(format!("item{count}"), AtomKind::Node, []).assert_ok())
			.assert_ok();
		let record: RecordId = format!("item{count}:a").parse().assert_ok();
		add.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		add.attach_node(graph.id(), &record, None, None).assert_ok();
		add.commit().assert_ok();
	}
}

/// A nested query owns its captures for both scalar aggregate and collection
/// unions, even when an outer query shadows the same graph parameter.
#[test]
fn live_query_graph_expression_nested_captures() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let first = setup.define_graph("first", GraphKind::Graph).assert_ok();
	let second = setup.define_graph("second", GraphKind::Graph).assert_ok();
	for (key, graph) in [("a", &first), ("b", &second), ("c", &second)] {
		let record: RecordId = format!("item:{key}").parse().assert_ok();
		setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		setup.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	setup.commit().assert_ok();
	for classes in 1..4 {
		for aggregate in [true, false] {
			let query = LiveQuery::new(LiveSource::GraphExpr(Box::new(LiveExpr::Parameter(
				"scope".into(),
			))))
			.with_parameters(Object::from_iter([("scope", Value::from("first"))]))
			.with_projection(LiveProjection::Value(if aggregate {
				LiveExpr::Call {
					name: "count".into(),
					arguments: Vec::new(),
				}
			} else {
				LiveExpr::field("id").assert_ok()
			}));
			let expression = if aggregate {
				LiveExpr::Select(Box::new(query))
			} else {
				LiveExpr::Literal(Value::from("item:a".parse::<RecordId>().assert_ok()))
					.binary(LiveBinaryOperator::In, LiveExpr::Select(Box::new(query)))
			};
			for outer_parameters in
				[Object::new(), Object::from_iter([("scope", Value::from("second"))])]
			{
				let result = crate::gql::live_lowering::evaluate_binding(
					&store.inner.current_root(),
					&expression,
					&outer_parameters,
					&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
					&LiveBindingOptions::default(),
					std::env::temp_dir(),
					&|_| {},
				)
				.assert_ok();
				assert_eq!(
					result.value,
					if aggregate {
						Value::from(1)
					} else {
						Value::from(true)
					},
					"{classes} classes"
				);
			}
			let outer = LiveQuery::new(LiveSource::GraphExpr(Box::new(LiveExpr::Parameter(
				"scope".into(),
			))))
			.with_parameters(Object::from_iter([("scope", Value::from("second"))]))
			.with_projection(LiveProjection::Value(expression));
			let result = evaluate_query(
				&store.inner.current_root(),
				&outer,
				&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
				&LiveOptions::default(),
				std::env::temp_dir(),
				&|_| {},
			)
			.assert_ok();
			assert_eq!(result.rows.len(), 2);
			assert!(result.rows.values().all(|value| *value
				== if aggregate {
					Value::from(1)
				} else {
					Value::from(true)
				}));
		}
		let mut add = store.begin();
		add.define_class(
			ClassSchema::new(format!("extra{classes}"), AtomKind::Node, []).assert_ok(),
		)
		.assert_ok();
		add.commit().assert_ok();
	}
}

/// Reused public aliases never become nested query capture owners.
#[test]
fn live_join_nested_graph_union_keeps_capture_owners() {
	use crate::{
		live::{LiveJoin, LiveJoinKind},
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	setup.define_class(ClassSchema::new("extra", AtomKind::Node, []).assert_ok()).assert_ok();
	let first = setup.define_graph("first", GraphKind::Graph).assert_ok();
	let second = setup.define_graph("second", GraphKind::Graph).assert_ok();
	for (name, graph) in [("a", &first), ("b", &second), ("c", &second)] {
		let record: RecordId = format!("item:{name}").parse().assert_ok();
		setup.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		setup.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	setup.commit().assert_ok();
	let nested = |scope: &str| -> FerrosGrynnResult<LiveExpr> {
		let source = LiveSource::GraphExpr(Box::new(LiveExpr::Parameter("scope".into())));
		let query = LiveQuery::new(source)
			.with_alias("l")?
			.with_parameters(Object::from_iter([("scope", scope)]))
			.with_joins(vec![LiveJoin::new(
				LiveSource::Record("item:a".parse()?),
				"r",
				LiveJoinKind::Cross,
				None,
			)?])
			.with_projection(LiveProjection::Value(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			}));
		Ok(LiveExpr::Select(Box::new(query)))
	};
	let expression =
		LiveExpr::Array(vec![nested("first").assert_ok(), nested("second").assert_ok()]);
	let result = crate::gql::live_lowering::evaluate_binding(
		&store.inner.current_root(),
		&expression,
		&Object::new(),
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveBindingOptions::default(),
		std::env::temp_dir(),
		&|_| {},
	)
	.assert_ok();
	assert_eq!(result.value, Value::from(vec![Value::from(1), Value::from(2)]));
}

/// Public qualifiers pay for every typed schema/plan clone before analysis.
#[test]
fn live_join_alias_copy_admission_precedes_analysis() {
	use crate::{
		live::{LiveJoin, LiveJoinKind},
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new(
				"item",
				AtomKind::Node,
				(0..16).map(|index| ClassField::new(format!("field{index}"), Type::Int)),
			)
			.assert_ok(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let alias = "r".repeat(16 * 1024);
	for primary in [false, true] {
		let query = LiveQuery::new(LiveSource::Record("item:missing".parse().assert_ok()))
			.with_alias(if primary {
				alias.as_str()
			} else {
				"l"
			})
			.assert_ok()
			.with_joins(vec![
				LiveJoin::new(
					LiveSource::Class(class),
					if primary {
						"r"
					} else {
						alias.as_str()
					},
					LiveJoinKind::Cross,
					None,
				)
				.assert_ok(),
			])
			.with_projection(LiveProjection::Value(LiveExpr::Literal(1.into())));
		let options = LiveOptions::default().with_budgets(320 * 1024, 1_000_000).assert_ok();
		let mut tx = store.begin();
		let result = super::super::validate_registration_controlled(
			&mut tx,
			&store.inner.current_root(),
			&query,
			&options,
			None,
		);
		let Err(error) = result else {
			return {
				let test_result: FerrosGrynnResult<()> = Err(FerrosGrynnError::invalid_argument(
					"long JOIN alias copied without admission",
				));
				test_result.assert_ok();
			};
		};
		assert_eq!(error.status().condition(), crate::gql_status::Condition::ProgramLimitExceeded);
	}
}

/// Zero classes do not bypass scope typing, graph existence, effects or
/// resource limits in either Rust entrypoint.
#[test]
fn live_query_graph_expression_empty_preflight() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let graph = setup.define_graph("empty", GraphKind::Graph).assert_ok();
	setup.commit().assert_ok();
	for scope in [
		LiveExpr::Literal(graph.clone().into()),
		LiveExpr::Literal("empty".into()),
		LiveExpr::Literal(Value::Null),
		LiveExpr::Literal("missing".into()),
		LiveExpr::Call {
			name: "time::now".into(),
			arguments: Vec::new(),
		},
	] {
		let valid = matches!(&scope, LiveExpr::Literal(Value::GraphRef(_)))
			|| scope == LiveExpr::Literal("empty".into());
		let query = LiveQuery::new(LiveSource::GraphExpr(Box::new(scope))).with_projection(
			LiveProjection::Value(LiveExpr::Call {
				name: "count".into(),
				arguments: Vec::new(),
			}),
		);
		let root = store.inner.current_root();
		let result = evaluate_query(
			&root,
			&query,
			&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_| {},
		);
		assert_eq!(result.is_ok(), valid);
		if let Ok(result) = result {
			assert_eq!(
				result.rows.get(&LiveIdentity::Aggregate(Vec::new())),
				Some(&Value::from(0))
			);
		}
		let result = crate::gql::live_lowering::evaluate_binding(
			&root,
			&LiveExpr::Select(Box::new(query)),
			&Object::new(),
			&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
			&LiveBindingOptions::default(),
			std::env::temp_dir(),
			&|_| {},
		);
		assert_eq!(result.is_ok(), valid);
		if let Ok(result) = result {
			assert_eq!(result.value, Value::from(0));
		}
	}
	let query = LiveQuery::new(LiveSource::GraphExpr(Box::new(LiveExpr::Literal(graph.into()))));
	assert!(
		store
			.begin()
			.register_live(query, LiveOptions::default().with_budgets(64, 1).assert_ok())
			.is_err()
	);
}

/// Each class retains its resolved overload inside a single aggregate domain.
#[test]
fn live_query_graph_aggregate_keeps_class_overloads() {
	use crate::{
		datastore::{FunctionEffect, SchemaFunction, SchemaFunctionParameter},
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Metagraph).assert_ok();
	let value = tx.intern_property("v").assert_ok();
	for (class, ty, number, result) in [
		("ints", Type::Int, Value::from(1_i64), 1),
		("floats", Type::Float, Value::from(1.0_f64), 10),
	] {
		tx.define_class(
			ClassSchema::new(class, AtomKind::Node, [ClassField::new("v", ty.clone())]).assert_ok(),
		)
		.assert_ok();
		tx.define_function(
			SchemaFunction::new(
				"fn",
				"score",
				vec![SchemaFunctionParameter::new("v", ty).assert_ok()],
				Type::Int,
				FunctionEffect::Pure,
				false,
				None,
				format!("{{ RETURN {result}; }}"),
			)
			.assert_ok(),
		)
		.assert_ok();
		let record: RecordId = format!("{class}:one").parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::from_iter([(value, number)])).assert_ok();
		tx.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	tx.commit().assert_ok();
	let projection = LiveProjection::Value(LiveExpr::Call {
		name: "sum".into(),
		arguments: vec![LiveExpr::Call {
			name: "fn::score".into(),
			arguments: vec![LiveExpr::field("v").assert_ok()],
		}],
	});
	for source in [
		LiveSource::Graph(graph.clone()),
		LiveSource::GraphExpr(Box::new(LiveExpr::Literal(graph.into()))),
	] {
		let query = LiveQuery::new(source).with_projection(projection.clone());
		let result = evaluate_query(
			&store.inner.current_root(),
			&query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
		.assert_ok();
		assert_eq!(
			result.rows.get(&LiveIdentity::Aggregate(Vec::new())),
			Some(&Value::from(11_i64))
		);
		assert_eq!(result.functions.len(), 2);
		let result = crate::gql::live_lowering::evaluate_binding(
			&store.inner.current_root(),
			&LiveExpr::Select(Box::new(query)),
			&Object::new(),
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveBindingOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
		.assert_ok();
		assert_eq!(result.value, Value::from(11_i64));
		assert_eq!(result.functions.len(), 2);
	}
}

/// Class layouts and row overloads survive a single shared grouping domain.
#[test]
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
fn live_query_group_union_keys_distinct_and_overloads() {
	use crate::{
		datastore::{FunctionEffect, SchemaFunction, SchemaFunctionParameter},
		test_assertions::AssertResult as _,
	};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let key = tx.intern_property("k").assert_ok();
	let value = tx.intern_property("v").assert_ok();
	for (class, ty, values, score) in [
		("ints", Type::Int, vec![Value::from(1_i64), Value::from(2_i64)], 1),
		("floats", Type::Float, vec![Value::from(1.0_f64)], 10),
	] {
		let mut fields = vec![ClassField::new("k", Type::Any), ClassField::new("v", ty.clone())];
		if class == "floats" {
			fields.insert(0, ClassField::new("extra", Type::Any));
		}
		tx.define_class(ClassSchema::new(class, AtomKind::Node, fields).assert_ok()).assert_ok();
		tx.define_function(
			SchemaFunction::new(
				"fn",
				"score",
				vec![SchemaFunctionParameter::new("v", ty).assert_ok()],
				Type::Int,
				FunctionEffect::Pure,
				false,
				None,
				format!("{{ RETURN {score}; }}"),
			)
			.assert_ok(),
		)
		.assert_ok();
		for (index, number) in values.into_iter().enumerate() {
			let record: RecordId = format!("{class}:{index}").parse().assert_ok();
			let group = if class == "ints" {
				Value::from(1_i64)
			} else {
				Value::from(1.0_f64)
			};
			tx.create_atom(record.clone(), PropertyMap::from_iter([(key, group), (value, number)]))
				.assert_ok();
			tx.attach_node(graph.id(), &record, None, None).assert_ok();
		}
	}
	tx.commit().assert_ok();
	let query = "SELECT properties.k AS bucket, count() AS n, avg(properties.v) AS average, \
	             count(DISTINCT properties.v) AS distinct_values, properties.k + \
	             sum(properties.v) AS mix, sum(fn::score(v)) AS score FROM {class} IN GRAPH scope \
	             GROUP BY properties.k;";
	let syntax = ["ints", "floats"].map(|class| query.replace("{class}", class)).join("\n");
	let rows = evaluate_group_union(&store.inner.current_root(), &syntax, 100_000).assert_ok();
	assert_eq!(rows.len(), 1);
	let Some(Value::Object(row)) = rows.get(&LiveIdentity::Aggregate(vec![1_i64.into()])) else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("missing merged group"));
			test_result.assert_ok();
		};
	};
	assert_eq!(row.get("bucket"), Some(&Value::from(1_i64)));
	assert_eq!(row.get("n"), Some(&Value::from(3_i64)));
	assert_eq!(row.get("distinct_values"), Some(&Value::from(2_i64)));
	assert_eq!(row.get("mix"), Some(&Value::from(5_i64)));
	assert_eq!(row.get("score"), Some(&Value::from(12_i64)));
	assert_eq!(row.get("average"), Some(&Value::from(4.0_f64 / 3.0_f64)));
	assert!(evaluate_group_union(&store.inner.current_root(), &syntax, 1).is_err());
}

/// Group identities retain exact numeric comparison and distinct absence.
#[test]
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
fn live_query_group_union_exact_keys_and_empty() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let key = tx.intern_property("k").assert_ok();
	let keys = [
		Value::None,
		Value::Null,
		Value::from(1_i64),
		Value::from(1.0_f64),
		Value::from(9_007_199_254_740_992_i64),
		Value::from(9_007_199_254_740_993_i64),
	];
	for class in ["left", "right"] {
		tx.define_class(
			ClassSchema::new(class, AtomKind::Node, [ClassField::new("k", Type::Any)]).assert_ok(),
		)
		.assert_ok();
	}
	for (index, value) in keys.into_iter().enumerate() {
		let record: RecordId = format!(
			"{}:{index}",
			if index % 2 == 0 {
				"left"
			} else {
				"right"
			}
		)
		.parse()
		.assert_ok();
		tx.create_atom(record.clone(), PropertyMap::from_iter([(key, value)])).assert_ok();
		tx.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	tx.commit().assert_ok();
	let syntax = "SELECT VALUE count() FROM left IN GRAPH scope GROUP BY properties.k; SELECT \
	              VALUE count() FROM right IN GRAPH scope GROUP BY properties.k;";
	let rows = evaluate_group_union(&store.inner.current_root(), syntax, 100_000).assert_ok();
	assert_eq!(rows.len(), 5);
	let query = LiveQuery::new(LiveSource::Graph(graph))
		.with_group_by(vec![LiveExpr::field("k").assert_ok()])
		.assert_ok()
		.with_projection(LiveProjection::Value(LiveExpr::Call {
			name: "count".into(),
			arguments: Vec::new(),
		}));
	let evaluate = |query: &LiveQuery, options: &LiveOptions| {
		evaluate_query(
			&store.inner.current_root(),
			query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			options,
			std::env::temp_dir(),
			&|_key| {},
		)
	};
	assert_eq!(evaluate(&query, &LiveOptions::default()).assert_ok().rows, rows);
	assert!(
		evaluate(&query, &LiveOptions::default().with_budgets(64, 100_000).assert_ok()).is_err()
	);
	let evaluate_nested = |expression: &LiveExpr| {
		crate::gql::live_lowering::evaluate_binding(
			&store.inner.current_root(),
			expression,
			&Object::new(),
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveBindingOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
	};
	assert!(evaluate_nested(&LiveExpr::Select(Box::new(query.clone()))).is_err());
	assert_eq!(
		evaluate_nested(
			&LiveExpr::Literal(Value::from(2_i64))
				.binary(LiveBinaryOperator::In, LiveExpr::Select(Box::new(query.clone())))
		)
		.assert_ok()
		.value,
		Value::from(true)
	);
	let empty_query = query.with_filter(LiveExpr::Literal(Value::from(false)));
	assert!(evaluate(&empty_query, &LiveOptions::default()).assert_ok().rows.is_empty());
	assert_eq!(
		evaluate(&empty_query.with_group_all(), &LiveOptions::default())
			.assert_ok()
			.rows
			.get(&LiveIdentity::Aggregate(Vec::new())),
		Some(&Value::from(0_i64))
	);
	for (key, count) in [
		(Value::None, 1),
		(Value::Null, 1),
		(Value::from(1_i64), 2),
		(Value::from(9_007_199_254_740_992_i64), 1),
		(Value::from(9_007_199_254_740_993_i64), 1),
	] {
		assert_eq!(rows.get(&LiveIdentity::Aggregate(vec![key])), Some(&Value::from(count)));
	}
	let empty = syntax.replace("GROUP BY", "WHERE false GROUP BY");
	assert!(
		evaluate_group_union(&store.inner.current_root(), &empty, 100_000).assert_ok().is_empty()
	);
	let all = "SELECT VALUE count() FROM left IN GRAPH scope WHERE false GROUP ALL; SELECT VALUE \
	           count() FROM right IN GRAPH scope WHERE false GROUP ALL;";
	assert_eq!(
		evaluate_group_union(&store.inner.current_root(), all, 100_000)
			.assert_ok()
			.get(&LiveIdentity::Aggregate(Vec::new())),
		Some(&Value::from(0_i64))
	);
}

/// Equal current values do not merge different lexical or reactive group keys.
#[test]
fn live_group_capture_provenance() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("k", Type::Int)]).assert_ok(),
		)
		.assert_ok();
	let first = setup
		.define_live_binding(
			LiveExpr::Literal(0.into()),
			Object::new(),
			LiveBindingOptions::default(),
		)
		.assert_ok();
	let second = setup
		.define_live_binding(
			LiveExpr::Literal(0.into()),
			Object::new(),
			LiveBindingOptions::default(),
		)
		.assert_ok();
	setup.commit().assert_ok();
	let query = |projected: LiveExpr, grouped: LiveExpr| -> FerrosGrynnResult<LiveQuery> {
		Ok(LiveQuery::new(LiveSource::Class(class))
			.with_group_by(vec![LiveExpr::field("k")?.binary(LiveBinaryOperator::Add, grouped)])?
			.with_projection(LiveProjection::Value(
				LiveExpr::field("k")?.binary(LiveBinaryOperator::Add, projected),
			))
			.with_parameters(Object::from_iter([("left", 0), ("right", 0)])))
	};
	let mut tx = store.begin();
	assert!(
		tx.register_live(
			query(LiveExpr::Binding(first), LiveExpr::Binding(second)).assert_ok(),
			LiveOptions::default(),
		)
		.is_err()
	);
	assert!(
		tx.register_live(
			query(LiveExpr::Parameter("left".into()), LiveExpr::Parameter("right".into()))
				.assert_ok(),
			LiveOptions::default(),
		)
		.is_err()
	);
	tx.register_live(
		query(LiveExpr::Binding(first), LiveExpr::Binding(first)).assert_ok(),
		LiveOptions::default(),
	)
	.assert_ok();
	tx.register_live(
		query(LiveExpr::Parameter("left".into()), LiveExpr::Parameter("left".into())).assert_ok(),
		LiveOptions::default(),
	)
	.assert_ok();
	tx.register_live(
		query(LiveExpr::Literal(0.into()), LiveExpr::Literal(0.into())).assert_ok(),
		LiveOptions::default(),
	)
	.assert_ok();
}

/// Interned immutable literals retain graph descriptor metadata in range
/// bounds.
#[test]
fn live_group_literal_ranges_preserve_graph_metadata() {
	use std::ops::Bound;

	use crate::{
		graph::{GraphRef, GraphStorageMode},
		test_assertions::AssertResult as _,
		values::Range,
	};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let original = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	tx.commit().assert_ok();
	let alternate =
		GraphRef::new(original.id(), "renamed", GraphKind::Graph, GraphStorageMode::Hybrid)
			.assert_ok();
	let expression = LiveExpr::Array(vec![
		LiveExpr::Literal(Range::closed(original.clone(), original.clone()).assert_ok().into()),
		LiveExpr::Literal(Range::closed(alternate.clone(), alternate.clone()).assert_ok().into()),
	]);
	let result = crate::gql::live_lowering::evaluate_binding(
		&store.inner.current_root(),
		&expression,
		&Object::new(),
		&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
		&LiveBindingOptions::default(),
		std::env::temp_dir(),
		&|_| {},
	)
	.assert_ok();
	let Value::Array(values) = result.value else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("range result array missing"));
			test_result.assert_ok();
		};
	};
	assert_eq!(values.len(), 2);
	for (value, expected) in values.iter().zip([original, alternate]) {
		let Value::Range(range) = value else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("range result missing"));
				test_result.assert_ok();
			};
		};
		for bound in [range.start(), range.end()] {
			let Bound::Included(Value::GraphRef(graph)) = bound else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("range graph endpoint missing"));
					test_result.assert_ok();
				};
			};
			assert_eq!(graph.id(), expected.id());
			assert_eq!(graph.name(), expected.name());
			assert_eq!(graph.storage_mode(), expected.storage_mode());
		}
	}
}

/// Exercises typed ordinary SELECT plans before adding owned clause recipes.
#[allow(
	clippy::mutable_key_type,
	reason = "Runtime value and reference keys retain stable logical Eq, Ord and Hash identities"
)]
fn evaluate_group_union(
	root: &Arc<DatabaseRoot>,
	syntax: &str,
	work_limit: u64,
) -> FerrosGrynnResult<BTreeMap<LiveIdentity, Value>> {
	use crate::test_assertions::AssertCondition as _;

	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = crate::gql::ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let remaining = Cell::new(work_limit);
	let work = |amount| {
		remaining.set(remaining.get().checked_sub(amount).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded("group union work", work_limit)
		})?);
		Ok(())
	};
	let read = RootRead::new(root, &control, &|_key| {}, &work, std::env::temp_dir());
	let syntax = crate::gql::parse(syntax, crate::syn::ParserSettings::default())?;
	let parameters = Object::new();
	let logical =
		crate::gql::analysis::analyze_with_runtime(&syntax, "", &read, &parameters, &cache)?;
	let plans = logical
		.statements
		.iter()
		.map(|statement| match statement {
			LogicalStatement::Select(plan) => Ok(plan),
			_ => Err(FerrosGrynnError::invalid_argument("group fixture SELECT missing")),
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	let slots = crate::gql::execution::initialize_slots(&logical, &parameters);
	let mut rows = BTreeMap::new();
	crate::gql::execution::visit_root_aggregate_union(
		&read,
		&plans,
		&slots,
		&control,
		|id, value| {
			(rows.insert(id, value).is_none()).assert_true();
			Ok(())
		},
	)?;
	Ok(rows)
}

/// An empty graph performs the same static output-path preflight as a class.
#[test]
fn live_query_empty_graph_rejects_overlapping_aliases() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("empty", GraphKind::Graph).assert_ok();
	for segments in [vec!["a".into()], vec!["a".into(), "b".into()]] {
		let query = LiveQuery::new(LiveSource::Graph(graph.clone())).with_projection(
			LiveProjection::Fields(vec![
				LiveField::new(LiveExpr::Literal(1_i64.into()))
					.with_alias(LivePath::field("a").assert_ok()),
				LiveField::new(LiveExpr::Literal(2_i64.into()))
					.with_alias(LivePath::new(segments).assert_ok()),
			]),
		);
		assert!(tx.register_live(query, LiveOptions::default()).is_err());
	}
	let query =
		LiveQuery::new(LiveSource::Graph(graph)).with_projection(LiveProjection::Fields(vec![
			LiveField::new(LiveExpr::Literal(1_i64.into()))
				.with_alias(LivePath::new(vec!["a".into(), "b".into()]).assert_ok()),
			LiveField::new(LiveExpr::Literal(2_i64.into()))
				.with_alias(LivePath::new(vec!["a".into(), "c".into()]).assert_ok()),
		]));
	tx.register_live(query, LiveOptions::default()).assert_ok();
}

/// Repeated argumentless counts charge every visited row before reduction.
#[test]
fn live_query_repeated_counts_obey_work_budget() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let class = tx
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("v", Type::Int)]).assert_ok(),
		)
		.assert_ok();
	let key = tx.intern_property("v").assert_ok();
	for index in 0..100 {
		tx.create_atom(
			format!("item:{index}").parse().assert_ok(),
			PropertyMap::from_iter([(key, 0_i64.into())]),
		)
		.assert_ok();
	}
	tx.commit().assert_ok();
	let query = LiveQuery::new(LiveSource::Class(class))
		.with_filter(
			LiveExpr::field("v")
				.assert_ok()
				.binary(LiveBinaryOperator::GreaterOrEqual, LiveExpr::Literal(0_i64.into())),
		)
		.with_projection(LiveProjection::Fields(
			(0..100)
				.map(|index| {
					Ok(LiveField::new(LiveExpr::Call {
						name: "count".into(),
						arguments: Vec::new(),
					})
					.with_alias(LivePath::field(format!("count{index}"))?))
				})
				.collect::<FerrosGrynnResult<_>>()
				.assert_ok(),
		));
	let evaluate = |options: &LiveOptions| {
		evaluate_query(
			&store.inner.current_root(),
			&query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			options,
			std::env::temp_dir(),
			&|_key| {},
		)
	};
	let unlimited = evaluate(&LiveOptions::default()).assert_ok();
	assert!(unlimited.work_used > 10_000);
	let limited = evaluate(
		&LiveOptions::default().with_budgets(64 << 20, unlimited.work_used - 1).assert_ok(),
	);
	assert!(limited.is_err(), "all reduction visits must share the cap");
}

/// Nested graph scalars share zero-domain and multi-class aggregate kernels.
#[test]
fn live_query_nested_graph_scalars_and_outer_aggregate() {
	use crate::{live::LiveBindingOptions, test_assertions::AssertResult as _, values::Object};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Metagraph).assert_ok();
	let count = || LiveExpr::Call {
		name: "count".into(),
		arguments: Vec::new(),
	};
	let graph_count = LiveExpr::Select(Box::new(
		LiveQuery::new(LiveSource::Graph(graph.clone()))
			.with_projection(LiveProjection::Value(count())),
	));
	let binding = tx
		.define_live_binding(graph_count.clone(), Object::new(), LiveBindingOptions::default())
		.assert_ok();
	assert_eq!(tx.live_binding_value(binding).assert_ok(), Value::from(0_i64));
	tx.commit().assert_ok();
	let mut tx = store.begin();
	let marker =
		tx.define_class(ClassSchema::new("marker", AtomKind::Node, []).assert_ok()).assert_ok();
	let item =
		tx.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.create_atom("marker:one".parse().assert_ok(), PropertyMap::new()).assert_ok();
	for index in 0..3 {
		let record: RecordId = format!("item:{index}").parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::new()).assert_ok();
		tx.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	tx.commit().assert_ok();
	assert_eq!(store.begin().live_binding_value(binding).assert_ok(), Value::from(3_i64));
	let evaluate = |query: &LiveQuery| {
		evaluate_query(
			&store.inner.current_root(),
			query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|_key| {},
		)
	};
	let nested = LiveQuery::new(LiveSource::Class(marker))
		.with_projection(LiveProjection::Value(graph_count));
	assert_eq!(
		evaluate(&nested)
			.assert_ok()
			.rows
			.get(&LiveIdentity::Record("marker:one".parse().assert_ok())),
		Some(&Value::from(3_i64))
	);
	let projection = LiveProjection::Value(
		count().binary(
			LiveBinaryOperator::Add,
			LiveExpr::Select(Box::new(
				LiveQuery::new(LiveSource::Class(marker))
					.with_projection(LiveProjection::Value(count())),
			)),
		),
	);
	let graph_query = LiveQuery::new(LiveSource::Graph(graph)).with_projection(projection.clone());
	let class_query = LiveQuery::new(LiveSource::Class(item)).with_projection(projection);
	assert_eq!(
		evaluate(&graph_query).assert_ok().rows.get(&LiveIdentity::Aggregate(Vec::new())),
		Some(&Value::from(4_i64))
	);
	assert_eq!(evaluate(&graph_query).assert_ok().rows, evaluate(&class_query).assert_ok().rows);
}

/// Owned constructors reduce all graph classes together, with ordinary
/// lazy branches and an independent class-domain oracle.
#[test]
fn live_query_aggregate_constructors() {
	use std::ops::Bound;

	use crate::{test_assertions::AssertResult as _, values::Range};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let property = tx.intern_property("v").assert_ok();
	let mut oracle = None;
	for (name, ty, values) in [
		("left", Type::Int, vec![Value::from(1_i64)]),
		("right", Type::Float, vec![Value::from(2.0_f64), Value::from(1.0_f64)]),
		("oracle", Type::Any, vec![Value::from(1_i64), Value::from(2.0_f64), Value::from(1.0_f64)]),
	] {
		let class = tx
			.define_class(
				ClassSchema::new(name, AtomKind::Node, [ClassField::new("v", ty)]).assert_ok(),
			)
			.assert_ok();
		if name == "oracle" {
			oracle = Some(class);
		}
		for (ordinal, value) in values.into_iter().enumerate() {
			let record: RecordId = format!("{name}:{ordinal}").parse().assert_ok();
			tx.create_atom(record.clone(), PropertyMap::from_iter([(property, value)])).assert_ok();
			if name != "oracle" {
				tx.attach_node(graph.id(), &record, None, None).assert_ok();
			}
		}
	}
	tx.commit().assert_ok();
	let oracle = oracle
		.ok_or_else(|| FerrosGrynnError::invalid_argument("oracle class missing"))
		.assert_ok();
	let count = || LiveExpr::Call {
		name: "count".into(),
		arguments: Vec::new(),
	};
	let sum = || -> FerrosGrynnResult<LiveExpr> {
		Ok(LiveExpr::Call {
			name: "sum".into(),
			arguments: vec![LiveExpr::field("v")?],
		})
	};
	let literal = |value| LiveExpr::Literal(Value::from(value));
	let divide_by_zero = || count().binary(LiveBinaryOperator::Divide, literal(0_i64));
	let cases = [
		(
			"array",
			LiveExpr::Array(vec![count(), sum().assert_ok()]),
			Value::Array([3_i64.into(), 4_i64.into()].into_iter().collect()),
		),
		(
			"set",
			LiveExpr::Set(vec![count(), count()]),
			Value::Set([3_i64.into()].into_iter().collect()),
		),
		(
			"object",
			LiveExpr::Object(vec![("total".into(), count())]),
			Value::Object(Object::from_iter([("total", Value::from(3_i64))])),
		),
		(
			"conditional",
			LiveExpr::Conditional {
				condition: Box::new(count().binary(LiveBinaryOperator::Greater, literal(0_i64))),
				then_value: Box::new(sum().assert_ok()),
				else_value: Box::new(divide_by_zero()),
			},
			4_i64.into(),
		),
		(
			"searched_case",
			LiveExpr::Case {
				operand: None,
				branches: vec![(
					count().binary(LiveBinaryOperator::Greater, literal(0_i64)),
					sum().assert_ok(),
				)],
				else_value: Some(Box::new(divide_by_zero())),
			},
			4_i64.into(),
		),
		(
			"simple_case",
			LiveExpr::Case {
				operand: Some(Box::new(count())),
				branches: vec![
					(literal(0_i64), divide_by_zero()),
					(literal(3_i64), sum().assert_ok()),
				],
				else_value: None,
			},
			4_i64.into(),
		),
		(
			"case_no_match",
			LiveExpr::Case {
				operand: None,
				branches: vec![(
					count().binary(LiveBinaryOperator::Equal, literal(0_i64)),
					divide_by_zero(),
				)],
				else_value: None,
			},
			Value::None,
		),
		(
			"field",
			LiveExpr::Access {
				base: Box::new(LiveExpr::Object(vec![("total".into(), count())])),
				field: "total".into(),
				null_safe: false,
			},
			3_i64.into(),
		),
		(
			"null_safe_field",
			LiveExpr::Access {
				base: Box::new(LiveExpr::Conditional {
					condition: Box::new(count().binary(LiveBinaryOperator::Equal, literal(0_i64))),
					then_value: Box::new(LiveExpr::Object(vec![("total".into(), count())])),
					else_value: Box::new(LiveExpr::Literal(Value::None)),
				}),
				field: "total".into(),
				null_safe: true,
			},
			Value::None,
		),
		(
			"index",
			LiveExpr::Index {
				base: Box::new(LiveExpr::Array(vec![count(), sum().assert_ok()])),
				index: Box::new(literal(1_i64)),
			},
			4_i64.into(),
		),
		(
			"cast",
			LiveExpr::Cast {
				target: "int".into(),
				value: Box::new(sum().assert_ok()),
			},
			4_i64.into(),
		),
		(
			"closed_range",
			LiveExpr::Range {
				start: Some(Box::new(count())),
				end: Some(Box::new(sum().assert_ok())),
				inclusive_end: true,
			},
			Value::Range(Range::closed(3_i64, 4_i64).assert_ok()),
		),
		(
			"open_range",
			LiveExpr::Range {
				start: None,
				end: Some(Box::new(count())),
				inclusive_end: false,
			},
			Value::Range(Range::new(Bound::Unbounded, Bound::Excluded(3_i64.into())).assert_ok()),
		),
	];
	let root = store.inner.current_root();
	let evaluate = |query: &LiveQuery, options: &LiveOptions| {
		evaluate_query(
			&root,
			query,
			&|_id| Err(FerrosGrynnError::invalid_argument("missing binding")),
			options,
			std::env::temp_dir(),
			&|_| {},
		)
	};
	for (name, expression, expected) in cases {
		let projection = LiveProjection::Value(expression);
		let graph_query = LiveQuery::new(LiveSource::Graph(graph.clone()))
			.with_projection(projection.clone())
			.with_group_all();
		let class_query =
			LiveQuery::new(LiveSource::Class(oracle)).with_projection(projection).with_group_all();
		let actual = evaluate(&graph_query, &LiveOptions::default())
			.map_err(|error| FerrosGrynnError::invalid_argument(format!("{name}: {error}")))
			.assert_ok();
		assert_eq!(
			actual.rows,
			evaluate(&class_query, &LiveOptions::default()).assert_ok().rows,
			"{name}"
		);
		assert_eq!(
			actual.rows.get(&LiveIdentity::Aggregate(Vec::new())),
			Some(&expected),
			"{name}"
		);
		let limited = LiveOptions::default().with_budgets(256 << 20, 1).assert_ok();
		assert!(evaluate(&graph_query, &limited).is_err(), "{name}");
		let limited = LiveOptions::default().with_budgets(128, 100_000).assert_ok();
		assert!(evaluate(&graph_query, &limited).is_err(), "{name}");
	}
}

/// Constructor descriptors cannot silently select the first class's
/// operation when the typed outer shapes differ, including empty inputs.
#[test]
fn live_query_aggregate_constructors_reject_mismatches() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["left", "right"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
	}
	tx.commit().assert_ok();
	for (left, right) in [
		("[count()]", "[count(), count()]"),
		("{first: count()}", "{second: count()}"),
		("CASE count() WHEN 0 THEN count() END", "CASE WHEN true THEN count() END"),
		("CASE WHEN true THEN count() END", "CASE WHEN true THEN count() ELSE 0 END"),
		("CASE WHEN true THEN count() END", "CASE WHEN true THEN count() WHEN false THEN 0 END"),
		("({first: count()}).first", "({second: count()}).second"),
		("<int>count()", "<float>count()"),
		("count()..=(count() + 1)", "count()..(count() + 1)"),
		("..count()", "count()..count()"),
		("IF true THEN count() ELSE 0 END", "CASE WHEN true THEN count() ELSE 0 END"),
	] {
		let syntax = format!("SELECT VALUE {left} FROM left; SELECT VALUE {right} FROM right;");
		let error = evaluate_group_union(&store.inner.current_root(), &syntax, 100_000)
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("constructor mismatch accepted"))
			.assert_ok();
		assert!(
			error.to_string().contains("incompatible outer typed expressions"),
			"{left} / {right}: {error}"
		);
	}
}

/// Zero-class sources preserve graph reads and validation through both
/// ordinary pull and nested aggregate paths.
#[test]
fn live_query_empty_scope_is_read_and_validated() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let removed = tx.define_graph("removed", GraphKind::Graph).assert_ok();
	tx.commit().assert_ok();
	let mut tx = store.begin();
	tx.remove_graph("removed").assert_ok();
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	for nested in [false, true] {
		for aggregate in [false, true] {
			let input = empty_scope_syntax(aggregate, nested).assert_ok();
			let expected = if aggregate {
				vec![Value::from(0_i64)]
			} else if nested {
				vec![Value::None]
			} else {
				Vec::new()
			};
			for scope in [Value::from("scope"), Value::from(graph.clone())] {
				let reads = RefCell::new(Vec::new());
				let values = evaluate_empty_scope(&root, &input, scope, 100_000, &|key| {
					reads.borrow_mut().push(key);
				})
				.assert_ok();
				assert_eq!(values, expected);
				assert!(reads.borrow().contains(&ConflictKey::GraphDefinition(graph.id())));
			}
			for invalid in [
				Value::Null,
				Value::from(3_i64),
				Value::from("missing"),
				Value::from(removed.clone()),
			] {
				assert!(evaluate_empty_scope(&root, &input, invalid, 100_000, &|_| {}).is_err());
			}
		}
	}
}

/// Empty relations cannot hide effect validation or escape bounded runtime
/// evaluation merely because there is no row to pull.
#[test]
fn live_query_empty_scope_effect_and_budgets() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	for nested in [false, true] {
		let mut input = empty_scope_syntax(true, nested).assert_ok();
		let call =
			crate::gql::parse("time::now();", crate::syn::ParserSettings::default()).assert_ok();
		let StatementKind::Expression(call) = &call.statements[0].kind else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("empty fixture call missing"));
				test_result.assert_ok();
			};
		};
		let select = match &mut input.statements[0].kind {
			StatementKind::Select(select) => select,
			StatementKind::Expression(crate::syn::Expr {
				kind: crate::syn::ExprKind::Subquery(select),
				..
			}) => select,
			_ => {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("empty fixture source missing"));
					test_result.assert_ok();
				};
			}
		};
		select.source.graph = Some(call.clone());
		let error = evaluate_empty_scope(&root, &input, Value::Null, 100_000, &|_| {})
			.err()
			.ok_or_else(|| FerrosGrynnError::invalid_argument("impure scope was admitted"))
			.assert_ok();
		assert!(error.to_string().contains("impure"), "{error}");
		let input = empty_scope_syntax(true, nested).assert_ok();
		assert!(
			evaluate_empty_scope(&root, &input, Value::from(graph.clone()), 0, &|_| {}).is_err()
		);
	}
}

/// Synthesizes the real zero-class relation used by SDK lowering; the rest
/// of the statement remains ordinary parser output.
fn empty_scope_syntax(aggregate: bool, nested: bool) -> FerrosGrynnResult<crate::syn::Program> {
	let source = if aggregate {
		"SELECT VALUE count() FROM absent IN GRAPH $scope;"
	} else {
		"SELECT * FROM absent IN GRAPH $scope LIMIT 0;"
	};
	let mut input = crate::gql::parse(source, crate::syn::ParserSettings::default())?;
	let StatementKind::Select(select) = &mut input.statements[0].kind else {
		return Err(FerrosGrynnError::invalid_argument("empty fixture SELECT missing"));
	};
	select.source.kind = crate::syn::SelectSourceKind::Empty;
	if nested {
		let select = select.clone();
		input.statements[0].kind = StatementKind::Expression({
			let split_field_0 = crate::syn::ExprKind::Subquery(Box::new(select));
			let split_field_1 = span();
			crate::syn::split_internal::new_expr(split_field_0, split_field_1)
		});
	}
	Ok(input)
}

/// Runs the existing analyzer and root-bound executor with an independent
/// dependency observer and a hard work ceiling.
fn evaluate_empty_scope(
	root: &Arc<DatabaseRoot>,
	input: &crate::syn::Program,
	scope: Value,
	work_limit: u64,
	observe: &dyn Fn(ConflictKey),
) -> FerrosGrynnResult<Vec<Value>> {
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = crate::gql::ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let remaining = Cell::new(work_limit);
	let work = |amount| {
		remaining.set(remaining.get().checked_sub(amount).ok_or_else(|| {
			FerrosGrynnError::traversal_resource_limit_exceeded("empty test work", work_limit)
		})?);
		Ok(())
	};
	let read = RootRead::new(root, &control, observe, &work, std::env::temp_dir());
	let parameters = crate::values::Object::from_iter([("scope", scope)]);
	let logical = if let StatementKind::Expression(expression) = &input.statements[0].kind {
		crate::gql::analysis::analyze_reactive_expression(expression, &read, &parameters, &cache)?
	} else {
		crate::gql::analysis::analyze_with_runtime(input, "", &read, &parameters, &cache)?
	};
	crate::gql::optimizer::validate_deterministic_statement(&logical.statements[0])?;
	let slots = crate::gql::execution::initialize_slots(&logical, &parameters);
	let mut values = Vec::new();
	match &logical.statements[0] {
		LogicalStatement::Select(plan) => {
			crate::gql::execution::visit_root_select(
				&read,
				plan,
				&slots,
				&control,
				|_id, value| {
					values.push(value);
					Ok(())
				},
			)?;
		}
		LogicalStatement::Expression {
			value,
		} => {
			let value =
				crate::gql::execution::evaluate_root_expression(&read, value, &slots, &control)?;
			values = match value {
				Value::Array(values) => values.into_iter().collect(),
				value => vec![value],
			};
		}
		_ => return Err(FerrosGrynnError::invalid_argument("empty fixture typed plan missing")),
	}
	Ok(values)
}

/// Class ranges cover only their base rows; point, FETCH and scoped reads stay
/// exact.
#[test]
fn live_class_dependency_ranges_preserve_external_reads() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut setup = store.begin();
	let class = setup
		.define_class(
			ClassSchema::new("item", AtomKind::Node, [ClassField::new("manager", Type::Record)])
				.assert_ok(),
		)
		.assert_ok();
	setup
		.define_class(
			ClassSchema::new("policy", AtomKind::Node, [ClassField::new("score", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
	let manager = setup.intern_property("manager").assert_ok();
	let score = setup.intern_property("score").assert_ok();
	let item: RecordId = "item:one".parse().assert_ok();
	let policy: RecordId = "policy:one".parse().assert_ok();
	setup.create_atom(policy.clone(), PropertyMap::from_iter([(score, 41.into())])).assert_ok();
	setup
		.create_atom(item.clone(), PropertyMap::from_iter([(manager, policy.clone().into())]))
		.assert_ok();
	let graph = setup.define_graph("scope", GraphKind::Graph).assert_ok();
	setup.attach_node(graph.id(), &item, None, None).assert_ok();
	setup.commit().assert_ok();
	let root = store.inner.current_root();
	let item_id = root
		.atom_by_record(&item)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("item missing"))
		.assert_ok()
		.id();
	let policy_id = root
		.atom_by_record(&policy)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("policy missing"))
		.assert_ok()
		.id();
	let evaluate = |source| {
		let observed = RefCell::new(BTreeSet::new());
		let result = evaluate_query(
			&root,
			&LiveQuery::new(source).with_fetch(vec![LivePath::field("manager")?]),
			&|_id| Err(FerrosGrynnError::invalid_argument("unexpected binding")),
			&LiveOptions::default(),
			std::env::temp_dir(),
			&|key| {
				observed.borrow_mut().insert(key);
			},
		)?;
		assert_eq!(observed.into_inner(), result.reads.iter().copied().collect());
		Ok::<_, FerrosGrynnError>(result)
	};
	let class_result = evaluate(LiveSource::Class(class)).assert_ok();
	assert_eq!(class_result.rows.len(), 1);
	assert!(class_result.reads.contains(&ConflictKey::BaseClassRange(class)));
	assert!(!class_result.reads.contains(&ConflictKey::BaseAtom(item_id)));
	assert!(class_result.reads.contains(&ConflictKey::BaseAtom(policy_id)));
	assert!(class_result.reads.contains(&ConflictKey::SchemaClass(class)));
	assert!(class_result.reads.contains(&ConflictKey::PropertyDictionary));
	let point_result = evaluate(LiveSource::Record(item)).assert_ok();
	assert!(point_result.reads.contains(&ConflictKey::BaseAtom(item_id)));
	let scoped_result = evaluate(LiveSource::GraphClass {
		graph: graph.clone(),
		class,
	})
	.assert_ok();
	assert!(scoped_result.reads.contains(&ConflictKey::BaseAtom(item_id)));
	assert!(scoped_result.reads.contains(&ConflictKey::GraphClassRange(graph.id(), class)));
}

/// Concurrent changes to rejected rows still invalidate serializable
/// registration.
#[test]
fn live_class_range_registration_preserves_ssi() {
	use crate::{
		datastore::{IsolationLevel, TransactionAccess, TransactionOptions},
		test_assertions::AssertResult as _,
	};
	for mutation in 0..4 {
		let store = Datastore::in_memory();
		let mut setup = store.begin();
		let class = setup
			.define_class(
				ClassSchema::new("item", AtomKind::Node, [
					ClassField::new("keep", Type::Bool),
					ClassField::new("score", Type::Int),
				])
				.assert_ok(),
			)
			.assert_ok();
		let keep = setup.intern_property("keep").assert_ok();
		let score = setup.intern_property("score").assert_ok();
		let record: RecordId = "item:rejected".parse().assert_ok();
		setup
			.create_atom(
				record.clone(),
				PropertyMap::from_iter([(keep, false.into()), (score, 1.into())]),
			)
			.assert_ok();
		setup.commit().assert_ok();
		let mut registration = store.begin_with(TransactionOptions::new(
			IsolationLevel::Serializable,
			TransactionAccess::ReadWrite,
		));
		let id = registration
			.register_live(
				LiveQuery::new(LiveSource::Class(class))
					.with_filter(LiveExpr::field("keep").assert_ok()),
				LiveOptions::default(),
			)
			.assert_ok();
		let mut writer = store.begin();
		match mutation {
			0 => {
				writer.set_base_property(&record, score, 2.into()).assert_ok();
			}
			1 => {
				writer.remove_base_property(&record, score).assert_ok();
			}
			2 => {
				writer.delete_atom(&record).assert_ok();
			}
			_ => {
				writer
					.create_atom(
						"item:new".parse().assert_ok(),
						PropertyMap::from_iter([(keep, true.into()), (score, 3.into())]),
					)
					.assert_ok();
			}
		}
		writer.commit().assert_ok();
		let Err(error) = registration.commit() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("concurrent class change was accepted"));
				test_result.assert_ok();
			};
		};
		assert!(matches!(
			error.status().status(),
			crate::gql_status::Status::SerializationFailure { .. }
		));
		assert!(store.begin().live_info(id).is_err());
	}
}
