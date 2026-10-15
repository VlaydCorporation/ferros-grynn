//! Process-isolated Mode 1 query-throughput probe.

use std::{error::Error, hint::black_box, io, time::Instant};

use ferros_grynn::{
	datastore::{ClassField, ClassSchema, Datastore},
	graph::{AtomKind, GraphKind},
	values::{Object, PropertyMap, RecordId, Type, Value},
};

/// Builds one class of graph-attached nodes before either measured variant
/// runs.
fn build_store(row_count: usize) -> Result<Datastore, Box<dyn Error>> {
	let store = Datastore::in_memory();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("item", AtomKind::Node, [ClassField::new(
		"payload",
		Type::Int,
	)])?)?;
	let graph = schema.define_graph("mode1-probe", GraphKind::Graph)?;
	schema.commit()?;
	let payload = store
		.property_dictionary()
		.get("payload")
		.ok_or_else(|| io::Error::other("published class is missing the payload property"))?;
	let mut transaction = store.begin();
	for row in 0..row_count {
		let mut properties = PropertyMap::new();
		properties.insert(payload, i64::try_from(row)?);
		let record = format!("item:{row}").parse::<RecordId>()?;
		transaction.create_atom(record.clone(), properties)?;
		transaction.attach_node(graph.id(), &record, None, None)?;
	}
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

/// Assertions shared with the production crate's unit-test harness.
#[cfg(test)]
#[path = "../../../../tests/common/assertions.rs"]
pub mod test_assertions;

#[cfg(test)]
/// Regression tests for mode1 query probe behavior.
mod tests {
	use ferros_grynn::{
		graph::GraphRead,
		values::{Object, Value},
	};

	use super::{build_store, query_row_count};
	use crate::test_assertions::{AssertOption as _, AssertResult as _};

	/// Verifies the probe's class rows, payloads and graph membership agree.
	#[test]
	fn fixture_uses_current_class_and_graph_api() {
		let store = build_store(3).assert_ok();
		assert_eq!(query_row_count(&store).assert_ok(), 3);
		let results = store
			.execute("SELECT VALUE payload FROM item ORDER BY payload", &Object::new())
			.assert_ok();
		let outcome = results.first().assert_some().result.as_ref().assert_ok();
		let rows = outcome.value().as_array().assert_some();
		assert_eq!(rows.as_slice(), &[Value::from(0), Value::from(1), Value::from(2)]);
		let mut transaction = store.begin();
		let graph = transaction.named_graph("mode1-probe").assert_ok().assert_some();
		let view = graph.read(&transaction).assert_ok();
		assert_eq!(view.node_count().assert_ok(), 3);
		assert_eq!(view.edge_count().assert_ok(), 0);
	}
}
