use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ferros_grynn::{
	graph::{
		AtomKind,
		AtomRef,
		Dag,
		DirtyTarget,
		DiskGraphView,
		EdgeTopology,
		Forest,
		Frozen,
		Graph,
		GraphRead,
		GraphRef,
		GraphStorageMode,
		LiveGraph,
		LiveUpdate,
		Mutable,
		Path,
		PathDirection,
		PathStep,
		RootedTree,
		SnapshotId,
		TraversalDirection,
		Tree,
		TreeOrientation,
		bfs,
		query_to_memory,
	},
	values::{PropertyMap, RecordId, RecordIdPart},
};

const SIZES: [usize; 2] = [128, 4_096];

fn benchmark_traversal(criterion: &mut Criterion) {
	let mut group = criterion.benchmark_group("graph/traversal_chain");
	for size in SIZES {
		let (general, general_root) = general_chain(size);
		let (tree, tree_root) = tree_chain(size);
		let forest: Forest<Frozen> = tree.clone().into();
		let (dag, dag_root) = dag_chain(size);
		group.throughput(Throughput::Elements(size as u64));
		group.bench_function(BenchmarkId::new("general_frozen", size), |bench| {
			bench.iter(|| {
				black_box(bfs(&general, general_root, TraversalDirection::Forward, None).unwrap())
			});
		});
		group.bench_function(BenchmarkId::new("tree_frozen", size), |bench| {
			bench.iter(|| {
				black_box(bfs(&tree, tree_root, TraversalDirection::Forward, None).unwrap())
			});
		});
		group.bench_function(BenchmarkId::new("forest_frozen", size), |bench| {
			bench.iter(|| {
				black_box(bfs(&forest, tree_root, TraversalDirection::Forward, None).unwrap())
			});
		});
		group.bench_function(BenchmarkId::new("dag_frozen", size), |bench| {
			bench.iter(|| {
				black_box(bfs(&dag, dag_root, TraversalDirection::Forward, None).unwrap())
			});
		});
	}
	group.finish();
}

fn benchmark_state_and_family_conversions(criterion: &mut Criterion) {
	let mut group = criterion.benchmark_group("graph/conversion");
	for size in SIZES {
		let (general, _) = general_chain(size);
		let (tree, _) = tree_chain(size);
		let (dag, _) = dag_chain(size);
		group.throughput(Throughput::Elements(size as u64));

		group.bench_function(BenchmarkId::new("general_thaw", size), |bench| {
			bench.iter_batched(
				|| general.clone(),
				|graph| black_box(graph.thaw()),
				BatchSize::LargeInput,
			);
		});
		group.bench_function(BenchmarkId::new("dag_thaw_rebuild_adjacency", size), |bench| {
			bench.iter_batched(
				|| dag.clone(),
				|graph| black_box(graph.thaw().unwrap()),
				BatchSize::LargeInput,
			);
		});
		group.bench_function(BenchmarkId::new("tree_into_general", size), |bench| {
			bench.iter_batched(
				|| tree.clone(),
				|graph| black_box(graph.into_graph().unwrap()),
				BatchSize::LargeInput,
			);
		});
		group.bench_function(BenchmarkId::new("general_try_into_dag", size), |bench| {
			bench.iter_batched(
				|| general.clone(),
				|graph| black_box(Dag::try_from(graph).unwrap()),
				BatchSize::LargeInput,
			);
		});
	}
	group.finish();
}

fn benchmark_access_modes(criterion: &mut Criterion) {
	let mut group = criterion.benchmark_group("graph/access_mode");
	for size in SIZES {
		let (source, root) = general_chain(size);
		let reference = GraphRef::new(
			RecordId::new("graph", RecordIdPart::Int(size as i64)).unwrap(),
			ferros_grynn::graph::GraphKind::Graph,
			GraphStorageMode::DiskBacked,
		);
		let disk = DiskGraphView::new(&reference, &source, SnapshotId::new(1, 1)).unwrap();
		let hybrid = query_to_memory::<Graph<Frozen>, _>(&disk).unwrap();
		let hybrid_root = hybrid.node_ids().next().unwrap();
		group.throughput(Throughput::Elements(size as u64));
		group.bench_function(BenchmarkId::new("disk_view_adapter_traversal", size), |bench| {
			bench.iter(|| black_box(bfs(&disk, root, TraversalDirection::Forward, None).unwrap()));
		});
		group.bench_function(BenchmarkId::new("hybrid_traversal", size), |bench| {
			bench.iter(|| {
				black_box(bfs(&hybrid, hybrid_root, TraversalDirection::Forward, None).unwrap())
			});
		});
		group.bench_function(BenchmarkId::new("query_to_memory", size), |bench| {
			bench.iter(|| black_box(query_to_memory::<Graph<Frozen>, _>(&disk).unwrap()));
		});
	}
	group.finish();
}

fn benchmark_path_and_live_publication(criterion: &mut Criterion) {
	let path = path(256);
	let copied_steps = path.steps().to_vec();
	let mut group = criterion.benchmark_group("graph/path_and_live");
	group.bench_function("path_arc_clone/256", |bench| {
		bench.iter(|| black_box(path.clone()));
	});
	group.bench_function("path_step_copy/256", |bench| {
		bench.iter(|| black_box(copied_steps.clone()));
	});
	group.bench_function("live_atomic_publish/4096", |bench| {
		bench.iter_batched(
			|| {
				let (current, _) = general_chain(4_096);
				let mut next = current.clone().thaw();
				next.add_node(None, PropertyMap::new()).unwrap();
				(LiveGraph::new(current), next.freeze())
			},
			|(live, next)| black_box(live.publish(next).unwrap()),
			BatchSize::LargeInput,
		);
	});
	group.bench_function("live_update_full_soa_baseline/4096", |bench| {
		bench.iter_batched(
			|| {
				let (current, _) = general_chain(4_096);
				let identity = current.identity();
				let revision = current.revision() + 1;
				let update = LiveUpdate::new(identity, revision, [DirtyTarget::Graph], |graph| {
					graph.add_node(None, PropertyMap::new())?;
					Ok(())
				});
				(LiveGraph::new(current), update)
			},
			|(live, update)| black_box(live.apply_update(&update).unwrap()),
			BatchSize::LargeInput,
		);
	});
	group.finish();
}

fn general_chain(size: usize) -> (Graph<Frozen>, ferros_grynn::graph::AtomId) {
	let mut graph = Graph::<Mutable>::with_capacity(size, size.saturating_sub(1));
	let root = graph.add_node(None, PropertyMap::new()).unwrap();
	let mut previous = root;
	for _ in 1..size {
		let next = graph.add_node(None, PropertyMap::new()).unwrap();
		graph
			.add_edge(EdgeTopology::directed([previous], [next]), None, PropertyMap::new())
			.unwrap();
		previous = next;
	}
	(graph.freeze(), root)
}

fn tree_chain(size: usize) -> (Tree<Frozen>, ferros_grynn::graph::AtomId) {
	let mut tree =
		RootedTree::<Mutable>::new(TreeOrientation::OutArborescence, None, PropertyMap::new())
			.unwrap();
	let root = tree.root();
	let mut previous = root;
	for _ in 1..size {
		let (next, _) =
			tree.add_child(previous, None, PropertyMap::new(), None, PropertyMap::new()).unwrap();
		previous = next;
	}
	(Tree::from(tree).freeze(), root)
}

fn dag_chain(size: usize) -> (Dag<Frozen>, ferros_grynn::graph::AtomId) {
	let mut dag = Dag::<Mutable>::new();
	let root = dag.add_node(None, PropertyMap::new()).unwrap();
	let mut previous = root;
	for _ in 1..size {
		let next = dag.add_node(None, PropertyMap::new()).unwrap();
		dag.add_edge(previous, next, None, PropertyMap::new()).unwrap();
		previous = next;
	}
	(dag.freeze(), root)
}

fn path(length: usize) -> Path {
	let steps = (0..=length)
		.map(|index| {
			let atom = AtomRef::new(
				AtomKind::Node,
				RecordId::new("node", RecordIdPart::Int(index as i64)).unwrap(),
			);
			let edge = (index > 0).then(|| {
				AtomRef::new(
					AtomKind::Edge,
					RecordId::new("edge", RecordIdPart::Int(index as i64)).unwrap(),
				)
			});
			PathStep::new(
				atom,
				edge,
				PathDirection::Forward,
				ferros_grynn::graph::EndpointRole::Target,
			)
		})
		.collect::<Vec<_>>();
	Path::new(ferros_grynn::values::Uuid::default(), SnapshotId::new(1, 1), steps, None)
}

criterion_group!(
	benches,
	benchmark_traversal,
	benchmark_state_and_family_conversions,
	benchmark_access_modes,
	benchmark_path_and_live_publication,
);
criterion_main!(benches);
