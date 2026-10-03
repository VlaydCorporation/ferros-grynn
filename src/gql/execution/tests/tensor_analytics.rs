//! Stage-26 vertical slices retain the same qualified Rust kernel contracts.

use crate::{
	datastore::{ClassSchema, Datastore, GraphIndex, MaintenanceOptions, TensorCacheFormat},
	gql::QueryExecutionOptions,
	graph::{
		AtomKind,
		EdgeTopology,
		GraphKind,
		tensor::{GraphAlgorithmLimits, TensorScope},
	},
	values::{Number, Object, PropertyMap, RecordId, Value},
};

/// Builds a regular undirected triangle with an isolated vertex.
fn store() -> Datastore {
	let store = Datastore::in_memory();
	let mut tx = store.begin();
	tx.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
	tx.define_class(ClassSchema::new("e", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = tx.define_graph("analytics", GraphKind::Graph).unwrap();
	let mut nodes = Vec::new();
	for record in ["n:a", "n:b", "n:c", "n:isolated"] {
		let record: RecordId = record.parse().unwrap();
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
		nodes.push(tx.attach_node(graph.id(), &record, None, None).unwrap());
	}
	for (ordinal, (source, target)) in [(0, 1), (1, 2), (2, 0)].into_iter().enumerate() {
		let record: RecordId = format!("e:{ordinal}").parse().unwrap();
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
		tx.attach_edge(
			graph.id(),
			&record,
			Some("road".into()),
			EdgeTopology::undirected([nodes[source], nodes[target]]),
			Some(Number::Int(2)),
			None,
		)
		.unwrap();
	}
	tx.commit().unwrap();
	store
}

/// Extracts the single complete graph-wide report from a traversal result.
fn report(store: &Datastore, query: &str) -> Object {
	let result = store.execute(query, &Object::new()).unwrap();
	let value = result.last().unwrap().result.as_ref().unwrap().value();
	let values = value.as_array().unwrap();
	assert_eq!(values.len(), 1);
	values.iter().next().unwrap().as_object().unwrap().clone()
}

#[test]
fn tensor_analytics_all_steps_are_typed_complete_induced_barriers() {
	let store = store();
	let prefix = "TRAVERSE FROM [n:a, n:b, n:c, n:a] IN GRAPH @analytics |>";
	for method in [
		"degree",
		"pageRank",
		"harmonicCloseness",
		"betweenness",
		"eigenvector",
		"katz",
		"triangles",
		"clustering",
		"labelPropagation",
		"louvain",
	] {
		let value = report(&store, &format!("{prefix} tv::{method}();"));
		let values = value.get("values").unwrap().as_array().unwrap();
		assert_eq!(values.len(), 3, "{method}");
		for row in values.iter() {
			assert!(matches!(row.as_object().unwrap().get("atom"), Some(Value::AtomOccurrence(_))));
		}
		if method == "pageRank" {
			for row in values.iter() {
				let Some(Value::Number(Number::Float(score))) =
					row.as_object().unwrap().get("value")
				else {
					panic!("missing PageRank score");
				};
				assert!((*score - 1.0 / 3.0).abs() < 1.0e-12);
			}
		}
		if method == "degree" {
			for row in values.iter() {
				let degree = row.as_object().unwrap().get("value").unwrap().as_object().unwrap();
				assert_eq!(degree.get("total"), Some(&Value::from(2_u64)));
				assert_eq!(degree.get("total_strength"), Some(&Value::from(4_i64)));
			}
		}
		if method == "triangles" {
			assert_eq!(
				value.get("totals").unwrap().as_object().unwrap().get("undirected"),
				Some(&Value::from(1_u64))
			);
		}
	}
	let degree = report(&store, "TRAVERSE FROM [n:a, n:b] IN GRAPH @analytics |> tv::degree();");
	for row in degree.get("values").unwrap().as_array().unwrap().iter() {
		assert_eq!(
			row.as_object().unwrap().get("value").unwrap().as_object().unwrap().get("total"),
			Some(&Value::from(1_u64))
		);
	}
	for method in [
		"degree",
		"pageRank",
		"harmonicCloseness",
		"betweenness",
		"eigenvector",
		"katz",
		"triangles",
		"clustering",
		"labelPropagation",
		"louvain",
	] {
		let empty = report(&store, &format!("TRAVERSE FROM [] |> tv::{method}();"));
		assert!(empty.get("values").unwrap().as_array().unwrap().is_empty());
	}
}

#[test]
fn tensor_analytics_cache_parity_options_and_shared_limits_are_exact() {
	let store = store();
	let query = "TRAVERSE FROM [n:a, n:b, n:c] IN GRAPH @analytics |> \
	             tv::pageRank({weights:'STORED', direction:'BOTH', labels:['road']});";
	let before = report(&store, query);
	let mut tx = store.begin();
	let graph = tx.named_graph("analytics").unwrap().unwrap();
	let id = tx
		.define_graph_index(
			"analytics",
			GraphIndex::new_tensor("tensor", TensorScope::Local, TensorCacheFormat::Auto, 1 << 20)
				.unwrap(),
		)
		.unwrap();
	tx.commit().unwrap();
	store.rebuild_graph_index(graph.id(), id, MaintenanceOptions::default()).unwrap();
	let after = report(&store, query);
	assert_eq!(before.get("values"), after.get("values"));
	for query in [
		"TRAVERSE FROM [] |> tv::pageRank(7);",
		"TRAVERSE FROM [] |> tv::degree({damping:0.8});",
		"TRAVERSE FROM [] |> tv::triangles({orientation:'invalid'});",
		"TRAVERSE FROM [] |> tv::pageRank({max_iterations:0});",
	] {
		let result = store.execute(query, &Object::new());
		assert!(
			result.is_err() || result.unwrap().iter().any(|value| value.result.is_err()),
			"{query}"
		);
	}
	let options = QueryExecutionOptions {
		graph_algorithm_limits: GraphAlgorithmLimits {
			max_work: 1,
			..Default::default()
		},
		..Default::default()
	};
	assert!(store.execute_with_options(query, &Object::new(), options).unwrap()[0].result.is_err());
}

#[test]
fn tensor_analytics_unweighted_projection_ignores_stored_overflow() {
	let store = store();
	let mut tx = store.begin();
	let graph = tx.named_graph("analytics").unwrap().unwrap();
	let tensor = tx.endpoint_tensor(&graph, Default::default()).unwrap();
	let axes = tensor.atoms();
	let view = tx.read_graph(&graph).unwrap();
	use crate::graph::{GraphRead, GraphRefReader};
	let a = view
		.atom_id(axes.iter().find(|atom| atom.atom().id().to_string() == "n:a").unwrap().atom())
		.unwrap()
		.unwrap();
	let b = view
		.atom_id(axes.iter().find(|atom| atom.atom().id().to_string() == "n:b").unwrap().atom())
		.unwrap()
		.unwrap();
	drop(view);
	for record in ["e:large1", "e:large2"] {
		let record: RecordId = record.parse().unwrap();
		tx.create_atom(record.clone(), PropertyMap::new()).unwrap();
		tx.attach_edge(
			graph.id(),
			&record,
			None,
			EdgeTopology::undirected([a, b]),
			Some(Number::Int(i64::MAX)),
			None,
		)
		.unwrap();
	}
	tx.commit().unwrap();
	for method in ["pageRank", "triangles", "clustering", "labelPropagation", "louvain", "katz"] {
		let value = report(
			&store,
			&format!("TRAVERSE FROM [n:a,n:b,n:c] IN GRAPH @analytics |> tv::{method}();"),
		);
		assert_eq!(value.get("values").unwrap().as_array().unwrap().len(), 3);
		if method == "katz" {
			assert!(matches!(
				value.get("parameters").unwrap().as_object().unwrap().get("alpha"),
				Some(Value::Number(_))
			));
		}
	}
	for method in ["degree()", "pageRank({weights:'STORED'})"] {
		assert!(
			store
				.execute(
					&format!("TRAVERSE FROM [n:a,n:b] IN GRAPH @analytics |> tv::{method};"),
					&Object::new()
				)
				.unwrap()[0]
				.result
				.is_err()
		);
	}
}

#[test]
fn tensor_analytics_gql_tensor_cache_ddl_reuses_maintenance() {
	let store = store();
	for format in ["CSR", "DENSE", "AUTO"] {
		let query = format!(
			"DEFINE INDEX tensor_{format} ON GRAPH analytics TENSOR FORMAT {format} SCOPE LOCAL \
			 WITH (MAX_BYTES=1048576); REBUILD INDEX tensor_{format} ON GRAPH analytics; INFO FOR \
			 INDEX tensor_{format} ON GRAPH analytics;"
		);
		for outcome in store.execute(&query, &Object::new()).unwrap() {
			assert!(outcome.result.is_ok(), "{:?}", outcome.result);
		}
		let value =
			report(&store, "TRAVERSE FROM [n:a,n:b,n:c] IN GRAPH @analytics |> tv::pageRank();");
		assert_eq!(value.get("values").unwrap().as_array().unwrap().len(), 3);
	}
	for query in [
		"DEFINE INDEX missing ON GRAPH analytics TENSOR;",
		"DEFINE INDEX bad ON GRAPH analytics TENSOR FORMAT INVALID WITH (MAX_BYTES=1048576);",
		"DEFINE INDEX subset ON GRAPH analytics TENSOR MODE NODE WITH (MAX_BYTES=1048576);",
	] {
		let result = store.execute(query, &Object::new());
		assert!(result.is_err() || result.unwrap()[0].result.is_err());
	}
}

/// Returns physical JSON and actual counters for one tensor consumer.
fn explain(store: &Datastore, query: &str, analyze: bool) -> Object {
	let result = store
		.execute(
			&format!(
				"EXPLAIN {} FORMAT JSON {query}",
				if analyze {
					"ANALYZE"
				} else {
					"FULL"
				}
			),
			&Object::new(),
		)
		.unwrap();
	result[0].result.as_ref().unwrap().value().as_object().unwrap().clone()
}

#[test]
fn tensor_planner_hash_binding_format_and_actual_counters_match_execution() {
	let store = store();
	let one = "MATCH path=(n:a)-[e*1..2 SHORTEST]-(n:b) IN GRAPH analytics RETURN path;";
	let all = one.replace("SHORTEST", "ALL SHORTEST");
	let before = explain(&store, &all, true);
	assert_ne!(explain(&store, one, false).get("plan_hash"), before.get("plan_hash"));
	let metrics = before.get("analyze").unwrap().as_object().unwrap();
	assert_eq!(metrics.get("tensor_fallbacks"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("algorithm_paths"), Some(&Value::from(1_u64)));
	assert!(
		matches!(metrics.get("algorithm_work"), Some(Value::Number(value)) if u64::try_from(*value).unwrap() > 0)
	);
	let mut previous = before.get("plan_hash").unwrap().clone();
	for (name, format) in [("csr", TensorCacheFormat::Csr), ("dense", TensorCacheFormat::Dense)] {
		let mut tx = store.begin();
		let graph = tx.named_graph("analytics").unwrap().unwrap();
		let id = tx
			.define_graph_index(
				"analytics",
				GraphIndex::new_tensor(name, TensorScope::Local, format, 1 << 20).unwrap(),
			)
			.unwrap();
		tx.commit().unwrap();
		store.rebuild_graph_index(graph.id(), id, MaintenanceOptions::default()).unwrap();
		let explained = explain(&store, &all, true);
		assert_ne!(&previous, explained.get("plan_hash").unwrap());
		let node = explained
			.get("nodes")
			.unwrap()
			.as_array()
			.unwrap()
			.iter()
			.find_map(|value| {
				let node = value.as_object().unwrap();
				(node.get("operator") == Some(&Value::from("TensorAllShortest"))).then_some(node)
			})
			.unwrap();
		assert_eq!(
			node.get("format"),
			Some(&Value::from(if format == TensorCacheFormat::Csr {
				"CSR"
			} else {
				"DENSE"
			}))
		);
		assert_eq!(node.get("scope"), Some(&Value::from("LOCAL")));
		assert_eq!(node.get("index_id"), Some(&Value::from(id.get())));
		let metrics = explained.get("analyze").unwrap().as_object().unwrap();
		assert_eq!(metrics.get("tensor_cache_hits"), Some(&Value::from(1_u64)));
		assert_eq!(metrics.get("tensor_fallbacks"), Some(&Value::from(0_u64)));
		assert_eq!(metrics.get("tensor_nonzeros"), Some(&Value::from(12_u64)));
		let analytics = explain(
			&store,
			"TRAVERSE FROM [n:a,n:b,n:c] IN GRAPH @analytics |> tv::pageRank();",
			true,
		);
		let metrics = analytics.get("analyze").unwrap().as_object().unwrap();
		assert_eq!(metrics.get("tensor_cache_hits"), Some(&Value::from(1_u64)));
		assert!(
			matches!(metrics.get("tensor_iterations"), Some(Value::Number(value)) if u64::try_from(*value).unwrap() > 0)
		);
		let mut tx = store.begin();
		tx.remove_graph_index("analytics", name).unwrap();
		tx.commit().unwrap();
		previous = explained.get("plan_hash").unwrap().clone();
	}
	assert_ne!(
		explain(
			&store,
			"TRAVERSE FROM [n:a,n:b] IN GRAPH @analytics |> tv::degree({direction:'IN'});",
			false
		)
		.get("plan_hash"),
		explain(
			&store,
			"TRAVERSE FROM [n:a,n:b] IN GRAPH @analytics |> tv::degree({direction:'OUT'});",
			false
		)
		.get("plan_hash")
	);
}
