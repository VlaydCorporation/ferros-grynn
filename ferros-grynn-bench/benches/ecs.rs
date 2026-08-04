use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ferros_grynn::{ecs::SparseSet, graph::AtomId};
use rapidhash::{HashMapExt, RapidHashMap};

const SIZES: [usize; 3] = [128, 4_096, 65_536];

fn atoms(size: usize) -> Vec<AtomId> {
	(0..size).map(|slot| AtomId::from_raw(slot as u64)).collect()
}

fn benchmark_build(criterion: &mut Criterion) {
	let mut group = criterion.benchmark_group("ecs/build");
	for size in SIZES {
		let ids = atoms(size);
		group.throughput(Throughput::Elements(size as u64));
		group.bench_with_input(BenchmarkId::new("paged_sparse_set", size), &ids, |bench, ids| {
			bench.iter(|| {
				let mut pool = SparseSet::with_capacity(ids.len());
				for (value, id) in ids.iter().copied().enumerate() {
					pool.insert(id, value as u64).unwrap();
				}
				black_box(pool)
			});
		});
		group.bench_with_input(BenchmarkId::new("rapid_hash_map", size), &ids, |bench, ids| {
			bench.iter(|| {
				let mut pool = RapidHashMap::with_capacity(ids.len());
				for (value, id) in ids.iter().copied().enumerate() {
					pool.insert(id, value as u64);
				}
				black_box(pool)
			});
		});
	}
	group.finish();
}

fn benchmark_lookup(criterion: &mut Criterion) {
	let mut group = criterion.benchmark_group("ecs/lookup_all_hits");
	for size in SIZES {
		let ids = atoms(size);
		let mut sparse = SparseSet::with_capacity(size);
		let mut hashed = RapidHashMap::with_capacity(size);
		for (value, id) in ids.iter().copied().enumerate() {
			sparse.insert(id, value as u64).unwrap();
			hashed.insert(id, value as u64);
		}

		group.throughput(Throughput::Elements(size as u64));
		group.bench_with_input(BenchmarkId::new("paged_sparse_set", size), &ids, |bench, ids| {
			bench.iter(|| {
				let sum = ids
					.iter()
					.map(|id| *sparse.get(*id).expect("benchmark ID must exist"))
					.fold(0_u64, u64::wrapping_add);
				black_box(sum)
			});
		});
		group.bench_with_input(BenchmarkId::new("rapid_hash_map", size), &ids, |bench, ids| {
			bench.iter(|| {
				let sum = ids
					.iter()
					.map(|id| *hashed.get(id).expect("benchmark ID must exist"))
					.fold(0_u64, u64::wrapping_add);
				black_box(sum)
			});
		});
	}
	group.finish();
}

fn benchmark_dense_iteration(criterion: &mut Criterion) {
	let mut group = criterion.benchmark_group("ecs/dense_iteration");
	for size in SIZES {
		let ids = atoms(size);
		let mut sparse = SparseSet::with_capacity(size);
		let mut hashed = RapidHashMap::with_capacity(size);
		for (value, id) in ids.iter().copied().enumerate() {
			sparse.insert(id, value as u64).unwrap();
			hashed.insert(id, value as u64);
		}

		group.throughput(Throughput::Elements(size as u64));
		group.bench_function(BenchmarkId::new("paged_sparse_set", size), |bench| {
			bench.iter(|| {
				let sum = sparse.components().iter().copied().fold(0_u64, u64::wrapping_add);
				black_box(sum)
			});
		});
		group.bench_function(BenchmarkId::new("rapid_hash_map", size), |bench| {
			bench.iter(|| {
				let sum = hashed.values().copied().fold(0_u64, u64::wrapping_add);
				black_box(sum)
			});
		});
	}
	group.finish();
}

criterion_group!(benches, benchmark_build, benchmark_lookup, benchmark_dense_iteration);
criterion_main!(benches);
