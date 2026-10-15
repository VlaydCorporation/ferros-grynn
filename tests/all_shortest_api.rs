#![cfg(test)]

//! Exact direct SDK path checks with independent bounded enumeration.
#![allow(clippy::unwrap_used, reason = "Assertions fail on every unexpected SDK error")]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use ferros_grynn::{
	graph::{
		EdgeTopology,
		Graph,
		Mutable,
		analytics::{AllShortestOptions, PathCost, PathMode},
		tensor::{
			EndpointTensor,
			GraphAlgorithmLimits,
			ProjectionOptions,
			TensorAtom,
			TensorOptions,
		},
	},
	values::{Number, PropertyMap},
};
use test_assertions::AssertOption as _;

/// Selects one canonical node axis by its public RecordId.
fn node(tensor: &EndpointTensor, ordinal: usize) -> TensorAtom {
	tensor
		.atoms()
		.iter()
		.find(|atom| atom.atom().id().to_string() == format!("n:{ordinal}"))
		.unwrap()
		.clone()
}

/// Verifies all shortest api matches exhaustive bounded trails.
#[test]
fn all_shortest_api_matches_exhaustive_bounded_trails() {
	for bits in 0..64_u64 {
		let mut graph = Graph::<Mutable>::new();
		let nodes: Vec<_> = (0..3)
			.map(|i| {
				graph
					.add_node_with_id(format!("n:{i}").parse().unwrap(), None, PropertyMap::new())
					.unwrap()
			})
			.collect();
		let mut edges = Vec::new();
		let mut bit = 0;
		for a in 0..3 {
			for b in 0..3 {
				if a == b {
					continue;
				}
				if bits & (1 << bit) != 0 {
					for parallel in 0..if a == 0 && b == 1 {
						2
					} else {
						1
					} {
						let weight = ((bits as usize + a + b + parallel) % 3) as i64;
						let name = format!("e:p{a}{b}{parallel}");
						let edge = graph
							.add_edge_with_id(
								name.parse().unwrap(),
								EdgeTopology::directed([nodes[a]], [nodes[b]]),
								None,
								PropertyMap::new(),
							)
							.unwrap();
						graph.set_edge_weight(edge, Some(Number::Int(weight))).unwrap();
						edges.push((a, b, weight, name));
					}
				}
				bit += 1;
			}
		}
		let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
		for weighted in [false, true] {
			let mut options = AllShortestOptions::default();
			options.max_depth = Some(4);
			if weighted {
				options.cost = PathCost::Stored;
			}
			let result = tensor
				.all_shortest_paths(
					&node(&tensor, 0),
					&node(&tensor, 2),
					ProjectionOptions::default(),
					options,
				)
				.unwrap()
				.collect_all()
				.unwrap();
			let mut oracle = Vec::new();
			walk(&edges, 0, 2, weighted, &mut Vec::new(), 0, &mut oracle);
			if let Some(best) = oracle.iter().map(|p| p.0).min() {
				oracle.retain(|p| p.0 == best);
			}
			oracle.sort();
			let mut actual: Vec<_> = result
				.iter()
				.map(|path| {
					(
						match path.cost() {
							Number::Int(n) => Some(n),
							_ => None,
						}
						.assert_some_with("integer oracle"),
						path.steps()
							.iter()
							.filter_map(|s| {
								s.via_edge.as_ref().map(|edge| edge.atom().id().to_string())
							})
							.collect::<Vec<_>>(),
					)
				})
				.collect();
			let canonical: Vec<_> = actual.iter().map(|p| &p.1).collect();
			assert!(canonical.windows(2).all(|pair| pair[0] < pair[1]));
			actual.sort();
			assert_eq!(actual, oracle, "bits={bits}, weighted={weighted}");
		}
	}
}

/// Independent exhaustive depth-four enumeration over original connectors.
fn walk(
	edges: &[(usize, usize, i64, String)],
	source: usize,
	target: usize,
	weighted: bool,
	hops: &mut Vec<usize>,
	cost: i64,
	output: &mut Vec<(i64, Vec<String>)>,
) {
	if source == target {
		output.push((cost, hops.iter().map(|at| edges[*at].3.clone()).collect()));
	}
	if hops.len() == 4 {
		return;
	}
	for (at, (from, to, weight, _)) in edges.iter().enumerate() {
		if *from == source && !hops.contains(&at) {
			hops.push(at);
			walk(
				edges,
				*to,
				target,
				weighted,
				hops,
				cost + if weighted {
					*weight
				} else {
					1
				},
				output,
			);
			hops.pop();
		}
	}
}

/// Verifies all shortest api zero cycles modes and exact prefix callbacks.
#[test]
fn all_shortest_api_zero_cycles_modes_and_exact_prefix_callbacks() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:0".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:1".parse().unwrap(), None, PropertyMap::new()).unwrap();
	for (name, source, target) in [("e:a", a, b), ("e:b", b, a), ("e:c", a, a)] {
		let edge = graph
			.add_edge_with_id(
				name.parse().unwrap(),
				EdgeTopology::directed([source], [target]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
		graph.set_edge_weight(edge, Some(Number::Int(0))).unwrap();
	}
	let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	let mut options = AllShortestOptions::default();
	options.cost = PathCost::Stored;
	let paths = tensor
		.all_shortest_paths(
			&node(&tensor, 0),
			&node(&tensor, 0),
			ProjectionOptions::default(),
			options,
		)
		.unwrap()
		.collect_all()
		.unwrap();
	assert_eq!(paths.len(), 5);
	assert!(paths.iter().all(|p| p.cost() == Number::Int(0)));
	options.mode = PathMode::Acyclic;
	assert_eq!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 0),
				ProjectionOptions::default(),
				options
			)
			.unwrap()
			.collect_all()
			.unwrap()
			.len(),
		1
	);
	options.mode = PathMode::Simple;
	assert_eq!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 0),
				ProjectionOptions::default(),
				options
			)
			.unwrap()
			.collect_all()
			.unwrap()
			.len(),
		3
	);
	options.mode = PathMode::Walk;
	assert!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 0),
				ProjectionOptions::default(),
				options
			)
			.is_err()
	);
	options.max_depth = Some(2);
	assert_eq!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 0),
				ProjectionOptions::default(),
				options
			)
			.unwrap()
			.collect_all()
			.unwrap()
			.len(),
		4
	);
	options.mode = PathMode::Trail;
	options.cost = PathCost::Custom;
	let mut prefixes = Vec::new();
	let result = tensor
		.all_shortest_paths_with_cost(
			&node(&tensor, 0),
			&node(&tensor, 1),
			ProjectionOptions::default(),
			options,
			&mut |context| {
				prefixes.push((context.prefix, context.hops.len()));
				Ok(if context.hops.is_empty() {
					Number::Int(0)
				} else {
					Number::Int(10)
				})
			},
			&mut || Ok(()),
		)
		.unwrap()
		.collect_all()
		.unwrap();
	assert_eq!(result.len(), 1);
	assert!(prefixes.iter().any(|(_, depth)| *depth == 1));
	assert_eq!(result[0].steps().len(), 2);
	assert!(result[0].materialize_path(GraphAlgorithmLimits::default()).is_err());
}

/// Verifies all shortest api filters charge work before minimum and capture
/// contributions.
#[test]
fn all_shortest_api_filters_charge_work_before_minimum_and_capture_contributions() {
	let mut graph = Graph::<Mutable>::new();
	let nodes: Vec<_> = (0..3)
		.map(|ordinal| {
			graph
				.add_node_with_id(format!("n:{ordinal}").parse().unwrap(), None, PropertyMap::new())
				.unwrap()
		})
		.collect();
	for (ordinal, (source, target)) in [(0, 2), (0, 1), (1, 2)].into_iter().enumerate() {
		graph
			.add_edge_with_id(
				format!("e:{ordinal}").parse().unwrap(),
				EdgeTopology::directed([nodes[source]], [nodes[target]]),
				None,
				PropertyMap::new(),
			)
			.unwrap();
	}
	let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	let mut options = AllShortestOptions::default();
	options.cost = PathCost::Custom;
	let paths = tensor
		.all_shortest_paths_with_filter(
			&node(&tensor, 0),
			&node(&tensor, 2),
			ProjectionOptions::default(),
			options,
			&mut |_context| Ok(Number::Int(1)),
			&mut |context| {
				context.charge_work(1)?;
				Ok(true)
			},
			&mut |context| {
				context.charge_work(1)?;
				Ok(context.hops.len() == 2)
			},
			&mut || Ok(()),
		)
		.unwrap()
		.collect_all()
		.unwrap();
	assert_eq!(paths.len(), 1);
	assert_eq!(paths[0].cost(), Number::Int(2));
	assert_eq!(paths[0].contributions(), &[Number::Int(1), Number::Int(1)]);
	assert!(
		tensor
			.all_shortest_paths_with_filter(
				&node(&tensor, 0),
				&node(&tensor, 2),
				ProjectionOptions::default(),
				options,
				&mut |_context| Ok(Number::Int(1)),
				&mut |_context| Ok(true),
				&mut |context| {
					context.charge_work(options.limits.max_work)?;
					Ok(true)
				},
				&mut || Ok(()),
			)
			.is_err()
	);
}

/// Verifies all shortest api costs do not reduce parallel adjacency strengths.
#[test]
fn all_shortest_api_costs_do_not_reduce_parallel_adjacency_strengths() {
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:0".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:1".parse().unwrap(), None, PropertyMap::new()).unwrap();
	for _ in 0..2 {
		let edge =
			graph.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new()).unwrap();
		graph.set_edge_weight(edge, Some(Number::Float(f64::MAX))).unwrap();
	}
	let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	assert!(tensor.project(ProjectionOptions::default()).is_err());
	let mut options = AllShortestOptions::default();
	options.cost = PathCost::Stored;
	let result = tensor
		.all_shortest_paths(
			&node(&tensor, 0),
			&node(&tensor, 1),
			ProjectionOptions::default(),
			options,
		)
		.unwrap()
		.collect_all()
		.unwrap();
	assert_eq!(result.len(), 2);
	assert!(result.iter().all(|path| path.cost() == Number::Float(f64::MAX)));
	let mut limits = options;
	limits.limits.max_paths = 1;
	assert!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 1),
				ProjectionOptions::default(),
				limits
			)
			.is_err()
	);
	limits = options;
	limits.limits.max_work = 1;
	assert!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 1),
				ProjectionOptions::default(),
				limits
			)
			.is_err()
	);
	limits = options;
	limits.limits.memory_bytes = 1024;
	assert!(
		tensor
			.all_shortest_paths(
				&node(&tensor, 0),
				&node(&tensor, 1),
				ProjectionOptions::default(),
				limits
			)
			.is_err()
	);
	assert!(
		tensor
			.all_shortest_paths_with_cost(
				&node(&tensor, 0),
				&node(&tensor, 1),
				ProjectionOptions::default(),
				options,
				&mut |_| Ok(Number::Int(1)),
				&mut || Err(ferros_grynn::gql_status::FerrosGrynnError::query_cancelled())
			)
			.is_err()
	);
	let other = Graph::<Mutable>::new();
	assert!(tensor.validate_binding(&other).is_err());
	assert_eq!(graph.node_count(), 2);
}

/// Verifies all shortest api materialization admits path length reduction.
#[test]
fn all_shortest_api_materialization_admits_path_length_reduction() {
	use std::sync::Arc;

	use ferros_grynn::graph::{GraphId, GraphKind, GraphRef, GraphStorageMode};
	let mut graph = Graph::<Mutable>::new();
	let a = graph.add_node_with_id("n:0".parse().unwrap(), None, PropertyMap::new()).unwrap();
	let b = graph.add_node_with_id("n:1".parse().unwrap(), None, PropertyMap::new()).unwrap();
	graph.add_edge(EdgeTopology::directed([a], [b]), None, PropertyMap::new()).unwrap();
	let tensor = EndpointTensor::from_graph(&graph, TensorOptions::default()).unwrap();
	let path = tensor
		.all_shortest_paths(
			&node(&tensor, 0),
			&node(&tensor, 1),
			ProjectionOptions::default(),
			AllShortestOptions::default(),
		)
		.unwrap()
		.enumerate()
		.next()
		.unwrap()
		.unwrap();
	let reference = Arc::new(
		GraphRef::new(
			GraphId::new(1).unwrap(),
			"runtime",
			GraphKind::Graph,
			GraphStorageMode::InMemory,
		)
		.unwrap(),
	);
	let mut limits = GraphAlgorithmLimits::default();
	limits.max_work = path.steps().len() as u64;
	assert!(
		path.materialize_path_with_references(limits, &mut |_origin| Ok(reference.clone()))
			.is_err()
	);
	let materialized = path
		.materialize_path_with_references(GraphAlgorithmLimits::default(), &mut |_origin| {
			Ok(reference.clone())
		})
		.unwrap();
	assert_eq!(materialized.length(), 1);
	assert!(materialized.resolve(&graph).is_ok());
}

/// Verifies all shortest api snapshot costs private writes and owned paths.
#[test]
fn all_shortest_api_snapshot_costs_private_writes_and_owned_paths() {
	use std::sync::Arc;

	use ferros_grynn::{
		datastore::{ClassField, ClassSchema, Datastore, GraphIndexCost},
		graph::{AtomKind, AtomOccurrenceRef, AtomRef, GraphKind},
		values::{Object, RecordId, Type, Value},
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
	setup
		.define_class(
			ClassSchema::new("e", AtomKind::Edge, [ClassField::new("fee", Type::Int)]).unwrap(),
		)
		.unwrap();
	let graph = setup.define_graph("paths", GraphKind::Graph).unwrap();
	setup.commit().unwrap();
	let fee = store.property_dictionary().get("fee").unwrap();
	let records: [RecordId; 6] =
		["n:0", "n:1", "n:2", "e:direct", "e:first", "e:second"].map(|s| s.parse().unwrap());
	let mut setup = store.begin();
	for record in &records {
		setup.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let nodes: Vec<_> = records[..3]
		.iter()
		.map(|record| setup.attach_node(graph.id(), record, None, None).unwrap())
		.collect();
	for (record, a, b, weight, cost) in [(3, 0, 2, 2, 3), (4, 0, 1, 1, 1), (5, 1, 2, 1, 1)] {
		setup
			.attach_edge(
				graph.id(),
				&records[record],
				None,
				EdgeTopology::directed([nodes[a]], [nodes[b]]),
				Some(Number::Int(weight)),
				None,
			)
			.unwrap();
		setup.set_base_property(&records[record], fee, Value::from(cost)).unwrap();
	}
	setup.commit().unwrap();
	for outcome in store
		.execute(
			"DEFINE PURE COST FUNCTION fn::fee(edge: any) -> number { RETURN <number>$edge.fee; \
			 }; DEFINE PURE COST FUNCTION fn::state(edge: any, prefix: number) -> number { RETURN \
			 $prefix + <number>$edge.fee; };",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	let occurrence = |record: usize| {
		AtomOccurrenceRef::new(
			AtomRef::new(AtomKind::Node, records[record].clone()),
			Arc::new(graph.clone()),
		)
	};
	let source = occurrence(0);
	let target = occurrence(2);
	let mut old = store.begin();
	let additive = old.named_path_cost("fn", "fee", false).unwrap();
	let stateful = old.named_path_cost("fn", "state", true).unwrap();
	let run = |tx: &mut ferros_grynn::datastore::Transaction, cost| {
		tx.all_shortest_paths(
			&graph,
			&source,
			&target,
			TensorOptions::default(),
			ProjectionOptions::default(),
			AllShortestOptions::default(),
			cost,
		)
		.unwrap()
	};
	assert_eq!(run(&mut old, GraphIndexCost::Hop).len(), 1);
	assert_eq!(run(&mut old, GraphIndexCost::Weighted).len(), 2);
	let saved = run(&mut old, additive);
	assert_eq!(saved.len(), 1);
	assert_eq!(saved[0].cost(), Some(Number::Int(2)));
	assert_eq!(run(&mut old, stateful).len(), 2);
	let mut private = store.begin();
	private.set_base_property(&records[3], fee, Value::from(0)).unwrap();
	let changed = run(&mut private, additive);
	assert_eq!(changed[0].cost(), Some(Number::Int(0)));
	assert_eq!(run(&mut old, additive)[0].cost(), Some(Number::Int(2)));
	drop(private);
	for outcome in
		store.execute("ALTER FUNCTION fn::fee(any) { RETURN 4; };", &Object::new()).unwrap()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	let mut fresh = store.begin();
	assert!(
		fresh
			.all_shortest_paths(
				&graph,
				&source,
				&target,
				TensorOptions::default(),
				ProjectionOptions::default(),
				AllShortestOptions::default(),
				additive
			)
			.is_err()
	);
	let replaced = fresh.named_path_cost("fn", "fee", false).unwrap();
	assert_eq!(run(&mut fresh, replaced)[0].cost(), Some(Number::Int(4)));
	assert_eq!(run(&mut old, additive)[0].cost(), Some(Number::Int(2)));
	drop(old);
	assert_eq!(saved[0].length(), 2);
	assert!(saved[0].steps().iter().all(|step| step.atom().graph().id() == graph.id()));
}

/// Verifies all shortest api named cost tracks external reads and hard limits.
#[test]
fn all_shortest_api_named_cost_tracks_external_reads_and_hard_limits() {
	use std::sync::Arc;

	use ferros_grynn::{
		datastore::{ClassField, ClassSchema, Datastore, GraphIndexCost},
		graph::{AtomKind, AtomOccurrenceRef, AtomRef, GraphKind},
		values::{Object, RecordId, Type, Value},
	};
	let store = Datastore::in_memory();
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("n", AtomKind::Node, []).unwrap()).unwrap();
	setup
		.define_class(
			ClassSchema::new("config", AtomKind::Node, [ClassField::new("fee", Type::Int)])
				.unwrap(),
		)
		.unwrap();
	setup
		.define_class(
			ClassSchema::new("e", AtomKind::Edge, [
				ClassField::new("ref", Type::Any),
				ClassField::new("payload", Type::String),
				ClassField::new("array", Type::Any),
				ClassField::new("time", Type::DateTime),
			])
			.unwrap(),
		)
		.unwrap();
	let graph = setup.define_graph("cost_reads", GraphKind::Graph).unwrap();
	setup.commit().unwrap();
	let fee = store.property_dictionary().get("fee").unwrap();
	let reference = store.property_dictionary().get("ref").unwrap();
	let payload = store.property_dictionary().get("payload").unwrap();
	let array = store.property_dictionary().get("array").unwrap();
	let time = store.property_dictionary().get("time").unwrap();
	let records: [RecordId; 4] = ["n:0", "n:1", "e:0", "config:cost"].map(|s| s.parse().unwrap());
	let mut setup = store.begin();
	for record in &records {
		setup.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	setup.set_base_property(&records[3], fee, Value::from(2)).unwrap();
	setup
		.set_base_property(
			&records[2],
			time,
			Value::DateTime("2024-01-01T00:00:00Z[UTC]".parse().unwrap()),
		)
		.unwrap();
	setup
		.set_base_property(&records[2], reference, Value::Record(Box::new(records[3].clone())))
		.unwrap();
	setup
		.set_base_property(&records[2], payload, Value::from("x".repeat(4 * 1024 * 1024)))
		.unwrap();
	let a = setup.attach_node(graph.id(), &records[0], None, None).unwrap();
	let b = setup.attach_node(graph.id(), &records[1], None, None).unwrap();
	setup
		.attach_edge(graph.id(), &records[2], None, EdgeTopology::directed([a], [b]), None, None)
		.unwrap();
	setup.commit().unwrap();
	for outcome in store
		.execute(
			"DEFINE PURE COST FUNCTION fn::external(edge: any) -> number { RETURN \
			 <number>$edge.ref.fee; }; DEFINE PURE COST FUNCTION fn::copy(edge: any) -> number { \
			 LET $copy = $edge.properties; RETURN 1; }; DEFINE PURE COST FUNCTION fn::loop(edge: \
			 any) -> number { FOR $i IN 0..100000 { }; RETURN 1; }; DEFINE PURE COST FUNCTION \
			 fn::missing(edge: any) -> number { TRY { RETURN <number>$edge.ref.fee; } CATCH |$e| \
			 { RETURN 1; }; }; DEFINE PURE COST FUNCTION fn::copy_array(edge: any) -> number { \
			 LET $copy = $edge.array; RETURN 1; }; DEFINE PURE COST FUNCTION fn::compare(edge: \
			 any) -> number { LET $copy = $edge.array CONTAINSALL $edge.array; RETURN 1; }; \
			 DEFINE PURE COST FUNCTION fn::regex(edge: any) -> number { LET $copy = \
			 string::matches('a', 'a{100000}'); RETURN 1; }; DEFINE PURE COST FUNCTION \
			 fn::literal_regex(edge: any) -> number { IF string::matches('привет', 'рив') AND NOT \
			 string::matches('привет', 'bye') { RETURN 1; }; RETURN 2; }; DEFINE PURE COST \
			 FUNCTION fn::format_time(edge: any) -> number { RETURN \
			 <number>time::format($edge.time, '%Y'); }; DEFINE PURE COST FUNCTION \
			 fn::wide_time(edge: any) -> number { LET $copy = time::format($edge.time, \
			 '%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%\
			 99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y%99Y'); RETURN 1; }; DEFINE PURE FUNCTION \
			 fn::slow_tokens(value: string) -> array<object> { FOR $i IN 0..100000 { }; RETURN \
			 [{term: $value, start: 0, end: 1}]; }; DEFINE ANALYZER slow FUNCTION \
			 fn::slow_tokens; DEFINE PURE COST FUNCTION fn::analyzer(edge: any) -> number { LET \
			 $copy = search::analyze('slow', 'x'); RETURN 1; };",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(outcome.result.is_ok(), "{:?}", outcome.result);
	}
	let source = AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Node, records[0].clone()),
		Arc::new(graph.clone()),
	);
	let target = AtomOccurrenceRef::new(
		AtomRef::new(AtomKind::Node, records[1].clone()),
		Arc::new(graph.clone()),
	);
	let run = |tx: &mut ferros_grynn::datastore::Transaction, cost, options| {
		tx.all_shortest_paths(
			&graph,
			&source,
			&target,
			TensorOptions::default(),
			ProjectionOptions::default(),
			options,
			cost,
		)
	};
	let mut reader = store.begin();
	let cost = reader.named_path_cost("fn", "external", false).unwrap();
	assert_eq!(
		run(&mut reader, cost, AllShortestOptions::default()).unwrap()[0].cost(),
		Some(Number::Int(2))
	);
	let mut writer = store.begin();
	writer.set_base_property(&records[3], fee, Value::from(3)).unwrap();
	writer.commit().unwrap();
	reader.create_atom("n:write".parse().unwrap(), PropertyMap::new()).unwrap();
	assert_eq!(reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
	let mut missing_reader = store.begin();
	let missing: RecordId = "config:missing".parse().unwrap();
	missing_reader
		.set_base_property(&records[2], reference, Value::Record(Box::new(missing.clone())))
		.unwrap();
	let missing_cost = missing_reader.named_path_cost("fn", "missing", false).unwrap();
	assert_eq!(
		run(&mut missing_reader, missing_cost, AllShortestOptions::default()).unwrap()[0].cost(),
		Some(Number::Int(1))
	);
	let mut writer = store.begin();
	writer.create_atom(missing, PropertyMap::new()).unwrap();
	writer.commit().unwrap();
	assert_eq!(missing_reader.commit().unwrap_err().status().code().to_string(), "FG-40001");
	let mut read = store.begin();
	let mut options = AllShortestOptions::default();
	options.limits.memory_bytes = 1024 * 1024;
	assert!(run(&mut read, GraphIndexCost::Hop, options).is_ok());
	let copy = read.named_path_cost("fn", "copy", false).unwrap();
	assert_eq!(run(&mut read, copy, options).unwrap_err().status().code().to_string(), "FG-54F13");
	options = AllShortestOptions::default();
	options.limits.max_work = 2000;
	assert!(run(&mut read, GraphIndexCost::Hop, options).is_ok());
	let loop_cost = read.named_path_cost("fn", "loop", false).unwrap();
	assert_eq!(
		run(&mut read, loop_cost, options).unwrap_err().status().code().to_string(),
		"FG-54F13"
	);
	let regex_cost = read.named_path_cost("fn", "regex", false).unwrap();
	let literal_regex = read.named_path_cost("fn", "literal_regex", false).unwrap();
	assert_eq!(
		run(&mut read, literal_regex, AllShortestOptions::default()).unwrap()[0].cost(),
		Some(Number::Int(1))
	);
	let format_time = read.named_path_cost("fn", "format_time", false).unwrap();
	assert_eq!(
		run(&mut read, format_time, AllShortestOptions::default()).unwrap()[0].cost(),
		Some(Number::Int(2024))
	);
	let wide_time = read.named_path_cost("fn", "wide_time", false).unwrap();
	let mut helper_limits = options;
	helper_limits.limits.max_work = 1000;
	assert!(run(&mut read, GraphIndexCost::Hop, helper_limits).is_ok());
	assert!(run(&mut read, format_time, helper_limits).is_ok());
	assert_eq!(
		run(&mut read, wide_time, helper_limits).unwrap_err().status().code().to_string(),
		"FG-54F13"
	);
	let analyzer = read.named_path_cost("fn", "analyzer", false).unwrap();
	assert_eq!(
		run(&mut read, analyzer, options).unwrap_err().status().code().to_string(),
		"FG-54F13"
	);
	assert_eq!(
		run(&mut read, regex_cost, AllShortestOptions::default())
			.unwrap_err()
			.status()
			.code()
			.to_string(),
		"FG-54F13"
	);
	read.set_base_property(
		&records[2],
		array,
		Value::Array((0..100000).map(Value::from).collect()),
	)
	.unwrap();
	let copy_array = read.named_path_cost("fn", "copy_array", false).unwrap();
	assert_eq!(
		run(&mut read, copy_array, options).unwrap_err().status().code().to_string(),
		"FG-54F13"
	);
	read.set_base_property(&records[2], array, Value::Array((0..100).map(Value::from).collect()))
		.unwrap();
	assert!(run(&mut read, copy_array, options).is_ok());
	let compare = read.named_path_cost("fn", "compare", false).unwrap();
	assert_eq!(
		run(&mut read, compare, options).unwrap_err().status().code().to_string(),
		"FG-54F13"
	);
}
