use std::{
	fs,
	num::NonZeroUsize,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use super::{
	IMPORT_FAILURE_POINT,
	ImportFailurePoint,
	cleanup::inject_cleanup_failures,
	open::{StartupTimings, last_startup_timings},
};
use crate::{
	datastore::{
		CheckpointMode,
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassSchema,
		Datastore,
		ImportOptions,
		ImportViolationPolicy,
		IndexScope,
		LocalOptions,
	},
	gql::QueryExecutionOptions,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId, RecordIdPart, Type, Value},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
	fn new() -> Self {
		Self(
			std::env::temp_dir()
				.join(format!("fgdb-{:016x}", NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed))),
		)
	}
}

impl Drop for TestDirectory {
	fn drop(&mut self) {
		drop(fs::remove_dir_all(&self.0));
	}
}

#[test]
fn durable_parameter_survives_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let defined = store
		.execute("DEFINE PARAM $region VALUE 'eu' COMMENT 'routing';", &Object::new())
		.unwrap();
	assert!(defined[0].result.is_ok());
	let identity = store.schema_catalog().parameter("region").unwrap().id();
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let parameter = reopened.schema_catalog().parameter("region").unwrap();
	assert_eq!(parameter.id(), identity);
	assert_eq!(parameter.comment().map(|comment| comment.as_str()), Some("routing"));
	let result = reopened.execute("RETURN $region;", &Object::new()).unwrap();
	assert_eq!(result[0].result.as_ref().unwrap().value(), &Value::from("eu"));
}

#[test]
fn durable_function_survives_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let defined = store
		.execute(
			"DEFINE PURE FUNCTION fn::double(value: int) -> int { RETURN $value * 2; } COMMENT \
			 'reopen';",
			&Object::new(),
		)
		.unwrap();
	assert!(defined[0].result.is_ok());
	let identity = store.schema_catalog().functions().next().unwrap().id();
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let catalog = reopened.schema_catalog();
	let function = catalog.functions().next().unwrap();
	assert_eq!(function.id(), identity);
	assert_eq!(function.comment().map(|comment| comment.as_str()), Some("reopen"));
	let result = reopened.execute("RETURN fn::double(21);", &Object::new()).unwrap();
	assert_eq!(result[0].result.as_ref().unwrap().value(), &Value::from(42_i64));
}

/// Measures total and phase-level reopen scaling for the format-v1 registry.
#[test]
#[ignore = "diagnostic creates up to 1,800 graph-generation files"]
fn local_file_layout_reopen_diagnostic() {
	for graph_count in [1_usize, 100] {
		let directory = TestDirectory::new();
		let options = LocalOptions {
			buffer_pool_pages: 256,
			..LocalOptions::default()
		};
		let store = Datastore::create_local(&directory.0, options).unwrap();
		let mut transaction = store.begin();
		for ordinal in 0..graph_count {
			transaction.define_graph(&format!("reopen-{ordinal}"), GraphKind::Graph).unwrap();
		}
		transaction.commit().unwrap();
		drop(store);

		let mut totals = Vec::with_capacity(5);
		let mut phases = Vec::with_capacity(5);
		let mut handle_counts = (0, 0);
		for _ in 0..5 {
			let started = Instant::now();
			let reopened = Datastore::open_local(&directory.0, options).unwrap();
			totals.push(started.elapsed());
			phases.push(last_startup_timings());
			assert_eq!(reopened.schema_catalog().graphs().count(), graph_count);
			let status = reopened.local_storage_status().unwrap();
			handle_counts = (status.registered_files(), status.open_file_handles());
			assert!(handle_counts.1 < handle_counts.0);
			drop(reopened);
		}
		println!(
			"layout_reopen graphs={graph_count} total_ms={:.3} catalog_ms={:.3} registry_ms={:.3} \
			 materialization_ms={:.3} derived_ms={:.3} index_ms={:.3} statistics_ms={:.3} \
			 validation_ms={:.3} registered_files={} open_handles={}",
			median_duration(&mut totals).as_secs_f64() * 1_000.0,
			median_phase(&mut phases, |phase| phase.catalog),
			median_phase(&mut phases, |phase| phase.registry),
			median_phase(&mut phases, |phase| phase.materialization),
			median_phase(&mut phases, |phase| phase.derived),
			median_phase(&mut phases, |phase| phase.indexes),
			median_phase(&mut phases, |phase| phase.statistics),
			median_phase(&mut phases, |phase| phase.validation),
			handle_counts.0,
			handle_counts.1,
		);
	}
}

/// Measures local reopen scaling for one atom/index fixture selected through
/// `FG_REOPEN_ATOMS`, `FG_REOPEN_INDEXES`, and `FG_REOPEN_RUNS`.
#[test]
#[ignore = "diagnostic may build one million atoms and eight indexes"]
fn local_atom_index_reopen_diagnostic() -> crate::gql_status::FerrosGrynnResult<()> {
	let atom_count = diagnostic_usize("FG_REOPEN_ATOMS", 1_000)?;
	let index_count = diagnostic_usize("FG_REOPEN_INDEXES", 0)?;
	let runs = diagnostic_usize("FG_REOPEN_RUNS", 3)?;
	if atom_count == 0 || runs == 0 {
		return Err(crate::gql_status::FerrosGrynnError::invalid_argument(
			"reopen diagnostic atom and run counts must be positive",
		));
	}

	let directory = TestDirectory::new();
	let options = LocalOptions {
		buffer_pool_pages: 512,
		..LocalOptions::default()
	};
	let store = Datastore::create_local(&directory.0, options)?;
	let mut class =
		ClassSchema::new("probe", AtomKind::Node, [ClassField::new("value", Type::Int)])?;
	for ordinal in 0..index_count {
		class = class.with_index(ClassIndex::new(
			format!("by_value_{ordinal:02}"),
			ClassIndexKind::Standard,
			IndexScope::Base,
			["value"],
		)?)?;
	}
	let mut schema = store.begin();
	schema.define_class(class)?;
	schema.commit()?;
	let class = store
		.class("probe")
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::class_does_not_exist("probe"))?;
	let value_key = class
		.fields()
		.next()
		.and_then(|field| field.key)
		.ok_or_else(|| crate::gql_status::FerrosGrynnError::unknown_field("value"))?;
	let mut root = (*store.inner.current_root()).clone();
	for ordinal in 0..atom_count {
		let mut properties = PropertyMap::new();
		properties.insert(
			value_key,
			i64::try_from(ordinal % 4_096).map_err(|_error| {
				crate::gql_status::FerrosGrynnError::invalid_argument(
					"reopen diagnostic ordinal exceeds i64",
				)
			})?,
		);
		Arc::make_mut(&mut root.atoms).insert(
			&class,
			RecordId::new(
				"probe",
				RecordIdPart::Int(i64::try_from(ordinal).map_err(|_error| {
					crate::gql_status::FerrosGrynnError::invalid_argument(
						"reopen diagnostic atom count exceeds i64",
					)
				})?),
			)?,
			properties,
		)?;
	}
	root.rebuild_derived_state_at(&directory.0.join("fixture-scratch"))?;
	root.commit_ts = root.commit_ts.checked_add(1).ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("commit clock exhausted")
	})?;
	drop(store.inner.persist_import_root(&root, None)?);
	drop(store);

	let mut totals = Vec::with_capacity(runs);
	let mut phases = Vec::with_capacity(runs);
	for _ in 0..runs {
		let started = Instant::now();
		let reopened = Datastore::open_local(&directory.0, options)?;
		totals.push(started.elapsed());
		phases.push(last_startup_timings());
		assert_eq!(reopened.inner.current_root().atoms.atoms().len(), atom_count);
		drop(reopened);
	}
	println!(
		"atom_reopen atoms={atom_count} indexes={index_count} total_ms={:.3} catalog_ms={:.3} \
		 registry_ms={:.3} materialization_ms={:.3} derived_ms={:.3} index_ms={:.3} \
		 statistics_ms={:.3} validation_ms={:.3}",
		median_duration(&mut totals).as_secs_f64() * 1_000.0,
		median_phase(&mut phases, |phase| phase.catalog),
		median_phase(&mut phases, |phase| phase.registry),
		median_phase(&mut phases, |phase| phase.materialization),
		median_phase(&mut phases, |phase| phase.derived),
		median_phase(&mut phases, |phase| phase.indexes),
		median_phase(&mut phases, |phase| phase.statistics),
		median_phase(&mut phases, |phase| phase.validation),
	);
	Ok(())
}

fn diagnostic_usize(name: &str, default: usize) -> crate::gql_status::FerrosGrynnResult<usize> {
	match std::env::var(name) {
		Ok(value) => value.parse().map_err(|_error| {
			crate::gql_status::FerrosGrynnError::invalid_argument(format!(
				"{name} must contain an unsigned integer"
			))
		}),
		Err(std::env::VarError::NotPresent) => Ok(default),
		Err(std::env::VarError::NotUnicode(_)) => {
			Err(crate::gql_status::FerrosGrynnError::invalid_argument(format!(
				"{name} must contain Unicode digits"
			)))
		}
	}
}

fn median_duration(values: &mut [Duration]) -> Duration {
	values.sort_unstable();
	values[values.len() / 2]
}

fn median_phase(values: &mut [StartupTimings], select: impl Fn(StartupTimings) -> Duration) -> f64 {
	let mut durations = values.iter().copied().map(select).collect::<Vec<_>>();
	median_duration(&mut durations).as_secs_f64() * 1_000.0
}

#[test]
fn every_prepublication_failure_discards_root_and_unreachable_generation() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	schema.commit().unwrap();
	let generation_count = generation_directories(&directory.0);

	for point in [
		ImportFailurePoint::AfterBuild,
		ImportFailurePoint::AfterFileSync,
		ImportFailurePoint::AfterCatalogStage,
		ImportFailurePoint::BeforeWalCommit,
	] {
		IMPORT_FAILURE_POINT.set(point);
		let result = store.import(ImportOptions::default(), |session| {
			session.apply_item(|transaction| {
				transaction.create_atom("person:alice".parse()?, PropertyMap::new())
			})?;
			Ok(())
		});
		IMPORT_FAILURE_POINT.set(ImportFailurePoint::None);

		assert!(result.is_err());
		assert!(
			store.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none()
		);
		assert_eq!(generation_directories(&directory.0), generation_count);
	}
}

#[test]
fn quarantine_reopens_survives_cow_and_is_removed_by_catalog_publication()
-> crate::gql_status::FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options)?;
	let mut schema = store.begin();
	schema.define_class(ClassSchema::new("person", AtomKind::Node, [])?)?;
	schema.commit()?;
	let import = ImportOptions {
		on_violation: ImportViolationPolicy::Quarantine,
		reject_limit: Some(1),
		..ImportOptions::default()
	};
	let report = store.import(import, |session| {
		session.apply_item_with_payload(Value::from("accepted"), |transaction| {
			transaction.create_atom("person:alice".parse()?, PropertyMap::new())
		})?;
		drop(session.apply_item_with_payload(Value::from("duplicate"), |transaction| {
			transaction.create_atom("person:alice".parse()?, PropertyMap::new())
		})?);
		Ok(())
	})?;
	let id = report.quarantine().ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("missing quarantine identity")
	})?;
	drop(store);

	let store = Datastore::open_local(&directory.0, options)?;
	let mut reader = store.open_quarantine(id)?;
	let item = reader.read_next()?.ok_or_else(|| {
		crate::gql_status::FerrosGrynnError::invalid_argument("missing quarantined item")
	})?;
	assert_eq!(item.payload(), &Value::from("duplicate"));
	drop(reader);
	let mut mutation = store.begin();
	mutation.create_atom("person:bob".parse()?, PropertyMap::new())?;
	mutation.commit()?;
	assert!(store.open_quarantine(id)?.read_next()?.is_some());
	store.remove_quarantine(id)?;
	assert!(store.open_quarantine(id).is_err());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options)?;
	assert!(reopened.open_quarantine(id).is_err());
	Ok(())
}

#[test]
fn graph_property_catalog_update_reuses_graph_generation_and_reopens() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let define = store
		.execute("DEFINE GRAPH sales TYPE DAG CONTENT {owner: 'analytics'};", &Object::new())
		.unwrap();
	assert!(define[0].result.is_ok());
	let before = generation_directories(&directory.0);

	let update = store
		.execute(
			"UPDATE GRAPH sales SET properties.owner = 'risk', properties.address.city = 'Berlin';",
			&Object::new(),
		)
		.unwrap();
	assert!(update[0].result.is_ok());
	assert_eq!(generation_directories(&directory.0), before);
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let schema = reopened.schema_catalog().named_graph("sales").unwrap();
	let dictionary = reopened.property_dictionary();
	let owner = dictionary.get("owner").unwrap();
	let address = dictionary.get("address").unwrap();
	assert_eq!(schema.properties().get(owner), Some(&Value::from("risk")));
	let Some(Value::Object(address)) = schema.properties().get(address) else {
		panic!("nested graph property must reopen as an object");
	};
	assert_eq!(address.get("city"), Some(&Value::from("Berlin")));
	let selected = reopened
		.execute("SELECT ONLY VALUE properties.owner FROM GRAPH @sales;", &Object::new())
		.unwrap();
	assert_eq!(selected[0].result.as_ref().unwrap().value(), &Value::from("risk"));
}

#[test]
fn repeated_cow_commits_retire_unpinned_generations_at_runtime() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:seed".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();
	let expected = generation_directories(&directory.0);

	for index in 0..8 {
		let mut transaction = store.begin();
		transaction
			.create_atom(format!("person:{index}").parse().unwrap(), PropertyMap::new())
			.unwrap();
		transaction.commit().unwrap();
		assert_eq!(generation_directories(&directory.0), expected);
	}
}

#[test]
fn atom_and_graph_cow_churn_keeps_only_live_generations() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let graph = seed.define_graph("social", GraphKind::Graph).unwrap();
	let identity = "person:seed".parse::<RecordId>().unwrap();
	seed.create_atom(identity.clone(), PropertyMap::new()).unwrap();
	seed.attach_node(graph.id(), &identity, None, None).unwrap();
	seed.commit().unwrap();
	let expected = generation_directories(&directory.0);

	for index in 0..32 {
		let identity = format!("person:churn-{index}").parse::<RecordId>().unwrap();
		let mut transaction = store.begin();
		transaction.create_atom(identity.clone(), PropertyMap::new()).unwrap();
		transaction.attach_node(graph.id(), &identity, None, None).unwrap();
		transaction.commit().unwrap();
		assert_eq!(generation_directories(&directory.0), expected);
	}
	let status = store.local_storage_status().unwrap();
	assert_eq!(status.pending_retired_generation_directories(), 0);
	assert_eq!(status.generation_cleanup_failures(), 0);
}

#[test]
fn physical_snapshot_pins_replaced_generation_until_maintenance() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:seed".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();
	let expected = generation_directories(&directory.0);
	let snapshot = store
		.inner
		.capture_physical_query_snapshot(store.inner.current_root().commit_ts)
		.unwrap()
		.unwrap();

	let mut transaction = store.begin();
	transaction.create_atom("person:next".parse().unwrap(), PropertyMap::new()).unwrap();
	transaction.commit().unwrap();
	assert_eq!(generation_directories(&directory.0), expected + 1);

	drop(snapshot);
	store.checkpoint(CheckpointMode::Passive).unwrap();
	assert_eq!(generation_directories(&directory.0), expected);
}

#[test]
fn cleanup_failure_does_not_undo_commit_and_is_retried() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.create_atom("person:seed".parse().unwrap(), PropertyMap::new()).unwrap();
	seed.commit().unwrap();
	let expected = generation_directories(&directory.0);
	inject_cleanup_failures(1);

	let next = "person:next".parse::<RecordId>().unwrap();
	let mut transaction = store.begin();
	transaction.create_atom(next.clone(), PropertyMap::new()).unwrap();
	transaction.commit().unwrap();
	assert!(store.begin().atom(&next).unwrap().is_some());
	assert_eq!(generation_directories(&directory.0), expected + 1);
	let status = store.local_storage_status().unwrap();
	assert_eq!(status.generation_cleanup_failures(), 1);
	assert_eq!(status.pending_retired_generation_directories(), 1);
	assert_eq!(status.options(), LocalOptions::default());
	assert_eq!(status.resident_capacity_pages(), LocalOptions::default().buffer_pool_pages);
	assert!(status.fixed_metadata_bytes() > 0);

	store.checkpoint(CheckpointMode::Passive).unwrap();
	assert_eq!(generation_directories(&directory.0), expected);
	assert_eq!(store.local_storage_status().unwrap().pending_retired_generation_directories(), 0);
}

#[test]
fn local_traversal_uses_exact_physical_snapshot_and_falls_back_after_mutation() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("knows", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("physical_routes", GraphKind::Graph).unwrap();
	let alice = "person:alice".parse::<RecordId>().unwrap();
	let bob = "person:bob".parse::<RecordId>().unwrap();
	let carol = "person:carol".parse::<RecordId>().unwrap();
	let alice_bob = "knows:alice-bob".parse::<RecordId>().unwrap();
	let bob_carol = "knows:bob-carol".parse::<RecordId>().unwrap();
	for record in [&alice, &bob, &carol, &alice_bob, &bob_carol] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let alice_id = seed.attach_node(graph.id(), &alice, None, None).unwrap();
	let bob_id = seed.attach_node(graph.id(), &bob, None, None).unwrap();
	let carol_id = seed.attach_node(graph.id(), &carol, None, None).unwrap();
	seed.attach_edge(
		graph.id(),
		&alice_bob,
		None,
		EdgeTopology::directed([alice_id], [bob_id]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph.clone())));
	variables.insert("source", Value::Record(Box::new(alice.clone())));
	let traversal = "TRAVERSE FROM $source IN GRAPH $graph |> tv::out() |> tv::values('id');";
	let value =
		store.execute(traversal, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(value, Value::Array(vec![Value::Record(Box::new(bob.clone()))].into()));

	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
	let physical_pages = [
		"physical_resident_pages",
		"physical_wal_pages",
		"physical_sync_pages",
		"physical_ioring_pages",
	]
	.into_iter()
	.map(|field| {
		metrics.get(field).and_then(Value::as_number).and_then(|value| value.as_i64()).unwrap_or(0)
	})
	.sum::<i64>();
	assert!(physical_pages > 0, "the physical route must read or probe exact pages");

	let mut transaction = store.begin();
	transaction
		.attach_edge(
			graph.id(),
			&bob_carol,
			None,
			EdgeTopology::directed([bob_id], [carol_id]),
			None,
			None,
		)
		.unwrap();
	let explained = transaction
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out() |> \
			 tv::out();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(1_u64)));
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(0_u64)));
}

#[test]
fn local_physical_traversal_reads_adjacency_overflow() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("knows", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("overflow_routes", GraphKind::Graph).unwrap();
	let source = "person:source".parse::<RecordId>().unwrap();
	seed.create_atom(source.clone(), PropertyMap::new()).unwrap();
	let source_id = seed.attach_node(graph.id(), &source, None, None).unwrap();
	for index in 0..6 {
		let target = format!("person:target-{index}").parse::<RecordId>().unwrap();
		let edge = format!("knows:edge-{index}").parse::<RecordId>().unwrap();
		seed.create_atom(target.clone(), PropertyMap::new()).unwrap();
		seed.create_atom(edge.clone(), PropertyMap::new()).unwrap();
		let target_id = seed.attach_node(graph.id(), &target, None, None).unwrap();
		seed.attach_edge(
			graph.id(),
			&edge,
			None,
			EdgeTopology::directed([source_id], [target_id]),
			None,
			None,
		)
		.unwrap();
	}
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph)));
	variables.insert("source", Value::Record(Box::new(source)));
	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> tv::out();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("actual_rows"), Some(&Value::from(6_u64)));
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
}

#[test]
fn local_physical_traversal_resolves_node_and_edge_inner_graphs() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("module", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("portal", AtomKind::Edge, []).unwrap()).unwrap();
	let node_child = seed.define_graph("physical_node_child", GraphKind::Graph).unwrap();
	let edge_child = seed.define_graph("physical_edge_child", GraphKind::Graph).unwrap();
	let meta = seed.define_graph("physical_meta", GraphKind::Metagraph).unwrap();
	let owner = "module:owner".parse::<RecordId>().unwrap();
	let target = "module:target".parse::<RecordId>().unwrap();
	let portal = "portal:owner-target".parse::<RecordId>().unwrap();
	for record in [&owner, &target, &portal] {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let owner_id = seed.attach_node(meta.id(), &owner, None, Some(node_child.id())).unwrap();
	let target_id = seed.attach_node(meta.id(), &target, None, None).unwrap();
	seed.attach_edge(
		meta.id(),
		&portal,
		None,
		EdgeTopology::directed([owner_id], [target_id]),
		None,
		Some(edge_child.id()),
	)
	.unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(meta)));
	variables.insert("owner", Value::Record(Box::new(owner)));
	variables.insert("portal", Value::Record(Box::new(portal)));
	for (source, child) in [("owner", node_child.clone()), ("portal", edge_child)] {
		let query = format!("TRAVERSE FROM ${source} IN GRAPH $graph |> tv::meta_inner();");
		let value =
			store.execute(&query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
		assert_eq!(value, Value::Array(vec![Value::GraphRef(Box::new(child.clone()))].into()));
	}

	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $owner IN GRAPH $graph |> tv::meta_inner();",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
}

#[test]
fn local_physical_traversal_resolves_effective_properties() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let graph = seed.define_graph("physical_fields", GraphKind::Graph).unwrap();
	let name = seed.intern_property("name").unwrap();
	let hidden = seed.intern_property("hidden").unwrap();
	let source = "person:source".parse::<RecordId>().unwrap();
	let mut properties = PropertyMap::new();
	properties.insert(name, Value::from("base"));
	properties.insert(hidden, Value::from("remove"));
	seed.create_atom(source.clone(), properties).unwrap();
	seed.attach_node(graph.id(), &source, None, None).unwrap();
	seed.set_overlay(graph.id(), &source, name, Value::from("effective")).unwrap();
	seed.hide_property(graph.id(), &source, hidden).unwrap();
	seed.commit().unwrap();

	let mut variables = Object::new();
	variables.insert("graph", Value::GraphRef(Box::new(graph.clone())));
	variables.insert("source", Value::Record(Box::new(source.clone())));
	let query = "TRAVERSE FROM $source IN GRAPH $graph |> tv::values('name');";
	let value = store.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(value, Value::Array(vec![Value::from("effective")].into()));

	let explained = store
		.execute(
			"EXPLAIN ANALYZE FORMAT JSON TRAVERSE FROM $source IN GRAPH $graph |> \
			 tv::values('properties');",
			&variables,
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let Value::Object(explained) = explained else {
		panic!("FORMAT JSON must return an object");
	};
	let Some(Value::Object(metrics)) = explained.get("analyze") else {
		panic!("ANALYZE metrics are missing");
	};
	assert_eq!(metrics.get("physical_windows"), Some(&Value::from(1_u64)));
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let value =
		reopened.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(value, Value::Array(vec![Value::from("effective")].into()));
}

#[test]
fn relational_select_spill_matches_after_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE CLASS department TYPE NODE SCHEMALESS; DEFINE CLASS person TYPE NODE \
			 SCHEMALESS; CREATE department:eng SET code = 'eng', name = 'Engineering'; CREATE \
			 department:risk SET code = 'risk', name = 'Risk'; CREATE person:alice SET department \
			 = 'eng', score = 3; CREATE person:bob SET department = 'risk', score = 2; CREATE \
			 person:carol SET department = 'eng', score = 1;",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let query = "SELECT d.properties.name AS department, count() AS people, \
	             sum(p.properties.score) AS total FROM person AS p LEFT JOIN department AS d ON \
	             p.properties.department = d.properties.code GROUP BY d.properties.name ORDER BY \
	             total DESC TEMPFILES;";
	let query_options = QueryExecutionOptions {
		barrier_memory_bytes: NonZeroUsize::new(64).unwrap_or(NonZeroUsize::MIN),
		..QueryExecutionOptions::default()
	};
	let before = store
		.execute_with_options(query, &Object::new(), query_options.clone())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let after = reopened
		.execute_with_options(query, &Object::new(), query_options)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(after, before);
}

fn generation_directories(root: &Path) -> usize {
	let mut count = 0;
	for family in ["atoms", "graphs"] {
		let family = root.join(family);
		let Ok(entries) = fs::read_dir(&family) else {
			continue;
		};
		for entry in entries.flatten() {
			let path = entry.path();
			if !path.is_dir() {
				continue;
			}
			if family.ends_with("atoms") {
				count += 1;
			} else if let Ok(generations) = fs::read_dir(path) {
				count += generations.flatten().filter(|entry| entry.path().is_dir()).count();
			}
		}
	}
	count
}
