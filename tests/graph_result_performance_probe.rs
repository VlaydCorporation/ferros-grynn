#![cfg(test)]
#![allow(
	clippy::unwrap_used,
	reason = "the ignored profiling probe must stop when its fixed fixture is invalid"
)]
/// Assertions shared with the root unit-test harness.
#[path = "common/assertions.rs"]
pub mod test_assertions;

use std::{
	error::Error,
	fs::{self, OpenOptions},
	hint::black_box,
	io::Write,
	path::{Path, PathBuf},
	time::Instant,
};

use ferros_grynn::{
	datastore::{ClassSchema, Datastore},
	graph::{AtomKind, EdgeTopology, Frozen, Graph, GraphRef, query_to_memory},
	values::{Object, PropertyMap, RecordId, Uuid, Value},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Elapsed nanoseconds, graph population and process-memory delta for one
/// probe.
type ProbeMeasurement = (u128, (usize, usize), i128);

/// Graph materialization query shared by the paired graph-result measurements.
const BUILD_QUERY: &str = "BUILD GRAPH <GRAPH> FROM $graph";

/// Runs the paired Stage-18 full-graph assembly performance gate.
#[test]
#[ignore = "stage-18 graph-result performance gate; writes target/gql-stage18 artifacts"]
fn gql_graph_result_stage_18_probe() -> Result<(), Box<dyn Error>> {
	let samples = env_usize("FG_STAGE18_SAMPLES", 9).max(9);
	let nodes = env_usize("FG_STAGE18_NODES", 10_000).max(1);
	let (store, graph) = fixture(nodes)?;
	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph.clone())));

	assert_counts(build_graph(&store, &variables)?.1, nodes)?;
	assert_counts(copy_graph(&store, &graph)?.1, nodes)?;

	let artifact = artifact_file()?;
	let mut csv = OpenOptions::new().create(true).truncate(true).write(true).open(&artifact)?;
	writeln!(csv, "round,order,candidate,nodes,edges,elapsed_ns,rss_delta_bytes")?;
	for round in 0..samples {
		let order = if round % 2 == 0 {
			["query_to_memory", "build_graph"]
		} else {
			["build_graph", "query_to_memory"]
		};
		for (position, candidate) in order.into_iter().enumerate() {
			let (elapsed, (actual_nodes, actual_edges), rss_delta) = match candidate {
				"query_to_memory" => copy_graph(&store, &graph)?,
				"build_graph" => build_graph(&store, &variables)?,
				_ => unreachable!(),
			};
			assert_counts((actual_nodes, actual_edges), nodes)?;
			writeln!(
				csv,
				"{round},{position},{candidate},{actual_nodes},{actual_edges},{elapsed},\
				 {rss_delta}"
			)?;
		}
	}
	csv.flush()?;
	println!("artifact={}", artifact.display());
	Ok(())
}

/// Builds graph for this module.
fn build_graph(store: &Datastore, variables: &Object) -> Result<ProbeMeasurement, Box<dyn Error>> {
	let before = process_memory();
	let started = Instant::now();
	let mut results = store.execute(BUILD_QUERY, variables)?;
	let value = results.remove(0).result?.into_parts().0;
	let Value::Graph(graph) = value else {
		return Err("BUILD GRAPH did not return a graph".into());
	};
	let elapsed = started.elapsed().as_nanos();
	let after = process_memory();
	let counts = (graph.node_count(), graph.edge_count());
	black_box(graph);
	Ok((elapsed, counts, i128::from(after) - i128::from(before)))
}

/// Returns copy graph for this module.
fn copy_graph(store: &Datastore, graph: &GraphRef) -> Result<ProbeMeasurement, Box<dyn Error>> {
	let before = process_memory();
	let started = Instant::now();
	let transaction = store.begin();
	let view = graph.read(&transaction)?;
	let copied = query_to_memory::<Graph<Frozen>, _>(&view)?;
	let elapsed = started.elapsed().as_nanos();
	let after = process_memory();
	let counts = (copied.node_count(), copied.edge_count());
	black_box(copied);
	Ok((elapsed, counts, i128::from(after) - i128::from(before)))
}

/// Returns assert counts for this module.
fn assert_counts(actual: (usize, usize), nodes: usize) -> Result<(), Box<dyn Error>> {
	let expected = (nodes, nodes.saturating_sub(1));
	if actual != expected {
		return Err(format!("graph counts are {actual:?}, expected {expected:?}").into());
	}
	Ok(())
}

/// Builds the fixture fixture used by nearby scenarios.
fn fixture(nodes: usize) -> Result<(Datastore, GraphRef), Box<dyn Error>> {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("stage18_node", AtomKind::Node, [])?)?;
	transaction.define_class(ClassSchema::new("stage18_edge", AtomKind::Edge, [])?)?;
	let graph = transaction.define_graph("stage18_probe", ferros_grynn::graph::GraphKind::Graph)?;
	let mut previous = None;
	for index in 0..nodes {
		let record = format!("stage18_node:{index}").parse::<RecordId>()?;
		transaction.create_atom(record.clone(), PropertyMap::new())?;
		let current = transaction.attach_node(graph.id(), &record, None, None)?;
		if let Some(previous) = previous {
			let edge = format!("stage18_edge:{index}").parse::<RecordId>()?;
			transaction.create_atom(edge.clone(), PropertyMap::new())?;
			transaction.attach_edge(
				graph.id(),
				&edge,
				None,
				EdgeTopology::directed([previous], [current]),
				None,
				None,
			)?;
		}
		previous = Some(current);
	}
	transaction.commit()?;
	Ok((store, graph))
}

/// Returns env usize for this module.
fn env_usize(name: &str, default: usize) -> usize {
	std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// Returns process memory for this module.
fn process_memory() -> u64 {
	let pid = Pid::from(std::process::id() as usize);
	let mut system = System::new();
	system.refresh_processes_specifics(
		ProcessesToUpdate::Some(&[pid]),
		true,
		ProcessRefreshKind::nothing().with_memory(),
	);
	system.process(pid).map_or(0, sysinfo::Process::memory)
}

/// Returns artifact file for this module.
fn artifact_file() -> Result<PathBuf, Box<dyn Error>> {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("gql-stage18");
	fs::create_dir_all(&root)?;
	Ok(root.join(format!("graph-result-{}.csv", Uuid::now_v7())))
}
