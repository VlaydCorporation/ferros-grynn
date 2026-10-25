use std::{
	fs::{self, OpenOptions},
	hint::black_box,
	io::Write,
	path::Path,
	time::Instant,
};

use super::select::{profile_usize, stage13_resource_store};
#[cfg(test)]
use crate::test_assertions::AssertOption as _;
#[cfg(test)]
use crate::test_assertions::AssertResult as _;
use crate::{
	datastore::Datastore,
	values::{Object, Value},
};

/// Verifies gql stage 14 optimizer probe.
#[test]
#[ignore = "stage-14 optimizer performance and profiling gate"]
fn gql_stage_14_optimizer_probe() {
	let rows = profile_usize("FG_OPT_ROWS", 100_000).max(1);
	let selectivity_bps = profile_usize("FG_OPT_SELECTIVITY_BPS", 100).min(10_000);
	let samples = profile_usize("FG_OPT_SAMPLES", 9).max(3);
	let threshold = rows.saturating_mul(selectivity_bps).div_ceil(10_000);
	let store = stage13_resource_store(rows, 16);
	let setup = store
		.execute(
			"DEFINE INDEX by_score ON CLASS person FIELDS score UNIQUE SCOPE BASE; DEFINE INDEX \
			 by_department_code ON CLASS department FIELDS code UNIQUE SCOPE BASE;",
			&Object::new(),
		)
		.assert_ok();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let scan = format!("SELECT VALUE score FROM person WITH NOINDEX WHERE score < {threshold};");
	let forced =
		format!("SELECT VALUE score FROM person WITH INDEX by_score WHERE score < {threshold};");
	let automatic = format!("SELECT VALUE score FROM person WHERE score < {threshold};");
	let expected = execute_value(&store, &scan);
	assert_eq!(execute_value(&store, &forced), expected);
	assert_eq!(execute_value(&store, &automatic), expected);
	let operator = explain_operator(&store, &automatic);
	if selectivity_bps <= 100 && rows >= 100_000 {
		assert_eq!(operator, "ScalarIndexScan");
	}

	let queries = [&scan, &forced, &automatic];
	let mut timings = [Vec::new(), Vec::new(), Vec::new()];
	for round in 0..samples {
		for offset in 0..queries.len() {
			let index = (round + offset) % queries.len();
			timings[index].push(time_query(&store, queries[index]));
		}
	}

	let order_scan =
		"SELECT VALUE score FROM person WITH NOINDEX ORDER BY score ASC NULLS FIRST LIMIT 100;";
	let order_index = "SELECT VALUE score FROM person WITH INDEX by_score ORDER BY score ASC \
	                   NULLS FIRST LIMIT 100;";
	assert_eq!(execute_value(&store, order_scan), execute_value(&store, order_index));
	let mut order_scan_ns = Vec::new();
	let mut order_index_ns = Vec::new();
	for round in 0..samples.min(7) {
		if round % 2 == 0 {
			order_scan_ns.push(time_query(&store, order_scan));
			order_index_ns.push(time_query(&store, order_index));
		} else {
			order_index_ns.push(time_query(&store, order_index));
			order_scan_ns.push(time_query(&store, order_scan));
		}
	}

	let planning_samples = samples.max(20);
	let explain = format!("EXPLAIN FULL FORMAT JSON {automatic}");
	let mut planning_ns = Vec::with_capacity(planning_samples);
	for _ in 0..planning_samples {
		let value = match execute_value(&store, &explain) {
			Value::Object(value) => Some(value),
			_ => None,
		}
		.assert_some_with("EXPLAIN FULL JSON must return an object");
		let value = value
			.get("planning_elapsed_ns")
			.and_then(Value::as_number)
			.and_then(|number| number.as_i64())
			.assert_some_with("EXPLAIN must report planning_elapsed_ns");
		planning_ns.push(u128::try_from(value).assert_ok());
	}

	let (join_auto_ns, join_forced_ns) =
		if std::env::var("FG_OPT_INCLUDE_JOIN").as_deref() == Ok("1") {
			let automatic = "SELECT VALUE p.id FROM person AS p JOIN department AS d ON \
			                 p.department = d.code LIMIT 100;";
			let forced = "SELECT VALUE p.id FROM person AS p JOIN department AS d ON p.department \
			              = d.code WITH INDEX d.by_department_code LIMIT 100;";
			assert_eq!(execute_value(&store, automatic), execute_value(&store, forced));
			(
				median((0..samples.min(5)).map(|_| time_query(&store, automatic)).collect()),
				median((0..samples.min(5)).map(|_| time_query(&store, forced)).collect()),
			)
		} else {
			(0, 0)
		};

	let result = ProbeResult {
		rows,
		selectivity_bps,
		samples,
		operator,
		scan_ns: median(std::mem::take(&mut timings[0])),
		forced_ns: median(std::mem::take(&mut timings[1])),
		automatic_ns: median(std::mem::take(&mut timings[2])),
		order_scan_ns: median(order_scan_ns),
		order_index_ns: median(order_index_ns),
		planning_p95_ns: percentile(planning_ns, 95),
		join_auto_ns,
		join_forced_ns,
	};
	emit_result(&result);
}

/// Timing samples and chosen operators reported by the optimizer performance
/// probe.
struct ProbeResult {
	/// Configured row count of the fixture or measurement.
	rows: usize,
	/// Selected-row fraction expressed in basis points.
	selectivity_bps: usize,
	/// Number of repeated observations included in the reported measurement.
	samples: usize,
	/// Chosen physical source operator reported by the optimizer probe.
	operator: &'static str,
	/// Elapsed time for the scan measurement, in nanoseconds.
	scan_ns: u128,
	/// Elapsed time for the forced measurement, in nanoseconds.
	forced_ns: u128,
	/// Elapsed time for the automatic measurement, in nanoseconds.
	automatic_ns: u128,
	/// Elapsed time for the order scan measurement, in nanoseconds.
	order_scan_ns: u128,
	/// Elapsed time for the order index measurement, in nanoseconds.
	order_index_ns: u128,
	/// Elapsed time for the planning p95 measurement, in nanoseconds.
	planning_p95_ns: u128,
	/// Elapsed time for the join auto measurement, in nanoseconds.
	join_auto_ns: u128,
	/// Elapsed time for the join forced measurement, in nanoseconds.
	join_forced_ns: u128,
}

/// Executes value for this module.
fn execute_value(store: &Datastore, query: &str) -> Value {
	let value = store
		.execute(query, &Object::new())
		.assert_ok()
		.remove(0)
		.result
		.assert_ok()
		.into_parts()
		.0;
	black_box(value.clone());
	value
}

/// Returns time query for this module.
fn time_query(store: &Datastore, query: &str) -> u128 {
	let started = Instant::now();
	black_box(execute_value(store, query));
	started.elapsed().as_nanos()
}

/// Returns explain operator for this module.
fn explain_operator(store: &Datastore, query: &str) -> &'static str {
	let explain = match execute_value(store, &format!("EXPLAIN FULL FORMAT JSON {query}")) {
		Value::Object(explain) => Some(explain),
		_ => None,
	}
	.assert_some_with("EXPLAIN FULL JSON must return an object");
	let nodes = match explain.get("nodes") {
		Some(Value::Array(nodes)) => Some(nodes),
		_ => None,
	}
	.assert_some_with("EXPLAIN must contain physical nodes");
	let node = match nodes.iter().next() {
		Some(Value::Object(node)) => Some(node),
		_ => None,
	}
	.assert_some_with("EXPLAIN must contain one source node");
	match node.get("operator").and_then(Value::as_str) {
		Some("ScalarIndexScan") => Some("ScalarIndexScan"),
		Some("ClassScan") => Some("ClassScan"),
		_ => None,
	}
	.assert_some_with("source operator must be ScalarIndexScan or ClassScan")
}

/// Returns median for this module.
fn median(values: Vec<u128>) -> u128 {
	percentile(values, 50)
}

/// Returns percentile for this module.
fn percentile(mut values: Vec<u128>, percentile: usize) -> u128 {
	values.sort_unstable();
	let index = values.len().saturating_sub(1).saturating_mul(percentile).div_ceil(100);
	values[index]
}

/// Returns emit result for this module.
fn emit_result(result: &ProbeResult) {
	let row = format!(
		"{},{},{},{},{},{},{},{},{},{},{},{}",
		result.rows,
		result.selectivity_bps,
		result.samples,
		result.operator,
		result.scan_ns,
		result.forced_ns,
		result.automatic_ns,
		result.order_scan_ns,
		result.order_index_ns,
		result.planning_p95_ns,
		result.join_auto_ns,
		result.join_forced_ns,
	);
	eprintln!("stage14_csv,{row}");
	let Ok(path) = std::env::var("FG_OPT_CSV") else {
		return;
	};
	let path = Path::new(&path);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).assert_ok();
	}
	let write_header = path.metadata().map_or(true, |metadata| metadata.len() == 0);
	let mut file = OpenOptions::new().create(true).append(true).open(path).assert_ok();
	if write_header {
		writeln!(
			file,
			"rows,selectivity_bps,samples,operator,scan_median_ns,forced_median_ns,\
			 automatic_median_ns,order_scan_median_ns,order_index_median_ns,planning_p95_ns,\
			 join_auto_median_ns,join_forced_median_ns"
		)
		.assert_ok();
	}
	writeln!(file, "{row}").assert_ok();
}
