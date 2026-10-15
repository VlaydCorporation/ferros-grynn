//! Result windows share ordinary SELECT semantics and preserve source anchors.

use std::cell::{Cell, RefCell};

use super::*;
use crate::{
	datastore::{ClassField, ClassSchema, ConflictKey, Datastore},
	graph::{AtomKind, GraphKind},
	live::LiveIdentity,
	values::{PropertyMap, Type},
};

/// INNER predicates are retained through scalar constructors and outer
/// barriers.
#[test]
fn live_window_join_predicate_placement_is_exact() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["first", "second", "tag"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
		for id in ["a", "b"] {
			tx.create_atom(format!("{class}:{id}").parse().assert_ok(), PropertyMap::new())
				.assert_ok();
		}
	}
	tx.commit().assert_ok();
	for (from, count) in [
		("first AS a JOIN tag AS b ON false", 0),
		("first AS a JOIN tag AS b ON true", 4),
		("first AS a JOIN tag AS b ON a.id = b.id", 0),
		("first AS a JOIN tag AS b ON CASE WHEN a.id != b.id THEN true ELSE false END", 4),
		("first AS a JOIN tag AS b ON false LEFT JOIN second AS c ON true", 0),
		("first AS a LEFT JOIN tag AS b ON false JOIN second AS c ON false", 0),
		("first AS a LEFT JOIN tag AS b ON false", 2),
	] {
		let query = format!("SELECT VALUE 7 FROM {from};");
		assert_eq!(
			evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok().len(),
			count,
			"{query}"
		);
		let result = store.execute(&query, &Object::new()).assert_ok().remove(0).result.assert_ok();
		let Value::Array(values) = result.value() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("JOIN predicate oracle missing"));
				test_result.assert_ok();
			};
		};
		assert_eq!(values.len(), count, "ordinary {query}");
	}
}

/// Reversed physical order preserves logical slots and checked suffix decoding.
#[test]
fn live_window_join_reordered_sources_and_invalid_suffixes() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["first", "second"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
		for id in ["a", "b"] {
			tx.create_atom(format!("{class}:{id}").parse().assert_ok(), PropertyMap::new())
				.assert_ok();
		}
	}
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let syntax = "SELECT VALUE 7 FROM first AS a JOIN second AS b ON a.id != b.id ORDER BY a.id, \
	              b.id TEMPFILES;";
	let parsed = crate::gql::parse(syntax, crate::syn::ParserSettings::default()).assert_ok();
	let logical =
		crate::gql::analysis::analyze_with_runtime(&parsed, syntax, &read, &Object::new(), &cache)
			.assert_ok();
	let LogicalStatement::Select(plan) = &logical.statements[0] else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("CBO fixture SELECT missing"));
			test_result.assert_ok();
		};
	};
	let snapshot = read.snapshot().assert_ok();
	let (mut plan, mut physical) =
		prepare_physical_plan(&read, snapshot, plan, &control).assert_ok();
	physical.execution_order = vec![1, 0];
	physical.joins[0].right_source = 0;
	for (a, b) in &mut physical.joins[0].key_columns {
		std::mem::swap(a, b);
	}
	let sources = JoinedSources::prepare(&read, snapshot, &mut plan, &[], &control).assert_ok();
	let output = execute_select_output(
		&read,
		snapshot,
		&plan,
		&physical,
		&[],
		syntax,
		&control,
		None,
		Some(&sources),
	)
	.assert_ok();
	let mut actual = Vec::new();
	visit_records(&read, output, 0, usize::MAX, &control, |record| {
		actual.push((
			identity::joined_identity(&read, &record.anchor.stable_key, 2, 0)?,
			record.value,
		));
		Ok(())
	})
	.assert_ok();
	assert_eq!(actual, evaluate(&store, syntax, &Object::new(), &|_| {}).assert_ok());
	for suffix in [
		Value::None,
		Value::from(vec![Value::None]),
		Value::from(vec![
			Value::from(vec![
				Value::from(vec![Value::None, Value::None]),
				Value::from(vec![Value::None, Value::None]),
			]),
			Value::from(Vec::<Value>::new()),
		]),
	] {
		assert!(identity::joined_identity(&read, &[Value::from(0), suffix], 2, 0).is_err());
	}
}

/// A dynamic primary scope is evaluated once per class input, including LIMIT0.
#[test]
fn live_window_join_union_freezes_scope_once() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["first", "second", "tag"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
	}
	tx.define_class(
		ClassSchema::new("policy", AtomKind::Node, [ClassField::new("scope", Type::Any)])
			.assert_ok(),
	)
	.assert_ok();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let key = tx.intern_property("scope").assert_ok();
	let config: RecordId = "policy:a".parse().assert_ok();
	tx.create_atom(config.clone(), PropertyMap::from_iter([(key, Value::from(graph))])).assert_ok();
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	let atom = root
		.atom_by_record(&config)
		.ok_or_else(|| FerrosGrynnError::invalid_argument("scope fixture atom missing"))
		.assert_ok()
		.id();
	let parameters = Object::from_iter([("cfg", Value::from(config))]);
	for limit in ["", "LIMIT 0"] {
		let queries = ["first", "second"].map(|class| {
			format!(
				"SELECT VALUE 7 FROM {class} IN GRAPH $cfg.scope AS a CROSS JOIN tag AS b {limit};"
			)
		});
		let observed = Cell::new(0);
		let observe = |key| {
			if key == ConflictKey::BaseAtom(atom) {
				observed.set(observed.get() + 1);
			}
		};
		assert!(evaluate(&store, &queries[0], &parameters, &observe).assert_ok().is_empty());
		let once = observed.replace(0);
		assert!(once > 0);
		assert!(
			evaluate(&store, &queries.join("\n"), &parameters, &observe).assert_ok().is_empty()
		);
		assert_eq!(observed.get(), 2 * once, "scope evaluated twice: {limit}");
	}
}

/// Reordered INNER prefixes keep every predicate before a LEFT/CROSS boundary.
#[test]
fn live_window_join_inner_prefix_before_outer_keeps_predicates() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["first", "second", "third", "tail"] {
		tx.define_class(
			ClassSchema::new(class, AtomKind::Node, [ClassField::new("key", Type::Int)])
				.assert_ok(),
		)
		.assert_ok();
	}
	let key = tx.intern_property("key").assert_ok();
	for (id, value) in [
		("first:a", 1),
		("first:b", 2),
		("second:a", 1),
		("second:b", 2),
		("third:only", 1),
		("tail:a", 1),
		("tail:b", 1),
	] {
		tx.create_atom(id.parse().assert_ok(), PropertyMap::from_iter([(key, value.into())]))
			.assert_ok();
	}
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	for outer in ["LEFT JOIN tail AS d ON a.key = d.key", "CROSS JOIN tail AS d"] {
		let query = format!(
			"SELECT a.id AS lhs, b.id AS mid, c.id AS point, d.id AS rhs FROM first AS a JOIN \
			 second AS b ON a.key = b.key JOIN third:only AS c ON b.key = c.key {outer} ORDER BY \
			 a.id,d.id;"
		);
		let parsed = crate::gql::parse(&query, crate::syn::ParserSettings::default()).assert_ok();
		let logical = crate::gql::analysis::analyze_with_runtime(
			&parsed,
			&query,
			&read,
			&Object::new(),
			&cache,
		)
		.assert_ok();
		let LogicalStatement::Select(plan) = &logical.statements[0] else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("INNER prefix fixture missing"));
				test_result.assert_ok();
			};
		};
		let (_plan, physical) =
			prepare_physical_plan(&read, read.snapshot().assert_ok(), plan, &control).assert_ok();
		assert_ne!(
			physical.execution_order[0], 0,
			"fixture must reorder prefix: {:?}",
			physical.execution_order
		);
		let result = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
		assert_eq!(
			result.len(),
			2,
			"predicate lost after prefix reorder: {:?}",
			physical.execution_order
		);
		let ordinary =
			store.execute(&query, &Object::new()).assert_ok().remove(0).result.assert_ok();
		let Value::Array(values) = ordinary.value() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("INNER prefix oracle missing"));
				test_result.assert_ok();
			};
		};
		assert_eq!(values.len(), 2);
	}
}

/// A zero-class graph axis keeps its universal schema and LEFT unmatched slot.
#[test]
fn live_window_join_empty_graph_axis_keeps_left_occurrences() {
	use crate::{live::LiveIdentityPart, test_assertions::AssertResult as _};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	let graph = tx.define_graph("empty_scope", GraphKind::Graph).assert_ok();
	tx.commit().assert_ok();
	for kind in ["LEFT", "CROSS"] {
		let query = if kind == "LEFT" {
			"SELECT VALUE 7 FROM item AS a LEFT JOIN item IN GRAPH $scope AS b ON true;"
		} else {
			"SELECT VALUE 7 FROM item AS a CROSS JOIN item IN GRAPH $scope AS b;"
		};
		let mut parsed =
			crate::gql::parse(query, crate::syn::ParserSettings::default()).assert_ok();
		let crate::syn::StatementKind::Select(select) = &mut parsed.statements[0].kind else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("empty graph JOIN fixture missing"));
				test_result.assert_ok();
			};
		};
		select.joins[0].source.kind = crate::syn::SelectSourceKind::Empty;
		let parameters = Object::from_iter([("scope", Value::from(graph.clone()))]);
		let rows = evaluate_input(&store, &parsed, &parameters, &|_| {}).assert_ok();
		if kind == "LEFT" {
			assert_eq!(rows, vec![(
				LiveIdentity::Composite(vec![
					LiveIdentityPart::Record("item:a".parse().assert_ok()),
					LiveIdentityPart::Unmatched
				]),
				Value::from(7)
			)]);
		} else {
			assert!(rows.is_empty());
		}
		assert!(
			evaluate_input(
				&store,
				&parsed,
				&Object::from_iter([("scope", Value::from("missing"))]),
				&|_| {}
			)
			.is_err()
		);
	}
}

thread_local! {
	/// Checks observed by a direct single-column SPLIT input evaluation.
	static SPLIT_CHECKS: Cell<u64> = const { Cell::new(0) };
	/// Cancellation starts at the first check after input evaluation.
	static SPLIT_CANCEL_AFTER: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// A deterministic test boundary, without sleeps or another thread.
fn split_cancel_check() -> FerrosGrynnResult<()> {
	let count = SPLIT_CHECKS.get().saturating_add(1);
	SPLIT_CHECKS.set(count);
	if count > SPLIT_CANCEL_AFTER.get() {
		Err(FerrosGrynnError::query_cancelled())
	} else {
		Ok(())
	}
}

/// Joined ownership is admitted even when result/input schemas omit its
/// payload.
#[test]
fn live_window_join_copy_admits_every_source_component() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let syntax = "SELECT VALUE 1 FROM item AS a CROSS JOIN item AS b;";
	let parsed = crate::gql::parse(syntax, crate::syn::ParserSettings::default()).assert_ok();
	let logical =
		crate::gql::analysis::analyze_with_runtime(&parsed, syntax, &read, &Object::new(), &cache)
			.assert_ok();
	let LogicalStatement::Select(plan) = &logical.statements[0] else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("JOIN copy fixture SELECT missing"));
			test_result.assert_ok();
		};
	};
	let options = crate::gql::QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(64 << 10)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("JOIN copy fixture budget zero"))
			.assert_ok(),
		..crate::gql::QueryExecutionOptions::default()
	};
	let control = ExecutionControl::new(&options, None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	admission::select_copy(&read, plan, 0).assert_ok();
	for case in ["alias", "schema", "scope", "predicate", "wildcard"] {
		let mut plan = plan.clone();
		let join = &mut plan.joins[0];
		let text = "x".repeat(256 << 10);
		let expression = TypedExpr {
			kind: TypedExprKind::Constant(Value::from(text.clone())),
			ty: Type::Any,
			span: crate::gql_status::TextSpan::new(0, 0),
		};
		match case {
			"alias" => join.source.alias = text.into_boxed_str(),
			"schema" => join.source.schema.columns[0].name = text.into_boxed_str(),
			"scope" => {
				let LogicalSelectSourceKind::Atoms {
					graph,
					..
				} = &mut join.source.kind
				else {
					return {
						let test_result: FerrosGrynnResult<()> = Err(
							FerrosGrynnError::invalid_argument("JOIN copy fixture source missing"),
						);
						test_result.assert_ok();
					};
				};
				*graph = Some(LogicalGraphReference::Expression(expression));
			}
			"predicate" => join.predicate = Some(expression),
			_ => join.source.wildcard.push(crate::gql::ir::LogicalWildcardField::Column {
				name: text.into_boxed_str(),
				index: 0,
			}),
		}
		let control = ExecutionControl::new(&options, None);
		let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
		assert!(
			admission::select_copy(&read, &plan, 0).is_err(),
			"JOIN ownership unchecked: {case}"
		);
	}
}

/// LEFT, parallel matches, aliases and SPLIT keep declaration-ordered
/// identities.
#[test]
fn live_window_join_admits_products_before_clone() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let left = Row::new(vec![Value::from("x".repeat(256 << 10)), Value::None]);
	let right = Row::new(vec![Value::None, Value::from(7)]);
	for work in [true, false] {
		let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
		let callback = |_| {
			if work {
				Err(FerrosGrynnError::traversal_resource_limit_exceeded("JOIN pair work", 0))
			} else {
				Ok(())
			}
		};
		let options = crate::gql::QueryExecutionOptions {
			barrier_memory_bytes: std::num::NonZeroUsize::new(64 << 10)
				.ok_or_else(|| FerrosGrynnError::invalid_argument("JOIN pair fixture budget zero"))
				.assert_ok(),
			..crate::gql::QueryExecutionOptions::default()
		};
		let narrow = ExecutionControl::new(&options, None);
		let read = RootRead::new(
			&root,
			if work {
				&control
			} else {
				&narrow
			},
			&|_| {},
			&callback,
			std::env::temp_dir(),
		);
		assert!(
			join::combine_rows(&read, &control, &left, &right, (1, 1)).is_err(),
			"pair copied without admission: work={work}"
		);
	}
}

/// LEFT, parallel matches, aliases and SPLIT keep declaration-ordered
/// identities.
#[test]
fn live_window_join_identity_matches_select_and_spill() {
	use crate::{live::LiveIdentityPart, test_assertions::AssertResult as _};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["item", "tag"] {
		tx.define_class(
			ClassSchema::new(class, AtomKind::Node, [
				ClassField::new("key", Type::Int),
				ClassField::new("tags", Type::Array(Box::new(Type::Int))),
			])
			.assert_ok(),
		)
		.assert_ok();
	}
	let key = tx.intern_property("key").assert_ok();
	let tags = tx.intern_property("tags").assert_ok();
	for (id, bucket, values) in [
		("item:a", 1, vec![Value::from(7), Value::from(7)]),
		("item:b", 2, vec![Value::from(9)]),
		("item:c", 1, Vec::new()),
		("tag:a", 1, Vec::new()),
		("tag:b", 1, Vec::new()),
	] {
		tx.create_atom(
			id.parse().assert_ok(),
			PropertyMap::from_iter([(key, bucket.into()), (tags, Value::from(values))]),
		)
		.assert_ok();
	}
	let left = tx.define_graph("left_scope", GraphKind::Graph).assert_ok();
	let right = tx.define_graph("right_scope", GraphKind::Graph).assert_ok();
	for id in ["item:a", "item:b"] {
		tx.attach_node(left.id(), &id.parse().assert_ok(), None, None).assert_ok();
	}
	for id in ["tag:a", "tag:b"] {
		tx.attach_node(right.id(), &id.parse().assert_ok(), None, None).assert_ok();
	}
	tx.set_overlay(left.id(), &"item:a".parse().assert_ok(), key, 2.into()).assert_ok();
	tx.set_overlay(right.id(), &"tag:a".parse().assert_ok(), key, 2.into()).assert_ok();
	tx.commit().assert_ok();
	let source = "FROM item AS a LEFT JOIN tag AS b ON a.properties.key = b.properties.key";
	let expected = [
		("item:a", Some("tag:a")),
		("item:a", Some("tag:b")),
		("item:b", None),
		("item:c", Some("tag:a")),
		("item:c", Some("tag:b")),
	]
	.into_iter()
	.map(|(left, right)| {
		Ok(LiveIdentity::Composite(vec![
			LiveIdentityPart::Record(left.parse()?),
			right
				.map(|id| id.parse().map(LiveIdentityPart::Record))
				.transpose()?
				.unwrap_or(LiveIdentityPart::Unmatched),
		]))
	})
	.collect::<FerrosGrynnResult<Vec<_>>>()
	.assert_ok();
	for (projection, clauses, splits) in [
		("VALUE 7", "ORDER BY a.id, b.id", false),
		("a.id, b.id", "OMIT id ORDER BY a.id, b.id", false),
		("VALUE 7", "SPLIT a.properties.tags ORDER BY a.id, b.id", true),
	] {
		for spill in ["", "TEMPFILES"] {
			let query = format!("SELECT {projection} {source} {clauses} {spill};");
			let actual = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
			let oracle =
				store.execute(&query, &Object::new()).assert_ok().remove(0).result.assert_ok();
			let Value::Array(values) = oracle.value() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("JOIN oracle rows missing"));
					test_result.assert_ok();
				};
			};
			assert_eq!(
				actual.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>(),
				values.iter().cloned().collect::<Vec<_>>(),
				"{query}"
			);
			assert_eq!(actual.len(), 5);
			if !splits {
				assert_eq!(
					actual.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
					expected,
					"{query}"
				);
			} else {
				assert!(actual.iter().all(|(id,_)| matches!(id, LiveIdentity::Composite(parts) if parts.len()==3 && matches!(parts.last(), Some(LiveIdentityPart::Split(_))))));
				assert_eq!(
					actual
						.iter()
						.map(|(id, _)| id)
						.collect::<std::collections::BTreeSet<_>>()
						.len(),
					5
				);
			}
		}
	}
	let query = "SELECT VALUE 7 FROM item IN GRAPH left_scope AS a JOIN tag IN GRAPH right_scope \
	             AS b ON a.properties.key = b.properties.key ORDER BY a.id TEMPFILES;";
	let actual = evaluate(&store, query, &Object::new(), &|_| {}).assert_ok();
	let oracle = store.execute(query, &Object::new()).assert_ok().remove(0).result.assert_ok();
	let Value::Array(oracle) = oracle.value() else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("qualified JOIN oracle missing"));
			test_result.assert_ok();
		};
	};
	assert_eq!(
		actual.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>(),
		oracle.iter().cloned().collect::<Vec<_>>()
	);
	let fields =
		query.replace("VALUE 7", "a.id AS lhs, a.key AS left_key, b.id AS rhs, b.key AS right_key");
	assert_eq!(
		actual.len(),
		2,
		"scoped fields {:?}",
		evaluate(&store, &fields, &Object::new(), &|_| {}).assert_ok()
	);
	assert!(actual.iter().all(|(id,_)| matches!(id, LiveIdentity::Composite(parts) if matches!(parts.as_slice(), [LiveIdentityPart::Occurrence(a),LiveIdentityPart::Occurrence(b)] if a.graph().id()==left.id() && b.graph().id()==right.id()))));
	let renamed = query
		.replace(" AS a", " AS x")
		.replace(" AS b", " AS y")
		.replace("a.", "x.")
		.replace("b.", "y.");
	assert_eq!(actual, evaluate(&store, &renamed, &Object::new(), &|_| {}).assert_ok());
}

/// Each class's joined rows feed the same global window and aggregate
/// reduction.
#[test]
fn live_window_join_union_preserves_rows_groups_and_zero_scopes() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["first", "second", "tag"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
		for id in ["a", "b"] {
			tx.create_atom(format!("{class}:{id}").parse().assert_ok(), PropertyMap::new())
				.assert_ok();
		}
	}
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	tx.commit().assert_ok();
	for clauses in ["", "ORDER BY a.id, b.id START AT 1 LIMIT 4"] {
		let queries = ["first", "second"].map(|class| {
			format!(
				"SELECT a.id AS lhs, b.id AS rhs FROM {class} AS a CROSS JOIN tag AS b {clauses};"
			)
		});
		let actual = evaluate(&store, &queries.join("\n"), &Object::new(), &|_| {}).assert_ok();
		let mut expected = Vec::new();
		for query in &queries {
			let query = query.replace(clauses, "");
			let result =
				store.execute(&query, &Object::new()).assert_ok().remove(0).result.assert_ok();
			let Value::Array(values) = result.value() else {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("JOIN union oracle missing"));
					test_result.assert_ok();
				};
			};
			expected.extend(values.iter().cloned());
		}
		if !clauses.is_empty() {
			expected = expected.into_iter().skip(1).take(4).collect();
		}
		assert_eq!(actual.into_iter().map(|(_, value)| value).collect::<Vec<_>>(), expected);
	}
	let grouped = "SELECT VALUE count() FROM first AS a CROSS JOIN tag AS b GROUP ALL; SELECT \
	               VALUE count() FROM second AS a CROSS JOIN tag AS b GROUP ALL;";
	assert_eq!(evaluate(&store, grouped, &Object::new(), &|_| {}).assert_ok(), vec![(
		LiveIdentity::Aggregate(Vec::new()),
		Value::from(8)
	)]);
	for query in [
		"SELECT VALUE 7 FROM first AS a CROSS JOIN tag IN GRAPH $scope AS b LIMIT 0;",
		"SELECT VALUE count() FROM first AS a CROSS JOIN tag IN GRAPH $scope AS b GROUP ALL LIMIT \
		 0;",
	] {
		let reads = RefCell::new(Vec::new());
		assert!(
			evaluate(
				&store,
				query,
				&Object::from_iter([("scope", Value::from(graph.clone()))]),
				&|key| reads.borrow_mut().push(key)
			)
			.assert_ok()
			.is_empty()
		);
		assert!(reads.borrow().contains(&ConflictKey::GraphDefinition(graph.id())));
		assert!(!reads.borrow().iter().any(|key| matches!(key, ConflictKey::BaseAtom(_))));
		for scope in [Value::Null, Value::from("missing"), Value::from(3)] {
			assert!(
				evaluate(&store, query, &Object::from_iter([("scope", scope)]), &|_| {}).is_err()
			);
		}
	}
}

/// The shared expansion observes cancellation and work immediately after its
/// input evaluation; unrelated payloads cannot trigger unchecked clone/spill.
#[test]
fn live_window_split_admits_before_expansion() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let row = Row::new(vec![
		Value::Array(vec![Value::from(1); 8].into()),
		Value::from("x".repeat(256 << 10)),
	]);
	let expression = TypedExpr {
		kind: TypedExprKind::Column(0),
		ty: Type::Any,
		span: crate::gql_status::TextSpan::new(0, 0),
	};
	let work = Cell::new(0_u64);
	let observe_work = |amount| {
		work.set(work.get() + amount);
		Ok(())
	};
	let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None)
		.with_cooperative_check(split_cancel_check);
	let read = RootRead::new(&root, &control, &|_| {}, &observe_work, std::env::temp_dir());
	let snapshot = read.snapshot().assert_ok();
	SPLIT_CHECKS.set(0);
	SPLIT_CANCEL_AFTER.set(u64::MAX);
	let _input =
		eval_select_expr(&read, snapshot, &expression, &row, None, &[], "", &control).assert_ok();
	let baseline_checks = SPLIT_CHECKS.get();
	let baseline_work = work.get();
	assert!(baseline_checks > 0);
	for cancel in [true, false] {
		let consumed = Cell::new(0_u64);
		let bounded_work = |amount| {
			consumed.set(consumed.get() + amount);
			if !cancel && consumed.get() > baseline_work {
				Err(FerrosGrynnError::traversal_resource_limit_exceeded(
					"SPLIT work",
					baseline_work,
				))
			} else {
				Ok(())
			}
		};
		let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None)
			.with_cooperative_check(split_cancel_check);
		let read = RootRead::new(&root, &control, &|_| {}, &bounded_work, std::env::temp_dir());
		let snapshot = read.snapshot().assert_ok();
		SPLIT_CHECKS.set(0);
		SPLIT_CANCEL_AFTER.set(if cancel {
			baseline_checks
		} else {
			u64::MAX
		});
		let result = split_rows(
			&read,
			snapshot,
			RowSet::Memory {
				rows: vec![row.clone()],
				_account: control.barrier_account(),
			},
			&expression,
			&[],
			"",
			&control,
		);
		SPLIT_CANCEL_AFTER.set(u64::MAX);
		assert!(result.is_err(), "expansion must stop before copying: cancellation={cancel}");
	}
	let options = crate::gql::QueryExecutionOptions {
		barrier_memory_bytes: std::num::NonZeroUsize::new(64 << 10)
			.ok_or_else(|| FerrosGrynnError::invalid_argument("SPLIT scratch fixture"))
			.assert_ok(),
		..crate::gql::QueryExecutionOptions::default()
	};
	let control = ExecutionControl::new(&options, None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	assert!(
		split_rows(
			&read,
			read.snapshot().assert_ok(),
			RowSet::Memory {
				rows: vec![row],
				_account: control.barrier_account()
			},
			&expression,
			&[],
			"",
			&control
		)
		.is_err(),
		"large unrelated payload must be admitted before clone or spill"
	);
}

/// Source payload duplicates retain different positions, including compound
/// SPLIT and forced spill; projection/OMIT does not replace provenance.
#[test]
fn live_window_split_preserves_qualified_duplicate_positions() {
	use crate::{live::LiveIdentityPart, test_assertions::AssertResult as _};
	let store = Datastore::in_memory();
	for result in store
		.execute(
			"DEFINE GRAPH split_scope; DEFINE CLASS item TYPE NODE SCHEMALESS; CREATE item:a SET \
			 tags = [7, 7], left = [1, 1], right = ['x', 'y'], bad = 1; ATTACH item:a TO GRAPH \
			 split_scope;",
			&Object::new(),
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	for scope in ["", "IN GRAPH split_scope"] {
		for spill in ["", "TEMPFILES"] {
			for (clauses, positions) in [
				("SPLIT properties.tags", vec![vec![0], vec![1]]),
				("SPLIT properties.left, properties.right", vec![
					vec![0, 0],
					vec![0, 1],
					vec![1, 0],
					vec![1, 1],
				]),
			] {
				let query = format!("SELECT VALUE 7 FROM item:a {scope} {clauses} {spill};");
				let actual = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
				let ordinary = store
					.execute(&query, &Object::new())
					.assert_ok()
					.remove(0)
					.result
					.assert_ok()
					.value()
					.clone();
				let Value::Array(oracle) = ordinary else {
					return {
						let test_result: FerrosGrynnResult<()> =
							Err(FerrosGrynnError::invalid_argument("SPLIT oracle array absent"));
						test_result.assert_ok();
					};
				};
				assert_eq!(
					actual.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>(),
					oracle.into_iter().collect::<Vec<_>>()
				);
				assert_eq!(actual.len(), positions.len());
				for ((identity, _), positions) in actual.iter().zip(positions) {
					let LiveIdentity::Composite(parts) = identity else {
						return {
							let test_result: FerrosGrynnResult<()> = Err(
								FerrosGrynnError::invalid_argument("SPLIT identity not composite"),
							);
							test_result.assert_ok();
						};
					};
					assert_eq!(parts.len(), positions.len() + 1);
					assert!(match &parts[0] {
						LiveIdentityPart::Record(record) =>
							scope.is_empty() && record == &"item:a".parse().assert_ok(),
						LiveIdentityPart::Occurrence(occurrence) =>
							!scope.is_empty()
								&& occurrence.atom().id() == &"item:a".parse().assert_ok(),
						_ => false,
					});
					assert_eq!(
						&parts[1..],
						positions
							.into_iter()
							.map(LiveIdentityPart::Split)
							.collect::<Vec<_>>()
							.as_slice()
					);
				}
			}
		}
	}
	let omitted = evaluate(
		&store,
		"SELECT * FROM item:a SPLIT properties.tags OMIT id;",
		&Object::new(),
		&|_| {},
	)
	.assert_ok();
	assert_eq!(omitted.len(), 2);
	assert!(omitted.iter().all(|(identity, _)| matches!(identity, LiveIdentity::Composite(_))));
	for query in [
		"SELECT VALUE 1 FROM item:a SPLIT properties.bad;",
		"SELECT ONLY VALUE 1 FROM item:a SPLIT properties.tags;",
	] {
		assert!(evaluate(&store, query, &Object::new(), &|_| {}).is_err());
		assert!(store.execute(query, &Object::new()).assert_ok().remove(0).result.is_err());
	}
}

/// WHERE precedes SPLIT, each class expands once, then one group/window
/// operates over the complete qualified domain.
#[test]
fn live_window_split_union_filters_groups_and_windows() {
	use crate::{live::LiveIdentityPart, test_assertions::AssertResult as _};
	let store = Datastore::in_memory();
	for result in store
		.execute(
			"DEFINE GRAPH split_scope; DEFINE CLASS first TYPE NODE SCHEMALESS; DEFINE CLASS \
			 second TYPE NODE SCHEMALESS; CREATE first:a SET tags = [7, 7]; CREATE second:a SET \
			 tags = [8]; ATTACH first:a TO GRAPH split_scope; ATTACH second:a TO GRAPH \
			 split_scope;",
			&Object::new(),
		)
		.assert_ok()
	{
		result.result.assert_ok();
	}
	for (window, expected) in
		[("", vec![7, 7, 8]), ("START AT 1 LIMIT 2", vec![7, 8]), ("START AT 2 LIMIT 1", vec![8])]
	{
		for spill in ["", "TEMPFILES"] {
			for order in ["", "ORDER BY properties.tags"] {
				let query = ["first", "second"]
					.map(|class| {
						format!(
							"SELECT VALUE properties.tags FROM {class} IN GRAPH split_scope SPLIT \
							 properties.tags {order} {window} {spill};"
						)
					})
					.join("\n");
				let rows = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
				assert_eq!(
					rows.iter().map(|(_, value)| value.clone()).collect::<Vec<_>>(),
					expected.iter().copied().map(Value::from).collect::<Vec<_>>(),
					"{query}"
				);
				assert!(rows.iter().all(|(identity,_)| matches!(identity,LiveIdentity::Composite(parts) if matches!(parts.first(),Some(LiveIdentityPart::Occurrence(_))))));
			}
		}
	}
	for (filter, expected) in [("", 3), ("WHERE array::len(properties.tags) = 2", 2)] {
		let query = ["first", "second"]
			.map(|class| {
				format!(
					"SELECT VALUE count() FROM {class} IN GRAPH split_scope {filter} SPLIT \
					 properties.tags GROUP ALL;"
				)
			})
			.join("\n");
		assert_eq!(evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok(), vec![(
			LiveIdentity::Aggregate(Vec::new()),
			Value::from(expected)
		)]);
	}
	let only = ["first", "second"]
		.map(|class| {
			format!(
				"SELECT ONLY VALUE properties.tags FROM {class} IN GRAPH split_scope SPLIT \
				 properties.tags START AT 2 LIMIT 1;"
			)
		})
		.join("\n");
	assert_eq!(evaluate(&store, &only, &Object::new(), &|_| {}).assert_ok().len(), 1);
	let invalid = "SELECT VALUE 1 FROM first SPLIT properties.tags; SELECT VALUE 1 FROM second;";
	assert!(evaluate(&store, invalid, &Object::new(), &|_| {}).is_err());
}

/// Runs actual analyzed SELECT plans with independent read observation.
fn evaluate(
	store: &Datastore,
	syntax: &str,
	parameters: &Object,
	observe: &dyn Fn(ConflictKey),
) -> FerrosGrynnResult<Vec<(LiveIdentity, Value)>> {
	let parsed = crate::gql::parse(syntax, crate::syn::ParserSettings::default())?;
	evaluate_input(store, &parsed, parameters, observe)
}

/// Empty source fixtures share the same analyzed execution path.
fn evaluate_input(
	store: &Datastore,
	parsed: &crate::syn::Program,
	parameters: &Object,
	observe: &dyn Fn(ConflictKey),
) -> FerrosGrynnResult<Vec<(LiveIdentity, Value)>> {
	let root = store.inner.current_root();
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let read = RootRead::new(&root, &control, observe, &|_| Ok(()), std::env::temp_dir());
	let logical =
		crate::gql::analysis::analyze_with_runtime(parsed, "", &read, parameters, &cache)?;
	let slots = initialize_slots(&logical, parameters);
	let mut result = Vec::new();
	let mut collect = |identity, value| {
		result.push((identity, value));
		Ok(())
	};
	let plans = logical
		.statements
		.iter()
		.map(|statement| match statement {
			LogicalStatement::Select(plan) => Ok(plan),
			_ => Err(FerrosGrynnError::invalid_argument("window fixture SELECT missing")),
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()?;
	if plans.len() == 1 {
		visit_root_select(&read, plans[0], &slots, &control, &mut collect)?;
	} else if plans[0].group.is_some() {
		visit_root_aggregate_union(&read, &plans, &slots, &control, &mut collect)?;
	} else {
		visit_root_row_union(&read, &plans, &slots, &control, &mut collect)?;
	}
	Ok(result)
}

/// Streaming, sorting and grouping all apply the same window as SELECT.
#[test]
fn live_window_matches_select_and_keeps_identities() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(
		ClassSchema::new("item", AtomKind::Node, [ClassField::new("score", Type::Int)]).assert_ok(),
	)
	.assert_ok();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	let score = tx.intern_property("score").assert_ok();
	for (key, value) in
		[("a", 9_007_199_254_740_993_i64), ("b", 3), ("c", 9_007_199_254_740_992), ("d", 3)]
	{
		let record: RecordId = format!("item:{key}").parse().assert_ok();
		tx.create_atom(record.clone(), PropertyMap::from_iter([(score, Value::from(value))]))
			.assert_ok();
		tx.attach_node(graph.id(), &record, None, None).assert_ok();
	}
	tx.commit().assert_ok();
	for query in [
		"SELECT id, properties.score AS score FROM item START AT 1 LIMIT 2;",
		"SELECT id, properties.score AS score FROM item ORDER BY score DESC START AT 1 LIMIT 2;",
		"SELECT id, properties.score AS score FROM item ORDER BY score DESC START AT 1 LIMIT 2 \
		 TEMPFILES;",
		"SELECT id, properties.score AS score FROM item IN GRAPH scope OMIT id ORDER BY score \
		 DESC START AT 1 LIMIT 2;",
		"SELECT VALUE count() FROM item GROUP ALL LIMIT 0;",
		"SELECT VALUE count() FROM item GROUP ALL START AT 1 LIMIT 1;",
		"SELECT properties.score AS score, count() AS n FROM item GROUP BY properties.score ORDER \
		 BY score DESC START AT 1 LIMIT 2;",
	] {
		let expected = store
			.execute(query, &Object::new())
			.assert_ok()
			.remove(0)
			.result
			.assert_ok()
			.value()
			.clone();
		let actual = evaluate(&store, query, &Object::new(), &|_| {}).assert_ok();
		let values: crate::values::Array = actual.iter().map(|(_, value)| value.clone()).collect();
		assert_eq!(Value::Array(values), expected, "{query}");
		if query.contains("OMIT") {
			assert_eq!(actual.len(), 2);
			assert!(actual.iter().all(|(id, value)| matches!(id, LiveIdentity::Occurrence(reference) if reference.graph().id() == graph.id())
				&& matches!(value, Value::Object(object) if object.get("id").is_none())));
		}
	}
}

/// Bad windows fail without observing any record, even for grouped sources.
#[test]
fn live_window_rejects_bounds_before_row_pull() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	tx.commit().assert_ok();
	for grouped in [false, true] {
		for bound in ["LIMIT $bound", "START AT $bound LIMIT 0"] {
			let query = if grouped {
				format!("SELECT VALUE count() FROM item GROUP ALL {bound};")
			} else {
				format!("SELECT * FROM item {bound};")
			};
			for empty in [false, true] {
				let mut input =
					crate::gql::parse(&query, crate::syn::ParserSettings::default()).assert_ok();
				if empty {
					let crate::syn::StatementKind::Select(select) = &mut input.statements[0].kind
					else {
						return {
							let test_result: FerrosGrynnResult<()> = Err(
								FerrosGrynnError::invalid_argument("empty window SELECT missing"),
							);
							test_result.assert_ok();
						};
					};
					select.source.kind = crate::syn::SelectSourceKind::Empty;
				}
				for value in [
					Value::from(-1_i64),
					Value::from(0.5_f64),
					Value::from("wrong"),
					Value::from(f64::MAX),
				] {
					let reads = RefCell::new(Vec::new());
					let result = evaluate_input(
						&store,
						&input,
						&Object::from_iter([("bound", value)]),
						&|key| reads.borrow_mut().push(key),
					);
					assert!(result.is_err(), "{query}");
					assert!(
						!reads.borrow().iter().any(|key| matches!(key, ConflictKey::BaseAtom(_)))
					);
				}
			}
		}
	}
}

/// LIMIT zero observes a valid scope and rejects absent or foreign scopes.
#[test]
fn live_window_zero_validates_nonempty_scope() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	let graph = tx.define_graph("scope", GraphKind::Graph).assert_ok();
	tx.create_atom("item:a".parse().assert_ok(), PropertyMap::new()).assert_ok();
	tx.attach_node(graph.id(), &"item:a".parse().assert_ok(), None, None).assert_ok();
	tx.commit().assert_ok();
	for query in [
		"SELECT * FROM item IN GRAPH $scope LIMIT 0;",
		"SELECT VALUE count() FROM item IN GRAPH $scope GROUP ALL LIMIT 0;",
	] {
		let reads = RefCell::new(Vec::new());
		let result = evaluate(
			&store,
			query,
			&Object::from_iter([("scope", Value::from(graph.clone()))]),
			&|key| reads.borrow_mut().push(key),
		)
		.assert_ok();
		assert!(result.is_empty());
		assert!(reads.borrow().contains(&ConflictKey::GraphDefinition(graph.id())));
		assert!(!reads.borrow().iter().any(|key| matches!(key, ConflictKey::BaseAtom(_))));
		for scope in [Value::Null, Value::from("missing"), Value::from(3_i64)] {
			assert!(
				evaluate(&store, query, &Object::from_iter([("scope", scope)]), &|_| {}).is_err()
			);
		}
	}
}

/// Class partitions contribute to one aggregate before its window is applied.
#[test]
fn live_window_union_has_one_aggregate_window() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	for class in ["first", "second"] {
		tx.define_class(ClassSchema::new(class, AtomKind::Node, []).assert_ok()).assert_ok();
		tx.create_atom(format!("{class}:a").parse().assert_ok(), PropertyMap::new()).assert_ok();
		tx.create_atom(format!("{class}:b").parse().assert_ok(), PropertyMap::new()).assert_ok();
	}
	tx.commit().assert_ok();
	for (window, expected) in [
		("LIMIT 1", vec![(LiveIdentity::Aggregate(Vec::new()), Value::from(4))]),
		("LIMIT 0", Vec::new()),
		("START AT 1 LIMIT 1", Vec::new()),
	] {
		let query = format!(
			"SELECT VALUE count() FROM first GROUP ALL {window}; SELECT VALUE count() FROM second \
			 GROUP ALL {window};"
		);
		assert_eq!(evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok(), expected);
	}
}

/// START's owned expression is admitted before any optimizer plan clone.
#[test]
fn live_window_row_union_orders_globally_with_wildcards() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	for (projection, omit, order) in
		[("id, properties.score AS score", "", "score"), ("*", "OMIT id", "properties.score")]
	{
		for spill in ["", "TEMPFILES"] {
			let query = ["first", "second"]
				.map(|class| {
					format!(
						"SELECT {projection} FROM {class} IN GRAPH scope {omit} ORDER BY {order} \
						 DESC START AT 1 LIMIT 2 {spill};"
					)
				})
				.join("\n");
			let rows = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
			assert_eq!(rows.len(), 2, "{query}");
			for ((identity, value), record) in rows.iter().zip(["second:a", "first:b"]) {
				let LiveIdentity::Occurrence(occurrence) = identity else {
					return {
						let test_result: FerrosGrynnResult<()> = Err(
							FerrosGrynnError::invalid_argument("union qualified identity absent"),
						);
						test_result.assert_ok();
					};
				};
				assert_eq!(occurrence.atom().id(), &record.parse::<RecordId>().assert_ok());
				let expected = store
					.execute(
						&format!("SELECT {projection} FROM {record} IN GRAPH scope {omit};"),
						&Object::new(),
					)
					.assert_ok()
					.remove(0)
					.result
					.assert_ok()
					.value()
					.clone();
				let Value::Array(expected) = expected else {
					return {
						let test_result: FerrosGrynnResult<()> =
							Err(FerrosGrynnError::invalid_argument("union oracle array absent"));
						test_result.assert_ok();
					};
				};
				assert_eq!(expected.as_slice(), std::slice::from_ref(value));
			}
		}
	}
}

/// Unordered limits do not evaluate projection/FETCH beyond the selected
/// prefix.
#[test]
fn live_window_unordered_union_stops_projection_at_limit() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	let query = ["first", "second"]
		.map(|class| {
			format!(
				"SELECT VALUE 1 / (properties.score - 9007199254740992) FROM {class} IN GRAPH \
				 scope LIMIT 1;"
			)
		})
		.join("\n");
	let rows = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
	assert_eq!(rows.len(), 1);
	assert_eq!(rows[0].1, Value::from(1));
	let LiveIdentity::Occurrence(occurrence) = &rows[0].0 else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("unordered union identity missing"));
			test_result.assert_ok();
		};
	};
	assert_eq!(occurrence.atom().id(), &"first:a".parse::<RecordId>().assert_ok());
}

/// Nested maximum is applied after the global START rather than within a class.
#[test]
fn live_window_nested_union_caps_after_global_start() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	let root = store.inner.current_root();
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let query = ["first", "second"]
		.map(|class| {
			format!(
				"SELECT VALUE id FROM {class} IN GRAPH scope ORDER BY properties.score DESC START \
				 AT 1 LIMIT 3;"
			)
		})
		.join("\n");
	let syntax = crate::gql::parse(&query, crate::syn::ParserSettings::default()).assert_ok();
	let logical =
		crate::gql::analysis::analyze_with_runtime(&syntax, "", &read, &Object::new(), &cache)
			.assert_ok();
	let slots = initialize_slots(&logical, &Object::new());
	let inputs = logical
		.statements
		.iter()
		.map(|statement| {
			let LogicalStatement::Select(plan) = statement else {
				return Err(FerrosGrynnError::invalid_argument("nested union SELECT missing"));
			};
			admission::select_copy(&read, plan, 0)?;
			Ok(plan.clone())
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()
		.assert_ok();
	admission::select_copy(&read, &inputs[0], 0).assert_ok();
	let mut wrapper = inputs[0].clone();
	wrapper.source.kind = LogicalSelectSourceKind::Empty {
		graph: None,
	};
	wrapper.union_inputs = inputs;
	for (maximum, expected) in
		[(0, Vec::new()), (1, vec![Value::from("second:a".parse::<RecordId>().assert_ok())])]
	{
		let mut values = Vec::new();
		let count = visit_select_values_dyn(
			&read,
			read.snapshot().assert_ok(),
			&wrapper,
			&slots,
			"",
			&control,
			None,
			Some(maximum),
			&mut |value| {
				values.push(value);
				Ok(())
			},
		)
		.assert_ok();
		assert_eq!(count, expected.len());
		assert_eq!(values, expected);
	}
}

/// Group aliases are rebased and aggregate ORDER arguments keep class
/// overloads.
#[test]
fn live_window_group_union_order_alias_and_overloads() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	for (order, expected_key) in [
		("bucket DESC", 9_007_199_254_740_992_i64),
		("count() DESC, bucket ASC", 3),
		("sum(fn::score(v)) DESC, bucket ASC", 3),
	] {
		for spill in ["", "TEMPFILES"] {
			let query = ["first", "second"]
				.map(|class| {
					format!(
						"SELECT properties.score AS bucket, count() AS n FROM {class} IN GRAPH \
						 scope GROUP BY properties.score ORDER BY {order} START AT 1 LIMIT 1 \
						 {spill};"
					)
				})
				.join("\n");
			let rows = evaluate(&store, &query, &Object::new(), &|_| {}).assert_ok();
			assert_eq!(rows.len(), 1, "{query}");
			assert_eq!(rows[0].0, LiveIdentity::Aggregate(vec![Value::from(expected_key)]));
			assert_eq!(
				rows[0].1,
				Value::Object(Object::from_iter([
					("bucket", Value::from(expected_key)),
					("n", Value::from(1))
				]))
			);
		}
	}
}

/// Incompatible window/comparison descriptors fail without source row reads.
#[test]
fn live_window_row_union_mismatch_preflight() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	for second in ["ORDER BY id DESC LIMIT 1", "ORDER BY id LIMIT 2"] {
		let query = format!(
			"SELECT VALUE id FROM first IN GRAPH scope ORDER BY id LIMIT 1; SELECT VALUE id FROM \
			 second IN GRAPH scope {second};"
		);
		let reads = RefCell::new(Vec::new());
		assert!(
			evaluate(&store, &query, &Object::new(), &|key| reads.borrow_mut().push(key)).is_err()
		);
		assert!(!reads.borrow().iter().any(|key| matches!(key, ConflictKey::BaseAtom(_))));
	}
}

/// Two class widths expose alias rebasing and preserve strict function types.
fn union_fixture() -> FerrosGrynnResult<Datastore> {
	use crate::datastore::{FunctionEffect, SchemaFunction, SchemaFunctionParameter};
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	let graph = tx.define_graph("scope", GraphKind::Graph)?;
	let score = tx.intern_property("score")?;
	let value = tx.intern_property("v")?;
	let extra = tx.intern_property("extra")?;
	for (class, ty, v, contribution) in
		[("first", Type::Int, Value::from(1), 1), ("second", Type::Float, Value::from(1.0_f64), 10)]
	{
		let mut fields =
			vec![ClassField::new("score", Type::Int), ClassField::new("v", ty.clone())];
		if class == "second" {
			fields.insert(0, ClassField::new("extra", Type::String));
		}
		tx.define_class(ClassSchema::new(class, AtomKind::Node, fields)?)?;
		tx.define_function(SchemaFunction::new(
			"fn",
			"score",
			vec![SchemaFunctionParameter::new("v", ty)?],
			Type::Int,
			FunctionEffect::Pure,
			false,
			None,
			format!("{{ RETURN {contribution}; }}"),
		)?)?;
		for (key, number) in [
			("a", 9_007_199_254_740_993_i64),
			(
				"b",
				if class == "first" {
					9_007_199_254_740_992
				} else {
					3
				},
			),
		] {
			let record: RecordId = format!("{class}:{key}").parse()?;
			let mut properties =
				PropertyMap::from_iter([(score, Value::from(number)), (value, v.clone())]);
			if class == "second" {
				properties.insert(extra, Value::from("second"));
			}
			tx.create_atom(record.clone(), properties)?;
			tx.attach_node(graph.id(), &record, None, None)?;
		}
	}
	tx.commit()?;
	Ok(store)
}

/// START's owned expression is admitted before any optimizer plan clone.
#[test]
fn live_window_admission_checks_offset_and_cursor_payloads() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("item", AtomKind::Node, []).assert_ok()).assert_ok();
	tx.commit().assert_ok();
	let root = store.inner.current_root();
	for after in [false, true] {
		let cache = Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(
			16 << 20,
			100_000,
		));
		let control = ExecutionControl::with_runtime(
			&crate::gql::QueryExecutionOptions {
				barrier_memory_bytes: std::num::NonZeroUsize::new(128 << 10)
					.ok_or_else(|| FerrosGrynnError::invalid_argument("window budget missing"))
					.assert_ok(),
				..crate::gql::QueryExecutionOptions::default()
			},
			None,
			None,
			cache.clone(),
		);
		let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
		let parsed = crate::gql::parse(
			"SELECT * FROM item START AT 0;",
			crate::syn::ParserSettings::default(),
		)
		.assert_ok();
		let mut logical =
			crate::gql::analysis::analyze_with_runtime(&parsed, "", &read, &Object::new(), &cache)
				.assert_ok();
		let LogicalStatement::Select(plan) = &mut logical.statements[0] else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("window admission SELECT missing"));
				test_result.assert_ok();
			};
		};
		admission::select_copy(&read, plan, 0).assert_ok();
		let Some(LogicalSelectStart::Offset(mut expression)) = plan.start.take() else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("window admission START missing"));
				test_result.assert_ok();
			};
		};
		expression.kind = TypedExprKind::Constant(Value::from("x".repeat(256 << 10)));
		plan.start = Some(if after {
			LogicalSelectStart::After {
				cursor: expression,
				read_committed: false,
			}
		} else {
			LogicalSelectStart::Offset(expression)
		});
		assert!(admission::select_copy(&read, plan, 0).is_err());
	}
}

/// Root visitors preserve ordinary ONLY shape, cardinality and error
/// precedence.
#[test]
fn live_window_only_matches_select() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	for query in [
		"SELECT ONLY id AS key FROM first WHERE false;",
		"SELECT ONLY id AS key FROM first LIMIT 0;",
		"SELECT ONLY id AS key FROM first LIMIT 1;",
		"SELECT ONLY id AS key FROM first START AT 1 LIMIT 3;",
		"SELECT ONLY id AS key FROM first START AT 2;",
		"SELECT ONLY id AS key FROM first;",
		"SELECT ONLY id AS key FROM first ORDER BY key;",
		"SELECT ONLY * FROM first OMIT id LIMIT 1;",
		"SELECT ONLY VALUE count() FROM first GROUP ALL;",
		"SELECT ONLY properties.score AS bucket, count() AS n FROM first GROUP BY \
		 properties.score;",
		"SELECT ONLY properties.score AS bucket, count() AS n FROM first GROUP BY \
		 properties.score ORDER BY bucket DESC START AT 1 LIMIT 1;",
		"SELECT ONLY VALUE 1 / (properties.score - 9007199254740992) FROM first;",
		"SELECT ONLY VALUE 1 / (properties.score - 9007199254740992) FROM first ORDER BY \
		 properties.score;",
	] {
		let expected = store.execute(query, &Object::new()).assert_ok().remove(0).result;
		let actual = evaluate(&store, query, &Object::new(), &|_| {});
		match (expected, actual) {
			(Ok(expected), Ok(actual)) => {
				assert!(actual.len() <= 1, "{query}");
				let actual = actual.into_iter().next().map(|(_identity, value)| value);
				assert_eq!(actual.unwrap_or(Value::None), *expected.value(), "{query}");
			}
			(Err(expected), Err(actual)) => {
				assert_eq!(actual.status().code(), expected.status().code(), "{query}");
			}
			_ => {
				return {
					let test_result: FerrosGrynnResult<()> =
						Err(FerrosGrynnError::invalid_argument("ONLY differential mismatch"));
					test_result.assert_ok();
				};
			}
		}
	}
}

/// One global cardinality check follows cross-class paging and grouping.
#[test]
fn live_window_only_union_checks_after_global_offset() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	let mut tx = store.begin();
	let score = tx.intern_property("score").assert_ok();
	let v = tx.intern_property("v").assert_ok();
	let record: RecordId = "first:c".parse().assert_ok();
	tx.create_atom(
		record.clone(),
		PropertyMap::from_iter([(score, Value::from(7)), (v, Value::from(1))]),
	)
	.assert_ok();
	let graph = tx
		.named_graph("scope")
		.assert_ok()
		.ok_or_else(|| FerrosGrynnError::invalid_argument("ONLY fixture graph absent"))
		.assert_ok();
	tx.attach_node(graph.id(), &record, None, None).assert_ok();
	tx.commit().assert_ok();
	let query = |projection, clauses: &str| {
		["first", "second"]
			.map(|class| format!("SELECT ONLY {projection} FROM {class} IN GRAPH scope {clauses};"))
			.join("\n")
	};
	for (clauses, expected) in [
		("START AT 2 LIMIT 1", "first:c"),
		("START AT 3 LIMIT 1", "second:a"),
		("ORDER BY id START AT 2 LIMIT 1", "first:c"),
	] {
		let rows =
			evaluate(&store, &query("VALUE id", clauses), &Object::new(), &|_| {}).assert_ok();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].1, Value::from(expected.parse::<RecordId>().assert_ok()));
		assert!(matches!(&rows[0].0, LiveIdentity::Occurrence(occurrence)
			if occurrence.graph().id() == graph.id()));
	}
	for clauses in ["", "ORDER BY id", "START AT 2 LIMIT 2"] {
		let Err(error) = evaluate(&store, &query("VALUE id", clauses), &Object::new(), &|_| {})
		else {
			return {
				let test_result: FerrosGrynnResult<()> =
					Err(FerrosGrynnError::invalid_argument("ONLY union accepted multiple rows"));
				test_result.assert_ok();
			};
		};
		assert_eq!(error.status().code().to_string(), "FG-22F22");
	}
	let rows =
		evaluate(&store, &query("VALUE count()", "GROUP ALL"), &Object::new(), &|_| {}).assert_ok();
	assert_eq!(rows, vec![(LiveIdentity::Aggregate(Vec::new()), Value::from(5))]);
	let groups = query("properties.score AS bucket, count() AS n", "GROUP BY properties.score");
	assert!(evaluate(&store, &groups, &Object::new(), &|_| {}).is_err());
}

/// Nested union ONLY is global, and incompatible layouts fail before row reads.
#[test]
fn live_window_only_nested_union_and_preflight() {
	use crate::test_assertions::AssertResult as _;

	let store = union_fixture().assert_ok();
	let root = store.inner.current_root();
	let cache =
		Arc::new(crate::gql::closure::LanguageClosureState::bounded_with_work(16 << 20, 100_000));
	let control = ExecutionControl::with_runtime(
		&crate::gql::QueryExecutionOptions::default(),
		None,
		None,
		cache.clone(),
	);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let syntax = crate::gql::parse(
		"SELECT ONLY VALUE id FROM first IN GRAPH scope; SELECT ONLY VALUE id FROM second IN \
		 GRAPH scope;",
		crate::syn::ParserSettings::default(),
	)
	.assert_ok();
	let logical =
		crate::gql::analysis::analyze_with_runtime(&syntax, "", &read, &Object::new(), &cache)
			.assert_ok();
	let slots = initialize_slots(&logical, &Object::new());
	let inputs = logical
		.statements
		.iter()
		.map(|statement| match statement {
			LogicalStatement::Select(input) => Ok(input.clone()),
			_ => Err(FerrosGrynnError::invalid_argument("ONLY union input missing")),
		})
		.collect::<FerrosGrynnResult<Vec<_>>>()
		.assert_ok();
	let mut wrapper = inputs[0].clone();
	wrapper.source.kind = LogicalSelectSourceKind::Empty {
		graph: None,
	};
	wrapper.union_inputs = inputs;
	let mut emitted = 0_usize;
	let Err(error) = visit_select_values_dyn(
		&read,
		read.snapshot().assert_ok(),
		&wrapper,
		&slots,
		"",
		&control,
		None,
		None,
		&mut |_value| {
			emitted += 1;
			Ok(())
		},
	) else {
		return {
			let test_result: FerrosGrynnResult<()> =
				Err(FerrosGrynnError::invalid_argument("nested ONLY accepted multiple rows"));
			test_result.assert_ok();
		};
	};
	assert_eq!(error.status().code().to_string(), "FG-22F22");
	assert_eq!(emitted, 2);
	for second in ["SELECT VALUE id", "SELECT ONLY VALUE id"] {
		let suffix = if second.contains("ONLY") {
			"LIMIT -1"
		} else {
			""
		};
		let query = format!(
			"SELECT ONLY VALUE id FROM first IN GRAPH scope; {second} FROM second IN GRAPH scope \
			 {suffix};"
		);
		let reads = RefCell::new(Vec::new());
		assert!(
			evaluate(&store, &query, &Object::new(), &|key| reads.borrow_mut().push(key)).is_err()
		);
		assert!(!reads.borrow().iter().any(|key| matches!(key, ConflictKey::BaseAtom(_))));
	}
}

/// A skipped record is budgeted and cancellation cannot drain an entire spool.
#[test]
fn live_window_pager_preserves_anchors_and_checks_skips() {
	use crate::test_assertions::AssertResult as _;

	let store = Datastore::in_memory();
	let root = store.inner.current_root();
	let records = || SortedRecords::Memory {
		records: (0..4)
			.map(|value| CursorRecord {
				value: Value::from(value),
				anchor: CursorAnchor {
					order_keys: vec![Value::from(4 - value)],
					stable_key: vec![Value::from(value)],
				},
			})
			.collect(),
		account: BarrierAccount::new(0),
	};
	let control = ExecutionControl::new(&crate::gql::QueryExecutionOptions::default(), None);
	let read = RootRead::new(&root, &control, &|_| {}, &|_| Ok(()), std::env::temp_dir());
	let mut actual = Vec::new();
	assert_eq!(
		visit_records(&read, records(), 1, 2, &control, |record| {
			actual.push(record);
			Ok(())
		})
		.assert_ok(),
		2
	);
	assert_eq!(actual[0].anchor.stable_key, vec![Value::from(1)]);
	assert_eq!(actual[0].anchor.order_keys, vec![Value::from(3)]);
	for cancellation in [false, true] {
		let token = crate::gql::QueryCancellation::default();
		let control = ExecutionControl::new(
			&crate::gql::QueryExecutionOptions {
				cancellation: Some(token.clone()),
				..crate::gql::QueryExecutionOptions::default()
			},
			None,
		);
		let calls = Cell::new(0);
		let work = |_amount| {
			calls.set(calls.get() + 1);
			if cancellation {
				token.cancel();
				Ok(())
			} else if calls.get() > 1 {
				Err(FerrosGrynnError::traversal_resource_limit_exceeded("window work", 1))
			} else {
				Ok(())
			}
		};
		let read = RootRead::new(&root, &control, &|_| {}, &work, std::env::temp_dir());
		let mut visited = false;
		assert!(
			visit_records(&read, records(), 3, 1, &control, |_record| {
				visited = true;
				Ok(())
			})
			.is_err()
		);
		assert!(!visited);
		assert_eq!(
			calls.get(),
			if cancellation {
				1
			} else {
				2
			}
		);
	}
}
