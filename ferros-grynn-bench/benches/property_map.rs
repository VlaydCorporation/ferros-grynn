use std::{
	collections::{BTreeMap, hash_map::Iter as HashIter},
	hint::black_box,
	mem::size_of,
	slice::Iter as SliceIter,
	time::Duration,
};

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ferros_grynn::values::{PropertyKeyId, PropertyMap, Value};
use rapidhash::{HashMapExt, RapidHashMap};
use smallvec::SmallVec;

const SIZES: [usize; 8] = [0, 1, 4, 8, 9, 16, 32, 128];
const INLINE_PROPERTIES: usize = 8;

type Entry = (PropertyKeyId, Value);
type InlineRepr = SmallVec<Entry, INLINE_PROPERTIES>;
type HeapRepr = RapidHashMap<PropertyKeyId, Value>;

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum BoxedEnumRepr {
	Inline(InlineRepr),
	Heap(HeapRepr),
}

#[derive(Default, Clone)]
struct BoxedEnumPropertyMap {
	repr: Option<Box<BoxedEnumRepr>>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum DirectHybridRepr {
	Inline(InlineRepr),
	Heap(HeapRepr),
}

impl Default for DirectHybridRepr {
	fn default() -> Self {
		Self::Inline(InlineRepr::new())
	}
}

#[derive(Default, Clone)]
struct DirectHybridPropertyMap {
	repr: DirectHybridRepr,
}

#[derive(Default, Clone)]
struct SortedVecPropertyMap {
	entries: Vec<Entry>,
}

enum BoxedEnumIter<'a> {
	Empty,
	Inline(SliceIter<'a, Entry>),
	Heap(HashIter<'a, PropertyKeyId, Value>),
}

impl<'a> Iterator for BoxedEnumIter<'a> {
	type Item = (PropertyKeyId, &'a Value);

	fn next(&mut self) -> Option<Self::Item> {
		match self {
			Self::Empty => None,
			Self::Inline(values) => values.next().map(|(key, value)| (*key, value)),
			Self::Heap(values) => values.next().map(|(key, value)| (*key, value)),
		}
	}
}

impl BoxedEnumPropertyMap {
	fn insert(&mut self, key: PropertyKeyId, value: Value) {
		let Some(repr) = self.repr.as_mut() else {
			let mut values = InlineRepr::new();
			values.push((key, value));
			self.repr = Some(Box::new(BoxedEnumRepr::Inline(values)));
			return;
		};

		let spill = match repr.as_mut() {
			BoxedEnumRepr::Inline(values) => {
				match values.binary_search_by_key(&key, |(key, _)| *key) {
					Ok(index) => {
						values[index].1 = value;
						return;
					}
					Err(index) if values.len() < INLINE_PROPERTIES => {
						values.insert(index, (key, value));
						return;
					}
					Err(_) => std::mem::take(values),
				}
			}
			BoxedEnumRepr::Heap(values) => {
				values.insert(key, value);
				return;
			}
		};

		let mut heap = HeapRepr::with_capacity(spill.len() + 1);
		heap.extend(spill);
		heap.insert(key, value);
		**repr = BoxedEnumRepr::Heap(heap);
	}

	fn get(&self, key: PropertyKeyId) -> Option<&Value> {
		match self.repr.as_deref() {
			None => None,
			Some(BoxedEnumRepr::Inline(values)) => values
				.binary_search_by_key(&key, |(key, _)| *key)
				.ok()
				.map(|index| &values[index].1),
			Some(BoxedEnumRepr::Heap(values)) => values.get(&key),
		}
	}

	fn iter(&self) -> BoxedEnumIter<'_> {
		match self.repr.as_deref() {
			None => BoxedEnumIter::Empty,
			Some(BoxedEnumRepr::Inline(values)) => BoxedEnumIter::Inline(values.iter()),
			Some(BoxedEnumRepr::Heap(values)) => BoxedEnumIter::Heap(values.iter()),
		}
	}
}

impl DirectHybridPropertyMap {
	fn insert(&mut self, key: PropertyKeyId, value: Value) {
		let spill = match &mut self.repr {
			DirectHybridRepr::Inline(values) => {
				match values.binary_search_by_key(&key, |(key, _)| *key) {
					Ok(index) => {
						values[index].1 = value;
						return;
					}
					Err(index) if values.len() < INLINE_PROPERTIES => {
						values.insert(index, (key, value));
						return;
					}
					Err(_) => std::mem::take(values),
				}
			}
			DirectHybridRepr::Heap(values) => {
				values.insert(key, value);
				return;
			}
		};

		let mut heap = HeapRepr::with_capacity(spill.len() + 1);
		heap.extend(spill);
		heap.insert(key, value);
		self.repr = DirectHybridRepr::Heap(heap);
	}

	fn get(&self, key: PropertyKeyId) -> Option<&Value> {
		match &self.repr {
			DirectHybridRepr::Inline(values) => values
				.binary_search_by_key(&key, |(key, _)| *key)
				.ok()
				.map(|index| &values[index].1),
			DirectHybridRepr::Heap(values) => values.get(&key),
		}
	}

	fn iter(&self) -> BoxedEnumIter<'_> {
		match &self.repr {
			DirectHybridRepr::Inline(values) => BoxedEnumIter::Inline(values.iter()),
			DirectHybridRepr::Heap(values) => BoxedEnumIter::Heap(values.iter()),
		}
	}
}

macro_rules! impl_sorted_map {
	($map:ty) => {
		impl $map {
			fn insert(&mut self, key: PropertyKeyId, value: Value) {
				match self.entries.binary_search_by_key(&key, |(key, _)| *key) {
					Ok(index) => self.entries[index].1 = value,
					Err(index) => self.entries.insert(index, (key, value)),
				}
			}

			fn get(&self, key: PropertyKeyId) -> Option<&Value> {
				self.entries
					.binary_search_by_key(&key, |(key, _)| *key)
					.ok()
					.map(|index| &self.entries[index].1)
			}

			fn iter(&self) -> SliceIter<'_, Entry> {
				self.entries.iter()
			}
		}
	};
}

impl_sorted_map!(SortedVecPropertyMap);
fn value(index: usize) -> Value {
	Value::from(index as i64)
}

fn property_map(size: usize) -> PropertyMap {
	(0..size).map(|index| (PropertyKeyId::new(index as u32), value(index))).collect()
}

fn boxed_enum_map(size: usize) -> BoxedEnumPropertyMap {
	let mut result = BoxedEnumPropertyMap::default();
	for index in 0..size {
		result.insert(PropertyKeyId::new(index as u32), value(index));
	}
	result
}

fn direct_hybrid_map(size: usize) -> DirectHybridPropertyMap {
	let mut result = DirectHybridPropertyMap::default();
	for index in 0..size {
		result.insert(PropertyKeyId::new(index as u32), value(index));
	}
	result
}

fn sorted_vec_map(size: usize) -> SortedVecPropertyMap {
	let mut result = SortedVecPropertyMap::default();
	for index in 0..size {
		result.insert(PropertyKeyId::new(index as u32), value(index));
	}
	result
}

fn btree_map(size: usize) -> BTreeMap<PropertyKeyId, Value> {
	(0..size).map(|index| (PropertyKeyId::new(index as u32), value(index))).collect()
}

fn rapid_map(size: usize) -> RapidHashMap<PropertyKeyId, Value> {
	(0..size).map(|index| (PropertyKeyId::new(index as u32), value(index))).collect()
}

fn bench_build(c: &mut Criterion) {
	print_layout();
	let mut group = c.benchmark_group("property_map/build");

	for size in SIZES {
		group.throughput(Throughput::Elements(size.max(1) as u64));

		group.bench_with_input(BenchmarkId::new("production", size), &size, |b, &size| {
			b.iter(|| black_box(property_map(size)));
		});
		group.bench_with_input(BenchmarkId::new("boxed_enum", size), &size, |b, &size| {
			b.iter(|| black_box(boxed_enum_map(size)));
		});
		group.bench_with_input(BenchmarkId::new("direct_hybrid", size), &size, |b, &size| {
			b.iter(|| black_box(direct_hybrid_map(size)));
		});
		group.bench_with_input(BenchmarkId::new("sorted_vec", size), &size, |b, &size| {
			b.iter(|| black_box(sorted_vec_map(size)));
		});
		group.bench_with_input(BenchmarkId::new("btree", size), &size, |b, &size| {
			b.iter(|| black_box(btree_map(size)));
		});
		group.bench_with_input(BenchmarkId::new("rapid_hash", size), &size, |b, &size| {
			b.iter(|| black_box(rapid_map(size)));
		});
	}

	group.finish();
}

fn bench_lookup(c: &mut Criterion) {
	let mut group = c.benchmark_group("property_map/lookup");

	for size in SIZES {
		let production = property_map(size);
		let boxed = boxed_enum_map(size);
		let direct = direct_hybrid_map(size);
		let sorted = sorted_vec_map(size);
		let btree = btree_map(size);
		let rapid = rapid_map(size);
		let hit = PropertyKeyId::new(size.saturating_sub(1) as u32);
		let miss = PropertyKeyId::new(size as u32);

		for (case, key) in [("hit", hit), ("miss", miss)] {
			group.bench_function(BenchmarkId::new(format!("production_{case}"), size), |b| {
				b.iter(|| black_box(production.get(black_box(key))));
			});
			group.bench_function(BenchmarkId::new(format!("boxed_enum_{case}"), size), |b| {
				b.iter(|| black_box(boxed.get(black_box(key))));
			});
			group.bench_function(BenchmarkId::new(format!("direct_hybrid_{case}"), size), |b| {
				b.iter(|| black_box(direct.get(black_box(key))));
			});
			group.bench_function(BenchmarkId::new(format!("sorted_vec_{case}"), size), |b| {
				b.iter(|| black_box(sorted.get(black_box(key))));
			});
			group.bench_function(BenchmarkId::new(format!("btree_{case}"), size), |b| {
				b.iter(|| black_box(btree.get(&black_box(key))));
			});
			group.bench_function(BenchmarkId::new(format!("rapid_hash_{case}"), size), |b| {
				b.iter(|| black_box(rapid.get(&black_box(key))));
			});
		}
	}

	group.finish();
}

fn bench_iterate_and_clone(c: &mut Criterion) {
	let mut iterate = c.benchmark_group("property_map/iterate");
	for size in SIZES {
		let production = property_map(size);
		let boxed = boxed_enum_map(size);
		let direct = direct_hybrid_map(size);
		let sorted = sorted_vec_map(size);
		let btree = btree_map(size);
		let rapid = rapid_map(size);

		iterate.throughput(Throughput::Elements(size.max(1) as u64));
		iterate.bench_function(BenchmarkId::new("production", size), |b| {
			b.iter(|| {
				for entry in production.iter() {
					black_box(entry);
				}
			});
		});
		iterate.bench_function(BenchmarkId::new("boxed_enum", size), |b| {
			b.iter(|| {
				for entry in boxed.iter() {
					black_box(entry);
				}
			});
		});
		iterate.bench_function(BenchmarkId::new("direct_hybrid", size), |b| {
			b.iter(|| {
				for entry in direct.iter() {
					black_box(entry);
				}
			});
		});
		iterate.bench_function(BenchmarkId::new("sorted_vec", size), |b| {
			b.iter(|| {
				for entry in sorted.iter() {
					black_box(entry);
				}
			});
		});
		iterate.bench_function(BenchmarkId::new("btree", size), |b| {
			b.iter(|| {
				for entry in &btree {
					black_box(entry);
				}
			});
		});
		iterate.bench_function(BenchmarkId::new("rapid_hash", size), |b| {
			b.iter(|| {
				for entry in &rapid {
					black_box(entry);
				}
			});
		});
	}
	iterate.finish();

	let mut clone = c.benchmark_group("property_map/clone");
	for size in SIZES {
		let production = property_map(size);
		let boxed = boxed_enum_map(size);
		let direct = direct_hybrid_map(size);
		let sorted = sorted_vec_map(size);
		let btree = btree_map(size);
		let rapid = rapid_map(size);

		clone.bench_function(BenchmarkId::new("production", size), |b| {
			b.iter_batched(|| &production, |value| black_box(value.clone()), BatchSize::SmallInput);
		});
		clone.bench_function(BenchmarkId::new("boxed_enum", size), |b| {
			b.iter_batched(|| &boxed, |value| black_box(value.clone()), BatchSize::SmallInput);
		});
		clone.bench_function(BenchmarkId::new("direct_hybrid", size), |b| {
			b.iter_batched(|| &direct, |value| black_box(value.clone()), BatchSize::SmallInput);
		});
		clone.bench_function(BenchmarkId::new("sorted_vec", size), |b| {
			b.iter_batched(|| &sorted, |value| black_box(value.clone()), BatchSize::SmallInput);
		});
		clone.bench_function(BenchmarkId::new("btree", size), |b| {
			b.iter_batched(|| &btree, |value| black_box(value.clone()), BatchSize::SmallInput);
		});
		clone.bench_function(BenchmarkId::new("rapid_hash", size), |b| {
			b.iter_batched(|| &rapid, |value| black_box(value.clone()), BatchSize::SmallInput);
		});
	}
	clone.finish();
}

fn print_layout() {
	println!(
		"PropertyMap layout: production={} B, Value={} B, old inline allocation={} B, hash \
		 header={} B, boxed hybrid allocation={} B, direct hybrid={} B, Vec={} B",
		size_of::<PropertyMap>(),
		size_of::<Value>(),
		size_of::<InlineRepr>(),
		size_of::<HeapRepr>(),
		size_of::<BoxedEnumRepr>(),
		size_of::<DirectHybridPropertyMap>(),
		size_of::<SortedVecPropertyMap>(),
	);
}

criterion_group!(
	name = benches;
	config = Criterion::default()
		.sample_size(50)
		.measurement_time(Duration::from_secs(5));
	targets = bench_build, bench_lookup, bench_iterate_and_clone
);
criterion_main!(benches);
