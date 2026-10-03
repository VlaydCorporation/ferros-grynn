//! Paired on-demand/Ready-cache SDK and local reopen measurements.

use std::{
	collections::BTreeMap,
	hint::black_box,
	num::NonZeroUsize,
	path::{Path, PathBuf},
	time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use ferros_grynn::{
	datastore::{
		ClassSchema,
		Datastore,
		GraphIndex,
		ImportOptions,
		IoMode,
		LocalOptions,
		MaintenanceOptions,
		TensorCacheFormat,
	},
	gql_status::FerrosGrynnResult,
	graph::{
		AtomKind,
		EdgeTopology,
		GraphKind,
		GraphRead,
		GraphRefReader,
		tensor::{EndpointTensor, TensorOptions, TensorScope},
	},
	values::{PropertyMap, RecordId},
};

/// Builds one graph before starting any paired timed read.
fn benchmark(criterion: &mut Criterion) {
	run(criterion).expect("cache fixture and complete-result comparisons must succeed");
}

/// Uses explicit SDK budgets while keeping maintenance's existing hard work
/// cap.
fn run(criterion: &mut Criterion) -> FerrosGrynnResult<()> {
	let rows = std::env::var("FG26_ROWS")
		.ok()
		.map(|value| value.parse::<usize>().expect("FG26_ROWS integer"))
		.unwrap_or(10_000)
		.max(4);
	let local = std::env::var("FG26_LOCAL").is_ok();
	let native = std::env::var("FG26_NATIVE").is_ok();
	assert!(!native || local, "FG26_NATIVE requires FG26_LOCAL");
	println!("FG26\tcache/backend\tlocal={local}\tnative={native}");
	let path = PathBuf::from(format!("target/stage26-local-{}-{rows}", std::process::id()));
	let mut local_options = LocalOptions::default();
	if native {
		local_options.io_mode = IoMode::NativeAsync;
	}
	local_options.max_snapshot_age = Duration::from_secs(3600);
	local_options.max_write_set_bytes = 1024 * 1024 * 1024;
	local_options.maintenance.max_memory_bytes =
		NonZeroUsize::new(4 * 1024 * 1024 * 1024).expect("positive memory");
	let store = if local {
		Datastore::create_local(&path, local_options)?
	} else {
		Datastore::in_memory()
	};
	let mut setup = store.begin();
	setup.define_class(ClassSchema::new("n", AtomKind::Node, [])?)?;
	setup.define_class(ClassSchema::new("e", AtomKind::Edge, [])?)?;
	let graph = setup.define_graph("probe", GraphKind::Graph)?;
	setup.commit()?;
	let imported = Instant::now();
	let mut import_options = ImportOptions::default();
	import_options.segment_size = 4096;
	store.import(import_options, |session| {
		for row in 0..rows {
			let record = format!("n:{row}").parse::<RecordId>()?;
			session.apply_item(|tx| {
				tx.create_atom(record.clone(), PropertyMap::new())?;
				tx.attach_node(graph.id(), &record, None, None)
			})?;
		}
		Ok(())
	})?;
	let nodes = {
		let tx = store.begin();
		let view = tx.read_graph(&graph)?;
		view.node_ids()?.collect::<FerrosGrynnResult<Vec<_>>>()?
	};
	store.import(import_options, |session| {
		for row in 0..rows {
			let record = format!("e:{row}").parse::<RecordId>()?;
			session.apply_item(|tx| {
				tx.create_atom(record.clone(), PropertyMap::new())?;
				tx.attach_edge(
					graph.id(),
					&record,
					None,
					EdgeTopology::directed([nodes[row]], [nodes[(row + 1) % rows]]),
					None,
					None,
				)
			})?;
		}
		Ok(())
	})?;
	println!("FG26\tcache/import/{rows}/{local}\t{:?}", imported.elapsed());
	let mut options = TensorOptions::default();
	options.limits.memory_bytes = 12 * 1024 * 1024 * 1024;
	options.limits.max_work = 1_000_000_000;
	let expected = store.begin().endpoint_tensor(&graph, options)?;
	assert_eq!(expected.shape(), (2 * rows, rows, 2));
	let mut define = store.begin();
	let index = define.define_graph_index(
		"probe",
		GraphIndex::new_tensor(
			"tensor",
			TensorScope::Local,
			TensorCacheFormat::Auto,
			4 * 1024 * 1024 * 1024,
		)?,
	)?;
	define.commit()?;
	let mut maintenance = MaintenanceOptions::default();
	maintenance.max_memory_bytes = local_options.maintenance.max_memory_bytes;
	let started = Instant::now();
	match store.rebuild_graph_index(graph.id(), index, maintenance) {
		Ok(()) => {}
		Err(error) => {
			assert_eq!(error.status().code().to_string(), "FG-54F13", "{error}");
			println!(
				"FG26\tcache/build/{rows}/{local}\tresource_rejected\t{:?}\t{error}",
				started.elapsed()
			);
			return Ok(());
		}
	}
	println!("FG26\tcache/build/{rows}/{local}\tcomplete\t{:?}", started.elapsed());
	let cached = store.begin().endpoint_tensor_using_index(&graph, options, index)?;
	assert_eq!(cached.atoms(), expected.atoms());
	assert_eq!(cached.connectors(), expected.connectors());
	println!(
		"FG26\tcache/resources/{rows}/{local}\t{:?}\t{:?}",
		(expected.admitted_bytes(), expected.work()),
		(cached.admitted_bytes(), cached.work())
	);
	let mut group = criterion.benchmark_group(format!("stage26/cache/{rows}/{local}/{native}"));
	group.bench_function("on_demand", |bench| {
		bench.iter(|| {
			let tx = store.begin();
			let view = tx.read_graph(&graph).expect("same snapshot graph");
			let tensor =
				EndpointTensor::from_graph(&view, options).expect("complete on-demand tensor");
			assert_eq!(tensor.shape(), (2 * rows, rows, 2));
			black_box(tensor);
		})
	});
	group.bench_function("ready", |bench| {
		bench.iter(|| {
			let tensor = store
				.begin()
				.endpoint_tensor_using_index(&graph, options, index)
				.expect("complete compatible generation");
			assert_eq!(tensor.shape(), (2 * rows, rows, 2));
			black_box(tensor);
		})
	});
	group.bench_function("rebuild", |bench| {
		bench.iter(|| {
			store
				.rebuild_graph_index(graph.id(), index, maintenance)
				.expect("bounded complete rebuild")
		})
	});
	group.finish();
	let mut old = store.begin();
	let wal_before = local.then(|| wal_sizes(&path));
	let started = Instant::now();
	let mut write = store.begin();
	write.set_occurrence_label(graph.id(), &"e:0".parse()?, Some("changed".into()))?;
	assert!(write.endpoint_tensor_using_index(&graph, options, index).is_err());
	write.commit()?;
	let mutation_time = started.elapsed();
	assert!(store.begin().endpoint_tensor_using_index(&graph, options, index).is_err());
	assert_eq!(
		old.endpoint_tensor_using_index(&graph, options, index)?.connectors(),
		cached.connectors()
	);
	let started = Instant::now();
	store.rebuild_graph_index(graph.id(), index, maintenance)?;
	let rebuild_time = started.elapsed();
	let changed = store.begin().endpoint_tensor_using_index(&graph, options, index)?;
	assert!(
		changed
			.connectors()
			.iter()
			.any(|connector| connector.label().is_some_and(|label| label.as_str() == "changed"))
	);
	println!(
		"FG26\tcache/mutation_rebuild/{rows}/{local}/{native}\t{mutation_time:?}\t{rebuild_time:?}"
	);
	if let Some(before) = wal_before {
		let after = wal_sizes(&path);
		let monotonic =
			before.iter().all(|(file, bytes)| after.get(file).is_some_and(|size| size >= bytes));
		let appended = monotonic.then(|| {
			after
				.iter()
				.map(|(file, bytes)| bytes - before.get(file).copied().unwrap_or(0))
				.sum::<u64>()
		});
		println!(
			"FG26\tcache/mutation_rebuild_wal/{rows}/{native}\t{appended:?}\tmonotonic={monotonic}"
		);
	}
	drop(old);
	if local {
		println!("FG26\tcache/storage/{rows}\t{:?}", store.local_storage_status());
		drop(expected);
		drop(cached);
		drop(store);
		criterion.bench_function(&format!("stage26/cache/{rows}/{native}/cold_reopen"), |bench| {
			bench.iter(|| {
				let store =
					Datastore::open_local(&path, local_options).expect("recovery must succeed");
				let tensor = store
					.begin()
					.endpoint_tensor_using_index(&graph, options, index)
					.expect("recovered compatible cache");
				assert_eq!(tensor.shape(), (2 * rows, rows, 2));
				black_box(tensor);
			})
		});
		println!("FG26\tcache/local_directory\t{}", path.display());
	}
	Ok(())
}

/// Captures actual segment lengths; retirement makes a byte delta inconclusive.
fn wal_sizes(path: &Path) -> BTreeMap<PathBuf, u64> {
	std::fs::read_dir(path.join("wal"))
		.expect("local WAL directory")
		.map(|entry| entry.expect("WAL entry"))
		.filter(|entry| entry.path().extension().is_some_and(|extension| extension == "wal"))
		.map(|entry| (entry.path(), entry.metadata().expect("WAL metadata").len()))
		.collect()
}

criterion_group! {
	name = benches;
	config = Criterion::default().sample_size(10).warm_up_time(Duration::from_millis(200)).measurement_time(Duration::from_secs(1));
	targets = benchmark
}
criterion_main!(benches);
