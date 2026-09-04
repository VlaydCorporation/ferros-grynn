//! Process-isolated Mode 1 query-throughput probe.

use std::{error::Error, hint::black_box, io, time::Instant};

use ferros_grynn::{
	datastore::{Datastore, TableField, TableKind, TableSchema},
	graph::{Graph, Mutable},
	values::{Object, PropertyMap, Type, Value},
};

/// Builds one immutable in-memory table used by both feature variants.
fn build_store(row_count: usize) -> Result<Datastore, Box<dyn Error>> {
	let store = Datastore::in_memory();
	let payload = store.intern_property("payload")?;
	let mut graph = Graph::<Mutable>::with_capacity(row_count, 0);
	for row in 0..row_count {
		let mut properties = PropertyMap::new();
		properties.insert(payload, i64::try_from(row)?);
		graph.add_node_with_id(format!("item:{row}").parse()?, None, properties)?;
	}
	let mut transaction = store.begin();
	let graph = transaction.create_graph("graph:mode1-probe".parse()?, graph.freeze().into())?;
	transaction.define_table(TableSchema::new("item", graph, TableKind::Normal, [
		TableField::new("payload", Type::Int),
	])?)?;
	transaction.commit()?;
	Ok(store)
}

/// Executes one full scan and returns its owned row count.
fn query_row_count(store: &Datastore) -> Result<usize, Box<dyn Error>> {
	let results = store.execute("SELECT VALUE payload FROM item", &Object::new())?;
	let result = results.first().ok_or_else(|| io::Error::other("query returned no result"))?;
	let outcome = result.result.as_ref().map_err(|error| io::Error::other(error.to_string()))?;
	let Value::Array(rows) = outcome.value() else {
		return Err(io::Error::other("query did not return an array").into());
	};
	Ok(rows.len())
}

/// Runs the process-isolated benchmark selected by Cargo dependency features.
fn main() -> Result<(), Box<dyn Error>> {
	let row_count =
		std::env::var("FG_MODE1_ROWS").unwrap_or_else(|_| "4096".to_owned()).parse::<usize>()?;
	let repetitions = std::env::var("FG_MODE1_REPETITIONS")
		.unwrap_or_else(|_| "200".to_owned())
		.parse::<usize>()?;
	if row_count == 0 || repetitions == 0 {
		return Err(io::Error::other("Mode 1 rows and repetitions must be positive").into());
	}
	let store = build_store(row_count)?;
	for _ in 0..3 {
		if query_row_count(&store)? != row_count {
			return Err(io::Error::other("Mode 1 warm-up returned a partial table").into());
		}
	}
	let started = Instant::now();
	let mut observed_rows = 0_usize;
	for _ in 0..repetitions {
		observed_rows = observed_rows
			.checked_add(black_box(query_row_count(&store)?))
			.ok_or_else(|| io::Error::other("Mode 1 observed-row count overflow"))?;
	}
	let elapsed = started.elapsed();
	let expected_rows = row_count
		.checked_mul(repetitions)
		.ok_or_else(|| io::Error::other("Mode 1 expected-row count overflow"))?;
	if observed_rows != expected_rows {
		return Err(io::Error::other("Mode 1 measured queries returned partial tables").into());
	}
	println!(
		"mode1_query rows={row_count} repetitions={repetitions} elapsed_ns={} \
		 queries_per_second={:.3} rows_per_second={:.3}",
		elapsed.as_nanos(),
		repetitions as f64 / elapsed.as_secs_f64(),
		observed_rows as f64 / elapsed.as_secs_f64(),
	);
	Ok(())
}
