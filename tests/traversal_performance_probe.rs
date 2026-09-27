#![allow(
	clippy::unwrap_used,
	reason = "the ignored profiling probe must stop when its fixed fixture is invalid"
)]

use std::{
	error::Error,
	fs::{self, OpenOptions},
	hint::black_box,
	io::Write,
	path::{Path, PathBuf},
	time::Instant,
};

use ferros_grynn::{
	datastore::{ClassSchema, Datastore, GqlSession},
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Array, Object, PropertyMap, RecordId, Uuid, Value},
};

const FAST_QUERY: &str = "TRAVERSE FROM [0] |> tv::branch(|$value| $fanout) |> tv::count();";
const MATCH_QUERY: &str = "MATCH (stage17_node:source AS source)-[stage17_edge]->(stage17_node AS \
                           target) IN GRAPH stage17_probe RETURN target;";
const TRAVERSE_QUERY: &str = "TRAVERSE FROM $source IN GRAPH $graph |> tv::out();";

/// Runs the paired Stage-17 VM performance gates under the profiling profile.
#[test]
#[ignore = "stage-17 traversal VM performance gate; writes target/gql-stage17 artifacts"]
fn gql_traversal_stage_17_probe() -> Result<(), Box<dyn Error>> {
	let samples = env_usize("FG_STAGE17_VM_SAMPLES", 15).max(9);
	let iterations = env_usize("FG_STAGE17_VM_ITERATIONS", 16).max(1);
	let fanout = env_usize("FG_STAGE17_VM_FANOUT", 8_192).max(2);
	let graph_fanout = env_usize("FG_STAGE17_GRAPH_FANOUT", 32).clamp(1, 256);
	let (store, graph, source) = fixture(graph_fanout)?;
	let mut variables = Object::new();
	variables.insert(
		"fanout",
		Value::Array(Array::new(
			(0..fanout).map(|index| Value::from((index % 2) as i64)).collect(),
		)),
	);
	let mut graph_variables = Object::new();
	graph_variables.insert("graph", Value::GraphRef(Box::new(graph)));
	graph_variables.insert("source", Value::Record(Box::new(source)));

	let mut best_effort = store.session();
	let mut fast = store.session();
	begin(&mut best_effort, "BEGIN;")?;
	begin(&mut fast, "BEGIN EXECUTION MODE FAST;")?;
	assert_count(&mut best_effort, &variables, fanout)?;
	assert_count(&mut fast, &variables, fanout)?;

	let artifact = artifact_file()?;
	let mut csv = OpenOptions::new().create(true).truncate(true).write(true).open(&artifact)?;
	writeln!(csv, "round,order,workload,candidate,operations,elapsed_ns,result_items")?;
	for round in 0..samples {
		let order = if round % 2 == 0 {
			[("best_effort", false), ("fast", true)]
		} else {
			[("fast", true), ("best_effort", false)]
		};
		for (position, (candidate, use_fast)) in order.into_iter().enumerate() {
			let session = if use_fast {
				&mut fast
			} else {
				&mut best_effort
			};
			let (elapsed, result_items) =
				measure(session, FAST_QUERY, &variables, iterations, Some(fanout))?;
			writeln!(
				csv,
				"{round},{position},unordered_fanout,{candidate},{iterations},{elapsed},\
				 {result_items}"
			)?;
		}

		let graph_order = if round % 2 == 0 {
			[("match", MATCH_QUERY), ("traverse", TRAVERSE_QUERY)]
		} else {
			[("traverse", TRAVERSE_QUERY), ("match", MATCH_QUERY)]
		};
		for (position, (candidate, query)) in graph_order.into_iter().enumerate() {
			let (elapsed, result_items) =
				measure(&mut best_effort, query, &graph_variables, iterations, None)?;
			if result_items != graph_fanout {
				return Err(format!(
					"{candidate} returned {result_items} rows, expected {graph_fanout}"
				)
				.into());
			}
			writeln!(
				csv,
				"{round},{position},one_hop,{candidate},{iterations},{elapsed},{result_items}"
			)?;
		}
	}
	csv.flush()?;
	best_effort.execute("CANCEL;", &Object::new())?;
	fast.execute("CANCEL;", &Object::new())?;
	println!("artifact={}", artifact.display());
	Ok(())
}

fn begin(session: &mut GqlSession, statement: &str) -> Result<(), Box<dyn Error>> {
	let mut results = session.execute(statement, &Object::new())?;
	results.remove(0).result?;
	Ok(())
}

fn assert_count(
	session: &mut GqlSession,
	variables: &Object,
	expected: usize,
) -> Result<(), Box<dyn Error>> {
	let result = execute(session, FAST_QUERY, variables)?;
	let Value::Array(values) = result else {
		return Err("count traversal did not return an array".into());
	};
	let actual =
		values.get(0).and_then(Value::as_number).and_then(|number| u64::try_from(number).ok());
	if actual != u64::try_from(expected).ok() {
		return Err(format!("count traversal returned {actual:?}, expected {expected}").into());
	}
	Ok(())
}

fn measure(
	session: &mut GqlSession,
	query: &str,
	variables: &Object,
	iterations: usize,
	expected_count: Option<usize>,
) -> Result<(u128, usize), Box<dyn Error>> {
	let started = Instant::now();
	let mut result_items = 0;
	for _ in 0..iterations {
		let value = execute(session, query, variables)?;
		let Value::Array(values) = &value else {
			return Err("profiled traversal did not return an array".into());
		};
		result_items = values.len();
		if let Some(expected) = expected_count {
			let actual = values
				.get(0)
				.and_then(Value::as_number)
				.and_then(|number| usize::try_from(number).ok());
			if actual != Some(expected) {
				return Err(
					format!("profiled count returned {actual:?}, expected {expected}").into()
				);
			}
		}
		black_box(value);
	}
	Ok((started.elapsed().as_nanos() / iterations as u128, result_items))
}

fn execute(
	session: &mut GqlSession,
	query: &str,
	variables: &Object,
) -> Result<Value, Box<dyn Error>> {
	let mut results = session.execute(query, variables)?;
	Ok(results.remove(0).result?.into_parts().0)
}

fn fixture(
	fanout: usize,
) -> Result<(Datastore, ferros_grynn::graph::GraphRef, RecordId), Box<dyn Error>> {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("stage17_node", AtomKind::Node, [])?)?;
	transaction.define_class(ClassSchema::new("stage17_edge", AtomKind::Edge, [])?)?;
	let graph = transaction.define_graph("stage17_probe", GraphKind::Graph)?;
	let source = "stage17_node:source".parse::<RecordId>()?;
	transaction.create_atom(source.clone(), PropertyMap::new())?;
	let source = transaction.attach_node(graph.id(), &source, None, None)?;
	for index in 0..fanout {
		let target = format!("stage17_node:target-{index}").parse::<RecordId>()?;
		let edge = format!("stage17_edge:edge-{index}").parse::<RecordId>()?;
		transaction.create_atom(target.clone(), PropertyMap::new())?;
		transaction.create_atom(edge.clone(), PropertyMap::new())?;
		let target = transaction.attach_node(graph.id(), &target, None, None)?;
		transaction.attach_edge(
			graph.id(),
			&edge,
			None,
			EdgeTopology::directed([source], [target]),
			None,
			None,
		)?;
	}
	transaction.commit()?;
	Ok((store, graph, "stage17_node:source".parse()?))
}

fn env_usize(name: &str, default: usize) -> usize {
	std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn artifact_file() -> Result<PathBuf, Box<dyn Error>> {
	let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("gql-stage17");
	fs::create_dir_all(&root)?;
	Ok(root.join(format!("traversal-vm-{}.csv", Uuid::now_v7())))
}
