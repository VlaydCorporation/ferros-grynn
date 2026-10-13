use std::{
	fs,
	io::{Read, Seek, SeekFrom, Write},
	num::NonZeroUsize,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicU64, Ordering},
	},
	time::{Duration, Instant},
};

use super::{
	GENERATION_FAILURE_POINT,
	GenerationFailurePoint,
	cleanup::inject_cleanup_failures,
	open::{StartupTimings, last_startup_timings},
};
use crate::{
	datastore::{
		CheckpointMode,
		ClassField,
		ClassIndex,
		ClassIndexKind,
		ClassIndexState,
		ClassSchema,
		Datastore,
		ImportOptions,
		ImportViolationPolicy,
		IndexScope,
		LocalOptions,
		MaintenanceJobKind,
		MaintenanceJobState,
		StoreBackend,
	},
	gql::QueryExecutionOptions,
	graph::{AtomKind, EdgeTopology, GraphKind},
	values::{Object, PropertyMap, RecordId, RecordIdPart, Type, Value, Vector},
};

mod changefeed_tests;
mod event_outbox_tests;
mod live_history_tests;
mod qualified_incidence_tests;
mod search_scope_tests;

#[test]
fn edge_endpoint_physical_hyperwalk_pins_old_generation_and_reopens() {
	use crate::graph::{EdgeEndpoint, TraversalDirection, TraversalIndex, TraversalSemantics};
	let directory = TestDirectory::new();
	let mut options = LocalOptions::default();
	options.buffer_pool_pages = 16;
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let mut seed = store.begin();
	seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	seed.define_class(ClassSchema::new("relation", AtomKind::Edge, []).unwrap()).unwrap();
	let graph = seed.define_graph("endpoints", GraphKind::Graph).unwrap();
	let mut nodes = Vec::new();
	for index in 0..8 {
		let record = format!("person:n{index}").parse::<RecordId>().unwrap();
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
		nodes.push(seed.attach_node(graph.id(), &record, None, None).unwrap());
	}
	let records = ["relation:base", "relation:hyper", "relation:both", "relation:undirected"]
		.map(|value| value.parse::<RecordId>().unwrap());
	for record in &records {
		seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
	}
	let endpoint = seed
		.attach_edge(
			graph.id(),
			&records[0],
			None,
			EdgeTopology::directed([nodes[0]], [nodes[1]]),
			None,
			None,
		)
		.unwrap();
	seed.attach_edge(
		graph.id(),
		&records[1],
		None,
		EdgeTopology::directed([endpoint, nodes[2]], nodes[3..].iter().copied().chain([endpoint])),
		None,
		None,
	)
	.unwrap();
	seed.attach_edge(
		graph.id(),
		&records[2],
		None,
		EdgeTopology::bidirectional([endpoint], [nodes[2]]),
		None,
		None,
	)
	.unwrap();
	seed.attach_edge(
		graph.id(),
		&records[3],
		None,
		EdgeTopology::undirected([endpoint, nodes[0], nodes[1]]),
		None,
		None,
	)
	.unwrap();
	seed.commit().unwrap();
	let mut old = store.begin();
	let capability = old.graphs().unwrap()[0].edge_endpoint_index().unwrap();
	assert!(store.checkpoint(CheckpointMode::Passive).unwrap());
	let snapshot = old.query_snapshot().unwrap();
	let exact =
		TraversalIndex::new(&old.query_graph(&snapshot, &graph).unwrap(), TraversalSemantics::Atom)
			.unwrap();
	for (forward, backward, direction) in [
		(true, false, TraversalDirection::Forward),
		(false, true, TraversalDirection::Backward),
		(true, true, TraversalDirection::Both),
	] {
		let physical = old
			.query_physical_traversal(&snapshot, &graph, endpoint, forward, backward, true)
			.unwrap()
			.unwrap();
		let mut expected = exact
			.steps(&EdgeEndpoint::Local(endpoint), direction)
			.map(|step| {
				(
					step.target().local_id().unwrap(),
					step.via_edge(),
					step.direction(),
					step.role(),
					step.topology(),
				)
			})
			.collect::<Vec<_>>();
		let mut actual = physical
			.candidates
			.iter()
			.map(|step| (step.target, step.edge, step.direction, step.role, step.topology))
			.collect::<Vec<_>>();
		expected.sort();
		expected.dedup();
		actual.sort();
		assert_eq!(actual, expected);
	}
	let variables = Object::from_iter([
		("graph", Value::GraphRef(Box::new(graph.clone()))),
		("source", Value::Record(Box::new(records[0].clone()))),
	]);
	let query = "TRAVERSE ATOM FROM $source IN GRAPH $graph |> tv::out() |> tv::values('id');";
	let before = store.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(before.as_array().unwrap().len(), 9);
	let mut remove = store.begin();
	remove.detach_occurrence(graph.id(), &records[1], false).unwrap();
	remove.commit().unwrap();
	assert_eq!(
		old.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	let current =
		store.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(current.as_array().unwrap().len(), 3);
	for result in store
		.execute(
			"INFO FOR INDEX edge_endpoint ON GRAPH endpoints; REBUILD INDEX edge_endpoint ON \
			 GRAPH endpoints;",
			&Object::new(),
		)
		.unwrap()
	{
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	drop(snapshot);
	drop(old);
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert_eq!(reopened.begin().graphs().unwrap()[0].edge_endpoint_index(), Some(capability));
	assert_eq!(
		reopened.execute(query, &variables).unwrap().remove(0).result.unwrap().into_parts().0,
		current
	);
}

#[test]
fn geometry_physical_delta_matches_scan_and_survives_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE CLASS place TYPE NODE SCHEMAFULL; DEFINE FIELD shape ON CLASS place TYPE \
			 geometry; CREATE place:a SET shape = <geometry>{type: 'Point', coordinates: [1, 1]}; \
			 CREATE place:b SET shape = <geometry>{type: 'Point', coordinates: [8, 8]}; CREATE \
			 place:c SET shape = <geometry>{type: 'Point', coordinates: [2, 2]}; DEFINE INDEX \
			 by_shape ON CLASS place FIELDS shape GEOMETRY SCOPE BASE;",
			&Object::new(),
		)
		.unwrap();
	for result in setup {
		assert!(result.result.is_ok(), "{:?}", result.result);
	}
	let query = Object::from_iter([(
		"area",
		Value::Geometry(
			"{\"type\":\"Polygon\",\"coordinates\":[[[0,0],[5,0],[5,5],[0,5],[0,0]]]}"
				.parse::<crate::values::Geometry>()
				.unwrap(),
		),
	)]);
	let indexed =
		"SELECT VALUE id FROM place WITH INDEX by_shape WHERE shape INSIDE $area ORDER BY id;";
	let scan = "SELECT VALUE id FROM place WITH NOINDEX WHERE shape INSIDE $area ORDER BY id;";
	let value = |store: &Datastore, text: &str| {
		store.execute(text, &query).unwrap().remove(0).result.unwrap().into_parts().0
	};
	assert_eq!(value(&store, indexed), value(&store, scan));
	let base = physical_index_files(&store, "by_shape").0;
	let mut old = store.begin();
	let before = value(&store, scan);
	store
		.execute(
			"UPDATE place:b SET shape = <geometry>{type: 'Point', coordinates: [3, 3]};",
			&Object::new(),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap();
	assert_eq!(physical_index_files(&store, "by_shape").0, base);
	assert!(physical_index_files(&store, "by_shape").1.is_some());
	assert_eq!(value(&store, indexed), value(&store, scan));
	assert_eq!(
		old.execute(indexed, &query).unwrap().remove(0).result.unwrap().into_parts().0,
		before
	);
	drop(old);
	store.execute("DELETE place:a;", &Object::new()).unwrap().remove(0).result.unwrap();
	assert_eq!(value(&store, indexed), value(&store, scan));
	let compacted = physical_index_files(&store, "by_shape").0;
	store.execute("DELETE place:b;", &Object::new()).unwrap().remove(0).result.unwrap();
	assert_eq!(physical_index_files(&store, "by_shape").0, compacted);
	assert!(physical_index_files(&store, "by_shape").1.is_some());
	assert_eq!(value(&store, indexed), value(&store, scan));
	let rejected = store
		.execute(
			"SELECT VALUE id FROM place WITH INDEX by_shape WHERE shape OUTSIDE $area;",
			&query,
		)
		.unwrap();
	assert!(rejected[0].result.is_err());
	let expected = value(&store, scan);
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert_eq!(value(&reopened, indexed), expected);
}

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

struct GenerationFailureGuard;

impl GenerationFailureGuard {
	fn set(point: GenerationFailurePoint) -> Self {
		GENERATION_FAILURE_POINT.set(point);
		Self
	}
}

impl Drop for GenerationFailureGuard {
	fn drop(&mut self) {
		GENERATION_FAILURE_POINT.set(GenerationFailurePoint::None);
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
	drop(store.inner.persist_import_root(&mut root, None)?);
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
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		let failure = GenerationFailureGuard::set(point);
		let result = store.import(ImportOptions::default(), |session| {
			session.apply_item(|transaction| {
				transaction.create_atom("person:alice".parse()?, PropertyMap::new())
			})?;
			Ok(())
		});
		drop(failure);

		assert!(result.is_err());
		assert!(
			store.begin().atom(&"person:alice".parse::<RecordId>().unwrap()).unwrap().is_none()
		);
		assert_eq!(generation_directories(&directory.0), generation_count);
	}
}

#[test]
fn streamed_search_index_failures_discard_unreachable_generation()
-> crate::gql_status::FerrosGrynnResult<()> {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
			 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD \
			 embedding ON CLASS document TYPE vector<f32>; DEFINE FIELD shape ON CLASS document \
			 TYPE geometry; CREATE document:a SET title = 'Rust graph', embedding = $embedding, \
			 shape = <geometry>{type:'Point', coordinates:[1,1]};",
			&Object::from_iter([("embedding", Value::Vector(Vector::F32(vec![1.0, 0.0])))]),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));
	let generations =
		|| -> crate::gql_status::FerrosGrynnResult<std::collections::BTreeSet<PathBuf>> {
			let io = |error| {
				crate::gql_status::FerrosGrynnError::invalid_storage_file(
					"test generation scan failed",
				)
				.with_technical_source(error)
			};
			let mut result = std::collections::BTreeSet::new();
			for family in ["atoms", "graphs"] {
				let entries = match fs::read_dir(directory.0.join(family)) {
					Ok(entries) => entries,
					Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
					Err(error) => return Err(io(error)),
				};
				for entry in entries {
					let entry = entry.map_err(io)?;
					if !entry.file_type().map_err(io)?.is_dir() {
						continue;
					}
					if family == "atoms" {
						result.insert(entry.path());
					} else {
						for generation in fs::read_dir(entry.path()).map_err(io)? {
							let generation = generation.map_err(io)?;
							if generation.file_type().map_err(io)?.is_dir() {
								result.insert(generation.path());
							}
						}
					}
				}
			}
			Ok(result)
		};
	let baseline_generations = generations()?;

	let definitions = [
		(
			"shape_geometry",
			"DEFINE INDEX shape_geometry ON CLASS document FIELDS shape GEOMETRY SCOPE BASE;",
		),
		(
			"title_search",
			"DEFINE INDEX title_search ON CLASS document FIELDS title FULLTEXT ANALYZER words \
			 BM25(1.2, 0.75) HIGHLIGHTS SCOPE BASE;",
		),
		(
			"embedding_hnsw",
			"DEFINE INDEX embedding_hnsw ON CLASS document FIELDS embedding HNSW DIMENSION 2 TYPE \
			 F32 DIST EUCLIDEAN EFC 16 M 4 SCOPE BASE;",
		),
		(
			"embedding_disk",
			"DEFINE INDEX embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION 2 \
			 TYPE F32 DIST EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE BASE;",
		),
	];
	for point in [
		GenerationFailurePoint::AfterBuild,
		GenerationFailurePoint::AfterFileSync,
		GenerationFailurePoint::AfterCatalogStage,
		GenerationFailurePoint::BeforeWalCommit,
	] {
		for (name, definition) in definitions {
			let failure = GenerationFailureGuard::set(point);
			let define = store.execute(definition, &Object::new()).unwrap();
			drop(failure);

			assert!(define[0].result.is_err());
			assert!(store.class("document").unwrap().indexes().all(|index| index.name() != name));
			// Cleanup may retire an older unpinned baseline generation; no
			// failed build may leave a new UUID directory behind.
			let remaining = generations()?;
			assert!(
				remaining.is_subset(&baseline_generations),
				"failed {name} build leaked generations: {remaining:?}; baseline: \
				 {baseline_generations:?}"
			);
			assert!(
				store.begin().atom(&"document:a".parse::<RecordId>().unwrap()).unwrap().is_some()
			);
		}
	}
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	assert!(reopened.class("document").unwrap().indexes().next().is_none());
	assert!(reopened.begin().atom(&"document:a".parse::<RecordId>().unwrap()).unwrap().is_some());
	Ok(())
}

#[test]
fn derived_graph_artifact_publication_failures_reuse_and_corrupt_reopen() {
	use crate::datastore::{
		GraphIndex,
		GraphIndexCost,
		GraphIndexDefinition,
		NeighbourhoodAlgorithm,
		PathIndexAlgorithm,
		ReachabilityAlgorithm,
		TensorCacheFormat,
	};
	for definition in [
		GraphIndexDefinition::Tensor {
			scope: crate::graph::tensor::TensorScope::Local,
			format: TensorCacheFormat::Csr,
		},
		GraphIndexDefinition::Tensor {
			scope: crate::graph::tensor::TensorScope::Rooted,
			format: TensorCacheFormat::Dense,
		},
		GraphIndexDefinition::Tensor {
			scope: crate::graph::tensor::TensorScope::Local,
			format: TensorCacheFormat::Auto,
		},
		GraphIndexDefinition::Path {
			algorithm: PathIndexAlgorithm::LandmarkSssp(2),
			cost: GraphIndexCost::Hop,
			patterns: Vec::new(),
		},
		GraphIndexDefinition::Path {
			algorithm: PathIndexAlgorithm::LandmarkSssp(2),
			cost: GraphIndexCost::Weighted,
			patterns: Vec::new(),
		},
		GraphIndexDefinition::Reachability {
			algorithm: ReachabilityAlgorithm::Grail(2),
		},
		GraphIndexDefinition::Neighbourhood {
			radius: 4,
			algorithm: NeighbourhoodAlgorithm::Exact,
		},
		GraphIndexDefinition::Neighbourhood {
			radius: 4,
			algorithm: NeighbourhoodAlgorithm::Sketch(12),
		},
		GraphIndexDefinition::Neighbourhood {
			radius: 4,
			algorithm: NeighbourhoodAlgorithm::Landmark(2),
		},
	] {
		let directory = TestDirectory::new();
		let options = LocalOptions::default();
		let store = Datastore::create_local(&directory.0, options).unwrap();
		let mut seed = store.begin();
		seed.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
		seed.define_class(ClassSchema::new("link", AtomKind::Edge, []).unwrap()).unwrap();
		let graph = seed.define_graph("network", GraphKind::Graph).unwrap();
		let records =
			["person:a", "person:b", "link:ab"].map(|record| record.parse::<RecordId>().unwrap());
		for record in &records {
			seed.create_atom(record.clone(), PropertyMap::new()).unwrap();
		}
		let a = seed.attach_node(graph.id(), &records[0], None, None).unwrap();
		let b = seed.attach_node(graph.id(), &records[1], None, None).unwrap();
		seed.attach_edge(
			graph.id(),
			&records[2],
			None,
			EdgeTopology::directed([a], [b]),
			None,
			None,
		)
		.unwrap();
		let index = seed
			.define_graph_index(
				"network",
				GraphIndex::new("reach", definition, Some(1 << 20)).unwrap(),
			)
			.unwrap();
		seed.commit().unwrap();
		let id = index;
		let rebuild = "REBUILD INDEX reach ON GRAPH network;";
		let generation_count = generation_directories(&directory.0);
		for point in [
			GenerationFailurePoint::AfterBuild,
			GenerationFailurePoint::AfterFileSync,
			GenerationFailurePoint::AfterCatalogStage,
			GenerationFailurePoint::BeforeWalCommit,
		] {
			let failure = GenerationFailureGuard::set(point);
			let result = store.execute(rebuild, &Object::new()).unwrap();
			drop(failure);
			assert!(result[0].result.is_err());
			assert_eq!(
				store.begin().graphs().unwrap()[0].indexes().next().unwrap().state(),
				ClassIndexState::Stale
			);
			assert!(!store.inner.current_root().reachability_artifacts.contains_key(&id));
			assert!(!store.inner.current_root().neighbourhood_artifacts.contains_key(&id));
			assert!(!store.inner.current_root().path_artifacts.contains_key(&id));
			assert!(!store.inner.current_root().tensor_artifacts.contains_key(&id));
			assert_eq!(generation_directories(&directory.0), generation_count);
		}
		assert!(store.execute(rebuild, &Object::new()).unwrap()[0].result.is_ok());
		let physical = |store: &Datastore| {
			let StoreBackend::Local(local) = &store.inner.backend else {
				panic!("local backend required")
			};
			let state = local.state.lock();
			let binding = state
				._database_catalog
				.graphs
				.iter()
				.find(|entry| entry.id == graph.id())
				.unwrap()
				.index_bindings[&id];
			let path = state
				.file_catalog
				.files
				.iter()
				.find(|entry| entry.file_id == binding.file_id)
				.unwrap()
				.path
				.as_str()
				.to_owned();
			(binding, path)
		};
		let before = physical(&store);
		let mut unrelated = store.begin();
		unrelated.create_atom("person:unrelated".parse().unwrap(), PropertyMap::new()).unwrap();
		unrelated.commit().unwrap();
		assert_eq!(physical(&store), before);
		let failure = GenerationFailureGuard::set(GenerationFailurePoint::BeforeWalCommit);
		assert!(store.execute(rebuild, &Object::new()).unwrap()[0].result.is_err());
		drop(failure);
		assert_eq!(physical(&store), before);
		assert_eq!(
			store.begin().graphs().unwrap()[0].indexes().next().unwrap().state(),
			ClassIndexState::Ready
		);
		drop(store);
		let reopened = Datastore::open_local(&directory.0, options).unwrap();
		assert_eq!(physical(&reopened), before);
		assert!(
			reopened.inner.current_root().reachability_artifacts.contains_key(&id)
				|| reopened.inner.current_root().neighbourhood_artifacts.contains_key(&id)
				|| reopened.inner.current_root().path_artifacts.contains_key(&id)
				|| reopened.inner.current_root().tensor_artifacts.contains_key(&id)
		);
		drop(reopened);
		corrupt_page_checksum(&directory.0.join(&before.1));
		let reopened = Datastore::open_local(&directory.0, options).unwrap();
		assert_eq!(
			reopened.begin().graphs().unwrap()[0].indexes().next().unwrap().state(),
			ClassIndexState::Stale
		);
		assert!(!reopened.inner.current_root().reachability_artifacts.contains_key(&id));
		assert!(!reopened.inner.current_root().neighbourhood_artifacts.contains_key(&id));
		assert!(!reopened.inner.current_root().path_artifacts.contains_key(&id));
		assert!(!reopened.inner.current_root().tensor_artifacts.contains_key(&id));
		assert!(reopened.begin().atom(&records[0]).unwrap().is_some());
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
	let StoreBackend::Local(local) = &store.inner.backend else {
		panic!("test requires local storage");
	};
	let graph_generations = local
		.state
		.lock()
		.bindings
		.graphs
		.iter()
		.map(|binding| (binding.graph_id, binding.generation))
		.collect::<Vec<_>>();

	let update = store
		.execute(
			"UPDATE GRAPH sales SET properties.owner = 'risk', properties.address.city = 'Berlin';",
			&Object::new(),
		)
		.unwrap();
	assert!(update[0].result.is_ok());
	assert_eq!(
		local
			.state
			.lock()
			.bindings
			.graphs
			.iter()
			.map(|binding| (binding.graph_id, binding.generation))
			.collect::<Vec<_>>(),
		graph_generations,
	);
	// Catalog publication may retire older unpinned atom generations.
	assert!(generation_directories(&directory.0) <= before);
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
fn fulltext_physical_results_match_scan_after_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS nfkc, lowercase, ngram(2, 3); DEFINE \
			 CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE \
			 string; DEFINE INDEX title_search ON CLASS document FIELDS title FULLTEXT ANALYZER \
			 words BM25(1.2, 0.75) HIGHLIGHTS SCOPE BASE; CREATE document:a SET title = 'Rust \
			 graph'; CREATE document:b SET title = 'Rust storage ﬃeld notes'; CREATE document:c \
			 SET title = 'Graph only';",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));
	let automatic = "SELECT id, search::score(7) AS score, search::highlight('[', ']', 7) AS \
	                 snippet FROM document WHERE title @7@ 'rust' ORDER BY id;";
	let scan = "SELECT id, search::score(7) AS score, search::highlight('[', ']', 7) AS snippet \
	            FROM document WITH NOINDEX WHERE title @7@ 'rust' ORDER BY id;";
	let expected =
		store.execute(scan, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0;
	let actual =
		store.execute(automatic, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(actual, expected);
	let unicode_automatic = "SELECT search::highlight('[', ']', 8) AS snippet FROM document WHERE \
	                         title @8@ 'ffi' ORDER BY id;";
	let unicode_scan = "SELECT search::highlight('[', ']', 8) AS snippet FROM document WITH \
	                    NOINDEX WHERE title @8@ 'ffi' ORDER BY id;";
	let unicode_expected = store
		.execute(unicode_scan, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(
		unicode_expected,
		Value::Array(
			vec![Value::Object(Object::from_iter([(
				"snippet",
				Value::from("Rust storage [ﬃeld] notes"),
			)]))]
			.into(),
		),
	);
	let unicode_actual = store
		.execute(unicode_automatic, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(unicode_actual, unicode_expected);
	let (base_before, delta_before, _) = physical_index_files(&store, "title_search");
	assert!(delta_before.is_some());
	let updated =
		store.execute("UPDATE document:c SET title = 'Rust graph only';", &Object::new()).unwrap();
	assert!(updated[0].result.is_ok());
	let (base_after, delta_after, delta_path) = physical_index_files(&store, "title_search");
	assert_eq!(base_after, base_before);
	assert_ne!(delta_after, delta_before);
	assert!(delta_path.as_deref().is_some_and(|path| path.contains("/idx/ft_delta_")));
	let expected =
		store.execute(scan, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0;
	let actual =
		store.execute(automatic, &Object::new()).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(actual, expected);
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let after = reopened
		.execute(automatic, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(after, expected);
	let unicode_after = reopened
		.execute(unicode_automatic, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(unicode_after, unicode_expected);
}

#[test]
fn vector_physical_results_match_scan_after_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD embedding ON CLASS document \
			 TYPE vector<f32>; DEFINE INDEX embedding_hnsw ON CLASS document FIELDS embedding \
			 HNSW DIMENSION 2 TYPE F32 DIST COSINE EFC 16 M 4 SCOPE BASE; DEFINE INDEX \
			 embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION 2 TYPE F32 DIST \
			 EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 HASHED_VECTOR SCOPE BASE;",
			&Object::new(),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));
	for (name, vector) in [
		("a", vec![1.0_f32, 0.0]),
		("b", vec![0.0_f32, 1.0]),
		("c", vec![-1.0_f32, 0.0]),
		("d", vec![0.0_f32, 1.0]),
	] {
		let mut parameters = Object::new();
		parameters.insert("vector", Value::Vector(Vector::F32(vector)));
		let results = store
			.execute(&format!("CREATE document:{name} SET embedding = $vector;"), &parameters)
			.unwrap();
		assert!(results[0].result.is_ok());
	}
	let mut parameters = Object::new();
	parameters.insert("query", Value::Vector(Vector::F32(vec![1.0, 0.0])));
	let cases = [
		(
			"embedding_hnsw",
			"COSINE",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 2, COSINE |> $query;",
		),
		(
			"embedding_disk",
			"EUCLIDEAN",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 2, EUCLIDEAN |> $query;",
		),
	];
	let expected = cases
		.iter()
		.map(|(_, _, scan)| {
			store.execute(scan, &parameters).unwrap().remove(0).result.unwrap().into_parts().0
		})
		.collect::<Vec<_>>();
	for ((index, distance, _), expected) in cases.iter().zip(&expected) {
		let query = format!(
			"SELECT VALUE id FROM document WITH INDEX {index} WHERE embedding <| 2, {distance} |> \
			 $query;"
		);
		let actual =
			store.execute(&query, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
		assert_eq!(&actual, expected);
	}
	let hnsw_before = physical_index_files(&store, "embedding_hnsw");
	let disk_before = physical_index_files(&store, "embedding_disk");
	let mut update = Object::new();
	update.insert("vector", Value::Vector(Vector::F32(vec![0.5, 0.5])));
	let updated = store.execute("UPDATE document:a SET embedding = $vector;", &update).unwrap();
	assert!(updated[0].result.is_ok());
	let hnsw_after = physical_index_files(&store, "embedding_hnsw");
	let disk_after = physical_index_files(&store, "embedding_disk");
	for (before, after, marker) in [
		(hnsw_before, hnsw_after, "/idx/hnsw_delta_"),
		(disk_before, disk_after, "/idx/diskann_delta_"),
	] {
		assert_eq!(after.0, before.0);
		assert!(after.1.is_some());
		assert!(after.2.as_deref().is_some_and(|path| path.contains(marker)));
	}
	let expected = cases
		.iter()
		.map(|(_, _, scan)| {
			store.execute(scan, &parameters).unwrap().remove(0).result.unwrap().into_parts().0
		})
		.collect::<Vec<_>>();
	for ((index, distance, _), expected) in cases.iter().zip(&expected) {
		let query = format!(
			"SELECT VALUE id FROM document WITH INDEX {index} WHERE embedding <| 2, {distance} |> \
			 $query;"
		);
		let actual =
			store.execute(&query, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
		assert_eq!(&actual, expected);
	}
	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let execution_options = QueryExecutionOptions {
		deterministic_seed: 23,
		..QueryExecutionOptions::default()
	};
	let mut session = reopened.session();
	let begun = session
		.execute_with_options(
			"BEGIN ISOLATION LEVEL SNAPSHOT EXECUTION MODE DETERMINISTIC;",
			&Object::new(),
			execution_options.clone(),
		)
		.unwrap();
	assert!(begun[0].result.is_ok());
	for ((index, distance, _), expected) in cases.iter().zip(&expected) {
		let query = format!(
			"SELECT VALUE id FROM document WITH INDEX {index} WHERE embedding <| 2, {distance} |> \
			 $query;"
		);
		let explain = format!("EXPLAIN FULL FORMAT JSON {query}");
		let mut expected_plan_hash = None;
		for _run in 0..20 {
			let actual = session
				.execute_with_options(&query, &parameters, execution_options.clone())
				.unwrap()
				.remove(0)
				.result
				.unwrap()
				.into_parts()
				.0;
			assert_eq!(&actual, expected);
			let explained = session
				.execute_with_options(&explain, &parameters, execution_options.clone())
				.unwrap()
				.remove(0)
				.result
				.unwrap()
				.into_parts()
				.0;
			let Value::Object(explained) = explained else {
				panic!("vector EXPLAIN must return an object");
			};
			let plan_hash = explained.get("plan_hash").cloned();
			assert!(plan_hash.is_some());
			match &expected_plan_hash {
				None => expected_plan_hash = plan_hash,
				Some(expected) => assert_eq!(Some(expected), plan_hash.as_ref()),
			}
		}
	}
	let cancelled = session.execute("CANCEL;", &Object::new()).unwrap();
	assert!(cancelled[0].result.is_ok());
}

#[test]
fn defining_sibling_vector_indexes_reuses_existing_bases() {
	let directory = TestDirectory::new();
	let store = Datastore::create_local(&directory.0, LocalOptions::default()).unwrap();
	let setup = store
		.execute(
			"DEFINE CLASS document TYPE NODE SCHEMAFULL; DEFINE FIELD embedding ON CLASS document \
			 TYPE vector<f32>; CREATE document:a SET embedding = $a; CREATE document:b SET \
			 embedding = $b; CREATE document:c SET embedding = $c; DEFINE INDEX embedding_hnsw ON \
			 CLASS document FIELDS embedding HNSW DIMENSION 2 TYPE F32 DIST EUCLIDEAN EFC 16 M 4 \
			 SCOPE BASE;",
			&Object::from_iter([
				("a", Value::Vector(Vector::F32(vec![0.0, 0.0]))),
				("b", Value::Vector(Vector::F32(vec![1.0, 0.0]))),
				("c", Value::Vector(Vector::F32(vec![2.0, 0.0]))),
			]),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));
	let hnsw = physical_index_files(&store, "embedding_hnsw");

	let diskann = store
		.execute(
			"DEFINE INDEX embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION 2 \
			 TYPE F32 DIST EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE BASE;",
			&Object::new(),
		)
		.unwrap();
	assert!(diskann[0].result.is_ok());
	assert_eq!(physical_index_files(&store, "embedding_hnsw"), hnsw);
	let diskann = physical_index_files(&store, "embedding_disk");

	let hashed = store
		.execute(
			"DEFINE INDEX embedding_disk_hashed ON CLASS document FIELDS embedding DISKANN \
			 DIMENSION 2 TYPE F32 DIST EUCLIDEAN DEGREE 4 L_BUILD 8 ALPHA 1.2 HASHED_VECTOR SCOPE \
			 BASE;",
			&Object::new(),
		)
		.unwrap();
	assert!(hashed[0].result.is_ok());
	assert_eq!(physical_index_files(&store, "embedding_hnsw"), hnsw);
	assert_eq!(physical_index_files(&store, "embedding_disk"), diskann);
}

#[test]
fn deferred_search_indexes_rebuild_before_becoming_query_visible() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
			 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD \
			 embedding ON CLASS document TYPE vector<f32>; CREATE document:a SET title = 'old', \
			 embedding = $a; CREATE document:b SET title = 'other', embedding = $b; DEFINE INDEX \
			 title_search ON CLASS document FIELDS title FULLTEXT ANALYZER words HIGHLIGHTS SCOPE \
			 BASE DEFER; DEFINE INDEX embedding_hnsw ON CLASS document FIELDS embedding HNSW \
			 DIMENSION 2 TYPE F32 DIST COSINE EFC 16 M 4 SCOPE BASE DEFER; DEFINE INDEX \
			 embedding_disk ON CLASS document FIELDS embedding DISKANN DIMENSION 2 TYPE F32 DIST \
			 COSINE DEGREE 4 L_BUILD 8 ALPHA 1.2 SCOPE BASE DEFER;",
			&Object::from_iter([
				("a", Value::Vector(Vector::F32(vec![1.0, 0.0]))),
				("b", Value::Vector(Vector::F32(vec![0.0, 1.0]))),
			]),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));

	let parameters = Object::from_iter([
		("query", Value::Vector(Vector::F32(vec![0.5, 0.5]))),
		("updated", Value::Vector(Vector::F32(vec![0.5, 0.5]))),
	]);
	let mut transaction = store.begin();
	let update = transaction
		.execute("UPDATE document:a SET title = 'Rust updated', embedding = $updated;", &parameters)
		.unwrap();
	assert!(update[0].result.is_ok());
	for (automatic, scan) in [
		(
			"SELECT VALUE id FROM document WHERE title @@ 'rust' ORDER BY id;",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ 'rust' ORDER BY id;",
		),
		(
			"SELECT VALUE id FROM document WHERE embedding <| 1, COSINE |> $query;",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 1, COSINE |> $query;",
		),
	] {
		let automatic = transaction
			.execute(automatic, &parameters)
			.unwrap()
			.remove(0)
			.result
			.unwrap()
			.into_parts()
			.0;
		let scan = transaction
			.execute(scan, &parameters)
			.unwrap()
			.remove(0)
			.result
			.unwrap()
			.into_parts()
			.0;
		assert_eq!(automatic, scan);
	}
	transaction.commit().unwrap();

	let deadline = Instant::now() + Duration::from_secs(10);
	loop {
		let jobs = store
			.jobs()
			.unwrap()
			.into_iter()
			.filter(|job| job.kind() == MaintenanceJobKind::DeferredIndexCatchUp)
			.collect::<Vec<_>>();
		assert!(!jobs.iter().any(|job| job.state() == MaintenanceJobState::Failed));
		if jobs.len() == 3 && jobs.iter().all(|job| job.state() == MaintenanceJobState::Completed) {
			break;
		}
		assert!(Instant::now() < deadline, "deferred search-index rebuild timed out: {jobs:?}");
		std::thread::sleep(Duration::from_millis(1));
	}
	let class = store.schema_catalog().class("document").unwrap();
	assert!(class.indexes().all(|index| index.state() == ClassIndexState::Ready));

	let queries = [
		(
			"SELECT VALUE id FROM document WITH INDEX title_search WHERE title @@ 'rust' ORDER BY \
			 id;",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ 'rust' ORDER BY id;",
		),
		(
			"SELECT VALUE id FROM document WITH INDEX embedding_hnsw WHERE embedding <| 1, COSINE \
			 |> $query;",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 1, COSINE |> $query;",
		),
		(
			"SELECT VALUE id FROM document WITH INDEX embedding_disk WHERE embedding <| 1, COSINE \
			 |> $query;",
			"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 1, COSINE |> $query;",
		),
	];
	let expected = queries
		.iter()
		.map(|(_, scan)| {
			store.execute(scan, &parameters).unwrap().remove(0).result.unwrap().into_parts().0
		})
		.collect::<Vec<_>>();
	for ((forced, _), expected) in queries.iter().zip(&expected) {
		let actual =
			store.execute(forced, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
		assert_eq!(&actual, expected);
	}
	drop(store);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	for ((forced, _), expected) in queries.iter().zip(&expected) {
		let actual =
			reopened.execute(forced, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
		assert_eq!(&actual, expected);
	}
}

#[test]
fn corrupt_search_indexes_fall_back_automatically_but_fail_when_forced() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE ANALYZER words TOKENIZERS blank FILTERS lowercase; DEFINE CLASS document TYPE \
			 NODE SCHEMAFULL; DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD \
			 embedding ON CLASS document TYPE vector<f32>; CREATE document:a SET title = 'Rust \
			 graph', embedding = $a; CREATE document:b SET title = 'Other', embedding = $b; \
			 CREATE document:c SET title = 'Rust storage', embedding = $c; DEFINE INDEX \
			 title_search ON CLASS document FIELDS title FULLTEXT ANALYZER words HIGHLIGHTS SCOPE \
			 BASE; DEFINE INDEX embedding_hnsw ON CLASS document FIELDS embedding HNSW DIMENSION \
			 2 TYPE F32 DIST COSINE EFC 16 M 4 SCOPE BASE;",
			&Object::from_iter([
				("a", Value::Vector(Vector::F32(vec![1.0, 0.0]))),
				("b", Value::Vector(Vector::F32(vec![0.0, 1.0]))),
				("c", Value::Vector(Vector::F32(vec![-1.0, 0.0]))),
			]),
		)
		.unwrap();
	assert!(setup.iter().all(|result| result.result.is_ok()));
	let fulltext_scan =
		"SELECT VALUE id FROM document WITH NOINDEX WHERE title @@ 'rust' ORDER BY id;";
	let fulltext_auto = "SELECT VALUE id FROM document WHERE title @@ 'rust' ORDER BY id;";
	let fulltext_forced =
		"SELECT VALUE id FROM document WITH INDEX title_search WHERE title @@ 'rust' ORDER BY id;";
	let vector_scan =
		"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 2, COSINE |> $query;";
	let vector_auto = "SELECT VALUE id FROM document WHERE embedding <| 2, COSINE |> $query;";
	let vector_forced = "SELECT VALUE id FROM document WITH INDEX embedding_hnsw WHERE embedding \
	                     <| 2, COSINE |> $query;";
	let parameters = Object::from_iter([("query", Value::Vector(Vector::F32(vec![1.0, 0.0])))]);
	let expected_fulltext = store
		.execute(fulltext_scan, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let expected_vector =
		store.execute(vector_scan, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
	let fulltext_path = physical_index_base_path(&store, &directory.0, "title_search");
	let vector_path = physical_index_base_path(&store, &directory.0, "embedding_hnsw");
	drop(store);
	corrupt_page_checksum(&fulltext_path);
	corrupt_page_checksum(&vector_path);

	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let automatic_fulltext = reopened
		.execute(fulltext_auto, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let automatic_vector = reopened
		.execute(vector_auto, &parameters)
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(automatic_fulltext, expected_fulltext);
	assert_eq!(automatic_vector, expected_vector);
	assert!(reopened.execute(fulltext_forced, &Object::new()).unwrap().remove(0).result.is_err());
	assert!(reopened.execute(vector_forced, &parameters).unwrap().remove(0).result.is_err());
}

#[test]
fn udf_backed_physical_indexes_survive_local_reopen() {
	let directory = TestDirectory::new();
	let options = LocalOptions::default();
	let store = Datastore::create_local(&directory.0, options).unwrap();
	let setup = store
		.execute(
			"DEFINE PURE FUNCTION fn::whole(value: string) -> array<object> { RETURN [{term: \
			 string::upper($value), start: 0, end: string::len($value)}]; }; DEFINE PURE COST \
			 FUNCTION fn::l1(left: vector<f32>, right: vector<f32>) -> float { RETURN \
			 math::abs($left[0] - $right[0]) + math::abs($left[1] - $right[1]); }; DEFINE \
			 ANALYZER whole FUNCTION fn::whole; DEFINE CLASS document TYPE NODE SCHEMAFULL; \
			 DEFINE FIELD title ON CLASS document TYPE string; DEFINE FIELD embedding ON CLASS \
			 document TYPE vector<f32>; DEFINE INDEX title_search ON CLASS document FIELDS title \
			 FULLTEXT ANALYZER whole BM25(1.2, 0.75) HIGHLIGHTS SCOPE BASE; DEFINE INDEX \
			 custom_hnsw ON CLASS document FIELDS embedding HNSW DIMENSION 2 TYPE F32 DIST fn::l1 \
			 EFC 16 M 4 SCOPE BASE; CREATE document:a SET title = 'rust graph', embedding = $a; \
			 CREATE document:b SET title = 'other', embedding = $b; CREATE document:c SET title = \
			 'rust graph', embedding = $c;",
			&Object::from_iter([
				("a", Value::Vector(Vector::F32(vec![0.0, 0.0]))),
				("b", Value::Vector(Vector::F32(vec![2.0, 0.0]))),
				("c", Value::Vector(Vector::F32(vec![1.0, 0.0]))),
			]),
		)
		.unwrap();
	assert!(
		setup.iter().all(|result| result.result.is_ok()),
		"UDF-backed index setup failed: {:?}",
		setup.iter().filter_map(|result| result.result.as_ref().err()).collect::<Vec<_>>()
	);
	let fulltext_base = physical_index_files(&store, "title_search").0;
	let vector_base = physical_index_files(&store, "custom_hnsw").0;
	store
		.execute(
			"UPDATE document:c SET title = 'rust updated', embedding = $updated;",
			&Object::from_iter([("updated", Value::Vector(Vector::F32(vec![0.25, 0.0])))]),
		)
		.unwrap()
		.remove(0)
		.result
		.unwrap();
	let fulltext_files = physical_index_files(&store, "title_search");
	let vector_files = physical_index_files(&store, "custom_hnsw");
	assert_eq!(fulltext_files.0, fulltext_base);
	assert_eq!(vector_files.0, vector_base);
	assert!(fulltext_files.1.is_some());
	assert!(vector_files.1.is_some());

	let fulltext_scan = "SELECT VALUE search::highlight('[', ']', 9) FROM document WITH NOINDEX \
	                     WHERE title @9@ 'rust graph' ORDER BY id;";
	let fulltext_index = "SELECT VALUE search::highlight('[', ']', 9) FROM document WITH INDEX \
	                      title_search WHERE title @9@ 'rust graph' ORDER BY id;";
	let expected_fulltext = store
		.execute(fulltext_scan, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let actual_fulltext = store
		.execute(fulltext_index, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	assert_eq!(actual_fulltext, expected_fulltext);
	assert_eq!(expected_fulltext, Value::Array(vec![Value::from("[rust graph]")].into()));

	let mut parameters = Object::new();
	parameters.insert("query", Value::Vector(Vector::F32(vec![0.0, 0.0])));
	let knn_scan =
		"SELECT VALUE id FROM document WITH NOINDEX WHERE embedding <| 2, fn::l1 |> $query;";
	let knn_index = "SELECT VALUE id FROM document WITH INDEX custom_hnsw WHERE embedding <| 2, \
	                 fn::l1 |> $query;";
	let expected_knn =
		store.execute(knn_scan, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
	let actual_knn =
		store.execute(knn_index, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(actual_knn, expected_knn);
	assert_ne!(physical_index_files(&store, "title_search").0, 0);
	assert_ne!(physical_index_files(&store, "custom_hnsw").0, 0);

	assert!(store.checkpoint(CheckpointMode::Restart).unwrap());
	drop(store);
	let reopened = Datastore::open_local(&directory.0, options).unwrap();
	let reopened_fulltext = reopened
		.execute(fulltext_index, &Object::new())
		.unwrap()
		.remove(0)
		.result
		.unwrap()
		.into_parts()
		.0;
	let reopened_knn =
		reopened.execute(knn_index, &parameters).unwrap().remove(0).result.unwrap().into_parts().0;
	assert_eq!(reopened_fulltext, expected_fulltext);
	assert_eq!(reopened_knn, expected_knn);
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

fn physical_index_files(store: &Datastore, name: &str) -> (u64, Option<u64>, Option<String>) {
	let StoreBackend::Local(local) = &store.inner.backend else {
		panic!("test requires a local datastore");
	};
	let state = local.state.lock();
	let index = state
		._database_catalog
		.classes
		.iter()
		.flat_map(|class| &class.indexes)
		.find(|index| index.name.as_str() == name)
		.unwrap_or_else(|| panic!("index {name} is absent"));
	let base = index.physical.expect("physical base is absent").file_id.get();
	let delta = index.delta.map(|binding| binding.file_id.get());
	let path = index.delta.and_then(|binding| {
		state
			.file_catalog
			.files
			.iter()
			.find(|entry| entry.file_id == binding.file_id)
			.map(|entry| entry.path.as_str().to_owned())
	});
	(base, delta, path)
}

fn physical_index_base_path(store: &Datastore, database: &Path, name: &str) -> PathBuf {
	let StoreBackend::Local(local) = &store.inner.backend else {
		panic!("test requires a local datastore");
	};
	let state = local.state.lock();
	let binding = state
		._database_catalog
		.classes
		.iter()
		.flat_map(|class| &class.indexes)
		.find(|index| index.name.as_str() == name)
		.and_then(|index| index.physical)
		.unwrap_or_else(|| panic!("physical index {name} is absent"));
	let path = state
		.file_catalog
		.files
		.iter()
		.find(|entry| entry.file_id == binding.file_id)
		.unwrap_or_else(|| panic!("physical file for index {name} is absent"))
		.path
		.as_str();
	database.join(path)
}

fn corrupt_page_checksum(path: &Path) {
	let mut file = fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
	file.seek(SeekFrom::Start(47)).unwrap();
	let mut byte = [0_u8; 1];
	file.read_exact(&mut byte).unwrap();
	byte[0] ^= 1;
	file.seek(SeekFrom::Start(47)).unwrap();
	file.write_all(&byte).unwrap();
	file.sync_all().unwrap();
}
