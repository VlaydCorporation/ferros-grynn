use std::{collections::*, hash::Hash, hint::black_box, time::Duration};

use arrayvec::ArrayVec;
use criterion::*;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use slotmap::{DefaultKey, DenseSlotMap, SlotMap};
use smallvec::SmallVec;
use tinyvec::TinyVec;

const N: usize = 1_000_000;

const STEPS: [usize; 6] = [2, 4, 10, 16, 32, 64];

#[derive(Clone, Copy, PartialEq, Ord, PartialOrd, Eq, Hash, Default)]
struct LargeData {
	pub v: u32,
	_v: u32,
	data: [u64; 31], // 256 bytes
}

impl From<LargeData> for u32 {
	fn from(value: LargeData) -> Self {
		value.v
	}
}

impl From<&LargeData> for u32 {
	fn from(value: &LargeData) -> Self {
		value.v as u32
	}
}

impl From<u32> for LargeData {
	fn from(v: u32) -> Self {
		Self {
			v,
			_v: v,
			data: [0; 31],
		}
	}
}

fn random_indices(n: usize) -> Vec<usize> {
	let mut rng = StdRng::seed_from_u64(42);
	(0..n).map(|_| rng.random_range(0..n)).collect()
}

// trait TestData: Clone + Copy + From<u32> + Into<u32> + PartialEq + Ord + Hash
// + Default {}
//
// impl TestData for u32 {}
// impl TestData for LargeData {}

fn bench_push_raw<const K: usize, const M: usize, const N: usize>(c: &mut Criterion) {
	let mut group = c.benchmark_group("push_raw");

	group.bench_function("Vec<u32>", |b| {
		b.iter_batched(
			|| Vec::<u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("Vec<LargeData>", |b| {
		b.iter_batched(
			|| Vec::<LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<u32>", |b| {
		b.iter_batched(
			|| VecDeque::<u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_back(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<LargeData>", |b| {
		b.iter_batched(
			|| VecDeque::<LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_back(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<u32>", |b| {
		b.iter_batched(
			|| BinaryHeap::<u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<LargeData>", |b| {
		b.iter_batched(
			|| BinaryHeap::<LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<u32> front", |b| {
		b.iter_batched(
			|| LinkedList::<u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_front(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<LargeData> front", |b| {
		b.iter_batched(
			|| LinkedList::<LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_front(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<u32> back", |b| {
		b.iter_batched(
			|| LinkedList::<u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_back(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<LargeData> back", |b| {
		b.iter_batched(
			|| LinkedList::<LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_back(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, u32> insert", |b| {
		b.iter_batched(
			|| HashMap::<u32, u32>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, LargeData> insert", |b| {
		b.iter_batched(
			|| HashMap::<u32, LargeData>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, u32> insert", |b| {
		b.iter_batched(
			|| RapidHashMap::<u32, u32>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, LargeData> insert", |b| {
		b.iter_batched(
			|| RapidHashMap::<u32, LargeData>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<u32> insert", |b| {
		b.iter_batched(
			|| HashSet::<u32>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<LargeData> insert", |b| {
		b.iter_batched(
			|| HashSet::<LargeData>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<u32> insert", |b| {
		b.iter_batched(
			|| RapidHashSet::<u32>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<LargeData> insert", |b| {
		b.iter_batched(
			|| RapidHashSet::<LargeData>::new(),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<u32>", |b| {
		b.iter_batched(
			|| SlotMap::<DefaultKey, u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<LargeData>", |b| {
		b.iter_batched(
			|| SlotMap::<DefaultKey, LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<u32>", |b| {
		b.iter_batched(
			|| DenseSlotMap::<DefaultKey, u32>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<LargeData>", |b| {
		b.iter_batched(
			|| DenseSlotMap::<DefaultKey, LargeData>::new(),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	for n in [K, M, N] {
		group.bench_function(format!("SmallVec<u32, {K}> push {n}"), |b| {
			b.iter_batched(
				|| SmallVec::<u32, K>::new(),
				|mut v| {
					for i in (0..n as u32).map(|i| i.into()) {
						v.push(i);
					}
					black_box(v)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(format!("SmallVec<LargeData, {K}> push {n}"), |b| {
			b.iter_batched(
				|| SmallVec::<LargeData, K>::new(),
				|mut v| {
					for i in (0..n as u32).map(|i| i.into()) {
						v.push(i);
					}
					black_box(v)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(format!("TinyVec<[u32; {K}]> push {n}"), |b| {
			b.iter_batched(
				|| TinyVec::<[u32; K]>::new(),
				|mut v| {
					for i in (0..n as u32).map(|i| i.into()) {
						v.push(i);
					}
					black_box(v)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(format!("TinyVec<[LargeData; {K}]> push {n}"), |b| {
			b.iter_batched(
				|| TinyVec::<[LargeData; K]>::new(),
				|mut v| {
					for i in (0..n as u32).map(|i| i.into()) {
						v.push(i);
					}
					black_box(v)
				},
				BatchSize::PerIteration,
			)
		});
	}

	group.bench_function(format!("ArrayVec<u32, {K}>"), |b| {
		b.iter_batched(
			|| ArrayVec::<u32, K>::new(),
			|mut v| {
				for i in (0..K as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function(format!("ArrayVec<LargeData, {K}>"), |b| {
		b.iter_batched(
			|| ArrayVec::<LargeData, K>::new(),
			|mut v| {
				for i in (0..K as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});
}

fn bench_push_preallocated<const K: usize, const M: usize, const N: usize>(c: &mut Criterion) {
	let mut group = c.benchmark_group("push_preallocated");

	group.bench_function("Vec<u32>", |b| {
		b.iter_batched(
			|| Vec::<u32>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("Vec<LargeData>", |b| {
		b.iter_batched(
			|| Vec::<LargeData>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<u32>", |b| {
		b.iter_batched(
			|| VecDeque::<u32>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_back(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<LargeData>", |b| {
		b.iter_batched(
			|| VecDeque::<LargeData>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push_back(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<u32>", |b| {
		b.iter_batched(
			|| BinaryHeap::<u32>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<LargeData>", |b| {
		b.iter_batched(
			|| BinaryHeap::<LargeData>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					v.push(i);
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, u32> insert", |b| {
		b.iter_batched(
			|| HashMap::<u32, u32>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, LargeData> insert", |b| {
		b.iter_batched(
			|| HashMap::<u32, LargeData>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, u32> insert", |b| {
		b.iter_batched(
			|| RapidHashMap::<u32, u32>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, LargeData> insert", |b| {
		b.iter_batched(
			|| RapidHashMap::<u32, LargeData>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i, i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<u32> insert", |b| {
		b.iter_batched(
			|| HashSet::<u32>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<LargeData> insert", |b| {
		b.iter_batched(
			|| HashSet::<LargeData>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<u32> insert", |b| {
		b.iter_batched(
			|| RapidHashSet::<u32>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<LargeData> insert", |b| {
		b.iter_batched(
			|| RapidHashSet::<LargeData>::with_capacity(N),
			|mut v| {
				for i in 0..N as u32 {
					v.insert(i.into());
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<u32>", |b| {
		b.iter_batched(
			|| SlotMap::<DefaultKey, u32>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<LargeData>", |b| {
		b.iter_batched(
			|| SlotMap::<DefaultKey, LargeData>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<u32>", |b| {
		b.iter_batched(
			|| DenseSlotMap::<DefaultKey, u32>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<LargeData>", |b| {
		b.iter_batched(
			|| DenseSlotMap::<DefaultKey, LargeData>::with_capacity(N),
			|mut v| {
				for i in (0..N as u32).map(|i| i.into()) {
					black_box(v.insert(i));
				}
				black_box(v)
			},
			BatchSize::PerIteration,
		)
	});

	for n in [K, M, N] {
		for m in [K, M, N] {
			group.bench_function(format!("SmallVec<u32, {K}> push {n}, preallocate {m}"), |b| {
				b.iter_batched(
					|| SmallVec::<u32, K>::with_capacity(m),
					|mut v| {
						for i in (0..n as u32).map(|i| i.into()) {
							v.push(i);
						}
						black_box(v)
					},
					BatchSize::PerIteration,
				)
			});

			group.bench_function(
				format!("SmallVec<LargeData, {K}> push {n}, preallocate {m}"),
				|b| {
					b.iter_batched(
						|| SmallVec::<LargeData, K>::with_capacity(m),
						|mut v| {
							for i in (0..n as u32).map(|i| i.into()) {
								v.push(i);
							}
							black_box(v)
						},
						BatchSize::PerIteration,
					)
				},
			);

			group.bench_function(format!("TinyVec<[u32; {K}]> push {n}, preallocate {m}"), |b| {
				b.iter_batched(
					|| TinyVec::<[u32; K]>::with_capacity(m),
					|mut v| {
						for i in (0..n as u32).map(|i| i.into()) {
							v.push(i);
						}
						black_box(v)
					},
					BatchSize::PerIteration,
				)
			});

			group.bench_function(
				format!("TinyVec<[LargeData; {K}]> push {n}, preallocate {m}"),
				|b| {
					b.iter_batched(
						|| TinyVec::<[LargeData; K]>::with_capacity(m),
						|mut v| {
							for i in (0..n as u32).map(|i| i.into()) {
								v.push(i);
							}
							black_box(v)
						},
						BatchSize::PerIteration,
					)
				},
			);
		}
	}
}

fn bench_collect<const K: usize, const M: usize, const N: usize>(c: &mut Criterion) {
	let mut group = c.benchmark_group("collect");

	group.bench_function("Vec<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<Vec<u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("Vec<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<Vec<LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<VecDeque<u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<VecDeque<LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<BinaryHeap<u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<BinaryHeap<LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<LinkedList<u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<LinkedList<LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| (i, i.into())),
			|data| black_box(data.collect::<HashMap<u32, u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| (i, i.into())),
			|data| black_box(data.collect::<HashMap<u32, LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| (i, i.into())),
			|data| black_box(data.collect::<RapidHashMap<u32, u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| (i, i.into())),
			|data| black_box(data.collect::<RapidHashMap<u32, LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<HashSet<u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<HashSet<LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<RapidHashSet<u32>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()),
			|data| black_box(data.collect::<RapidHashSet<LargeData>>()),
			BatchSize::PerIteration,
		)
	});

	for n in [K, M, N] {
		group.bench_function(format!("SmallVec<u32, {K}> collect {n}"), |b| {
			b.iter_batched(
				|| (0..n as u32).map(|i| i.into()),
				|data| black_box(data.collect::<SmallVec<u32, K>>()),
				BatchSize::PerIteration,
			)
		});

		group.bench_function(format!("SmallVec<LargeData, {K}> collect {n}"), |b| {
			b.iter_batched(
				|| (0..n as u32).map(|i| i.into()),
				|data| black_box(data.collect::<SmallVec<LargeData, K>>()),
				BatchSize::PerIteration,
			)
		});

		group.bench_function(format!("TinyVec<[u32; {K}]> collect {n}"), |b| {
			b.iter_batched(
				|| (0..n as u32).map(|i| i.into()),
				|data| black_box(data.collect::<TinyVec<[u32; K]>>()),
				BatchSize::PerIteration,
			)
		});

		group.bench_function(format!("TinyVec<[LargeData; {K}]> collect {n}"), |b| {
			b.iter_batched(
				|| (0..n as u32).map(|i| i.into()),
				|data| black_box(data.collect::<TinyVec<[LargeData; K]>>()),
				BatchSize::PerIteration,
			)
		});
	}

	group.bench_function(format!("ArrayVec<u32, {K}>"), |b| {
		b.iter_batched(
			|| (0..K as u32).map(|i| i.into()),
			|data| black_box(data.collect::<ArrayVec<u32, K>>()),
			BatchSize::PerIteration,
		)
	});

	group.bench_function(format!("ArrayVec<LargeData, {K}>"), |b| {
		b.iter_batched(
			|| (0..K as u32).map(|i| i.into()),
			|data| black_box(data.collect::<ArrayVec<LargeData, K>>()),
			BatchSize::PerIteration,
		)
	});
}

fn bench_iter_sequential_owned(c: &mut Criterion) {
	let mut group = c.benchmark_group("iter_sequential_owned");

	group.bench_function("Vec<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<Vec<u32>>(),
			|vec| {
				let mut sum = 0u32;
				for v in vec {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("Vec<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<Vec<LargeData>>(),
			|vec| {
				let mut sum = 0u32;
				for v in vec {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<VecDeque<u32>>(),
			|vec| {
				let mut sum = 0;
				for v in vec {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<VecDeque<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for v in vec {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<BinaryHeap<u32>>(),
			|vec| {
				let mut sum = 0;
				for v in vec {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<BinaryHeap<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for v in vec {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<LinkedList<u32>>(),
			|vec| {
				let mut sum = 0;
				for v in vec {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<LinkedList<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for v in vec {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, u32>", |b| {
		b.iter_batched(
			|| {
				let mut map = HashMap::<u32, u32>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut map = HashMap::<u32, LargeData>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, u32>", |b| {
		b.iter_batched(
			|| {
				let mut map = RapidHashMap::<u32, u32>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut map = RapidHashMap::<u32, LargeData>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<u32>", |b| {
		b.iter_batched(
			|| {
				let mut set = HashSet::<u32>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut set = HashSet::<LargeData>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<u32>", |b| {
		b.iter_batched(
			|| {
				let mut set = RapidHashSet::<u32>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut set = RapidHashSet::<LargeData>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<u32>", |b| {
		b.iter_batched(
			|| {
				let mut slot: SlotMap<DefaultKey, u32> = SlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut slot: SlotMap<DefaultKey, LargeData> = SlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<u32>", |b| {
		b.iter_batched(
			|| {
				let mut slot: DenseSlotMap<DefaultKey, u32> = DenseSlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut slot: DenseSlotMap<DefaultKey, LargeData> = DenseSlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	// We don't need to bench SmallVec, ArrayVec and TinyVec here, because
	// with small elements number on stack, it will be extremely fast,
	// and with large elements number in heap it will be similar to Vec.
}

fn bench_iter_sequential_borrowed(c: &mut Criterion) {
	let mut group = c.benchmark_group("iter_sequential_borrowed");

	group.bench_function("Vec<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<Vec<u32>>(),
			|vec| {
				let mut sum = 0u32;
				for v in vec.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("Vec<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<Vec<LargeData>>(),
			|vec| {
				let mut sum = 0u32;
				for v in vec.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<VecDeque<u32>>(),
			|vec| {
				let mut sum = 0;
				for v in vec.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<VecDeque<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for v in vec.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<BinaryHeap<u32>>(),
			|vec| {
				let mut sum = 0;
				for v in vec.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("BinaryHeap<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<BinaryHeap<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for v in vec.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<LinkedList<u32>>(),
			|vec| {
				let mut sum = 0;
				for v in vec.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("LinkedList<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<LinkedList<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for v in vec.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, u32>", |b| {
		b.iter_batched(
			|| {
				let mut map = HashMap::<u32, u32>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut map = HashMap::<u32, LargeData>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, u32>", |b| {
		b.iter_batched(
			|| {
				let mut map = RapidHashMap::<u32, u32>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut map = RapidHashMap::<u32, LargeData>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for (_k, v) in map.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<u32>", |b| {
		b.iter_batched(
			|| {
				let mut set = HashSet::<u32>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut set = HashSet::<LargeData>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<u32>", |b| {
		b.iter_batched(
			|| {
				let mut set = RapidHashSet::<u32>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut set = RapidHashSet::<LargeData>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for v in set.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<u32>", |b| {
		b.iter_batched(
			|| {
				let mut slot: SlotMap<DefaultKey, u32> = SlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut slot: SlotMap<DefaultKey, LargeData> = SlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<u32>", |b| {
		b.iter_batched(
			|| {
				let mut slot: DenseSlotMap<DefaultKey, u32> = DenseSlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot.iter() {
					sum += v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut slot: DenseSlotMap<DefaultKey, LargeData> = DenseSlotMap::with_capacity(N);
				for i in 0..N as u32 {
					slot.insert(i.into());
				}
				slot
			},
			|slot| {
				let mut sum = 0;
				for (_k, v) in slot.iter() {
					sum += v.v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	// We don't need to bench SmallVec, ArrayVec and TinyVec here, because
	// with small elements number on stack, it will be extremely fast,
	// and with large elements number in heap it will be similar to Vec.
}

fn bench_iter_sequential_with_step(c: &mut Criterion) {
	let mut group = c.benchmark_group("iter_sequential_with_step");

	for step in STEPS {
		group.bench_with_input(BenchmarkId::new("Vec<u32>", step), &step, |b, &step| {
			b.iter_batched(
				|| {
					let vec = (0..N as u32).map(|i| i.into()).collect::<Vec<u32>>();
					let range = (0..vec.len()).step_by(step);
					(vec, range)
				},
				|(vec, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += vec[i];
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_with_input(BenchmarkId::new("Vec<LargeData>", step), &step, |b, &step| {
			b.iter_batched(
				|| {
					let vec = (0..N as u32).map(|i| i.into()).collect::<Vec<LargeData>>();
					let range = (0..vec.len()).step_by(step);
					(vec, range)
				},
				|(vec, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += vec[i].v;
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_with_input(BenchmarkId::new("VecDeque<u32>", step), &step, |b, &step| {
			b.iter_batched(
				|| {
					let vec = (0..N as u32).map(|i| i.into()).collect::<VecDeque<u32>>();
					let range = (0..vec.len()).step_by(step);
					(vec, range)
				},
				|(vec, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += vec[i];
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_with_input(BenchmarkId::new("VecDeque<LargeData>", step), &step, |b, &step| {
			b.iter_batched(
				|| {
					let vec = (0..N as u32).map(|i| i.into()).collect::<VecDeque<LargeData>>();
					let range = (0..vec.len()).step_by(step);
					(vec, range)
				},
				|(vec, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += vec[i].v;
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("HashSet<u32>", step), |b| {
			b.iter_batched(
				|| {
					let mut set = HashSet::<u32>::with_capacity(N);
					for i in 0..N {
						set.insert((i as u32).into());
					}
					let range = (0..set.len()).step_by(step);
					(set, range)
				},
				|(set, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += set.get(&((i as u32).into())).unwrap();
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("HashSet<LargeData>", step), |b| {
			b.iter_batched(
				|| {
					let mut set = HashSet::<LargeData>::with_capacity(N);
					for i in 0..N {
						set.insert((i as u32).into());
					}
					let range = (0..set.len()).step_by(step);
					(set, range)
				},
				|(set, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += set.get(&((i as u32).into())).unwrap().v;
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("RapidHashSet<u32>", step), |b| {
			b.iter_batched(
				|| {
					let mut set = RapidHashSet::<u32>::with_capacity(N);
					for i in 0..N {
						set.insert((i as u32).into());
					}
					let range = (0..set.len()).step_by(step);
					(set, range)
				},
				|(set, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += set.get(&((i as u32).into())).unwrap();
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("RapidHashSet<LargeData>", step), |b| {
			b.iter_batched(
				|| {
					let mut set = RapidHashSet::<LargeData>::with_capacity(N);
					for i in 0..N {
						set.insert((i as u32).into());
					}
					let range = (0..set.len()).step_by(step);
					(set, range)
				},
				|(set, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += set.get(&((i as u32).into())).unwrap().v;
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("SlotMap<u32>", step), |b| {
			b.iter_batched(
				|| {
					let mut slot: SlotMap<DefaultKey, u32> = SlotMap::with_capacity(N);
					let mut keys = Vec::with_capacity(N);
					for i in 0..N as u32 {
						keys.push(slot.insert(i.into()));
					}
					let range = (0..slot.len()).step_by(step);
					(slot, keys, range)
				},
				|(slot, keys, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += slot.get(keys[i]).unwrap();
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("SlotMap<LargeData>", step), |b| {
			b.iter_batched(
				|| {
					let mut slot: SlotMap<DefaultKey, LargeData> = SlotMap::with_capacity(N);
					let mut keys = Vec::with_capacity(N);
					for i in 0..N as u32 {
						keys.push(slot.insert(i.into()));
					}
					let range = (0..slot.len()).step_by(step);
					(slot, keys, range)
				},
				|(slot, keys, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += slot.get(keys[i]).unwrap().v;
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("DenseSlotMap<u32>", step), |b| {
			b.iter_batched(
				|| {
					let mut slot: DenseSlotMap<DefaultKey, u32> = DenseSlotMap::with_capacity(N);
					let mut keys = Vec::with_capacity(N);
					for i in 0..N as u32 {
						keys.push(slot.insert(i.into()));
					}
					let range = (0..slot.len()).step_by(step);
					(slot, keys, range)
				},
				|(slot, keys, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += slot.get(keys[i]).unwrap();
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		group.bench_function(BenchmarkId::new("DenseSlotMap<LargeData>", step), |b| {
			b.iter_batched(
				|| {
					let mut slot: DenseSlotMap<DefaultKey, LargeData> =
						DenseSlotMap::with_capacity(N);
					let mut keys = Vec::with_capacity(N);
					for i in 0..N as u32 {
						keys.push(slot.insert(i.into()));
					}
					let range = (0..slot.len()).step_by(step);
					(slot, keys, range)
				},
				|(slot, keys, steps)| {
					let mut sum = 0;
					for i in steps {
						sum += slot.get(keys[i]).unwrap().v;
					}
					black_box(sum)
				},
				BatchSize::PerIteration,
			)
		});

		// We don't need to bench SmallVec, ArrayVec and TinyVec here, because
		// with small elements number on stack, it will be extremely fast,
		// and with large elements number in heap it will be similar to Vec.
	}
}

fn batch_random_access(c: &mut Criterion) {
	let mut group = c.benchmark_group("random_access");

	let random = random_indices(N);

	group.bench_function("Vec<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<Vec<u32>>(),
			|vec| {
				let mut sum = 0;
				for &i in &random {
					sum += vec[i];
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("Vec<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<Vec<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for &i in &random {
					sum += vec[i].v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<u32>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<VecDeque<u32>>(),
			|vec| {
				let mut sum = 0;
				for &i in &random {
					sum += vec[i];
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("VecDeque<LargeData>", |b| {
		b.iter_batched(
			|| (0..N as u32).map(|i| i.into()).collect::<VecDeque<LargeData>>(),
			|vec| {
				let mut sum = 0;
				for &i in &random {
					sum += vec[i].v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, u32>", |b| {
		b.iter_batched(
			|| {
				let mut map = HashMap::<u32, u32>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for &i in &random {
					sum += map.get(&(i as u32)).unwrap();
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut map = HashMap::<u32, LargeData>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for &i in &random {
					sum += map.get(&(i as u32)).unwrap().v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, u32>", |b| {
		b.iter_batched(
			|| {
				let mut map = RapidHashMap::<u32, u32>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for &i in &random {
					sum += map.get(&(i as u32)).unwrap();
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashMap<u32, LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut map = RapidHashMap::<u32, LargeData>::with_capacity(N);
				for i in 0..N {
					map.insert(i as u32, (i as u32).into());
				}
				map
			},
			|map| {
				let mut sum = 0;
				for &i in &random {
					sum += map.get(&(i as u32)).unwrap().v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<u32>", |b| {
		b.iter_batched(
			|| {
				let mut set = HashSet::<u32>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for &i in &random {
					sum += set.get(&((i as u32).into())).unwrap();
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("HashSet<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut set = HashSet::<LargeData>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for &i in &random {
					sum += set.get(&((i as u32).into())).unwrap().v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<u32>", |b| {
		b.iter_batched(
			|| {
				let mut set = RapidHashSet::<u32>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for &i in &random {
					sum += set.get(&((i as u32).into())).unwrap();
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("RapidHashSet<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut set = RapidHashSet::<LargeData>::with_capacity(N);
				for i in 0..N {
					set.insert((i as u32).into());
				}
				set
			},
			|set| {
				let mut sum = 0;
				for &i in &random {
					sum += set.get(&((i as u32).into())).unwrap().v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<u32>", |b| {
		b.iter_batched(
			|| {
				let mut slot: SlotMap<DefaultKey, u32> = SlotMap::with_capacity(N);
				let mut keys = Vec::with_capacity(N);
				for i in 0..N as u32 {
					keys.push(slot.insert(i.into()));
				}
				(slot, keys)
			},
			|(slot, keys)| {
				let mut sum = 0;
				for &i in &random {
					sum += slot.get(keys[i]).unwrap();
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("SlotMap<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut slot: SlotMap<DefaultKey, LargeData> = SlotMap::with_capacity(N);
				let mut keys = Vec::with_capacity(N);
				for i in 0..N as u32 {
					keys.push(slot.insert(i.into()));
				}
				(slot, keys)
			},
			|(slot, keys)| {
				let mut sum = 0;
				for &i in &random {
					sum += slot.get(keys[i]).unwrap().v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<u32>", |b| {
		b.iter_batched(
			|| {
				let mut slot: DenseSlotMap<DefaultKey, u32> = DenseSlotMap::with_capacity(N);
				let mut keys = Vec::with_capacity(N);
				for i in 0..N as u32 {
					keys.push(slot.insert(i.into()));
				}
				(slot, keys)
			},
			|(slot, keys)| {
				let mut sum = 0;
				for &i in &random {
					sum += slot.get(keys[i]).unwrap();
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});

	group.bench_function("DenseSlotMap<LargeData>", |b| {
		b.iter_batched(
			|| {
				let mut slot: DenseSlotMap<DefaultKey, LargeData> = DenseSlotMap::with_capacity(N);
				let mut keys = Vec::with_capacity(N);
				for i in 0..N as u32 {
					keys.push(slot.insert(i.into()));
				}
				(slot, keys)
			},
			|(slot, keys)| {
				let mut sum = 0;
				for &i in &random {
					sum += slot.get(keys[i]).unwrap().v;
				}
				black_box(sum)
			},
			BatchSize::PerIteration,
		)
	});
}

// ======================================================
// Vec / VecDeque / LinkedList / BinaryHeap
// ======================================================
//

// fn bench_vec<T: TestData>(c: &mut Criterion) {
// 	let random = random_indices(N);
//
//
//
// 	group.bench_function("insert_start", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.insert(0, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_middle", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.insert(vec.len() / 2, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.push(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_start", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.remove(0));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_middle", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.remove(v.len() / 2));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.pop());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("search_best", |b| b.iter(||
// black_box(vec.contains(&(0.into())))));
//
// 	group.bench_function("search_worst", |b| {
// 		b.iter(|| black_box(vec.contains(&((N as u32 + 1).into()))))
// 	});
//
// 	group.bench_function("binary_search_best", |b| {
// 		b.iter(|| black_box(vec.binary_search(&(0.into()))))
// 	});
//
// 	group.bench_function("binary_search_worst", |b| {
// 		b.iter(|| black_box(vec.binary_search(&((N as u32 + 1).into()))))
// 	});
//
// 	group.finish();
// }
//
// fn bench_vec_deq<T: TestData>(c: &mut Criterion) {
// 	let name = format!("VecDeque: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let vec: VecDeque<T> = (0..N as u32).map(|i| i.into()).collect();
// 	let random = random_indices(N);
//
//
//
// 	group.bench_function("insert_start", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.push_front(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_middle", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.insert(vec.len() / 2, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.push_back(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_start", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.pop_front());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_middle", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.remove(v.len() / 2));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.pop_back());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("search_best", |b| b.iter(||
// black_box(vec.contains(&(0.into())))));
//
// 	group.bench_function("search_worst", |b| {
// 		b.iter(|| black_box(vec.contains(&((N as u32 + 1).into()))))
// 	});
//
// 	group.bench_function("binary_search_best", |b| {
// 		b.iter(|| black_box(vec.binary_search(&(0.into()))))
// 	});
//
// 	group.bench_function("binary_search_worst", |b| {
// 		b.iter(|| black_box(vec.binary_search(&((N as u32 + 1).into()))))
// 	});
//
// 	group.finish();
// }
//
// fn bench_bin_heap<T: TestData>(c: &mut Criterion) {
// 	let name = format!("BinaryHeap: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let vec: BinaryHeap<T> = (0..N as u32).map(|i| i.into()).collect();
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.push(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.pop());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.finish();
// }
//
// fn bench_linked_list<T: TestData>(c: &mut Criterion) {
// 	let name = format!("LinkedList: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let vec: LinkedList<T> = (0..N as u32).map(|i| i.into()).collect();
//
//
//
//
// 	group.bench_function("sequential_iter()_iter", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for v in vec.iter() {
// 				sum += (*v).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert_start", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.push_front(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				v.push_back(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_start", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.pop_front());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| vec.clone(),
// 			|mut v| {
// 				black_box(v.pop_back());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("search_best", |b| b.iter(||
// black_box(vec.contains(&(0.into())))));
//
// 	group.bench_function("search_worst", |b| {
// 		b.iter(|| black_box(vec.contains(&((N as u32 + 1).into()))))
// 	});
//
// 	group.bench_function("half_split", |b| {
// 		b.iter(|| {
// 			let mut v = vec.clone();
// 			black_box(v.split_off(v.len() / 2))
// 		})
// 	});
//
// 	group.finish();
// }
//
// //
// // ======================================================
// // HashMap / RapidHashMap
// // ======================================================
// //
//
// fn bench_std_hashmap<T: TestData>(c: &mut Criterion) {
// 	let name = format!("StdHashMap: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let random = random_indices(N);
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += (*map.get(&(i as u32)).unwrap()).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert", |b| {
// 		b.iter_batched(
// 			|| map.clone(),
// 			|mut map| {
// 				black_box(map.insert((map.len() + 1) as u32, ((map.len() + 1) as
// u32).into())) 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove", |b| {
// 		b.iter_batched(
// 			|| map.clone(),
// 			|mut map| black_box(map.remove(&((map.len() - 1) as u32))),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("contains", |b| {
// 		b.iter(|| {
// 			black_box(map.contains_key(&((map.len() - 1) as u32)));
// 		})
// 	});
//
// 	group.finish();
// }
//
// fn bench_rapid_hashmap<T: TestData>(c: &mut Criterion) {
// 	let name = format!("RapidHashMap: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let map = {
// 		let mut map = RapidHashMap::<u32, T>::with_capacity(N);
// 		for i in 0..N {
// 			map.insert(i as u32, (i as u32).into());
// 		}
// 		map
// 	};
//
// 	let random = random_indices(N);
//
// 	group.bench_function("extend", |b| {
// 		b.iter_batched(
// 			|| {
// 				(
// 					(0..(N / 2))
// 						.map(|i| (i as u32, (i as u32).into()))
// 						.collect::<RapidHashMap<u32, T>>(),
// 					((N / 2 + 1)..N)
// 						.map(|i| (i as u32, (i as u32).into()))
// 						.collect::<RapidHashMap<u32, T>>(),
// 				)
// 			},
// 			|(mut v, v1)| black_box(v.extend(v1)),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	let pred = |v: T| <T as Into<u32>>::into(v) % 2 == 0;
//
// 	group.bench_function("retain", |b| {
// 		b.iter_batched(
// 			|| map.clone(),
// 			|mut v| {
// 				v.retain(|_k, v| pred(*v));
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("sequential_for_loop_iter", |b| {
// 		b.iter_batched(
// 			|| map.clone(),
// 			|map| {
// 				let mut sum = 0;
// 				for (_k, v) in map {
// 					sum += v.into();
// 				}
// 				black_box(sum)
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("sequential_iter()_iter", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for (_k, v) in map.iter() {
// 				sum += (*v).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += (*map.get(&(i as u32)).unwrap()).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert", |b| {
// 		b.iter_batched(
// 			|| map.clone(),
// 			|mut map| {
// 				black_box(map.insert((map.len() + 1) as u32, ((map.len() + 1) as
// u32).into())) 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove", |b| {
// 		b.iter_batched(
// 			|| map.clone(),
// 			|mut map| black_box(map.remove(&((map.len() - 1) as u32))),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("contains", |b| {
// 		b.iter(|| {
// 			black_box(map.contains_key(&((map.len() - 1) as u32)));
// 		})
// 	});
//
// 	group.finish();
// }
//
// //
// // ======================================================
// // HashSet / RapidHashSet
// // ======================================================
// //
//
// fn bench_std_hashset<T: TestData>(c: &mut Criterion) {
// 	let name = format!("StdHashSet: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let set = {
//
// 	};
//
// 	let random = random_indices(N);
//
//
//
// 	group.bench_function("insert", |b| {
// 		b.iter_batched(
// 			|| set.clone(),
// 			|mut set| black_box(set.insert(((set.len() + 1) as u32).into())),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove", |b| {
// 		b.iter_batched(
// 			|| set.clone(),
// 			|mut set| black_box(set.remove(&(((set.len() - 1) as u32).into()))),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("contains", |b| {
// 		b.iter(|| {
// 			black_box(set.contains(&(((set.len() - 1) as u32).into())));
// 		})
// 	});
//
// 	group.finish();
// }
//
// fn bench_rapid_hashset<T: TestData>(c: &mut Criterion) {
// 	let name = format!("RapidHashSet: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let set = {
// 		let mut set = RapidHashSet::<T>::with_capacity(N);
// 		for i in 0..N {
// 			set.insert((i as u32).into());
// 		}
// 		set
// 	};
//
// 	let random = random_indices(N);
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += (*set.get(&((i as u32).into())).unwrap()).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert", |b| {
// 		b.iter_batched(
// 			|| set.clone(),
// 			|mut set| black_box(set.insert(((set.len() + 1) as u32).into())),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove", |b| {
// 		b.iter_batched(
// 			|| set.clone(),
// 			|mut set| black_box(set.remove(&(((set.len() - 1) as u32).into()))),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("contains", |b| {
// 		b.iter(|| {
// 			black_box(set.contains(&(((set.len() - 1) as u32).into())));
// 		})
// 	});
//
// 	group.finish();
// }
//
// //
// // ======================================================
// // SlotMap / DenseSlotMap
// // ======================================================
// //
//
// fn bench_slotmaps<T: TestData>(c: &mut Criterion) {
// 	let name = format!("SlotMap: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let (slot, keys) = {
// 		let mut slot: SlotMap<DefaultKey, T> = SlotMap::with_capacity(N);
//
// 		let mut keys = Vec::with_capacity(N);
//
// 		for i in 0..N as u32 {
// 			keys.push(slot.insert(i.into()));
// 		}
//
// 		(slot, keys)
// 	};
//
// 	let random = random_indices(N);
//
// 	group.bench_function("insert", |b| {
// 		b.iter_batched(
// 			|| slot.clone(),
// 			|mut slot| black_box(slot.insert(((slot.len() + 1) as u32).into())),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove", |b| {
// 		b.iter_batched(
// 			|| slot.clone(),
// 			|mut slot| black_box(slot.remove(*keys.last().unwrap())),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("contains", |b| {
// 		b.iter(|| {
// 			for key in keys.iter() {
// 				black_box(slot.contains_key(*key));
// 			}
// 		})
// 	});
//
// 	group.finish();
// }
//
// fn bench_dense_slotmaps<T: TestData>(c: &mut Criterion) {
// 	let name = format!("DenseSlotMap: {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let (slot, keys) = {
// 		let mut slot: DenseSlotMap<DefaultKey, T> = DenseSlotMap::with_capacity(N);
//
// 		let mut keys = Vec::with_capacity(N);
//
// 		for i in 0..N as u32 {
// 			keys.push(slot.insert(i.into()));
// 		}
//
// 		(slot, keys)
// 	};
//
// 	let random = random_indices(N);
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += (*slot.get(keys[i]).unwrap()).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert", |b| {
// 		b.iter_batched(
// 			|| slot.clone(),
// 			|mut slot| black_box(slot.insert(((slot.len() + 1) as u32).into())),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove", |b| {
// 		b.iter_batched(
// 			|| slot.clone(),
// 			|mut slot| black_box(slot.remove(*keys.last().unwrap())),
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("contains", |b| {
// 		b.iter(|| {
// 			black_box(slot.contains_key(*keys.last().unwrap()));
// 		})
// 	});
//
// 	group.finish();
// }
//
// //
// // ======================================================
// // SmallVec / ArrayVec / TinyVec
// // ======================================================
// //
//
// fn bench_smallvec<const K: usize, const M: usize, T: TestData>(c: &mut
// Criterion) { 	let name = format!("SmallVector: {M}/{K}_size, {}_data_bytes",
// size_of::<T>()); 	let mut group = c.benchmark_group(name);
//
// 	let small = {
// 		let mut small: SmallVec<T, K> = SmallVec::new();
// 		for i in 0..M as u32 {
// 			small.push(i.into());
// 		}
// 		small
// 	};
//
// 	let random = random_indices(M);
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += small[i].into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert_start", |b| {
// 		b.iter_batched(
// 			|| small.clone(),
// 			|mut v| {
// 				v.insert(0, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_middle", |b| {
// 		b.iter_batched(
// 			|| small.clone(),
// 			|mut v| {
// 				v.insert(v.len() / 2, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| small.clone(),
// 			|mut v| {
// 				v.push(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_start", |b| {
// 		b.iter_batched(
// 			|| small.clone(),
// 			|mut v| {
// 				black_box(v.remove(0));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_middle", |b| {
// 		b.iter_batched(
// 			|| small.clone(),
// 			|mut v| {
// 				black_box(v.remove(v.len() / 2));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| small.clone(),
// 			|mut v| {
// 				black_box(v.pop());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("search_best", |b| b.iter(||
// black_box(small.contains(&(0.into())))));
//
// 	group.bench_function("search_worst", |b| {
// 		b.iter(|| black_box(small.contains(&((small.len() as u32 + 1).into()))))
// 	});
//
// 	group.bench_function("binary_search_best", |b| {
// 		b.iter(|| black_box(small.binary_search(&(0.into()))))
// 	});
//
// 	group.bench_function("binary_search_worst", |b| {
// 		b.iter(|| black_box(small.binary_search(&((small.len() as u32 +
// 1).into())))) 	});
//
// 	group.finish();
// }
//
// fn bench_arrayvec<const K: usize, T: TestData>(c: &mut Criterion) {
// 	let name = format!("ArrayVec: {K}_size, {}_data_bytes", size_of::<T>());
// 	let mut group = c.benchmark_group(name);
//
// 	let array = {
// 		let mut array: ArrayVec<T, K> = ArrayVec::new();
// 		for i in 0..K as u32 {
// 			array.push(i.into());
// 		}
// 		array
// 	};
//
// 	let random = random_indices(K);
//
// 	let pred = |v: T| <T as Into<u32>>::into(v) % 2 == 0;
//
// 	group.bench_function("retain", |b| {
// 		b.iter_batched(
// 			|| array.clone(),
// 			|mut v| {
// 				v.retain(|i| pred(*i));
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("sequential_for_loop_iter", |b| {
// 		b.iter_batched(
// 			|| array.clone(),
// 			|array| {
// 				let mut sum = 0;
// 				for v in array {
// 					sum += v.into();
// 				}
// 				black_box(sum)
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("sequential_iter()_iter", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for v in array.iter() {
// 				sum += (*v).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += array[i].into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert_start", |b| {
// 		b.iter_batched(
// 			|| {
// 				let mut array: ArrayVec<T, K> = ArrayVec::new();
// 				for i in 0..(K - 1) as u32 {
// 					array.push(i.into());
// 				}
// 				array
// 			},
// 			|mut v| {
// 				v.insert(0, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_middle", |b| {
// 		b.iter_batched(
// 			|| {
// 				let mut array: ArrayVec<T, K> = ArrayVec::new();
// 				for i in 0..(K - 1) as u32 {
// 					array.push(i.into());
// 				}
// 				array
// 			},
// 			|mut v| {
// 				v.insert(v.len() / 2, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| {
// 				let mut array: ArrayVec<T, K> = ArrayVec::new();
// 				for i in 0..(K - 1) as u32 {
// 					array.push(i.into());
// 				}
// 				array
// 			},
// 			|mut v| {
// 				v.push(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_start", |b| {
// 		b.iter_batched(
// 			|| array.clone(),
// 			|mut v| {
// 				black_box(v.remove(0));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_middle", |b| {
// 		b.iter_batched(
// 			|| array.clone(),
// 			|mut v| {
// 				black_box(v.remove(v.len() / 2));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| array.clone(),
// 			|mut v| {
// 				black_box(v.pop());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("search_best", |b| b.iter(||
// black_box(array.contains(&(0.into())))));
//
// 	group.bench_function("search_worst", |b| {
// 		b.iter(|| black_box(array.contains(&((array.len() as u32 + 1).into()))))
// 	});
//
// 	group.bench_function("binary_search_best", |b| {
// 		b.iter(|| black_box(array.binary_search(&(0.into()))))
// 	});
//
// 	group.bench_function("binary_search_worst", |b| {
// 		b.iter(|| black_box(array.binary_search(&((array.len() as u32 +
// 1).into())))) 	});
//
// 	group.finish();
// }
//
// fn bench_tinyvec<const K: usize, const M: usize, T: TestData>(c: &mut
// Criterion) { 	let name = format!("TinyVec: {M}/{K}_size, {}_data_bytes",
// size_of::<T>()); 	let mut group = c.benchmark_group(name);
//
// 	let mut tiny: TinyVec<[T; K]> = TinyVec::new();
// 	let random = random_indices(M);
//
// 	for i in 0..M as u32 {
// 		tiny.push(i.into());
// 	}
//
// 	// Not testing 'extend' yet, because there are too many cases.
//
// 	let pred = |v: T| <T as Into<u32>>::into(v) % 2 == 0;
//
// 	group.bench_function("retain", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				v.retain(|i| pred(*i));
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("sequential_for_loop_iter", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for v in tiny.clone() {
// 				sum += v.into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("sequential_iter()_iter", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for v in tiny.iter() {
// 				sum += (*v).into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("random_access", |b| {
// 		b.iter(|| {
// 			let mut sum = 0;
// 			for &i in &random {
// 				sum += tiny[i].into();
// 			}
// 			black_box(sum)
// 		})
// 	});
//
// 	group.bench_function("insert_start", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				v.insert(0, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_middle", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				v.insert(v.len() / 2, 1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("insert_end", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				v.push(1.into());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_start", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				black_box(v.remove(0));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_middle", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				black_box(v.remove(v.len() / 2));
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("remove_end", |b| {
// 		b.iter_batched(
// 			|| tiny.clone(),
// 			|mut v| {
// 				black_box(v.pop());
// 				black_box(v);
// 			},
// 			BatchSize::PerIteration,
// 		)
// 	});
//
// 	group.bench_function("search_best", |b| b.iter(||
// black_box(tiny.contains(&(0.into())))));
//
// 	group.bench_function("search_worst", |b| {
// 		b.iter(|| black_box(tiny.contains(&((tiny.len() as u32 + 1).into()))))
// 	});
//
// 	group.bench_function("binary_search_best", |b| {
// 		b.iter(|| black_box(tiny.binary_search(&(0.into()))))
// 	});
//
// 	group.bench_function("binary_search_worst", |b| {
// 		b.iter(|| black_box(tiny.binary_search(&((tiny.len() as u32 + 1).into()))))
// 	});
//
// 	group.finish();
// }
//
// // ==== == == == == == == == == ==== == == == == == == == == == == == == ==
// == == // Register
// // ==== == == == == == == == == ==== == == == == == == == == == == == == ==
// == ==
//
// criterion_group!(
// 	name = benches;
// 	config = Criterion::default().sample_size(100).
// measurement_time(Duration::from_secs(20)); 	targets =
// 	bench_vec::<u32>,
// 	bench_vec::<LargeData>,
// 	bench_vec_deq::<u32>,
// 	bench_vec_deq::<LargeData>,
// 	bench_bin_heap::<u32>,
// 	bench_bin_heap::<LargeData>,
// 	bench_linked_list::<u32>,
// 	bench_linked_list::<LargeData>,
// 	bench_std_hashmap::<u32>,
// 	bench_std_hashmap::<LargeData>,
// 	bench_rapid_hashmap::<u32>,
// 	bench_rapid_hashmap::<LargeData>,
// 	bench_std_hashset::<u32>,
// 	bench_std_hashset::<LargeData>,
// 	bench_rapid_hashset::<u32>,
// 	bench_rapid_hashset::<LargeData>,
// 	bench_slotmaps::<u32>,
// 	bench_slotmaps::<LargeData>,
// 	bench_dense_slotmaps::<u32>,
// 	bench_dense_slotmaps::<LargeData>,
//
// 	bench_smallvec::<10, 10, u32>,
// 	bench_smallvec::<10, 20, u32>,
// 	bench_smallvec::<10, N, u32>,
// 	bench_smallvec::<10, 10, LargeData>,
// 	bench_smallvec::<10, 20, LargeData>,
// 	bench_smallvec::<10, N, LargeData>,
//
// 	bench_tinyvec::<10, 10, u32>,
// 	bench_tinyvec::<10, 20, u32>,
// 	bench_tinyvec::<10, N, u32>,
// 	bench_tinyvec::<10, 10, LargeData>,
// 	bench_tinyvec::<10, 20, LargeData>,
// 	bench_tinyvec::<10, N, LargeData>,
//
// 	bench_arrayvec::<20, u32>,
// 	bench_arrayvec::<1024, u32>,
// 	bench_arrayvec::<N, u32>,
// 	bench_arrayvec::<20, LargeData>,
// 	bench_arrayvec::<1024, LargeData>,
// 	bench_arrayvec::<N, LargeData>,
// );

criterion_group!(
	name = benches;
	config = Criterion::default().sample_size(100).measurement_time(Duration::from_secs(15));
	targets =
	bench_push_raw::<10, 20, N>,
	bench_push_preallocated::<10, 20, N>,
	bench_collect::<10, 20, N>,
	bench_iter_sequential_owned,
	bench_iter_sequential_borrowed,
	bench_iter_sequential_with_step,
	batch_random_access
);

criterion_main!(benches);
