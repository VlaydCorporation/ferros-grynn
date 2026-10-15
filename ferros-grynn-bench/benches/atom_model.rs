use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use ferros_grynn::{
	datastore::{ClassSchema, Datastore},
	graph::{AtomKind, AtomRef, GraphId, GraphKind, GraphRead, GraphRef, NodeRead},
	values::{Object, PropertyKeyId, PropertyMap, RecordId, Text, Value},
};

/// Seeded atoms and graph occurrences shared by point-read benchmarks.
struct PointFixture {
	/// Datastore retained by the fixture or operation.
	store: Datastore,
	/// Stable class-qualified record identity used by this operation.
	record: RecordId,
	/// Interned property-name identifier used for value lookup.
	key: PropertyKeyId,
	/// Graph containing the explicit overlay used by the point-read fixture.
	overlay_graph: GraphId,
	/// Graph used to observe inherited base properties in the point-read
	/// fixture.
	inherited_graph: GraphId,
	/// Owned graph reference identifying the operation scope.
	overlay_reference: GraphRef,
	/// Owned graph reference identifying the operation scope.
	inherited_reference: GraphRef,
	/// Stable atom reference used by the point-read fixture.
	atom: AtomRef,
}

/// Builds the point fixture used by nearby scenarios.
fn point_fixture() -> PointFixture {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let overlay_reference = transaction.define_graph("overlay", GraphKind::Graph).unwrap();
	let inherited_reference = transaction.define_graph("inherited", GraphKind::Graph).unwrap();
	let overlay_graph = overlay_reference.id();
	let inherited_graph = inherited_reference.id();
	let key = PropertyKeyId::new(1);
	let record = "person:shared".parse::<RecordId>().unwrap();
	let mut properties = PropertyMap::new();
	properties.insert(key, "base");
	transaction.create_atom(record.clone(), properties).unwrap();
	transaction.attach_node(overlay_graph, &record, None, None).unwrap();
	transaction.attach_node(inherited_graph, &record, None, None).unwrap();
	transaction.set_overlay(overlay_graph, &record, key, Value::from("overlay")).unwrap();
	transaction.commit().unwrap();
	PointFixture {
		store,
		record: record.clone(),
		key,
		overlay_graph,
		inherited_graph,
		overlay_reference,
		inherited_reference,
		atom: AtomRef::new(AtomKind::Node, record),
	}
}

/// Measures point reads with the benchmark harness.
fn benchmark_point_reads(criterion: &mut Criterion) {
	let fixture = point_fixture();
	let mut group = criterion.benchmark_group("atom_model/point_read");
	group.bench_function("base", |bench| {
		bench.iter_batched(
			|| fixture.store.begin(),
			|mut transaction| black_box(transaction.atom(black_box(&fixture.record)).unwrap()),
			BatchSize::SmallInput,
		);
	});
	group.bench_function("base_repeated", |bench| {
		// Keep one transaction to measure repeated reads with its retained
		// snapshot and read state.
		let mut transaction = fixture.store.begin();
		bench.iter(|| black_box(transaction.atom(black_box(&fixture.record)).unwrap()));
	});
	group.bench_function("effective_inherited", |bench| {
		bench.iter_batched(
			|| fixture.store.begin(),
			|mut transaction| {
				black_box(
					transaction
						.effective_property(
							fixture.inherited_graph,
							black_box(&fixture.record),
							fixture.key,
						)
						.unwrap(),
				)
			},
			BatchSize::SmallInput,
		);
	});
	group.bench_function("effective_overlay", |bench| {
		bench.iter_batched(
			|| fixture.store.begin(),
			|mut transaction| {
				black_box(
					transaction
						.effective_property(
							fixture.overlay_graph,
							black_box(&fixture.record),
							fixture.key,
						)
						.unwrap(),
				)
			},
			BatchSize::SmallInput,
		);
	});
	group.bench_function("graph_point_inherited", |bench| {
		let transaction = fixture.store.begin();
		let graph = fixture.inherited_reference.read(&transaction).unwrap();
		let local = graph.atom_id(&fixture.atom).unwrap().unwrap();
		bench.iter(|| {
			let node = graph.node(black_box(local)).unwrap().unwrap();
			black_box(node.properties().unwrap().get(fixture.key));
		});
	});
	group.bench_function("graph_point_overlay", |bench| {
		let transaction = fixture.store.begin();
		let graph = fixture.overlay_reference.read(&transaction).unwrap();
		let local = graph.atom_id(&fixture.atom).unwrap().unwrap();
		bench.iter(|| {
			let node = graph.node(black_box(local)).unwrap().unwrap();
			black_box(node.properties().unwrap().get(fixture.key));
		});
	});
	group.finish();
}

/// Builds the fanout fixture used by nearby scenarios.
fn fanout_fixture(memberships: usize) -> (Datastore, RecordId, PropertyKeyId) {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let key = PropertyKeyId::new(1);
	let record = "person:shared".parse::<RecordId>().unwrap();
	transaction.create_atom(record.clone(), PropertyMap::new()).unwrap();
	for index in 0..memberships {
		let graph = transaction.define_graph(format!("g{index}"), GraphKind::Graph).unwrap();
		transaction.attach_node(graph.id(), &record, None, None).unwrap();
	}
	transaction.commit().unwrap();
	(store, record, key)
}

/// Measures base update fanout with the benchmark harness.
fn benchmark_base_update_fanout(criterion: &mut Criterion) {
	let fixtures = [1_usize, 8, 32].map(|memberships| {
		let (store, record, key) = fanout_fixture(memberships);
		(memberships, store, record, key)
	});
	let mut group = criterion.benchmark_group("atom_model/base_update_fanout");
	for (memberships, store, record, key) in &fixtures {
		group.bench_with_input(
			BenchmarkId::from_parameter(memberships),
			memberships,
			|bench, _| {
				bench.iter_batched(
					|| {
						let mut transaction = store.begin();
						transaction.set_base_property(record, *key, Value::from(1_i64)).unwrap();
						transaction
					},
					|transaction| black_box(transaction.commit().unwrap()),
					BatchSize::PerIteration,
				);
			},
		);
	}
	group.finish();
}

/// Builds the scan fixture used by nearby scenarios.
fn scan_fixture(atoms: usize) -> Datastore {
	let store = Datastore::in_memory();
	let mut transaction = store.begin();
	transaction.define_class(ClassSchema::new("person", AtomKind::Node, []).unwrap()).unwrap();
	let graph = transaction.define_graph("social", GraphKind::Graph).unwrap();
	for index in 0..atoms {
		let record = format!("person:p{index}").parse::<RecordId>().unwrap();
		transaction.create_atom(record.clone(), PropertyMap::new()).unwrap();
		let label = (index % 2 == 0).then(|| Text::from("Employee"));
		transaction.attach_node(graph.id(), &record, label, None).unwrap();
	}
	transaction.commit().unwrap();
	store
}

/// Measures mode one scans with the benchmark harness.
fn benchmark_mode_one_scans(criterion: &mut Criterion) {
	let store = scan_fixture(1_024);
	let mut group = criterion.benchmark_group("atom_model/mode1_scan");
	group.bench_function("class_1024", |bench| {
		bench.iter_batched(
			|| store.begin(),
			|mut transaction| {
				black_box(
					transaction.execute("SELECT VALUE id FROM person;", &Object::new()).unwrap(),
				)
			},
			BatchSize::SmallInput,
		);
	});
	group.bench_function("label_512", |bench| {
		bench.iter_batched(
			|| store.begin(),
			|mut transaction| {
				black_box(
					transaction
						.execute(
							"MATCH (* AS n LABEL Employee) IN GRAPH social RETURN n;",
							&Object::new(),
						)
						.unwrap(),
				)
			},
			BatchSize::SmallInput,
		);
	});
	group.finish();
}

criterion_group!(
	benches,
	benchmark_point_reads,
	benchmark_base_update_fanout,
	benchmark_mode_one_scans
);
criterion_main!(benches);
