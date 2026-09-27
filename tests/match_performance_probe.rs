#![allow(
	clippy::expect_used,
	clippy::unwrap_used,
	reason = "the ignored profiling probe must stop on invalid setup or query results"
)]

use std::{
	fs::{self, OpenOptions},
	hint::black_box,
	io::Write,
	path::Path,
	time::Instant,
};

use ferros_grynn::{
	datastore::{ClassSchema, Datastore, Transaction},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId, Value},
};

#[test]
#[ignore = "stage-15/16 MATCH performance gate"]
fn gql_match_stage_15_16_probe() {
	let rows = env_usize("FG_MATCH_ROWS", 4_096).max(64);
	let fanout = env_usize("FG_MATCH_FANOUT", 4).clamp(1, 32);
	let depth = env_usize("FG_MATCH_DEPTH", 4).clamp(1, 7);
	let samples = env_usize("FG_MATCH_SAMPLES", 15).max(5);
	let store = match_store(rows, fanout);
	let queries = [
		("class", "MATCH (probe_node AS p) IN GRAPH match_probe RETURN p".to_owned()),
		("label", "MATCH (probe_node AS p LABEL hot) IN GRAPH match_probe RETURN p".to_owned()),
		(
			"one_hop",
			"MATCH (probe_node:n0 AS p)-[probe_edge]->(probe_node AS q) IN GRAPH match_probe \
			 RETURN q"
				.to_owned(),
		),
		(
			"trail",
			format!(
				"MATCH (probe_node:n0 AS p)-[probe_edge*1..{depth}]->(probe_node AS q) IN GRAPH \
				 match_probe RETURN q"
			),
		),
	];
	let mut transaction = store.begin();
	let expected = queries
		.iter()
		.map(|(_, query)| execute(&mut transaction, query).as_array().unwrap().len())
		.collect::<Vec<_>>();
	let mut elapsed = vec![Vec::with_capacity(samples); queries.len()];
	for round in 0..samples {
		for offset in 0..queries.len() {
			let index = (round + offset) % queries.len();
			let started = Instant::now();
			let value = execute(&mut transaction, &queries[index].1);
			assert_eq!(value.as_array().unwrap().len(), expected[index]);
			black_box(value);
			elapsed[index].push(started.elapsed().as_nanos());
		}
	}
	let variant = std::env::var("FG_MATCH_VARIANT").unwrap_or_else(|_| "current".to_owned());
	for (index, (workload, _)) in queries.iter().enumerate() {
		elapsed[index].sort_unstable();
		emit(ProbeRow {
			variant: &variant,
			workload,
			rows,
			fanout,
			depth,
			samples,
			result_rows: expected[index],
			median_ns: percentile(&elapsed[index], 50),
			p95_ns: percentile(&elapsed[index], 95),
		});
	}
}

#[test]
#[ignore = "stage-16 graph-first resource matrix"]
fn gql_match_stage_16_resource_probe() {
	for (branching, depth) in [(32, 1), (8, 4), (2, 7), (1, 30)] {
		let store = layered_path_store(branching, depth);
		let query = format!(
			"EXPLAIN ANALYZE FORMAT JSON MATCH (probe_node:n0 AS \
			 source)-[probe_edge*{depth}..{depth} SHORTEST]->(probe_node AS target) IN GRAPH \
			 resource_path RETURN target"
		);
		emit_resource("path", branching, depth, &analyze(&store, &query));
	}
	for arity in [2, 8, 32] {
		let store = hyper_store(arity);
		emit_resource(
			"hyper",
			arity,
			1,
			&analyze(
				&store,
				"EXPLAIN ANALYZE FORMAT JSON MATCH HYPER [probe_hyper_edge AS e] CONTAINS HEAD \
				 [(probe_hyper_node AS h)], TAIL [(probe_hyper_node AS t)] IN GRAPH \
				 resource_hyper RETURN h, t",
			),
		);
	}
	for fanout in [1, 4, 16] {
		let store = nesting_store(fanout);
		emit_resource(
			"within",
			fanout,
			1,
			&analyze(
				&store,
				"EXPLAIN ANALYZE FORMAT JSON MATCH (probe_leaf AS child) WITHIN <(probe_owner AS \
				 owner)> IN GRAPH resource_meta RETURN child, owner",
			),
		);
	}
}

fn match_store(rows: usize, fanout: usize) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("probe_node", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("probe_edge", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = transaction.define_graph("match_probe", GraphKind::Graph).unwrap();
	let nodes = (0..rows)
		.map(|index| format!("probe_node:n{index}").parse::<RecordId>().unwrap())
		.collect::<Vec<_>>();
	for node in &nodes {
		transaction.create_atom(node.clone(), PropertyMap::new()).unwrap();
	}
	let local = nodes
		.iter()
		.enumerate()
		.map(|(index, node)| {
			transaction
				.attach_node(graph.id(), node, (index % 2 == 0).then(|| "hot".into()), None)
				.unwrap()
		})
		.collect::<Vec<_>>();
	for source in 0..rows {
		for offset in 1..=fanout {
			let edge = format!("probe_edge:e{source}_{offset}").parse::<RecordId>().unwrap();
			transaction.create_atom(edge.clone(), PropertyMap::new()).unwrap();
			transaction
				.attach_edge(
					graph.id(),
					&edge,
					None,
					EdgeTopology::directed([local[source]], [local[(source + offset) % rows]]),
					None,
					None,
				)
				.unwrap();
		}
	}
	transaction.commit().unwrap();
	store
}

fn layered_path_store(branching: usize, depth: usize) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("probe_node", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("probe_edge", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = transaction.define_graph("resource_path", GraphKind::Graph).unwrap();
	let root = "probe_node:n0".parse::<RecordId>().unwrap();
	transaction.create_atom(root.clone(), PropertyMap::new()).unwrap();
	let mut frontier = vec![transaction.attach_node(graph.id(), &root, None, None).unwrap()];
	let mut next_node = 1_usize;
	for level in 0..depth {
		let mut next = Vec::with_capacity(frontier.len().saturating_mul(branching));
		for parent in frontier {
			for branch in 0..branching {
				let node = format!("probe_node:n{next_node}").parse::<RecordId>().unwrap();
				let edge = format!("probe_edge:e{level}_{next_node}_{branch}")
					.parse::<RecordId>()
					.unwrap();
				transaction.create_atom(node.clone(), PropertyMap::new()).unwrap();
				transaction.create_atom(edge.clone(), PropertyMap::new()).unwrap();
				let local = transaction.attach_node(graph.id(), &node, None, None).unwrap();
				transaction
					.attach_edge(
						graph.id(),
						&edge,
						None,
						EdgeTopology::directed([parent], [local]),
						None,
						None,
					)
					.unwrap();
				next.push(local);
				next_node += 1;
			}
		}
		frontier = next;
	}
	transaction.commit().unwrap();
	store
}

fn hyper_store(arity: usize) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction
		.define_class(ClassSchema::new("probe_hyper_node", AtomKind::Node, []).unwrap())
		.unwrap();
	transaction
		.define_class(ClassSchema::new("probe_hyper_edge", AtomKind::Edge, []).unwrap())
		.unwrap();
	let graph = transaction.define_graph("resource_hyper", GraphKind::Hypergraph).unwrap();
	let mut heads = Vec::with_capacity(arity);
	for index in 0..arity {
		let node = format!("probe_hyper_node:h{index}").parse::<RecordId>().unwrap();
		transaction.create_atom(node.clone(), PropertyMap::new()).unwrap();
		heads.push(transaction.attach_node(graph.id(), &node, None, None).unwrap());
	}
	let tail = "probe_hyper_node:tail".parse::<RecordId>().unwrap();
	let edge = "probe_hyper_edge:edge".parse::<RecordId>().unwrap();
	transaction.create_atom(tail.clone(), PropertyMap::new()).unwrap();
	transaction.create_atom(edge.clone(), PropertyMap::new()).unwrap();
	let tail = transaction.attach_node(graph.id(), &tail, None, None).unwrap();
	transaction
		.attach_edge(graph.id(), &edge, None, EdgeTopology::directed(heads, [tail]), None, None)
		.unwrap();
	transaction.commit().unwrap();
	store
}

fn nesting_store(fanout: usize) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("probe_owner", AtomKind::Node, []).unwrap()).unwrap();
	transaction.define_class(ClassSchema::new("probe_leaf", AtomKind::Node, []).unwrap()).unwrap();
	let child = transaction.define_graph("resource_child", GraphKind::Graph).unwrap();
	let meta = transaction.define_graph("resource_meta", GraphKind::Metagraph).unwrap();
	let leaf = "probe_leaf:leaf".parse::<RecordId>().unwrap();
	transaction.create_atom(leaf.clone(), PropertyMap::new()).unwrap();
	transaction.attach_node(child.id(), &leaf, None, None).unwrap();
	for index in 0..fanout {
		let owner = format!("probe_owner:o{index}").parse::<RecordId>().unwrap();
		transaction.create_atom(owner.clone(), PropertyMap::new()).unwrap();
		transaction.attach_node(meta.id(), &owner, None, Some(child.id())).unwrap();
	}
	transaction.commit().unwrap();
	store
}

fn execute(transaction: &mut Transaction, query: &str) -> Value {
	transaction.execute(query, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0
}

fn analyze(store: &Datastore, query: &str) -> Object {
	let Value::Object(root) =
		store.execute(query, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0
	else {
		panic!("EXPLAIN ANALYZE must return an object")
	};
	let Some(Value::Object(metrics)) = root.get("analyze") else {
		panic!("EXPLAIN ANALYZE metrics are missing")
	};
	metrics.clone()
}

fn metric(metrics: &Object, name: &str) -> u64 {
	metrics
		.get(name)
		.and_then(Value::as_number)
		.and_then(|value| u64::try_from(value).ok())
		.unwrap()
}

fn env_usize(name: &str, default: usize) -> usize {
	std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
	let index = values.len().saturating_sub(1).saturating_mul(percentile).div_ceil(100);
	values[index]
}

struct ProbeRow<'a> {
	variant: &'a str,
	workload: &'a str,
	rows: usize,
	fanout: usize,
	depth: usize,
	samples: usize,
	result_rows: usize,
	median_ns: u128,
	p95_ns: u128,
}

fn emit(row: ProbeRow<'_>) {
	let csv = format!(
		"{},{},{},{},{},{},{},{},{}",
		row.variant,
		row.workload,
		row.rows,
		row.fanout,
		row.depth,
		row.samples,
		row.result_rows,
		row.median_ns,
		row.p95_ns,
	);
	eprintln!("stage15_16_csv,{csv}");
	let Ok(path) = std::env::var("FG_MATCH_CSV") else {
		return;
	};
	let path = Path::new(&path);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).unwrap();
	}
	let write_header = path.metadata().map_or(true, |metadata| metadata.len() == 0);
	let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
	if write_header {
		writeln!(file, "variant,workload,rows,fanout,depth,samples,result_rows,median_ns,p95_ns")
			.unwrap();
	}
	writeln!(file, "{csv}").unwrap();
}

fn emit_resource(workload: &str, width: usize, depth: usize, metrics: &Object) {
	let csv = format!(
		"{workload},{width},{depth},{},{},{},{},{},{},{},{}",
		metric(metrics, "actual_rows"),
		metric(metrics, "elapsed_ns"),
		metric(metrics, "path_expansions"),
		metric(metrics, "frontier_peak"),
		metric(metrics, "peak_accounted_memory"),
		metric(metrics, "graph_opens"),
		metric(metrics, "hyper_assignments"),
		metric(metrics, "containment_links"),
	);
	eprintln!("stage16_resource_csv,{csv}");
	let Ok(path) = std::env::var("FG_MATCH_RESOURCE_CSV") else {
		return;
	};
	let path = Path::new(&path);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).unwrap();
	}
	let write_header = path.metadata().map_or(true, |metadata| metadata.len() == 0);
	let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
	if write_header {
		writeln!(
			file,
			"workload,width,depth,actual_rows,elapsed_ns,path_expansions,frontier_peak,\
			 peak_accounted_memory,graph_opens,hyper_assignments,containment_links"
		)
		.unwrap();
	}
	writeln!(file, "{csv}").unwrap();
}
